//! Beneficiarios de la cuenta (migración 0020): la API, sus reglas, la
//! concurrencia y la cadena de versiones que el backend recalcula.
//! `tests/db/0020_beneficiarios.sql` cubre lo que garantiza la base de datos
//! por sí sola.

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
use synthetic_capital::{
    db::{self, Rol},
    error::AppError,
    kyc::curp::digito_verificador,
    models::kyc::RolUsuario,
    routes, AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

const MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
                   Chrome/126.0.0.0 Safari/537.36";

struct Banco {
    app: Router,
    estado: AppState,
}

async fn banco(pool: &PgPool) -> Banco {
    let mut cfg = preparar(pool).await;
    cfg.http.proxy_confiable = true;
    let estado = AppState::nuevo(pool.clone(), cfg);
    Banco { app: routes::router(estado.clone()), estado }
}

impl Banco {
    async fn llamar(&self, metodo: &str, token: &str, cuerpo: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(metodo)
            .uri("/api/v1/kyc/beneficiarios")
            .header("authorization", format!("Bearer {token}"))
            .header("user-agent", MAC)
            .header("x-forwarded-for", "203.0.113.7");
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

    async fn leer(&self, token: &str) -> Value {
        let (s, v) = self.llamar("GET", token, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn guardar(&self, token: &str, version_base: i64, beneficiarios: Value) -> (StatusCode, Value) {
        self.llamar("POST", token, Some(json!({ "beneficiarios": beneficiarios, "version_base": version_base }))).await
    }
}

/// Una CURP válida (estructura, fecha, entidad y dígito verificador).
fn curp(base17: &str) -> String {
    format!("{base17}{}", digito_verificador(base17).unwrap())
}

fn codigo(v: &Value) -> &str {
    v["error"]["codigo"].as_str().unwrap_or_default()
}

fn hija() -> Value {
    json!({ "nombre_completo": "maría lópez   rojas", "parentesco": "HIJO",
            "curp": curp("LORM900101MDFPJR0"), "fecha_nacimiento": "1990-01-01", "porcentaje": "60" })
}

fn hijo(porcentaje: &str) -> Value {
    json!({ "nombre_completo": "José López Rojas", "parentesco": "HIJO", "fecha_nacimiento": "2005-06-07",
            "porcentaje": porcentaje })
}

#[sqlx::test(migrations = "../migrations")]
async fn designar_por_versiones_con_su_cadena(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.estado.config, "Ana").await;
    let token = token_de(&b.estado, ana.cliente.id, RolUsuario::Cliente, false).await;

    let v = b.leer(&token).await;
    assert_eq!(
        (&v["version"], &v["vigente"], &v["puede_designar"], &v["maximo"], &v["cadena_verificada"]),
        (&json!(0), &Value::Null, &json!(true), &json!(5), &json!(true)),
        "{v}"
    );
    assert_eq!(v["parentescos"].as_array().unwrap().len(), 8);

    // Versión 1: dos hijos, 60% y 40%.
    let (s, v) = b.guardar(&token, 0, json!([hija(), hijo("40.00")])).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let vigente = &v["vigente"];
    assert_eq!((&v["version"], &vigente["version"], &v["cadena_verificada"]), (&json!(1), &json!(1), &json!(true)));
    let nombres: Vec<&str> =
        vigente["beneficiarios"].as_array().unwrap().iter().map(|b| b["nombre_completo"].as_str().unwrap()).collect();
    assert_eq!(nombres, ["MARÍA LÓPEZ ROJAS", "JOSÉ LÓPEZ ROJAS"], "nombres legales normalizados");
    assert_eq!(
        (&vigente["beneficiarios"][0]["porcentaje"], &vigente["beneficiarios"][1]["porcentaje"]),
        (&json!("60.00"), &json!("40.00"))
    );
    assert_eq!(
        (&vigente["dispositivo"], &vigente["ip"], &vigente["hash_anterior"]),
        (&json!("Chrome on macOS"), &json!("203.0.113.7"), &json!("0".repeat(64)))
    );
    let hash_v1 = vigente["hash"].as_str().unwrap().to_owned();

    // Versión 2: tres, 33.34 + 33.33 + 33.33; encadenada a la 1.
    let tercero = json!({ "nombre_completo": "Pedro Rojas Díaz", "parentesco": "CONYUGE",
                          "fecha_nacimiento": "1984-02-29", "porcentaje": "33.33" });
    let mut uno = hija();
    uno["porcentaje"] = json!("33.34");
    let (s, v) = b.guardar(&token, 1, json!([uno, hijo("33.33"), tercero])).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((&v["version"], &v["vigente"]["hash_anterior"]), (&json!(2), &json!(hash_v1)));
    let historial: Vec<i64> =
        v["historial"].as_array().unwrap().iter().map(|h| h["version"].as_i64().unwrap()).collect();
    assert_eq!(historial, [2, 1], "el historial, la más reciente primero");

    // Editar sobre una versión vieja (otra sesión guardó antes): 409, nada cambia.
    let (s, v) = b.guardar(&token, 1, json!([hijo("100")])).await;
    assert_eq!((s, codigo(&v)), (StatusCode::CONFLICT, "DESIGNACION_DESACTUALIZADA"), "{v}");

    // Retirar la designación: una versión vacía.
    let (s, v) = b.guardar(&token, 2, json!([])).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((&v["version"], v["vigente"]["beneficiarios"].as_array().unwrap().len()), (&json!(3), 0));

    let primera_mala: Option<i32> = sqlx::query_scalar("SELECT kyc.verificar_designaciones($1)")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(primera_mala, None, "la cadena de la base de datos cuadra también");
}

#[sqlx::test(migrations = "../migrations")]
async fn las_reglas_de_una_designacion(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.estado.config, "Ana").await;
    let token = token_de(&b.estado, ana.cliente.id, RolUsuario::Cliente, false).await;
    let propia = ana.cliente.curp.clone().expect("Ana tiene CURP");

    let casos: Vec<(&str, Value, StatusCode, &str)> = vec![
        ("no suman 100", json!([hija(), hijo("30")]), StatusCode::UNPROCESSABLE_ENTITY, "BENEFICIARIOS_PORCENTAJE"),
        ("pasan de 100", json!([hija(), hijo("50")]), StatusCode::UNPROCESSABLE_ENTITY, "BENEFICIARIOS_PORCENTAJE"),
        (
            "seis",
            json!((0..6).map(|_| hijo("10")).collect::<Vec<_>>()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "BENEFICIARIOS_MAXIMO",
        ),
        ("a milésimos", json!([hija(), hijo("39.999")]), StatusCode::BAD_REQUEST, "VALIDACION"),
        (
            "un número JSON",
            json!([{ "nombre_completo": "José López", "parentesco": "HIJO",
                                     "fecha_nacimiento": "2005-06-07", "porcentaje": 100 }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "sin apellido",
            json!([{ "nombre_completo": "José", "parentesco": "HIJO",
                                   "fecha_nacimiento": "2005-06-07", "porcentaje": "100" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "sin CURP ni fecha",
            json!([{ "nombre_completo": "José López", "parentesco": "HIJO", "porcentaje": "100" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "CURP con otro dígito",
            json!([{ "nombre_completo": "María López", "parentesco": "HIJO",
                                           "curp": format!("{}{}", &curp("LORM900101MDFPJR0")[..17],
                                               if curp("LORM900101MDFPJR0").ends_with('0') { '1' } else { '0' }),
                                           "porcentaje": "100" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "la fecha no es la de la CURP",
            json!([{ "nombre_completo": "María López", "parentesco": "HIJO",
                                                   "curp": curp("LORM900101MDFPJR0"), "fecha_nacimiento": "1991-01-01",
                                                   "porcentaje": "100" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "nace mañana",
            json!([{ "nombre_completo": "José López", "parentesco": "HIJO",
                                  "fecha_nacimiento": "2099-01-01", "porcentaje": "100" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "el titular",
            json!([{ "nombre_completo": "Ana Prueba", "parentesco": "OTRO", "curp": propia,
                                 "porcentaje": "100" }]),
            StatusCode::UNPROCESSABLE_ENTITY,
            "BENEFICIARIO_TITULAR",
        ),
        (
            "la misma CURP dos veces",
            json!([hija(), { "nombre_completo": "María L. Rojas", "parentesco": "HIJO",
                                                      "curp": curp("LORM900101MDFPJR0"), "porcentaje": "40" }]),
            StatusCode::UNPROCESSABLE_ENTITY,
            "BENEFICIARIO_DUPLICADO",
        ),
        (
            "parentesco fuera del catálogo",
            json!([{ "nombre_completo": "José López", "parentesco": "VECINO",
                                                    "fecha_nacimiento": "2005-06-07", "porcentaje": "100" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
        (
            "campo desconocido",
            json!([{ "nombre_completo": "José López", "parentesco": "HIJO",
                                        "fecha_nacimiento": "2005-06-07", "porcentaje": "100", "telefono": "5555" }]),
            StatusCode::BAD_REQUEST,
            "VALIDACION",
        ),
    ];
    for (caso, beneficiarios, status, esperado) in casos {
        let (s, v) = b.guardar(&token, 0, beneficiarios).await;
        assert_eq!((s, codigo(&v)), (status, esperado), "{caso}: {v}");
    }
    assert_eq!(b.leer(&token).await["version"], 0, "nada se guardó");
}

#[sqlx::test(migrations = "../migrations")]
async fn solo_un_cliente_verificado_designa_y_cada_quien_ve_lo_suyo(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.estado.config, "Ana").await;
    let beto = cliente_sin_kyc(&pool, &b.estado.config, "Beto").await;
    let carla = cliente(&pool, &b.estado.config, "Carla").await;
    let t_ana = token_de(&b.estado, ana.cliente.id, RolUsuario::Cliente, false).await;
    let t_beto = token_de(&b.estado, beto.cliente.id, RolUsuario::Cliente, false).await;
    let t_carla = token_de(&b.estado, carla.cliente.id, RolUsuario::Cliente, false).await;

    assert_eq!(b.leer(&t_beto).await["puede_designar"], false);
    let (s, v) = b.guardar(&t_beto, 0, json!([hijo("100")])).await;
    assert_eq!((s, codigo(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "KYC_NO_VERIFICADO"), "{v}");

    let (s, _) = b.guardar(&t_ana, 0, json!([hijo("100")])).await;
    assert_eq!(s, StatusCode::CREATED);
    let de_carla = b.leer(&t_carla).await;
    assert_eq!((&de_carla["version"], &de_carla["vigente"]), (&json!(0), &Value::Null), "Carla no ve lo de Ana");

    // El personal no tiene beneficiarios que designar aquí.
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&b.estado, oficial_id).await;
    let t_oficial = token_de(&b.estado, oficial_id, RolUsuario::Cumplimiento, true).await;
    let (s, _) = b.llamar("GET", &t_oficial, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../migrations")]
async fn la_base_de_datos_repite_la_suma_aunque_el_backend_fallara(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    // Como si el backend no validara: 60% + 30%, declarado como 100%.
    let mut tx = db::begin(&pool, Rol::Kyc).await.unwrap();
    let id = Uuid::new_v4();
    for (posicion, porcentaje) in [(1i16, "60"), (2, "30")] {
        sqlx::query(
            "INSERT INTO kyc.beneficiarios (designacion_id, posicion, nombre_completo, parentesco, fecha_nacimiento, porcentaje) \
             VALUES ($1, $2, 'JOSÉ LÓPEZ', 'HIJO', '2005-06-07', $3::numeric)",
        )
        .bind(id)
        .bind(posicion)
        .bind(porcentaje)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO kyc.designaciones_beneficiarios (id, cliente_id, num_beneficiarios, total_porcentaje) \
         VALUES ($1, $2, 2, 100)",
    )
    .bind(id)
    .bind(ana.cliente.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let e = AppError::from(tx.commit().await.unwrap_err());
    assert_eq!((e.codigo(), e.status()), ("BENEFICIARIOS_PORCENTAJE", StatusCode::UNPROCESSABLE_ENTITY), "{e:?}");
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM kyc.designaciones_beneficiarios").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0, "nada quedó");
}

#[sqlx::test(migrations = "../migrations")]
async fn una_alteracion_por_fuera_rompe_la_cadena(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.estado.config, "Ana").await;
    let token = token_de(&b.estado, ana.cliente.id, RolUsuario::Cliente, false).await;
    let (s, _) = b.guardar(&token, 0, json!([hija(), hijo("40")])).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(b.leer(&token).await["cadena_verificada"], true);

    // El dueño del esquema desactiva el trigger y cambia un porcentaje.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE kyc.beneficiarios DISABLE TRIGGER trg_beneficiarios_inmutable")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE kyc.beneficiarios SET porcentaje = 70 WHERE posicion = 1").execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE kyc.beneficiarios SET porcentaje = 30 WHERE posicion = 2").execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE kyc.beneficiarios ENABLE TRIGGER trg_beneficiarios_inmutable")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let v = b.leer(&token).await;
    assert_eq!(v["cadena_verificada"], false, "la suma sigue en 100, pero el hash ya no cuadra: {v}");
}
