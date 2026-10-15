//! Plazos de los contratos de mutuo y su tasa anual.
//!
//! El interés se calcula con convención ACT/360 (estándar del mercado
//! mexicano), redondeado al centavo con redondeo bancario.

use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct PlazoTasa {
    pub plazo_dias: i32,
    pub tasa_anual: Decimal,
    pub etiqueta: &'static str,
}

/// Plazos admitidos. Tasas de ejemplo; en producción vendrían de tesorería.
pub fn tabla() -> [PlazoTasa; 5] {
    [
        PlazoTasa { plazo_dias: 28, tasa_anual: Decimal::new(450, 4), etiqueta: "28 días" },
        PlazoTasa { plazo_dias: 91, tasa_anual: Decimal::new(600, 4), etiqueta: "91 días" },
        PlazoTasa { plazo_dias: 182, tasa_anual: Decimal::new(725, 4), etiqueta: "182 días" },
        PlazoTasa { plazo_dias: 364, tasa_anual: Decimal::new(850, 4), etiqueta: "364 días" },
        PlazoTasa { plazo_dias: 728, tasa_anual: Decimal::new(900, 4), etiqueta: "728 días" },
    ]
}

pub fn tasa_para(plazo_dias: i32) -> Option<Decimal> {
    tabla().iter().find(|p| p.plazo_dias == plazo_dias).map(|p| p.tasa_anual)
}

/// Interés simple ACT/360 al vencimiento, en centavos.
pub fn interes(principal: Decimal, tasa_anual: Decimal, plazo_dias: i32) -> Decimal {
    (principal * tasa_anual * Decimal::from(plazo_dias) / Decimal::from(360))
        .round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn interes_act_360() {
        // 10,000 al 8.5% por 364 días = 859.444... -> 859.44
        assert_eq!(interes(Decimal::from(10_000), Decimal::from_str("0.085").unwrap(), 364).to_string(), "859.44");
        assert_eq!(interes(Decimal::from(1_000), Decimal::from_str("0.045").unwrap(), 28).to_string(), "3.50");
        assert_eq!(interes(Decimal::from(100), Decimal::ZERO, 28), Decimal::ZERO);
    }

    #[test]
    fn solo_plazos_de_la_tabla() {
        assert!(tasa_para(28).is_some());
        assert!(tasa_para(29).is_none());
    }
}
