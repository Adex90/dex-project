//! Planes de ahorro (migración 0023, `mercado::planes`, `routes/mercado.rs`)
//! por la API y el programador: el alta con su primera compra y su
//! calendario, la idempotencia, las reglas, cada compra como una orden de
//! mercado (ledger encadenado, movimiento IFPE, reserva del 100 % y PLD), la
//! compra omitida sin efectivo que no rompe el plan, pausar, reanudar y
//! cancelar, el congelamiento PLD sin aviso y las carreras. Los números son
//! los de tests/db/0023_planes_ahorro.sql. Las reglas de la base de datos
//! (calendario, términos inmutables, bitácora) están en esa suite.

mod comun;

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use comun::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    config::Config,
    kyc::Registro,
    ledger::hash,
    mercado::planes::{self, Cambio, Compra, ResumenPlanes},
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

    /// Token de una sesión real del cliente (su fila en `kyc.sesiones`).
    async fn token(&self, cliente_id: Uuid) -> String {
        token_de(&self.estado, cliente_id, RolUsuario::Cliente, false).await
    }

    async fn ordenar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        llamar_json(&self.app, "POST", "/api/v1/market/order", Some(token), Some(cuerpo)).await
    }

    /// Un plan que debe crearse (201): la respuesta.
    async fn creado(&self, token: &str, cuerpo: Value) -> Value {
        let (s, p) = self.ordenar(token, cuerpo.clone()).await;
        assert_eq!(s, StatusCode::CREATED, "{cuerpo}: {p}");
        p
    }

    /// Un rechazo: estado y `error.codigo`.
    async fn rechazado(&self, token: &str, cuerpo: Value) -> (StatusCode, String) {
        let (s, e) = self.ordenar(token, cuerpo).await;
        (s, e["error"]["codigo"].as_str().unwrap_or_else(|| panic!("sin error.codigo: {e}")).to_string())
    }

    async fn planes(&self, token: &str) -> Value {
        let (s, l) = llamar_json(&self.app, "GET", "/api/v1/market/savings-plans", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{l}");
        l
    }

    /// `accion`: pause, resume o cancel.
    async fn cambiar(&self, token: &str, id: &Value, accion: &str) -> (StatusCode, Value) {
        let ruta = format!("/api/v1/market/savings-plans/{}/{accion}", id.as_str().unwrap());
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

fn plan(ticker: &str, monto: &str, frecuencia: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "side": "buy", "amount": monto, "idempotency_key": clave_de(etiqueta),
            "order_type": "savings_plan", "frequency": frecuencia })
}

fn orden(ticker: &str, lado: &str, monto: &str, etiqueta: &str) -> Value {
    json!({ "ticker": ticker, "side": lado, "amount": monto, "idempotency_key": clave_de(etiqueta) })
}

/// Cliente con KYC verificado y `monto` en su bóveda.
async fn con_efectivo(pool: &PgPool, cfg: &Config, nombre: &str, monto: &str) -> Registro {
    let r = cliente(pool, cfg, nombre).await;
    depositar(pool, cfg, r.cuenta_ifpe.id, monto).await;
    r
}

fn id_de(p: &Value) -> Uuid {
    p["plan_id"].as_str().unwrap_or_else(|| panic!("sin plan_id: {p}")).parse().unwrap()
}

fn instante(v: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(v.as_str().unwrap_or_else(|| panic!("no es una fecha: {v}"))).unwrap().to_utc()
}

/// El saldo disponible de la cuenta IFPE.
async fn disponible(pool: &PgPool, cuenta: Uuid) -> Decimal {
    sqlx::query_scalar("SELECT saldo_disponible FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Los títulos de la posición (cero si no hay).
async fn tenencia(pool: &PgPool, cliente: Uuid, ticker: &str) -> Decimal {
    sqlx::query_scalar("SELECT cantidad FROM mercado.posiciones WHERE cliente_id = $1 AND ticker = $2")
        .bind(cliente)
        .bind(ticker)
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap_or_default()
}

async fn version_ordenes(pool: &PgPool, cuenta: Uuid) -> i64 {
    sqlx::query_scalar("SELECT version_ordenes FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Las compras `periodos` del calendario del plan, como las calcula la base de datos.
async fn calendario(pool: &PgPool, plan: Uuid, periodos: &[i32]) -> Vec<DateTime<Utc>> {
    sqlx::query_scalar(
        "SELECT mercado.momento_plan(p.frecuencia, p.fecha_ancla, k) \
           FROM mercado.planes_ahorro p, unnest($2::int[]) AS k WHERE p.id = $1 ORDER BY k",
    )
    .bind(plan)
    .bind(periodos)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// La bitácora del plan: (evento, periodo, motivo), en orden.
async fn eventos(pool: &PgPool, plan: Uuid) -> Vec<(String, Option<i32>, Option<String>)> {
    sqlx::query_as("SELECT evento, periodo, motivo FROM mercado.eventos_plan WHERE plan_id = $1 ORDER BY id")
        .bind(plan)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Las órdenes del ledger del mercado de un plan: (clave, cantidad, precio, monto, monto_solicitado).
async fn compras(pool: &PgPool, plan: Uuid) -> Vec<(String, Decimal, Decimal, Decimal, Decimal)> {
    sqlx::query_as(
        "SELECT clave_idempotencia, cantidad, precio, monto, monto_solicitado FROM mercado.ordenes \
          WHERE clave_idempotencia LIKE 'plan:' || $1 || ':%' ORDER BY secuencia",
    )
    .bind(plan.to_string())
    .fetch_all(pool)
    .await
    .unwrap()
}

fn resumen(revisados: usize, ejecutados: usize, omitidos: usize) -> ResumenPlanes {
    ResumenPlanes { revisados, ejecutados, omitidos, errores: 0 }
}

fn ev(evento: &str, periodo: Option<i32>, motivo: Option<&str>) -> (String, Option<i32>, Option<String>) {
    (evento.to_string(), periodo, motivo.map(str::to_string))
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

// -----------------------------------------------------------------------------
// Crear: la primera compra, el calendario, la idempotencia
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn un_plan_nace_con_su_primera_compra_y_su_calendario(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id).await;
    let version = version_ordenes(&pool, cuenta).await;

    // 500 MXN de oro cada semana: la primera compra es ya, 0.3100 g por 499.84.
    let cuerpo = plan("AUG1", "500.00", "weekly", "plan-oro");
    let p = m.creado(&token, cuerpo.clone()).await;
    let id = id_de(&p);
    assert_eq!(
        compras(&pool, id).await,
        vec![(format!("plan:{id}:0"), d("0.31"), d("1612.40"), d("499.84"), d("500"))]
    );
    let orden_id: Uuid = sqlx::query_scalar("SELECT ultima_orden_id FROM mercado.planes_ahorro WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let ancla: String = sqlx::query_scalar("SELECT ((now() AT TIME ZONE 'America/Mexico_City')::date)::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        p,
        json!({
            "plan_id": id,
            "order_type": "savings_plan",
            "status": "active",
            "ticker": "AUG1",
            "side": "buy",
            "amount": "500.00000000",
            "frequency": "weekly",
            "anchor_date": ancla,
            "next_execution_at": p["next_execution_at"],
            "upcoming": p["upcoming"],
            "last_execution_at": p["last_execution_at"],
            "last_result": "executed",
            "failure_reason": null,
            "last_purchase": {
                "order_id": orden_id,
                "quantity": "0.31000000",
                "price": "1612.40000000",
                "amount": "499.84000000",
                "commission": "0.00000000",
                "executed_at": p["last_purchase"]["executed_at"],
            },
            "purchases": 1,
            "skipped": 0,
            "invested": "499.84000000",
            "quantity": "0.31000000",
            "created_at": p["created_at"],
            "cancelled_at": null,
        })
    );
    // El calendario: cada semana desde el ancla, a las 9:00 de la Ciudad de México (15:00 UTC).
    let fechas = calendario(&pool, id, &[1, 2, 3]).await;
    assert_eq!(instante(&p["next_execution_at"]), fechas[0]);
    assert_eq!(p["upcoming"].as_array().unwrap().iter().map(instante).collect::<Vec<_>>(), fechas);
    assert!(fechas[0] > Utc::now(), "la siguiente compra es futura: {fechas:?}");
    assert_eq!((fechas[1] - fechas[0], fechas[2] - fechas[1]), (Duration::days(7), Duration::days(7)));
    assert_eq!(fechas[0].format("%H:%M").to_string(), "15:00");
    assert_eq!(fechas[0].date_naive().to_string(), {
        let a: chrono::NaiveDate = ancla.parse().unwrap();
        (a + Duration::days(7)).to_string()
    });
    let creado = instante(&p["created_at"]);
    assert!(instante(&p["last_execution_at"]) >= creado);

    // Una orden de mercado como cualquier otra: el movimiento IFPE, PLD y la posición.
    let (tipo, monto, espejo): (String, Decimal, Uuid) = sqlx::query_as(
        "SELECT tipo, monto, ref_movimiento_espejo FROM ifpe.transacciones WHERE clave_idempotencia = $1",
    )
    .bind(format!("plan:{id}:0"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((tipo.as_str(), monto, espejo), ("COMPRA_MERCADO", d("499.84"), orden_id));
    let evaluadas = evaluaciones_de(&pool, cliente_id)
        .await
        .into_iter()
        .filter(|(canal, resultado, _, _)| canal == "COMPRA_MERCADO" && resultado == "PERMITIDA")
        .count();
    assert_eq!(evaluadas, 1);
    assert_eq!(disponible(&pool, cuenta).await, d("19500.16"));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, d("0.31"));
    let v = m.boveda(&token).await;
    assert_eq!(v["balance"]["available_cash"], "19500.16000000", "{v}");
    // La bitácora (alta y compra), y la bóveda avisó por cada evento.
    assert_eq!(eventos(&pool, id).await, vec![ev("CREADO", None, None), ev("EJECUTADO", Some(0), None)]);
    assert_eq!(version_ordenes(&pool, cuenta).await, version + 2);

    // La lista lo trae tal cual.
    assert_eq!(m.planes(&token).await, json!({ "plans": [p.clone()] }));

    // La misma clave: el mismo plan (200), sin otra compra.
    let (s, r) = m.ordenar(&token, cuerpo).await;
    assert_eq!((s, &r), (StatusCode::OK, &p));
    assert_eq!(compras(&pool, id).await.len(), 1);
    // La misma clave con otros términos, o para una orden: 409. Y al revés.
    m.creado(&token, orden("AUG1", "buy", "100.00", "mercado-1")).await;
    for otra in [
        plan("AUG1", "500.00", "monthly", "plan-oro"),
        plan("AUG1", "400.00", "weekly", "plan-oro"),
        orden("AUG1", "buy", "500.00", "plan-oro"),
        json!({ "ticker": "AUG1", "side": "buy", "amount": "500.00", "idempotency_key": clave_de("plan-oro"),
                "order_type": "limit", "limit_price": "1500" }),
        plan("AUG1", "100.00", "weekly", "mercado-1"),
    ] {
        let (s, codigo) = m.rechazado(&token, otra.clone()).await;
        assert_eq!((s, codigo.as_str()), (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA"), "{otra}");
    }

    // Proof of Reserve y el ledger del mercado, con la compra del plan.
    let corte = reserva::generar_corte(&pool).await.expect("corte con la compra de un plan");
    assert_eq!(corte.total_pasivo, corte.total_reserva);
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    assert_eq!((r.valido, r.movimientos_verificados), (true, 2), "{r:?}");
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn reglas_de_los_planes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = m.token(ana.cliente.id).await;

    let base = |extra: Value| {
        let mut o = plan("AUG1", "100.00", "weekly", &format!("regla-{}", Uuid::new_v4().simple()));
        o.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        o
    };
    let (q, u) = (StatusCode::BAD_REQUEST, StatusCode::UNPROCESSABLE_ENTITY);
    let casos: Vec<(Value, StatusCode, &str)> = vec![
        // La forma: una compra con su frecuencia, sin precios.
        (base(json!({ "frequency": null })), q, "VALIDACION"),
        (base(json!({ "frequency": "daily" })), q, "VALIDACION"),
        (base(json!({ "frequency": "WEEKLY" })), q, "VALIDACION"),
        (base(json!({ "side": "sell" })), q, "VALIDACION"),
        (base(json!({ "limit_price": "1600" })), q, "VALIDACION"),
        (base(json!({ "stop_price": "1700" })), q, "VALIDACION"),
        (base(json!({ "amount": "100.005" })), q, "VALIDACION"),
        (base(json!({ "amount": "0" })), q, "VALIDACION"),
        (base(json!({ "amount": "1000000000" })), q, "VALIDACION"),
        (base(json!({ "amount": 100 })), q, ""),
        (base(json!({ "order_type": "market" })), q, "VALIDACION"),
        // Un monto que hoy no compra un incremento, un activo que no existe.
        (base(json!({ "amount": "0.10" })), u, "MONTO_INSUFICIENTE"),
        (base(json!({ "ticker": "XAU" })), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
        (base(json!({ "ticker": "aug1" })), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
    ];
    for (cuerpo, status, codigo) in casos {
        let (s, e) = m.ordenar(&token, cuerpo.clone()).await;
        assert_eq!(s, status, "{cuerpo}: {e}");
        if !codigo.is_empty() {
            assert_eq!(e["error"]["codigo"], codigo, "{cuerpo}: {e}");
        }
    }
    // Un activo suspendido no recibe planes.
    sqlx::query("UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    let (s, codigo) = m.rechazado(&token, plan("AUG1", "100.00", "monthly", "suspendido")).await;
    assert_eq!((s, codigo.as_str()), (u, "MERCADO_CERRADO"));
    sqlx::query("UPDATE mercado.activos SET operable = TRUE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    // Sin MODO_DEMO, un precio simulado no se opera.
    let produccion = Mercado::nuevo(&pool, Config { modo_demo: false, ..cfg.clone() });
    let (s, codigo) = produccion
        .rechazado(&produccion.token(ana.cliente.id).await, plan("AUG1", "100.00", "weekly", "produccion"))
        .await;
    assert_eq!((s, codigo.as_str()), (StatusCode::SERVICE_UNAVAILABLE, "MERCADO_NO_DISPONIBLE"));
    // Sin KYC verificado, no.
    let carla = cliente_sin_kyc(&pool, &cfg, "Carla").await;
    let (s, codigo) = m.rechazado(&m.token(carla.cliente.id).await, plan("AUG1", "100.00", "weekly", "sin-kyc")).await;
    assert_eq!((s, codigo.as_str()), (u, "KYC_NO_VERIFICADO"));
    // Ni el personal interno.
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&m.estado, oficial_id).await;
    let interno = token_de(&m.estado, oficial_id, RolUsuario::Cumplimiento, true).await;
    let (s, _) = llamar_json(&m.app, "GET", "/api/v1/market/savings-plans", Some(&interno), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = m.ordenar(&interno, plan("AUG1", "100.00", "weekly", "interno")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Nada de lo rechazado creó un plan ni movió dinero.
    let planes_: i64 = sqlx::query_scalar("SELECT count(*) FROM mercado.planes_ahorro").fetch_one(&pool).await.unwrap();
    assert_eq!(planes_, 0);
    assert_eq!(disponible(&pool, ana.cuenta_ifpe.id).await, d("1000"));
    assert_eq!(m.planes(&token).await, json!({ "plans": [] }));

    // A lo más 20 planes abiertos por cliente; los cancelados no cuentan.
    // Beto no tiene efectivo: cada primera compra se omite y el plan sigue.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_beto = m.token(beto.cliente.id).await;
    let mut primero = Value::Null;
    for i in 0..planes::MAXIMO_PLANES {
        let p = m.creado(&tok_beto, plan("CETES28", "10.00", "monthly", &format!("tope-{i}"))).await;
        assert_eq!((&p["status"], &p["last_result"]), (&json!("active"), &json!("skipped")), "{p}");
        assert_eq!(p["failure_reason"], "SALDO_INSUFICIENTE", "{p}");
        if i == 0 {
            primero = p["plan_id"].clone();
        }
    }
    let (s, codigo) = m.rechazado(&tok_beto, plan("CETES28", "10.00", "monthly", "tope-de-mas")).await;
    assert_eq!((s, codigo.as_str()), (u, "LIMITE_PLANES"));
    let (s, c) = m.cambiar(&tok_beto, &primero, "cancel").await;
    assert_eq!((s, &c["status"]), (StatusCode::OK, &json!("cancelled")), "{c}");
    m.creado(&tok_beto, plan("CETES28", "10.00", "monthly", "tope-otro")).await;
    assert_eq!(m.planes(&tok_beto).await["plans"].as_array().unwrap().len(), 20);
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// El programador: cada compra en su fecha; sin efectivo, omitida
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_programador_compra_en_cada_fecha_y_omite_sin_efectivo(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "600").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id).await;
    let p = m.creado(&token, plan("AUG1", "500.00", "weekly", "plan-semanal")).await;
    let id = id_de(&p);
    assert_eq!(disponible(&pool, cuenta).await, d("100.16"));

    // Nada vence antes de su fecha.
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));
    assert_eq!(planes::ejecutar(&pool, &cfg, id).await.unwrap(), Compra::SinCambio);

    // Llega la segunda compra con 100.16 disponibles: se omite y el plan sigue.
    vencer_plan(&pool, id).await;
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(1, 0, 1));
    let fechas = calendario(&pool, id, &[2, 3, 4]).await;
    let l = m.planes(&token).await;
    let o = &l["plans"][0];
    assert_eq!(
        (&o["status"], &o["last_result"], &o["failure_reason"], &o["purchases"], &o["skipped"]),
        (&json!("active"), &json!("skipped"), &json!("SALDO_INSUFICIENTE"), &json!(1), &json!(1)),
        "{o}"
    );
    assert_eq!(instante(&o["next_execution_at"]), fechas[0], "la siguiente es la del periodo 2");
    assert_eq!(o["upcoming"].as_array().unwrap().iter().map(instante).collect::<Vec<_>>(), fechas);
    assert_eq!(o["last_purchase"], p["last_purchase"], "la compra hecha más reciente sigue siendo la primera");
    assert_eq!(disponible(&pool, cuenta).await, d("100.16"));
    // Ni PLD ni el ledger vieron la omitida.
    assert_eq!(compras(&pool, id).await.len(), 1);
    assert_eq!(evaluaciones_de(&pool, cliente_id).await.iter().filter(|e| e.0 == "COMPRA_MERCADO").count(), 1);

    // Con efectivo y a otro precio, la tercera se compra: 500 MXN a 1 600 son 0.3125 g.
    depositar(&pool, &cfg, cuenta, "1000").await;
    precio(&pool, "AUG1", "1600").await;
    vencer_plan(&pool, id).await;
    let version = version_ordenes(&pool, cuenta).await;
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(1, 1, 0));
    assert_eq!(
        compras(&pool, id).await,
        vec![
            (format!("plan:{id}:0"), d("0.31"), d("1612.40"), d("499.84"), d("500")),
            (format!("plan:{id}:2"), d("0.3125"), d("1600"), d("500.00"), d("500")),
        ]
    );
    assert_eq!(disponible(&pool, cuenta).await, d("600.16"));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, d("0.6225"));
    assert_eq!(version_ordenes(&pool, cuenta).await, version + 1);
    let o = m.planes(&token).await["plans"][0].clone();
    assert_eq!(
        (&o["last_result"], &o["failure_reason"], &o["purchases"], &o["skipped"], &o["invested"], &o["quantity"]),
        (&json!("executed"), &Value::Null, &json!(2), &json!(1), &json!("999.84000000"), &json!("0.62250000")),
        "{o}"
    );
    assert_eq!(
        (&o["last_purchase"]["price"], &o["last_purchase"]["amount"]),
        (&json!("1600.00000000"), &json!("500.00000000"))
    );
    assert_eq!(instante(&o["next_execution_at"]), calendario(&pool, id, &[3]).await[0]);
    assert_eq!(
        eventos(&pool, id).await,
        vec![
            ev("CREADO", None, None),
            ev("EJECUTADO", Some(0), None),
            ev("OMITIDO", Some(1), Some("SALDO_INSUFICIENTE")),
            ev("EJECUTADO", Some(2), None),
        ]
    );

    // Un activo suspendido en su fecha: la compra se omite (no espera), y el plan sigue.
    sqlx::query("UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'").execute(&pool).await.unwrap();
    vencer_plan(&pool, id).await;
    assert_eq!(planes::ejecutar(&pool, &cfg, id).await.unwrap(), Compra::Omitida("MERCADO_CERRADO".into()));
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));
    assert_eq!(disponible(&pool, cuenta).await, d("600.16"));

    // PLD evaluó las dos compras hechas; el ledger del mercado las encadena.
    assert_eq!(evaluaciones_de(&pool, cliente_id).await.iter().filter(|e| e.0 == "COMPRA_MERCADO").count(), 2);
    let r = hash::verificar_cadena_mercado(&pool).await.unwrap();
    assert_eq!((r.valido, r.movimientos_verificados), (true, 2), "{r:?}");
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// Pausar, reanudar, cancelar
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn pausar_reanudar_y_cancelar(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id).await;
    let p = m.creado(&token, plan("AUG1", "500.00", "biweekly", "plan-quincenal")).await;
    let id = id_de(&p);
    let plan_id = p["plan_id"].clone();
    let siguiente = calendario(&pool, id, &[1, 2, 3]).await;
    assert_eq!(siguiente[1] - siguiente[0], Duration::days(14));

    // Pausado: sin compras programadas. Repetirlo devuelve lo mismo.
    let (s, pausado) = m.cambiar(&token, &plan_id, "pause").await;
    assert_eq!(s, StatusCode::OK, "{pausado}");
    let mut esperado = p.clone();
    esperado["status"] = json!("paused");
    esperado["next_execution_at"] = Value::Null;
    esperado["upcoming"] = json!([]);
    assert_eq!(pausado, esperado);
    assert_eq!(m.cambiar(&token, &plan_id, "pause").await, (StatusCode::OK, pausado.clone()));
    assert_eq!(m.planes(&token).await, json!({ "plans": [pausado] }));
    assert_eq!(planes::ejecutar(&pool, &cfg, id).await.unwrap(), Compra::SinCambio);
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));

    // Reanudado: en la primera fecha de su calendario que aún no llega.
    let (s, reanudado) = m.cambiar(&token, &plan_id, "resume").await;
    assert_eq!((s, &reanudado), (StatusCode::OK, &p), "la misma siguiente compra: aún no llegaba");
    assert_eq!(m.cambiar(&token, &plan_id, "resume").await, (StatusCode::OK, p.clone()));

    // Una pausa larga: las compras que pasaron no se reponen. (El dueño del
    // esquema mueve el ancla 30 días atrás, como si el plan llevara un mes.)
    m.cambiar(&token, &plan_id, "pause").await;
    let mut tx = pool.begin().await.unwrap();
    for sql in [
        "ALTER TABLE mercado.planes_ahorro DISABLE TRIGGER trg_planes_b_transicion",
        "UPDATE mercado.planes_ahorro SET fecha_ancla = fecha_ancla - 30 WHERE id = $1",
        "ALTER TABLE mercado.planes_ahorro ENABLE TRIGGER trg_planes_b_transicion",
    ] {
        sqlx::query(sql).bind(id).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let (_, reanudado) = m.cambiar(&token, &plan_id, "resume").await;
    // Ancla - 30: las compras 1 (día -16) y 2 (día -2) ya pasaron; la siguiente es la 3 (día +12).
    let fechas = calendario(&pool, id, &[3, 4, 5]).await;
    assert_eq!(instante(&reanudado["next_execution_at"]), fechas[0], "{reanudado}");
    assert_eq!(reanudado["upcoming"].as_array().unwrap().iter().map(instante).collect::<Vec<_>>(), fechas);
    assert!(fechas[0] > Utc::now() && fechas[0] < Utc::now() + Duration::days(14));

    // Cancelado: para siempre. Sus compras se quedan; la lista ya no lo trae.
    let (s, cancelado) = m.cambiar(&token, &plan_id, "cancel").await;
    assert_eq!((s, &cancelado["status"]), (StatusCode::OK, &json!("cancelled")), "{cancelado}");
    assert_eq!((&cancelado["next_execution_at"], &cancelado["upcoming"]), (&Value::Null, &json!([])));
    assert!(instante(&cancelado["cancelled_at"]) >= instante(&p["created_at"]));
    assert_eq!(m.cambiar(&token, &plan_id, "cancel").await, (StatusCode::OK, cancelado.clone()));
    for accion in ["pause", "resume"] {
        let (s, e) = m.cambiar(&token, &plan_id, accion).await;
        assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("PLAN_CANCELADO")), "{accion}: {e}");
    }
    assert_eq!(m.planes(&token).await, json!({ "plans": [] }));
    assert_eq!(tenencia(&pool, cliente_id, "AUG1").await, d("0.31"));
    assert_eq!(disponible(&pool, cuenta).await, d("19500.16"));

    // El plan de otro cliente no existe para Beto; un id que no es de nadie, tampoco.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_beto = m.token(beto.cliente.id).await;
    let de_ana = m.creado(&token, plan("AUG1", "500.00", "monthly", "plan-mensual")).await;
    for accion in ["pause", "resume", "cancel"] {
        let (s, e) = m.cambiar(&tok_beto, &de_ana["plan_id"], accion).await;
        assert_eq!((s, &e["error"]["codigo"]), (StatusCode::NOT_FOUND, &json!("NO_ENCONTRADO")), "{e}");
        let (s, _) = m.cambiar(&token, &json!(Uuid::new_v4().to_string()), accion).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let (s, _) =
        llamar_json(&m.app, "POST", "/api/v1/market/savings-plans/no-es-un-id/pause", Some(&token), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(m.planes(&tok_beto).await, json!({ "plans": [] }));
    assert_eq!(m.planes(&token).await["plans"].as_array().unwrap().len(), 1);

    // Cada cambio, a la bitácora del plan.
    assert_eq!(
        eventos(&pool, id).await,
        vec![
            ev("CREADO", None, None),
            ev("EJECUTADO", Some(0), None),
            ev("PAUSADO", None, None),
            ev("REANUDADO", None, None),
            ev("PAUSADO", None, None),
            ev("REANUDADO", None, None),
            ev("CANCELADO", None, None),
        ]
    );
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// PLD y carreras
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn un_congelamiento_pld_omite_las_compras_sin_aviso(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let fer = con_efectivo(&pool, &cfg, "Fer", "1000").await;
    let (cliente_id, cuenta) = (fer.cliente.id, fer.cuenta_ifpe.id);
    let token = m.token(cliente_id).await;
    let p = m.creado(&token, plan("AUG1", "500.00", "weekly", "plan-fer")).await;
    let id = id_de(&p);

    // Estructuración: PLD congela al cliente (CONGELADO_PREVENTIVO).
    congelar_por_estructuracion(&pool, &cfg, cuenta).await;
    assert_eq!(estado_cuenta(&pool, cuenta).await, "CONGELADO_PREVENTIVO");
    let intentos = congelamiento_activo(&pool, cliente_id).await.unwrap().2;
    let saldo = disponible(&pool, cuenta).await;

    // No crea otro plan: el error constante, sin aviso.
    let (s, e) = m.ordenar(&token, plan("AUG1", "100.00", "weekly", "plan-congelado")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("OPERACION_NO_DISPONIBLE")));
    let (_, otra) = m.ordenar(&token, orden("AUG1", "sell", "100.00", "venta-congelada")).await;
    assert_eq!(e, otra, "el mismo error que cualquier otra operación bloqueada");
    // La compra en su fecha se omite con el mismo código, y el plan sigue.
    vencer_plan(&pool, id).await;
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(1, 0, 1));
    let o = m.planes(&token).await["plans"][0].clone();
    assert_eq!(
        (&o["status"], &o["last_result"], &o["failure_reason"]),
        (&json!("active"), &json!("skipped"), &json!("OPERACION_NO_DISPONIBLE")),
        "{o}"
    );
    // El programador no es un intento del cliente: no cuenta ni evalúa.
    assert_eq!(congelamiento_activo(&pool, cliente_id).await.unwrap().2, intentos);
    assert_eq!(evaluaciones_de(&pool, cliente_id).await.iter().filter(|e| e.0 == "COMPRA_MERCADO").count(), 1);
    assert_eq!(disponible(&pool, cuenta).await, saldo);

    // Ninguna lectura de sus planes delata la situación.
    let (_, cuerpo) = llamar_json(&m.app, "GET", "/api/v1/market/savings-plans", Some(&token), None).await;
    let texto = cuerpo.to_string().to_lowercase();
    for palabra in
        ["aml", "pld", "alerta", "cumplimiento", "uif", "sospech", "congel", "preventiv", "riesgo", "retenido"]
    {
        assert!(!texto.contains(palabra), "los planes contienen «{palabra}»: {texto}");
    }
    // Pausar y cancelar siempre se puede.
    let (s, c) = m.cambiar(&token, &p["plan_id"], "pause").await;
    assert_eq!((s, &c["status"]), (StatusCode::OK, &json!("paused")), "{c}");
    let (s, c) = m.cambiar(&token, &p["plan_id"], "cancel").await;
    assert_eq!((s, &c["status"]), (StatusCode::OK, &json!("cancelled")), "{c}");
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn dos_pasadas_a_la_vez_compran_una_sola_vez(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let m = Mercado::nuevo(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "20000").await;
    let (cliente_id, cuenta) = (ana.cliente.id, ana.cuenta_ifpe.id);
    let token = m.token(cliente_id).await;

    // La misma alta dos veces a la vez: un plan, una primera compra.
    let cuerpo = plan("AUG1", "500.00", "weekly", "plan-doble");
    let (a, b) = tokio::join!(m.ordenar(&token, cuerpo.clone()), m.ordenar(&token, cuerpo.clone()));
    let mut estados = [a.0, b.0];
    estados.sort();
    assert_eq!(estados, [StatusCode::OK, StatusCode::CREATED], "{a:?} {b:?}");
    assert_eq!(a.1["plan_id"], b.1["plan_id"]);
    let id = id_de(&a.1);
    assert_eq!(compras(&pool, id).await.len(), 1);

    // Dos réplicas del programador sobre la misma compra vencida: una la hace.
    vencer_plan(&pool, id).await;
    let (x, y) = tokio::join!(planes::ejecutar_vencidos(&pool, &cfg), planes::ejecutar_vencidos(&pool, &cfg));
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!((x.ejecutados + y.ejecutados, x.omitidos + y.omitidos, x.errores + y.errores), (1, 0, 0), "{x:?} {y:?}");
    assert_eq!(compras(&pool, id).await.len(), 2);
    assert_eq!(disponible(&pool, cuenta).await, d("19000.32"));

    // Pausar mientras compra: una de dos, y el plan queda coherente.
    vencer_plan(&pool, id).await;
    let (pausa, compra) =
        tokio::join!(planes::cambiar(&pool, cliente_id, id, Cambio::Pausar), planes::ejecutar(&pool, &cfg, id));
    let (pausa, compra) = (pausa.unwrap(), compra.unwrap());
    assert_eq!(pausa.status, planes::EstadoPlan::Pausado);
    let hechas = compras(&pool, id).await.len();
    match compra {
        Compra::Ejecutada => assert_eq!(hechas, 3),
        Compra::SinCambio => assert_eq!(hechas, 2),
        otro => panic!("compra inesperada: {otro:?}"),
    }
    assert_eq!(pausa.compras, i64::try_from(hechas).unwrap());
    assert_eq!(planes::ejecutar_vencidos(&pool, &cfg).await.unwrap(), resumen(0, 0, 0));
    verificar_invariantes(&pool).await;
}
