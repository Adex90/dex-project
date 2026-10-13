use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};

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
        .route("/reserva/cortes", post(generar_corte))
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

async fn generar_corte(State(e): State<AppState>, u: Usuario) -> AppResult<(StatusCode, Json<CorteReserva>)> {
    u.exigir_operador_o_auditor()?;
    Ok((StatusCode::CREATED, Json(reserva::generar_corte(&e.pool).await?)))
}

/// Prueba de inclusión (MerkleProof) de la cuenta del cliente en el último corte.
async fn prueba(State(e): State<AppState>, u: Usuario) -> AppResult<Json<PruebaReserva>> {
    u.exigir_cliente()?;
    Ok(Json(reserva::prueba_de_cliente(&e.pool, u.cliente_id, e.config.corte_min_intervalo_seg).await?))
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
