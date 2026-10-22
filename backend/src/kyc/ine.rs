//! Credencial para votar (INE): clave de elector, CIC, vigencia, zona de
//! lectura mecánica (MRZ, formato TD1 de ICAO 9303) y cotejo del OCR que
//! devuelve el proveedor contra el expediente.
//!
//! Errores (el expediente no se verifica): CURP o clave de elector distintas,
//! fecha de nacimiento o sexo que no coinciden, MRZ con dígitos de control
//! inválidos o que contradice el anverso, credencial vencida. Advertencias
//! (van a revisión humana): nombre leído distinto del capturado, código
//! postal distinto, entidad de la clave de elector distinta de la de la CURP.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::{
    curp::{self, Curp, Sexo},
    texto,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorIne {
    #[error("la clave de elector tiene 18 caracteres: 6 letras, fecha AAMMDD, entidad, sexo y 3 dígitos")]
    ClaveElector,
    #[error("la fecha de la clave de elector no existe")]
    FechaClaveElector,
    #[error("el CIC de la credencial tiene 9 dígitos")]
    Cic,
    #[error("el identificador del ciudadano tiene 9 o 13 dígitos")]
    Identificador,
}

/// Clave de elector con estructura válida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaveElector {
    valor: String,
}

impl ClaveElector {
    pub fn parse(entrada: &str) -> Result<Self, ErrorIne> {
        let valor = entrada.trim().to_uppercase();
        let b = valor.as_bytes();
        let estructura = b.len() == 18
            && b[..6].iter().all(u8::is_ascii_uppercase)
            && b[6..14].iter().all(u8::is_ascii_digit)
            && matches!(b[14], b'H' | b'M' | b'X')
            && b[15..].iter().all(u8::is_ascii_digit);
        if !estructura || &valor[12..14] == "00" {
            return Err(ErrorIne::ClaveElector);
        }
        let n = |i: usize| u32::from(b[i] - b'0') * 10 + u32::from(b[i + 1] - b'0');
        if ![1900, 2000].iter().any(|s| NaiveDate::from_ymd_opt(*s + n(6) as i32, n(8), n(10)).is_some()) {
            return Err(ErrorIne::FechaClaveElector);
        }
        Ok(Self { valor })
    }

    pub fn as_str(&self) -> &str {
        &self.valor
    }

    /// Fecha de nacimiento AAMMDD.
    pub fn aammdd(&self) -> &str {
        &self.valor[6..12]
    }

    /// Clave numérica de la entidad de nacimiento.
    pub fn entidad(&self) -> u8 {
        self.valor[12..14].parse().expect("dos dígitos")
    }

    pub fn sexo(&self) -> Sexo {
        Sexo::desde_letra(char::from(self.valor.as_bytes()[14])).expect("validado en parse")
    }
}

pub fn validar_cic(cic: &str) -> Result<(), ErrorIne> {
    if cic.len() == 9 && cic.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ErrorIne::Cic)
    }
}

pub fn validar_identificador(id: &str) -> Result<(), ErrorIne> {
    if matches!(id.len(), 9 | 13) && id.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ErrorIne::Identificador)
    }
}

fn aammdd(fecha: NaiveDate) -> String {
    format!("{:02}{:02}{:02}", fecha.year() % 100, fecha.month(), fecha.day())
}

/// Datos de la credencial que captura el cliente.
#[derive(Debug, Clone, Copy)]
pub struct CredencialDeclarada<'a> {
    pub clave_elector: &'a str,
    pub cic: &'a str,
    pub identificador_ciudadano: Option<&'a str>,
    pub emision: i16,
    pub vigencia: i16,
}

/// Validación de la credencial capturada contra la CURP del expediente.
/// Devuelve (errores, advertencias).
pub fn validar_credencial(c: &CredencialDeclarada<'_>, curp: &Curp, hoy: NaiveDate) -> (Vec<String>, Vec<String>) {
    let (mut errores, mut advertencias) = (Vec::new(), Vec::new());
    match ClaveElector::parse(c.clave_elector) {
        Err(e) => errores.push(e.to_string()),
        Ok(clave) => {
            if clave.aammdd() != aammdd(curp.fecha_nacimiento()) {
                errores.push("la fecha de nacimiento de la clave de elector no coincide con la de la CURP".to_owned());
            }
            let sexos = (clave.sexo(), curp.sexo());
            if sexos.0 != Sexo::X && sexos.1 != Sexo::X && sexos.0 != sexos.1 {
                errores.push("el sexo de la clave de elector no coincide con el de la CURP".to_owned());
            }
            if curp::clave_inegi(curp.entidad()).is_some_and(|n| n != clave.entidad()) {
                advertencias.push("la entidad de nacimiento de la clave de elector no coincide con la de la CURP".to_owned());
            }
        }
    }
    if let Err(e) = validar_cic(c.cic) {
        errores.push(e.to_string());
    }
    if let Some(Err(e)) = c.identificador_ciudadano.map(validar_identificador) {
        errores.push(e.to_string());
    }
    let anio = hoy.year() as i16;
    if c.emision > anio || c.emision < 1990 {
        errores.push(format!("año de emisión {} inválido", c.emision));
    }
    if c.vigencia < c.emision || c.vigencia - c.emision > 10 {
        errores.push("la vigencia de una credencial es de hasta 10 años desde su emisión".to_owned());
    }
    // La credencial vale hasta el 31 de diciembre del año de vigencia.
    if c.vigencia < anio {
        errores.push(format!("la credencial venció en {}", c.vigencia));
    }
    (errores, advertencias)
}

// -----------------------------------------------------------------------------
// MRZ (TD1: 3 líneas de 30 caracteres)
// -----------------------------------------------------------------------------
pub mod mrz {
    use super::texto;

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ErrorMrz {
        #[error("la MRZ de una credencial TD1 tiene 3 líneas de 30 caracteres (A-Z, 0-9, <)")]
        Formato,
        #[error("dígito de control inválido en la MRZ ({0})")]
        Control(&'static str),
    }

    /// Dígito de control ICAO 9303 (pesos 7, 3, 1; `<` vale 0).
    pub fn digito(campo: &str) -> Option<char> {
        let mut suma = 0u32;
        for (i, c) in campo.chars().enumerate() {
            let valor = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'A'..='Z' => c as u32 - 'A' as u32 + 10,
                '<' => 0,
                _ => return None,
            };
            suma += valor * [7, 3, 1][i % 3];
        }
        char::from_digit(suma % 10, 10)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Td1 {
        pub tipo: String,
        pub emisor: String,
        pub numero_documento: String,
        pub datos_opcionales: String,
        /// AAMMDD.
        pub nacimiento: String,
        /// `M` (masculino), `F` (femenino) o `<`.
        pub sexo: char,
        /// AAMMDD.
        pub vencimiento: String,
        pub nacionalidad: String,
        pub apellidos: Vec<String>,
        pub nombres: Vec<String>,
    }

    fn limpiar(s: &str) -> String {
        s.trim_end_matches('<').to_owned()
    }

    pub fn leer(lineas: &[String]) -> Result<Td1, ErrorMrz> {
        let valida = |l: &String| l.len() == 30 && l.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'<');
        if lineas.len() != 3 || !lineas.iter().all(valida) {
            return Err(ErrorMrz::Formato);
        }
        let (l1, l2, l3) = (&lineas[0], &lineas[1], &lineas[2]);
        let control = |campo: &str, c: &str, nombre: &'static str| {
            if digito(campo).map(String::from).as_deref() == Some(c) {
                Ok(())
            } else {
                Err(ErrorMrz::Control(nombre))
            }
        };
        control(&l1[5..14], &l1[14..15], "número de documento")?;
        control(&l2[0..6], &l2[6..7], "fecha de nacimiento")?;
        control(&l2[8..14], &l2[14..15], "vencimiento")?;
        let compuesto = format!("{}{}{}{}", &l1[5..30], &l2[0..7], &l2[8..15], &l2[18..29]);
        control(&compuesto, &l2[29..30], "compuesto")?;
        let (primario, secundario) = l3.split_once("<<").unwrap_or((l3.as_str(), ""));
        let partes = |s: &str| s.split('<').filter(|p| !p.is_empty()).map(str::to_owned).collect::<Vec<_>>();
        Ok(Td1 {
            tipo: limpiar(&l1[0..2]),
            emisor: l1[2..5].to_owned(),
            numero_documento: limpiar(&l1[5..14]),
            datos_opcionales: l1[15..30].trim_matches('<').to_owned(),
            nacimiento: l2[0..6].to_owned(),
            sexo: char::from(l2.as_bytes()[7]),
            vencimiento: l2[8..14].to_owned(),
            nacionalidad: limpiar(&l2[15..18]),
            apellidos: partes(primario),
            nombres: partes(secundario),
        })
    }

    /// Rellena con `<` hasta `largo` (o trunca).
    fn relleno(s: &str, largo: usize) -> String {
        let mut v = s.to_owned();
        v.truncate(largo);
        format!("{v:<<largo$}")
    }

    /// Línea 3 (nombres) que corresponde a una persona: apellidos `<<`
    /// nombres, transliterados según ICAO 9303 y truncados o rellenados a 30.
    pub fn linea_nombres(apellido_paterno: &str, apellido_materno: Option<&str>, nombres: &str) -> String {
        let apellidos = format!("{} {}", apellido_paterno, apellido_materno.unwrap_or_default());
        relleno(&format!("{}<<{}", texto::transliterar_mrz(&apellidos), texto::transliterar_mrz(nombres)), 30)
    }

    /// MRZ de una credencial INE (la usan el proveedor simulado y las pruebas).
    #[allow(clippy::too_many_arguments)]
    pub fn componer_ine(
        cic: &str,
        identificador: Option<&str>,
        nacimiento_aammdd: &str,
        sexo_icao: char,
        vigencia: i16,
        apellido_paterno: &str,
        apellido_materno: Option<&str>,
        nombres: &str,
    ) -> Vec<String> {
        let numero = relleno(cic, 9);
        let opcional = relleno(&format!("<<{}", identificador.unwrap_or_default()), 15);
        let l1 = format!("IDMEX{numero}{}{opcional}", digito(&numero).expect("alfabeto MRZ"));
        let vence = format!("{:02}1231", vigencia % 100);
        let mut l2 = format!(
            "{nacimiento_aammdd}{}{sexo_icao}{vence}{}MEX{}",
            digito(nacimiento_aammdd).expect("dígitos"),
            digito(&vence).expect("dígitos"),
            "<".repeat(11)
        );
        let compuesto = format!("{}{}{}{}", &l1[5..30], &l2[0..7], &l2[8..15], &l2[18..29]);
        l2.push(digito(&compuesto).expect("alfabeto MRZ"));
        vec![l1, l2, linea_nombres(apellido_paterno, apellido_materno, nombres)]
    }
}

// -----------------------------------------------------------------------------
// Cotejo del OCR del proveedor
// -----------------------------------------------------------------------------

/// Lo que el proveedor leyó de la credencial (anverso, reverso y MRZ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrIne {
    pub nombres: String,
    pub apellido_paterno: String,
    #[serde(default)]
    pub apellido_materno: Option<String>,
    pub curp: String,
    pub clave_elector: String,
    pub fecha_nacimiento: NaiveDate,
    /// `H` o `M` (como lo imprime la credencial).
    pub sexo: String,
    pub emision: i16,
    pub vigencia: i16,
    pub cic: String,
    #[serde(default)]
    pub identificador_ciudadano: Option<String>,
    #[serde(default)]
    pub codigo_postal: Option<String>,
    pub mrz: Vec<String>,
}

/// Identidad del expediente contra la que se coteja.
#[derive(Debug, Clone, Copy)]
pub struct IdentidadDeclarada<'a> {
    pub nombres: &'a str,
    pub apellido_paterno: &'a str,
    pub apellido_materno: Option<&'a str>,
    pub curp: &'a str,
    pub fecha_nacimiento: NaiveDate,
    pub sexo: Sexo,
    pub credencial: CredencialDeclarada<'a>,
    pub codigo_postal: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Cotejo {
    pub errores: Vec<String>,
    pub advertencias: Vec<String>,
}

pub fn cotejar(ocr: &OcrIne, d: &IdentidadDeclarada<'_>, hoy: NaiveDate) -> Cotejo {
    let mut c = Cotejo::default();
    let mut error = |m: &str| c.errores.push(m.to_owned());
    if ocr.curp.trim().to_uppercase() != d.curp {
        error("la CURP de la credencial no es la del expediente");
    }
    if ocr.clave_elector.trim().to_uppercase() != d.credencial.clave_elector {
        error("la clave de elector de la credencial no es la capturada");
    }
    if ocr.fecha_nacimiento != d.fecha_nacimiento {
        error("la fecha de nacimiento de la credencial no coincide");
    }
    if Sexo::desde_letra(ocr.sexo.trim().chars().next().unwrap_or(' ')) != Some(d.sexo) {
        error("el sexo de la credencial no coincide");
    }
    if ocr.cic.trim() != d.credencial.cic {
        error("el CIC de la credencial no es el capturado");
    }
    if ocr.emision != d.credencial.emision || ocr.vigencia != d.credencial.vigencia {
        error("la emisión o la vigencia de la credencial no son las capturadas");
    }
    if ocr.vigencia < hoy.year() as i16 {
        error("la credencial está vencida");
    }
    match mrz::leer(&ocr.mrz) {
        Err(e) => error(&e.to_string()),
        Ok(m) => {
            if m.emisor != "MEX" || !m.tipo.starts_with('I') {
                error("la MRZ no es de una identificación mexicana");
            }
            if m.numero_documento != d.credencial.cic {
                error("el número de documento de la MRZ no es el CIC");
            }
            if m.nacimiento != aammdd(d.fecha_nacimiento) {
                error("la fecha de nacimiento de la MRZ no coincide");
            }
            let sexo_icao = match d.sexo {
                Sexo::H => 'M',
                Sexo::M => 'F',
                Sexo::X => '<',
            };
            if m.sexo != sexo_icao {
                error("el sexo de la MRZ no coincide");
            }
            if m.vencimiento[..2] != format!("{:02}", d.credencial.vigencia % 100) {
                error("el vencimiento de la MRZ no coincide con la vigencia");
            }
            // La línea 3 no tiene dígito de control: se compara entera con la
            // que corresponde al nombre capturado (misma transliteración y el
            // mismo truncado a 30 caracteres).
            if ocr.mrz[2] != mrz::linea_nombres(d.apellido_paterno, d.apellido_materno, d.nombres) {
                c.advertencias.push("el nombre de la MRZ no coincide con el capturado".to_owned());
            }
        }
    }
    if !texto::mismo_nombre(&ocr.nombres, d.nombres, true)
        || !texto::mismo_nombre(&ocr.apellido_paterno, d.apellido_paterno, true)
        || !texto::mismo_nombre(ocr.apellido_materno.as_deref().unwrap_or(""), d.apellido_materno.unwrap_or(""), true)
    {
        c.advertencias.push("el nombre impreso en la credencial no coincide con el capturado".to_owned());
    }
    if let (Some(a), Some(b)) = (ocr.codigo_postal.as_deref(), d.codigo_postal) {
        if a.trim() != b {
            c.advertencias.push("el código postal de la credencial no es el del domicilio declarado".to_owned());
        }
    }
    if let (Some(a), Some(b)) = (ocr.identificador_ciudadano.as_deref(), d.credencial.identificador_ciudadano) {
        if a.trim() != b {
            c.advertencias.push("el identificador del ciudadano no coincide".to_owned());
        }
    }
    c
}

/// OCR consistente con una identidad (lo que "leería" el proveedor simulado
/// de una credencial auténtica de esa persona).
pub fn ocr_de(d: &IdentidadDeclarada<'_>) -> OcrIne {
    let sexo_icao = match d.sexo {
        Sexo::H => 'M',
        Sexo::M => 'F',
        Sexo::X => '<',
    };
    OcrIne {
        nombres: d.nombres.to_owned(),
        apellido_paterno: d.apellido_paterno.to_owned(),
        apellido_materno: d.apellido_materno.map(str::to_owned),
        curp: d.curp.to_owned(),
        clave_elector: d.credencial.clave_elector.to_owned(),
        fecha_nacimiento: d.fecha_nacimiento,
        sexo: d.sexo.letra().to_string(),
        emision: d.credencial.emision,
        vigencia: d.credencial.vigencia,
        cic: d.credencial.cic.to_owned(),
        identificador_ciudadano: d.credencial.identificador_ciudadano.map(str::to_owned),
        codigo_postal: d.codigo_postal.map(str::to_owned),
        mrz: mrz::componer_ine(
            d.credencial.cic,
            d.credencial.identificador_ciudadano,
            &aammdd(d.fecha_nacimiento),
            sexo_icao,
            d.credencial.vigencia,
            d.apellido_paterno,
            d.apellido_materno,
            d.nombres,
        ),
    }
}

/// Clave de elector válida para una persona (pruebas y demostración): las 6
/// letras son las primeras consonantes de apellidos y nombre.
pub fn construir_clave_elector(
    nombres: &str,
    paterno: &str,
    materno: Option<&str>,
    fecha: NaiveDate,
    entidad_inegi: u8,
    sexo: Sexo,
    homoclave: u16,
) -> String {
    let dos = |s: &str| {
        let p = texto::normalizar(s).replace('Ñ', "X").replace(' ', "");
        let mut letras: Vec<char> = p.chars().take(1).collect();
        letras.extend(p.chars().skip(1).filter(|c| !"AEIOU".contains(*c)).take(1));
        while letras.len() < 2 {
            letras.push('X');
        }
        letras.into_iter().collect::<String>()
    };
    format!(
        "{}{}{}{}{:02}{}{:03}",
        dos(paterno),
        dos(materno.unwrap_or("X")),
        dos(nombres),
        aammdd(fecha),
        entidad_inegi,
        sexo.letra(),
        homoclave % 1000
    )
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn hoy() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 13).unwrap()
    }

    fn persona() -> (Curp, String) {
        let fecha = NaiveDate::from_ymd_opt(1953, 11, 13).unwrap();
        let curp = Curp::parse("LOOA531113HTCPBN07").unwrap();
        let clave = construir_clave_elector("Andrés Manuel", "López", Some("Obrador"), fecha, 27, Sexo::H, 100);
        (curp, clave)
    }

    #[test]
    fn digito_de_control_icao() {
        // Ejemplo de ICAO 9303 parte 3: "520727" -> 3; "L898902C<" -> 3.
        assert_eq!(mrz::digito("520727"), Some('3'));
        assert_eq!(mrz::digito("L898902C<"), Some('3'));
        assert_eq!(mrz::digito("abc"), None);
    }

    #[test]
    fn clave_de_elector() {
        let (curp, clave) = persona();
        assert_eq!(clave, "LPOBAN53111327H100");
        let c = ClaveElector::parse(&clave).unwrap();
        assert_eq!(c.aammdd(), "531113");
        assert_eq!(c.entidad(), 27);
        assert_eq!(c.sexo(), Sexo::H);
        assert!(ClaveElector::parse("LPOBAN531113").is_err());
        assert_eq!(ClaveElector::parse("LPOBAN53131327H100"), Err(ErrorIne::FechaClaveElector));
        let cred = CredencialDeclarada {
            clave_elector: &clave,
            cic: "123456789",
            identificador_ciudadano: Some("987654321"),
            emision: 2020,
            vigencia: 2030,
        };
        assert_eq!(validar_credencial(&cred, &curp, hoy()), (vec![], vec![]));
        let vencida = CredencialDeclarada { emision: 2014, vigencia: 2024, ..cred };
        assert_eq!(validar_credencial(&vencida, &curp, hoy()).0.len(), 1);
        let otra_fecha = construir_clave_elector("A", "B", None, NaiveDate::from_ymd_opt(1953, 11, 14).unwrap(), 27, Sexo::H, 1);
        let ajena = CredencialDeclarada { clave_elector: &otra_fecha, ..cred };
        assert_eq!(validar_credencial(&ajena, &curp, hoy()).0.len(), 1);
        let otra_entidad = construir_clave_elector("A", "B", None, curp.fecha_nacimiento(), 9, Sexo::H, 1);
        let otra = CredencialDeclarada { clave_elector: &otra_entidad, ..cred };
        assert_eq!(validar_credencial(&otra, &curp, hoy()), (vec![], vec![
            "la entidad de nacimiento de la clave de elector no coincide con la de la CURP".to_owned()
        ]));
    }

    fn declarada<'a>(curp: &'a Curp, clave: &'a str) -> IdentidadDeclarada<'a> {
        IdentidadDeclarada {
            nombres: "Andrés Manuel",
            apellido_paterno: "López",
            apellido_materno: Some("Obrador"),
            curp: curp.as_str(),
            fecha_nacimiento: curp.fecha_nacimiento(),
            sexo: curp.sexo(),
            credencial: CredencialDeclarada {
                clave_elector: clave,
                cic: "123456789",
                identificador_ciudadano: Some("987654321"),
                emision: 2020,
                vigencia: 2030,
            },
            codigo_postal: Some("86000"),
        }
    }

    #[test]
    fn mrz_compuesta_se_lee_y_valida() {
        let (curp, clave) = persona();
        let d = declarada(&curp, &clave);
        let ocr = ocr_de(&d);
        assert_eq!(ocr.mrz.iter().map(String::len).collect::<Vec<_>>(), vec![30, 30, 30]);
        let m = mrz::leer(&ocr.mrz).unwrap();
        assert_eq!(m.numero_documento, "123456789");
        assert_eq!(m.nacimiento, "531113");
        assert_eq!(m.sexo, 'M');
        assert_eq!(m.vencimiento, "301231");
        assert_eq!(m.apellidos, vec!["LOPEZ", "OBRADOR"]);
        assert_eq!(m.nombres, vec!["ANDRES", "MANUEL"]);
        assert_eq!(cotejar(&ocr, &d, hoy()), Cotejo::default());
    }

    #[test]
    fn cotejo_detecta_credenciales_ajenas_y_alteradas() {
        let (curp, clave) = persona();
        let d = declarada(&curp, &clave);
        let buena = ocr_de(&d);

        let mut otra_curp = buena.clone();
        otra_curp.curp = "HEGG560427MVZRRL04".into();
        assert_eq!(cotejar(&otra_curp, &d, hoy()).errores.len(), 1);

        // Un dígito alterado en la MRZ rompe su dígito de control.
        let mut alterada = buena.clone();
        alterada.mrz[1].replace_range(0..1, "6");
        let c = cotejar(&alterada, &d, hoy());
        assert!(c.errores.iter().any(|e| e.contains("dígito de control")), "{c:?}");

        let mut vencida = buena.clone();
        vencida.vigencia = 2025;
        assert!(cotejar(&vencida, &d, hoy()).errores.iter().any(|e| e.contains("vencida")));

        // Línea de nombres truncada: se compara entera, no se omite.
        let largo = IdentidadDeclarada {
            nombres: "María Guadalupe Concepción",
            apellido_paterno: "Hernández",
            apellido_materno: Some("Gutiérrez"),
            ..d
        };
        let mut ajena = ocr_de(&largo);
        assert_eq!(cotejar(&ajena, &largo, hoy()).advertencias, Vec::<String>::new());
        ajena.mrz[2] = mrz::linea_nombres("Ramírez", Some("Gutiérrez"), "Francisco Javier Alejandro");
        assert_eq!(cotejar(&ajena, &largo, hoy()).advertencias.len(), 1);

        // Apóstrofo omitido en la MRZ (ICAO): O'FARRILL -> OFARRILL.
        let apostrofo = IdentidadDeclarada { apellido_paterno: "O'Farrill", ..d };
        let mut o = ocr_de(&apostrofo);
        assert!(o.mrz[2].starts_with("OFARRILL<OBRADOR<<"), "{}", o.mrz[2]);
        o.apellido_paterno = "O'Farrill".into();
        assert_eq!(cotejar(&o, &apostrofo, hoy()), Cotejo::default());

        let mut otro_nombre = buena.clone();
        otro_nombre.nombres = "Andrés".into();
        let c = cotejar(&otro_nombre, &d, hoy());
        assert!(c.errores.is_empty());
        assert_eq!(c.advertencias.len(), 1);
    }
}
