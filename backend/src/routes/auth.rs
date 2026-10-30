use axum::{
    extract::State,
    http::{header, StatusCode},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{self, mfa, restablecimiento, IdentidadMfa, ResultadoLogin, Sesion, Usuario},
    error::AppResult,
    kyc::{self, CuentaAbierta, DatosRegistro},
    ledger,
    models::{ifpe::CuentaIfpe, kyc::ClienteVista},
    reserva::{self, EstadoReservaVista},
    routes::extraccion::{IpCliente, Json},
    sofom::{self, Posicion},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/auth/registro", post(registro))
        .route("/auth/login", post(login))
        .route("/auth/verify-mfa", post(verificar_mfa))
        .route("/auth/mfa/setup", post(configurar_mfa))
        .route("/auth/mfa/enable", post(activar_mfa))
        .route("/auth/password/forgot", post(olvido_password))
        .route("/auth/password/reset", post(restablecer_password))
        .route("/yo", get(yo))
        .route("/tablero", get(tablero))
}

/// Las proyecciones del propio cliente usan [`ClienteVista`]: sin nivel de
/// riesgo PLD ni marca PEP (sin aviso al cliente).
#[derive(Serialize)]
struct RespuestaRegistro {
    sesion: Sesion,
    cliente: ClienteVista,
    cuenta_ifpe: CuentaAbierta,
}

async fn registro(
    State(e): State<AppState>,
    Json(datos): Json<DatosRegistro>,
) -> AppResult<(StatusCode, Json<RespuestaRegistro>)> {
    let r = kyc::registrar(&e.pool, &e.config.clabe_prefijo, datos).await?;
    let (token, expira_en) = e.jwt.emitir(r.cliente.id, crate::models::kyc::RolUsuario::Cliente, false)?;
    let sesion = Sesion { token, expira_en, cliente_id: r.cliente.id, rol: crate::models::kyc::RolUsuario::Cliente };
    Ok((StatusCode::CREATED, Json(RespuestaRegistro { sesion, cliente: r.cliente.into(), cuenta_ifpe: r.cuenta_ifpe })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credenciales {
    email: String,
    password: String,
}

/// `estado`: `OK` (sesión), `2FA_REQUIRED` (falta el código TOTP) o
/// `MFA_ENROLLMENT_REQUIRED` (personal interno sin segundo factor).
async fn login(State(e): State<AppState>, Json(c): Json<Credenciales>) -> AppResult<Json<ResultadoLogin>> {
    Ok(Json(auth::iniciar_sesion(&e, &c.email, &c.password).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificacionMfa {
    token_mfa: String,
    codigo: String,
}

/// Segundo paso del inicio de sesión: el desafío de `/auth/login` y el código TOTP.
async fn verificar_mfa(
    State(e): State<AppState>,
    IpCliente(ip): IpCliente,
    Json(v): Json<VerificacionMfa>,
) -> AppResult<Json<ResultadoLogin>> {
    Ok(Json(mfa::verificar_inicio(&e, &v.token_mfa, &v.codigo, ip).await?))
}

/// Genera el secreto TOTP (URL otpauth y QR). La respuesta no se guarda en cachés.
async fn configurar_mfa(
    State(e): State<AppState>,
    id: IdentidadMfa,
    IpCliente(ip): IpCliente,
) -> AppResult<([(header::HeaderName, &'static str); 1], Json<mfa::ConfiguracionMfa>)> {
    let configuracion = mfa::configurar(&e, id.cliente_id, ip).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(configuracion)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodigoMfa {
    codigo: String,
}

/// Verifica el primer código y activa el segundo factor; devuelve la sesión.
async fn activar_mfa(
    State(e): State<AppState>,
    id: IdentidadMfa,
    IpCliente(ip): IpCliente,
    Json(c): Json<CodigoMfa>,
) -> AppResult<Json<ResultadoLogin>> {
    Ok(Json(mfa::activar(&e, id.cliente_id, &c.codigo, ip).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OlvidoPassword {
    email: String,
}

/// Siempre 202 con el mismo cuerpo, exista o no la cuenta.
async fn olvido_password(
    State(e): State<AppState>,
    IpCliente(ip): IpCliente,
    Json(o): Json<OlvidoPassword>,
) -> AppResult<(StatusCode, Json<Value>)> {
    restablecimiento::solicitar(&e, &o.email, ip).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "estado": "SOLICITUD_RECIBIDA",
            "mensaje": "si el email está registrado, enviamos un enlace para restablecer la contraseña"
        })),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestablecerPassword {
    token: String,
    password: String,
}

async fn restablecer_password(
    State(e): State<AppState>,
    IpCliente(ip): IpCliente,
    Json(r): Json<RestablecerPassword>,
) -> AppResult<Json<Value>> {
    restablecimiento::confirmar(&e, &r.token, &r.password, ip).await?;
    Ok(Json(json!({ "estado": "OK" })))
}

async fn yo(State(e): State<AppState>, u: Usuario) -> AppResult<Json<ClienteVista>> {
    Ok(Json(kyc::perfil(&e.pool, u.cliente_id).await?.into()))
}

/// Resumen para el dashboard. Cada sección se lee en su propia transacción
/// con su propio rol: el aislamiento IFPE/SOFOM se mantiene incluso al leer.
#[derive(Serialize)]
struct Tablero {
    cliente: ClienteVista,
    boveda: Option<CuentaIfpe>,
    inversion: Posicion,
    reserva: EstadoReservaVista,
}

async fn tablero(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Tablero>> {
    u.exigir_cliente()?;
    let (cliente, boveda, inversion, reserva) = tokio::try_join!(
        kyc::perfil(&e.pool, u.cliente_id),
        ledger::cuenta_de_cliente(&e.pool, u.cliente_id),
        sofom::posicion_de_cliente(&e.pool, u.cliente_id),
        reserva::estado_actual(&e.pool),
    )?;
    Ok(Json(Tablero { cliente: cliente.into(), boveda, inversion, reserva }))
}
