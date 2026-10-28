//! Contrato del binario `synthetic-capital` que usan docker-compose, ECS y el
//! HEALTHCHECK de la imagen: códigos de salida, `salud` contra un servidor
//! real, y `migrar` de punta a punta con secretos en archivos (`_FILE`)
//! seguido del servicio en modo producción (solo el login de la aplicación).

mod comun;

use std::{
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use comun::*;
use sqlx::{postgres::PgPoolOptions, ConnectOptions, PgPool};
use synthetic_capital::{routes, servidor, AppState};
use tokio::sync::oneshot;

const BINARIO: &str = env!("CARGO_BIN_EXE_synthetic-capital");

/// Entorno limpio: ni las variables del proceso de pruebas ni un `.env` del árbol.
fn binario(args: &[&str], entorno: &[(&str, &str)]) -> Command {
    let mut c = Command::new(BINARIO);
    c.args(args).env_clear().current_dir(std::env::temp_dir()).env("RUST_LOG", "warn").envs(entorno.iter().copied());
    c
}

/// Ejecuta el binario sin bloquear el runtime (el servidor de la prueba corre en él).
async fn correr(args: &[&str], entorno: &[(&str, &str)]) -> Output {
    let mut c = binario(args, entorno);
    tokio::task::spawn_blocking(move || c.output().unwrap()).await.unwrap()
}

fn texto(salida: &Output) -> String {
    format!(
        "{}\nstdout: {}\nstderr: {}",
        salida.status,
        String::from_utf8_lossy(&salida.stdout),
        String::from_utf8_lossy(&salida.stderr)
    )
}

/// Archivo temporal (un secreto montado) que se borra al salir.
struct Archivo(PathBuf);

impl Archivo {
    fn nuevo(contenido: &str) -> Self {
        let ruta = std::env::temp_dir().join(format!("sc-cli-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&ruta, format!("{contenido}\n")).unwrap();
        Self(ruta)
    }

    fn ruta(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for Archivo {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn puerto_libre() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn ayuda_version_y_errores_de_uso() {
    let ayuda = binario(&["--help"], &[]).output().unwrap();
    assert!(ayuda.status.success(), "{}", texto(&ayuda));
    let ayuda = String::from_utf8_lossy(&ayuda.stdout);
    assert!(ayuda.contains("migrar") && ayuda.contains("salud [vivo|listo]") && ayuda.contains("_FILE"), "{ayuda}");

    let version = binario(&["--version"], &[]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("synthetic-capital {}", env!("CARGO_PKG_VERSION"))
    );

    for args in [&["migrate"][..], &["salud", "otro"], &["migrar", "ya"]] {
        let salida = binario(args, &[]).output().unwrap();
        assert_eq!(salida.status.code(), Some(2), "{args:?}: {}", texto(&salida));
        assert!(String::from_utf8_lossy(&salida.stderr).contains("Uso: synthetic-capital"), "{}", texto(&salida));
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn salud_sale_con_cero_solo_si_el_servidor_responde_200(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let limites = cfg.http.clone();
    let app = routes::router(AppState::nuevo(pool.clone(), cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = listener.local_addr().unwrap().to_string();
    let (apagar, senal) = oneshot::channel::<()>();
    let tarea = tokio::spawn(async move {
        servidor::servir(listener, app, &limites, async {
            let _ = senal.await;
        })
        .await;
    });

    let vivo = correr(&["salud", "vivo"], &[("BIND_ADDR", &bind)]).await;
    assert!(vivo.status.success(), "{}", texto(&vivo));
    assert!(String::from_utf8_lossy(&vivo.stdout).contains("GET /vivo"), "{}", texto(&vivo));
    let listo = correr(&["salud"], &[("BIND_ADDR", &bind)]).await;
    assert!(listo.status.success(), "{}", texto(&listo));
    assert!(String::from_utf8_lossy(&listo.stdout).contains("GET /salud"), "{}", texto(&listo));
    // BIND_ADDR también admite _FILE.
    let archivo = Archivo::nuevo(&bind);
    let listo = correr(&["salud", "listo"], &[("BIND_ADDR_FILE", archivo.ruta())]).await;
    assert!(listo.status.success(), "{}", texto(&listo));
    let ambas = correr(&["salud", "vivo"], &[("BIND_ADDR", &bind), ("BIND_ADDR_FILE", archivo.ruta())]).await;
    assert_eq!(ambas.status.code(), Some(1), "{}", texto(&ambas));
    assert!(String::from_utf8_lossy(&ambas.stderr).contains("BIND_ADDR y BIND_ADDR_FILE"), "{}", texto(&ambas));

    apagar.send(()).unwrap();
    tarea.await.unwrap();
    // Sin servidor: falla rápido con código 1.
    let inicio = Instant::now();
    let caido = correr(&["salud", "vivo"], &[("BIND_ADDR", &bind)]).await;
    assert_eq!(caido.status.code(), Some(1), "{}", texto(&caido));
    assert!(inicio.elapsed() < Duration::from_secs(5), "{:?}", inicio.elapsed());
}

async fn entra(pool: &PgPool, usuario: &str, password: &str) -> bool {
    let opciones = pool.connect_options().as_ref().clone().username(usuario).password(password);
    match PgPoolOptions::new().max_connections(1).connect_with(opciones).await {
        Ok(p) => {
            p.close().await;
            true
        }
        Err(_) => false,
    }
}

/// Servidor real (el binario) hasta que lo maten al soltarlo.
struct Servicio(Child);

impl Drop for Servicio {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `migrar` sobre una base vacía, como lo corre el trabajo de despliegue: solo
/// la URL del dueño y la contraseña del login, ambas en archivos; sin
/// JWT_SECRET. Después, el servicio arranca como ese login, sin la URL del
/// dueño y sin migrar ni arrancar, y sus sondas responden.
#[sqlx::test(migrations = false)]
async fn migrar_aprovisiona_y_el_servicio_corre_solo_con_el_login(pool: PgPool) {
    let login = LoginTemporal::nuevo(&pool);
    let usuario = login.usuario.as_str();
    let url_dueno = pool.connect_options().to_url_lossy().to_string();
    let url = Archivo::nuevo(&url_dueno);
    let primera = hex::encode(rand::random::<[u8; 24]>());
    let secreto = Archivo::nuevo(&primera);
    let entorno = [
        ("DATABASE_URL_PROPIETARIO_FILE", url.ruta()),
        ("DB_APP_USUARIO", usuario),
        ("DB_APP_PASSWORD_FILE", secreto.ruta()),
    ];
    let salida = correr(&["migrar"], &entorno).await;
    assert!(salida.status.success(), "{}", texto(&salida));

    let migraciones = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../migrations"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "sql"))
        .count() as i64;
    let aplicadas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success").fetch_one(&pool).await.unwrap();
    assert_eq!(aplicadas, migraciones);
    let reservas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ifpe.reserva_custodia WHERE activa").fetch_one(&pool).await.unwrap();
    assert_eq!(reservas, 1, "el arranque creó la cuenta de reserva");
    assert!(entra(&pool, usuario, &primera).await, "el login entra con la contraseña del archivo");

    // Repetir rota la contraseña (aquí, por variable de entorno).
    let segunda = hex::encode(rand::random::<[u8; 24]>());
    let salida = correr(
        &["migrar"],
        &[("DATABASE_URL_PROPIETARIO", &url_dueno), ("DB_APP_USUARIO", usuario), ("DB_APP_PASSWORD", &segunda)],
    )
    .await;
    assert!(salida.status.success(), "{}", texto(&salida));
    assert!(!entra(&pool, usuario, &primera).await, "la contraseña anterior ya no entra");
    assert!(entra(&pool, usuario, &segunda).await);

    // NOMBRE y NOMBRE_FILE a la vez: error, sin tocar nada.
    let salida = correr(&["migrar"], &[entorno[0], entorno[1], entorno[2], ("DB_APP_PASSWORD", &segunda)]).await;
    assert_eq!(salida.status.code(), Some(1), "{}", texto(&salida));
    assert!(String::from_utf8_lossy(&salida.stderr).contains("DB_APP_PASSWORD y DB_APP_PASSWORD_FILE"));
    assert!(entra(&pool, usuario, &segunda).await);

    // El servicio, como lo despliega producción.
    let opciones = pool.connect_options().as_ref().clone().username(usuario).password(&segunda);
    let url_app = Archivo::nuevo(opciones.to_url_lossy().as_str());
    let jwt = Archivo::nuevo(&hex::encode(rand::random::<[u8; 32]>()));
    let bind = SocketAddr::from(([127, 0, 0, 1], puerto_libre())).to_string();
    let servicio = binario(
        &[],
        &[
            ("DATABASE_URL_FILE", url_app.ruta()),
            ("JWT_SECRET_FILE", jwt.ruta()),
            ("BIND_ADDR", &bind),
            ("MIGRAR_AL_INICIO", "false"),
            ("ARRANQUE_AL_INICIO", "false"),
            ("CORTE_INTERVALO_SEG", "0"),
            ("LIQUIDACION_INTERVALO_SEG", "0"),
            ("AML_MONITOREO_INTERVALO_SEG", "0"),
            ("SPEI_DESPACHO_INTERVALO_MS", "0"),
        ],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    let mut servicio = Servicio(servicio);
    let limite = Instant::now() + Duration::from_secs(20);
    loop {
        let vivo = correr(&["salud", "vivo"], &[("BIND_ADDR", &bind)]).await;
        if vivo.status.success() {
            break;
        }
        assert!(servicio.0.try_wait().unwrap().is_none(), "el servicio terminó: {}", texto(&vivo));
        assert!(Instant::now() < limite, "el servicio no respondió: {}", texto(&vivo));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let listo = correr(&["salud", "listo"], &[("BIND_ADDR", &bind)]).await;
    assert!(listo.status.success(), "{}", texto(&listo));
    drop(servicio);
}
