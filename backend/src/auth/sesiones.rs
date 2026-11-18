//! Sesiones por dispositivo (migración 0019).
//!
//! Cada inicio de sesión (contraseña, segundo factor, alta de la cuenta o
//! activación del segundo factor) abre una fila en `kyc.sesiones`, y el token
//! de acceso lleva su id (`sid`). En cada petición, además de lo que ya revisa
//! [`super::validar_contra_bd`] (rol, revocación total, bloqueo, segundo
//! factor), la sesión del token debe seguir vigente:
//!
//! - **no revocada**: el cliente la cerró (`/auth/logout`), la cerró desde otro
//!   dispositivo, cerró todas las demás, cambió o restableció la contraseña,
//!   activó el segundo factor, o un operador revocó sus sesiones;
//! - **sin vencer** (`expira_en`, el `exp` del token);
//! - **activa**: usada dentro del cierre por inactividad que eligió el cliente
//!   (5, 15, 30 o 60 minutos; [`INACTIVIDAD_PREDETERMINADA`] si no eligió).
//!   Las peticiones HTTP marcan el último uso, como mucho una vez cada
//!   [`SEGUNDOS_ENTRE_USOS`]; la revalidación periódica del socket de la bóveda
//!   no, así que una pestaña abierta sin peticiones también se cierra.
//!
//! Un token sin `sid` (emitido antes de 0019) ya no vale: desplegar esta
//! versión cierra todas las sesiones una vez.
//!
//! De cada sesión se guarda un resumen del dispositivo derivado del User-Agent
//! ("Chrome on macOS", en inglés como la interfaz; nunca el User-Agent
//! completo) y la IP del inicio. Todo queda en la bitácora inmutable
//! `kyc.eventos_autenticacion`, con la sesión que actuó.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{Sesion, Usuario};
use crate::{
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    models::kyc::RolUsuario,
    state::AppState,
};

/// Cierre por inactividad cuando el cliente no eligió otro.
pub const INACTIVIDAD_PREDETERMINADA: i16 = 30;
/// Los plazos que admite `kyc.preferencias_seguridad` (CHECK de 0019).
pub const OPCIONES_INACTIVIDAD: [i16; 4] = [5, 15, 30, 60];
/// Una petición marca el último uso de su sesión si el anterior tiene más de esto.
pub const SEGUNDOS_ENTRE_USOS: i32 = 60;

/// Lo que se guarda cuando no hay User-Agent (o no se reconoce nada).
const DESCONOCIDO: &str = "Unknown device";
/// El CHECK de `kyc.sesiones.dispositivo`.
const LARGO_MAXIMO: usize = 80;

// -----------------------------------------------------------------------------
// De dónde llega una petición
// -----------------------------------------------------------------------------

/// La IP del cliente (la misma regla que el límite por IP) y el dispositivo
/// resumido de su User-Agent. Extractor en `routes::extraccion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origen {
    pub ip: Option<IpAddr>,
    pub dispositivo: String,
}

impl Origen {
    pub fn nuevo(ip: Option<IpAddr>, user_agent: Option<&str>) -> Self {
        Self { ip, dispositivo: dispositivo_de(user_agent) }
    }

    /// Sin cabeceras: tareas internas y pruebas.
    pub fn desconocido() -> Self {
        Self::nuevo(None, None)
    }
}

/// "Chrome on macOS": navegador y sistema de un User-Agent. Lo que no se
/// reconoce queda como el nombre del programa ("curl") o "Unknown device".
/// Siempre ASCII imprimible, entre 1 y 80 caracteres.
pub fn dispositivo_de(user_agent: Option<&str>) -> String {
    let ua = user_agent.map(str::trim).unwrap_or_default();
    if ua.is_empty() {
        return DESCONOCIDO.into();
    }
    let texto = match (navegador_de(ua), sistema_de(ua)) {
        (Some(n), Some(s)) => format!("{n} on {s}"),
        (Some(n), None) => n.to_owned(),
        (None, Some(s)) => format!("Unknown browser on {s}"),
        (None, None) => programa_de(ua).unwrap_or_else(|| DESCONOCIDO.into()),
    };
    let limpio: String = texto.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(LARGO_MAXIMO).collect();
    if limpio.trim().is_empty() {
        DESCONOCIDO.into()
    } else {
        limpio.trim().to_owned()
    }
}

/// El orden importa: Edge, Opera y Samsung Internet también se anuncian como
/// Chrome y Safari; Chromium como Chrome; casi todos como Safari.
fn navegador_de(ua: &str) -> Option<&'static str> {
    const REGLAS: [(&str, &str); 12] = [
        ("Edg/", "Edge"),
        ("EdgA/", "Edge"),
        ("EdgiOS/", "Edge"),
        ("OPR/", "Opera"),
        ("OPiOS/", "Opera"),
        ("SamsungBrowser/", "Samsung Internet"),
        ("Firefox/", "Firefox"),
        ("FxiOS/", "Firefox"),
        ("HeadlessChrome/", "Headless Chrome"),
        ("Chromium/", "Chromium"),
        ("CriOS/", "Chrome"),
        ("Chrome/", "Chrome"),
    ];
    if let Some((_, nombre)) = REGLAS.iter().find(|(marca, _)| ua.contains(marca)) {
        return Some(nombre);
    }
    (ua.contains("Safari/") && ua.contains("Version/")).then_some("Safari")
}

/// iPhone y iPad antes que macOS ("like Mac OS X"); Android y ChromeOS antes
/// que Linux.
fn sistema_de(ua: &str) -> Option<&'static str> {
    const REGLAS: [(&str, &str); 9] = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("iPod", "iPod"),
        ("Android", "Android"),
        ("CrOS", "ChromeOS"),
        ("Windows", "Windows"),
        ("Macintosh", "macOS"),
        ("Mac OS X", "macOS"),
        ("Linux", "Linux"),
    ];
    REGLAS.iter().find(|(marca, _)| ua.contains(marca)).map(|(_, nombre)| *nombre)
}

/// El primer producto de un User-Agent que no es de navegador ("curl/8.4" →
/// "curl"), con solo letras, dígitos, `.`, `_` y `-`. "Mozilla" no dice nada.
fn programa_de(ua: &str) -> Option<String> {
    let producto = ua.split(['/', ' ', '(']).next().unwrap_or_default();
    let limpio: String =
        producto.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(40).collect();
    (!limpio.is_empty() && !limpio.eq_ignore_ascii_case("mozilla")).then_some(limpio)
}

// -----------------------------------------------------------------------------
// Bitácora
// -----------------------------------------------------------------------------

/// Un renglón de `kyc.eventos_autenticacion` (inmutable; lo escribe sc_auth).
///
/// `sesion_id` es la sesión que actuó (la que se abre, en un inicio de
/// sesión); `dispositivo`, el del evento: el de quien actuó o, al revocar una
/// sesión desde otra, el de la sesión revocada. `detalle`, un número: cuántas
/// sesiones se cerraron o los minutos de inactividad elegidos.
#[derive(Debug, Clone)]
pub(crate) struct Evento<'a> {
    cliente_id: Uuid,
    evento: &'static str,
    ip: Option<IpAddr>,
    sesion_id: Option<Uuid>,
    dispositivo: Option<&'a str>,
    detalle: Option<u64>,
}

impl<'a> Evento<'a> {
    /// Con la IP y el dispositivo de la petición.
    pub(crate) fn de(cliente_id: Uuid, evento: &'static str, origen: &'a Origen) -> Self {
        Self {
            cliente_id,
            evento,
            ip: origen.ip,
            sesion_id: None,
            dispositivo: Some(&origen.dispositivo),
            detalle: None,
        }
    }

    /// Sin petición del cliente detrás (un operador, el dueño del esquema).
    pub(crate) fn interno(cliente_id: Uuid, evento: &'static str) -> Self {
        Self { cliente_id, evento, ip: None, sesion_id: None, dispositivo: None, detalle: None }
    }

    pub(crate) fn sesion(mut self, id: Uuid) -> Self {
        self.sesion_id = Some(id);
        self
    }

    pub(crate) fn dispositivo(mut self, dispositivo: &'a str) -> Self {
        self.dispositivo = Some(dispositivo);
        self
    }

    /// Seis cifras como mucho (CHECK de 0019).
    pub(crate) fn detalle(mut self, n: u64) -> Self {
        self.detalle = Some(n.min(999_999));
        self
    }
}

/// Registra un evento en la transacción `tx` (rol sc_auth).
pub(crate) async fn registrar(tx: &mut Tx, e: &Evento<'_>) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle) \
         VALUES ($1, $2, $3::inet, $4, $5, $6)",
    )
    .bind(e.cliente_id)
    .bind(e.evento)
    .bind(e.ip.map(|i| i.to_string()))
    .bind(e.sesion_id)
    .bind(e.dispositivo)
    .bind(e.detalle.map(|n| n.to_string()))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Abrir, usar y revocar
// -----------------------------------------------------------------------------

/// Abre una sesión y emite su token de acceso, en su propia transacción.
pub async fn abrir(
    estado: &AppState,
    cliente_id: Uuid,
    rol: RolUsuario,
    mfa: bool,
    origen: &Origen,
) -> AppResult<Sesion> {
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let sesion = abrir_en(&mut tx, estado, cliente_id, rol, mfa, origen, None).await?;
    tx.commit().await?;
    Ok(sesion)
}

/// Como [`abrir`], dentro de una transacción de sc_auth. `iat`: tras revocar
/// todos los tokens con el reloj de la base de datos, el segundo desde el que
/// vale el nuevo (ver [`super::jwt::ClavesJwt::emitir_desde`]).
pub(crate) async fn abrir_en(
    tx: &mut Tx,
    estado: &AppState,
    cliente_id: Uuid,
    rol: RolUsuario,
    mfa: bool,
    origen: &Origen,
    iat: Option<i64>,
) -> AppResult<Sesion> {
    let sid = Uuid::new_v4();
    let (token, expira_en) = match iat {
        Some(iat) => estado.jwt.emitir_desde(cliente_id, rol, mfa, sid, iat)?,
        None => estado.jwt.emitir(cliente_id, rol, mfa, sid)?,
    };
    sqlx::query(
        "INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, ip) \
         VALUES ($1, $2, $3, $4, $5, $6::inet)",
    )
    .bind(sid)
    .bind(cliente_id)
    .bind(expira_en)
    .bind(mfa)
    .bind(&origen.dispositivo)
    .bind(origen.ip.map(|i| i.to_string()))
    .execute(&mut **tx)
    .await?;
    registrar(tx, &Evento::de(cliente_id, "INICIO_SESION", origen).sesion(sid)).await?;
    tracing::info!(target: "auth", %cliente_id, %sid, ?rol, mfa, "sesión abierta");
    Ok(Sesion { token, expira_en, cliente_id, rol, sesion_id: sid })
}

/// Marca el último uso de la sesión (lo llama la validación de una petición,
/// como mucho una vez cada [`SEGUNDOS_ENTRE_USOS`]). Si falla, la petición
/// sigue: lo peor es que la sesión se cierre antes por inactividad.
pub(crate) async fn tocar(pool: &PgPool, sid: Uuid) {
    let resultado = async {
        let mut tx = db::begin(pool, Rol::Auth).await?;
        sqlx::query(
            "UPDATE kyc.sesiones SET ultimo_uso_en = greatest(ultimo_uso_en, now()) \
             WHERE id = $1 AND revocada_en IS NULL",
        )
        .bind(sid)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = resultado {
        tracing::warn!(target: "auth", %sid, error = %e, "no se pudo registrar el uso de la sesión");
    }
}

/// Minutos de inactividad que cierran las sesiones de la persona. Sirve en
/// una transacción de sc_auth, sc_kyc o sc_cliente (los tres leen la columna).
pub(crate) async fn inactividad(tx: &mut Tx, cliente_id: Uuid) -> AppResult<i16> {
    let minutos: Option<i16> =
        sqlx::query_scalar("SELECT inactividad_minutos FROM kyc.preferencias_seguridad WHERE cliente_id = $1")
            .bind(cliente_id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(minutos.unwrap_or(INACTIVIDAD_PREDETERMINADA))
}

/// Revoca, con `motivo`, las sesiones sin vencer de la persona salvo
/// `excepto`. Devuelve cuántas seguían activas (no inactivas): las que el
/// cliente veía en su lista.
pub(crate) async fn revocar_vigentes(
    tx: &mut Tx,
    cliente_id: Uuid,
    excepto: Option<Uuid>,
    motivo: &str,
) -> AppResult<u64> {
    let minutos = inactividad(tx, cliente_id).await?;
    let activas: i64 = sqlx::query_scalar(
        "WITH revocadas AS ( \
           UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = $3 \
           WHERE cliente_id = $1 AND revocada_en IS NULL AND expira_en > now() AND id IS DISTINCT FROM $2 \
           RETURNING ultimo_uso_en) \
         SELECT count(*) FILTER (WHERE ultimo_uso_en > now() - make_interval(mins => $4)) FROM revocadas",
    )
    .bind(cliente_id)
    .bind(excepto)
    .bind(motivo)
    .bind(i32::from(minutos))
    .fetch_one(&mut **tx)
    .await?;
    Ok(u64::try_from(activas).unwrap_or(0))
}

/// Una sesión activa, como la ve su dueño.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SesionVista {
    pub id: Uuid,
    /// "Chrome on macOS".
    pub dispositivo: String,
    /// IP del inicio de sesión.
    pub ip: Option<String>,
    pub creada_en: DateTime<Utc>,
    /// Exacto a [`SEGUNDOS_ENTRE_USOS`].
    pub ultimo_uso_en: DateTime<Utc>,
    pub expira_en: DateTime<Utc>,
    /// La sesión verificó el segundo factor.
    pub mfa: bool,
    /// La de esta petición.
    pub actual: bool,
}

#[derive(Debug, Serialize)]
pub struct ListaSesiones {
    /// La actual primero; después, la de uso más reciente.
    pub sesiones: Vec<SesionVista>,
    pub inactividad_minutos: i16,
}

/// Las sesiones activas de quien pregunta: no revocadas, sin vencer y usadas
/// dentro de su cierre por inactividad. Lectura con su rol y RLS (0019).
pub async fn listar(pool: &PgPool, u: &Usuario) -> AppResult<ListaSesiones> {
    let mut tx = db::begin_lectura_cliente(pool, u.cliente_id).await?;
    let minutos = inactividad(&mut tx, u.cliente_id).await?;
    let sesiones: Vec<SesionVista> = sqlx::query_as(
        "SELECT id, dispositivo, host(ip) AS ip, creada_en, ultimo_uso_en, expira_en, mfa, id = $2 AS actual \
         FROM kyc.sesiones \
         WHERE cliente_id = $1 AND revocada_en IS NULL AND expira_en > now() \
           AND ultimo_uso_en > now() - make_interval(mins => $3) \
         ORDER BY id = $2 DESC, ultimo_uso_en DESC, creada_en DESC",
    )
    .bind(u.cliente_id)
    .bind(u.sesion_id)
    .bind(i32::from(minutos))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ListaSesiones { sesiones, inactividad_minutos: minutos })
}

/// Cierra la sesión de la petición (`POST /auth/logout`).
pub async fn cerrar(pool: &PgPool, u: &Usuario, origen: &Origen) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Auth).await?;
    let dispositivo: Option<String> = sqlx::query_scalar(
        "UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'CIERRE' \
         WHERE id = $1 AND cliente_id = $2 AND revocada_en IS NULL RETURNING dispositivo",
    )
    .bind(u.sesion_id)
    .bind(u.cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(dispositivo) = dispositivo.as_deref() {
        let evento = Evento::de(u.cliente_id, "SESION_CERRADA", origen).sesion(u.sesion_id).dispositivo(dispositivo);
        registrar(&mut tx, &evento).await?;
    }
    tx.commit().await?;
    tracing::info!(target: "auth", cliente_id = %u.cliente_id, sid = %u.sesion_id, "sesión cerrada");
    Ok(())
}

/// Cierra otra sesión de quien pregunta (la propia equivale a [`cerrar`]).
/// Una sesión ajena, inexistente, vencida o ya cerrada responde lo mismo: 404.
pub async fn revocar(pool: &PgPool, u: &Usuario, sid: Uuid, origen: &Origen) -> AppResult<()> {
    if sid == u.sesion_id {
        return cerrar(pool, u, origen).await;
    }
    let mut tx = db::begin(pool, Rol::Auth).await?;
    let dispositivo: Option<String> = sqlx::query_scalar(
        "UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'REVOCADA' \
         WHERE id = $1 AND cliente_id = $2 AND revocada_en IS NULL AND expira_en > now() RETURNING dispositivo",
    )
    .bind(sid)
    .bind(u.cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(dispositivo) = dispositivo else {
        return Err(AppError::no_encontrado("la sesión no existe o ya está cerrada"));
    };
    let evento = Evento::de(u.cliente_id, "SESION_REVOCADA", origen).sesion(u.sesion_id).dispositivo(&dispositivo);
    registrar(&mut tx, &evento).await?;
    tx.commit().await?;
    tracing::info!(target: "auth", cliente_id = %u.cliente_id, %sid, "sesión revocada desde otra");
    Ok(())
}

/// "Cerrar todas las demás": revoca toda sesión de quien pregunta salvo la
/// de la petición. Devuelve cuántas seguían activas.
pub async fn revocar_otras(pool: &PgPool, u: &Usuario, origen: &Origen) -> AppResult<u64> {
    let mut tx = db::begin(pool, Rol::Auth).await?;
    let n = revocar_vigentes(&mut tx, u.cliente_id, Some(u.sesion_id), "OTRAS_REVOCADAS").await?;
    let evento = Evento::de(u.cliente_id, "SESIONES_REVOCADAS", origen).sesion(u.sesion_id).detalle(n);
    registrar(&mut tx, &evento).await?;
    tx.commit().await?;
    tracing::info!(target: "auth", cliente_id = %u.cliente_id, revocadas = n, "las demás sesiones se cerraron");
    Ok(n)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn resume_los_navegadores_comunes() {
        for (ua, esperado) in [
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/126.0.0.0 Safari/537.36",
                "Chrome on macOS",
            ),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) \
                 Version/17.5 Mobile/15E148 Safari/604.1",
                "Safari on iPhone",
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/126.0.0.0 Safari/537.36 Edg/126.0.2592.68",
                "Edge on Windows",
            ),
            ("Mozilla/5.0 (X11; Linux x86_64; rv:127.0) Gecko/20100101 Firefox/127.0", "Firefox on Linux"),
            (
                "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/126.0.6478.71 Mobile Safari/537.36",
                "Chrome on Android",
            ),
            (
                "Mozilla/5.0 (Linux; Android 14; SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) \
                 SamsungBrowser/25.0 Chrome/121.0.0.0 Mobile Safari/537.36",
                "Samsung Internet on Android",
            ),
            (
                "Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) \
                 CriOS/126.0.6478.54 Mobile/15E148 Safari/604.1",
                "Chrome on iPad",
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/126.0.0.0 Safari/537.36 OPR/111.0.0.0",
                "Opera on Windows",
            ),
            (
                "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/126.0.0.0 Safari/537.36",
                "Chrome on ChromeOS",
            ),
            (
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 HeadlessChrome/126.0.6478.17 Safari/537.36",
                "Headless Chrome on Linux",
            ),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) \
                 Version/17.5 Safari/605.1.15",
                "Safari on macOS",
            ),
        ] {
            assert_eq!(dispositivo_de(Some(ua)), esperado, "{ua}");
        }
    }

    #[test]
    fn lo_que_no_es_un_navegador() {
        assert_eq!(dispositivo_de(Some("curl/8.4.0")), "curl");
        assert_eq!(dispositivo_de(Some("python-requests/2.31.0")), "python-requests");
        assert_eq!(dispositivo_de(Some("node")), "node");
        assert_eq!(dispositivo_de(Some("Mozilla/5.0 (Linux; U)")), "Unknown browser on Linux");
        assert_eq!(dispositivo_de(Some("Mozilla/5.0 (compatible)")), "Unknown device");
        assert_eq!(dispositivo_de(Some("   ")), "Unknown device");
        assert_eq!(dispositivo_de(None), "Unknown device");
        assert_eq!(dispositivo_de(Some("<script>/1")), "script");
    }

    #[test]
    fn cabe_siempre_en_la_columna() {
        let largo = format!("{}/1.0", "x".repeat(500));
        let d = dispositivo_de(Some(&largo));
        assert!((1..=40).contains(&d.len()), "{d}");
        for ua in ["\u{1F600}/1", "Ñandú/2 (Windows)", "\0\t/3"] {
            let d = dispositivo_de(Some(ua));
            assert!(
                (1..=LARGO_MAXIMO).contains(&d.len()) && d.chars().all(|c| c.is_ascii_graphic() || c == ' '),
                "{d}"
            );
        }
    }

    #[test]
    fn el_detalle_cabe_en_seis_cifras() {
        let e = Evento::interno(Uuid::nil(), "SESIONES_REVOCADAS").detalle(5_000_000);
        assert_eq!(e.detalle, Some(999_999));
    }
}
