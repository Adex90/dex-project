//! Servidor HTTP/1.1 con límites explícitos (en lugar de `axum::serve`, que no
//! fija ninguno):
//!
//! - **Cabeceras:** una conexión que no envía una petición completa en
//!   `HTTP_CABECERAS_SEG` se cierra, también si está ociosa entre peticiones
//!   keep-alive o si nunca envió un byte.
//! - **Conexiones:** como máximo `HTTP_MAX_CONEXIONES` a la vez. Al llegar al
//!   tope se deja de aceptar (el kernel encola en el backlog) en vez de agotar
//!   los descriptores de archivo, que también necesita el pool de PostgreSQL.
//! - **Apagado:** al recibir la señal se deja de aceptar, se pide a cada
//!   conexión que termine su petición en curso y se espera como máximo
//!   `HTTP_APAGADO_SEG`; después se cierran las que queden.
//!
//! Cuerpo y duración de cada petición se limitan en el router
//! (`routes::router`). En producción conviene además un proxy inverso con TLS.

use std::{future::Future, io, sync::Arc, time::Duration};

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::{
    net::TcpListener,
    sync::{watch, Semaphore},
};

use crate::config::LimitesHttp;

pub async fn servir(listener: TcpListener, app: Router, limites: &LimitesHttp, apagado: impl Future<Output = ()>) {
    let cupo = Arc::new(Semaphore::new(limites.max_conexiones));
    let (avisar, aviso) = watch::channel(());
    let mut constructor = http1::Builder::new();
    constructor
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(limites.cabeceras_seg))
        .keep_alive(true);
    tokio::pin!(apagado);

    loop {
        let permiso = tokio::select! {
            p = Arc::clone(&cupo).acquire_owned() => p.expect("el semáforo de conexiones nunca se cierra"),
            () = &mut apagado => break,
        };
        let (socket, remoto) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) if es_error_de_conexion(&e) => continue,
                Err(e) => {
                    // EMFILE/ENFILE: esperar a que se liberen descriptores.
                    tracing::error!(error = %e, "no se pudo aceptar la conexión");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            },
            () = &mut apagado => break,
        };
        let _ = socket.set_nodelay(true);
        let servicio = TowerToHyperService::new(app.clone());
        let constructor = constructor.clone();
        let mut aviso = aviso.clone();
        tokio::spawn(async move {
            // El permiso vive lo mismo que la conexión.
            let _permiso = permiso;
            let conexion = constructor.serve_connection(TokioIo::new(socket), servicio);
            tokio::pin!(conexion);
            let resultado = tokio::select! {
                r = conexion.as_mut() => r,
                _ = aviso.changed() => {
                    conexion.as_mut().graceful_shutdown();
                    conexion.await
                }
            };
            if let Err(e) = resultado {
                tracing::debug!(%remoto, error = %e, "conexión cerrada con error");
            }
        });
    }

    drop(listener);
    let _ = avisar.send(());
    let total = u32::try_from(limites.max_conexiones).unwrap_or(u32::MAX);
    let plazo = Duration::from_secs(limites.apagado_seg);
    // Cada conexión devuelve su permiso al cerrar: tenerlos todos = no queda ninguna.
    let todas = tokio::time::timeout(plazo, cupo.acquire_many(total)).await;
    match todas {
        Ok(_) => tracing::info!("todas las conexiones terminaron"),
        Err(_) => tracing::warn!(
            abiertas = limites.max_conexiones - cupo.available_permits(),
            segundos = plazo.as_secs(),
            "venció el plazo de apagado; se cierran las conexiones restantes"
        ),
    }
}

fn es_error_de_conexion(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset
    )
}
