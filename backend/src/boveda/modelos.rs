//! Lo que viaja al navegador. Los campos llevan los nombres del contrato
//! (frontend/src/types/vault.ts), en inglés; los importes son decimales
//! exactos que se serializan como texto (rust_decimal con `serde-with-str`),
//! nunca `f64`.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// La bóveda completa: el cuerpo de `GET /api/v1/vault` y el `vault` de un
/// mensaje `vault.snapshot`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Boveda {
    #[serde(rename = "user_id")]
    pub cliente_id: Uuid,
    pub clabe: String,
    /// Institución que lleva la CLABE (`SPEI_BANCO_NOMBRE`).
    #[serde(rename = "bank_name")]
    pub banco: String,
    #[serde(rename = "balance")]
    pub saldo: SaldoBoveda,
    #[serde(rename = "reserve_proof")]
    pub prueba_reserva: PruebaReserva,
    /// Las posiciones del mercado de valores (migración 0013), al precio vigente.
    #[serde(rename = "holdings")]
    pub posiciones: Vec<Posicion>,
}

/// Los totales, en MXN.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SaldoBoveda {
    /// Patrimonio: efectivo (disponible, comprometido en órdenes y retenido)
    /// más lo invertido. Con una retención PLD, `available_cash` + `held_cash`
    /// + `invested_assets` es menor que el total.
    #[serde(rename = "total_mxn")]
    pub total: Decimal,
    /// Libre disposición: `saldo_disponible` de la cuenta IFPE.
    #[serde(rename = "available_cash")]
    pub disponible: Decimal,
    /// Comprometido en compras límite y stop pendientes (`saldo_comprometido`,
    /// migración 0015): sigue siendo del cliente, pero no se puede gastar
    /// hasta que la orden se ejecute o se cancele.
    #[serde(rename = "held_cash")]
    pub comprometido: Decimal,
    /// Principal en mutuos SOFOM vigentes (o vencidos y aún por liquidar) más
    /// el valor de las posiciones del mercado al precio vigente.
    #[serde(rename = "invested_assets")]
    pub invertido: Decimal,
    /// Ganancia o pérdida no realizada sobre lo invertido, en por ciento:
    /// (valor de las posiciones − su costo) / (mutuos + costo de las
    /// posiciones). Los mutuos se llevan a costo y su interés se realiza al
    /// liquidarse, así que solo el mercado la mueve; 0.00 sin inversiones.
    #[serde(rename = "unrealized_pnl_percentage")]
    pub rendimiento_no_realizado: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoPrueba {
    /// El último corte cubre el 100 % de los depósitos y la reserva sigue al 100 %.
    #[serde(rename = "verified")]
    Verificada,
    /// Aún no se publica ningún corte.
    #[serde(rename = "pending")]
    Pendiente,
    /// El último corte, o la reserva en vivo, no cubre los depósitos.
    #[serde(rename = "failed")]
    Fallida,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PruebaReserva {
    #[serde(rename = "status")]
    pub estado: EstadoPrueba,
    /// Raíz Merkle del último corte de Proof of Reserve (64 caracteres hex;
    /// ceros mientras no hay corte).
    #[serde(rename = "zk_root_hash")]
    pub raiz: String,
}

/// Una posición del portafolio en el mercado de valores.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Posicion {
    pub ticker: String,
    #[serde(rename = "name")]
    pub nombre: String,
    /// Todos los títulos del cliente, comprometidos incluidos.
    #[serde(rename = "shares")]
    pub titulos: Decimal,
    /// Los comprometidos en ventas límite y stop pendientes (0015): una venta
    /// de mercado solo vende `shares` − `held_shares`.
    #[serde(rename = "held_shares")]
    pub titulos_comprometidos: Decimal,
    #[serde(rename = "current_price")]
    pub precio: Decimal,
    #[serde(rename = "total_value")]
    pub valor: Decimal,
    #[serde(rename = "daily_change_pct")]
    pub cambio_diario: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Direccion {
    #[serde(rename = "inbound")]
    Entrante,
    #[serde(rename = "outbound")]
    Saliente,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoTransferencia {
    /// Acreditado en la cuenta.
    #[serde(rename = "settled")]
    Liquidada,
    /// Orden saliente reembolsada (rechazada o devuelta).
    #[serde(rename = "returned")]
    Devuelta,
}

/// El SPEI que movió el saldo, para el mensaje `spei.transfer`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransferenciaSpei {
    #[serde(rename = "tracking_key")]
    pub clave_rastreo: String,
    #[serde(rename = "direction")]
    pub direccion: Direccion,
    #[serde(rename = "status")]
    pub estado: EstadoTransferencia,
    #[serde(rename = "amount")]
    pub monto: Decimal,
    #[serde(rename = "counterparty_name")]
    pub contraparte: String,
    /// Banco de la contraparte: su clave de 3 dígitos, la de la CLABE (los
    /// últimos 3 de la clave de participante SPEI: 012 = BBVA México), en
    /// los dos sentidos. Sin catálogo de nombres por ahora.
    #[serde(rename = "counterparty_bank")]
    pub banco_contraparte: String,
    #[serde(rename = "occurred_at")]
    pub ocurrio_en: DateTime<Utc>,
}

/// Mensajes del servidor por el WebSocket (en el frontend,
/// `VaultRealtimeMessage`). Cada uno lleva la versión de la cuenta (`seq`)
/// cuyos valores trae.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum Mensaje {
    /// El estado completo: al conectar, al resincronizar, con un corte de
    /// reserva nuevo (con la misma `seq`) y con cualquier cambio de saldo
    /// que no sea un SPEI.
    #[serde(rename = "vault.snapshot")]
    Instantanea {
        seq: i64,
        #[serde(rename = "vault")]
        boveda: Boveda,
    },
    /// Un SPEI acreditado o reembolsado, con el saldo que dejó.
    #[serde(rename = "spei.transfer")]
    Spei {
        seq: i64,
        #[serde(rename = "transfer")]
        transferencia: TransferenciaSpei,
        #[serde(rename = "balance")]
        saldo: SaldoBoveda,
    },
}

impl Mensaje {
    pub fn seq(&self) -> i64 {
        match self {
            Self::Instantanea { seq, .. } | Self::Spei { seq, .. } => *seq,
        }
    }
}

/// Lo único que el navegador envía: `{"type": "resync"}`, cuando ve un hueco
/// en `seq`. Cualquier otra cosa se ignora.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MensajeCliente {
    #[serde(rename = "type")]
    pub tipo: String,
}

impl MensajeCliente {
    pub fn pide_resincronizar(texto: &str) -> bool {
        serde_json::from_str::<Self>(texto).is_ok_and(|m| m.tipo == "resync")
    }
}
