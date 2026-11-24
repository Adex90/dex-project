//! Verificación reforzada (step-up) antes de las acciones sensibles
//! (migración 0022).
//!
//! Sacar dinero de la cuenta (un retiro a una cuenta vinculada, una
//! transferencia a otra cuenta, un pago SPEI a terceros) y cambiar a los
//! beneficiarios piden, además de una sesión válida, haber comprobado de nuevo
//! una credencial hace poco:
//!
//! - **el código TOTP vigente**, si la persona activó el segundo factor (la
//!   contraseña no basta: un token robado junto con una contraseña filtrada no
//!   alcanza); o
//! - **su contraseña**, si aún no lo activó.
//!
//! `POST /auth/step-up` comprueba la credencial y abre en la sesión una
//! ventana de `kyc.vigencia_step_up()` (5 minutos, o hasta que la sesión
//! venza): la fecha la pone la base de datos, que además deja
//! `STEP_UP_VERIFICADO` en la bitácora con la IP y el dispositivo de esta
//! petición. Cada intento cuenta en el mismo cupo que el inicio de sesión (5
//! fallos bloquean la cuenta 15 minutos) y se cuenta **antes** de verificar,
//! así una ráfaga en paralelo no prueba más códigos. Cada fallo deja
//! `STEP_UP_FALLIDO` y la respuesta dice cuántos intentos quedan; el que agota
//! el cupo cierra la sesión (`BLOQUEO`), y quien tiene un token robado no se
//! queda adivinando. Un código TOTP sirve una sola vez, como en el inicio de
//! sesión.
//!
//! Las rutas sensibles llaman a [`Usuario::exigir_step_up`]: sin una ventana
//! abierta responden `403 STEP_UP_REQUERIDO` antes de tocar nada, y el cliente
//! repite la misma petición (con la misma clave de idempotencia) después de
//! verificar. La validación de cada petición lee la ventana junto con la
//! sesión; la designación de beneficiarios la vuelve a exigir en la base de
//! datos.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use super::{
    mfa, password, rechazar_en_sesion, sesiones::Origen, Usuario, MAX_INTENTOS, MINUTOS_BLOQUEO, SQL_RESERVAR_INTENTO,
};
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    state::AppState,
};

/// Con qué se verifica: el código TOTP con el segundo factor activo; si no, la
/// contraseña.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Metodo {
    Totp,
    Contrasena,
}

impl Metodo {
    fn de(mfa: bool) -> Self {
        if mfa {
            Self::Totp
        } else {
            Self::Contrasena
        }
    }
}

/// `GET /auth/step-up`: con qué se verifica esta sesión y si ya tiene una
/// ventana abierta.
#[derive(Debug, Serialize)]
pub struct EstadoStepUp {
    pub metodo: Metodo,
    /// Fin de la ventana abierta, si la hay.
    pub valido_hasta: Option<DateTime<Utc>>,
    /// Lo que le queda, con el reloj de la base de datos (0 sin ventana).
    pub segundos_restantes: i64,
    /// Lo que dura una ventana nueva.
    pub vigencia_segundos: i64,
}

/// `POST /auth/step-up`: la ventana recién abierta.
#[derive(Debug, Serialize)]
pub struct VentanaStepUp {
    pub metodo: Metodo,
    pub valido_hasta: DateTime<Utc>,
    /// Segundos de la ventana: 300, o menos si la sesión vence antes.
    pub segundos: i64,
}

/// El estado de la sesión de `u` (lectura con sc_kyc, como la validación).
pub async fn estado(pool: &PgPool, u: &Usuario) -> AppResult<EstadoStepUp> {
    let mut tx = db::begin_lectura(pool, Rol::Kyc).await?;
    let (valido_hasta, segundos_restantes, vigencia_segundos): (Option<DateTime<Utc>>, i64, i64) = sqlx::query_as(
        "SELECT CASE WHEN s.step_up_valido_hasta > now() THEN s.step_up_valido_hasta END, \
                greatest(coalesce(floor(extract(epoch FROM s.step_up_valido_hasta - now())), 0), 0)::bigint, \
                extract(epoch FROM kyc.vigencia_step_up())::bigint \
         FROM kyc.sesiones s WHERE s.id = $1 AND s.cliente_id = $2",
    )
    .bind(u.sesion_id)
    .bind(u.cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(EstadoStepUp { metodo: Metodo::de(u.mfa), valido_hasta, segundos_restantes, vigencia_segundos })
}

#[derive(sqlx::FromRow)]
struct FilaIntento {
    password_hash: String,
    /// Este intento agotó el cupo y bloqueó la cuenta.
    agota: bool,
    /// Fallos contados, este incluido.
    intentos_fallidos: i32,
    mfa_enabled: bool,
    totp_secret: Option<String>,
    totp_ultimo_paso: Option<i64>,
}

/// Comprueba la credencial de `u` (el código TOTP con el segundo factor
/// activo; si no, la contraseña) y abre la ventana de su sesión.
pub async fn verificar(
    estado: &AppState,
    u: &Usuario,
    codigo: Option<&str>,
    password_plano: Option<&str>,
    origen: &Origen,
) -> AppResult<VentanaStepUp> {
    // Lo que no depende de la base de datos no gasta un intento.
    let codigo = codigo.map(str::trim).filter(|c| !c.is_empty());
    let password_plano = password_plano.filter(|p| !p.is_empty());
    if codigo.is_some() && password_plano.is_some() {
        return Err(AppError::validacion("envía el código de tu app autenticadora o tu contraseña, no los dos"));
    }
    let codigo = match codigo {
        Some(c) => Some(mfa::normalizar_codigo(c)?),
        None => None,
    };
    if u.mfa && codigo.is_none() {
        return Err(codigo_requerido());
    }
    if !u.mfa && password_plano.is_none() {
        return Err(contrasena_requerida());
    }

    // 1. El intento cuenta antes de verificar (como el inicio de sesión).
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let reservado: Option<(String, bool)> = sqlx::query_as(SQL_RESERVAR_INTENTO)
        .bind(u.cliente_id)
        .bind(MAX_INTENTOS)
        .bind(MINUTOS_BLOQUEO as i32)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((_, agota)) = reservado else {
        return Err(cuenta_bloqueada());
    };
    let fila: FilaIntento = sqlx::query_as(
        "SELECT password_hash, $2 AS agota, intentos_fallidos, mfa_enabled, totp_secret, totp_ultimo_paso \
         FROM kyc.credenciales WHERE cliente_id = $1",
    )
    .bind(u.cliente_id)
    .bind(agota)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let restantes = (MAX_INTENTOS - fila.intentos_fallidos).max(0);

    // 2. La credencial del método vigente: el segundo factor manda aunque haya
    //    cambiado desde que se validó la petición.
    let metodo = Metodo::de(fila.mfa_enabled);
    let paso = match metodo {
        Metodo::Totp => {
            let Some(codigo) = codigo.as_deref() else {
                return Err(rechazar(estado, u, origen, fila.agota, codigo_requerido()).await);
            };
            let sobre = fila
                .totp_secret
                .as_deref()
                .ok_or_else(|| AppError::Integridad(format!("{}: segundo factor activo sin secreto", u.cliente_id)))?;
            let secreto = mfa::descifrar(estado, sobre, u.cliente_id)?;
            match mfa::verificar_codigo(&secreto, codigo, mfa::ahora_seg(), fila.totp_ultimo_paso) {
                Some(paso) => Some(paso),
                None => {
                    let error = AppError::CredencialRechazada {
                        codigo: "CODIGO_MFA_INVALIDO",
                        mensaje: "el código de verificación no es válido; usa el código actual de tu app autenticadora"
                            .into(),
                        intentos_restantes: restantes,
                    };
                    return Err(rechazar(estado, u, origen, fila.agota, error).await);
                }
            }
        }
        Metodo::Contrasena => {
            let Some(password_plano) = password_plano else {
                return Err(rechazar(estado, u, origen, fila.agota, contrasena_requerida()).await);
            };
            // Argon2 sin retener una conexión.
            if !password::verificar(password_plano.to_owned(), fila.password_hash.clone()).await? {
                let error = AppError::CredencialRechazada {
                    codigo: "CONTRASENA_ACTUAL_INCORRECTA",
                    mensaje: "la contraseña no es correcta".into(),
                    intentos_restantes: restantes,
                };
                return Err(rechazar(estado, u, origen, fila.agota, error).await);
            }
            None
        }
    };

    // 3. Todo o nada: el cupo vuelve a cero, el paso TOTP queda usado y la
    //    sesión abre su ventana (la fecha y el evento los pone la base de
    //    datos, con la IP y el dispositivo de esta petición).
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let n = sqlx::query(
        "UPDATE kyc.credenciales SET intentos_fallidos = 0, bloqueado_hasta = NULL, \
           totp_ultimo_paso = coalesce($2, totp_ultimo_paso) \
         WHERE cliente_id = $1 AND ($2::bigint IS NULL OR totp_ultimo_paso IS NULL OR totp_ultimo_paso < $2)",
    )
    .bind(u.cliente_id)
    .bind(paso)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n != 1 {
        // Otra petición usó el mismo código al mismo tiempo.
        return Err(AppError::conflicto(
            "CODIGO_MFA_USADO",
            "ese código ya se usó; espera el siguiente de tu app autenticadora",
        ));
    }
    sqlx::query("SELECT set_config('sc.origen_ip', $1, true), set_config('sc.origen_dispositivo', $2, true)")
        .bind(origen.ip.map(|ip| ip.to_string()).unwrap_or_default())
        .bind(&origen.dispositivo)
        .execute(&mut *tx)
        .await?;
    let ventana: Option<(DateTime<Utc>, i64)> = sqlx::query_as(
        "UPDATE kyc.sesiones SET step_up_valido_hasta = now() \
         WHERE id = $1 AND cliente_id = $2 AND revocada_en IS NULL AND expira_en > now() \
         RETURNING step_up_valido_hasta, floor(extract(epoch FROM step_up_valido_hasta - now()))::bigint",
    )
    .bind(u.sesion_id)
    .bind(u.cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    // La sesión se cerró mientras tanto: nada queda.
    let Some((valido_hasta, segundos)) = ventana else {
        return Err(AppError::NoAutenticado);
    };
    tx.commit().await?;
    tracing::info!(target: "auth", cliente_id = %u.cliente_id, sid = %u.sesion_id, ?metodo, segundos,
        "verificación reforzada: ventana abierta");
    Ok(VentanaStepUp { metodo, valido_hasta, segundos })
}

/// Un intento rechazado (ya contado): `STEP_UP_FALLIDO` en la bitácora. Si
/// agotó el cupo, la cuenta queda bloqueada, la sesión se cierra y la
/// respuesta lo dice (`SESION_BLOQUEADA`) en lugar de `error`. Una cuenta que
/// ya estaba bloqueada responde `BLOQUEADO` sin contar ni cerrar nada.
async fn rechazar(estado: &AppState, u: &Usuario, origen: &Origen, agota: bool, error: AppError) -> AppError {
    if let Err(e) = rechazar_en_sesion(&estado.pool, u, "STEP_UP_FALLIDO", agota, origen).await {
        return e;
    }
    if agota {
        AppError::SesionBloqueada { minutos: MINUTOS_BLOQUEO }
    } else {
        error
    }
}

fn codigo_requerido() -> AppError {
    AppError::negocio("CODIGO_MFA_REQUERIDO", "con el segundo factor activo, la verificación pide el código de tu app")
}

fn contrasena_requerida() -> AppError {
    AppError::negocio("CONTRASENA_REQUERIDA", "sin segundo factor, la verificación pide tu contraseña")
}

fn cuenta_bloqueada() -> AppError {
    AppError::Bloqueado("demasiados intentos fallidos; la cuenta está bloqueada temporalmente".into())
}
