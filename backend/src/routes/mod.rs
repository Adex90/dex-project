//! API HTTP (Axum). Todas las rutas de negocio viven bajo `/api/v1`.
//!
//! | Método | Ruta | Acceso |
//! |---|---|---|
//! | GET  | `/salud` | público |
//! | POST | `/api/v1/auth/registro` | público |
//! | POST | `/api/v1/auth/login` | público |
//! | GET  | `/api/v1/yo` | cualquier rol (sin nivel de riesgo PLD) |
//! | GET  | `/api/v1/kyc/expediente` | cliente: estado del onboarding |
//! | POST | `/api/v1/kyc/perfil` | cliente: datos personales (paso 1) |
//! | POST | `/api/v1/kyc/verificar` | cliente: INE + selfie (pasos 2 y 3) |
//! | POST | `/api/v1/kyc/webhook` | proveedor biométrico (firma HMAC) |
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
//! | GET  | `/api/v1/reserva/cortes`, `/api/v1/reserva/cortes/{id}` | público (historial de anclas) |
//! | POST | `/api/v1/reserva/verificar` | público |
//! | GET  | `/api/v1/reserva/prueba` | cliente |
//! | POST | `/api/v1/reserva/cortes` | operador / auditor |
//! | GET  | `/api/v1/auditoria/ledger/{ifpe,sofom}` | operador / auditor |
//! | GET  | `/api/v1/kyc/expediente` | cliente: estado, intentos, bloqueo y último resultado |
//! | POST | `/api/v1/kyc/perfil` | cliente: datos personales y domicilio |
//! | POST | `/api/v1/kyc/verificar` | cliente: INE + selfie (hasta 12 MB) |
//! | POST | `/api/v1/kyc/webhook` | proveedor biométrico (firma HMAC `X-Kyc-Firma`) |
//! | POST | `/api/v1/operaciones/depositos` | operador (SPEI entrante) |
//! | POST | `/api/v1/operaciones/retenciones` | operador |
//! | GET  | `/api/v1/operaciones/kyc/pendientes` | operador |
//! | POST | `/api/v1/operaciones/kyc/{cliente_id}` | operador |
//! | GET/POST | `/api/v1/operaciones/creditos` | operador |
//! | POST | `/api/v1/operaciones/creditos/{id}/pagos` | operador |
//! | POST | `/api/v1/operaciones/capital` | operador |
//! | POST | `/api/v1/spei/webhook/inbound`, `/api/v1/spei/webhook/estado` | proveedor SPEI (firma HMAC) |
//! | POST | `/api/v1/spei/transfer` | cliente: SPEI saliente |
//! | GET  | `/api/v1/spei/transferencias`, `/api/v1/spei/recibidos` | cliente |
//! | GET  | `/api/v1/spei/conciliacion`, `/api/v1/spei/en-revision` | operador / auditor |
//! | POST | `/api/v1/operaciones/spei/recibidos/{id}/correccion` | operador: revierte un abono SPEI erróneo |
//! | POST | `/api/v1/operaciones/spei/recibidos/{id}/anulacion` | operador: anula una captura manual errónea |
//! | POST | `/api/v1/operaciones/spei/enviados/{id}/reembolso` | operador: reembolsa una orden en revisión que el proveedor no tiene |
//! | POST | `/api/v1/operaciones/spei/en-revision/{id}/descarte` | operador: descarta como duplicado una notificación retenida |
//! | GET  | `/api/v1/aml/resumen` | cumplimiento / auditor: contadores de la consola PLD |
//! | GET  | `/api/v1/aml/alertas` | cumplimiento / auditor: bandeja de alertas PLD (filtros, cursor, sondeo) |
//! | GET  | `/api/v1/aml/alertas/{id}` | cumplimiento / auditor: expediente de la alerta (queda en la bitácora) |
//! | POST | `/api/v1/aml/alertas/{id}/resolver` | cumplimiento: tomar, liberar, escalar, confirmar o falso positivo |
//! | POST | `/api/v1/aml/alertas/{id}/reporte-uif` | cumplimiento: folio UIF de una alerta confirmada |
//! | GET  | `/api/v1/aml/clientes/{id}` | cumplimiento / auditor: revisión PLD del cliente (queda en la bitácora) |
//! | POST | `/api/v1/aml/clientes/{id}/congelar` | cumplimiento: congelamiento preventivo manual |
//! | GET  | `/api/v1/aml/congelamientos` | cumplimiento / auditor: congelamientos (ACTIVO por omisión) |
//! | POST | `/api/v1/aml/congelamientos/{id}/solicitudes` | cumplimiento: pide levantar o liberar retenciones (cuatro ojos) |
//! | GET  | `/api/v1/aml/solicitudes` | cumplimiento / auditor: cola de cuatro ojos |
//! | POST | `/api/v1/aml/solicitudes/{id}/decision` | cumplimiento, distinto del solicitante: aprueba o rechaza |
//! | GET  | `/api/v1/aml/notificaciones` | cumplimiento / auditor: bandeja de salida de avisos PLD |
//! | GET  | `/api/v1/aml/reglas` | cumplimiento / auditor: parámetros, matriz de riesgo y propuestas pendientes |
//! | GET/POST | `/api/v1/aml/propuestas` | cumplimiento / auditor (GET); cumplimiento propone cambios (POST) |
//! | POST | `/api/v1/aml/propuestas/{id}/decision` | cumplimiento, distinto del proponente: aprueba o rechaza |
//! | GET  | `/api/v1/aml/relevantes` | cumplimiento / auditor: registro trimestral de operaciones relevantes |
//! | GET  | `/api/v1/aml/bitacora/verificacion` | cumplimiento / auditor: verifica hashes y sellos de la bitácora PLD |

mod aml;
mod auth;
mod extraccion;
mod ifpe;
mod kyc;
mod operaciones;
mod reserva;
mod sofom;
mod spei;

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
        .merge(kyc::rutas())
        .merge(sofom::rutas())
        .merge(reserva::rutas())
        .merge(operaciones::rutas())
        .merge(spei::rutas())
        .merge(aml::rutas(estado.clone()));

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
