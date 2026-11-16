//! Patrimonio: las cuentas vinculadas del cliente y los retiros a ellas
//! (`crate::patrimonio`). Solo el cliente; rutas del grupo `api` del límite
//! por IP (limites.rs), con los nombres en inglés del frontend
//! (`frontend/src/types/wealth.ts`).

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
    error::AppResult,
    patrimonio::{
        self,
        modelos::{CuentasVinculadas, ReciboRetiro},
        SolicitudRetiro,
    },
    routes::extraccion::Json,
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new().route("/wealth/linked-accounts", get(cuentas_vinculadas)).route("/wealth/withdraw", post(retirar))
}

/// Las cuentas vinculadas activas del cliente (`LinkedAccounts`): a dónde
/// puede retirar. Con `MODO_DEMO`, la primera consulta vincula la de
/// demostración.
async fn cuentas_vinculadas(State(e): State<AppState>, u: Usuario) -> AppResult<Json<CuentasVinculadas>> {
    u.exigir_cliente()?;
    Ok(Json(patrimonio::cuentas_vinculadas(&e.pool, &e.config, u.cliente_id).await?))
}

/// `WithdrawalRequest`. Campos desconocidos: 400.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudRetiroHttp {
    linked_account_id: Uuid,
    /// MXN con a lo más 2 decimales (SPEI liquida en centavos).
    #[serde(with = "rust_decimal::serde::str")]
    amount: Decimal,
    idempotency_key: String,
}

/// Retira efectivo disponible a una cuenta vinculada: 201 con el recibo
/// (`WithdrawalReceipt`, en proceso); 200 si repite una clave ya usada para
/// el mismo retiro, con su estado actual.
async fn retirar(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudRetiroHttp>,
) -> AppResult<(StatusCode, Json<ReciboRetiro>)> {
    u.exigir_cliente()?;
    let solicitud = SolicitudRetiro {
        cliente_id: u.cliente_id,
        cuenta_vinculada_id: s.linked_account_id,
        monto: s.amount,
        clave_idempotencia: u.clave(&s.idempotency_key)?,
    };
    let r = patrimonio::retirar(&e.pool, &e.config, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r.recibo)))
}
