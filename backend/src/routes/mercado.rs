//! Mercado de valores: catálogo, cotizaciones y órdenes (`crate::mercado`).
//!
//! Las tres rutas están en el grupo `api` del límite por IP (limites.rs) y
//! exigen sesión (el extractor [`Usuario`] valida el JWT y la sesión en la
//! base de datos). Colocar una orden es solo del cliente.

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::Deserialize;

use super::status_de;
use crate::{
    auth::Usuario,
    error::AppResult,
    mercado::{self, Catalogo, Cotizacion, Lado, Recibo, SolicitudOrden},
    routes::extraccion::{Json, Path},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/market/assets", get(catalogo))
        .route("/market/quote/{ticker}", get(cotizacion))
        .route("/market/order", post(orden))
}

/// El catálogo (`MarketCatalog`): lo que hoy se cotiza y se opera. Cualquier sesión válida.
async fn catalogo(State(e): State<AppState>, _u: Usuario) -> AppResult<Json<Catalogo>> {
    Ok(Json(mercado::catalogo(&e.pool, &e.config).await?))
}

/// El activo y su historial de precios (`MarketQuote`). Cualquier sesión válida.
async fn cotizacion(State(e): State<AppState>, _u: Usuario, Path(ticker): Path<String>) -> AppResult<Json<Cotizacion>> {
    Ok(Json(mercado::cotizar(&e.pool, &e.config, &ticker).await?))
}

/// `OrderRequest` (frontend/src/types/market.ts). Campos desconocidos: 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudOrdenHttp {
    ticker: String,
    side: Lado,
    /// Pesos con centavos, como texto ("1250.00"): nunca un número JSON.
    #[serde(with = "rust_decimal::serde::str")]
    amount: Decimal,
    idempotency_key: String,
}

/// Orden de mercado: 201 con el recibo; 200 si repite una clave ya ejecutada.
async fn orden(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudOrdenHttp>,
) -> AppResult<(StatusCode, Json<Recibo>)> {
    u.exigir_cliente()?;
    let solicitud = SolicitudOrden {
        cliente_id: u.cliente_id,
        ticker: s.ticker,
        lado: s.side,
        monto: s.amount,
        clave_idempotencia: u.clave(&s.idempotency_key)?,
    };
    let r = mercado::colocar(&e.pool, &e.config, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r.recibo)))
}
