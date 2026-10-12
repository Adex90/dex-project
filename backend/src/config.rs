//! Configuración desde variables de entorno.

use std::{env, net::SocketAddr, str::FromStr};

use anyhow::{bail, Context};
use rust_decimal::Decimal;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub max_conexiones: u32,
    pub bind_addr: SocketAddr,
    pub jwt_secret: String,
    pub jwt_ttl_minutos: i64,
    pub frontend_origin: String,
    /// Habilita atajos de demostración: KYC automático al registrarse y
    /// depósitos simulados por el propio cliente. Nunca en producción.
    pub modo_demo: bool,
    pub migrar_al_inicio: bool,
    /// Banco (3) + plaza (3) para generar CLABEs.
    pub clabe_prefijo: String,
    pub pld: UmbralesPld,
    /// Periodicidad de cortes automáticos de Proof of Reserve (0 = desactivado).
    pub corte_intervalo_seg: u64,
    /// Periodicidad de liquidación automática de mutuos vencidos (0 = desactivado).
    pub liquidacion_intervalo_seg: u64,
    pub reserva_institucion: String,
    pub reserva_cuenta: String,
    pub caja_institucion: String,
    pub caja_cuenta: String,
    pub operador_email: Option<String>,
    pub operador_password: Option<String>,
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

impl Config {
    pub fn desde_entorno() -> anyhow::Result<Self> {
        let modo_demo = var_o("MODO_DEMO", false)?;
        let jwt_secret = var("JWT_SECRET").context("JWT_SECRET es obligatoria")?;
        if jwt_secret.len() < 32 && !modo_demo {
            bail!("JWT_SECRET debe tener al menos 32 caracteres (genera una con: openssl rand -hex 64)");
        }
        let clabe_prefijo = var("CLABE_PREFIJO").unwrap_or_else(|| "999180".into());
        if clabe_prefijo.len() != 6 || !clabe_prefijo.bytes().all(|b| b.is_ascii_digit()) {
            bail!("CLABE_PREFIJO debe ser de 6 dígitos (banco + plaza)");
        }
        let defecto = UmbralesPld::default();
        Ok(Self {
            database_url: var("DATABASE_URL").context("DATABASE_URL es obligatoria")?,
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
            liquidacion_intervalo_seg: var_o("LIQUIDACION_INTERVALO_SEG", 3600)?,
            reserva_institucion: var("RESERVA_INSTITUCION").unwrap_or_else(|| "Banco de México".into()),
            reserva_cuenta: var("RESERVA_CUENTA").unwrap_or_else(|| "SC-IFPE-RESERVA-001".into()),
            caja_institucion: var("CAJA_INSTITUCION").unwrap_or_else(|| "Banco Custodio SOFOM".into()),
            caja_cuenta: var("CAJA_CUENTA").unwrap_or_else(|| "SC-SOFOM-CAJA-001".into()),
            operador_email: var("BOOTSTRAP_OPERADOR_EMAIL"),
            operador_password: var("BOOTSTRAP_OPERADOR_PASSWORD"),
        })
    }

    /// Configuración para pruebas: sin tareas de fondo ni migraciones al inicio.
    pub fn para_pruebas(database_url: impl Into<String>) -> Self {
        Self {
            database_url: database_url.into(),
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
            liquidacion_intervalo_seg: 0,
            reserva_institucion: "Banco de México".into(),
            reserva_cuenta: "SC-IFPE-RESERVA-001".into(),
            caja_institucion: "Banco Custodio SOFOM".into(),
            caja_cuenta: "SC-SOFOM-CAJA-001".into(),
            operador_email: None,
            operador_password: None,
        }
    }
}
