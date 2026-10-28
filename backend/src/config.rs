//! Configuración desde variables de entorno.
//!
//! Toda variable admite la convención `_FILE` (secretos de Docker y
//! Kubernetes): con `NOMBRE_FILE`, el valor se lee de ese archivo y no pasa
//! por el entorno del proceso (ver `var`).

use std::{env, fmt, io::Read, net::SocketAddr, str::FromStr};

use anyhow::{bail, Context};
use rust_decimal::Decimal;

use crate::db::Rol;

#[derive(Debug, Clone)]
pub struct Config {
    /// Usuario con el que corre la aplicación. En producción, un login sin
    /// privilegios propios, miembro de los roles `sc_*`.
    pub database_url: String,
    /// Dueño del esquema: migraciones y arranque. Si falta, se usa `database_url`.
    pub database_url_propietario: Option<String>,
    pub max_conexiones: u32,
    pub bind_addr: SocketAddr,
    pub jwt_secret: String,
    pub jwt_ttl_minutos: i64,
    pub frontend_origin: String,
    /// Habilita depósitos simulados por el propio cliente (solo con KYC
    /// verificado). Nunca en producción.
    pub modo_demo: bool,
    pub migrar_al_inicio: bool,
    /// Tareas de arranque ([`crate::bootstrap::ejecutar`]) al iniciar el
    /// servidor. En producción las hace `synthetic-capital migrar` y el
    /// servicio arranca con `MIGRAR_AL_INICIO=false` y `ARRANQUE_AL_INICIO=false`,
    /// sin `DATABASE_URL_PROPIETARIO`.
    pub arranque_al_inicio: bool,
    /// Prefijo de las CLABEs propias: banco (3) + plaza (3) [+ hasta 4 dígitos].
    pub clabe_prefijo: String,
    pub pld: ConfigAml,
    /// Periodicidad de cortes automáticos de Proof of Reserve (0 = desactivado).
    pub corte_intervalo_seg: u64,
    /// Antigüedad mínima del último corte para que la petición de prueba de
    /// un cliente nuevo pueda disparar uno (acota el costo a 1 corte por intervalo).
    pub corte_min_intervalo_seg: u64,
    /// Periodicidad de liquidación automática de mutuos vencidos (0 = desactivado).
    pub liquidacion_intervalo_seg: u64,
    pub reserva_institucion: String,
    pub reserva_cuenta: String,
    pub caja_institucion: String,
    pub caja_cuenta: String,
    pub operador_email: Option<String>,
    pub operador_password: Option<String>,
    pub http: LimitesHttp,
    pub spei: ConfigSpei,
    pub kyc: ConfigKyc,
}

/// Onboarding y KYC biométrico.
#[derive(Debug, Clone)]
pub struct ConfigKyc {
    /// Secretos HMAC con que firma sus resultados el proveedor biométrico (el
    /// vigente y, durante una rotación, el anterior). Sin `KYC_WEBHOOK_SECRETO`
    /// se genera uno aleatorio al arrancar: solo el proveedor simulado de este
    /// proceso puede firmar y el webhook externo queda cerrado.
    pub webhook_secretos: Vec<String>,
    pub tolerancia_firma_seg: u64,
    /// Si se indican, el arranque los fija en `kyc.parametros` (la base de
    /// datos es la que los hace cumplir).
    pub score_minimo: Option<Decimal>,
    pub score_revision: Option<Decimal>,
    pub max_intentos: Option<i16>,
}

/// Integración SPEI.
#[derive(Debug, Clone)]
pub struct ConfigSpei {
    /// Secretos HMAC de los webhooks del proveedor (el vigente y, durante una
    /// rotación, el anterior). Sin secreto, los webhooks responden 503.
    pub webhook_secretos: Vec<String>,
    /// Antigüedad máxima de la firma de un webhook.
    pub tolerancia_firma_seg: u64,
    /// Prefijo de nuestras claves de rastreo (1 a 8 letras/dígitos).
    pub prefijo_rastreo: String,
    /// Periodicidad del despachador de órdenes salientes (0 = desactivado).
    pub despacho_intervalo_ms: u64,
    /// Espera entre consultas de estado de una orden enviada.
    pub consulta_intervalo_seg: u64,
    /// Intentos sin resultado definitivo antes de pedir revisión manual.
    pub max_intentos: i32,
    /// Órdenes por ciclo del despachador.
    pub lote: i64,
    /// Espera mínima antes de volver a consultar una orden de resultado
    /// desconocido (el proveedor puede tardar en mostrar una orden registrada).
    pub espera_incierto_seg: u64,
    /// Días después de liquidar durante los que se consulta si la orden fue
    /// devuelta por el banco receptor.
    pub ventana_devolucion_dias: i32,
    /// Reposo exigido (sin que el despachador toque la orden) antes de que el
    /// operador reembolse una orden en revisión manual. Debe exceder el
    /// arrendamiento del despachador (60 s) más el tiempo máximo de una
    /// llamada al proveedor: así ningún registro tardío sigue en vuelo.
    pub espera_resolucion_seg: u64,
    /// Hora (Ciudad de México) en que el proveedor cambia la fecha de
    /// operación (corte SPEI). Una orden de resultado desconocido no se
    /// reenvía si el día de operación pudo cambiar desde su creación: el
    /// proveedor solo rechaza como duplicada la clave de rastreo del mismo día.
    pub hora_corte: chrono::NaiveTime,
    /// Holgura alrededor del corte: desfase de relojes (nuestro, de la base de
    /// datos y del proveedor) más la duración máxima de una llamada al
    /// proveedor.
    pub margen_corte_seg: u64,
}

impl Default for ConfigSpei {
    fn default() -> Self {
        Self {
            webhook_secretos: Vec::new(),
            tolerancia_firma_seg: 300,
            prefijo_rastreo: "SC".into(),
            despacho_intervalo_ms: 1_000,
            consulta_intervalo_seg: 5,
            max_intentos: 20,
            lote: 50,
            espera_incierto_seg: 60,
            ventana_devolucion_dias: 3,
            espera_resolucion_seg: 900,
            // Por confirmar contra el contrato con el participante liquidador.
            hora_corte: chrono::NaiveTime::from_hms_opt(17, 30, 0).expect("hora válida"),
            margen_corte_seg: 600,
        }
    }
}

/// Límites del servidor HTTP. Sin ellos, conexiones ociosas o lentas agotan
/// los descriptores de archivo y un cliente lento retiene el apagado.
#[derive(Debug, Clone)]
pub struct LimitesHttp {
    /// Tiempo máximo para recibir las cabeceras de una petición (también entre
    /// peticiones de una conexión keep-alive ociosa).
    pub cabeceras_seg: u64,
    /// Tiempo máximo para recibir el cuerpo de una petición (408).
    pub cuerpo_seg: u64,
    /// Tiempo máximo de una petición completa, incluido el handler.
    pub peticion_seg: u64,
    /// Conexiones TCP simultáneas; al llegar al tope se deja de aceptar.
    pub max_conexiones: usize,
    /// Plazo para terminar las peticiones en curso al apagar.
    pub apagado_seg: u64,
    /// Detrás de un proxy inverso propio: la IP del cliente es la última de
    /// `X-Forwarded-For` (la que agregó el proxy). Sin proxy, nunca: cualquiera
    /// podría escribir esa cabecera.
    pub proxy_confiable: bool,
}

impl Default for LimitesHttp {
    fn default() -> Self {
        Self {
            cabeceras_seg: 10,
            cuerpo_seg: 15,
            peticion_seg: 30,
            max_conexiones: 512,
            apagado_seg: 20,
            proxy_confiable: false,
        }
    }
}

/// Monitoreo PLD/FT. Los umbrales y reglas viven en la base de datos (aml.reglas_riesgo,
/// aml.parametros) y solo cambian con aprobación de cuatro ojos; aquí solo hay operación.
#[derive(Debug, Clone)]
pub struct ConfigAml {
    /// Periodicidad del ciclo de monitoreo por lotes (0 = desactivado).
    pub monitoreo_intervalo_seg: u64,
    /// Movimientos del ledger (IFPE + SOFOM) que revisa cada ciclo.
    pub monitoreo_lote: i64,
    /// Solo MODO_DEMO: oficiales de cumplimiento creados al arrancar.
    pub bootstrap_emails: Vec<String>,
    pub bootstrap_password: Option<String>,
}

impl Default for ConfigAml {
    fn default() -> Self {
        Self {
            monitoreo_intervalo_seg: 300,
            monitoreo_lote: 5_000,
            bootstrap_emails: Vec::new(),
            bootstrap_password: None,
        }
    }
}

/// Tope de un archivo `NOMBRE_FILE`: un secreto o una URL, nunca más.
const MAX_ARCHIVO: u64 = 64 * 1024;

/// Valor de una variable de configuración; vacía o solo espacios = no definida.
///
/// Convención `_FILE`: si `NOMBRE_FILE` está definida, el valor se lee de ese
/// archivo, sin los saltos de línea finales. Definir `NOMBRE` y `NOMBRE_FILE`
/// a la vez es un error. Los mensajes de error nombran la variable y la ruta,
/// nunca el contenido.
fn var(nombre: &str) -> anyhow::Result<Option<String>> {
    valor(nombre, |n| env::var(n).ok())
}

fn valor(nombre: &str, entorno: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<String>> {
    let definida = |v: Option<String>| v.filter(|v| !v.trim().is_empty());
    let directo = definida(entorno(nombre));
    let Some(ruta) = definida(entorno(&format!("{nombre}_FILE"))) else {
        return Ok(directo);
    };
    if directo.is_some() {
        bail!("{nombre} y {nombre}_FILE están definidas; usa solo una");
    }
    let contenido = leer_archivo(&ruta).with_context(|| format!("no se pudo leer {nombre}_FILE ({ruta})"))?;
    Ok(definida(Some(contenido.trim_end_matches(['\n', '\r']).to_owned())))
}

fn leer_archivo(ruta: &str) -> anyhow::Result<String> {
    let mut contenido = String::new();
    std::fs::File::open(ruta)?.take(MAX_ARCHIVO + 1).read_to_string(&mut contenido)?;
    if contenido.len() as u64 > MAX_ARCHIVO {
        bail!("el archivo excede {MAX_ARCHIVO} bytes");
    }
    Ok(contenido)
}

fn var_o<T: FromStr>(nombre: &str, defecto: T) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    match var(nombre)? {
        None => Ok(defecto),
        Some(v) => v.parse().map_err(|e| anyhow::anyhow!("variable {nombre} inválida ({v}): {e}")),
    }
}

/// Dirección de escucha del servidor (`BIND_ADDR`, por omisión 0.0.0.0:8080).
/// También la usa `synthetic-capital salud` para sondear el puerto local.
pub fn direccion_de_escucha() -> anyhow::Result<SocketAddr> {
    var_o("BIND_ADDR", SocketAddr::from(([0, 0, 0, 0], 8080)))
}

/// Login con el que corre el servicio. `synthetic-capital migrar` lo crea o
/// actualiza con `DB_APP_USUARIO` (por omisión `sc_app`) y `DB_APP_PASSWORD`.
#[derive(Clone)]
pub struct LoginAplicacion {
    usuario: String,
    password: String,
}

impl fmt::Debug for LoginAplicacion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginAplicacion").field("usuario", &self.usuario).field("password", &"<oculta>").finish()
    }
}

impl LoginAplicacion {
    pub const USUARIO_POR_OMISION: &'static str = "sc_app";

    /// `None` sin `DB_APP_PASSWORD`: el login se administra por fuera (README).
    pub fn desde_entorno() -> anyhow::Result<Option<Self>> {
        let usuario = var("DB_APP_USUARIO")?;
        match var("DB_APP_PASSWORD")? {
            Some(password) => Self::nuevo(usuario.as_deref().unwrap_or(Self::USUARIO_POR_OMISION), &password).map(Some),
            None => {
                if let Some(usuario) = usuario {
                    tracing::warn!(%usuario, "DB_APP_USUARIO sin DB_APP_PASSWORD: no se aprovisiona el login de la aplicación");
                }
                Ok(None)
            }
        }
    }

    /// El usuario es un identificador de PostgreSQL en minúsculas, distinto
    /// de los roles del esquema. La contraseña sigue las reglas de cualquier
    /// secreto y es ASCII imprimible sin espacios (ver [`crate::crypto::scram`]).
    pub fn nuevo(usuario: &str, password: &str) -> anyhow::Result<Self> {
        validar_usuario_bd(usuario)?;
        validar_secreto("DB_APP_PASSWORD", password)?;
        if !password.bytes().all(|b| b.is_ascii_graphic()) {
            bail!("DB_APP_PASSWORD solo admite ASCII imprimible sin espacios (genera una con: openssl rand -hex 32)");
        }
        Ok(Self { usuario: usuario.to_owned(), password: password.to_owned() })
    }

    pub fn usuario(&self) -> &str {
        &self.usuario
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

/// Identificador sin comillas de PostgreSQL, en minúsculas y de hasta 63
/// bytes; nunca un rol del esquema (`sc_ifpe`...) ni uno reservado (`pg_*`).
fn validar_usuario_bd(usuario: &str) -> anyhow::Result<()> {
    let mut bytes = usuario.bytes();
    let valido = usuario.len() <= 63
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !valido {
        bail!("DB_APP_USUARIO debe ser un identificador en minúsculas ([a-z_][a-z0-9_]*, hasta 63 caracteres): {usuario:?}");
    }
    if usuario.starts_with("pg_") {
        bail!("DB_APP_USUARIO no puede empezar con pg_ (prefijo reservado de PostgreSQL)");
    }
    if usuario == "sc_auditor_pld" || Rol::TODOS.iter().any(|r| r.nombre() == usuario) {
        bail!("DB_APP_USUARIO no puede ser un rol del esquema ({usuario})");
    }
    Ok(())
}

/// El JWT es HS256: conocer el secreto equivale a poder firmar tokens de
/// operador. Se exige también en MODO_DEMO (un demo expuesto sigue siendo un
/// banco expuesto) y se rechazan valores de ejemplo o de baja entropía.
pub fn validar_secreto_jwt(secreto: &str) -> anyhow::Result<()> {
    validar_secreto("JWT_SECRET", secreto)
}

/// Mismas reglas para cualquier secreto HMAC (JWT, webhooks SPEI).
pub fn validar_secreto(nombre: &str, secreto: &str) -> anyhow::Result<()> {
    if secreto.len() < 32 {
        bail!("{nombre} debe tener al menos 32 caracteres (genera uno con: openssl rand -hex 64)");
    }
    let minusculas = secreto.to_lowercase();
    if ["change-me", "changeme", "cambiame", "secret", "password", "example"].iter().any(|p| minusculas.contains(p)) {
        bail!("{nombre} parece un valor de ejemplo; genera uno aleatorio con: openssl rand -hex 64");
    }
    let distintos = secreto.chars().collect::<std::collections::HashSet<_>>().len();
    if distintos < 10 {
        bail!("{nombre} tiene muy poca variedad de caracteres; genera uno aleatorio con: openssl rand -hex 64");
    }
    Ok(())
}

/// Para qué se lee la configuración.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Uso {
    /// Servidor HTTP: exige `DATABASE_URL` y `JWT_SECRET`.
    Servidor,
    /// `synthetic-capital migrar`: basta la URL del dueño del esquema.
    Migracion,
}

impl Config {
    /// Configuración del servidor HTTP.
    pub fn desde_entorno() -> anyhow::Result<Self> {
        Self::leer(Uso::Servidor)
    }

    /// Configuración de `synthetic-capital migrar` (migraciones y arranque
    /// con el dueño del esquema): no exige `JWT_SECRET`, que queda vacío
    /// porque `migrar` no firma tokens, ni `DATABASE_URL` si está
    /// `DATABASE_URL_PROPIETARIO`. Lee igual que el servidor lo que usa el
    /// arranque (MODO_DEMO, KYC_*, RESERVA_*, CAJA_*, BOOTSTRAP_*).
    pub fn para_migrar() -> anyhow::Result<Self> {
        Self::leer(Uso::Migracion)
    }

    fn leer(uso: Uso) -> anyhow::Result<Self> {
        let modo_demo = var_o("MODO_DEMO", false)?;
        let jwt_secret = match var("JWT_SECRET")? {
            Some(secreto) => {
                validar_secreto_jwt(&secreto)?;
                secreto
            }
            None if uso == Uso::Migracion => String::new(),
            None => bail!("JWT_SECRET es obligatoria"),
        };
        let clabe_prefijo = var("CLABE_PREFIJO")?.unwrap_or_else(|| "999180".into());
        crate::spei::clabe::validar_prefijo(&clabe_prefijo).map_err(|e| anyhow::anyhow!("CLABE_PREFIJO: {e}"))?;
        for nombre in ["PLD_UMBRAL_RELEVANTE", "PLD_UMBRAL_RIESGO_ALTO", "PLD_MULTIPLO_PERFIL"] {
            if var(nombre)?.is_some() {
                tracing::warn!(
                    "{nombre} ya no se usa: los umbrales PLD viven en aml.reglas_riesgo y cambian con aprobación de cuatro ojos"
                );
            }
        }
        let database_url_propietario = var("DATABASE_URL_PROPIETARIO")?;
        let database_url = match (var("DATABASE_URL")?, uso) {
            (Some(url), _) => url,
            (None, Uso::Migracion) => {
                database_url_propietario.clone().context("DATABASE_URL_PROPIETARIO (o DATABASE_URL) es obligatoria")?
            }
            (None, Uso::Servidor) => bail!("DATABASE_URL es obligatoria"),
        };
        Ok(Self {
            database_url,
            database_url_propietario,
            max_conexiones: var_o("DB_MAX_CONEXIONES", 20)?,
            bind_addr: direccion_de_escucha()?,
            jwt_secret,
            jwt_ttl_minutos: var_o("JWT_TTL_MINUTOS", 60)?,
            frontend_origin: var("FRONTEND_ORIGIN")?.unwrap_or_else(|| "http://localhost:3000".into()),
            modo_demo,
            migrar_al_inicio: var_o("MIGRAR_AL_INICIO", true)?,
            arranque_al_inicio: var_o("ARRANQUE_AL_INICIO", true)?,
            clabe_prefijo,
            pld: ConfigAml {
                monitoreo_intervalo_seg: var_o("AML_MONITOREO_INTERVALO_SEG", 300)?,
                monitoreo_lote: var_o("AML_MONITOREO_LOTE", 5_000_i64)?.clamp(100, 100_000),
                bootstrap_emails: var("BOOTSTRAP_CUMPLIMIENTO_EMAILS")?
                    .map(|v| v.split(',').map(str::trim).filter(|e| !e.is_empty()).map(String::from).collect())
                    .unwrap_or_default(),
                bootstrap_password: var("BOOTSTRAP_CUMPLIMIENTO_PASSWORD")?,
            },
            corte_intervalo_seg: var_o("CORTE_INTERVALO_SEG", 300)?,
            corte_min_intervalo_seg: var_o("CORTE_MIN_INTERVALO_SEG", 60)?,
            liquidacion_intervalo_seg: var_o("LIQUIDACION_INTERVALO_SEG", 3600)?,
            reserva_institucion: var("RESERVA_INSTITUCION")?.unwrap_or_else(|| "Banco de México".into()),
            reserva_cuenta: var("RESERVA_CUENTA")?.unwrap_or_else(|| "SC-IFPE-RESERVA-001".into()),
            caja_institucion: var("CAJA_INSTITUCION")?.unwrap_or_else(|| "Banco Custodio SOFOM".into()),
            caja_cuenta: var("CAJA_CUENTA")?.unwrap_or_else(|| "SC-SOFOM-CAJA-001".into()),
            operador_email: var("BOOTSTRAP_OPERADOR_EMAIL")?,
            operador_password: var("BOOTSTRAP_OPERADOR_PASSWORD")?,
            spei: {
                let d = ConfigSpei::default();
                let mut webhook_secretos = Vec::new();
                for nombre in ["SPEI_WEBHOOK_SECRETO", "SPEI_WEBHOOK_SECRETO_ANTERIOR"] {
                    if let Some(s) = var(nombre)? {
                        validar_secreto(nombre, &s)?;
                        webhook_secretos.push(s);
                    }
                }
                let prefijo_rastreo = var("SPEI_PREFIJO_RASTREO")?.unwrap_or(d.prefijo_rastreo).to_ascii_uppercase();
                if prefijo_rastreo.is_empty()
                    || prefijo_rastreo.len() > 8
                    || !prefijo_rastreo.bytes().all(|b| b.is_ascii_alphanumeric())
                {
                    bail!("SPEI_PREFIJO_RASTREO debe tener de 1 a 8 letras o dígitos");
                }
                ConfigSpei {
                    webhook_secretos,
                    tolerancia_firma_seg: var_o("SPEI_TOLERANCIA_FIRMA_SEG", d.tolerancia_firma_seg)?,
                    prefijo_rastreo,
                    despacho_intervalo_ms: var_o("SPEI_DESPACHO_INTERVALO_MS", d.despacho_intervalo_ms)?,
                    consulta_intervalo_seg: var_o("SPEI_CONSULTA_INTERVALO_SEG", d.consulta_intervalo_seg)?.max(1),
                    max_intentos: var_o("SPEI_MAX_INTENTOS", d.max_intentos)?.max(1),
                    lote: var_o("SPEI_LOTE", d.lote)?.clamp(1, 500),
                    espera_incierto_seg: var_o("SPEI_ESPERA_INCIERTO_SEG", d.espera_incierto_seg)?,
                    ventana_devolucion_dias: var_o("SPEI_VENTANA_DEVOLUCION_DIAS", d.ventana_devolucion_dias)?
                        .clamp(0, 60),
                    espera_resolucion_seg: var_o("SPEI_ESPERA_RESOLUCION_SEG", d.espera_resolucion_seg)?,
                    hora_corte: match var("SPEI_HORA_CORTE")? {
                        None => d.hora_corte,
                        Some(v) => chrono::NaiveTime::parse_from_str(v.trim(), "%H:%M")
                            .map_err(|e| anyhow::anyhow!("SPEI_HORA_CORTE debe ser HH:MM ({v}): {e}"))?,
                    },
                    margen_corte_seg: var_o("SPEI_MARGEN_CORTE_SEG", d.margen_corte_seg)?.max(60),
                }
            },
            http: {
                let d = LimitesHttp::default();
                LimitesHttp {
                    cabeceras_seg: var_o("HTTP_CABECERAS_SEG", d.cabeceras_seg)?.max(1),
                    cuerpo_seg: var_o("HTTP_CUERPO_SEG", d.cuerpo_seg)?.max(1),
                    peticion_seg: var_o("HTTP_PETICION_SEG", d.peticion_seg)?.max(1),
                    max_conexiones: var_o("HTTP_MAX_CONEXIONES", d.max_conexiones)?.max(1),
                    apagado_seg: var_o("HTTP_APAGADO_SEG", d.apagado_seg)?,
                    proxy_confiable: var_o("HTTP_PROXY_CONFIABLE", d.proxy_confiable)?,
                }
            },
            kyc: {
                let mut webhook_secretos = Vec::new();
                for nombre in ["KYC_WEBHOOK_SECRETO", "KYC_WEBHOOK_SECRETO_ANTERIOR"] {
                    if let Some(s) = var(nombre)? {
                        validar_secreto(nombre, &s)?;
                        webhook_secretos.push(s);
                    }
                }
                if webhook_secretos.is_empty() {
                    let aleatorio: [u8; 32] = rand::random();
                    webhook_secretos.push(hex::encode(aleatorio));
                }
                let score = |nombre: &str| -> anyhow::Result<Option<Decimal>> {
                    let v: Option<Decimal> = var(nombre)?
                        .map(|v| v.parse())
                        .transpose()
                        .map_err(|e| anyhow::anyhow!("{nombre} inválida: {e}"))?;
                    if v.is_some_and(|s| s < Decimal::new(5, 1) || s >= Decimal::ONE) {
                        bail!("{nombre} debe estar entre 0.5 y 0.9999");
                    }
                    Ok(v)
                };
                ConfigKyc {
                    webhook_secretos,
                    tolerancia_firma_seg: var_o("KYC_TOLERANCIA_FIRMA_SEG", 300)?,
                    score_minimo: score("KYC_SCORE_MINIMO")?,
                    score_revision: score("KYC_SCORE_REVISION")?,
                    max_intentos: var("KYC_MAX_INTENTOS")?
                        .map(|v| v.parse::<i16>())
                        .transpose()
                        .map_err(|e| anyhow::anyhow!("KYC_MAX_INTENTOS inválida: {e}"))?,
                }
            },
        })
    }

    /// Configuración para pruebas: sin tareas de fondo, migraciones ni arranque al inicio.
    pub fn para_pruebas(database_url: impl Into<String>) -> Self {
        Self {
            database_url: database_url.into(),
            database_url_propietario: None,
            max_conexiones: 10,
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            jwt_secret: "secreto-de-pruebas-con-al-menos-32-caracteres".into(),
            jwt_ttl_minutos: 60,
            frontend_origin: "http://localhost:3000".into(),
            modo_demo: true,
            migrar_al_inicio: false,
            arranque_al_inicio: false,
            clabe_prefijo: "999180".into(),
            pld: ConfigAml { monitoreo_intervalo_seg: 0, ..ConfigAml::default() },
            corte_intervalo_seg: 0,
            corte_min_intervalo_seg: 0,
            liquidacion_intervalo_seg: 0,
            reserva_institucion: "Banco de México".into(),
            reserva_cuenta: "SC-IFPE-RESERVA-001".into(),
            caja_institucion: "Banco Custodio SOFOM".into(),
            caja_cuenta: "SC-SOFOM-CAJA-001".into(),
            operador_email: None,
            operador_password: None,
            http: LimitesHttp::default(),
            spei: ConfigSpei {
                webhook_secretos: vec!["secreto-webhook-spei-de-pruebas-0123456789abcdef".into()],
                despacho_intervalo_ms: 0,
                ..ConfigSpei::default()
            },
            kyc: ConfigKyc {
                webhook_secretos: vec!["secreto-resultados-kyc-de-pruebas-0123456789abcdef".into()],
                tolerancia_firma_seg: 300,
                score_minimo: None,
                score_revision: None,
                max_intentos: None,
            },
        }
    }
}

#[cfg(test)]
mod pruebas {
    use std::{collections::HashMap, path::PathBuf};

    use super::{validar_secreto_jwt, valor, LoginAplicacion, MAX_ARCHIVO};

    #[test]
    fn rechaza_secretos_debiles() {
        assert!(validar_secreto_jwt("corto").is_err());
        assert!(validar_secreto_jwt("change-me-change-me-change-me-change-me").is_err());
        assert!(validar_secreto_jwt(&"a".repeat(64)).is_err());
        assert!(validar_secreto_jwt("my-jwt-secret-for-the-bank-0123456789").is_err());
        assert!(validar_secreto_jwt("9f2c1e7ab04d5b6c8e3f1a2b7c9d0e4f6a8b1c3d5e7f9a0b").is_ok());
    }

    /// Archivo temporal que se borra al salir de la prueba.
    struct Archivo(PathBuf);

    impl Archivo {
        fn nuevo(contenido: &[u8]) -> Self {
            let ruta = std::env::temp_dir().join(format!("sc-config-{}", uuid::Uuid::new_v4().simple()));
            std::fs::write(&ruta, contenido).unwrap();
            Self(ruta)
        }

        fn ruta(&self) -> String {
            self.0.display().to_string()
        }
    }

    impl Drop for Archivo {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Entorno simulado: las pruebas no tocan las variables del proceso.
    fn entorno(pares: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let mapa: HashMap<String, String> = pares.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |nombre| mapa.get(nombre).cloned()
    }

    #[test]
    fn variable_directa_vacia_o_ausente() {
        assert_eq!(valor("X", entorno(&[("X", "abc")])).unwrap().as_deref(), Some("abc"));
        assert_eq!(valor("X", entorno(&[("X", "  ")])).unwrap(), None);
        assert_eq!(valor("X", entorno(&[])).unwrap(), None);
        assert_eq!(valor("X", entorno(&[("X", "abc"), ("X_FILE", "")])).unwrap().as_deref(), Some("abc"));
    }

    #[test]
    fn variable_desde_archivo() {
        let lf = Archivo::nuevo(b"valor-del-secreto\n");
        assert_eq!(valor("X", entorno(&[("X_FILE", &lf.ruta())])).unwrap().as_deref(), Some("valor-del-secreto"));
        let crlf = Archivo::nuevo(b"postgres://u:p@h/db\r\n");
        assert_eq!(valor("X", entorno(&[("X_FILE", &crlf.ruta())])).unwrap().as_deref(), Some("postgres://u:p@h/db"));
        // Solo se quitan los saltos de línea finales.
        let espacios = Archivo::nuevo(b" con espacios \n\n");
        assert_eq!(valor("X", entorno(&[("X_FILE", &espacios.ruta())])).unwrap().as_deref(), Some(" con espacios "));
        // Una línea `X=` vacía (plantilla .env) no choca con X_FILE.
        assert_eq!(
            valor("X", entorno(&[("X", ""), ("X_FILE", &lf.ruta())])).unwrap().as_deref(),
            Some("valor-del-secreto")
        );
        // Archivo vacío = no definida, igual que una variable vacía.
        let vacio = Archivo::nuevo(b"\n");
        assert_eq!(valor("X", entorno(&[("X_FILE", &vacio.ruta())])).unwrap(), None);
    }

    #[test]
    fn errores_de_la_convencion_file() {
        let a = Archivo::nuevo(b"valor-del-secreto\n");
        let e = valor("X", entorno(&[("X", "otro"), ("X_FILE", &a.ruta())])).unwrap_err().to_string();
        assert!(e.contains("X y X_FILE"), "{e}");
        assert!(!e.contains("valor-del-secreto") && !e.contains("otro"), "{e}");

        let e = format!("{:#}", valor("X", entorno(&[("X_FILE", "/no/existe/sc-config")])).unwrap_err());
        assert!(e.contains("X_FILE") && e.contains("/no/existe/sc-config"), "{e}");

        let binario = Archivo::nuevo(&[0xff, 0xfe, 0x00]);
        assert!(valor("X", entorno(&[("X_FILE", &binario.ruta())])).is_err());

        let grande = Archivo::nuevo(&vec![b'a'; MAX_ARCHIVO as usize + 1]);
        let e = format!("{:#}", valor("X", entorno(&[("X_FILE", &grande.ruta())])).unwrap_err());
        assert!(e.contains("excede") && !e.contains("aaaa"), "{e}");
        let al_limite = Archivo::nuevo(&vec![b'a'; MAX_ARCHIVO as usize]);
        assert!(valor("X", entorno(&[("X_FILE", &al_limite.ruta())])).unwrap().is_some());
    }

    #[test]
    fn login_de_aplicacion_valida_usuario_y_password() {
        let pw = "9f2c1e7ab04d5b6c8e3f1a2b7c9d0e4f6a8b1c3d";
        let login = LoginAplicacion::nuevo("sc_app", pw).unwrap();
        assert_eq!((login.usuario(), login.password()), ("sc_app", pw));
        assert!(LoginAplicacion::nuevo("_app_2", pw).is_ok());
        let largo = "a".repeat(64);
        for usuario in [
            "",
            "SC_APP",
            "1app",
            "sc-app",
            "sc app",
            "pg_app",
            "sc_ifpe",
            "sc_aml",
            "sc_auditor_pld",
            &largo,
            "a\"; DROP ROLE x; --",
        ] {
            assert!(LoginAplicacion::nuevo(usuario, pw).is_err(), "{usuario:?}");
        }
        for password in [
            "corta",
            "change-me-change-me-change-me-change-me",
            "con espacio 9f2c1e7ab04d5b6c8e3f1a2b7c9d",
            "contraseña-9f2c1e7ab04d5b6c8e3f1a2b7c9d0e",
        ] {
            assert!(LoginAplicacion::nuevo("sc_app", password).is_err(), "{password:?}");
        }
        assert!(!format!("{login:?}").contains(pw), "Debug no muestra la contraseña");
    }
}
