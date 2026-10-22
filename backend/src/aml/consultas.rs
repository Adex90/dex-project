//! Lectura de la consola de cumplimiento (`/api/v1/aml`, rol `sc_aml`).
//!
//! Todo cuerpo JSON lo construye la base de datos (`aml.*_json`, `aml.resumen`):
//! Rust solo agrega los campos de paginación. La primera sentencia de cada
//! transacción es `aml.exigir_lector(actor)` con el usuario del JWT, así que
//! incluso los listados que son un SELECT simple repiten en la base de datos
//! la comprobación de rol (`SC_AML_NO_AUTORIZADO` → 403).

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Usuario,
    db::{self, Rol, Tx},
    error::AppResult,
};

/// Filtro `asignada` de la bandeja.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asignacion {
    /// `asignada_a` = el actor.
    Yo,
    /// Sin asignar.
    Nadie,
}

/// Filtros de `GET /api/v1/aml/alertas` (ya validados por la ruta).
#[derive(Debug, Clone, Default)]
pub struct FiltroAlertas {
    /// Vacío = todos los estados.
    pub estados: Vec<String>,
    /// Vacío = todos los niveles.
    pub niveles: Vec<String>,
    pub regla: Option<String>,
    pub cliente_id: Option<Uuid>,
    pub asignada: Option<Asignacion>,
    /// Modo sondeo: `actualizado_en >= actualizadas_desde`, en orden de cambio.
    pub actualizadas_desde: Option<DateTime<Utc>>,
    /// Modo página: `folio < antes_de_folio`, folio descendente.
    pub antes_de_folio: Option<i64>,
    pub limite: i64,
}

/// Transacción de lectura de la consola: valida el rol del actor en la base
/// de datos antes de cualquier otra consulta (errata E5).
async fn abrir_lectura(pool: &PgPool, u: &Usuario) -> AppResult<Tx> {
    let mut tx = db::begin_lectura(pool, Rol::Aml).await?;
    sqlx::query("SELECT aml.exigir_lector($1)").bind(u.cliente_id).execute(&mut *tx).await?;
    Ok(tx)
}

/// Contadores de la barra de resumen (`aml.resumen()`).
pub async fn resumen(pool: &PgPool, u: &Usuario) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let mut tx = abrir_lectura(pool, u).await?;
    let r: Value = sqlx::query_scalar("SELECT aml.resumen()").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(r)
}

/// Bandeja de alertas: modo página (folio descendente, cursor
/// `antes_de_folio`) o modo sondeo (`actualizadas_desde`, en orden de cambio).
pub async fn alertas(pool: &PgPool, u: &Usuario, f: &FiltroAlertas) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let limite = f.limite.clamp(1, 200);
    let mut tx = abrir_lectura(pool, u).await?;
    // Mismo formato que las marcas de tiempo de los constructores JSON.
    let servidor_en: Value = sqlx::query_scalar("SELECT to_jsonb(clock_timestamp())").fetch_one(&mut *tx).await?;
    let asignada = f.asignada.map(|a| match a {
        Asignacion::Yo => "YO",
        Asignacion::Nadie => "NADIE",
    });
    let filas: Vec<(Value, i64)> = sqlx::query_as(
        "SELECT aml.alerta_json(a.id), a.folio FROM aml.alertas a \
          WHERE (cardinality($1::text[]) = 0 OR a.estado::text = ANY ($1)) \
            AND (cardinality($2::text[]) = 0 OR a.nivel_riesgo::text = ANY ($2)) \
            AND ($3::text IS NULL OR a.regla_codigo = $3) \
            AND ($4::uuid IS NULL OR a.cliente_id = $4) \
            AND CASE $5::text WHEN 'YO' THEN a.asignada_a IS NOT DISTINCT FROM $6 \
                              WHEN 'NADIE' THEN a.asignada_a IS NULL ELSE TRUE END \
            AND ($7::timestamptz IS NULL OR a.actualizado_en >= $7) \
            AND ($8::bigint IS NULL OR a.folio < $8) \
          ORDER BY CASE WHEN $7::timestamptz IS NULL THEN NULL ELSE a.actualizado_en END, \
                   CASE WHEN $7::timestamptz IS NULL THEN -a.folio ELSE a.folio END \
          LIMIT $9",
    )
    .bind(&f.estados)
    .bind(&f.niveles)
    .bind(&f.regla)
    .bind(f.cliente_id)
    .bind(asignada)
    .bind(u.cliente_id)
    .bind(f.actualizadas_desde)
    .bind(f.antes_de_folio)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let pagina_llena = filas.len() as i64 == limite;
    let siguiente = match (f.actualizadas_desde, filas.last()) {
        (None, Some((_, folio))) if pagina_llena => Some(*folio),
        _ => None,
    };
    let alertas: Vec<Value> = filas.into_iter().map(|(j, _)| j).collect();
    Ok(json!({ "alertas": alertas, "siguiente_antes_de_folio": siguiente, "servidor_en": servidor_en }))
}

/// Expediente de una alerta (`aml.consultar_alerta`). Transacción de
/// escritura: la consulta deja el renglón `CONSULTA_EXPEDIENTE` en la bitácora
/// (quién vio qué expediente y cuándo).
pub async fn alerta(pool: &PgPool, u: &Usuario, alerta_id: Uuid) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let r: Value = sqlx::query_scalar("SELECT aml.consultar_alerta($1, $2)")
        .bind(u.cliente_id)
        .bind(alerta_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(r)
}

/// Revisión de un cliente (`aml.consultar_cliente`): KYC, cuentas, agregados
/// de 24 h / 7 d / 30 d, movimientos, órdenes, mutuos, créditos, alertas y
/// congelamientos. También deja `CONSULTA_EXPEDIENTE` en la bitácora.
pub async fn cliente(pool: &PgPool, u: &Usuario, cliente_id: Uuid) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let r: Value = sqlx::query_scalar("SELECT aml.consultar_cliente($1, $2)")
        .bind(u.cliente_id)
        .bind(cliente_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(r)
}

/// Congelamientos, folio descendente. `estado`: `ACTIVO`, `LEVANTADO` o
/// `None` (todos); cursor `antes_de_folio`.
pub async fn congelamientos(
    pool: &PgPool,
    u: &Usuario,
    estado: Option<&str>,
    cliente_id: Option<Uuid>,
    antes_de_folio: Option<i64>,
    limite: i64,
) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let limite = limite.clamp(1, 200);
    let mut tx = abrir_lectura(pool, u).await?;
    let filas: Vec<(Value, i64)> = sqlx::query_as(
        "SELECT aml.congelamiento_json(f.id), f.folio FROM aml.congelamientos f \
          WHERE ($1::text IS NULL OR f.estado = $1) \
            AND ($2::uuid IS NULL OR f.cliente_id = $2) \
            AND ($3::bigint IS NULL OR f.folio < $3) \
          ORDER BY f.folio DESC LIMIT $4",
    )
    .bind(estado)
    .bind(cliente_id)
    .bind(antes_de_folio)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let siguiente = match filas.last() {
        Some((_, folio)) if filas.len() as i64 == limite => Some(*folio),
        _ => None,
    };
    let congelamientos: Vec<Value> = filas.into_iter().map(|(j, _)| j).collect();
    Ok(json!({ "congelamientos": congelamientos, "siguiente_antes_de_folio": siguiente }))
}

/// Cola de cuatro ojos. Las PENDIENTE van de la más antigua a la más reciente
/// (se atienden en orden); las demás, por fecha de decisión descendente.
pub async fn solicitudes(pool: &PgPool, u: &Usuario, estado: Option<&str>, limite: i64) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let limite = limite.clamp(1, 200);
    let mut tx = abrir_lectura(pool, u).await?;
    let filas: Vec<Value> = sqlx::query_scalar(
        "SELECT aml.solicitud_json(s.id) FROM aml.solicitudes_levantamiento s \
          WHERE ($1::text IS NULL OR s.estado = $1) \
          ORDER BY CASE WHEN $1::text = 'PENDIENTE' THEN s.solicitada_en END ASC, \
                   s.decidida_en DESC NULLS FIRST, s.folio DESC \
          LIMIT $2",
    )
    .bind(estado)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({ "solicitudes": filas }))
}

/// Bandeja de salida de notificaciones después de un id (exclusivo), en orden
/// de id. Solo códigos y folios: ninguna notificación lleva montos.
pub async fn notificaciones(pool: &PgPool, u: &Usuario, despues_de: Option<i64>, limite: i64) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let limite = limite.clamp(1, 200);
    let mut tx = abrir_lectura(pool, u).await?;
    let filas: Vec<(Value, i64)> = sqlx::query_as(
        "SELECT jsonb_build_object( \
                    'id', n.id, 'creado_en', n.creado_en, 'tipo', n.tipo, 'prioridad', n.prioridad, \
                    'cliente_id', n.cliente_id, 'alerta_id', n.alerta_id, 'congelamiento_id', n.congelamiento_id, \
                    'solicitud_id', n.solicitud_id, 'propuesta_id', n.propuesta_id, 'resumen', n.resumen, \
                    'entregada_en', n.entregada_en), n.id \
           FROM aml.notificaciones n WHERE n.id > $1 ORDER BY n.id LIMIT $2",
    )
    .bind(despues_de.unwrap_or(0))
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let ultimo = filas.last().map(|(_, id)| *id);
    let notificaciones: Vec<Value> = filas.into_iter().map(|(j, _)| j).collect();
    Ok(json!({ "notificaciones": notificaciones, "ultimo_id": ultimo }))
}

/// Parámetros vigentes, reglas (solo las activas salvo `incluir_inactivas`) y
/// propuestas pendientes.
pub async fn reglas(pool: &PgPool, u: &Usuario, incluir_inactivas: bool) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let mut tx = abrir_lectura(pool, u).await?;
    let parametros: Value = sqlx::query_scalar("SELECT aml.parametros_json()").fetch_one(&mut *tx).await?;
    let reglas: Vec<Value> = sqlx::query_scalar(
        "SELECT aml.regla_json(r.id) FROM aml.reglas_riesgo r WHERE $1 OR r.activa \
          ORDER BY r.codigo, r.nivel_cliente, r.version DESC",
    )
    .bind(incluir_inactivas)
    .fetch_all(&mut *tx)
    .await?;
    let propuestas: Vec<Value> = sqlx::query_scalar(
        "SELECT aml.propuesta_json(p.id) FROM aml.propuestas_cambio p WHERE p.estado = 'PENDIENTE' ORDER BY p.folio DESC",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({ "parametros": parametros, "reglas": reglas, "propuestas_pendientes": propuestas }))
}

/// Propuestas de cambio, folio descendente (`estado` `None` = todas).
pub async fn propuestas(pool: &PgPool, u: &Usuario, estado: Option<&str>, limite: i64) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let limite = limite.clamp(1, 200);
    let mut tx = abrir_lectura(pool, u).await?;
    let filas: Vec<Value> = sqlx::query_scalar(
        "SELECT aml.propuesta_json(p.id) FROM aml.propuestas_cambio p \
          WHERE ($1::text IS NULL OR p.estado = $1) ORDER BY p.folio DESC LIMIT $2",
    )
    .bind(estado)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({ "propuestas": filas }))
}

/// Registro trimestral de operaciones relevantes, `creado_en` descendente,
/// con cursor `antes_de` (exclusivo).
pub async fn relevantes(
    pool: &PgPool,
    u: &Usuario,
    trimestre: Option<&str>,
    antes_de: Option<DateTime<Utc>>,
    limite: i64,
) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let limite = limite.clamp(1, 500);
    let mut tx = abrir_lectura(pool, u).await?;
    let filas: Vec<(Value, Value)> = sqlx::query_as(
        "SELECT jsonb_build_object( \
                    'id', r.id, 'cliente', aml.persona_json(r.cliente_id), 'entidad', r.entidad, \
                    'transaccion_id', r.transaccion_id, 'sentido', r.sentido, 'canal', r.canal, \
                    'monto', r.monto::text, 'trimestre', r.trimestre, 'creado_en', r.creado_en, \
                    'reporte_folio', r.reporte_folio, 'reportada_en', to_char(r.reportada_en, 'YYYY-MM-DD')), \
                to_jsonb(r.creado_en) \
           FROM aml.operaciones_relevantes r \
          WHERE ($1::text IS NULL OR r.trimestre = $1) AND ($2::timestamptz IS NULL OR r.creado_en < $2) \
          ORDER BY r.creado_en DESC, r.id DESC LIMIT $3",
    )
    .bind(trimestre)
    .bind(antes_de)
    .bind(limite)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let siguiente = match filas.last() {
        Some((_, creado_en)) if filas.len() as i64 == limite => creado_en.clone(),
        _ => Value::Null,
    };
    let relevantes: Vec<Value> = filas.into_iter().map(|(j, _)| j).collect();
    Ok(json!({ "relevantes": relevantes, "siguiente_antes_de": siguiente }))
}

/// Recalcula el hash de cada renglón de la bitácora y la cadena de sellos.
pub async fn verificar_bitacora(pool: &PgPool, u: &Usuario) -> AppResult<Value> {
    u.exigir_cumplimiento_o_auditor()?;
    let mut tx = abrir_lectura(pool, u).await?;
    let (valido, filas, sellos, primer_error): (bool, i64, i64, Option<String>) =
        sqlx::query_as("SELECT valido, filas, sellos, primer_error FROM aml.verificar_bitacora()")
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(json!({ "valido": valido, "filas": filas, "sellos": sellos, "primer_error": primer_error }))
}
