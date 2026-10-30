//! Montos: decimales exactos con escala 8, igual que `NUMERIC(18, 8)`.
//! Nunca se usa punto flotante para dinero.

use rust_decimal::Decimal;

use crate::error::AppError;

/// Decimales de todo monto almacenado.
pub const ESCALA: u32 = 8;

/// Máximo representable en `NUMERIC(18, 8)`: 9 999 999 999.99999999.
pub fn maximo() -> Decimal {
    Decimal::from_i128_with_scale(999_999_999_999_999_999, ESCALA)
}

/// Valida un monto de operación y lo normaliza a escala 8.
/// Rechaza (en vez de redondear) montos con más de 8 decimales.
pub fn validar_monto(monto: Decimal) -> Result<Decimal, AppError> {
    if monto <= Decimal::ZERO {
        return Err(AppError::validacion("el monto debe ser mayor a cero"));
    }
    let normal = monto.normalize();
    if normal.scale() > ESCALA {
        return Err(AppError::validacion("el monto admite como máximo 8 decimales"));
    }
    if normal > maximo() {
        return Err(AppError::validacion("el monto excede el máximo permitido"));
    }
    Ok(con_escala(normal))
}

/// Copia del monto con escala exactamente 8 (sin perder precisión si escala <= 8).
pub fn con_escala(monto: Decimal) -> Decimal {
    let mut m = monto;
    m.rescale(ESCALA);
    m
}

/// Representación canónica: idéntica a `NUMERIC(18, 8)::text` en PostgreSQL.
pub fn canonico(monto: Decimal) -> String {
    con_escala(monto).to_string()
}

/// Monto como entero de unidades de 10^-8 (para el árbol Merkle de sumas).
/// `None` si es negativo o no cabe.
pub fn a_unidades(monto: Decimal) -> Option<u128> {
    let m = con_escala(monto);
    if m.is_sign_negative() && !m.is_zero() {
        return None;
    }
    u128::try_from(m.mantissa()).ok()
}

/// Como [`a_unidades`], para montos que llegan de fuera (una prueba de
/// inclusión que envía un tercero): rechaza en vez de redondear. Un valor con
/// más de 8 decimales o negativo no es un saldo comprometido en el árbol.
pub fn a_unidades_estricto(monto: Decimal) -> Option<u128> {
    if monto.is_sign_negative() && !monto.is_zero() {
        return None;
    }
    if monto.normalize().scale() > ESCALA {
        return None;
    }
    a_unidades(monto)
}

/// Inverso de [`a_unidades`]. `None` si excede la mantisa de 96 bits de `Decimal`.
pub fn desde_unidades(unidades: u128) -> Option<Decimal> {
    let mantisa = i128::try_from(unidades).ok()?;
    Decimal::try_from_i128_with_scale(mantisa, ESCALA).ok()
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn canonico_igual_que_postgres() {
        assert_eq!(canonico(d("1000")), "1000.00000000");
        assert_eq!(canonico(d("0.5")), "0.50000000");
        assert_eq!(canonico(d("12.345678900")), "12.34567890");
    }

    #[test]
    fn validacion_de_montos() {
        assert!(validar_monto(d("0")).is_err());
        assert!(validar_monto(d("-1")).is_err());
        assert!(validar_monto(d("0.000000001")).is_err());
        assert_eq!(validar_monto(d("1.100000000")).unwrap().to_string(), "1.10000000");
        assert!(validar_monto(d("9999999999.99999999")).is_ok());
        assert!(validar_monto(d("10000000000")).is_err());
    }

    #[test]
    fn unidades_ida_y_vuelta() {
        assert_eq!(a_unidades(d("1.5")), Some(150_000_000));
        assert_eq!(a_unidades(d("-1")), None);
        assert_eq!(a_unidades(Decimal::ZERO), Some(0));
        assert_eq!(desde_unidades(150_000_000).unwrap(), d("1.5"));
    }

    #[test]
    fn unidades_estrictas_no_redondean() {
        assert_eq!(a_unidades_estricto(d("10000202.10000000")), Some(1_000_020_210_000_000));
        assert_eq!(a_unidades_estricto(d("1.500000000000")), Some(150_000_000));
        assert_eq!(a_unidades_estricto(d("10000202.100000004")), None);
        assert_eq!(a_unidades_estricto(d("-0.000000001")), None);
        assert_eq!(a_unidades_estricto(d("-1")), None);
        assert_eq!(a_unidades_estricto(Decimal::ZERO), Some(0));
        // El redondeo sigue disponible solo para valores ya tipados por la base de datos.
        assert_eq!(a_unidades(d("-0.000000001")), Some(0));
    }
}
