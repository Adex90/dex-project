//! Login de la aplicación que aprovisiona `synthetic-capital migrar`: el
//! verificador SCRAM calculado en Rust es válido (se entra con la contraseña en
//! claro), el login es NOINHERIT, puede cambiar a cada rol sc_* sin heredar sus
//! privilegios, repetir rota la contraseña y cualquier desvío falla cerrado.

mod comun;

use std::time::Duration;

use comun::*;
use sqlx::{postgres::PgPoolOptions, Executor, PgPool};
use synthetic_capital::{
    bootstrap,
    config::LoginAplicacion,
    db::{self, Rol},
};

/// Contraseña aleatoria como la de `openssl rand -hex 24`.
fn password() -> String {
    hex::encode(rand::random::<[u8; 24]>())
}

async fn asegurar(pool: &PgPool, usuario: &str, password: &str) -> anyhow::Result<()> {
    bootstrap::asegurar_login_aplicacion(pool, &LoginAplicacion::nuevo(usuario, password).unwrap()).await
}

/// Conexión nueva por TCP con usuario y contraseña (pg_hba: scram-sha-256).
async fn conectar(pool: &PgPool, usuario: &str, password: &str) -> Result<PgPool, sqlx::Error> {
    let opciones = pool.connect_options().as_ref().clone().username(usuario).password(password);
    PgPoolOptions::new().max_connections(2).acquire_timeout(Duration::from_secs(5)).connect_with(opciones).await
}

/// (puede cambiar con SET ROLE, hereda privilegios) del login sobre `rol`.
async fn relacion(pool: &PgPool, usuario: &str, rol: &str) -> (bool, bool) {
    let cambiar = if version(pool).await >= 160_000 { "SET" } else { "MEMBER" };
    sqlx::query_as("SELECT pg_has_role($1, $2, $3), pg_has_role($1, $2, 'USAGE')")
        .bind(usuario)
        .bind(rol)
        .bind(cambiar)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn version(pool: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT current_setting('server_version_num')::int").fetch_one(pool).await.unwrap()
}

/// (rolinherit, algún atributo de administración).
async fn atributos(pool: &PgPool, usuario: &str) -> (bool, bool) {
    sqlx::query_as(
        "SELECT rolinherit, rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication OR rolbypassrls \
           FROM pg_roles WHERE rolname = $1",
    )
    .bind(usuario)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn assert_solo_set_role(pool: &PgPool, usuario: &str) {
    for rol in Rol::TODOS {
        assert_eq!(relacion(pool, usuario, rol.nombre()).await, (true, false), "{usuario} con {}", rol.nombre());
    }
    let pld: bool = sqlx::query_scalar("SELECT pg_has_role($1, 'sc_auditor_pld', 'MEMBER')")
        .bind(usuario)
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(!pld, "sc_auditor_pld es para un verificador externo, no para el servicio");
}

#[sqlx::test(migrations = "../migrations")]
async fn el_login_entra_con_la_contrasena_y_solo_actua_con_set_role(pool: PgPool) {
    let login = LoginTemporal::nuevo(&pool);
    let usuario = login.usuario.as_str();
    let cfg = preparar(&pool).await;
    let pw = password();
    asegurar(&pool, usuario, &pw).await.unwrap();

    // El servidor verifica contraseñas por TCP: una incorrecta no entra...
    assert!(conectar(&pool, usuario, &password()).await.is_err(), "pg_hba debe exigir contraseña");
    // ...y la correcta sí: el verificador SCRAM calculado en Rust es válido.
    let app = conectar(&pool, usuario, &pw).await.expect("entrar con la contraseña en claro");

    assert_eq!(atributos(&pool, usuario).await, (false, false), "NOINHERIT y sin atributos de administración");
    assert_solo_set_role(&pool, usuario).await;

    // Sin SET ROLE no tiene privilegios; con db::begin (SET LOCAL ROLE) opera.
    let e = sqlx::query("SELECT 1 FROM ifpe.cuentas").execute(&app).await.unwrap_err().to_string();
    assert!(e.contains("permission denied"), "{e}");
    db::verificar_usuario_de_aplicacion(&app).await.unwrap();
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&app, &cfg, ana.cuenta_ifpe.id, "100").await;
    let mut tx = db::begin_lectura(&app, Rol::Ifpe).await.unwrap();
    let rol: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(rol, "sc_ifpe");
    tx.rollback().await.unwrap();
    app.close().await;
}

#[sqlx::test(migrations = "../migrations")]
async fn repetir_rota_la_contrasena(pool: PgPool) {
    let login = LoginTemporal::nuevo(&pool);
    let usuario = login.usuario.as_str();
    let primera = password();
    asegurar(&pool, usuario, &primera).await.unwrap();
    let app = conectar(&pool, usuario, &primera).await.unwrap();
    let mut abierta = app.acquire().await.unwrap();

    let segunda = password();
    asegurar(&pool, usuario, &segunda).await.expect("idempotente: el login ya existe");
    assert!(conectar(&pool, usuario, &primera).await.is_err(), "la contraseña anterior ya no entra");
    conectar(&pool, usuario, &segunda).await.expect("la nueva entra").close().await;
    // Las sesiones abiertas siguen hasta reiniciar el servicio.
    sqlx::query("SELECT 1").execute(&mut *abierta).await.unwrap();
    drop(abierta);
    app.close().await;

    assert_eq!(atributos(&pool, usuario).await, (false, false));
    assert_solo_set_role(&pool, usuario).await;
}

/// Un login dado de alta a mano sin NOINHERIT queda corregido.
#[sqlx::test(migrations = "../migrations")]
async fn corrige_un_login_que_heredaba(pool: PgPool) {
    let login = LoginTemporal::nuevo(&pool);
    let usuario = login.usuario.as_str();
    pool.execute(format!("CREATE ROLE \"{usuario}\" LOGIN INHERIT").as_str()).await.unwrap();
    pool.execute(format!("GRANT sc_ifpe, sc_sofom TO \"{usuario}\"").as_str()).await.unwrap();
    assert_eq!(relacion(&pool, usuario, "sc_ifpe").await, (true, true), "punto de partida: hereda");

    let pw = password();
    asegurar(&pool, usuario, &pw).await.unwrap();
    assert_eq!(atributos(&pool, usuario).await, (false, false));
    assert_solo_set_role(&pool, usuario).await;
    conectar(&pool, usuario, &pw).await.unwrap().close().await;
}

/// Falla cerrado: un login con atributos de administración no se toca (la
/// transacción se revierte, ni siquiera cambia la contraseña).
#[sqlx::test(migrations = "../migrations")]
async fn no_aprovisiona_un_login_privilegiado(pool: PgPool) {
    let login = LoginTemporal::nuevo(&pool);
    let usuario = login.usuario.as_str();
    pool.execute(format!("CREATE ROLE \"{usuario}\" LOGIN CREATEDB").as_str()).await.unwrap();
    let pw = password();
    let e = format!("{:#}", asegurar(&pool, usuario, &pw).await.unwrap_err());
    assert!(e.contains("CREATEDB"), "{e}");
    assert_eq!(atributos(&pool, usuario).await, (true, true), "sin cambios: sigue INHERIT y CREATEDB");
    assert!(conectar(&pool, usuario, &pw).await.is_err(), "la contraseña no se fijó");
    assert_eq!(relacion(&pool, usuario, "sc_ifpe").await, (false, false), "las concesiones se revirtieron");
}

/// DB_APP_USUARIO no puede ser el usuario con que corre migrar.
#[sqlx::test(migrations = "../migrations")]
async fn no_aprovisiona_al_dueno_del_esquema(pool: PgPool) {
    let dueno: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&pool).await.unwrap();
    let antes = atributos(&pool, &dueno).await;
    let e = asegurar(&pool, &dueno, &password()).await.unwrap_err().to_string();
    assert!(e.contains("usuario con que corre migrar"), "{e}");
    assert_eq!(atributos(&pool, &dueno).await, antes, "el dueño queda intacto");
}
