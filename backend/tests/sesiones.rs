//! Sesiones por dispositivo y centro de seguridad (migración 0019).
//!
//! Cada inicio de sesión tiene su fila en `kyc.sesiones`, el token la lleva
//! (`sid`) y cada petición la valida: el cliente ve sus dispositivos, cierra
//! uno o todos los demás, cambia su contraseña y elige su cierre por
//! inactividad, y todo queda en su bitácora. `tests/db/0019_sesiones_seguridad.sql`
//! cubre lo que garantiza la base de datos por sí sola.

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
use synthetic_capital::{auth, models::kyc::RolUsuario, routes, AppState};
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "contraseña-de-prueba-123";

/// Un dispositivo: su User-Agent y la IP que agrega el proxy propio.
#[derive(Clone, Copy)]
struct Dispositivo {
    ua: &'static str,
    ip: &'static str,
}

const MAC: Dispositivo = Dispositivo {
    ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/126.0.0.0 Safari/537.36",
    ip: "203.0.113.7",
};
const IPHONE: Dispositivo = Dispositivo {
    ua: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) \
         Version/17.5 Mobile/15E148 Safari/604.1",
    ip: "198.51.100.20",
};
const LINUX: Dispositivo =
    Dispositivo { ua: "Mozilla/5.0 (X11; Linux x86_64; rv:127.0) Gecko/20100101 Firefox/127.0", ip: "192.0.2.33" };

struct Banco {
    app: Router,
    estado: AppState,
}

async fn banco(pool: &PgPool) -> Banco {
    let mut cfg = preparar(pool).await;
    // Detrás del proxy propio: la IP del cliente es la última de X-Forwarded-For.
    cfg.http.proxy_confiable = true;
    let estado = AppState::nuevo(pool.clone(), cfg);
    Banco { app: routes::router(estado.clone()), estado }
}

impl Banco {
    async fn llamar(
        &self,
        d: Dispositivo,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req =
            Request::builder().method(metodo).uri(ruta).header("user-agent", d.ua).header("x-forwarded-for", d.ip);
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
        let valor = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        (status, valor)
    }

    async fn login(&self, d: Dispositivo, email: &str, password: &str) -> (StatusCode, Value) {
        self.llamar(d, "POST", "/api/v1/auth/login", None, Some(json!({ "email": email, "password": password }))).await
    }

    /// Inicia sesión desde `d`: el token de acceso.
    async fn entrar(&self, d: Dispositivo, email: &str) -> String {
        let (s, v) = self.login(d, email, PASSWORD).await;
        assert_eq!((s, v["estado"].as_str()), (StatusCode::OK, Some("OK")), "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    fn sid(&self, token: &str) -> Uuid {
        self.estado.jwt.validar(token).unwrap().sid.expect("el token lleva su sesión")
    }

    async fn sesiones(&self, token: &str) -> Value {
        let (s, v) = self.llamar(MAC, "GET", "/api/v1/auth/sessions", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn eventos(&self, token: &str) -> Vec<Value> {
        let (s, v) = self.llamar(MAC, "GET", "/api/v1/auth/security/events", Some(token), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["eventos"].as_array().unwrap().clone()
    }

    /// ¿El token todavía abre la API?
    async fn vale(&self, token: &str) -> bool {
        let (s, v) = self.llamar(MAC, "GET", "/api/v1/yo", Some(token), None).await;
        match s {
            StatusCode::OK => true,
            StatusCode::UNAUTHORIZED => false,
            otro => panic!("{otro}: {v}"),
        }
    }

    /// Cierra la sesión `sid` (otra o la propia) con `token`.
    async fn revocar(&self, token: &str, sid: &str) -> (StatusCode, Value) {
        self.llamar(MAC, "POST", &format!("/api/v1/auth/sessions/{sid}/revoke"), Some(token), None).await
    }

    async fn cambiar(&self, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        self.llamar(MAC, "POST", "/api/v1/auth/password/change", Some(token), Some(cuerpo)).await
    }

    async fn preferir(&self, token: &str, minutos: i64) -> (StatusCode, Value) {
        let cuerpo = json!({ "inactividad_minutos": minutos });
        self.llamar(MAC, "POST", "/api/v1/auth/security/preferences", Some(token), Some(cuerpo)).await
    }
}

fn codigo(v: &Value) -> Option<&str> {
    v["error"]["codigo"].as_str()
}

/// Los eventos de un tipo, el más reciente primero.
fn de_tipo<'a>(eventos: &'a [Value], evento: &str) -> Vec<&'a Value> {
    eventos.iter().filter(|e| e["evento"] == evento).collect()
}

fn ids(lista: &Value) -> Vec<String> {
    lista["sesiones"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_owned()).collect()
}

async fn motivo(pool: &PgPool, sid: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT motivo_revocacion FROM kyc.sesiones WHERE id = $1")
        .bind(sid)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn ultimo_uso(pool: &PgPool, sid: Uuid) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT ultimo_uso_en FROM kyc.sesiones WHERE id = $1").bind(sid).fetch_one(pool).await.unwrap()
}

async fn intentos(pool: &PgPool, cliente_id: Uuid) -> i32 {
    sqlx::query_scalar("SELECT intentos_fallidos FROM kyc.credenciales WHERE cliente_id = $1")
        .bind(cliente_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Simula el paso del tiempo para una sesión: recorre su apertura y su último
/// uso hacia atrás. Solo el dueño del esquema puede, desactivando el trigger
/// que la protege (la aplicación nunca puede).
async fn envejecer_sesion(pool: &PgPool, sid: Uuid, minutos: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE kyc.sesiones DISABLE TRIGGER trg_sesiones_proteger").execute(&mut *tx).await.unwrap();
    sqlx::query(
        "UPDATE kyc.sesiones SET creada_en = creada_en - make_interval(mins => $2), \
           ultimo_uso_en = ultimo_uso_en - make_interval(mins => $2) WHERE id = $1",
    )
    .bind(sid)
    .bind(minutos)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE kyc.sesiones ENABLE TRIGGER trg_sesiones_proteger").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

// -----------------------------------------------------------------------------
// Abrir sesiones
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn cada_inicio_de_sesion_abre_una_sesion_con_su_dispositivo(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let (s, v) = b.login(MAC, "ana@example.mx", PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let t_mac = v["token"].as_str().unwrap().to_owned();
    let sid_mac = b.sid(&t_mac);
    assert_eq!(v["sesion_id"], sid_mac.to_string(), "el inicio de sesión dice qué sesión abrió");
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;
    let sid_iphone = b.sid(&t_iphone);

    // Desde el Mac: las dos, la actual primero, con su dispositivo y su IP.
    let v = b.sesiones(&t_mac).await;
    assert_eq!(v["inactividad_minutos"], 30, "{v}");
    let lista = v["sesiones"].as_array().unwrap();
    assert_eq!(lista.len(), 2, "{v}");
    let resumen = |s: &Value| {
        (
            s["id"].as_str().unwrap().to_owned(),
            s["dispositivo"].as_str().unwrap().to_owned(),
            s["ip"].as_str().unwrap().to_owned(),
            s["actual"].as_bool().unwrap(),
            s["mfa"].as_bool().unwrap(),
        )
    };
    assert_eq!(resumen(&lista[0]), (sid_mac.to_string(), "Chrome on macOS".into(), "203.0.113.7".into(), true, false));
    assert_eq!(
        resumen(&lista[1]),
        (sid_iphone.to_string(), "Safari on iPhone".into(), "198.51.100.20".into(), false, false)
    );
    // Desde el iPhone, la actual es la suya.
    let v = b.sesiones(&t_iphone).await;
    assert_eq!(ids(&v), vec![sid_iphone.to_string(), sid_mac.to_string()]);
    assert_eq!(v["sesiones"][0]["actual"], true);

    // La fila vence con el token y no guarda el User-Agent completo.
    let (expira_en, dispositivo): (DateTime<Utc>, String) =
        sqlx::query_as("SELECT expira_en, dispositivo FROM kyc.sesiones WHERE id = $1")
            .bind(sid_mac)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(expira_en.timestamp(), b.estado.jwt.validar(&t_mac).unwrap().exp);
    assert_eq!(dispositivo, "Chrome on macOS");

    // La bitácora: los dos inicios, el más reciente primero, con dispositivo e IP.
    let eventos = b.eventos(&t_mac).await;
    let inicios = de_tipo(&eventos, "INICIO_SESION");
    assert_eq!(inicios.len(), 2, "{eventos:?}");
    assert_eq!(
        (inicios[0]["dispositivo"].as_str(), inicios[0]["ip"].as_str(), inicios[0]["sesion_actual"].as_bool()),
        (Some("Safari on iPhone"), Some("198.51.100.20"), Some(false))
    );
    assert_eq!(
        (inicios[1]["dispositivo"].as_str(), inicios[1]["ip"].as_str(), inicios[1]["sesion_actual"].as_bool()),
        (Some("Chrome on macOS"), Some("203.0.113.7"), Some(true))
    );

    // El alta de una cuenta abre su primera sesión.
    let (s, v) = b
        .llamar(
            LINUX,
            "POST",
            "/api/v1/auth/registro",
            None,
            Some(json!({ "tipo_persona": "FISICA", "nombre": "Beto", "apellido_paterno": "Prueba",
                         "email": "beto@example.mx", "password": PASSWORD })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let t_beto = v["sesion"]["token"].as_str().unwrap().to_owned();
    let v = b.sesiones(&t_beto).await;
    assert_eq!(ids(&v), vec![b.sid(&t_beto).to_string()]);
    assert_eq!(v["sesiones"][0]["dispositivo"], "Firefox on Linux");
    assert_eq!(de_tipo(&b.eventos(&t_beto).await, "INICIO_SESION").len(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn un_token_sin_una_sesion_vigente_no_vale(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    cliente_sin_kyc(&pool, &b.estado.config, "Beto").await;
    let t = b.entrar(MAC, "ana@example.mx").await;
    assert!(b.vale(&t).await);

    // Firmado por el servidor, pero de una sesión que no existe.
    let (inventado, _) = b.estado.jwt.emitir(ana.cliente.id, RolUsuario::Cliente, false, Uuid::new_v4()).unwrap();
    assert!(!b.vale(&inventado).await);
    // Con la sesión de otra persona.
    let t_beto = b.entrar(LINUX, "beto@example.mx").await;
    let (ajeno, _) = b.estado.jwt.emitir(ana.cliente.id, RolUsuario::Cliente, false, b.sid(&t_beto)).unwrap();
    assert!(!b.vale(&ajeno).await);
    // Sin sesión: un token como los de antes de 0019, con la misma clave.
    let ahora = Utc::now().timestamp();
    let viejo = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &json!({ "sub": ana.cliente.id, "rol": "CLIENTE", "iss": "synthetic-capital",
                 "aud": "synthetic-capital/acceso", "iat": ahora, "exp": ahora + 600,
                 "jti": Uuid::new_v4(), "mfa": false }),
        &jsonwebtoken::EncodingKey::from_secret(b.estado.config.jwt_secret.as_bytes()),
    )
    .unwrap();
    assert!(b.estado.jwt.validar(&viejo).is_ok(), "la firma es válida");
    assert!(!b.vale(&viejo).await, "pero no tiene sesión");
    // Una sesión vencida, aunque el JWT siguiera vigente.
    let sid = b.sid(&t);
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE kyc.sesiones DISABLE TRIGGER trg_sesiones_proteger").execute(&mut *tx).await.unwrap();
    sqlx::query(
        "UPDATE kyc.sesiones SET creada_en = now() - interval '2 hours', expira_en = now() - interval '1 minute' \
         WHERE id = $1",
    )
    .bind(sid)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE kyc.sesiones ENABLE TRIGGER trg_sesiones_proteger").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(!b.vale(&t).await);
    assert!(b.vale(&t_beto).await, "las de otros siguen");
}

// -----------------------------------------------------------------------------
// Cerrar sesiones
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn cerrar_una_sesion_desde_otro_dispositivo(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    cliente_sin_kyc(&pool, &b.estado.config, "Beto").await;
    let t_mac = b.entrar(MAC, "ana@example.mx").await;
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;
    let sid_iphone = b.sid(&t_iphone);
    let (s, v) = b.revocar(&t_mac, &sid_iphone.to_string()).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    assert!(!b.vale(&t_iphone).await, "el iPhone quedó fuera");
    assert!(b.vale(&t_mac).await, "el Mac sigue");
    assert_eq!(motivo(&pool, sid_iphone).await.as_deref(), Some("REVOCADA"));
    assert_eq!(ids(&b.sesiones(&t_mac).await), vec![b.sid(&t_mac).to_string()]);

    // Ya cerrada, de otra persona o con un id inválido: nada que cerrar.
    let (s, v) = b.revocar(&t_mac, &sid_iphone.to_string()).await;
    assert_eq!((s, codigo(&v)), (StatusCode::NOT_FOUND, Some("NO_ENCONTRADO")), "{v}");
    let t_beto = b.entrar(LINUX, "beto@example.mx").await;
    let (s, _) = b.revocar(&t_mac, &b.sid(&t_beto).to_string()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(b.vale(&t_beto).await, "la sesión de Beto sigue abierta");
    let (s, v) = b.revocar(&t_mac, "no-es-un-uuid").await;
    assert_eq!((s, codigo(&v)), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{v}");
    let (s, _) = b.revocar(&t_iphone, &sid_iphone.to_string()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "una sesión cerrada no cierra nada");

    // La bitácora: el dispositivo cerrado, desde esta sesión y su IP.
    let eventos = b.eventos(&t_mac).await;
    let cerradas = de_tipo(&eventos, "SESION_REVOCADA");
    assert_eq!(cerradas.len(), 1, "{eventos:?}");
    assert_eq!(
        (cerradas[0]["dispositivo"].as_str(), cerradas[0]["ip"].as_str(), cerradas[0]["sesion_actual"].as_bool()),
        (Some("Safari on iPhone"), Some("203.0.113.7"), Some(true))
    );
    // Beto no ve nada de Ana.
    let de_beto = b.eventos(&t_beto).await;
    assert_eq!(de_beto.len(), 1, "{de_beto:?}");
    assert_eq!(de_beto[0]["evento"], "INICIO_SESION");
}

#[sqlx::test(migrations = "../migrations")]
async fn cerrar_todas_las_demas_sesiones(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t_mac = b.entrar(MAC, "ana@example.mx").await;
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;
    let t_linux = b.entrar(LINUX, "ana@example.mx").await;
    // Una cuarta, abandonada: ya no vale ni se ve, pero también se revoca.
    let t_vieja = b.entrar(LINUX, "ana@example.mx").await;
    envejecer_sesion(&pool, b.sid(&t_vieja), 31).await;
    assert_eq!(ids(&b.sesiones(&t_mac).await).len(), 3);

    let (s, v) = b.llamar(MAC, "POST", "/api/v1/auth/sessions/revoke-others", Some(&t_mac), None).await;
    assert_eq!((s, v["sesiones_revocadas"].as_u64()), (StatusCode::OK, Some(2)), "{v}");
    assert!(b.vale(&t_mac).await);
    for t in [&t_iphone, &t_linux, &t_vieja] {
        assert!(!b.vale(t).await);
        assert_eq!(motivo(&pool, b.sid(t)).await.as_deref(), Some("OTRAS_REVOCADAS"));
    }
    assert_eq!(ids(&b.sesiones(&t_mac).await), vec![b.sid(&t_mac).to_string()]);

    // Otra vez: nada más que cerrar, y también queda registrado.
    let (s, v) = b.llamar(MAC, "POST", "/api/v1/auth/sessions/revoke-others", Some(&t_mac), None).await;
    assert_eq!((s, v["sesiones_revocadas"].as_u64()), (StatusCode::OK, Some(0)), "{v}");
    let eventos = b.eventos(&t_mac).await;
    let detalles: Vec<_> =
        de_tipo(&eventos, "SESIONES_REVOCADAS").iter().map(|e| e["detalle"].as_str().unwrap().to_owned()).collect();
    assert_eq!(detalles, vec!["0", "2"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn cerrar_la_sesion_propia(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t_mac = b.entrar(MAC, "ana@example.mx").await;
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;

    let (s, v) = b.llamar(IPHONE, "POST", "/api/v1/auth/logout", Some(&t_iphone), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    assert!(!b.vale(&t_iphone).await);
    assert_eq!(motivo(&pool, b.sid(&t_iphone)).await.as_deref(), Some("CIERRE"));
    let (s, _) = b.llamar(IPHONE, "POST", "/api/v1/auth/logout", Some(&t_iphone), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "ya cerrada");
    assert!(b.vale(&t_mac).await, "las demás siguen abiertas");

    // Cerrar la propia por su id es lo mismo que salir.
    let sid_mac = b.sid(&t_mac);
    let (s, _) = b.llamar(MAC, "POST", &format!("/api/v1/auth/sessions/{sid_mac}/revoke"), Some(&t_mac), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(!b.vale(&t_mac).await);
    assert_eq!(motivo(&pool, sid_mac).await.as_deref(), Some("CIERRE"));

    let t = b.entrar(MAC, "ana@example.mx").await;
    let eventos = b.eventos(&t).await;
    let cierres = de_tipo(&eventos, "SESION_CERRADA");
    assert_eq!(cierres.len(), 2, "{eventos:?}");
    assert_eq!(cierres[1]["dispositivo"], "Safari on iPhone");
}

#[sqlx::test(migrations = "../migrations")]
async fn restablecer_la_contrasena_o_un_operador_cierran_todas_las_sesiones(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t1 = b.entrar(MAC, "ana@example.mx").await;
    let t2 = b.entrar(IPHONE, "ana@example.mx").await;

    // Un operador revoca todas: tokens y filas, y queda en la bitácora sin su IP.
    auth::revocar_sesiones(&pool, ana.cliente.id).await.unwrap();
    for t in [&t1, &t2] {
        assert!(!b.vale(t).await);
        assert_eq!(motivo(&pool, b.sid(t)).await.as_deref(), Some("OPERADOR"));
    }
    let t3 = b.entrar(MAC, "ana@example.mx").await;
    let eventos = b.eventos(&t3).await;
    let revocadas = de_tipo(&eventos, "SESIONES_REVOCADAS");
    assert_eq!(revocadas.len(), 1, "{eventos:?}");
    assert_eq!((revocadas[0]["detalle"].as_str(), revocadas[0]["ip"].as_str()), (Some("2"), None));

    // Restablecer la contraseña por correo también las cierra.
    let (s, _) =
        b.llamar(LINUX, "POST", "/api/v1/auth/password/forgot", None, Some(json!({ "email": "ana@example.mx" }))).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let enlace = b.estado.correo.ultimo_para("ana@example.mx").unwrap().enlace;
    let token = enlace.split_once("#token=").unwrap().1.to_owned();
    let cuerpo = json!({ "token": token, "password": "otra-contraseña-456" });
    let (s, v) = b.llamar(LINUX, "POST", "/api/v1/auth/password/reset", None, Some(cuerpo)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(!b.vale(&t3).await);
    assert_eq!(motivo(&pool, b.sid(&t3)).await.as_deref(), Some("CONTRASENA_RESTABLECIDA"));
    let (s, v) = b.login(MAC, "ana@example.mx", "otra-contraseña-456").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let eventos = b.eventos(v["token"].as_str().unwrap()).await;
    let restablecida = de_tipo(&eventos, "CONTRASENA_RESTABLECIDA");
    assert_eq!(
        (restablecida[0]["detalle"].as_str(), restablecida[0]["dispositivo"].as_str()),
        (Some("1"), Some("Firefox on Linux"))
    );
}

// -----------------------------------------------------------------------------
// Inactividad
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn el_cierre_por_inactividad_lo_elige_el_cliente_y_no_revive_sesiones(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t = b.entrar(MAC, "ana@example.mx").await;
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;
    let t_linux = b.entrar(LINUX, "ana@example.mx").await;

    let (s, v) = b.llamar(MAC, "GET", "/api/v1/auth/security/preferences", Some(&t), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "inactividad_minutos": 30, "opciones_inactividad": [5, 15, 30, 60], "mfa_activo": false }));

    // 31 minutos sin uso: cerrada. 20: sigue (sin tocarla, que la marcaría como usada).
    envejecer_sesion(&pool, b.sid(&t_iphone), 31).await;
    envejecer_sesion(&pool, b.sid(&t_linux), 20).await;
    assert!(!b.vale(&t_iphone).await);
    assert_eq!(ids(&b.sesiones(&t).await), vec![b.sid(&t).to_string(), b.sid(&t_linux).to_string()]);

    // 15 minutos: la de 20 deja de valer; la inactiva según los 30 de antes se revoca.
    let (s, v) = b.preferir(&t, 15).await;
    assert_eq!((s, v["inactividad_minutos"].as_i64()), (StatusCode::OK, Some(15)), "{v}");
    assert_eq!(motivo(&pool, b.sid(&t_iphone)).await.as_deref(), Some("INACTIVIDAD"));
    assert_eq!(motivo(&pool, b.sid(&t_linux)).await, None);
    assert_eq!(ids(&b.sesiones(&t).await), vec![b.sid(&t).to_string()]);
    // Volver a 60 no la revive: inactiva según los 15, se revoca antes del cambio.
    let (s, _) = b.preferir(&t, 60).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(motivo(&pool, b.sid(&t_linux)).await.as_deref(), Some("INACTIVIDAD"));
    assert!(!b.vale(&t_linux).await);
    assert!(!b.vale(&t_iphone).await);
    assert!(b.vale(&t).await);
    // Lo mismo otra vez no es un cambio: sin evento nuevo.
    let (s, _) = b.preferir(&t, 60).await;
    assert_eq!(s, StatusCode::OK);

    for invalido in [0, 10, 120] {
        let (s, v) = b.preferir(&t, invalido).await;
        assert_eq!((s, codigo(&v)), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{invalido}: {v}");
    }
    let cuerpo = json!({ "inactividad_minutos": 15, "biometria": true });
    let (s, _) = b.llamar(MAC, "POST", "/api/v1/auth/security/preferences", Some(&t), Some(cuerpo)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "campos desconocidos");

    let eventos = b.eventos(&t).await;
    let cambios: Vec<_> = de_tipo(&eventos, "PREFERENCIAS_ACTUALIZADAS")
        .iter()
        .map(|e| e["detalle"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(cambios, vec!["60", "15"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn el_uso_se_marca_cada_minuto_y_la_revalidacion_del_socket_no_cuenta(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t = b.entrar(MAC, "ana@example.mx").await;
    let sid = b.sid(&t);
    let claims = b.estado.jwt.validar(&t).unwrap();

    let u0 = ultimo_uso(&pool, sid).await;
    assert!(b.vale(&t).await);
    assert_eq!(ultimo_uso(&pool, sid).await, u0, "menos de un minuto después: no se marca");

    envejecer_sesion(&pool, sid, 2).await;
    let u1 = ultimo_uso(&pool, sid).await;
    auth::revalidar(&pool, &claims).await.unwrap();
    assert_eq!(ultimo_uso(&pool, sid).await, u1, "la revalidación periódica del socket no es actividad");
    assert!(b.vale(&t).await);
    assert!(ultimo_uso(&pool, sid).await > u1, "una petición sí");

    // Una sesión inactiva tampoco pasa la revalidación: el socket se cierra.
    envejecer_sesion(&pool, sid, 31).await;
    assert!(auth::revalidar(&pool, &claims).await.is_err());
}

// -----------------------------------------------------------------------------
// Contraseña
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn cambiar_la_contrasena_cierra_las_demas_sesiones(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let id = ana.cliente.id;
    let t_mac = b.entrar(MAC, "ana@example.mx").await;
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;
    let t_linux = b.entrar(LINUX, "ana@example.mx").await;
    // Un enlace de restablecimiento pendiente.
    let (s, _) =
        b.llamar(LINUX, "POST", "/api/v1/auth/password/forgot", None, Some(json!({ "email": "ana@example.mx" }))).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let enlace = b.estado.correo.ultimo_para("ana@example.mx").unwrap().enlace;
    let token_enlace = enlace.split_once("#token=").unwrap().1.to_owned();

    // Lo que no depende de la contraseña actual no gasta intentos.
    let (s, v) = b.cambiar(&t_mac, json!({ "password_actual": PASSWORD, "password_nueva": "corta" })).await;
    assert_eq!((s, codigo(&v)), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{v}");
    let (s, v) = b.cambiar(&t_mac, json!({ "password_actual": PASSWORD, "password_nueva": PASSWORD })).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("CONTRASENA_REPETIDA")), "{v}");
    let (s, _) = b.cambiar(&t_mac, json!({ "password_actual": PASSWORD, "nueva": "x" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(intentos(&pool, id).await, 0);

    // La actual equivocada: cuenta en el bloqueo del inicio de sesión, y la sesión sigue.
    let cuerpo = json!({ "password_actual": "no-es-la-actual-123", "password_nueva": "nueva-contraseña-456" });
    let (s, v) = b.cambiar(&t_mac, cuerpo).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("CONTRASENA_ACTUAL_INCORRECTA")), "{v}");
    assert_eq!(intentos(&pool, id).await, 1);
    assert!(b.vale(&t_mac).await);

    let cuerpo = json!({ "password_actual": PASSWORD, "password_nueva": "nueva-contraseña-456" });
    let (s, v) = b.cambiar(&t_mac, cuerpo).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "estado": "OK", "sesiones_revocadas": 2 }));
    assert!(b.vale(&t_mac).await, "la sesión que la cambió sigue");
    for t in [&t_iphone, &t_linux] {
        assert!(!b.vale(t).await);
        assert_eq!(motivo(&pool, b.sid(t)).await.as_deref(), Some("CONTRASENA_CAMBIADA"));
    }
    assert_eq!(intentos(&pool, id).await, 0);
    let pendiente: Option<String> =
        sqlx::query_scalar("SELECT password_reset_token FROM kyc.credenciales WHERE cliente_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pendiente, None, "el enlace pendiente se anula");
    let cuerpo = json!({ "token": token_enlace, "password": "la-del-enlace-789" });
    let (s, v) = b.llamar(LINUX, "POST", "/api/v1/auth/password/reset", None, Some(cuerpo)).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("TOKEN_RESTABLECIMIENTO_INVALIDO")), "{v}");

    // La anterior ya no entra; la nueva sí.
    let (s, v) = b.login(IPHONE, "ana@example.mx", PASSWORD).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNAUTHORIZED, Some("CREDENCIALES_INVALIDAS")), "{v}");
    let (s, _) = b.login(IPHONE, "ana@example.mx", "nueva-contraseña-456").await;
    assert_eq!(s, StatusCode::OK);

    let eventos = b.eventos(&t_mac).await;
    let cambio = de_tipo(&eventos, "CONTRASENA_CAMBIADA");
    assert_eq!(cambio.len(), 1, "{eventos:?}");
    assert_eq!(
        (cambio[0]["detalle"].as_str(), cambio[0]["sesion_actual"].as_bool(), cambio[0]["dispositivo"].as_str()),
        (Some("2"), Some(true), Some("Chrome on macOS"))
    );
    assert_eq!(de_tipo(&eventos, "CAMBIO_CONTRASENA_RECHAZADO").len(), 1);
    assert_eq!(de_tipo(&eventos, "INICIO_FALLIDO").len(), 1, "la contraseña anterior en el iPhone");
}

#[sqlx::test(migrations = "../migrations")]
async fn con_segundo_factor_el_cambio_pide_el_codigo(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let totp = Autenticadora::default();
    let t0 = b.entrar(MAC, "ana@example.mx").await;

    // Activar el segundo factor desde la sesión: todas se cierran y abre una nueva que lo verificó.
    let (s, conf) = b.llamar(MAC, "POST", "/api/v1/auth/mfa/setup", Some(&t0), None).await;
    assert_eq!(s, StatusCode::OK, "{conf}");
    totp.guardar("ana@example.mx", secreto_base32(conf["secreto"].as_str().unwrap()));
    let cuerpo = json!({ "codigo": totp.codigo("ana@example.mx").await });
    let (s, v) = b.llamar(MAC, "POST", "/api/v1/auth/mfa/enable", Some(&t0), Some(cuerpo)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let t = v["token"].as_str().unwrap().to_owned();
    assert!(!b.vale(&t0).await);
    assert_eq!(motivo(&pool, b.sid(&t0)).await.as_deref(), Some("MFA_ACTIVADO"));
    let v = b.sesiones(&t).await;
    assert_eq!((ids(&v), v["sesiones"][0]["mfa"].as_bool()), (vec![b.sid(&t).to_string()], Some(true)));
    let (_, v) = b.llamar(MAC, "GET", "/api/v1/auth/security/preferences", Some(&t), None).await;
    assert_eq!(v["mfa_activo"], true);

    let nueva = "nueva-contraseña-456";
    let (s, v) = b.cambiar(&t, json!({ "password_actual": PASSWORD, "password_nueva": nueva })).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("CODIGO_MFA_REQUERIDO")), "{v}");
    assert_eq!(intentos(&pool, ana.cliente.id).await, 0);
    let secreto = totp.secreto("ana@example.mx").unwrap();
    let equivocado = if codigo_totp(&secreto, paso_totp_actual()) == "000000" { "111111" } else { "000000" };
    let cuerpo = json!({ "password_actual": PASSWORD, "password_nueva": nueva, "codigo": equivocado });
    let (s, v) = b.cambiar(&t, cuerpo).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("CODIGO_MFA_INVALIDO")), "{v}");
    assert_eq!(intentos(&pool, ana.cliente.id).await, 1);
    assert!(b.vale(&t).await, "un código equivocado no cierra la sesión");

    let cuerpo =
        json!({ "password_actual": PASSWORD, "password_nueva": nueva, "codigo": totp.codigo("ana@example.mx").await });
    let (s, v) = b.cambiar(&t, cuerpo.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["sesiones_revocadas"], 0);
    // El mismo código no sirve dos veces (ni para volver a cambiarla).
    let repetido =
        json!({ "password_actual": nueva, "password_nueva": "otra-mas-789-abc", "codigo": cuerpo["codigo"] });
    let (s, v) = b.cambiar(&t, repetido).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("CODIGO_MFA_INVALIDO")), "{v}");
}

#[sqlx::test(migrations = "../migrations")]
async fn agotar_los_intentos_al_cambiar_la_contrasena_bloquea_y_cierra_esa_sesion(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t = b.entrar(MAC, "ana@example.mx").await;
    let t_iphone = b.entrar(IPHONE, "ana@example.mx").await;
    let cuerpo = json!({ "password_actual": "adivinando-la-actual", "password_nueva": "nueva-contraseña-456" });
    for intento in 1..=5 {
        let (s, v) = b.cambiar(&t, cuerpo.clone()).await;
        assert_eq!(
            (s, codigo(&v)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("CONTRASENA_ACTUAL_INCORRECTA")),
            "intento {intento}: {v}"
        );
        assert_eq!(b.vale(&t).await, intento < 5, "intento {intento}");
    }
    assert_eq!(motivo(&pool, b.sid(&t)).await.as_deref(), Some("BLOQUEO"));
    assert!(b.vale(&t_iphone).await, "solo se cierra la sesión que agotó los intentos");
    let (s, v) = b.login(LINUX, "ana@example.mx", PASSWORD).await;
    assert_eq!((s, codigo(&v)), (StatusCode::LOCKED, Some("BLOQUEADO")), "{v}");
    // Bloqueada, otro intento desde otra sesión ni se verifica.
    let cuerpo = json!({ "password_actual": PASSWORD, "password_nueva": "nueva-contraseña-456" });
    let (s, v) = b.cambiar(&t_iphone, cuerpo).await;
    assert_eq!((s, codigo(&v)), (StatusCode::LOCKED, Some("BLOQUEADO")), "{v}");

    let eventos = b.eventos(&t_iphone).await;
    assert_eq!(de_tipo(&eventos, "CAMBIO_CONTRASENA_RECHAZADO").len(), 5, "{eventos:?}");
    assert_eq!(de_tipo(&eventos, "CUENTA_BLOQUEADA").len(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn los_inicios_fallidos_y_el_bloqueo_quedan_en_la_bitacora(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    for i in 0..5 {
        let (s, _) = b.login(LINUX, "ana@example.mx", &format!("equivocada-{i}-abcdef")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    // Durante el bloqueo ni se verifica ni se registra: la bitácora no crece sin límite.
    for _ in 0..3 {
        let (s, _) = b.login(LINUX, "ana@example.mx", "otra-equivocada-123").await;
        assert_eq!(s, StatusCode::LOCKED);
    }
    // Un correo que no existe no deja nada (no hay a quién).
    let (s, _) = b.login(LINUX, "nadie@example.mx", PASSWORD).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE kyc.credenciales SET bloqueado_hasta = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let t = b.entrar(MAC, "ana@example.mx").await;

    let eventos = b.eventos(&t).await;
    let nombres: Vec<_> = eventos.iter().map(|e| e["evento"].as_str().unwrap()).collect();
    assert_eq!(
        nombres,
        ["INICIO_SESION", "CUENTA_BLOQUEADA"].into_iter().chain(["INICIO_FALLIDO"; 5]).collect::<Vec<_>>()
    );
    let fallido = de_tipo(&eventos, "INICIO_FALLIDO")[0];
    assert_eq!(
        (fallido["dispositivo"].as_str(), fallido["ip"].as_str(), fallido["sesion_actual"].as_bool()),
        (Some("Firefox on Linux"), Some("192.0.2.33"), Some(false))
    );
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM kyc.eventos_autenticacion").fetch_one(&pool).await.unwrap();
    assert_eq!(total, 7, "solo los de Ana");
}

// -----------------------------------------------------------------------------
// Bitácora y personal
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn la_bitacora_se_pagina_y_el_personal_tambien_tiene_sesiones(pool: PgPool) {
    let b = banco(&pool).await;
    cliente_sin_kyc(&pool, &b.estado.config, "Ana").await;
    let t = b.entrar(MAC, "ana@example.mx").await;
    b.entrar(IPHONE, "ana@example.mx").await;
    for (consulta, esperado) in [
        ("?limite=1", StatusCode::OK),
        ("?limite=0", StatusCode::BAD_REQUEST),
        ("?limite=101", StatusCode::BAD_REQUEST),
        ("?otro=1", StatusCode::BAD_REQUEST),
    ] {
        let ruta = format!("/api/v1/auth/security/events{consulta}");
        let (s, v) = b.llamar(MAC, "GET", &ruta, Some(&t), None).await;
        assert_eq!(s, esperado, "{consulta}: {v}");
        if s == StatusCode::OK {
            assert_eq!(v["eventos"].as_array().unwrap().len(), 1, "{v}");
            assert_eq!(v["eventos"][0]["dispositivo"], "Safari on iPhone", "el más reciente");
        }
    }
    for ruta in ["/api/v1/auth/sessions", "/api/v1/auth/security/events", "/api/v1/auth/security/preferences"] {
        let (s, _) = b.llamar(MAC, "GET", ruta, None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{ruta} sin token");
    }

    // El personal interno abre sesiones con el segundo factor verificado.
    let totp = Autenticadora::default();
    let (_, email) = oficial(&pool, "Ofelia").await;
    let sesion = iniciar_sesion(&b.app, &totp, &email, PASSWORD_INTERNO).await;
    let token = sesion["token"].as_str().unwrap();
    let v = b.sesiones(token).await;
    assert_eq!((ids(&v).len(), v["sesiones"][0]["mfa"].as_bool()), (1, Some(true)), "{v}");
    let eventos = b.eventos(token).await;
    for evento in ["MFA_SECRETO_GENERADO", "MFA_ACTIVADO", "INICIO_SESION"] {
        assert_eq!(de_tipo(&eventos, evento).len(), 1, "{evento}: {eventos:?}");
    }
}
