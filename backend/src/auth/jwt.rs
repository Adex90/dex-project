//! Tokens JWT (HS256) de vida corta.
//!
//! Tres usos, separados por audiencia (`aud`) y, los dos últimos, por una clave
//! derivada distinta (defensa en profundidad: un desafío nunca valida como
//! token de acceso aunque fallara la comprobación de audiencia):
//!
//! - **Acceso** (`aud` = [`AUD_ACCESO`]): autoriza la API. El claim `mfa` dice
//!   si la sesión verificó el segundo factor. Vida: `JWT_TTL_MINUTOS`; el
//!   personal interno, como mucho `JWT_TTL_PERSONAL_MINUTOS`.
//! - **Desafío MFA** ([`Desafio::Mfa`]): la contraseña fue correcta y falta el
//!   código TOTP. Solo lo acepta `POST /auth/verify-mfa`. 5 minutos.
//! - **Inscripción** ([`Desafio::Inscripcion`]): personal interno sin segundo
//!   factor todavía. Solo lo aceptan `/auth/mfa/setup` y `/auth/mfa/enable`. 15 minutos.

use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::{error::AppError, models::kyc::RolUsuario};

pub const EMISOR: &str = "synthetic-capital";
pub const AUD_ACCESO: &str = "synthetic-capital/acceso";
pub const AUD_MFA: &str = "synthetic-capital/mfa";
pub const AUD_INSCRIPCION: &str = "synthetic-capital/mfa-inscripcion";

/// Etiqueta de derivación de la clave de los desafíos (HMAC-SHA256 con `JWT_SECRET`).
const DERIVACION_DESAFIOS: &[u8] = b"synthetic-capital/jwt/desafios/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub rol: RolUsuario,
    pub iss: String,
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: Uuid,
    /// La sesión verificó el segundo factor (siempre `false` en un desafío).
    pub mfa: bool,
}

/// Token parcial: la persona está a medio autenticar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desafio {
    /// Falta el código TOTP (`POST /auth/verify-mfa`).
    Mfa,
    /// Falta inscribir el segundo factor (`/auth/mfa/setup`, `/auth/mfa/enable`).
    Inscripcion,
}

impl Desafio {
    fn audiencia(self) -> &'static str {
        match self {
            Desafio::Mfa => AUD_MFA,
            Desafio::Inscripcion => AUD_INSCRIPCION,
        }
    }

    fn vida(self) -> Duration {
        match self {
            Desafio::Mfa => Duration::minutes(5),
            Desafio::Inscripcion => Duration::minutes(15),
        }
    }
}

struct Par {
    codificar: EncodingKey,
    decodificar: DecodingKey,
}

impl Par {
    fn de(secreto: &[u8]) -> Self {
        Self { codificar: EncodingKey::from_secret(secreto), decodificar: DecodingKey::from_secret(secreto) }
    }
}

pub struct ClavesJwt {
    acceso: Par,
    desafios: Par,
    ttl: Duration,
    ttl_personal: Duration,
}

fn validacion(audiencia: &str) -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.set_issuer(&[EMISOR]);
    v.set_audience(&[audiencia]);
    v.set_required_spec_claims(&["exp", "iss", "sub", "aud"]);
    v.leeway = 30;
    v
}

impl ClavesJwt {
    pub fn nuevas(secreto: &str, ttl_minutos: i64, ttl_personal_minutos: i64) -> Self {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secreto.as_bytes()).expect("HMAC acepta claves de cualquier largo");
        mac.update(DERIVACION_DESAFIOS);
        let derivada = mac.finalize().into_bytes();
        Self {
            acceso: Par::de(secreto.as_bytes()),
            desafios: Par::de(&derivada),
            ttl: Duration::minutes(ttl_minutos),
            ttl_personal: Duration::minutes(ttl_minutos.min(ttl_personal_minutos)),
        }
    }

    /// Vida de una sesión de acceso: el personal interno, la más corta.
    pub fn ttl_de(&self, rol: RolUsuario) -> Duration {
        if rol == RolUsuario::Cliente {
            self.ttl
        } else {
            self.ttl_personal
        }
    }

    /// Token de acceso emitido ahora.
    pub fn emitir(&self, cliente_id: Uuid, rol: RolUsuario, mfa: bool) -> Result<(String, DateTime<Utc>), AppError> {
        self.emitir_desde(cliente_id, rol, mfa, Utc::now().timestamp())
    }

    /// Token de acceso con `iat` dado: tras revocar las sesiones con el reloj de
    /// la base de datos, el nuevo token se emite en ese mismo segundo, así un
    /// desfase de reloj entre servidor y base de datos no lo invalida.
    pub fn emitir_desde(
        &self,
        cliente_id: Uuid,
        rol: RolUsuario,
        mfa: bool,
        iat: i64,
    ) -> Result<(String, DateTime<Utc>), AppError> {
        let iat = iat.max(Utc::now().timestamp());
        let expira = DateTime::<Utc>::from_timestamp(iat, 0).unwrap_or_else(Utc::now) + self.ttl_de(rol);
        let claims = Claims {
            sub: cliente_id,
            rol,
            iss: EMISOR.into(),
            aud: AUD_ACCESO.into(),
            iat,
            exp: expira.timestamp(),
            jti: Uuid::new_v4(),
            mfa,
        };
        firmar(&claims, &self.acceso).map(|t| (t, expira))
    }

    pub fn emitir_desafio(
        &self,
        cliente_id: Uuid,
        rol: RolUsuario,
        desafio: Desafio,
    ) -> Result<(String, DateTime<Utc>), AppError> {
        let ahora = Utc::now();
        let expira = ahora + desafio.vida();
        let claims = Claims {
            sub: cliente_id,
            rol,
            iss: EMISOR.into(),
            aud: desafio.audiencia().into(),
            iat: ahora.timestamp(),
            exp: expira.timestamp(),
            jti: Uuid::new_v4(),
            mfa: false,
        };
        firmar(&claims, &self.desafios).map(|t| (t, expira))
    }

    /// Token de acceso (firma, emisor, audiencia y vigencia).
    pub fn validar(&self, token: &str) -> Result<Claims, AppError> {
        decode::<Claims>(token, &self.acceso.decodificar, &validacion(AUD_ACCESO))
            .map(|d| d.claims)
            .map_err(|_| AppError::NoAutenticado)
    }

    /// Token parcial del uso indicado.
    pub fn validar_desafio(&self, token: &str, desafio: Desafio) -> Result<Claims, AppError> {
        decode::<Claims>(token, &self.desafios.decodificar, &validacion(desafio.audiencia()))
            .map(|d| d.claims)
            .map_err(|_| AppError::DesafioVencido)
    }
}

fn firmar(claims: &Claims, par: &Par) -> Result<String, AppError> {
    encode(&Header::new(Algorithm::HS256), claims, &par.codificar)
        .map_err(|e| AppError::Interno(anyhow::anyhow!("no se pudo firmar el JWT: {e}")))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    const SECRETO: &str = "secreto-de-pruebas-con-al-menos-32-caracteres";

    #[test]
    fn emite_y_valida() {
        let claves = ClavesJwt::nuevas(SECRETO, 5, 5);
        let id = Uuid::new_v4();
        let (token, _) = claves.emitir(id, RolUsuario::Cliente, false).unwrap();
        let c = claves.validar(&token).unwrap();
        assert_eq!(c.sub, id);
        assert_eq!(c.rol, RolUsuario::Cliente);
        assert!(!c.mfa);
        let (token, _) = claves.emitir(id, RolUsuario::Cumplimiento, true).unwrap();
        assert!(claves.validar(&token).unwrap().mfa);
    }

    #[test]
    fn rechaza_firma_ajena_y_alg_none() {
        let a = ClavesJwt::nuevas("secreto-a-secreto-a-secreto-a-secreto-a", 5, 5);
        let b = ClavesJwt::nuevas("secreto-b-secreto-b-secreto-b-secreto-b", 5, 5);
        let (token, _) = a.emitir(Uuid::new_v4(), RolUsuario::Operador, true).unwrap();
        assert!(b.validar(&token).is_err());
        // Token sin firma (alg=none) con rol OPERADOR.
        let partes: Vec<&str> = token.split('.').collect();
        let sin_firma = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{}.", partes[1]);
        assert!(a.validar(&sin_firma).is_err());
    }

    #[test]
    fn rechaza_token_expirado() {
        let claves = ClavesJwt::nuevas(SECRETO, -10, -10);
        let (token, _) = claves.emitir(Uuid::new_v4(), RolUsuario::Cliente, false).unwrap();
        assert!(claves.validar(&token).is_err());
    }

    #[test]
    fn un_desafio_nunca_es_un_token_de_acceso_ni_otro_desafio() {
        let claves = ClavesJwt::nuevas(SECRETO, 60, 30);
        let id = Uuid::new_v4();
        let (mfa, _) = claves.emitir_desafio(id, RolUsuario::Cumplimiento, Desafio::Mfa).unwrap();
        let (inscripcion, _) = claves.emitir_desafio(id, RolUsuario::Cumplimiento, Desafio::Inscripcion).unwrap();
        let (acceso, _) = claves.emitir(id, RolUsuario::Cliente, false).unwrap();
        assert!(matches!(claves.validar(&mfa), Err(AppError::NoAutenticado)));
        assert!(matches!(claves.validar(&inscripcion), Err(AppError::NoAutenticado)));
        assert!(claves.validar_desafio(&mfa, Desafio::Mfa).is_ok());
        assert!(claves.validar_desafio(&mfa, Desafio::Inscripcion).is_err());
        assert!(claves.validar_desafio(&inscripcion, Desafio::Mfa).is_err());
        assert!(claves.validar_desafio(&acceso, Desafio::Mfa).is_err());
        assert!(claves.validar_desafio(&acceso, Desafio::Inscripcion).is_err());
    }

    #[test]
    fn el_personal_vive_menos() {
        let claves = ClavesJwt::nuevas(SECRETO, 60, 30);
        assert_eq!(claves.ttl_de(RolUsuario::Cliente), Duration::minutes(60));
        for rol in [RolUsuario::Operador, RolUsuario::Auditor, RolUsuario::Cumplimiento] {
            assert_eq!(claves.ttl_de(rol), Duration::minutes(30));
        }
        // Nunca más que la vida general.
        assert_eq!(ClavesJwt::nuevas(SECRETO, 15, 30).ttl_de(RolUsuario::Cumplimiento), Duration::minutes(15));
    }
}
