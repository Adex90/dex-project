//! Línea de comandos del binario `synthetic-capital`.
//!
//! - Sin argumentos (o `servir`): servidor HTTP, como siempre.
//! - `migrar`: migraciones y tareas de arranque con el dueño del esquema y,
//!   con `DB_APP_PASSWORD`, el login de la aplicación; después termina.
//! - `salud [vivo|listo]`: sonda HTTP del servidor local para imágenes sin
//!   shell ni curl (distroless). Sale con 0 solo si la ruta responde 200. Solo
//!   usa `std`: ni runtime de tokio ni conexión a la base de datos.

use std::{
    ffi::OsString,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    process::ExitCode,
    time::{Duration, Instant},
};

pub const USO: &str = "\
Uso: synthetic-capital [COMANDO]

Comandos:
  (ninguno), servir    servidor HTTP en BIND_ADDR (por omisión 0.0.0.0:8080)
  migrar               migraciones y arranque con el dueño del esquema
                       (DATABASE_URL_PROPIETARIO, si no DATABASE_URL); con
                       DB_APP_PASSWORD crea o actualiza el login de la
                       aplicación (DB_APP_USUARIO, por omisión sc_app); termina
  salud [vivo|listo]   sonda HTTP del servidor local: vivo = GET /vivo (no
                       consulta la base de datos), listo = GET /salud (por
                       omisión); sale con 0 solo si responde 200
  -h, --help           esta ayuda
  -V, --version        versión

Cada variable de configuración NOMBRE admite NOMBRE_FILE: el valor se lee de
ese archivo (secretos de Docker o Kubernetes). Definir ambas es un error.";

/// Plazo total de `salud` (conexión, petición y línea de estado).
pub const PLAZO_SONDA: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comando {
    Servir,
    Migrar,
    Salud(Sonda),
    Ayuda,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sonda {
    /// El proceso atiende HTTP (salud del contenedor). No consulta la base de datos.
    Vivo,
    /// El servicio puede atender peticiones: la base de datos responde (balanceador).
    Listo,
}

impl Sonda {
    pub const fn ruta(self) -> &'static str {
        match self {
            Sonda::Vivo => "/vivo",
            Sonda::Listo => "/salud",
        }
    }
}

/// Interpreta los argumentos, sin el nombre del programa.
pub fn interpretar<I>(args: I) -> Result<Comando, String>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.into().to_string_lossy().into_owned()).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["servir"] => Ok(Comando::Servir),
        ["migrar"] => Ok(Comando::Migrar),
        ["salud"] | ["salud", "listo"] => Ok(Comando::Salud(Sonda::Listo)),
        ["salud", "vivo"] => Ok(Comando::Salud(Sonda::Vivo)),
        ["-h" | "--help" | "ayuda"] => Ok(Comando::Ayuda),
        ["-V" | "--version"] => Ok(Comando::Version),
        _ => Err(format!("argumentos no reconocidos: {}", args.join(" "))),
    }
}

/// `synthetic-capital salud`: sondea el servidor local y da el código de salida.
pub fn salud(sonda: Sonda) -> ExitCode {
    let ruta = sonda.ruta();
    let dir = match crate::config::direccion_de_escucha() {
        Ok(bind) => direccion_local(bind),
        Err(e) => {
            eprintln!("salud: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    match sondear(dir, ruta, PLAZO_SONDA) {
        Ok(200) => {
            println!("ok: GET {ruta} en {dir} respondió 200");
            ExitCode::SUCCESS
        }
        Ok(codigo) => {
            eprintln!("salud: GET {ruta} en {dir} respondió {codigo}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("salud: GET {ruta} en {dir} falló: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Dirección a sondear: el loopback si el servidor escucha en todas las
/// interfaces (0.0.0.0 o ::); si no, la dirección exacta en que escucha.
pub fn direccion_local(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, bind.port())
}

/// GET mínimo por HTTP/1.1 con un plazo total; devuelve el código de estado.
pub fn sondear(dir: SocketAddr, ruta: &str, plazo: Duration) -> io::Result<u16> {
    let limite = Instant::now() + plazo;
    let mut conexion = TcpStream::connect_timeout(&dir, plazo)?;
    conexion.set_write_timeout(Some(plazo))?;
    let peticion = format!(
        "GET {ruta} HTTP/1.1\r\nHost: {dir}\r\nUser-Agent: synthetic-capital-salud\r\nConnection: close\r\n\r\n"
    );
    conexion.write_all(peticion.as_bytes())?;
    // Basta la línea de estado ("HTTP/1.1 200 OK").
    let mut recibido = Vec::with_capacity(128);
    let mut bloque = [0u8; 512];
    while !recibido.windows(2).any(|par| par == b"\r\n") && recibido.len() <= 1024 {
        let restante = limite.saturating_duration_since(Instant::now());
        if restante.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("sin respuesta en {plazo:?}")));
        }
        conexion.set_read_timeout(Some(restante))?;
        match conexion.read(&mut bloque) {
            Ok(0) => break,
            Ok(n) => recibido.extend_from_slice(&bloque[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("sin respuesta en {plazo:?}")));
            }
            Err(e) => return Err(e),
        }
    }
    let texto = String::from_utf8_lossy(&recibido);
    let mut partes = texto.lines().next().unwrap_or_default().split(' ');
    match (partes.next(), partes.next().map(str::parse::<u16>)) {
        (Some(version), Some(Ok(codigo))) if version.starts_with("HTTP/") => Ok(codigo),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "respuesta HTTP inválida")),
    }
}

#[cfg(test)]
mod pruebas {
    use std::{net::TcpListener, sync::mpsc, thread};

    use super::*;

    #[test]
    fn interpreta_los_comandos() {
        let sin_args: [&str; 0] = [];
        assert_eq!(interpretar(sin_args), Ok(Comando::Servir));
        assert_eq!(interpretar(["servir"]), Ok(Comando::Servir));
        assert_eq!(interpretar(["migrar"]), Ok(Comando::Migrar));
        assert_eq!(interpretar(["salud"]), Ok(Comando::Salud(Sonda::Listo)));
        assert_eq!(interpretar(["salud", "listo"]), Ok(Comando::Salud(Sonda::Listo)));
        assert_eq!(interpretar(["salud", "vivo"]), Ok(Comando::Salud(Sonda::Vivo)));
        for ayuda in ["-h", "--help", "ayuda"] {
            assert_eq!(interpretar([ayuda]), Ok(Comando::Ayuda));
        }
        assert_eq!(interpretar(["--version"]), Ok(Comando::Version));
        assert_eq!(interpretar(["-V"]), Ok(Comando::Version));
        assert_eq!(Sonda::Vivo.ruta(), "/vivo");
        assert_eq!(Sonda::Listo.ruta(), "/salud");
    }

    #[test]
    fn rechaza_argumentos_desconocidos() {
        for args in [&["migrar", "ya"][..], &["salud", "otro"], &["salud", "vivo", "x"], &["migrate"], &["--migrar"]] {
            let e = interpretar(args.iter().copied()).unwrap_err();
            assert!(e.contains(&args.join(" ")), "{e}");
        }
    }

    #[test]
    fn sondea_el_loopback_si_escucha_en_todas_las_interfaces() {
        let d = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert_eq!(direccion_local(d("0.0.0.0:8080")), d("127.0.0.1:8080"));
        assert_eq!(direccion_local(d("[::]:9000")), d("[::1]:9000"));
        assert_eq!(direccion_local(d("10.1.2.3:8080")), d("10.1.2.3:8080"));
        assert_eq!(direccion_local(d("127.0.0.1:8081")), d("127.0.0.1:8081"));
    }

    /// Servidor de una sola conexión: devuelve la petición recibida y responde `respuesta`.
    fn servidor(respuesta: &'static [u8]) -> (SocketAddr, mpsc::Receiver<String>) {
        let escucha = TcpListener::bind("127.0.0.1:0").unwrap();
        let dir = escucha.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut s, _) = escucha.accept().unwrap();
            let mut recibido = Vec::new();
            let mut bloque = [0u8; 256];
            while !recibido.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut bloque) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => recibido.extend_from_slice(&bloque[..n]),
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&recibido).into_owned());
            let _ = s.write_all(respuesta);
        });
        (dir, rx)
    }

    #[test]
    fn sondeo_devuelve_el_codigo_de_estado() {
        let (dir, peticion) = servidor(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
        assert_eq!(sondear(dir, "/vivo", Duration::from_secs(2)).unwrap(), 200);
        let peticion = peticion.recv().unwrap();
        assert!(peticion.starts_with("GET /vivo HTTP/1.1\r\n"), "{peticion}");
        assert!(peticion.contains(&format!("Host: {dir}\r\n")), "{peticion}");
        assert!(peticion.ends_with("Connection: close\r\n\r\n"), "{peticion}");

        let (dir, _) = servidor(b"HTTP/1.1 503 Service Unavailable\r\n\r\n");
        assert_eq!(sondear(dir, "/salud", Duration::from_secs(2)).unwrap(), 503);

        let (dir, _) = servidor(b"hola\r\n");
        assert_eq!(sondear(dir, "/vivo", Duration::from_secs(2)).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn sondeo_falla_rapido_sin_servidor_o_sin_respuesta() {
        // Puerto cerrado: conexión rechazada.
        let cerrado = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(sondear(cerrado, "/vivo", Duration::from_secs(2)).is_err());

        // Acepta pero nunca responde: vence el plazo.
        let mudo = TcpListener::bind("127.0.0.1:0").unwrap();
        let dir = mudo.local_addr().unwrap();
        let inicio = Instant::now();
        let e = sondear(dir, "/vivo", Duration::from_millis(300)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        assert!(inicio.elapsed() < Duration::from_secs(2), "{:?}", inicio.elapsed());
        drop(mudo);
    }
}
