//! Mercado de valores: catálogo, cotizaciones y órdenes (`crate::mercado`).
//!
//! Las rutas están en el grupo `api` del límite por IP (limites.rs) y exigen
//! sesión (el extractor [`Usuario`] valida el JWT y la sesión en la base de
//! datos). Colocar, listar y cancelar órdenes es solo del cliente.

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::status_de;
use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    mercado::{
        self,
        pendiente::{self, SolicitudPendiente},
        Catalogo, Cotizacion, Lado, OrdenesPendientes, Recibo, SolicitudOrden, TipoPendiente, VistaPendiente,
    },
    routes::extraccion::{Json, Path},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/market/assets", get(catalogo))
        .route("/market/quote/{ticker}", get(cotizacion))
        .route("/market/order", post(orden))
        .route("/market/orders/pending", get(pendientes))
        .route("/market/orders/{id}/cancel", post(cancelar))
}

/// El catálogo (`MarketCatalog`): lo que hoy se cotiza y se opera. Cualquier sesión válida.
async fn catalogo(State(e): State<AppState>, _u: Usuario) -> AppResult<Json<Catalogo>> {
    Ok(Json(mercado::catalogo(&e.pool, &e.config).await?))
}

/// El activo y su historial de precios (`MarketQuote`). Cualquier sesión válida.
async fn cotizacion(State(e): State<AppState>, _u: Usuario, Path(ticker): Path<String>) -> AppResult<Json<Cotizacion>> {
    Ok(Json(mercado::cotizar(&e.pool, &e.config, &ticker).await?))
}

/// `order_type` de `OrderRequest`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
enum TipoOrdenHttp {
    /// El contrato original: sin `order_type`, una orden de mercado.
    #[default]
    #[serde(rename = "market")]
    Mercado,
    #[serde(rename = "limit")]
    Limite,
    #[serde(rename = "stop")]
    Stop,
    /// Plan de ahorro (compras periódicas): llegará con su programador.
    #[serde(rename = "savings_plan")]
    PlanDeAhorro,
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
    #[serde(default)]
    order_type: TipoOrdenHttp,
    /// Solo de una orden límite, como texto ("1600.0000").
    #[serde(default, with = "rust_decimal::serde::str_option")]
    limit_price: Option<Decimal>,
    /// Solo de una orden stop, como texto.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    stop_price: Option<Decimal>,
    /// Solo de un plan de ahorro.
    #[serde(default)]
    frequency: Option<String>,
}

/// La respuesta de una orden: el recibo de una de mercado (`OrderReceipt`) o
/// la orden límite o stop en su estado (`PendingOrder`). Ambas llevan
/// `order_type`.
#[derive(Serialize)]
#[serde(untagged)]
enum RespuestaOrden {
    Mercado(Recibo),
    Pendiente(VistaPendiente),
}

/// Orden de mercado, límite o stop: 201 con el recibo o la orden; 200 si
/// repite una clave ya usada para la misma orden.
async fn orden(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudOrdenHttp>,
) -> AppResult<(StatusCode, Json<RespuestaOrden>)> {
    u.exigir_cliente()?;
    let clave_idempotencia = u.clave(&s.idempotency_key)?;
    if s.order_type == TipoOrdenHttp::PlanDeAhorro {
        return Err(AppError::negocio(
            "TIPO_ORDEN_NO_DISPONIBLE",
            "los planes de ahorro aún no están disponibles; usa una orden de mercado, límite o stop",
        ));
    }
    if s.frequency.is_some() {
        return Err(AppError::validacion("frequency solo aplica a un plan de ahorro (savings_plan)"));
    }
    let (tipo, precio) = match (s.order_type, s.limit_price, s.stop_price) {
        (TipoOrdenHttp::Mercado, None, None) => {
            let solicitud = SolicitudOrden {
                cliente_id: u.cliente_id,
                ticker: s.ticker,
                lado: s.side,
                monto: s.amount,
                clave_idempotencia,
            };
            let r = mercado::colocar(&e.pool, &e.config, &solicitud).await?;
            return Ok((status_de(r.idempotente), Json(RespuestaOrden::Mercado(r.recibo))));
        }
        (TipoOrdenHttp::Mercado, _, _) => {
            return Err(AppError::validacion("una orden de mercado no lleva limit_price ni stop_price"));
        }
        (TipoOrdenHttp::Limite, Some(precio), None) => (TipoPendiente::Limite, precio),
        (TipoOrdenHttp::Limite, None, _) => return Err(AppError::validacion("una orden límite requiere limit_price")),
        (TipoOrdenHttp::Stop, None, Some(precio)) => (TipoPendiente::Stop, precio),
        (TipoOrdenHttp::Stop, _, None) => return Err(AppError::validacion("una orden stop requiere stop_price")),
        _ => {
            return Err(AppError::validacion(
                "una orden límite lleva solo limit_price, y una orden stop solo stop_price",
            ));
        }
    };
    let solicitud = SolicitudPendiente {
        cliente_id: u.cliente_id,
        ticker: s.ticker,
        lado: s.side,
        tipo,
        monto: s.amount,
        precio,
        clave_idempotencia,
    };
    let r = pendiente::colocar(&e.pool, &e.config, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(RespuestaOrden::Pendiente(r.orden))))
}

/// Las órdenes límite y stop del cliente que esperan (`PendingOrders`).
async fn pendientes(State(e): State<AppState>, u: Usuario) -> AppResult<Json<OrdenesPendientes>> {
    u.exigir_cliente()?;
    Ok(Json(OrdenesPendientes { ordenes: pendiente::pendientes(&e.pool, u.cliente_id).await? }))
}

/// Cancela una orden límite o stop del cliente: lo comprometido regresa.
async fn cancelar(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<VistaPendiente>> {
    u.exigir_cliente()?;
    Ok(Json(pendiente::cancelar(&e.pool, u.cliente_id, id).await?))
}
