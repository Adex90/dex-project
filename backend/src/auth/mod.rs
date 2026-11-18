//! Autenticación: contraseñas Argon2id, JWT HS256, segundo factor TOTP,
//! restablecimiento de contraseña, sesiones por dispositivo, centro de
//! seguridad y extractor de usuario.

pub mod correo;
pub mod jwt;
pub mod mfa;
pub mod password;
pub mod restablecimiento;
pub mod seguridad;
pub mod sesiones;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use self::sesiones::{Evento, Origen};
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
    /// La sesión del token (`kyc.sesiones`, migración 0019).
    pub sesion_id: Uuid,
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
        validar_contra_bd(&estado.pool, &claims, Uso::Peticion).await
    }
}

/// Las mismas comprobaciones del extractor [`Usuario`] (firma, audiencia y
/// vigencia del JWT; después, en la base de datos, rol actual, sesión vigente,
/// revocación, bloqueo y segundo factor) para un token que no llega en
/// `Authorization`: un navegador no puede poner cabeceras en un WebSocket.
/// Abrir la conexión cuenta como actividad de la sesión. Devuelve también los
/// claims, cuyo `exp` acota la vida de la conexión.
pub async fn autenticar_token(estado: &AppState, token: &str) -> AppResult<(Usuario, jwt::Claims)> {
    let claims = estado.jwt.validar(token)?;
    let usuario = validar_contra_bd(&estado.pool, &claims, Uso::Peticion).await?;
    Ok((usuario, claims))
}

/// Vuelve a confirmar contra la base de datos una sesión ya abierta: una
/// conexión larga no debe sobrevivir a una revocación, un cambio de rol, un
/// bloqueo ni al cierre por inactividad. No cuenta como actividad: una
/// pestaña abierta sin peticiones también se cierra.
pub async fn revalidar(pool: &PgPool, claims: &jwt::Claims) -> AppResult<Usuario> {
    validar_contra_bd(pool, claims, Uso::Revalidacion).await
}

/// Por qué se valida un token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Uso {
    /// Una petición de la persona: marca el último uso de la sesión.
    Peticion,
    /// La revalidación periódica de una conexión abierta: no lo marca.
    Revalidacion,
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
    /// La sesión del token es de esta persona y sigue vigente: no revocada,
    /// sin vencer y usada dentro de su cierre por inactividad.
    sesion_vigente: bool,
    /// Su último uso tiene más de [`sesiones::SEGUNDOS_ENTRE_USOS`].
    tocar: bool,
}

/// Lo que valida cada petición, en una sola lectura con sc_kyc. Sin `sid`
/// (los desafíos), `sesion_vigente` es `false`.
async fn leer_sesion(pool: &PgPool, claims: &jwt::Claims) -> AppResult<Option<FilaSesion>> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let fila: Option<FilaSesion> = sqlx::query_as(
        "SELECT cr.rol, c.en_lista_bloqueadas, cr.mfa_enabled, \
                coalesce(floor(extract(epoch FROM cr.tokens_validos_desde)) > $2, false) AS revocado, \
                coalesce(s.revocada_en IS NULL AND s.expira_en > now() \
                         AND s.ultimo_uso_en > now() - make_interval(mins => coalesce(p.inactividad_minutos, $4)), \
                         false) AS sesion_vigente, \
                coalesce(s.ultimo_uso_en < now() - make_interval(secs => $5), false) AS tocar \
         FROM kyc.credenciales cr JOIN kyc.clientes c ON c.id = cr.cliente_id \
         LEFT JOIN kyc.sesiones s ON s.id = $3 AND s.cliente_id = cr.cliente_id \
         LEFT JOIN kyc.preferencias_seguridad p ON p.cliente_id = cr.cliente_id \
         WHERE cr.cliente_id = $1",
    )
    .bind(claims.sub)
    .bind(claims.iat)
    .bind(claims.sid)
    .bind(sesiones::INACTIVIDAD_PREDETERMINADA)
    .bind(f64::from(sesiones::SEGUNDOS_ENTRE_USOS))
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fila)
}

/// La firma del JWT no basta: en cada petición se confirma contra la base de
/// datos que la persona existe, que su rol **actual** coincide con el del
/// token, que la sesión del token sigue vigente (no revocada, sin vencer y
/// activa: ver [`sesiones`]), que sus sesiones no fueron revocadas todas y que
/// no está bloqueada. Así, degradar a un operador, cerrar una sesión o
/// revocarlas todas surte efecto de inmediato. Un token sin sesión (`sid`,
/// emitido antes de 0019) no vale.
///
/// Segundo factor: si la persona lo tiene activo, la sesión debe haberlo
/// verificado; el personal interno, además, no opera sin él (`MFA_REQUERIDO`).
async fn validar_contra_bd(pool: &PgPool, claims: &jwt::Claims, uso: Uso) -> AppResult<Usuario> {
    let sesion_id = claims.sid.ok_or(AppError::NoAutenticado)?;
    let fila = leer_sesion(pool, claims).await?.ok_or(AppError::NoAutenticado)?;
    if fila.revocado || fila.rol != claims.rol || !fila.sesion_vigente {
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
    if uso == Uso::Peticion && fila.tocar {
        sesiones::tocar(pool, sesion_id).await;
    }
    Ok(Usuario { cliente_id: claims.sub, rol: fila.rol, mfa, sesion_id })
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
        let u = validar_contra_bd(&estado.pool, &estado.jwt.validar(token)?, Uso::Peticion).await?;
        Ok(Self { cliente_id: u.cliente_id, rol: u.rol })
    }
}

/// Invalida todos los JWT emitidos hasta ahora para una persona y cierra sus
/// sesiones (un operador: `POST /operaciones/usuarios/{id}/revocar-sesiones`).
/// Queda en su bitácora, sin la IP del operador.
pub async fn revocar_sesiones(pool: &PgPool, cliente_id: Uuid) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Auth).await?;
    let n = sqlx::query("UPDATE kyc.credenciales SET tokens_validos_desde = now() WHERE cliente_id = $1")
        .bind(cliente_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::no_encontrado("persona no encontrada"));
    }
    let revocadas = sesiones::revocar_vigentes(&mut tx, cliente_id, None, "OPERADOR").await?;
    sesiones::registrar(&mut tx, &Evento::interno(cliente_id, "SESIONES_REVOCADAS").detalle(revocadas)).await?;
    tx.commit().await?;
    tracing::info!(target: "auth", %cliente_id, revocadas, "sesiones revocadas por un operador");
    Ok(())
}

// -----------------------------------------------------------------------------
// Inicio de sesión
// -----------------------------------------------------------------------------

const MAX_INTENTOS: i32 = 5;
const MINUTOS_BLOQUEO: i64 = 15;

/// Cuenta un intento fallido (contraseña o código TOTP) y, al llegar a
/// `MAX_INTENTOS` ($2), bloquea la cuenta `MINUTOS_BLOQUEO` ($3) minutos. Una
/// ventana de bloqueo vencida empieza de nuevo en 1. Devuelve si este fallo
/// bloqueó la cuenta (quien lo llama comprobó antes que no lo estaba).
pub(crate) const SQL_REGISTRAR_FALLO: &str = "UPDATE kyc.credenciales SET \
       intentos_fallidos = CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END, \
       bloqueado_hasta = CASE WHEN (CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END) >= $2 \
                              THEN now() + make_interval(mins => $3) ELSE NULL END \
     WHERE cliente_id = $1 RETURNING bloqueado_hasta IS NOT NULL";

/// Cuenta el intento como fallido **antes** de verificar (ver
/// [`iniciar_sesion`]); sin fila si la cuenta está bloqueada. Devuelve el hash
/// y si este intento agotó el cupo.
pub(crate) const SQL_RESERVAR_INTENTO: &str = "UPDATE kyc.credenciales SET \
       intentos_fallidos = CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END, \
       bloqueado_hasta = CASE WHEN (CASE WHEN bloqueado_hasta IS NULL THEN intentos_fallidos + 1 ELSE 1 END) >= $2 \
                              THEN now() + make_interval(mins => $3) ELSE NULL END \
     WHERE cliente_id = $1 AND (bloqueado_hasta IS NULL OR bloqueado_hasta <= now()) \
     RETURNING password_hash, bloqueado_hasta IS NOT NULL AS agota";

/// Una sesión de acceso recién abierta.
#[derive(Debug, Clone, Serialize)]
pub struct Sesion {
    pub token: String,
    pub expira_en: DateTime<Utc>,
    pub cliente_id: Uuid,
    pub rol: RolUsuario,
    /// La fila de `kyc.sesiones` (el `sid` del token).
    pub sesion_id: Uuid,
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
///
/// Cada contraseña incorrecta queda en la bitácora del cliente
/// (`INICIO_FALLIDO`, con la IP y el dispositivo), y también el intento que
/// bloquea la cuenta (`CUENTA_BLOQUEADA`); los intentos durante el bloqueo no
/// se verifican ni se registran (la bitácora no crece sin límite). El correcto
/// abre una sesión ([`sesiones::abrir`]).
pub async fn iniciar_sesion(
    estado: &AppState,
    email: &str,
    password_plano: &str,
    origen: &Origen,
) -> AppResult<ResultadoLogin> {
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
    let reservado: Option<(String, bool)> = sqlx::query_as(SQL_RESERVAR_INTENTO)
        .bind(cliente_id)
        .bind(MAX_INTENTOS)
        .bind(MINUTOS_BLOQUEO as i32)
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    let Some((password_hash, agota)) = reservado else { return Err(bloqueada()) };

    if !password::verificar(password_plano.to_owned(), password_hash).await? {
        // El intento ya quedó contado en la reserva; falta la bitácora.
        registrar_fallo(pool, cliente_id, "INICIO_FALLIDO", agota, origen, None).await?;
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
    let sesion = sesiones::abrir(estado, fila.cliente_id, fila.rol, false, origen).await?;
    Ok(ResultadoLogin::Sesion(sesion))
}

/// Registra un intento fallido ya contado (`evento`) y, si agotó el cupo,
/// también el bloqueo de la cuenta. `sesion_id`: la sesión desde la que se
/// intentó (un cambio de contraseña), si la hay.
pub(crate) async fn registrar_fallo(
    pool: &PgPool,
    cliente_id: Uuid,
    evento: &'static str,
    agota: bool,
    origen: &Origen,
    sesion_id: Option<Uuid>,
) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Auth).await?;
    let eventos: &[&'static str] = if agota { &[evento, "CUENTA_BLOQUEADA"] } else { &[evento] };
    for &nombre in eventos {
        let mut e = Evento::de(cliente_id, nombre, origen);
        if let Some(sid) = sesion_id {
            e = e.sesion(sid);
        }
        sesiones::registrar(&mut tx, &e).await?;
    }
    tx.commit().await?;
    if agota {
        tracing::warn!(target: "auth", %cliente_id, "cuenta bloqueada por intentos fallidos");
    }
    Ok(())
}
