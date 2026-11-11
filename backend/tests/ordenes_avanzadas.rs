//! Órdenes límite y stop (migración 0015, `mercado::pendiente`,
//! `routes/mercado.rs`) por la API y el motor de disparo: lo comprometido
//! (efectivo y títulos) con la reserva del 100 % intacta, idempotencia, la
//! lista de pendientes, la cancelación, la ejecución al alcanzar el precio
//! (o al colocarse, si ya se puede), el rechazo que devuelve lo comprometido,
//! las ventas de mercado que solo venden títulos libres, PLD, carreras y la
//! auditoría. Los números son los de tests/db/0015_ordenes_avanzadas.sql. El
//! aviso por WebSocket está en tests/boveda.rs.

mod comun;

use axum::http::StatusCode;
use comun::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    config::Config,
    kyc::Registro,
    ledger::hash,
    mercado::pendiente::{self, Desenlace, ResumenDisparo},
    models::kyc::RolUsuario,
    reserva, routes, AppError, AppState,
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

    async fn ordenar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        llamar_json(&self.app, "POST", "/api/v1/market/order", Some(token), Some(cuerpo)).await
    }

    /// Una orden que debe quedar colocada (201): la respuesta.
    async fn colocada(&self, token: &str, cuerpo: Value) -> Value {
        let (s, o) = self.ordenar(token, cuerpo.clone()).await;
        assert_eq!(s, StatusCode::CREATED, "{cuerpo}: {o}");
        o
    }

    /// Una orden rechazada: estado y `error.codigo`.
    async fn rechazada(&self, token: &str, cuerpo: Value) -> (StatusCode, String) {
        let (s, e) = self.ordenar(token, cuerpo).await;
        (s, e["error"]["codigo"].as_str().unwrap_or_else(|| panic!("sin error.codigo: {e}")).to_string())
    }

    async fn pendientes(&self, token: &str) -> Value {
        let (s, l) = llamar_json(&self.app, "GET", "/api/v1/market/orders/pending", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{l}");
        l
    }

    async fn cancelar(&self, token: &str, id: &Value) -> (StatusCode, Value) {
        let ruta = format!("/api/v1/market/orders/{}/cancel", id.as_str().unwrap());
        llamar_json(&self.app, "POST", &ruta, Some(token), None).await
    }

    async fn boveda(&self, token: &str) -> Value {
        let (s, v) = llamar_json(&self.app, "GET", "/api/v1/vault", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }
}

/// `etiqueta` se vuelve una clave válida (8 a 64 caracteres): `prueba-<etiqueta>`.
fn clave_de(etiqueta: &str) -> String {
    format!("prueba-{etiqueta}")
}

fn orden(ticker: &str, lado: &str, monto: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "side": lado, "amount": monto, "idempotency_key": clave_de(etiqueta) })
}

fn limite(ticker: &str, lado: &str, monto: &str, precio: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "side": lado, "amount": monto, "idempotency_key": clave_de(etiqueta),
            "order_type": "limit", "limit_price": precio })
}

fn stop(ticker: &str, lado: &str, monto: &str, precio: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "side": lado, "amount": monto, "idempotency_key": clave_de(etiqueta),
            "order_type": "stop", "stop_price": precio })
}

/// Cliente con KYC verificado y `monto` en su bóveda.
async fn con_efectivo(pool: &PgPool, cfg: &Config, nombre: &str, monto: &str) -> Registro {
    let r = cliente(pool, cfg, nombre).await;
    depositar(pool, cfg, r.cuenta_ifpe.id, monto).await;
    r
}

/// (disponible, comprometido) de la cuenta IFPE.
async fn saldos(pool: &PgPool, cuenta: Uuid) -> (Decimal, Decimal) {
    sqlx::query_as("SELECT saldo_disponible, saldo_comprometido FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// (cantidad, comprometida, costo) de la posición; ceros si no hay.
async fn tenencia(pool: &PgPool, cliente: Uuid, ticker: &str) -> (Decimal, Decimal, Decimal) {
    sqlx::query_as(
        "SELECT cantidad, cantidad_comprometida, costo FROM mercado.posiciones WHERE cliente_id = $1 AND ticker = $2",
    )
    .bind(cliente)
    .bind(ticker)
    .fetch_optional(pool)
    .await
    .unwrap()
    .unwrap_or_default()
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

/// (estado, motivo) de una orden pendiente.
async fn estado(pool: &PgPool, id: &Value) -> (String, Option<String>) {
    let id: Uuid = id.as_str().unwrap().parse().unwrap();
    sqlx::query_as("SELECT estado, motivo FROM mercado.ordenes_pendientes WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// La orden del ledger del mercado que ejecutó a una pendiente:
/// (cantidad, precio, monto, monto_solicitado).
async fn ejecucion(pool: &PgPool, id: &Value) -> (Decimal, Decimal, Decimal, Decimal) {
    sqlx::query_as(
        "SELECT o.cantidad, o.precio, o.monto, o.monto_solicitado FROM mercado.ordenes o \
           JOIN mercado.ordenes_pendientes p ON p.orden_ejecutada_id = o.id \
          WHERE p.id = $1::uuid AND o.clave_idempotencia = 'ord:' || p.id || ':ejecucion'",
    )
    .bind(id.as_str().unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

fn resumen(revisadas: usize, ejecutadas: usize, rechazadas: usize) -> ResumenDisparo {
    ResumenDisparo { revisadas, ejecutadas, rechazadas, errores: 0 }
}

// -----------------------------------------------------------------------------
// Colocar: lo comprometido, la lista y la idempotencia
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn una_compra_limite_compromete_su_efectivo_y_espera(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let cuenta = ana.cuenta_ifpe.id;
    let token = m.token(ana.cliente.id);

    // Comprar 1 250 MXN de oro a 1 600 o menos, con el precio en 1 612.40: espera.
    let cuerpo = limite("AUG1", "buy", "1250.00", "1600.0000", "limite-1");
    let o = m.colocada(&token, cuerpo.clone()).await;
    let id: Uuid = o["order_id"].as_str().unwrap().parse().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(o["created_at"].as_str().unwrap()).is_ok(), "{o}");
    assert_eq!(
        o,
        json!({
            "order_id": id,
            "order_type": "limit",
            "status": "pending",
            "ticker": "AUG1",
            "side": "buy",
            "amount": "1250.00000000",
            "limit_price": "1600.00000000",
            "stop_price": null,
            "held_cash": "1250.00000000",
            "held_quantity": "0.00000000",
            "created_at": o["created_at"],
            "closed_at": null,
            "reject_reason": null,
            "execution": null,
        })
    );
    // Los 1 250 pasan de disponible a comprometido; nada sale de la custodia.
    assert_eq!(saldos(&pool, cuenta).await, (d("18750"), d("1250")));
    let (tipo, monto, origen, destino, espejo, clave_tx, reserva_id): (
        String,
        Decimal,
        Uuid,
        Uuid,
        Uuid,
        String,
        Option<Uuid>,
    ) = sqlx::query_as(
        "SELECT tipo, monto, cuenta_origen_id, cuenta_destino_id, ref_movimiento_espejo, clave_idempotencia, reserva_id \
           FROM ifpe.transacciones WHERE ref_movimiento_espejo = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (tipo.as_str(), monto, origen, destino, espejo, clave_tx, reserva_id),
        ("COMPROMISO_ORDEN", d("1250"), cuenta, cuenta, id, format!("ord:{id}:compromiso"), None)
    );
    // Comprometer no es una salida: PLD evalúa la compra cuando se ejecute
    // (la única evaluación es la del depósito SPEI).
    let evaluaciones = evaluaciones_de(&pool, ana.cliente.id).await;
    assert!(!evaluaciones.iter().any(|(canal, ..)| canal == "COMPRA_MERCADO"), "{evaluaciones:?}");

    // La bóveda: el efectivo comprometido cuenta en el total, no en el disponible.
    let v = m.boveda(&token).await;
    assert_eq!(
        v["balance"],
        json!({
            "total_mxn": "20000.00000000",
            "available_cash": "18750.00000000",
            "held_cash": "1250.00000000",
            "invested_assets": "0.00000000",
            "unrealized_pnl_percentage": "0.00",
        })
    );
    // La lista de pendientes la trae tal cual.
    assert_eq!(m.pendientes(&token).await, json!({ "orders": [o.clone()] }));

    // La misma clave: la misma orden (200), sin comprometer otra vez.
    let (s, r) = m.ordenar(&token, cuerpo).await;
    assert_eq!((s, &r), (StatusCode::OK, &o));
    assert_eq!(saldos(&pool, cuenta).await, (d("18750"), d("1250")));
    // La misma clave con otros términos, o para una orden de mercado: 409.
    for otra in [limite("AUG1", "buy", "1250.00", "1599.0000", "limite-1"), orden("AUG1", "buy", "1250.00", "limite-1")]
    {
        let (s, codigo) = m.rechazada(&token, otra).await;
        assert_eq!((s, codigo.as_str()), (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA"));
    }
    // … y la clave de una orden de mercado no sirve para una límite.
    m.colocada(&token, orden("AUG1", "buy", "100.00", "mercado-1")).await;
    let (s, codigo) = m.rechazada(&token, limite("AUG1", "buy", "100.00", "1600", "mercado-1")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA"));

    // Un corte de Proof of Reserve con efectivo comprometido: la hoja de Ana
    // y su prueba cuentan los 1 250 (siguen siendo suyos y están en custodia).
    let corte = reserva::generar_corte(&pool).await.expect("corte con una orden pendiente");
    assert_eq!(corte.total_pasivo, corte.total_reserva);
    let (s, prueba) = llamar_json(&m.app, "GET", "/api/v1/reserva/prueba", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{prueba}");
    // 20 000 − 99.97 de la compra de mercado (0.0620 g); 1 250 de ellos comprometidos.
    assert_eq!(saldos(&pool, cuenta).await, (d("18650.03"), d("1250")));
    assert_eq!(
        (&prueba["valida"], &prueba["hoja"]["saldo"], &prueba["saldo_actual"]),
        (&json!(true), &json!("19900.03000000"), &json!("19900.03000000")),
        "{prueba}"
    );

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// El motor de disparo
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_motor_ejecuta_cada_orden_al_alcanzar_su_precio(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id);

    // 1 250 MXN de oro a mercado: 0.7752 g por 1 249.93.
    m.colocada(&token, orden("AUG1", "buy", "1250.00", "compra")).await;
    // Las órdenes de tests/db/0015_ordenes_avanzadas.sql.
    let compra_limite = m.colocada(&token, limite("AUG1", "buy", "1250.00", "1600", "compra-limite")).await;
    let otra = m.colocada(&token, limite("AUG1", "buy", "500.00", "1500", "otra-compra")).await;
    let (s, _) = m.cancelar(&token, &otra["order_id"]).await;
    assert_eq!(s, StatusCode::OK);
    // 500 MXN a 1 700 son 0.2941 g; 200 MXN a 1 550, 0.1290 g.
    let venta_limite = m.colocada(&token, limite("AUG1", "sell", "500.00", "1700", "venta-limite")).await;
    assert_eq!(venta_limite["held_quantity"], "0.29410000", "{venta_limite}");
    let stop_compra = m.colocada(&token, stop("AUG1", "buy", "100.00", "1650", "stop-compra")).await;
    let stop_venta = m.colocada(&token, stop("AUG1", "sell", "200.00", "1550", "stop-venta")).await;
    assert_eq!(stop_venta["held_quantity"], "0.12900000", "{stop_venta}");
    assert_eq!(saldos(&pool, cuenta).await, (d("17400.07"), d("1350")));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("0.7752"), d("0.4231"), d("1249.93")));
    assert_eq!(m.pendientes(&token).await["orders"].as_array().unwrap().len(), 4);

    // Una venta de mercado de los títulos libres: 0.0620 g por 99.97, costo promedio sobre los 0.7752.
    let r = m.colocada(&token, orden("AUG1", "sell", "100.00", "libres")).await;
    assert_eq!((&r["quantity"], &r["amount"]), (&json!("0.06200000"), &json!("99.97000000")));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("0.7132"), d("0.4231"), d("1149.96139835")));

    // Al mismo precio, nada se dispara.
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));

    // 1 590: la compra límite (1 600 o menos) compra 0.7861 g por 1 249.90 con sus 1 250.
    precio(&pool, "AUG1", "1590").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(1, 1, 0));
    assert_eq!(estado(&pool, &compra_limite["order_id"]).await, ("EJECUTADA".into(), None));
    assert_eq!(ejecucion(&pool, &compra_limite["order_id"]).await, (d("0.7861"), d("1590"), d("1249.90"), d("1250")));
    assert_eq!(saldos(&pool, cuenta).await, (d("17500.14"), d("100")), "el resto (0.10) queda disponible");
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("1.4993"), d("0.4231"), d("2399.86139835")));

    // 1 710: la venta límite (1 700 o más) y el stop de compra (1 650).
    precio(&pool, "AUG1", "1710").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(2, 2, 0));
    // Vende exactamente sus 0.2941 g, por 502.91: más que los 500, el precio superó el límite.
    assert_eq!(ejecucion(&pool, &venta_limite["order_id"]).await, (d("0.2941"), d("1710"), d("502.91"), d("500")));
    assert_eq!(ejecucion(&pool, &stop_compra["order_id"]).await, (d("0.0584"), d("1710"), d("99.86"), d("100")));
    assert_eq!(saldos(&pool, cuenta).await, (d("18003.19"), d("0")));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("1.2636"), d("0.1290"), d("2028.96888901")));

    // 1 540: el stop de venta (1 550).
    precio(&pool, "AUG1", "1540").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(1, 1, 0));
    assert_eq!(ejecucion(&pool, &stop_venta["order_id"]).await, (d("0.1290"), d("1540"), d("198.66"), d("200")));
    assert_eq!(saldos(&pool, cuenta).await, (d("18201.85"), d("0")));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("1.1346"), d("0"), d("1821.83293880")));
    assert_eq!(m.pendientes(&token).await, json!({ "orders": [] }));
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));

    // PLD evaluó cada compra al ejecutarse (y la de mercado), nunca un compromiso.
    let compras = evaluaciones_de(&pool, cliente_id)
        .await
        .into_iter()
        .filter(|(canal, resultado, _, _)| canal == "COMPRA_MERCADO" && resultado == "PERMITIDA")
        .count();
    assert_eq!(compras, 3);
    // Tres compromisos (1 250, 500 y 100) y sus tres liberaciones.
    let (compromisos, liberaciones): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE tipo = 'COMPROMISO_ORDEN'), count(*) FILTER (WHERE tipo = 'LIBERACION_ORDEN') \
           FROM ifpe.transacciones WHERE cuenta_origen_id = $1",
    )
    .bind(cuenta)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((compromisos, liberaciones), (3, 3));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
    // Dos órdenes de mercado y cuatro ejecuciones, encadenadas y conciliadas.
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    assert_eq!((r.valido, r.movimientos_verificados), (true, 6), "{r:?}");
}

#[sqlx::test(migrations = "../migrations")]
async fn una_orden_limite_ejecutable_se_ejecuta_al_colocarse(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let cuenta = ana.cuenta_ifpe.id;
    let token = m.token(ana.cliente.id);

    // Comprar a 1 700 o menos con el precio en 1 612.40: se ejecuta en seguida.
    let cuerpo = limite("AUG1", "buy", "1250.00", "1700", "compra-ejecutable");
    let o = m.colocada(&token, cuerpo.clone()).await;
    let e = &o["execution"];
    assert_eq!(
        (&o["status"], &o["held_cash"], o["closed_at"].is_string()),
        (&json!("filled"), &json!("0.00000000"), true),
        "{o}"
    );
    assert_eq!(
        e,
        &json!({
            "order_id": e["order_id"],
            "quantity": "0.77520000",
            "price": "1612.40000000",
            "amount": "1249.93000000",
            "commission": "0.00000000",
            "executed_at": e["executed_at"],
        })
    );
    assert_eq!(saldos(&pool, cuenta).await, (d("18750.07"), d("0")));
    // En el ledger IFPE: el compromiso, su liberación y la compra.
    let tipos: Vec<String> =
        sqlx::query_scalar("SELECT tipo FROM ifpe.transacciones WHERE cuenta_origen_id = $1 ORDER BY secuencia")
            .bind(cuenta)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(tipos, ["COMPROMISO_ORDEN", "LIBERACION_ORDEN", "COMPRA_MERCADO"]);
    // Repetirla devuelve la orden ejecutada.
    let (s, r) = m.ordenar(&token, cuerpo).await;
    assert_eq!((s, &r), (StatusCode::OK, &o));

    // Vender 500 MXN a 1 500 o más: 0.3333 g (lo que el monto vende a su
    // límite), ejecutados en seguida a 1 612.40 por 537.41.
    let o = m.colocada(&token, limite("AUG1", "sell", "500.00", "1500", "venta-ejecutable")).await;
    assert_eq!(
        (&o["status"], &o["held_quantity"], &o["execution"]["quantity"], &o["execution"]["amount"]),
        (&json!("filled"), &json!("0.00000000"), &json!("0.33330000"), &json!("537.41000000")),
        "{o}"
    );
    assert_eq!(tenencia(&pool, ana.cliente.id, "AUG1").await.0, d("0.4419"));
    assert_eq!(m.pendientes(&token).await, json!({ "orders": [] }));
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Reglas
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn reglas_de_las_ordenes_limite_y_stop(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let cuenta = ana.cuenta_ifpe.id;
    let token = m.token(ana.cliente.id);

    let base = |extra: Value| {
        let mut o = orden("AUG1", "buy", "100.00", &format!("regla-{}", Uuid::new_v4().simple()));
        o.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        o
    };
    let (q, u) = (StatusCode::BAD_REQUEST, StatusCode::UNPROCESSABLE_ENTITY);
    let casos: Vec<(Value, StatusCode, &str)> = vec![
        // La forma: cada tipo con su precio, y solo el suyo.
        (base(json!({ "order_type": "limit" })), q, "VALIDACION"),
        (base(json!({ "order_type": "limit", "limit_price": "1600", "stop_price": "1700" })), q, "VALIDACION"),
        (base(json!({ "order_type": "stop" })), q, "VALIDACION"),
        (base(json!({ "order_type": "stop", "limit_price": "1700" })), q, "VALIDACION"),
        (base(json!({ "limit_price": "1600" })), q, "VALIDACION"),
        (base(json!({ "order_type": "market", "stop_price": "1700" })), q, "VALIDACION"),
        (base(json!({ "order_type": "limit", "limit_price": "1600", "frequency": "weekly" })), q, "VALIDACION"),
        (base(json!({ "order_type": "limit", "limit_price": "0" })), q, "VALIDACION"),
        (base(json!({ "order_type": "limit", "limit_price": "-1" })), q, "VALIDACION"),
        (base(json!({ "order_type": "limit", "limit_price": "1600.123456789" })), q, "VALIDACION"),
        (base(json!({ "order_type": "limit", "limit_price": 1600 })), q, ""),
        (base(json!({ "order_type": "twap" })), q, ""),
        // Los planes de ahorro llegan después.
        (base(json!({ "order_type": "savings_plan", "frequency": "weekly" })), u, "TIPO_ORDEN_NO_DISPONIBLE"),
        // Un stop que el precio (1 612.40) ya alcanzó.
        (stop("AUG1", "buy", "100.00", "1612.40", "stop-igual"), u, "PRECIO_STOP_INVALIDO"),
        (stop("AUG1", "buy", "100.00", "1600", "stop-abajo"), u, "PRECIO_STOP_INVALIDO"),
        (stop("AUG1", "sell", "100.00", "1700", "stop-arriba"), u, "PRECIO_STOP_INVALIDO"),
        // Efectivo, incremento mínimo al precio de la orden, posición, activo, centavos.
        (limite("AUG1", "buy", "1000.01", "1600", "sin-saldo"), u, "SALDO_INSUFICIENTE"),
        (limite("AUG1", "buy", "0.10", "1600", "sin-incremento"), u, "MONTO_INSUFICIENTE"),
        (limite("AUG1", "sell", "100.00", "1700", "sin-posicion"), u, "POSICION_INSUFICIENTE"),
        (limite("XAU", "buy", "100.00", "1600", "sin-activo"), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
        (limite("AUG1", "buy", "100.005", "1600", "centavos"), q, "VALIDACION"),
    ];
    for (cuerpo, status, codigo) in casos {
        let (s, e) = m.ordenar(&token, cuerpo.clone()).await;
        assert_eq!(s, status, "{cuerpo}: {e}");
        if !codigo.is_empty() {
            assert_eq!(e["error"]["codigo"], codigo, "{cuerpo}: {e}");
        }
    }
    // Un activo suspendido no recibe órdenes.
    sqlx::query("UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    let (s, codigo) = m.rechazada(&token, limite("AUG1", "buy", "100.00", "1600", "suspendido")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "MERCADO_CERRADO"));
    sqlx::query("UPDATE mercado.activos SET operable = TRUE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();

    // Nada de lo rechazado comprometió algo.
    assert_eq!(saldos(&pool, cuenta).await, (d("1000"), d("0")));
    let pendientes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mercado.ordenes_pendientes").fetch_one(&pool).await.unwrap();
    assert_eq!(pendientes, 0);

    // A lo más 50 órdenes abiertas por cliente; al cancelar una cabe otra.
    let mut primera = Value::Null;
    for i in 0..pendiente::MAXIMO_PENDIENTES {
        let o = m.colocada(&token, limite("AUG1", "buy", "1.00", "1000", &format!("tope-{i}"))).await;
        if i == 0 {
            primera = o["order_id"].clone();
        }
    }
    let (s, codigo) = m.rechazada(&token, limite("AUG1", "buy", "1.00", "1000", "tope-de-mas")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "LIMITE_ORDENES_PENDIENTES"));
    assert_eq!(m.cancelar(&token, &primera).await.0, StatusCode::OK);
    m.colocada(&token, limite("AUG1", "buy", "1.00", "1000", "tope-otra")).await;
    assert_eq!(saldos(&pool, cuenta).await, (d("950"), d("50")));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn sin_modo_demo_no_se_colocan_ni_se_disparan_precios_simulados(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let demo = Mercado::nuevo(&pool, cfg.clone());
    let o = demo.colocada(&demo.token(ana.cliente.id), limite("AUG1", "buy", "100.00", "1600", "demo")).await;

    let produccion = Config { modo_demo: false, ..cfg.clone() };
    let m = Mercado::nuevo(&pool, produccion.clone());
    let (s, codigo) = m.rechazada(&m.token(ana.cliente.id), limite("AUG1", "buy", "100.00", "1600", "prod")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::SERVICE_UNAVAILABLE, "MERCADO_NO_DISPONIBLE"));
    // El motor no ejecuta a un precio simulado fuera de MODO_DEMO: la orden espera.
    precio(&pool, "AUG1", "1590").await;
    assert_eq!(pendiente::disparar(&pool, &produccion).await.unwrap(), resumen(0, 0, 0));
    let id: Uuid = o["order_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(pendiente::ejecutar(&pool, &produccion, id).await.unwrap(), Desenlace::SinCambio);
    assert_eq!(estado(&pool, &o["order_id"]).await.0, "PENDIENTE");
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(1, 1, 0));
}

// -----------------------------------------------------------------------------
// Cancelar
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn cancelar_devuelve_lo_comprometido(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id);

    // Una compra: el efectivo regresa, con su LIBERACION_ORDEN.
    let compra = m.colocada(&token, limite("AUG1", "buy", "500.00", "1500", "cancelar-compra")).await;
    assert_eq!(saldos(&pool, cuenta).await, (d("4500"), d("500")));
    let (s, c) = m.cancelar(&token, &compra["order_id"]).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let mut esperada = compra.clone();
    esperada["status"] = json!("cancelled");
    esperada["held_cash"] = json!("0.00000000");
    esperada["closed_at"] = c["closed_at"].clone();
    assert!(c["closed_at"].is_string(), "{c}");
    assert_eq!(c, esperada);
    assert_eq!(saldos(&pool, cuenta).await, (d("5000"), d("0")));
    let id: Uuid = compra["order_id"].as_str().unwrap().parse().unwrap();
    let (concepto, clave_tx): (String, String) = sqlx::query_as(
        "SELECT concepto, clave_idempotencia FROM ifpe.transacciones \
          WHERE tipo = 'LIBERACION_ORDEN' AND ref_movimiento_espejo = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (concepto.as_str(), clave_tx),
        ("Orden límite de compra de AUG1 cancelada", format!("ord:{id}:liberacion"))
    );
    // Cancelarla otra vez: la misma orden cancelada.
    let (s, otra) = m.cancelar(&token, &compra["order_id"]).await;
    assert_eq!((s, &otra), (StatusCode::OK, &c));

    // Una venta: sus títulos se liberan.
    m.colocada(&token, orden("AUG1", "buy", "1250.00", "compra")).await;
    let venta = m.colocada(&token, limite("AUG1", "sell", "500.00", "1700", "cancelar-venta")).await;
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("0.7752"), d("0.2941"), d("1249.93")));
    let (s, c) = m.cancelar(&token, &venta["order_id"]).await;
    assert_eq!((s, &c["status"], &c["held_quantity"]), (StatusCode::OK, &json!("cancelled"), &json!("0.00000000")));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, (d("0.7752"), d("0"), d("1249.93")));

    // La de otro cliente no existe para Beto; ni la ve.
    let beto = con_efectivo(&pool, &cfg, "Beto", "100").await;
    let token_beto = m.token(beto.cliente.id);
    let otra_de_ana = m.colocada(&token, limite("AUG1", "buy", "100.00", "1500", "de-ana")).await;
    let (s, e) = m.cancelar(&token_beto, &otra_de_ana["order_id"]).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::NOT_FOUND, &json!("NO_ENCONTRADO")), "{e}");
    assert_eq!(m.pendientes(&token_beto).await, json!({ "orders": [] }));
    assert_eq!(estado(&pool, &otra_de_ana["order_id"]).await.0, "PENDIENTE");
    // Una ejecutada no se cancela: 409.
    let ejecutada = m.colocada(&token, limite("AUG1", "buy", "100.00", "1700", "ejecutada")).await;
    assert_eq!(ejecutada["status"], "filled", "{ejecutada}");
    let (s, e) = m.cancelar(&token, &ejecutada["order_id"]).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("ORDEN_NO_PENDIENTE")), "{e}");
    // Ids inexistentes o mal formados.
    let (s, _) = m.cancelar(&token, &json!(Uuid::new_v4().to_string())).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = m.cancelar(&token, &json!("no-es-un-uuid")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Solo clientes: el personal interno no cancela.
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&m.estado, oficial_id).await;
    let token_oficial = m.estado.jwt.emitir(oficial_id, RolUsuario::Cumplimiento, true).unwrap().0;
    let (s, _) = m.cancelar(&token_oficial, &otra_de_ana["order_id"]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Con la cuenta congelada por el operador, Ana todavía recupera lo comprometido.
    sqlx::query("UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = $1")
        .bind(cuenta)
        .execute(&pool)
        .await
        .unwrap();
    let (antes, comprometido) = saldos(&pool, cuenta).await;
    assert_eq!(comprometido, d("100"));
    let (s, c) = m.cancelar(&token, &otra_de_ana["order_id"]).await;
    assert_eq!((s, &c["status"]), (StatusCode::OK, &json!("cancelled")), "{c}");
    assert_eq!(saldos(&pool, cuenta).await, (antes + d("100"), d("0")));
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Ventas de mercado y títulos comprometidos
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn una_venta_de_mercado_no_vende_titulos_comprometidos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    let cliente_id = ana.cliente.id;
    let token = m.token(cliente_id);

    m.colocada(&token, orden("AUG1", "buy", "1250.00", "compra")).await;
    // 1 000 MXN a 1 700 son 0.5882 g: quedan libres 0.1870, que valen 301.51888.
    let venta = m.colocada(&token, limite("AUG1", "sell", "1000.00", "1700", "venta-limite")).await;
    assert_eq!(venta["held_quantity"], "0.58820000");
    let (s, codigo) = m.rechazada(&token, orden("AUG1", "sell", "1249.93", "todo")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "POSICION_INSUFICIENTE"));
    // «Max» de los libres: los 0.1870 g, por 301.52.
    let r = m.colocada(&token, orden("AUG1", "sell", "301.51", "libres")).await;
    assert_eq!((&r["quantity"], &r["amount"]), (&json!("0.18700000"), &json!("301.52000000")));
    let (cantidad, comprometida, _) = tenencia(&pool, cliente_id, "AUG1").await;
    assert_eq!((cantidad, comprometida), (d("0.5882"), d("0.5882")));
    // Ya todo está comprometido: ni a mercado ni en otra orden pendiente.
    for cuerpo in [orden("AUG1", "sell", "1.00", "nada"), limite("AUG1", "sell", "1.00", "1800", "nada-pendiente")] {
        let (s, e) = m.ordenar(&token, cuerpo).await;
        assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("POSICION_INSUFICIENTE")));
        assert!(e["error"]["mensaje"].as_str().unwrap_or_default().contains("comprometidos"), "{e}");
    }
    // La bóveda muestra todos sus títulos y cuáles están comprometidos.
    let v = m.boveda(&token).await;
    assert_eq!(
        (&v["holdings"][0]["shares"], &v["holdings"][0]["held_shares"]),
        (&json!("0.58820000"), &json!("0.58820000")),
        "{v}"
    );
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Rechazos al dispararse
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn una_orden_disparada_que_ya_no_procede_se_rechaza(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());

    // Un stop de compra de 10 MXN de SCT28 (enteros, a 9.92) que se dispara a 10.50: ya no alcanza un certificado.
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token_ana = m.token(ana.cliente.id);
    let cetes = m.colocada(&token_ana, stop("SCT28", "buy", "10.00", "9.95", "stop-cetes")).await;
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await, (d("990"), d("10")));
    precio(&pool, "SCT28", "10.50").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(1, 0, 1));
    assert_eq!(estado(&pool, &cetes["order_id"]).await, ("RECHAZADA".into(), Some("MONTO_INSUFICIENTE".into())));
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await, (d("1000"), d("0")));

    // Tres compras límite a 1 600: Beto pierde su KYC, a Caro la congela el
    // operador y Dani sigue en regla. A 1 590 se disparan las tres.
    let mut ordenes = Vec::new();
    for nombre in ["Beto", "Caro", "Dani"] {
        let r = con_efectivo(&pool, &cfg, nombre, "500").await;
        let o = m.colocada(&m.token(r.cliente.id), limite("AUG1", "buy", "100.00", "1600", nombre)).await;
        ordenes.push((r, o));
    }
    revocar_kyc(&pool, ordenes[0].0.cliente.id).await;
    sqlx::query("UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = $1")
        .bind(ordenes[1].0.cuenta_ifpe.id)
        .execute(&pool)
        .await
        .unwrap();
    precio(&pool, "AUG1", "1590").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(3, 1, 2));
    let esperados =
        [("RECHAZADA", Some("KYC_NO_VERIFICADO")), ("RECHAZADA", Some("CUENTA_CONGELADA")), ("EJECUTADA", None)];
    for ((r, o), (estado_esperado, motivo)) in ordenes.iter().zip(esperados) {
        let (e, m_) = estado(&pool, &o["order_id"]).await;
        assert_eq!((e.as_str(), m_.as_deref()), (estado_esperado, motivo), "{o}");
        // Rechazada o no, nada queda comprometido.
        assert_eq!(saldos(&pool, r.cuenta_ifpe.id).await.1, d("0"));
    }
    // Los rechazados recuperan sus 500 completos (también Caro, congelada).
    assert_eq!(saldos(&pool, ordenes[0].0.cuenta_ifpe.id).await.0, d("500"));
    assert_eq!(saldos(&pool, ordenes[1].0.cuenta_ifpe.id).await.0, d("500"));
    // Dani: 100 MXN a 1 590 son 0.0628 g por 99.85.
    assert_eq!(ejecucion(&pool, &ordenes[2].1["order_id"]).await, (d("0.0628"), d("1590"), d("99.85"), d("100")));

    // Un activo suspendido no rechaza la orden: espera a que vuelva a operar.
    let dani = &ordenes[2].0;
    let token_dani = m.token(dani.cliente.id);
    let espera = m.colocada(&token_dani, limite("AUG1", "buy", "100.00", "1580", "espera")).await;
    sqlx::query("UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    precio(&pool, "AUG1", "1570").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));
    let id: Uuid = espera["order_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(pendiente::ejecutar(&pool, &cfg, id).await.unwrap(), Desenlace::SinCambio);
    assert_eq!(estado(&pool, &espera["order_id"]).await.0, "PENDIENTE");
    sqlx::query("UPDATE mercado.activos SET operable = TRUE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(1, 1, 0));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn un_congelamiento_pld_rechaza_sin_aviso_y_devuelve_lo_comprometido(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let fer = con_efectivo(&pool, &cfg, "Fer", "1000").await;
    let cuenta = fer.cuenta_ifpe.id;
    let token = m.token(fer.cliente.id);
    let disparable = m.colocada(&token, limite("AUG1", "buy", "300.00", "1600", "disparable")).await;
    let cancelable = m.colocada(&token, limite("AUG1", "buy", "200.00", "1500", "cancelable")).await;

    // Estructuración: PLD congela al cliente (CONGELADO_PREVENTIVO).
    congelar_por_estructuracion(&pool, &cfg, cuenta).await;
    assert_eq!(estado_cuenta(&pool, cuenta).await, "CONGELADO_PREVENTIVO");
    let disponible = saldos(&pool, cuenta).await.0;

    // No coloca: el error constante, sin aviso.
    let (s, codigo) = m.rechazada(&token, limite("AUG1", "buy", "100.00", "1600", "congelado")).await;
    assert_eq!((s, codigo.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "OPERACION_NO_DISPONIBLE"));
    // Lo disparado se rechaza con el mismo código, y el efectivo regresa (sigue congelado para salir).
    precio(&pool, "AUG1", "1590").await;
    assert_eq!(pendiente::disparar(&pool, &cfg).await.unwrap(), resumen(1, 0, 1));
    assert_eq!(
        estado(&pool, &disparable["order_id"]).await,
        ("RECHAZADA".into(), Some("OPERACION_NO_DISPONIBLE".into()))
    );
    assert_eq!(saldos(&pool, cuenta).await, (disponible + d("300"), d("200")));
    // Y cancelar siempre se puede.
    let (s, c) = m.cancelar(&token, &cancelable["order_id"]).await;
    assert_eq!((s, &c["status"]), (StatusCode::OK, &json!("cancelled")), "{c}");
    assert_eq!(saldos(&pool, cuenta).await, (disponible + d("500"), d("0")));
    // Ninguna compra llegó a PLD ni al ledger del mercado.
    let ordenes: i64 = sqlx::query_scalar("SELECT count(*) FROM mercado.ordenes").fetch_one(&pool).await.unwrap();
    assert_eq!(ordenes, 0);
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// Carreras y auditoría
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn cancelar_y_ejecutar_a_la_vez_cierran_la_orden_una_sola_vez(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id);

    // Dos órdenes de 600 con 1 000 disponibles: solo una compromete.
    let (a, b) = tokio::join!(
        m.ordenar(&token, limite("AUG1", "buy", "600.00", "1500", "simultanea-a")),
        m.ordenar(&token, limite("AUG1", "buy", "600.00", "1500", "simultanea-b")),
    );
    let mut estados = [a.0, b.0];
    estados.sort();
    assert_eq!(estados, [StatusCode::CREATED, StatusCode::UNPROCESSABLE_ENTITY], "{a:?} {b:?}");
    assert_eq!(saldos(&pool, cuenta).await, (d("400"), d("600")));
    let colocada = if a.0 == StatusCode::CREATED { a.1 } else { b.1 };
    let id: Uuid = colocada["order_id"].as_str().unwrap().parse().unwrap();

    // Disparada y cancelada a la vez: exactamente una de las dos la cierra.
    precio(&pool, "AUG1", "1490").await;
    let (cancelacion, ejecucion_) =
        tokio::join!(pendiente::cancelar(&pool, cliente_id, id), pendiente::ejecutar(&pool, &cfg, id));
    match (cancelacion, ejecucion_.unwrap()) {
        (Ok(c), Desenlace::SinCambio) => {
            assert_eq!(c.status, synthetic_capital::mercado::EstadoPendiente::Cancelada);
            assert_eq!(saldos(&pool, cuenta).await, (d("1000"), d("0")));
        }
        (Err(AppError::Conflicto { codigo, .. }), Desenlace::Ejecutada) => {
            assert_eq!(codigo, "ORDEN_NO_PENDIENTE");
            assert_eq!(saldos(&pool, cuenta).await.1, d("0"));
            assert_eq!(tenencia(&pool, cliente_id, "AUG1").await.0, d("0.4026"), "600 MXN a 1 490");
        }
        otro => panic!("cierre inesperado: {otro:?}"),
    }
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn la_auditoria_detecta_efectivo_comprometido_sin_orden(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = m.token(ana.cliente.id);
    m.colocada(&token, limite("AUG1", "buy", "300.00", "1500", "auditada")).await;
    assert!(hash::verificar_cadena_mercado(&pool).await.unwrap().valido);

    // El dueño del esquema desactiva los triggers y "cierra" la orden sin
    // devolver el efectivo: la cuenta queda con 300 comprometidos sin orden.
    sqlx::raw_sql(
        "ALTER TABLE mercado.ordenes_pendientes DISABLE TRIGGER USER; \
         UPDATE mercado.ordenes_pendientes SET estado = 'CANCELADA', cerrado_en = now(), \
                tx_liberacion_id = gen_random_uuid(); \
         ALTER TABLE mercado.ordenes_pendientes ENABLE TRIGGER USER;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    assert!(
        !r.valido
            && r.discrepancias.iter().any(|d| d.contains("300.00000000 comprometido; sus compras pendientes suman 0")),
        "{r:?}"
    );
}
