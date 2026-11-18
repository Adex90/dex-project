use axum::{
    extract::State,
    http::{header, StatusCode},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{
        self, mfa, restablecimiento,
        seguridad::{self, EventoVista, Preferencias},
        sesiones::{self, ListaSesiones, Origen},
        IdentidadMfa, ResultadoLogin, Sesion, Usuario,
    },
    error::AppResult,
    kyc::{self, CuentaAbierta, DatosRegistro},
    ledger,
    models::{
        ifpe::CuentaIfpe,
        kyc::{ClienteVista, RolUsuario},
    },
    reserva::{self, EstadoReservaVista},
    routes::{
        extraccion::{Json, Path, Query},
        limites::{self, Limitadores},
    },
    sofom::{self, Posicion},
    state::AppState,
};

pub fn rutas(limites: &Limitadores) -> Router<AppState> {
    // Las que prueban credenciales o crean cuentas: el límite por IP estricto.
    let acceso = Router::new()
        .route("/auth/registro", post(registro))
        .route("/auth/login", post(login))
        .route("/auth/verify-mfa", post(verificar_mfa))
        .route("/auth/mfa/setup", post(configurar_mfa))
        .route("/auth/mfa/enable", post(activar_mfa))
        .route("/auth/password/forgot", post(olvido_password))
        .route("/auth/password/reset", post(restablecer_password))
        .route("/auth/password/change", post(cambiar_password));
    // Centro de seguridad (sesiones por dispositivo y preferencias, 0019): el límite general.
    Router::new()
        .merge(limites::aplicar(acceso, &limites.auth))
        .route("/yo", get(yo))
        .route("/tablero", get(tablero))
        .route("/auth/logout", post(cerrar_sesion))
        .route("/auth/sessions", get(listar_sesiones))
        .route("/auth/sessions/revoke-others", post(revocar_otras))
        .route("/auth/sessions/{id}/revoke", post(revocar_sesion))
        .route("/auth/security/events", get(eventos))
        .route("/auth/security/preferences", get(preferencias).post(guardar_preferencias))
}

/// Las proyecciones del propio cliente usan [`ClienteVista`]: sin nivel de
/// riesgo PLD ni marca PEP (sin aviso al cliente).
#[derive(Serialize)]
struct RespuestaRegistro {
    sesion: Sesion,
    cliente: ClienteVista,
    cuenta_ifpe: CuentaAbierta,
}

/// Abre la cuenta y su primera sesión.
async fn registro(
    State(e): State<AppState>,
    origen: Origen,
    Json(datos): Json<DatosRegistro>,
) -> AppResult<(StatusCode, Json<RespuestaRegistro>)> {
    let r = kyc::registrar(&e.pool, &e.config.clabe_prefijo, datos).await?;
    let sesion = sesiones::abrir(&e, r.cliente.id, RolUsuario::Cliente, false, &origen).await?;
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
async fn login(
    State(e): State<AppState>,
    origen: Origen,
    Json(c): Json<Credenciales>,
) -> AppResult<Json<ResultadoLogin>> {
    Ok(Json(auth::iniciar_sesion(&e, &c.email, &c.password, &origen).await?))
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
    origen: Origen,
    Json(v): Json<VerificacionMfa>,
) -> AppResult<Json<ResultadoLogin>> {
    Ok(Json(mfa::verificar_inicio(&e, &v.token_mfa, &v.codigo, &origen).await?))
}

/// Genera el secreto TOTP (URL otpauth y QR). La respuesta no se guarda en cachés.
async fn configurar_mfa(
    State(e): State<AppState>,
    id: IdentidadMfa,
    origen: Origen,
) -> AppResult<([(header::HeaderName, &'static str); 1], Json<mfa::ConfiguracionMfa>)> {
    let configuracion = mfa::configurar(&e, id.cliente_id, &origen).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(configuracion)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodigoMfa {
    codigo: String,
}

/// Verifica el primer código y activa el segundo factor; cierra las demás
/// sesiones y devuelve una nueva.
async fn activar_mfa(
    State(e): State<AppState>,
    id: IdentidadMfa,
    origen: Origen,
    Json(c): Json<CodigoMfa>,
) -> AppResult<Json<ResultadoLogin>> {
    Ok(Json(mfa::activar(&e, id.cliente_id, &c.codigo, &origen).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OlvidoPassword {
    email: String,
}

/// Siempre 202 con el mismo cuerpo, exista o no la cuenta.
async fn olvido_password(
    State(e): State<AppState>,
    origen: Origen,
    Json(o): Json<OlvidoPassword>,
) -> AppResult<(StatusCode, Json<Value>)> {
    restablecimiento::solicitar(&e, &o.email, &origen).await?;
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
    origen: Origen,
    Json(r): Json<RestablecerPassword>,
) -> AppResult<Json<Value>> {
    restablecimiento::confirmar(&e, &r.token, &r.password, &origen).await?;
    Ok(Json(json!({ "estado": "OK" })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CambioPassword {
    password_actual: String,
    password_nueva: String,
    /// Código TOTP: obligatorio con el segundo factor activo.
    codigo: Option<String>,
}

/// Cambia la contraseña (la actual, y el código TOTP si está activo) y cierra
/// las demás sesiones. Los intentos cuentan en el bloqueo del inicio de sesión.
async fn cambiar_password(
    State(e): State<AppState>,
    u: Usuario,
    origen: Origen,
    Json(c): Json<CambioPassword>,
) -> AppResult<Json<Value>> {
    let n = seguridad::cambiar_password(&e, &u, &c.password_actual, &c.password_nueva, c.codigo.as_deref(), &origen)
        .await?;
    Ok(Json(json!({ "estado": "OK", "sesiones_revocadas": n })))
}

/// Cierra la sesión del token; las demás siguen abiertas.
async fn cerrar_sesion(State(e): State<AppState>, u: Usuario, origen: Origen) -> AppResult<StatusCode> {
    sesiones::cerrar(&e.pool, &u, &origen).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sesiones activas por dispositivo; `actual` marca la de la petición.
async fn listar_sesiones(State(e): State<AppState>, u: Usuario) -> AppResult<Json<ListaSesiones>> {
    Ok(Json(sesiones::listar(&e.pool, &u).await?))
}

/// Cierra una sesión de quien pregunta (otro dispositivo, o esta misma).
async fn revocar_sesion(
    State(e): State<AppState>,
    u: Usuario,
    origen: Origen,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    sesiones::revocar(&e.pool, &u, id, &origen).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct SesionesRevocadas {
    sesiones_revocadas: u64,
}

/// "Cerrar sesión en todos los demás dispositivos".
async fn revocar_otras(State(e): State<AppState>, u: Usuario, origen: Origen) -> AppResult<Json<SesionesRevocadas>> {
    let sesiones_revocadas = sesiones::revocar_otras(&e.pool, &u, &origen).await?;
    Ok(Json(SesionesRevocadas { sesiones_revocadas }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaEventos {
    limite: Option<i64>,
}

#[derive(Serialize)]
struct Eventos {
    eventos: Vec<EventoVista>,
}

/// Bitácora de seguridad de quien pregunta, la más reciente primero.
async fn eventos(State(e): State<AppState>, u: Usuario, Query(q): Query<ConsultaEventos>) -> AppResult<Json<Eventos>> {
    Ok(Json(Eventos { eventos: seguridad::eventos(&e.pool, &u, q.limite).await? }))
}

async fn preferencias(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Preferencias>> {
    Ok(Json(seguridad::preferencias(&e.pool, &u).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NuevasPreferencias {
    inactividad_minutos: i16,
}

/// Cierre por inactividad: 5, 15, 30 o 60 minutos.
async fn guardar_preferencias(
    State(e): State<AppState>,
    u: Usuario,
    origen: Origen,
    Json(p): Json<NuevasPreferencias>,
) -> AppResult<Json<Preferencias>> {
    Ok(Json(seguridad::guardar_preferencias(&e.pool, &u, p.inactividad_minutos, &origen).await?))
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
