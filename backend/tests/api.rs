//! Flujo HTTP completo contra el router de Axum.

mod comun;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use comun::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{bootstrap, routes, AppState};
use tower::ServiceExt;

struct Api {
    app: Router,
}

impl Api {
    async fn llamar(
        &self,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(metodo).uri(ruta);
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
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let valor = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
        (status, valor)
    }
}

async fn api(pool: &PgPool) -> Api {
    let cfg = preparar(pool).await;
    bootstrap::asegurar_operador(pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    Api { app: routes::router(AppState::nuevo(pool.clone(), cfg)) }
}

async fn registrar(api: &Api, nombre: &str) -> (String, Value) {
    let (s, r) = api
        .llamar(
            "POST",
            "/api/v1/auth/registro",
            None,
            Some(json!({
                "tipo_persona": "FISICA",
                "nombre": nombre,
                "apellido_paterno": "Prueba",
                "email": format!("{}@example.mx", nombre.to_lowercase()),
                "password": "contraseña-segura-123"
            })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    (r["sesion"]["token"].as_str().unwrap().to_string(), r)
}

#[sqlx::test(migrations = "../migrations")]
async fn flujo_completo_cliente(pool: PgPool) {
    let api = api(&pool).await;
    let (s, _) = api.llamar("GET", "/salud", None, None).await;
    assert_eq!(s, StatusCode::OK);

    let (token, registro) = registrar(&api, "Ana").await;
    assert_eq!(registro["cliente"]["estatus_kyc"], "VERIFICADO");

    // Depósito simulado (modo demo) e idempotencia HTTP: 201 y luego 200.
    let deposito = json!({ "monto": "10000.00", "clave_idempotencia": "deposito-0001" });
    let (s, r) = api.llamar("POST", "/api/v1/ifpe/depositos/simulados", Some(&token), Some(deposito.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let (s, r2) = api.llamar("POST", "/api/v1/ifpe/depositos/simulados", Some(&token), Some(deposito)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["transaccion"]["id"], r2["transaccion"]["id"]);

    // Puente IFPE -> SOFOM.
    let (s, mutuo) = api
        .llamar(
            "POST",
            "/api/v1/sofom/mutuos",
            Some(&token),
            Some(json!({ "monto": "2500", "plazo_dias": 91, "clave_idempotencia": "mutuo-0001" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{mutuo}");
    assert_eq!(mutuo["contrato"]["estado"], "VIGENTE");
    assert_eq!(mutuo["contrato"]["hash_terminos"], mutuo["terminos"]["hash"]);
    assert_eq!(mutuo["terminos"]["retiro_anticipado"], false);

    // Tablero: bóveda 7,500 líquidos, inversión 2,500, reserva al 100%.
    let (s, tablero) = api.llamar("GET", "/api/v1/tablero", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tablero["boveda"]["saldo_disponible"], "7500.00000000");
    assert_eq!(tablero["inversion"]["total_invertido"], "2500.00000000");
    assert_eq!(tablero["reserva"]["reserva_100"], true);

    // Proof of Reserve: la prueba del cliente verifica en el servicio público.
    let (s, prueba) = api.llamar("GET", "/api/v1/reserva/prueba", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{prueba}");
    assert_eq!(prueba["valida"], true);
    let verificacion = json!({ "corte_id": prueba["corte"]["id"], "hoja": prueba["hoja"], "ruta": prueba["ruta"] });
    let (s, v) = api.llamar("POST", "/api/v1/reserva/verificar", None, Some(verificacion)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["valida"], true);
    assert_eq!(v["raiz_calculada"], prueba["corte"]["merkle_root"]);

    // Liquidación anticipada rechazada.
    let ruta = format!("/api/v1/sofom/mutuos/{}/liquidar", mutuo["contrato"]["id"].as_str().unwrap());
    let (s, e) = api.llamar("POST", &ruta, Some(&token), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(e["error"]["codigo"], "MUTUO_NO_VENCIDO");

    // Saldo insuficiente con código estable.
    let (s, e) = api
        .llamar(
            "POST",
            "/api/v1/ifpe/retiros",
            Some(&token),
            Some(json!({ "monto": "7500.01", "clabe_destino": "646180157000000017", "clave_idempotencia": "retiro-0001" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(e["error"]["codigo"], "SALDO_INSUFICIENTE");

    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn autorizacion_por_rol(pool: PgPool) {
    let api = api(&pool).await;
    let (cliente, _) = registrar(&api, "Ana").await;
    let (s, _) = api.llamar("GET", "/api/v1/tablero", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = api.llamar("GET", "/api/v1/tablero", Some("token-falso"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    for ruta in ["/api/v1/auditoria/ledger/ifpe", "/api/v1/operaciones/alertas", "/api/v1/operaciones/creditos"] {
        let (s, _) = api.llamar("GET", ruta, Some(&cliente), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{ruta}");
    }

    let (s, sesion) = api
        .llamar(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": "operador@synthetic.mx", "password": "operador-de-pruebas-123" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sesion["rol"], "OPERADOR");
    let operador = sesion["token"].as_str().unwrap();
    let (s, auditoria) = api.llamar("GET", "/api/v1/auditoria/ledger/ifpe", Some(operador), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(auditoria["valido"], true);
    // El operador no opera como cliente.
    let (s, _) = api.llamar("GET", "/api/v1/tablero", Some(operador), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../migrations")]
async fn login_bloquea_tras_intentos_fallidos(pool: PgPool) {
    let api = api(&pool).await;
    registrar(&api, "Ana").await;
    let intento = |password: &str| json!({ "email": "ana@example.mx", "password": password });
    for _ in 0..5 {
        let (s, _) = api.llamar("POST", "/api/v1/auth/login", None, Some(intento("incorrecta-123456"))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, _) = api.llamar("POST", "/api/v1/auth/login", None, Some(intento("contraseña-segura-123"))).await;
    assert_eq!(s, StatusCode::LOCKED, "bloqueada aunque ahora la contraseña sea correcta");
    let (s, _) = api
        .llamar("POST", "/api/v1/auth/login", None, Some(json!({"email": "nadie@example.mx", "password": "x"})))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../migrations")]
async fn operador_deposita_por_spei_y_origina_credito(pool: PgPool) {
    let api = api(&pool).await;
    let (ana, registro) = registrar(&api, "Ana").await;
    let (_, taller) = registrar(&api, "Taller").await;
    let (_, sesion) = api
        .llamar(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": "operador@synthetic.mx", "password": "operador-de-pruebas-123" })),
        )
        .await;
    let operador = sesion["token"].as_str().unwrap();

    let (s, r) = api
        .llamar(
            "POST",
            "/api/v1/operaciones/depositos",
            Some(operador),
            Some(json!({
                "clabe": registro["cuenta_ifpe"]["clabe"],
                "monto": "5000",
                "banco_emisor": "40012",
                "fecha_operacion": "2026-10-01",
                "clave_rastreo": "SPEI123456"
            })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");

    // Un segundo operador registra el mismo SPEI: no se acredita otra vez.
    bootstrap::asegurar_operador(&pool, "operador2@synthetic.mx", "operador-de-pruebas-456").await.unwrap();
    let (_, sesion2) = api
        .llamar(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": "operador2@synthetic.mx", "password": "operador-de-pruebas-456" })),
        )
        .await;
    let (s, r2) = api
        .llamar(
            "POST",
            "/api/v1/operaciones/depositos",
            Some(sesion2["token"].as_str().unwrap()),
            Some(json!({
                "clabe": registro["cuenta_ifpe"]["clabe"],
                "monto": "5000",
                "banco_emisor": "40012",
                "fecha_operacion": "2026-10-01",
                "clave_rastreo": "spei123456"
            })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r2}");
    assert_eq!(r2["transaccion"]["id"], r["transaccion"]["id"]);

    let (s, _) = api
        .llamar(
            "POST",
            "/api/v1/sofom/mutuos",
            Some(&ana),
            Some(json!({ "monto": "4000", "plazo_dias": 364, "clave_idempotencia": "mutuo-anual-01" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);

    let solicitud = |principal: &str, plazo: i32, clave: &str| {
        json!({
            "acreditado_id": taller["cliente"]["id"],
            "principal": principal,
            "tasa_anual": "0.18",
            "plazo_dias": plazo,
            "clabe_destino": "646180157000000017",
            "clave_idempotencia": clave
        })
    };
    // Crédito a 2 años con mutuos de 1 año: imposible.
    let (s, e) = api
        .llamar("POST", "/api/v1/operaciones/creditos", Some(operador), Some(solicitud("1000", 730, "credito-0001")))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(e["error"]["codigo"], "FONDEO_A_PLAZO_INSUFICIENTE");
    let (s, c) = api
        .llamar("POST", "/api/v1/operaciones/creditos", Some(operador), Some(solicitud("3000", 300, "credito-0002")))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["credito"]["estado"], "VIGENTE");

    let (s, alertas) = api.llamar("GET", "/api/v1/operaciones/alertas", Some(operador), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(alertas.as_array().unwrap().is_empty());
    verificar_invariantes(&pool).await;
}

/// Hallazgo de la revisión: el rol del JWT se confirma contra la base de
/// datos en cada petición, y las sesiones pueden revocarse de inmediato.
#[sqlx::test(migrations = "../migrations")]
async fn sesiones_revocables_y_rol_verificado(pool: PgPool) {
    let api = api(&pool).await;
    let (cliente, registro) = registrar(&api, "Ana").await;
    let (_, sesion) = api
        .llamar(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": "operador@synthetic.mx", "password": "operador-de-pruebas-123" })),
        )
        .await;
    let operador = sesion["token"].as_str().unwrap().to_string();
    let operador_id = sesion["cliente_id"].as_str().unwrap().to_string();

    // Revocar las sesiones de la clienta: su token deja de servir al instante.
    let ruta = format!("/api/v1/operaciones/usuarios/{}/revocar-sesiones", registro["cliente"]["id"].as_str().unwrap());
    // (Esperar al siguiente segundo: `iat` del JWT tiene resolución de segundos.)
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (s, _) = api.llamar("POST", &ruta, Some(&operador), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = api.llamar("GET", "/api/v1/tablero", Some(&cliente), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Un inicio de sesión nuevo sí funciona.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (s, nueva) = api
        .llamar(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": "ana@example.mx", "password": "contraseña-segura-123" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = api.llamar("GET", "/api/v1/tablero", Some(nueva["token"].as_str().unwrap()), None).await;
    assert_eq!(s, StatusCode::OK);

    // Degradar al operador: su token (aún vigente) pierde el poder de inmediato.
    sqlx::query("UPDATE kyc.credenciales SET rol = 'CLIENTE' WHERE cliente_id = $1::uuid")
        .bind(&operador_id)
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = api.llamar("GET", "/api/v1/operaciones/alertas", Some(&operador), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
