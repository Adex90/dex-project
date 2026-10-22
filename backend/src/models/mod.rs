//! Modelos que mapean las tablas de los esquemas `kyc`, `ifpe` y `sofom`.
//!
//! Las columnas `TEXT` con dominio cerrado (CHECK ... IN (...)) se mapean a
//! enums de Rust; los montos `NUMERIC(18, 8)` a `rust_decimal::Decimal`.

pub mod ifpe;
pub mod kyc;
pub mod sofom;

use serde::{Deserialize, Serialize};

/// Entidad regulada a la que pertenece un registro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Entidad {
    Ifpe,
    Sofom,
}

impl Entidad {
    pub const fn as_str(self) -> &'static str {
        match self {
            Entidad::Ifpe => "IFPE",
            Entidad::Sofom => "SOFOM",
        }
    }
}

/// Estado de una cuenta (IFPE o SOFOM).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EstadoCuenta {
    Activa,
    Congelada,
    Cerrada,
    /// Congelamiento preventivo PLD (solo `ifpe.cuentas`, espejo de un
    /// congelamiento ACTIVO en `aml.congelamientos`). Se lee y escribe en la
    /// base de datos como `CONGELADO_PREVENTIVO`, pero **toda** proyección JSON
    /// lo muestra como `ACTIVA`: el cliente nunca debe enterarse (sin aviso).
    #[serde(rename(serialize = "ACTIVA", deserialize = "CONGELADO_PREVENTIVO"))]
    CongeladoPreventivo,
}
