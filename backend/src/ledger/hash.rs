//! Hash SHA-256 encadenado de los ledgers.
//!
//! La base de datos asigna `secuencia`, `hash_anterior` y `tx_hash` en un
//! trigger (es la fuente de verdad del orden). Este módulo implementa el mismo
//! payload canónico de forma independiente para:
//!
//! 1. verificar, dentro de la misma transacción, que el hash que devolvió la
//!    base de datos es correcto (si no, se aborta), y
//! 2. auditar la cadena completa (`verificar_cadena_*`).
//!
//! Payload canónico v1: cada campo se codifica como `<bytes UTF-8>:<valor>`
//! (o `~` si es NULL) y se une con `|`. El prefijo de longitud hace la
//! codificación inyectiva aunque un concepto contenga `|`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    db::{self, Rol},
    dinero,
    error::AppResult,
    models::{
        ifpe::{LedgerCabeza, TransaccionIfpe},
        sofom::TransaccionSofom,
    },
};

/// Hash anterior del primer movimiento de cada ledger.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const VERSION_IFPE: &str = "SC-IFPE-v1";
const VERSION_SOFOM: &str = "SC-SOFOM-v1";

pub fn sha256_hex(datos: &[u8]) -> String {
    hex::encode(Sha256::digest(datos))
}

/// Construye el payload canónico campo por campo (equivale a `nucleo.campo`).
#[derive(Default)]
struct Payload(String);

impl Payload {
    fn campo(&mut self, valor: Option<&str>) -> &mut Self {
        if !self.0.is_empty() {
            self.0.push('|');
        }
        match valor {
            None => self.0.push('~'),
            Some(v) => {
                self.0.push_str(&v.len().to_string());
                self.0.push(':');
                self.0.push_str(v);
            }
        }
        self
    }

    fn texto(&mut self, valor: &str) -> &mut Self {
        self.campo(Some(valor))
    }

    fn opcional<T: ToString>(&mut self, valor: Option<T>) -> &mut Self {
        let v = valor.map(|v| v.to_string());
        self.campo(v.as_deref())
    }
}

/// Microsegundos desde epoch (equivale a `nucleo.epoch_micros`).
fn epoch_micros(ts: &DateTime<Utc>) -> String {
    ts.timestamp_micros().to_string()
}

pub fn payload_ifpe(t: &TransaccionIfpe) -> String {
    let mut p = Payload::default();
    p.texto(VERSION_IFPE)
        .texto(&t.secuencia.to_string())
        .texto(&t.id.to_string())
        .texto(t.tipo.as_str())
        .opcional(t.cuenta_origen_id)
        .opcional(t.cuenta_destino_id)
        .opcional(t.reserva_id)
        .texto(&dinero::canonico(t.monto))
        .texto(&t.moneda)
        .campo(t.concepto.as_deref())
        .campo(t.referencia_externa.as_deref())
        .texto(&t.clave_idempotencia)
        .opcional(t.ref_movimiento_espejo)
        .texto(&epoch_micros(&t.creado_en))
        .texto(&t.hash_anterior);
    p.0
}

pub fn payload_sofom(t: &TransaccionSofom) -> String {
    let mut p = Payload::default();
    p.texto(VERSION_SOFOM)
        .texto(&t.secuencia.to_string())
        .texto(&t.id.to_string())
        .texto(t.tipo.as_str())
        .opcional(t.cuenta_id)
        .opcional(t.contrato_id)
        .opcional(t.credito_id)
        .opcional(t.caja_id)
        .texto(&dinero::canonico(t.monto))
        .texto(&t.moneda)
        .campo(t.concepto.as_deref())
        .texto(&t.clave_idempotencia)
        .opcional(t.ref_movimiento_espejo)
        .texto(&epoch_micros(&t.creado_en))
        .texto(&t.hash_anterior);
    p.0
}

pub fn hash_ifpe(t: &TransaccionIfpe) -> String {
    sha256_hex(payload_ifpe(t).as_bytes())
}

pub fn hash_sofom(t: &TransaccionSofom) -> String {
    sha256_hex(payload_sofom(t).as_bytes())
}

/// Hash canónico de una lista de campos (mismo esquema de codificación).
/// Se usa, por ejemplo, para el `hash_terminos` de un contrato de mutuo.
pub fn hash_campos(campos: &[&str]) -> String {
    let mut p = Payload::default();
    for c in campos {
        p.texto(c);
    }
    sha256_hex(p.0.as_bytes())
}

// -----------------------------------------------------------------------------
// Verificación de la cadena completa
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FallaCadena {
    pub secuencia: i64,
    pub motivo: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoVerificacion {
    pub ledger: &'static str,
    pub valido: bool,
    pub movimientos_verificados: u64,
    pub ultima_secuencia: i64,
    pub ultimo_hash: String,
    /// `ledger_cabeza` apunta exactamente al último movimiento verificado.
    pub cabeza_coincide: bool,
    pub falla: Option<FallaCadena>,
}

/// Verificador incremental: recibe movimientos en orden de secuencia.
pub struct VerificadorCadena {
    siguiente: i64,
    hash_previo: String,
    verificados: u64,
}

impl Default for VerificadorCadena {
    fn default() -> Self {
        Self { siguiente: 1, hash_previo: GENESIS.to_string(), verificados: 0 }
    }
}

impl VerificadorCadena {
    pub fn verificar(
        &mut self,
        secuencia: i64,
        hash_anterior: &str,
        tx_hash: &str,
        hash_recalculado: &str,
    ) -> Result<(), FallaCadena> {
        let falla = |motivo: String| FallaCadena { secuencia, motivo };
        if secuencia != self.siguiente {
            return Err(falla(format!("secuencia {secuencia} rompe la continuidad (se esperaba {})", self.siguiente)));
        }
        if hash_anterior != self.hash_previo {
            return Err(falla("hash_anterior no enlaza con el movimiento previo".into()));
        }
        if tx_hash != hash_recalculado {
            return Err(falla(format!(
                "tx_hash almacenado {tx_hash} no coincide con el recalculado {hash_recalculado}"
            )));
        }
        self.siguiente += 1;
        self.hash_previo = tx_hash.to_string();
        self.verificados += 1;
        Ok(())
    }

    fn resultado(
        self,
        ledger: &'static str,
        cabeza: &LedgerCabeza,
        falla: Option<FallaCadena>,
    ) -> ResultadoVerificacion {
        let ultima_secuencia = self.siguiente - 1;
        let cabeza_coincide =
            falla.is_none() && cabeza.ultima_secuencia == ultima_secuencia && cabeza.ultimo_hash == self.hash_previo;
        ResultadoVerificacion {
            ledger,
            valido: falla.is_none() && cabeza_coincide,
            movimientos_verificados: self.verificados,
            ultima_secuencia,
            ultimo_hash: self.hash_previo,
            cabeza_coincide,
            falla,
        }
    }
}

const LOTE: i64 = 1_000;

/// Recorre todo el ledger IFPE (foto consistente) y verifica la cadena.
pub async fn verificar_cadena_ifpe(pool: &PgPool) -> AppResult<ResultadoVerificacion> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Auditor).await?;
    let cabeza: LedgerCabeza =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM ifpe.ledger_cabeza").fetch_one(&mut *tx).await?;
    let mut v = VerificadorCadena::default();
    let mut desde = 0_i64;
    loop {
        let lote: Vec<TransaccionIfpe> =
            sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE secuencia > $1 ORDER BY secuencia LIMIT $2")
                .bind(desde)
                .bind(LOTE)
                .fetch_all(&mut *tx)
                .await?;
        let Some(ultima) = lote.last() else { break };
        desde = ultima.secuencia;
        for t in &lote {
            if let Err(f) = v.verificar(t.secuencia, &t.hash_anterior, &t.tx_hash, &hash_ifpe(t)) {
                return Ok(v.resultado("IFPE", &cabeza, Some(f)));
            }
        }
    }
    tx.commit().await?;
    Ok(v.resultado("IFPE", &cabeza, None))
}

/// Recorre todo el ledger SOFOM (foto consistente) y verifica la cadena.
pub async fn verificar_cadena_sofom(pool: &PgPool) -> AppResult<ResultadoVerificacion> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Sofom).await?;
    let cabeza: LedgerCabeza =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM sofom.ledger_cabeza").fetch_one(&mut *tx).await?;
    let mut v = VerificadorCadena::default();
    let mut desde = 0_i64;
    loop {
        let lote: Vec<TransaccionSofom> =
            sqlx::query_as("SELECT * FROM sofom.transacciones WHERE secuencia > $1 ORDER BY secuencia LIMIT $2")
                .bind(desde)
                .bind(LOTE)
                .fetch_all(&mut *tx)
                .await?;
        let Some(ultima) = lote.last() else { break };
        desde = ultima.secuencia;
        for t in &lote {
            if let Err(f) = v.verificar(t.secuencia, &t.hash_anterior, &t.tx_hash, &hash_sofom(t)) {
                return Ok(v.resultado("SOFOM", &cabeza, Some(f)));
            }
        }
    }
    tx.commit().await?;
    Ok(v.resultado("SOFOM", &cabeza, None))
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use crate::models::{ifpe::TipoTxIfpe, sofom::TipoTxSofom};
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use uuid::Uuid;

    fn u(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Vector generado con `ifpe.payload_canonico` / `nucleo.sha256_hex` en PostgreSQL.
    #[test]
    fn paridad_con_postgres_ifpe() {
        let t = TransaccionIfpe {
            id: u("11111111-2222-3333-4444-555555555555"),
            secuencia: 42,
            tipo: TipoTxIfpe::Transferencia,
            cuenta_origen_id: Some(u("aaaaaaaa-0000-0000-0000-000000000001")),
            cuenta_destino_id: Some(u("bbbbbbbb-0000-0000-0000-000000000002")),
            reserva_id: None,
            monto: Decimal::from_str("1234.5").unwrap(),
            moneda: "MXN".into(),
            concepto: Some("Renta | octubre ñ".into()),
            referencia_externa: None,
            clave_idempotencia: "cli:abc:k-1".into(),
            ref_movimiento_espejo: None,
            hash_anterior: "ab".repeat(32),
            tx_hash: String::new(),
            creado_en: ts("2026-10-12T20:15:30.123456Z"),
        };
        assert_eq!(
            payload_ifpe(&t),
            "10:SC-IFPE-v1|2:42|36:11111111-2222-3333-4444-555555555555|13:TRANSFERENCIA|\
             36:aaaaaaaa-0000-0000-0000-000000000001|36:bbbbbbbb-0000-0000-0000-000000000002|~|\
             13:1234.50000000|3:MXN|18:Renta | octubre ñ|~|11:cli:abc:k-1|~|16:1791836130123456|\
             64:abababababababababababababababababababababababababababababababab"
        );
        assert_eq!(hash_ifpe(&t), "b0ed8c2a3a0d1679c73ffb1da181bc66ef5e3dd23f7b1be946b0719f3f0da389");
    }

    #[test]
    fn paridad_con_postgres_sofom() {
        let t = TransaccionSofom {
            id: u("99999999-8888-7777-6666-555555555555"),
            secuencia: 7,
            tipo: TipoTxSofom::AportacionMutuo,
            cuenta_id: Some(u("cccccccc-0000-0000-0000-000000000003")),
            contrato_id: Some(u("dddddddd-0000-0000-0000-000000000004")),
            credito_id: None,
            caja_id: Some(u("eeeeeeee-0000-0000-0000-000000000005")),
            monto: Decimal::from(600),
            moneda: "MXN".into(),
            concepto: None,
            clave_idempotencia: "cli:abc:mutuo-1".into(),
            ref_movimiento_espejo: Some(u("11111111-2222-3333-4444-555555555555")),
            hash_anterior: GENESIS.into(),
            tx_hash: String::new(),
            creado_en: ts("2026-01-01T00:00:00Z"),
        };
        assert_eq!(hash_sofom(&t), "1dee69af21ec769c6a87bd08a019fc4f23b0b60a5f2491a14cf095460545d728");
    }

    #[test]
    fn verificador_detecta_rupturas() {
        let mut v = VerificadorCadena::default();
        assert!(v.verificar(1, GENESIS, "h1", "h1").is_ok());
        assert!(v.verificar(2, "h1", "h2", "h2").is_ok());
        // salto de secuencia
        assert!(VerificadorCadena { siguiente: 3, hash_previo: "h2".into(), verificados: 2 }
            .verificar(4, "h2", "h4", "h4")
            .is_err());
        // eslabón que no enlaza
        assert!(v.verificar(3, "otro", "h3", "h3").is_err());
        // hash alterado
        assert!(v.verificar(3, "h2", "h3", "x3").is_err());
    }
}
