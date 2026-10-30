//! CURP (Clave Única de Registro de Población, RENAPO).
//!
//! | Posición | Contenido |
//! |---|---|
//! | 1 | inicial del primer apellido |
//! | 2 | primera vocal interna del primer apellido |
//! | 3 | inicial del segundo apellido (X si no hay) |
//! | 4 | inicial del nombre (el segundo si el primero es MARÍA o JOSÉ) |
//! | 5-10 | fecha de nacimiento AAMMDD |
//! | 11 | sexo: H, M o X |
//! | 12-13 | entidad de nacimiento (NE: nacido en el extranjero) |
//! | 14-16 | primera consonante interna de cada apellido y del nombre |
//! | 17 | homoclave: dígito si nació antes de 2000, letra desde 2000 |
//! | 18 | dígito verificador (módulo 10 sobre las 17 anteriores) |
//!
//! Estructura, fecha, entidad y dígito verificador son errores. Que las
//! letras no correspondan al nombre capturado es una advertencia: hay reglas
//! de excepción (homonimias, nombres compuestos, asignación manual de RENAPO)
//! que no se pueden reproducir con certeza; esos casos van a revisión humana.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::texto;

/// Clave RENAPO, nombre y clave numérica INEGI (la que usa la clave de
/// elector) de cada entidad. NE no tiene clave INEGI.
pub const ENTIDADES: [(&str, &str, u8); 33] = [
    ("AS", "Aguascalientes", 1),
    ("BC", "Baja California", 2),
    ("BS", "Baja California Sur", 3),
    ("CC", "Campeche", 4),
    ("CL", "Coahuila", 5),
    ("CM", "Colima", 6),
    ("CS", "Chiapas", 7),
    ("CH", "Chihuahua", 8),
    ("DF", "Ciudad de México", 9),
    ("DG", "Durango", 10),
    ("GT", "Guanajuato", 11),
    ("GR", "Guerrero", 12),
    ("HG", "Hidalgo", 13),
    ("JC", "Jalisco", 14),
    ("MC", "Estado de México", 15),
    ("MN", "Michoacán", 16),
    ("MS", "Morelos", 17),
    ("NT", "Nayarit", 18),
    ("NL", "Nuevo León", 19),
    ("OC", "Oaxaca", 20),
    ("PL", "Puebla", 21),
    ("QT", "Querétaro", 22),
    ("QR", "Quintana Roo", 23),
    ("SP", "San Luis Potosí", 24),
    ("SL", "Sinaloa", 25),
    ("SR", "Sonora", 26),
    ("TC", "Tabasco", 27),
    ("TS", "Tamaulipas", 28),
    ("TL", "Tlaxcala", 29),
    ("VZ", "Veracruz", 30),
    ("YN", "Yucatán", 31),
    ("ZS", "Zacatecas", 32),
    ("NE", "Nacido en el extranjero", 0),
];

pub fn clave_inegi(entidad: &str) -> Option<u8> {
    ENTIDADES.iter().find(|(c, _, _)| *c == entidad).map(|(_, _, n)| *n).filter(|n| *n > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sexo {
    /// Hombre.
    H,
    /// Mujer.
    M,
    /// No binario.
    X,
}

impl Sexo {
    pub fn letra(self) -> char {
        match self {
            Sexo::H => 'H',
            Sexo::M => 'M',
            Sexo::X => 'X',
        }
    }

    pub fn desde_letra(c: char) -> Option<Self> {
        match c {
            'H' => Some(Sexo::H),
            'M' => Some(Sexo::M),
            'X' => Some(Sexo::X),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorCurp {
    #[error("la CURP debe tener 18 caracteres")]
    Longitud,
    #[error("la CURP no tiene la estructura oficial (4 letras, fecha AAMMDD, sexo, entidad, 3 consonantes, homoclave y dígito)")]
    Estructura,
    #[error("la fecha de nacimiento de la CURP no existe")]
    Fecha,
    #[error("la entidad de nacimiento {0} no existe")]
    Entidad(String),
    #[error("el dígito verificador de la CURP no corresponde")]
    DigitoVerificador,
}

/// CURP con estructura, fecha, entidad y dígito verificador válidos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Curp {
    valor: String,
    fecha_nacimiento: NaiveDate,
    sexo: Sexo,
}

const ALFABETO: &str = "0123456789ABCDEFGHIJKLMNÑOPQRSTUVWXYZ";

/// Dígito verificador de las 17 primeras posiciones.
pub fn digito_verificador(primeros17: &str) -> Option<char> {
    let mut suma = 0u32;
    let mut n = 0u32;
    for (i, c) in primeros17.chars().enumerate() {
        let valor = ALFABETO.chars().position(|a| a == c)? as u32;
        suma += valor * (18 - i as u32);
        n += 1;
    }
    if n != 17 {
        return None;
    }
    char::from_digit((10 - suma % 10) % 10, 10)
}

fn dos_digitos(b: &[u8]) -> u32 {
    u32::from(b[0] - b'0') * 10 + u32::from(b[1] - b'0')
}

impl Curp {
    pub fn parse(entrada: &str) -> Result<Self, ErrorCurp> {
        let valor = entrada.trim().to_uppercase();
        let b = valor.as_bytes();
        if valor.chars().count() != 18 || b.len() != 18 {
            return Err(ErrorCurp::Longitud);
        }
        let estructura = b[..4].iter().all(u8::is_ascii_uppercase)
            && b[4..10].iter().all(u8::is_ascii_digit)
            && matches!(b[10], b'H' | b'M' | b'X')
            && b[11..16].iter().all(u8::is_ascii_uppercase)
            && (b[16].is_ascii_uppercase() || b[16].is_ascii_digit())
            && b[17].is_ascii_digit();
        if !estructura {
            return Err(ErrorCurp::Estructura);
        }
        let siglo = if b[16].is_ascii_digit() { 1900 } else { 2000 };
        let anio = siglo + dos_digitos(&b[4..6]) as i32;
        let fecha_nacimiento =
            NaiveDate::from_ymd_opt(anio, dos_digitos(&b[6..8]), dos_digitos(&b[8..10])).ok_or(ErrorCurp::Fecha)?;
        let entidad = &valor[11..13];
        if !ENTIDADES.iter().any(|(c, _, _)| *c == entidad) {
            return Err(ErrorCurp::Entidad(entidad.to_owned()));
        }
        if digito_verificador(&valor[..17]) != Some(char::from(b[17])) {
            return Err(ErrorCurp::DigitoVerificador);
        }
        let sexo = Sexo::desde_letra(char::from(b[10])).ok_or(ErrorCurp::Estructura)?;
        Ok(Self { valor, fecha_nacimiento, sexo })
    }

    pub fn as_str(&self) -> &str {
        &self.valor
    }

    pub fn fecha_nacimiento(&self) -> NaiveDate {
        self.fecha_nacimiento
    }

    pub fn sexo(&self) -> Sexo {
        self.sexo
    }

    /// Clave RENAPO de la entidad de nacimiento (p. ej. "DF", "NE").
    pub fn entidad(&self) -> &str {
        &self.valor[11..13]
    }

    /// Diferencias entre las letras de la CURP y las que corresponden al
    /// nombre (advertencias, ver el encabezado del módulo).
    pub fn advertencias_de_nombre(&self, nombres: &str, paterno: &str, materno: Option<&str>) -> Vec<String> {
        let esperado = derivar_letras(nombres, paterno, materno);
        let real: String = self.valor[..4].chars().chain(self.valor[13..16].chars()).collect();
        if esperado == real {
            Vec::new()
        } else {
            vec![format!(
                "las letras de la CURP ({}…{}) no corresponden al nombre capturado (se esperaban {}…{})",
                &real[..4],
                &real[4..],
                &esperado[..4],
                &esperado[4..]
            )]
        }
    }
}

// -----------------------------------------------------------------------------
// Derivación de las letras a partir del nombre (Instructivo Normativo RENAPO)
// -----------------------------------------------------------------------------

/// Preposiciones, conjunciones y contracciones que se omiten en nombres y
/// apellidos compuestos.
const PARTICULAS: &[&str] = &[
    "DA", "DAS", "DE", "DEL", "DER", "DI", "DIE", "DD", "EL", "LA", "LOS", "LAS", "LE", "LES", "MAC", "MC", "VAN",
    "VON", "Y",
];

/// Si el nombre es compuesto y empieza con alguno de estos, se usa el segundo.
const NOMBRES_OMITIDOS: &[&str] = &["MARIA", "MA", "M", "JOSE", "J"];

/// Palabras altisonantes: la segunda letra se sustituye por X.
const INCONVENIENTES: &[&str] = &[
    "BACA", "BAKA", "BUEI", "BUEY", "CACA", "CACO", "CAGA", "CAGO", "CAKA", "CAKO", "COGE", "COGI", "COJA", "COJE",
    "COJI", "COJO", "COLA", "CULO", "FALO", "FETO", "GETA", "GUEI", "GUEY", "JETA", "JOTO", "KACA", "KACO", "KAGA",
    "KAGO", "KAKA", "KAKO", "KOGE", "KOGI", "KOJA", "KOJE", "KOJI", "KOJO", "KOLA", "KULO", "LILO", "LOCA", "LOCO",
    "LOKA", "LOKO", "MAME", "MAMO", "MEAR", "MEAS", "MEON", "MIAR", "MION", "MOCO", "MOKO", "MULA", "MULO", "NACA",
    "NACO", "PEDA", "PEDO", "PENE", "PIPI", "PITO", "POPO", "PUTA", "PUTO", "QULO", "RATA", "ROBA", "ROBE", "ROBO",
    "RUIN", "SENO", "TETA", "VACA", "VAGA", "VAGO", "VAKA", "VUEI", "VUEY",
];

fn sin_particulas(s: &str) -> Vec<String> {
    let todas = texto::palabras(s);
    let utiles: Vec<String> = todas.iter().filter(|p| !PARTICULAS.contains(&p.as_str())).cloned().collect();
    if utiles.is_empty() {
        todas
    } else {
        utiles
    }
}

pub(crate) fn apellido_util(s: &str) -> Option<String> {
    sin_particulas(s).into_iter().next()
}

fn nombre_util(s: &str) -> Option<String> {
    let p = sin_particulas(s);
    match p.as_slice() {
        [primero, segundo, ..] if NOMBRES_OMITIDOS.contains(&primero.as_str()) => Some(segundo.clone()),
        [primero, ..] => Some(primero.clone()),
        [] => None,
    }
}

fn es_vocal(c: char) -> bool {
    matches!(c, 'A' | 'E' | 'I' | 'O' | 'U')
}

fn letra(c: char) -> char {
    if c == 'Ñ' {
        'X'
    } else {
        c
    }
}

fn inicial(p: Option<&str>) -> char {
    p.and_then(|p| p.chars().next()).map_or('X', letra)
}

fn vocal_interna(p: Option<&str>) -> char {
    p.and_then(|p| p.chars().skip(1).find(|c| es_vocal(*c))).unwrap_or('X')
}

fn consonante_interna(p: Option<&str>) -> char {
    p.and_then(|p| p.chars().skip(1).find(|c| !es_vocal(*c))).map_or('X', letra)
}

/// Las 7 letras que dependen del nombre: posiciones 1-4 y 14-16.
pub fn derivar_letras(nombres: &str, paterno: &str, materno: Option<&str>) -> String {
    let pat = apellido_util(paterno);
    let mat = materno.and_then(apellido_util);
    let nom = nombre_util(nombres);
    let mut cuatro: Vec<char> =
        vec![inicial(pat.as_deref()), vocal_interna(pat.as_deref()), inicial(mat.as_deref()), inicial(nom.as_deref())];
    let palabra: String = cuatro.iter().collect();
    if INCONVENIENTES.contains(&palabra.as_str()) {
        cuatro[1] = 'X';
    }
    cuatro
        .into_iter()
        .chain([
            consonante_interna(pat.as_deref()),
            consonante_interna(mat.as_deref()),
            consonante_interna(nom.as_deref()),
        ])
        .collect()
}

/// Construye una CURP válida (estructura y dígito verificador) a partir de
/// los datos de una persona. La homoclave real la asigna RENAPO; aquí se
/// recibe. Útil para pruebas y para la demostración.
pub fn construir(
    nombres: &str,
    paterno: &str,
    materno: Option<&str>,
    fecha: NaiveDate,
    sexo: Sexo,
    entidad: &str,
    homoclave: u8,
) -> String {
    let letras = derivar_letras(nombres, paterno, materno);
    let diferenciador =
        if fecha.year() < 2000 { char::from(b'0' + homoclave % 10) } else { char::from(b'A' + homoclave % 26) };
    let primeros: String = format!(
        "{}{:02}{:02}{:02}{}{}{}{}",
        &letras[..4],
        fecha.year() % 100,
        fecha.month(),
        fecha.day(),
        sexo.letra(),
        entidad,
        &letras[4..],
        diferenciador
    );
    let dv = digito_verificador(&primeros).expect("17 caracteres del alfabeto CURP");
    format!("{primeros}{dv}")
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn digito_verificador_de_curps_reales() {
        for c in ["LOOA531113HTCPBN07", "HEGG560427MVZRRL04", "MAHJ280603MSPRRV09"] {
            assert_eq!(digito_verificador(&c[..17]), c.chars().last(), "{c}");
            assert!(Curp::parse(c).is_ok(), "{c}");
        }
    }

    #[test]
    fn rechaza_curps_invalidas() {
        assert_eq!(Curp::parse("LOOA531113HTCPBN08"), Err(ErrorCurp::DigitoVerificador));
        assert_eq!(Curp::parse("LOOA531113HTCPBN0"), Err(ErrorCurp::Longitud));
        assert_eq!(Curp::parse("LOOA531113ZTCPBN07"), Err(ErrorCurp::Estructura));
        assert_eq!(Curp::parse("LOOA531313HTCPBN07"), Err(ErrorCurp::Fecha));
        assert!(matches!(Curp::parse("LOOA531113HZZPBN07"), Err(ErrorCurp::Entidad(_))));
        // 29 de febrero de 1900 no existe (1900 no fue bisiesto).
        let sin_dv = "AAAA000229HDFAAA0";
        let c = format!("{sin_dv}{}", digito_verificador(sin_dv).unwrap());
        assert_eq!(Curp::parse(&c), Err(ErrorCurp::Fecha));
    }

    #[test]
    fn interpreta_fecha_sexo_y_entidad() {
        let c = Curp::parse(" looa531113htcpbn07 ").unwrap();
        assert_eq!(c.fecha_nacimiento(), NaiveDate::from_ymd_opt(1953, 11, 13).unwrap());
        assert_eq!(c.sexo(), Sexo::H);
        assert_eq!(c.entidad(), "TC");
        // Homoclave con letra: nacido desde 2000.
        let sin_dv = "GAXP050101MDFRXRA";
        let c = Curp::parse(&format!("{sin_dv}{}", digito_verificador(sin_dv).unwrap())).unwrap();
        assert_eq!(c.fecha_nacimiento().year(), 2005);
    }

    #[test]
    fn deriva_las_letras_del_nombre() {
        assert_eq!(derivar_letras("Andrés Manuel", "López", Some("Obrador")), "LOOAPBN");
        assert_eq!(derivar_letras("Gloria", "Hernández", Some("García")), "HEGGRRL");
        // MARÍA se omite en nombres compuestos; partículas fuera.
        assert_eq!(derivar_letras("María Guadalupe", "Pérez", Some("de la Cruz")), "PECGRRD");
        // Sin segundo apellido: X.
        assert_eq!(derivar_letras("Ana", "Ruiz", None), "RUXAZXN");
        // Ñ cuenta como X.
        assert_eq!(derivar_letras("Íñigo", "Muñoz", Some("Ñuñez")), "MUXIXXX");
        // Palabra inconveniente: BUEY -> BXEY.
        assert_eq!(&derivar_letras("Yolanda", "Buendía", Some("Esparza"))[..4], "BXEY");
    }

    #[test]
    fn construye_curps_validas_y_detecta_nombres_ajenos() {
        let fecha = NaiveDate::from_ymd_opt(1990, 7, 15).unwrap();
        let c = construir("Ludwig", "Von Mises", None, fecha, Sexo::H, "DF", 3);
        let curp = Curp::parse(&c).unwrap();
        assert_eq!(curp.fecha_nacimiento(), fecha);
        assert!(curp.advertencias_de_nombre("Ludwig", "Von Mises", None).is_empty());
        assert_eq!(curp.advertencias_de_nombre("Murray", "Rothbard", None).len(), 1);
    }
}
