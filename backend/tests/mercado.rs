//! Mercado de valores (migración 0013, `mercado`, `routes/mercado.rs`) por la
//! API: cotización, compra y venta con su efectivo por la bóveda, la bóveda
//! con sus posiciones, idempotencia, reglas (saldo, posición, incremento,
//! centavos, forma de la petición), KYC y rol, congelamiento PLD, órdenes
//! concurrentes, auditoría del ledger y precios simulados fuera de MODO_DEMO.
//! El aviso por WebSocket de una orden está en tests/boveda.rs.

mod comun;

use axum::http::StatusCode;
use comun::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    config::Config,
    kyc::Registro,
    ledger::hash,
    mercado::{self, Lado, SolicitudOrden},
    models::kyc::RolUsuario,
    reserva, routes, AppState,
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

    fn token(&self, cliente_id: Uuid) -> String {
        self.estado.jwt.emitir(cliente_id, RolUsuario::Cliente, false).unwrap().0
    }

    async fn catalogo(&self, token: &str) -> (StatusCode, Value) {
        llamar_json(&self.app, "GET", "/api/v1/market/assets", Some(token), None).await
    }

    async fn cotizar(&self, token: &str, ticker: &str) -> (StatusCode, Value) {
        llamar_json(&self.app, "GET", &format!("/api/v1/market/quote/{ticker}"), Some(token), None).await
    }

    async fn ordenar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        llamar_json(&self.app, "POST", "/api/v1/market/order", Some(token), Some(cuerpo)).await
    }

    /// Una orden rechazada: estado y `error.codigo`.
    async fn rechazada(&self, token: &str, cuerpo: Value) -> (StatusCode, String) {
        let (s, e) = self.ordenar(token, cuerpo).await;
        (s, e["error"]["codigo"].as_str().unwrap_or_else(|| panic!("sin error.codigo: {e}")).to_string())
    }
}

/// `etiqueta` se vuelve una clave válida (8 a 64 caracteres): `prueba-<etiqueta>`.
fn orden(ticker: &str, lado: &str, monto: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "side": lado, "amount": monto, "idempotency_key": format!("prueba-{etiqueta}") })
}

/// Cliente con KYC verificado y `monto` en su bóveda.
async fn con_efectivo(pool: &PgPool, cfg: &Config, nombre: &str, monto: &str) -> Registro {
    let r = cliente(pool, cfg, nombre).await;
    depositar(pool, cfg, r.cuenta_ifpe.id, monto).await;
    r
}

async fn posicion(pool: &PgPool, cliente: Uuid, ticker: &str) -> Option<(Decimal, Decimal)> {
    sqlx::query_as("SELECT cantidad, costo FROM mercado.posiciones WHERE cliente_id = $1 AND ticker = $2")
        .bind(cliente)
        .bind(ticker)
        .fetch_optional(pool)
        .await
        .unwrap()
}

// -----------------------------------------------------------------------------
// Catálogo
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn catalogo_por_la_api(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = m.token(ana.cliente.id);

    let (s, c) = m.catalogo(&token).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert!(chrono::DateTime::parse_from_rfc3339(c["as_of"].as_str().unwrap()).is_ok(), "{c}");
    // Los activos de 0013 y 0014, por ticker (orden binario), todos simulados.
    let activos = c["assets"].as_array().unwrap();
    let tickers: Vec<&str> = activos.iter().map(|a| a["ticker"].as_str().unwrap()).collect();
    assert_eq!(tickers, ["AGT1", "AUG1", "CETES28", "SCT28", "STSLA"]);

    // Cada renglón es la ficha de su cotización (MarketAsset) y su fuente de
    // precio: lo que el explorador lista se abre igual en la página del activo.
    for activo in activos {
        let ticker = activo["ticker"].as_str().unwrap();
        let (s, q) = m.cotizar(&token, ticker).await;
        assert_eq!(s, StatusCode::OK, "{ticker}: {q}");
        let mut ficha = activo.clone();
        let fuente = ficha.as_object_mut().unwrap().remove("price_source");
        assert_eq!((&ficha, fuente.as_ref()), (&q["asset"], Some(&q["price_source"])), "{ticker}");
    }

    // Los dos de 0014: una acción sintética que baja y un CETES que paga.
    assert_eq!(
        activos[4],
        json!({
            "ticker": "STSLA",
            "name": "Synthetic Tesla, 1 share",
            "asset_class": "Equity",
            "price": "4415.30000000",
            "change_24h_pct": "-2.40",
            "market_cap": "215000000.00",
            "volume_24h": "96500000.00",
            "yield_pct": null,
            "quantity_decimals": 4,
            "price_source": "simulated",
        })
    );
    assert_eq!(
        activos[2],
        json!({
            "ticker": "CETES28",
            "name": "CETES, 28 days",
            "asset_class": "Fixed income",
            "price": "9.94390000",
            "change_24h_pct": "0.02",
            "market_cap": null,
            "volume_24h": "18400000.00",
            "yield_pct": "7.25",
            "quantity_decimals": 0,
            "price_source": "simulated",
        })
    );

    let (s, _) = llamar_json(&m.app, "GET", "/api/v1/market/assets", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "sin sesión");
}

#[sqlx::test(migrations = "../migrations")]
async fn lo_que_lista_el_catalogo_se_opera(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "10000").await;
    let token = m.token(ana.cliente.id);

    // Fracciones de una acción: 1 000 a 4 415.30 son 0.2264 por 999.62.
    let (s, r) = m.ordenar(&token, orden("STSLA", "buy", "1000.00", "stsla")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!((&r["quantity"], &r["amount"]), (&json!("0.22640000"), &json!("999.62000000")), "{r}");
    // CETES en títulos enteros: 1 000 a 9.9439 son 100 títulos por 994.39.
    let (s, r) = m.ordenar(&token, orden("CETES28", "buy", "1000.00", "cetes")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!((&r["quantity"], &r["amount"]), (&json!("100.00000000"), &json!("994.39000000")), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("8005.99"), d("0")));
    assert_eq!(posicion(&pool, ana.cliente.id, "CETES28").await, Some((d("100"), d("994.39"))));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn sin_modo_demo_el_catalogo_solo_lista_precios_reales(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    cfg.modo_demo = false;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let token = m.token(ana.cliente.id);

    // Todo es simulado: el mercado no ha abierto, como en la cotización.
    let (s, e) = m.catalogo(&token).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::SERVICE_UNAVAILABLE, &json!("MERCADO_NO_DISPONIBLE")), "{e}");

    // Con el precio de un activo del oráculo, se lista ese y solo ese.
    sqlx::query("UPDATE mercado.activos SET fuente_precio = 'ORACULO' WHERE ticker = 'CETES28'")
        .execute(&pool)
        .await
        .unwrap();
    let (s, c) = m.catalogo(&token).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let activos = c["assets"].as_array().unwrap();
    assert_eq!(
        (activos.len(), &activos[0]["ticker"], &activos[0]["price_source"]),
        (1, &json!("CETES28"), &json!("oracle")),
        "{c}"
    );
}

// -----------------------------------------------------------------------------
// Cotización
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn cotizacion_por_la_api(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = m.token(ana.cliente.id);

    let (s, q) = m.cotizar(&token, "AUG1").await;
    assert_eq!(s, StatusCode::OK, "{q}");
    // La forma exacta de MarketAsset (frontend/src/types/market.ts).
    assert_eq!(
        q["asset"],
        json!({
            "ticker": "AUG1",
            "name": "Tokenized gold, 1 g",
            "asset_class": "Commodity",
            "price": "1612.40000000",
            "change_24h_pct": "0.84",
            "market_cap": "1250000000.00",
            "volume_24h": "84300000.00",
            "yield_pct": null,
            "quantity_decimals": 4,
        })
    );
    assert_eq!(q["price_source"], "simulated");
    let historial = q["history"].as_array().unwrap();
    assert!(historial.len() > 500, "un año de puntos: {}", historial.len());
    let instantes: Vec<i64> = historial.iter().map(|p| p["at"].as_i64().unwrap()).collect();
    assert!(instantes.windows(2).all(|w| w[0] < w[1]), "del más antiguo al más reciente");
    // El último punto es el precio vigente, a la hora de la cotización.
    assert_eq!(historial.last().unwrap()["value"], "1612.40000000");
    let al = chrono::DateTime::parse_from_rfc3339(q["as_of"].as_str().unwrap()).unwrap();
    assert_eq!(*instantes.last().unwrap(), al.timestamp() * 1000);

    // Un certificado: renta fija, rendimiento a 2 decimales, unidades enteras.
    let (s, q) = m.cotizar(&token, "SCT28").await;
    assert_eq!(s, StatusCode::OK, "{q}");
    assert_eq!(
        (
            &q["asset"]["asset_class"],
            &q["asset"]["yield_pct"],
            &q["asset"]["quantity_decimals"],
            &q["asset"]["market_cap"]
        ),
        (&json!("Fixed income"), &json!("10.25"), &json!(0), &Value::Null)
    );

    // Un activo que no existe o mal escrito: 404. Sin sesión: 401.
    for ticker in ["XAU", "aug1", "AUG1%20"] {
        let (s, e) = m.cotizar(&token, ticker).await;
        assert_eq!((s, &e["error"]["codigo"]), (StatusCode::NOT_FOUND, &json!("NO_ENCONTRADO")), "{ticker}: {e}");
    }
    let (s, _) = llamar_json(&m.app, "GET", "/api/v1/market/quote/AUG1", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../migrations")]
async fn sin_modo_demo_un_precio_simulado_no_se_cotiza_ni_se_opera(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    cfg.modo_demo = false;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let token = m.token(ana.cliente.id);

    let (s, e) = m.cotizar(&token, "AUG1").await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::SERVICE_UNAVAILABLE, &json!("MERCADO_NO_DISPONIBLE")), "{e}");
    let (s, codigo) = m.rechazada(&token, orden("AUG1", "buy", "100.00", "sin-demo")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::SERVICE_UNAVAILABLE, "MERCADO_NO_DISPONIBLE"));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("5000"), "nada se movió");

    // Con un precio del oráculo, opera también sin MODO_DEMO.
    sqlx::query("UPDATE mercado.activos SET fuente_precio = 'ORACULO' WHERE ticker = 'AUG1'")
        .execute(&pool)
        .await
        .unwrap();
    let (s, q) = m.cotizar(&token, "AUG1").await;
    assert_eq!((s, &q["price_source"]), (StatusCode::OK, &json!("oracle")), "{q}");
    assert_eq!(q["history"].as_array().unwrap().len(), 1, "sin historia guardada: el precio vigente");
    let (s, r) = m.ordenar(&token, orden("AUG1", "buy", "100.00", "oraculo")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Compra y venta
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn compra_y_venta_por_la_api(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let cuenta = ana.cuenta_ifpe.id;
    let token = m.token(ana.cliente.id);

    // Compra: 1 250 MXN de oro a 1 612.40 son 0.7752 g por 1 249.93; el
    // resto (0.07) se queda en efectivo. Sin comisión.
    let (s, r) = m.ordenar(&token, orden("AUG1", "buy", "1250.00", "compra-1")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let orden_id: Uuid = r["order_id"].as_str().unwrap().parse().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(r["executed_at"].as_str().unwrap()).is_ok());
    assert_eq!(
        r,
        json!({
            "order_id": orden_id,
            "status": "filled",
            "ticker": "AUG1",
            "side": "buy",
            "quantity": "0.77520000",
            "price": "1612.40000000",
            "amount": "1249.93000000",
            "commission": "0.00000000",
            "executed_at": r["executed_at"],
        })
    );
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("18750.07"), d("0")));
    assert_eq!(posicion(&pool, ana.cliente.id, "AUG1").await, Some((d("0.7752"), d("1249.93"))));

    // El movimiento IFPE y su orden se referencian; PLD evaluó la compra en línea.
    let (tipo, monto, origen, espejo): (String, Decimal, Uuid, Uuid) = sqlx::query_as(
        "SELECT t.tipo, t.monto, t.cuenta_origen_id, t.ref_movimiento_espejo \
           FROM ifpe.transacciones t JOIN mercado.ordenes o ON o.tx_ifpe_id = t.id WHERE o.id = $1",
    )
    .bind(orden_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((tipo.as_str(), monto, origen, espejo), ("COMPRA_MERCADO", d("1249.93"), cuenta, orden_id));
    let evaluaciones = evaluaciones_de(&pool, ana.cliente.id).await;
    assert!(
        evaluaciones.iter().any(|(canal, resultado, origen, _)| canal == "COMPRA_MERCADO"
            && resultado == "PERMITIDA"
            && origen == "EN_LINEA"),
        "{evaluaciones:?}"
    );

    // La bóveda: el efectivo, lo invertido a precio de mercado y la posición.
    let (s, v) = llamar_json(&m.app, "GET", "/api/v1/vault", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["balance"],
        json!({
            "total_mxn": "20000.00248000",
            "available_cash": "18750.07000000",
            "invested_assets": "1249.93248000",
            "unrealized_pnl_percentage": "0.00",
        })
    );
    assert_eq!(
        v["holdings"],
        json!([{
            "ticker": "AUG1",
            "name": "Tokenized gold, 1 g",
            "shares": "0.77520000",
            "current_price": "1612.40000000",
            "total_value": "1249.93248000",
            "daily_change_pct": "0.84",
        }])
    );

    // Venta parcial: 500 MXN son 0.3100 g por 499.84 (libera su costo promedio).
    let (s, r) = m.ordenar(&token, orden("AUG1", "sell", "500.00", "venta-1")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(
        (&r["side"], &r["quantity"], &r["amount"]),
        (&json!("sell"), &json!("0.31000000"), &json!("499.84000000"))
    );
    assert_eq!(posicion(&pool, ana.cliente.id, "AUG1").await, Some((d("0.4652"), d("750.08699174"))));

    // Un corte de Proof of Reserve con la posición abierta: su reproducción del
    // ledger conoce las patas del mercado (0013 las había dejado fuera, y cada
    // corte después de una orden fallaba con FALLA DE INTEGRIDAD).
    let corte = reserva::generar_corte(&pool).await.expect("corte de reserva después de comprar y vender");
    assert_eq!(corte.total_pasivo, corte.total_reserva);
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("19249.91"), d("0")), "20 000 − 1 249.93 + 499.84");

    // Lo que vale el resto (750.08, el «Max» del frontend) vende toda la
    // posición, sin residuos: 0.4652 g por 750.09.
    let (s, r) = m.ordenar(&token, orden("AUG1", "sell", "750.08", "venta-2")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!((&r["quantity"], &r["amount"]), (&json!("0.46520000"), &json!("750.09000000")));
    assert_eq!(posicion(&pool, ana.cliente.id, "AUG1").await, Some((d("0"), d("0"))));
    // Al mismo precio, comprar y vender todo devuelve exactamente lo pagado.
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("20000"), d("0")));
    let (_, v) = llamar_json(&m.app, "GET", "/api/v1/vault", Some(&token), None).await;
    assert_eq!((&v["holdings"], &v["balance"]["invested_assets"]), (&json!([]), &json!("0.00000000")), "{v}");

    // Reserva al 100 %, cadenas IFPE, SOFOM y del mercado, posiciones y pares.
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    assert_eq!((r.valido, r.movimientos_verificados, r.saldos_conciliados), (true, 3, 1), "{r:?}");
}

#[sqlx::test(migrations = "../migrations")]
async fn la_misma_clave_devuelve_el_mismo_recibo(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    let token = m.token(ana.cliente.id);

    let (s, primera) = m.ordenar(&token, orden("AUG1", "buy", "1000.00", "una-vez")).await;
    assert_eq!(s, StatusCode::CREATED, "{primera}");
    // Repetir (el reintento de una red caída): 200 con el mismo recibo, nada se mueve.
    let (s, repetida) = m.ordenar(&token, orden("AUG1", "buy", "1000.00", "una-vez")).await;
    assert_eq!((s, &repetida), (StatusCode::OK, &primera));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("5000") - d(primera["amount"].as_str().unwrap()));

    // La misma clave para otra orden: 409.
    for distinta in [orden("AUG1", "buy", "1000.01", "una-vez"), orden("AGT1", "buy", "1000.00", "una-vez")] {
        let (s, codigo) = m.rechazada(&token, distinta).await;
        assert_eq!((s, codigo.as_str()), (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA"));
    }
    // Una clave ya usada en otra operación del ledger (una transferencia): 409.
    let beto = cliente(&pool, &cfg, "Beto").await;
    synthetic_capital::ledger::transferir(
        &pool,
        &cfg.pld,
        &synthetic_capital::ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: beto.cuenta_ifpe.clabe.clone(),
            monto: d("10"),
            concepto: None,
            clave_idempotencia: format!("cli:{}:prueba-ya-usada", ana.cliente.id),
        },
    )
    .await
    .unwrap();
    let (s, codigo) = m.rechazada(&token, orden("AUG1", "buy", "100.00", "ya-usada")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA"));
    // Dos clientes pueden usar la misma clave: cada una se acota al suyo.
    let beto_token = m.token(beto.cliente.id);
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "2000").await;
    let (s, r) = m.ordenar(&beto_token, orden("AUG1", "buy", "1000.00", "una-vez")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_ne!(r["order_id"], primera["order_id"]);
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Reglas
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn reglas_de_una_orden(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = m.token(ana.cliente.id);

    let casos: Vec<(Value, StatusCode, &str)> = vec![
        // Más que el efectivo disponible.
        (orden("AUG1", "buy", "1000.01", "r1"), StatusCode::UNPROCESSABLE_ENTITY, "SALDO_INSUFICIENTE"),
        // Menos que un incremento (0.0001 g cuestan 0.16).
        (orden("AUG1", "buy", "0.16", "r2"), StatusCode::UNPROCESSABLE_ENTITY, "MONTO_INSUFICIENTE"),
        (orden("SCT28", "buy", "9.91", "r3"), StatusCode::UNPROCESSABLE_ENTITY, "MONTO_INSUFICIENTE"),
        // Vender lo que no se tiene.
        (orden("AUG1", "sell", "100.00", "r4"), StatusCode::UNPROCESSABLE_ENTITY, "POSICION_INSUFICIENTE"),
        // Forma: fracciones de centavo, cero, negativo, número JSON, campos de más o de menos.
        (orden("AUG1", "buy", "100.005", "r5"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (orden("AUG1", "buy", "0", "r6"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (orden("AUG1", "buy", "-5.00", "r7"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (
            json!({ "ticker": "AUG1", "side": "buy", "amount": 100, "idempotency_key": "r8" }),
            StatusCode::BAD_REQUEST,
            "",
        ),
        (
            json!({ "ticker": "AUG1", "side": "buy", "amount": "100.00", "idempotency_key": "r9", "price": "1" }),
            StatusCode::BAD_REQUEST,
            "",
        ),
        (json!({ "ticker": "AUG1", "side": "buy", "amount": "100.00" }), StatusCode::BAD_REQUEST, ""),
        (orden("AUG1", "hold", "100.00", "r10"), StatusCode::BAD_REQUEST, ""),
        (orden("XAU", "buy", "100.00", "r11"), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
    ];
    for (cuerpo, status, codigo) in casos {
        let (s, e) = m.ordenar(&token, cuerpo.clone()).await;
        assert_eq!(s, status, "{cuerpo}: {e}");
        if !codigo.is_empty() {
            assert_eq!(e["error"]["codigo"], codigo, "{cuerpo}: {e}");
        }
    }

    // Más de lo que vale la posición.
    let (s, r) = m.ordenar(&token, orden("AUG1", "buy", "500.00", "posicion")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let (s, codigo) = m.rechazada(&token, orden("AUG1", "sell", "600.00", "de-mas")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "POSICION_INSUFICIENTE"));

    // Un activo suspendido no opera.
    sqlx::query("UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    let (s, codigo) = m.rechazada(&token, orden("AUG1", "sell", "100.00", "suspendido")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "MERCADO_CERRADO"));

    // Nada de lo rechazado movió dinero: solo la compra de 500.
    let compra: Decimal = d(r["amount"].as_str().unwrap());
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000") - compra);
    let ordenes: i64 = sqlx::query_scalar("SELECT count(*) FROM mercado.ordenes").fetch_one(&pool).await.unwrap();
    assert_eq!(ordenes, 1);
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn solo_un_cliente_con_kyc_coloca_ordenes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());

    // Sin KYC verificado: 422 KYC_NO_VERIFICADO (también lo impide la base de datos).
    let carla = cliente_sin_kyc(&pool, &cfg, "Carla").await;
    let (s, codigo) = m.rechazada(&m.token(carla.cliente.id), orden("AUG1", "buy", "100.00", "sin-kyc")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "KYC_NO_VERIFICADO"));

    // KYC revocado después de comprar: ya no compra ni vende.
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = m.token(ana.cliente.id);
    let (s, r) = m.ordenar(&token, orden("AUG1", "buy", "100.00", "antes")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    revocar_kyc(&pool, ana.cliente.id).await;
    for (lado, clave) in [("buy", "despues-1"), ("sell", "despues-2")] {
        let (s, codigo) = m.rechazada(&token, orden("AUG1", lado, "50.00", clave)).await;
        assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "KYC_NO_VERIFICADO"), "{lado}");
    }

    // El personal no opera por la API del cliente; la cotización sí la ve.
    bootstrap::asegurar_operador(&pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    let operador: Uuid = sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = 'operador@synthetic.mx'")
        .fetch_one(&pool)
        .await
        .unwrap();
    activar_mfa(&m.estado, operador).await;
    let token_operador = m.estado.jwt.emitir(operador, RolUsuario::Operador, true).unwrap().0;
    let (s, codigo) = m.rechazada(&token_operador, orden("AUG1", "buy", "100.00", "operador")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::FORBIDDEN, "PROHIBIDO"));
    let (s, _) = m.cotizar(&token_operador, "AUG1").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = m.catalogo(&token_operador).await;
    assert_eq!(s, StatusCode::OK, "el catálogo también");
    // … y la auditoría del ledger del mercado es del personal.
    let (s, a) = llamar_json(&m.app, "GET", "/api/v1/auditoria/ledger/mercado", Some(&token_operador), None).await;
    assert_eq!((s, &a["ledger"], &a["valido"]), (StatusCode::OK, &json!("MERCADO"), &json!(true)), "{a}");
    let (s, _) = llamar_json(&m.app, "GET", "/api/v1/auditoria/ledger/mercado", Some(&token), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../migrations")]
async fn un_cliente_congelado_no_compra_ni_vende(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    let token = m.token(ana.cliente.id);
    let (s, r) = m.ordenar(&token, orden("AUG1", "buy", "1000.00", "antes")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");

    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let (_, _, intentos) = congelamiento_activo(&pool, ana.cliente.id).await.unwrap();
    let saldo = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await;

    // El mismo error constante que cualquier operación bloqueada, sin aviso.
    for (lado, clave) in [("buy", "congelado-1"), ("sell", "congelado-2")] {
        let (s, codigo) = m.rechazada(&token, orden("AUG1", lado, "100.00", clave)).await;
        assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "OPERACION_NO_DISPONIBLE"), "{lado}");
    }
    // La compra pasó por PLD: el intento bloqueado queda registrado (sin renglón del ledger).
    let (_, _, despues) = congelamiento_activo(&pool, ana.cliente.id).await.unwrap();
    assert_eq!(despues, intentos + 1, "el intento de compra cuenta; la venta no llega a PLD");
    let evaluaciones = evaluaciones_de(&pool, ana.cliente.id).await;
    assert!(evaluaciones.iter().any(|(canal, resultado, _, _)| canal == "COMPRA_MERCADO" && resultado == "BLOQUEADA"));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, saldo, "nada se movió");
    let ordenes: i64 = sqlx::query_scalar("SELECT count(*) FROM mercado.ordenes").fetch_one(&pool).await.unwrap();
    assert_eq!(ordenes, 1);
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// Concurrencia y auditoría
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn ordenes_simultaneas_no_gastan_dos_veces(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;

    // Dos compras de 15 000 con 20 000: el candado de la cuenta las ordena y
    // la segunda ya no tiene saldo.
    let compras = (0..2).map(|i| {
        let (pool, cfg) = (pool.clone(), cfg.clone());
        let s = SolicitudOrden {
            cliente_id: ana.cliente.id,
            ticker: "AUG1".into(),
            lado: Lado::Compra,
            monto: d("15000"),
            clave_idempotencia: format!("cli:{}:simultanea-{i}", ana.cliente.id),
        };
        tokio::spawn(async move { mercado::colocar(&pool, &cfg, &s).await })
    });
    let resultados: Vec<_> = futures_util::future::join_all(compras).await.into_iter().map(Result::unwrap).collect();
    let ejecutadas = resultados.iter().filter(|r| r.is_ok()).count();
    let rechazos: Vec<_> = resultados.iter().filter_map(|r| r.as_ref().err()).map(|e| e.codigo()).collect();
    assert_eq!((ejecutadas, rechazos), (1, vec!["SALDO_INSUFICIENTE"]));
    let saldo = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0;
    assert!(saldo >= d("5000") && saldo < d("5000.02"), "{saldo}");
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn la_auditoria_detecta_un_ledger_alterado(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    for (lado, monto, clave) in [(Lado::Compra, "1250", "a"), (Lado::Venta, "500", "b")] {
        let s = SolicitudOrden {
            cliente_id: ana.cliente.id,
            ticker: "AUG1".into(),
            lado,
            monto: d(monto),
            clave_idempotencia: format!("cli:{}:{clave}", ana.cliente.id),
        };
        mercado::colocar(&pool, &cfg, &s).await.unwrap();
    }
    assert!(hash::verificar_cadena_mercado(&pool).await.unwrap().valido);

    // Alguien con el dueño del esquema desactiva los triggers y edita una posición:
    // la reproducción del ledger no la justifica.
    sqlx::raw_sql(
        "ALTER TABLE mercado.posiciones DISABLE TRIGGER trg_posiciones_ledger; \
         UPDATE mercado.posiciones SET cantidad = cantidad + 1; \
         ALTER TABLE mercado.posiciones ENABLE TRIGGER trg_posiciones_ledger;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    assert!(!r.valido && r.discrepancias.iter().any(|d| d.contains("posición de")), "{r:?}");

    // … y edita una orden (las restricciones de la tabla impiden cambiar
    // cantidad, precio o importe; la hora no): su hash ya no es el de su contenido.
    sqlx::raw_sql(
        "ALTER TABLE mercado.ordenes DISABLE TRIGGER trg_ordenes_inmutable; \
         UPDATE mercado.ordenes SET creado_en = creado_en - interval '1 day' WHERE secuencia = 1; \
         ALTER TABLE mercado.ordenes ENABLE TRIGGER trg_ordenes_inmutable;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    let falla = r.falla.expect("la cadena debía romperse");
    assert_eq!(falla.secuencia, 1, "{falla:?}");
    assert!(!r.valido);
}
