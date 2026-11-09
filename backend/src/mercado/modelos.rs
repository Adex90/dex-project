//! Filas del esquema `mercado` (migración 0013) y lo que viaja al navegador.
//!
//! Lo que se serializa lleva los nombres del contrato
//! (`frontend/src/types/market.ts`), en inglés; los importes son decimales
//! exactos que viajan como texto (rust_decimal con `serde-with-str`).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use super::{calculo, historial::PuntoPrecio};
use crate::dinero;

/// Clase de activo (`mercado.activos.clase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaseActivo {
    MateriaPrima,
    RentaFija,
    RentaVariable,
    Fondo,
}

impl ClaseActivo {
    /// Cómo la muestra el contrato (`MarketAsset.asset_class`).
    pub const fn etiqueta(self) -> &'static str {
        match self {
            ClaseActivo::MateriaPrima => "Commodity",
            ClaseActivo::RentaFija => "Fixed income",
            ClaseActivo::RentaVariable => "Equity",
            ClaseActivo::Fondo => "Fund",
        }
    }
}

/// De dónde viene el precio. SIMULADO hasta el oráculo de precios: solo se
/// cotiza y opera con MODO_DEMO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FuentePrecio {
    Simulado,
    Oraculo,
}

impl FuentePrecio {
    pub const fn as_str(self) -> &'static str {
        match self {
            FuentePrecio::Simulado => "SIMULADO",
            FuentePrecio::Oraculo => "ORACULO",
        }
    }

    /// `MarketQuote.price_source`.
    pub const fn etiqueta(self) -> &'static str {
        match self {
            FuentePrecio::Simulado => "simulated",
            FuentePrecio::Oraculo => "oracle",
        }
    }
}

/// Lado de una orden: `COMPRA`/`VENTA` en la base de datos, `buy`/`sell` en la API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum Lado {
    #[sqlx(rename = "COMPRA")]
    #[serde(rename = "buy")]
    Compra,
    #[sqlx(rename = "VENTA")]
    #[serde(rename = "sell")]
    Venta,
}

impl Lado {
    pub const fn as_str(self) -> &'static str {
        match self {
            Lado::Compra => "COMPRA",
            Lado::Venta => "VENTA",
        }
    }
}

/// `mercado.activos`.
#[derive(Debug, Clone, FromRow)]
pub struct Activo {
    pub ticker: String,
    pub nombre: String,
    pub clase: ClaseActivo,
    pub decimales_cantidad: i16,
    pub precio: Decimal,
    pub precio_cierre_previo: Decimal,
    pub capitalizacion: Option<Decimal>,
    pub volumen_24h: Option<Decimal>,
    pub rendimiento_anual: Option<Decimal>,
    pub fuente_precio: FuentePrecio,
    pub precio_en: DateTime<Utc>,
    pub operable: bool,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

impl Activo {
    /// Decimales del incremento de la cantidad (0 a 8, por restricción).
    pub fn decimales(&self) -> u32 {
        u32::try_from(self.decimales_cantidad).unwrap_or(0)
    }
}

/// `mercado.ordenes`: una orden ejecutada, renglón del ledger del mercado.
#[derive(Debug, Clone, FromRow)]
pub struct Orden {
    pub id: Uuid,
    pub secuencia: i64,
    pub cliente_id: Uuid,
    pub cuenta_ifpe_id: Uuid,
    pub ticker: String,
    pub lado: Lado,
    pub monto_solicitado: Decimal,
    pub cantidad: Decimal,
    pub precio: Decimal,
    pub monto: Decimal,
    pub comision: Decimal,
    pub costo_liberado: Option<Decimal>,
    pub fuente_precio: FuentePrecio,
    pub tx_ifpe_id: Uuid,
    pub clave_idempotencia: String,
    pub hash_anterior: String,
    pub tx_hash: String,
    pub creado_en: DateTime<Utc>,
}

/// Un activo como lo lee el frontend (`MarketAsset`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivoMercado {
    pub ticker: String,
    #[serde(rename = "name")]
    pub nombre: String,
    #[serde(rename = "asset_class")]
    pub clase: &'static str,
    #[serde(rename = "price")]
    pub precio: Decimal,
    /// Contra el cierre anterior, en por ciento: "0.84" es +0.84 %.
    #[serde(rename = "change_24h_pct")]
    pub cambio_24h: Decimal,
    #[serde(rename = "market_cap")]
    pub capitalizacion: Option<Decimal>,
    #[serde(rename = "volume_24h")]
    pub volumen_24h: Option<Decimal>,
    /// Rendimiento anual en por ciento; null si el activo no paga.
    #[serde(rename = "yield_pct")]
    pub rendimiento: Option<Decimal>,
    #[serde(rename = "quantity_decimals")]
    pub decimales_cantidad: u32,
}

impl From<&Activo> for ActivoMercado {
    fn from(a: &Activo) -> Self {
        Self {
            ticker: a.ticker.clone(),
            nombre: a.nombre.clone(),
            clase: a.clase.etiqueta(),
            precio: dinero::con_escala(a.precio),
            cambio_24h: calculo::cambio_pct(a.precio, a.precio_cierre_previo),
            capitalizacion: a.capitalizacion,
            volumen_24h: a.volumen_24h,
            rendimiento: a.rendimiento_anual.map(|r| {
                let mut r = r.round_dp(2);
                r.rescale(2);
                r
            }),
            decimales_cantidad: a.decimales(),
        }
    }
}

/// `GET /api/v1/market/quote/{ticker}` (`MarketQuote`).
#[derive(Debug, Clone, Serialize)]
pub struct Cotizacion {
    #[serde(rename = "asset")]
    pub activo: ActivoMercado,
    /// Del más antiguo al más reciente; el último punto es el precio vigente.
    #[serde(rename = "history")]
    pub historial: Vec<PuntoPrecio>,
    /// Cuándo se leyó el precio: el borde derecho de cada rango de la gráfica.
    #[serde(rename = "as_of")]
    pub al: DateTime<Utc>,
    #[serde(rename = "price_source")]
    pub fuente: &'static str,
}

/// Estado de una orden. Una orden de mercado se ejecuta completa al colocarse;
/// `pending` queda para las órdenes límite (módulo 3 de la hoja de ruta).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoOrden {
    #[serde(rename = "filled")]
    Ejecutada,
}

/// `POST /api/v1/market/order` (`OrderReceipt`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recibo {
    #[serde(rename = "order_id")]
    pub orden_id: Uuid,
    pub status: EstadoOrden,
    pub ticker: String,
    #[serde(rename = "side")]
    pub lado: Lado,
    /// Unidades compradas o vendidas.
    #[serde(rename = "quantity")]
    pub cantidad: Decimal,
    /// Precio de ejecución.
    #[serde(rename = "price")]
    pub precio: Decimal,
    /// Lo que se cargó o abonó a la bóveda: `quantity × price` al centavo.
    #[serde(rename = "amount")]
    pub monto: Decimal,
    #[serde(rename = "commission")]
    pub comision: Decimal,
    #[serde(rename = "executed_at")]
    pub ejecutada_en: DateTime<Utc>,
}

impl From<&Orden> for Recibo {
    fn from(o: &Orden) -> Self {
        Self {
            orden_id: o.id,
            status: EstadoOrden::Ejecutada,
            ticker: o.ticker.clone(),
            lado: o.lado,
            cantidad: dinero::con_escala(o.cantidad),
            precio: dinero::con_escala(o.precio),
            monto: dinero::con_escala(o.monto),
            comision: dinero::con_escala(o.comision),
            ejecutada_en: o.creado_en,
        }
    }
}
