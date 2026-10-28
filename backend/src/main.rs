//! Synthetic Capital — servidor HTTP y tareas de despliegue (ver [`cli`]).

use std::{future::Future, io::IsTerminal, process::ExitCode, time::Duration};

use anyhow::Context;
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    cli::{self, Comando},
    config::{Config, LoginAplicacion},
    db, routes, servidor, tareas, AppState,
};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let comando = match cli::interpretar(std::env::args_os().skip(1)) {
        Ok(comando) => comando,
        Err(e) => {
            eprintln!("{e}\n\n{}", cli::USO);
            return ExitCode::from(2);
        }
    };
    match comando {
        Comando::Ayuda => {
            println!("{}", cli::USO);
            ExitCode::SUCCESS
        }
        Comando::Version => {
            println!("synthetic-capital {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        // Sonda del contenedor: solo std, sin runtime ni base de datos.
        Comando::Salud(sonda) => cli::salud(sonda),
        Comando::Servir => ejecutar(servir()),
        Comando::Migrar => ejecutar(migrar()),
    }
}

/// Ejecuta un comando asíncrono; un error sale con código 1 y su cadena de causas.
/// Colores ANSI solo en una terminal: en contenedores la salida va a un
/// colector de logs (docker, CloudWatch) que guardaría los códigos de escape.
fn ejecutar(comando: impl Future<Output = anyhow::Result<()>>) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    let resultado = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("no se pudo crear el runtime de tokio")
        .and_then(|runtime| runtime.block_on(comando));
    match resultado {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

async fn servir() -> anyhow::Result<()> {
    let cfg = Config::desde_entorno()?;
    if cfg.modo_demo {
        tracing::warn!("MODO_DEMO activo: KYC automático y depósitos simulados habilitados. No usar en producción.");
    }
    let pool = db::crear_pool(&cfg).await.context("no se pudo conectar a PostgreSQL")?;
    if cfg.migrar_al_inicio || cfg.arranque_al_inicio {
        // Migraciones y arranque con el dueño del esquema; el servicio, con un
        // usuario que solo puede actuar mediante los roles sc_*.
        let propietario = match &cfg.database_url_propietario {
            Some(url) => db::crear_pool_propietario(url).await.context("no se pudo conectar como dueño del esquema")?,
            None => pool.clone(),
        };
        let preparada = preparar_base(&propietario, &cfg, cfg.migrar_al_inicio, cfg.arranque_al_inicio).await;
        if cfg.database_url_propietario.is_some() {
            propietario.close().await;
        }
        preparada?;
    } else if cfg.database_url_propietario.is_some() {
        tracing::warn!(
            "DATABASE_URL_PROPIETARIO está definida pero MIGRAR_AL_INICIO y ARRANQUE_AL_INICIO están desactivadas: \
             el servicio no la usa; quítala de su entorno (solo la necesita `synthetic-capital migrar`)"
        );
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

/// `synthetic-capital migrar`: migraciones, arranque y (con `DB_APP_PASSWORD`)
/// el login de la aplicación, todo como dueño del esquema; después termina.
/// Siempre migra y arranca: MIGRAR_AL_INICIO y ARRANQUE_AL_INICIO son del servidor.
async fn migrar() -> anyhow::Result<()> {
    let cfg = Config::para_migrar()?;
    let login = LoginAplicacion::desde_entorno()?;
    let url = cfg.database_url_propietario.as_deref().unwrap_or(&cfg.database_url);
    let propietario = db::crear_pool_propietario(url).await.context("no se pudo conectar como dueño del esquema")?;
    let resultado = async {
        preparar_base(&propietario, &cfg, true, true).await?;
        match &login {
            Some(login) => bootstrap::asegurar_login_aplicacion(&propietario, login)
                .await
                .with_context(|| format!("no se pudo aprovisionar el login de la aplicación {}", login.usuario())),
            None => {
                tracing::info!("DB_APP_PASSWORD no está definida: no se aprovisiona el login de la aplicación");
                Ok(())
            }
        }
    }
    .await;
    propietario.close().await;
    resultado?;
    tracing::info!("migraciones y arranque completos");
    Ok(())
}

async fn preparar_base(propietario: &PgPool, cfg: &Config, migrar: bool, arrancar: bool) -> anyhow::Result<()> {
    if migrar {
        sqlx::migrate!("../migrations").run(propietario).await.context("falló la migración")?;
    }
    if arrancar {
        // `context` conserva la cadena de causas (tabla, permiso, restricción).
        bootstrap::ejecutar(propietario, cfg).await.context("arranque")?;
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
