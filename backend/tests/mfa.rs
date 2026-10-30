//! Segundo factor (TOTP), restablecimiento de contraseña y sesiones del
//! personal (migración 0010).
//!
//! Reglas de la tarea: (1) el personal de cumplimiento (y todo el personal
//! interno) no llega a `/api/v1/aml/*` sin una sesión que haya verificado el
//! segundo factor; (2) el secreto TOTP nunca está en claro en la base de datos.

mod comun;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use comun::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    crypto::cifrado::{Cifrador, Clave},
    db::Rol,
    models::kyc::RolUsuario,
    routes, AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

struct Banco {
    app: Router,
    estado: AppState,
    totp: Autenticadora,
}

async fn banco(pool: &PgPool) -> Banco {
    let cfg = preparar(pool).await;
    let estado = AppState::nuevo(pool.clone(), cfg);
    Banco { app: routes::router(estado.clone()), estado, totp: Autenticadora::default() }
}

impl Banco {
    async fn llamar(
        &self,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
    ) -> (StatusCode, Value) {
        llamar_json(&self.app, metodo, ruta, token, cuerpo).await
    }

    /// Primer paso del inicio de sesión (la respuesta tal cual).
    async fn login(&self, email: &str, password: &str) -> (StatusCode, Value) {
        self.llamar("POST", "/api/v1/auth/login", None, Some(json!({ "email": email, "password": password }))).await
    }

    async fn verificar(&self, token_mfa: &str, codigo: &str) -> (StatusCode, Value) {
        self.llamar("POST", "/api/v1/auth/verify-mfa", None, Some(json!({ "token_mfa": token_mfa, "codigo": codigo })))
            .await
    }

    async fn olvido(&self, email: &str) -> (StatusCode, Value) {
        self.llamar("POST", "/api/v1/auth/password/forgot", None, Some(json!({ "email": email }))).await
    }

    async fn restablecer(&self, token: &str, password: &str) -> (StatusCode, Value) {
        self.llamar("POST", "/api/v1/auth/password/reset", None, Some(json!({ "token": token, "password": password })))
            .await
    }

    /// Cliente nuevo (`<nombre>@example.mx`): su token de sesión y su email.
    async fn cliente(&self, nombre: &str) -> (String, String) {
        let email = format!("{}@example.mx", nombre.to_lowercase());
        let (s, r) = self
            .llamar(
                "POST",
                "/api/v1/auth/registro",
                None,
                Some(json!({
                    "tipo_persona": "FISICA",
                    "nombre": nombre,
                    "apellido_paterno": "Prueba",
                    "email": email,
                    "password": "contraseña-segura-123"
                })),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{r}");
        (r["sesion"]["token"].as_str().unwrap().to_string(), email)
    }

    /// Token de inscripción del personal sin segundo factor.
    async fn inscripcion(&self, email: &str) -> String {
        let (s, r) = self.login(email, PASSWORD_INTERNO).await;
        assert_eq!((s, r["estado"].as_str()), (StatusCode::OK, Some("MFA_ENROLLMENT_REQUIRED")), "{r}");
        r["token_inscripcion"].as_str().unwrap().to_string()
    }

    /// Desafío TOTP del personal con segundo factor.
    async fn desafio(&self, email: &str, password: &str) -> String {
        let (s, r) = self.login(email, password).await;
        assert_eq!((s, r["estado"].as_str()), (StatusCode::OK, Some("2FA_REQUIRED")), "{r}");
        assert!(r.get("token").is_none(), "un desafío nunca trae token de acceso: {r}");
        r["token_mfa"].as_str().unwrap().to_string()
    }
}

fn codigo_de_error(v: &Value) -> Option<&str> {
    v["error"]["codigo"].as_str()
}

/// Código válido del paso actual, sin marcarlo como usado: para peticiones que
/// el servidor rechaza antes de mirar el código.
fn codigo_actual(secreto: &[u8]) -> String {
    codigo_totp(secreto, paso_totp_actual())
}

/// Un código de 6 dígitos que no es válido para ninguno de los pasos tolerados.
fn codigo_incorrecto(secreto: &[u8]) -> String {
    let actual = paso_totp_actual();
    let validos: Vec<String> = (actual - 2..=actual + 2).map(|p| codigo_totp(secreto, p)).collect();
    (0..1_000_000u32).map(|n| format!("{n:06}")).find(|c| !validos.contains(c)).unwrap()
}

/// Rutas de la consola PLD, de lectura y de escritura (sin cuerpo: la capa
/// de la consola responde antes de leerlo).
fn rutas_aml() -> Vec<(&'static str, String)> {
    let id = Uuid::new_v4();
    vec![
        ("GET", "/api/v1/aml/resumen".into()),
        ("GET", "/api/v1/aml/alertas".into()),
        ("GET", format!("/api/v1/aml/alertas/{id}")),
        ("GET", format!("/api/v1/aml/clientes/{id}")),
        ("GET", "/api/v1/aml/reglas".into()),
        ("GET", "/api/v1/aml/bitacora/verificacion".into()),
        ("POST", format!("/api/v1/aml/alertas/{id}/resolver")),
        ("POST", format!("/api/v1/aml/clientes/{id}/congelar")),
        ("POST", "/api/v1/aml/propuestas".into()),
        ("POST", format!("/api/v1/aml/solicitudes/{id}/decision")),
    ]
}

async fn eventos(pool: &PgPool, cliente_id: Uuid) -> Vec<(String, String)> {
    sqlx::query_as("SELECT evento, registrado_por FROM kyc.eventos_autenticacion WHERE cliente_id = $1 ORDER BY id")
        .bind(cliente_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Contexto (AAD) del cifrado de un secreto TOTP: forma parte del formato
/// guardado, así que cambiarlo dejaría ilegibles los secretos existentes.
fn contexto(cliente_id: Uuid) -> Vec<u8> {
    format!("synthetic-capital/totp/v1/{cliente_id}").into_bytes()
}

// -----------------------------------------------------------------------------
// Regla 1: sin segundo factor verificado no hay consola PLD
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn oficial_sin_segundo_factor_no_entra_a_la_consola_pld(pool: PgPool) {
    let b = banco(&pool).await;
    let (id, email) = oficial(&pool, "Ofelia").await;

    // La contraseña sola no da una sesión: el personal inscribe primero el segundo factor.
    let (s, r) = b.login(&email, PASSWORD_INTERNO).await;
    assert_eq!((s, r["estado"].as_str()), (StatusCode::OK, Some("MFA_ENROLLMENT_REQUIRED")), "{r}");
    assert!(r.get("token").is_none(), "sin token de acceso: {r}");
    let inscripcion = r["token_inscripcion"].as_str().unwrap();

    // Ni el token de inscripción, ni un token de acceso sin el claim `mfa`, ni
    // uno que diga `mfa: true` sin el factor activo en la base de datos.
    let sin_mfa = b.estado.jwt.emitir(id, RolUsuario::Cumplimiento, false).unwrap().0;
    let falsificado = b.estado.jwt.emitir(id, RolUsuario::Cumplimiento, true).unwrap().0;
    for (metodo, ruta) in rutas_aml() {
        let (s, v) = b.llamar(metodo, &ruta, Some(inscripcion), None).await;
        assert_eq!(
            (s, codigo_de_error(&v)),
            (StatusCode::UNAUTHORIZED, Some("NO_AUTENTICADO")),
            "{metodo} {ruta}: {v}"
        );
        for token in [&sin_mfa, &falsificado] {
            let (s, v) = b.llamar(metodo, &ruta, Some(token), None).await;
            assert_eq!(
                (s, codigo_de_error(&v)),
                (StatusCode::FORBIDDEN, Some("MFA_REQUERIDO")),
                "{metodo} {ruta}: {v}"
            );
        }
    }
    let (s, _) = b.llamar("GET", "/api/v1/yo", Some(inscripcion), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "el token de inscripción no sirve fuera de la inscripción");

    // Lo mismo para el resto del personal: el auditor en la consola, el operador en las operaciones.
    let (aud, _) = auditor(&pool, "Aurelio").await;
    let token = b.estado.jwt.emitir(aud, RolUsuario::Auditor, false).unwrap().0;
    let (s, v) = b.llamar("GET", "/api/v1/aml/alertas", Some(&token), None).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::FORBIDDEN, Some("MFA_REQUERIDO")), "{v}");
    bootstrap::asegurar_operador(&pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    let op: Uuid = sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = 'operador@synthetic.mx'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let token = b.estado.jwt.emitir(op, RolUsuario::Operador, false).unwrap().0;
    let (s, v) = b.llamar("GET", "/api/v1/operaciones/creditos", Some(&token), None).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::FORBIDDEN, Some("MFA_REQUERIDO")), "{v}");

    // Con el segundo factor inscrito y verificado, la consola responde.
    let sesion = iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;
    let token = sesion["token"].as_str().unwrap();
    assert!(b.estado.jwt.validar(token).unwrap().mfa);
    let (s, v) = b.llamar("GET", "/api/v1/aml/resumen", Some(token), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

// -----------------------------------------------------------------------------
// Inscripción y regla 2: el secreto solo existe cifrado
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn la_inscripcion_guarda_el_secreto_cifrado_y_activa_con_el_primer_codigo(pool: PgPool) {
    let b = banco(&pool).await;
    let (id, email) = oficial(&pool, "Olga").await;
    let inscripcion = b.inscripcion(&email).await;

    // setup: secreto, URL otpauth y QR; la respuesta no se guarda en cachés.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/mfa/setup")
        .header("authorization", format!("Bearer {inscripcion}"))
        .body(Body::empty())
        .unwrap();
    let resp = b.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    let conf: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let base32 = conf["secreto"].as_str().unwrap();
    assert_eq!(base32.len(), 32, "160 bits en base32: {conf}");
    let url = conf["otpauth_url"].as_str().unwrap();
    assert!(url.starts_with("otpauth://totp/Synthetic%20Capital:olga%40cumplimiento.mx?"), "{url}");
    assert!(url.contains(&format!("secret={base32}")) && url.contains("issuer=Synthetic%20Capital"), "{url}");
    assert!(conf["qr_svg"].as_str().unwrap().starts_with("data:image/svg+xml;base64,"));
    assert_eq!((conf["digitos"].as_u64(), conf["periodo_seg"].as_u64()), (Some(6), Some(30)));

    // En la base de datos: solo el sobre AES-256-GCM, ligado a su dueño.
    let (sobre, activo): (String, bool) =
        sqlx::query_as("SELECT totp_secret, mfa_enabled FROM kyc.credenciales WHERE cliente_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!activo, "el factor no está activo hasta verificar el primer código");
    assert!(sobre.starts_with("v1:") && !sobre.contains(base32), "{sobre}");
    let secreto = secreto_base32(base32);
    let cifrador = Cifrador::nuevo(b.estado.config.mfa.clave.as_ref().unwrap(), None);
    assert_eq!(&cifrador.descifrar(&sobre, &contexto(id)).unwrap().texto[..], &secreto[..]);
    assert!(cifrador.descifrar(&sobre, &contexto(Uuid::new_v4())).is_err(), "en la fila de otra persona no se abre");

    // enable: un código incorrecto no activa nada y cuenta como intento.
    let (s, v) = b
        .llamar(
            "POST",
            "/api/v1/auth/mfa/enable",
            Some(&inscripcion),
            Some(json!({ "codigo": codigo_incorrecto(&secreto) })),
        )
        .await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("CODIGO_MFA_INVALIDO")), "{v}");
    let (s, v) =
        b.llamar("POST", "/api/v1/auth/mfa/enable", Some(&inscripcion), Some(json!({ "codigo": "12345" }))).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{v}");

    // El primer código correcto activa el factor y entrega la sesión.
    b.totp.guardar(&email, secreto.clone());
    let codigo = b.totp.codigo(&email).await;
    let (s, sesion) =
        b.llamar("POST", "/api/v1/auth/mfa/enable", Some(&inscripcion), Some(json!({ "codigo": codigo }))).await;
    assert_eq!(
        (s, sesion["estado"].as_str(), sesion["rol"].as_str()),
        (StatusCode::OK, Some("OK"), Some("CUMPLIMIENTO"))
    );
    let token = sesion["token"].as_str().unwrap();
    let (s, _) = b.llamar("GET", "/api/v1/aml/alertas", Some(token), None).await;
    assert_eq!(s, StatusCode::OK);

    // Ya activo: no se vuelve a generar (cambiar de dispositivo pasa por soporte).
    let (s, v) = b.llamar("POST", "/api/v1/auth/mfa/setup", Some(token), None).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::CONFLICT, Some("MFA_YA_ACTIVO")), "{v}");
    let (s, v) = b
        .llamar("POST", "/api/v1/auth/mfa/enable", Some(token), Some(json!({ "codigo": codigo_actual(&secreto) })))
        .await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::CONFLICT, Some("MFA_YA_ACTIVO")), "{v}");

    let ev: Vec<String> = eventos(&pool, id).await.into_iter().map(|(e, quien)| format!("{e}/{quien}")).collect();
    assert_eq!(
        ev,
        ["MFA_SECRETO_GENERADO/sc_auth", "MFA_CODIGO_RECHAZADO/sc_auth", "MFA_ACTIVADO/sc_auth"],
        "la bitácora registra la inscripción"
    );
}

// -----------------------------------------------------------------------------
// Inicio de sesión con segundo factor
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn con_segundo_factor_el_login_exige_el_codigo_y_no_acepta_repetirlo(pool: PgPool) {
    let b = banco(&pool).await;
    let (id, email) = oficial(&pool, "Octavio").await;
    let inscripcion = b.inscripcion(&email).await;
    iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;

    let desafio = b.desafio(&email, PASSWORD_INTERNO).await;
    // Un desafío no es un token de acceso ni de inscripción.
    for ruta in ["/api/v1/yo", "/api/v1/aml/resumen"] {
        let (s, _) = b.llamar("GET", ruta, Some(&desafio), None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{ruta}");
    }
    let (s, _) = b.llamar("POST", "/api/v1/auth/mfa/setup", Some(&desafio), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Y nada que no sea un desafío vigente sirve como desafío.
    let secreto = b.totp.secreto(&email).unwrap();
    let acceso = b.estado.jwt.emitir(id, RolUsuario::Cumplimiento, true).unwrap().0;
    for otro in [&inscripcion, &acceso, &"no-es-un-jwt".to_string()] {
        let (s, v) = b.verificar(otro, &codigo_actual(&secreto)).await;
        assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("DESAFIO_MFA_VENCIDO")), "{v}");
    }

    let (s, v) = b.verificar(&desafio, "12 34").await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{v}");
    let (s, v) = b.verificar(&desafio, &codigo_incorrecto(&secreto)).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("CODIGO_MFA_INVALIDO")), "{v}");

    let codigo = b.totp.codigo(&email).await;
    let (s, sesion) = b.verificar(&desafio, &codigo).await;
    assert_eq!((s, sesion["estado"].as_str()), (StatusCode::OK, Some("OK")), "{sesion}");
    let (s, _) = b.llamar("GET", "/api/v1/aml/resumen", Some(sesion["token"].as_str().unwrap()), None).await;
    assert_eq!(s, StatusCode::OK);

    // El mismo código no abre una segunda sesión (ni con un desafío nuevo).
    let otro = b.desafio(&email, PASSWORD_INTERNO).await;
    let (s, v) = b.verificar(&otro, &codigo).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("CODIGO_MFA_INVALIDO")), "{v}");
}

#[sqlx::test(migrations = "../migrations")]
async fn los_codigos_incorrectos_bloquean_la_cuenta_aunque_se_repita_la_contrasena(pool: PgPool) {
    let b = banco(&pool).await;
    let (id, email) = oficial(&pool, "Olivia").await;
    iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;
    let malo = codigo_incorrecto(&b.totp.secreto(&email).unwrap());

    // Cada contraseña cuenta hasta que se verifica el código: login (1), código
    // malo (2), login (3), código malo (4); el quinto intento bloquea la cuenta.
    for _ in 0..2 {
        let desafio = b.desafio(&email, PASSWORD_INTERNO).await;
        let (s, v) = b.verificar(&desafio, &malo).await;
        assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("CODIGO_MFA_INVALIDO")), "{v}");
    }
    let (s, v) = b.login(&email, PASSWORD_INTERNO).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::LOCKED, Some("BLOQUEADO")), "{v}");
    let (s, v) = b.login(&email, PASSWORD_INTERNO).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::LOCKED, Some("BLOQUEADO")), "{v}");

    // Un desafío emitido antes del bloqueo tampoco completa la sesión.
    sqlx::query("UPDATE kyc.credenciales SET bloqueado_hasta = NULL, intentos_fallidos = 0 WHERE cliente_id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let desafio = b.desafio(&email, PASSWORD_INTERNO).await;
    for _ in 0..4 {
        let (s, _) = b.verificar(&desafio, &malo).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, v) = b.verificar(&desafio, &codigo_actual(&b.totp.secreto(&email).unwrap())).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::LOCKED, Some("BLOQUEADO")), "{v}");

    let rechazos = eventos(&pool, id).await.iter().filter(|(e, _)| e == "MFA_CODIGO_RECHAZADO").count();
    assert_eq!(rechazos, 6);
}

#[sqlx::test(migrations = "../migrations")]
async fn la_sesion_del_personal_dura_menos_que_la_del_cliente(pool: PgPool) {
    let b = banco(&pool).await;
    let (_, email) = oficial(&pool, "Osvaldo").await;
    let personal = iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;
    b.cliente("Carla").await;
    let cliente = iniciar_sesion(&b.app, &b.totp, "carla@example.mx", "contraseña-segura-123").await;
    let minutos = |s: &Value| {
        let expira: chrono::DateTime<chrono::Utc> = s["expira_en"].as_str().unwrap().parse().unwrap();
        (expira - chrono::Utc::now()).num_minutes()
    };
    assert!((29..=30).contains(&minutos(&personal)), "{personal}");
    assert!((59..=60).contains(&minutos(&cliente)), "{cliente}");
}

#[sqlx::test(migrations = "../migrations")]
async fn un_cliente_puede_activar_el_segundo_factor_y_revoca_sus_otras_sesiones(pool: PgPool) {
    let b = banco(&pool).await;
    let (token, email) = b.cliente("Celia").await;
    let (s, conf) = b.llamar("POST", "/api/v1/auth/mfa/setup", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{conf}");
    b.totp.guardar(&email, secreto_base32(conf["secreto"].as_str().unwrap()));
    // (`iat` tiene resolución de segundos: la revocación se nota desde el siguiente.)
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let codigo = b.totp.codigo(&email).await;
    let (s, sesion) =
        b.llamar("POST", "/api/v1/auth/mfa/enable", Some(&token), Some(json!({ "codigo": codigo }))).await;
    assert_eq!((s, sesion["estado"].as_str()), (StatusCode::OK, Some("OK")), "{sesion}");

    let (s, _) = b.llamar("GET", "/api/v1/tablero", Some(&token), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "la sesión anterior quedó revocada");
    let (s, _) = b.llamar("GET", "/api/v1/tablero", Some(sesion["token"].as_str().unwrap()), None).await;
    assert_eq!(s, StatusCode::OK);
    // Desde ahora también el cliente contesta el desafío.
    b.desafio(&email, "contraseña-segura-123").await;
}

// -----------------------------------------------------------------------------
// Restablecimiento de contraseña
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn restablecer_la_contrasena_con_un_enlace_de_un_solo_uso(pool: PgPool) {
    let b = banco(&pool).await;
    let (token_viejo, email) = b.cliente("Rosa").await;
    let id: Uuid =
        sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = $1").bind(&email).fetch_one(&pool).await.unwrap();

    // La respuesta no revela si la cuenta existe.
    let nadie = b.olvido("nadie@example.mx").await;
    let rosa = b.olvido(&email).await;
    assert_eq!(nadie, rosa);
    assert_eq!(rosa.0, StatusCode::ACCEPTED);
    assert!(b.estado.correo.ultimo_para("nadie@example.mx").is_none());
    let correo = b.estado.correo.ultimo_para(&email).expect("correo simulado");
    let (base, token) = correo.enlace.split_once("#token=").unwrap();
    assert_eq!(base, "http://localhost:3000/auth/reset-password", "el token va en el fragmento, nunca en la URL");
    assert_eq!(token.len(), 43, "256 bits en base64url");

    // En la base de datos solo el hash, con 30 minutos de vigencia.
    let (guardado, vigencia_ok): (String, bool) = sqlx::query_as(
        "SELECT password_reset_token, token_expires_at BETWEEN now() + interval '29 minutes' AND now() + interval '31 minutes' \
         FROM kyc.credenciales WHERE cliente_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(guardado, hex::encode(Sha256::digest(token.as_bytes())));
    assert!(vigencia_ok);

    // Otro pedido antes de un minuto no manda otro correo.
    assert_eq!(b.olvido(&email).await.0, StatusCode::ACCEPTED);
    assert_eq!(b.estado.correo.cuantos_para(&email), 1);

    // Un token ajeno no sirve; una contraseña débil se rechaza sin gastar el token.
    let (s, v) = b.restablecer("token-inventado-por-alguien", "nueva-contraseña-456").await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("TOKEN_RESTABLECIMIENTO_INVALIDO")));
    let (s, v) = b.restablecer(token, "corta").await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::BAD_REQUEST, Some("VALIDACION")), "{v}");

    // (`iat` tiene resolución de segundos: la revocación se nota desde el siguiente.)
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (s, v) = b.restablecer(token, "nueva-contraseña-456").await;
    assert_eq!((s, v["estado"].as_str()), (StatusCode::OK, Some("OK")), "{v}");

    // Todas las sesiones anteriores quedan revocadas; vale la contraseña nueva.
    let (s, _) = b.llamar("GET", "/api/v1/tablero", Some(&token_viejo), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = b.login(&email, "contraseña-segura-123").await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("CREDENCIALES_INVALIDAS")));
    let (s, v) = b.login(&email, "nueva-contraseña-456").await;
    assert_eq!((s, v["estado"].as_str()), (StatusCode::OK, Some("OK")), "{v}");

    // Un solo uso.
    let (s, v) = b.restablecer(token, "otra-contraseña-789").await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("TOKEN_RESTABLECIMIENTO_INVALIDO")));

    let ev: Vec<String> = eventos(&pool, id).await.into_iter().map(|(e, _)| e).collect();
    assert_eq!(ev, ["RESTABLECIMIENTO_SOLICITADO", "CONTRASENA_RESTABLECIDA"]);
}

#[sqlx::test(migrations = "../migrations")]
async fn un_enlace_vencido_no_sirve_y_el_restablecimiento_desbloquea(pool: PgPool) {
    let b = banco(&pool).await;
    let (_, email) = b.cliente("Raquel").await;
    b.olvido(&email).await;
    let token = b.estado.correo.ultimo_para(&email).unwrap().enlace.split_once("#token=").unwrap().1.to_string();
    sqlx::query("UPDATE kyc.credenciales SET token_expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let (s, v) = b.restablecer(&token, "nueva-contraseña-456").await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, Some("TOKEN_RESTABLECIMIENTO_INVALIDO")));

    // Bloqueada por contraseñas equivocadas: un enlace nuevo la desbloquea.
    for _ in 0..5 {
        b.login(&email, "equivocada-123456").await;
    }
    let (s, _) = b.login(&email, "contraseña-segura-123").await;
    assert_eq!(s, StatusCode::LOCKED);
    b.olvido(&email).await;
    let nuevo = b.estado.correo.ultimo_para(&email).unwrap().enlace.split_once("#token=").unwrap().1.to_string();
    assert_ne!(nuevo, token);
    let (s, _) = b.restablecer(&nuevo, "nueva-contraseña-456").await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = b.login(&email, "nueva-contraseña-456").await;
    assert_eq!((s, v["estado"].as_str()), (StatusCode::OK, Some("OK")), "{v}");
}

#[sqlx::test(migrations = "../migrations")]
async fn restablecer_la_contrasena_no_quita_el_segundo_factor(pool: PgPool) {
    let b = banco(&pool).await;
    let (_, email) = oficial(&pool, "Rebeca").await;
    iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;
    let desafio_previo = b.desafio(&email, PASSWORD_INTERNO).await;

    b.olvido(&email).await;
    let token = b.estado.correo.ultimo_para(&email).unwrap().enlace.split_once("#token=").unwrap().1.to_string();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (s, _) = b.restablecer(&token, "nueva-contraseña-456").await;
    assert_eq!(s, StatusCode::OK);

    // El desafío emitido antes del cambio ya no completa una sesión...
    let (s, v) = b.verificar(&desafio_previo, &codigo_actual(&b.totp.secreto(&email).unwrap())).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::UNAUTHORIZED, Some("DESAFIO_MFA_VENCIDO")), "{v}");
    // ...y la contraseña nueva sigue pidiendo el código.
    let desafio = b.desafio(&email, "nueva-contraseña-456").await;
    let (s, v) = b.verificar(&desafio, &b.totp.codigo(&email).await).await;
    assert_eq!((s, v["estado"].as_str()), (StatusCode::OK, Some("OK")), "{v}");
}

// -----------------------------------------------------------------------------
// Base de datos: solo el dueño del esquema desactiva el factor; sc_kyc no lee secretos
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn solo_el_dueno_del_esquema_desactiva_el_segundo_factor(pool: PgPool) {
    let b = banco(&pool).await;
    let (id, email) = oficial(&pool, "Daniela").await;
    let sesion = iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;
    let token = sesion["token"].as_str().unwrap();

    let desactivar = format!("UPDATE kyc.credenciales SET mfa_enabled = FALSE WHERE cliente_id = '{id}'");
    let e = como(&pool, Rol::Auth, &desactivar).await.unwrap_err();
    assert!(e.contains("SC_MFA_SOLO_DUENO"), "{e}");
    let e = como(&pool, Rol::Kyc, "SELECT totp_secret FROM kyc.credenciales").await.unwrap_err();
    assert!(e.contains("permission denied"), "{e}");

    // El DBA (dueño del esquema) lo restablece, p. ej. por pérdida del dispositivo.
    sqlx::query(
        "UPDATE kyc.credenciales SET mfa_enabled = FALSE, totp_secret = NULL, totp_ultimo_paso = NULL WHERE cliente_id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(eventos(&pool, id).await.iter().any(|(e, _)| e == "MFA_DESACTIVADO"), "queda en la bitácora");
    // La sesión que tenía deja de abrir la consola, y el siguiente login vuelve a la inscripción.
    let (s, v) = b.llamar("GET", "/api/v1/aml/resumen", Some(token), None).await;
    assert_eq!((s, codigo_de_error(&v)), (StatusCode::FORBIDDEN, Some("MFA_REQUERIDO")), "{v}");
    b.inscripcion(&email).await;
}

// -----------------------------------------------------------------------------
// Rotación de la clave de cifrado
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn rotar_la_clave_vuelve_a_cifrar_los_secretos(pool: PgPool) {
    let b = banco(&pool).await;
    let (id, email) = oficial(&pool, "Rodrigo").await;
    iniciar_sesion(&b.app, &b.totp, &email, PASSWORD_INTERNO).await;
    let leer = || async {
        sqlx::query_scalar::<_, String>("SELECT totp_secret FROM kyc.credenciales WHERE cliente_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let antes = leer().await;

    // migrar con la clave nueva y la anterior: todo queda cifrado con la nueva.
    let vieja = b.estado.config.mfa.clave.clone().unwrap();
    let nueva = Clave(*b"otra-clave-de-pruebas-mfa-32-by!");
    let mut cfg = (*b.estado.config).clone();
    cfg.mfa.clave = Some(nueva.clone());
    cfg.mfa.clave_anterior = Some(vieja.clone());
    bootstrap::ejecutar(&pool, &cfg).await.unwrap();
    let despues = leer().await;
    assert_ne!(antes, despues);
    assert!(Cifrador::nuevo(&nueva, None).descifrar(&despues, &contexto(id)).is_ok());
    assert!(Cifrador::nuevo(&vieja, None).descifrar(&despues, &contexto(id)).is_err());

    // Retirada la clave anterior, el servicio sigue verificando los códigos.
    cfg.mfa.clave_anterior = None;
    let estado = AppState::nuevo(pool.clone(), cfg);
    let app = routes::router(estado);
    let (s, r) = llamar_json(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(json!({ "email": email, "password": PASSWORD_INTERNO })),
    )
    .await;
    assert_eq!((s, r["estado"].as_str()), (StatusCode::OK, Some("2FA_REQUIRED")), "{r}");
    let (s, v) = llamar_json(
        &app,
        "POST",
        "/api/v1/auth/verify-mfa",
        None,
        Some(json!({ "token_mfa": r["token_mfa"], "codigo": b.totp.codigo(&email).await })),
    )
    .await;
    assert_eq!((s, v["estado"].as_str()), (StatusCode::OK, Some("OK")), "{v}");
}
