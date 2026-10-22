//! Acciones de los oficiales de cumplimiento (`/api/v1/aml`, rol `sc_aml`).
//!
//! Cada acción es una sola función definer de la base de datos (§2.9), que
//! repite la comprobación de rol (`aml.exigir_oficial`), la idempotencia por
//! clave (`aml.acciones`, con huella de todos los parámetros), el orden de
//! candados, la autorresolución y los cuatro ojos. Rust:
//!
//! 1. exige el rol CUMPLIMIENTO y acota la clave a `cum:<actor>:<clave>`;
//! 2. normaliza y valida la solicitud antes de tocar la base de datos (los
//!    valores por omisión se aplican aquí, así que la huella cubre la
//!    solicitud normalizada);
//! 3. abre una transacción `sc_aml` simple (READ COMMITTED, **sin**
//!    savepoints: los triggers de protección verifican que la aprobación de
//!    cuatro ojos ocurrió en esta misma transacción comparando el `xmin` del
//!    renglón con `pg_current_xact_id()`);
//! 4. vuelve a calcular el hash de cada LIBERACION_RETENCION que crea la
//!    aprobación de un levantamiento antes de confirmar.

use serde_json::Value;
use sqlx::{postgres::PgArguments, query::QueryScalar, PgPool, Postgres};
use uuid::Uuid;

use crate::{
    auth::Usuario,
    db::{self, Rol},
    error::{AppError, AppResult},
    ledger::hash,
    models::ifpe::TransaccionIfpe,
};

use super::modelos::{
    AccionResolucion, SolicitudCongelamiento, SolicitudDecision, SolicitudLevantamiento, SolicitudPropuesta,
    SolicitudReporteUif, SolicitudResolucion, TipoPropuesta, TipoReporte, TipoSolicitud,
};

/// Mínimo de caracteres de un dictamen, motivo o justificación.
const DICTAMEN_MIN: usize = 30;
/// Mínimo de caracteres del comentario de una decisión de cuatro ojos.
const COMENTARIO_MIN: usize = 10;
const TEXTO_MAX: usize = 2000;
/// Tope de claves en `cambios` de una propuesta (la base de datos valida
/// nombres y tipos).
const CAMBIOS_MAX: usize = 20;

/// Texto narrativo recortado: admite saltos de línea y rechaza cualquier otro
/// carácter de control. Mismo código que la validación en plpgsql
/// (`SC_AML_DICTAMEN_INVALIDO`, 422), que lo repite.
fn narrativo(campo: &str, valor: &str, min: usize) -> AppResult<String> {
    let v = valor.trim();
    if v.chars().any(|c| c.is_control() && c != '\n') {
        return Err(AppError::negocio("AML_DICTAMEN_INVALIDO", format!("{campo} contiene caracteres de control")));
    }
    let n = v.chars().count();
    if !(min..=TEXTO_MAX).contains(&n) {
        return Err(AppError::negocio(
            "AML_DICTAMEN_INVALIDO",
            format!("{campo} debe tener entre {min} y {TEXTO_MAX} caracteres"),
        ));
    }
    Ok(v.to_owned())
}

/// Un campo que no aplica a la acción debe venir ausente o `null`.
fn ausente<T>(campo: &str, valor: &Option<T>, accion: AccionResolucion) -> AppResult<()> {
    match valor {
        None => Ok(()),
        Some(_) => Err(AppError::validacion(format!("{campo} no aplica a la acción {}", accion.as_str()))),
    }
}

/// `true` cuando la base de datos devolvió el resultado guardado de una
/// solicitud anterior con la misma clave.
pub fn es_idempotente(r: &Value) -> bool {
    r.get("idempotente").and_then(Value::as_bool).unwrap_or(false)
}

type Consulta<'q> = QueryScalar<'q, Postgres, Value, PgArguments>;

/// Ejecuta una función de oficial en su propia transacción `sc_aml` (con
/// reintentos ante contención; cada intento es una transacción nueva y la
/// clave de idempotencia hace seguro repetirlo).
async fn accion<'q, F>(pool: &PgPool, sql: &'q str, enlazar: F) -> AppResult<Value>
where
    F: Fn(Consulta<'q>) -> Consulta<'q>,
{
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Aml).await?;
        let r = enlazar(sqlx::query_scalar(sql)).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(r)
    })
    .await
}

/// Resolución normalizada de una alerta (valores por omisión aplicados).
struct Resolucion {
    accion: AccionResolucion,
    dictamen: Option<String>,
    tipo_reporte: Option<TipoReporte>,
    urgente: Option<bool>,
    congelar: Option<bool>,
    tipo_levantamiento: Option<TipoSolicitud>,
}

impl Resolucion {
    fn de(s: &SolicitudResolucion) -> AppResult<Self> {
        use AccionResolucion::*;
        let a = s.accion;
        let dictamen = match a {
            Tomar | Liberar => {
                ausente("dictamen", &s.dictamen, a)?;
                None
            }
            Escalar | Confirmar | FalsoPositivo => {
                let d = s.dictamen.as_deref().ok_or_else(|| {
                    AppError::negocio("AML_DICTAMEN_INVALIDO", format!("{} requiere un dictamen", a.as_str()))
                })?;
                Some(narrativo("el dictamen", d, DICTAMEN_MIN)?)
            }
        };
        if a != Confirmar {
            ausente("tipo_reporte", &s.tipo_reporte, a)?;
            ausente("urgente", &s.urgente, a)?;
        }
        if !matches!(a, Escalar | Confirmar) {
            ausente("congelar", &s.congelar, a)?;
        }
        if a != FalsoPositivo {
            ausente("tipo_levantamiento", &s.tipo_levantamiento, a)?;
        }
        let tipo_levantamiento = match (a, s.tipo_levantamiento) {
            (FalsoPositivo, Some(TipoSolicitud::LiberarRetenciones)) => {
                return Err(AppError::validacion(
                    "tipo_levantamiento: LEVANTAR_Y_LIBERAR | LEVANTAR_CONSERVANDO_RETENCIONES",
                ))
            }
            (FalsoPositivo, t) => Some(t.unwrap_or(TipoSolicitud::LevantarYLiberar)),
            _ => None,
        };
        Ok(Self {
            accion: a,
            dictamen,
            tipo_reporte: s.tipo_reporte,
            urgente: (a == Confirmar).then(|| s.urgente.unwrap_or(false)),
            congelar: matches!(a, Escalar | Confirmar).then(|| s.congelar.unwrap_or(false)),
            tipo_levantamiento,
        })
    }
}

/// TOMAR, LIBERAR, ESCALAR, CONFIRMAR (reporte de operación inusual o
/// preocupante) o FALSO_POSITIVO sobre una alerta (`aml.resolver_alerta`).
/// Devuelve `ResolucionAlertaAml`.
pub async fn resolver_alerta(pool: &PgPool, u: &Usuario, alerta_id: Uuid, s: &SolicitudResolucion) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let r = Resolucion::de(s)?;
    accion(pool, "SELECT aml.resolver_alerta($1, $2, $3, $4, $5, $6, $7, $8, $9)", |q| {
        q.bind(u.cliente_id)
            .bind(alerta_id)
            .bind(r.accion.as_str())
            .bind(r.dictamen.as_deref())
            .bind(r.tipo_reporte.map(TipoReporte::as_str))
            .bind(r.urgente)
            .bind(r.congelar)
            .bind(r.tipo_levantamiento.map(TipoSolicitud::as_str))
            .bind(clave.as_str())
    })
    .await
}

/// Folio SITI de la UIF de una alerta CONFIRMADO, una sola vez
/// (`aml.registrar_reporte_uif`). Devuelve `{ alerta, idempotente }`.
pub async fn registrar_reporte_uif(
    pool: &PgPool,
    u: &Usuario,
    alerta_id: Uuid,
    s: &SolicitudReporteUif,
) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let folio = s.folio_uif.trim();
    if !(4..=40).contains(&folio.len())
        || !folio.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(AppError::validacion("folio_uif: de 4 a 40 caracteres A-Z, 0-9 o guion"));
    }
    accion(pool, "SELECT aml.registrar_reporte_uif($1, $2, $3, $4, $5)", |q| {
        q.bind(u.cliente_id).bind(alerta_id).bind(folio).bind(s.reportada_en).bind(clave.as_str())
    })
    .await
}

/// Congelamiento manual (un solo oficial puede endurecer;
/// `aml.congelar_manual`). Si el cliente ya tenía un congelamiento ACTIVO se
/// devuelve ese con `ya_congelado: true`, y la respuesta cuenta como
/// idempotente (HTTP 200): `idempotente = idempotente || ya_congelado`.
pub async fn congelar_manual(
    pool: &PgPool,
    u: &Usuario,
    cliente_id: Uuid,
    s: &SolicitudCongelamiento,
) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let motivo = narrativo("el motivo", &s.motivo, DICTAMEN_MIN)?;
    let mut r = accion(pool, "SELECT aml.congelar_manual($1, $2, $3, $4, $5)", |q| {
        q.bind(u.cliente_id).bind(cliente_id).bind(motivo.as_str()).bind(s.alerta_id).bind(clave.as_str())
    })
    .await?;
    let ya_congelado = r.get("ya_congelado").and_then(Value::as_bool).unwrap_or(false);
    let idempotente = es_idempotente(&r) || ya_congelado;
    if let Some(o) = r.as_object_mut() {
        o.insert("idempotente".into(), Value::Bool(idempotente));
    }
    Ok(r)
}

/// Solicitud de cuatro ojos sobre un congelamiento: levantarlo (liberando o
/// conservando las retenciones) o liberar las retenciones conservadas
/// (`aml.solicitar_levantamiento`). Devuelve `{ solicitud, congelamiento, idempotente }`.
pub async fn solicitar_levantamiento(
    pool: &PgPool,
    u: &Usuario,
    congelamiento_id: Uuid,
    s: &SolicitudLevantamiento,
) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let justificacion = narrativo("la justificación", &s.justificacion, DICTAMEN_MIN)?;
    accion(pool, "SELECT aml.solicitar_levantamiento($1, $2, $3, $4, $5)", |q| {
        q.bind(u.cliente_id)
            .bind(congelamiento_id)
            .bind(s.tipo.as_str())
            .bind(justificacion.as_str())
            .bind(clave.as_str())
    })
    .await
}

/// Decisión de un segundo oficial sobre una solicitud de levantamiento o
/// liberación (`aml.decidir_levantamiento`). Antes de confirmar se relee cada
/// LIBERACION_RETENCION creada y se verifica su `tx_hash` (si no cuadra, la
/// transacción se revierte con `INTEGRIDAD`). Devuelve `DecisionLevantamientoAml`.
pub async fn decidir_levantamiento(
    pool: &PgPool,
    u: &Usuario,
    solicitud_id: Uuid,
    s: &SolicitudDecision,
) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let comentario = narrativo("el comentario", &s.comentario, COMENTARIO_MIN)?;
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Aml).await?;
        let r: Value = sqlx::query_scalar("SELECT aml.decidir_levantamiento($1, $2, $3, $4, $5)")
            .bind(u.cliente_id)
            .bind(solicitud_id)
            .bind(s.aprobar)
            .bind(comentario.as_str())
            .bind(clave.as_str())
            .fetch_one(&mut *tx)
            .await?;
        let ids = ids_de_liberaciones(&r)?;
        if !ids.is_empty() {
            let filas: Vec<TransaccionIfpe> =
                sqlx::query_as("SELECT * FROM aml.movimientos_ledger($1)").bind(&ids).fetch_all(&mut *tx).await?;
            if filas.len() != ids.len() {
                return Err(AppError::Integridad(format!(
                    "solicitud {solicitud_id}: {} liberaciones y {} movimientos en el ledger",
                    ids.len(),
                    filas.len()
                )));
            }
            for t in &filas {
                let recalculado = hash::hash_ifpe(t);
                if recalculado != t.tx_hash {
                    return Err(AppError::Integridad(format!(
                        "IFPE secuencia {}: tx_hash de la liberación {} != recalculado {}",
                        t.secuencia, t.tx_hash, recalculado
                    )));
                }
            }
        }
        tx.commit().await?;
        Ok(r)
    })
    .await
}

/// `liberaciones[].transaccion_id` del resultado de `aml.decidir_levantamiento`.
fn ids_de_liberaciones(r: &Value) -> AppResult<Vec<Uuid>> {
    let Some(liberaciones) = r.get("liberaciones").and_then(Value::as_array) else {
        return Err(AppError::Integridad("decidir_levantamiento: falta liberaciones en el resultado".into()));
    };
    liberaciones
        .iter()
        .map(|l| {
            l.get("transaccion_id")
                .and_then(Value::as_str)
                .and_then(|t| t.parse().ok())
                .ok_or_else(|| AppError::Integridad(format!("liberación sin transaccion_id válido: {l}")))
        })
        .collect()
}

/// Propuesta de cambio a una regla o a los parámetros, incluido el modo de
/// observación (`aml.proponer_cambio`). Rust solo exige un objeto `cambios`
/// no vacío de a lo más 20 claves; nombres y tipos los valida la base de
/// datos (`AML_CAMBIO_INVALIDO`). Devuelve `{ propuesta, idempotente }`.
pub async fn proponer_cambio(pool: &PgPool, u: &Usuario, s: &SolicitudPropuesta) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let justificacion = narrativo("la justificación", &s.justificacion, DICTAMEN_MIN)?;
    if s.cambios.is_empty() || s.cambios.len() > CAMBIOS_MAX {
        return Err(AppError::validacion(format!("cambios: de 1 a {CAMBIOS_MAX} claves")));
    }
    let regla_codigo = s.regla_codigo.as_deref().map(str::trim);
    match s.tipo {
        TipoPropuesta::Regla => {
            let valido = regla_codigo.is_some_and(|c| {
                !c.is_empty() && c.len() <= 40 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            });
            if !valido || s.nivel_cliente.is_none() {
                return Err(AppError::validacion("una propuesta REGLA requiere regla_codigo y nivel_cliente"));
            }
        }
        TipoPropuesta::Parametros => {
            if regla_codigo.is_some() || s.nivel_cliente.is_some() {
                return Err(AppError::validacion("regla_codigo y nivel_cliente solo aplican a una propuesta REGLA"));
            }
        }
    }
    let cambios = Value::Object(s.cambios.clone());
    accion(pool, "SELECT aml.proponer_cambio($1, $2, $3, $4, $5, $6, $7)", |q| {
        q.bind(u.cliente_id)
            .bind(s.tipo.as_str())
            .bind(regla_codigo)
            .bind(s.nivel_cliente.map(|n| n.as_str()))
            .bind(&cambios)
            .bind(justificacion.as_str())
            .bind(clave.as_str())
    })
    .await
}

/// Aprobación o rechazo de una propuesta por un oficial distinto del que la
/// hizo (`aml.decidir_propuesta`). Devuelve `{ propuesta, regla, parametros, idempotente }`.
pub async fn decidir_propuesta(
    pool: &PgPool,
    u: &Usuario,
    propuesta_id: Uuid,
    s: &SolicitudDecision,
) -> AppResult<Value> {
    u.exigir_cumplimiento()?;
    let clave = u.clave(&s.clave_idempotencia)?;
    let comentario = narrativo("el comentario", &s.comentario, COMENTARIO_MIN)?;
    accion(pool, "SELECT aml.decidir_propuesta($1, $2, $3, $4, $5)", |q| {
        q.bind(u.cliente_id).bind(propuesta_id).bind(s.aprobar).bind(comentario.as_str()).bind(clave.as_str())
    })
    .await
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn solicitud(v: serde_json::Value) -> SolicitudResolucion {
        serde_json::from_value(v).unwrap()
    }

    const DICTAMEN: &str = "Depósitos fraccionados sin justificación económica aparente.";

    #[test]
    fn narrativo_recorta_y_valida() {
        assert_eq!(narrativo("x", "  diez letras  \n", 10).unwrap(), "diez letras");
        assert_eq!(narrativo("x", "línea uno\nlínea dos", 10).unwrap(), "línea uno\nlínea dos");
        let e = narrativo("x", "corto", 10).unwrap_err();
        assert_eq!(e.codigo(), "AML_DICTAMEN_INVALIDO");
        assert_eq!(narrativo("x", "con\ttabulador largo", 10).unwrap_err().codigo(), "AML_DICTAMEN_INVALIDO");
        assert!(narrativo("x", &"á".repeat(2000), 10).is_ok());
        assert!(narrativo("x", &"á".repeat(2001), 10).is_err());
    }

    #[test]
    fn resolucion_aplica_valores_por_omision() {
        let r = Resolucion::de(&solicitud(serde_json::json!({"accion": "TOMAR", "clave_idempotencia": "k1234567"})))
            .unwrap();
        assert_eq!((r.dictamen, r.urgente, r.congelar, r.tipo_levantamiento), (None, None, None, None));

        let r = Resolucion::de(&solicitud(serde_json::json!({
            "accion": "CONFIRMAR", "dictamen": DICTAMEN, "clave_idempotencia": "k1234567"
        })))
        .unwrap();
        assert_eq!((r.urgente, r.congelar, r.tipo_reporte), (Some(false), Some(false), None));

        let r = Resolucion::de(&solicitud(serde_json::json!({
            "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "clave_idempotencia": "k1234567"
        })))
        .unwrap();
        assert_eq!(r.tipo_levantamiento, Some(TipoSolicitud::LevantarYLiberar));
        assert_eq!(r.congelar, None);
    }

    #[test]
    fn resolucion_rechaza_campos_fuera_de_lugar() {
        let casos = [
            serde_json::json!({"accion": "TOMAR", "dictamen": DICTAMEN, "clave_idempotencia": "k1234567"}),
            serde_json::json!({"accion": "TOMAR", "urgente": false, "clave_idempotencia": "k1234567"}),
            serde_json::json!({"accion": "ESCALAR", "dictamen": DICTAMEN, "tipo_reporte": "INUSUAL", "clave_idempotencia": "k1234567"}),
            serde_json::json!({"accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "congelar": true, "clave_idempotencia": "k1234567"}),
            serde_json::json!({"accion": "CONFIRMAR", "dictamen": DICTAMEN, "tipo_levantamiento": "LEVANTAR_Y_LIBERAR", "clave_idempotencia": "k1234567"}),
            serde_json::json!({"accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "tipo_levantamiento": "LIBERAR_RETENCIONES", "clave_idempotencia": "k1234567"}),
        ];
        for c in casos {
            let e = Resolucion::de(&solicitud(c.clone())).err().unwrap_or_else(|| panic!("{c}"));
            assert_eq!(e.codigo(), "VALIDACION", "{c}");
        }
        // Falta el dictamen: mismo código que la validación de la base de datos.
        let e = Resolucion::de(&solicitud(serde_json::json!({"accion": "ESCALAR", "clave_idempotencia": "k1234567"})))
            .err()
            .unwrap();
        assert_eq!(e.codigo(), "AML_DICTAMEN_INVALIDO");
        // null equivale a ausente.
        assert!(Resolucion::de(&solicitud(serde_json::json!({
            "accion": "LIBERAR", "dictamen": null, "tipo_reporte": null, "clave_idempotencia": "k1234567"
        })))
        .is_ok());
    }
}
