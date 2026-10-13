//! Synthetic Capital — banca con reserva del 100% (modelo rothbardiano).
//!
//! Capas:
//! - [`db`]: transacciones con rol de PostgreSQL por operación (`SET LOCAL ROLE`).
//! - [`ledger`]: motor de la IFPE (bóveda a la vista) y verificador de la cadena SHA-256.
//! - [`sofom`]: puente IFPE -> contratos de mutuo y originación de crédito con calce de plazos.
//! - [`crypto::merkle`]: árbol Merkle de sumas para Proof of Reserve.
//! - [`reserva`]: cortes de Proof of Reserve y pruebas de inclusión por cliente.
//! - [`routes`]: API HTTP (Axum).
//!
//! Las reglas de negocio inviolables se hacen cumplir dos veces: aquí, con
//! errores claros, y en la base de datos (triggers, CHECKs y privilegios por
//! rol), que es la última palabra.

pub mod auth;
pub mod bootstrap;
pub mod clabe;
pub mod config;
pub mod crypto;
pub mod db;
pub mod dinero;
pub mod error;
pub mod kyc;
pub mod ledger;
pub mod models;
pub mod pld;
pub mod reserva;
pub mod routes;
pub mod servidor;
pub mod sofom;
pub mod state;
pub mod tareas;

pub use error::{AppError, AppResult};
pub use state::AppState;
