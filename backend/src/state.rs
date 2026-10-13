use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    auth::jwt::ClavesJwt,
    config::Config,
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
}

impl AppState {
    pub fn nuevo(pool: PgPool, config: Config) -> Self {
        Self::con_pasarela(pool, config, Arc::new(PasarelaSimulada::default()))
    }

    pub fn con_pasarela(pool: PgPool, config: Config, pasarela: Arc<dyn Pasarela>) -> Self {
        let jwt = ClavesJwt::nuevas(&config.jwt_secret, config.jwt_ttl_minutos);
        Self { pool, config: Arc::new(config), jwt: Arc::new(jwt), cortes: Arc::default(), pasarela }
    }
}
