//! Verificación reforzada (step-up) antes de las acciones sensibles
//! (migración 0022): `/auth/step-up`, su método (la contraseña, o el código
//! TOTP con el segundo factor activo), el cupo de intentos que comparte con el
//! inicio de sesión, la ventana de cada sesión y las rutas que la exigen.
//! `tests/db/0022_verificacion_reforzada.sql` cubre lo que garantiza la base
//! de datos por sí sola; `tests/beneficiarios.rs`, que la designación la
//! repite en la base de datos; `tests/aml.rs`, que no delata un congelamiento.

mod comun;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use chrono::{DateTime, Utc};
use comun::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    config::Config, models::kyc::RolUsuario, patrimonio::vinculadas::clabe_demo, routes, spei::clabe, AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

const MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
                   Chrome/126.0.0.0 Safari/537.36";
/// La IP que agrega el proxy propio: la de la petición que verifica.
const IP: &str = "198.51.100.23";
const INCORRECTA: &str = "no-es-la-contraseña-1";
const MENSAJE_403: &str =
    "esta operación pide verificar de nuevo tu identidad: el código de tu app autenticadora o tu contraseña";

struct Banco {
    app: Router,
    estado: AppState,
    cfg: Config,
}

async fn banco(pool: &PgPool) -> Banco {
    let mut cfg = preparar(pool).await;
    cfg.http.proxy_confiable = true;
    // Los pagos SPEI a terceros también sacan dinero: encendidos aquí.
    cfg.spei.transferencias_terceros = true;
    let estado = AppState::nuevo(pool.clone(), cfg.clone());
    Banco { app: routes::router(estado.clone()), estado, cfg }
}

impl Banco {
    async fn llamar(
        &self,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req =
            Request::builder().method(metodo).uri(ruta).header("user-agent", MAC).header("x-forwarded-for", IP);
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
        (status, if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() })
    }

    async fn verificar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        self.llamar("POST", "/api/v1/auth/step-up", Some(token), Some(cuerpo)).await
    }

    async fn estado_step_up(&self, token: &str) -> Value {
        let (s, v) = self.llamar("GET", "/api/v1/auth/step-up", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn sesion(&self, cliente_id: Uuid, mfa: bool) -> String {
        token_de(&self.estado, cliente_id, RolUsuario::Cliente, mfa).await
    }

    fn sid(&self, token: &str) -> Uuid {
        self.estado.jwt.validar(token).unwrap().sid.unwrap()
    }
}

fn codigo(v: &Value) -> &str {
    v["error"]["codigo"].as_str().unwrap_or_default()
}

fn clave_api() -> String {
    Uuid::new_v4().to_string()
}

/// CLABE externa válida (Banorte) a nombre de un tercero.
fn externa() -> String {
    let primeros = "07218000000000004";
    format!("{primeros}{}", clabe::digito_verificador(primeros).unwrap())
}

/// Fallos contados en el cupo del inicio de sesión.
async fn intentos(pool: &PgPool, cliente: Uuid) -> i32 {
    sqlx::query_scalar("SELECT intentos_fallidos FROM kyc.credenciales WHERE cliente_id = $1")
        .bind(cliente)
        .fetch_one(pool)
        .await
        .unwrap()
}

type Evento = (String, Option<Uuid>, Option<String>, Option<String>, Option<String>, String);

/// La bitácora de seguridad del cliente sin los inicios de sesión: (evento,
/// sesión, IP, dispositivo, detalle, quién lo registró).
async fn bitacora(pool: &PgPool, cliente: Uuid) -> Vec<Evento> {
    sqlx::query_as(
        "SELECT evento, sesion_id, host(ip), dispositivo, detalle, registrado_por FROM kyc.eventos_autenticacion \
          WHERE cliente_id = $1 AND evento IN ('STEP_UP_VERIFICADO', 'STEP_UP_FALLIDO', 'CUENTA_BLOQUEADA') \
          ORDER BY id",
    )
    .bind(cliente)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn evento(nombre: &str, sesion: Uuid, detalle: Option<&str>) -> Evento {
    (
        nombre.into(),
        Some(sesion),
        Some(IP.into()),
        Some("Chrome on macOS".into()),
        detalle.map(Into::into),
        "sc_auth".into(),
    )
}

/// Sin una verificación reciente, ninguna salida de dinero ni cambio de
/// beneficiarios: 403 antes de tocar nada. Con la ventana abierta, las mismas
/// peticiones (con su misma clave de idempotencia) pasan.
#[sqlx::test(migrations = "../migrations")]
async fn sin_ventana_no_sale_dinero_ni_cambian_los_beneficiarios(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let beto = cliente(&pool, &b.cfg, "Beto").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let token = b.sesion(ana.cliente.id, false).await;

    assert_eq!(
        b.estado_step_up(&token).await,
        json!({ "metodo": "CONTRASENA", "valido_hasta": null, "segundos_restantes": 0, "vigencia_segundos": 300 })
    );
    let (s, cuentas) = b.llamar("GET", "/api/v1/wealth/linked-accounts", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{cuentas}");
    let acciones = [
        (
            "/api/v1/wealth/withdraw",
            json!({ "linked_account_id": cuentas["accounts"][0]["id"], "amount": "100.00", "idempotency_key": clave_api() }),
        ),
        (
            "/api/v1/ifpe/retiros",
            json!({ "clabe_destino": clabe_demo(ana.cliente.id, 0).as_str(), "monto": "100.00",
                    "clave_idempotencia": clave_api() }),
        ),
        (
            "/api/v1/ifpe/transferencias",
            json!({ "clabe_destino": beto.cuenta_ifpe.clabe, "monto": "100.00", "clave_idempotencia": clave_api() }),
        ),
        (
            "/api/v1/spei/transfer",
            json!({ "clabe_destino": externa(), "beneficiario_nombre": "Proveedor Externo", "monto": "100.00",
                    "clave_idempotencia": clave_api() }),
        ),
        (
            "/api/v1/kyc/beneficiarios",
            json!({ "version_base": 0, "beneficiarios": [{ "nombre_completo": "José López Rojas", "parentesco": "HIJO",
                                                            "fecha_nacimiento": "2005-06-07", "porcentaje": "100" }] }),
        ),
    ];
    for (ruta, cuerpo) in &acciones {
        let (s, v) = b.llamar("POST", ruta, Some(&token), Some(cuerpo.clone())).await;
        assert_eq!((s, codigo(&v)), (StatusCode::FORBIDDEN, "STEP_UP_REQUERIDO"), "{ruta}: {v}");
        assert_eq!(v["error"]["mensaje"], MENSAJE_403, "{ruta}");
    }
    // Nada se movió ni se guardó; leer sigue abierto.
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    let ordenes: i64 = sqlx::query_scalar("SELECT count(*) FROM ifpe.spei_outbound WHERE cliente_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let designaciones: i64 =
        sqlx::query_scalar("SELECT count(*) FROM kyc.designaciones_beneficiarios").fetch_one(&pool).await.unwrap();
    assert_eq!((ordenes, designaciones), (0, 0));
    let (s, _) = b.llamar("GET", "/api/v1/ifpe/cuenta", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK);
    // Sin sesión no hay verificación.
    let (s, _) = b.llamar("POST", "/api/v1/auth/step-up", None, Some(json!({ "password": PASSWORD_CLIENTE }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, v) = b.verificar(&token, json!({ "password": PASSWORD_CLIENTE })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for (ruta, cuerpo) in &acciones {
        let (s, v) = b.llamar("POST", ruta, Some(&token), Some(cuerpo.clone())).await;
        assert_eq!(s, StatusCode::CREATED, "{ruta}: {v}");
    }
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("600"), "salieron los cuatro pagos de 100");
    verificar_invariantes(&pool).await;
}

/// La contraseña abre una ventana de 5 minutos en esa sesión y no en las
/// demás; el fallo y la verificación quedan en la bitácora con la IP y el
/// dispositivo de la petición, y la verificación correcta vuelve el cupo a cero.
#[sqlx::test(migrations = "../migrations")]
async fn la_contrasena_abre_cinco_minutos_en_esa_sesion(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let token = b.sesion(ana.cliente.id, false).await;
    let otra = b.sesion(ana.cliente.id, false).await;

    let (s, v) = b.verificar(&token, json!({ "password": INCORRECTA })).await;
    assert_eq!(
        (s, codigo(&v), &v["error"]["intentos_restantes"]),
        (StatusCode::UNPROCESSABLE_ENTITY, "CONTRASENA_ACTUAL_INCORRECTA", &json!(4)),
        "{v}"
    );
    assert_eq!(intentos(&pool, ana.cliente.id).await, 1);
    assert_eq!(b.estado_step_up(&token).await["valido_hasta"], Value::Null);

    let (s, v) = b.verificar(&token, json!({ "password": PASSWORD_CLIENTE })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((&v["metodo"], &v["segundos"]), (&json!("CONTRASENA"), &json!(300)), "{v}");
    let hasta: DateTime<Utc> = v["valido_hasta"].as_str().unwrap().parse().unwrap();
    assert!((295..=300).contains(&(hasta - Utc::now()).num_seconds()), "{hasta}");
    assert_eq!(intentos(&pool, ana.cliente.id).await, 0, "una verificación correcta vuelve el cupo a cero");

    let estado = b.estado_step_up(&token).await;
    assert_eq!((&estado["metodo"], &estado["valido_hasta"]), (&json!("CONTRASENA"), &v["valido_hasta"]));
    assert!((295..=300).contains(&estado["segundos_restantes"].as_i64().unwrap()), "{estado}");
    assert_eq!(b.estado_step_up(&otra).await["valido_hasta"], Value::Null, "la otra sesión sigue sin ventana");

    let sid = b.sid(&token);
    assert_eq!(
        bitacora(&pool, ana.cliente.id).await,
        [evento("STEP_UP_FALLIDO", sid, None), evento("STEP_UP_VERIFICADO", sid, Some("300"))]
    );
    // El cliente los ve en su centro de seguridad, marcados en esta sesión.
    let (s, v) = b.llamar("GET", "/api/v1/auth/security/events", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let vistos: Vec<(&str, bool)> = v["eventos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["evento"].as_str().unwrap().starts_with("STEP_UP"))
        .map(|e| (e["evento"].as_str().unwrap(), e["sesion_actual"].as_bool().unwrap()))
        .collect();
    assert_eq!(vistos, [("STEP_UP_VERIFICADO", true), ("STEP_UP_FALLIDO", true)], "{v}");
}

/// Los fallos cuentan en el mismo cupo que el inicio de sesión: el quinto
/// bloquea la cuenta 15 minutos y cierra la sesión que lo intentó.
#[sqlx::test(migrations = "../migrations")]
async fn cinco_fallos_bloquean_la_cuenta_y_cierran_la_sesion(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let token = b.sesion(ana.cliente.id, false).await;
    let otra = b.sesion(ana.cliente.id, false).await;

    for restantes in [4, 3, 2, 1] {
        let (s, v) = b.verificar(&token, json!({ "password": INCORRECTA })).await;
        assert_eq!(
            (s, codigo(&v), &v["error"]["intentos_restantes"]),
            (StatusCode::UNPROCESSABLE_ENTITY, "CONTRASENA_ACTUAL_INCORRECTA", &json!(restantes)),
            "{v}"
        );
    }
    let (s, v) = b.verificar(&token, json!({ "password": INCORRECTA })).await;
    assert_eq!((s, codigo(&v)), (StatusCode::LOCKED, "SESION_BLOQUEADA"), "{v}");
    assert_eq!(
        v["error"]["mensaje"],
        "demasiados intentos fallidos: la cuenta quedó bloqueada 15 minutos y esta sesión se cerró"
    );

    // La sesión que lo intentó ya no vale, y queda dicho por qué.
    let (s, _) = b.llamar("GET", "/api/v1/yo", Some(&token), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let motivo: Option<String> = sqlx::query_scalar("SELECT motivo_revocacion FROM kyc.sesiones WHERE id = $1")
        .bind(b.sid(&token))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(motivo.as_deref(), Some("BLOQUEO"));

    // Mientras dure el bloqueo, ni la contraseña correcta: desde otra sesión
    // (que sigue abierta) ni para iniciar sesión. Esos intentos no se cuentan.
    let (s, _) = b.llamar("GET", "/api/v1/yo", Some(&otra), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = b.verificar(&otra, json!({ "password": PASSWORD_CLIENTE })).await;
    assert_eq!((s, codigo(&v)), (StatusCode::LOCKED, "BLOQUEADO"), "{v}");
    let login = json!({ "email": "ana@example.mx", "password": PASSWORD_CLIENTE });
    let (s, v) = b.llamar("POST", "/api/v1/auth/login", None, Some(login)).await;
    assert_eq!((s, codigo(&v)), (StatusCode::LOCKED, "BLOQUEADO"), "{v}");

    let sid = b.sid(&token);
    let mut esperado = vec![evento("STEP_UP_FALLIDO", sid, None); 5];
    esperado.push(evento("CUENTA_BLOQUEADA", sid, None));
    assert_eq!(bitacora(&pool, ana.cliente.id).await, esperado);
}

/// Con el segundo factor activo, la verificación pide el código de la app (la
/// contraseña no basta), y cada código sirve una sola vez.
#[sqlx::test(migrations = "../migrations")]
async fn con_el_segundo_factor_pide_el_codigo_una_sola_vez(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let secreto = activar_mfa(&b.estado, ana.cliente.id).await;
    let token = b.sesion(ana.cliente.id, true).await;
    assert_eq!(b.estado_step_up(&token).await["metodo"], "TOTP");

    // Lo que no es un intento no gasta uno.
    for (cuerpo, esperado) in [
        (json!({ "password": PASSWORD_CLIENTE }), (StatusCode::UNPROCESSABLE_ENTITY, "CODIGO_MFA_REQUERIDO")),
        (json!({}), (StatusCode::UNPROCESSABLE_ENTITY, "CODIGO_MFA_REQUERIDO")),
        (json!({ "codigo": "12ab56" }), (StatusCode::BAD_REQUEST, "VALIDACION")),
        (json!({ "codigo": "123456", "password": PASSWORD_CLIENTE }), (StatusCode::BAD_REQUEST, "VALIDACION")),
    ] {
        let (s, v) = b.verificar(&token, cuerpo.clone()).await;
        assert_eq!((s, codigo(&v)), esperado, "{cuerpo}: {v}");
    }
    assert_eq!(intentos(&pool, ana.cliente.id).await, 0);

    // El paso actual ya lo usó la activación: el siguiente (tolerado).
    let bueno = codigo_totp(&secreto, paso_totp_actual() + 1);
    let ultimo = bueno.chars().last().unwrap().to_digit(10).unwrap();
    let malo = format!("{}{}", &bueno[..5], (ultimo + 1) % 10);
    let (s, v) = b.verificar(&token, json!({ "codigo": malo })).await;
    assert_eq!(
        (s, codigo(&v), &v["error"]["intentos_restantes"]),
        (StatusCode::UNPROCESSABLE_ENTITY, "CODIGO_MFA_INVALIDO", &json!(4)),
        "{v}"
    );
    // Con espacios, como lo muestran algunas apps.
    let (s, v) = b.verificar(&token, json!({ "codigo": format!("{} {}", &bueno[..3], &bueno[3..]) })).await;
    assert_eq!((s, &v["metodo"]), (StatusCode::OK, &json!("TOTP")), "{v}");
    assert_eq!(intentos(&pool, ana.cliente.id).await, 0);

    // El mismo código no sirve dos veces, ni en otra sesión.
    let otra = b.sesion(ana.cliente.id, true).await;
    let (s, v) = b.verificar(&otra, json!({ "codigo": bueno })).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "CODIGO_MFA_INVALIDO"), "{v}");
    assert_eq!(b.estado_step_up(&otra).await["valido_hasta"], Value::Null);
}

/// Una ventana vencida ya no vale: se verifica otra vez.
#[sqlx::test(migrations = "../migrations")]
async fn una_ventana_vencida_ya_no_vale(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let token = token_verificado(&b.estado, &b.app, ana.cliente.id).await;

    // Como si hubieran pasado los 5 minutos: el dueño del esquema la vence.
    let mut tx = pool.begin().await.unwrap();
    for sql in [
        "ALTER TABLE kyc.sesiones DISABLE TRIGGER USER",
        "UPDATE kyc.sesiones SET step_up_valido_hasta = now() - interval '1 second'",
        "ALTER TABLE kyc.sesiones ENABLE TRIGGER USER",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();

    let estado = b.estado_step_up(&token).await;
    assert_eq!((&estado["valido_hasta"], &estado["segundos_restantes"]), (&Value::Null, &json!(0)), "{estado}");
    let (s, cuentas) = b.llamar("GET", "/api/v1/wealth/linked-accounts", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{cuentas}");
    let retiro = json!({ "linked_account_id": cuentas["accounts"][0]["id"], "amount": "100.00",
                         "idempotency_key": clave_api() });
    let (s, v) = b.llamar("POST", "/api/v1/wealth/withdraw", Some(&token), Some(retiro.clone())).await;
    assert_eq!((s, codigo(&v)), (StatusCode::FORBIDDEN, "STEP_UP_REQUERIDO"), "{v}");

    verificar_de_nuevo(&b.app, &token).await;
    let (s, v) = b.llamar("POST", "/api/v1/wealth/withdraw", Some(&token), Some(retiro)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}
