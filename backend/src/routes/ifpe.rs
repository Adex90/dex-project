use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use rust_decimal::Decimal;
use serde::Deserialize;

use super::status_de;
use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    ledger::{self, Movimiento},
    models::ifpe::{CuentaIfpe, TransaccionIfpe},
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/ifpe/cuenta", get(cuenta))
        .route("/ifpe/movimientos", get(movimientos))
        .route("/ifpe/transferencias", post(transferir))
        .route("/ifpe/retiros", post(retirar))
        .route("/ifpe/depositos/simulados", post(deposito_simulado))
}

async fn cuenta(State(e): State<AppState>, u: Usuario) -> AppResult<Json<CuentaIfpe>> {
    u.exigir_cliente()?;
    ledger::cuenta_de_cliente(&e.pool, u.cliente_id)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::no_encontrado("no tienes cuenta en la bóveda IFPE"))
}

#[derive(Deserialize)]
struct Paginacion {
    limite: Option<i64>,
    antes_de: Option<i64>,
}

async fn movimientos(
    State(e): State<AppState>,
    u: Usuario,
    Query(p): Query<Paginacion>,
) -> AppResult<Json<Vec<TransaccionIfpe>>> {
    u.exigir_cliente()?;
    Ok(Json(ledger::movimientos_de_cliente(&e.pool, u.cliente_id, p.limite.unwrap_or(50), p.antes_de).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudTransferencia {
    clabe_destino: String,
    monto: Decimal,
    concepto: Option<String>,
    clave_idempotencia: String,
}

async fn transferir(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudTransferencia>,
) -> AppResult<(StatusCode, Json<Movimiento>)> {
    u.exigir_cliente()?;
    let t = ledger::Transferencia {
        cliente_origen_id: u.cliente_id,
        clabe_destino: s.clabe_destino,
        monto: s.monto,
        concepto: s.concepto,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let m = ledger::transferir(&e.pool, &e.config.pld, &t).await?;
    Ok((status_de(m.idempotente), Json(m)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudRetiro {
    monto: Decimal,
    clabe_destino: String,
    concepto: Option<String>,
    clave_idempotencia: String,
}

async fn retirar(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudRetiro>,
) -> AppResult<(StatusCode, Json<Movimiento>)> {
    u.exigir_cliente()?;
    let r = ledger::Retiro {
        cliente_id: u.cliente_id,
        monto: s.monto,
        clabe_destino: s.clabe_destino,
        concepto: s.concepto,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let m = ledger::retirar(&e.pool, &e.config.pld, &r).await?;
    Ok((status_de(m.idempotente), Json(m)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudDepositoSimulado {
    monto: Decimal,
    clave_idempotencia: String,
}

/// Solo en `MODO_DEMO`: simula un SPEI entrante a la cuenta del propio cliente.
/// Mueve la reserva en custodia igual que un depósito real.
async fn deposito_simulado(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudDepositoSimulado>,
) -> AppResult<(StatusCode, Json<Movimiento>)> {
    u.exigir_cliente()?;
    if !e.config.modo_demo {
        return Err(AppError::no_encontrado("ruta no disponible"));
    }
    let cuenta = ledger::cuenta_de_cliente(&e.pool, u.cliente_id)
        .await?
        .ok_or_else(|| AppError::no_encontrado("no tienes cuenta en la bóveda IFPE"))?;
    let d = ledger::Deposito {
        cuenta_id: cuenta.id,
        monto: s.monto,
        referencia_externa: None,
        concepto: Some("Depósito simulado (modo demo)".into()),
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let m = ledger::depositar(&e.pool, &e.config.pld, &d).await?;
    Ok((status_de(m.idempotente), Json(m)))
}
