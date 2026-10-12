//! Tokens JWT (HS256) de vida corta.

use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, models::kyc::RolUsuario};

pub const EMISOR: &str = "synthetic-capital";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub rol: RolUsuario,
    pub iss: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: Uuid,
}

pub struct ClavesJwt {
    codificar: EncodingKey,
    decodificar: DecodingKey,
    validacion: Validation,
    ttl: Duration,
}

impl ClavesJwt {
    pub fn nuevas(secreto: &str, ttl_minutos: i64) -> Self {
        let mut validacion = Validation::new(Algorithm::HS256);
        validacion.set_issuer(&[EMISOR]);
        validacion.set_required_spec_claims(&["exp", "iss", "sub"]);
        validacion.leeway = 30;
        Self {
            codificar: EncodingKey::from_secret(secreto.as_bytes()),
            decodificar: DecodingKey::from_secret(secreto.as_bytes()),
            validacion,
            ttl: Duration::minutes(ttl_minutos),
        }
    }

    pub fn emitir(&self, cliente_id: Uuid, rol: RolUsuario) -> Result<(String, DateTime<Utc>), AppError> {
        let ahora = Utc::now();
        let expira = ahora + self.ttl;
        let claims = Claims {
            sub: cliente_id,
            rol,
            iss: EMISOR.into(),
            iat: ahora.timestamp(),
            exp: expira.timestamp(),
            jti: Uuid::new_v4(),
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.codificar)
            .map_err(|e| AppError::Interno(anyhow::anyhow!("no se pudo firmar el JWT: {e}")))?;
        Ok((token, expira))
    }

    pub fn validar(&self, token: &str) -> Result<Claims, AppError> {
        decode::<Claims>(token, &self.decodificar, &self.validacion)
            .map(|d| d.claims)
            .map_err(|_| AppError::NoAutenticado)
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn emite_y_valida() {
        let claves = ClavesJwt::nuevas("secreto-de-pruebas-con-al-menos-32-caracteres", 5);
        let id = Uuid::new_v4();
        let (token, _) = claves.emitir(id, RolUsuario::Cliente).unwrap();
        let c = claves.validar(&token).unwrap();
        assert_eq!(c.sub, id);
        assert_eq!(c.rol, RolUsuario::Cliente);
    }

    #[test]
    fn rechaza_firma_ajena_y_alg_none() {
        let a = ClavesJwt::nuevas("secreto-a-secreto-a-secreto-a-secreto-a", 5);
        let b = ClavesJwt::nuevas("secreto-b-secreto-b-secreto-b-secreto-b", 5);
        let (token, _) = a.emitir(Uuid::new_v4(), RolUsuario::Operador).unwrap();
        assert!(b.validar(&token).is_err());
        // Token sin firma (alg=none) con rol OPERADOR.
        let partes: Vec<&str> = token.split('.').collect();
        let sin_firma = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{}.", partes[1]);
        assert!(a.validar(&sin_firma).is_err());
    }

    #[test]
    fn rechaza_token_expirado() {
        let claves = ClavesJwt::nuevas("secreto-de-pruebas-con-al-menos-32-caracteres", -10);
        let (token, _) = claves.emitir(Uuid::new_v4(), RolUsuario::Cliente).unwrap();
        assert!(claves.validar(&token).is_err());
    }
}
