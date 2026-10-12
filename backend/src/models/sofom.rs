//! Esquema `sofom`: inversión a plazo (mutuo) y crédito con calce de plazos.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use super::EstadoCuenta;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CuentaSofom {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub tipo_entidad: String,
    pub numero_cuenta: i64,
    pub moneda: String,
    pub saldo_disponible: Decimal,
    pub saldo_bloqueado: Decimal,
    pub estado: EstadoCuenta,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Caja {
    pub id: Uuid,
    pub institucion: String,
    pub numero_cuenta: String,
    pub moneda: String,
    pub saldo: Decimal,
    pub activa: bool,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EstadoMutuo {
    Vigente,
    Vencido,
    Liquidado,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ContratoMutuo {
    pub id: Uuid,
    pub folio: i64,
    pub cliente_id: Uuid,
    pub cuenta_sofom_id: Uuid,
    pub cuenta_ifpe_origen_id: Uuid,
    pub tx_aportacion_ifpe_id: Uuid,
    pub principal: Decimal,
    pub tasa_anual: Decimal,
    pub plazo_dias: i32,
    pub fecha_inicio: NaiveDate,
    pub fecha_vencimiento: NaiveDate,
    pub principal_asignado: Decimal,
    pub interes_pagado: Decimal,
    pub estado: EstadoMutuo,
    pub hash_terminos: String,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EstadoCredito {
    EnFondeo,
    Vigente,
    Vencido,
    Liquidado,
    Castigado,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Credito {
    pub id: Uuid,
    pub folio: i64,
    pub acreditado_id: Uuid,
    pub principal: Decimal,
    pub tasa_anual: Decimal,
    pub plazo_dias: i32,
    pub fecha_desembolso: NaiveDate,
    pub fecha_vencimiento: NaiveDate,
    pub saldo_insoluto: Decimal,
    pub estado: EstadoCredito,
    pub destino: Option<String>,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AsignacionFondeo {
    pub id: Uuid,
    pub credito_id: Uuid,
    pub contrato_id: Uuid,
    pub monto: Decimal,
    pub liberada: bool,
    pub creado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum TipoTxSofom {
    #[sqlx(rename = "APORTACION_MUTUO")]
    #[serde(rename = "APORTACION_MUTUO")]
    AportacionMutuo,
    #[sqlx(rename = "VENCIMIENTO_MUTUO")]
    #[serde(rename = "VENCIMIENTO_MUTUO")]
    VencimientoMutuo,
    #[sqlx(rename = "PAGO_INTERES_MUTUO")]
    #[serde(rename = "PAGO_INTERES_MUTUO")]
    PagoInteresMutuo,
    #[sqlx(rename = "RETORNO_A_IFPE")]
    #[serde(rename = "RETORNO_A_IFPE")]
    RetornoAIfpe,
    #[sqlx(rename = "DESEMBOLSO_CREDITO")]
    #[serde(rename = "DESEMBOLSO_CREDITO")]
    DesembolsoCredito,
    #[sqlx(rename = "COBRO_CREDITO")]
    #[serde(rename = "COBRO_CREDITO")]
    CobroCredito,
    #[sqlx(rename = "APORTACION_CAPITAL")]
    #[serde(rename = "APORTACION_CAPITAL")]
    AportacionCapital,
}

impl TipoTxSofom {
    pub const fn as_str(self) -> &'static str {
        match self {
            TipoTxSofom::AportacionMutuo => "APORTACION_MUTUO",
            TipoTxSofom::VencimientoMutuo => "VENCIMIENTO_MUTUO",
            TipoTxSofom::PagoInteresMutuo => "PAGO_INTERES_MUTUO",
            TipoTxSofom::RetornoAIfpe => "RETORNO_A_IFPE",
            TipoTxSofom::DesembolsoCredito => "DESEMBOLSO_CREDITO",
            TipoTxSofom::CobroCredito => "COBRO_CREDITO",
            TipoTxSofom::AportacionCapital => "APORTACION_CAPITAL",
        }
    }
}

/// Movimiento del ledger SOFOM (inmutable, cadena independiente de la IFPE).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TransaccionSofom {
    pub id: Uuid,
    pub secuencia: i64,
    pub tipo: TipoTxSofom,
    pub cuenta_id: Option<Uuid>,
    pub contrato_id: Option<Uuid>,
    pub credito_id: Option<Uuid>,
    pub caja_id: Option<Uuid>,
    pub monto: Decimal,
    pub moneda: String,
    pub concepto: Option<String>,
    pub clave_idempotencia: String,
    pub ref_movimiento_espejo: Option<Uuid>,
    pub hash_anterior: String,
    pub tx_hash: String,
    pub creado_en: DateTime<Utc>,
}
