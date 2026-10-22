//! Originación y cobranza de crédito (rol `sc_sofom`).
//!
//! Un crédito solo puede desembolsarse si queda fondeado al 100% por
//! contratos de mutuo vigentes cuyo **vencimiento es igual o posterior** al
//! del crédito (calce de plazos: nunca se presta a largo con dinero a corto).
//! La asignación elige primero los mutuos que vencen antes (el calce más
//! ajustado), preservando los mutuos largos para créditos largos.
//!
//! La base de datos repite cada regla: `sofom.fn_validar_asignacion`
//! (calce, sobreasignación) y el constraint trigger diferido
//! `trg_credito_fondeo_total` (fondeo completo al COMMIT).
//!
//! Orden de bloqueo: candado PLD del acreditado (su fila IFPE y luego el
//! candado consultivo, `aml::bloquear_cliente_sofom`) → caja → contratos (por
//! vencimiento, folio) → crédito. El desembolso y el cobro se evalúan en PLD
//! antes de cualquier escritura; un desembolso bloqueado confirma sin crédito,
//! asignaciones ni movimiento.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{bloquear_caja, buscar_tx_por_clave, insertar_tx_sofom, NuevaTxSofom};
use crate::{
    aml::{self, Canal, Contraparte, Decision, Operacion},
    config::ConfigAml,
    db::{self, Rol},
    dinero,
    error::{AppError, AppResult},
    models::sofom::{AsignacionFondeo, ContratoMutuo, Credito, EstadoCredito, TipoTxSofom, TransaccionSofom},
    pld,
    spei::clabe,
};

#[derive(Debug, Clone)]
pub struct SolicitudCredito {
    pub acreditado_id: Uuid,
    pub principal: Decimal,
    pub tasa_anual: Decimal,
    pub plazo_dias: i32,
    pub destino: Option<String>,
    pub clabe_destino: String,
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoCredito {
    pub credito: Credito,
    pub asignaciones: Vec<AsignacionFondeo>,
    pub desembolso: TransaccionSofom,
    pub idempotente: bool,
}

/// Los umbrales PLD viven en la base de datos (`aml.reglas_riesgo`); el
/// parámetro de configuración se conserva para que los llamadores no cambien.
pub async fn originar(pool: &PgPool, _aml: &ConfigAml, s: &SolicitudCredito) -> AppResult<ResultadoCredito> {
    let principal = dinero::validar_monto(s.principal)?;
    if s.tasa_anual < Decimal::ZERO || s.tasa_anual >= Decimal::ONE || s.tasa_anual.normalize().scale() > 6 {
        return Err(AppError::validacion("tasa_anual debe estar en [0, 1) con máximo 6 decimales"));
    }
    if !(1..=3650).contains(&s.plazo_dias) {
        return Err(AppError::validacion("plazo_dias debe estar entre 1 y 3650"));
    }
    if !clabe::es_valida(&s.clabe_destino) {
        return Err(AppError::validacion("CLABE de desembolso inválida"));
    }
    if s.destino.as_deref().is_some_and(|d| d.chars().count() > 200) {
        return Err(AppError::validacion("destino admite como máximo 200 caracteres"));
    }
    db::con_reintentos(|| originar_tx(pool, s, principal)).await
}

async fn originar_tx(pool: &PgPool, s: &SolicitudCredito, principal: Decimal) -> AppResult<ResultadoCredito> {
    let mut tx = db::begin(pool, Rol::Sofom).await?;
    // Primer candado: la fila IFPE del acreditado y su candado PLD, antes de la
    // caja (orden global; un congelamiento concurrente se serializa aquí).
    aml::bloquear_cliente_sofom(&mut tx, s.acreditado_id).await?;
    let caja = bloquear_caja(&mut tx).await?;

    if let Some(previa) = buscar_tx_por_clave(&mut tx, &s.clave_idempotencia).await? {
        let credito: Option<Credito> = match (previa.tipo, previa.credito_id) {
            (TipoTxSofom::DesembolsoCredito, Some(id)) => {
                sqlx::query_as("SELECT * FROM sofom.creditos WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?
            }
            _ => None,
        };
        let credito = credito
            .filter(|c| {
                c.acreditado_id == s.acreditado_id
                    && c.principal == principal
                    && c.plazo_dias == s.plazo_dias
                    && c.tasa_anual == s.tasa_anual
                    && c.destino == s.destino
                    && c.clabe_desembolso.as_deref() == Some(s.clabe_destino.as_str())
            })
            .ok_or_else(|| {
                AppError::conflicto("CLAVE_IDEMPOTENCIA_REUTILIZADA", "la clave ya se usó para otra operación")
            })?;
        let asignaciones =
            sqlx::query_as("SELECT * FROM sofom.asignaciones_fondeo WHERE credito_id = $1 ORDER BY creado_en")
                .bind(credito.id)
                .fetch_all(&mut *tx)
                .await?;
        return Ok(ResultadoCredito { credito, asignaciones, desembolso: previa, idempotente: true });
    }

    let perfil = pld::cargar_perfil(&mut tx, s.acreditado_id).await?;
    perfil.exigir_operable()?;

    // PLD antes de crear el crédito (la única activación EN_FONDEO → VIGENTE
    // ocurre en esta misma transacción): un BLOQUEAR confirma sin crédito,
    // asignaciones ni desembolso.
    let desembolso_id = Uuid::new_v4();
    let d = aml::evaluar(
        &mut tx,
        &Operacion {
            cliente_id: s.acreditado_id,
            canal: Canal::DesembolsoCredito,
            monto: principal,
            transaccion_id: desembolso_id,
            clave: &s.clave_idempotencia,
            contraparte: Contraparte { cuenta: Some(s.clabe_destino.clone()), ..Default::default() },
        },
    )
    .await?;
    if d.decision == Decision::Bloquear {
        return aml::confirmar_bloqueo(tx, &d).await;
    }

    // Fecha valor: hoy según la base de datos (trg_credito_fecha_valor la exige).
    let hoy: NaiveDate = sqlx::query_scalar("SELECT current_date").fetch_one(&mut *tx).await?;
    let credito: Credito = sqlx::query_as(
        "INSERT INTO sofom.creditos \
         (id, acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso, destino, clabe_desembolso) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(s.acreditado_id)
    .bind(principal)
    .bind(s.tasa_anual)
    .bind(s.plazo_dias)
    .bind(hoy)
    .bind(&s.destino)
    .bind(&s.clabe_destino)
    .fetch_one(&mut *tx)
    .await?;

    // Solo mutuos que vencen en o después del crédito y que ya existían al
    // desembolso. Sin lock aquí: todas las operaciones SOFOM se serializan en
    // la caja, y el trigger de asignación bloquea y revalida cada mutuo.
    let candidatos: Vec<ContratoMutuo> = sqlx::query_as(
        "SELECT * FROM sofom.contratos_mutuo \
         WHERE estado = 'VIGENTE' AND fecha_vencimiento >= $1 AND fecha_inicio <= $2 \
           AND principal_asignado < principal \
         ORDER BY fecha_vencimiento, folio",
    )
    .bind(credito.fecha_vencimiento)
    .bind(credito.fecha_desembolso)
    .fetch_all(&mut *tx)
    .await?;

    let mut restante = principal;
    let mut asignaciones = Vec::new();
    for c in &candidatos {
        if restante.is_zero() {
            break;
        }
        let disponible = c.principal - c.principal_asignado;
        let monto = disponible.min(restante);
        let a: AsignacionFondeo = sqlx::query_as(
            "INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto) VALUES ($1, $2, $3) RETURNING *",
        )
        .bind(credito.id)
        .bind(c.id)
        .bind(monto)
        .fetch_one(&mut *tx)
        .await?;
        asignaciones.push(a);
        restante -= monto;
    }
    if restante > Decimal::ZERO {
        return Err(AppError::negocio(
            "FONDEO_A_PLAZO_INSUFICIENTE",
            format!(
                "faltan {restante} MXN de contratos de mutuo con vencimiento en o después del {}; \
                 la SOFOM solo presta dinero depositado a un plazo igual o mayor",
                credito.fecha_vencimiento
            ),
        ));
    }
    // Desembolsar convierte efectivo en cartera peso por peso: la regla de
    // liquidez (caja >= principal vigente - cartera viva) se conserva si ya se
    // cumplía. Se exige además que la caja tenga el efectivo.
    let requerido: Decimal = sqlx::query_scalar("SELECT sofom.requerido_liquidez(NULL)").fetch_one(&mut *tx).await?;
    if caja.saldo < principal || caja.saldo < requerido {
        return Err(AppError::negocio("LIQUIDEZ_SOFOM_INSUFICIENTE", "la caja de la SOFOM no cubre el desembolso"));
    }

    let credito: Credito = sqlx::query_as(
        "UPDATE sofom.creditos SET estado = 'VIGENTE', saldo_insoluto = principal WHERE id = $1 RETURNING *",
    )
    .bind(credito.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE sofom.caja SET saldo = saldo - $1 WHERE id = $2")
        .bind(principal)
        .bind(caja.id)
        .execute(&mut *tx)
        .await?;
    let concepto = format!("Desembolso del crédito folio {} a CLABE {}", credito.folio, s.clabe_destino);
    let desembolso = insertar_tx_sofom(
        &mut tx,
        NuevaTxSofom {
            id: desembolso_id,
            tipo: TipoTxSofom::DesembolsoCredito,
            cuenta: None,
            contrato: None,
            credito: Some(credito.id),
            caja: Some(caja.id),
            monto: principal,
            concepto: Some(&concepto),
            clave: &s.clave_idempotencia,
            espejo: None,
        },
    )
    .await?;
    // Al COMMIT, trg_credito_fondeo_total verifica Σ asignaciones == principal.
    tx.commit().await?;
    Ok(ResultadoCredito { credito, asignaciones, desembolso, idempotente: false })
}

// -----------------------------------------------------------------------------
// Cobranza
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PagoCredito {
    pub credito_id: Uuid,
    pub principal: Decimal,
    pub interes: Decimal,
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoPago {
    pub credito: Credito,
    pub cobro: TransaccionSofom,
    pub idempotente: bool,
}

fn no_negativo(nombre: &str, m: Decimal) -> AppResult<Decimal> {
    if m.is_zero() {
        return Ok(dinero::con_escala(Decimal::ZERO));
    }
    dinero::validar_monto(m).map_err(|_| AppError::validacion(format!("{nombre} debe ser >= 0 con máximo 8 decimales")))
}

pub async fn pagar(pool: &PgPool, p: &PagoCredito) -> AppResult<ResultadoPago> {
    let principal = no_negativo("principal", p.principal)?;
    let interes = no_negativo("interes", p.interes)?;
    let total = dinero::validar_monto(principal + interes)?;
    db::con_reintentos(|| pagar_tx(pool, p, principal, total)).await
}

async fn pagar_tx(pool: &PgPool, p: &PagoCredito, principal: Decimal, total: Decimal) -> AppResult<ResultadoPago> {
    let mut tx = db::begin(pool, Rol::Sofom).await?;
    // Candado PLD del acreditado antes de la caja (orden global). Sin candado
    // sobre el crédito todavía: `acreditado_id` es inmutable.
    let acreditado: Uuid = sqlx::query_scalar("SELECT acreditado_id FROM sofom.creditos WHERE id = $1")
        .bind(p.credito_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("crédito no encontrado"))?;
    aml::bloquear_cliente_sofom(&mut tx, acreditado).await?;
    let caja = bloquear_caja(&mut tx).await?;
    let credito: Credito = sqlx::query_as("SELECT * FROM sofom.creditos WHERE id = $1 FOR NO KEY UPDATE")
        .bind(p.credito_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("crédito no encontrado"))?;
    if let Some(previa) = buscar_tx_por_clave(&mut tx, &p.clave_idempotencia).await? {
        let desglose: Option<(Decimal, Decimal)> =
            sqlx::query_as("SELECT principal, interes FROM sofom.cobros WHERE transaccion_id = $1")
                .bind(previa.id)
                .fetch_optional(&mut *tx)
                .await?;
        let mismo = previa.tipo == TipoTxSofom::CobroCredito
            && previa.credito_id == Some(credito.id)
            && desglose == Some((principal, total - principal));
        if !mismo {
            return Err(AppError::conflicto(
                "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                "la clave ya se usó para otra operación",
            ));
        }
        return Ok(ResultadoPago { credito, cobro: previa, idempotente: true });
    }
    // Un crédito castigado admite recuperaciones (ingreso para la SOFOM).
    if !matches!(credito.estado, EstadoCredito::Vigente | EstadoCredito::Vencido | EstadoCredito::Castigado) {
        return Err(AppError::negocio("CREDITO_NO_COBRABLE", format!("el crédito está {:?}", credito.estado)));
    }
    if principal > credito.saldo_insoluto {
        return Err(AppError::negocio(
            "PAGO_EXCEDE_SALDO",
            format!("el pago de principal {principal} excede el saldo insoluto {}", credito.saldo_insoluto),
        ));
    }
    let saldo = credito.saldo_insoluto - principal;
    let estado = if saldo.is_zero() && credito.estado != EstadoCredito::Castigado {
        EstadoCredito::Liquidado
    } else {
        credito.estado
    };

    // PLD antes de escribir. Un cobro es una ENTRADA: nunca se bloquea y la
    // SOFOM nunca retiene; un patrón crítico congela al acreditado dentro de
    // la evaluación.
    let cobro_id = Uuid::new_v4();
    aml::evaluar(
        &mut tx,
        &Operacion {
            cliente_id: credito.acreditado_id,
            canal: Canal::CobroCredito,
            monto: total,
            transaccion_id: cobro_id,
            clave: &p.clave_idempotencia,
            contraparte: Contraparte::default(),
        },
    )
    .await?;

    sqlx::query("UPDATE sofom.caja SET saldo = saldo + $1 WHERE id = $2")
        .bind(total)
        .bind(caja.id)
        .execute(&mut *tx)
        .await?;
    // Si queda en 0, el trigger libera el fondeo de los mutuos (principal_asignado).
    let credito: Credito =
        sqlx::query_as("UPDATE sofom.creditos SET saldo_insoluto = $1, estado = $2 WHERE id = $3 RETURNING *")
            .bind(saldo)
            .bind(estado)
            .bind(credito.id)
            .fetch_one(&mut *tx)
            .await?;
    let concepto = format!("Cobro crédito folio {}: principal {principal}", credito.folio);
    let cobro = insertar_tx_sofom(
        &mut tx,
        NuevaTxSofom {
            id: cobro_id,
            tipo: TipoTxSofom::CobroCredito,
            cuenta: None,
            contrato: None,
            credito: Some(credito.id),
            caja: Some(caja.id),
            monto: total,
            concepto: Some(&concepto),
            clave: &p.clave_idempotencia,
            espejo: None,
        },
    )
    .await?;
    sqlx::query("INSERT INTO sofom.cobros (transaccion_id, credito_id, principal, interes) VALUES ($1, $2, $3, $4)")
        .bind(cobro.id)
        .bind(credito.id)
        .bind(principal)
        .bind(total - principal)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ResultadoPago { credito, cobro, idempotente: false })
}

// -----------------------------------------------------------------------------
// Castigo (write-off)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoCastigo {
    pub credito: Credito,
    /// Saldo insoluto que absorbe el capital de la SOFOM.
    pub perdida: Decimal,
}

/// Castiga un crédito vencido. Su fondeo se libera (los mutuos que lo
/// fondeaban pueden vencer y pagarse completos): la pérdida la absorbe el
/// capital propio de la SOFOM, nunca el inversionista ni la bóveda. Si el
/// capital no alcanza, la base de datos rechaza el castigo (SC_LIQUIDEZ_MUTUOS)
/// hasta que se aporte capital.
pub async fn castigar(pool: &PgPool, credito_id: Uuid) -> AppResult<ResultadoCastigo> {
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Sofom).await?;
        let caja = bloquear_caja(&mut tx).await?;
        let credito: Credito = sqlx::query_as("SELECT * FROM sofom.creditos WHERE id = $1 FOR NO KEY UPDATE")
            .bind(credito_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::no_encontrado("crédito no encontrado"))?;
        if credito.estado != EstadoCredito::Vencido {
            return Err(AppError::negocio(
                "CREDITO_NO_CASTIGABLE",
                format!("solo se castigan créditos vencidos (está {:?})", credito.estado),
            ));
        }
        // Al castigar, el saldo insoluto deja de contar como cartera que
        // respalda a los mutuos: el capital propio debe cubrirlo.
        let requerido: Decimal =
            sqlx::query_scalar("SELECT sofom.requerido_liquidez(NULL)").fetch_one(&mut *tx).await?;
        let requerido_tras_castigo = requerido + credito.saldo_insoluto;
        if caja.saldo < requerido_tras_castigo {
            return Err(AppError::negocio(
                "CAPITAL_INSUFICIENTE_PARA_CASTIGO",
                format!(
                    "la pérdida de {} MXN debe cubrirse con capital propio; faltan {} MXN de capital",
                    credito.saldo_insoluto,
                    requerido_tras_castigo - caja.saldo
                ),
            ));
        }
        let credito: Credito =
            sqlx::query_as("UPDATE sofom.creditos SET estado = 'CASTIGADO' WHERE id = $1 RETURNING *")
                .bind(credito.id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        tracing::warn!(credito = %credito.id, folio = credito.folio, perdida = %credito.saldo_insoluto, "crédito castigado");
        Ok(ResultadoCastigo { perdida: credito.saldo_insoluto, credito })
    })
    .await
}

// -----------------------------------------------------------------------------
// Capital propio y mantenimiento
// -----------------------------------------------------------------------------

/// Aportación de capital propio a la caja de la SOFOM (nunca dinero de la IFPE).
pub async fn aportar_capital(
    pool: &PgPool,
    monto: Decimal,
    referencia: Option<String>,
    clave_idempotencia: String,
) -> AppResult<(TransaccionSofom, bool)> {
    let monto = dinero::validar_monto(monto)?;
    if referencia.as_deref().is_some_and(|r| r.chars().count() > 140) {
        return Err(AppError::validacion("referencia admite como máximo 140 caracteres"));
    }
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Sofom).await?;
        let caja = bloquear_caja(&mut tx).await?;
        if let Some(previa) = buscar_tx_por_clave(&mut tx, &clave_idempotencia).await? {
            if previa.tipo != TipoTxSofom::AportacionCapital
                || previa.monto != monto
                || previa.concepto.as_deref() != referencia.as_deref()
            {
                return Err(AppError::conflicto(
                    "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                    "la clave ya se usó para otra operación",
                ));
            }
            return Ok((previa, true));
        }
        sqlx::query("UPDATE sofom.caja SET saldo = saldo + $1 WHERE id = $2")
            .bind(monto)
            .bind(caja.id)
            .execute(&mut *tx)
            .await?;
        let t = insertar_tx_sofom(
            &mut tx,
            NuevaTxSofom {
                id: Uuid::new_v4(),
                tipo: TipoTxSofom::AportacionCapital,
                cuenta: None,
                contrato: None,
                credito: None,
                caja: Some(caja.id),
                monto,
                concepto: referencia.as_deref(),
                clave: &clave_idempotencia,
                espejo: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok((t, false))
    })
    .await
}

/// Marca como VENCIDOS los créditos con saldo pasada su fecha de vencimiento.
pub async fn marcar_creditos_vencidos(pool: &PgPool) -> AppResult<u64> {
    let mut tx = db::begin(pool, Rol::Sofom).await?;
    let n = sqlx::query(
        "UPDATE sofom.creditos SET estado = 'VENCIDO' \
         WHERE estado = 'VIGENTE' AND fecha_vencimiento < current_date AND saldo_insoluto > 0",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n)
}

pub async fn listar_creditos(pool: &PgPool, limite: i64) -> AppResult<Vec<Credito>> {
    let mut tx = db::begin_lectura(pool, Rol::Sofom).await?;
    let creditos = sqlx::query_as("SELECT * FROM sofom.creditos ORDER BY folio DESC LIMIT $1")
        .bind(limite.clamp(1, 500))
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(creditos)
}
