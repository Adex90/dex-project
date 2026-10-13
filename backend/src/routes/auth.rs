use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{self, Sesion, Usuario},
    error::AppResult,
    kyc::{self, CuentaAbierta, DatosRegistro},
    ledger,
    models::{ifpe::CuentaIfpe, kyc::Cliente},
    reserva::{self, EstadoReservaVista},
    routes::extraccion::Json,
    sofom::{self, Posicion},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/auth/registro", post(registro))
        .route("/auth/login", post(login))
        .route("/yo", get(yo))
        .route("/tablero", get(tablero))
}

#[derive(Serialize)]
struct RespuestaRegistro {
    sesion: Sesion,
    cliente: Cliente,
    cuenta_ifpe: CuentaAbierta,
}

async fn registro(
    State(e): State<AppState>,
    Json(datos): Json<DatosRegistro>,
) -> AppResult<(StatusCode, Json<RespuestaRegistro>)> {
    let r = kyc::registrar(&e.pool, &e.config.clabe_prefijo, e.config.modo_demo, datos).await?;
    let (token, expira_en) = e.jwt.emitir(r.cliente.id, crate::models::kyc::RolUsuario::Cliente)?;
    let sesion = Sesion { token, expira_en, cliente_id: r.cliente.id, rol: crate::models::kyc::RolUsuario::Cliente };
    Ok((StatusCode::CREATED, Json(RespuestaRegistro { sesion, cliente: r.cliente, cuenta_ifpe: r.cuenta_ifpe })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credenciales {
    email: String,
    password: String,
}

async fn login(State(e): State<AppState>, Json(c): Json<Credenciales>) -> AppResult<Json<Sesion>> {
    Ok(Json(auth::iniciar_sesion(&e, &c.email, &c.password).await?))
}

async fn yo(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Cliente>> {
    Ok(Json(kyc::perfil(&e.pool, u.cliente_id).await?))
}

/// Resumen para el dashboard. Cada sección se lee en su propia transacción
/// con su propio rol: el aislamiento IFPE/SOFOM se mantiene incluso al leer.
#[derive(Serialize)]
struct Tablero {
    cliente: Cliente,
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
    Ok(Json(Tablero { cliente, boveda, inversion, reserva }))
}
