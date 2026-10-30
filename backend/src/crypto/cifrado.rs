//! Cifrado autenticado de secretos en reposo (AES-256-GCM).
//!
//! Formato del sobre: `v1:` + base64 de `nonce (12) || texto cifrado || etiqueta (16)`.
//! El dato asociado (AAD) liga el sobre a su contexto, p. ej. el cliente dueño
//! de un secreto TOTP: copiado a la fila de otra persona, ya no se descifra.
//! La clave vive fuera de la base de datos (`MFA_CLAVE_CIFRADO`); durante una
//! rotación, la anterior (`MFA_CLAVE_CIFRADO_ANTERIOR`) solo descifra, y
//! `synthetic-capital migrar` vuelve a cifrar con la vigente.

use std::fmt;

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use zeroize::Zeroizing;

const PREFIJO: &str = "v1:";
const LONGITUD_NONCE: usize = 12;
const LONGITUD_ETIQUETA: usize = 16;

/// Clave AES-256 (32 bytes). Su `Debug` nunca muestra el valor.
#[derive(Clone, PartialEq, Eq)]
pub struct Clave(pub [u8; 32]);

impl fmt::Debug for Clave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Clave(<oculta>)")
    }
}

impl Clave {
    /// 64 caracteres hexadecimales (genera una con: `openssl rand -hex 32`).
    pub fn desde_hex(nombre: &str, valor: &str) -> anyhow::Result<Self> {
        let valor = valor.trim();
        let bytes = hex::decode(valor).ok().filter(|b| b.len() == 32).ok_or_else(|| {
            anyhow::anyhow!("{nombre} debe tener 64 caracteres hexadecimales (genera una con: openssl rand -hex 32)")
        })?;
        let distintos = valor.to_ascii_lowercase().chars().collect::<std::collections::HashSet<_>>().len();
        if distintos < 10 {
            anyhow::bail!("{nombre} tiene muy poca variedad de caracteres; genera una con: openssl rand -hex 32");
        }
        let mut clave = [0u8; 32];
        clave.copy_from_slice(&bytes);
        Ok(Self(clave))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ErrorCifrado {
    #[error("el sobre no tiene el formato v1")]
    Formato,
    #[error("ninguna clave descifra el sobre en este contexto")]
    Descifrado,
    #[error("no se pudo cifrar")]
    Cifrado,
}

/// Texto descifrado (se borra de la memoria al soltarlo).
pub struct Descifrado {
    pub texto: Zeroizing<Vec<u8>>,
    /// Lo descifró la clave anterior: conviene volver a cifrarlo con la vigente.
    pub con_clave_anterior: bool,
}

impl fmt::Debug for Descifrado {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Descifrado").field("con_clave_anterior", &self.con_clave_anterior).finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Cifrador {
    vigente: Aes256Gcm,
    anterior: Option<Aes256Gcm>,
}

impl fmt::Debug for Cifrador {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cifrador").field("anterior", &self.anterior.is_some()).finish_non_exhaustive()
    }
}

impl Cifrador {
    pub fn nuevo(vigente: &Clave, anterior: Option<&Clave>) -> Self {
        let aes = |c: &Clave| Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&c.0));
        Self { vigente: aes(vigente), anterior: anterior.map(aes) }
    }

    pub fn tiene_clave_anterior(&self) -> bool {
        self.anterior.is_some()
    }

    /// Cifra con la clave vigente y un nonce aleatorio de 96 bits.
    pub fn cifrar(&self, texto: &[u8], contexto: &[u8]) -> Result<String, ErrorCifrado> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let cifrado =
            self.vigente.encrypt(&nonce, Payload { msg: texto, aad: contexto }).map_err(|_| ErrorCifrado::Cifrado)?;
        let mut sobre = Vec::with_capacity(LONGITUD_NONCE + cifrado.len());
        sobre.extend_from_slice(&nonce);
        sobre.extend_from_slice(&cifrado);
        Ok(format!("{PREFIJO}{}", STANDARD.encode(sobre)))
    }

    /// Descifra con la clave vigente y, si falla, con la anterior.
    pub fn descifrar(&self, sobre: &str, contexto: &[u8]) -> Result<Descifrado, ErrorCifrado> {
        let crudo = sobre
            .strip_prefix(PREFIJO)
            .and_then(|b| STANDARD.decode(b).ok())
            .filter(|b| b.len() > LONGITUD_NONCE + LONGITUD_ETIQUETA)
            .ok_or(ErrorCifrado::Formato)?;
        let (nonce, cifrado) = crudo.split_at(LONGITUD_NONCE);
        let nonce = Nonce::from_slice(nonce);
        let abrir = |aes: &Aes256Gcm| aes.decrypt(nonce, Payload { msg: cifrado, aad: contexto }).ok();
        if let Some(texto) = abrir(&self.vigente) {
            return Ok(Descifrado { texto: Zeroizing::new(texto), con_clave_anterior: false });
        }
        match self.anterior.as_ref().and_then(abrir) {
            Some(texto) => Ok(Descifrado { texto: Zeroizing::new(texto), con_clave_anterior: true }),
            None => Err(ErrorCifrado::Descifrado),
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn clave(b: u8) -> Clave {
        Clave(std::array::from_fn(|i| b.wrapping_add(i as u8)))
    }

    #[test]
    fn cifra_y_descifra_con_su_contexto() {
        let c = Cifrador::nuevo(&clave(1), None);
        let sobre = c.cifrar(b"secreto de 20 bytes!", b"cliente-a").unwrap();
        assert!(sobre.starts_with("v1:"));
        assert_eq!(sobre.len(), 3 + 64, "20 bytes cifrados: el formato que exige la base de datos");
        let d = c.descifrar(&sobre, b"cliente-a").unwrap();
        assert_eq!(&d.texto[..], b"secreto de 20 bytes!");
        assert!(!d.con_clave_anterior);
    }

    #[test]
    fn el_mismo_texto_nunca_da_el_mismo_sobre() {
        let c = Cifrador::nuevo(&clave(1), None);
        assert_ne!(c.cifrar(b"x", b"k").unwrap(), c.cifrar(b"x", b"k").unwrap());
    }

    #[test]
    fn otro_contexto_otra_clave_o_un_sobre_alterado_no_descifran() {
        let c = Cifrador::nuevo(&clave(1), None);
        let sobre = c.cifrar(b"secreto", b"cliente-a").unwrap();
        assert_eq!(c.descifrar(&sobre, b"cliente-b").unwrap_err(), ErrorCifrado::Descifrado);
        assert_eq!(
            Cifrador::nuevo(&clave(2), None).descifrar(&sobre, b"cliente-a").unwrap_err(),
            ErrorCifrado::Descifrado
        );
        let mut bytes = STANDARD.decode(&sobre[3..]).unwrap();
        let ultimo = bytes.len() - 1;
        bytes[ultimo] ^= 1;
        let alterado = format!("v1:{}", STANDARD.encode(bytes));
        assert_eq!(c.descifrar(&alterado, b"cliente-a").unwrap_err(), ErrorCifrado::Descifrado);
        assert_eq!(c.descifrar("JBSWY3DPEHPK3PXP", b"cliente-a").unwrap_err(), ErrorCifrado::Formato);
    }

    #[test]
    fn la_clave_anterior_solo_descifra() {
        let viejo = Cifrador::nuevo(&clave(1), None);
        let sobre = viejo.cifrar(b"secreto", b"k").unwrap();
        let rotado = Cifrador::nuevo(&clave(2), Some(&clave(1)));
        let d = rotado.descifrar(&sobre, b"k").unwrap();
        assert!(d.con_clave_anterior);
        let nuevo = rotado.cifrar(&d.texto, b"k").unwrap();
        assert!(!rotado.descifrar(&nuevo, b"k").unwrap().con_clave_anterior);
        assert!(viejo.descifrar(&nuevo, b"k").is_err(), "lo nuevo se cifra con la clave vigente");
    }

    #[test]
    fn valida_la_clave_y_no_la_muestra() {
        assert!(Clave::desde_hex("MFA_CLAVE_CIFRADO", "abc").is_err());
        assert!(Clave::desde_hex("MFA_CLAVE_CIFRADO", &"a".repeat(64)).is_err());
        let buena = "9f2c1e7ab04d5b6c8e3f1a2b7c9d0e4f6a8b1c3d5e7f9a0b2c4d6e8f0a1b3c5d";
        let k = Clave::desde_hex("MFA_CLAVE_CIFRADO", buena).unwrap();
        assert!(!format!("{k:?}").contains("9f2c"));
    }
}
