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
mod ifpe;
mod operaciones;
mod reserva;
mod sofom;

use axum::{
    extract::State,
    http::{header, HeaderValue, Method, StatusCode},
    routing::get,
    Json, Router,
};
use serde_json::json;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::state::AppState;

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

    Router::new()
        .route("/salud", get(salud))
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(estado)
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
