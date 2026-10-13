use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    ledger::hash::{self, ResultadoVerificacion},
    models::ifpe::CorteReserva,
    reserva::{self, EstadoReservaVista, PruebaReserva, ResultadoVerificacionPrueba, SolicitudVerificacion},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/reserva/estado", get(estado))
        .route("/reserva/cortes/ultimo", get(ultimo_corte))
        .route("/reserva/cortes", get(listar_cortes).post(generar_corte))
        .route("/reserva/cortes/{id}", get(corte))
        .route("/reserva/prueba", get(prueba))
        .route("/reserva/verificar", post(verificar))
        .route("/auditoria/ledger/ifpe", get(auditar_ifpe))
        .route("/auditoria/ledger/sofom", get(auditar_sofom))
}

async fn estado(State(e): State<AppState>) -> AppResult<Json<EstadoReservaVista>> {
    Ok(Json(reserva::estado_actual(&e.pool).await?))
}

async fn ultimo_corte(State(e): State<AppState>) -> AppResult<Json<CorteReserva>> {
    reserva::ultimo_corte(&e.pool)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::no_encontrado("aún no hay cortes de reserva publicados"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FiltroCortes {
    antes: Option<DateTime<Utc>>,
    limite: Option<i64>,
}

/// Historial público de anclas (raíz, totales, secuencia y hash del ledger).
async fn listar_cortes(State(e): State<AppState>, Query(f): Query<FiltroCortes>) -> AppResult<Json<Vec<CorteReserva>>> {
    Ok(Json(reserva::listar_cortes(&e.pool, f.antes, f.limite.unwrap_or(100)).await?))
}

/// Un corte publicado, sin autenticación: el navegador del cliente compara su
/// prueba con esta copia pública, no solo con la respuesta autenticada.
async fn corte(State(e): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<CorteReserva>> {
    Ok(Json(reserva::corte_por_id(&e.pool, id).await?))
}

async fn generar_corte(State(e): State<AppState>, u: Usuario) -> AppResult<(StatusCode, Json<CorteReserva>)> {
    u.exigir_operador_o_auditor()?;
    Ok((StatusCode::CREATED, Json(reserva::generar_corte(&e.pool).await?)))
}

/// Prueba de inclusión (MerkleProof) de la cuenta del cliente en el último corte.
async fn prueba(State(e): State<AppState>, u: Usuario) -> AppResult<Json<PruebaReserva>> {
    u.exigir_cliente()?;
    Ok(Json(reserva::prueba_de_cliente(&e.pool, &e.cortes, u.cliente_id, e.config.corte_min_intervalo_seg).await?))
}

async fn verificar(
    State(e): State<AppState>,
    Json(s): Json<SolicitudVerificacion>,
) -> AppResult<Json<ResultadoVerificacionPrueba>> {
    Ok(Json(reserva::verificar_prueba(&e.pool, &s).await?))
}

async fn auditar_ifpe(State(e): State<AppState>, u: Usuario) -> AppResult<Json<ResultadoVerificacion>> {
    u.exigir_operador_o_auditor()?;
    Ok(Json(hash::verificar_cadena_ifpe(&e.pool).await?))
}

async fn auditar_sofom(State(e): State<AppState>, u: Usuario) -> AppResult<Json<ResultadoVerificacion>> {
    u.exigir_operador_o_auditor()?;
    Ok(Json(hash::verificar_cadena_sofom(&e.pool).await?))
}
