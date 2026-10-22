//! Interceptor en línea de los caminos del dinero.
//!
//! Es el único código Rust que llama a `ifpe.aml_evaluar`, `ifpe.aml_retener`,
//! `ifpe.aml_cliente_congelado`, `sofom.aml_evaluar` y
//! `sofom.aml_bloquear_cliente` (lo comprueba un escaneo del código fuente).
//! Las envolturas son SECURITY DEFINER en los esquemas transaccionales: los
//! roles `sc_ifpe`, `sc_spei`, `sc_puente` y `sc_sofom` no tienen USAGE sobre
//! `aml` y solo pasan hechos de la operación, nunca un veredicto.

use uuid::Uuid;

use crate::{
    db::Tx,
    error::{AppError, AppResult},
    ledger::hash,
    models::{ifpe::TransaccionIfpe, Entidad},
};

use super::modelos::{Decision, Dictamen, Operacion, Sentido};

/// Filtro del reclamo del despachador SPEI (§3.3 c): una orden nunca
/// intentada de un cliente congelado no se reclama. Sin él, las filas que
/// `trg_spei_outbound_aml_retenida` omite (`RETURN NULL`) conservarían
/// `proximo_intento_en` en el pasado y acapararían el `LIMIT` del lote. Las
/// órdenes ya intentadas, ENVIADAS o LIQUIDADAS siguen su curso; el trigger es
/// el respaldo de un congelamiento que confirme entre la subconsulta y el UPDATE.
pub const FILTRO_DESPACHO: &str =
    "AND NOT (estado = 'PENDIENTE' AND intentos_envio = 0 AND ifpe.aml_cliente_congelado(cliente_id))";

/// Evaluación en línea. Corre dentro de la transacción del dinero: después de
/// que el camino bloqueó la fila IFPE del cliente (caminos SOFOM: después de
/// [`bloquear_cliente_sofom`]), después de la idempotencia y las validaciones
/// de negocio, y ANTES de cualquier UPDATE de saldos, del candado de la
/// reserva y de cualquier INSERT de ledger, orden o crédito.
pub async fn evaluar(tx: &mut Tx, op: &Operacion<'_>) -> AppResult<Dictamen> {
    let entidad = op.canal.entidad();
    let sql = match entidad {
        Entidad::Ifpe => {
            "SELECT evaluacion_id, decision, puntaje, nivel, monto_retener \
             FROM ifpe.aml_evaluar($1, $2, $3, $4, $5, $6, $7)"
        }
        Entidad::Sofom => {
            "SELECT evaluacion_id, decision, puntaje, nivel, monto_retener \
             FROM sofom.aml_evaluar($1, $2, $3, $4, $5, $6, $7)"
        }
    };
    let contraparte = serde_json::to_value(&op.contraparte).map_err(anyhow::Error::from)?;
    let d: Dictamen = sqlx::query_as(sql)
        .bind(op.cliente_id)
        .bind(op.canal.sentido().as_str())
        .bind(op.canal.as_str())
        .bind(op.monto)
        .bind(op.transaccion_id)
        .bind(op.clave)
        .bind(contraparte)
        .fetch_one(&mut **tx)
        .await?;
    // Garantías del motor (§2.6.4): una entrada nunca se rechaza y la SOFOM
    // nunca retiene. Si no se cumplen, el motor y este código discrepan.
    if op.canal.sentido() == Sentido::Entrada && d.decision == Decision::Bloquear {
        return Err(AppError::Integridad(format!(
            "evaluación {}: una entrada ({}) no puede bloquearse",
            d.evaluacion_id,
            op.canal.as_str()
        )));
    }
    if entidad == Entidad::Sofom && d.decision == Decision::Retener {
        return Err(AppError::Integridad(format!(
            "evaluación {}: la SOFOM ({}) no retiene",
            d.evaluacion_id,
            op.canal.as_str()
        )));
    }
    Ok(d)
}

/// Retención de una evaluación RETENER. Se llama DESPUÉS de insertar el
/// renglón del abono en esta misma transacción: la retención cubre
/// `LEAST(saldo_disponible, monto_retener)`. La función definer inserta el
/// RETENCION (clave neutra `ret:<id>`, concepto "Retención temporal") y su
/// vínculo en `aml.retenciones`; aquí se relee y se verifica su hash.
pub async fn retener(tx: &mut Tx, d: &Dictamen) -> AppResult<TransaccionIfpe> {
    if d.decision != Decision::Retener {
        return Err(AppError::Integridad(format!("evaluación {}: retención sin decisión RETENER", d.evaluacion_id)));
    }
    let id: Uuid = sqlx::query_scalar("SELECT ifpe.aml_retener($1)")
        .bind(d.evaluacion_id)
        .fetch_one(&mut **tx)
        .await?;
    let t: TransaccionIfpe = sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let recalculado = hash::hash_ifpe(&t);
    if recalculado != t.tx_hash {
        return Err(AppError::Integridad(format!(
            "IFPE secuencia {}: tx_hash de la retención {} != recalculado {}",
            t.secuencia, t.tx_hash, recalculado
        )));
    }
    Ok(t)
}

/// Caminos SOFOM: primera sentencia después de abrir la transacción, antes de
/// `bloquear_caja` (bloquea la fila IFPE del acreditado y luego el candado
/// consultivo PLD del cliente; orden global de candados, `ledger` 1–2).
pub async fn bloquear_cliente_sofom(tx: &mut Tx, cliente_id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT sofom.aml_bloquear_cliente($1)")
        .bind(cliente_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// BLOQUEAR: confirma `tx` (evaluación, alerta y congelamiento; el llamador no
/// escribió nada más) y devuelve `Err(AppError::OperacionNoDisponible)`. El
/// registro no lleva monto, motivo ni datos del cliente. Un error al
/// confirmar se devuelve tal cual (`con_reintentos` reintenta uno
/// `Reintentable`, que vuelve a evaluar desde cero).
pub async fn confirmar_bloqueo<T>(tx: Tx, d: &Dictamen) -> AppResult<T> {
    tx.commit().await?;
    tracing::warn!(target: "aml", evaluacion = %d.evaluacion_id, "operación saliente bloqueada por PLD");
    Err(AppError::OperacionNoDisponible)
}
