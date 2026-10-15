//! Integración SPEI (Sistema de Pagos Electrónicos Interbancarios de Banxico)
//! a través de un participante liquidador como STP.
//!
//! - [`clabe`]: validación (dígito verificador módulo 10) y generación de CLABEs.
//! - [`firma`]: firma HMAC-SHA256 de los webhooks del proveedor.
//! - [`inbound`]: SPEI recibidos (webhook y captura manual del operador).
//! - [`outbound`]: órdenes de envío, despachador y reversos.
//! - [`pasarela`]: interfaz con el proveedor y una implementación simulada.
//!
//! Todo SPEI mueve exclusivamente la bóveda IFPE y su reserva en custodia; la
//! SOFOM no participa (migración `0005_spei_integration.sql`). Las reglas que
//! protegen el dinero viven en la base de datos: ningún abono `spei:` sin su
//! registro en `ifpe.spei_inbound`, ningún RETIRO sin su orden en
//! `ifpe.spei_outbound`, y la reserva del 100% al COMMIT.

pub mod clabe;
pub mod firma;
pub mod inbound;
pub mod outbound;
pub mod pasarela;

use chrono::NaiveDate;

use crate::error::{AppError, AppResult};

/// Fecha calendario en la Ciudad de México (UTC-6 todo el año desde 2022).
pub fn hoy_cdmx() -> NaiveDate {
    (chrono::Utc::now() - chrono::Duration::hours(6)).date_naive()
}

/// Días hacia adelante que puede tener una fecha de operación: un pago en
/// fin de semana o tras el corte lleva el siguiente día hábil (con puentes,
/// hasta 4 días después).
pub const DIAS_FECHA_OPERACION_FUTURA: u64 = 5;

/// Identidad de un pago SPEI. Banxico exige que la clave de rastreo sea única
/// por participante emisor y día de operación (el CEP pide los tres), y se
/// compara sin distinguir mayúsculas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentidadSpei {
    /// Clave de participante SPEI del emisor (5 dígitos, p. ej. 40012).
    pub institucion_emisora: String,
    pub fecha_operacion: NaiveDate,
    pub clave_rastreo: String,
}

impl IdentidadSpei {
    /// Normaliza y valida: emisor de 5 dígitos, rastreo alfanumérico de 1 a 30
    /// caracteres en mayúsculas, fecha razonable.
    pub fn nueva(institucion_emisora: &str, fecha_operacion: NaiveDate, clave_rastreo: &str) -> AppResult<Self> {
        let emisor = institucion_emisora.trim();
        if emisor.len() != 5 || !emisor.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AppError::validacion(
                "la institución emisora debe ser la clave de participante SPEI de 5 dígitos (p. ej. 40012)",
            ));
        }
        let rastreo = clave_rastreo.trim().to_ascii_uppercase();
        if rastreo.is_empty() || rastreo.len() > 30 || !rastreo.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(AppError::validacion("la clave de rastreo SPEI debe tener de 1 a 30 caracteres alfanuméricos"));
        }
        let maxima = hoy_cdmx() + chrono::Days::new(DIAS_FECHA_OPERACION_FUTURA);
        let minima = NaiveDate::from_ymd_opt(2000, 1, 1).expect("fecha válida");
        if fecha_operacion > maxima || fecha_operacion < minima {
            return Err(AppError::validacion("fecha de operación SPEI fuera de rango"));
        }
        Ok(Self { institucion_emisora: emisor.to_owned(), fecha_operacion, clave_rastreo: rastreo })
    }

    /// Forma canónica `<emisor>:<AAAAMMDD>:<RASTREO>` (igual a
    /// `ifpe.identidad_spei`). Va en `referencia_externa` del DEPOSITO (dentro
    /// del hash de la cadena) y en su clave de idempotencia `spei:<canónica>`.
    pub fn canonica(&self) -> String {
        format!("{}:{}:{}", self.institucion_emisora, self.fecha_operacion.format("%Y%m%d"), self.clave_rastreo)
    }
}

/// Monto SPEI: positivo, en centavos (2 decimales) y dentro del máximo de la
/// columna. Se rechaza en lugar de redondear.
pub fn validar_monto_spei(monto: rust_decimal::Decimal) -> AppResult<rust_decimal::Decimal> {
    let monto = crate::dinero::validar_monto(monto)?;
    if monto.normalize().scale() > 2 {
        return Err(AppError::validacion("SPEI liquida en centavos: el monto admite como máximo 2 decimales"));
    }
    Ok(monto)
}

/// Texto para un campo SPEI saliente (concepto, nombre del beneficiario): los
/// participantes solo aceptan letras sin acento, dígitos, espacio y `. , / -`.
/// Los acentos y la ñ del español se transliteran; cualquier otro carácter se
/// rechaza (un rechazo del participante llegaría tarde, con el dinero ya
/// comprometido).
pub fn texto_spei(campo: &str, valor: &str, max: usize) -> AppResult<String> {
    let mut salida = String::with_capacity(valor.len());
    for c in valor.trim().chars() {
        let t = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'Á' | 'À' | 'Ä' | 'Â' => 'A',
            'É' | 'È' | 'Ë' | 'Ê' => 'E',
            'Í' | 'Ì' | 'Ï' | 'Î' => 'I',
            'Ó' | 'Ò' | 'Ö' | 'Ô' => 'O',
            'Ú' | 'Ù' | 'Ü' | 'Û' => 'U',
            'ñ' => 'n',
            'Ñ' => 'N',
            c if c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ',' | '/' | '-') => c,
            otro => {
                return Err(AppError::validacion(format!(
                    "{campo}: el carácter {otro:?} no se admite en SPEI (letras, dígitos, espacio y . , / -)"
                )))
            }
        };
        // Sin espacios repetidos.
        if !(t == ' ' && salida.ends_with(' ')) {
            salida.push(t);
        }
    }
    if salida.is_empty() {
        return Err(AppError::validacion(format!("{campo} es obligatorio")));
    }
    if salida.chars().count() > max {
        return Err(AppError::validacion(format!("{campo} admite como máximo {max} caracteres en SPEI")));
    }
    Ok(salida)
}

/// Causas de devolución del catálogo de Banxico que usa este sistema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausaDevolucion {
    /// 1: cuenta inexistente.
    CuentaInexistente = 1,
    /// 2: cuenta bloqueada.
    CuentaBloqueada = 2,
    /// 3: cuenta cancelada.
    CuentaCancelada = 3,
    /// 14: falta información mandatoria para completar el pago.
    FaltaInformacion = 14,
}

impl CausaDevolucion {
    pub fn codigo(self) -> i16 {
        self as i16
    }

    pub fn descripcion(self) -> &'static str {
        match self {
            CausaDevolucion::CuentaInexistente => "cuenta inexistente",
            CausaDevolucion::CuentaBloqueada => "cuenta bloqueada",
            CausaDevolucion::CuentaCancelada => "cuenta cancelada",
            CausaDevolucion::FaltaInformacion => "falta información mandatoria",
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn identidad_canonica() {
        let f = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let id = IdentidadSpei::nueva(" 40012 ", f, " mban0100261 ").unwrap();
        assert_eq!(id.canonica(), "40012:20261001:MBAN0100261");
        assert!(IdentidadSpei::nueva("012", f, "X1").is_err(), "3 dígitos es código de banco, no de participante");
        assert!(IdentidadSpei::nueva("40012", f, "CON-GUION").is_err());
        assert!(IdentidadSpei::nueva("40012", f, &"A".repeat(31)).is_err());
        assert!(IdentidadSpei::nueva("40012", NaiveDate::from_ymd_opt(1999, 1, 1).unwrap(), "A").is_err());
        // Pago del sábado con fecha de operación del martes siguiente a un puente.
        assert!(IdentidadSpei::nueva("40012", hoy_cdmx() + chrono::Days::new(4), "A").is_ok());
        assert!(IdentidadSpei::nueva("40012", hoy_cdmx() + chrono::Days::new(30), "A").is_err());
    }

    #[test]
    fn montos_en_centavos() {
        let d = |s| rust_decimal::Decimal::from_str(s).unwrap();
        assert_eq!(validar_monto_spei(d("1500.5")).unwrap(), d("1500.50000000"));
        assert!(validar_monto_spei(d("0.001")).is_err());
        assert!(validar_monto_spei(d("0")).is_err());
        assert!(validar_monto_spei(d("-1")).is_err());
    }

    #[test]
    fn texto_transliterado() {
        assert_eq!(texto_spei("concepto", "  Pago  de  Mamá Ñandú ", 40).unwrap(), "Pago de Mama Nandu");
        assert!(texto_spei("concepto", "renta #3", 40).is_err());
        assert!(texto_spei("concepto", "   ", 40).is_err());
        assert!(texto_spei("concepto", &"a".repeat(41), 40).is_err());
    }
}
