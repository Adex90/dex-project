//! Onboarding digital y KYC biométrico.
//!
//! | Método | Ruta | Acceso |
//! |---|---|---|
//! | GET  | `/kyc/expediente` | cliente: estado, intentos y último resultado |
//! | POST | `/kyc/perfil` | cliente: datos personales y domicilio (paso 1) |
//! | POST | `/kyc/verificar` | cliente: INE + selfie (pasos 2 y 3) |
//! | POST | `/kyc/webhook` | proveedor biométrico (firma HMAC `X-Kyc-Firma`) |

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    routing::{get, post},
    Router,
};

use crate::{
    auth::Usuario,
    error::AppResult,
    kyc::{
        self, biometria,
        expediente::{self, DatosGuardados, DatosPersonales, OrigenEnvio, SolicitudVerificacion, VistaExpediente},
        verificacion::{self, ResultadoVerificacion},
        EnvioVerificacion,
    },
    routes::extraccion::{Cuerpo, IpCliente, Json},
    spei,
    state::AppState,
};

/// INE (2 imágenes) + hasta 8 cuadros de la selfie en base64. El resto de la
/// API conserva su límite de 64 KB.
const MAX_CUERPO_VERIFICACION: usize = 12 * 1024 * 1024;

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/kyc/expediente", get(ver_expediente))
        .route("/kyc/perfil", post(guardar_perfil))
        .route("/kyc/verificar", post(verificar).layer(DefaultBodyLimit::max(MAX_CUERPO_VERIFICACION)))
        .route("/kyc/webhook", post(webhook))
}

async fn ver_expediente(State(e): State<AppState>, u: Usuario) -> AppResult<Json<VistaExpediente>> {
    u.exigir_cliente()?;
    Ok(Json(expediente::vista(&e.pool, u.cliente_id).await?))
}

async fn guardar_perfil(
    State(e): State<AppState>,
    u: Usuario,
    Json(d): Json<DatosPersonales>,
) -> AppResult<Json<DatosGuardados>> {
    u.exigir_cliente()?;
    Ok(Json(expediente::guardar_datos(&e.pool, u.cliente_id, &d, spei::hoy_cdmx()).await?))
}

/// 200 con el dictamen si el proveedor respondió de inmediato; 202 si el
/// resultado llegará por el webhook (el expediente queda EN_REVISION).
async fn verificar(
    State(e): State<AppState>,
    u: Usuario,
    IpCliente(ip): IpCliente,
    cabeceras: HeaderMap,
    Json(s): Json<SolicitudVerificacion>,
) -> AppResult<(StatusCode, Json<EnvioVerificacion>)> {
    u.exigir_cliente()?;
    let origen = OrigenEnvio {
        ip,
        user_agent: cabeceras.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_owned),
    };
    let r = kyc::verificar_identidad(&e.pool, &e.config.kyc, e.biometria.as_ref(), u.cliente_id, &s, &origen).await?;
    let status = if r.resultado.is_some() { StatusCode::OK } else { StatusCode::ACCEPTED };
    Ok((status, Json(r)))
}

/// Resultado asíncrono del proveedor. Se autentica por la firma del cuerpo
/// crudo; repetirlo es idempotente.
async fn webhook(
    State(e): State<AppState>,
    cabeceras: HeaderMap,
    Cuerpo(cuerpo): Cuerpo,
) -> AppResult<Json<ResultadoVerificacion>> {
    let firma = cabeceras.get(biometria::CABECERA).and_then(|v| v.to_str().ok());
    Ok(Json(verificacion::procesar(&e.pool, &e.config.kyc, firma, &cuerpo).await?))
}
