//! Centro de seguridad del cliente: cambio de contraseña, cierre por
//! inactividad y bitácora de seguridad.
//!
//! - **Cambio de contraseña** (`POST /auth/password/change`): exige la
//!   contraseña actual y, con el segundo factor activo, el código TOTP. Cada
//!   intento cuenta en el mismo cupo que el inicio de sesión (5 fallos
//!   bloquean la cuenta 15 minutos) y se cuenta **antes** de verificar, así
//!   una ráfaga en paralelo no prueba más contraseñas. La nueva cumple la
//!   política y es distinta de la actual. Al cambiarla se cierran todas las
//!   demás sesiones y se anula un enlace de restablecimiento pendiente. La
//!   sesión que agota el cupo se cierra: quien adivina la contraseña desde una
//!   sesión robada no la conserva.
//! - **Cierre por inactividad** (`/auth/security/preferences`): 5, 15, 30 o 60
//!   minutos. Al cambiarlo, las sesiones ya inactivas según el anterior se
//!   revocan (`INACTIVIDAD`): alargar el plazo no revive una sesión abandonada.
//! - **Bitácora** (`GET /auth/security/events`): los eventos de
//!   `kyc.eventos_autenticacion` del propio cliente, leídos con su rol y RLS.
//!
//! Todo cambio queda en la bitácora inmutable, con la sesión que lo hizo.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use super::{
    mfa, password, registrar_fallo,
    sesiones::{self, Evento, Origen, OPCIONES_INACTIVIDAD},
    Usuario, MAX_INTENTOS, MINUTOS_BLOQUEO, SQL_RESERVAR_INTENTO,
};
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    state::AppState,
};

/// Eventos que devuelve la bitácora por omisión, y como mucho.
pub const EVENTOS_POR_OMISION: i64 = 50;
pub const EVENTOS_MAXIMO: i64 = 100;

// -----------------------------------------------------------------------------
// Cambio de contraseña
// -----------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct FilaCambio {
    password_hash: String,
    /// Este intento agotó el cupo y bloqueó la cuenta.
    agota: bool,
    mfa_enabled: bool,
    totp_secret: Option<String>,
    totp_ultimo_paso: Option<i64>,
}

/// Cambia la contraseña de quien pregunta. Devuelve cuántas de sus otras
/// sesiones seguían activas y se cerraron.
pub async fn cambiar_password(
    estado: &AppState,
    u: &Usuario,
    actual: &str,
    nueva: &str,
    codigo: Option<&str>,
    origen: &Origen,
) -> AppResult<u64> {
    // Lo que no depende de la base de datos no gasta un intento.
    password::validar_politica(nueva)?;
    if nueva == actual {
        return Err(AppError::negocio("CONTRASENA_REPETIDA", "la contraseña nueva debe ser distinta de la actual"));
    }
    let codigo = match codigo.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => Some(mfa::normalizar_codigo(c)?),
        None => None,
    };
    if u.mfa && codigo.is_none() {
        return Err(codigo_requerido());
    }

    // 1. El intento cuenta antes de verificar (como el inicio de sesión).
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let reservado: Option<(String, bool)> = sqlx::query_as(SQL_RESERVAR_INTENTO)
        .bind(u.cliente_id)
        .bind(MAX_INTENTOS)
        .bind(MINUTOS_BLOQUEO as i32)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((password_hash, agota)) = reservado else {
        return Err(cuenta_bloqueada());
    };
    let fila: FilaCambio = sqlx::query_as(
        "SELECT password_hash, $2 AS agota, mfa_enabled, totp_secret, totp_ultimo_paso \
         FROM kyc.credenciales WHERE cliente_id = $1",
    )
    .bind(u.cliente_id)
    .bind(agota)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    debug_assert_eq!(fila.password_hash, password_hash);

    // 2. La contraseña actual (Argon2 sin retener una conexión).
    if !password::verificar(actual.to_owned(), fila.password_hash.clone()).await? {
        rechazar(estado, u, origen, fila.agota).await?;
        return Err(AppError::negocio("CONTRASENA_ACTUAL_INCORRECTA", "la contraseña actual no es correcta"));
    }

    // 3. El código TOTP, si el segundo factor está activo (anti-repetición incluida).
    let paso = if fila.mfa_enabled {
        let Some(codigo) = codigo.as_deref() else {
            rechazar(estado, u, origen, fila.agota).await?;
            return Err(codigo_requerido());
        };
        let sobre = fila
            .totp_secret
            .as_deref()
            .ok_or_else(|| AppError::Integridad(format!("{}: segundo factor activo sin secreto", u.cliente_id)))?;
        let secreto = mfa::descifrar(estado, sobre, u.cliente_id)?;
        match mfa::verificar_codigo(&secreto, codigo, mfa::ahora_seg(), fila.totp_ultimo_paso) {
            Some(paso) => Some(paso),
            None => {
                rechazar(estado, u, origen, fila.agota).await?;
                return Err(AppError::negocio(
                    "CODIGO_MFA_INVALIDO",
                    "el código de verificación no es válido; usa el código actual de tu app autenticadora",
                ));
            }
        }
    } else {
        None
    };

    // 4. Argon2 de la nueva, también sin conexión.
    let nuevo_hash = password::hashear(nueva.to_owned()).await?;

    // 5. Todo o nada: la contraseña (si nadie la cambió mientras tanto), el
    //    paso TOTP usado, el cupo de intentos, el enlace de restablecimiento,
    //    las demás sesiones y la bitácora.
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let n = sqlx::query(
        "UPDATE kyc.credenciales SET password_hash = $3, intentos_fallidos = 0, bloqueado_hasta = NULL, \
           password_reset_token = NULL, token_expires_at = NULL, \
           totp_ultimo_paso = coalesce($4, totp_ultimo_paso) \
         WHERE cliente_id = $1 AND password_hash = $2 \
           AND ($4::bigint IS NULL OR totp_ultimo_paso IS NULL OR totp_ultimo_paso < $4)",
    )
    .bind(u.cliente_id)
    .bind(&fila.password_hash)
    .bind(&nuevo_hash)
    .bind(paso)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(AppError::conflicto(
            "CONTRASENA_CAMBIADA_EN_PARALELO",
            "la contraseña o el código se usaron en otra operación al mismo tiempo; vuelve a intentarlo",
        ));
    }
    let revocadas = sesiones::revocar_vigentes(&mut tx, u.cliente_id, Some(u.sesion_id), "CONTRASENA_CAMBIADA").await?;
    let evento = Evento::de(u.cliente_id, "CONTRASENA_CAMBIADA", origen).sesion(u.sesion_id).detalle(revocadas);
    sesiones::registrar(&mut tx, &evento).await?;
    tx.commit().await?;
    tracing::info!(target: "auth", cliente_id = %u.cliente_id, revocadas, "contraseña cambiada; las demás sesiones se cerraron");
    Ok(revocadas)
}

/// Un intento rechazado (ya contado): a la bitácora. Si agotó el cupo, la
/// cuenta queda bloqueada y la sesión que lo intentó se cierra.
async fn rechazar(estado: &AppState, u: &Usuario, origen: &Origen, agota: bool) -> AppResult<()> {
    registrar_fallo(&estado.pool, u.cliente_id, "CAMBIO_CONTRASENA_RECHAZADO", agota, origen, Some(u.sesion_id))
        .await?;
    if agota {
        let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
        sqlx::query(
            "UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'BLOQUEO' \
             WHERE id = $1 AND cliente_id = $2 AND revocada_en IS NULL",
        )
        .bind(u.sesion_id)
        .bind(u.cliente_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::warn!(target: "auth", cliente_id = %u.cliente_id, sid = %u.sesion_id,
            "la sesión agotó los intentos de cambio de contraseña: cerrada");
    }
    Ok(())
}

fn codigo_requerido() -> AppError {
    AppError::negocio("CODIGO_MFA_REQUERIDO", "con el segundo factor activo, el cambio pide el código de tu app")
}

fn cuenta_bloqueada() -> AppError {
    AppError::Bloqueado("demasiados intentos fallidos; la cuenta está bloqueada temporalmente".into())
}

// -----------------------------------------------------------------------------
// Cierre por inactividad
// -----------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Preferencias {
    /// Minutos sin peticiones tras los que una sesión se cierra.
    pub inactividad_minutos: i16,
    pub opciones_inactividad: [i16; 4],
    /// El segundo factor (TOTP) está activo.
    pub mfa_activo: bool,
}

/// Las preferencias de quien pregunta. `mfa_activo` sale de la sesión: con
/// el factor activo, solo una sesión que lo verificó es válida.
pub async fn preferencias(pool: &PgPool, u: &Usuario) -> AppResult<Preferencias> {
    let mut tx = db::begin_lectura_cliente(pool, u.cliente_id).await?;
    let minutos = sesiones::inactividad(&mut tx, u.cliente_id).await?;
    tx.commit().await?;
    Ok(Preferencias { inactividad_minutos: minutos, opciones_inactividad: OPCIONES_INACTIVIDAD, mfa_activo: u.mfa })
}

/// Guarda el cierre por inactividad. Las sesiones ya inactivas según el plazo
/// anterior se revocan antes, para que un plazo más largo no las reviva.
pub async fn guardar_preferencias(
    pool: &PgPool,
    u: &Usuario,
    inactividad_minutos: i16,
    origen: &Origen,
) -> AppResult<Preferencias> {
    if !OPCIONES_INACTIVIDAD.contains(&inactividad_minutos) {
        return Err(AppError::validacion("inactividad_minutos: 5, 15, 30 o 60"));
    }
    let mut tx = db::begin(pool, Rol::Auth).await?;
    let anterior = sesiones::inactividad(&mut tx, u.cliente_id).await?;
    sqlx::query(
        "UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'INACTIVIDAD' \
         WHERE cliente_id = $1 AND revocada_en IS NULL AND expira_en > now() \
           AND ultimo_uso_en <= now() - make_interval(mins => $2)",
    )
    .bind(u.cliente_id)
    .bind(i32::from(anterior))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO kyc.preferencias_seguridad (cliente_id, inactividad_minutos) VALUES ($1, $2) \
         ON CONFLICT (cliente_id) DO UPDATE SET inactividad_minutos = EXCLUDED.inactividad_minutos",
    )
    .bind(u.cliente_id)
    .bind(inactividad_minutos)
    .execute(&mut *tx)
    .await?;
    if anterior != inactividad_minutos {
        let evento = Evento::de(u.cliente_id, "PREFERENCIAS_ACTUALIZADAS", origen)
            .sesion(u.sesion_id)
            .detalle(u64::from(inactividad_minutos.unsigned_abs()));
        sesiones::registrar(&mut tx, &evento).await?;
    }
    tx.commit().await?;
    Ok(Preferencias { inactividad_minutos, opciones_inactividad: OPCIONES_INACTIVIDAD, mfa_activo: u.mfa })
}

// -----------------------------------------------------------------------------
// Bitácora
// -----------------------------------------------------------------------------

/// Un evento de seguridad, como lo ve su dueño (sin `registrado_por`).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EventoVista {
    pub id: i64,
    /// `INICIO_SESION`, `CONTRASENA_CAMBIADA`... (catálogo de 0010 y 0019).
    pub evento: String,
    pub ocurrido_en: DateTime<Utc>,
    pub ip: Option<String>,
    pub dispositivo: Option<String>,
    /// Cuántas sesiones se cerraron, o los minutos de inactividad elegidos.
    pub detalle: Option<String>,
    /// La sesión que actuó es la de esta petición.
    pub sesion_actual: bool,
}

/// Los eventos más recientes de quien pregunta (lectura con su rol y RLS).
pub async fn eventos(pool: &PgPool, u: &Usuario, limite: Option<i64>) -> AppResult<Vec<EventoVista>> {
    let limite = limite.unwrap_or(EVENTOS_POR_OMISION);
    if !(1..=EVENTOS_MAXIMO).contains(&limite) {
        return Err(AppError::validacion(format!("limite: de 1 a {EVENTOS_MAXIMO}")));
    }
    let mut tx = db::begin_lectura_cliente(pool, u.cliente_id).await?;
    let filas: Vec<EventoVista> = sqlx::query_as(
        "SELECT id, evento, ocurrido_en, host(ip) AS ip, dispositivo, detalle, \
                coalesce(sesion_id = $2, false) AS sesion_actual \
         FROM kyc.eventos_autenticacion WHERE cliente_id = $1 \
         ORDER BY ocurrido_en DESC, id DESC LIMIT $3",
    )
    .bind(u.cliente_id)
    .bind(u.sesion_id)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(filas)
}
