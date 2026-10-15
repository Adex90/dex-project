//! RFC de persona física (SAT): 4 letras + fecha AAMMDD + homoclave de 2
//! caracteres + dígito verificador (módulo 11).

use chrono::NaiveDate;

use super::curp::Curp;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorRfc {
    #[error("el RFC de persona física tiene 13 caracteres")]
    Longitud,
    #[error("el RFC no tiene la estructura oficial (4 letras, fecha AAMMDD y homoclave)")]
    Estructura,
    #[error("la fecha del RFC no existe")]
    Fecha,
    #[error("el dígito verificador del RFC no corresponde")]
    DigitoVerificador,
    #[error("el RFC genérico no identifica a una persona")]
    Generico,
}

const ALFABETO: &str = "0123456789ABCDEFGHIJKLMN&OPQRSTUVWXYZ Ñ";

/// Dígito verificador de las 12 (persona física) u 11 (moral) primeras
/// posiciones. Comprobado con RFC reales (SAT970701NN3, PME380607P35).
pub fn digito_verificador(base: &str) -> Option<char> {
    let mut caracteres: Vec<char> = base.chars().collect();
    if caracteres.len() == 11 {
        caracteres.insert(0, ' ');
    }
    if caracteres.len() != 12 {
        return None;
    }
    let mut suma = 0u32;
    for (i, c) in caracteres.iter().enumerate() {
        let valor = ALFABETO.chars().position(|a| a == *c)? as u32;
        suma += valor * (13 - i as u32);
    }
    Some(match suma % 11 {
        0 => '0',
        r if 11 - r == 10 => 'A',
        r => char::from_digit(11 - r, 10).expect("menor que 10"),
    })
}

/// Valida un RFC de persona física y lo devuelve en mayúsculas.
pub fn validar_persona_fisica(entrada: &str) -> Result<String, ErrorRfc> {
    let rfc = entrada.trim().to_uppercase();
    let c: Vec<char> = rfc.chars().collect();
    if c.len() != 13 {
        return Err(ErrorRfc::Longitud);
    }
    if rfc == "XAXX010101000" || rfc == "XEXX010101000" {
        return Err(ErrorRfc::Generico);
    }
    let estructura = c[..4].iter().all(|ch| ch.is_ascii_uppercase() || *ch == 'Ñ' || *ch == '&')
        && c[4..10].iter().all(char::is_ascii_digit)
        && c[10..12].iter().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        && (c[12].is_ascii_digit() || c[12] == 'A');
    if !estructura {
        return Err(ErrorRfc::Estructura);
    }
    let digito = |i: usize| c[i].to_digit(10).expect("dígito");
    let (aa, mm, dd) = (digito(4) * 10 + digito(5), digito(6) * 10 + digito(7), digito(8) * 10 + digito(9));
    let existe = [1900, 2000].iter().any(|s| NaiveDate::from_ymd_opt(*s + aa as i32, mm, dd).is_some());
    if !existe {
        return Err(ErrorRfc::Fecha);
    }
    let base: String = c[..12].iter().collect();
    if digito_verificador(&base) != Some(c[12]) {
        return Err(ErrorRfc::DigitoVerificador);
    }
    Ok(rfc)
}

/// Cruce con la CURP: la fecha es la misma (error si no); las letras casi
/// siempre (advertencia si difieren). El SAT las forma distinto que RENAPO
/// cuando no hay segundo apellido o el primero tiene una o dos letras (reglas
/// 4 y 7 del instructivo del RFC: UC PECH -> UPJU, no UXPJ); en esos casos
/// no se comparan. También usa otra lista de palabras inconvenientes.
pub fn cotejar_con_curp(rfc: &str, curp: &Curp, paterno: &str, materno: Option<&str>) -> (Vec<String>, Vec<String>) {
    let (mut errores, mut advertencias) = (Vec::new(), Vec::new());
    let r: Vec<char> = rfc.chars().collect();
    let c: Vec<char> = curp.as_str().chars().collect();
    if r.len() != 13 {
        errores.push("RFC inválido".to_owned());
        return (errores, advertencias);
    }
    if r[4..10] != c[4..10] {
        errores.push("la fecha de nacimiento del RFC no coincide con la de la CURP".to_owned());
    }
    let materno_util = materno.and_then(super::curp::apellido_util);
    let paterno_corto = super::curp::apellido_util(paterno).is_none_or(|p| p.chars().count() <= 2);
    if materno_util.is_some() && !paterno_corto && r[..4] != c[..4] {
        advertencias.push("las letras del RFC no coinciden con las de la CURP".to_owned());
    }
    (errores, advertencias)
}

/// RFC válido para una CURP (para pruebas y demostración).
pub fn construir_desde_curp(curp: &str, homoclave: &str) -> String {
    let base = format!("{}{}", &curp[..10], homoclave);
    let dv = digito_verificador(&base).expect("12 caracteres del alfabeto RFC");
    format!("{base}{dv}")
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn digito_verificador_de_rfc_reales() {
        for rfc in ["SAT970701NN3", "BBA830831LJ2", "CFE370814QI0", "PME380607P35", "TME840315KT6", "GODE561231GR8"] {
            let n = rfc.chars().count();
            assert_eq!(digito_verificador(&rfc[..n - 1]), rfc.chars().last(), "{rfc}");
        }
    }

    #[test]
    fn valida_persona_fisica() {
        assert_eq!(validar_persona_fisica(" gode561231gr8 ").unwrap(), "GODE561231GR8");
        assert_eq!(validar_persona_fisica("GODE561231GR9"), Err(ErrorRfc::DigitoVerificador));
        assert_eq!(validar_persona_fisica("SAT970701NN3"), Err(ErrorRfc::Longitud));
        assert_eq!(validar_persona_fisica("GODE561331GR8"), Err(ErrorRfc::Fecha));
        assert_eq!(validar_persona_fisica("GOD1561231GR8"), Err(ErrorRfc::Estructura));
        assert_eq!(validar_persona_fisica("XEXX010101000"), Err(ErrorRfc::Generico));
    }

    #[test]
    fn coteja_con_la_curp() {
        let curp = Curp::parse("LOOA531113HTCPBN07").unwrap();
        let rfc = construir_desde_curp(curp.as_str(), "XY");
        assert!(validar_persona_fisica(&rfc).is_ok());
        assert_eq!(cotejar_con_curp(&rfc, &curp, "López", Some("Obrador")), (vec![], vec![]));
        let otra_fecha = construir_desde_curp("LOOA531114HTCPBN07", "XY");
        assert_eq!(cotejar_con_curp(&otra_fecha, &curp, "López", Some("Obrador")).0.len(), 1);
        // Letras distintas: advertencia, salvo donde el SAT forma otras letras.
        let otras_letras = construir_desde_curp("LOBA531113HTCPBN07", "XY");
        assert_eq!(cotejar_con_curp(&otras_letras, &curp, "López", Some("Obrador")).1.len(), 1);
        assert!(cotejar_con_curp(&otras_letras, &curp, "Ruiz", None).1.is_empty(), "un solo apellido");
        assert!(cotejar_con_curp(&otras_letras, &curp, "Uc", Some("Pech")).1.is_empty(), "apellido de dos letras");
    }
}
