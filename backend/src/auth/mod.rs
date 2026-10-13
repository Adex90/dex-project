//! Autenticación: contraseñas Argon2id, JWT HS256 y extractor de usuario.

pub mod jwt;
pub mod password;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use chrono::{DateTime, Utc};
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
        validar_contra_bd(&estado.pool, &claims).await
    }
}

#[derive(sqlx::FromRow)]
struct FilaSesion {
    rol: RolUsuario,
    estatus_kyc: EstatusKyc,
    en_lista_bloqueadas: bool,
    revocado: bool,
}

/// La firma del JWT no basta: en cada petición se confirma contra la base de
/// datos que la persona existe, que su rol **actual** coincide con el del
/// token, que sus sesiones no fueron revocadas y que no está bloqueada. Así,
/// degradar a un operador o revocar sus sesiones surte efecto de inmediato.
async fn validar_contra_bd(pool: &PgPool, claims: &jwt::Claims) -> AppResult<Usuario> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let fila: Option<FilaSesion> = sqlx::query_as(
        "SELECT cr.rol, c.estatus_kyc, c.en_lista_bloqueadas, \
                coalesce(floor(extract(epoch FROM cr.tokens_validos_desde)) > $2, false) AS revocado \
         FROM kyc.credenciales cr JOIN kyc.clientes c ON c.id = cr.cliente_id \
         WHERE cr.cliente_id = $1",
    )
    .bind(claims.sub)
    .bind(claims.iat)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let fila = fila.ok_or(AppError::NoAutenticado)?;
    if fila.revocado || fila.rol != claims.rol {
        return Err(AppError::NoAutenticado);
    }
    if fila.en_lista_bloqueadas || fila.estatus_kyc == EstatusKyc::Rechazado {
        return Err(AppError::Prohibido);
    }
    Ok(Usuario { cliente_id: claims.sub, rol: fila.rol })
}

/// Invalida todos los JWT emitidos hasta ahora para una persona.
pub async fn revocar_sesiones(pool: &PgPool, cliente_id: Uuid) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let n = sqlx::query("UPDATE kyc.credenciales SET tokens_validos_desde = now() WHERE cliente_id = $1")
        .bind(cliente_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::no_encontrado("persona no encontrada"));
    }
    tx.commit().await?;
    Ok(())
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
    bloqueado_hasta: Option<DateTime<Utc>>,
}

/// Inicio de sesión en tres pasos para no retener una conexión del pool (ni
/// un lock de fila) durante el cómputo de Argon2:
/// 1. lectura corta de las credenciales;
/// 2. verificación Argon2 sin conexión (con tope de concurrencia);
/// 3. registro atómico del resultado (conteo de fallos / bloqueo temporal).
pub async fn iniciar_sesion(estado: &AppState, email: &str, password_plano: &str) -> AppResult<Sesion> {
    let invalidas = || AppError::CredencialesInvalidas("email o contraseña incorrectos".into());
    let bloqueada =
        || AppError::Bloqueado("demasiados intentos fallidos; la cuenta está bloqueada temporalmente".into());
    let pool: &PgPool = &estado.pool;

    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let fila: Option<FilaLogin> = sqlx::query_as(
        "SELECT c.id AS cliente_id, c.estatus_kyc, c.en_lista_bloqueadas, cr.password_hash, cr.bloqueado_hasta \
         FROM kyc.clientes c JOIN kyc.credenciales cr ON cr.cliente_id = c.id \
         WHERE lower(c.email) = lower($1)",
    )
    .bind(email.trim())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    let Some(fila) = fila else {
        // Mismo costo que una verificación real: no revela qué emails existen.
        password::verificar_simulado(password_plano.to_owned()).await;
        return Err(invalidas());
    };
    if fila.bloqueado_hasta.is_some_and(|hasta| hasta > Utc::now()) {
        return Err(bloqueada());
    }

    let correcta = password::verificar(password_plano.to_owned(), fila.password_hash.clone()).await?;

    let mut tx = db::begin(pool, Rol::Kyc).await?;
    if !correcta {
        // Incremento atómico: intentos concurrentes no se pierden.
        sqlx::query(
            "UPDATE kyc.credenciales SET \
               intentos_fallidos = CASE WHEN intentos_fallidos + 1 >= $2 THEN 0 ELSE intentos_fallidos + 1 END, \
               bloqueado_hasta = CASE WHEN intentos_fallidos + 1 >= $2 \
                                      THEN now() + make_interval(mins => $3) ELSE bloqueado_hasta END \
             WHERE cliente_id = $1 AND (bloqueado_hasta IS NULL OR bloqueado_hasta <= now())",
        )
        .bind(fila.cliente_id)
        .bind(MAX_INTENTOS)
        .bind(MINUTOS_BLOQUEO as i32)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Err(invalidas());
    }
    if fila.en_lista_bloqueadas || fila.estatus_kyc == EstatusKyc::Rechazado {
        return Err(AppError::Prohibido);
    }
    // Si otra petición bloqueó la cuenta mientras se verificaba, no se abre sesión.
    let rol: Option<RolUsuario> = sqlx::query_scalar(
        "UPDATE kyc.credenciales SET intentos_fallidos = 0, bloqueado_hasta = NULL, ultimo_acceso = now() \
         WHERE cliente_id = $1 AND (bloqueado_hasta IS NULL OR bloqueado_hasta <= now()) RETURNING rol",
    )
    .bind(fila.cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let rol = rol.ok_or_else(bloqueada)?;
    let (token, expira_en) = estado.jwt.emitir(fila.cliente_id, rol)?;
    Ok(Sesion { token, expira_en, cliente_id: fila.cliente_id, rol })
}
