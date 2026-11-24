//! La lista de seguimiento, las alertas de precio (migración 0024,
//! `mercado::seguimiento`, `mercado::alertas`) y el historial de órdenes
//! (`mercado::historial_ordenes`) por la API y el motor: seguir y dejar de
//! seguir (idempotente con el estado pedido), la ficha de cada activo, los
//! límites, el alta y la cancelación de alertas con sus reglas, el disparo
//! con el precio del catálogo, un congelamiento PLD que no cambia nada, y el
//! historial unificado con sus páginas. Las reglas de la base de datos están
//! en tests/db/0024_seguimiento_alertas.sql.

mod comun;

use axum::http::StatusCode;
use chrono::DateTime;
use comun::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    config::Config,
    kyc::Registro,
    mercado::{
        alertas::{self, ResumenAlertas},
        pendiente,
    },
    models::kyc::RolUsuario,
    routes, AppState,
};
use uuid::Uuid;

struct Mercado {
    app: axum::Router,
    estado: AppState,
}

impl Mercado {
    fn nuevo(pool: &PgPool, cfg: Config) -> Self {
        let estado = AppState::nuevo(pool.clone(), cfg);
        Self { app: routes::router(estado.clone()), estado }
    }

    async fn token(&self, cliente_id: Uuid) -> String {
        token_de(&self.estado, cliente_id, RolUsuario::Cliente, false).await
    }

    async fn get(&self, token: &str, ruta: &str) -> (StatusCode, Value) {
        llamar_json(&self.app, "GET", ruta, Some(token), None).await
    }

    async fn post(&self, token: &str, ruta: &str, cuerpo: Option<Value>) -> (StatusCode, Value) {
        llamar_json(&self.app, "POST", ruta, Some(token), cuerpo).await
    }

    /// GET que debe responder 200.
    async fn leer(&self, token: &str, ruta: &str) -> Value {
        let (s, v) = self.get(token, ruta).await;
        assert_eq!(s, StatusCode::OK, "{ruta}: {v}");
        v
    }

    async fn seguir(&self, token: &str, ticker: &str, cuerpo: Option<Value>) -> (StatusCode, Value) {
        self.post(token, &format!("/api/v1/market/watchlist/{ticker}/toggle"), cuerpo).await
    }

    async fn alerta(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        self.post(token, "/api/v1/market/alerts", Some(cuerpo)).await
    }

    /// Una orden que debe colocarse (201): la respuesta.
    async fn ordenada(&self, token: &str, cuerpo: Value) -> Value {
        let (s, r) = self.post(token, "/api/v1/market/order", Some(cuerpo.clone())).await;
        assert_eq!(s, StatusCode::CREATED, "{cuerpo}: {r}");
        r
    }

    async fn cancelar_alerta(&self, token: &str, id: &Value) -> (StatusCode, Value) {
        let ruta = format!("/api/v1/market/alerts/{}/cancel", id.as_str().unwrap());
        self.post(token, &ruta, None).await
    }
}

fn alerta(ticker: &str, objetivo: &str, direccion: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "target_price": objetivo, "direction": direccion,
            "idempotency_key": format!("prueba-{etiqueta}") })
}

fn orden(cuerpo: Value, etiqueta: &str) -> Value {
    let mut o = cuerpo;
    o["idempotency_key"] = json!(format!("prueba-{etiqueta}"));
    o
}

async fn version_ordenes(pool: &PgPool, cuenta: Uuid) -> i64 {
    sqlx::query_scalar("SELECT version_ordenes FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// El oráculo mueve el precio (como dueño del esquema).
async fn precio(pool: &PgPool, ticker: &str, nuevo: &str) {
    sqlx::query("UPDATE mercado.activos SET precio = $2::numeric WHERE ticker = $1")
        .bind(ticker)
        .bind(nuevo)
        .execute(pool)
        .await
        .unwrap();
}

/// `n` activos de prueba más en el catálogo (TST1, TST2…), para los máximos.
async fn activos_de_prueba(pool: &PgPool, n: i32) {
    sqlx::query(
        "INSERT INTO mercado.activos (ticker, nombre, clase, decimales_cantidad, precio, precio_cierre_previo, \
         fuente_precio) SELECT 'TST' || i, 'Activo de prueba ' || i, 'FONDO', 4, 100, 100, 'SIMULADO' \
         FROM generate_series(1, $1) AS i",
    )
    .bind(n)
    .execute(pool)
    .await
    .unwrap();
}

async fn con_efectivo(pool: &PgPool, cfg: &Config, nombre: &str, monto: &str) -> Registro {
    let r = cliente(pool, cfg, nombre).await;
    depositar(pool, cfg, r.cuenta_ifpe.id, monto).await;
    r
}

fn resumen(clientes: usize, disparadas: usize) -> ResumenAlertas {
    ResumenAlertas { clientes, disparadas, errores: 0 }
}

/// Un token del personal (oficial de cumplimiento con su segundo factor).
async fn token_interno(pool: &PgPool, m: &Mercado) -> String {
    let (oficial_id, _) = oficial(pool, "Olga").await;
    activar_mfa(&m.estado, oficial_id).await;
    token_de(&m.estado, oficial_id, RolUsuario::Cumplimiento, true).await
}

// -----------------------------------------------------------------------------
// La lista de seguimiento
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn la_lista_de_seguimiento(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let cuenta = ana.cuenta_ifpe.id;
    let token = m.token(ana.cliente.id).await;
    assert_eq!(m.leer(&token, "/api/v1/market/watchlist").await, json!({ "items": [] }));

    // Sin cuerpo, alterna: la sigue. La bóveda avisa.
    let version = version_ordenes(&pool, cuenta).await;
    let (s, e) = m.seguir(&token, "AUG1", None).await;
    assert_eq!((s, &e["ticker"], &e["watched"]), (StatusCode::OK, &json!("AUG1"), &json!(true)), "{e}");
    assert!(DateTime::parse_from_rfc3339(e["added_at"].as_str().unwrap()).is_ok(), "{e}");
    assert_eq!(version_ordenes(&pool, cuenta).await, version + 1);
    // La lista la trae con su ficha, la misma del catálogo.
    let catalogo = m.leer(&token, "/api/v1/market/assets").await;
    let ficha = catalogo["assets"].as_array().unwrap().iter().find(|a| a["ticker"] == "AUG1").unwrap().clone();
    assert_eq!(
        m.leer(&token, "/api/v1/market/watchlist").await,
        json!({ "items": [{ "ticker": "AUG1", "added_at": e["added_at"], "asset": ficha }] })
    );

    // Con el estado pedido, repetirlo no cambia nada (ni avisa otra vez).
    let (s, otra) = m.seguir(&token, "AUG1", Some(json!({ "watched": true }))).await;
    assert_eq!((s, &otra), (StatusCode::OK, &e));
    assert_eq!(version_ordenes(&pool, cuenta).await, version + 1);
    // La plata después: la más reciente primero.
    m.seguir(&token, "AGT1", Some(json!({ "watched": true }))).await;
    let l = m.leer(&token, "/api/v1/market/watchlist").await;
    let tickers: Vec<&str> = l["items"].as_array().unwrap().iter().map(|i| i["ticker"].as_str().unwrap()).collect();
    assert_eq!(tickers, ["AGT1", "AUG1"]);
    // Sin cuerpo otra vez: deja de seguirla.
    let (s, e) = m.seguir(&token, "AUG1", None).await;
    assert_eq!((s, e), (StatusCode::OK, json!({ "ticker": "AUG1", "watched": false, "added_at": null })));
    let (s, e) = m.seguir(&token, "AUG1", Some(json!({ "watched": false }))).await;
    assert_eq!((s, &e["watched"]), (StatusCode::OK, &json!(false)), "dejar de seguir dos veces: igual");
    assert_eq!(m.leer(&token, "/api/v1/market/watchlist").await["items"].as_array().unwrap().len(), 1);
    assert_eq!(version_ordenes(&pool, cuenta).await, version + 3);

    // Lo que no es un activo, o no se pide bien.
    let (q, n) = (StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND);
    for (ticker, cuerpo, status) in [
        ("XAU", None, n),
        ("aug1", None, n),
        ("AUG1", Some(json!({ "watched": true, "rol": "OPERADOR" })), q),
        ("AUG1", Some(json!({ "watched": "yes" })), q),
        ("AUG1", Some(json!([1])), q),
    ] {
        let (s, e) = m.seguir(&token, ticker, cuerpo.clone()).await;
        assert_eq!(s, status, "{ticker} {cuerpo:?}: {e}");
    }
    // El personal no tiene lista; cada cliente ve la suya.
    let interno = token_interno(&pool, &m).await;
    assert_eq!(m.get(&interno, "/api/v1/market/watchlist").await.0, StatusCode::FORBIDDEN);
    assert_eq!(m.seguir(&interno, "AUG1", None).await.0, StatusCode::FORBIDDEN);
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_beto = m.token(beto.cliente.id).await;
    assert_eq!(m.leer(&tok_beto, "/api/v1/market/watchlist").await, json!({ "items": [] }));

    // Sin MODO_DEMO, un precio simulado no se lista ni se sigue; dejar de seguir, sí.
    let produccion = Mercado::nuevo(&pool, Config { modo_demo: false, ..cfg.clone() });
    let tok_prod = produccion.token(ana.cliente.id).await;
    assert_eq!(produccion.leer(&tok_prod, "/api/v1/market/watchlist").await, json!({ "items": [] }));
    let (s, e) = produccion.seguir(&tok_prod, "STSLA", Some(json!({ "watched": true }))).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::SERVICE_UNAVAILABLE, &json!("MERCADO_NO_DISPONIBLE")));
    let (s, e) = produccion.seguir(&tok_prod, "AGT1", Some(json!({ "watched": false }))).await;
    assert_eq!((s, &e["watched"]), (StatusCode::OK, &json!(false)), "{e}");

    // A lo más 50 activos seguidos.
    activos_de_prueba(&pool, 50).await;
    for i in 1..=50 {
        let (s, e) = m.seguir(&tok_beto, &format!("TST{i}"), Some(json!({ "watched": true }))).await;
        assert_eq!(s, StatusCode::OK, "{i}: {e}");
    }
    let (s, e) = m.seguir(&tok_beto, "AUG1", Some(json!({ "watched": true }))).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("LIMITE_SEGUIMIENTO")));
    m.seguir(&tok_beto, "TST1", None).await;
    assert_eq!(m.seguir(&tok_beto, "AUG1", None).await.0, StatusCode::OK, "deja uno y cabe otro");
}

// -----------------------------------------------------------------------------
// Las alertas de precio: alta, reglas, cancelación
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn alta_y_cancelacion_de_alertas(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let cuenta = ana.cuenta_ifpe.id;
    let token = m.token(ana.cliente.id).await;
    let version = version_ordenes(&pool, cuenta).await;

    // Avísame si el oro (1 612.40) sube a 1 700.
    let cuerpo = alerta("AUG1", "1700.0000", "above", "oro-1700");
    let (s, a) = m.alerta(&token, cuerpo.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert!(DateTime::parse_from_rfc3339(a["created_at"].as_str().unwrap()).is_ok(), "{a}");
    assert_eq!(
        a,
        json!({
            "alert_id": a["alert_id"],
            "ticker": "AUG1",
            "direction": "above",
            "target_price": "1700.00000000",
            "status": "active",
            "reference_price": "1612.40000000",
            "triggered_price": null,
            "created_at": a["created_at"],
            "triggered_at": null,
            "cancelled_at": null,
        })
    );
    assert_eq!(version_ordenes(&pool, cuenta).await, version + 1, "la bóveda avisa");
    assert_eq!(m.leer(&token, "/api/v1/market/alerts").await, json!({ "alerts": [a.clone()] }));
    // La misma clave: la misma alerta (200); con otro objetivo, 409.
    assert_eq!(m.alerta(&token, cuerpo).await, (StatusCode::OK, a.clone()));
    let (s, e) = m.alerta(&token, alerta("AUG1", "1800", "above", "oro-1700")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("CLAVE_IDEMPOTENCIA_REUTILIZADA")));

    // Las reglas.
    let (q, u) = (StatusCode::BAD_REQUEST, StatusCode::UNPROCESSABLE_ENTITY);
    let casos: Vec<(Value, StatusCode, &str)> = vec![
        // Un objetivo que el precio ya alcanzó, de cualquier lado.
        (alerta("AUG1", "1612.40", "above", "r1"), u, "AVISO_YA_ALCANZADO"),
        (alerta("AUG1", "1500", "above", "r2"), u, "AVISO_YA_ALCANZADO"),
        (alerta("AUG1", "1650", "below", "r3"), u, "AVISO_YA_ALCANZADO"),
        // La forma.
        (alerta("AUG1", "0", "above", "r4"), q, "VALIDACION"),
        (alerta("AUG1", "-5", "above", "r5"), q, "VALIDACION"),
        (alerta("AUG1", "1700.123456789", "above", "r6"), q, "VALIDACION"),
        (alerta("AUG1", "1700", "cross", "r7"), q, ""),
        (
            json!({ "ticker": "AUG1", "target_price": 1700, "direction": "above", "idempotency_key": "prueba-r8" }),
            q,
            "",
        ),
        (
            json!({ "ticker": "AUG1", "target_price": "1700", "direction": "above", "idempotency_key": "prueba-r9",
                 "status": "triggered" }),
            q,
            "",
        ),
        (
            json!({ "ticker": "AUG1", "target_price": "1700", "direction": "above", "idempotency_key": "corta" }),
            q,
            "VALIDACION",
        ),
        (alerta("XAU", "1700", "above", "r10"), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
    ];
    for (cuerpo, status, codigo) in casos {
        let (s, e) = m.alerta(&token, cuerpo.clone()).await;
        assert_eq!(s, status, "{cuerpo}: {e}");
        if !codigo.is_empty() {
            assert_eq!(e["error"]["codigo"], codigo, "{cuerpo}: {e}");
        }
    }
    let produccion = Mercado::nuevo(&pool, Config { modo_demo: false, ..cfg.clone() });
    let (s, e) = produccion.alerta(&produccion.token(ana.cliente.id).await, alerta("AUG1", "1700", "above", "p")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::SERVICE_UNAVAILABLE, &json!("MERCADO_NO_DISPONIBLE")));
    let interno = token_interno(&pool, &m).await;
    assert_eq!(m.alerta(&interno, alerta("AUG1", "1700", "above", "interno")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(m.get(&interno, "/api/v1/market/alerts").await.0, StatusCode::FORBIDDEN);
    assert_eq!(m.leer(&token, "/api/v1/market/alerts").await["alerts"].as_array().unwrap().len(), 1);

    // Cancelar: la de otro cliente no existe; repetirlo devuelve la misma.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_beto = m.token(beto.cliente.id).await;
    assert_eq!(m.cancelar_alerta(&tok_beto, &a["alert_id"]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(m.cancelar_alerta(&token, &json!(Uuid::new_v4().to_string())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(m.post(&token, "/api/v1/market/alerts/no-es-un-id/cancel", None).await.0, StatusCode::BAD_REQUEST);
    let (s, c) = m.cancelar_alerta(&token, &a["alert_id"]).await;
    assert_eq!((s, &c["status"]), (StatusCode::OK, &json!("cancelled")), "{c}");
    assert!(DateTime::parse_from_rfc3339(c["cancelled_at"].as_str().unwrap()).is_ok());
    assert_eq!(m.cancelar_alerta(&token, &a["alert_id"]).await, (StatusCode::OK, c));
    assert_eq!(m.leer(&token, "/api/v1/market/alerts").await, json!({ "alerts": [] }), "las canceladas no se listan");
    assert_eq!(m.leer(&tok_beto, "/api/v1/market/alerts").await, json!({ "alerts": [] }));

    // A lo más 50 activas; las cerradas no cuentan.
    let mut primera = Value::Null;
    for i in 1..=50 {
        let (s, a) = m.alerta(&tok_beto, alerta("AUG1", &format!("1700.{i:02}"), "above", &format!("tope-{i}"))).await;
        assert_eq!(s, StatusCode::CREATED, "{i}: {a}");
        if i == 1 {
            primera = a["alert_id"].clone();
        }
    }
    let (s, e) = m.alerta(&tok_beto, alerta("AUG1", "1800", "above", "tope-de-mas")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("LIMITE_AVISOS_PRECIO")));
    assert_eq!(m.cancelar_alerta(&tok_beto, &primera).await.0, StatusCode::OK);
    assert_eq!(m.alerta(&tok_beto, alerta("AUG1", "1800", "above", "tope-otra")).await.0, StatusCode::CREATED);
}

// -----------------------------------------------------------------------------
// El motor: dispara lo que el precio alcanzó
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_motor_dispara_las_alertas_alcanzadas(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let (tok_ana, tok_beto) = (m.token(ana.cliente.id).await, m.token(beto.cliente.id).await);
    let sube = m.alerta(&tok_ana, alerta("AUG1", "1700", "above", "a-sube")).await.1;
    let baja = m.alerta(&tok_ana, alerta("AUG1", "1500", "below", "a-baja")).await.1;
    let plata = m.alerta(&tok_ana, alerta("AGT1", "600", "above", "a-plata")).await.1;
    let de_beto = m.alerta(&tok_beto, alerta("AUG1", "1650", "above", "b-sube")).await.1;
    assert_eq!(alertas::evaluar(&pool, &cfg).await.unwrap(), resumen(0, 0), "nada alcanzado todavía");

    // El oro sube a 1 701.20: se disparan las dos al alza, de Ana y de Beto.
    precio(&pool, "AUG1", "1701.20").await;
    let (va, vb) =
        (version_ordenes(&pool, ana.cuenta_ifpe.id).await, version_ordenes(&pool, beto.cuenta_ifpe.id).await);
    assert_eq!(alertas::evaluar(&pool, &cfg).await.unwrap(), resumen(2, 2));
    assert_eq!(version_ordenes(&pool, ana.cuenta_ifpe.id).await, va + 1, "la bóveda de Ana avisa");
    assert_eq!(version_ordenes(&pool, beto.cuenta_ifpe.id).await, vb + 1);
    let l = m.leer(&tok_ana, "/api/v1/market/alerts").await;
    let disparada = l["alerts"].as_array().unwrap().iter().find(|a| a["alert_id"] == sube["alert_id"]).unwrap();
    assert_eq!(
        (&disparada["status"], &disparada["triggered_price"]),
        (&json!("triggered"), &json!("1701.20000000")),
        "{disparada}"
    );
    assert!(DateTime::parse_from_rfc3339(disparada["triggered_at"].as_str().unwrap()).is_ok());
    // La disparada primero; las otras dos siguen activas.
    assert_eq!(l["alerts"][0]["alert_id"], sube["alert_id"]);
    let activas: Vec<&Value> = l["alerts"].as_array().unwrap().iter().filter(|a| a["status"] == "active").collect();
    assert_eq!(activas.len(), 2);
    let de_beto_ahora = m.leer(&tok_beto, "/api/v1/market/alerts").await;
    assert_eq!(
        (&de_beto_ahora["alerts"][0]["alert_id"], &de_beto_ahora["alerts"][0]["status"]),
        (&de_beto["alert_id"], &json!("triggered"))
    );
    // Una disparada no se cancela, y no se vuelve a disparar.
    let (s, e) = m.cancelar_alerta(&tok_ana, &sube["alert_id"]).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("AVISO_DISPARADO")));
    assert_eq!(alertas::evaluar(&pool, &cfg).await.unwrap(), resumen(0, 0));

    // Baja a 1 490: la de la baja.
    precio(&pool, "AUG1", "1490").await;
    assert_eq!(alertas::evaluar(&pool, &cfg).await.unwrap(), resumen(1, 1));
    let l = m.leer(&tok_ana, "/api/v1/market/alerts").await;
    let b = l["alerts"].as_array().unwrap().iter().find(|a| a["alert_id"] == baja["alert_id"]).unwrap();
    assert_eq!((&b["status"], &b["triggered_price"]), (&json!("triggered"), &json!("1490.00000000")));

    // Sin MODO_DEMO, un precio simulado no dispara nada; con él, sí.
    precio(&pool, "AGT1", "610").await;
    let produccion = Config { modo_demo: false, ..cfg.clone() };
    assert_eq!(alertas::evaluar(&pool, &produccion).await.unwrap(), resumen(0, 0));
    // Dos pasadas a la vez (dos réplicas): se dispara una vez.
    let (x, y) = tokio::join!(alertas::evaluar(&pool, &cfg), alertas::evaluar(&pool, &cfg));
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!((x.disparadas + y.disparadas, x.errores + y.errores), (1, 0), "{x:?} {y:?}");
    let l = m.leer(&tok_ana, "/api/v1/market/alerts").await;
    let p = l["alerts"].as_array().unwrap().iter().find(|a| a["alert_id"] == plata["alert_id"]).unwrap();
    assert_eq!((&p["status"], &p["triggered_price"]), (&json!("triggered"), &json!("610.00000000")));
}

#[sqlx::test(migrations = "../migrations")]
async fn un_congelamiento_pld_no_cambia_nada(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let fer = cliente(&pool, &cfg, "Fer").await;
    congelar_por_estructuracion(&pool, &cfg, fer.cuenta_ifpe.id).await;
    assert_eq!(estado_cuenta(&pool, fer.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    let token = m.token(fer.cliente.id).await;

    // Sigue y pone alertas como cualquiera: no mueven dinero, y negarlas delataría.
    assert_eq!(m.seguir(&token, "AUG1", None).await.0, StatusCode::OK);
    let (s, a) = m.alerta(&token, alerta("AUG1", "1700", "above", "congelado")).await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    precio(&pool, "AUG1", "1700").await;
    assert_eq!(alertas::evaluar(&pool, &cfg).await.unwrap(), resumen(1, 1));
    for ruta in ["/api/v1/market/watchlist", "/api/v1/market/alerts", "/api/v1/market/orders/history"] {
        let texto = m.leer(&token, ruta).await.to_string().to_lowercase();
        for palabra in ["aml", "pld", "alerta", "cumplimiento", "uif", "sospech", "congel", "preventiv", "riesgo"] {
            assert!(!texto.contains(palabra), "{ruta} contiene «{palabra}»: {texto}");
        }
    }
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// El historial de órdenes
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_historial_de_ordenes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let token = m.token(ana.cliente.id).await;
    assert_eq!(m.leer(&token, "/api/v1/market/orders/history").await, json!({ "orders": [], "next_cursor": null }));

    // 1. Una compra de mercado: 0.3100 g por 499.84.
    let mercado =
        m.ordenada(&token, orden(json!({ "ticker": "AUG1", "side": "buy", "amount": "500.00" }), "mercado")).await;
    // 2. La primera compra de un plan de ahorro: 10 CETES por 99.44.
    let plan = m
        .ordenada(
            &token,
            orden(
                json!({ "ticker": "CETES28", "side": "buy", "amount": "100.00", "order_type": "savings_plan",
                "frequency": "monthly" }),
                "plan",
            ),
        )
        .await;
    // 3. Una compra límite a 1 600 que espera…
    let limite = m.ordenada(&token, orden(
        json!({ "ticker": "AUG1", "side": "buy", "amount": "1250.00", "order_type": "limit", "limit_price": "1600" }),
        "limite",
    ))
    .await;
    // 4. …una venta límite a 1 700, cancelada…
    let venta = m.ordenada(&token, orden(
        json!({ "ticker": "AUG1", "side": "sell", "amount": "100.00", "order_type": "limit", "limit_price": "1700" }),
        "venta",
    ))
    .await;
    let (s, _) =
        m.post(&token, &format!("/api/v1/market/orders/{}/cancel", venta["order_id"].as_str().unwrap()), None).await;
    assert_eq!(s, StatusCode::OK);
    // 5. …el precio baja a 1 590 y la límite se ejecuta: 0.7861 g por 1 249.90.
    precio(&pool, "AUG1", "1590").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap().ejecutadas, 1);
    // 6. Una compra límite a 1 500; sin KYC al dispararse, se rechaza.
    let rechazada = m.ordenada(&token, orden(
        json!({ "ticker": "AUG1", "side": "buy", "amount": "100.00", "order_type": "limit", "limit_price": "1500" }),
        "rechazada",
    ))
    .await;
    revocar_kyc(&pool, ana.cliente.id).await;
    precio(&pool, "AUG1", "1490").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap().rechazadas, 1);

    let h = m.leer(&token, "/api/v1/market/orders/history").await;
    let o = h["orders"].as_array().unwrap();
    assert_eq!((o.len(), &h["next_cursor"]), (5, &Value::Null), "{h}");
    // De la más reciente a la más antigua.
    let lim_id = limite["order_id"].as_str().unwrap();
    let fill_limite: String =
        sqlx::query_scalar("SELECT orden_ejecutada_id::text FROM mercado.ordenes_pendientes WHERE id = $1::uuid")
            .bind(lim_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let fill_plan: String =
        sqlx::query_scalar("SELECT ultima_orden_id::text FROM mercado.planes_ahorro WHERE id = $1::uuid")
            .bind(plan["plan_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let ids: Vec<&str> = o.iter().map(|e| e["order_id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            rechazada["order_id"].as_str().unwrap(),
            fill_limite.as_str(),
            venta["order_id"].as_str().unwrap(),
            fill_plan.as_str(),
            mercado["order_id"].as_str().unwrap(),
        ]
    );
    let campos = |e: &Value| {
        let mut e = e.clone();
        for k in ["placed_at", "closed_at"] {
            assert!(DateTime::parse_from_rfc3339(e[k].as_str().unwrap()).is_ok(), "{e}");
            e[k] = Value::Null;
        }
        e
    };
    assert_eq!(
        campos(&o[0]),
        json!({ "order_id": rechazada["order_id"], "status": "rejected", "order_type": "limit", "side": "buy",
                "ticker": "AUG1", "amount": "100.00000000", "limit_price": "1500.00000000", "stop_price": null,
                "quantity": null, "price": null, "total": null, "commission": null,
                "reject_reason": "KYC_NO_VERIFICADO", "placed_at": null, "closed_at": null,
                "source_order_id": null, "plan_id": null })
    );
    assert_eq!(
        campos(&o[1]),
        json!({ "order_id": fill_limite, "status": "filled", "order_type": "limit", "side": "buy", "ticker": "AUG1",
                "amount": "1250.00000000", "limit_price": "1600.00000000", "stop_price": null,
                "quantity": "0.78610000", "price": "1590.00000000", "total": "1249.90000000",
                "commission": "0.00000000", "reject_reason": null, "placed_at": null, "closed_at": null,
                "source_order_id": limite["order_id"], "plan_id": null })
    );
    // Colocada al ponerla, ejecutada al dispararse.
    let instante = |v: &Value| DateTime::parse_from_rfc3339(v.as_str().unwrap()).unwrap();
    assert!(instante(&o[1]["placed_at"]) < instante(&o[1]["closed_at"]));
    assert_eq!(o[1]["placed_at"], limite["created_at"]);
    assert_eq!(
        campos(&o[2]),
        json!({ "order_id": venta["order_id"], "status": "cancelled", "order_type": "limit", "side": "sell",
                "ticker": "AUG1", "amount": "100.00000000", "limit_price": "1700.00000000", "stop_price": null,
                "quantity": null, "price": null, "total": null, "commission": null, "reject_reason": null,
                "placed_at": null, "closed_at": null, "source_order_id": null, "plan_id": null })
    );
    assert_eq!(
        campos(&o[3]),
        json!({ "order_id": fill_plan, "status": "filled", "order_type": "savings_plan", "side": "buy",
                "ticker": "CETES28", "amount": "100.00000000", "limit_price": null, "stop_price": null,
                "quantity": "10.00000000", "price": "9.94390000", "total": "99.44000000",
                "commission": "0.00000000", "reject_reason": null, "placed_at": null, "closed_at": null,
                "source_order_id": null, "plan_id": plan["plan_id"] })
    );
    assert_eq!(
        campos(&o[4]),
        json!({ "order_id": mercado["order_id"], "status": "filled", "order_type": "market", "side": "buy",
                "ticker": "AUG1", "amount": "500.00000000", "limit_price": null, "stop_price": null,
                "quantity": "0.31000000", "price": "1612.40000000", "total": "499.84000000",
                "commission": "0.00000000", "reject_reason": null, "placed_at": null, "closed_at": null,
                "source_order_id": null, "plan_id": null })
    );
    assert_eq!(o[4]["placed_at"], o[4]["closed_at"], "una de mercado se ejecuta al colocarse");

    // Por páginas: 2, 2 y 1, sin repetir ni saltar.
    let mut vistas = Vec::new();
    let mut ruta = "/api/v1/market/orders/history?limit=2".to_string();
    let mut paginas = 0;
    loop {
        let p = m.leer(&token, &ruta).await;
        paginas += 1;
        vistas.extend(p["orders"].as_array().unwrap().iter().map(|e| e["order_id"].clone()));
        match p["next_cursor"].as_str() {
            Some(c) => ruta = format!("/api/v1/market/orders/history?limit=2&cursor={c}"),
            None => break,
        }
    }
    assert_eq!((paginas, vistas), (3, o.iter().map(|e| e["order_id"].clone()).collect::<Vec<_>>()));

    // Lo que no se pide bien; el personal; otro cliente.
    for ruta in [
        "/api/v1/market/orders/history?limit=0",
        "/api/v1/market/orders/history?limit=101",
        "/api/v1/market/orders/history?limit=diez",
        "/api/v1/market/orders/history?cursor=no-es-un-cursor",
        "/api/v1/market/orders/history?pagina=2",
    ] {
        let (s, e) = m.get(&token, ruta).await;
        assert_eq!((s, &e["error"]["codigo"]), (StatusCode::BAD_REQUEST, &json!("VALIDACION")), "{ruta}");
    }
    let interno = token_interno(&pool, &m).await;
    assert_eq!(m.get(&interno, "/api/v1/market/orders/history").await.0, StatusCode::FORBIDDEN);
    let beto = cliente(&pool, &cfg, "Beto").await;
    assert_eq!(
        m.leer(&m.token(beto.cliente.id).await, "/api/v1/market/orders/history").await,
        json!({ "orders": [], "next_cursor": null })
    );
}
