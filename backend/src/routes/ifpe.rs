use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;

use super::status_de;
use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    ledger::{self, Movimiento},
    models::ifpe::{CuentaIfpe, TransaccionIfpe},
    routes::extraccion::{Json, Query},
    spei::{
        inbound::{self, ResultadoAbono},
        outbound::{self, ResultadoOrden, SolicitudSpei},
        IdentidadSpei,
    },
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
    #[serde(with = "rust_decimal::serde::str")]
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
    #[serde(with = "rust_decimal::serde::str")]
    monto: Decimal,
    clabe_destino: String,
    concepto: Option<String>,
    clave_idempotencia: String,
}

/// Retiro a una cuenta propia en otro banco: es un SPEI saliente con el nombre
/// del cliente como beneficiario (ver `spei::outbound`).
async fn retirar(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudRetiro>,
) -> AppResult<(StatusCode, Json<ResultadoOrden>)> {
    u.exigir_cliente()?;
    let solicitud = SolicitudSpei {
        cliente_id: u.cliente_id,
        clabe_destino: s.clabe_destino,
        beneficiario_nombre: None,
        beneficiario_rfc_curp: None,
        monto: s.monto,
        concepto: Some(s.concepto.unwrap_or_else(|| "RETIRO A CUENTA PROPIA".into())),
        referencia_numerica: None,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let r = outbound::solicitar(&e.pool, &e.config.pld, &e.config.spei, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudDepositoSimulado {
    #[serde(with = "rust_decimal::serde::str")]
    monto: Decimal,
    clave_idempotencia: String,
}

/// Solo en `MODO_DEMO`: simula un SPEI entrante a la cuenta del propio cliente.
/// Pasa por el mismo proceso que un SPEI real (registro en `spei_inbound` con
/// origen DEMO, reserva en custodia 1:1). La identidad del pago se deriva de la
/// clave de idempotencia, así que repetirla no abona dos veces.
async fn deposito_simulado(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudDepositoSimulado>,
) -> AppResult<(StatusCode, Json<ResultadoAbono>)> {
    u.exigir_cliente()?;
    if !e.config.modo_demo {
        return Err(AppError::no_encontrado("ruta no disponible"));
    }
    let cuenta = ledger::cuenta_de_cliente(&e.pool, u.cliente_id)
        .await?
        .ok_or_else(|| AppError::no_encontrado("no tienes cuenta en la bóveda IFPE"))?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let rastreo = format!("DEMO{}", &crate::ledger::hash::sha256_hex(clave.as_bytes())[..26]).to_ascii_uppercase();
    let abono = inbound::Abono {
        identidad: IdentidadSpei::nueva("99999", NaiveDate::from_ymd_opt(2000, 1, 1).expect("fecha"), &rastreo)?,
        institucion_receptora: None,
        receptor_clabe: cuenta.clabe.clone(),
        monto: s.monto,
        nombre_ordenante: Some("DEPOSITO SIMULADO".into()),
        cuenta_ordenante: None,
        rfc_curp_ordenante: None,
        referencia_numerica: None,
        concepto: Some("modo demo".into()),
        tipo_pago: None,
        rastreo_devuelto: None,
        causa_devolucion: None,
        id_proveedor: None,
        raw_payload: serde_json::json!({ "demo": true, "monto": s.monto.to_string() }),
    };
    let r = inbound::abonar(
        &e.pool,
        &e.config.pld,
        &e.config.spei,
        &abono,
        inbound::Origen::Demo,
        inbound::SiNoAcreditable::Rechazar,
    )
    .await?;
    Ok((status_de(r.idempotente), Json(r)))
}
