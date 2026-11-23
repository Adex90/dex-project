//! Patrimonio: las cuentas vinculadas del cliente y los retiros a ellas
//! (`crate::patrimonio`). Solo el cliente; rutas del grupo `api` del límite
//! por IP (limites.rs), con los nombres en inglés del frontend
//! (`frontend/src/types/wealth.ts`).

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
        documentos::{self, Documento, Documentos},
        impuestos::{self, CentroFiscal, EstadoW8ben, SolicitudW8ben},
        modelos::{CuentasVinculadas, ReciboRetiro},
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
    let solicitud = SolicitudRetiro {
        cliente_id: u.cliente_id,
        cuenta_vinculada_id: s.linked_account_id,
        monto: s.amount,
        clave_idempotencia: u.clave(&s.idempotency_key)?,
    };
    let r = patrimonio::retirar(&e.pool, &e.config, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r.recibo)))
}
