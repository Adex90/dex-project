//! Rutas de operador: SPEI entrante, retenciones, KYC, PLD y crédito.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::status_de;
use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    kyc::{self, DictamenKyc},
    ledger::{self, Movimiento},
    models::{
        kyc::{AlertaPld, Cliente},
        sofom::{Credito, TransaccionSofom},
    },
    sofom::credito::{self, ResultadoCastigo, ResultadoCredito, ResultadoPago},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/operaciones/depositos", post(deposito))
        .route("/operaciones/retenciones", post(retencion))
        .route("/operaciones/kyc/{cliente_id}", post(dictamen_kyc))
        .route("/operaciones/alertas", get(alertas))
        .route("/operaciones/creditos", get(listar_creditos).post(originar_credito))
        .route("/operaciones/creditos/{id}/pagos", post(pagar_credito))
        .route("/operaciones/creditos/{id}/castigo", post(castigar_credito))
        .route("/operaciones/usuarios/{id}/revocar-sesiones", post(revocar_sesiones))
        .route("/operaciones/capital", post(aportar_capital))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudDeposito {
    clabe: String,
    monto: Decimal,
    /// Clave del participante SPEI que envía el pago (3 a 5 dígitos).
    banco_emisor: String,
    /// Día de operación SPEI (AAAA-MM-DD).
    fecha_operacion: chrono::NaiveDate,
    /// Clave de rastreo: junto con emisor y fecha identifica el pago y hace
    /// la operación idempotente.
    clave_rastreo: String,
    concepto: Option<String>,
}

/// SPEI entrante: acredita a la cuenta y sube la reserva en custodia 1:1.
async fn deposito(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudDeposito>,
) -> AppResult<(StatusCode, Json<Movimiento>)> {
    u.exigir_operador()?;
    let cuenta_id = cuenta_por_clabe(&e, &s.clabe).await?;
    let spei = ledger::IdentidadSpei {
        banco_emisor: s.banco_emisor,
        fecha_operacion: s.fecha_operacion,
        clave_rastreo: s.clave_rastreo,
    };
    let d = ledger::Deposito::spei(cuenta_id, s.monto, &spei, s.concepto)?;
    let m = ledger::depositar(&e.pool, &e.config.pld, &d).await?;
    Ok((status_de(m.idempotente), Json(m)))
}

async fn cuenta_por_clabe(e: &AppState, clabe: &str) -> AppResult<Uuid> {
    if !crate::clabe::es_valida(clabe) {
        return Err(AppError::validacion("CLABE inválida"));
    }
    let mut tx = crate::db::begin_lectura(&e.pool, crate::db::Rol::Ifpe).await?;
    let id = sqlx::query_scalar("SELECT id FROM ifpe.cuentas WHERE clabe = $1")
        .bind(clabe)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("CLABE no encontrada"))?;
    tx.commit().await?;
    Ok(id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudRetencion {
    cuenta_id: Uuid,
    monto: Decimal,
    motivo: String,
    #[serde(default)]
    liberar: bool,
    clave_idempotencia: String,
}

async fn retencion(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudRetencion>,
) -> AppResult<(StatusCode, Json<Movimiento>)> {
    u.exigir_operador()?;
    let r = ledger::Retencion {
        cuenta_id: s.cuenta_id,
        monto: s.monto,
        motivo: s.motivo,
        liberar: s.liberar,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let m = ledger::retener(&e.pool, &r).await?;
    Ok((status_de(m.idempotente), Json(m)))
}

async fn dictamen_kyc(
    State(e): State<AppState>,
    u: Usuario,
    Path(cliente_id): Path<Uuid>,
    Json(d): Json<DictamenKyc>,
) -> AppResult<Json<Cliente>> {
    u.exigir_operador()?;
    Ok(Json(kyc::dictaminar(&e.pool, cliente_id, &d).await?))
}

#[derive(Deserialize)]
struct Limite {
    limite: Option<i64>,
}

async fn alertas(State(e): State<AppState>, u: Usuario, Query(q): Query<Limite>) -> AppResult<Json<Vec<AlertaPld>>> {
    u.exigir_operador()?;
    Ok(Json(kyc::alertas_abiertas(&e.pool, q.limite.unwrap_or(100)).await?))
}

async fn listar_creditos(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<Limite>,
) -> AppResult<Json<Vec<Credito>>> {
    u.exigir_operador()?;
    Ok(Json(credito::listar_creditos(&e.pool, q.limite.unwrap_or(100)).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudCreditoHttp {
    acreditado_id: Uuid,
    principal: Decimal,
    tasa_anual: Decimal,
    plazo_dias: i32,
    destino: Option<String>,
    clabe_destino: String,
    clave_idempotencia: String,
}

async fn originar_credito(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudCreditoHttp>,
) -> AppResult<(StatusCode, Json<ResultadoCredito>)> {
    u.exigir_operador()?;
    let solicitud = credito::SolicitudCredito {
        acreditado_id: s.acreditado_id,
        principal: s.principal,
        tasa_anual: s.tasa_anual,
        plazo_dias: s.plazo_dias,
        destino: s.destino,
        clabe_destino: s.clabe_destino,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let r = credito::originar(&e.pool, &e.config.pld, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudPago {
    principal: Decimal,
    interes: Decimal,
    clave_idempotencia: String,
}

async fn pagar_credito(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudPago>,
) -> AppResult<(StatusCode, Json<ResultadoPago>)> {
    u.exigir_operador()?;
    let p = credito::PagoCredito {
        credito_id: id,
        principal: s.principal,
        interes: s.interes,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let r = credito::pagar(&e.pool, &p).await?;
    Ok((status_de(r.idempotente), Json(r)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudCapital {
    monto: Decimal,
    referencia: Option<String>,
    clave_idempotencia: String,
}

#[derive(Serialize)]
struct RespuestaCapital {
    transaccion: TransaccionSofom,
    idempotente: bool,
}

async fn aportar_capital(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudCapital>,
) -> AppResult<(StatusCode, Json<RespuestaCapital>)> {
    u.exigir_operador()?;
    let (transaccion, idempotente) =
        credito::aportar_capital(&e.pool, s.monto, s.referencia, u.clave(&s.clave_idempotencia)?).await?;
    Ok((status_de(idempotente), Json(RespuestaCapital { transaccion, idempotente })))
}

/// Castigo de un crédito vencido: la pérdida la absorbe el capital de la SOFOM.
async fn castigar_credito(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ResultadoCastigo>> {
    u.exigir_operador()?;
    Ok(Json(credito::castigar(&e.pool, id).await?))
}

/// Invalida de inmediato todos los JWT de una persona (baja de un operador,
/// credenciales comprometidas, etc.).
async fn revocar_sesiones(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<StatusCode> {
    u.exigir_operador()?;
    crate::auth::revocar_sesiones(&e.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
