//! Alta de clientes, expediente KYC y bandeja PLD (rol `sc_kyc`).
//!
//! El alta crea, en una sola transacción, el cliente, sus credenciales y su
//! cuenta en la bóveda IFPE. El rol `sc_kyc` solo puede insertar las columnas
//! de identidad de `ifpe.cuentas` (`cliente_id`, `clabe`): los saldos nacen en
//! cero por DEFAULT y este código no tendría privilegio para fijarlos.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::password,
    clabe,
    db::{self, Rol},
    error::{AppError, AppResult},
    models::kyc::{AlertaPld, Cliente, EstatusKyc, NivelRiesgo, TipoPersona},
};

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

/// Alta de cliente + credenciales + cuenta IFPE. En modo demo el KYC queda
/// verificado al instante (riesgo BAJO) para poder operar de inmediato.
pub async fn registrar(pool: &PgPool, clabe_prefijo: &str, modo_demo: bool, d: DatosRegistro) -> AppResult<Registro> {
    let nombre = d.nombre.trim().to_string();
    if nombre.is_empty() {
        return Err(AppError::validacion("el nombre es obligatorio"));
    }
    let email = d.email.trim().to_lowercase();
    if email.len() > 254 || !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
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
    if d.ingreso_mensual_estimado.is_some_and(|i| i < Decimal::ZERO) {
        return Err(AppError::validacion("ingreso_mensual_estimado no puede ser negativo"));
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
    if modo_demo {
        sqlx::query(
            "UPDATE kyc.clientes SET estatus_kyc = 'VERIFICADO', kyc_verificado_en = now(), \
             nivel_riesgo_pld = 'BAJO', riesgo_evaluado_en = now() WHERE id = $1",
        )
        .bind(cliente_id)
        .execute(&mut *tx)
        .await?;
    }

    // Apertura de la cuenta IFPE. Colisión de CLABE aleatoria: reintentar.
    let mut cuenta = None;
    for _ in 0..8 {
        let candidata = clabe::generar(clabe_prefijo);
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictamenKyc {
    pub estatus_kyc: EstatusKyc,
    pub nivel_riesgo_pld: Option<NivelRiesgo>,
    pub nivel_cuenta: Option<i16>,
}

/// Dictamen del oficial de cumplimiento sobre el expediente KYC.
pub async fn dictaminar(pool: &PgPool, cliente_id: Uuid, d: &DictamenKyc) -> AppResult<Cliente> {
    if d.nivel_cuenta.is_some_and(|n| !(1..=4).contains(&n)) {
        return Err(AppError::validacion("nivel_cuenta debe estar entre 1 y 4"));
    }
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let cliente: Cliente = sqlx::query_as(
        "UPDATE kyc.clientes SET \
           estatus_kyc = $2, \
           kyc_verificado_en = CASE WHEN $2 = 'VERIFICADO' THEN now() ELSE kyc_verificado_en END, \
           nivel_riesgo_pld = coalesce($3, nivel_riesgo_pld), \
           riesgo_evaluado_en = CASE WHEN $3 IS NULL THEN riesgo_evaluado_en ELSE now() END, \
           nivel_cuenta = coalesce($4, nivel_cuenta) \
         WHERE id = $1 RETURNING *",
    )
    .bind(cliente_id)
    .bind(d.estatus_kyc)
    .bind(d.nivel_riesgo_pld)
    .bind(d.nivel_cuenta)
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
