//! CLABE (Clave Bancaria Estandarizada): 18 dígitos.
//!
//! ```text
//! 646 180 1234567890 7
//! │   │   │          └ dígito verificador
//! │   │   └ número de cuenta (11 dígitos)
//! │   └ plaza (3 dígitos)
//! └ banco (3 dígitos; los últimos 3 de la clave de participante SPEI,
//!   p. ej. 012 = BBVA México (40012), 646 = STP (90646))
//! ```
//!
//! Dígito verificador (algoritmo oficial, módulo 10 ponderado): se multiplica
//! cada uno de los primeros 17 dígitos por su peso `3, 7, 1, 3, 7, 1, ...`, se
//! toma el módulo 10 de cada producto, se suman, y el dígito es
//! `(10 - suma mod 10) mod 10`. Es la misma regla que `nucleo.clabe_valida` en
//! PostgreSQL (lo comprueba una prueba de integración).

use std::fmt;

use rand::Rng;

/// Pesos oficiales de las posiciones 1 a 17.
pub const PESOS: [u8; 17] = [3, 7, 1, 3, 7, 1, 3, 7, 1, 3, 7, 1, 3, 7, 1, 3, 7];

/// Por qué una cadena no es una CLABE válida.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorClabe {
    #[error("la CLABE debe tener 18 dígitos (tiene {0})")]
    Longitud(usize),
    #[error("la CLABE solo admite dígitos")]
    NoNumerica,
    #[error("dígito verificador inválido: se esperaba {esperado} y tiene {recibido}")]
    DigitoVerificador { esperado: u8, recibido: u8 },
}

/// Dígito verificador de los primeros 17 dígitos. `None` si no son 17 dígitos.
pub fn digito_verificador(primeros_17: &str) -> Option<u8> {
    let b = primeros_17.as_bytes();
    if b.len() != 17 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let suma: u32 = b.iter().zip(PESOS).map(|(d, peso)| u32::from(((d - b'0') * peso) % 10)).sum();
    Some(((10 - suma % 10) % 10) as u8)
}

/// CLABE validada (18 dígitos con dígito verificador correcto).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Clabe(String);

impl Clabe {
    pub fn parse(texto: &str) -> Result<Self, ErrorClabe> {
        let b = texto.as_bytes();
        if !b.iter().all(u8::is_ascii_digit) {
            return Err(ErrorClabe::NoNumerica);
        }
        if b.len() != 18 {
            return Err(ErrorClabe::Longitud(b.len()));
        }
        let esperado = digito_verificador(&texto[..17]).expect("17 dígitos comprobados");
        let recibido = b[17] - b'0';
        if esperado != recibido {
            return Err(ErrorClabe::DigitoVerificador { esperado, recibido });
        }
        Ok(Self(texto.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Código de banco (3 dígitos).
    pub fn banco(&self) -> &str {
        &self.0[..3]
    }

    /// Plaza (3 dígitos).
    pub fn plaza(&self) -> &str {
        &self.0[3..6]
    }

    /// Número de cuenta (11 dígitos).
    pub fn cuenta(&self) -> &str {
        &self.0[6..17]
    }

    pub fn digito(&self) -> u8 {
        self.0.as_bytes()[17] - b'0'
    }
}

impl fmt::Display for Clabe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn es_valida(clabe: &str) -> bool {
    Clabe::parse(clabe).is_ok()
}

/// Prefijo para CLABEs propias: banco (3) + plaza (3) y, opcionalmente, hasta
/// 4 dígitos más (p. ej. el prefijo de empresa que asigna STP a cada cliente:
/// `646180` + `1234`). El resto de los 17 dígitos se llena al azar.
pub fn validar_prefijo(prefijo: &str) -> Result<(), String> {
    if !(6..=10).contains(&prefijo.len()) || !prefijo.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("el prefijo de CLABE debe tener de 6 a 10 dígitos (tiene {prefijo:?})"));
    }
    Ok(())
}

/// Genera una CLABE virtual con el prefijo dado, cuenta aleatoria y dígito
/// verificador. La unicidad la garantiza la base de datos (la apertura de
/// cuenta reintenta ante colisión).
pub fn generar(prefijo: &str) -> Clabe {
    debug_assert!(validar_prefijo(prefijo).is_ok(), "prefijo validado en Config");
    let faltan = 17 - prefijo.len();
    let mut rng = rand::thread_rng();
    let mut primeros = String::with_capacity(18);
    primeros.push_str(prefijo);
    for _ in 0..faltan {
        primeros.push(char::from(b'0' + rng.gen_range(0..10u8)));
    }
    let dv = digito_verificador(&primeros).expect("17 dígitos");
    primeros.push(char::from(b'0' + dv));
    Clabe(primeros)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn algoritmo_oficial_paso_a_paso() {
        // 032180000118359719 (ejemplo de referencia habitual):
        // productos módulo 10 de 0,3,2,1,8,0,0,0,0,1,1,8,3,5,9,7,1 por 3,7,1,...
        // = 0,1,2,3,6,0,0,0,0,3,7,8,9,5,9,1,7 -> suma 61 -> (10 - 1) % 10 = 9.
        assert_eq!(digito_verificador("03218000011835971"), Some(9));
        assert!(es_valida("032180000118359719"));
    }

    #[test]
    fn clabes_validas() {
        for c in [
            "032180000118359719",
            "646180157000000017",
            "646180157000000020",
            "002010077777777771",
            "012180001234567899",
        ] {
            assert!(es_valida(c), "{c}");
        }
    }

    #[test]
    fn digito_verificador_incorrecto() {
        assert_eq!(Clabe::parse("032180000118359710"), Err(ErrorClabe::DigitoVerificador { esperado: 9, recibido: 0 }));
        // Cambiar cualquier dígito de los 17 primeros invalida la CLABE.
        let base = "646180157000000017";
        for i in 0..17 {
            let mut b = base.as_bytes().to_vec();
            b[i] = if b[i] == b'9' { b'0' } else { b[i] + 1 };
            let alterada = String::from_utf8(b).unwrap();
            assert!(!es_valida(&alterada), "{alterada}");
        }
    }

    #[test]
    fn transposicion_de_digitos_adyacentes_se_detecta_casi_siempre() {
        // Intercambiar dos dígitos vecinos d, e cambia la suma en (d - e) por la
        // diferencia de sus pesos (4, 6 o 2): múltiplo de 10 solo si |d - e| = 5.
        let base = "646180157000000017";
        let b = base.as_bytes();
        for i in 0..16 {
            if b[i] != b[i + 1] && (i32::from(b[i]) - i32::from(b[i + 1])).abs() != 5 {
                let mut t = b.to_vec();
                t.swap(i, i + 1);
                assert!(!es_valida(std::str::from_utf8(&t).unwrap()), "transposición en {i}");
            }
        }
    }

    #[test]
    fn formato_invalido() {
        assert_eq!(Clabe::parse("64618015700000001"), Err(ErrorClabe::Longitud(17)));
        assert_eq!(Clabe::parse("6461801570000000171"), Err(ErrorClabe::Longitud(19)));
        assert_eq!(Clabe::parse("64618015700000001a"), Err(ErrorClabe::NoNumerica));
        assert_eq!(Clabe::parse("646 180 157000000017"), Err(ErrorClabe::NoNumerica));
        assert_eq!(Clabe::parse("６４６180157000000017"), Err(ErrorClabe::NoNumerica));
        assert_eq!(Clabe::parse(""), Err(ErrorClabe::Longitud(0)));
        assert_eq!(digito_verificador("1234"), None);
    }

    #[test]
    fn partes_de_la_clabe() {
        let c = Clabe::parse("646180157000000017").unwrap();
        assert_eq!((c.banco(), c.plaza(), c.cuenta(), c.digito()), ("646", "180", "15700000001", 7));
    }

    #[test]
    fn genera_clabes_validas_con_prefijos_de_6_a_10() {
        for prefijo in ["999180", "6461801", "64618012", "646180123", "6461801234"] {
            for _ in 0..500 {
                let c = generar(prefijo);
                assert!(c.as_str().starts_with(prefijo));
                assert!(es_valida(c.as_str()), "{c}");
            }
        }
        assert!(validar_prefijo("64618").is_err());
        assert!(validar_prefijo("64618012345").is_err());
        assert!(validar_prefijo("64618a").is_err());
    }
}
