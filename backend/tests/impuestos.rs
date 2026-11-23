//! Centro fiscal (`/api/v1/wealth/impuestos`): el resumen del ejercicio
//! derivado de los ledgers, el W-8BEN (certificar, renovar, bitácora,
//! elegibilidad y cadena) y la exportación contable en CSV (RFC 4180). Las
//! cifras se comparan con la base de datos, no con lo que el backend calculó.

mod comun;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use chrono::{Datelike, Days, Duration, NaiveDate, Utc};
use comun::*;
use http_body_util::BodyExt;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{config::Config, kyc::Registro, models::kyc::RolUsuario, routes, sofom::credito, AppState};
use tower::ServiceExt;
use uuid::Uuid;

const MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
                   Chrome/126.0.0.0 Safari/537.36";

struct Api {
    app: Router,
    estado: AppState,
}

struct Respuesta {
    status: StatusCode,
    tipo: String,
    disposicion: String,
    cuerpo: Vec<u8>,
}

impl Api {
    async fn nueva(pool: &PgPool) -> Self {
        let mut cfg = preparar(pool).await;
        cfg.http.proxy_confiable = true;
        let estado = AppState::nuevo(pool.clone(), cfg);
        Self { app: routes::router(estado.clone()), estado }
    }

    async fn pedir(&self, metodo: &str, ruta: &str, token: Option<&str>, cuerpo: Option<Value>) -> Respuesta {
        let mut req = Request::builder()
            .method(metodo)
            .uri(ruta)
            .header("user-agent", MAC)
            .header("x-forwarded-for", "203.0.113.7");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let body = match cuerpo {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let cabecera =
            |h: header::HeaderName| resp.headers().get(h).map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
        let (tipo, disposicion) = (cabecera(header::CONTENT_TYPE), cabecera(header::CONTENT_DISPOSITION));
        let status = resp.status();
        let cuerpo = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        Respuesta { status, tipo, disposicion, cuerpo }
    }

    async fn json(&self, metodo: &str, ruta: &str, token: &str, cuerpo: Option<Value>) -> (StatusCode, Value) {
        let r = self.pedir(metodo, ruta, Some(token), cuerpo).await;
        let v = if r.cuerpo.is_empty() { Value::Null } else { serde_json::from_slice(&r.cuerpo).unwrap() };
        (r.status, v)
    }

    async fn centro(&self, token: &str) -> Value {
        let (s, v) = self.json("GET", "/api/v1/wealth/impuestos", token, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn ordenar(&self, token: &str, cuerpo: Value) {
        let (s, v) = self.json("POST", "/api/v1/market/order", token, Some(cuerpo)).await;
        assert!(s.is_success(), "{v}");
    }
}

fn dec(v: &Value) -> Decimal {
    v.as_str().unwrap_or_else(|| panic!("no es un decimal en texto: {v}")).parse().unwrap()
}

fn centavos(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

/// El ejercicio en curso (año de la Ciudad de México).
fn ejercicio() -> i32 {
    (Utc::now() - Duration::hours(6)).year()
}

/// La retención estimada de un mutuo, calculada aquí por separado: la tasa
/// anual de cada año del plazo (0.50% en 2025, 0.90% en 2026) ÷ 365 por día.
fn retencion_esperada(principal: Decimal, inicio: NaiveDate, plazo: i32) -> Option<Decimal> {
    let mut total = Decimal::ZERO;
    for i in 0..u64::try_from(plazo).unwrap() {
        let tasa = match (inicio + Days::new(i)).year() {
            2025 => Decimal::new(50, 2),
            2026 => Decimal::new(90, 2),
            _ => return None,
        };
        total += principal * tasa / Decimal::ONE_HUNDRED / Decimal::from(365);
    }
    Some(centavos(total))
}

/// Un CSV RFC 4180 (comillas dobles, CRLF) en filas de campos.
fn leer_csv(texto: &str) -> Vec<Vec<String>> {
    assert!(texto.ends_with("\r\n"), "cada línea termina en CRLF");
    let mut filas = Vec::new();
    let (mut fila, mut campo, mut comillas) = (Vec::new(), String::new(), false);
    let mut chars = texto.chars().peekable();
    while let Some(c) = chars.next() {
        match (comillas, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                campo.push('"');
            }
            (true, '"') => comillas = false,
            (true, _) => campo.push(c),
            (false, '"') => comillas = true,
            (false, ',') => fila.push(std::mem::take(&mut campo)),
            (false, '\r') => {
                assert_eq!(chars.next(), Some('\n'), "CR sin LF");
                fila.push(std::mem::take(&mut campo));
                filas.push(std::mem::take(&mut fila));
            }
            (false, _) => campo.push(c),
        }
    }
    filas
}

#[derive(sqlx::FromRow)]
struct Venta {
    ticker: String,
    monto: Decimal,
    costo_liberado: Decimal,
}

#[sqlx::test(migrations = "../migrations")]
async fn el_resumen_del_ejercicio_sale_de_los_ledgers(pool: PgPool) {
    let api = Api::nueva(&pool).await;
    let cfg = api.estado.config.clone();
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_de(&api.estado, ana.cliente.id, RolUsuario::Cliente, false).await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "30000").await;

    // Acciones (STSLA) con ganancia, oro (AUG1) con pérdida.
    api.ordenar(
        &token,
        json!({ "ticker": "STSLA", "side": "buy", "amount": "2000.00", "idempotency_key": "isr-compra-acciones" }),
    )
    .await;
    api.ordenar(
        &token,
        json!({ "ticker": "AUG1", "side": "buy", "amount": "1500.00", "idempotency_key": "isr-compra-oro" }),
    )
    .await;
    sqlx::query("UPDATE mercado.activos SET precio = precio * 1.25 WHERE ticker = 'STSLA'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mercado.activos SET precio = precio * 0.80 WHERE ticker = 'AUG1'")
        .execute(&pool)
        .await
        .unwrap();
    api.ordenar(
        &token,
        json!({ "ticker": "STSLA", "side": "sell", "amount": "1000.00", "idempotency_key": "isr-venta-acciones" }),
    )
    .await;
    api.ordenar(
        &token,
        json!({ "ticker": "AUG1", "side": "sell", "amount": "600.00", "idempotency_key": "isr-venta-oro" }),
    )
    .await;

    // Un mutuo a 28 días, vencido y pagado hoy.
    let mutuo = puente_invertir(&pool, &cfg, &ana, "5000", 28).await;
    envejecer_mutuo(&pool, mutuo, 30).await;
    credito::aportar_capital(&pool, d("1000"), Some("Capital inicial".into()), clave()).await.unwrap();
    synthetic_capital::sofom::puente::liquidar(&pool, mutuo, None).await.unwrap();
    // Y otro abierto.
    puente_invertir(&pool, &cfg, &ana, "2000", 28).await;

    let ventas: Vec<Venta> = sqlx::query_as(
        "SELECT ticker, monto, costo_liberado FROM mercado.ordenes \
         WHERE cliente_id = $1 AND lado = 'VENTA' ORDER BY secuencia",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let (interes, principal, inicio, plazo): (Decimal, Decimal, NaiveDate, i32) = sqlx::query_as(
        "SELECT interes_pagado, principal, fecha_inicio, plazo_dias FROM sofom.contratos_mutuo WHERE id = $1",
    )
    .bind(mutuo)
    .fetch_one(&pool)
    .await
    .unwrap();
    let resultado =
        |t: &str| ventas.iter().filter(|v| v.ticker == t).map(|v| v.monto - v.costo_liberado).sum::<Decimal>();
    let (neto_acciones, neto_oro) = (resultado("STSLA"), resultado("AUG1"));
    assert!(neto_acciones > Decimal::ZERO && neto_oro < Decimal::ZERO, "{neto_acciones} {neto_oro}");

    let v = api.centro(&token).await;
    let s = &v["summary"];
    assert_eq!((&s["year"], &s["in_progress"]), (&json!(ejercicio()), &json!(true)));
    assert_eq!(v["years"], json!([ejercicio()]));

    // Mercado: las ventas, su costo promedio y su resultado, como en la base de datos.
    let m = &s["market"];
    assert_eq!((&m["buys"], &m["sells"]), (&json!(2), &json!(2)));
    let ingresos: Decimal = ventas.iter().map(|v| v.monto).sum();
    let costo: Decimal = ventas.iter().map(|v| v.costo_liberado).sum();
    assert_eq!((dec(&m["proceeds"]), dec(&m["cost_basis"])), (ingresos, costo));
    assert_eq!(dec(&m["net_result"]), ingresos - costo);
    assert_eq!((dec(&m["gains"]), dec(&m["losses"])), (neto_acciones, -neto_oro));
    assert_eq!(dec(&m["commissions"]), Decimal::ZERO);
    let activos: Vec<(&str, &str, &str)> = m["by_asset"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["ticker"].as_str().unwrap(), a["asset_class"].as_str().unwrap(), a["treatment"].as_str().unwrap()))
        .collect();
    assert_eq!(activos, [("AUG1", "commodity", "annual_return"), ("STSLA", "equity", "listed_shares")]);

    // Plazo fijo: el interés pagado en el ejercicio y el mutuo abierto.
    let f = &s["fixed_term"];
    assert_eq!((&f["contracts_paid"], &f["contracts_open"]), (&json!(1), &json!(1)));
    assert_eq!((dec(&f["interest_paid"]), dec(&f["principal_returned"])), (interes, principal));
    assert_eq!(dec(&f["principal_open"]), d("2000"));

    // ISR: nada retenido; la estimación con las tasas del ejercicio.
    let isr = &s["isr"];
    assert_eq!(dec(&isr["withheld"]), Decimal::ZERO);
    assert_eq!(dec(&isr["annual_return_result"]), neto_oro, "el oro se acumula en la declaración anual");
    match retencion_esperada(principal, inicio, plazo) {
        Some(retencion) => {
            let e = &isr["estimated"];
            let impuesto = centavos(neto_acciones * Decimal::TEN / Decimal::ONE_HUNDRED);
            assert_eq!(dec(&e["interest_withholding"]), retencion);
            assert_eq!((dec(&e["listed_shares_net"]), dec(&e["listed_shares_tax"])), (neto_acciones, impuesto));
            assert_eq!(dec(&e["total"]), retencion + impuesto);
            assert_eq!(isr["rates"]["listed_shares"], "10.0000");
        }
        None => assert_eq!(isr["not_estimated_reason"], "no_rates", "{isr}"),
    }
    let referencia = s["reference"].as_str().unwrap();
    assert!(referencia.len() == 64 && referencia.chars().all(|c| c.is_ascii_hexdigit()));

    // El perfil fiscal: lo del expediente, normalizado por la base de datos.
    let (nombre, domicilio, rfc, curp): (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT kyc.nombre_legal(nombre, apellido_paterno, apellido_materno), kyc.domicilio_w8ben(domicilio), \
                rfc::text, curp::text FROM kyc.clientes WHERE id = $1",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let p = &v["profile"];
    assert_eq!((p["name"].as_str(), p["address"].as_str()), (Some(nombre.as_str()), Some(domicilio.as_str())));
    assert_eq!((p["rfc"].as_str(), p["curp"].as_str()), (rfc.as_deref(), curp.as_deref()));
    assert_eq!(
        (&p["person_type"], &p["nationality"], &p["tax_residence"], &p["identity_verified"]),
        (&json!("individual"), &json!("MX"), &json!("MX"), &json!(true))
    );

    // Un ejercicio anterior, sin actividad: ceros; uno futuro o anterior a 2000: 400.
    let (st, previo) =
        api.json("GET", &format!("/api/v1/wealth/impuestos?year={}", ejercicio() - 1), &token, None).await;
    assert_eq!(st, StatusCode::OK, "{previo}");
    assert_eq!((&previo["summary"]["in_progress"], &previo["summary"]["market"]["sells"]), (&json!(false), &json!(0)));
    for ruta in [
        format!("/api/v1/wealth/impuestos?year={}", ejercicio() + 1),
        "/api/v1/wealth/impuestos?year=1999".to_owned(),
        "/api/v1/wealth/impuestos?anio=2026".to_owned(),
    ] {
        let (st, e) = api.json("GET", &ruta, &token, None).await;
        assert_eq!((st, e["error"]["codigo"].as_str()), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{ruta}: {e}");
    }

    // La exportación contable del ejercicio.
    let r = api.pedir("GET", "/api/v1/wealth/impuestos/export.csv", Some(&token), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.tipo, "text/csv; charset=utf-8; header=present");
    assert_eq!(r.disposicion, format!("attachment; filename=\"synthetic-capital-taxes-{}.csv\"", ejercicio()));
    let texto = String::from_utf8(r.cuerpo).unwrap();
    assert!(texto.contains("\"Synthetic Tesla, 1 share\""), "un campo con coma va entre comillas: {texto}");
    let filas = leer_csv(&texto);
    assert_eq!(
        filas[0].join(","),
        "date,type,reference,ticker,asset_name,asset_class,quantity,price,amount,commission,cost_basis,\
         realized_result,interest,balance,ledger_sequence,ledger_hash"
    );
    assert!(filas.iter().all(|f| f.len() == 16), "todas las filas tienen 16 campos");
    let columna = |nombre: &str| filas[0].iter().position(|c| c == nombre).unwrap();
    let (tipo, resultado_col, interes_col, saldo_col) =
        (columna("type"), columna("realized_result"), columna("interest"), columna("balance"));
    let suma = |col: usize, t: &str| -> Decimal {
        filas[1..].iter().filter(|f| f[tipo] == t).map(|f| f[col].parse::<Decimal>().unwrap()).sum()
    };
    assert_eq!(suma(resultado_col, "market_sell"), ingresos - costo, "el resultado de las ventas cuadra");
    assert_eq!(suma(interes_col, "from_fixed_term"), interes, "el interés pagado cuadra");
    let tipos: Vec<&str> = filas[1..].iter().map(|f| f[tipo].as_str()).collect();
    assert_eq!(
        tipos,
        [
            "deposit",
            "market_buy",
            "market_buy",
            "market_sell",
            "market_sell",
            "to_fixed_term",
            "from_fixed_term",
            "to_fixed_term"
        ],
        "solo lo que cambia el saldo, en orden del ledger"
    );
    let (disponible, bloqueado) = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await;
    assert_eq!(
        filas.last().unwrap()[saldo_col].parse::<Decimal>().unwrap(),
        disponible + bloqueado,
        "el saldo de la última fila es el de la cuenta"
    );
}

/// `puente::invertir` (un mutuo a `plazo` días): devuelve su id.
async fn puente_invertir(pool: &PgPool, cfg: &Config, r: &Registro, monto: &str, plazo: i32) -> Uuid {
    synthetic_capital::sofom::puente::invertir(
        pool,
        &cfg.pld,
        &synthetic_capital::sofom::puente::SolicitudMutuo {
            cliente_id: r.cliente.id,
            monto: d(monto),
            plazo_dias: plazo,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap()
    .contrato
    .id
}

fn certificacion(firma: &str) -> Value {
    json!({ "beneficial_owner": true, "not_us_person": true, "treaty_resident": true, "signature": firma })
}

#[sqlx::test(migrations = "../migrations")]
async fn el_w8ben_se_certifica_se_renueva_y_queda_en_la_bitacora(pool: PgPool) {
    let api = Api::nueva(&pool).await;
    let cfg = api.estado.config.clone();
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_de(&api.estado, ana.cliente.id, RolUsuario::Cliente, false).await;

    let v = api.centro(&token).await;
    let w = &v["w8ben"];
    assert_eq!(
        (&w["status"], &w["can_certify"], &w["chain_verified"], &w["current"]),
        (&json!("pending"), &json!(true), &json!(true), &Value::Null),
        "{w}"
    );
    assert_eq!((&w["us_dividend_rate"], &w["us_dividend_rate_treaty"]), (&json!("30.00"), &json!("10.00")));
    let nombre = v["profile"]["name"].as_str().unwrap().to_owned();

    // Lo que no se acepta: otra firma, una certificación sin confirmar, un campo de más.
    let ruta = "/api/v1/wealth/impuestos/w8ben";
    let (s, e) = api.json("POST", ruta, &token, Some(certificacion("Otra Persona"))).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("W8BEN_FIRMA")));
    let mut sin_confirmar = certificacion(&nombre);
    sin_confirmar["not_us_person"] = json!(false);
    let (s, e) = api.json("POST", ruta, &token, Some(sin_confirmar)).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("W8BEN_CERTIFICACIONES")));
    let mut de_mas = certificacion(&nombre);
    de_mas["valid_until"] = json!("2099-12-31");
    let (s, _) = api.json("POST", ruta, &token, Some(de_mas)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "la vigencia la pone la base de datos");

    // Certificar: la firma en minúsculas y con espacios de más vale.
    let firma = format!("  {}  ", nombre.to_lowercase().replace(' ', "  "));
    let (s, w) = api.json("POST", ruta, &token, Some(certificacion(&firma))).await;
    assert_eq!(s, StatusCode::CREATED, "{w}");
    let hasta = NaiveDate::from_ymd_opt(ejercicio() + 3, 12, 31).unwrap().to_string();
    assert_eq!(
        (&w["status"], &w["current"]["version"], &w["chain_verified"]),
        (&json!("active"), &json!(1), &json!(true))
    );
    assert_eq!(w["current"]["valid_until"], hasta, "hasta el 31 de diciembre del tercer año siguiente");
    assert_eq!(
        (w["current"]["name"].as_str(), w["current"]["signature"].as_str()),
        (Some(nombre.as_str()), Some(nombre.as_str()))
    );
    assert_eq!(w["current"]["foreign_tin"], json!(ana.cliente.rfc), "el RFC es el TIN extranjero");
    assert_eq!(
        (&w["current"]["device"], &w["current"]["ip"], &w["us_dividend_rate"]),
        (&json!("Chrome on macOS"), &json!("203.0.113.7"), &json!("10.00"))
    );

    // Renovar: una versión nueva, encadenada.
    let (s, w) = api.json("POST", ruta, &token, Some(certificacion(&nombre))).await;
    assert_eq!(s, StatusCode::CREATED, "{w}");
    let versiones: Vec<i64> = w["history"].as_array().unwrap().iter().map(|h| h["version"].as_i64().unwrap()).collect();
    assert_eq!(versiones, [2, 1]);
    assert_eq!(w["current"]["previous_hash"], w["history"][1]["hash"]);

    // Cada versión, en la bitácora de seguridad del cliente, con su sesión.
    let (s, eventos) = api.json("GET", "/api/v1/auth/security/events", &token, None).await;
    assert_eq!(s, StatusCode::OK);
    let w8: Vec<(&str, bool)> = eventos["eventos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["evento"] == "W8BEN_CERTIFICADO")
        .map(|e| (e["detalle"].as_str().unwrap(), e["sesion_actual"].as_bool().unwrap()))
        .collect();
    assert_eq!(w8, [("2", true), ("1", true)], "{eventos}");

    // Un cambio de circunstancias (otro domicilio) deja el formulario desactualizado.
    sqlx::query("UPDATE kyc.clientes SET domicilio = jsonb_set(domicilio, '{calle}', '\"Calle Nueva\"') WHERE id = $1")
        .bind(ana.cliente.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(api.centro(&token).await["w8ben"]["status"], "outdated");

    // Una alteración por fuera de la aplicación rompe la cadena.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE kyc.formularios_w8ben DISABLE TRIGGER trg_w8ben_inmutable")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE kyc.formularios_w8ben SET vigente_hasta = '2099-12-31' WHERE cliente_id = $1 AND version = 1")
        .bind(ana.cliente.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE kyc.formularios_w8ben ENABLE TRIGGER trg_w8ben_inmutable")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(api.centro(&token).await["w8ben"]["chain_verified"], false);
}

#[sqlx::test(migrations = "../migrations")]
async fn solo_certifica_quien_puede_y_cada_quien_ve_lo_suyo(pool: PgPool) {
    let api = Api::nueva(&pool).await;
    let cfg = api.estado.config.clone();
    let ruta = "/api/v1/wealth/impuestos/w8ben";

    // Un ciudadano de EUA: el W-9, no el W-8BEN.
    let beto = cliente(&pool, &cfg, "Beto").await;
    sqlx::query("UPDATE kyc.clientes SET nacionalidad = 'US' WHERE id = $1")
        .bind(beto.cliente.id)
        .execute(&pool)
        .await
        .unwrap();
    let tok_beto = token_de(&api.estado, beto.cliente.id, RolUsuario::Cliente, false).await;
    let w = &api.centro(&tok_beto).await["w8ben"];
    assert_eq!((&w["can_certify"], &w["blocked_reason"]), (&json!(false), &json!("us_person")));
    let nombre = api.centro(&tok_beto).await["profile"]["name"].as_str().unwrap().to_owned();
    let (s, e) = api.json("POST", ruta, &tok_beto, Some(certificacion(&nombre))).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("W8BEN_PERSONA_EUA")));

    // Sin identidad verificada.
    let carla = cliente_sin_kyc(&pool, &cfg, "Carla").await;
    let tok_carla = token_de(&api.estado, carla.cliente.id, RolUsuario::Cliente, false).await;
    let v = api.centro(&tok_carla).await;
    assert_eq!(
        (&v["w8ben"]["blocked_reason"], &v["profile"]["identity_verified"]),
        (&json!("identity_unverified"), &json!(false))
    );
    let (s, e) = api.json("POST", ruta, &tok_carla, Some(certificacion("Carla"))).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("KYC_NO_VERIFICADO")));

    // La base de datos rechaza lo mismo aunque el backend lo dejara pasar.
    let e = sqlx::query(
        "INSERT INTO kyc.formularios_w8ben (id, cliente_id, beneficiario_efectivo, no_persona_eua, residente_tratado, firma) \
         VALUES ($1, $2, true, true, true, $3)",
    )
    .bind(Uuid::new_v4())
    .bind(beto.cliente.id)
    .bind(&nombre)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(e.to_string().contains("SC_W8BEN_PERSONA_EUA"), "{e}");

    // Sin sesión: 401; el personal: 403; la exportación también.
    for r in ["/api/v1/wealth/impuestos", "/api/v1/wealth/impuestos/export.csv"] {
        assert_eq!(api.pedir("GET", r, None, None).await.status, StatusCode::UNAUTHORIZED, "{r}");
    }
    let (operador, _) = oficial(&pool, "Ofelia").await;
    let tok_staff = token_de(&api.estado, operador, RolUsuario::Cumplimiento, true).await;
    for r in ["/api/v1/wealth/impuestos", "/api/v1/wealth/impuestos/export.csv"] {
        assert_eq!(api.pedir("GET", r, Some(&tok_staff), None).await.status, StatusCode::FORBIDDEN, "{r}");
    }
    let (s, _) = api.json("POST", ruta, &tok_staff, Some(certificacion("Ofelia"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Un cliente sin operaciones: la exportación es solo el encabezado.
    let r = api.pedir("GET", "/api/v1/wealth/impuestos/export.csv", Some(&tok_carla), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(leer_csv(&String::from_utf8(r.cuerpo).unwrap()).len(), 1);
}
