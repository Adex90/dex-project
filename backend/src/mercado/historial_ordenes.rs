//! El historial de órdenes del cliente (`GET /api/v1/market/orders/history`):
//! cada orden ejecutada del ledger del mercado (una orden de mercado, la
//! ejecución de una límite o stop, la compra de un plan de ahorro) y cada
//! orden límite o stop que se cerró sin ejecutarse (cancelada o rechazada),
//! de la más reciente a la más antigua, por páginas.
//!
//! - Una ejecutada sale una vez, con su orden del ledger: su tipo es el de la
//!   orden pendiente que ejecutó (`ordenes_pendientes.orden_ejecutada_id`) o
//!   el plan cuya compra registró (`eventos_plan`, EJECUTADO); si no, de
//!   mercado. Lleva la cantidad, el precio de ejecución, el importe y, si
//!   vino de una límite o stop, su precio y su orden de origen.
//! - Una cancelada o rechazada lleva su precio límite o stop y, si se
//!   rechazó, el código de la regla (la pantalla lo dice en inglés).
//! - Con el rol del cliente y RLS: solo lo suyo. Cada página trae a lo más
//!   `limit` (de 1 a [`MAXIMO_PAGINA`], [`PAGINA`] si no se pide) y un cursor
//!   opaco para la siguiente; null en la última.
//!
//! Lee lo que ya existe (migraciones 0013, 0015 y 0023): no escribe nada.

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::modelos::Lado;
use crate::{
    db, dinero,
    error::{AppError, AppResult},
};

/// Órdenes por página, si no se pide otra cosa.
pub const PAGINA: i64 = 50;
/// Órdenes por página, a lo más.
pub const MAXIMO_PAGINA: i64 = 100;

/// Cómo terminó: ejecutada, cancelada por el cliente o rechazada por una regla.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoHistorial {
    #[serde(rename = "filled")]
    Ejecutada,
    #[serde(rename = "cancelled")]
    Cancelada,
    #[serde(rename = "rejected")]
    Rechazada,
}

/// Una orden del historial (`OrderHistoryEntry`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntradaHistorial {
    /// La orden del ledger del mercado (ejecutada) o la orden pendiente (cerrada sin ejecutarse).
    #[serde(rename = "order_id")]
    pub id: Uuid,
    pub status: EstadoHistorial,
    /// `market`, `limit`, `stop` o `savings_plan`.
    #[serde(rename = "order_type")]
    pub tipo: &'static str,
    #[serde(rename = "side")]
    pub lado: Lado,
    pub ticker: String,
    /// Lo que pidió el cliente, en MXN.
    #[serde(rename = "amount")]
    pub monto: Decimal,
    #[serde(rename = "limit_price")]
    pub precio_limite: Option<Decimal>,
    #[serde(rename = "stop_price")]
    pub precio_stop: Option<Decimal>,
    /// Ejecutada: unidades, precio de ejecución, importe y comisión.
    #[serde(rename = "quantity")]
    pub cantidad: Option<Decimal>,
    #[serde(rename = "price")]
    pub precio: Option<Decimal>,
    #[serde(rename = "total")]
    pub importe: Option<Decimal>,
    #[serde(rename = "commission")]
    pub comision: Option<Decimal>,
    /// Rechazada: el código de la regla.
    #[serde(rename = "reject_reason")]
    pub motivo: Option<String>,
    /// Cuándo se colocó (una límite o stop ejecutada, cuando se colocó la pendiente).
    #[serde(rename = "placed_at")]
    pub colocada_en: DateTime<Utc>,
    /// Cuándo se ejecutó, se canceló o se rechazó.
    #[serde(rename = "closed_at")]
    pub cerrada_en: DateTime<Utc>,
    /// La orden límite o stop que ejecutó.
    #[serde(rename = "source_order_id")]
    pub orden_origen_id: Option<Uuid>,
    /// El plan de ahorro de la compra.
    #[serde(rename = "plan_id")]
    pub plan_id: Option<Uuid>,
}

/// `GET /api/v1/market/orders/history` (`OrderHistory`).
#[derive(Debug, Clone, Serialize)]
pub struct HistorialOrdenes {
    #[serde(rename = "orders")]
    pub ordenes: Vec<EntradaHistorial>,
    /// Para pedir la página siguiente (`?cursor=`); null en la última.
    pub next_cursor: Option<String>,
}

#[derive(FromRow)]
struct Fila {
    id: Uuid,
    estado: String,
    tipo: String,
    lado: Lado,
    ticker: String,
    monto: Decimal,
    precio_limite: Option<Decimal>,
    precio_stop: Option<Decimal>,
    cantidad: Option<Decimal>,
    precio: Option<Decimal>,
    importe: Option<Decimal>,
    comision: Option<Decimal>,
    motivo: Option<String>,
    colocada_en: DateTime<Utc>,
    cerrada_en: DateTime<Utc>,
    orden_origen_id: Option<Uuid>,
    plan_id: Option<Uuid>,
}

impl Fila {
    fn entrada(self) -> AppResult<EntradaHistorial> {
        let status = match self.estado.as_str() {
            "EJECUTADA" => EstadoHistorial::Ejecutada,
            "CANCELADA" => EstadoHistorial::Cancelada,
            "RECHAZADA" => EstadoHistorial::Rechazada,
            otro => return Err(AppError::Integridad(format!("historial: estado desconocido {otro}"))),
        };
        let tipo = match self.tipo.as_str() {
            "MERCADO" => "market",
            "LIMITE" => "limit",
            "STOP" => "stop",
            "PLAN" => "savings_plan",
            otro => return Err(AppError::Integridad(format!("historial: tipo desconocido {otro}"))),
        };
        Ok(EntradaHistorial {
            id: self.id,
            status,
            tipo,
            lado: self.lado,
            ticker: self.ticker,
            monto: dinero::con_escala(self.monto),
            precio_limite: self.precio_limite.map(dinero::con_escala),
            precio_stop: self.precio_stop.map(dinero::con_escala),
            cantidad: self.cantidad.map(dinero::con_escala),
            precio: self.precio.map(dinero::con_escala),
            importe: self.importe.map(dinero::con_escala),
            comision: self.comision.map(dinero::con_escala),
            motivo: self.motivo,
            colocada_en: self.colocada_en,
            cerrada_en: self.cerrada_en,
            orden_origen_id: self.orden_origen_id,
            plan_id: self.plan_id,
        })
    }
}

/// El cursor de la página que sigue a la orden cerrada en `en` con `id`:
/// microsegundos desde la época y el id, sin nada que escapar en una URL.
fn cursor_de(en: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", en.timestamp_micros(), id.simple())
}

fn leer_cursor(cursor: &str) -> AppResult<(DateTime<Utc>, Uuid)> {
    let invalido = || AppError::validacion("el cursor no es válido; pide la primera página sin cursor");
    let (micros, id) = cursor.split_once('_').ok_or_else(invalido)?;
    let micros: i64 = micros.parse().map_err(|_| invalido())?;
    let en = Utc.timestamp_micros(micros).single().ok_or_else(invalido)?;
    let id = Uuid::try_parse(id).map_err(|_| invalido())?;
    Ok((en, id))
}

/// Una página del historial del cliente, de la más reciente a la más antigua.
pub async fn listar(
    pool: &PgPool,
    cliente_id: Uuid,
    limite: Option<i64>,
    cursor: Option<&str>,
) -> AppResult<HistorialOrdenes> {
    let limite = limite.unwrap_or(PAGINA);
    if !(1..=MAXIMO_PAGINA).contains(&limite) {
        return Err(AppError::validacion(format!("limit va de 1 a {MAXIMO_PAGINA}")));
    }
    let desde = cursor.map(leer_cursor).transpose()?;

    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let mut filas: Vec<Fila> = sqlx::query_as(
        "SELECT * FROM ( \
            SELECT o.id, 'EJECUTADA' AS estado, \
                   CASE WHEN p.id IS NOT NULL THEN p.tipo WHEN e.plan_id IS NOT NULL THEN 'PLAN' ELSE 'MERCADO' END \
                     AS tipo, \
                   o.lado, o.ticker, o.monto_solicitado AS monto, p.precio_limite, p.precio_stop, \
                   o.cantidad, o.precio, o.monto AS importe, o.comision, NULL::text AS motivo, \
                   coalesce(p.creado_en, o.creado_en) AS colocada_en, o.creado_en AS cerrada_en, \
                   p.id AS orden_origen_id, e.plan_id \
              FROM mercado.ordenes o \
              LEFT JOIN mercado.ordenes_pendientes p ON p.orden_ejecutada_id = o.id \
              LEFT JOIN mercado.eventos_plan e ON e.orden_id = o.id AND e.evento = 'EJECUTADO' \
             WHERE o.cliente_id = $1 \
            UNION ALL \
            SELECT p.id, p.estado, p.tipo, p.lado, p.ticker, p.monto, p.precio_limite, p.precio_stop, \
                   NULL, NULL, NULL, NULL, p.motivo, p.creado_en, coalesce(p.cerrado_en, p.creado_en), NULL, NULL \
              FROM mercado.ordenes_pendientes p \
             WHERE p.cliente_id = $1 AND p.estado IN ('CANCELADA', 'RECHAZADA') \
         ) h \
         WHERE $2::timestamptz IS NULL OR (h.cerrada_en, h.id) < ($2, $3) \
         ORDER BY h.cerrada_en DESC, h.id DESC \
         LIMIT $4",
    )
    .bind(cliente_id)
    .bind(desde.map(|(en, _)| en))
    .bind(desde.map(|(_, id)| id))
    .bind(limite + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    // Una de más dice si hay otra página.
    let hay_mas = filas.len() as i64 > limite;
    filas.truncate(limite as usize);
    let next_cursor = if hay_mas { filas.last().map(|f| cursor_de(f.cerrada_en, f.id)) } else { None };
    let ordenes = filas.into_iter().map(Fila::entrada).collect::<AppResult<Vec<_>>>()?;
    Ok(HistorialOrdenes { ordenes, next_cursor })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn el_cursor_va_y_vuelve() {
        let en = Utc.timestamp_micros(1_795_546_401_502_123).single().unwrap();
        let id = Uuid::parse_str("9a000000-0000-0000-0000-000000000001").unwrap();
        let cursor = cursor_de(en, id);
        assert_eq!(cursor, "1795546401502123_9a000000000000000000000000000001");
        assert_eq!(leer_cursor(&cursor).unwrap(), (en, id));
        for malo in ["", "123", "abc_9a000000000000000000000000000001", "123_no-es-un-uuid", "_", "1_2_3"] {
            assert!(leer_cursor(malo).is_err(), "{malo}");
        }
    }
}
