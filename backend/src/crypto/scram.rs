//! Verificadores SCRAM-SHA-256 (RFC 5802 y RFC 7677) en el formato de PostgreSQL:
//! `SCRAM-SHA-256$<iteraciones>:<sal>$<StoredKey>:<ServerKey>` (base64).
//!
//! `synthetic-capital migrar` fija la contraseña del login de la aplicación
//! con un verificador calculado aquí: la contraseña en claro nunca viaja al
//! servidor ni queda en sus bitácoras. PostgreSQL guarda tal cual un valor que
//! ya es un verificador, sin importar `password_encryption`.
//!
//! PostgreSQL normaliza la contraseña con SASLprep antes de derivar las claves
//! y sqlx no lo hace: ambos coinciden solo si la contraseña es ASCII imprimible
//! (ahí SASLprep es la identidad). [`crate::config::LoginAplicacion`] lo exige.

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Iteraciones por omisión de PostgreSQL (`scram_iterations`).
pub const ITERACIONES: u32 = 4096;

/// Longitud de la sal que usa PostgreSQL.
pub const LONGITUD_SAL: usize = 16;

/// Verificador con una sal aleatoria nueva y [`ITERACIONES`].
pub fn verificador(password: &str) -> String {
    let sal: [u8; LONGITUD_SAL] = rand::random();
    verificador_con(password.as_bytes(), &sal, ITERACIONES)
}

/// Verificador con la sal y las iteraciones indicadas.
pub fn verificador_con(password: &[u8], sal: &[u8], iteraciones: u32) -> String {
    let (stored_key, server_key) = claves(password, sal, iteraciones);
    format!(
        "SCRAM-SHA-256${iteraciones}:{}${}:{}",
        STANDARD.encode(sal),
        STANDARD.encode(stored_key),
        STANDARD.encode(server_key)
    )
}

/// `(StoredKey, ServerKey)`:
/// - SaltedPassword := Hi(password, sal, i)
/// - StoredKey := H(HMAC(SaltedPassword, "Client Key"))
/// - ServerKey := HMAC(SaltedPassword, "Server Key")
fn claves(password: &[u8], sal: &[u8], iteraciones: u32) -> ([u8; 32], [u8; 32]) {
    let salted_password = hi(password, sal, iteraciones);
    let client_key = hmac(&salted_password, b"Client Key");
    let stored_key: [u8; 32] = Sha256::digest(client_key).into();
    (stored_key, hmac(&salted_password, b"Server Key"))
}

/// Hi() de RFC 5802: PBKDF2-HMAC-SHA-256 con un solo bloque de salida.
fn hi(password: &[u8], sal: &[u8], iteraciones: u32) -> [u8; 32] {
    let clave = HmacSha256::new_from_slice(password).expect("HMAC acepta claves de cualquier longitud");
    let mut mac = clave.clone();
    mac.update(sal);
    mac.update(&1u32.to_be_bytes());
    let mut u: [u8; 32] = mac.finalize().into_bytes().into();
    let mut resultado = u;
    for _ in 1..iteraciones {
        let mut mac = clave.clone();
        mac.update(&u);
        u = mac.finalize().into_bytes().into();
        resultado.iter_mut().zip(u).for_each(|(r, b)| *r ^= b);
    }
    resultado
}

fn hmac(clave: &[u8], dato: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(clave).expect("HMAC acepta claves de cualquier longitud");
    mac.update(dato);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod pruebas {
    use super::*;

    /// Separa un verificador en (iteraciones, sal, StoredKey, ServerKey).
    fn partes(v: &str) -> (u32, Vec<u8>, Vec<u8>, Vec<u8>) {
        let resto = v.strip_prefix("SCRAM-SHA-256$").expect("prefijo SCRAM-SHA-256$");
        let (parametros, claves) = resto.split_once('$').expect("dos secciones separadas por $");
        let (iteraciones, sal) = parametros.split_once(':').expect("iteraciones:sal");
        let (stored, server) = claves.split_once(':').expect("StoredKey:ServerKey");
        let b64 = |s: &str| STANDARD.decode(s).expect("base64 estándar");
        (iteraciones.parse().expect("iteraciones numéricas"), b64(sal), b64(stored), b64(server))
    }

    /// PBKDF2-HMAC-SHA256, RFC 7914 §11 (primer bloque de P="passwd", S="salt", c=1).
    #[test]
    fn hi_coincide_con_pbkdf2_de_referencia() {
        assert_eq!(
            hex::encode(hi(b"passwd", b"salt", 1)),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
    }

    /// Intercambio de RFC 7677 §3 (usuario "user", contraseña "pencil"): con el
    /// verificador se valida la prueba del cliente igual que lo hace el
    /// servidor, y la firma del servidor coincide con la del RFC.
    #[test]
    fn verificador_valida_el_intercambio_de_rfc_7677() {
        let sal = STANDARD.decode("W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
        let v = verificador_con(b"pencil", &sal, 4096);
        let (iteraciones, sal_leida, stored_key, server_key) = partes(&v);
        assert_eq!((iteraciones, sal_leida), (4096, sal));

        let mensaje = "n=user,r=rOprNGfwEbeRWgbNEkqO,\
                       r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096,\
                       c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
        // Servidor: ClientKey := ClientProof XOR HMAC(StoredKey, AuthMessage); H(ClientKey) == StoredKey.
        let prueba = STANDARD.decode("dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=").unwrap();
        let firma_cliente = hmac(&stored_key, mensaje.as_bytes());
        let client_key: Vec<u8> = prueba.iter().zip(firma_cliente).map(|(p, f)| p ^ f).collect();
        assert_eq!(Sha256::digest(&client_key).as_slice(), stored_key.as_slice());
        // ServerSignature := HMAC(ServerKey, AuthMessage).
        assert_eq!(
            STANDARD.encode(hmac(&server_key, mensaje.as_bytes())),
            "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
        );
    }

    #[test]
    fn formato_de_postgresql_con_sal_aleatoria() {
        let password = "9f2c1e7ab04d5b6c8e3f1a2b7c9d0e4f";
        let v = verificador(password);
        assert!(v.starts_with("SCRAM-SHA-256$4096:"), "{v}");
        assert!(!v.contains(password));
        assert!(v.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=$:-".contains(&b)), "{v}");
        let (iteraciones, sal, stored_key, server_key) = partes(&v);
        assert_eq!(iteraciones, ITERACIONES);
        assert_eq!(sal.len(), LONGITUD_SAL);
        assert_eq!((stored_key.len(), server_key.len()), (32, 32));
        // Misma contraseña con la misma sal: mismo verificador; sal nueva: otro.
        assert_eq!(verificador_con(password.as_bytes(), &sal, ITERACIONES), v);
        assert_ne!(verificador(password), v);
    }
}
