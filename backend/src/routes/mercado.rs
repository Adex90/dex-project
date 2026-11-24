//! Mercado de valores: catálogo, cotizaciones y órdenes (`crate::mercado`).
//!
//! Las rutas están en el grupo `api` del límite por IP (limites.rs) y exigen
//! sesión (el extractor [`Usuario`] valida el JWT y la sesión en la base de
//! datos). Colocar, listar y cancelar órdenes, crear y cambiar planes de
//! ahorro, su historial de órdenes, su lista de seguimiento y sus alertas de
//! precio son solo del cliente.

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
        alertas::{self, Alertas, Direccion, SolicitudAlerta, VistaAlerta},
        historial_ordenes::{self, HistorialOrdenes},
        pendiente::{self, SolicitudPendiente},
        planes::{self, Cambio, Frecuencia, PlanesAhorro, SolicitudPlan, VistaPlan},
        seguimiento::{self, EstadoSeguimiento, ListaSeguimiento},
        Catalogo, Cotizacion, Lado, OrdenesPendientes, Recibo, SolicitudOrden, TipoPendiente, VistaPendiente,
    },
    routes::extraccion::{Cuerpo, Json, Path, Query},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/market/assets", get(catalogo))
        .route("/market/quote/{ticker}", get(cotizacion))
        .route("/market/order", post(orden))
        .route("/market/orders/pending", get(pendientes))
        .route("/market/orders/{id}/cancel", post(cancelar))
        .route("/market/savings-plans", get(planes_de_ahorro))
        .route("/market/savings-plans/{id}/pause", post(pausar_plan))
        .route("/market/savings-plans/{id}/resume", post(reanudar_plan))
        .route("/market/savings-plans/{id}/cancel", post(cancelar_plan))
        .route("/market/orders/history", get(historial))
        .route("/market/watchlist", get(lista_seguimiento))
        .route("/market/watchlist/{ticker}/toggle", post(alternar_seguimiento))
        .route("/market/alerts", get(alertas_de_precio).post(crear_alerta))
        .route("/market/alerts/{id}/cancel", post(cancelar_alerta))
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
    /// Plan de ahorro: la misma compra cada semana, cada dos semanas o cada mes.
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
    /// Solo de un plan de ahorro: `weekly`, `biweekly` o `monthly`.
    #[serde(default)]
    frequency: Option<Frecuencia>,
}

/// La respuesta de una orden: el recibo de una de mercado (`OrderReceipt`),
/// la orden límite o stop en su estado (`PendingOrder`) o el plan de ahorro
/// con su primera compra (`SavingsPlan`). Las tres llevan `order_type`.
#[derive(Serialize)]
#[serde(untagged)]
enum RespuestaOrden {
    Mercado(Recibo),
    Pendiente(VistaPendiente),
    Plan(VistaPlan),
}

/// Orden de mercado, límite o stop, o plan de ahorro: 201 con el recibo, la
/// orden o el plan; 200 si repite una clave ya usada para lo mismo.
async fn orden(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudOrdenHttp>,
) -> AppResult<(StatusCode, Json<RespuestaOrden>)> {
    u.exigir_cliente()?;
    let clave_idempotencia = u.clave(&s.idempotency_key)?;
    if s.order_type == TipoOrdenHttp::PlanDeAhorro {
        if s.limit_price.is_some() || s.stop_price.is_some() {
            return Err(AppError::validacion("un plan de ahorro no lleva limit_price ni stop_price"));
        }
        let Some(frecuencia) = s.frequency else {
            return Err(AppError::validacion("un plan de ahorro requiere frequency (weekly, biweekly o monthly)"));
        };
        if s.side != Lado::Compra {
            return Err(AppError::validacion("un plan de ahorro solo compra (side buy)"));
        }
        let solicitud = SolicitudPlan {
            cliente_id: u.cliente_id,
            ticker: s.ticker,
            monto: s.amount,
            frecuencia,
            clave_idempotencia,
        };
        let r = planes::crear(&e.pool, &e.config, &solicitud).await?;
        return Ok((status_de(r.idempotente), Json(RespuestaOrden::Plan(r.plan))));
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

/// Los planes de ahorro activos y pausados del cliente (`SavingsPlans`).
async fn planes_de_ahorro(State(e): State<AppState>, u: Usuario) -> AppResult<Json<PlanesAhorro>> {
    u.exigir_cliente()?;
    Ok(Json(PlanesAhorro { planes: planes::listar(&e.pool, u.cliente_id).await? }))
}

/// Pausa un plan de ahorro del cliente: no compra hasta reanudarlo.
async fn pausar_plan(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<VistaPlan>> {
    cambiar_plan(&e, &u, id, Cambio::Pausar).await
}

/// Reanuda un plan de ahorro pausado: compra en la siguiente fecha de su calendario.
async fn reanudar_plan(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<VistaPlan>> {
    cambiar_plan(&e, &u, id, Cambio::Reanudar).await
}

/// Cancela un plan de ahorro del cliente, para siempre (sus compras se quedan).
async fn cancelar_plan(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<VistaPlan>> {
    cambiar_plan(&e, &u, id, Cambio::Cancelar).await
}

async fn cambiar_plan(e: &AppState, u: &Usuario, id: Uuid, cambio: Cambio) -> AppResult<Json<VistaPlan>> {
    u.exigir_cliente()?;
    Ok(Json(planes::cambiar(&e.pool, u.cliente_id, id, cambio).await?))
}

/// `?limit=&cursor=` del historial. Campos desconocidos: 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaHistorialHttp {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
}

/// Cada orden ejecutada, cancelada o rechazada del cliente, por páginas (`OrderHistory`).
async fn historial(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaHistorialHttp>,
) -> AppResult<Json<HistorialOrdenes>> {
    u.exigir_cliente()?;
    Ok(Json(historial_ordenes::listar(&e.pool, u.cliente_id, q.limit, q.cursor.as_deref()).await?))
}

/// Los activos que sigue el cliente, con su ficha (`Watchlist`).
async fn lista_seguimiento(State(e): State<AppState>, u: Usuario) -> AppResult<Json<ListaSeguimiento>> {
    u.exigir_cliente()?;
    Ok(Json(seguimiento::listar(&e.pool, &e.config, u.cliente_id).await?))
}

/// El cuerpo, opcional, de seguir o dejar de seguir: el estado pedido.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudSeguimientoHttp {
    watched: bool,
}

/// Sigue o deja de seguir un activo: con `{"watched": bool}`, ese estado
/// (repetirlo no cambia nada); sin cuerpo, alterna (`WatchState`).
async fn alternar_seguimiento(
    State(e): State<AppState>,
    u: Usuario,
    Path(ticker): Path<String>,
    Cuerpo(cuerpo): Cuerpo,
) -> AppResult<Json<EstadoSeguimiento>> {
    u.exigir_cliente()?;
    let pedido = if cuerpo.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        let s: SolicitudSeguimientoHttp = serde_json::from_slice(&cuerpo)
            .map_err(|e| AppError::validacion(format!("el cuerpo no es válido: {e}")))?;
        Some(s.watched)
    };
    Ok(Json(seguimiento::cambiar(&e.pool, &e.config, u.cliente_id, &ticker, pedido).await?))
}

/// `PriceAlertRequest`. Campos desconocidos: 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudAlertaHttp {
    ticker: String,
    /// Como texto ("1700.0000"): nunca un número JSON.
    #[serde(with = "rust_decimal::serde::str")]
    target_price: Decimal,
    direction: Direccion,
    idempotency_key: String,
}

/// Las alertas de precio activas del cliente y las disparadas en los últimos días (`PriceAlerts`).
async fn alertas_de_precio(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Alertas>> {
    u.exigir_cliente()?;
    Ok(Json(alertas::listar(&e.pool, u.cliente_id).await?))
}

/// Una alerta de precio: 201 con la alerta; 200 si repite una clave ya usada para la misma.
async fn crear_alerta(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudAlertaHttp>,
) -> AppResult<(StatusCode, Json<VistaAlerta>)> {
    u.exigir_cliente()?;
    let solicitud = SolicitudAlerta {
        cliente_id: u.cliente_id,
        ticker: s.ticker,
        precio_objetivo: s.target_price,
        direccion: s.direction,
        clave_idempotencia: u.clave(&s.idempotency_key)?,
    };
    let r = alertas::crear(&e.pool, &e.config, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r.alerta)))
}

/// Cancela una alerta activa del cliente.
async fn cancelar_alerta(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<VistaAlerta>> {
    u.exigir_cliente()?;
    Ok(Json(alertas::cancelar(&e.pool, u.cliente_id, id).await?))
}
