//! Firma de los webhooks del proveedor SPEI.
//!
//! Cabecera `X-Spei-Firma: t=<unix>,v1=<hex>` donde
//! `v1 = HMAC-SHA256(secreto, "<t>.<cuerpo crudo>")`. El cuerpo se firma tal
//! cual llegó (bytes), antes de interpretarlo como JSON. La marca de tiempo va
//! dentro de lo firmado y debe estar a menos de `tolerancia` segundos del
//! reloj del servidor: una firma capturada no sirve después. Se admiten varias
//! `v1` y varios secretos para rotar el secreto sin cortar el servicio. La
//! comparación es en tiempo constante (`Mac::verify_slice`).
//!
//! Nota: STP en particular no firma con HMAC sus notificaciones (se protegen
//! por lista de IPs y TLS); este esquema es el de la pasarela o proxy que
//! recibe a STP y reenvía a este servicio. Las órdenes que se envían a STP se
//! firman con RSA sobre la "cadena original", responsabilidad del cliente real
//! de [`super::pasarela`].

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const CABECERA: &str = "x-spei-firma";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorFirma {
    #[error("falta la cabecera de firma")]
    Ausente,
    #[error("cabecera de firma mal formada")]
    MalFormada,
    #[error("la firma está fuera de la ventana de tiempo permitida")]
    Vencida,
    #[error("firma inválida")]
    Invalida,
}

fn mac(secreto: &[u8], marca: i64, cuerpo: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secreto).expect("HMAC acepta claves de cualquier longitud");
    m.update(marca.to_string().as_bytes());
    m.update(b".");
    m.update(cuerpo);
    m
}

/// Valor de la cabecera para un cuerpo (la usan la pasarela simulada y las pruebas).
pub fn firmar(secreto: &[u8], marca: i64, cuerpo: &[u8]) -> String {
    format!("t={marca},v1={}", hex::encode(mac(secreto, marca, cuerpo).finalize().into_bytes()))
}

/// Verifica la cabecera contra alguno de los secretos vigentes.
pub fn verificar(
    secretos: &[&[u8]],
    cabecera: Option<&str>,
    cuerpo: &[u8],
    ahora: i64,
    tolerancia_seg: u64,
) -> Result<(), ErrorFirma> {
    let cabecera = cabecera.ok_or(ErrorFirma::Ausente)?;
    let mut marca: Option<i64> = None;
    let mut firmas: Vec<Vec<u8>> = Vec::new();
    for parte in cabecera.split(',') {
        let (k, v) = parte.trim().split_once('=').ok_or(ErrorFirma::MalFormada)?;
        match k {
            "t" => {
                if marca.is_some() {
                    return Err(ErrorFirma::MalFormada);
                }
                marca = Some(v.parse().map_err(|_| ErrorFirma::MalFormada)?);
            }
            "v1" => firmas.push(hex::decode(v).map_err(|_| ErrorFirma::MalFormada)?),
            _ => {} // otros esquemas futuros
        }
    }
    let marca = marca.ok_or(ErrorFirma::MalFormada)?;
    if firmas.is_empty() || firmas.len() > 4 {
        return Err(ErrorFirma::MalFormada);
    }
    if ahora.abs_diff(marca) > tolerancia_seg {
        return Err(ErrorFirma::Vencida);
    }
    for secreto in secretos {
        for firma in &firmas {
            if mac(secreto, marca, cuerpo).verify_slice(firma).is_ok() {
                return Ok(());
            }
        }
    }
    Err(ErrorFirma::Invalida)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    const SECRETO: &[u8] = b"secreto-del-proveedor-spei-de-32-bytes!";

    #[test]
    fn vector_rfc_4231_caso_2() {
        // RFC 4231, caso 2: clave "Jefe", datos "what do ya want for nothing?".
        let mut m = HmacSha256::new_from_slice(b"Jefe").unwrap();
        m.update(b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(m.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn firma_valida_y_alteraciones() {
        let cuerpo = br#"{"claveRastreo":"ABC","monto":10.5}"#;
        let cab = firmar(SECRETO, 1_000, cuerpo);
        assert_eq!(verificar(&[SECRETO], Some(&cab), cuerpo, 1_000, 300), Ok(()));
        assert_eq!(verificar(&[SECRETO], Some(&cab), cuerpo, 1_299, 300), Ok(()));
        // Cuerpo alterado en un byte.
        let otro = br#"{"claveRastreo":"ABC","monto":10.6}"#;
        assert_eq!(verificar(&[SECRETO], Some(&cab), otro, 1_000, 300), Err(ErrorFirma::Invalida));
        // Marca alterada (la firma la cubre).
        let movida = cab.replacen("t=1000", "t=1100", 1);
        assert_eq!(verificar(&[SECRETO], Some(&movida), cuerpo, 1_100, 300), Err(ErrorFirma::Invalida));
        // Repetición fuera de la ventana.
        assert_eq!(verificar(&[SECRETO], Some(&cab), cuerpo, 1_301, 300), Err(ErrorFirma::Vencida));
        assert_eq!(verificar(&[SECRETO], Some(&cab), cuerpo, 699, 300), Err(ErrorFirma::Vencida));
        // Otro secreto.
        assert_eq!(verificar(&[b"otro"], Some(&cab), cuerpo, 1_000, 300), Err(ErrorFirma::Invalida));
    }

    #[test]
    fn rotacion_de_secreto() {
        let cuerpo = b"{}";
        let cab = firmar(b"anterior", 50, cuerpo);
        assert_eq!(verificar(&[SECRETO, b"anterior"], Some(&cab), cuerpo, 50, 300), Ok(()));
        // Dos firmas en la cabecera durante la rotación.
        let doble = format!("{},v1={}", firmar(SECRETO, 50, cuerpo), &firmar(b"x", 50, cuerpo)[8..]);
        assert_eq!(verificar(&[SECRETO], Some(&doble), cuerpo, 50, 300), Ok(()));
    }

    #[test]
    fn cabeceras_mal_formadas() {
        let c = b"{}";
        assert_eq!(verificar(&[SECRETO], None, c, 0, 300), Err(ErrorFirma::Ausente));
        for cab in ["", "t=1", "v1=00", "t=x,v1=00", "t=1,v1=zz", "t=1,t=2,v1=00", "basura"] {
            assert!(verificar(&[SECRETO], Some(cab), c, 1, 300).is_err(), "{cab}");
        }
    }
}
