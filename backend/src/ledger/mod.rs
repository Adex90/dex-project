//! Motor transaccional de la IFPE (bóveda a la vista, reserva del 100%).
//!
//! Cada operación:
//! 1. abre una transacción con el rol `sc_ifpe` ([`db::begin`]);
//! 2. bloquea las filas involucradas con `SELECT ... FOR UPDATE` en un orden
//!    global fijo (cuentas por id → reserva → [estado_reserva y cabeza del
//!    ledger, que toman los triggers]) para evitar deadlocks;
//! 3. comprueba la idempotencia *después* de bloquear, de modo que dos
//!    peticiones con la misma clave se serializan y la segunda ve la primera;
//! 4. mueve saldos y, si entra o sale efectivo, la reserva en custodia por el
//!    mismo monto (si no, el trigger diferido `SC_RESERVA_100` aborta el COMMIT);
//! 5. inserta el movimiento en el ledger y verifica que el `tx_hash` que
//!    calculó la base de datos coincide con el recalculado en Rust;
//! 6. evalúa reglas PLD y hace COMMIT. Cualquier error antes del COMMIT
//!    descarta la transacción completa (ROLLBACK al soltar `Tx`).

pub mod hash;

use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    clabe,
    config::UmbralesPld,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    models::{
        ifpe::{CuentaIfpe, ReservaCustodia, TipoTxIfpe, TransaccionIfpe},
        Entidad, EstadoCuenta,
    },
    pld::{self, EventoPld},
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
    sqlx::query_as("SELECT * FROM ifpe.cuentas WHERE id = $1 FOR UPDATE")
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
    sqlx::query_as("SELECT * FROM ifpe.reserva_custodia WHERE activa ORDER BY creado_en, id LIMIT 1 FOR UPDATE")
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
pub(crate) fn exigir_misma_operacion(
    previa: &TransaccionIfpe,
    tipo: TipoTxIfpe,
    monto: Decimal,
    origen: Option<Uuid>,
    destino: Option<Uuid>,
) -> AppResult<()> {
    if previa.tipo != tipo
        || previa.monto != monto
        || previa.cuenta_origen_id != origen
        || previa.cuenta_destino_id != destino
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

fn validar_texto(campo: &str, valor: Option<&str>, max: usize) -> AppResult<()> {
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

// -----------------------------------------------------------------------------
// Depósito (entrada de efectivo: SPEI entrante, ventanilla)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Deposito {
    pub cuenta_id: Uuid,
    pub monto: Decimal,
    pub referencia_externa: Option<String>,
    pub concepto: Option<String>,
    /// Clave ya acotada por quien la origina (p. ej. `op:...` o `cli:<id>:...`).
    pub clave_idempotencia: String,
}

pub async fn depositar(pool: &PgPool, umbrales: &UmbralesPld, d: &Deposito) -> AppResult<Movimiento> {
    let monto = dinero::validar_monto(d.monto)?;
    validar_texto("concepto", d.concepto.as_deref(), 140)?;
    validar_texto("referencia_externa", d.referencia_externa.as_deref(), 64)?;
    db::con_reintentos(|| depositar_tx(pool, umbrales, d, monto)).await
}

async fn depositar_tx(pool: &PgPool, umbrales: &UmbralesPld, d: &Deposito, monto: Decimal) -> AppResult<Movimiento> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let cuenta = bloquear_cuenta_ifpe(&mut tx, d.cuenta_id).await?;
    if let Some(previa) = buscar_por_clave(&mut tx, &d.clave_idempotencia).await? {
        exigir_misma_operacion(&previa, TipoTxIfpe::Deposito, monto, None, Some(cuenta.id))?;
        return Ok(Movimiento { transaccion: previa, idempotente: true });
    }
    let perfil = pld::cargar_perfil(&mut tx, cuenta.cliente_id).await?;
    perfil.exigir_operable()?;
    exigir_acreditable(&cuenta)?;
    let reserva = bloquear_reserva(&mut tx).await?;

    // Pasivo y reserva suben juntos: 1 peso depositado = 1 peso en custodia.
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $1 WHERE id = $2")
        .bind(monto)
        .bind(cuenta.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo + $1 WHERE id = $2")
        .bind(monto)
        .bind(reserva.id)
        .execute(&mut *tx)
        .await?;
    let t = insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: Uuid::new_v4(),
            tipo: TipoTxIfpe::Deposito,
            origen: None,
            destino: Some(cuenta.id),
            reserva: Some(reserva.id),
            monto,
            concepto: d.concepto.as_deref(),
            referencia_externa: d.referencia_externa.as_deref(),
            clave: &d.clave_idempotencia,
            espejo: None,
        },
    )
    .await?;
    let evento = EventoPld {
        entidad: Entidad::Ifpe,
        cliente: &perfil,
        transaccion_id: t.id,
        operacion: "DEPOSITO",
        monto,
        cuenta_deposito: Some(cuenta.id),
    };
    pld::evaluar(&mut tx, &evento, umbrales).await?;
    tx.commit().await?;
    Ok(Movimiento { transaccion: t, idempotente: false })
}

// -----------------------------------------------------------------------------
// Retiro (salida de efectivo: SPEI saliente a una CLABE externa)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Retiro {
    pub cliente_id: Uuid,
    pub monto: Decimal,
    pub clabe_destino: String,
    pub concepto: Option<String>,
    pub clave_idempotencia: String,
}

pub async fn retirar(pool: &PgPool, umbrales: &UmbralesPld, r: &Retiro) -> AppResult<Movimiento> {
    let monto = dinero::validar_monto(r.monto)?;
    validar_texto("concepto", r.concepto.as_deref(), 140)?;
    if !clabe::es_valida(&r.clabe_destino) {
        return Err(AppError::validacion("CLABE destino inválida"));
    }
    db::con_reintentos(|| retirar_tx(pool, umbrales, r, monto)).await
}

async fn retirar_tx(pool: &PgPool, umbrales: &UmbralesPld, r: &Retiro, monto: Decimal) -> AppResult<Movimiento> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let cuenta_id = id_cuenta_ifpe_de_cliente(&mut tx, r.cliente_id).await?;
    let cuenta = bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    if let Some(previa) = buscar_por_clave(&mut tx, &r.clave_idempotencia).await? {
        exigir_misma_operacion(&previa, TipoTxIfpe::Retiro, monto, Some(cuenta.id), None)?;
        return Ok(Movimiento { transaccion: previa, idempotente: true });
    }
    let perfil = pld::cargar_perfil(&mut tx, cuenta.cliente_id).await?;
    perfil.exigir_operable()?;
    exigir_debitable(&cuenta)?;
    exigir_saldo(&cuenta, monto)?;
    let reserva = bloquear_reserva(&mut tx).await?;

    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1 WHERE id = $2")
        .bind(monto)
        .bind(cuenta.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo - $1 WHERE id = $2")
        .bind(monto)
        .bind(reserva.id)
        .execute(&mut *tx)
        .await?;
    let t = insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: Uuid::new_v4(),
            tipo: TipoTxIfpe::Retiro,
            origen: Some(cuenta.id),
            destino: None,
            reserva: Some(reserva.id),
            monto,
            concepto: r.concepto.as_deref(),
            referencia_externa: Some(&r.clabe_destino),
            clave: &r.clave_idempotencia,
            espejo: None,
        },
    )
    .await?;
    let evento = EventoPld {
        entidad: Entidad::Ifpe,
        cliente: &perfil,
        transaccion_id: t.id,
        operacion: "RETIRO",
        monto,
        cuenta_deposito: None,
    };
    pld::evaluar(&mut tx, &evento, umbrales).await?;
    tx.commit().await?;
    Ok(Movimiento { transaccion: t, idempotente: false })
}

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

pub async fn transferir(pool: &PgPool, umbrales: &UmbralesPld, t: &Transferencia) -> AppResult<Movimiento> {
    let monto = dinero::validar_monto(t.monto)?;
    validar_texto("concepto", t.concepto.as_deref(), 140)?;
    if !clabe::es_valida(&t.clabe_destino) {
        return Err(AppError::validacion("CLABE destino inválida"));
    }
    db::con_reintentos(|| transferir_tx(pool, umbrales, t, monto)).await
}

async fn transferir_tx(
    pool: &PgPool,
    umbrales: &UmbralesPld,
    t: &Transferencia,
    monto: Decimal,
) -> AppResult<Movimiento> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let origen_id = id_cuenta_ifpe_de_cliente(&mut tx, t.cliente_origen_id).await?;
    let destino_id: Uuid = sqlx::query_scalar("SELECT id FROM ifpe.cuentas WHERE clabe = $1")
        .bind(&t.clabe_destino)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("la CLABE destino no corresponde a una cuenta de Synthetic Capital"))?;
    if origen_id == destino_id {
        return Err(AppError::validacion("no puedes transferir a tu propia cuenta"));
    }

    // Orden global de bloqueo: por id ascendente.
    let (primero, segundo) = if origen_id < destino_id { (origen_id, destino_id) } else { (destino_id, origen_id) };
    let a = bloquear_cuenta_ifpe(&mut tx, primero).await?;
    let b = bloquear_cuenta_ifpe(&mut tx, segundo).await?;
    let (origen, destino) = if a.id == origen_id { (a, b) } else { (b, a) };

    if let Some(previa) = buscar_por_clave(&mut tx, &t.clave_idempotencia).await? {
        exigir_misma_operacion(&previa, TipoTxIfpe::Transferencia, monto, Some(origen.id), Some(destino.id))?;
        return Ok(Movimiento { transaccion: previa, idempotente: true });
    }
    let perfil = pld::cargar_perfil(&mut tx, origen.cliente_id).await?;
    perfil.exigir_operable()?;
    let perfil_destino = pld::cargar_perfil(&mut tx, destino.cliente_id).await?;
    if perfil_destino.en_lista_bloqueadas {
        return Err(AppError::negocio(
            "DESTINO_EN_LISTA_BLOQUEADAS",
            "el beneficiario está en la Lista de Personas Bloqueadas",
        ));
    }
    exigir_debitable(&origen)?;
    exigir_acreditable(&destino)?;
    exigir_saldo(&origen, monto)?;

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
            id: Uuid::new_v4(),
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
    let evento = EventoPld {
        entidad: Entidad::Ifpe,
        cliente: &perfil,
        transaccion_id: mov.id,
        operacion: "TRANSFERENCIA",
        monto,
        cuenta_deposito: None,
    };
    pld::evaluar(&mut tx, &evento, umbrales).await?;
    tx.commit().await?;
    Ok(Movimiento { transaccion: mov, idempotente: false })
}

// -----------------------------------------------------------------------------
// Retención / liberación (cumplimiento). El dinero retenido sigue respaldado.
// -----------------------------------------------------------------------------

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
    let tipo = if r.liberar { TipoTxIfpe::LiberacionRetencion } else { TipoTxIfpe::Retencion };
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let cuenta = bloquear_cuenta_ifpe(&mut tx, r.cuenta_id).await?;
    if let Some(previa) = buscar_por_clave(&mut tx, &r.clave_idempotencia).await? {
        exigir_misma_operacion(&previa, tipo, monto, Some(cuenta.id), Some(cuenta.id))?;
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
    let t = insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: Uuid::new_v4(),
            tipo,
            origen: Some(cuenta.id),
            destino: Some(cuenta.id),
            reserva: None,
            monto,
            concepto: Some(&r.motivo),
            referencia_externa: None,
            clave: &r.clave_idempotencia,
            espejo: None,
        },
    )
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
