//! El rastreador SPEI y las aclaraciones (roadmap 17, migración 0025,
//! `patrimonio::transferencias`, `patrimonio::aclaraciones`) por la API: cada
//! SPEI del cliente con su etapa, su contraparte enmascarada y los datos de su
//! CEP; la búsqueda por un movimiento del ledger; las páginas y el RLS; las
//! aclaraciones con sus reglas (KYC, dueño, una activa por SPEI, máximo, 90
//! días), su idempotencia y su historia en la bitácora; el operador que las
//! revisa y las cierra con una nota neutral; y un congelamiento PLD que no
//! cambia nada ni lo delata. Las reglas de la base de datos están en
//! tests/db/0025_aclaraciones_spei.sql.

mod comun;

use axum::http::StatusCode;
use chrono::Utc;
use comun::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    config::Config,
    kyc::Registro,
    models::kyc::RolUsuario,
    routes,
    spei::{inbound::Origen, outbound},
    AppState,
};
use uuid::Uuid;

struct Banca {
    app: axum::Router,
    estado: AppState,
    cfg: Config,
}

impl Banca {
    fn nueva(pool: &PgPool, cfg: Config) -> Self {
        let estado = AppState::nuevo(pool.clone(), cfg.clone());
        Self { app: routes::router(estado.clone()), estado, cfg }
    }

    async fn token(&self, cliente_id: Uuid) -> String {
        token_de(&self.estado, cliente_id, RolUsuario::Cliente, false).await
    }

    /// El operador SPEI, con su segundo factor.
    async fn token_operador(&self, pool: &PgPool) -> String {
        bootstrap::asegurar_operador(pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
        let id: Uuid = sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = 'operador@synthetic.mx'")
            .fetch_one(pool)
            .await
            .unwrap();
        activar_mfa(&self.estado, id).await;
        token_de(&self.estado, id, RolUsuario::Operador, true).await
    }

    async fn get(&self, token: &str, ruta: &str) -> (StatusCode, Value) {
        llamar_json(&self.app, "GET", ruta, Some(token), None).await
    }

    async fn post(&self, token: &str, ruta: &str, cuerpo: Value) -> (StatusCode, Value) {
        llamar_json(&self.app, "POST", ruta, Some(token), Some(cuerpo)).await
    }

    /// GET que debe responder 200.
    async fn leer(&self, token: &str, ruta: &str) -> Value {
        let (s, v) = self.get(token, ruta).await;
        assert_eq!(s, StatusCode::OK, "{ruta}: {v}");
        v
    }

    async fn transferencias(&self, token: &str) -> Vec<Value> {
        self.leer(token, "/api/v1/wealth/spei/transfers").await["transfers"].as_array().unwrap().clone()
    }

    async fn aclarar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        self.post(token, "/api/v1/wealth/spei/disputes", cuerpo).await
    }

    async fn cambiar(&self, token: &str, id: &Value, cuerpo: Value) -> (StatusCode, Value) {
        let ruta = format!("/api/v1/operaciones/spei/aclaraciones/{}", id.as_str().unwrap());
        self.post(token, &ruta, cuerpo).await
    }

    /// Un retiro a la cuenta vinculada (BBVA, a nombre del cliente).
    async fn retiro(&self, pool: &PgPool, cliente_id: Uuid, monto: &str, clave: &str) -> outbound::ResultadoOrden {
        retirar(pool, &self.cfg, cliente_id, monto, CLABE_BBVA, None, clave).await.unwrap()
    }

    /// Lo que informa el liquidador sobre una orden.
    async fn evento(&self, pool: &PgPool, orden: Uuid, evento: outbound::Evento) {
        outbound::aplicar(pool, &self.cfg.spei, orden, &evento).await.unwrap();
    }
}

const CLABE_BBVA: &str = "012180001234567899";

fn aclaracion(transfer: &Value, direccion: &str, motivo: &str, descripcion: Option<&str>, clave: &str) -> Value {
    json!({
        "transfer_id": transfer["transfer_id"], "direction": direccion, "reason": motivo,
        "description": descripcion, "idempotency_key": format!("prueba-{clave}"),
    })
}

fn por_id<'a>(lista: &'a [Value], id: &Value) -> &'a Value {
    lista.iter().find(|t| t["transfer_id"] == *id).unwrap_or_else(|| panic!("{id} no está en {lista:?}"))
}

async fn version_ordenes(pool: &PgPool, cuenta: Uuid) -> i64 {
    sqlx::query_scalar("SELECT version_ordenes FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Un SPEI que llegó a la cuenta y se devolvió al emisor (como lo registra
/// `spei::inbound` con una cuenta que no puede recibir), hace `dias` días.
async fn devuelto(pool: &PgPool, r: &Registro, rastreo: &str, dias: i32) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, \
                                        monto, estado, causa_devolucion, motivo, origen, raw_payload, recibido_en) \
         VALUES ('40014', $1, (now() - make_interval(days => $3))::date, $2, $4, 250, 'DEVUELTO', 2, \
                 'el cliente está en la Lista de Personas Bloqueadas; operación rechazada', 'WEBHOOK', '{}', \
                 now() - make_interval(days => $3)) RETURNING id",
    )
    .bind(rastreo)
    .bind(&r.cuenta_ifpe.clabe)
    .bind(dias)
    .bind(r.cuenta_ifpe.id)
    .fetch_one(pool)
    .await
    .unwrap()
}

// -----------------------------------------------------------------------------
// El rastreador
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_rastreador_spei(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let b = Banca::nueva(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = b.token(ana.cliente.id).await;
    assert_eq!(b.leer(&token, "/api/v1/wealth/spei/transfers").await, json!({ "transfers": [], "next_cursor": null }));

    // Un depósito de BBVA, liquidado: con su CEP, su contraparte enmascarada y su movimiento del ledger.
    let deposito = depositar(&pool, &cfg, ana.cuenta_ifpe.id, "5000.00").await.transaccion;
    let lista = b.transferencias(&token).await;
    assert_eq!(lista.len(), 1);
    let d = &lista[0];
    let hoy = Utc::now().date_naive().to_string();
    assert_eq!(
        (
            &d["direction"],
            &d["kind"],
            &d["status"],
            &d["stage"],
            &d["amount"],
            &d["operation_date"],
            &d["transaction_id"],
            &d["simulated"],
            &d["clarification"],
            &d["return_reason"],
        ),
        (
            &json!("incoming"),
            &json!("deposit"),
            &json!("settled"),
            &json!("settled"),
            &json!("5000.00000000"),
            &json!(hoy),
            &json!(deposito.id),
            &json!(false),
            &Value::Null,
            &Value::Null,
        ),
        "{d}"
    );
    assert_eq!(
        d["counterparty"],
        json!({ "name": "ORDENANTE DE PRUEBA", "bank": { "code": "012", "name": "BBVA México" }, "account_last4": "7899" })
    );
    assert!(d["tracking_key"].as_str().unwrap().starts_with("PRUEBA"), "{d}");
    assert_eq!(d["settled_at"], d["initiated_at"]);
    assert_eq!(
        d["cep"],
        json!({
            "available": true, "operation_date": hoy, "tracking_key": d["tracking_key"],
            "sender_bank": { "code": "012", "name": "BBVA México" },
            "receiver_bank": { "code": &ana.cuenta_ifpe.clabe[..3], "name": "Synthetic Capital" },
            "beneficiary_account": ana.cuenta_ifpe.clabe, "beneficiary_account_last4": &ana.cuenta_ifpe.clabe[14..],
            "amount": "5000.00000000", "url": "https://www.banxico.org.mx/cep/",
        }),
        "el beneficiario de un depósito es el cliente: su propia CLABE, completa"
    );

    // Un retiro a su BBVA: iniciado, sin CEP; la CLABE completa del destino no sale.
    let r = b.retiro(&pool, ana.cliente.id, "1000.00", "prueba-retiro-1").await;
    let lista = b.transferencias(&token).await;
    let w = &lista[0];
    assert_eq!(lista.len(), 2, "el más reciente primero");
    assert_eq!(
        (&w["transfer_id"], &w["direction"], &w["kind"], &w["status"], &w["stage"], &w["cep"], &w["sent_at"]),
        (
            &json!(r.orden.id),
            &json!("outgoing"),
            &json!("withdrawal"),
            &json!("pending"),
            &json!("initiated"),
            &Value::Null,
            &Value::Null
        ),
        "{w}"
    );
    assert_eq!(
        (
            &w["tracking_key"],
            &w["transaction_id"],
            &w["counterparty"]["account_last4"],
            &w["counterparty"]["bank"]["code"]
        ),
        (&json!(r.orden.clave_rastreo), &json!(r.transaccion.id), &json!("7899"), &json!("012"))
    );
    assert!(!b.leer(&token, "/api/v1/wealth/spei/transfers").await.to_string().contains(CLABE_BBVA));

    // El liquidador la acepta, la liquida y el banco receptor la devuelve: cada etapa, y el reembolso.
    // Cada paso avisa a la bóveda (migración 0026), aunque no mueva el saldo: la pantalla vuelve a leer.
    let cuenta = ana.cuenta_ifpe.id;
    let v0 = version_ordenes(&pool, cuenta).await;
    b.evento(&pool, r.orden.id, outbound::Evento::Aceptada { id_proveedor: Some("987".into()), fecha_operacion: None })
        .await;
    assert_eq!(version_ordenes(&pool, cuenta).await, v0 + 1, "ENVIADA avisa a la bóveda");
    let w = por_id(&b.transferencias(&token).await, &json!(r.orden.id)).clone();
    assert_eq!((&w["status"], &w["stage"]), (&json!("pending"), &json!("sent")));
    assert!(w["sent_at"].is_string() && w["cep"].is_null(), "{w}");
    b.evento(&pool, r.orden.id, outbound::Evento::Liquidada { fecha_operacion: Some(Utc::now().date_naive()) }).await;
    assert_eq!(version_ordenes(&pool, cuenta).await, v0 + 2, "LIQUIDADA (con su fecha de operación) avisa una vez");
    let w = por_id(&b.transferencias(&token).await, &json!(r.orden.id)).clone();
    assert_eq!((&w["status"], &w["stage"], &w["simulated"]), (&json!("settled"), &json!("settled"), &json!(true)));
    assert_eq!(
        (&w["cep"]["available"], &w["cep"]["beneficiary_account"], &w["cep"]["beneficiary_account_last4"]),
        (&json!(false), &Value::Null, &json!("7899")),
        "la pasarela simulada no pasa por Banxico: los datos sí, el CEP no; la CLABE ajena, enmascarada"
    );
    assert_eq!(
        (&w["cep"]["sender_bank"]["name"], &w["cep"]["receiver_bank"]["name"]),
        (&json!("Synthetic Capital"), &json!("BBVA México"))
    );
    b.evento(&pool, r.orden.id, outbound::Evento::Devuelta { causa: 1, motivo: "cuenta inexistente".into() }).await;
    assert_eq!(version_ordenes(&pool, cuenta).await, v0 + 3, "DEVUELTA avisa (y su reembolso, con el saldo)");
    let w = por_id(&b.transferencias(&token).await, &json!(r.orden.id)).clone();
    assert_eq!(
        (&w["status"], &w["stage"], &w["return_reason"]),
        (&json!("returned"), &json!("returned"), &json!("account_not_found"))
    );
    assert!(w["returned_at"].is_string() && w["refund_transaction_id"].is_string() && w["cep"].is_object(), "{w}");

    // Un depósito devuelto al emisor: sin CEP ni movimiento, y sin la causa ni el motivo (aviso indebido).
    let regreso = devuelto(&pool, &ana, "DEVUELTO1", 0).await;
    assert_eq!(version_ordenes(&pool, cuenta).await, v0 + 4, "un depósito devuelto avisa: el saldo no cambió");
    let todo = b.leer(&token, "/api/v1/wealth/spei/transfers").await;
    let v = por_id(todo["transfers"].as_array().unwrap(), &json!(regreso));
    assert_eq!(
        (&v["status"], &v["stage"], &v["return_reason"], &v["cep"], &v["transaction_id"]),
        (&json!("returned"), &json!("returned"), &Value::Null, &Value::Null, &Value::Null),
        "{v}"
    );
    let texto = todo.to_string().to_lowercase();
    for palabra in ["bloquead", "lista de personas", "motivo", "cuenta inexistente", "raw_payload"] {
        assert!(!texto.contains(palabra), "el rastreador dice «{palabra}»: {texto}");
    }

    // Un depósito de demostración: simulado, sin fecha de operación ni ordenante.
    depositar_por(&pool, &cfg, ana.cuenta_ifpe.id, "100.00", Origen::Demo).await;
    let demo = &b.transferencias(&token).await[0];
    assert_eq!(
        (&demo["simulated"], &demo["operation_date"], &demo["counterparty"]["name"], &demo["concept"]),
        (&json!(true), &Value::Null, &Value::Null, &Value::Null)
    );
    assert_eq!(demo["cep"]["available"], json!(false));

    // Uno, por su id o por el de cualquiera de sus movimientos del ledger.
    for id in [r.orden.id, r.transaccion.id, Uuid::parse_str(w["refund_transaction_id"].as_str().unwrap()).unwrap()] {
        let uno = b.leer(&token, &format!("/api/v1/wealth/spei/transfers/{id}")).await;
        assert_eq!(uno["transfer_id"], json!(r.orden.id), "{id}");
    }
    let uno = b.leer(&token, &format!("/api/v1/wealth/spei/transfers/{}", deposito.id)).await;
    assert_eq!(uno["kind"], json!("deposit"));
    for (ruta, status) in [
        (format!("/api/v1/wealth/spei/transfers/{}", Uuid::new_v4()), StatusCode::NOT_FOUND),
        ("/api/v1/wealth/spei/transfers/no-es-un-id".into(), StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(b.get(&token, &ruta).await.0, status, "{ruta}");
    }

    // Por páginas de 2, cada una con el cursor de la anterior.
    let mut vistos = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let ruta = match &cursor {
            Some(c) => format!("/api/v1/wealth/spei/transfers?limit=2&cursor={c}"),
            None => "/api/v1/wealth/spei/transfers?limit=2".into(),
        };
        let pagina = b.leer(&token, &ruta).await;
        vistos.extend(pagina["transfers"].as_array().unwrap().iter().map(|t| t["transfer_id"].clone()));
        match pagina["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    let todos: Vec<Value> = b.transferencias(&token).await.iter().map(|t| t["transfer_id"].clone()).collect();
    assert_eq!((vistos.len(), &vistos), (4, &todos));
    for ruta in ["?limit=0", "?limit=101", "?cursor=no-vale", "?campo=1"] {
        let (s, _) = b.get(&token, &format!("/api/v1/wealth/spei/transfers{ruta}")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{ruta}");
    }

    // Cada quien lo suyo; el personal no tiene rastreador.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_beto = b.token(beto.cliente.id).await;
    assert!(b.transferencias(&tok_beto).await.is_empty());
    let ajeno = format!("/api/v1/wealth/spei/transfers/{}", r.orden.id);
    assert_eq!(b.get(&tok_beto, &ajeno).await.0, StatusCode::NOT_FOUND);
    let operador = b.token_operador(&pool).await;
    assert_eq!(b.get(&operador, "/api/v1/wealth/spei/transfers").await.0, StatusCode::FORBIDDEN);
}

// -----------------------------------------------------------------------------
// Las aclaraciones del cliente
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn las_aclaraciones(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let b = Banca::nueva(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = b.token(ana.cliente.id).await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "5000.00").await;
    let r = b.retiro(&pool, ana.cliente.id, "1000.00", "prueba-retiro-1").await;
    let lista = b.transferencias(&token).await;
    let (retiro, deposito) = (lista[0].clone(), lista[1].clone());
    let version = version_ordenes(&pool, ana.cuenta_ifpe.id).await;

    // Su retiro no llega: la 1, abierta, con la clave de rastreo de la orden.
    let cuerpo = aclaracion(&retiro, "outgoing", "delayed", Some("  Lo mandé ayer y mi banco no lo ve.  "), "a1");
    let (s, a) = b.aclarar(&token, cuerpo.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert_eq!(
        (
            &a["number"],
            &a["transfer_id"],
            &a["direction"],
            &a["tracking_key"],
            &a["reason"],
            &a["description"],
            &a["status"],
            &a["resolution_note"],
            &a["closed_at"]
        ),
        (
            &json!(1),
            &json!(r.orden.id),
            &json!("outgoing"),
            &json!(r.orden.clave_rastreo),
            &json!("delayed"),
            &json!("Lo mandé ayer y mi banco no lo ve."),
            &json!("open"),
            &Value::Null,
            &Value::Null
        ),
        "{a}"
    );
    // La historia viene de la bitácora inmutable (su propio reloj).
    assert_eq!(a["history"].as_array().map(|h| (h.len(), h[0]["status"].clone())), Some((1, json!("open"))));
    assert!(a["history"][0]["at"].as_str().is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok()));
    assert_eq!(version_ordenes(&pool, ana.cuenta_ifpe.id).await, version + 1, "la bóveda avisa");
    // La misma clave: la misma (200); con otro motivo, 409.
    assert_eq!(b.aclarar(&token, cuerpo).await, (StatusCode::OK, a.clone()));
    let (s, e) = b.aclarar(&token, aclaracion(&retiro, "outgoing", "not_received", None, "a1")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("CLAVE_IDEMPOTENCIA_REUTILIZADA")));
    // Otra activa del mismo SPEI, no.
    let (s, e) = b.aclarar(&token, aclaracion(&retiro, "outgoing", "not_received", None, "a2")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("ACLARACION_ACTIVA")));

    // El rastreador la muestra en su SPEI; la bitácora de seguridad, con su folio.
    let w = por_id(&b.transferencias(&token).await, &json!(r.orden.id)).clone();
    assert_eq!(w["clarification"], json!({ "clarification_id": a["clarification_id"], "number": 1, "status": "open" }));
    let eventos = b.leer(&token, "/api/v1/auth/security/events").await;
    let abierta = eventos["eventos"].as_array().unwrap().iter().find(|e| e["evento"] == "ACLARACION_SPEI_ABIERTA");
    assert_eq!(abierta.map(|e| e["detalle"].clone()), Some(json!("1")), "{eventos}");

    // Las reglas: lo que no es suyo no existe; la forma; "other" pide qué pasó.
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "100.00").await;
    let de_beto = b.transferencias(&b.token(beto.cliente.id).await).await[0].clone();
    let largo = "x".repeat(501);
    for (cuerpo, status, codigo) in [
        (aclaracion(&de_beto, "incoming", "not_recognized", None, "x1"), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
        (aclaracion(&deposito, "outgoing", "delayed", None, "x2"), StatusCode::NOT_FOUND, "NO_ENCONTRADO"),
        (
            json!({ "transfer_id": Uuid::new_v4(), "direction": "incoming", "reason": "delayed", "idempotency_key": "prueba-x3" }),
            StatusCode::NOT_FOUND,
            "NO_ENCONTRADO",
        ),
        (aclaracion(&deposito, "incoming", "other", None, "x4"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (aclaracion(&deposito, "incoming", "other", Some("muy corto"), "x5"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (aclaracion(&deposito, "incoming", "wrong_amount", Some(&largo), "x6"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (aclaracion(&deposito, "incoming", "fraud", None, "x7"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (aclaracion(&deposito, "sideways", "delayed", None, "x8"), StatusCode::BAD_REQUEST, "VALIDACION"),
        (
            json!({ "transfer_id": deposito["transfer_id"], "direction": "incoming", "reason": "delayed",
                    "idempotency_key": "prueba-x9", "status": "resolved" }),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
    ] {
        let (s, e) = b.aclarar(&token, cuerpo.clone()).await;
        assert_eq!((s, &e["error"]["codigo"]), (status, &json!(codigo)), "{cuerpo}: {e}");
    }

    // Su depósito, que no reconoce: la 2.
    let (s, a2) = b.aclarar(&token, aclaracion(&deposito, "incoming", "not_recognized", None, "a3")).await;
    assert_eq!(
        (s, &a2["number"], &a2["direction"], &a2["description"]),
        (StatusCode::CREATED, &json!(2), &json!("incoming"), &Value::Null)
    );

    // La lista: la más reciente primero, cada una con su historia; Beto no ve ninguna.
    let todas = b.leer(&token, "/api/v1/wealth/spei/disputes").await;
    let numeros: Vec<Value> = todas["clarifications"].as_array().unwrap().iter().map(|c| c["number"].clone()).collect();
    assert_eq!(numeros, [json!(2), json!(1)]);
    assert_eq!(
        b.leer(&b.token(beto.cliente.id).await, "/api/v1/wealth/spei/disputes").await,
        json!({ "clarifications": [] })
    );

    // Sin KYC no se abre ninguna (antes de buscar el SPEI).
    let carla = cliente_sin_kyc(&pool, &cfg, "Carla").await;
    let (s, e) =
        b.aclarar(&b.token(carla.cliente.id).await, aclaracion(&deposito, "incoming", "delayed", None, "c1")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("KYC_NO_VERIFICADO")));
    // El personal no abre aclaraciones de clientes.
    let operador = b.token_operador(&pool).await;
    assert_eq!(
        b.aclarar(&operador, aclaracion(&deposito, "incoming", "delayed", None, "o1")).await.0,
        StatusCode::FORBIDDEN
    );
}

// -----------------------------------------------------------------------------
// El operador las revisa y las cierra
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_operador_cierra_las_aclaraciones(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let b = Banca::nueva(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = b.token(ana.cliente.id).await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "5000.00").await;
    b.retiro(&pool, ana.cliente.id, "1000.00", "prueba-retiro-1").await;
    let lista = b.transferencias(&token).await;
    let (_, a) = b.aclarar(&token, aclaracion(&lista[0], "outgoing", "delayed", None, "a1")).await;
    let (_, a2) =
        b.aclarar(&token, aclaracion(&lista[1], "incoming", "wrong_amount", Some("Esperaba 5 100."), "a2")).await;
    let operador = b.token_operador(&pool).await;

    // La cola: las activas, la más antigua primero, con lo que escribió el cliente.
    let (s, cola) = b.get(&operador, "/api/v1/operaciones/spei/aclaraciones").await;
    assert_eq!(s, StatusCode::OK, "{cola}");
    let ids: Vec<Value> = cola.as_array().unwrap().iter().map(|c| c["id"].clone()).collect();
    assert_eq!(ids, [a["clarification_id"].clone(), a2["clarification_id"].clone()]);
    assert_eq!(cola[1]["descripcion"], json!("Esperaba 5 100."));
    // El cliente no la ve; el operador no cambia sin estado válido.
    assert_eq!(b.get(&token, "/api/v1/operaciones/spei/aclaraciones").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        b.cambiar(&token, &a["clarification_id"], json!({ "estado": "EN_REVISION" })).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        b.cambiar(&operador, &a["clarification_id"], json!({ "estado": "ABIERTA" })).await.0,
        StatusCode::BAD_REQUEST
    );

    // En revisión, sin nota: el cliente lo ve en la historia.
    let (s, c) = b.cambiar(&operador, &a["clarification_id"], json!({ "estado": "EN_REVISION" })).await;
    assert_eq!((s, &c["estado"]), (StatusCode::OK, &json!("EN_REVISION")), "{c}");
    let (s, e) = b.cambiar(&operador, &a["clarification_id"], json!({ "estado": "EN_REVISION" })).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("ACLARACION_TRANSICION")));
    let vista = &b.leer(&token, "/api/v1/wealth/spei/disputes").await["clarifications"][1];
    assert_eq!(vista["status"], json!("in_review"));
    let pasos: Vec<Value> = vista["history"].as_array().unwrap().iter().map(|p| p["status"].clone()).collect();
    assert_eq!(pasos, [json!("open"), json!("in_review")]);

    // Cerrarla lleva una nota para el cliente, y la nota no nombra la prevención de lavado de dinero.
    for (cuerpo, status, codigo) in [
        (json!({ "estado": "RESUELTA" }), StatusCode::BAD_REQUEST, "VALIDACION"),
        (json!({ "estado": "RESUELTA", "nota": "Listo." }), StatusCode::BAD_REQUEST, "VALIDACION"),
        (
            json!({ "estado": "RECHAZADA", "nota": "Retenida por una alerta de PLD." }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ACLARACION_NOTA",
        ),
        (
            json!({ "estado": "RECHAZADA", "nota": "The account is frozen for review." }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ACLARACION_NOTA",
        ),
        (json!({ "estado": "RESUELTA", "nota": "x", "extra": 1 }), StatusCode::BAD_REQUEST, "VALIDACION"),
    ] {
        let (s, e) = b.cambiar(&operador, &a["clarification_id"], cuerpo.clone()).await;
        assert_eq!((s, &e["error"]["codigo"]), (status, &json!(codigo)), "{cuerpo}: {e}");
    }
    let nota = "El banco receptor confirmó el abono del 24 de noviembre.";
    let (s, c) = b.cambiar(&operador, &a["clarification_id"], json!({ "estado": "RESUELTA", "nota": nota })).await;
    assert_eq!((s, &c["estado"], &c["nota_resolucion"]), (StatusCode::OK, &json!("RESUELTA"), &json!(nota)), "{c}");
    let (s, e) = b.cambiar(&operador, &a["clarification_id"], json!({ "estado": "RECHAZADA", "nota": nota })).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::CONFLICT, &json!("ACLARACION_CERRADA")));
    // Una abierta se rechaza directamente.
    let nota2 = "El depósito fue por 5 000: así lo envió tu banco.";
    let (s, _) = b.cambiar(&operador, &a2["clarification_id"], json!({ "estado": "RECHAZADA", "nota": nota2 })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        b.cambiar(&operador, &json!(Uuid::new_v4()), json!({ "estado": "EN_REVISION" })).await.0,
        StatusCode::NOT_FOUND
    );

    // Lo que ve el cliente: resuelta y rechazada (declined), con su nota, su cierre y su historia.
    let todas = b.leer(&token, "/api/v1/wealth/spei/disputes").await;
    let (rechazada, resuelta) = (&todas["clarifications"][0], &todas["clarifications"][1]);
    assert_eq!(
        (&resuelta["status"], &resuelta["resolution_note"], &rechazada["status"], &rechazada["resolution_note"]),
        (&json!("resolved"), &json!(nota), &json!("declined"), &json!(nota2))
    );
    assert!(resuelta["closed_at"].is_string() && rechazada["closed_at"].is_string());
    let pasos: Vec<Value> = resuelta["history"].as_array().unwrap().iter().map(|p| p["status"].clone()).collect();
    assert_eq!(pasos, [json!("open"), json!("in_review"), json!("resolved")]);
    let cola = b.get(&operador, "/api/v1/operaciones/spei/aclaraciones").await.1;
    assert_eq!(cola, json!([]), "la cola solo tiene activas");

    // Cerrada, el mismo SPEI se puede volver a aclarar: la 3.
    let (s, a3) = b.aclarar(&token, aclaracion(&lista[0], "outgoing", "not_received", None, "a4")).await;
    assert_eq!((s, &a3["number"]), (StatusCode::CREATED, &json!(3)));
}

// -----------------------------------------------------------------------------
// Los límites: 10 activas, 90 días
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn los_limites_de_las_aclaraciones(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let b = Banca::nueva(&pool, cfg.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = b.token(ana.cliente.id).await;
    for _ in 0..11 {
        depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10.00").await;
    }
    let lista = b.transferencias(&token).await;
    for (i, t) in lista.iter().take(10).enumerate() {
        let (s, a) = b.aclarar(&token, aclaracion(t, "incoming", "delayed", None, &format!("t{i}"))).await;
        assert_eq!(s, StatusCode::CREATED, "{i}: {a}");
    }
    let (s, e) = b.aclarar(&token, aclaracion(&lista[10], "incoming", "delayed", None, "t10")).await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("LIMITE_ACLARACIONES")));

    // Un SPEI de hace más de 90 días ya no se aclara (LTOSF, art. 23); el de hace 89, sí.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_beto = b.token(beto.cliente.id).await;
    let viejo = devuelto(&pool, &beto, "VIEJO1", 91).await;
    let reciente = devuelto(&pool, &beto, "RECIENTE1", 89).await;
    let (s, e) = b
        .aclarar(&tok_beto, json!({ "transfer_id": viejo, "direction": "incoming", "reason": "returned", "idempotency_key": "prueba-v1" }))
        .await;
    assert_eq!((s, &e["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("ACLARACION_VENCIDA")));
    let (s, a) = b
        .aclarar(&tok_beto, json!({ "transfer_id": reciente, "direction": "incoming", "reason": "returned", "idempotency_key": "prueba-v2" }))
        .await;
    assert_eq!((s, &a["tracking_key"]), (StatusCode::CREATED, &json!("RECIENTE1")));
}

// -----------------------------------------------------------------------------
// PLD: un cliente congelado aclara como cualquiera, y nada lo delata
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn un_congelamiento_pld_no_cambia_nada(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let b = Banca::nueva(&pool, cfg.clone());
    let fer = cliente(&pool, &cfg, "Fer").await;
    congelar_por_estructuracion(&pool, &cfg, fer.cuenta_ifpe.id).await;
    assert_eq!(estado_cuenta(&pool, fer.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    let token = b.token(fer.cliente.id).await;

    // Sus depósitos (los que lo llevaron a revisión) se listan como cualquiera; aclara uno.
    let lista = b.transferencias(&token).await;
    assert!(!lista.is_empty());
    let (s, a) = b.aclarar(&token, aclaracion(&lista[0], "incoming", "not_recognized", None, "f1")).await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let operador = b.token_operador(&pool).await;
    let (s, _) = b
        .cambiar(
            &operador,
            &a["clarification_id"],
            json!({ "estado": "RESUELTA", "nota": "El depósito vino de tu banco." }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    for ruta in ["/api/v1/wealth/spei/transfers", "/api/v1/wealth/spei/disputes"] {
        let texto = b.leer(&token, ruta).await.to_string().to_lowercase();
        for palabra in
            ["aml", "pld", "alerta", "cumplimiento", "uif", "sospech", "congel", "preventiv", "riesgo", "retenid"]
        {
            assert!(!texto.contains(palabra), "{ruta} contiene «{palabra}»: {texto}");
        }
    }
    verificar_invariantes_aml(&pool).await;
}
