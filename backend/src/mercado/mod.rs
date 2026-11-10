//! Mercado de valores (migraciones 0013 y 0014): catálogo, cotizaciones y
//! órdenes de mercado.
//!
//! - [`catalogo`]: los activos que hoy se cotizan y se operan
//!   (`GET /api/v1/market/assets`), el explorador del frontend.
//! - [`cotizar`]: el activo del catálogo y su historial de precios
//!   (`GET /api/v1/market/quote/{ticker}`).
//! - [`colocar`]: una orden de compra o venta que se ejecuta completa al
//!   precio vigente (`POST /api/v1/market/order`); ver [`orden`].
//!
//! El mercado es una entidad aparte de la IFPE y la SOFOM: su propio esquema,
//! su rol (`sc_mercado`) y su ledger encadenado (`mercado.ordenes`). El
//! efectivo de una orden entra o sale de la bóveda como un movimiento del
//! ledger IFPE en la misma transacción (COMPRA_MERCADO / VENTA_MERCADO), así
//! que la reserva sigue al 100 % y el WebSocket de la bóveda avisa al
//! confirmar (la versión de la cuenta sube, migración 0012).
//!
//! **Precios simulados.** Hasta que llegue el oráculo, el catálogo trae
//! precios SIMULADO y el historial se genera ([`historial`]). Fuera de
//! MODO_DEMO, un precio simulado no se cotiza ni se opera
//! (`503 MERCADO_NO_DISPONIBLE`): nadie compra en producción a un precio de
//! demostración.

pub mod calculo;
pub mod historial;
pub mod modelos;
mod orden;

use axum::http::StatusCode;
use chrono::Utc;
use sqlx::PgPool;

use self::historial::PuntoPrecio;
pub use self::modelos::*;
pub use self::orden::{colocar, ResultadoOrden, SolicitudOrden};
use crate::{
    config::Config,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
};

/// Como la restricción de `mercado.activos.ticker`: `[A-Z][A-Z0-9]{0,11}`.
pub fn ticker_valido(ticker: &str) -> bool {
    let mut letras = ticker.bytes();
    matches!(letras.next(), Some(b'A'..=b'Z'))
        && ticker.len() <= 12
        && letras.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn no_existe(ticker: &str) -> AppError {
    AppError::no_encontrado(format!("el activo {ticker} no existe"))
}

/// El activo, leído en la transacción dada.
async fn leer_activo(tx: &mut Tx, ticker: &str) -> AppResult<Activo> {
    sqlx::query_as("SELECT * FROM mercado.activos WHERE ticker = $1")
        .bind(ticker)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| no_existe(ticker))
}

/// Un precio simulado solo se cotiza, se lista y se opera con MODO_DEMO.
fn se_ofrece(activo: &Activo, config: &Config) -> bool {
    activo.fuente_precio != FuentePrecio::Simulado || config.modo_demo
}

fn mercado_no_disponible() -> AppError {
    AppError::peticion(
        StatusCode::SERVICE_UNAVAILABLE,
        "MERCADO_NO_DISPONIBLE",
        "el mercado abre cuando se conecte el oráculo de precios; hoy los precios son simulados",
    )
}

fn exigir_precio_real_o_demo(activo: &Activo, config: &Config) -> AppResult<()> {
    if se_ofrece(activo, config) {
        Ok(())
    } else {
        Err(mercado_no_disponible())
    }
}

/// El catálogo del explorador: cada activo que hoy se cotiza y se opera, por
/// ticker. Lee la tabla de la que cotizan y ejecutan las órdenes, así que todo
/// lo listado se puede abrir y comprar. Sin MODO_DEMO lista solo los precios
/// reales; si ninguno lo es, el mercado no ha abierto (503
/// `MERCADO_NO_DISPONIBLE`, como la cotización).
pub async fn catalogo(pool: &PgPool, config: &Config) -> AppResult<Catalogo> {
    let mut tx = db::begin_lectura(pool, Rol::Mercado).await?;
    let activos: Vec<Activo> =
        sqlx::query_as("SELECT * FROM mercado.activos ORDER BY ticker COLLATE \"C\"").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let listados: Vec<ActivoCatalogo> =
        activos.iter().filter(|a| se_ofrece(a, config)).map(ActivoCatalogo::from).collect();
    if listados.is_empty() && !config.modo_demo {
        return Err(mercado_no_disponible());
    }
    Ok(Catalogo { activos: listados, al: Utc::now() })
}

/// La cotización de un activo: su ficha, su historial y la hora del precio.
pub async fn cotizar(pool: &PgPool, config: &Config, ticker: &str) -> AppResult<Cotizacion> {
    if !ticker_valido(ticker) {
        return Err(no_existe(ticker));
    }
    let mut tx = db::begin_lectura(pool, Rol::Mercado).await?;
    let activo = leer_activo(&mut tx, ticker).await?;
    tx.commit().await?;
    exigir_precio_real_o_demo(&activo, config)?;

    let al = Utc::now();
    let historial = match activo.fuente_precio {
        FuentePrecio::Simulado => historial::simular(&activo.ticker, activo.precio, activo.precio_cierre_previo, al),
        // Sin historia guardada todavía: el precio del oráculo y su hora.
        FuentePrecio::Oraculo => {
            vec![PuntoPrecio { en_ms: activo.precio_en.timestamp_millis(), valor: dinero::con_escala(activo.precio) }]
        }
    };
    Ok(Cotizacion { activo: ActivoMercado::from(&activo), historial, al, fuente: activo.fuente_precio.etiqueta() })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn tickers_como_la_base_de_datos() {
        for bueno in ["AUG1", "AGT1", "SCT28", "A", "ABCDEFGHIJKL"] {
            assert!(ticker_valido(bueno), "{bueno}");
        }
        for malo in ["", "aug1", "1AUG", "AU-G1", "ABCDEFGHIJKLM", "AUG1 ", "ÁUG1", "../x"] {
            assert!(!ticker_valido(malo), "{malo}");
        }
    }
}
