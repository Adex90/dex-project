//! Esquema `kyc`: clientes y credenciales.

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
    /// Oficial de cumplimiento PLD/FT: consola `/api/v1/aml`.
    Cumplimiento,
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

/// Proyección de [`Cliente`] para el propio cliente (`/yo`, `/tablero`,
/// `/auth/registro`) y para el operador (respuesta del dictamen KYC): sin
/// `nivel_riesgo_pld`, `es_pep` ni `riesgo_evaluado_en`. Un dictamen de
/// cumplimiento sube el nivel de riesgo; mostrarlo revelaría la existencia de
/// una alerta (las Disposiciones prohíben avisar al cliente, y el operador no
/// tiene acceso a PLD). `actualizado_en` no se mueve cuando solo cambia el
/// nivel (`trg_clientes_zz_aml_riesgo`, migración 0009).
#[derive(Debug, Clone, Serialize)]
pub struct ClienteVista {
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
    pub en_lista_bloqueadas: bool,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

impl From<Cliente> for ClienteVista {
    fn from(c: Cliente) -> Self {
        Self {
            id: c.id,
            tipo_persona: c.tipo_persona,
            nombre: c.nombre,
            apellido_paterno: c.apellido_paterno,
            apellido_materno: c.apellido_materno,
            fecha_nacimiento: c.fecha_nacimiento,
            nacionalidad: c.nacionalidad,
            pais_residencia: c.pais_residencia,
            rfc: c.rfc,
            curp: c.curp,
            email: c.email,
            telefono: c.telefono,
            domicilio: c.domicilio,
            actividad_economica: c.actividad_economica,
            ocupacion: c.ocupacion,
            nivel_cuenta: c.nivel_cuenta,
            estatus_kyc: c.estatus_kyc,
            documento_identidad_tipo: c.documento_identidad_tipo,
            documento_identidad_numero: c.documento_identidad_numero,
            kyc_verificado_en: c.kyc_verificado_en,
            origen_recursos: c.origen_recursos,
            destino_recursos: c.destino_recursos,
            ingreso_mensual_estimado: c.ingreso_mensual_estimado,
            en_lista_bloqueadas: c.en_lista_bloqueadas,
            creado_en: c.creado_en,
            actualizado_en: c.actualizado_en,
        }
    }
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
