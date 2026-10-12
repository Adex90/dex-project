//! Tareas periódicas en segundo plano.
//!
//! - Corte de Proof of Reserve cuando el ledger IFPE avanzó desde el último.
//! - Liquidación de mutuos vencidos (regreso de principal + interés a la IFPE)
//!   y marcado de créditos vencidos.

use std::time::Duration;

use chrono::Utc;

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
                    tracing::error!(error = %err, "falló el corte automático de reserva");
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
        reserva::generar_corte(&e.pool).await?;
    }
    Ok(())
}

async fn liquidaciones(e: &AppState) {
    let hoy = Utc::now().date_naive();
    match sofom::credito::marcar_creditos_vencidos(&e.pool, hoy).await {
        Ok(n) if n > 0 => tracing::warn!(n, "créditos marcados como vencidos"),
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err, "no se pudieron marcar créditos vencidos"),
    }
    let pendientes = match sofom::puente::contratos_por_liquidar(&e.pool, hoy, 500).await {
        Ok(ids) => ids,
        Err(err) => {
            tracing::error!(error = %err, "no se pudieron listar mutuos vencidos");
            return;
        }
    };
    for id in pendientes {
        if let Err(err) = sofom::puente::liquidar(&e.pool, id, None, hoy).await {
            tracing::error!(contrato = %id, codigo = err.codigo(), error = %err, "no se pudo liquidar el mutuo");
        }
    }
}
