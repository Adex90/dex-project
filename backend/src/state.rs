use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    auth::{correo::BuzonSimulado, jwt::ClavesJwt},
    config::Config,
    crypto::cifrado::Cifrador,
    kyc::biometria::{ProveedorBiometrico, ProveedorNoConfigurado, ProveedorSimulado},
    reserva::CacheCortes,
    spei::pasarela::{Pasarela, PasarelaSimulada},
};

/// Estado compartido por todos los handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub jwt: Arc<ClavesJwt>,
    /// Cifrado de los secretos TOTP (AES-256-GCM, `MFA_CLAVE_CIFRADO`).
    pub mfa: Arc<Cifrador>,
    /// Correo saliente simulado (restablecimiento de contraseña).
    pub correo: Arc<BuzonSimulado>,
    /// Árbol Merkle verificado del último corte (una construcción por corte).
    pub cortes: Arc<CacheCortes>,
    /// Participante liquidador SPEI (STP). Por ahora, la pasarela simulada.
    pub pasarela: Arc<dyn Pasarela>,
    /// Proveedor biométrico (prueba de vida, comparación facial, OCR de la
    /// INE). El simulado solo en MODO_DEMO: fuera de él, mientras no haya un
    /// proveedor certificado integrado, el onboarding responde 503 (falla
    /// cerrado) y la base de datos rechaza resultados del simulado.
    pub biometria: Arc<dyn ProveedorBiometrico>,
}

impl AppState {
    pub fn nuevo(pool: PgPool, config: Config) -> Self {
        Self::con_pasarela(pool, config, Arc::new(PasarelaSimulada::default()))
    }

    pub fn con_pasarela(pool: PgPool, config: Config, pasarela: Arc<dyn Pasarela>) -> Self {
        let jwt = ClavesJwt::nuevas(&config.jwt_secret, config.jwt_ttl_minutos, config.jwt_ttl_personal_minutos);
        let clave = config.mfa.clave.as_ref().expect("Config::desde_entorno exige MFA_CLAVE_CIFRADO para el servidor");
        let mfa = Arc::new(Cifrador::nuevo(clave, config.mfa.clave_anterior.as_ref()));
        let correo = Arc::new(BuzonSimulado::nuevo(config.modo_demo));
        let biometria: Arc<dyn ProveedorBiometrico> = if config.modo_demo {
            Arc::new(ProveedorSimulado::nuevo(config.kyc.webhook_secretos[0].as_bytes()))
        } else {
            Arc::new(ProveedorNoConfigurado)
        };
        Self {
            pool,
            config: Arc::new(config),
            jwt: Arc::new(jwt),
            mfa,
            correo,
            cortes: Arc::default(),
            pasarela,
            biometria,
        }
    }

    /// Sustituye el proveedor biométrico (pruebas).
    pub fn con_biometria(mut self, biometria: Arc<dyn ProveedorBiometrico>) -> Self {
        self.biometria = biometria;
        self
    }
}
