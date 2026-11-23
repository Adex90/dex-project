//! Buzón de documentos (`/api/v1/wealth/documentos`): comprobantes de
//! operación y estados de cuenta derivados de los ledgers, con su sello de
//! reserva. Las cifras se comparan con la base de datos, no con lo que el
//! propio backend calculó.

mod comun;

use axum::http::StatusCode;
use comun::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{models::kyc::RolUsuario, reserva, routes, AppState};
use uuid::Uuid;

struct Api {
    app: axum::Router,
    estado: AppState,
}

impl Api {
    async fn nueva(pool: &PgPool) -> Self {
        let cfg = preparar(pool).await;
        let estado = AppState::nuevo(pool.clone(), cfg);
        Self { app: routes::router(estado.clone()), estado }
    }

    async fn get(&self, token: &str, ruta: &str) -> (StatusCode, Value) {
        llamar_json(&self.app, "GET", ruta, Some(token), None).await
    }

    async fn documentos(&self, token: &str) -> Value {
        let (s, v) = self.get(token, "/api/v1/wealth/documentos").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn documento(&self, token: &str, id: &str) -> Value {
        let (s, v) = self.get(token, &format!("/api/v1/wealth/documentos/{id}")).await;
        assert_eq!(s, StatusCode::OK, "{id}: {v}");
        v
    }

    async fn ordenar(&self, token: &str, cuerpo: Value) -> Value {
        let (s, v) = llamar_json(&self.app, "POST", "/api/v1/market/order", Some(token), Some(cuerpo)).await;
        assert!(s.is_success(), "{v}");
        v
    }
}

/// Una orden ejecutada y su pata IFPE, como están en la base de datos.
#[derive(sqlx::FromRow)]
struct OrdenEnBase {
    id: Uuid,
    secuencia: i64,
    cantidad: Decimal,
    precio: Decimal,
    monto: Decimal,
    comision: Decimal,
    tx_hash: String,
    secuencia_ifpe: i64,
    hash_ifpe: String,
}

fn d(v: &Value) -> Decimal {
    v.as_str().unwrap_or_else(|| panic!("no es un decimal en texto: {v}")).parse().unwrap()
}

/// El mes de la Ciudad de México (UTC−6) de ahora: "2026-11".
fn mes_en_curso() -> String {
    (chrono::Utc::now() - chrono::Duration::hours(6)).format("%Y-%m").to_string()
}

#[sqlx::test(migrations = "../migrations")]
async fn comprobantes_y_estado_de_cuenta_de_operaciones_reales(pool: PgPool) {
    let api = Api::nueva(&pool).await;
    let cfg = api.estado.config.clone();
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_de(&api.estado, ana.cliente.id, RolUsuario::Cliente, false).await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "25000").await;

    // Una compra de mercado, una límite que se ejecuta al colocarse y una venta.
    api.ordenar(
        &token,
        json!({ "ticker": "AUG1", "side": "buy", "amount": "1250.00", "idempotency_key": "doc-compra-1" }),
    )
    .await;
    api.ordenar(
        &token,
        json!({ "ticker": "AUG1", "side": "buy", "amount": "800.00", "idempotency_key": "doc-limite-1",
                "order_type": "limit", "limit_price": "1700" }),
    )
    .await;
    api.ordenar(
        &token,
        json!({ "ticker": "AUG1", "side": "sell", "amount": "500.00", "idempotency_key": "doc-venta-1" }),
    )
    .await;

    // Lo que dice la base de datos.
    let ordenes: Vec<OrdenEnBase> = sqlx::query_as(
        "SELECT o.id, o.secuencia, o.cantidad, o.precio, o.monto, o.comision, o.tx_hash, \
                t.secuencia AS secuencia_ifpe, t.tx_hash AS hash_ifpe \
         FROM mercado.ordenes o JOIN ifpe.transacciones t ON t.id = o.tx_ifpe_id \
         WHERE o.cliente_id = $1 ORDER BY o.secuencia",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ordenes.len(), 3);

    // La lista: tres comprobantes y el estado de cuenta del mes, el más reciente primero.
    let lista = api.documentos(&token).await;
    let docs = lista["documents"].as_array().unwrap();
    assert_eq!(lista["truncated"], false);
    let comprobantes: Vec<&Value> = docs.iter().filter(|x| x["kind"] == "trade_confirmation").collect();
    let estados: Vec<&Value> = docs.iter().filter(|x| x["kind"] == "account_statement").collect();
    assert_eq!((comprobantes.len(), estados.len()), (3, 1), "{lista}");
    assert_eq!(comprobantes[0]["id"], format!("co_{}", ordenes[2].id), "el más reciente primero");
    assert_eq!(comprobantes[0]["folio"], format!("CO-{:08}", ordenes[2].secuencia));
    assert_eq!(comprobantes[0]["reference"], ordenes[2].tx_hash, "la referencia es el hash de la orden en su ledger");
    let periodo = mes_en_curso();
    assert_eq!(
        (&estados[0]["id"], &estados[0]["provisional"]),
        (&json!(format!("ec_{periodo}")), &json!(true)),
        "el mes en curso es provisional"
    );

    // Un comprobante: las cifras exactas de la orden, sin comisión.
    let OrdenEnBase {
        id,
        secuencia,
        cantidad,
        precio,
        monto,
        comision,
        tx_hash: hash,
        secuencia_ifpe: sec_ifpe,
        hash_ifpe,
    } = &ordenes[0];
    let c = api.documento(&token, &format!("co_{id}")).await;
    assert_eq!(
        (&c["kind"], &c["side"], &c["ticker"], &c["order_type"], &c["price_source"]),
        (&json!("trade_confirmation"), &json!("buy"), &json!("AUG1"), &json!("market"), &json!("simulated"))
    );
    assert_eq!((d(&c["quantity"]), d(&c["price"]), d(&c["gross_amount"])), (*cantidad, *precio, *monto));
    assert_eq!((d(&c["commission"]), *comision), (Decimal::ZERO, Decimal::ZERO), "cero comisión");
    assert_eq!(d(&c["net_amount"]), *monto);
    assert_eq!(
        (&c["market_sequence"], &c["reference"], &c["settlement_sequence"], &c["settlement_hash"]),
        (&json!(secuencia), &json!(hash), &json!(sec_ifpe), &json!(hash_ifpe))
    );
    assert_eq!(c["account_clabe_last4"], &ana.cuenta_ifpe.clabe[14..]);
    assert_eq!(c["reserve_seal"]["status"], "pending", "ningún corte lo cubre todavía");
    // La límite y la venta.
    let limite = api.documento(&token, &format!("co_{}", ordenes[1].id)).await;
    assert_eq!((&limite["order_type"], d(&limite["limit_price"])), (&json!("limit"), Decimal::from(1700)));
    let venta = api.documento(&token, &format!("co_{}", ordenes[2].id)).await;
    assert_eq!(venta["side"], "sell");
    assert_eq!(d(&venta["realized_pnl"]), d(&venta["net_amount"]) - d(&venta["cost_basis"]));

    // El estado de cuenta cuadra con la bóveda y con el ledger.
    let e = api.documento(&token, &format!("ec_{periodo}")).await;
    let (disponible, bloqueado, comprometido): (Decimal, Decimal, Decimal) =
        sqlx::query_as("SELECT saldo_disponible, saldo_bloqueado, saldo_comprometido FROM ifpe.cuentas WHERE id = $1")
            .bind(ana.cuenta_ifpe.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let total = disponible + bloqueado + comprometido;
    assert_eq!(d(&e["opening_balance"]), Decimal::ZERO, "primer mes de la cuenta");
    assert_eq!(d(&e["closing_balance"]), total, "el saldo final es el de la bóveda");
    assert_eq!(d(&e["closing_breakdown"]["available"]), disponible);
    assert_eq!(d(&e["credits"]) - d(&e["debits"]), total);
    let asientos = e["entries"].as_array().unwrap();
    let tipos: Vec<&str> = asientos.iter().map(|a| a["kind"].as_str().unwrap()).collect();
    assert_eq!(tipos, ["deposit", "market_buy", "market_buy", "market_sell"], "sin las reclasificaciones internas");
    assert_eq!(d(&asientos[3]["balance"]), total, "el saldo corre hasta el final");
    assert_eq!(asientos[1]["ticker"], "AUG1");
    assert_eq!((&e["trades"]["buys"], &e["trades"]["sells"]), (&json!(2), &json!(1)));
    assert_eq!(d(&e["trades"]["commission"]), Decimal::ZERO);
    let posicion: Decimal =
        sqlx::query_scalar("SELECT cantidad FROM mercado.posiciones WHERE cliente_id = $1 AND ticker = 'AUG1'")
            .bind(ana.cliente.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((&e["positions"][0]["ticker"], d(&e["positions"][0]["quantity"])), (&json!("AUG1"), posicion));
    assert_eq!(e["account_clabe"], ana.cuenta_ifpe.clabe);
    // Determinista: la misma referencia en cada lectura.
    let otra_vez = api.documento(&token, &format!("ec_{periodo}")).await;
    assert_eq!(otra_vez["reference"], e["reference"]);
    assert_eq!(e["reserve_seal"]["status"], "pending");

    // Un corte de Proof of Reserve los sella: su raíz ya incluye todo.
    let corte = reserva::generar_corte(&pool).await.unwrap();
    let c = api.documento(&token, &format!("co_{id}")).await;
    let sello = &c["reserve_seal"];
    assert_eq!(
        (&sello["status"], &sello["snapshot_id"], &sello["root"], &sello["reserve_ratio"]),
        (&json!("sealed"), &json!(corte.id), &json!(corte.merkle_root), &json!("100.00"))
    );
    assert!(
        sello["ledger_anchor"].as_i64().unwrap() >= *sec_ifpe && sello["market_anchor"].as_i64().unwrap() >= *secuencia
    );
    let e = api.documento(&token, &format!("ec_{periodo}")).await;
    assert_eq!(
        (&e["reserve_seal"]["status"], &e["reserve_seal"]["root"]),
        (&json!("sealed"), &json!(corte.merkle_root))
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn cada_quien_ve_sus_documentos(pool: PgPool) {
    let api = Api::nueva(&pool).await;
    let cfg = api.estado.config.clone();
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let t_ana = token_de(&api.estado, ana.cliente.id, RolUsuario::Cliente, false).await;
    let t_beto = token_de(&api.estado, beto.cliente.id, RolUsuario::Cliente, false).await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "5000").await;
    api.ordenar(
        &t_ana,
        json!({ "ticker": "AUG1", "side": "buy", "amount": "1000.00", "idempotency_key": "doc-ana-1" }),
    )
    .await;
    let de_ana = api.documentos(&t_ana).await;
    let comprobante =
        de_ana["documents"].as_array().unwrap().iter().find(|x| x["kind"] == "trade_confirmation").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();

    assert_eq!(api.documentos(&t_beto).await["documents"], json!([]), "Beto aún no tiene actividad");
    for ruta in [
        format!("/api/v1/wealth/documentos/{comprobante}"),
        "/api/v1/wealth/documentos/co_no-es-uuid".into(),
        "/api/v1/wealth/documentos/ec_2026-13".into(),
        format!("/api/v1/wealth/documentos/ec_{}", mes_en_curso()),
        "/api/v1/wealth/documentos/xx_1".into(),
    ] {
        let (s, v) = api.get(&t_beto, &ruta).await;
        assert_eq!((s, v["error"]["codigo"].as_str()), (StatusCode::NOT_FOUND, Some("NO_ENCONTRADO")), "{ruta}: {v}");
    }
    let (s, _) = llamar_json(&api.app, "GET", "/api/v1/wealth/documentos", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&api.estado, oficial_id).await;
    let t_oficial = token_de(&api.estado, oficial_id, RolUsuario::Cumplimiento, true).await;
    let (s, _) = api.get(&t_oficial, "/api/v1/wealth/documentos").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
