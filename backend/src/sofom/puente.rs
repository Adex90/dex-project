//! Puente IFPE -> SOFOM (rol `sc_puente`).
//!
//! **Inversión** ([`invertir`]): el cliente compromete saldo disponible de su
//! bóveda en un contrato de mutuo a plazo fijo. En una sola transacción:
//!
//! - IFPE: `saldo_disponible` y la reserva en custodia bajan por el mismo monto
//!   (el efectivo sale de la custodia de la IFPE hacia la caja de la SOFOM), y
//!   se registra `TRASPASO_A_SOFOM`. La reserva sigue siendo exactamente el 100%
//!   del pasivo restante.
//! - SOFOM: el monto entra a `saldo_bloqueado` de la cuenta de inversión y a la
//!   caja; se crea el contrato de mutuo con `hash_terminos` y se registra
//!   `APORTACION_MUTUO`. Ambos movimientos se referencian mutuamente.
//!
//! **Liquidación** ([`liquidar`]): al vencimiento, principal + intereses
//! regresan a la bóveda. No existe retiro anticipado (lo impide también un
//! trigger) y no se liquida un mutuo que aún fondea cartera viva.
//!
//! El rol `sc_puente` no tiene privilegios sobre `sofom.creditos` ni
//! `sofom.asignaciones_fondeo`: este módulo no puede originar préstamos.
//!
//! Orden de bloqueo (común a todo el sistema): cuenta IFPE → reserva → cuenta
//! SOFOM → caja → contrato → [estado_reserva → cabeza IFPE → cabeza SOFOM].
//! Por eso el movimiento IFPE se inserta siempre antes que los de la SOFOM.

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{bloquear_caja, insertar_tx_sofom, tasas, NuevaTxSofom};
use crate::{
    config::UmbralesPld,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    ledger::{self, hash, NuevaTxIfpe},
    models::{
        ifpe::{TipoTxIfpe, TransaccionIfpe},
        sofom::{ContratoMutuo, CuentaSofom, EstadoMutuo, TipoTxSofom, TransaccionSofom},
        Entidad, EstadoCuenta,
    },
    pld::{self, EventoPld},
};

pub const VERSION_CONDICIONES: &str = "SC-MUTUO-v1";
pub const CONVENCION: &str = "ACT/360";

/// Términos del contrato que el cliente acepta. `hash` = SHA-256 del payload
/// canónico de estos campos; se guarda en `contratos_mutuo.hash_terminos`.
#[derive(Debug, Clone, Serialize)]
pub struct TerminosMutuo {
    pub version: &'static str,
    pub cliente_id: Uuid,
    pub cuenta_sofom_id: Uuid,
    pub cuenta_ifpe_origen_id: Uuid,
    pub tx_aportacion_ifpe_id: Uuid,
    pub principal: String,
    pub tasa_anual: String,
    pub plazo_dias: i32,
    pub fecha_inicio: NaiveDate,
    pub fecha_vencimiento: NaiveDate,
    pub convencion: &'static str,
    pub retiro_anticipado: bool,
    pub hash: String,
}

fn tasa_canonica(tasa: Decimal) -> String {
    let mut t = tasa;
    t.rescale(6); // NUMERIC(9, 6)
    t.to_string()
}

pub fn fecha_vencimiento(fecha_inicio: NaiveDate, plazo_dias: i32) -> AppResult<NaiveDate> {
    u64::try_from(plazo_dias)
        .ok()
        .and_then(|d| fecha_inicio.checked_add_days(Days::new(d)))
        .ok_or_else(|| AppError::validacion("plazo inválido"))
}

#[allow(clippy::too_many_arguments)]
pub fn terminos(
    cliente_id: Uuid,
    cuenta_sofom_id: Uuid,
    cuenta_ifpe_origen_id: Uuid,
    tx_aportacion_ifpe_id: Uuid,
    principal: Decimal,
    tasa_anual: Decimal,
    plazo_dias: i32,
    fecha_inicio: NaiveDate,
) -> AppResult<TerminosMutuo> {
    let principal = dinero::canonico(principal);
    let tasa_anual = tasa_canonica(tasa_anual);
    let fecha_vencimiento = fecha_vencimiento(fecha_inicio, plazo_dias)?;
    let hash = hash::hash_campos(&[
        VERSION_CONDICIONES,
        &cliente_id.to_string(),
        &cuenta_sofom_id.to_string(),
        &cuenta_ifpe_origen_id.to_string(),
        &tx_aportacion_ifpe_id.to_string(),
        &principal,
        &tasa_anual,
        &plazo_dias.to_string(),
        &fecha_inicio.to_string(),
        &fecha_vencimiento.to_string(),
        CONVENCION,
        "SIN_RETIRO_ANTICIPADO",
    ]);
    Ok(TerminosMutuo {
        version: VERSION_CONDICIONES,
        cliente_id,
        cuenta_sofom_id,
        cuenta_ifpe_origen_id,
        tx_aportacion_ifpe_id,
        principal,
        tasa_anual,
        plazo_dias,
        fecha_inicio,
        fecha_vencimiento,
        convencion: CONVENCION,
        retiro_anticipado: false,
        hash,
    })
}

pub fn terminos_de(c: &ContratoMutuo) -> AppResult<TerminosMutuo> {
    terminos(
        c.cliente_id,
        c.cuenta_sofom_id,
        c.cuenta_ifpe_origen_id,
        c.tx_aportacion_ifpe_id,
        c.principal,
        c.tasa_anual,
        c.plazo_dias,
        c.fecha_inicio,
    )
}

// -----------------------------------------------------------------------------
// Inversión: IFPE -> contrato de mutuo
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SolicitudMutuo {
    pub cliente_id: Uuid,
    pub monto: Decimal,
    pub plazo_dias: i32,
    /// Clave ya acotada al cliente.
    pub clave_idempotencia: String,
    pub fecha_inicio: NaiveDate,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoMutuo {
    pub contrato: ContratoMutuo,
    pub terminos: TerminosMutuo,
    pub interes_al_vencimiento: Decimal,
    pub tx_ifpe: TransaccionIfpe,
    pub tx_sofom: TransaccionSofom,
    pub idempotente: bool,
}

pub async fn invertir(pool: &PgPool, umbrales: &UmbralesPld, s: &SolicitudMutuo) -> AppResult<ResultadoMutuo> {
    let monto = dinero::validar_monto(s.monto)?;
    let tasa = tasas::tasa_para(s.plazo_dias).ok_or_else(|| {
        AppError::validacion(format!("plazo de {} días no admitido; consulta GET /api/v1/sofom/tasas", s.plazo_dias))
    })?;
    db::con_reintentos(|| invertir_tx(pool, umbrales, s, monto, tasa)).await
}

async fn invertir_tx(
    pool: &PgPool,
    umbrales: &UmbralesPld,
    s: &SolicitudMutuo,
    monto: Decimal,
    tasa: Decimal,
) -> AppResult<ResultadoMutuo> {
    let mut tx = db::begin(pool, Rol::Puente).await?;

    // 1. Cuenta IFPE de origen (primer lock).
    let cuenta_ifpe_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    let cuenta_ifpe = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_ifpe_id).await?;

    // 2. Idempotencia (después del lock: una petición repetida espera y ve la primera).
    if let Some(previa) = ledger::buscar_por_clave(&mut tx, &s.clave_idempotencia).await? {
        ledger::exigir_misma_operacion(&previa, TipoTxIfpe::TraspasoASofom, monto, Some(cuenta_ifpe.id), None)?;
        return reconstruir_inversion(&mut tx, previa, s.plazo_dias).await;
    }

    // 3. Reglas de negocio.
    let perfil = pld::cargar_perfil(&mut tx, s.cliente_id).await?;
    perfil.exigir_operable()?;
    ledger::exigir_debitable(&cuenta_ifpe)?;
    ledger::exigir_saldo(&cuenta_ifpe, monto)?;

    // 4. Resto de locks en orden global.
    let reserva = ledger::bloquear_reserva(&mut tx).await?;
    sqlx::query("INSERT INTO sofom.cuentas (cliente_id) VALUES ($1) ON CONFLICT (cliente_id, moneda) DO NOTHING")
        .bind(s.cliente_id)
        .execute(&mut *tx)
        .await?;
    let cuenta_sofom: CuentaSofom =
        sqlx::query_as("SELECT * FROM sofom.cuentas WHERE cliente_id = $1 AND moneda = 'MXN' FOR UPDATE")
            .bind(s.cliente_id)
            .fetch_one(&mut *tx)
            .await?;
    if cuenta_sofom.estado != EstadoCuenta::Activa {
        return Err(AppError::negocio("CUENTA_SOFOM_NO_ACTIVA", "la cuenta de inversión no está activa"));
    }
    let caja = bloquear_caja(&mut tx).await?;

    let tx_ifpe_id = Uuid::new_v4();
    let tx_sofom_id = Uuid::new_v4();
    let contrato_id = Uuid::new_v4();
    let concepto = format!("Aportación a contrato de mutuo a {} días", s.plazo_dias);

    // 5. IFPE: el pasivo y la reserva bajan juntos; el efectivo sale a la SOFOM.
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1 WHERE id = $2")
        .bind(monto)
        .bind(cuenta_ifpe.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo - $1 WHERE id = $2")
        .bind(monto)
        .bind(reserva.id)
        .execute(&mut *tx)
        .await?;
    let tx_ifpe = ledger::insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: tx_ifpe_id,
            tipo: TipoTxIfpe::TraspasoASofom,
            origen: Some(cuenta_ifpe.id),
            destino: None,
            reserva: Some(reserva.id),
            monto,
            concepto: Some(&concepto),
            referencia_externa: None,
            clave: &s.clave_idempotencia,
            espejo: Some(tx_sofom_id),
        },
    )
    .await?;

    // 6. SOFOM: saldo bloqueado a plazo + efectivo en caja + contrato.
    sqlx::query("UPDATE sofom.cuentas SET saldo_bloqueado = saldo_bloqueado + $1 WHERE id = $2")
        .bind(monto)
        .bind(cuenta_sofom.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sofom.caja SET saldo = saldo + $1 WHERE id = $2")
        .bind(monto)
        .bind(caja.id)
        .execute(&mut *tx)
        .await?;
    let terminos =
        terminos(s.cliente_id, cuenta_sofom.id, cuenta_ifpe.id, tx_ifpe_id, monto, tasa, s.plazo_dias, s.fecha_inicio)?;
    let contrato: ContratoMutuo = sqlx::query_as(
        "INSERT INTO sofom.contratos_mutuo \
         (id, cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id, principal, tasa_anual, \
          plazo_dias, fecha_inicio, hash_terminos) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *",
    )
    .bind(contrato_id)
    .bind(s.cliente_id)
    .bind(cuenta_sofom.id)
    .bind(cuenta_ifpe.id)
    .bind(tx_ifpe_id)
    .bind(monto)
    .bind(tasa)
    .bind(s.plazo_dias)
    .bind(s.fecha_inicio)
    .bind(&terminos.hash)
    .fetch_one(&mut *tx)
    .await?;
    if contrato.fecha_vencimiento != terminos.fecha_vencimiento || terminos_de(&contrato)?.hash != terminos.hash {
        return Err(AppError::Integridad("los términos almacenados no coinciden con los aceptados".into()));
    }
    let tx_sofom = insertar_tx_sofom(
        &mut tx,
        NuevaTxSofom {
            id: tx_sofom_id,
            tipo: TipoTxSofom::AportacionMutuo,
            cuenta: Some(cuenta_sofom.id),
            contrato: Some(contrato.id),
            credito: None,
            caja: Some(caja.id),
            monto,
            concepto: Some(&concepto),
            clave: &s.clave_idempotencia,
            espejo: Some(tx_ifpe.id),
        },
    )
    .await?;

    let evento = EventoPld {
        entidad: Entidad::Sofom,
        cliente: &perfil,
        transaccion_id: tx_sofom.id,
        operacion: "APORTACION_MUTUO",
        monto,
        cuenta_deposito: None,
    };
    pld::evaluar(&mut tx, &evento, umbrales).await?;
    tx.commit().await?;

    Ok(ResultadoMutuo {
        interes_al_vencimiento: tasas::interes(contrato.principal, contrato.tasa_anual, contrato.plazo_dias),
        contrato,
        terminos,
        tx_ifpe,
        tx_sofom,
        idempotente: false,
    })
}

async fn reconstruir_inversion(tx: &mut Tx, previa: TransaccionIfpe, plazo_dias: i32) -> AppResult<ResultadoMutuo> {
    let contrato: ContratoMutuo =
        sqlx::query_as("SELECT * FROM sofom.contratos_mutuo WHERE tx_aportacion_ifpe_id = $1")
            .bind(previa.id)
            .fetch_one(&mut **tx)
            .await?;
    if contrato.plazo_dias != plazo_dias {
        return Err(AppError::conflicto(
            "CLAVE_IDEMPOTENCIA_REUTILIZADA",
            "la clave de idempotencia ya se usó para un mutuo con otro plazo",
        ));
    }
    let tx_sofom: TransaccionSofom = sqlx::query_as("SELECT * FROM sofom.transacciones WHERE id = $1")
        .bind(previa.ref_movimiento_espejo)
        .fetch_one(&mut **tx)
        .await?;
    Ok(ResultadoMutuo {
        terminos: terminos_de(&contrato)?,
        interes_al_vencimiento: tasas::interes(contrato.principal, contrato.tasa_anual, contrato.plazo_dias),
        contrato,
        tx_ifpe: previa,
        tx_sofom,
        idempotente: true,
    })
}

// -----------------------------------------------------------------------------
// Liquidación al vencimiento: mutuo -> IFPE
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoLiquidacion {
    pub contrato: ContratoMutuo,
    pub interes: Decimal,
    pub total_retornado: Decimal,
    pub tx_ifpe: TransaccionIfpe,
    pub movimientos_sofom: Vec<TransaccionSofom>,
    pub idempotente: bool,
}

fn clave_liquidacion(contrato_id: Uuid, parte: &str) -> String {
    format!("sistema:liquidacion:{contrato_id}:{parte}")
}

/// Liquida un mutuo vencido. `solicitante = Some(cliente)` exige que el
/// contrato sea suyo; `None` lo ejecuta el sistema u operador.
pub async fn liquidar(
    pool: &PgPool,
    contrato_id: Uuid,
    solicitante: Option<Uuid>,
    hoy: NaiveDate,
) -> AppResult<ResultadoLiquidacion> {
    db::con_reintentos(|| liquidar_tx(pool, contrato_id, solicitante, hoy)).await
}

async fn liquidar_tx(
    pool: &PgPool,
    contrato_id: Uuid,
    solicitante: Option<Uuid>,
    hoy: NaiveDate,
) -> AppResult<ResultadoLiquidacion> {
    let mut tx = db::begin(pool, Rol::Puente).await?;
    let previo: Option<ContratoMutuo> = sqlx::query_as("SELECT * FROM sofom.contratos_mutuo WHERE id = $1")
        .bind(contrato_id)
        .fetch_optional(&mut *tx)
        .await?;
    let previo = previo
        .filter(|c| solicitante.map_or(true, |s| s == c.cliente_id))
        .ok_or_else(|| AppError::no_encontrado("contrato de mutuo no encontrado"))?;

    let clave_ifpe = clave_liquidacion(contrato_id, "ifpe");
    let cuenta_ifpe = ledger::bloquear_cuenta_ifpe(&mut tx, previo.cuenta_ifpe_origen_id).await?;
    if let Some(previa) = ledger::buscar_por_clave(&mut tx, &clave_ifpe).await? {
        return reconstruir_liquidacion(&mut tx, contrato_id, previa).await;
    }
    let reserva = ledger::bloquear_reserva(&mut tx).await?;
    let cuenta_sofom: CuentaSofom = sqlx::query_as("SELECT * FROM sofom.cuentas WHERE id = $1 FOR UPDATE")
        .bind(previo.cuenta_sofom_id)
        .fetch_one(&mut *tx)
        .await?;
    let caja = bloquear_caja(&mut tx).await?;
    let contrato: ContratoMutuo = sqlx::query_as("SELECT * FROM sofom.contratos_mutuo WHERE id = $1 FOR UPDATE")
        .bind(contrato_id)
        .fetch_one(&mut *tx)
        .await?;

    if contrato.estado != EstadoMutuo::Vigente {
        return Err(AppError::negocio("MUTUO_NO_VIGENTE", format!("el contrato está {:?}", contrato.estado)));
    }
    if hoy < contrato.fecha_vencimiento {
        return Err(AppError::negocio(
            "MUTUO_NO_VENCIDO",
            format!("el contrato vence el {}; el mutuo no admite retiro anticipado", contrato.fecha_vencimiento),
        ));
    }
    if contrato.principal_asignado > Decimal::ZERO {
        return Err(AppError::negocio(
            "MUTUO_CON_CARTERA_VIGENTE",
            format!("{} del principal sigue fondeando créditos no liquidados", contrato.principal_asignado),
        ));
    }
    ledger::exigir_acreditable(&cuenta_ifpe)?;

    let interes = tasas::interes(contrato.principal, contrato.tasa_anual, contrato.plazo_dias);
    let total = contrato.principal + interes;
    if caja.saldo < total {
        return Err(AppError::negocio(
            "LIQUIDEZ_SOFOM_INSUFICIENTE",
            "la caja de la SOFOM no cubre principal más intereses; se requiere aportación de capital",
        ));
    }
    let tx_ifpe_id = Uuid::new_v4();
    let tx_retorno_id = Uuid::new_v4();
    let concepto = format!("Liquidación de mutuo folio {}", contrato.folio);

    // IFPE primero (orden de locks): el efectivo regresa a la custodia, 1:1 con el pasivo.
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $1 WHERE id = $2")
        .bind(total)
        .bind(cuenta_ifpe.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo + $1 WHERE id = $2")
        .bind(total)
        .bind(reserva.id)
        .execute(&mut *tx)
        .await?;
    let tx_ifpe = ledger::insertar_tx_ifpe(
        &mut tx,
        NuevaTxIfpe {
            id: tx_ifpe_id,
            tipo: TipoTxIfpe::RetornoDeSofom,
            origen: None,
            destino: Some(cuenta_ifpe.id),
            reserva: Some(reserva.id),
            monto: total,
            concepto: Some(&concepto),
            referencia_externa: None,
            clave: &clave_ifpe,
            espejo: Some(tx_retorno_id),
        },
    )
    .await?;

    // SOFOM: vencimiento, interés, retorno del efectivo y cierre del contrato.
    sqlx::query("UPDATE sofom.contratos_mutuo SET estado = 'VENCIDO', interes_pagado = $1 WHERE id = $2")
        .bind(interes)
        .bind(contrato.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE sofom.cuentas SET saldo_bloqueado = saldo_bloqueado - $1, saldo_disponible = saldo_disponible + $1 WHERE id = $2",
    )
    .bind(contrato.principal)
    .bind(cuenta_sofom.id)
    .execute(&mut *tx)
    .await?;
    let mut movimientos = vec![
        insertar_tx_sofom(
            &mut tx,
            NuevaTxSofom {
                id: Uuid::new_v4(),
                tipo: TipoTxSofom::VencimientoMutuo,
                cuenta: Some(cuenta_sofom.id),
                contrato: Some(contrato.id),
                credito: None,
                caja: None,
                monto: contrato.principal,
                concepto: Some(&concepto),
                clave: &clave_liquidacion(contrato_id, "vencimiento"),
                espejo: None,
            },
        )
        .await?,
    ];
    if interes > Decimal::ZERO {
        sqlx::query("UPDATE sofom.cuentas SET saldo_disponible = saldo_disponible + $1 WHERE id = $2")
            .bind(interes)
            .bind(cuenta_sofom.id)
            .execute(&mut *tx)
            .await?;
        movimientos.push(
            insertar_tx_sofom(
                &mut tx,
                NuevaTxSofom {
                    id: Uuid::new_v4(),
                    tipo: TipoTxSofom::PagoInteresMutuo,
                    cuenta: Some(cuenta_sofom.id),
                    contrato: Some(contrato.id),
                    credito: None,
                    caja: Some(caja.id),
                    monto: interes,
                    concepto: Some(&concepto),
                    clave: &clave_liquidacion(contrato_id, "interes"),
                    espejo: None,
                },
            )
            .await?,
        );
    }
    sqlx::query("UPDATE sofom.caja SET saldo = saldo - $1 WHERE id = $2")
        .bind(total)
        .bind(caja.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sofom.cuentas SET saldo_disponible = saldo_disponible - $1 WHERE id = $2")
        .bind(total)
        .bind(cuenta_sofom.id)
        .execute(&mut *tx)
        .await?;
    movimientos.push(
        insertar_tx_sofom(
            &mut tx,
            NuevaTxSofom {
                id: tx_retorno_id,
                tipo: TipoTxSofom::RetornoAIfpe,
                cuenta: Some(cuenta_sofom.id),
                contrato: Some(contrato.id),
                credito: None,
                caja: Some(caja.id),
                monto: total,
                concepto: Some(&concepto),
                clave: &clave_liquidacion(contrato_id, "retorno"),
                espejo: Some(tx_ifpe.id),
            },
        )
        .await?,
    );
    let contrato: ContratoMutuo =
        sqlx::query_as("UPDATE sofom.contratos_mutuo SET estado = 'LIQUIDADO' WHERE id = $1 RETURNING *")
            .bind(contrato.id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    tracing::info!(contrato = %contrato.id, folio = contrato.folio, %total, "mutuo liquidado y retornado a la IFPE");
    Ok(ResultadoLiquidacion {
        contrato,
        interes,
        total_retornado: total,
        tx_ifpe,
        movimientos_sofom: movimientos,
        idempotente: false,
    })
}

async fn reconstruir_liquidacion(
    tx: &mut Tx,
    contrato_id: Uuid,
    previa: TransaccionIfpe,
) -> AppResult<ResultadoLiquidacion> {
    let contrato: ContratoMutuo = sqlx::query_as("SELECT * FROM sofom.contratos_mutuo WHERE id = $1")
        .bind(contrato_id)
        .fetch_one(&mut **tx)
        .await?;
    let movimientos: Vec<TransaccionSofom> = sqlx::query_as(
        "SELECT * FROM sofom.transacciones WHERE contrato_id = $1 AND clave_idempotencia LIKE $2 ORDER BY secuencia",
    )
    .bind(contrato_id)
    .bind(format!("sistema:liquidacion:{contrato_id}:%"))
    .fetch_all(&mut **tx)
    .await?;
    Ok(ResultadoLiquidacion {
        interes: contrato.interes_pagado,
        total_retornado: previa.monto,
        contrato,
        tx_ifpe: previa,
        movimientos_sofom: movimientos,
        idempotente: true,
    })
}

/// Contratos vigentes ya vencidos y sin cartera viva (candidatos a liquidación automática).
pub async fn contratos_por_liquidar(pool: &PgPool, hoy: NaiveDate, limite: i64) -> AppResult<Vec<Uuid>> {
    let mut tx = db::begin_lectura(pool, Rol::Puente).await?;
    let ids = sqlx::query_scalar(
        "SELECT id FROM sofom.contratos_mutuo \
         WHERE estado = 'VIGENTE' AND fecha_vencimiento <= $1 AND principal_asignado = 0 \
         ORDER BY fecha_vencimiento, folio LIMIT $2",
    )
    .bind(hoy)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ids)
}
