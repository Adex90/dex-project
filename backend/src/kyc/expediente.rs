//! Expediente KYC del cliente (`kyc.kyc_perfiles`, rol `sc_kyc`).
//!
//! 1. [`guardar_datos`]: datos personales y domicilio (paso 1 del onboarding).
//!    Fecha de nacimiento, sexo y entidad se derivan de la CURP.
//! 2. [`enviar`]: datos de la INE + imágenes (anverso, reverso y cuadros de la
//!    selfie) -> el expediente pasa a EN_REVISION. Las imágenes no se guardan:
//!    solo sus huellas SHA-256, que el resultado del proveedor debe repetir.
//!
//! El rol `sc_kyc` solo ve como editable un expediente PENDIENTE (RLS): una
//! actualización sobre otro estado afecta 0 filas y aquí se traduce en 409.

use std::net::IpAddr;

use base64::Engine;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{
    curp::{Curp, Sexo},
    ine::{self, CredencialDeclarada, IdentidadDeclarada},
    rfc, texto,
};
use crate::{
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    models::kyc::{EstatusKyc, TipoPersona},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EstadoKyc {
    Pendiente,
    EnRevision,
    Verificado,
    Rechazado,
}

impl EstadoKyc {
    pub fn como_texto(self) -> &'static str {
        match self {
            EstadoKyc::Pendiente => "PENDIENTE",
            EstadoKyc::EnRevision => "EN_REVISION",
            EstadoKyc::Verificado => "VERIFICADO",
            EstadoKyc::Rechazado => "RECHAZADO",
        }
    }
}

/// Fila de `kyc.kyc_perfiles` (sin IP ni huellas de las imágenes).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Expediente {
    pub cliente_id: Uuid,
    pub nombres: Option<String>,
    pub apellido_paterno: Option<String>,
    pub apellido_materno: Option<String>,
    pub apellidos: Option<String>,
    pub curp: Option<String>,
    pub rfc: Option<String>,
    pub fecha_nacimiento: Option<NaiveDate>,
    pub sexo: Option<String>,
    pub entidad_nacimiento: Option<String>,
    pub nacionalidad: String,
    pub domicilio: serde_json::Value,
    pub ine_clave_elector: Option<String>,
    pub ine_cic: Option<String>,
    pub ine_identificador_ciudadano: Option<String>,
    pub ine_emision: Option<i16>,
    pub ine_vigencia: Option<i16>,
    pub estado_kyc: EstadoKyc,
    pub intentos: i16,
    pub verificacion_en_curso: Option<Uuid>,
    pub enviado_en: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub evidencia: serde_json::Value,
    pub metadata_proveedor: serde_json::Value,
    pub motivo_rechazo: Option<String>,
    pub verificado_en: Option<DateTime<Utc>>,
    pub revisado_por: Option<Uuid>,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
}

pub(crate) const COLUMNAS: &str = "cliente_id, nombres, apellido_paterno, apellido_materno, apellidos, curp, rfc, \
     fecha_nacimiento, sexo::text AS sexo, entidad_nacimiento::text AS entidad_nacimiento, \
     nacionalidad::text AS nacionalidad, domicilio, ine_clave_elector, ine_cic, ine_identificador_ciudadano, \
     ine_emision, ine_vigencia, estado_kyc::text AS estado_kyc, intentos, verificacion_en_curso, enviado_en, \
     evidencia, metadata_proveedor, motivo_rechazo, verificado_en, revisado_por, creado_en, actualizado_en";

/// Identidad completa de un expediente enviado (todas las columnas que
/// `ck_perfil_envio_completo` exige).
#[derive(Debug, Clone)]
pub struct IdentidadPropia {
    pub nombres: String,
    pub apellido_paterno: String,
    pub apellido_materno: Option<String>,
    pub curp: String,
    pub rfc: String,
    pub fecha_nacimiento: NaiveDate,
    pub sexo: Sexo,
    pub clave_elector: String,
    pub cic: String,
    pub identificador_ciudadano: Option<String>,
    pub emision: i16,
    pub vigencia: i16,
    pub codigo_postal: Option<String>,
}

impl IdentidadPropia {
    pub fn de(e: &Expediente) -> AppResult<Self> {
        let falta = || AppError::negocio("KYC_DATOS_INCOMPLETOS", "el expediente no tiene completos sus datos de identidad");
        Ok(Self {
            nombres: e.nombres.clone().ok_or_else(falta)?,
            apellido_paterno: e.apellido_paterno.clone().ok_or_else(falta)?,
            apellido_materno: e.apellido_materno.clone(),
            curp: e.curp.clone().ok_or_else(falta)?,
            rfc: e.rfc.clone().ok_or_else(falta)?,
            fecha_nacimiento: e.fecha_nacimiento.ok_or_else(falta)?,
            sexo: e.sexo.as_deref().and_then(|s| s.chars().next()).and_then(Sexo::desde_letra).ok_or_else(falta)?,
            clave_elector: e.ine_clave_elector.clone().ok_or_else(falta)?,
            cic: e.ine_cic.clone().ok_or_else(falta)?,
            identificador_ciudadano: e.ine_identificador_ciudadano.clone(),
            emision: e.ine_emision.ok_or_else(falta)?,
            vigencia: e.ine_vigencia.ok_or_else(falta)?,
            codigo_postal: e.domicilio.get("codigo_postal").and_then(|v| v.as_str()).map(str::to_owned),
        })
    }

    pub fn como_declarada(&self) -> IdentidadDeclarada<'_> {
        IdentidadDeclarada {
            nombres: &self.nombres,
            apellido_paterno: &self.apellido_paterno,
            apellido_materno: self.apellido_materno.as_deref(),
            curp: &self.curp,
            fecha_nacimiento: self.fecha_nacimiento,
            sexo: self.sexo,
            credencial: CredencialDeclarada {
                clave_elector: &self.clave_elector,
                cic: &self.cic,
                identificador_ciudadano: self.identificador_ciudadano.as_deref(),
                emision: self.emision,
                vigencia: self.vigencia,
            },
            codigo_postal: self.codigo_postal.as_deref(),
        }
    }
}

/// Último resultado biométrico (columnas que `sc_kyc` puede leer).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ResumenResultado {
    pub verificacion_id: Uuid,
    pub proveedor: String,
    pub score_similitud: Option<Decimal>,
    pub prueba_vida_exitosa: bool,
    pub documento_valido: bool,
    pub coincide_identidad: bool,
    pub resultado: String,
    pub umbral_aplicado: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
    pub detalles_fallo: serde_json::Value,
}

/// Por qué un expediente PENDIENTE no puede enviarse: las mismas reglas que
/// `kyc.fn_proteger_perfil` aplica al envío (0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Bloqueo {
    /// Agotó sus intentos (`kyc.parametros.max_intentos`).
    SinIntentos,
    /// Demasiados envíos con identidades registradas en otra cuenta en los
    /// últimos 30 días (`kyc.conflictos_recientes`).
    ConflictosIdentidad,
    /// Su CURP acumula verificaciones rechazadas en otras cuentas en los
    /// últimos 30 días (`kyc.rechazos_de_identidad`).
    IdentidadBloqueada,
}

impl Bloqueo {
    fn error(self) -> AppError {
        match self {
            Bloqueo::SinIntentos => AppError::negocio(
                "KYC_SIN_INTENTOS",
                "agotaste tus intentos de verificación; cumplimiento revisará tu expediente",
            ),
            Bloqueo::ConflictosIdentidad => AppError::negocio(
                "KYC_SIN_INTENTOS",
                "demasiados envíos con identidades registradas en otra cuenta; acude a cumplimiento",
            ),
            Bloqueo::IdentidadBloqueada => AppError::negocio(
                "KYC_IDENTIDAD_BLOQUEADA",
                "esa CURP acumula verificaciones rechazadas en otras cuentas; acude a cumplimiento",
            ),
        }
    }
}

/// Bloqueo vigente de un expediente PENDIENTE (`None` en otro estado: ahí
/// el cliente no envía de todos modos).
async fn bloqueo(tx: &mut Tx, e: &Expediente, max_intentos: i16) -> AppResult<Option<Bloqueo>> {
    if e.estado_kyc != EstadoKyc::Pendiente {
        return Ok(None);
    }
    if e.intentos >= max_intentos {
        return Ok(Some(Bloqueo::SinIntentos));
    }
    let conflictos: i64 =
        sqlx::query_scalar("SELECT kyc.conflictos_recientes($1)").bind(e.cliente_id).fetch_one(&mut **tx).await?;
    if conflictos >= i64::from(max_intentos) {
        return Ok(Some(Bloqueo::ConflictosIdentidad));
    }
    if let Some(curp) = e.curp.as_deref() {
        let rechazos: i64 = sqlx::query_scalar("SELECT kyc.rechazos_de_identidad($1, $2)")
            .bind(curp)
            .bind(e.cliente_id)
            .fetch_one(&mut **tx)
            .await?;
        if rechazos >= i64::from(max_intentos) {
            return Ok(Some(Bloqueo::IdentidadBloqueada));
        }
    }
    Ok(None)
}

/// Lo que ve el cliente en el paso 4 (y el operador).
#[derive(Debug, Clone, Serialize)]
pub struct VistaExpediente {
    #[serde(flatten)]
    pub expediente: Expediente,
    pub tipo_persona: TipoPersona,
    /// Estatus de la cuenta (`kyc.clientes.estatus_kyc`): espejo del
    /// expediente, salvo SUSPENDIDO (medida de cumplimiento sobre un
    /// expediente RECHAZADO).
    pub estatus_cliente: EstatusKyc,
    pub max_intentos: i16,
    /// 0 si hay un bloqueo.
    pub intentos_restantes: i16,
    /// Por qué un expediente PENDIENTE no puede enviarse.
    pub bloqueo: Option<Bloqueo>,
    pub ultimo_resultado: Option<ResumenResultado>,
}

impl VistaExpediente {
    /// Sin score ni umbral (ver `routes::kyc::verificar`).
    pub fn para_cliente(mut self) -> Self {
        if let Some(r) = self.ultimo_resultado.as_mut() {
            r.score_similitud = None;
            r.umbral_aplicado = None;
        }
        self
    }
}

pub(crate) async fn cargar(tx: &mut Tx, cliente_id: Uuid) -> AppResult<Expediente> {
    sqlx::query_as(&format!("SELECT {COLUMNAS} FROM kyc.kyc_perfiles WHERE cliente_id = $1"))
        .bind(cliente_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("expediente KYC no encontrado"))
}

pub async fn vista(pool: &PgPool, cliente_id: Uuid) -> AppResult<VistaExpediente> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let expediente = cargar(&mut tx, cliente_id).await?;
    let (tipo_persona, estatus_cliente): (TipoPersona, EstatusKyc) =
        sqlx::query_as("SELECT tipo_persona, estatus_kyc FROM kyc.clientes WHERE id = $1")
            .bind(cliente_id)
            .fetch_one(&mut *tx)
            .await?;
    let max_intentos: i16 = sqlx::query_scalar("SELECT max_intentos FROM kyc.parametros WHERE id").fetch_one(&mut *tx).await?;
    let bloqueo = bloqueo(&mut tx, &expediente, max_intentos).await?;
    let ultimo_resultado = sqlx::query_as(
        "SELECT verificacion_id, proveedor, score_similitud, prueba_vida_exitosa, documento_valido, coincide_identidad, \
                resultado, umbral_aplicado, \"timestamp\", detalles_fallo \
         FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let intentos_restantes = if bloqueo.is_some() { 0 } else { (max_intentos - expediente.intentos).max(0) };
    Ok(VistaExpediente {
        expediente,
        tipo_persona,
        estatus_cliente,
        max_intentos,
        intentos_restantes,
        bloqueo,
        ultimo_resultado,
    })
}

fn no_editable(estado: EstadoKyc) -> AppError {
    let mensaje = match estado {
        EstadoKyc::EnRevision => "el expediente ya está en revisión; espera el resultado",
        EstadoKyc::Verificado => "el expediente ya está verificado",
        EstadoKyc::Rechazado => "el expediente fue rechazado; solo cumplimiento puede reabrirlo",
        EstadoKyc::Pendiente => "el expediente no se pudo actualizar",
    };
    AppError::conflicto("KYC_EXPEDIENTE_NO_EDITABLE", mensaje)
}

async fn exigir_persona_fisica(tx: &mut Tx, cliente_id: Uuid) -> AppResult<()> {
    let tipo: Option<TipoPersona> = sqlx::query_scalar("SELECT tipo_persona FROM kyc.clientes WHERE id = $1")
        .bind(cliente_id)
        .fetch_optional(&mut **tx)
        .await?;
    match tipo {
        None => Err(AppError::no_encontrado("cliente no encontrado")),
        Some(TipoPersona::Moral) => Err(AppError::negocio(
            "KYC_SOLO_PERSONA_FISICA",
            "la identificación biométrica remota es para personas físicas; las personas morales se dan de alta en sucursal",
        )),
        Some(TipoPersona::Fisica) => Ok(()),
    }
}

// -----------------------------------------------------------------------------
// Paso 1: datos personales y domicilio
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Domicilio {
    pub calle: String,
    pub numero_exterior: String,
    #[serde(default)]
    pub numero_interior: Option<String>,
    pub colonia: String,
    pub municipio: String,
    pub estado: String,
    pub codigo_postal: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatosPersonales {
    pub nombres: String,
    pub apellido_paterno: String,
    #[serde(default)]
    pub apellido_materno: Option<String>,
    pub curp: String,
    pub rfc: String,
    pub domicilio: Domicilio,
}

/// Datos ya validados, listos para guardar.
#[derive(Debug, Clone)]
pub struct DatosValidados {
    pub nombres: String,
    pub apellido_paterno: String,
    pub apellido_materno: Option<String>,
    pub curp: Curp,
    pub rfc: String,
    pub domicilio: Domicilio,
    pub advertencias: Vec<String>,
}

fn nombre(campo: &str, v: &str) -> AppResult<String> {
    let t = texto::texto_corto(campo, v, 120).map_err(AppError::validacion)?;
    if texto::palabras(&t).is_empty() {
        return Err(AppError::validacion(format!("{campo} debe contener letras")));
    }
    Ok(t)
}

/// Mayoría de edad a la fecha `hoy`.
fn edad(nacimiento: NaiveDate, hoy: NaiveDate) -> i32 {
    let mut anios = hoy.year() - nacimiento.year();
    if (hoy.month(), hoy.day()) < (nacimiento.month(), nacimiento.day()) {
        anios -= 1;
    }
    anios
}

pub fn validar_datos(d: &DatosPersonales, hoy: NaiveDate) -> AppResult<DatosValidados> {
    let nombres = nombre("nombres", &d.nombres)?;
    let apellido_paterno = nombre("apellido_paterno", &d.apellido_paterno)?;
    let apellido_materno = match d.apellido_materno.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(m) => Some(nombre("apellido_materno", m)?),
    };
    let curp = Curp::parse(&d.curp).map_err(|e| AppError::validacion(e.to_string()))?;
    let rfc = rfc::validar_persona_fisica(&d.rfc).map_err(|e| AppError::validacion(e.to_string()))?;
    let (errores, mut advertencias) = rfc::cotejar_con_curp(&rfc, &curp, &apellido_paterno, apellido_materno.as_deref());
    if let Some(e) = errores.into_iter().next() {
        return Err(AppError::validacion(e));
    }
    advertencias.extend(curp.advertencias_de_nombre(&nombres, &apellido_paterno, apellido_materno.as_deref()));
    let anios = edad(curp.fecha_nacimiento(), hoy);
    if anios < 18 {
        return Err(AppError::negocio("KYC_MENOR_DE_EDAD", "la apertura remota es solo para mayores de edad"));
    }
    if anios > 120 {
        return Err(AppError::validacion("la fecha de nacimiento de la CURP no es plausible"));
    }
    let dom = &d.domicilio;
    let corto = |campo: &str, v: &str, max: usize| texto::texto_corto(campo, v, max).map_err(AppError::validacion);
    let domicilio = Domicilio {
        calle: corto("calle", &dom.calle, 120)?,
        numero_exterior: corto("numero_exterior", &dom.numero_exterior, 20)?,
        numero_interior: match dom.numero_interior.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(n) => Some(corto("numero_interior", n, 20)?),
        },
        colonia: corto("colonia", &dom.colonia, 120)?,
        municipio: corto("municipio", &dom.municipio, 120)?,
        estado: corto("estado", &dom.estado, 60)?,
        codigo_postal: {
            let cp = dom.codigo_postal.trim();
            if cp.len() != 5 || !cp.bytes().all(|b| b.is_ascii_digit()) {
                return Err(AppError::validacion("el código postal tiene 5 dígitos"));
            }
            cp.to_owned()
        },
    };
    Ok(DatosValidados { nombres, apellido_paterno, apellido_materno, curp, rfc, domicilio, advertencias })
}

#[derive(Debug, Clone, Serialize)]
pub struct DatosGuardados {
    pub expediente: Expediente,
    /// Inconsistencias que no impiden continuar pero llevarán el expediente
    /// a revisión humana (p. ej. letras de la CURP que no corresponden al nombre).
    pub advertencias: Vec<String>,
}

pub async fn guardar_datos(pool: &PgPool, cliente_id: Uuid, d: &DatosPersonales, hoy: NaiveDate) -> AppResult<DatosGuardados> {
    let v = validar_datos(d, hoy)?;
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    exigir_persona_fisica(&mut tx, cliente_id).await?;
    let domicilio = serde_json::to_value(&v.domicilio).map_err(|e| AppError::Interno(e.into()))?;
    let expediente: Option<Expediente> = sqlx::query_as(&format!(
        "UPDATE kyc.kyc_perfiles SET nombres = $2, apellido_paterno = $3, apellido_materno = $4, curp = $5, rfc = $6, \
                fecha_nacimiento = $7, sexo = $8, entidad_nacimiento = $9, domicilio = $10 \
         WHERE cliente_id = $1 RETURNING {COLUMNAS}"
    ))
    .bind(cliente_id)
    .bind(&v.nombres)
    .bind(&v.apellido_paterno)
    .bind(&v.apellido_materno)
    .bind(v.curp.as_str())
    .bind(&v.rfc)
    .bind(v.curp.fecha_nacimiento())
    .bind(v.curp.sexo().letra().to_string())
    .bind(v.curp.entidad())
    .bind(domicilio)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(expediente) = expediente else {
        let estado = cargar(&mut tx, cliente_id).await?.estado_kyc;
        return Err(no_editable(estado));
    };
    tx.commit().await?;
    Ok(DatosGuardados { expediente, advertencias: v.advertencias })
}

// -----------------------------------------------------------------------------
// Pasos 2 y 3: INE y selfie
// -----------------------------------------------------------------------------

pub const MIN_CUADROS_SELFIE: usize = 3;
pub const MAX_CUADROS_SELFIE: usize = 8;
const MIN_BYTES_IMAGEN: usize = 1024;
const MAX_BYTES_IMAGEN: usize = 3 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatosIne {
    pub clave_elector: String,
    pub cic: String,
    #[serde(default)]
    pub identificador_ciudadano: Option<String>,
    pub emision: i16,
    pub vigencia: i16,
    /// Imagen del anverso (base64 o data URL JPEG/PNG/WebP).
    pub frente: String,
    pub reverso: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selfie {
    /// Cuadros capturados con la cámara durante la prueba de vida.
    pub frames: Vec<String>,
    /// Reto mostrado al cliente (p. ej. "GIRA_IZQUIERDA"); informativo.
    #[serde(default)]
    pub reto: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudVerificacion {
    pub ine: DatosIne,
    pub selfie: Selfie,
    /// Consentimiento expreso para tratar datos biométricos (dato personal
    /// sensible, LFPDPPP art. 9). Sin él no se envía nada al proveedor.
    pub consentimiento_biometrico: bool,
}

/// Imagen decodificada con su huella.
#[derive(Debug, Clone)]
pub struct Imagen {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub tipo: &'static str,
}

fn tipo_imagen(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

pub fn decodificar_imagen(campo: &str, entrada: &str) -> AppResult<Imagen> {
    let datos = match entrada.split_once(',') {
        Some((cabecera, datos)) if cabecera.starts_with("data:") => datos,
        _ => entrada,
    };
    let limpio: String = datos.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(limpio.as_bytes())
        .map_err(|_| AppError::validacion(format!("{campo}: la imagen debe venir en base64")))?;
    if bytes.len() < MIN_BYTES_IMAGEN || bytes.len() > MAX_BYTES_IMAGEN {
        return Err(AppError::validacion(format!("{campo}: la imagen debe pesar entre 1 KB y 3 MB")));
    }
    let tipo = tipo_imagen(&bytes).ok_or_else(|| AppError::validacion(format!("{campo}: se admite JPEG, PNG o WebP")))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Ok(Imagen { bytes, sha256, tipo })
}

/// Imágenes de un envío y sus huellas.
#[derive(Debug, Clone)]
pub struct Imagenes {
    pub ine_frente: Imagen,
    pub ine_reverso: Imagen,
    pub selfie: Vec<Imagen>,
}

impl Imagenes {
    pub fn evidencia(&self) -> Evidencia {
        Evidencia {
            ine_frente: self.ine_frente.sha256.clone(),
            ine_reverso: self.ine_reverso.sha256.clone(),
            selfie: self.selfie.iter().map(|i| i.sha256.clone()).collect(),
        }
    }
}

/// Huellas SHA-256 de las imágenes analizadas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidencia {
    pub ine_frente: String,
    pub ine_reverso: String,
    pub selfie: Vec<String>,
}

pub fn decodificar_imagenes(s: &SolicitudVerificacion) -> AppResult<Imagenes> {
    let n = s.selfie.frames.len();
    if !(MIN_CUADROS_SELFIE..=MAX_CUADROS_SELFIE).contains(&n) {
        return Err(AppError::validacion(format!(
            "la prueba de vida requiere de {MIN_CUADROS_SELFIE} a {MAX_CUADROS_SELFIE} cuadros de la cámara"
        )));
    }
    Ok(Imagenes {
        ine_frente: decodificar_imagen("ine.frente", &s.ine.frente)?,
        ine_reverso: decodificar_imagen("ine.reverso", &s.ine.reverso)?,
        selfie: s
            .selfie
            .frames
            .iter()
            .enumerate()
            .map(|(i, f)| decodificar_imagen(&format!("selfie.frames[{i}]"), f))
            .collect::<AppResult<_>>()?,
    })
}

/// Origen del envío (se guarda con el expediente y pasa a la auditoría).
#[derive(Debug, Clone, Default)]
pub struct OrigenEnvio {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
}

pub fn exigir_consentimiento(s: &SolicitudVerificacion) -> AppResult<()> {
    if s.consentimiento_biometrico {
        Ok(())
    } else {
        Err(AppError::negocio(
            "KYC_SIN_CONSENTIMIENTO",
            "para verificar tu identidad necesitamos tu consentimiento expreso para tratar tus datos biométricos",
        ))
    }
}

/// Comprobación barata antes de decodificar imágenes: persona física,
/// expediente PENDIENTE con sus datos y sin [`Bloqueo`] (intentos,
/// conflictos de identidad o CURP bloqueada; la base de datos lo vuelve a
/// exigir al enviar).
pub async fn exigir_enviable(pool: &PgPool, cliente_id: Uuid) -> AppResult<()> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    exigir_persona_fisica(&mut tx, cliente_id).await?;
    let e = cargar(&mut tx, cliente_id).await?;
    let max_intentos: i16 = sqlx::query_scalar("SELECT max_intentos FROM kyc.parametros WHERE id").fetch_one(&mut *tx).await?;
    let bloqueo = bloqueo(&mut tx, &e, max_intentos).await?;
    tx.commit().await?;
    if e.estado_kyc != EstadoKyc::Pendiente {
        return Err(no_editable(e.estado_kyc));
    }
    if let Some(b) = bloqueo {
        return Err(b.error());
    }
    if e.curp.is_none() || e.rfc.is_none() || e.nombres.is_none() || e.domicilio.get("codigo_postal").is_none() {
        return Err(AppError::negocio("KYC_DATOS_INCOMPLETOS", "primero captura tus datos personales y tu domicilio"));
    }
    Ok(())
}

/// Un envío chocó con una identidad registrada en otra cuenta (ver 0007).
pub async fn registrar_conflicto(pool: &PgPool, cliente_id: Uuid) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    sqlx::query("SELECT kyc.registrar_conflicto_identidad($1)").bind(cliente_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Envío a revisión: devuelve el expediente EN_REVISION, con su
/// `verificacion_en_curso` generada por la base de datos.
pub async fn enviar(
    pool: &PgPool,
    cliente_id: Uuid,
    s: &SolicitudVerificacion,
    imagenes: &Imagenes,
    origen: &OrigenEnvio,
    hoy: NaiveDate,
) -> AppResult<Expediente> {
    exigir_consentimiento(s)?;
    let clave_elector = s.ine.clave_elector.trim().to_uppercase();
    let cic = s.ine.cic.trim().to_owned();
    let identificador = s.ine.identificador_ciudadano.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    exigir_persona_fisica(&mut tx, cliente_id).await?;
    let actual = cargar(&mut tx, cliente_id).await?;
    if actual.estado_kyc != EstadoKyc::Pendiente {
        return Err(no_editable(actual.estado_kyc));
    }
    // Bloquea el expediente: la credencial se valida contra la CURP que se
    // envía, no contra una que /kyc/perfil cambie en paralelo.
    let actual: Expediente =
        sqlx::query_as(&format!("SELECT {COLUMNAS} FROM kyc.kyc_perfiles WHERE cliente_id = $1 FOR UPDATE"))
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| no_editable(EstadoKyc::EnRevision))?;
    let curp = actual
        .curp
        .as_deref()
        .and_then(|c| Curp::parse(c).ok())
        .filter(|_| actual.nombres.is_some() && actual.rfc.is_some() && actual.domicilio.get("codigo_postal").is_some())
        .ok_or_else(|| AppError::negocio("KYC_DATOS_INCOMPLETOS", "primero captura tus datos personales y tu domicilio"))?;
    let credencial = CredencialDeclarada {
        clave_elector: &clave_elector,
        cic: &cic,
        identificador_ciudadano: identificador,
        emision: s.ine.emision,
        vigencia: s.ine.vigencia,
    };
    let (errores, _) = ine::validar_credencial(&credencial, &curp, hoy);
    if !errores.is_empty() {
        return Err(AppError::validacion(errores.join("; ")));
    }
    let todas: Vec<&str> = std::iter::once(imagenes.ine_frente.sha256.as_str())
        .chain(std::iter::once(imagenes.ine_reverso.sha256.as_str()))
        .chain(imagenes.selfie.iter().map(|i| i.sha256.as_str()))
        .collect();
    let distintas: std::collections::HashSet<&str> = todas.iter().copied().collect();
    if distintas.len() != todas.len() {
        return Err(AppError::validacion("cada imagen (anverso, reverso y cuadros de la selfie) debe ser distinta"));
    }
    // Las huellas y el consentimiento quedan con el envío (y en la bitácora).
    let mut evidencia = serde_json::to_value(imagenes.evidencia()).map_err(|e| AppError::Interno(e.into()))?;
    evidencia["consentimiento_biometrico"] = serde_json::json!({ "otorgado": true, "registrado_en": Utc::now() });
    let user_agent: Option<String> = origen.user_agent.as_deref().map(|u| u.chars().take(512).collect());
    let n = sqlx::query(
        "UPDATE kyc.kyc_perfiles SET ine_clave_elector = $2, ine_cic = $3, ine_identificador_ciudadano = $4, \
                ine_emision = $5, ine_vigencia = $6, evidencia = $7, envio_ip = $8::inet, envio_user_agent = $9 \
         WHERE cliente_id = $1",
    )
    .bind(cliente_id)
    .bind(&clave_elector)
    .bind(&cic)
    .bind(identificador)
    .bind(s.ine.emision)
    .bind(s.ine.vigencia)
    .bind(evidencia)
    .bind(origen.ip.map(|ip| ip.to_string()))
    .bind(user_agent)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(no_editable(EstadoKyc::EnRevision));
    }
    let expediente: Expediente = sqlx::query_as(&format!(
        "UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = $1 RETURNING {COLUMNAS}"
    ))
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| no_editable(EstadoKyc::EnRevision))?;
    tx.commit().await?;
    tracing::info!(cliente = %cliente_id, intentos = expediente.intentos, "expediente KYC enviado a revisión");
    Ok(expediente)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn datos() -> DatosPersonales {
        DatosPersonales {
            nombres: " Andrés  Manuel ".into(),
            apellido_paterno: "López".into(),
            apellido_materno: Some("Obrador".into()),
            curp: "looa531113htcpbn07".into(),
            rfc: rfc::construir_desde_curp("LOOA531113HTCPBN07", "AB"),
            domicilio: Domicilio {
                calle: "Av. Paseo Tabasco".into(),
                numero_exterior: "1401".into(),
                numero_interior: Some(" ".into()),
                colonia: "Tabasco 2000".into(),
                municipio: "Centro".into(),
                estado: "Tabasco".into(),
                codigo_postal: "86035".into(),
            },
        }
    }

    fn hoy() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 13).unwrap()
    }

    #[test]
    fn valida_y_normaliza_los_datos_personales() {
        let v = validar_datos(&datos(), hoy()).unwrap();
        assert_eq!(v.nombres, "Andrés Manuel");
        assert_eq!(v.curp.as_str(), "LOOA531113HTCPBN07");
        assert_eq!(v.domicilio.numero_interior, None);
        assert!(v.advertencias.is_empty(), "{:?}", v.advertencias);
    }

    #[test]
    fn rechaza_datos_inconsistentes() {
        let mut d = datos();
        d.curp = "LOOA531113HTCPBN08".into();
        assert!(validar_datos(&d, hoy()).is_err());
        let mut d = datos();
        d.rfc = rfc::construir_desde_curp("LOOA531114HTCPBN07", "AB");
        assert!(validar_datos(&d, hoy()).is_err());
        let mut d = datos();
        d.domicilio.codigo_postal = "8603".into();
        assert!(validar_datos(&d, hoy()).is_err());
        // Nombre que no corresponde a la CURP: se acepta con advertencia.
        let mut d = datos();
        d.nombres = "Beatriz".into();
        assert_eq!(validar_datos(&d, hoy()).unwrap().advertencias.len(), 1);
        // Menor de edad.
        let curp = super::super::curp::construir(
            "Ana", "Ruiz", None, NaiveDate::from_ymd_opt(2010, 1, 1).unwrap(), Sexo::M, "DF", 1);
        let mut d = datos();
        d.rfc = rfc::construir_desde_curp(&curp, "AB");
        d.curp = curp;
        assert_eq!(validar_datos(&d, hoy()).unwrap_err().codigo(), "KYC_MENOR_DE_EDAD");
    }

    #[test]
    fn decodifica_imagenes() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.resize(2048, 7);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
        let i = decodificar_imagen("x", &format!("data:image/jpeg;base64,{b64}")).unwrap();
        assert_eq!(i.tipo, "image/jpeg");
        assert_eq!(i.sha256.len(), 64);
        assert!(decodificar_imagen("x", "no-es-base64!").is_err());
        let texto_plano = base64::engine::general_purpose::STANDARD.encode(vec![b'a'; 2048]);
        assert!(decodificar_imagen("x", &texto_plano).is_err());
        let chica = base64::engine::general_purpose::STANDARD.encode(&jpeg[..100]);
        assert!(decodificar_imagen("x", &chica).is_err());
    }
}
