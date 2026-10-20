//! Rutas SPEI.
//!
//! | Método | Ruta | Acceso |
//! |---|---|---|
//! | POST | `/spei/webhook/inbound` | proveedor (firma HMAC): SPEI recibido |
//! | POST | `/spei/webhook/estado` | proveedor (firma HMAC): cambio de estado de una orden |
//! | POST | `/spei/transfer` | cliente: enviar un SPEI |
//! | GET  | `/spei/transferencias` | cliente: sus órdenes |
//! | GET  | `/spei/recibidos` | cliente: sus SPEI recibidos |
//! | GET  | `/spei/conciliacion` | operador / auditor |
//! | GET  | `/spei/en-revision` | operador / auditor: notificaciones retenidas sin resolver |

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};

use super::status_de;
use crate::{
    auth::Usuario,
    error::{AppError, AppResult},
    routes::extraccion::{Cuerpo, Json, Query},
    spei::{
        firma,
        inbound::{self, Origen, ResultadoAbono, Retenida, SiNoAcreditable, SpeiInbound},
        outbound::{self, Conciliacion, Evento, ResultadoOrden, SolicitudSpei, SpeiOutbound},
        CausaDevolucion,
    },
    state::AppState,
};

pub fn rutas() -> Router<AppState> {
    Router::new()
        .route("/spei/webhook/inbound", post(webhook_inbound))
        .route("/spei/webhook/estado", post(webhook_estado))
        .route("/spei/transfer", post(transferir))
        .route("/spei/transferencias", get(transferencias))
        .route("/spei/recibidos", get(recibidos))
        .route("/spei/conciliacion", get(conciliacion))
        .route("/spei/en-revision", get(en_revision))
}

/// Autentica la notificación del proveedor antes de leerla.
fn verificar_firma(e: &AppState, cabeceras: &HeaderMap, cuerpo: &[u8]) -> AppResult<()> {
    let secretos: Vec<&[u8]> = e.config.spei.webhook_secretos.iter().map(|s| s.as_bytes()).collect();
    if secretos.is_empty() {
        return Err(AppError::peticion(
            StatusCode::SERVICE_UNAVAILABLE,
            "SPEI_NO_CONFIGURADO",
            "los webhooks SPEI no tienen secreto configurado",
        ));
    }
    let cabecera = cabeceras.get(firma::CABECERA).and_then(|v| v.to_str().ok());
    firma::verificar(&secretos, cabecera, cuerpo, chrono::Utc::now().timestamp(), e.config.spei.tolerancia_firma_seg)
        .map_err(|err| {
            tracing::warn!(error = %err, "webhook SPEI rechazado por firma");
            AppError::peticion(StatusCode::UNAUTHORIZED, "FIRMA_INVALIDA", err.to_string())
        })
}

/// SPEI recibido. Responde `{"mensaje":"confirmar"}` si se abonó (también
/// ante una repetición) o `{"mensaje":"devolver","id":<causa>}` si la cuenta
/// no puede recibir. Un error transitorio responde 503 para que el proveedor
/// reintente. Una notificación autenticada nunca recibe un 409 sin dejar
/// registro:
/// - una devolución (de cualquier tipo) que no corresponde a ninguna orden (o
///   cuyo reverso es imposible porque la cuenta está cerrada: la orden queda
///   en revisión manual) va a cuarentena y se confirma (`CUARENTENA`): SPEI no
///   permite devolver una devolución;
/// - un posible duplicado con otra identidad o una identidad ya registrada
///   con otro monto o CLABE se retiene sin abonar y se confirma
///   (`EN_REVISION`): devolverlo dejaría la reserva corta si el pago ya se
///   abonó con la identidad equivocada. También una devolución repetida que
///   dice devolver otra orden. El operador lo resuelve (captura con
///   `distinto_de`, anulación de la captura errónea o descarte como
///   duplicado).
///
/// Si no se puede guardar el registro, el error se propaga y el proveedor
/// reintenta: nunca se confirma sin registro.
async fn webhook_inbound(
    State(e): State<AppState>,
    cabeceras: HeaderMap,
    Cuerpo(cuerpo): Cuerpo,
) -> AppResult<Json<Value>> {
    verificar_firma(&e, &cabeceras, &cuerpo)?;
    let abono = match inbound::interpretar_notificacion(&cuerpo) {
        Ok(a) => a,
        // Autenticada pero inutilizable (sin identidad válida, monto con más de
        // 2 decimales...): se guarda para revisión y se pide devolverla, en vez
        // de dejar el dinero en el custodio sin dueño.
        Err(err) => {
            inbound::cuarentena(&e.pool, "ABONO", &cuerpo, &err.to_string()).await?;
            return Ok(Json(json!({
                "mensaje": "devolver",
                "id": CausaDevolucion::FaltaInformacion.codigo(),
                "estado": "CUARENTENA",
            })));
        }
    };
    let r: ResultadoAbono =
        match inbound::abonar(&e.pool, &e.config.pld, &e.config.spei, &abono, Origen::Webhook, SiNoAcreditable::Devolver)
            .await
        {
            Ok(r) => r,
            Err(err) if err.codigo() == inbound::DEVOLUCION_SIN_ORDEN => {
                inbound::cuarentena(&e.pool, "ABONO", &cuerpo, &format!("{}: {err}", err.codigo())).await?;
                return Ok(Json(json!({
                    "mensaje": "confirmar",
                    "estado": "CUARENTENA",
                    "identidad": abono.identidad.canonica(),
                })));
            }
            Err(err) if err.codigo() == inbound::POSIBLE_DUPLICADO || err.codigo() == inbound::IDENTIDAD_REUTILIZADA => {
                let tipo = err.codigo().trim_start_matches("SPEI_").to_owned();
                inbound::retener(&e.pool, &tipo, &abono, &cuerpo, &format!("{}: {err}", err.codigo())).await?;
                return Ok(Json(json!({
                    "mensaje": "confirmar",
                    "estado": "EN_REVISION",
                    "motivo": tipo,
                    "identidad": abono.identidad.canonica(),
                })));
            }
            Err(err) => return Err(err),
        };
    Ok(Json(r.respuesta_proveedor()))
}

/// Cambio de estado de una orden enviada (`LIQUIDADA`/`Exito`, `CANCELADA`,
/// `DEVUELTA`). Se identifica por el id del proveedor o por la clave de
/// rastreo (y fecha de operación).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificacionEstado {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    clave_rastreo: Option<String>,
    #[serde(default)]
    fecha_operacion: Option<Value>,
    estado: String,
    #[serde(default)]
    causa_devolucion: Option<i16>,
}

async fn webhook_estado(
    State(e): State<AppState>,
    cabeceras: HeaderMap,
    Cuerpo(cuerpo): Cuerpo,
) -> AppResult<Json<Value>> {
    verificar_firma(&e, &cabeceras, &cuerpo)?;
    let n: NotificacionEstado = match serde_json::from_slice(&cuerpo) {
        Ok(n) => n,
        Err(err) => {
            inbound::cuarentena(&e.pool, "ESTADO", &cuerpo, &err.to_string()).await?;
            return Err(AppError::validacion(format!("notificación de estado ilegible: {err}")));
        }
    };
    let fecha = match &n.fecha_operacion {
        None => None,
        Some(v) => {
            let texto = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return Err(AppError::validacion("fechaOperacion inválida")),
            };
            chrono::NaiveDate::parse_from_str(&texto, "%Y%m%d")
                .or_else(|_| chrono::NaiveDate::parse_from_str(&texto, "%Y-%m-%d"))
                .ok()
        }
    };
    let orden: SpeiOutbound = match (&n.clave_rastreo, &n.id) {
        (Some(rastreo), _) => outbound::buscar_por_rastreo(&e.pool, rastreo).await?,
        (None, Some(id)) => {
            let id = match id {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return Err(AppError::validacion("id inválido")),
            };
            outbound::buscar_por_id_proveedor(&e.pool, &id).await?
        }
        (None, None) => return Err(AppError::validacion("la notificación debe traer claveRastreo o id")),
    };
    let causa = n.causa_devolucion.unwrap_or(0);
    let evento = match n.estado.trim().to_ascii_uppercase().as_str() {
        "LIQUIDADA" | "LIQUIDADO" | "EXITO" | "ÉXITO" | "LQ" => Evento::Liquidada { fecha_operacion: fecha },
        "CANCELADA" | "CANCELADO" | "RECHAZADA" | "CN" => Evento::Rechazada {
            causa: if causa > 0 { causa } else { 99 },
            motivo: format!("{} por el proveedor", n.estado),
        },
        "DEVUELTA" | "DEVUELTO" | "D" => Evento::Devuelta {
            causa: if causa > 0 { causa } else { 99 },
            motivo: "devuelta por el banco receptor".into(),
        },
        otro => return Err(AppError::validacion(format!("estado SPEI desconocido: {otro}"))),
    };
    let t = outbound::aplicar(&e.pool, &e.config.spei, orden.id, &evento).await?;
    Ok(Json(json!({ "mensaje": "recibido", "estado": t.orden.estado, "cambio": t.cambio })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolicitudTransferencia {
    clabe_destino: String,
    beneficiario_nombre: String,
    #[serde(default)]
    beneficiario_rfc_curp: Option<String>,
    #[serde(with = "rust_decimal::serde::str")]
    monto: Decimal,
    #[serde(default)]
    concepto: Option<String>,
    #[serde(default)]
    referencia_numerica: Option<i32>,
    clave_idempotencia: String,
}

/// Envía un SPEI: el saldo y la reserva bajan al instante y la orden queda
/// PENDIENTE para el despachador. 201 si es nueva, 200 si es repetición.
async fn transferir(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudTransferencia>,
) -> AppResult<(StatusCode, Json<ResultadoOrden>)> {
    u.exigir_cliente()?;
    let solicitud = SolicitudSpei {
        cliente_id: u.cliente_id,
        clabe_destino: s.clabe_destino,
        beneficiario_nombre: Some(s.beneficiario_nombre),
        beneficiario_rfc_curp: s.beneficiario_rfc_curp,
        monto: s.monto,
        concepto: s.concepto,
        referencia_numerica: s.referencia_numerica,
        clave_idempotencia: u.clave(&s.clave_idempotencia)?,
    };
    let r = outbound::solicitar(&e.pool, &e.config.pld, &e.config.spei, &solicitud).await?;
    Ok((status_de(r.idempotente), Json(r)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limite {
    limite: Option<i64>,
}

async fn transferencias(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<Limite>,
) -> AppResult<Json<Vec<SpeiOutbound>>> {
    u.exigir_cliente()?;
    Ok(Json(outbound::de_cliente(&e.pool, u.cliente_id, q.limite.unwrap_or(50)).await?))
}

async fn recibidos(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<Limite>,
) -> AppResult<Json<Vec<SpeiInbound>>> {
    u.exigir_cliente()?;
    Ok(Json(inbound::de_cliente(&e.pool, u.cliente_id, q.limite.unwrap_or(50)).await?))
}

async fn conciliacion(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Vec<Conciliacion>>> {
    u.exigir_operador_o_auditor()?;
    Ok(Json(outbound::conciliacion(&e.pool).await?))
}

/// Cola de revisión: notificaciones autenticadas que se confirmaron al
/// proveedor y se retuvieron sin abonar (posible duplicado, identidad
/// reutilizada) y que aún no resuelve una captura del operador.
async fn en_revision(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Vec<Retenida>>> {
    u.exigir_operador_o_auditor()?;
    Ok(Json(inbound::en_revision(&e.pool).await?))
}
