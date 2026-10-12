//! Contraseñas con Argon2id. El cómputo es costoso a propósito, así que se
//! ejecuta en el pool de hilos bloqueantes de Tokio.

use std::sync::OnceLock;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

use crate::error::{AppError, AppResult};

pub const LONGITUD_MINIMA: usize = 12;

pub fn validar_politica(password: &str) -> AppResult<()> {
    let n = password.chars().count();
    if n < LONGITUD_MINIMA {
        return Err(AppError::validacion(format!("la contraseña debe tener al menos {LONGITUD_MINIMA} caracteres")));
    }
    if n > 128 {
        return Err(AppError::validacion("la contraseña admite como máximo 128 caracteres"));
    }
    Ok(())
}

fn hashear_sync(password: &str) -> AppResult<String> {
    let sal = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &sal)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Interno(anyhow::anyhow!("argon2: {e}")))
}

pub async fn hashear(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || hashear_sync(&password)).await.map_err(|e| AppError::Interno(e.into()))?
}

pub async fn verificar(password: String, hash: String) -> AppResult<bool> {
    tokio::task::spawn_blocking(move || {
        let parsed = PasswordHash::new(&hash).map_err(|e| AppError::Interno(anyhow::anyhow!("hash inválido: {e}")))?;
        Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
    .map_err(|e| AppError::Interno(e.into()))?
}

/// Verificación contra un hash ficticio para igualar el tiempo de respuesta
/// cuando el email no existe.
pub async fn verificar_simulado(password: String) {
    static FICTICIO: OnceLock<String> = OnceLock::new();
    let hash =
        FICTICIO.get_or_init(|| hashear_sync("contraseña-ficticia-para-igualar-tiempos").unwrap_or_default()).clone();
    let _ = verificar(password, hash).await;
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[tokio::test]
    async fn hashea_y_verifica() {
        let h = hashear("una contraseña larga".into()).await.unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verificar("una contraseña larga".into(), h.clone()).await.unwrap());
        assert!(!verificar("otra contraseña".into(), h).await.unwrap());
    }

    #[test]
    fn politica() {
        assert!(validar_politica("corta").is_err());
        assert!(validar_politica("suficientemente-larga").is_ok());
    }
}
