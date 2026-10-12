//! Autenticación: contraseñas Argon2id, JWT HS256 y extractor de usuario.

pub mod jwt;
pub mod password;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    models::kyc::{EstatusKyc, RolUsuario},
    state::AppState,
};

/// Usuario autenticado (extraído del header `Authorization: Bearer <jwt>`).
#[derive(Debug, Clone, Copy)]
pub struct Usuario {
    pub cliente_id: Uuid,
    pub rol: RolUsuario,
}

impl Usuario {
    pub fn exigir_operador(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Operador => Ok(()),
            _ => Err(AppError::Prohibido),
        }
    }

    pub fn exigir_operador_o_auditor(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Operador | RolUsuario::Auditor => Ok(()),
            RolUsuario::Cliente => Err(AppError::Prohibido),
        }
    }

    pub fn exigir_cliente(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Cliente => Ok(()),
            _ => Err(AppError::Prohibido),
        }
    }

    /// Acota una clave de idempotencia al usuario, para que nadie pueda
    /// colisionar (ni sondear) las claves de otro.
    pub fn clave(&self, clave: &str) -> AppResult<String> {
        validar_clave_idempotencia(clave)?;
        let prefijo = match self.rol {
            RolUsuario::Cliente => "cli",
            RolUsuario::Operador => "op",
            RolUsuario::Auditor => "aud",
        };
        Ok(format!("{prefijo}:{}:{clave}", self.cliente_id))
    }
}

pub fn validar_clave_idempotencia(clave: &str) -> AppResult<()> {
    let valida = (8..=64).contains(&clave.len())
        && clave.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !valida {
        return Err(AppError::validacion(
            "clave_idempotencia: 8 a 64 caracteres [A-Za-z0-9-_.] (usa un UUID por operación)",
        ));
    }
    Ok(())
}

impl<S> FromRequestParts<S> for Usuario
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let estado = AppState::from_ref(state);
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AppError::NoAutenticado)?;
        let claims = estado.jwt.validar(token)?;
        Ok(Usuario { cliente_id: claims.sub, rol: claims.rol })
    }
}

// -----------------------------------------------------------------------------
// Inicio de sesión
// -----------------------------------------------------------------------------

const MAX_INTENTOS: i32 = 5;
const MINUTOS_BLOQUEO: i64 = 15;

#[derive(Debug, Clone, Serialize)]
pub struct Sesion {
    pub token: String,
    pub expira_en: DateTime<Utc>,
    pub cliente_id: Uuid,
    pub rol: RolUsuario,
}

#[derive(sqlx::FromRow)]
struct FilaLogin {
    cliente_id: Uuid,
    estatus_kyc: EstatusKyc,
    en_lista_bloqueadas: bool,
    password_hash: String,
    intentos_fallidos: i32,
    bloqueado_hasta: Option<DateTime<Utc>>,
    rol: RolUsuario,
}

pub async fn iniciar_sesion(estado: &AppState, email: &str, password_plano: &str) -> AppResult<Sesion> {
    let invalidas = || AppError::CredencialesInvalidas("email o contraseña incorrectos".into());
    let pool: &PgPool = &estado.pool;
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let fila: Option<FilaLogin> = sqlx::query_as(
        "SELECT c.id AS cliente_id, c.estatus_kyc, c.en_lista_bloqueadas, \
                cr.password_hash, cr.intentos_fallidos, cr.bloqueado_hasta, cr.rol \
         FROM kyc.clientes c JOIN kyc.credenciales cr ON cr.cliente_id = c.id \
         WHERE lower(c.email) = lower($1) FOR UPDATE OF cr",
    )
    .bind(email.trim())
    .fetch_optional(&mut *tx)
    .await?;

    let Some(fila) = fila else {
        // Mismo costo que una verificación real: no revela qué emails existen.
        password::verificar_simulado(password_plano.to_owned()).await;
        return Err(invalidas());
    };
    let ahora = Utc::now();
    if fila.bloqueado_hasta.is_some_and(|hasta| hasta > ahora) {
        return Err(AppError::Bloqueado("demasiados intentos fallidos; la cuenta está bloqueada temporalmente".into()));
    }
    let correcta = password::verificar(password_plano.to_owned(), fila.password_hash.clone()).await?;
    if !correcta {
        let intentos = fila.intentos_fallidos + 1;
        let (intentos, bloqueo) = if intentos >= MAX_INTENTOS {
            (0, Some(ahora + Duration::minutes(MINUTOS_BLOQUEO)))
        } else {
            (intentos, None)
        };
        sqlx::query("UPDATE kyc.credenciales SET intentos_fallidos = $1, bloqueado_hasta = $2 WHERE cliente_id = $3")
            .bind(intentos)
            .bind(bloqueo)
            .bind(fila.cliente_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?; // el conteo de fallos debe persistir
        return Err(invalidas());
    }
    if fila.en_lista_bloqueadas || fila.estatus_kyc == EstatusKyc::Rechazado {
        return Err(AppError::Prohibido);
    }
    sqlx::query(
        "UPDATE kyc.credenciales SET intentos_fallidos = 0, bloqueado_hasta = NULL, ultimo_acceso = now() \
         WHERE cliente_id = $1",
    )
    .bind(fila.cliente_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let (token, expira_en) = estado.jwt.emitir(fila.cliente_id, fila.rol)?;
    Ok(Sesion { token, expira_en, cliente_id: fila.cliente_id, rol: fila.rol })
}
