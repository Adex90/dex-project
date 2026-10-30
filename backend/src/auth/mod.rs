//! Autenticación: contraseñas Argon2id, JWT HS256, segundo factor TOTP,
//! restablecimiento de contraseña y extractor de usuario.

pub mod correo;
pub mod jwt;
pub mod mfa;
pub mod password;
pub mod restablecimiento;

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
    models::kyc::RolUsuario,
    state::AppState,
};

/// Usuario autenticado (extraído del header `Authorization: Bearer <jwt>`).
#[derive(Debug, Clone, Copy)]
pub struct Usuario {
    pub cliente_id: Uuid,
    pub rol: RolUsuario,
    /// La sesión verificó el segundo factor. Siempre `true` para el personal
    /// interno: sin él, el extractor rechaza la petición (`MFA_REQUERIDO`).
    pub mfa: bool,
}

impl Usuario {
    /// Sesión con segundo factor verificado (la consola PLD lo exige además del rol).
    pub fn exigir_mfa(&self) -> AppResult<()> {
        if self.mfa {
            Ok(())
        } else {
            Err(AppError::MfaRequerido)
        }
    }

    pub fn exigir_operador(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Operador => Ok(()),
            _ => Err(AppError::Prohibido),
        }
    }

    pub fn exigir_operador_o_auditor(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Operador | RolUsuario::Auditor => Ok(()),
            RolUsuario::Cliente | RolUsuario::Cumplimiento => Err(AppError::Prohibido),
        }
    }

    /// Acciones de la consola PLD: solo el oficial de cumplimiento. La base de
    /// datos lo repite (`aml.exigir_oficial`, `SC_AML_NO_AUTORIZADO`).
    pub fn exigir_cumplimiento(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Cumplimiento => Ok(()),
            _ => Err(AppError::Prohibido),
        }
    }

    /// Lectura de la consola PLD: oficial de cumplimiento o auditor.
    pub fn exigir_cumplimiento_o_auditor(&self) -> AppResult<()> {
        match self.rol {
            RolUsuario::Cumplimiento | RolUsuario::Auditor => Ok(()),
            RolUsuario::Cliente | RolUsuario::Operador => Err(AppError::Prohibido),
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
            RolUsuario::Cumplimiento => "cum",
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
        // Ya validado por una capa de la ruta en esta misma petición (las
        // extensiones solo las escribe el servidor, nunca el cliente).
        if let Some(u) = parts.extensions.get::<Usuario>() {
            return Ok(*u);
        }
        let estado = AppState::from_ref(state);
        let claims = estado.jwt.validar(bearer(parts)?)?;
        validar_contra_bd(&estado.pool, &claims).await
    }
}

fn bearer(parts: &Parts) -> AppResult<&str> {
    parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::NoAutenticado)
}

#[derive(sqlx::FromRow)]
struct FilaSesion {
    rol: RolUsuario,
    en_lista_bloqueadas: bool,
    revocado: bool,
    mfa_enabled: bool,
}

async fn leer_sesion(pool: &PgPool, claims: &jwt::Claims) -> AppResult<Option<FilaSesion>> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let fila: Option<FilaSesion> = sqlx::query_as(
        "SELECT cr.rol, c.en_lista_bloqueadas, cr.mfa_enabled, \
                coalesce(floor(extract(epoch FROM cr.tokens_validos_desde)) > $2, false) AS revocado \
         FROM kyc.credenciales cr JOIN kyc.clientes c ON c.id = cr.cliente_id \
         WHERE cr.cliente_id = $1",
    )
    .bind(claims.sub)
    .bind(claims.iat)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fila)
}

/// La firma del JWT no basta: en cada petición se confirma contra la base de
/// datos que la persona existe, que su rol **actual** coincide con el del
/// token, que sus sesiones no fueron revocadas y que no está bloqueada. Así,
/// degradar a un operador o revocar sus sesiones surte efecto de inmediato.
///
/// Segundo factor: si la persona lo tiene activo, la sesión debe haberlo
/// verificado; el personal interno, además, no opera sin él (`MFA_REQUERIDO`).
async fn validar_contra_bd(pool: &PgPool, claims: &jwt::Claims) -> AppResult<Usuario> {
    let fila = leer_sesion(pool, claims).await?.ok_or(AppError::NoAutenticado)?;
    if fila.revocado || fila.rol != claims.rol {
        return Err(AppError::NoAutenticado);
    }
    if fila.en_lista_bloqueadas {
        return Err(AppError::Prohibido);
    }
    if fila.mfa_enabled && !claims.mfa {
        return Err(AppError::NoAutenticado);
    }
    let mfa = claims.mfa && fila.mfa_enabled;
    if mfa::exige_mfa(fila.rol) && !mfa {
        return Err(AppError::MfaRequerido);
    }
    Ok(Usuario { cliente_id: claims.sub, rol: fila.rol, mfa })
}

/// Quien inscribe su segundo factor (`/auth/mfa/setup`, `/auth/mfa/enable`):
/// el personal interno con el token de inscripción que le dio el inicio de
/// sesión, o cualquiera con una sesión de acceso válida (un cliente que lo
/// activa por su cuenta).
#[derive(Debug, Clone, Copy)]
pub struct IdentidadMfa {
    pub cliente_id: Uuid,
    pub rol: RolUsuario,
}

impl<S> FromRequestParts<S> for IdentidadMfa
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let estado = AppState::from_ref(state);
        let token = bearer(parts)?;
        if let Ok(claims) = estado.jwt.validar_desafio(token, jwt::Desafio::Inscripcion) {
            let fila = leer_sesion(&estado.pool, &claims).await?.ok_or(AppError::DesafioVencido)?;
            if fila.revocado || fila.rol != claims.rol {
                return Err(AppError::DesafioVencido);
            }
            if fila.en_lista_bloqueadas {
                return Err(AppError::Prohibido);
            }
            return Ok(Self { cliente_id: claims.sub, rol: fila.rol });
        }
        let u = validar_contra_bd(&estado.pool, &estado.jwt.validar(token)?).await?;
        Ok(Self { cliente_id: u.cliente_id, rol: u.rol })
    }
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

/// Cuenta un intento fallido (contraseña o código TOTP) y, al llegar a
/// `MAX_INTENTOS` ($2), bloquea la cuenta `MINUTOS_BLOQUEO` ($3) minutos. Una
/// ventana de bloqueo vencida empieza de nuevo en 1.
pub(crate) const SQL_REGISTRAR_FALLO: &str = "UPDATE kyc.credenciales SET \
       intentos_fallidos = CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END, \
       bloqueado_hasta = CASE WHEN (CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END) >= $2 \
                              THEN now() + make_interval(mins => $3) ELSE NULL END \
     WHERE cliente_id = $1";

#[derive(Debug, Clone, Serialize)]
pub struct Sesion {
    pub token: String,
    pub expira_en: DateTime<Utc>,
    pub cliente_id: Uuid,
    pub rol: RolUsuario,
}

/// Respuesta de `POST /auth/login` (y de los pasos que completan la sesión).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "estado")]
pub enum ResultadoLogin {
    /// Sesión de acceso.
    #[serde(rename = "OK")]
    Sesion(Sesion),
    /// Contraseña correcta; falta el código TOTP: `POST /auth/verify-mfa` con `token_mfa`.
    #[serde(rename = "2FA_REQUIRED")]
    MfaRequerido { token_mfa: String, expira_en: DateTime<Utc> },
    /// Personal interno sin segundo factor: inscribirlo con `token_inscripcion`
    /// (`/auth/mfa/setup` y `/auth/mfa/enable`) antes de operar.
    #[serde(rename = "MFA_ENROLLMENT_REQUIRED")]
    InscripcionRequerida { token_inscripcion: String, expira_en: DateTime<Utc> },
}

#[derive(sqlx::FromRow)]
struct FilaLogin {
    cliente_id: Uuid,
    en_lista_bloqueadas: bool,
    rol: RolUsuario,
    mfa_enabled: bool,
    /// La propia reserva de este intento bloqueó la cuenta (era el último del cupo).
    bloqueada: bool,
}

/// Inicio de sesión sin retener una conexión del pool (ni un lock de fila)
/// durante el cómputo de Argon2:
///
/// 1. **Reserva del intento** (una sentencia atómica): cuenta el intento como
///    fallido *antes* de verificar y, al llegar a `MAX_INTENTOS`, bloquea la
///    cuenta. Si ya está bloqueada no devuelve fila y no se calcula ningún
///    hash. Así, una ráfaga de N contraseñas en paralelo solo verifica
///    `MAX_INTENTOS`; las demás reciben `BLOQUEADO` sin importar si eran
///    correctas, y la respuesta no revela nada.
/// 2. Verificación Argon2 sin conexión (con tope de concurrencia).
/// 3. Contraseña correcta:
///    - con segundo factor activo: un desafío (`2FA_REQUIRED`) y **ningún**
///      token de acceso. El intento sigue contado hasta que el código TOTP se
///      verifique, así repetir la contraseña no regala intentos de código;
///    - personal interno sin segundo factor: un token de inscripción
///      (`MFA_ENROLLMENT_REQUIRED`), nunca uno de acceso;
///    - cliente sin segundo factor: la sesión, y el contador vuelve a cero
///      (también si la propia reserva acababa de bloquear la cuenta: el intento
///      estaba dentro del cupo).
pub async fn iniciar_sesion(estado: &AppState, email: &str, password_plano: &str) -> AppResult<ResultadoLogin> {
    let invalidas = || AppError::CredencialesInvalidas("email o contraseña incorrectos".into());
    let bloqueada =
        || AppError::Bloqueado("demasiados intentos fallidos; la cuenta está bloqueada temporalmente".into());
    let pool: &PgPool = &estado.pool;

    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let cliente: Option<Uuid> = sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE lower(email) = lower($1)")
        .bind(email.trim())
        .fetch_optional(&mut *tx)
        .await?;
    let Some(cliente_id) = cliente else {
        tx.commit().await?;
        // Mismo costo que una verificación real: no revela qué emails existen.
        password::verificar_simulado(password_plano.to_owned()).await;
        return Err(invalidas());
    };
    // Una ventana de bloqueo vencida empieza de nuevo en 1.
    let reservado: Option<String> = sqlx::query_scalar(
        "UPDATE kyc.credenciales SET \
           intentos_fallidos = CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END, \
           bloqueado_hasta = CASE WHEN (CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END) >= $2 \
                                  THEN now() + make_interval(mins => $3) ELSE NULL END \
         WHERE cliente_id = $1 AND (bloqueado_hasta IS NULL OR bloqueado_hasta <= now()) \
         RETURNING password_hash",
    )
    .bind(cliente_id)
    .bind(MAX_INTENTOS)
    .bind(MINUTOS_BLOQUEO as i32)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some(password_hash) = reservado else { return Err(bloqueada()) };

    if !password::verificar(password_plano.to_owned(), password_hash).await? {
        // El intento ya quedó contado en la reserva.
        return Err(invalidas());
    }

    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let fila: FilaLogin = sqlx::query_as(
        "SELECT c.id AS cliente_id, c.en_lista_bloqueadas, cr.rol, cr.mfa_enabled, \
                coalesce(cr.bloqueado_hasta > now(), false) AS bloqueada \
         FROM kyc.clientes c JOIN kyc.credenciales cr ON cr.cliente_id = c.id WHERE c.id = $1",
    )
    .bind(cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    if fila.en_lista_bloqueadas {
        tx.commit().await?;
        return Err(AppError::Prohibido);
    }
    if fila.mfa_enabled {
        tx.commit().await?;
        // Sin el código, el intento no termina: si fue el último del cupo, la
        // cuenta queda bloqueada y no se emite un desafío que ya no podría completarse.
        if fila.bloqueada {
            return Err(bloqueada());
        }
        let (token_mfa, expira_en) = estado.jwt.emitir_desafio(fila.cliente_id, fila.rol, jwt::Desafio::Mfa)?;
        return Ok(ResultadoLogin::MfaRequerido { token_mfa, expira_en });
    }
    let inscribir = mfa::exige_mfa(fila.rol);
    sqlx::query(
        "UPDATE kyc.credenciales SET intentos_fallidos = 0, bloqueado_hasta = NULL, \
           ultimo_acceso = CASE WHEN $2 THEN ultimo_acceso ELSE now() END \
         WHERE cliente_id = $1",
    )
    .bind(fila.cliente_id)
    .bind(inscribir)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if inscribir {
        let (token_inscripcion, expira_en) =
            estado.jwt.emitir_desafio(fila.cliente_id, fila.rol, jwt::Desafio::Inscripcion)?;
        return Ok(ResultadoLogin::InscripcionRequerida { token_inscripcion, expira_en });
    }
    let (token, expira_en) = estado.jwt.emitir(fila.cliente_id, fila.rol, false)?;
    Ok(ResultadoLogin::Sesion(Sesion { token, expira_en, cliente_id: fila.cliente_id, rol: fila.rol }))
}
