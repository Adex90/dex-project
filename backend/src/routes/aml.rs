//! Consola de cumplimiento PLD/FT (`/api/v1/aml`).
//!
//! | Método | Ruta | Acceso |
//! |---|---|---|
//! | GET  | `/aml/resumen` | cumplimiento / auditor: contadores de la barra de resumen |
//! | GET  | `/aml/alertas` | cumplimiento / auditor: bandeja con filtros, cursor y sondeo de cambios |
//! | GET  | `/aml/alertas/{id}` | cumplimiento / auditor: expediente (deja `CONSULTA_EXPEDIENTE` en la bitácora) |
//! | POST | `/aml/alertas/{id}/resolver` | cumplimiento: TOMAR, LIBERAR, ESCALAR, CONFIRMAR, FALSO_POSITIVO |
//! | POST | `/aml/alertas/{id}/reporte-uif` | cumplimiento: folio SITI de una alerta confirmada (una vez) |
//! | GET  | `/aml/clientes/{id}` | cumplimiento / auditor: revisión del cliente (deja `CONSULTA_EXPEDIENTE`) |
//! | POST | `/aml/clientes/{id}/congelar` | cumplimiento: congelamiento manual |
//! | GET  | `/aml/congelamientos` | cumplimiento / auditor: congelamientos (ACTIVO por omisión) |
//! | POST | `/aml/congelamientos/{id}/solicitudes` | cumplimiento: solicitud de levantamiento o liberación (cuatro ojos) |
//! | GET  | `/aml/solicitudes` | cumplimiento / auditor: cola de cuatro ojos (PENDIENTE por omisión) |
//! | POST | `/aml/solicitudes/{id}/decision` | cumplimiento, distinto del solicitante: aprueba o rechaza |
//! | GET  | `/aml/notificaciones` | cumplimiento / auditor: bandeja de salida después de un id |
//! | GET  | `/aml/reglas` | cumplimiento / auditor: parámetros, reglas y propuestas pendientes |
//! | GET  | `/aml/propuestas` | cumplimiento / auditor: propuestas de cambio (PENDIENTE por omisión) |
//! | POST | `/aml/propuestas` | cumplimiento: propone un cambio de regla o de parámetros |
//! | POST | `/aml/propuestas/{id}/decision` | cumplimiento, distinto del proponente: aprueba o rechaza |
//! | GET  | `/aml/relevantes` | cumplimiento / auditor: registro trimestral de operaciones relevantes |
//! | GET  | `/aml/bitacora/verificacion` | cumplimiento / auditor: recalcula hashes y sellos de la bitácora |
//!
//! Toda ruta exige una sesión con segundo factor (TOTP) verificado.
//!
//! CLIENTE y OPERADOR reciben 403 en toda ruta y AUDITOR en todo POST. El rol
//! se comprueba en una capa de las rutas ([`exigir_rol`]) **antes** de leer la
//! ruta, la query o el cuerpo: quien no es de cumplimiento recibe 403 aunque la
//! petición esté mal formada, así que no puede explorar el esquema de las
//! peticiones ni las validaciones de la consola. Cada handler lo repite y la
//! base de datos también (`aml.exigir_lector`, `aml.exigir_oficial`,
//! `SC_AML_NO_AUTORIZADO`). Todo POST lleva
//! `clave_idempotencia`: 201 si la acción la realizó esta petición, 200 si es
//! una repetición idempotente (el cuerpo siempre trae `idempotente`).

use axum::{
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::status_de;
use crate::{
    aml::{
        consultas::{self, Asignacion, FiltroAlertas},
        oficial, SolicitudCongelamiento, SolicitudDecision, SolicitudLevantamiento, SolicitudPropuesta,
        SolicitudReporteUif, SolicitudResolucion,
    },
    auth::Usuario,
    error::{AppError, AppResult},
    routes::extraccion::{Json, Path, Query},
    state::AppState,
};

pub fn rutas(estado: AppState) -> Router<AppState> {
    Router::new()
        .route("/aml/resumen", get(resumen))
        .route("/aml/alertas", get(alertas))
        .route("/aml/alertas/{id}", get(alerta))
        .route("/aml/alertas/{id}/resolver", post(resolver_alerta))
        .route("/aml/alertas/{id}/reporte-uif", post(reporte_uif))
        .route("/aml/clientes/{id}", get(cliente))
        .route("/aml/clientes/{id}/congelar", post(congelar))
        .route("/aml/congelamientos", get(congelamientos))
        .route("/aml/congelamientos/{id}/solicitudes", post(solicitar_levantamiento))
        .route("/aml/solicitudes", get(solicitudes))
        .route("/aml/solicitudes/{id}/decision", post(decidir_levantamiento))
        .route("/aml/notificaciones", get(notificaciones))
        .route("/aml/reglas", get(reglas))
        .route("/aml/propuestas", get(propuestas).post(proponer_cambio))
        .route("/aml/propuestas/{id}/decision", post(decidir_propuesta))
        .route("/aml/relevantes", get(relevantes))
        .route("/aml/bitacora/verificacion", get(verificar_bitacora))
        .route_layer(middleware::from_fn_with_state(estado, exigir_rol))
}

/// Capa de las rutas de la consola: autentica y exige el rol antes que
/// cualquier extractor del handler (ruta, query, cuerpo). GET: cumplimiento o
/// auditor; cualquier otro método: cumplimiento. Además, la sesión debe haber
/// verificado el segundo factor (`MFA_REQUERIDO`): el extractor ya rechaza al
/// personal sin él, y esta comprobación lo garantiza en la frontera de la
/// consola aunque esa política cambiara. El `Usuario` validado viaja en las
/// extensiones de la petición, así que el handler no vuelve a consultar la
/// base de datos para extraerlo.
async fn exigir_rol(u: Usuario, mut req: Request, next: Next) -> Result<Response, AppError> {
    if req.method() == Method::GET || req.method() == Method::HEAD {
        u.exigir_cumplimiento_o_auditor()?;
    } else {
        u.exigir_cumplimiento()?;
    }
    u.exigir_mfa()?;
    req.extensions_mut().insert(u);
    Ok(next.run(req).await)
}

/// Respuesta de una acción de oficial: 201 si la realizó esta petición, 200
/// si es una repetición idempotente.
fn respuesta(r: Value) -> (StatusCode, Json<Value>) {
    (status_de(oficial::es_idempotente(&r)), Json(r))
}

async fn resumen(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    Ok(Json(consultas::resumen(&e.pool, &u).await?))
}

const ESTADOS: [&str; 4] = ["PENDIENTE", "EN_REVISION", "CONFIRMADO", "FALSO_POSITIVO"];
const NIVELES: [&str; 4] = ["BAJO", "MEDIO", "ALTO", "CRITICO"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaAlertas {
    estado: Option<String>,
    nivel: Option<String>,
    regla: Option<String>,
    cliente_id: Option<Uuid>,
    asignada: Option<String>,
    antes_de_folio: Option<i64>,
    actualizadas_desde: Option<DateTime<Utc>>,
    limite: Option<i64>,
}

/// Lista separada por comas de valores de un catálogo (`TODOS` = sin filtro).
fn lista(campo: &str, valor: Option<&str>, catalogo: &[&str], defecto: &[&str]) -> AppResult<Vec<String>> {
    let Some(valor) = valor else {
        return Ok(defecto.iter().map(|v| (*v).to_owned()).collect());
    };
    if valor == "TODOS" {
        return Ok(Vec::new());
    }
    valor
        .split(',')
        .map(|v| {
            let v = v.trim();
            if catalogo.contains(&v) {
                Ok(v.to_owned())
            } else {
                Err(AppError::validacion(format!("{campo}: valor desconocido {v:?}")))
            }
        })
        .collect()
}

impl ConsultaAlertas {
    fn validar(self) -> AppResult<FiltroAlertas> {
        let sondeo = self.actualizadas_desde.is_some();
        if sondeo && self.antes_de_folio.is_some() {
            return Err(AppError::validacion("antes_de_folio no se combina con actualizadas_desde"));
        }
        let limite = self.limite.unwrap_or(50);
        if !(1..=200).contains(&limite) {
            return Err(AppError::validacion("limite debe estar entre 1 y 200"));
        }
        let asignada = match self.asignada.as_deref() {
            None => None,
            Some("yo") => Some(Asignacion::Yo),
            Some("nadie") => Some(Asignacion::Nadie),
            Some(_) => return Err(AppError::validacion("asignada: yo | nadie")),
        };
        if let Some(r) = &self.regla {
            if r.is_empty() || r.len() > 40 || !r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(AppError::validacion("regla: código de regla inválido"));
            }
        }
        // Página: abiertas por defecto. Sondeo: todo cambio (también los cierres).
        let estados_defecto: &[&str] = if sondeo { &[] } else { &["PENDIENTE", "EN_REVISION"] };
        Ok(FiltroAlertas {
            estados: lista("estado", self.estado.as_deref(), &ESTADOS, estados_defecto)?,
            niveles: lista("nivel", self.nivel.as_deref(), &NIVELES, &[])?,
            regla: self.regla,
            cliente_id: self.cliente_id,
            asignada,
            actualizadas_desde: self.actualizadas_desde,
            antes_de_folio: self.antes_de_folio,
            limite,
        })
    }
}

async fn alertas(State(e): State<AppState>, u: Usuario, Query(q): Query<ConsultaAlertas>) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    let filtro = q.validar()?;
    Ok(Json(consultas::alertas(&e.pool, &u, &filtro).await?))
}

async fn alerta(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    Ok(Json(consultas::alerta(&e.pool, &u, id).await?))
}

async fn cliente(State(e): State<AppState>, u: Usuario, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    Ok(Json(consultas::cliente(&e.pool, &u, id).await?))
}

/// `limite` dentro de `[1, max]`, `defecto` si falta.
fn limite(valor: Option<i64>, defecto: i64, max: i64) -> AppResult<i64> {
    let l = valor.unwrap_or(defecto);
    if (1..=max).contains(&l) {
        Ok(l)
    } else {
        Err(AppError::validacion(format!("limite debe estar entre 1 y {max}")))
    }
}

/// Filtro de estado de un catálogo: `defecto` si falta, `None` con `TODOS`.
fn estado<'a>(valor: Option<&'a str>, catalogo: &[&'a str], defecto: &'a str) -> AppResult<Option<&'a str>> {
    match valor.unwrap_or(defecto) {
        "TODOS" => Ok(None),
        v if catalogo.contains(&v) => Ok(Some(v)),
        v => Err(AppError::validacion(format!("estado: valor desconocido {v:?}"))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaCongelamientos {
    estado: Option<String>,
    cliente_id: Option<Uuid>,
    antes_de_folio: Option<i64>,
    limite: Option<i64>,
}

async fn congelamientos(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaCongelamientos>,
) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    let estado = estado(q.estado.as_deref(), &["ACTIVO", "LEVANTADO"], "ACTIVO")?;
    let limite = limite(q.limite, 50, 200)?;
    Ok(Json(consultas::congelamientos(&e.pool, &u, estado, q.cliente_id, q.antes_de_folio, limite).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaEstado {
    estado: Option<String>,
    limite: Option<i64>,
}

async fn solicitudes(State(e): State<AppState>, u: Usuario, Query(q): Query<ConsultaEstado>) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    let estado = estado(q.estado.as_deref(), &["PENDIENTE", "APROBADA", "RECHAZADA", "ANULADA"], "PENDIENTE")?;
    let limite = limite(q.limite, 50, 200)?;
    Ok(Json(consultas::solicitudes(&e.pool, &u, estado, limite).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaNotificaciones {
    despues_de: Option<i64>,
    limite: Option<i64>,
}

async fn notificaciones(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaNotificaciones>,
) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    if q.despues_de.is_some_and(|d| d < 0) {
        return Err(AppError::validacion("despues_de no puede ser negativo"));
    }
    let limite = limite(q.limite, 50, 200)?;
    Ok(Json(consultas::notificaciones(&e.pool, &u, q.despues_de, limite).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaReglas {
    incluir_inactivas: Option<bool>,
}

async fn reglas(State(e): State<AppState>, u: Usuario, Query(q): Query<ConsultaReglas>) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    Ok(Json(consultas::reglas(&e.pool, &u, q.incluir_inactivas.unwrap_or(false)).await?))
}

async fn propuestas(State(e): State<AppState>, u: Usuario, Query(q): Query<ConsultaEstado>) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    let estado = estado(q.estado.as_deref(), &["PENDIENTE", "APROBADA", "RECHAZADA"], "PENDIENTE")?;
    let limite = limite(q.limite, 50, 200)?;
    Ok(Json(consultas::propuestas(&e.pool, &u, estado, limite).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsultaRelevantes {
    trimestre: Option<String>,
    antes_de: Option<DateTime<Utc>>,
    limite: Option<i64>,
}

/// `AAAA-T1` a `AAAA-T4`.
fn es_trimestre(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 7 && b[..4].iter().all(u8::is_ascii_digit) && &b[4..6] == b"-T" && (b'1'..=b'4').contains(&b[6])
}

async fn relevantes(
    State(e): State<AppState>,
    u: Usuario,
    Query(q): Query<ConsultaRelevantes>,
) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    if q.trimestre.as_deref().is_some_and(|t| !es_trimestre(t)) {
        return Err(AppError::validacion("trimestre: AAAA-T1 a AAAA-T4"));
    }
    let limite = limite(q.limite, 100, 500)?;
    Ok(Json(consultas::relevantes(&e.pool, &u, q.trimestre.as_deref(), q.antes_de, limite).await?))
}

async fn verificar_bitacora(State(e): State<AppState>, u: Usuario) -> AppResult<Json<Value>> {
    u.exigir_cumplimiento_o_auditor()?;
    Ok(Json(consultas::verificar_bitacora(&e.pool, &u).await?))
}

// ---- acciones de los oficiales (solo CUMPLIMIENTO) ---------------------------

async fn resolver_alerta(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudResolucion>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::resolver_alerta(&e.pool, &u, id, &s).await?))
}

async fn reporte_uif(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudReporteUif>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::registrar_reporte_uif(&e.pool, &u, id, &s).await?))
}

async fn congelar(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudCongelamiento>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::congelar_manual(&e.pool, &u, id, &s).await?))
}

async fn solicitar_levantamiento(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudLevantamiento>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::solicitar_levantamiento(&e.pool, &u, id, &s).await?))
}

async fn decidir_levantamiento(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudDecision>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::decidir_levantamiento(&e.pool, &u, id, &s).await?))
}

async fn proponer_cambio(
    State(e): State<AppState>,
    u: Usuario,
    Json(s): Json<SolicitudPropuesta>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::proponer_cambio(&e.pool, &u, &s).await?))
}

async fn decidir_propuesta(
    State(e): State<AppState>,
    u: Usuario,
    Path(id): Path<Uuid>,
    Json(s): Json<SolicitudDecision>,
) -> AppResult<(StatusCode, Json<Value>)> {
    u.exigir_cumplimiento()?;
    Ok(respuesta(oficial::decidir_propuesta(&e.pool, &u, id, &s).await?))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn trimestre_valido() {
        assert!(es_trimestre("2026-T4"));
        assert!(!es_trimestre("2026-T5"));
        assert!(!es_trimestre("26-T1"));
        assert!(!es_trimestre("2026T1x"));
    }

    #[test]
    fn filtros_de_estado_y_limite() {
        assert_eq!(estado(None, &["ACTIVO", "LEVANTADO"], "ACTIVO").unwrap(), Some("ACTIVO"));
        assert_eq!(estado(Some("TODOS"), &["ACTIVO"], "ACTIVO").unwrap(), None);
        assert_eq!(estado(Some("activo"), &["ACTIVO"], "ACTIVO").unwrap_err().codigo(), "VALIDACION");
        assert_eq!(limite(None, 50, 200).unwrap(), 50);
        assert!(limite(Some(0), 50, 200).is_err());
        assert!(limite(Some(201), 50, 200).is_err());
    }
}
