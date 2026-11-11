//! Filas del esquema `mercado` (migraciones 0013 y 0015) y lo que viaja al navegador.
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

/// Un activo del catálogo (`MarketListing`): su ficha y de dónde viene su precio.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivoCatalogo {
    #[serde(flatten)]
    pub activo: ActivoMercado,
    #[serde(rename = "price_source")]
    pub fuente: &'static str,
}

impl From<&Activo> for ActivoCatalogo {
    fn from(a: &Activo) -> Self {
        Self { activo: ActivoMercado::from(a), fuente: a.fuente_precio.etiqueta() }
    }
}

/// `GET /api/v1/market/assets` (`MarketCatalog`): lo que hoy se cotiza y se
/// opera, por ticker.
#[derive(Debug, Clone, Serialize)]
pub struct Catalogo {
    #[serde(rename = "assets")]
    pub activos: Vec<ActivoCatalogo>,
    /// Cuándo se leyeron los precios.
    #[serde(rename = "as_of")]
    pub al: DateTime<Utc>,
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

/// Estado de una orden de mercado: se ejecuta completa al colocarse. Las
/// órdenes límite y stop esperan ([`EstadoPendiente`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoOrden {
    #[serde(rename = "filled")]
    Ejecutada,
}

/// `POST /api/v1/market/order` de una orden de mercado (`OrderReceipt`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recibo {
    #[serde(rename = "order_id")]
    pub orden_id: Uuid,
    /// Siempre `market`: distingue el recibo de una orden límite o stop
    /// ([`VistaPendiente`]) en la misma respuesta.
    #[serde(rename = "order_type")]
    pub tipo: &'static str,
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
            tipo: "market",
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

// -----------------------------------------------------------------------------
// Órdenes límite y stop (migración 0015)
// -----------------------------------------------------------------------------

/// Tipo de una orden que espera a su precio (`mercado.ordenes_pendientes.tipo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum TipoPendiente {
    /// Compra a su precio límite o menos; vende a su precio límite o más.
    #[sqlx(rename = "LIMITE")]
    #[serde(rename = "limit")]
    Limite,
    /// Se dispara cuando el precio alcanza su stop: una compra al subir a él,
    /// una venta al bajar a él. Se ejecuta al precio vigente.
    #[sqlx(rename = "STOP")]
    #[serde(rename = "stop")]
    Stop,
}

impl TipoPendiente {
    pub const fn as_str(self) -> &'static str {
        match self {
            TipoPendiente::Limite => "LIMITE",
            TipoPendiente::Stop => "STOP",
        }
    }
}

/// Estado de una orden límite o stop: solo avanza (de PENDIENTE a uno de los otros tres).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum EstadoPendiente {
    #[sqlx(rename = "PENDIENTE")]
    #[serde(rename = "pending")]
    Pendiente,
    #[sqlx(rename = "EJECUTADA")]
    #[serde(rename = "filled")]
    Ejecutada,
    #[sqlx(rename = "CANCELADA")]
    #[serde(rename = "cancelled")]
    Cancelada,
    #[sqlx(rename = "RECHAZADA")]
    #[serde(rename = "rejected")]
    Rechazada,
}

impl EstadoPendiente {
    pub const fn as_str(self) -> &'static str {
        match self {
            EstadoPendiente::Pendiente => "PENDIENTE",
            EstadoPendiente::Ejecutada => "EJECUTADA",
            EstadoPendiente::Cancelada => "CANCELADA",
            EstadoPendiente::Rechazada => "RECHAZADA",
        }
    }
}

/// `mercado.ordenes_pendientes`: una orden límite o stop. Sus términos no
/// cambian; al cerrarse guarda cómo (y, si se ejecutó, con qué orden).
#[derive(Debug, Clone, FromRow)]
pub struct OrdenPendiente {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub cuenta_ifpe_id: Uuid,
    pub ticker: String,
    pub lado: Lado,
    pub tipo: TipoPendiente,
    /// Pesos con centavos: cuánto invertir (compra) o cuánto vender.
    pub monto: Decimal,
    pub precio_limite: Option<Decimal>,
    pub precio_stop: Option<Decimal>,
    /// Compra: todo el monto, comprometido en la cuenta IFPE.
    pub efectivo_comprometido: Decimal,
    /// Venta: los títulos que el monto vende al precio de la orden.
    pub titulos_comprometidos: Decimal,
    pub estado: EstadoPendiente,
    /// Código de la regla que la rechazó al dispararse.
    pub motivo: Option<String>,
    pub tx_compromiso_id: Option<Uuid>,
    pub tx_liberacion_id: Option<Uuid>,
    /// La orden del ledger del mercado que la ejecutó.
    pub orden_ejecutada_id: Option<Uuid>,
    pub clave_idempotencia: String,
    pub creado_en: DateTime<Utc>,
    pub cerrado_en: Option<DateTime<Utc>>,
}

impl OrdenPendiente {
    /// Su precio límite o su precio stop (la base de datos exige el que corresponde).
    pub fn precio(&self) -> Decimal {
        match self.tipo {
            TipoPendiente::Limite => self.precio_limite,
            TipoPendiente::Stop => self.precio_stop,
        }
        .unwrap_or_default()
    }

    /// ¿Se ejecuta a `precio`? Una límite, a su precio o mejor; una stop, ya
    /// alcanzado su precio. La base de datos aplica la misma regla a la orden
    /// que la ejecuta (`mercado.fn_validar_orden`, 0015).
    pub fn disparada(&self, precio: Decimal) -> bool {
        disparada(self.tipo, self.lado, self.precio(), precio)
    }
}

/// [`OrdenPendiente::disparada`] con los términos sueltos.
pub fn disparada(tipo: TipoPendiente, lado: Lado, precio_orden: Decimal, precio: Decimal) -> bool {
    match (tipo, lado) {
        (TipoPendiente::Limite, Lado::Compra) => precio <= precio_orden,
        (TipoPendiente::Limite, Lado::Venta) => precio >= precio_orden,
        (TipoPendiente::Stop, Lado::Compra) => precio >= precio_orden,
        (TipoPendiente::Stop, Lado::Venta) => precio <= precio_orden,
    }
}

/// La ejecución de una orden límite o stop (`OrderExecution`): la orden del
/// ledger del mercado que la cumplió.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EjecucionPendiente {
    /// Id de la orden ejecutada en `mercado.ordenes`.
    #[serde(rename = "order_id")]
    pub orden_id: Uuid,
    #[serde(rename = "quantity")]
    pub cantidad: Decimal,
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

impl From<&Orden> for EjecucionPendiente {
    fn from(o: &Orden) -> Self {
        Self {
            orden_id: o.id,
            cantidad: dinero::con_escala(o.cantidad),
            precio: dinero::con_escala(o.precio),
            monto: dinero::con_escala(o.monto),
            comision: dinero::con_escala(o.comision),
            ejecutada_en: o.creado_en,
        }
    }
}

/// Una orden límite o stop como la lee el frontend (`PendingOrder`): la
/// respuesta de colocarla o cancelarla y cada renglón de
/// `GET /api/v1/market/orders/pending`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VistaPendiente {
    #[serde(rename = "order_id")]
    pub id: Uuid,
    #[serde(rename = "order_type")]
    pub tipo: TipoPendiente,
    pub status: EstadoPendiente,
    pub ticker: String,
    #[serde(rename = "side")]
    pub lado: Lado,
    /// Lo que pidió el cliente, en MXN.
    #[serde(rename = "amount")]
    pub monto: Decimal,
    #[serde(rename = "limit_price")]
    pub precio_limite: Option<Decimal>,
    #[serde(rename = "stop_price")]
    pub precio_stop: Option<Decimal>,
    /// Efectivo comprometido ahora (compras pendientes; 0 al cerrarse).
    #[serde(rename = "held_cash")]
    pub efectivo_comprometido: Decimal,
    /// Títulos comprometidos ahora (ventas pendientes; 0 al cerrarse).
    #[serde(rename = "held_quantity")]
    pub titulos_comprometidos: Decimal,
    #[serde(rename = "created_at")]
    pub creada_en: DateTime<Utc>,
    #[serde(rename = "closed_at")]
    pub cerrada_en: Option<DateTime<Utc>>,
    /// Código de la regla que la rechazó (`rejected`).
    #[serde(rename = "reject_reason")]
    pub motivo: Option<String>,
    /// La orden que la ejecutó (`filled`).
    #[serde(rename = "execution")]
    pub ejecucion: Option<EjecucionPendiente>,
}

impl VistaPendiente {
    pub fn de(p: &OrdenPendiente, ejecucion: Option<&Orden>) -> Self {
        let abierta = p.estado == EstadoPendiente::Pendiente;
        let ahora = |d: Decimal| dinero::con_escala(if abierta { d } else { Decimal::ZERO });
        Self {
            id: p.id,
            tipo: p.tipo,
            status: p.estado,
            ticker: p.ticker.clone(),
            lado: p.lado,
            monto: dinero::con_escala(p.monto),
            precio_limite: p.precio_limite.map(dinero::con_escala),
            precio_stop: p.precio_stop.map(dinero::con_escala),
            efectivo_comprometido: ahora(p.efectivo_comprometido),
            titulos_comprometidos: ahora(p.titulos_comprometidos),
            creada_en: p.creado_en,
            cerrada_en: p.cerrado_en,
            motivo: p.motivo.clone(),
            ejecucion: ejecucion.map(EjecucionPendiente::from),
        }
    }
}

/// `GET /api/v1/market/orders/pending` (`PendingOrders`): las órdenes que
/// esperan, de la más nueva a la más antigua.
#[derive(Debug, Clone, Serialize)]
pub struct OrdenesPendientes {
    #[serde(rename = "orders")]
    pub ordenes: Vec<VistaPendiente>,
}
