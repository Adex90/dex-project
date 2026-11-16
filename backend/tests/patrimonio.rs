//! Cuentas vinculadas y retiros (migración 0016, `patrimonio`,
//! `routes/patrimonio.rs`) por la API: la cuenta de demostración a nombre del
//! cliente, el retiro que descuenta el disponible (nunca lo comprometido en
//! órdenes) y queda en proceso con la reserva del 100 % intacta, la
//! idempotencia, la lista blanca en las dos rutas de retiro a cuenta propia
//! (después de PLD: a un cliente congelado se le responde lo de siempre), KYC
//! y la entrada. Las reglas de la base de datos están en
//! tests/db/0016_cuentas_vinculadas.sql y el aviso por WebSocket en
//! tests/boveda.rs.

mod comun;

use axum::http::StatusCode;
use comun::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    config::Config, db, kyc::Registro, models::kyc::RolUsuario, patrimonio::vinculadas::clabe_demo, routes,
    spei::clabe, AppState,
};
use uuid::Uuid;

struct Api {
    app: axum::Router,
    estado: AppState,
}

impl Api {
    fn nueva(pool: &PgPool, cfg: Config) -> Self {
        let estado = AppState::nuevo(pool.clone(), cfg);
        Self { app: routes::router(estado.clone()), estado }
    }

    fn token(&self, cliente_id: Uuid) -> String {
        self.estado.jwt.emitir(cliente_id, RolUsuario::Cliente, false).unwrap().0
    }

    async fn cuentas(&self, token: &str) -> Value {
        let (s, v) = llamar_json(&self.app, "GET", "/api/v1/wealth/linked-accounts", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    /// La primera cuenta vinculada del cliente (con MODO_DEMO, la de demostración).
    async fn primera(&self, token: &str) -> Value {
        let v = self.cuentas(token).await;
        assert_eq!(v["accounts"].as_array().map(Vec::len), Some(1), "{v}");
        v["accounts"][0].clone()
    }

    async fn retirar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        llamar_json(&self.app, "POST", "/api/v1/wealth/withdraw", Some(token), Some(cuerpo)).await
    }

    /// Un retiro rechazado: estado y `error.codigo`.
    async fn rechazo(&self, token: &str, cuerpo: Value) -> (StatusCode, String) {
        let (s, e) = self.retirar(token, cuerpo).await;
        (s, e["error"]["codigo"].as_str().unwrap_or_else(|| panic!("sin error.codigo: {e}")).to_string())
    }

    /// `POST /api/v1/ifpe/retiros`: el retiro a cuenta propia por CLABE.
    async fn retiro_por_clabe(&self, token: &str, clabe: &str, monto: &str) -> (StatusCode, Value) {
        let cuerpo = json!({ "clabe_destino": clabe, "monto": monto, "clave_idempotencia": clave_api() });
        llamar_json(&self.app, "POST", "/api/v1/ifpe/retiros", Some(token), Some(cuerpo)).await
    }
}

/// Clave de idempotencia válida en la API.
fn clave_api() -> String {
    Uuid::new_v4().to_string()
}

fn retiro(cuenta: &Value, monto: &str, clave: &str) -> Value {
    json!({ "linked_account_id": cuenta["id"], "amount": monto, "idempotency_key": clave })
}

/// CLABE externa válida (Banorte) que nadie tiene vinculada.
fn externa(n: u64) -> String {
    let primeros = format!("072180{n:011}");
    format!("{primeros}{}", clabe::digito_verificador(&primeros).unwrap())
}

/// Cliente con KYC verificado y `monto` en su bóveda.
async fn con_efectivo(pool: &PgPool, cfg: &Config, nombre: &str, monto: &str) -> Registro {
    let r = cliente(pool, cfg, nombre).await;
    depositar(pool, cfg, r.cuenta_ifpe.id, monto).await;
    r
}

/// (disponible, comprometido en órdenes) de la cuenta IFPE.
async fn saldos(pool: &PgPool, cuenta: Uuid) -> (Decimal, Decimal) {
    sqlx::query_as("SELECT saldo_disponible, saldo_comprometido FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// El nombre del cliente en kyc.clientes, en mayúsculas: el titular de su
/// cuenta de demostración.
async fn nombre_kyc(pool: &PgPool, cliente: Uuid) -> String {
    sqlx::query_scalar(
        "SELECT upper(concat_ws(' ', nombre, apellido_paterno, apellido_materno)) FROM kyc.clientes WHERE id = $1",
    )
    .bind(cliente)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn vinculadas_de(pool: &PgPool, cliente: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ifpe.cuentas_vinculadas WHERE cliente_id = $1")
        .bind(cliente)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn ordenes_de(pool: &PgPool, cliente: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ifpe.spei_outbound WHERE cliente_id = $1")
        .bind(cliente)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn uuid_de(v: &Value) -> Uuid {
    v.as_str().unwrap_or_else(|| panic!("no es un id: {v}")).parse().unwrap()
}

// -----------------------------------------------------------------------------
// Cuentas vinculadas
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn la_cuenta_de_demostracion_esta_a_nombre_del_cliente(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let api = Api::nueva(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = api.token(ana.cliente.id);

    let v = api.cuentas(&token).await;
    let c = &v["accounts"][0];
    assert_eq!(v["accounts"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(
        (&c["bank_code"], &c["bank_name"], &c["clabe_last4"]),
        (&json!("012"), &json!("BBVA México"), &json!("1234")),
        "{c}"
    );
    assert_eq!(c["holder_name"], nombre_kyc(&pool, ana.cliente.id).await);
    assert!(chrono::DateTime::parse_from_rfc3339(c["linked_at"].as_str().unwrap()).is_ok(), "{c}");
    // La CLABE completa no sale de la API.
    let completa = clabe_demo(ana.cliente.id, 0);
    assert!(!v.to_string().contains(completa.as_str()), "{v}");
    // En la base de datos: la CLABE de demostración del cliente, activa.
    let fila: (String, String, String) =
        sqlx::query_as("SELECT clabe::text, origen, estado FROM ifpe.cuentas_vinculadas WHERE id = $1")
            .bind(uuid_de(&c["id"]))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(fila, (completa.as_str().to_owned(), "DEMO".to_owned(), "ACTIVA".to_owned()));

    // Consultar otra vez no vincula otra.
    assert_eq!(api.cuentas(&token).await, v);
    assert_eq!(vinculadas_de(&pool, ana.cliente.id).await, 1);

    // Dos consultas a la vez de un cliente nuevo vinculan una sola.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let token_b = api.token(beto.cliente.id);
    let (a, b) = tokio::join!(api.cuentas(&token_b), api.cuentas(&token_b));
    assert_eq!(a, b);
    assert_eq!(vinculadas_de(&pool, beto.cliente.id).await, 1);
    assert_ne!(a["accounts"][0]["id"], c["id"]);

    // Sin KYC verificado no hay cuenta.
    let carla = cliente_sin_kyc(&pool, &cfg, "Carla").await;
    assert_eq!(api.cuentas(&api.token(carla.cliente.id)).await, json!({ "accounts": [] }));
    assert_eq!(vinculadas_de(&pool, carla.cliente.id).await, 0);

    // Sin MODO_DEMO, nadie recibe una cuenta de demostración.
    let mut real = cfg.clone();
    real.modo_demo = false;
    let api_real = Api::nueva(&pool, real);
    let dani = cliente(&pool, &cfg, "Dani").await;
    assert_eq!(api_real.cuentas(&api_real.token(dani.cliente.id)).await, json!({ "accounts": [] }));
    assert_eq!(vinculadas_de(&pool, dani.cliente.id).await, 0);

    // Solo el cliente.
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&api.estado, oficial_id).await;
    let token_oficial = api.estado.jwt.emitir(oficial_id, RolUsuario::Cumplimiento, true).unwrap().0;
    let (s, _) = llamar_json(&api.app, "GET", "/api/v1/wealth/linked-accounts", Some(&token_oficial), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = api.retirar(&token_oficial, retiro(c, "10.00", &clave_api())).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = llamar_json(&api.app, "GET", "/api/v1/wealth/linked-accounts", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// -----------------------------------------------------------------------------
// Retiros
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn un_retiro_descuenta_el_disponible_y_queda_en_proceso(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let api = Api::nueva(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    let token = api.token(ana.cliente.id);
    let cuenta = api.primera(&token).await;

    let (s, r) = api.retirar(&token, retiro(&cuenta, "1250.50", "retiro-0001")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let w = &r["withdrawal"];
    assert_eq!((&w["status"], &w["amount"]), (&json!("processing"), &json!("1250.50000000")), "{w}");
    assert_eq!(w["linked_account"], cuenta);
    let rastreo = w["tracking_key"].as_str().unwrap();
    assert!(rastreo.len() >= 10 && rastreo.bytes().all(|b| b.is_ascii_alphanumeric()), "{rastreo}");
    assert!(chrono::DateTime::parse_from_rfc3339(w["requested_at"].as_str().unwrap()).is_ok(), "{w}");

    // El disponible bajó al instante; nada quedó comprometido ni retenido.
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await, (d("3749.50"), d("0")));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.1, d("0"));

    // La orden: PENDIENTE (en proceso), ligada a la cuenta vinculada, a la
    // CLABE de demostración y a nombre del propio cliente.
    let (estado, vinculada, destino, beneficiario, ordenante, monto, concepto, clave_rastreo): (
        String,
        Option<Uuid>,
        String,
        String,
        String,
        Decimal,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT estado, cuenta_vinculada_id, beneficiario_clabe::text, beneficiario_nombre, ordenante_nombre, monto, \
                concepto, clave_rastreo FROM ifpe.spei_outbound WHERE id = $1",
    )
    .bind(uuid_de(&w["id"]))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((estado.as_str(), vinculada), ("PENDIENTE", Some(uuid_de(&cuenta["id"]))));
    assert_eq!(destino, clabe_demo(ana.cliente.id, 0).as_str());
    assert_eq!(beneficiario, ordenante);
    assert_eq!((monto, concepto.as_str(), clave_rastreo.as_str()), (d("1250.5"), "RETIRO A CUENTA PROPIA", rastreo));
    // Su cargo en el ledger: un RETIRO a esa CLABE.
    let (tipo, referencia): (String, Option<String>) = sqlx::query_as(
        "SELECT t.tipo, t.referencia_externa FROM ifpe.transacciones t \
           JOIN ifpe.spei_outbound o ON o.tx_retiro_id = t.id WHERE o.id = $1",
    )
    .bind(uuid_de(&w["id"]))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((tipo.as_str(), referencia.as_deref()), ("RETIRO", Some(destino.as_str())));

    // La misma clave: el mismo retiro (200), sin cargar dos veces.
    let (s, otra_vez) = api.retirar(&token, retiro(&cuenta, "1250.50", "retiro-0001")).await;
    assert_eq!((s, &otra_vez), (StatusCode::OK, &r));
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await.0, d("3749.50"));
    // La misma clave para otro monto: conflicto.
    assert_eq!(
        api.rechazo(&token, retiro(&cuenta, "100.00", "retiro-0001")).await,
        (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA".to_owned())
    );

    // Todo el disponible (el botón Max), y después nada más.
    let (s, todo) = api.retirar(&token, retiro(&cuenta, "3749.50", "retiro-0002")).await;
    assert_eq!(s, StatusCode::CREATED, "{todo}");
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await, (d("0"), d("0")));
    assert_eq!(
        api.rechazo(&token, retiro(&cuenta, "0.01", "retiro-0003")).await,
        (StatusCode::UNPROCESSABLE_ENTITY, "SALDO_INSUFICIENTE".to_owned())
    );
    assert_eq!(ordenes_de(&pool, ana.cliente.id).await, 2);
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn solo_se_retira_el_disponible_no_lo_comprometido_en_ordenes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let api = Api::nueva(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = api.token(ana.cliente.id);

    // Una compra límite que espera compromete 600 (0015).
    let orden = json!({ "ticker": "AUG1", "side": "buy", "amount": "600.00", "order_type": "limit",
                        "limit_price": "1500", "idempotency_key": "prueba-limite" });
    let (s, o) = llamar_json(&api.app, "POST", "/api/v1/market/order", Some(&token), Some(orden)).await;
    assert_eq!(s, StatusCode::CREATED, "{o}");
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await, (d("400"), d("600")));

    let cuenta = api.primera(&token).await;
    assert_eq!(
        api.rechazo(&token, retiro(&cuenta, "400.01", "prueba-de-mas")).await,
        (StatusCode::UNPROCESSABLE_ENTITY, "SALDO_INSUFICIENTE".to_owned())
    );
    let (s, r) = api.retirar(&token, retiro(&cuenta, "400.00", "prueba-todo")).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    // Lo comprometido sigue en su orden.
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await, (d("0"), d("600")));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn la_lista_blanca_en_las_dos_rutas_de_retiro(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let api = Api::nueva(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "5000").await;
    let beto = con_efectivo(&pool, &cfg, "Beto", "100").await;
    let (ta, tb) = (api.token(ana.cliente.id), api.token(beto.cliente.id));
    let cuenta_a = api.primera(&ta).await;
    let cuenta_b = api.primera(&tb).await;
    let no_vinculada = (StatusCode::UNPROCESSABLE_ENTITY, Some("CUENTA_NO_VINCULADA"));

    // La cuenta vinculada de otro cliente no existe para Ana; una inventada, tampoco.
    assert_eq!(
        api.rechazo(&ta, retiro(&cuenta_b, "10.00", &clave_api())).await,
        (StatusCode::NOT_FOUND, "NO_ENCONTRADO".to_owned())
    );
    let inventada = json!({ "linked_account_id": Uuid::new_v4(), "amount": "10.00", "idempotency_key": clave_api() });
    assert_eq!(api.rechazo(&ta, inventada).await, (StatusCode::NOT_FOUND, "NO_ENCONTRADO".to_owned()));

    // /ifpe/retiros: ni a una CLABE sin vincular ni a la cuenta vinculada de Beto.
    for destino in [externa(1), clabe_demo(beto.cliente.id, 0).as_str().to_owned()] {
        let (s, e) = api.retiro_por_clabe(&ta, &destino, "10.00").await;
        assert_eq!((s, e["error"]["codigo"].as_str()), no_vinculada, "{destino}: {e}");
    }
    // A su propia cuenta vinculada, sí: la orden queda ligada a ella.
    let (s, r) = api.retiro_por_clabe(&ta, clabe_demo(ana.cliente.id, 0).as_str(), "10.00").await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["orden"]["cuenta_vinculada_id"], cuenta_a["id"]);

    // Un pago a un tercero (/spei/transfer) no es un retiro: no lleva cuenta vinculada.
    let pago = json!({ "clabe_destino": externa(1), "beneficiario_nombre": "Proveedor Externo", "monto": "10.00",
                       "clave_idempotencia": clave_api() });
    let (s, p) = llamar_json(&api.app, "POST", "/api/v1/spei/transfer", Some(&ta), Some(pago)).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(p["orden"]["cuenta_vinculada_id"], Value::Null);
    // Los rechazos no tocaron el saldo: salieron solo el retiro y el pago.
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await.0, d("4980"));
    assert_eq!(ordenes_de(&pool, ana.cliente.id).await, 2);

    // Dada de baja, la cuenta ya no recibe retiros por ninguna de las dos rutas.
    let baja = format!("UPDATE ifpe.cuentas_vinculadas SET estado = 'BAJA' WHERE id = '{}'", uuid_de(&cuenta_a["id"]));
    como(&pool, db::Rol::Ifpe, &baja).await.unwrap();
    let (s, e) = api.retirar(&ta, retiro(&cuenta_a, "10.00", &clave_api())).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), no_vinculada, "{e}");
    let (s, e) = api.retiro_por_clabe(&ta, clabe_demo(ana.cliente.id, 0).as_str(), "10.00").await;
    assert_eq!((s, e["error"]["codigo"].as_str()), no_vinculada, "{e}");
    // La lista ya no la muestra; con MODO_DEMO se vincula otra vez (otra fila, la misma CLABE).
    let nueva = api.primera(&ta).await;
    assert_ne!(nueva["id"], cuenta_a["id"]);
    assert_eq!(nueva["clabe_last4"], "1234");
    let (s, r) = api.retirar(&ta, retiro(&nueva, "10.00", &clave_api())).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await.0, d("4970"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn un_cliente_congelado_recibe_la_respuesta_de_siempre(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let api = Api::nueva(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = api.token(ana.cliente.id);
    let cuenta = api.primera(&token).await;
    // Estructuración: PLD congela al cliente (CONGELADO_PREVENTIVO). Lo que ya
    // tenía sigue disponible: el rechazo no es por saldo.
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    let disponible = saldos(&pool, ana.cuenta_ifpe.id).await.0;
    assert!(disponible >= d("100"), "{disponible}");
    let neutral = (StatusCode::UNPROCESSABLE_ENTITY, Some("OPERACION_NO_DISPONIBLE"));

    let (s, e) = api.retirar(&token, retiro(&cuenta, "100.00", &clave_api())).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), neutral, "{e}");
    // También a una CLABE sin vincular: PLD responde antes que la lista blanca,
    // y otro rechazo delataría el congelamiento.
    let (s, e) = api.retiro_por_clabe(&token, &externa(9), "100.00").await;
    assert_eq!((s, e["error"]["codigo"].as_str()), neutral, "{e}");
    assert_eq!(
        e["error"]["mensaje"],
        "la operación no está disponible en este momento; si necesitas ayuda, contacta a soporte"
    );
    // Ni orden ni cargo.
    assert_eq!(ordenes_de(&pool, ana.cliente.id).await, 0);
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await.0, disponible);
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn sin_kyc_vigente_o_con_entrada_invalida_no_hay_retiro(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let api = Api::nueva(&pool, cfg.clone());
    let ana = con_efectivo(&pool, &cfg, "Ana", "1000").await;
    let token = api.token(ana.cliente.id);
    let cuenta = api.primera(&token).await;

    let id = cuenta["id"].clone();
    for cuerpo in [
        json!({ "linked_account_id": id, "amount": "0", "idempotency_key": clave_api() }),
        json!({ "linked_account_id": id, "amount": "-5.00", "idempotency_key": clave_api() }),
        // SPEI liquida en centavos.
        json!({ "linked_account_id": id, "amount": "10.001", "idempotency_key": clave_api() }),
        // Montos como número JSON: nunca (se redondearían).
        json!({ "linked_account_id": id, "amount": 10, "idempotency_key": clave_api() }),
        json!({ "amount": "10.00", "idempotency_key": clave_api() }),
        json!({ "linked_account_id": "no-es-un-id", "amount": "10.00", "idempotency_key": clave_api() }),
        json!({ "linked_account_id": id, "amount": "10.00", "idempotency_key": "corta" }),
        json!({ "linked_account_id": id, "amount": "10.00", "idempotency_key": clave_api(), "clabe": externa(1) }),
    ] {
        let (s, e) = api.retirar(&token, cuerpo.clone()).await;
        assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{cuerpo}: {e}");
    }
    assert_eq!(saldos(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));

    // Sin KYC vigente, la cuenta sigue vinculada pero no se retira.
    revocar_kyc(&pool, ana.cliente.id).await;
    assert_eq!(
        api.rechazo(&token, retiro(&cuenta, "10.00", &clave_api())).await,
        (StatusCode::UNPROCESSABLE_ENTITY, "KYC_NO_VERIFICADO".to_owned())
    );
    assert_eq!(ordenes_de(&pool, ana.cliente.id).await, 0);
    verificar_invariantes(&pool).await;
}
