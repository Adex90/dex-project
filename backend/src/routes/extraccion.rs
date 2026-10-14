//! Extractores que rechazan en el formato de error de la API.
//!
//! Los de axum responden texto plano (415, 400, 422) cuando el cuerpo, la
//! query o la ruta no se pueden leer. Estos envuelven a los originales y
//! convierten el rechazo en `AppError`, así que todo cliente recibe
//! `{"error": {"codigo", "mensaje"}}`. `Json` sirve también como respuesta.

use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
};

use axum::{
    extract::{ConnectInfo, FromRequest, FromRequestParts},
    http::request::Parts,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{error::AppError, state::AppState};

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct Query<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct Path<T>(pub T);

/// Cuerpo crudo (los webhooks se firman sobre los bytes exactos).
pub struct Cuerpo(pub axum::body::Bytes);

impl<S: Send + Sync> FromRequest<S> for Cuerpo {
    type Rejection = AppError;

    async fn from_request(req: axum::extract::Request, estado: &S) -> Result<Self, Self::Rejection> {
        axum::body::Bytes::from_request(req, estado).await.map(Cuerpo).map_err(AppError::from)
    }
}

/// IP del cliente: la de la conexión TCP o, detrás de un proxy inverso propio
/// (`HTTP_PROXY_CONFIABLE=true`), la última de `X-Forwarded-For`, que es la
/// que agregó ese proxy (las anteriores las escribe el cliente).
pub struct IpCliente(pub Option<IpAddr>);

impl FromRequestParts<AppState> for IpCliente {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, estado: &AppState) -> Result<Self, Self::Rejection> {
        if estado.config.http.proxy_confiable {
            let reenviada = parts
                .headers
                .get_all("x-forwarded-for")
                .iter()
                .next_back()
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit(',').next())
                .and_then(|v| v.trim().parse::<IpAddr>().ok());
            if reenviada.is_some() {
                return Ok(Self(reenviada));
            }
        }
        Ok(Self(parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())))
    }
}
