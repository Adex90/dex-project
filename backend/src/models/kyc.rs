//! Esquema `kyc`: clientes, credenciales y alertas PLD/FT.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TipoPersona {
    Fisica,
    Moral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EstatusKyc {
    Pendiente,
    EnRevision,
    Verificado,
    Rechazado,
    Suspendido,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NivelRiesgo {
    Bajo,
    Medio,
    Alto,
}

/// Rol de la persona autenticada (columna `kyc.credenciales.rol`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RolUsuario {
    Cliente,
    Operador,
    Auditor,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Cliente {
    pub id: Uuid,
    pub tipo_persona: TipoPersona,
    pub nombre: String,
    pub apellido_paterno: Option<String>,
    pub apellido_materno: Option<String>,
    pub fecha_nacimiento: Option<NaiveDate>,
    pub nacionalidad: String,
    pub pais_residencia: String,
    pub rfc: Option<String>,
    pub curp: Option<String>,
    pub email: String,
    pub telefono: Option<String>,
    pub domicilio: serde_json::Value,
    pub actividad_economica: Option<String>,
    pub ocupacion: Option<String>,
    pub nivel_cuenta: i16,
    pub estatus_kyc: EstatusKyc,
    pub documento_identidad_tipo: Option<String>,
    pub documento_identidad_numero: Option<String>,
    pub kyc_verificado_en: Option<DateTime<Utc>>,
    pub origen_recursos: Option<String>,
    pub destino_recursos: Option<String>,
    pub ingreso_mensual_estimado: Option<Decimal>,
    pub es_pep: bool,
    pub en_lista_bloqueadas: bool,
    pub nivel_riesgo_pld: NivelRiesgo,
    pub riesgo_evaluado_en: Option<DateTime<Utc>>,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

/// Credenciales de acceso. No implementa `Serialize`: el hash nunca sale de aquí.
#[derive(Debug, Clone, FromRow)]
pub struct Credencial {
    pub cliente_id: Uuid,
    pub password_hash: String,
    pub intentos_fallidos: i32,
    pub bloqueado_hasta: Option<DateTime<Utc>>,
    pub ultimo_acceso: Option<DateTime<Utc>>,
    pub rol: RolUsuario,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TipoOperacionPld {
    Relevante,
    Inusual,
    InternaPreocupante,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NivelAlerta {
    Bajo,
    Medio,
    Alto,
    Critico,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EstadoAlerta {
    Abierta,
    EnRevision,
    Descartada,
    ReportadaUif,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AlertaPld {
    pub id: Uuid,
    pub folio: i64,
    pub cliente_id: Uuid,
    pub entidad: super::Entidad,
    pub transaccion_id: Option<Uuid>,
    pub tipo_operacion: TipoOperacionPld,
    pub regla: String,
    pub nivel_riesgo: NivelAlerta,
    pub puntaje: Decimal,
    pub monto: Option<Decimal>,
    pub descripcion: String,
    pub detalle: serde_json::Value,
    pub estado: EstadoAlerta,
    pub analista: Option<String>,
    pub dictamen: Option<String>,
    pub folio_uif: Option<String>,
    pub reportada_uif_en: Option<DateTime<Utc>>,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}
