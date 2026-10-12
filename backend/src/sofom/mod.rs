//! División de crédito (SOFOM).
//!
//! - [`puente`]: el único camino IFPE -> SOFOM. Convierte saldo disponible de
//!   la bóveda en un contrato de mutuo a plazo fijo (rol `sc_puente`), y lo
//!   regresa a la bóveda al vencimiento con sus intereses.
//! - [`credito`]: originación de crédito fondeado al 100% por mutuos cuyo
//!   vencimiento es igual o posterior al del crédito (rol `sc_sofom`).
//! - [`tasas`]: plazos admitidos y tasa anual de cada uno.

pub mod credito;
pub mod puente;
pub mod tasas;

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    ledger::hash,
    models::sofom::{Caja, ContratoMutuo, CuentaSofom, TipoTxSofom, TransaccionSofom},
};

pub(crate) struct NuevaTxSofom<'a> {
    pub id: Uuid,
    pub tipo: TipoTxSofom,
    pub cuenta: Option<Uuid>,
    pub contrato: Option<Uuid>,
    pub credito: Option<Uuid>,
    pub caja: Option<Uuid>,
    pub monto: rust_decimal::Decimal,
    pub concepto: Option<&'a str>,
    pub clave: &'a str,
    pub espejo: Option<Uuid>,
}

/// Inserta un movimiento del ledger SOFOM y verifica el hash calculado por la base de datos.
pub(crate) async fn insertar_tx_sofom(tx: &mut Tx, n: NuevaTxSofom<'_>) -> AppResult<TransaccionSofom> {
    let t: TransaccionSofom = sqlx::query_as(
        "INSERT INTO sofom.transacciones \
         (id, tipo, cuenta_id, contrato_id, credito_id, caja_id, monto, concepto, clave_idempotencia, ref_movimiento_espejo) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *",
    )
    .bind(n.id)
    .bind(n.tipo)
    .bind(n.cuenta)
    .bind(n.contrato)
    .bind(n.credito)
    .bind(n.caja)
    .bind(n.monto)
    .bind(n.concepto)
    .bind(n.clave)
    .bind(n.espejo)
    .fetch_one(&mut **tx)
    .await?;
    let recalculado = hash::hash_sofom(&t);
    if recalculado != t.tx_hash {
        return Err(AppError::Integridad(format!(
            "SOFOM secuencia {}: tx_hash de la base de datos {} != recalculado {}",
            t.secuencia, t.tx_hash, recalculado
        )));
    }
    Ok(t)
}

/// Caja por defecto de la SOFOM (la activa más antigua), bloqueada.
pub(crate) async fn bloquear_caja(tx: &mut Tx) -> AppResult<Caja> {
    sqlx::query_as("SELECT * FROM sofom.caja WHERE activa ORDER BY creado_en, id LIMIT 1 FOR UPDATE")
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::Interno(anyhow::anyhow!("no hay caja SOFOM activa")))
}

pub(crate) async fn buscar_tx_por_clave(tx: &mut Tx, clave: &str) -> AppResult<Option<TransaccionSofom>> {
    Ok(sqlx::query_as("SELECT * FROM sofom.transacciones WHERE clave_idempotencia = $1")
        .bind(clave)
        .fetch_optional(&mut **tx)
        .await?)
}

/// Posición de inversión de un cliente.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Posicion {
    pub cuenta: Option<CuentaSofom>,
    pub contratos: Vec<ContratoMutuo>,
    /// Principal en mutuos vigentes.
    pub total_invertido: rust_decimal::Decimal,
    /// Interés que pagarán los mutuos vigentes a su vencimiento.
    pub intereses_por_recibir: rust_decimal::Decimal,
}

pub async fn posicion_de_cliente(pool: &PgPool, cliente_id: Uuid) -> AppResult<Posicion> {
    let mut tx = db::begin_lectura(pool, Rol::Sofom).await?;
    let cuenta: Option<CuentaSofom> =
        sqlx::query_as("SELECT * FROM sofom.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'")
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?;
    let contratos: Vec<ContratoMutuo> =
        sqlx::query_as("SELECT * FROM sofom.contratos_mutuo WHERE cliente_id = $1 ORDER BY folio DESC")
            .bind(cliente_id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    let vigentes = contratos.iter().filter(|c| c.estado == crate::models::sofom::EstadoMutuo::Vigente);
    let (total_invertido, intereses_por_recibir) = vigentes
        .fold((rust_decimal::Decimal::ZERO, rust_decimal::Decimal::ZERO), |(p, i), c| {
            (p + c.principal, i + tasas::interes(c.principal, c.tasa_anual, c.plazo_dias))
        });
    Ok(Posicion { cuenta, contratos, total_invertido, intereses_por_recibir })
}
