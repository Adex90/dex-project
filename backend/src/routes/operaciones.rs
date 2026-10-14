//! Rutas de operador: SPEI entrante, retenciones, KYC, PLD y crédito.

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::status_de;
use crate::{
    auth::Usuario,
    error::AppResult,
    kyc::{self, DictamenKyc},
    ledger::{self, Movimiento},
    models::{
        kyc::{AlertaPld, Cliente},
        sofom::{Credito, TransaccionSofom},
    },
    routes::extraccion::{Json, Path, Query},
    sofom::credito::{self, ResultadoCastigo, ResultadoCredito, ResultadoPago},
    spei::{
        inbound::{self, Origen, ResultadoAbono, ResultadoCorreccion, SiNoAcreditable, SolicitudCorreccion},
        IdentidadSpei,
    },
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/operaciones/spei/recibidos/{id}/correccion", post(corregir_spei))
        .route("/operaciones/depositos", post(deposito))
        .route("/operaciones/retenciones", post(retencion))
        .route("/operaciones/kyc/pendientes", get(kyc_pendientes))
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
    #[serde(with = "rust_decimal::serde::str")]
    monto: Decimal,
    /// Clave de participante SPEI del emisor (5 dígitos, p. ej. 40012).
    banco_emisor: String,
    /// Día de operación SPEI (AAAA-MM-DD).
    fecha_operacion: chrono::NaiveDate,
    /// Clave de rastreo: junto con emisor y fecha identifica el pago y hace
    /// la operación idempotente.
    clave_rastreo: String,
    concepto: Option<String>,
}

/// SPEI entrante capturado por el operador (conciliación manual). Pasa por el
/// mismo proceso que el webhook del proveedor: misma identidad de pago, así
/// que un pago capturado a mano y notificado por el proveedor se abona una
/// sola vez. Si la cuenta no puede recibir, se rechaza la captura (el operador
/// decide la devolución).
async fn deposito(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudDeposito>,
) -> AppResult<(StatusCode, Json<ResultadoAbono>)> {
    u.exigir_operador()?;
    let raw_payload = serde_json::json!({
        "clabe": s.clabe,
        "monto": s.monto.to_string(),
        "banco_emisor": s.banco_emisor,
        "fecha_operacion": s.fecha_operacion,
        "clave_rastreo": s.clave_rastreo,
        "concepto": s.concepto,
    });
    let abono = inbound::Abono {
        identidad: IdentidadSpei::nueva(&s.banco_emisor, s.fecha_operacion, &s.clave_rastreo)?,
        institucion_receptora: None,
        receptor_clabe: s.clabe.trim().to_owned(),
        monto: s.monto,
        nombre_ordenante: None,
        cuenta_ordenante: None,
        rfc_curp_ordenante: None,
        referencia_numerica: None,
        concepto: s.concepto,
        tipo_pago: None,
        rastreo_devuelto: None,
        causa_devolucion: None,
        id_proveedor: None,
        raw_payload,
    };
    let r = inbound::abonar(
        &e.pool,
        &e.config.pld,
        &e.config.spei,
        &abono,
        Origen::Operador(u.cliente_id),
        SiNoAcreditable::Rechazar,
    )
    .await?;
    Ok((status_de(r.idempotente), Json(r)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudRetencion {
    cuenta_id: Uuid,
    #[serde(with = "rust_decimal::serde::str")]
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
    Ok(Json(kyc::dictaminar(&e.pool, u.cliente_id, cliente_id, &d).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FiltroPendientes {
    limite: Option<i64>,
}

/// Expedientes EN_REVISION con el resultado del proveedor (cola del oficial
/// de cumplimiento).
async fn kyc_pendientes(
    State(e): State<AppState>,
    u: Usuario,
    Query(f): Query<FiltroPendientes>,
) -> AppResult<Json<Vec<kyc::dictamen::PendienteRevision>>> {
    u.exigir_operador()?;
    Ok(Json(kyc::dictamen::pendientes(&e.pool, f.limite.unwrap_or(100)).await?))
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
    #[serde(with = "rust_decimal::serde::str")]
    principal: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
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
    #[serde(with = "rust_decimal::serde::str")]
    principal: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
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
    #[serde(with = "rust_decimal::serde::str")]
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudCorreccionHttp {
    #[serde(with = "rust_decimal::serde::str")]
    monto: Decimal,
    motivo: String,
    clave_idempotencia: String,
}

/// Corrige (total o parcialmente) un SPEI recibido que se abonó por error:
/// cargo al cliente y a la reserva ligado al abono original, nunca más que su
/// monto. Requiere saldo disponible (si el cliente ya lo gastó, primero se
/// gestiona la recuperación).
async fn corregir_spei(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudCorreccionHttp>,
) -> AppResult<(StatusCode, Json<ResultadoCorreccion>)> {
    u.exigir_operador()?;
    let solicitud = SolicitudCorreccion {
        spei_inbound_id: id,
        monto: s.monto,
        motivo: s.motivo,
        operador_id: u.cliente_id,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let r = inbound::corregir(&e.pool, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r)))
}
