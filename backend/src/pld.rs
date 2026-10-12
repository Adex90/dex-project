//! Prevención de lavado de dinero y financiamiento al terrorismo (PLD/FT).
//!
//! - [`PerfilCliente::exigir_operable`]: solo clientes con KYC verificado y
//!   fuera de la Lista de Personas Bloqueadas pueden mover dinero.
//! - [`evaluar`]: monitoreo transaccional. Las alertas se registran en
//!   `kyc.alertas_pld` dentro de la misma transacción que la operación (si la
//!   operación se revierte, la alerta también). Las alertas no bloquean la
//!   operación: alimentan la bandeja del oficial de cumplimiento.

use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::{
    config::UmbralesPld,
    db::Tx,
    error::{AppError, AppResult},
    models::{
        kyc::{EstatusKyc, NivelAlerta, NivelRiesgo, TipoOperacionPld},
        Entidad,
    },
};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PerfilCliente {
    pub id: Uuid,
    pub estatus_kyc: EstatusKyc,
    pub en_lista_bloqueadas: bool,
    pub nivel_riesgo_pld: NivelRiesgo,
    pub ingreso_mensual_estimado: Option<Decimal>,
    pub es_pep: bool,
}

/// Carga el perfil de riesgo. Todos los roles transaccionales pueden leer `kyc.clientes`.
pub async fn cargar_perfil(tx: &mut Tx, cliente_id: Uuid) -> AppResult<PerfilCliente> {
    sqlx::query_as(
        "SELECT id, estatus_kyc, en_lista_bloqueadas, nivel_riesgo_pld, ingreso_mensual_estimado, es_pep \
         FROM kyc.clientes WHERE id = $1",
    )
    .bind(cliente_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::no_encontrado("cliente no encontrado"))
}

impl PerfilCliente {
    pub fn exigir_operable(&self) -> AppResult<()> {
        if self.en_lista_bloqueadas {
            return Err(AppError::negocio(
                "CLIENTE_EN_LISTA_BLOQUEADAS",
                "el cliente está en la Lista de Personas Bloqueadas; operación rechazada",
            ));
        }
        if self.estatus_kyc != EstatusKyc::Verificado {
            return Err(AppError::negocio("KYC_NO_VERIFICADO", "el expediente KYC del cliente no está verificado"));
        }
        Ok(())
    }
}

/// Operación a evaluar.
pub struct EventoPld<'a> {
    pub entidad: Entidad,
    pub cliente: &'a PerfilCliente,
    pub transaccion_id: Uuid,
    pub operacion: &'a str,
    pub monto: Decimal,
    /// Entrada de recursos de terceros a la IFPE (depósito): aplica fraccionamiento.
    pub cuenta_deposito: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct AlertaNueva {
    pub tipo_operacion: TipoOperacionPld,
    pub regla: &'static str,
    pub nivel: NivelAlerta,
    pub puntaje: Decimal,
    pub descripcion: String,
    pub detalle: serde_json::Value,
}

/// Reglas que no requieren consultar historia.
pub fn reglas_inmediatas(ev: &EventoPld<'_>, u: &UmbralesPld) -> Vec<AlertaNueva> {
    let mut alertas = Vec::new();
    if ev.monto >= u.relevante {
        alertas.push(AlertaNueva {
            tipo_operacion: TipoOperacionPld::Relevante,
            regla: "R01_OPERACION_RELEVANTE",
            nivel: NivelAlerta::Medio,
            puntaje: Decimal::new(50, 0),
            descripcion: format!(
                "{} por {} MXN, igual o mayor al umbral de operación relevante",
                ev.operacion, ev.monto
            ),
            detalle: json!({ "umbral": u.relevante, "monto": ev.monto }),
        });
    }
    if ev.cliente.nivel_riesgo_pld == NivelRiesgo::Alto && ev.monto >= u.riesgo_alto {
        alertas.push(AlertaNueva {
            tipo_operacion: TipoOperacionPld::Inusual,
            regla: "R03_CLIENTE_RIESGO_ALTO",
            nivel: NivelAlerta::Alto,
            puntaje: Decimal::new(70, 0),
            descripcion: format!("{} por {} MXN de un cliente con riesgo PLD ALTO", ev.operacion, ev.monto),
            detalle: json!({ "umbral": u.riesgo_alto, "es_pep": ev.cliente.es_pep }),
        });
    }
    if let Some(ingreso) = ev.cliente.ingreso_mensual_estimado.filter(|i| *i > Decimal::ZERO) {
        if ev.monto > ingreso * u.multiplo_perfil {
            alertas.push(AlertaNueva {
                tipo_operacion: TipoOperacionPld::Inusual,
                regla: "R04_FUERA_DE_PERFIL",
                nivel: NivelAlerta::Medio,
                puntaje: Decimal::new(60, 0),
                descripcion: format!(
                    "{} por {} MXN excede {} veces el ingreso mensual declarado",
                    ev.operacion, ev.monto, u.multiplo_perfil
                ),
                detalle: json!({ "ingreso_mensual_estimado": ingreso }),
            });
        }
    }
    alertas
}

/// Evalúa la operación y registra las alertas. Devuelve las alertas creadas.
pub async fn evaluar(tx: &mut Tx, ev: &EventoPld<'_>, u: &UmbralesPld) -> AppResult<Vec<AlertaNueva>> {
    let mut alertas = reglas_inmediatas(ev, u);

    // R02: fraccionamiento — depósitos individuales bajo el umbral que en 24 h lo
    // alcanzan (los depósitos que ya son relevantes se reportan por R01).
    if let Some(cuenta) = ev.cuenta_deposito {
        if ev.monto < u.relevante {
            let (suma, n): (Decimal, i64) = sqlx::query_as(
                "SELECT coalesce(sum(monto), 0), count(*) FROM ifpe.transacciones \
                 WHERE cuenta_destino_id = $1 AND tipo = 'DEPOSITO' AND monto < $2 \
                   AND creado_en > now() - interval '24 hours'",
            )
            .bind(cuenta)
            .bind(u.relevante)
            .fetch_one(&mut **tx)
            .await?;
            if n >= 2 && suma >= u.relevante && suma - ev.monto < u.relevante {
                alertas.push(AlertaNueva {
                    tipo_operacion: TipoOperacionPld::Inusual,
                    regla: "R02_FRACCIONAMIENTO",
                    nivel: NivelAlerta::Alto,
                    puntaje: Decimal::new(80, 0),
                    descripcion: format!("{n} depósitos en 24 h suman {suma} MXN, cada uno bajo el umbral relevante"),
                    detalle: json!({ "depositos_24h": n, "suma_24h": suma, "umbral": u.relevante }),
                });
            }
        }
    }

    for a in &alertas {
        // Sin RETURNING: los roles transaccionales solo tienen INSERT sobre alertas.
        sqlx::query(
            "INSERT INTO kyc.alertas_pld \
             (id, cliente_id, entidad, transaccion_id, tipo_operacion, regla, nivel_riesgo, puntaje, monto, descripcion, detalle) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(Uuid::new_v4())
        .bind(ev.cliente.id)
        .bind(ev.entidad)
        .bind(ev.transaccion_id)
        .bind(a.tipo_operacion)
        .bind(a.regla)
        .bind(a.nivel)
        .bind(a.puntaje)
        .bind(ev.monto)
        .bind(&a.descripcion)
        .bind(&a.detalle)
        .execute(&mut **tx)
        .await?;
        tracing::info!(regla = a.regla, cliente = %ev.cliente.id, monto = %ev.monto, "alerta PLD registrada");
    }
    Ok(alertas)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn perfil(riesgo: NivelRiesgo, ingreso: Option<i64>) -> PerfilCliente {
        PerfilCliente {
            id: Uuid::nil(),
            estatus_kyc: EstatusKyc::Verificado,
            en_lista_bloqueadas: false,
            nivel_riesgo_pld: riesgo,
            ingreso_mensual_estimado: ingreso.map(Decimal::from),
            es_pep: false,
        }
    }

    fn evento<'a>(p: &'a PerfilCliente, monto: i64) -> EventoPld<'a> {
        EventoPld {
            entidad: Entidad::Ifpe,
            cliente: p,
            transaccion_id: Uuid::nil(),
            operacion: "DEPOSITO",
            monto: Decimal::from(monto),
            cuenta_deposito: None,
        }
    }

    #[test]
    fn reglas_de_umbral() {
        let u = UmbralesPld::default();
        let bajo = perfil(NivelRiesgo::Bajo, None);
        assert!(reglas_inmediatas(&evento(&bajo, 1_000), &u).is_empty());
        let r = reglas_inmediatas(&evento(&bajo, 150_000), &u);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].regla, "R01_OPERACION_RELEVANTE");

        let alto = perfil(NivelRiesgo::Alto, Some(10_000));
        let r = reglas_inmediatas(&evento(&alto, 60_000), &u);
        let reglas: Vec<_> = r.iter().map(|a| a.regla).collect();
        assert_eq!(reglas, ["R03_CLIENTE_RIESGO_ALTO", "R04_FUERA_DE_PERFIL"]);
    }

    #[test]
    fn exige_kyc_y_listas() {
        let mut p = perfil(NivelRiesgo::Bajo, None);
        assert!(p.exigir_operable().is_ok());
        p.estatus_kyc = EstatusKyc::Pendiente;
        assert_eq!(p.exigir_operable().unwrap_err().codigo(), "KYC_NO_VERIFICADO");
        p.estatus_kyc = EstatusKyc::Suspendido;
        p.en_lista_bloqueadas = true;
        assert_eq!(p.exigir_operable().unwrap_err().codigo(), "CLIENTE_EN_LISTA_BLOQUEADAS");
    }
}
