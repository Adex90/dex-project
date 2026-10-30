//! Restablecimiento de contraseña por enlace de un solo uso.
//!
//! - `solicitar` responde igual exista o no el email (no revela cuentas). Si
//!   existe, genera un token de 256 bits y guarda **solo su hash** SHA-256 con
//!   un vencimiento de [`MINUTOS_VIGENCIA`]; el token viaja únicamente en el
//!   correo (simulado, ver [`super::correo`]). Un nuevo pedido antes de
//!   [`SEGUNDOS_ESPERA`] no genera otro correo.
//! - El enlace lleva el token en el fragmento (`#token=`): el navegador no lo
//!   envía al servidor, así no queda en bitácoras de acceso ni en `Referer`.
//! - `confirmar` comprueba primero el token (barato) y solo entonces calcula
//!   Argon2, así tokens al azar no consumen CPU. Luego cambia la contraseña y
//!   anula el token en una sola sentencia (un solo uso, también en paralelo),
//!   desbloquea la cuenta y **revoca todas las sesiones**. El segundo factor
//!   sigue activo: el siguiente inicio de sesión vuelve a pedir el código.

use std::net::IpAddr;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{mfa::registrar_evento, password};
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    state::AppState,
};

pub const MINUTOS_VIGENCIA: i64 = 30;
pub const SEGUNDOS_ESPERA: i64 = 60;
const BYTES_TOKEN: usize = 32;

/// SHA-256 en hexadecimal: lo único que guarda la base de datos.
fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn token_invalido() -> AppError {
    AppError::negocio(
        "TOKEN_RESTABLECIMIENTO_INVALIDO",
        "el enlace de restablecimiento no es válido o ya venció; solicita uno nuevo",
    )
}

/// Pide un enlace de restablecimiento para `email`. Siempre `Ok` si el email
/// tiene forma de email (exista o no la cuenta).
pub async fn solicitar(estado: &AppState, email: &str, ip: Option<IpAddr>) -> AppResult<()> {
    let email = email.trim().to_lowercase();
    if email.is_empty() || email.len() > 254 || !email.contains('@') {
        return Err(AppError::validacion("email inválido"));
    }
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let fila: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT cr.cliente_id, \
                coalesce(cr.token_expires_at > now() + make_interval(secs => $2), false) AS reciente \
         FROM kyc.clientes c JOIN kyc.credenciales cr ON cr.cliente_id = c.id \
         WHERE lower(c.email) = $1 FOR UPDATE OF cr",
    )
    .bind(&email)
    .bind((MINUTOS_VIGENCIA * 60 - SEGUNDOS_ESPERA) as f64)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((cliente_id, reciente)) = fila else {
        tx.commit().await?;
        return Ok(());
    };
    if reciente {
        tx.commit().await?;
        tracing::info!(target: "auth", %cliente_id, "restablecimiento repetido antes de la espera mínima: sin correo nuevo");
        return Ok(());
    }
    let mut crudo = [0u8; BYTES_TOKEN];
    rand::rngs::OsRng.fill_bytes(&mut crudo);
    let token = URL_SAFE_NO_PAD.encode(crudo);
    sqlx::query(
        "UPDATE kyc.credenciales SET password_reset_token = $2, \
           token_expires_at = now() + make_interval(mins => $3) WHERE cliente_id = $1",
    )
    .bind(cliente_id)
    .bind(hash_token(&token))
    .bind(MINUTOS_VIGENCIA as i32)
    .execute(&mut *tx)
    .await?;
    registrar_evento(&mut tx, cliente_id, "RESTABLECIMIENTO_SOLICITADO", ip).await?;
    tx.commit().await?;
    let enlace = format!("{}/auth/reset-password#token={token}", estado.config.frontend_origin.trim_end_matches('/'));
    estado.correo.restablecimiento(cliente_id, &email, enlace);
    Ok(())
}

/// Cambia la contraseña con el token del enlace.
pub async fn confirmar(estado: &AppState, token: &str, password_nueva: &str, ip: Option<IpAddr>) -> AppResult<()> {
    password::validar_politica(password_nueva)?;
    let token = token.trim();
    if token.is_empty() || token.len() > 128 {
        return Err(token_invalido());
    }
    let hash = hash_token(token);

    // 1. Barato: ¿hay un token vigente con ese hash?
    let mut tx = db::begin_lectura(&estado.pool, Rol::Auth).await?;
    let vigente: Option<Uuid> = sqlx::query_scalar(
        "SELECT cliente_id FROM kyc.credenciales WHERE password_reset_token = $1 AND token_expires_at > now()",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    if vigente.is_none() {
        return Err(token_invalido());
    }

    // 2. Argon2 sin retener una conexión.
    let nuevo_hash = password::hashear(password_nueva.to_owned()).await?;

    // 3. Un solo uso: la misma condición, en la misma sentencia que el cambio.
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let cliente_id: Option<Uuid> = sqlx::query_scalar(
        "UPDATE kyc.credenciales SET password_hash = $2, password_reset_token = NULL, token_expires_at = NULL, \
           intentos_fallidos = 0, bloqueado_hasta = NULL, tokens_validos_desde = now() \
         WHERE password_reset_token = $1 AND token_expires_at > now() RETURNING cliente_id",
    )
    .bind(&hash)
    .bind(&nuevo_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(cliente_id) = cliente_id else {
        return Err(token_invalido());
    };
    registrar_evento(&mut tx, cliente_id, "CONTRASENA_RESTABLECIDA", ip).await?;
    tx.commit().await?;
    tracing::info!(target: "auth", %cliente_id, "contraseña restablecida; sesiones revocadas");
    Ok(())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn el_hash_cumple_el_formato_de_la_base_de_datos() {
        let h = hash_token("abc");
        assert_eq!(h.len(), 64);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(hash_token("abc"), hash_token("abd"));
    }
}
