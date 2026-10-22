//! Tareas periódicas en segundo plano.
//!
//! - Corte de Proof of Reserve cuando el ledger IFPE avanzó desde el último.
//! - Liquidación de mutuos vencidos (regreso de principal + interés a la IFPE)
//!   y marcado de créditos vencidos.
//! - Despacho de órdenes SPEI salientes (registro y consulta con el proveedor).
//! - Monitoreo PLD por lotes ([`aml::monitoreo`]): reglas de 7 y 30 días y de
//!   redes entre clientes, reevaluación de marcas diferidas, plazos de
//!   cumplimiento, sellado de la bitácora y entrega de notificaciones.

use std::time::Duration;

use crate::{aml, reserva, sofom, spei, state::AppState};

pub fn lanzar(estado: AppState) {
    let (corte_seg, liquidacion_seg) = (estado.config.corte_intervalo_seg, estado.config.liquidacion_intervalo_seg);
    if corte_seg > 0 {
        let e = estado.clone();
        tokio::spawn(async move {
            let mut intervalo = tokio::time::interval(Duration::from_secs(corte_seg));
            loop {
                intervalo.tick().await;
                if let Err(err) = corte_si_hay_cambios(&e).await {
                    tracing::error!(codigo = err.codigo(), error = %err.detalle(), "falló el corte automático de reserva");
                }
            }
        });
    }
    let despacho_ms = estado.config.spei.despacho_intervalo_ms;
    if despacho_ms > 0 {
        let e = estado.clone();
        tokio::spawn(async move {
            let mut intervalo = tokio::time::interval(Duration::from_millis(despacho_ms));
            intervalo.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                intervalo.tick().await;
                match spei::outbound::despachar(&e.pool, &*e.pasarela, &e.config.spei).await {
                    Ok(d) if d.cambios > 0 || d.errores > 0 => {
                        tracing::info!(
                            reclamadas = d.reclamadas,
                            cambios = d.cambios,
                            errores = d.errores,
                            "despacho SPEI"
                        )
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!(codigo = err.codigo(), error = %err.detalle(), "falló el despacho SPEI")
                    }
                }
            }
        });
    }
    let aml_seg = estado.config.pld.monitoreo_intervalo_seg;
    if aml_seg > 0 {
        let e = estado.clone();
        tokio::spawn(async move {
            let mut intervalo = tokio::time::interval(Duration::from_secs(aml_seg));
            intervalo.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                intervalo.tick().await;
                match aml::monitoreo::ciclo(&e.pool, &e.config.pld).await {
                    Ok(c) if c.alertas_lote + c.alertas_redes > 0 || c.errores > 0 => {
                        tracing::info!(target: "aml", ?c, "ciclo de monitoreo")
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!(target: "aml", codigo = err.codigo(), error = %err.detalle(), "falló el ciclo de monitoreo AML")
                    }
                }
            }
        });
    }
    if liquidacion_seg > 0 {
        let e = estado;
        tokio::spawn(async move {
            let mut intervalo = tokio::time::interval(Duration::from_secs(liquidacion_seg));
            loop {
                intervalo.tick().await;
                liquidaciones(&e).await;
            }
        });
    }
}

async fn corte_si_hay_cambios(e: &AppState) -> crate::AppResult<()> {
    let ultimo = reserva::ultimo_corte(&e.pool).await?;
    let actual = reserva::estado_actual(&e.pool).await?;
    let hay_cambios = ultimo.map_or(true, |c| c.secuencia_ledger != actual.ledger.ultima_secuencia);
    if hay_cambios {
        match reserva::generar_corte(&e.pool).await {
            Err(crate::AppError::Conflicto { codigo, .. }) if codigo == "CORTE_EN_CURSO" => {}
            otro => {
                otro?;
            }
        }
    }
    Ok(())
}

async fn liquidaciones(e: &AppState) {
    match sofom::credito::marcar_creditos_vencidos(&e.pool).await {
        Ok(n) if n > 0 => tracing::warn!(n, "créditos marcados como vencidos"),
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err.detalle(), "no se pudieron marcar créditos vencidos"),
    }
    match sofom::puente::contratos_bloqueados_por_cartera(&e.pool).await {
        Ok(bloqueados) => {
            for (folio, comprometido) in bloqueados {
                tracing::warn!(folio, %comprometido, "mutuo vencido bloqueado por cartera vencida: requiere cobranza o castigo");
            }
        }
        Err(err) => tracing::error!(error = %err.detalle(), "no se pudieron listar mutuos bloqueados"),
    }
    let pendientes = match sofom::puente::contratos_por_liquidar(&e.pool, 500).await {
        Ok(ids) => ids,
        Err(err) => {
            tracing::error!(error = %err.detalle(), "no se pudieron listar mutuos vencidos");
            return;
        }
    };
    for id in pendientes {
        if let Err(err) = sofom::puente::liquidar(&e.pool, id, None).await {
            tracing::error!(contrato = %id, codigo = err.codigo(), error = %err.detalle(), "no se pudo liquidar el mutuo");
        }
    }
}
