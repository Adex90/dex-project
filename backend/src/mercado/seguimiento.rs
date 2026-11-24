//! Lista de seguimiento (migración 0024): los activos que el cliente sigue,
//! los tenga o no. La estrella del explorador y de la ficha del activo los
//! agrega o los quita, y la bóveda muestra los que sigue sin tenerlos.
//!
//! - **Listar** ([`listar`], `GET /api/v1/market/watchlist`): con el rol del
//!   cliente y RLS, del más reciente al más antiguo, cada uno con su ficha
//!   como la lista el catálogo (`MarketListing`). Sin MODO_DEMO, los de precio
//!   simulado no se listan, como en el catálogo.
//! - **Seguir o dejar de seguir** ([`cambiar`],
//!   `POST /api/v1/market/watchlist/{ticker}/toggle`): con `{"watched": bool}`
//!   deja el activo en ese estado (repetirlo no cambia nada, así que un
//!   reintento es seguro); sin cuerpo, alterna. Rol `sc_mercado`: la cuenta
//!   IFPE del cliente primero (el primer candado del orden global; el trigger
//!   sube su versión y la bóveda avisa), y a lo más [`MAXIMO_SEGUIDOS`].
//!
//! No exige KYC ni se niega a una cuenta congelada: no mueve dinero, y negarlo
//! delataría el congelamiento.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{exigir_precio_real_o_demo, leer_activo, se_ofrece, ticker_valido, Activo, ActivoCatalogo};
use crate::{
    config::Config,
    db::{self, Rol},
    error::{AppError, AppResult},
    ledger,
};

/// Activos que sigue un cliente, a lo más (`mercado.maximo_seguidos()`).
pub const MAXIMO_SEGUIDOS: i64 = 50;

/// Un activo seguido (`WatchlistItem`).
#[derive(Debug, Clone, Serialize)]
pub struct Seguido {
    pub ticker: String,
    #[serde(rename = "added_at")]
    pub agregado_en: DateTime<Utc>,
    /// Su ficha, como la del catálogo.
    #[serde(rename = "asset")]
    pub activo: ActivoCatalogo,
}

/// `GET /api/v1/market/watchlist` (`Watchlist`): del más reciente al más antiguo.
#[derive(Debug, Clone, Serialize)]
pub struct ListaSeguimiento {
    #[serde(rename = "items")]
    pub seguidos: Vec<Seguido>,
}

/// Lo que responde seguir o dejar de seguir (`WatchState`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EstadoSeguimiento {
    pub ticker: String,
    #[serde(rename = "watched")]
    pub seguido: bool,
    /// Desde cuándo lo sigue; null si no.
    #[serde(rename = "added_at")]
    pub agregado_en: Option<DateTime<Utc>>,
}

#[derive(FromRow)]
struct FilaSeguido {
    agregado_en: DateTime<Utc>,
    #[sqlx(flatten)]
    activo: Activo,
}

/// Los activos que sigue el cliente. Con su rol y RLS: solo los suyos.
pub async fn listar(pool: &PgPool, config: &Config, cliente_id: Uuid) -> AppResult<ListaSeguimiento> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let filas: Vec<FilaSeguido> = sqlx::query_as(
        "SELECT w.creado_en AS agregado_en, a.* FROM mercado.watchlist w \
           JOIN mercado.activos a ON a.ticker = w.ticker \
          WHERE w.cliente_id = $1 ORDER BY w.creado_en DESC, w.ticker COLLATE \"C\"",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let seguidos = filas
        .iter()
        .filter(|f| se_ofrece(&f.activo, config))
        .map(|f| Seguido {
            ticker: f.activo.ticker.clone(),
            agregado_en: f.agregado_en,
            activo: ActivoCatalogo::from(&f.activo),
        })
        .collect();
    Ok(ListaSeguimiento { seguidos })
}

/// Sigue (`Some(true)`) o deja de seguir (`Some(false)`) el activo; sin
/// estado pedido, alterna. Devuelve cómo quedó.
pub async fn cambiar(
    pool: &PgPool,
    config: &Config,
    cliente_id: Uuid,
    ticker: &str,
    pedido: Option<bool>,
) -> AppResult<EstadoSeguimiento> {
    if !ticker_valido(ticker) {
        return Err(AppError::no_encontrado(format!("el activo {ticker} no existe")));
    }
    db::con_reintentos(|| cambiar_tx(pool, config, cliente_id, ticker, pedido)).await
}

async fn cambiar_tx(
    pool: &PgPool,
    config: &Config,
    cliente_id: Uuid,
    ticker: &str,
    pedido: Option<bool>,
) -> AppResult<EstadoSeguimiento> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    // La cuenta primero (orden global de candados): el trigger sube su versión.
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, cliente_id).await?;
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let desde: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT creado_en FROM mercado.watchlist WHERE cliente_id = $1 AND ticker = $2")
            .bind(cliente_id)
            .bind(ticker)
            .fetch_optional(&mut *tx)
            .await?;
    let seguir = pedido.unwrap_or(desde.is_none());
    let agregado_en = match (seguir, desde) {
        // Ya estaba así: nada que hacer.
        (true, Some(t)) => Some(t),
        (false, None) => None,
        (true, None) => {
            let activo = leer_activo(&mut tx, ticker).await?;
            exigir_precio_real_o_demo(&activo, config)?;
            let seguidos: i64 = sqlx::query_scalar("SELECT count(*) FROM mercado.watchlist WHERE cliente_id = $1")
                .bind(cliente_id)
                .fetch_one(&mut *tx)
                .await?;
            if seguidos >= MAXIMO_SEGUIDOS {
                return Err(AppError::negocio(
                    "LIMITE_SEGUIMIENTO",
                    format!("ya sigues {MAXIMO_SEGUIDOS} activos; deja de seguir alguno para agregar otro"),
                ));
            }
            let t: DateTime<Utc> = sqlx::query_scalar(
                "INSERT INTO mercado.watchlist (cliente_id, ticker) VALUES ($1, $2) RETURNING creado_en",
            )
            .bind(cliente_id)
            .bind(ticker)
            .fetch_one(&mut *tx)
            .await?;
            Some(t)
        }
        (false, Some(_)) => {
            sqlx::query("DELETE FROM mercado.watchlist WHERE cliente_id = $1 AND ticker = $2")
                .bind(cliente_id)
                .bind(ticker)
                .execute(&mut *tx)
                .await?;
            None
        }
    };
    tx.commit().await?;
    Ok(EstadoSeguimiento { ticker: ticker.to_string(), seguido: agregado_en.is_some(), agregado_en })
}
