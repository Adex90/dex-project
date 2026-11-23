//! Beneficiarios de la cuenta (migración 0020).
//!
//! El titular designa hasta [`MAXIMO`] beneficiarios: nombre legal completo,
//! parentesco, CURP o fecha de nacimiento (al menos uno; si van los dos, la
//! fecha es la de la CURP) y su porcentaje, a centésimos, que suman
//! exactamente 100%. Guardar nunca corrige: crea una versión nueva, inmutable
//! y encadenada por hash (`SC-BENEF-v1`), con la sesión, la IP y el
//! dispositivo que la hicieron. La vigente es la última; una versión vacía
//! retira la designación. El historial es la bitácora.
//!
//! - **Validación.** Aquí, con mensajes claros, y otra vez en la base de datos
//!   al COMMIT (suma exacta, posiciones, titular, identidad, hash).
//! - **Concurrencia.** El cliente manda la versión sobre la que editó
//!   (`version_base`): si otra sesión guardó antes, `409
//!   DESIGNACION_DESACTUALIZADA`, y vuelve a leer. Dos guardados simultáneos
//!   sobre la misma versión: uno gana, el otro choca con la versión única.
//! - **Cadena.** Al leer, el backend recalcula el hash de cada versión con su
//!   propia implementación del payload y comprueba el encadenamiento: una
//!   alteración por fuera de la aplicación se nota (`cadena_verificada`).
//!
//! Solo un cliente con KYC VERIFICADO designa. Lee con su rol y RLS; escribe
//! sc_kyc.

use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::curp::Curp;
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    ledger::hash::{sha256_hex, GENESIS},
};

/// Beneficiarios por designación (CHECK de 0020).
pub const MAXIMO: usize = 5;
/// Versiones del historial que devuelve la lectura (la cadena se comprueba completa).
pub const HISTORIAL_MAXIMO: usize = 50;
const NOMBRE_MIN: usize = 3;
const NOMBRE_MAX: usize = 120;
const VERSION_PAYLOAD: &str = "SC-BENEF-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Parentesco {
    Conyuge,
    /// Concubina o concubinario.
    Concubino,
    Hijo,
    Padre,
    Hermano,
    Nieto,
    Abuelo,
    Otro,
}

impl Parentesco {
    pub const TODOS: [Parentesco; 8] = [
        Parentesco::Conyuge,
        Parentesco::Concubino,
        Parentesco::Hijo,
        Parentesco::Padre,
        Parentesco::Hermano,
        Parentesco::Nieto,
        Parentesco::Abuelo,
        Parentesco::Otro,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Parentesco::Conyuge => "CONYUGE",
            Parentesco::Concubino => "CONCUBINO",
            Parentesco::Hijo => "HIJO",
            Parentesco::Padre => "PADRE",
            Parentesco::Hermano => "HERMANO",
            Parentesco::Nieto => "NIETO",
            Parentesco::Abuelo => "ABUELO",
            Parentesco::Otro => "OTRO",
        }
    }
}

// -----------------------------------------------------------------------------
// Entrada y validación
// -----------------------------------------------------------------------------

/// Un beneficiario como lo captura el cliente.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeneficiarioEntrada {
    pub nombre_completo: String,
    pub parentesco: Parentesco,
    #[serde(default)]
    pub curp: Option<String>,
    #[serde(default)]
    pub fecha_nacimiento: Option<NaiveDate>,
    /// Texto decimal ("33.34"), como todo monto de la API.
    #[serde(with = "rust_decimal::serde::str")]
    pub porcentaje: Decimal,
}

/// `POST /kyc/beneficiarios`: la designación completa que reemplaza a la vigente.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudDesignacion {
    /// Vacía retira la designación.
    pub beneficiarios: Vec<BeneficiarioEntrada>,
    /// La versión vigente cuando el cliente empezó a editar (0 si no había).
    pub version_base: i32,
}

/// Un beneficiario validado, como se guarda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Beneficiario {
    pub posicion: i16,
    pub nombre_completo: String,
    pub parentesco: Parentesco,
    pub curp: Option<String>,
    pub fecha_nacimiento: Option<NaiveDate>,
    /// A centésimos ("33.34").
    pub porcentaje: Decimal,
}

/// Nombre legal: mayúsculas (NFC), espacios simples; solo letras, espacios,
/// apóstrofos, guiones y puntos; al menos un nombre y un apellido.
pub fn normalizar_nombre(entrada: &str) -> Result<String, String> {
    let nombre: String = entrada.nfc().flat_map(char::to_uppercase).collect::<String>();
    let nombre = nombre.split_whitespace().collect::<Vec<_>>().join(" ");
    let largo = nombre.chars().count();
    if !(NOMBRE_MIN..=NOMBRE_MAX).contains(&largo) {
        return Err(format!("el nombre completo lleva de {NOMBRE_MIN} a {NOMBRE_MAX} caracteres"));
    }
    if !nombre.chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '\'' | '-' | '.')) {
        return Err("el nombre completo solo lleva letras, espacios, apóstrofos, guiones y puntos".into());
    }
    let palabras: Vec<&str> = nombre.split(' ').collect();
    if palabras.len() < 2 || !palabras.iter().all(|p| p.chars().any(char::is_alphabetic)) {
        return Err("escribe el nombre completo: nombre(s) y apellido(s)".into());
    }
    Ok(nombre)
}

/// Valida una designación completa contra el titular (`curp_titular`) y la
/// fecha de hoy. Devuelve los beneficiarios en sus posiciones 1..n.
pub fn validar(
    entradas: &[BeneficiarioEntrada],
    curp_titular: Option<&str>,
    hoy: NaiveDate,
) -> AppResult<Vec<Beneficiario>> {
    if entradas.len() > MAXIMO {
        return Err(AppError::negocio(
            "BENEFICIARIOS_MAXIMO",
            format!("se designan como máximo {MAXIMO} beneficiarios"),
        ));
    }
    let minimo = NaiveDate::from_ymd_opt(1900, 1, 1).expect("fecha fija");
    let mut validados = Vec::with_capacity(entradas.len());
    let mut suma = Decimal::ZERO;
    for (i, e) in entradas.iter().enumerate() {
        let n = i + 1;
        let invalido = |m: String| AppError::validacion(format!("beneficiario {n}: {m}"));
        let nombre_completo = normalizar_nombre(&e.nombre_completo).map_err(invalido)?;
        let curp = match e.curp.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(Curp::parse(c).map_err(|err| invalido(err.to_string()))?),
            None => None,
        };
        if let Some(fecha) = e.fecha_nacimiento {
            if fecha > hoy {
                return Err(invalido("la fecha de nacimiento no puede ser futura".into()));
            }
            if fecha < minimo {
                return Err(invalido("la fecha de nacimiento es anterior a 1900".into()));
            }
        }
        match (&curp, e.fecha_nacimiento) {
            (None, None) => return Err(invalido("indica su CURP o su fecha de nacimiento".into())),
            (Some(c), Some(f)) if c.fecha_nacimiento() != f => {
                return Err(invalido(format!(
                    "la fecha de nacimiento ({f}) no es la de su CURP ({})",
                    c.fecha_nacimiento()
                )))
            }
            _ => {}
        }
        if e.porcentaje <= Decimal::ZERO || e.porcentaje > Decimal::ONE_HUNDRED {
            return Err(invalido("el porcentaje es mayor que 0 y no pasa de 100".into()));
        }
        if e.porcentaje.normalize().scale() > 2 {
            return Err(invalido("el porcentaje lleva a lo más dos decimales".into()));
        }
        if let (Some(c), Some(titular)) = (&curp, curp_titular) {
            if c.as_str() == titular {
                return Err(AppError::negocio(
                    "BENEFICIARIO_TITULAR",
                    "no puedes designarte a ti mismo como beneficiario",
                ));
            }
        }
        if let Some(c) = &curp {
            if validados.iter().any(|b: &Beneficiario| b.curp.as_deref() == Some(c.as_str())) {
                return Err(AppError::negocio(
                    "BENEFICIARIO_DUPLICADO",
                    format!("la CURP {} aparece dos veces", c.as_str()),
                ));
            }
        }
        suma += e.porcentaje;
        let mut porcentaje = e.porcentaje;
        porcentaje.rescale(2);
        validados.push(Beneficiario {
            posicion: n as i16,
            nombre_completo,
            parentesco: e.parentesco,
            curp: curp.map(|c| c.as_str().to_owned()),
            fecha_nacimiento: e.fecha_nacimiento,
            porcentaje,
        });
    }
    if !validados.is_empty() && suma != Decimal::ONE_HUNDRED {
        return Err(AppError::negocio(
            "BENEFICIARIOS_PORCENTAJE",
            format!("los porcentajes suman {}% y deben sumar exactamente 100%", suma.normalize()),
        ));
    }
    Ok(validados)
}

// -----------------------------------------------------------------------------
// Payload canónico (el mismo que kyc.payload_designacion)
// -----------------------------------------------------------------------------

/// Una versión como se lee de la base de datos, con los textos tal como los
/// usa el payload (la IP con `host()`, el porcentaje con 4 decimales).
#[derive(Debug, Clone, sqlx::FromRow)]
struct FilaDesignacion {
    id: Uuid,
    cliente_id: Uuid,
    version: i32,
    num_beneficiarios: i16,
    total_porcentaje: Decimal,
    sesion_id: Option<Uuid>,
    ip: Option<String>,
    dispositivo: Option<String>,
    hash_anterior: String,
    hash: String,
    creada_en: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct FilaBeneficiario {
    designacion_id: Uuid,
    posicion: i16,
    nombre_completo: String,
    parentesco: Parentesco,
    curp: Option<String>,
    fecha_nacimiento: Option<NaiveDate>,
    porcentaje: Decimal,
}

fn campo(p: &mut String, valor: Option<&str>) {
    if !p.is_empty() {
        p.push('|');
    }
    match valor {
        None => p.push('~'),
        Some(v) => {
            p.push_str(&v.len().to_string());
            p.push(':');
            p.push_str(v);
        }
    }
}

fn con_escala(mut d: Decimal, escala: u32) -> String {
    d.rescale(escala);
    d.to_string()
}

/// `SC-BENEF-v1` (ver la migración 0020).
fn payload(d: &FilaDesignacion, beneficiarios: &[&FilaBeneficiario]) -> String {
    let mut p = String::new();
    campo(&mut p, Some(VERSION_PAYLOAD));
    campo(&mut p, Some(&d.cliente_id.to_string()));
    campo(&mut p, Some(&d.version.to_string()));
    campo(&mut p, Some(&d.creada_en.timestamp_micros().to_string()));
    campo(&mut p, Some(&d.num_beneficiarios.to_string()));
    campo(&mut p, Some(&con_escala(d.total_porcentaje, 2)));
    campo(&mut p, d.sesion_id.map(|s| s.to_string()).as_deref());
    campo(&mut p, d.ip.as_deref());
    campo(&mut p, d.dispositivo.as_deref());
    let mut ordenados = beneficiarios.to_vec();
    ordenados.sort_by_key(|b| b.posicion);
    if !ordenados.is_empty() {
        let mut bloque = String::new();
        for b in ordenados {
            campo(&mut bloque, Some(&b.posicion.to_string()));
            campo(&mut bloque, Some(&b.nombre_completo));
            campo(&mut bloque, Some(b.parentesco.as_str()));
            campo(&mut bloque, b.curp.as_deref());
            campo(&mut bloque, b.fecha_nacimiento.map(|f| f.format("%Y-%m-%d").to_string()).as_deref());
            campo(&mut bloque, Some(&con_escala(b.porcentaje, 4)));
        }
        // string_agg de bloques ya codificados: van tal cual, separados por '|'.
        p.push('|');
        p.push_str(&bloque);
    }
    campo(&mut p, Some(&d.hash_anterior));
    p
}

// -----------------------------------------------------------------------------
// Lectura
// -----------------------------------------------------------------------------

/// Una versión de la designación, como la ve su titular.
#[derive(Debug, Clone, Serialize)]
pub struct Designacion {
    pub version: i32,
    pub creada_en: DateTime<Utc>,
    /// Dispositivo y IP de la sesión que la guardó.
    pub dispositivo: Option<String>,
    pub ip: Option<String>,
    pub hash: String,
    pub hash_anterior: String,
    pub beneficiarios: Vec<Beneficiario>,
}

/// Resumen de una versión del historial.
#[derive(Debug, Clone, Serialize)]
pub struct ResumenDesignacion {
    pub version: i32,
    pub creada_en: DateTime<Utc>,
    pub num_beneficiarios: i16,
    pub dispositivo: Option<String>,
    pub hash: String,
}

/// `GET /kyc/beneficiarios`.
#[derive(Debug, Clone, Serialize)]
pub struct VistaBeneficiarios {
    /// La vigente; `None` si nunca designó.
    pub vigente: Option<Designacion>,
    /// La versión vigente (0 si nunca designó): la `version_base` del próximo guardado.
    pub version: i32,
    /// Las versiones, la más reciente primero (hasta [`HISTORIAL_MAXIMO`]).
    pub historial: Vec<ResumenDesignacion>,
    /// Cada versión cuadra con su hash y con la anterior (recalculado aquí).
    pub cadena_verificada: bool,
    /// El titular puede designar (KYC verificado).
    pub puede_designar: bool,
    pub maximo: usize,
    pub parentescos: [Parentesco; 8],
}

fn a_beneficiario(b: &FilaBeneficiario) -> Beneficiario {
    let mut porcentaje = b.porcentaje;
    porcentaje.rescale(2);
    Beneficiario {
        posicion: b.posicion,
        nombre_completo: b.nombre_completo.clone(),
        parentesco: b.parentesco,
        curp: b.curp.clone(),
        fecha_nacimiento: b.fecha_nacimiento,
        porcentaje,
    }
}

/// La designación del cliente, su historial y la cadena comprobada.
pub async fn consultar(pool: &PgPool, cliente_id: Uuid) -> AppResult<VistaBeneficiarios> {
    let mut tx = db::begin_lectura_instantanea_cliente(pool, cliente_id).await?;
    let estatus: Option<String> = sqlx::query_scalar("SELECT estatus_kyc FROM kyc.clientes WHERE id = $1")
        .bind(cliente_id)
        .fetch_optional(&mut *tx)
        .await?;
    let versiones: Vec<FilaDesignacion> = sqlx::query_as(
        "SELECT id, cliente_id, version, num_beneficiarios, total_porcentaje, sesion_id, host(ip) AS ip, \
                dispositivo, hash_anterior, hash, creada_en \
         FROM kyc.designaciones_beneficiarios WHERE cliente_id = $1 ORDER BY version",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    let filas: Vec<FilaBeneficiario> = sqlx::query_as(
        "SELECT b.designacion_id, b.posicion, b.nombre_completo, b.parentesco, b.curp, b.fecha_nacimiento, \
                b.porcentaje \
         FROM kyc.beneficiarios b JOIN kyc.designaciones_beneficiarios d ON d.id = b.designacion_id \
         WHERE d.cliente_id = $1 ORDER BY d.version, b.posicion",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let de = |d: &FilaDesignacion| filas.iter().filter(|b| b.designacion_id == d.id).collect::<Vec<_>>();
    let mut anterior = GENESIS.to_owned();
    let mut cadena_verificada = true;
    for (i, d) in versiones.iter().enumerate() {
        let propios = de(d);
        let cuadra = d.version == i as i32 + 1
            && d.hash_anterior == anterior
            && sha256_hex(payload(d, &propios).as_bytes()) == d.hash
            && propios.len() == d.num_beneficiarios as usize;
        if !cuadra {
            tracing::error!(%cliente_id, version = d.version, "FALLA DE INTEGRIDAD: la cadena de beneficiarios no cuadra");
            cadena_verificada = false;
        }
        anterior = d.hash.clone();
    }
    let vigente = versiones.last().map(|d| Designacion {
        version: d.version,
        creada_en: d.creada_en,
        dispositivo: d.dispositivo.clone(),
        ip: d.ip.clone(),
        hash: d.hash.clone(),
        hash_anterior: d.hash_anterior.clone(),
        beneficiarios: de(d).into_iter().map(a_beneficiario).collect(),
    });
    let historial = versiones
        .iter()
        .rev()
        .take(HISTORIAL_MAXIMO)
        .map(|d| ResumenDesignacion {
            version: d.version,
            creada_en: d.creada_en,
            num_beneficiarios: d.num_beneficiarios,
            dispositivo: d.dispositivo.clone(),
            hash: d.hash.clone(),
        })
        .collect();
    Ok(VistaBeneficiarios {
        version: versiones.last().map_or(0, |d| d.version),
        vigente,
        historial,
        cadena_verificada,
        puede_designar: estatus.as_deref() == Some("VERIFICADO"),
        maximo: MAXIMO,
        parentescos: Parentesco::TODOS,
    })
}

// -----------------------------------------------------------------------------
// Escritura
// -----------------------------------------------------------------------------

/// Quién guarda: la sesión, su IP y su dispositivo.
pub struct Autor<'a> {
    pub sesion_id: Option<Uuid>,
    pub ip: Option<IpAddr>,
    pub dispositivo: Option<&'a str>,
}

/// Guarda una versión nueva de la designación, completa y atómica. Devuelve
/// la vista actualizada.
pub async fn designar(
    pool: &PgPool,
    cliente_id: Uuid,
    solicitud: &SolicitudDesignacion,
    autor: &Autor<'_>,
    hoy: NaiveDate,
) -> AppResult<VistaBeneficiarios> {
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let titular: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT estatus_kyc, curp FROM kyc.clientes WHERE id = $1")
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (estatus, curp_titular) = titular.ok_or(AppError::NoAutenticado)?;
    if estatus != "VERIFICADO" {
        return Err(AppError::negocio("KYC_NO_VERIFICADO", "verifica tu identidad (KYC) para designar beneficiarios"));
    }
    let beneficiarios = validar(&solicitud.beneficiarios, curp_titular.as_deref(), hoy)?;

    let vigente: i32 = sqlx::query_scalar(
        "SELECT coalesce(max(version), 0) FROM kyc.designaciones_beneficiarios WHERE cliente_id = $1",
    )
    .bind(cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    if vigente != solicitud.version_base {
        return Err(desactualizada());
    }

    let id = Uuid::new_v4();
    for b in &beneficiarios {
        sqlx::query(
            "INSERT INTO kyc.beneficiarios \
               (designacion_id, posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(b.posicion)
        .bind(&b.nombre_completo)
        .bind(b.parentesco)
        .bind(&b.curp)
        .bind(b.fecha_nacimiento)
        .bind(b.porcentaje)
        .execute(&mut *tx)
        .await?;
    }
    let total = if beneficiarios.is_empty() { Decimal::ZERO } else { Decimal::ONE_HUNDRED };
    let version: i32 = sqlx::query_scalar(
        "INSERT INTO kyc.designaciones_beneficiarios \
           (id, cliente_id, num_beneficiarios, total_porcentaje, sesion_id, ip, dispositivo) \
         VALUES ($1, $2, $3, $4, $5, $6::inet, $7) RETURNING version",
    )
    .bind(id)
    .bind(cliente_id)
    .bind(beneficiarios.len() as i16)
    .bind(total)
    .bind(autor.sesion_id)
    .bind(autor.ip.map(|i| i.to_string()))
    .bind(autor.dispositivo)
    .fetch_one(&mut *tx)
    .await?;
    // Las comprobaciones diferidas (suma, posiciones, titular, hash) corren aquí.
    tx.commit().await?;
    tracing::info!(target: "kyc", %cliente_id, version, beneficiarios = beneficiarios.len(), "beneficiarios designados");
    consultar(pool, cliente_id).await
}

/// La versión vigente cambió desde que el cliente empezó a editar (otra sesión
/// guardó antes). La versión única de 0020 da el mismo error si dos guardados
/// coinciden (error.rs).
fn desactualizada() -> AppError {
    AppError::conflicto(
        "DESIGNACION_DESACTUALIZADA",
        "tus beneficiarios cambiaron en otra sesión; vuelve a cargarlos antes de guardar",
    )
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use crate::kyc::curp::digito_verificador;

    fn curp(base17: &str) -> String {
        format!("{base17}{}", digito_verificador(base17).unwrap())
    }

    fn entrada(nombre: &str, curp: Option<&str>, fecha: Option<&str>, porcentaje: &str) -> BeneficiarioEntrada {
        BeneficiarioEntrada {
            nombre_completo: nombre.into(),
            parentesco: Parentesco::Hijo,
            curp: curp.map(Into::into),
            fecha_nacimiento: fecha.map(|f| f.parse().unwrap()),
            porcentaje: porcentaje.parse().unwrap(),
        }
    }

    fn hoy() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 23).unwrap()
    }

    #[test]
    fn nombres_legales() {
        assert_eq!(normalizar_nombre("  maría   López  garcía ").unwrap(), "MARÍA LÓPEZ GARCÍA");
        assert_eq!(normalizar_nombre("Jean-Luc O'Neil").unwrap(), "JEAN-LUC O'NEIL");
        assert!(normalizar_nombre("María").is_err(), "falta el apellido");
        assert!(normalizar_nombre("María L0pez").is_err());
        assert!(normalizar_nombre("María <López>").is_err());
        assert!(normalizar_nombre("- -").is_err());
        assert!(normalizar_nombre(&format!("ANA {}", "B".repeat(120))).is_err());
    }

    #[test]
    fn una_designacion_valida_se_normaliza() {
        let c = curp("LOGM900101MDFPRR0");
        let v = validar(
            &[
                entrada("maría lópez garcía", Some(&c.to_lowercase()), Some("1990-01-01"), "33.34"),
                entrada("José López", None, Some("2005-06-07"), "33.33"),
                entrada("Pedro Rojas", None, Some("1984-02-29"), "33.33"),
            ],
            None,
            hoy(),
        )
        .unwrap();
        assert_eq!(v.iter().map(|b| b.posicion).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(v[0].nombre_completo, "MARÍA LÓPEZ GARCÍA");
        assert_eq!(v[0].curp.as_deref(), Some(c.as_str()));
        assert_eq!(v[1].porcentaje.to_string(), "33.33");
        assert!(validar(&[], None, hoy()).unwrap().is_empty(), "vacía: retira la designación");
    }

    #[test]
    fn suma_exacta_y_a_centesimos() {
        let codigo = |r: AppResult<Vec<Beneficiario>>| r.unwrap_err().codigo().to_owned();
        let a = |p| entrada("Ana Pérez", None, Some("1990-01-01"), p);
        let b = |p| entrada("Beto Pérez", None, Some("1991-01-01"), p);
        assert_eq!(codigo(validar(&[a("60"), b("30")], None, hoy())), "BENEFICIARIOS_PORCENTAJE");
        assert_eq!(codigo(validar(&[a("60"), b("50")], None, hoy())), "BENEFICIARIOS_PORCENTAJE");
        assert_eq!(codigo(validar(&[a("33.333"), b("66.667")], None, hoy())), "VALIDACION");
        assert_eq!(codigo(validar(&[a("0"), b("100")], None, hoy())), "VALIDACION");
        assert_eq!(codigo(validar(&[a("-10"), b("110")], None, hoy())), "VALIDACION");
        assert!(validar(&[a("60.50"), b("39.5")], None, hoy()).is_ok());
        let seis: Vec<_> = (0..6).map(|_| a("10")).collect();
        assert_eq!(codigo(validar(&seis, None, hoy())), "BENEFICIARIOS_MAXIMO");
    }

    #[test]
    fn identidad_de_cada_beneficiario() {
        let codigo = |r: AppResult<Vec<Beneficiario>>| r.unwrap_err().codigo().to_owned();
        let c = curp("LOGM900101MDFPRR0");
        assert_eq!(codigo(validar(&[entrada("Ana Pérez", None, None, "100")], None, hoy())), "VALIDACION");
        assert_eq!(
            codigo(validar(&[entrada("Ana Pérez", Some(&c), Some("1990-02-01"), "100")], None, hoy())),
            "VALIDACION",
            "la fecha no es la de la CURP"
        );
        // La misma CURP con otro dígito verificador.
        let mala = format!("{}{}", &c[..17], if c.ends_with('0') { '1' } else { '0' });
        assert_eq!(codigo(validar(&[entrada("Ana Pérez", Some(&mala), None, "100")], None, hoy())), "VALIDACION");
        assert_eq!(
            codigo(validar(&[entrada("Ana Pérez", None, Some("2026-12-01"), "100")], None, hoy())),
            "VALIDACION",
            "futura"
        );
        assert_eq!(
            codigo(validar(&[entrada("Ana Pérez", None, Some("1899-12-31"), "100")], None, hoy())),
            "VALIDACION"
        );
        assert_eq!(
            codigo(validar(
                &[entrada("Ana Pérez", Some(&c), None, "50"), entrada("Otra Pérez", Some(&c), None, "50")],
                None,
                hoy()
            )),
            "BENEFICIARIO_DUPLICADO"
        );
        assert_eq!(
            codigo(validar(&[entrada("Ana Pérez", Some(&c), None, "100")], Some(&c), hoy())),
            "BENEFICIARIO_TITULAR"
        );
    }

    #[test]
    fn el_payload_codifica_cada_campo_sin_ambiguedad() {
        let d = FilaDesignacion {
            id: Uuid::nil(),
            cliente_id: Uuid::nil(),
            version: 2,
            num_beneficiarios: 1,
            total_porcentaje: "100".parse().unwrap(),
            sesion_id: None,
            ip: Some("203.0.113.7".into()),
            dispositivo: Some("Chrome on macOS".into()),
            hash_anterior: GENESIS.into(),
            hash: String::new(),
            creada_en: DateTime::from_timestamp(1_795_000_000, 123_456_000).unwrap(),
        };
        let b = FilaBeneficiario {
            designacion_id: Uuid::nil(),
            posicion: 1,
            nombre_completo: "MARÍA LÓPEZ".into(),
            parentesco: Parentesco::Hijo,
            curp: None,
            fecha_nacimiento: NaiveDate::from_ymd_opt(1990, 1, 1),
            porcentaje: "100".parse().unwrap(),
        };
        assert_eq!(
            payload(&d, &[&b]),
            format!(
                "11:SC-BENEF-v1|36:{n}|1:2|16:1795000000123456|1:1|6:100.00|~|11:203.0.113.7|15:Chrome on macOS\
                 |1:1|13:MARÍA LÓPEZ|4:HIJO|~|10:1990-01-01|8:100.0000|64:{g}",
                n = Uuid::nil(),
                g = GENESIS
            )
        );
    }
}
