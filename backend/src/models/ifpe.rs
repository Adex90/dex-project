//! Esquema `ifpe`: bóveda a la vista con reserva del 100%.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use super::EstadoCuenta;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CuentaIfpe {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub tipo_entidad: String,
    pub clabe: String,
    pub moneda: String,
    pub saldo_disponible: Decimal,
    pub saldo_bloqueado: Decimal,
    pub estado: EstadoCuenta,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ReservaCustodia {
    pub id: Uuid,
    pub institucion: String,
    pub tipo: String,
    pub numero_cuenta: String,
    pub moneda: String,
    pub saldo: Decimal,
    pub activa: bool,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

/// Fila única `ifpe.estado_reserva`, mantenida por triggers.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct EstadoReserva {
    pub pasivo_clientes: Decimal,
    pub activo_reserva: Decimal,
    pub coeficiente_reserva: Decimal,
    pub actualizado_en: DateTime<Utc>,
}

/// Fila única `ifpe.ledger_cabeza` / `sofom.ledger_cabeza`.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct LedgerCabeza {
    pub ultima_secuencia: i64,
    pub ultimo_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum TipoTxIfpe {
    #[sqlx(rename = "DEPOSITO")]
    #[serde(rename = "DEPOSITO")]
    Deposito,
    #[sqlx(rename = "RETIRO")]
    #[serde(rename = "RETIRO")]
    Retiro,
    #[sqlx(rename = "TRANSFERENCIA")]
    #[serde(rename = "TRANSFERENCIA")]
    Transferencia,
    #[sqlx(rename = "TRASPASO_A_SOFOM")]
    #[serde(rename = "TRASPASO_A_SOFOM")]
    TraspasoASofom,
    #[sqlx(rename = "RETORNO_DE_SOFOM")]
    #[serde(rename = "RETORNO_DE_SOFOM")]
    RetornoDeSofom,
    #[sqlx(rename = "RETENCION")]
    #[serde(rename = "RETENCION")]
    Retencion,
    #[sqlx(rename = "LIBERACION_RETENCION")]
    #[serde(rename = "LIBERACION_RETENCION")]
    LiberacionRetencion,
}

impl TipoTxIfpe {
    pub const fn as_str(self) -> &'static str {
        match self {
            TipoTxIfpe::Deposito => "DEPOSITO",
            TipoTxIfpe::Retiro => "RETIRO",
            TipoTxIfpe::Transferencia => "TRANSFERENCIA",
            TipoTxIfpe::TraspasoASofom => "TRASPASO_A_SOFOM",
            TipoTxIfpe::RetornoDeSofom => "RETORNO_DE_SOFOM",
            TipoTxIfpe::Retencion => "RETENCION",
            TipoTxIfpe::LiberacionRetencion => "LIBERACION_RETENCION",
        }
    }
}

/// Movimiento del ledger IFPE (inmutable, encadenado por `tx_hash`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TransaccionIfpe {
    pub id: Uuid,
    pub secuencia: i64,
    pub tipo: TipoTxIfpe,
    pub cuenta_origen_id: Option<Uuid>,
    pub cuenta_destino_id: Option<Uuid>,
    pub reserva_id: Option<Uuid>,
    pub monto: Decimal,
    pub moneda: String,
    pub concepto: Option<String>,
    pub referencia_externa: Option<String>,
    pub clave_idempotencia: String,
    pub ref_movimiento_espejo: Option<Uuid>,
    pub hash_anterior: String,
    pub tx_hash: String,
    pub creado_en: DateTime<Utc>,
}

/// Corte publicado de Proof of Reserve.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CorteReserva {
    pub id: Uuid,
    pub merkle_root: String,
    pub num_hojas: i32,
    pub total_pasivo: Decimal,
    pub total_reserva: Decimal,
    pub secuencia_ledger: i64,
    pub hash_ledger: String,
    pub algoritmo: String,
    pub creado_en: DateTime<Utc>,
}

/// Hoja de un corte (cuenta + saldo + sal).
#[derive(Debug, Clone, FromRow)]
pub struct HojaCorte {
    pub corte_id: Uuid,
    pub indice: i32,
    pub cuenta_id: Uuid,
    pub saldo_total: Decimal,
    pub sal: String,
    pub hoja_hash: String,
}
