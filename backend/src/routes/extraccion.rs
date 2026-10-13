//! Extractores que rechazan en el formato de error de la API.
//!
//! Los de axum responden texto plano (415, 400, 422) cuando el cuerpo, la
//! query o la ruta no se pueden leer. Estos envuelven a los originales y
//! convierten el rechazo en `AppError`, así que todo cliente recibe
//! `{"error": {"codigo", "mensaje"}}`. `Json` sirve también como respuesta.

use axum::{
    extract::{FromRequest, FromRequestParts},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::error::AppError;

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
