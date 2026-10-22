use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use super::status_de;
use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    models::kyc::RolUsuario,
    routes::extraccion::{Json, Path},
    sofom::{
        self,
        puente::{self, ResultadoLiquidacion, ResultadoMutuo, TerminosMutuo},
        tasas::{self, PlazoTasa},
        Posicion,
    },
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/sofom/tasas", get(tabla_tasas))
        .route("/sofom/posicion", get(posicion))
        .route("/sofom/mutuos", post(invertir))
        .route("/sofom/mutuos/{id}/terminos", get(terminos))
        .route("/sofom/mutuos/{id}/liquidar", post(liquidar))
}

async fn tabla_tasas() -> Json<[PlazoTasa; 5]> {
    Json(tasas::tabla())
}

async fn posicion(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Posicion>> {
    u.exigir_cliente()?;
    Ok(Json(sofom::posicion_de_cliente(&e.pool, u.cliente_id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudInversion {
    #[serde(with = "rust_decimal::serde::str")]
    monto: Decimal,
    plazo_dias: i32,
    clave_idempotencia: String,
}

/// Puente IFPE -> SOFOM: compromete saldo disponible de la bóveda en un
/// contrato de mutuo a plazo fijo.
async fn invertir(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudInversion>,
) -> AppResult<(StatusCode, Json<ResultadoMutuo>)> {
    u.exigir_cliente()?;
    let solicitud = puente::SolicitudMutuo {
        cliente_id: u.cliente_id,
        monto: s.monto,
        plazo_dias: s.plazo_dias,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let r = puente::invertir(&e.pool, &e.config.pld, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r)))
}

async fn terminos(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<TerminosMutuo>> {
    u.exigir_cliente()?;
    let posicion = sofom::posicion_de_cliente(&e.pool, u.cliente_id).await?;
    let contrato = posicion
        .contratos
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::no_encontrado("contrato de mutuo no encontrado"))?;
    Ok(Json(puente::verificar_terminos(contrato)?))
}

async fn liquidar(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
) -> AppResult<(StatusCode, Json<ResultadoLiquidacion>)> {
    let solicitante = match u.rol {
        RolUsuario::Cliente => Some(u.cliente_id),
        RolUsuario::Operador => None,
        // Un oficial de cumplimiento no mueve dinero (solo la consola PLD).
        RolUsuario::Auditor | RolUsuario::Cumplimiento => return Err(AppError::Prohibido),
    };
    let r = puente::liquidar(&e.pool, id, solicitante).await?;
    Ok((status_de(r.idempotente), Json(r)))
}
