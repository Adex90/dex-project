//! Hallazgo #7: límites del servidor HTTP contra un socket real.

mod comun;

use std::{net::SocketAddr, time::Duration};

use comun::*;
use sqlx::{postgres::PgPoolOptions, PgPool};
use synthetic_capital::{
    cli,
    config::{Config, LimitesHttp},
    routes, servidor, AppState,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    task::JoinHandle,
    time::{timeout, Instant},
};

struct Servidor {
    dir: SocketAddr,
    apagar: oneshot::Sender<()>,
    tarea: JoinHandle<()>,
}

async fn arrancar(pool: &PgPool, limites: LimitesHttp) -> Servidor {
    let mut cfg = preparar(pool).await;
    cfg.http = limites.clone();
    arrancar_con(AppState::nuevo(pool.clone(), cfg), limites).await
}

async fn arrancar_con(estado: AppState, limites: LimitesHttp) -> Servidor {
    let app = routes::router(estado);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = listener.local_addr().unwrap();
    let (apagar, senal) = oneshot::channel::<()>();
    let tarea = tokio::spawn(async move {
        servidor::servir(listener, app, &limites, async {
            let _ = senal.await;
        })
        .await;
    });
    Servidor { dir, apagar, tarea }
}

fn limites() -> LimitesHttp {
    LimitesHttp {
        cabeceras_seg: 1,
        cuerpo_seg: 1,
        peticion_seg: 5,
        max_conexiones: 64,
        apagado_seg: 1,
        proxy_confiable: false,
    }
}

/// Lee hasta que el servidor cierre la conexión; devuelve lo recibido.
async fn leer_hasta_cierre(s: &mut TcpStream, plazo: Duration) -> Option<String> {
    let mut buf = Vec::new();
    match timeout(plazo, s.read_to_end(&mut buf)).await {
        Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
        Err(_) => None,
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn conexiones_ociosas_o_con_cabeceras_incompletas_se_cierran(pool: PgPool) {
    let srv = arrancar(&pool, limites()).await;
    let inicio = Instant::now();
    let mut muda = TcpStream::connect(srv.dir).await.unwrap();
    let mut parcial = TcpStream::connect(srv.dir).await.unwrap();
    parcial.write_all(b"GET /salud HTTP/1.1\r\nHost: x\r\n").await.unwrap();
    assert!(leer_hasta_cierre(&mut muda, Duration::from_secs(4)).await.is_some(), "conexión sin bytes no se cerró");
    assert!(
        leer_hasta_cierre(&mut parcial, Duration::from_secs(4)).await.is_some(),
        "cabeceras parciales no se cerraron"
    );
    assert!(inicio.elapsed() < Duration::from_secs(4));

    // Keep-alive: tras una respuesta, la conexión ociosa también se cierra.
    let mut viva = TcpStream::connect(srv.dir).await.unwrap();
    viva.write_all(b"GET /salud HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let recibido = leer_hasta_cierre(&mut viva, Duration::from_secs(4)).await.expect("keep-alive ocioso no se cerró");
    assert!(recibido.starts_with("HTTP/1.1 200"), "{recibido}");
}

#[sqlx::test(migrations = "../migrations")]
async fn cuerpo_lento_recibe_408_en_json(pool: PgPool) {
    let srv = arrancar(&pool, limites()).await;
    let mut s = TcpStream::connect(srv.dir).await.unwrap();
    s.write_all(
        b"POST /api/v1/auth/login HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{\"em",
    )
    .await
    .unwrap();
    let inicio = Instant::now();
    let mut buf = vec![0u8; 4096];
    let n = timeout(Duration::from_secs(4), s.read(&mut buf)).await.expect("sin respuesta al cuerpo lento").unwrap();
    let respuesta = String::from_utf8_lossy(&buf[..n]);
    assert!(respuesta.starts_with("HTTP/1.1 408"), "{respuesta}");
    assert!(respuesta.contains("\"TIEMPO_AGOTADO\""), "{respuesta}");
    assert!(inicio.elapsed() < Duration::from_secs(3));
}

/// Al llegar al tope de conexiones el servidor deja de aceptar en lugar de
/// agotar descriptores; atiende en cuanto se libera una.
#[sqlx::test(migrations = "../migrations")]
async fn tope_de_conexiones(pool: PgPool) {
    let srv = arrancar(&pool, LimitesHttp { max_conexiones: 2, cabeceras_seg: 30, ..limites() }).await;
    let ocupada_1 = TcpStream::connect(srv.dir).await.unwrap();
    let ocupada_2 = TcpStream::connect(srv.dir).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut tercera = TcpStream::connect(srv.dir).await.unwrap(); // queda en el backlog
    tercera.write_all(b"GET /salud HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut buf = vec![0u8; 1024];
    assert!(timeout(Duration::from_millis(500), tercera.read(&mut buf)).await.is_err(), "no debía atenderse aún");
    drop(ocupada_1);
    let n = timeout(Duration::from_secs(3), tercera.read(&mut buf)).await.expect("no se atendió al liberar").unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    drop(ocupada_2);
}

/// El apagado no espera indefinidamente a un cliente lento.
#[sqlx::test(migrations = "../migrations")]
async fn apagado_con_plazo(pool: PgPool) {
    let srv = arrancar(&pool, LimitesHttp { cuerpo_seg: 60, peticion_seg: 60, ..limites() }).await;
    let mut lento = TcpStream::connect(srv.dir).await.unwrap();
    lento
        .write_all(b"POST /api/v1/auth/login HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let inicio = Instant::now();
    srv.apagar.send(()).unwrap();
    timeout(Duration::from_secs(5), srv.tarea).await.expect("el apagado no terminó").unwrap();
    assert!(inicio.elapsed() < Duration::from_secs(3), "apagado tardó {:?}", inicio.elapsed());
    drop(lento);
}

/// GET con `Connection: close`; devuelve la respuesta completa.
async fn obtener(dir: SocketAddr, ruta: &str) -> String {
    let mut s = TcpStream::connect(dir).await.unwrap();
    s.write_all(format!("GET {ruta} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    leer_hasta_cierre(&mut s, Duration::from_secs(5)).await.expect("sin respuesta")
}

/// Las sondas de `synthetic-capital salud` contra el router real.
#[sqlx::test(migrations = "../migrations")]
async fn vivo_y_salud_responden_200(pool: PgPool) {
    let srv = arrancar(&pool, limites()).await;
    for ruta in ["/vivo", "/salud"] {
        let dir = srv.dir;
        let codigo = tokio::task::spawn_blocking(move || cli::sondear(dir, ruta, Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(codigo, 200, "{ruta}");
    }
    let vivo = obtener(srv.dir, "/vivo").await;
    assert!(vivo.starts_with("HTTP/1.1 200") && vivo.contains(r#"{"estado":"ok"}"#), "{vivo}");
}

/// /vivo (salud del contenedor) no toca la base de datos: con PostgreSQL
/// inalcanzable responde 200, mientras /salud (preparado) responde 503.
#[tokio::test]
async fn vivo_no_consulta_la_base_de_datos() {
    let puerto = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy(&format!("postgres://nadie@127.0.0.1:{puerto}/nada"))
        .unwrap();
    let srv = arrancar_con(AppState::nuevo(pool, Config::para_pruebas("")), limites()).await;
    let inicio = Instant::now();
    let vivo = obtener(srv.dir, "/vivo").await;
    assert!(vivo.starts_with("HTTP/1.1 200"), "{vivo}");
    assert!(inicio.elapsed() < Duration::from_millis(500), "{:?}", inicio.elapsed());
    let salud = obtener(srv.dir, "/salud").await;
    assert!(salud.starts_with("HTTP/1.1 503") && salud.contains(r#""base_de_datos":"error""#), "{salud}");
}
