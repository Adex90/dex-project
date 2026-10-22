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

    /// Petición con cuerpo y cabeceras arbitrarios. Exige que la respuesta sea
    /// el sobre JSON de error de la API.
    async fn crudo(&self, metodo: &str, ruta: &str, tipo: Option<&str>, cuerpo: Vec<u8>) -> (StatusCode, String) {
        let mut req = Request::builder().method(metodo).uri(ruta);
        if let Some(t) = tipo {
            req = req.header("content-type", t);
        }
        let resp = self.app.clone().oneshot(req.body(Body::from(cuerpo)).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let valor: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!("{metodo} {ruta}: respuesta {status} sin JSON: {:?}", String::from_utf8_lossy(&bytes))
        });
        let codigo = valor["error"]["codigo"].as_str().unwrap_or_else(|| panic!("sin error.codigo: {valor}"));
        assert!(valor["error"]["mensaje"].is_string(), "{valor}");
        (status, codigo.to_string())
    }
}

async fn api(pool: &PgPool) -> Api {
    let cfg = preparar(pool).await;
    bootstrap::asegurar_operador(pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    Api { app: routes::router(AppState::nuevo(pool.clone(), cfg)) }
}

/// Registro + onboarding completo por HTTP (datos, INE y selfie).
async fn registrar(api: &Api, nombre: &str) -> (String, Value) {
    let (token, r) = registrar_sin_kyc(api, nombre).await;
    let id = IdentidadPrueba::nueva(nombre);
    let (s, d) = api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json())).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    let (s, v) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["estado_kyc"], "VERIFICADO", "{v}");
    (token, r)
}

async fn registrar_sin_kyc(api: &Api, nombre: &str) -> (String, Value) {
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

    let (token, registro) = registrar_sin_kyc(&api, "Ana").await;
    assert_eq!(registro["cliente"]["estatus_kyc"], "PENDIENTE");

    // Sin KYC verificado no entra dinero.
    let deposito = json!({ "monto": "10000.00", "clave_idempotencia": "deposito-0001" });
    let (s, e) = api.llamar("POST", "/api/v1/ifpe/depositos/simulados", Some(&token), Some(deposito.clone())).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    assert_eq!(e["error"]["codigo"], "KYC_NO_VERIFICADO");

    // Onboarding: datos personales, INE + selfie -> VERIFICADO.
    let id = IdentidadPrueba::nueva("Ana");
    let (s, exp) = api.llamar("GET", "/api/v1/kyc/expediente", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{exp}");
    assert_eq!(exp["estado_kyc"], "PENDIENTE");
    assert_eq!(exp["intentos_restantes"], 3);
    let (s, d) = api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json())).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["advertencias"], json!([]));
    let (s, v) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["estado_kyc"], "VERIFICADO");
    assert_eq!(v["resultado"]["resultado"], "APROBADO");
    let (_, yo) = api.llamar("GET", "/api/v1/yo", Some(&token), None).await;
    assert_eq!(yo["estatus_kyc"], "VERIFICADO");
    assert_eq!(yo["curp"], id.curp.as_str());

    // Depósito simulado (modo demo) e idempotencia HTTP: 201 y luego 200.
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
    for ruta in ["/api/v1/auditoria/ledger/ifpe", "/api/v1/aml/alertas", "/api/v1/operaciones/creditos"] {
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

    // El operador no tiene acceso a la consola PLD (solo CUMPLIMIENTO y AUDITOR).
    let (s, _) = api.llamar("GET", "/api/v1/aml/alertas", Some(operador), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
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
    let (s, cuerpo) = api.llamar("GET", "/api/v1/tablero", Some(nueva["token"].as_str().unwrap()), None).await;
    assert_eq!(s, StatusCode::OK, "{cuerpo}");

    // Degradar al operador: su token (aún vigente) pierde el poder de inmediato.
    sqlx::query("UPDATE kyc.credenciales SET rol = 'CLIENTE' WHERE cliente_id = $1::uuid")
        .bind(&operador_id)
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = api.llamar("GET", "/api/v1/operaciones/creditos", Some(&operador), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// -----------------------------------------------------------------------------
// Ronda 2 de revisión: contrato de la API
// -----------------------------------------------------------------------------

/// Hallazgo #14: todo error, también los del framework (cuerpo ilegible, tipo
/// de contenido, ruta o método inexistente), usa el sobre JSON de la API.
#[sqlx::test(migrations = "../migrations")]
async fn errores_siempre_en_sobre_json(pool: PgPool) {
    let api = api(&pool).await;
    let login = br#"{"email":"a@b.mx","password":"x"}"#.to_vec();
    type Caso<'a> = (&'a str, &'a str, Option<&'a str>, Vec<u8>, StatusCode, &'a str);
    let casos: Vec<Caso> = vec![
        ("POST", "/api/v1/auth/login", None, login.clone(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "TIPO_NO_SOPORTADO"),
        (
            "POST",
            "/api/v1/auth/login",
            Some("application/json"),
            br#"{"email":"#.to_vec(),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "POST",
            "/api/v1/auth/registro",
            Some("application/json"),
            br#"{"tipo_persona":"FISICA","nombre":"A","email":"a@b.mx","password":"x","x":1}"#.to_vec(),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        ("GET", "/api/v1/nada", None, vec![], StatusCode::NOT_FOUND, "RUTA_INEXISTENTE"),
        ("GET", "/nada", None, vec![], StatusCode::NOT_FOUND, "RUTA_INEXISTENTE"),
        ("DELETE", "/api/v1/auth/login", None, vec![], StatusCode::METHOD_NOT_ALLOWED, "METODO_NO_PERMITIDO"),
        ("GET", "/api/v1/reserva/cortes/no-es-uuid", None, vec![], StatusCode::BAD_REQUEST, "VALIDACION"),
        ("GET", "/api/v1/reserva/cortes?limite=muchos", None, vec![], StatusCode::BAD_REQUEST, "VALIDACION"),
        (
            "POST",
            "/api/v1/reserva/verificar",
            Some("application/json"),
            vec![b' '; 70 * 1024],
            StatusCode::PAYLOAD_TOO_LARGE,
            "CUERPO_DEMASIADO_GRANDE",
        ),
    ];
    for (metodo, ruta, tipo, cuerpo, status, codigo) in casos {
        let (s, c) = api.crudo(metodo, ruta, tipo, cuerpo).await;
        assert_eq!((s, c.as_str()), (status, codigo), "{metodo} {ruta}");
    }
}

/// Hallazgo #12: un monto enviado como número JSON pasaba por f64 y se
/// redondeaba en silencio. Ahora solo se aceptan cadenas.
#[sqlx::test(migrations = "../migrations")]
async fn montos_como_numero_json_rechazados(pool: PgPool) {
    let api = api(&pool).await;
    let (token, _) = registrar(&api, "Ana").await;
    for monto in [json!(1000000000.00000001_f64), json!(10), json!("10000000.001")] {
        let cuerpo = json!({ "monto": monto, "clave_idempotencia": format!("deposito-{}", uuid::Uuid::new_v4()) });
        let (s, e) = api.llamar("POST", "/api/v1/ifpe/depositos/simulados", Some(&token), Some(cuerpo)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{monto}: {e}");
        assert_eq!(e["error"]["codigo"], "VALIDACION");
    }
    let movimientos: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ifpe.transacciones").fetch_one(&pool).await.unwrap();
    assert_eq!(movimientos, 0, "ningún monto redondeado llegó al ledger");
    // Un depósito (simulado como SPEI) llega exacto, en centavos.
    let cuerpo = json!({ "monto": "1000000000.01", "clave_idempotencia": "deposito-exacto" });
    let (s, r) = api.llamar("POST", "/api/v1/ifpe/depositos/simulados", Some(&token), Some(cuerpo)).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["transaccion"]["monto"], "1000000000.01000000");
}

/// Hallazgo #11: entradas que no caben en las columnas responden 400, no 500.
#[sqlx::test(migrations = "../migrations")]
async fn entrada_invalida_es_400_no_500(pool: PgPool) {
    let api = api(&pool).await;
    let base = json!({
        "tipo_persona": "FISICA",
        "nombre": "Ana",
        "apellido_paterno": "Prueba",
        "email": "ana@example.mx",
        "password": "contraseña-segura-123"
    });
    for (campo, valor) in [
        ("rfc", json!("ABCDEFGHIJKLMNOPQ")),
        ("curp", json!("GODE561231HDFRRN09XXXXX")),
        ("ingreso_mensual_estimado", json!("100000000000")),
        ("ingreso_mensual_estimado", json!(5000)),
        ("telefono", json!("12")),
        ("email", json!("ana@sin-dominio")),
    ] {
        let mut cuerpo = base.clone();
        cuerpo[campo] = valor.clone();
        let (s, e) = api.llamar("POST", "/api/v1/auth/registro", None, Some(cuerpo)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{campo}={valor}: {e}");
        assert_eq!(e["error"]["codigo"], "VALIDACION");
    }
}

/// Hallazgo #15: una ráfaga de contraseñas en paralelo verifica como máximo
/// `MAX_INTENTOS`; el resto recibe BLOQUEADO sin calcular el hash. Con la
/// cuenta bloqueada, la contraseña correcta recibe la misma respuesta que una
/// equivocada, así que la respuesta no revela cuál era.
///
/// (Una contraseña correcta *dentro* del cupo abre sesión y reinicia el
/// contador; eso solo lo puede provocar quien ya conoce la contraseña.)
#[sqlx::test(migrations = "../migrations")]
async fn rafaga_de_login_no_supera_el_cupo(pool: PgPool) {
    use synthetic_capital::auth;
    let cfg = preparar(&pool).await;
    let estado = AppState::nuevo(pool.clone(), cfg);
    let api = Api { app: routes::router(estado.clone()) };
    registrar(&api, "Ana").await;

    let rafaga = |passwords: Vec<String>| {
        let estado = estado.clone();
        async move {
            let tareas: Vec<_> = passwords
                .into_iter()
                .map(|p| {
                    let estado = estado.clone();
                    tokio::spawn(async move {
                        match auth::iniciar_sesion(&estado, "ana@example.mx", &p).await {
                            Ok(_) => "SESION".to_string(),
                            Err(e) => e.codigo().to_string(),
                        }
                    })
                })
                .collect();
            let mut codigos = Vec::new();
            for t in tareas {
                codigos.push(t.await.unwrap());
            }
            codigos
        }
    };

    // 40 contraseñas equivocadas en paralelo: como máximo 5 se verifican.
    let codigos = rafaga((0..40).map(|i| format!("intento-equivocado-{i}")).collect()).await;
    let verificados = codigos.iter().filter(|c| *c == "CREDENCIALES_INVALIDAS").count();
    assert!((1..=5).contains(&verificados), "{verificados} contraseñas verificadas en una ráfaga: {codigos:?}");
    assert!(codigos.iter().all(|c| c == "CREDENCIALES_INVALIDAS" || c == "BLOQUEADO"), "{codigos:?}");

    // Bloqueada: la correcta, sola o mezclada con equivocadas, recibe BLOQUEADO como todas.
    let mut mezcla: Vec<String> = (0..9).map(|i| format!("otra-equivocada-{i}")).collect();
    mezcla.insert(4, "contraseña-segura-123".into());
    let codigos = rafaga(mezcla).await;
    assert!(codigos.iter().all(|c| c == "BLOQUEADO"), "{codigos:?}");

    // Bloqueada: ni la contraseña correcta abre sesión hasta que venza la ventana.
    let e = auth::iniciar_sesion(&estado, "ana@example.mx", "contraseña-segura-123").await.unwrap_err();
    assert_eq!(e.codigo(), "BLOQUEADO");
    sqlx::query("UPDATE kyc.credenciales SET bloqueado_hasta = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    // Ventana nueva: cuatro errores y luego la correcta abre sesión y reinicia el contador.
    for i in 0..4 {
        let e = auth::iniciar_sesion(&estado, "ana@example.mx", &format!("otra-{i}")).await.unwrap_err();
        assert_eq!(e.codigo(), "CREDENCIALES_INVALIDAS");
    }
    auth::iniciar_sesion(&estado, "ana@example.mx", "contraseña-segura-123").await.unwrap();
    let intentos: i32 =
        sqlx::query_scalar("SELECT intentos_fallidos FROM kyc.credenciales").fetch_one(&pool).await.unwrap();
    assert_eq!(intentos, 0);
}
