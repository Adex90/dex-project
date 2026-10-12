//! Synthetic Capital — servidor HTTP.

use anyhow::Context;
use synthetic_capital::{bootstrap, config::Config, db, routes, tareas, AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cfg = Config::desde_entorno()?;
    if cfg.modo_demo {
        tracing::warn!("MODO_DEMO activo: KYC automático y depósitos simulados habilitados. No usar en producción.");
    }
    let pool = db::crear_pool(&cfg).await.context("no se pudo conectar a PostgreSQL")?;
    if cfg.migrar_al_inicio {
        sqlx::migrate!("../migrations").run(&pool).await.context("falló la migración")?;
    }
    bootstrap::ejecutar(&pool, &cfg).await.map_err(|e| anyhow::anyhow!("arranque: {e}"))?;

    let bind = cfg.bind_addr;
    let estado = AppState::nuevo(pool, cfg);
    tareas::lanzar(estado.clone());
    let app = routes::router(estado);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "Synthetic Capital escuchando");
    axum::serve(listener, app).with_graceful_shutdown(senal_de_apagado()).await?;
    Ok(())
}

async fn senal_de_apagado() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminar = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminar = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminar => {},
    }
    tracing::info!("apagando de forma ordenada");
}
