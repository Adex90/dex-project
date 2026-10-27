//! Dictamen del oficial de cumplimiento (rol `sc_kyc_verificador`).
//!
//! El operador queda identificado en la transacción (`sc.operador_id`): la base
//! de datos lo registra en el historial, comprueba que sea un OPERADOR distinto
//! del cliente y, para verificar, exige un resultado biométrico con prueba de
//! vida, documento válido, identidad coincidente y score >= umbral de revisión.
//! Un operador no puede verificar a quien el proveedor no pudo identificar.
//!
//! El operador no tiene acceso a PLD: la respuesta es la proyección
//! [`ClienteVista`] (sin nivel de riesgo, fecha de evaluación ni PEP), así que
//! un dictamen no revela si cumplimiento confirmó una operación sospechosa. El
//! nivel de riesgo que fija el operador solo baja si el cliente no está bajo
//! revisión de PLD; si lo está, la base de datos conserva el nivel sin error
//! (un error delataría el caso) y deja el intento en `aml.bitacora`
//! (`trg_clientes_zz_aml_riesgo`, migración 0009).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::expediente::{self, EstadoKyc, Expediente};
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    models::kyc::{Cliente, ClienteVista, NivelRiesgo},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictamenKyc {
    /// VERIFICADO, RECHAZADO o PENDIENTE (reabrir o pedir un nuevo envío).
    #[serde(default)]
    pub estatus_kyc: Option<EstadoKyc>,
    /// Obligatorio para rechazar.
    #[serde(default)]
    pub motivo: Option<String>,
    /// Envío que el operador revisó (`verificacion_en_curso` de la cola).
    /// Obligatorio para verificar: si el cliente reenvió mientras tanto, el
    /// dictamen no se aplica a un envío que nadie revisó.
    #[serde(default)]
    pub verificacion_id: Option<Uuid>,
    /// Nivel de riesgo PLD (asignación del onboarding). No baja mientras el
    /// cliente está bajo revisión de PLD (ver el encabezado del módulo).
    #[serde(default)]
    pub nivel_riesgo_pld: Option<NivelRiesgo>,
    #[serde(default)]
    pub nivel_cuenta: Option<i16>,
}

pub async fn dictaminar(
    pool: &PgPool,
    operador_id: Uuid,
    cliente_id: Uuid,
    d: &DictamenKyc,
) -> AppResult<ClienteVista> {
    if d.estatus_kyc.is_none() && d.nivel_riesgo_pld.is_none() && d.nivel_cuenta.is_none() {
        // Un dictamen vacío no cambia nada: no sirve como lectura del cliente.
        return Err(AppError::validacion("el dictamen debe indicar estatus_kyc, nivel_riesgo_pld o nivel_cuenta"));
    }
    if d.nivel_cuenta.is_some_and(|n| !(1..=4).contains(&n)) {
        return Err(AppError::validacion("nivel_cuenta debe estar entre 1 y 4"));
    }
    if d.estatus_kyc == Some(EstadoKyc::EnRevision) {
        return Err(AppError::validacion("un expediente pasa a revisión solo cuando el cliente lo envía"));
    }
    let motivo = d.motivo.as_deref().map(str::trim).filter(|m| !m.is_empty());
    if motivo.is_some_and(|m| m.chars().count() > 500) {
        return Err(AppError::validacion("el motivo admite como máximo 500 caracteres"));
    }
    if d.estatus_kyc == Some(EstadoKyc::Rechazado) && motivo.is_none() {
        return Err(AppError::validacion("para rechazar un expediente indica el motivo"));
    }
    if d.estatus_kyc == Some(EstadoKyc::Verificado) && d.verificacion_id.is_none() {
        return Err(AppError::validacion("para verificar indica verificacion_id (el envío que revisaste)"));
    }
    if operador_id == cliente_id {
        return Err(AppError::negocio("KYC_AUTODICTAMEN", "un operador no dictamina su propio expediente"));
    }

    let mut tx = db::begin(pool, Rol::KycVerificador).await?;
    sqlx::query("SELECT set_config('sc.operador_id', $1, true)")
        .bind(operador_id.to_string())
        .execute(&mut *tx)
        .await?;
    if let Some(estado) = d.estatus_kyc {
        let n = sqlx::query(
            "UPDATE kyc.kyc_perfiles SET estado_kyc = $2::kyc.estado_kyc, \
                    motivo_rechazo = CASE WHEN $2 = 'RECHAZADO' THEN $3 ELSE motivo_rechazo END \
             WHERE cliente_id = $1 AND ($4::uuid IS NULL OR verificacion_en_curso = $4)",
        )
        .bind(cliente_id)
        .bind(estado.como_texto())
        .bind(motivo)
        .bind(d.verificacion_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 0 {
            let existe: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kyc.kyc_perfiles WHERE cliente_id = $1)")
                    .bind(cliente_id)
                    .fetch_one(&mut *tx)
                    .await?;
            return Err(if existe {
                AppError::conflicto(
                    "KYC_DICTAMEN_DESACTUALIZADO",
                    "el expediente tiene un envío distinto del que revisaste; vuelve a cargar la cola",
                )
            } else {
                AppError::no_encontrado("cliente no encontrado")
            });
        }
    }
    if d.nivel_riesgo_pld.is_some() || d.nivel_cuenta.is_some() {
        sqlx::query(
            "UPDATE kyc.clientes SET \
               nivel_riesgo_pld = coalesce($2, nivel_riesgo_pld), \
               riesgo_evaluado_en = CASE WHEN $2 IS NULL THEN riesgo_evaluado_en ELSE now() END, \
               nivel_cuenta = coalesce($3, nivel_cuenta) \
             WHERE id = $1",
        )
        .bind(cliente_id)
        .bind(d.nivel_riesgo_pld)
        .bind(d.nivel_cuenta)
        .execute(&mut *tx)
        .await?;
    }
    let cliente: Cliente = sqlx::query_as("SELECT * FROM kyc.clientes WHERE id = $1")
        .bind(cliente_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("cliente no encontrado"))?;
    tx.commit().await?;
    tracing::info!(operador = %operador_id, cliente = %cliente_id, estado = ?d.estatus_kyc, "dictamen KYC");
    Ok(cliente.into())
}

/// Expediente en espera de un operador, con el resultado del proveedor.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PendienteRevision {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub expediente: Expediente,
    pub email: String,
    pub resultado: Option<String>,
    pub score_similitud: Option<Decimal>,
    pub prueba_vida_exitosa: Option<bool>,
    pub documento_valido: Option<bool>,
    pub coincide_identidad: Option<bool>,
    pub detalles_fallo: Option<serde_json::Value>,
    pub evaluado_en: Option<DateTime<Utc>>,
}

pub async fn pendientes(pool: &PgPool, limite: i64) -> AppResult<Vec<PendienteRevision>> {
    let columnas = expediente::COLUMNAS.split(", ").map(|c| format!("p.{}", c.trim())).collect::<Vec<_>>().join(", ");
    let mut tx = db::begin_lectura(pool, Rol::KycVerificador).await?;
    let filas = sqlx::query_as(&format!(
        "SELECT {columnas}, c.email, a.resultado, a.score_similitud, a.prueba_vida_exitosa, a.documento_valido, \
                a.coincide_identidad, a.detalles_fallo, a.\"timestamp\" AS evaluado_en \
         FROM kyc.kyc_perfiles p \
         JOIN kyc.clientes c ON c.id = p.cliente_id \
         LEFT JOIN kyc.kyc_auditoria_biometrica a ON a.verificacion_id = p.verificacion_en_curso \
         WHERE p.estado_kyc = 'EN_REVISION' \
         ORDER BY p.enviado_en \
         LIMIT $1"
    ))
    .bind(limite.clamp(1, 500))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(filas)
}
