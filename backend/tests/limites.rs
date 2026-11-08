//! Límite de peticiones por IP (`routes/limites.rs`), encendido y con cuotas
//! chicas: cada petición lleva la IP de la conexión como la pone el servidor
//! (`ConnectInfo`).

mod comun;

use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    Router,
};
use comun::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    config::{Cuota, LimitesPorIp},
    routes, AppState,
};
use tower::ServiceExt;

/// Ráfagas distintas por grupo, para saber qué cuota cortó.
fn cuotas() -> LimitesPorIp {
    LimitesPorIp {
        activo: true,
        auth: Cuota { por_minuto: 1, rafaga: 2 },
        kyc: Cuota { por_minuto: 1, rafaga: 1 },
        api: Cuota { por_minuto: 1, rafaga: 3 },
        webhook: Cuota { por_minuto: 1, rafaga: 4 },
    }
}

async fn app(pool: &PgPool, proxy_confiable: bool, por_ip: LimitesPorIp) -> Router {
    let mut cfg = preparar(pool).await;
    cfg.http.proxy_confiable = proxy_confiable;
    cfg.http.por_ip = por_ip;
    routes::router(AppState::nuevo(pool.clone(), cfg))
}

struct Respuesta {
    status: StatusCode,
    reintentar: Option<u64>,
    cuerpo: Value,
}

async fn pedir(
    app: &Router,
    metodo: &str,
    ruta: &str,
    desde: &str,
    xff: Option<&str>,
    cuerpo: Option<Value>,
) -> Respuesta {
    let mut req = Request::builder().method(metodo).uri(ruta);
    if let Some(x) = xff {
        req = req.header("x-forwarded-for", x);
    }
    let body = match cuerpo {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let mut req = req.body(body).unwrap();
    req.extensions_mut().insert(ConnectInfo(desde.parse::<SocketAddr>().unwrap()));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let reintentar = resp.headers().get("retry-after").map(|v| v.to_str().unwrap().parse().unwrap());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    Respuesta { status, reintentar, cuerpo: serde_json::from_slice(&bytes).unwrap_or(Value::Null) }
}

async fn login(app: &Router, desde: &str, xff: Option<&str>) -> Respuesta {
    let cuerpo = json!({ "email": "nadie@example.mx", "password": "incorrecta-0123456789" });
    pedir(app, "POST", "/api/v1/auth/login", desde, xff, Some(cuerpo)).await
}

fn limitada(r: &Respuesta) {
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.cuerpo);
    assert_eq!(r.cuerpo["error"]["codigo"], "REINTENTAR", "{}", r.cuerpo);
    assert!(r.reintentar.is_some_and(|s| s >= 1), "sin Retry-After");
}

fn atendida(r: &Respuesta) {
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.cuerpo);
}

#[sqlx::test(migrations = "../migrations")]
async fn el_inicio_de_sesion_se_limita_por_ip(pool: PgPool) {
    let app = app(&pool, false, cuotas()).await;
    for _ in 0..2 {
        let r = login(&app, "203.0.113.7:40000", None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.cuerpo);
    }
    limitada(&login(&app, "203.0.113.7:40001", None).await);
    // Otra IP tiene su propia cuenta.
    let r = login(&app, "198.51.100.9:40000", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.cuerpo);
}

#[sqlx::test(migrations = "../migrations")]
async fn detras_del_proxy_cuenta_la_ip_que_agrega_el_proxy(pool: PgPool) {
    let app = app(&pool, true, cuotas()).await;
    let proxy = "10.0.1.20:51000";
    // El cliente escribe lo que quiera antes; el proxy agrega la IP real al final.
    atendida(&login(&app, proxy, Some("1.1.1.1, 203.0.113.7")).await);
    atendida(&login(&app, proxy, Some("8.8.8.8, 203.0.113.7")).await);
    limitada(&login(&app, proxy, Some("9.9.9.9, 203.0.113.7")).await);
    // Otro cliente detrás del mismo proxy no comparte la cuenta.
    atendida(&login(&app, proxy, Some("203.0.113.7, 198.51.100.9")).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn sin_proxy_confiable_la_cabecera_no_cuenta(pool: PgPool) {
    let app = app(&pool, false, cuotas()).await;
    atendida(&login(&app, "203.0.113.7:40000", Some("198.51.100.1")).await);
    atendida(&login(&app, "203.0.113.7:40000", Some("198.51.100.2")).await);
    // Rotar X-Forwarded-For no da una cuenta nueva: cuenta la conexión.
    limitada(&login(&app, "203.0.113.7:40000", Some("198.51.100.3")).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn ipv6_cuenta_por_su_64(pool: PgPool) {
    let app = app(&pool, false, cuotas()).await;
    atendida(&login(&app, "[2001:db8:1:2::1]:40000", None).await);
    atendida(&login(&app, "[2001:db8:1:2:ffff::9]:40000", None).await);
    limitada(&login(&app, "[2001:db8:1:2:abcd::1]:40000", None).await);
    atendida(&login(&app, "[2001:db8:1:3::1]:40000", None).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn la_verificacion_kyc_se_limita_antes_de_autenticar(pool: PgPool) {
    let app = app(&pool, false, cuotas()).await;
    let r = pedir(&app, "POST", "/api/v1/kyc/verificar", "203.0.113.7:40000", None, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.cuerpo);
    limitada(&pedir(&app, "POST", "/api/v1/kyc/verificar", "203.0.113.7:40000", None, Some(json!({}))).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn los_webhooks_tienen_su_propio_limite(pool: PgPool) {
    let app = app(&pool, false, cuotas()).await;
    let ip = "203.0.113.7:40000";
    for _ in 0..3 {
        atendida(&pedir(&app, "GET", "/api/v1/reserva/estado", ip, None, None).await);
    }
    limitada(&pedir(&app, "GET", "/api/v1/reserva/estado", ip, None, None).await);
    // La cuota general está agotada, pero los webhooks no cuentan en ella:
    // la firma inválida llega al handler (401), hasta su propia ráfaga.
    for _ in 0..4 {
        let r = pedir(&app, "POST", "/api/v1/spei/webhook/estado", ip, None, Some(json!({}))).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.cuerpo);
    }
    limitada(&pedir(&app, "POST", "/api/v1/spei/webhook/estado", ip, None, Some(json!({}))).await);
}

#[sqlx::test(migrations = "../migrations")]
async fn las_sondas_de_salud_no_se_limitan(pool: PgPool) {
    let app = app(&pool, false, cuotas()).await;
    for _ in 0..10 {
        let r = pedir(&app, "GET", "/salud", "203.0.113.7:40000", None, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.cuerpo);
        let r = pedir(&app, "GET", "/vivo", "203.0.113.7:40000", None, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.cuerpo);
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn apagados_no_limitan(pool: PgPool) {
    let app = app(&pool, false, LimitesPorIp { activo: false, ..cuotas() }).await;
    for _ in 0..6 {
        let r = login(&app, "203.0.113.7:40000", None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.cuerpo);
    }
}
