//! Identidad: alta de clientes, expediente KYC biométrico y bandeja PLD.
//!
//! El alta crea, en una sola transacción, el cliente, sus credenciales y su
//! cuenta en la bóveda IFPE. El rol `sc_kyc` solo puede insertar las columnas
//! de identidad de `ifpe.cuentas` (`cliente_id`, `clabe`): los saldos nacen en
//! cero por DEFAULT y este código no tendría privilegio para fijarlos.
//!
//! Un cliente recién dado de alta está PENDIENTE y la base de datos (RLS) le
//! impide recibir depósitos, transferir o invertir hasta que su expediente
//! quede VERIFICADO:
//!
//! 1. [`expediente::guardar_datos`]: datos personales, CURP, RFC y domicilio.
//! 2. [`verificar_identidad`]: INE (anverso y reverso) + selfie con prueba de
//!    vida -> proveedor biométrico -> [`verificacion::procesar`] (resultado
//!    firmado) -> VERIFICADO, revisión humana, reintento o RECHAZADO.
//! 3. [`dictamen::dictaminar`]: decisión del oficial de cumplimiento.

pub mod biometria;
pub mod curp;
pub mod dictamen;
pub mod expediente;
pub mod ine;
pub mod rfc;
pub mod texto;
pub mod verificacion;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::password,
    config::ConfigKyc,
    db::{self, Rol},
    dinero,
    error::{AppError, AppResult},
    models::kyc::{AlertaPld, Cliente, TipoPersona},
    spei::{self, clabe},
};

pub use dictamen::{dictaminar, DictamenKyc};
use biometria::{Entrega, ProveedorBiometrico, SolicitudProveedor};
use expediente::{EstadoKyc, IdentidadPropia, OrigenEnvio, SolicitudVerificacion};
use verificacion::ResultadoVerificacion;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatosRegistro {
    pub tipo_persona: TipoPersona,
    pub nombre: String,
    pub apellido_paterno: Option<String>,
    pub apellido_materno: Option<String>,
    pub email: String,
    pub password: String,
    pub curp: Option<String>,
    pub rfc: Option<String>,
    pub telefono: Option<String>,
    pub actividad_economica: Option<String>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub ingreso_mensual_estimado: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuentaAbierta {
    pub id: Uuid,
    pub clabe: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Registro {
    pub cliente: Cliente,
    pub cuenta_ifpe: CuentaAbierta,
}

fn texto_opcional(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn exigir_longitud(campo: &str, v: Option<&str>, max: usize) -> AppResult<()> {
    if v.is_some_and(|s| s.chars().count() > max) {
        return Err(AppError::validacion(format!("{campo} admite como máximo {max} caracteres")));
    }
    Ok(())
}

/// Mismas reglas que los CHECK de `kyc.clientes`. Se validan antes de
/// calcular Argon2: una entrada inválida responde 400 sin costo de CPU (antes
/// llegaba a la base de datos y respondía 500).
fn rfc_valido(rfc: &str) -> bool {
    // ^[A-ZÑ&]{3,4}[0-9]{6}[A-Z0-9]{3}$
    let c: Vec<char> = rfc.chars().collect();
    let letras = match c.len() {
        12 => 3,
        13 => 4,
        _ => return false,
    };
    let (prefijo, resto) = c.split_at(letras);
    prefijo.iter().all(|ch| ch.is_ascii_uppercase() || *ch == 'Ñ' || *ch == '&')
        && resto[..6].iter().all(char::is_ascii_digit)
        && resto[6..].iter().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        && rfc::digito_verificador(&c[..c.len() - 1].iter().collect::<String>()) == c.last().copied()
}

fn email_valido(email: &str) -> bool {
    // ^[^@\s]+@[^@\s]+\.[^@\s]+$
    let Some((local, dominio)) = email.split_once('@') else { return false };
    let sin_espacios = |s: &str| !s.is_empty() && !s.contains('@') && !s.chars().any(char::is_whitespace);
    sin_espacios(local)
        && sin_espacios(dominio)
        && dominio.char_indices().any(|(i, ch)| ch == '.' && i > 0 && i + 1 < dominio.len())
}

fn telefono_valido(t: &str) -> bool {
    // ^\+?[0-9]{10,15}$
    let digitos = t.strip_prefix('+').unwrap_or(t);
    (10..=15).contains(&digitos.len()) && digitos.bytes().all(|b| b.is_ascii_digit())
}

/// Alta de cliente + credenciales + cuenta IFPE. El expediente KYC nace
/// PENDIENTE (lo crea la base de datos) y la cuenta no opera hasta que el
/// cliente complete la verificación de identidad.
pub async fn registrar(pool: &PgPool, clabe_prefijo: &str, d: DatosRegistro) -> AppResult<Registro> {
    let nombre = d.nombre.trim().to_string();
    if nombre.is_empty() {
        return Err(AppError::validacion("el nombre es obligatorio"));
    }
    let email = d.email.trim().to_lowercase();
    if email.len() > 254 || !email_valido(&email) {
        return Err(AppError::validacion("email inválido"));
    }
    password::validar_politica(&d.password)?;
    let apellido_paterno = texto_opcional(d.apellido_paterno);
    let apellido_materno = texto_opcional(d.apellido_materno);
    let curp = texto_opcional(d.curp).map(|s| s.to_uppercase());
    let rfc = texto_opcional(d.rfc).map(|s| s.to_uppercase());
    let telefono = texto_opcional(d.telefono);
    let actividad = texto_opcional(d.actividad_economica);
    for (campo, valor) in [
        ("nombre", Some(nombre.as_str())),
        ("apellido_paterno", apellido_paterno.as_deref()),
        ("apellido_materno", apellido_materno.as_deref()),
        ("actividad_economica", actividad.as_deref()),
    ] {
        exigir_longitud(campo, valor, 120)?;
    }
    if d.tipo_persona == TipoPersona::Fisica && apellido_paterno.is_none() {
        return Err(AppError::validacion("apellido_paterno es obligatorio para persona física"));
    }
    if d.tipo_persona == TipoPersona::Moral && curp.is_some() {
        return Err(AppError::validacion("una persona moral no tiene CURP"));
    }
    if rfc.as_deref().is_some_and(|r| !rfc_valido(r)) {
        return Err(AppError::validacion(
            "RFC inválido (12 o 13 caracteres: letras, fecha AAMMDD, homoclave y dígito verificador)",
        ));
    }
    if let Some(c) = curp.as_deref() {
        curp::Curp::parse(c).map_err(|e| AppError::validacion(e.to_string()))?;
    }
    if telefono.as_deref().is_some_and(|t| !telefono_valido(t)) {
        return Err(AppError::validacion("teléfono inválido (10 a 15 dígitos, con + opcional)"));
    }
    if let Some(ingreso) = d.ingreso_mensual_estimado {
        if ingreso < Decimal::ZERO {
            return Err(AppError::validacion("ingreso_mensual_estimado no puede ser negativo"));
        }
        if ingreso > dinero::maximo() || ingreso.normalize().scale() > dinero::ESCALA {
            return Err(AppError::validacion("ingreso_mensual_estimado fuera de rango"));
        }
    }
    let hash = password::hashear(d.password).await?;

    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let cliente_id: Uuid = sqlx::query_scalar(
        "INSERT INTO kyc.clientes \
         (tipo_persona, nombre, apellido_paterno, apellido_materno, email, curp, rfc, telefono, \
          actividad_economica, ingreso_mensual_estimado) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(d.tipo_persona)
    .bind(&nombre)
    .bind(&apellido_paterno)
    .bind(&apellido_materno)
    .bind(&email)
    .bind(&curp)
    .bind(&rfc)
    .bind(&telefono)
    .bind(&actividad)
    .bind(d.ingreso_mensual_estimado)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO kyc.credenciales (cliente_id, password_hash) VALUES ($1, $2)")
        .bind(cliente_id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;

    // Apertura de la cuenta IFPE. Colisión de CLABE aleatoria: reintentar.
    let mut cuenta = None;
    for _ in 0..8 {
        let candidata = clabe::generar(clabe_prefijo).to_string();
        cuenta = sqlx::query_as::<_, (Uuid, String)>(
            "INSERT INTO ifpe.cuentas (cliente_id, clabe) VALUES ($1, $2) \
             ON CONFLICT (clabe) DO NOTHING RETURNING id, clabe",
        )
        .bind(cliente_id)
        .bind(&candidata)
        .fetch_optional(&mut *tx)
        .await?;
        if cuenta.is_some() {
            break;
        }
    }
    let (cuenta_id, cuenta_clabe) =
        cuenta.ok_or_else(|| AppError::Interno(anyhow::anyhow!("no se pudo asignar una CLABE única")))?;
    let cliente: Cliente =
        sqlx::query_as("SELECT * FROM kyc.clientes WHERE id = $1").bind(cliente_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    tracing::info!(cliente = %cliente_id, "cliente registrado con cuenta IFPE");
    Ok(Registro { cliente, cuenta_ifpe: CuentaAbierta { id: cuenta_id, clabe: cuenta_clabe } })
}

pub async fn perfil(pool: &PgPool, cliente_id: Uuid) -> AppResult<Cliente> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let cliente = sqlx::query_as("SELECT * FROM kyc.clientes WHERE id = $1")
        .bind(cliente_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("cliente no encontrado"))?;
    tx.commit().await?;
    Ok(cliente)
}

pub async fn alertas_abiertas(pool: &PgPool, limite: i64) -> AppResult<Vec<AlertaPld>> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let alertas = sqlx::query_as(
        "SELECT * FROM kyc.alertas_pld WHERE estado IN ('ABIERTA', 'EN_REVISION') \
         ORDER BY CASE nivel_riesgo WHEN 'CRITICO' THEN 0 WHEN 'ALTO' THEN 1 WHEN 'MEDIO' THEN 2 ELSE 3 END, creado_en \
         LIMIT $1",
    )
    .bind(limite.clamp(1, 500))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(alertas)
}

/// Respuesta del envío de INE + selfie.
#[derive(Debug, Clone, Serialize)]
pub struct EnvioVerificacion {
    pub verificacion_id: uuid::Uuid,
    pub estado_kyc: EstadoKyc,
    pub intentos: i16,
    /// Dictamen inmediato (proveedor síncrono). `None`: el resultado llegará
    /// por el webhook del proveedor y el expediente sigue EN_REVISION.
    pub resultado: Option<ResultadoVerificacion>,
}

/// Pasos 2 y 3 del onboarding: envía INE y selfie al proveedor y procesa su
/// resultado firmado por el mismo camino que el webhook.
pub async fn verificar_identidad(
    pool: &PgPool,
    cfg: &ConfigKyc,
    proveedor: &dyn ProveedorBiometrico,
    cliente_id: uuid::Uuid,
    s: &SolicitudVerificacion,
    origen: &OrigenEnvio,
) -> AppResult<EnvioVerificacion> {
    let imagenes = expediente::decodificar_imagenes(s)?;
    let enviado = expediente::enviar(pool, cliente_id, s, &imagenes, origen, spei::hoy_cdmx()).await?;
    let verificacion_id = enviado
        .verificacion_en_curso
        .ok_or_else(|| AppError::Interno(anyhow::anyhow!("envío sin id de verificación")))?;
    let solicitud = SolicitudProveedor { verificacion_id, cliente_id, identidad: IdentidadPropia::de(&enviado)?, imagenes };
    let pendiente = EnvioVerificacion { verificacion_id, estado_kyc: enviado.estado_kyc, intentos: enviado.intentos, resultado: None };
    match proveedor.verificar(&solicitud).await {
        Ok(Entrega::Inmediata { cuerpo, firma }) => {
            let r = verificacion::procesar(pool, cfg, Some(&firma), &cuerpo).await?;
            Ok(EnvioVerificacion { estado_kyc: r.estado_kyc, intentos: r.intentos, resultado: Some(r), ..pendiente })
        }
        Ok(Entrega::Pendiente) => Ok(pendiente),
        // El expediente queda EN_REVISION: el resultado puede llegar después
        // por el webhook, o un operador lo reabre.
        Err(e) => {
            tracing::warn!(cliente = %cliente_id, error = %e, proveedor = proveedor.nombre(), "proveedor biométrico no disponible");
            Ok(pendiente)
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn identidades_como_los_check_de_la_base() {
        assert!(rfc_valido("GODE561231GR8"));
        assert!(rfc_valido(&rfc::construir_desde_curp("LOOA531113HTCPBN07", "AB")));
        assert!(!rfc_valido("GODE561231GR9"));
        assert!(!rfc_valido("ABCDEFGHIJKLMNOPQ"));
        assert!(!rfc_valido("GODE5612311GR8"));
        assert!(curp::Curp::parse("LOOA531113HTCPBN07").is_ok());
        assert!(curp::Curp::parse("GODE561231HDFRRN09XXXXX").is_err());
        assert!(curp::Curp::parse("GODE561231ZDFRRN09").is_err());
        // Estructura válida pero dígito verificador incorrecto.
        assert!(curp::Curp::parse("GODE561231HDFRRN09").is_err());
        assert!(email_valido("ana@banco.mx"));
        assert!(!email_valido("ana@banco"));
        assert!(!email_valido("ana @banco.mx"));
        assert!(!email_valido("ana@.mx"));
        assert!(telefono_valido("+525512345678"));
        assert!(!telefono_valido("12345"));
    }
}
