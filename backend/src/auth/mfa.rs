//! Segundo factor: TOTP (RFC 6238: SHA-1, 6 dígitos, pasos de 30 s), el que
//! entienden todas las apps autenticadoras.
//!
//! - **Obligatorio** para el personal interno (operador, auditor, cumplimiento,
//!   ver [`exige_mfa`]): sin una sesión que lo haya verificado no hay token de
//!   acceso, y el extractor de [`super::Usuario`] y la consola PLD lo repiten.
//! - **Inscripción**: `configurar` genera un secreto de 160 bits, lo guarda
//!   cifrado (AES-256-GCM, AAD = cliente; la base de datos solo ve el sobre) y
//!   devuelve la URL `otpauth://` con su QR; `activar` verifica el primer código
//!   y activa el factor. Con el factor activo, la aplicación ya no puede
//!   desactivarlo ni cambiar el secreto: solo el dueño del esquema (migración
//!   0010), p. ej. cuando alguien pierde su dispositivo.
//! - **Verificación**: un código se acepta con ±1 paso de tolerancia y nunca dos
//!   veces (`totp_ultimo_paso`). Los códigos incorrectos cuentan en el mismo
//!   contador que las contraseñas: al quinto fallo la cuenta se bloquea 15 min.
//! - Todo queda en `kyc.eventos_autenticacion` (inmutable), con la IP.
//!
//! Solo el rol `sc_auth` lee o escribe los secretos.

use std::{fmt::Write as _, net::IpAddr};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use qrcodegen::{QrCode, QrCodeEcc};
use rand::RngCore;
use serde::Serialize;
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, TOTP};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{jwt::Desafio, ResultadoLogin, Sesion, SQL_REGISTRAR_FALLO};
use crate::{
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    models::kyc::RolUsuario,
    state::AppState,
};

/// Emisor que muestran las apps autenticadoras.
pub const EMISOR_TOTP: &str = "Synthetic Capital";
pub const PASO_SEG: u64 = 30;
pub const DIGITOS: usize = 6;
/// 160 bits, el tamaño que recomienda RFC 4226.
const BYTES_SECRETO: usize = 20;

/// El personal interno no opera sin segundo factor; los clientes pueden activarlo.
pub fn exige_mfa(rol: RolUsuario) -> bool {
    rol != RolUsuario::Cliente
}

/// Dato asociado del cifrado: el sobre solo se abre en la fila de su dueño.
fn contexto(cliente_id: Uuid) -> Vec<u8> {
    format!("synthetic-capital/totp/v1/{cliente_id}").into_bytes()
}

fn generar_secreto() -> Zeroizing<Vec<u8>> {
    let mut secreto = Zeroizing::new(vec![0u8; BYTES_SECRETO]);
    rand::rngs::OsRng.fill_bytes(&mut secreto);
    secreto
}

/// El código tal como lo teclea la persona: 6 dígitos, se toleran espacios.
fn normalizar_codigo(codigo: &str) -> AppResult<String> {
    let limpio: String = codigo.chars().filter(|c| !c.is_whitespace()).collect();
    if limpio.len() != DIGITOS || !limpio.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::validacion("el código de verificación tiene 6 dígitos"));
    }
    Ok(limpio)
}

/// Paso aceptado para `codigo` en `ahora` (segundos unix) con ±1 paso de
/// tolerancia, siempre posterior a `ultimo` (anti-repetición). Compara en
/// tiempo constante y revisa los tres pasos siempre.
pub fn verificar_codigo(secreto: &[u8], codigo: &str, ahora: u64, ultimo: Option<i64>) -> Option<i64> {
    let totp = TOTP::new_unchecked(Algorithm::SHA1, DIGITOS, 1, PASO_SEG, secreto.to_vec(), None, String::new());
    let actual = ahora / PASO_SEG;
    let mut aceptado = None;
    for paso in [actual.saturating_sub(1), actual, actual + 1] {
        let esperado = Zeroizing::new(totp.generate(paso * PASO_SEG));
        let coincide = bool::from(esperado.as_bytes().ct_eq(codigo.as_bytes()));
        let paso = i64::try_from(paso).unwrap_or(i64::MAX);
        if coincide && ultimo.map_or(true, |u| paso > u) && aceptado.is_none() {
            aceptado = Some(paso);
        }
    }
    aceptado
}

/// QR (corrección media) de `texto` como data URL SVG: sin dependencias de
/// imagen y nítido a cualquier tamaño. La CSP admite `img-src data:`.
fn qr_svg(texto: &str) -> AppResult<String> {
    let qr = QrCode::encode_text(texto, QrCodeEcc::Medium)
        .map_err(|e| AppError::Interno(anyhow::anyhow!("no se pudo generar el QR: {e:?}")))?;
    const BORDE: i32 = 4;
    let lado = qr.size() + 2 * BORDE;
    let mut ruta = String::new();
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                let _ = write!(ruta, "M{},{}h1v1h-1z", x + BORDE, y + BORDE);
            }
        }
    }
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {lado} {lado}" shape-rendering="crispEdges"><rect width="100%" height="100%" fill="#ffffff"/><path d="{ruta}" fill="#000000"/></svg>"##
    );
    Ok(format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg)))
}

/// Inscripción en curso: se muestra una sola vez.
#[derive(Debug, Serialize)]
pub struct ConfiguracionMfa {
    /// Secreto en base32, para capturarlo a mano si no se puede escanear el QR.
    pub secreto: String,
    pub otpauth_url: String,
    /// QR de `otpauth_url` (data URL SVG).
    pub qr_svg: String,
    pub emisor: &'static str,
    pub cuenta: String,
    pub digitos: usize,
    pub periodo_seg: u64,
}

/// Registra un evento de autenticación en la transacción `tx` (rol sc_auth).
pub(crate) async fn registrar_evento(tx: &mut Tx, cliente_id: Uuid, evento: &str, ip: Option<IpAddr>) -> AppResult<()> {
    sqlx::query("INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip) VALUES ($1, $2, $3::inet)")
        .bind(cliente_id)
        .bind(evento)
        .bind(ip.map(|i| i.to_string()))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn descifrar(estado: &AppState, sobre: &str, cliente_id: Uuid) -> AppResult<Zeroizing<Vec<u8>>> {
    estado.mfa.descifrar(sobre, &contexto(cliente_id)).map(|d| d.texto).map_err(|e| {
        AppError::Interno(anyhow::anyhow!(
            "no se pudo descifrar el secreto TOTP de {cliente_id} ({e}): revisa MFA_CLAVE_CIFRADO y MFA_CLAVE_CIFRADO_ANTERIOR"
        ))
    })
}

fn ahora_seg() -> u64 {
    u64::try_from(Utc::now().timestamp()).unwrap_or(0)
}

/// Genera (o reemplaza, mientras no esté activo) el secreto TOTP de la persona.
pub async fn configurar(estado: &AppState, cliente_id: Uuid, ip: Option<IpAddr>) -> AppResult<ConfiguracionMfa> {
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let fila: Option<(String, bool)> = sqlx::query_as(
        "SELECT c.email, cr.mfa_enabled FROM kyc.credenciales cr JOIN kyc.clientes c ON c.id = cr.cliente_id \
         WHERE cr.cliente_id = $1 FOR UPDATE OF cr",
    )
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (email, activo) = fila.ok_or(AppError::NoAutenticado)?;
    if activo {
        return Err(mfa_ya_activo());
    }
    let secreto = generar_secreto();
    let sobre = estado.mfa.cifrar(&secreto, &contexto(cliente_id)).map_err(|e| AppError::Interno(e.into()))?;
    sqlx::query("UPDATE kyc.credenciales SET totp_secret = $2, totp_ultimo_paso = NULL WHERE cliente_id = $1")
        .bind(cliente_id)
        .bind(&sobre)
        .execute(&mut *tx)
        .await?;
    registrar_evento(&mut tx, cliente_id, "MFA_SECRETO_GENERADO", ip).await?;
    tx.commit().await?;

    // Las apps separan emisor y cuenta con ':'; un email nunca lo lleva, pero por si acaso.
    let cuenta = if email.contains(':') { cliente_id.to_string() } else { email };
    let totp =
        TOTP::new(Algorithm::SHA1, DIGITOS, 1, PASO_SEG, secreto.to_vec(), Some(EMISOR_TOTP.into()), cuenta.clone())
            .map_err(|e| AppError::Interno(anyhow::anyhow!("TOTP inválido: {e}")))?;
    let otpauth_url = totp.get_url();
    Ok(ConfiguracionMfa {
        secreto: totp.get_secret_base32(),
        qr_svg: qr_svg(&otpauth_url)?,
        otpauth_url,
        emisor: EMISOR_TOTP,
        cuenta,
        digitos: DIGITOS,
        periodo_seg: PASO_SEG,
    })
}

#[derive(sqlx::FromRow)]
struct FilaMfa {
    rol: RolUsuario,
    totp_secret: Option<String>,
    mfa_enabled: bool,
    totp_ultimo_paso: Option<i64>,
    bloqueada: bool,
}

/// Verifica el primer código y activa el segundo factor. Revoca las demás
/// sesiones y devuelve una de acceso con el factor verificado.
pub async fn activar(
    estado: &AppState,
    cliente_id: Uuid,
    codigo: &str,
    ip: Option<IpAddr>,
) -> AppResult<ResultadoLogin> {
    let codigo = normalizar_codigo(codigo)?;
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let fila: FilaMfa = sqlx::query_as(
        "SELECT rol, totp_secret, mfa_enabled, totp_ultimo_paso, coalesce(bloqueado_hasta > now(), false) AS bloqueada \
         FROM kyc.credenciales WHERE cliente_id = $1 FOR UPDATE",
    )
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NoAutenticado)?;
    if fila.bloqueada {
        return Err(cuenta_bloqueada());
    }
    if fila.mfa_enabled {
        return Err(mfa_ya_activo());
    }
    let Some(sobre) = fila.totp_secret else {
        return Err(AppError::negocio("MFA_SIN_CONFIGURAR", "primero genera el secreto (POST /api/v1/auth/mfa/setup)"));
    };
    let secreto = descifrar(estado, &sobre, cliente_id)?;
    let Some(paso) = verificar_codigo(&secreto, &codigo, ahora_seg(), fila.totp_ultimo_paso) else {
        return rechazar_codigo(tx, cliente_id, ip).await;
    };
    let (rol, iat): (RolUsuario, i64) = sqlx::query_as(
        "UPDATE kyc.credenciales SET mfa_enabled = TRUE, totp_ultimo_paso = $2, intentos_fallidos = 0, \
           bloqueado_hasta = NULL, tokens_validos_desde = now(), ultimo_acceso = now() \
         WHERE cliente_id = $1 RETURNING rol, floor(extract(epoch FROM now()))::bigint",
    )
    .bind(cliente_id)
    .bind(paso)
    .fetch_one(&mut *tx)
    .await?;
    registrar_evento(&mut tx, cliente_id, "MFA_ACTIVADO", ip).await?;
    tx.commit().await?;
    tracing::info!(target: "auth", %cliente_id, ?rol, "segundo factor activado");
    let (token, expira_en) = estado.jwt.emitir_desde(cliente_id, rol, true, iat)?;
    debug_assert_eq!(rol, fila.rol);
    Ok(ResultadoLogin::Sesion(Sesion { token, expira_en, cliente_id, rol }))
}

#[derive(sqlx::FromRow)]
struct FilaDesafio {
    rol: RolUsuario,
    totp_secret: Option<String>,
    mfa_enabled: bool,
    totp_ultimo_paso: Option<i64>,
    bloqueada: bool,
    revocado: bool,
    en_lista_bloqueadas: bool,
}

/// Segundo paso del inicio de sesión: el desafío de `POST /auth/login` y el código TOTP.
pub async fn verificar_inicio(
    estado: &AppState,
    token_mfa: &str,
    codigo: &str,
    ip: Option<IpAddr>,
) -> AppResult<ResultadoLogin> {
    let desafio = estado.jwt.validar_desafio(token_mfa, Desafio::Mfa)?;
    let codigo = normalizar_codigo(codigo)?;
    let cliente_id = desafio.sub;
    let mut tx = db::begin(&estado.pool, Rol::Auth).await?;
    let fila: FilaDesafio = sqlx::query_as(
        "SELECT cr.rol, cr.totp_secret, cr.mfa_enabled, cr.totp_ultimo_paso, \
                coalesce(cr.bloqueado_hasta > now(), false) AS bloqueada, \
                coalesce(floor(extract(epoch FROM cr.tokens_validos_desde)) > $2, false) AS revocado, \
                c.en_lista_bloqueadas \
         FROM kyc.credenciales cr JOIN kyc.clientes c ON c.id = cr.cliente_id \
         WHERE cr.cliente_id = $1 FOR UPDATE OF cr",
    )
    .bind(cliente_id)
    .bind(desafio.iat)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::DesafioVencido)?;
    // Sesiones revocadas (p. ej. una contraseña restablecida), otro rol o un
    // factor restablecido por el DBA desde que se emitió el desafío.
    if fila.revocado || fila.rol != desafio.rol || !fila.mfa_enabled {
        return Err(AppError::DesafioVencido);
    }
    if fila.en_lista_bloqueadas {
        return Err(AppError::Prohibido);
    }
    if fila.bloqueada {
        return Err(cuenta_bloqueada());
    }
    let sobre = fila.totp_secret.ok_or(AppError::DesafioVencido)?;
    let secreto = descifrar(estado, &sobre, cliente_id)?;
    let Some(paso) = verificar_codigo(&secreto, &codigo, ahora_seg(), fila.totp_ultimo_paso) else {
        return rechazar_codigo(tx, cliente_id, ip).await;
    };
    // La fila está bloqueada (FOR UPDATE); la condición repite la regla anti-repetición.
    let n = sqlx::query(
        "UPDATE kyc.credenciales SET totp_ultimo_paso = $2, intentos_fallidos = 0, bloqueado_hasta = NULL, \
           ultimo_acceso = now() \
         WHERE cliente_id = $1 AND (totp_ultimo_paso IS NULL OR totp_ultimo_paso < $2)",
    )
    .bind(cliente_id)
    .bind(paso)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n != 1 {
        return rechazar_codigo(tx, cliente_id, ip).await;
    }
    registrar_evento(&mut tx, cliente_id, "MFA_VERIFICADO", ip).await?;
    tx.commit().await?;
    let (token, expira_en) = estado.jwt.emitir(cliente_id, fila.rol, true)?;
    Ok(ResultadoLogin::Sesion(Sesion { token, expira_en, cliente_id, rol: fila.rol }))
}

/// Código incorrecto: cuenta como intento fallido (bloquea al quinto) y queda registrado.
async fn rechazar_codigo<T>(mut tx: Tx, cliente_id: Uuid, ip: Option<IpAddr>) -> AppResult<T> {
    sqlx::query(SQL_REGISTRAR_FALLO)
        .bind(cliente_id)
        .bind(super::MAX_INTENTOS)
        .bind(super::MINUTOS_BLOQUEO as i32)
        .execute(&mut *tx)
        .await?;
    registrar_evento(&mut tx, cliente_id, "MFA_CODIGO_RECHAZADO", ip).await?;
    tx.commit().await?;
    tracing::warn!(target: "auth", %cliente_id, "código TOTP rechazado");
    Err(AppError::CodigoMfaInvalido)
}

/// Rotación de `MFA_CLAVE_CIFRADO`: vuelve a cifrar con la clave vigente cada
/// secreto que solo abre la anterior. Corre como dueño del esquema (`migrar`):
/// con el segundo factor activo, solo el dueño puede cambiar el secreto.
pub async fn recifrar_secretos(pool: &sqlx::PgPool, cifrador: &crate::crypto::cifrado::Cifrador) -> AppResult<u64> {
    let mut tx = db::begin_administrativa(pool).await?;
    let filas: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT cliente_id, totp_secret FROM kyc.credenciales WHERE totp_secret IS NOT NULL ORDER BY cliente_id FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut recifrados = 0;
    for (cliente_id, sobre) in filas {
        match cifrador.descifrar(&sobre, &contexto(cliente_id)) {
            Ok(d) if d.con_clave_anterior => {
                let nuevo =
                    cifrador.cifrar(&d.texto, &contexto(cliente_id)).map_err(|e| AppError::Interno(e.into()))?;
                sqlx::query("UPDATE kyc.credenciales SET totp_secret = $2 WHERE cliente_id = $1")
                    .bind(cliente_id)
                    .bind(nuevo)
                    .execute(&mut *tx)
                    .await?;
                recifrados += 1;
            }
            Ok(_) => {}
            Err(e) => tracing::error!(
                %cliente_id, error = %e,
                "ninguna clave descifra este secreto TOTP: restablece su segundo factor (DEPLOYMENT.md)"
            ),
        }
    }
    tx.commit().await?;
    Ok(recifrados)
}

fn mfa_ya_activo() -> AppError {
    AppError::conflicto(
        "MFA_YA_ACTIVO",
        "el segundo factor ya está activo; para cambiar de dispositivo, pide a soporte que lo restablezca",
    )
}

fn cuenta_bloqueada() -> AppError {
    AppError::Bloqueado("demasiados intentos fallidos; la cuenta está bloqueada temporalmente".into())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    // Vector de RFC 6238 (apéndice B, SHA-1): secreto ASCII "12345678901234567890".
    const SECRETO_RFC: &[u8] = b"12345678901234567890";

    #[test]
    fn coincide_con_los_vectores_del_rfc_6238() {
        // T = 59 -> 94287082 (8 dígitos); los 6 últimos: 287082.
        assert_eq!(verificar_codigo(SECRETO_RFC, "287082", 59, None), Some(1));
        // T = 1111111109 -> 07081804 -> 081804.
        assert_eq!(verificar_codigo(SECRETO_RFC, "081804", 1_111_111_109, None), Some(1_111_111_109 / 30));
    }

    #[test]
    fn tolera_un_paso_y_nada_mas() {
        let ahora = 1_111_111_109;
        let paso = ahora / 30;
        let totp = TOTP::new_unchecked(Algorithm::SHA1, 6, 1, 30, SECRETO_RFC.to_vec(), None, String::new());
        for (desfase, aceptado) in [(-2i64, false), (-1, true), (0, true), (1, true), (2, false)] {
            let t = ((paso as i64 + desfase) * 30) as u64;
            let codigo = totp.generate(t);
            assert_eq!(verificar_codigo(SECRETO_RFC, &codigo, ahora, None).is_some(), aceptado, "desfase {desfase}");
        }
    }

    #[test]
    fn un_codigo_no_se_acepta_dos_veces_ni_uno_anterior() {
        let ahora = 1_111_111_109;
        let paso = (ahora / 30) as i64;
        assert_eq!(verificar_codigo(SECRETO_RFC, "081804", ahora, Some(paso)), None, "ya usado");
        assert_eq!(verificar_codigo(SECRETO_RFC, "081804", ahora, Some(paso + 1)), None, "anterior al último");
        assert_eq!(verificar_codigo(SECRETO_RFC, "081804", ahora, Some(paso - 1)), Some(paso));
    }

    #[test]
    fn rechaza_codigos_ajenos_y_mal_formados() {
        assert_eq!(verificar_codigo(SECRETO_RFC, "000000", 59, None), None);
        assert!(normalizar_codigo("12345").is_err());
        assert!(normalizar_codigo("12345a").is_err());
        assert!(normalizar_codigo("1234567").is_err());
        assert_eq!(normalizar_codigo(" 287 082 ").unwrap(), "287082");
    }

    #[test]
    fn el_qr_es_un_svg_que_codifica_la_url() {
        let datos = qr_svg("otpauth://totp/Synthetic%20Capital:ana%40example.mx?secret=ABC&issuer=Synthetic%20Capital")
            .unwrap();
        let svg =
            String::from_utf8(STANDARD.decode(datos.strip_prefix("data:image/svg+xml;base64,").unwrap()).unwrap())
                .unwrap();
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.contains("<path d=\"M"));
    }

    #[test]
    fn el_personal_exige_mfa_y_el_cliente_no() {
        assert!(!exige_mfa(RolUsuario::Cliente));
        for rol in [RolUsuario::Operador, RolUsuario::Auditor, RolUsuario::Cumplimiento] {
            assert!(exige_mfa(rol));
        }
    }
}
