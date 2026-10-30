//! Normalización de nombres para cotejar identidades (CURP, RFC, INE, MRZ).

use unicode_normalization::UnicodeNormalization;

/// Mayúsculas, sin acentos ni diéresis, con cualquier signo (guion, punto,
/// apóstrofo, diagonal) convertido en espacio y los espacios colapsados. La
/// Ñ se conserva: cada regla decide qué hacer con ella. Primero compone el
/// texto (NFC): una "é" escrita como "e" + acento combinado es la misma letra.
pub fn normalizar(s: &str) -> String {
    let mapeado: String = s
        .nfc()
        .flat_map(char::to_uppercase)
        .map(|c| match c {
            'Á' | 'À' | 'Â' | 'Ä' | 'Ã' => 'A',
            'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
            'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => 'O',
            'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
            'Ç' => 'C',
            c if c.is_ascii_uppercase() || c == 'Ñ' => c,
            _ => ' ',
        })
        .collect();
    mapeado.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Palabras normalizadas.
pub fn palabras(s: &str) -> Vec<String> {
    normalizar(s).split(' ').filter(|p| !p.is_empty()).map(str::to_owned).collect()
}

/// Nombre en el alfabeto de la zona de lectura mecánica (ICAO 9303 parte 3):
/// sin acentos, Ñ como N, apóstrofos omitidos (O'FARRILL -> OFARRILL) y
/// espacios o guiones como `<`.
pub fn transliterar_mrz(s: &str) -> String {
    let sin_apostrofos: String = s.nfc().filter(|c| !matches!(c, '\'' | '\u{2019}' | '`' | '\u{00B4}')).collect();
    normalizar(&sin_apostrofos).replace('Ñ', "N").replace(' ', "<")
}

/// Igualdad de nombres tal como los escribe cada fuente. `estricta = false`
/// trata Ñ como N (la zona de lectura mecánica no tiene Ñ).
pub fn mismo_nombre(a: &str, b: &str, estricta: bool) -> bool {
    let (a, b) = (normalizar(a), normalizar(b));
    if estricta {
        a == b
    } else {
        a.replace('Ñ', "N") == b.replace('Ñ', "N")
    }
}

/// Texto libre corto (nombres, calle, colonia): sin caracteres de control y
/// con longitud acotada. Devuelve el texto recortado o un mensaje de error.
pub fn texto_corto(campo: &str, valor: &str, max: usize) -> Result<String, String> {
    let v = valor.trim();
    if v.is_empty() {
        return Err(format!("{campo} es obligatorio"));
    }
    if v.chars().count() > max {
        return Err(format!("{campo} admite como máximo {max} caracteres"));
    }
    if v.chars().any(char::is_control) {
        return Err(format!("{campo} contiene caracteres no válidos"));
    }
    Ok(v.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn normaliza_acentos_y_signos() {
        assert_eq!(normalizar("  José-María  de la O'Neill "), "JOSE MARIA DE LA O NEILL");
        assert_eq!(normalizar("Muñoz Güemes"), "MUÑOZ GUEMES");
        assert_eq!(palabras("Ma. Guadalupe"), vec!["MA", "GUADALUPE"]);
        assert!(mismo_nombre("Peña", "PENA", false));
        assert!(!mismo_nombre("Peña", "PENA", true));
        // Acentos descompuestos (NFD): la misma letra, no dos palabras.
        assert_eq!(normalizar("Mun\u{0303}oz Pe\u{0301}rez"), "MUÑOZ PEREZ");
        assert_eq!(transliterar_mrz("O'Farrill-Peña"), "OFARRILL<PENA");
    }

    #[test]
    fn texto_corto_acota() {
        assert_eq!(texto_corto("calle", "  Av.  Reforma ", 20).unwrap(), "Av. Reforma");
        assert!(texto_corto("calle", "   ", 10).is_err());
        assert!(texto_corto("calle", "abc\u{0}", 10).is_err());
        assert!(texto_corto("calle", "abcdefghijk", 10).is_err());
    }
}
