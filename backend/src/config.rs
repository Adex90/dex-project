//! Configuración desde variables de entorno.

use std::{env, net::SocketAddr, str::FromStr};

use anyhow::{bail, Context};
use rust_decimal::Decimal;

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
    /// Habilita atajos de demostración: KYC automático al registrarse y
    /// depósitos simulados por el propio cliente. Nunca en producción.
    pub modo_demo: bool,
    pub migrar_al_inicio: bool,
    /// Prefijo de las CLABEs propias: banco (3) + plaza (3) [+ hasta 4 dígitos].
    pub clabe_prefijo: String,
    pub pld: UmbralesPld,
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
}

impl Default for LimitesHttp {
    fn default() -> Self {
        Self { cabeceras_seg: 10, cuerpo_seg: 15, peticion_seg: 30, max_conexiones: 512, apagado_seg: 20 }
    }
}

/// Umbrales del monitoreo transaccional PLD/FT (MXN).
#[derive(Debug, Clone)]
pub struct UmbralesPld {
    /// Operación relevante (y base para detectar fraccionamiento en 24 h).
    pub relevante: Decimal,
    /// Operación inusual para clientes de riesgo ALTO.
    pub riesgo_alto: Decimal,
    /// Múltiplo del ingreso mensual declarado que dispara alerta de perfil.
    pub multiplo_perfil: Decimal,
}

impl Default for UmbralesPld {
    fn default() -> Self {
        Self {
            relevante: Decimal::new(150_000, 0),
            riesgo_alto: Decimal::new(50_000, 0),
            multiplo_perfil: Decimal::new(3, 0),
        }
    }
}

fn var(nombre: &str) -> Option<String> {
    env::var(nombre).ok().filter(|v| !v.trim().is_empty())
}

fn var_o<T: FromStr>(nombre: &str, defecto: T) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    match var(nombre) {
        None => Ok(defecto),
        Some(v) => v.parse().map_err(|e| anyhow::anyhow!("variable {nombre} inválida ({v}): {e}")),
    }
}

/// El JWT es HS256: conocer el secreto equivale a poder firmar tokens de
/// operador. Se exige también en MODO_DEMO (un demo expuesto sigue siendo un
/// banco expuesto) y se rechazan valores de ejemplo o de baja entropía.
pub fn validar_secreto_jwt(secreto: &str) -> anyhow::Result<()> {
    if secreto.len() < 32 {
        bail!("JWT_SECRET debe tener al menos 32 caracteres (genera uno con: openssl rand -hex 64)");
    }
    let minusculas = secreto.to_lowercase();
    if ["change-me", "changeme", "cambiame", "secret", "password", "example"].iter().any(|p| minusculas.contains(p)) {
        bail!("JWT_SECRET parece un valor de ejemplo; genera uno aleatorio con: openssl rand -hex 64");
    }
    let distintos = secreto.chars().collect::<std::collections::HashSet<_>>().len();
    if distintos < 10 {
        bail!("JWT_SECRET tiene muy poca variedad de caracteres; genera uno aleatorio con: openssl rand -hex 64");
    }
    Ok(())
}

impl Config {
    pub fn desde_entorno() -> anyhow::Result<Self> {
        let modo_demo = var_o("MODO_DEMO", false)?;
        let jwt_secret = var("JWT_SECRET").context("JWT_SECRET es obligatoria")?;
        validar_secreto_jwt(&jwt_secret)?;
        let clabe_prefijo = var("CLABE_PREFIJO").unwrap_or_else(|| "999180".into());
        crate::spei::clabe::validar_prefijo(&clabe_prefijo).map_err(|e| anyhow::anyhow!("CLABE_PREFIJO: {e}"))?;
        let defecto = UmbralesPld::default();
        Ok(Self {
            database_url: var("DATABASE_URL").context("DATABASE_URL es obligatoria")?,
            database_url_propietario: var("DATABASE_URL_PROPIETARIO"),
            max_conexiones: var_o("DB_MAX_CONEXIONES", 20)?,
            bind_addr: var_o("BIND_ADDR", SocketAddr::from(([0, 0, 0, 0], 8080)))?,
            jwt_secret,
            jwt_ttl_minutos: var_o("JWT_TTL_MINUTOS", 60)?,
            frontend_origin: var("FRONTEND_ORIGIN").unwrap_or_else(|| "http://localhost:3000".into()),
            modo_demo,
            migrar_al_inicio: var_o("MIGRAR_AL_INICIO", true)?,
            clabe_prefijo,
            pld: UmbralesPld {
                relevante: var_o("PLD_UMBRAL_RELEVANTE", defecto.relevante)?,
                riesgo_alto: var_o("PLD_UMBRAL_RIESGO_ALTO", defecto.riesgo_alto)?,
                multiplo_perfil: var_o("PLD_MULTIPLO_PERFIL", defecto.multiplo_perfil)?,
            },
            corte_intervalo_seg: var_o("CORTE_INTERVALO_SEG", 300)?,
            corte_min_intervalo_seg: var_o("CORTE_MIN_INTERVALO_SEG", 60)?,
            liquidacion_intervalo_seg: var_o("LIQUIDACION_INTERVALO_SEG", 3600)?,
            reserva_institucion: var("RESERVA_INSTITUCION").unwrap_or_else(|| "Banco de México".into()),
            reserva_cuenta: var("RESERVA_CUENTA").unwrap_or_else(|| "SC-IFPE-RESERVA-001".into()),
            caja_institucion: var("CAJA_INSTITUCION").unwrap_or_else(|| "Banco Custodio SOFOM".into()),
            caja_cuenta: var("CAJA_CUENTA").unwrap_or_else(|| "SC-SOFOM-CAJA-001".into()),
            operador_email: var("BOOTSTRAP_OPERADOR_EMAIL"),
            operador_password: var("BOOTSTRAP_OPERADOR_PASSWORD"),
            spei: {
                let d = ConfigSpei::default();
                let mut webhook_secretos = Vec::new();
                for nombre in ["SPEI_WEBHOOK_SECRETO", "SPEI_WEBHOOK_SECRETO_ANTERIOR"] {
                    if let Some(s) = var(nombre) {
                        validar_secreto_jwt(&s).map_err(|e| anyhow::anyhow!("{nombre}: {e}"))?;
                        webhook_secretos.push(s);
                    }
                }
                let prefijo_rastreo = var("SPEI_PREFIJO_RASTREO").unwrap_or(d.prefijo_rastreo).to_ascii_uppercase();
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
                }
            },
        })
    }

    /// Configuración para pruebas: sin tareas de fondo ni migraciones al inicio.
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
            clabe_prefijo: "999180".into(),
            pld: UmbralesPld::default(),
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
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::validar_secreto_jwt;

    #[test]
    fn rechaza_secretos_debiles() {
        assert!(validar_secreto_jwt("corto").is_err());
        assert!(validar_secreto_jwt("change-me-change-me-change-me-change-me").is_err());
        assert!(validar_secreto_jwt(&"a".repeat(64)).is_err());
        assert!(validar_secreto_jwt("my-jwt-secret-for-the-bank-0123456789").is_err());
        assert!(validar_secreto_jwt("9f2c1e7ab04d5b6c8e3f1a2b7c9d0e4f6a8b1c3d5e7f9a0b").is_ok());
    }
}
