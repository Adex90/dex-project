//! Tareas periódicas en segundo plano.
//!
//! - Corte de Proof of Reserve cuando el ledger IFPE avanzó desde el último.
//! - Liquidación de mutuos vencidos (regreso de principal + interés a la IFPE)
//!   y marcado de créditos vencidos.

use std::time::Duration;

use crate::{reserva, sofom, state::AppState};

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
