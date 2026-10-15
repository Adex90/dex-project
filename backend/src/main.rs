//! Synthetic Capital — servidor HTTP.

use anyhow::Context;
use std::time::Duration;

use synthetic_capital::{bootstrap, config::Config, db, routes, servidor, tareas, AppState};
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
    // Migraciones y arranque con el dueño del esquema; el servicio, con un
    // usuario que solo puede actuar mediante los roles sc_*.
    let propietario = match &cfg.database_url_propietario {
        Some(url) => db::crear_pool_propietario(url).await.context("no se pudo conectar como dueño del esquema")?,
        None => pool.clone(),
    };
    if cfg.migrar_al_inicio {
        sqlx::migrate!("../migrations").run(&propietario).await.context("falló la migración")?;
    }
    // `context` conserva la cadena de causas (tabla, permiso, restricción).
    bootstrap::ejecutar(&propietario, &cfg).await.context("arranque")?;
    if cfg.database_url_propietario.is_some() {
        propietario.close().await;
    }
    db::verificar_usuario_de_aplicacion(&pool).await.context("no se pudo comprobar el usuario de la aplicación")?;

    let bind = cfg.bind_addr;
    let limites = cfg.http.clone();
    let estado = AppState::nuevo(pool.clone(), cfg);
    tareas::lanzar(estado.clone());
    let app = routes::router(estado);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, max_conexiones = limites.max_conexiones, "Synthetic Capital escuchando");
    servidor::servir(listener, app, &limites, senal_de_apagado()).await;
    // Las tareas de fondo terminan con el runtime; se cierra el pool con plazo.
    if tokio::time::timeout(Duration::from_secs(5), pool.close()).await.is_err() {
        tracing::warn!("el pool de PostgreSQL no cerró a tiempo");
    }
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
