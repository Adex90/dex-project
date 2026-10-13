//! API HTTP (Axum). Todas las rutas de negocio viven bajo `/api/v1`.
//!
//! | Método | Ruta | Acceso |
//! |---|---|---|
//! | GET  | `/salud` | público |
//! | POST | `/api/v1/auth/registro` | público |
//! | POST | `/api/v1/auth/login` | público |
//! | GET  | `/api/v1/yo` | cliente |
//! | GET  | `/api/v1/tablero` | cliente |
//! | GET  | `/api/v1/ifpe/cuenta` | cliente |
//! | GET  | `/api/v1/ifpe/movimientos` | cliente |
//! | POST | `/api/v1/ifpe/transferencias` | cliente |
//! | POST | `/api/v1/ifpe/retiros` | cliente |
//! | POST | `/api/v1/ifpe/depositos/simulados` | cliente (solo `MODO_DEMO`) |
//! | GET  | `/api/v1/sofom/tasas` | público |
//! | GET  | `/api/v1/sofom/posicion` | cliente |
//! | POST | `/api/v1/sofom/mutuos` | cliente — **puente IFPE -> SOFOM** |
//! | GET  | `/api/v1/sofom/mutuos/{id}/terminos` | cliente |
//! | POST | `/api/v1/sofom/mutuos/{id}/liquidar` | cliente dueño u operador |
//! | GET  | `/api/v1/reserva/estado` | público |
//! | GET  | `/api/v1/reserva/cortes/ultimo` | público |
//! | POST | `/api/v1/reserva/verificar` | público |
//! | GET  | `/api/v1/reserva/prueba` | cliente |
//! | POST | `/api/v1/reserva/cortes` | operador / auditor |
//! | GET  | `/api/v1/auditoria/ledger/{ifpe,sofom}` | operador / auditor |
//! | POST | `/api/v1/operaciones/depositos` | operador (SPEI entrante) |
//! | POST | `/api/v1/operaciones/retenciones` | operador |
//! | POST | `/api/v1/operaciones/kyc/{cliente_id}` | operador |
//! | GET  | `/api/v1/operaciones/alertas` | operador |
//! | GET/POST | `/api/v1/operaciones/creditos` | operador |
//! | POST | `/api/v1/operaciones/creditos/{id}/pagos` | operador |
//! | POST | `/api/v1/operaciones/capital` | operador |

mod auth;
mod extraccion;
mod ifpe;
mod operaciones;
mod reserva;
mod sofom;

use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use tower_http::{cors::CorsLayer, timeout::RequestBodyTimeoutLayer, trace::TraceLayer};

use crate::{error::AppError, state::AppState};

/// Ninguna petición legítima se acerca a esto (una prueba de inclusión con
/// 64 niveles ocupa unos 10 KB).
const MAX_CUERPO: usize = 64 * 1024;

pub fn router(estado: AppState) -> Router {
    let cors = match estado.config.frontend_origin.parse::<HeaderValue>() {
        Ok(origen) => CorsLayer::new()
            .allow_origin(origen)
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        Err(_) => CorsLayer::new(),
    };

    let api = Router::new()
        .merge(auth::rutas())
        .merge(ifpe::rutas())
        .merge(sofom::rutas())
        .merge(reserva::rutas())
        .merge(operaciones::rutas());

    let limites = &estado.config.http;
    let peticion = Duration::from_secs(limites.peticion_seg);
    let cuerpo = Duration::from_secs(limites.cuerpo_seg);
    Router::new()
        .route("/salud", get(salud))
        .nest("/api/v1", api)
        .fallback(ruta_inexistente)
        .method_not_allowed_fallback(metodo_no_permitido)
        .layer(DefaultBodyLimit::max(MAX_CUERPO))
        .layer(RequestBodyTimeoutLayer::new(cuerpo))
        .layer(middleware::from_fn_with_state(peticion, tiempo_maximo))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(estado)
}

async fn ruta_inexistente() -> AppError {
    AppError::peticion(StatusCode::NOT_FOUND, "RUTA_INEXISTENTE", "la ruta no existe")
}

async fn metodo_no_permitido() -> AppError {
    AppError::peticion(StatusCode::METHOD_NOT_ALLOWED, "METODO_NO_PERMITIDO", "método no permitido en esta ruta")
}

/// Tope de tiempo por petición completa (cuerpo + handler). Al vencer se
/// abandona el handler: la transacción en curso se revierte al soltar la
/// conexión.
async fn tiempo_maximo(State(limite): State<Duration>, peticion: Request, siguiente: Next) -> Response {
    let ruta = peticion.uri().path().to_owned();
    match tokio::time::timeout(limite, siguiente.run(peticion)).await {
        Ok(respuesta) => respuesta,
        Err(_) => {
            tracing::warn!(%ruta, segundos = limite.as_secs(), "petición abandonada por tiempo máximo");
            AppError::peticion(
                StatusCode::SERVICE_UNAVAILABLE,
                "TIEMPO_AGOTADO",
                "la operación tardó demasiado; reintenta con la misma clave de idempotencia",
            )
            .into_response()
        }
    }
}

async fn salud(State(e): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    match sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&e.pool).await {
        Ok(_) => (StatusCode::OK, Json(json!({ "estado": "ok", "base_de_datos": "ok" }))),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "estado": "degradado", "base_de_datos": "error" }))),
    }
}

/// 201 si la operación es nueva, 200 si es una repetición idempotente.
pub(crate) fn status_de(idempotente: bool) -> StatusCode {
    if idempotente {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    }
}
