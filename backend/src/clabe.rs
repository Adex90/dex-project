//! CLABE (Clave Bancaria Estandarizada, 18 dígitos): banco (3) + plaza (3) +
//! cuenta (11) + dígito verificador. Misma regla que `nucleo.clabe_valida`.

use rand::Rng;

const PESOS: [u32; 17] = [3, 7, 1, 3, 7, 1, 3, 7, 1, 3, 7, 1, 3, 7, 1, 3, 7];

/// Dígito verificador de los primeros 17 dígitos. `None` si no son 17 dígitos.
pub fn digito_verificador(primeros_17: &str) -> Option<u32> {
    if primeros_17.len() != 17 || !primeros_17.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let suma: u32 = primeros_17.bytes().zip(PESOS).map(|(b, peso)| (u32::from(b - b'0') * peso) % 10).sum();
    Some((10 - suma % 10) % 10)
}

pub fn es_valida(clabe: &str) -> bool {
    clabe.len() == 18
        && clabe.bytes().all(|b| b.is_ascii_digit())
        && digito_verificador(&clabe[..17]) == Some(u32::from(clabe.as_bytes()[17] - b'0'))
}

/// Genera una CLABE con el prefijo (banco + plaza, 6 dígitos) y 11 dígitos aleatorios.
pub fn generar(prefijo: &str) -> String {
    debug_assert_eq!(prefijo.len(), 6);
    let cuenta: u64 = rand::thread_rng().gen_range(0..100_000_000_000);
    let primeros = format!("{prefijo}{cuenta:011}");
    let dv = digito_verificador(&primeros).expect("prefijo de 6 dígitos validado en Config");
    format!("{primeros}{dv}")
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn coincide_con_la_regla_de_la_base_de_datos() {
        // Mismos valores usados en tests/db/0001_invariantes.sql.
        assert!(es_valida("646180157000000017"));
        assert!(es_valida("646180157000000020"));
        assert!(!es_valida("646180157000000018"));
        assert!(!es_valida("64618015700000001"));
        assert!(!es_valida("64618015700000001a"));
    }

    #[test]
    fn genera_clabes_validas() {
        for _ in 0..1_000 {
            let c = generar("999180");
            assert!(c.starts_with("999180"));
            assert!(es_valida(&c), "{c}");
        }
    }
}
