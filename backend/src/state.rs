use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    auth::jwt::ClavesJwt,
    config::Config,
    kyc::biometria::{ProveedorBiometrico, ProveedorSimulado},
    reserva::CacheCortes,
    spei::pasarela::{Pasarela, PasarelaSimulada},
};

/// Estado compartido por todos los handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub jwt: Arc<ClavesJwt>,
    /// Árbol Merkle verificado del último corte (una construcción por corte).
    pub cortes: Arc<CacheCortes>,
    /// Participante liquidador SPEI (STP). Por ahora, la pasarela simulada.
    pub pasarela: Arc<dyn Pasarela>,
    /// Proveedor biométrico (prueba de vida, comparación facial, OCR de la
    /// INE). Por ahora, el simulado.
    pub biometria: Arc<dyn ProveedorBiometrico>,
}

impl AppState {
    pub fn nuevo(pool: PgPool, config: Config) -> Self {
        Self::con_pasarela(pool, config, Arc::new(PasarelaSimulada::default()))
    }

    pub fn con_pasarela(pool: PgPool, config: Config, pasarela: Arc<dyn Pasarela>) -> Self {
        let jwt = ClavesJwt::nuevas(&config.jwt_secret, config.jwt_ttl_minutos);
        let biometria = Arc::new(ProveedorSimulado::nuevo(config.kyc.webhook_secretos[0].as_bytes()));
        Self { pool, config: Arc::new(config), jwt: Arc::new(jwt), cortes: Arc::default(), pasarela, biometria }
    }

    /// Sustituye el proveedor biométrico (pruebas).
    pub fn con_biometria(mut self, biometria: Arc<dyn ProveedorBiometrico>) -> Self {
        self.biometria = biometria;
        self
    }
}
