//! Tipos de patrimonio. Los de la API usan los nombres en inglés del
//! frontend (`frontend/src/types/wealth.ts`).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

/// Registro de `ifpe.cuentas_vinculadas` (migración 0016).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CuentaVinculada {
    pub id: Uuid,
    pub cliente_id: Uuid,
    /// CHAR(18): 18 dígitos.
    pub clabe: String,
    pub banco: String,
    pub titular: String,
    /// DEMO o VERIFICACION.
    pub origen: String,
    /// ACTIVA o BAJA.
    pub estado: String,
    pub creado_en: DateTime<Utc>,
    pub baja_en: Option<DateTime<Utc>>,
}

/// `LinkedAccount`: la cuenta como la ve su cliente. La CLABE completa no
/// sale de la API: basta con sus últimos cuatro dígitos para reconocerla.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VistaCuentaVinculada {
    pub id: Uuid,
    /// Clave de 3 dígitos del banco ("012").
    #[serde(rename = "bank_code")]
    pub banco: String,
    /// Nombre del banco, si está en el catálogo (`null` si no).
    #[serde(rename = "bank_name")]
    pub nombre_banco: Option<&'static str>,
    #[serde(rename = "clabe_last4")]
    pub clabe_final: String,
    /// El titular: el cliente, con su nombre del expediente KYC.
    #[serde(rename = "holder_name")]
    pub titular: String,
    #[serde(rename = "linked_at")]
    pub vinculada_en: DateTime<Utc>,
}

/// `LinkedAccounts`: las cuentas vinculadas activas, la más antigua primero.
#[derive(Debug, Clone, Serialize)]
pub struct CuentasVinculadas {
    #[serde(rename = "accounts")]
    pub cuentas: Vec<VistaCuentaVinculada>,
}

/// `WithdrawalStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoRetiro {
    /// La orden espera o viaja por la red SPEI (PENDIENTE o ENVIADA): el
    /// dinero ya salió del disponible.
    #[serde(rename = "processing")]
    EnProceso,
    /// Liquidada: el dinero llegó al banco del cliente.
    #[serde(rename = "settled")]
    Liquidado,
    /// Rechazada o devuelta: el dinero regresó al disponible.
    #[serde(rename = "returned")]
    Devuelto,
}

/// `Withdrawal`.
#[derive(Debug, Clone, Serialize)]
pub struct VistaRetiro {
    /// La orden SPEI.
    pub id: Uuid,
    #[serde(rename = "status")]
    pub estado: EstadoRetiro,
    /// MXN, escala 8.
    #[serde(rename = "amount")]
    pub monto: Decimal,
    #[serde(rename = "linked_account")]
    pub cuenta: VistaCuentaVinculada,
    /// La clave de rastreo SPEI: con ella el banco del cliente (o el CEP de
    /// Banxico) encuentra el pago.
    #[serde(rename = "tracking_key")]
    pub clave_rastreo: String,
    #[serde(rename = "requested_at")]
    pub solicitado_en: DateTime<Utc>,
}

/// `WithdrawalReceipt`: la respuesta de `POST /api/v1/wealth/withdraw`.
#[derive(Debug, Clone, Serialize)]
pub struct ReciboRetiro {
    #[serde(rename = "withdrawal")]
    pub retiro: VistaRetiro,
}
