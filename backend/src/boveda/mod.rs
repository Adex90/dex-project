//! Bóveda Soberana: la vista del cliente en el dashboard (`GET /api/v1/vault`)
//! y su canal en tiempo real (`GET /api/v1/vault/ws`). El contrato JSON es el
//! de frontend/src/types/vault.ts.
//!
//! - [`modelos`]: lo que viaja, con los nombres en inglés del contrato.
//! - [`consulta`]: lecturas a nombre del cliente (rol `sc_cliente`, RLS).
//! - [`central`]: los sockets abiertos en esta tarea y los avisos de
//!   PostgreSQL (`LISTEN sc_boveda`, migración 0012) que los alimentan.
//! - [`socket`]: una conexión: foto inicial, cambios en orden de versión,
//!   resincronización, latido y vida de la sesión.

pub mod central;
pub mod consulta;
pub mod modelos;
pub mod socket;

pub use central::CentralBoveda;
