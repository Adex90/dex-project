//! Contraseñas con Argon2id. El cómputo es costoso a propósito, así que se
//! ejecuta en el pool de hilos bloqueantes de Tokio.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

use tokio::sync::Semaphore;

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

/// Tope de cómputos Argon2 simultáneos (uno por núcleo): una ráfaga de
/// inicios de sesión no puede acaparar CPU ni hilos bloqueantes.
fn cupo_argon2() -> &'static Arc<Semaphore> {
    static CUPO: OnceLock<Arc<Semaphore>> = OnceLock::new();
    CUPO.get_or_init(|| Arc::new(Semaphore::new(std::thread::available_parallelism().map_or(2, |n| n.get()).max(2))))
}

/// Espera máxima por un turno de Argon2 antes de responder "reintenta".
const ESPERA_MAXIMA: Duration = Duration::from_secs(10);

/// Ejecuta `f` en el pool bloqueante con un permiso del cupo. El permiso se
/// mueve al hilo bloqueante: si el cliente se desconecta y la petición se
/// cancela, el permiso se libera cuando el hash termina, no antes, así que el
/// tope se respeta aunque haya cancelaciones.
async fn con_cupo<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    let permiso = tokio::time::timeout(ESPERA_MAXIMA, Arc::clone(cupo_argon2()).acquire_owned())
        .await
        .map_err(|_| AppError::Reintentable)?
        .map_err(|e| AppError::Interno(e.into()))?;
    tokio::task::spawn_blocking(move || {
        let _permiso = permiso;
        EJECUTANDO.fetch_add(1, Ordering::SeqCst);
        let resultado = f();
        EJECUTANDO.fetch_sub(1, Ordering::SeqCst);
        resultado
    })
    .await
    .map_err(|e| AppError::Interno(e.into()))?
}

static EJECUTANDO: AtomicUsize = AtomicUsize::new(0);

/// Cómputos Argon2 corriendo en este momento (para pruebas y métricas).
pub fn en_curso() -> usize {
    EJECUTANDO.load(Ordering::SeqCst)
}

/// Cómputos Argon2 simultáneos permitidos.
pub fn capacidad() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get()).max(2)
}

pub async fn hashear(password: String) -> AppResult<String> {
    con_cupo(move || hashear_sync(&password)).await
}

pub async fn verificar(password: String, hash: String) -> AppResult<bool> {
    con_cupo(move || {
        let parsed = PasswordHash::new(&hash).map_err(|e| AppError::Interno(anyhow::anyhow!("hash inválido: {e}")))?;
        Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
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

    /// Hallazgo #19: cancelar la petición (cliente desconectado) no libera el
    /// cupo mientras el hash sigue corriendo, así que nunca hay más cómputos
    /// simultáneos que la capacidad.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cupo_se_respeta_aunque_se_cancelen_peticiones() {
        let hash = hashear("contraseña de prueba larga".into()).await.unwrap();
        let maximo = Arc::new(AtomicUsize::new(0));
        let vigia = {
            let maximo = maximo.clone();
            tokio::spawn(async move {
                loop {
                    maximo.fetch_max(en_curso(), Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
        };
        for _ in 0..6 {
            let tareas: Vec<_> =
                (0..capacidad() * 2).map(|_| tokio::spawn(verificar("intento".into(), hash.clone()))).collect();
            tokio::time::sleep(Duration::from_millis(15)).await;
            for t in &tareas {
                t.abort();
            }
        }
        // Espera a que terminen los hashes huérfanos.
        while en_curso() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        vigia.abort();
        let pico = maximo.load(Ordering::Relaxed);
        assert!(pico <= capacidad(), "pico de {pico} cómputos con capacidad {}", capacidad());
        assert!(pico > 0);
    }

    #[test]
    fn politica() {
        assert!(validar_politica("corta").is_err());
        assert!(validar_politica("suficientemente-larga").is_ok());
    }
}
