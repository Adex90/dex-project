//! Motor transaccional de la IFPE (bóveda a la vista, reserva del 100%).
//!
//! Cada operación:
//! 1. abre una transacción con el rol `sc_ifpe` ([`db::begin`]);
//! 2. bloquea las filas involucradas con `SELECT ... FOR NO KEY UPDATE` en el
//!    orden global fijo de abajo para evitar deadlocks;
//! 3. comprueba la idempotencia *después* de bloquear, de modo que dos
//!    peticiones con la misma clave se serializan y la segunda ve la primera;
//! 4. mueve saldos y, si entra o sale efectivo, la reserva en custodia por el
//!    mismo monto (si no, el trigger diferido `SC_RESERVA_100` aborta el COMMIT);
//! 5. inserta el movimiento en el ledger y verifica que el `tx_hash` que
//!    calculó la base de datos coincide con el recalculado en Rust;
//! 6. evalúa PLD con `aml::evaluar` antes de escribir (después de los candados
//!    de cuentas y de las validaciones, antes del candado de la reserva y de
//!    cualquier UPDATE o INSERT) y hace COMMIT. Cualquier error antes del
//!    COMMIT descarta la transacción completa (ROLLBACK al soltar `Tx`). Un
//!    BLOQUEAR de PLD es la única excepción: `aml::confirmar_bloqueo` confirma
//!    la evaluación, la alerta y el congelamiento **sin** renglones del ledger
//!    y responde `OPERACION_NO_DISPONIBLE`.
//!
//! Orden global de candados (todos los caminos del dinero, IFPE y SOFOM):
//!
//! 0. candados propios del camino que preceden a las cuentas: el candado
//!    consultivo de identidad del pago SPEI (`spei-inbound:<identidad>`, solo
//!    entradas); el candado de idempotencia del oficial (`aml:accion:<clave>`)
//!    y la fila de `kyc.clientes` `FOR NO KEY UPDATE` (solo funciones de los
//!    oficiales y el trigger de la Lista de Personas Bloqueadas). Los caminos
//!    del dinero nunca bloquean `kyc.clientes` más fuerte que el `KEY SHARE`
//!    de las llaves foráneas, que no choca con `FOR NO KEY UPDATE`;
//! 1. filas de `ifpe.cuentas`, por id ascendente;
//! 2. candados consultivos PLD por cliente (`aml:cliente:<id>`, los toma
//!    `aml.bloquear_cliente` justo después de volver a bloquear la fila IFPE de
//!    ese cliente). En una transferencia ambas filas ya están tomadas, así que
//!    tomar origen y luego destino no forma ciclos;
//! 3. reserva en custodia (`ifpe.reserva_custodia`);
//! 4. cuenta SOFOM;
//! 5. caja de la SOFOM;
//! 6. contrato o crédito;
//! 7. [`estado_reserva`, cabeza IFPE, cabeza SOFOM], que toman los triggers.
//!    La bitácora PLD no tiene cabeza (se sella por lotes).
//!
//! Toda escritura PLD de un camino del dinero ocurre bajo 1–2. La única
//! posterior a 3–7 es la retención (`aml.aplicar_retencion`), que toca la
//! fila IFPE y la cabeza IFPE que la transacción ya tiene.

pub mod hash;

use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    aml::{self, Canal, Contraparte, Decision, Operacion},
    config::ConfigAml,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    models::{
        ifpe::{CuentaIfpe, ReservaCustodia, TipoTxIfpe, TransaccionIfpe},
        kyc::EstatusKyc,
        EstadoCuenta,
    },
    pld,
    spei::clabe,
};

/// Resultado de una operación del ledger.
#[derive(Debug, Clone, Serialize)]
pub struct Movimiento {
    pub transaccion: TransaccionIfpe,
    /// `true` si la clave de idempotencia ya existía y se devolvió el movimiento original.
    pub idempotente: bool,
}

// -----------------------------------------------------------------------------
// Primitivas compartidas (también las usa el puente IFPE -> SOFOM)
// -----------------------------------------------------------------------------

pub(crate) struct NuevaTxIfpe<'a> {
    pub id: Uuid,
    pub tipo: TipoTxIfpe,
    pub origen: Option<Uuid>,
    pub destino: Option<Uuid>,
    pub reserva: Option<Uuid>,
    pub monto: Decimal,
    pub concepto: Option<&'a str>,
    pub referencia_externa: Option<&'a str>,
    pub clave: &'a str,
    pub espejo: Option<Uuid>,
}

/// Inserta un movimiento y verifica su hash. La base de datos asigna
/// `secuencia`, `hash_anterior`, `creado_en` y `tx_hash` (trigger
/// `ifpe.fn_encadenar_tx`, que además bloquea la cabeza del ledger).
pub(crate) async fn insertar_tx_ifpe(tx: &mut Tx, n: NuevaTxIfpe<'_>) -> AppResult<TransaccionIfpe> {
    let t: TransaccionIfpe = sqlx::query_as(
        "INSERT INTO ifpe.transacciones \
         (id, tipo, cuenta_origen_id, cuenta_destino_id, reserva_id, monto, concepto, referencia_externa, \
          clave_idempotencia, ref_movimiento_espejo) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *",
    )
    .bind(n.id)
    .bind(n.tipo)
    .bind(n.origen)
    .bind(n.destino)
    .bind(n.reserva)
    .bind(n.monto)
    .bind(n.concepto)
    .bind(n.referencia_externa)
    .bind(n.clave)
    .bind(n.espejo)
    .fetch_one(&mut **tx)
    .await?;
    let recalculado = hash::hash_ifpe(&t);
    if recalculado != t.tx_hash {
        return Err(AppError::Integridad(format!(
            "IFPE secuencia {}: tx_hash de la base de datos {} != recalculado {}",
            t.secuencia, t.tx_hash, recalculado
        )));
    }
    Ok(t)
}

pub(crate) async fn bloquear_cuenta_ifpe(tx: &mut Tx, cuenta_id: Uuid) -> AppResult<CuentaIfpe> {
    sqlx::query_as("SELECT * FROM ifpe.cuentas WHERE id = $1 FOR NO KEY UPDATE")
        .bind(cuenta_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("cuenta IFPE no encontrada"))
}

pub(crate) async fn id_cuenta_ifpe_de_cliente(tx: &mut Tx, cliente_id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar("SELECT id FROM ifpe.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'")
        .bind(cliente_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("el cliente no tiene cuenta en la bóveda IFPE"))
}

/// Cuenta de reserva en custodia por defecto (la activa más antigua), bloqueada.
pub(crate) async fn bloquear_reserva(tx: &mut Tx) -> AppResult<ReservaCustodia> {
    sqlx::query_as("SELECT * FROM ifpe.reserva_custodia WHERE activa ORDER BY creado_en, id LIMIT 1 FOR NO KEY UPDATE")
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::Interno(anyhow::anyhow!("no hay cuenta de reserva en custodia activa")))
}

pub(crate) async fn buscar_por_clave(tx: &mut Tx, clave: &str) -> AppResult<Option<TransaccionIfpe>> {
    Ok(sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE clave_idempotencia = $1")
        .bind(clave)
        .fetch_optional(&mut **tx)
        .await?)
}

/// Una clave reutilizada debe describir exactamente la misma operación.
/// Compara **todos** los campos materiales (incluidos concepto y referencia
/// externa, p. ej. la CLABE destino de un retiro): una repetición con datos
/// distintos es otra operación y se rechaza con 409, nunca se "confirma".
#[allow(clippy::too_many_arguments)]
pub(crate) fn exigir_misma_operacion(
    previa: &TransaccionIfpe,
    tipo: TipoTxIfpe,
    monto: Decimal,
    origen: Option<Uuid>,
    destino: Option<Uuid>,
    concepto: Option<&str>,
    referencia_externa: Option<&str>,
) -> AppResult<()> {
    if previa.tipo != tipo
        || previa.monto != monto
        || previa.cuenta_origen_id != origen
        || previa.cuenta_destino_id != destino
        || previa.concepto.as_deref() != concepto
        || previa.referencia_externa.as_deref() != referencia_externa
    {
        return Err(AppError::conflicto(
            "CLAVE_IDEMPOTENCIA_REUTILIZADA",
            "la clave de idempotencia ya se usó para una operación distinta",
        ));
    }
    Ok(())
}

pub(crate) fn exigir_debitable(cuenta: &CuentaIfpe) -> AppResult<()> {
    match cuenta.estado {
        EstadoCuenta::Activa => Ok(()),
        EstadoCuenta::Congelada => Err(AppError::negocio("CUENTA_CONGELADA", "la cuenta IFPE está congelada")),
        EstadoCuenta::Cerrada => Err(AppError::negocio("CUENTA_CERRADA", "la cuenta IFPE está cerrada")),
        // Congelamiento preventivo PLD: mismo error constante que la guardia
        // de la base de datos (sin aviso al cliente).
        EstadoCuenta::CongeladoPreventivo => Err(AppError::OperacionNoDisponible),
    }
}

/// Como [`exigir_debitable`], pero deja pasar el congelamiento preventivo: los
/// caminos de salida que evalúan PLD deben llegar a `aml::evaluar`, que
/// registra el intento (evaluación BLOQUEADA, `intentos_bloqueados + 1`,
/// bitácora `OPERACION_BLOQUEADA`) antes de confirmar sin ledger.
pub(crate) fn exigir_debitable_salvo_aml(cuenta: &CuentaIfpe) -> AppResult<()> {
    match cuenta.estado {
        EstadoCuenta::CongeladoPreventivo => Ok(()),
        _ => exigir_debitable(cuenta),
    }
}

pub(crate) fn exigir_acreditable(cuenta: &CuentaIfpe) -> AppResult<()> {
    if cuenta.estado == EstadoCuenta::Cerrada {
        return Err(AppError::negocio("CUENTA_CERRADA", "la cuenta IFPE está cerrada"));
    }
    Ok(())
}

pub(crate) fn exigir_saldo(cuenta: &CuentaIfpe, monto: Decimal) -> AppResult<()> {
    if cuenta.saldo_disponible < monto {
        return Err(AppError::negocio(
            "SALDO_INSUFICIENTE",
            format!("saldo disponible {} insuficiente para {}", cuenta.saldo_disponible, monto),
        ));
    }
    Ok(())
}

/// Entra efectivo: el saldo disponible de la cuenta y la reserva en custodia
/// suben juntos y se registra el DEPOSITO con el `id` indicado (el que se
/// evaluó en PLD, si aplica). La cuenta y la reserva deben estar bloqueadas
/// por el llamador (orden cuentas -> reserva).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn abonar_efectivo(
    tx: &mut Tx,
    id: Uuid,
    cuenta_id: Uuid,
    reserva_id: Uuid,
    monto: Decimal,
    concepto: Option<&str>,
    referencia_externa: Option<&str>,
    clave: &str,
) -> AppResult<TransaccionIfpe> {
    // Pasivo y reserva suben juntos: 1 peso depositado = 1 peso en custodia.
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $1 WHERE id = $2")
        .bind(monto)
        .bind(cuenta_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo + $1 WHERE id = $2")
        .bind(monto)
        .bind(reserva_id)
        .execute(&mut **tx)
        .await?;
    insertar_tx_ifpe(
        tx,
        NuevaTxIfpe {
            id,
            tipo: TipoTxIfpe::Deposito,
            origen: None,
            destino: Some(cuenta_id),
            reserva: Some(reserva_id),
            monto,
            concepto,
            referencia_externa,
            clave,
            espejo: None,
        },
    )
    .await
}

/// Sale efectivo: saldo disponible y reserva bajan juntos (RETIRO). Mismas
/// condiciones de bloqueo que [`abonar_efectivo`]; el saldo ya se comprobó.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn cargar_efectivo(
    tx: &mut Tx,
    id: Uuid,
    cuenta_id: Uuid,
    reserva_id: Uuid,
    monto: Decimal,
    concepto: Option<&str>,
    referencia_externa: Option<&str>,
    clave: &str,
) -> AppResult<TransaccionIfpe> {
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1 WHERE id = $2")
        .bind(monto)
        .bind(cuenta_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo - $1 WHERE id = $2")
        .bind(monto)
        .bind(reserva_id)
        .execute(&mut **tx)
        .await?;
    insertar_tx_ifpe(
        tx,
        NuevaTxIfpe {
            id,
            tipo: TipoTxIfpe::Retiro,
            origen: Some(cuenta_id),
            destino: None,
            reserva: Some(reserva_id),
            monto,
            concepto,
            referencia_externa,
            clave,
            espejo: None,
        },
    )
    .await
}

pub(crate) fn validar_texto(campo: &str, valor: Option<&str>, max: usize) -> AppResult<()> {
    if let Some(v) = valor {
        if v.chars().count() > max {
            return Err(AppError::validacion(format!("{campo} admite como máximo {max} caracteres")));
        }
        if v.chars().any(|c| c.is_control()) {
            return Err(AppError::validacion(format!("{campo} contiene caracteres de control")));
        }
    }
    Ok(())
}

// Las entradas y salidas de efectivo (DEPOSITO / RETIRO) solo ocurren por SPEI:
// `crate::spei::inbound` (abonos, reversos) y `crate::spei::outbound` (órdenes).
// La base de datos rechaza cualquier otra (migración 0005).

// -----------------------------------------------------------------------------
// Transferencia entre clientes de la bóveda (la reserva no se mueve)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Transferencia {
    pub cliente_origen_id: Uuid,
    pub clabe_destino: String,
    pub monto: Decimal,
    pub concepto: Option<String>,
    pub clave_idempotencia: String,
}

/// Los umbrales PLD viven en la base de datos (`aml.reglas_riesgo`); el
/// parámetro de configuración se conserva para que los llamadores no cambien.
pub async fn transferir(pool: &PgPool, _aml: &ConfigAml, t: &Transferencia) -> AppResult<Movimiento> {
    let monto = dinero::validar_monto(t.monto)?;
    validar_texto("concepto", t.concepto.as_deref(), 140)?;
    if !clabe::es_valida(&t.clabe_destino) {
        return Err(AppError::validacion("CLABE destino inválida"));
    }
    db::con_reintentos(|| transferir_tx(pool, t, monto)).await
}

async fn transferir_tx(pool: &PgPool, t: &Transferencia, monto: Decimal) -> AppResult<Movimiento> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let origen_id = id_cuenta_ifpe_de_cliente(&mut tx, t.cliente_origen_id).await?;
    // Cualquier CLABE activa de la cuenta (la principal o una de STP).
    let destino_id: Uuid =
        sqlx::query_scalar("SELECT cuenta_id FROM ifpe.clabes_asignadas WHERE clabe = $1 AND activa")
            .bind(&t.clabe_destino)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                AppError::no_encontrado("la CLABE destino no corresponde a una cuenta de Synthetic Capital")
            })?;
    if origen_id == destino_id {
        return Err(AppError::validacion("no puedes transferir a tu propia cuenta"));
    }

    // Orden global de bloqueo: por id ascendente.
    let (primero, segundo) = if origen_id < destino_id { (origen_id, destino_id) } else { (destino_id, origen_id) };
    let a = bloquear_cuenta_ifpe(&mut tx, primero).await?;
    let b = bloquear_cuenta_ifpe(&mut tx, segundo).await?;
    let (origen, destino) = if a.id == origen_id { (a, b) } else { (b, a) };

    if let Some(previa) = buscar_por_clave(&mut tx, &t.clave_idempotencia).await? {
        exigir_misma_operacion(
            &previa,
            TipoTxIfpe::Transferencia,
            monto,
            Some(origen.id),
            Some(destino.id),
            t.concepto.as_deref(),
            None,
        )?;
        return Ok(Movimiento { transaccion: previa, idempotente: true });
    }
    let perfil = pld::cargar_perfil(&mut tx, origen.cliente_id).await?;
    perfil.exigir_operable()?;
    // Una transferencia es un depósito para el beneficiario: exige su KYC
    // verificado (la política RLS de ifpe.transacciones lo repite) y que no
    // esté en la Lista de Personas Bloqueadas. Una sola respuesta para ambos
    // casos: el ordenante no tiene por qué conocer el estado KYC ni la
    // situación PLD de otro cliente (secreto bancario).
    let perfil_destino = pld::cargar_perfil(&mut tx, destino.cliente_id).await?;
    if perfil_destino.en_lista_bloqueadas || perfil_destino.estatus_kyc != EstatusKyc::Verificado {
        tracing::info!(destino = %destino.id, "transferencia a una cuenta que no puede recibir");
        return Err(AppError::negocio(
            "DESTINO_NO_ACREDITABLE",
            "la cuenta destino no puede recibir esta transferencia",
        ));
    }
    exigir_debitable_salvo_aml(&origen)?;
    // Acepta el congelamiento preventivo: una transferencia a un cliente
    // congelado se acredita (y se retiene) sin que el ordenante lo note.
    exigir_acreditable(&destino)?;
    exigir_saldo(&origen, monto)?;

    // PLD antes de escribir: primero la salida del ordenante (puede bloquear),
    // luego la entrada del beneficiario (nunca bloquea; puede retener).
    let mov_id = Uuid::new_v4();
    let salida = aml::evaluar(
        &mut tx,
        &Operacion {
            cliente_id: origen.cliente_id,
            canal: Canal::TransferenciaEnviada,
            monto,
            transaccion_id: mov_id,
            clave: &t.clave_idempotencia,
            contraparte: Contraparte {
                cuenta: Some(t.clabe_destino.clone()),
                cliente_id: Some(destino.cliente_id),
                ..Default::default()
            },
        },
    )
    .await?;
    if salida.decision == Decision::Bloquear {
        return aml::confirmar_bloqueo(tx, &salida).await;
    }
    let entrada = aml::evaluar(
        &mut tx,
        &Operacion {
            cliente_id: destino.cliente_id,
            canal: Canal::TransferenciaRecibida,
            monto,
            transaccion_id: mov_id,
            clave: &t.clave_idempotencia,
            contraparte: Contraparte {
                cuenta: Some(origen.clabe.clone()),
                cliente_id: Some(origen.cliente_id),
                ..Default::default()
            },
        },
    )
    .await?;

    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1 WHERE id = $2")
        .bind(monto)
        .bind(origen.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $1 WHERE id = $2")
        .bind(monto)
        .bind(destino.id)
        .execute(&mut *tx)
        .await?;
    let mov = insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: mov_id,
            tipo: TipoTxIfpe::Transferencia,
            origen: Some(origen.id),
            destino: Some(destino.id),
            reserva: None,
            monto,
            concepto: t.concepto.as_deref(),
            referencia_externa: None,
            clave: &t.clave_idempotencia,
            espejo: None,
        },
    )
    .await?;
    if entrada.decision == Decision::Retener {
        aml::retener(&mut tx, &entrada).await?;
    }
    tx.commit().await?;
    Ok(Movimiento { transaccion: mov, idempotente: false })
}

// -----------------------------------------------------------------------------
// Retención / liberación (cumplimiento). El dinero retenido sigue respaldado.
// -----------------------------------------------------------------------------

/// Concepto neutro de una retención en el ledger del cliente, el mismo que usa
/// PLD (`aml.aplicar_retencion`).
pub const CONCEPTO_RETENCION: &str = "Retención temporal";
/// Concepto neutro de una liberación, el mismo que usa PLD (`aml.liberar_retencion`).
pub const CONCEPTO_LIBERACION: &str = "Liberación de retención";

/// Retención o liberación del operador (`POST /operaciones/retenciones`).
///
/// En el ledger del cliente (`/ifpe/movimientos`) tiene la misma forma que una
/// retención de PLD: clave `ret:<id>` / `lib:<id>` y concepto neutro. El motivo
/// del operador y su clave de idempotencia quedan en
/// `ifpe.retenciones_operador`, que ninguna ruta del cliente sirve. Antes la
/// clave era `op:<operador>:<clave>` con el texto del operador, así que el
/// cliente sabía que una retención `ret:` era de cumplimiento.
#[derive(Debug, Clone)]
pub struct Retencion {
    pub cuenta_id: Uuid,
    pub monto: Decimal,
    pub motivo: String,
    /// `false` = retener (disponible -> bloqueado); `true` = liberar.
    pub liberar: bool,
    pub clave_idempotencia: String,
}

pub async fn retener(pool: &PgPool, r: &Retencion) -> AppResult<Movimiento> {
    let monto = dinero::validar_monto(r.monto)?;
    if r.motivo.trim().is_empty() {
        return Err(AppError::validacion("el motivo de la retención es obligatorio"));
    }
    validar_texto("motivo", Some(&r.motivo), 140)?;
    db::con_reintentos(|| retener_tx(pool, r, monto)).await
}

async fn retener_tx(pool: &PgPool, r: &Retencion, monto: Decimal) -> AppResult<Movimiento> {
    let (tipo, prefijo, concepto) = if r.liberar {
        (TipoTxIfpe::LiberacionRetencion, "lib", CONCEPTO_LIBERACION)
    } else {
        (TipoTxIfpe::Retencion, "ret", CONCEPTO_RETENCION)
    };
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let cuenta = bloquear_cuenta_ifpe(&mut tx, r.cuenta_id).await?;
    // Idempotencia (después del candado): la clave del operador vive en
    // ifpe.retenciones_operador, no en el ledger.
    let previa: Option<(Uuid, String)> =
        sqlx::query_as("SELECT transaccion_id, motivo FROM ifpe.retenciones_operador WHERE clave_idempotencia = $1")
            .bind(&r.clave_idempotencia)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((transaccion_id, motivo)) = previa {
        let previa: TransaccionIfpe = sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE id = $1")
            .bind(transaccion_id)
            .fetch_one(&mut *tx)
            .await?;
        exigir_misma_operacion(&previa, tipo, monto, Some(cuenta.id), Some(cuenta.id), Some(concepto), None)?;
        if motivo != r.motivo {
            return Err(AppError::conflicto(
                "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                "la clave de idempotencia ya se usó para una operación distinta",
            ));
        }
        return Ok(Movimiento { transaccion: previa, idempotente: true });
    }
    // Una clave ya usada en el ledger por otra operación sigue siendo un conflicto.
    if let Some(previa) = buscar_por_clave(&mut tx, &r.clave_idempotencia).await? {
        exigir_misma_operacion(&previa, tipo, monto, Some(cuenta.id), Some(cuenta.id), Some(&r.motivo), None)?;
        return Ok(Movimiento { transaccion: previa, idempotente: true });
    }
    exigir_acreditable(&cuenta)?;
    let (sql, disponible_requerido) = if r.liberar {
        (
            "UPDATE ifpe.cuentas SET saldo_bloqueado = saldo_bloqueado - $1, saldo_disponible = saldo_disponible + $1 WHERE id = $2",
            cuenta.saldo_bloqueado,
        )
    } else {
        (
            "UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1, saldo_bloqueado = saldo_bloqueado + $1 WHERE id = $2",
            cuenta.saldo_disponible,
        )
    };
    if disponible_requerido < monto {
        return Err(AppError::negocio("SALDO_INSUFICIENTE", "saldo insuficiente para la retención/liberación"));
    }
    sqlx::query(sql).bind(monto).bind(cuenta.id).execute(&mut *tx).await?;
    let registro = Uuid::new_v4();
    let clave = format!("{prefijo}:{registro}");
    let t = insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: Uuid::new_v4(),
            tipo,
            origen: Some(cuenta.id),
            destino: Some(cuenta.id),
            reserva: None,
            monto,
            concepto: Some(concepto),
            referencia_externa: None,
            clave: &clave,
            espejo: None,
        },
    )
    .await?;
    // El trigger diferido trg_ifpe_tx_zz_aml_clave exige este registro (misma
    // transacción, mismo monto, cuenta y concepto neutro) para la clave reservada.
    sqlx::query(
        "INSERT INTO ifpe.retenciones_operador (id, clave_idempotencia, tipo, cuenta_id, monto, motivo, transaccion_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(registro)
    .bind(&r.clave_idempotencia)
    .bind(tipo)
    .bind(cuenta.id)
    .bind(monto)
    .bind(&r.motivo)
    .bind(t.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Movimiento { transaccion: t, idempotente: false })
}

// -----------------------------------------------------------------------------
// Consultas
// -----------------------------------------------------------------------------

pub async fn cuenta_de_cliente(pool: &PgPool, cliente_id: Uuid) -> AppResult<Option<CuentaIfpe>> {
    let mut tx = db::begin_lectura(pool, Rol::Ifpe).await?;
    let cuenta = sqlx::query_as("SELECT * FROM ifpe.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'")
        .bind(cliente_id)
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(cuenta)
}

pub async fn movimientos_de_cliente(
    pool: &PgPool,
    cliente_id: Uuid,
    limite: i64,
    antes_de_secuencia: Option<i64>,
) -> AppResult<Vec<TransaccionIfpe>> {
    let mut tx = db::begin_lectura(pool, Rol::Ifpe).await?;
    let cuenta_id = id_cuenta_ifpe_de_cliente(&mut tx, cliente_id).await?;
    let movimientos = sqlx::query_as(
        "SELECT * FROM ifpe.transacciones \
         WHERE (cuenta_origen_id = $1 OR cuenta_destino_id = $1) AND secuencia < $2 \
         ORDER BY secuencia DESC LIMIT $3",
    )
    .bind(cuenta_id)
    .bind(antes_de_secuencia.unwrap_or(i64::MAX))
    .bind(limite.clamp(1, 200))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(movimientos)
}
