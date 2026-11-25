//! Patrimonio: las cuentas vinculadas del cliente y los retiros a ellas, el
//! buzón, el Centro Fiscal, y el rastreador SPEI con sus aclaraciones
//! (`crate::patrimonio`). Solo el cliente; rutas del grupo `api` del límite
//! por IP (limites.rs), con los nombres en inglés del frontend
//! (`frontend/src/types/wealth.ts`, `frontend/src/types/transfers.ts`).

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use super::status_de;
use crate::{
    auth::{sesiones::Origen, Usuario},
    error::AppResult,
    kyc::beneficiarios::Autor,
    patrimonio::{
        self,
        aclaraciones::{self, Aclaraciones, Motivo, SolicitudAclaracion, VistaAclaracion},
        documentos::{self, Documento, Documentos},
        impuestos::{self, CentroFiscal, EstadoW8ben, SolicitudW8ben},
        modelos::{CuentasVinculadas, ReciboRetiro},
        transferencias::{self, Direccion, Transferencia, Transferencias},
        SolicitudRetiro,
    },
    routes::extraccion::{Json, Path, Query},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/wealth/linked-accounts", get(cuentas_vinculadas))
        .route("/wealth/withdraw", post(retirar))
        .route("/wealth/documentos", get(listar_documentos))
        .route("/wealth/documentos/{id}", get(ver_documento))
        .route("/wealth/impuestos", get(ver_impuestos))
        .route("/wealth/impuestos/w8ben", post(certificar_w8ben))
        .route("/wealth/impuestos/export.csv", get(exportar_impuestos))
        .route("/wealth/spei/transfers", get(transferencias_spei))
        .route("/wealth/spei/transfers/{id}", get(transferencia_spei))
        .route("/wealth/spei/disputes", get(aclaraciones_spei).post(abrir_aclaracion))
}

/// `?limit=&cursor=` del rastreador SPEI.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaTransferencias {
    limit: Option<i64>,
    cursor: Option<String>,
}

/// Con la pasarela simulada (`MODO_DEMO`), los SPEI salientes no pasan por
/// Banxico: no tienen CEP.
fn pasarela_simulada(e: &AppState) -> bool {
    e.pasarela.nombre() == "simulada"
}

/// Los SPEI del cliente (`SpeiTransfers`), del más reciente al más antiguo.
async fn transferencias_spei(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaTransferencias>,
) -> AppResult<Json<Transferencias>> {
    u.exigir_cliente()?;
    let simulada = pasarela_simulada(&e);
    Ok(Json(transferencias::listar(&e.pool, u.cliente_id, simulada, q.limit, q.cursor.as_deref()).await?))
}

/// Un SPEI del cliente (`TrackedTransfer`), por su id o por el de un movimiento
/// del ledger que hizo. 404 si no es suyo.
async fn transferencia_spei(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Transferencia>> {
    u.exigir_cliente()?;
    Ok(Json(transferencias::obtener(&e.pool, u.cliente_id, pasarela_simulada(&e), id).await?))
}

/// Las aclaraciones SPEI del cliente (`SpeiClarifications`).
async fn aclaraciones_spei(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Aclaraciones>> {
    u.exigir_cliente()?;
    Ok(Json(aclaraciones::listar(&e.pool, u.cliente_id).await?))
}

/// `SpeiClarificationRequest`. Campos desconocidos: 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudAclaracionHttp {
    transfer_id: Uuid,
    direction: Direccion,
    reason: Motivo,
    #[serde(default)]
    description: Option<String>,
    idempotency_key: String,
}

/// Abre una aclaración sobre un SPEI del cliente: 201 con ella; 200 si repite
/// la clave de la misma.
async fn abrir_aclaracion(
    State(e): State<AppState>,
    u: Usuario,
    origen: Origen,
    Json(s): Json<SolicitudAclaracionHttp>,
) -> AppResult<(StatusCode, Json<VistaAclaracion>)> {
    u.exigir_cliente()?;
    let solicitud = SolicitudAclaracion {
        cliente_id: u.cliente_id,
        transferencia_id: s.transfer_id,
        direccion: s.direction,
        motivo: s.reason,
        descripcion: s.description,
        clave_idempotencia: u.clave(&s.idempotency_key)?,
        sesion_id: Some(u.sesion_id),
        ip: origen.ip,
        dispositivo: Some(origen.dispositivo.clone()),
    };
    let r = aclaraciones::abrir(&e.pool, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r.aclaracion)))
}

/// `?year=`: el ejercicio (el actual si no viene).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaEjercicio {
    year: Option<i32>,
}

/// El Centro Fiscal (`TaxCenter`): el perfil fiscal, el W-8BEN y el resumen
/// del ejercicio pedido.
async fn ver_impuestos(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaEjercicio>,
) -> AppResult<Json<CentroFiscal>> {
    u.exigir_cliente()?;
    Ok(Json(impuestos::consultar(&e.pool, u.cliente_id, q.year, Utc::now()).await?))
}

/// Certifica (o renueva) el W-8BEN: una versión nueva (201) con el estado
/// que resulta (`W8benState`).
async fn certificar_w8ben(
    State(e): State<AppState>,
    u: Usuario,
    origen: Origen,
    Json(s): Json<SolicitudW8ben>,
) -> AppResult<(StatusCode, Json<EstadoW8ben>)> {
    u.exigir_cliente()?;
    let autor = Autor { sesion_id: Some(u.sesion_id), ip: origen.ip, dispositivo: Some(&origen.dispositivo) };
    let estado = impuestos::certificar_w8ben(&e.pool, u.cliente_id, &s, &autor, Utc::now()).await?;
    Ok((StatusCode::CREATED, Json(estado)))
}

/// La exportación contable del ejercicio: CSV (RFC 4180) para descargar.
async fn exportar_impuestos(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaEjercicio>,
) -> AppResult<Response> {
    u.exigir_cliente()?;
    let (nombre, contenido) = impuestos::exportar_csv(&e.pool, u.cliente_id, q.year, Utc::now()).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8; header=present".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{nombre}\"")),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        contenido,
    )
        .into_response())
}

/// El buzón (`Documents`): estados de cuenta de cada mes con actividad y
/// comprobantes de operación, el más reciente primero.
async fn listar_documentos(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Documentos>> {
    u.exigir_cliente()?;
    Ok(Json(documentos::listar(&e.pool, u.cliente_id).await?))
}

/// Un documento completo (`Document`): `co_<orden>` o `ec_<AAAA-MM>`. 404 si
/// no existe o no es del cliente.
async fn ver_documento(State(e): State<AppState>, u: Usuario, Path(id): Path<String>) -> AppResult<Json<Documento>> {
    u.exigir_cliente()?;
    Ok(Json(documentos::obtener(&e.pool, u.cliente_id, &id).await?))
}

/// Las cuentas vinculadas activas del cliente (`LinkedAccounts`): a dónde
/// puede retirar. Con `MODO_DEMO`, la primera consulta vincula la de
/// demostración.
async fn cuentas_vinculadas(State(e): State<AppState>, u: Usuario) -> AppResult<Json<CuentasVinculadas>> {
    u.exigir_cliente()?;
    Ok(Json(patrimonio::cuentas_vinculadas(&e.pool, &e.config, u.cliente_id).await?))
}

/// `WithdrawalRequest`. Campos desconocidos: 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudRetiroHttp {
    linked_account_id: Uuid,
    /// MXN con a lo más 2 decimales (SPEI liquida en centavos).
    #[serde(with = "rust_decimal::serde::str")]
    amount: Decimal,
    idempotency_key: String,
}

/// Retira efectivo disponible a una cuenta vinculada: 201 con el recibo
/// (`WithdrawalReceipt`, en proceso); 200 si repite una clave ya usada para
/// el mismo retiro, con su estado actual.
async fn retirar(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudRetiroHttp>,
) -> AppResult<(StatusCode, Json<ReciboRetiro>)> {
    u.exigir_cliente()?;
    // El dinero sale de la cuenta: verificación reforzada (0022).
    u.exigir_step_up()?;
    let solicitud = SolicitudRetiro {
        cliente_id: u.cliente_id,
        cuenta_vinculada_id: s.linked_account_id,
        monto: s.amount,
        clave_idempotencia: u.clave(&s.idempotency_key)?,
    };
    let r = patrimonio::retirar(&e.pool, &e.config, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r.recibo)))
}
