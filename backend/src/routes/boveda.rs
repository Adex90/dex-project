//! Bóveda Soberana: `GET /api/v1/vault` (la foto, por REST) y
//! `GET /api/v1/vault/ws` (los cambios, por WebSocket). Solo para clientes,
//! y las dos dentro del límite por IP del grupo `api`.
//!
//! Autenticación del WebSocket: un navegador no puede poner cabeceras en un
//! WebSocket, así que el JWT viaja como una entrada del subprotocolo:
//!
//! ```text
//! new WebSocket(url, ["sc.vault.v1", `bearer.${token}`])
//! ```
//!
//! El servidor confirma solo `sc.vault.v1` (el token nunca vuelve en la
//! respuesta) y lo valida con las mismas comprobaciones que `Authorization:
//! Bearer`, que también se acepta para clientes que no son navegadores. CORS
//! no aplica a un WebSocket, así que `Origin`, si viene, debe ser el del
//! frontend.

use axum::{
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};

use super::extraccion::Json;
use crate::{
    auth::{self, Usuario},
    boveda::{
        central::Rechazo,
        consulta,
        modelos::Boveda,
        socket::{self, PREFIJO_TOKEN, PROTOCOLO},
    },
    error::{AppError, AppResult},
    state::AppState,
};

/// Lo más grande que un navegador envía es `{"type":"resync"}`.
const MAX_MENSAJE: usize = 4 * 1024;

pub fn rutas() -> Router<AppState> {
    Router::new().route("/vault", get(boveda)).route("/vault/ws", get(conectar))
}

/// La bóveda del cliente, leída con su rol (`sc_cliente`) y RLS.
async fn boveda(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Boveda>> {
    u.exigir_cliente()?;
    Ok(Json(consulta::leer(&e.pool, &e.config, u.cliente_id).await?.boveda))
}

async fn conectar(
    State(e): State<AppState>,
    cabeceras: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> AppResult<Response> {
    let ws = ws.map_err(|r| {
        AppError::peticion(r.status(), "WEBSOCKET_REQUERIDO", "esta ruta solo acepta una conexión WebSocket")
    })?;
    if let Some(origen) = cabeceras.get(header::ORIGIN) {
        if origen.as_bytes() != e.config.frontend_origin.as_bytes() {
            return Err(AppError::Prohibido);
        }
    }
    let ofrecidos = subprotocolos(&cabeceras);
    if !ofrecidos.contains(&PROTOCOLO) {
        return Err(AppError::peticion(
            StatusCode::BAD_REQUEST,
            "PROTOCOLO_REQUERIDO",
            format!("pide el subprotocolo {PROTOCOLO}"),
        ));
    }
    let token = bearer(&cabeceras)
        .or_else(|| ofrecidos.iter().find_map(|p| p.strip_prefix(PREFIJO_TOKEN)))
        .ok_or(AppError::NoAutenticado)?;
    let (usuario, claims) = auth::autenticar_token(&e, token).await?;
    usuario.exigir_cliente()?;

    // Se inscribe antes de abrir: el tope se respeta y ningún cambio que
    // confirme mientras se abre el socket se pierde.
    let suscripcion = e.boveda.suscribir(usuario.cliente_id).map_err(|r| match r {
        Rechazo::TareaLlena => AppError::peticion(
            StatusCode::SERVICE_UNAVAILABLE,
            "REINTENTAR",
            "demasiadas conexiones abiertas; reintenta en unos segundos",
        ),
        Rechazo::DemasiadosDelCliente => AppError::peticion(
            StatusCode::TOO_MANY_REQUESTS,
            "DEMASIADAS_CONEXIONES",
            "la bóveda ya está abierta en demasiadas pestañas o dispositivos",
        ),
    })?;

    Ok(ws
        .protocols([PROTOCOLO])
        .max_message_size(MAX_MENSAJE)
        .max_frame_size(MAX_MENSAJE)
        .on_upgrade(move |conexion| socket::atender(conexion, suscripcion, claims)))
}

/// Las entradas de `Sec-WebSocket-Protocol`, en una o varias cabeceras.
fn subprotocolos(cabeceras: &HeaderMap) -> Vec<&str> {
    cabeceras
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

fn bearer(cabeceras: &HeaderMap) -> Option<&str> {
    cabeceras.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "))
}
