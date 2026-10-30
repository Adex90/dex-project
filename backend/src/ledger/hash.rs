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

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::{self, Rol},
    dinero,
    error::AppResult,
    models::{
        ifpe::{LedgerCabeza, TipoTxIfpe, TransaccionIfpe},
        sofom::{ContratoMutuo, TipoTxSofom, TransaccionSofom},
    },
    sofom::puente,
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
    /// Primera ruptura de la cadena (continuidad, enlace o hash).
    pub falla: Option<FallaCadena>,
    /// Anclas publicadas (cortes de Proof of Reserve) comprobadas contra la cadena.
    pub anclas_verificadas: u64,
    /// Saldos vivos y sombra conciliados con la reproducción independiente del ledger.
    pub saldos_conciliados: u64,
    /// Anclas que ya no existen en la cadena, saldos que el ledger no justifica,
    /// movimientos espejo IFPE<->SOFOM rotos, conteos que no cuadran.
    pub discrepancias: Vec<String>,
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
            anclas_verificadas: 0,
            saldos_conciliados: 0,
            discrepancias: Vec::new(),
        }
    }
}

impl ResultadoVerificacion {
    fn cerrar(mut self) -> Self {
        self.valido = self.falla.is_none() && self.cabeza_coincide && self.discrepancias.is_empty();
        self
    }
}

const LOTE: i64 = 1_000;

/// Saldos reproducidos a partir del ledger (dos componentes: disponible y bloqueado).
#[derive(Default)]
struct Reproduccion(HashMap<Uuid, (Decimal, Decimal)>);

impl Reproduccion {
    fn mover(&mut self, id: Option<Uuid>, d1: Decimal, d2: Decimal) {
        if let Some(id) = id {
            let e = self.0.entry(id).or_default();
            e.0 += d1;
            e.1 += d2;
        }
    }

    /// Compara con los saldos de una tabla (vivos o sombra) y anota discrepancias.
    fn conciliar(&self, nombre: &str, saldos: &[(Uuid, Decimal, Decimal)], r: &mut ResultadoVerificacion) {
        let cero = (Decimal::ZERO, Decimal::ZERO);
        let mut vistos = std::collections::HashSet::new();
        for (id, a, b) in saldos {
            vistos.insert(*id);
            let esperado = self.0.get(id).copied().unwrap_or(cero);
            if (*a, *b) != esperado {
                r.discrepancias.push(format!(
                    "{nombre} {id}: saldo ({a}, {b}) pero el ledger justifica ({}, {})",
                    esperado.0, esperado.1
                ));
            }
            r.saldos_conciliados += 1;
        }
        for (id, v) in &self.0 {
            if !vistos.contains(id) && *v != cero {
                r.discrepancias.push(format!("{nombre} {id}: el ledger justifica ({}, {}) pero no existe", v.0, v.1));
            }
        }
    }
}

async fn conteo_ledger(tx: &mut db::Tx, tabla: &str) -> AppResult<(i64, Option<i64>)> {
    let sql = format!("SELECT count(*), min(secuencia) FROM {tabla}");
    Ok(sqlx::query_as(&sql).fetch_one(&mut **tx).await?)
}

fn verificar_conteo(r: &mut ResultadoVerificacion, conteo: (i64, Option<i64>), cabeza: &LedgerCabeza) {
    let (n, min) = conteo;
    if n != cabeza.ultima_secuencia || (n > 0 && min != Some(1)) {
        r.discrepancias.push(format!(
            "el ledger tiene {n} filas (mínima secuencia {min:?}) pero la cabeza declara {}",
            cabeza.ultima_secuencia
        ));
    }
}

/// Recorre todo el ledger IFPE (foto consistente): cadena de hashes, anclas de
/// los cortes publicados y reproducción de todos los saldos.
pub async fn verificar_cadena_ifpe(pool: &PgPool) -> AppResult<ResultadoVerificacion> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Auditor).await?;
    let cabeza: LedgerCabeza =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM ifpe.ledger_cabeza").fetch_one(&mut *tx).await?;
    // Anclas publicadas: (secuencia, hash, corte) en orden de publicación.
    let anclas: Vec<(i64, String, Uuid)> =
        sqlx::query_as("SELECT secuencia_ledger, hash_ledger, id FROM ifpe.cortes_reserva ORDER BY creado_en, id")
            .fetch_all(&mut *tx)
            .await?;
    let mut por_secuencia: HashMap<i64, Vec<(String, Uuid)>> = HashMap::new();
    for (sec, h, id) in &anclas {
        por_secuencia.entry(*sec).or_default().push((h.clone(), *id));
    }

    let mut v = VerificadorCadena::default();
    let mut cuentas = Reproduccion::default();
    let mut reservas = Reproduccion::default();
    let mut anclas_ok = 0_u64;
    let mut discrepancias = Vec::new();
    let mut falla = None;
    // Desde i64::MIN: una fila con secuencia <= 0 rompe la continuidad.
    let mut desde = i64::MIN;
    'lotes: loop {
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
                falla = Some(f);
                break 'lotes;
            }
            for (h, corte) in por_secuencia.get(&t.secuencia).into_iter().flatten() {
                if *h == t.tx_hash {
                    anclas_ok += 1;
                } else {
                    discrepancias.push(format!("el corte {corte} ancla la secuencia {} con otro hash", t.secuencia));
                }
            }
            let m = t.monto;
            let cero = Decimal::ZERO;
            match t.tipo {
                TipoTxIfpe::Deposito | TipoTxIfpe::RetornoDeSofom => {
                    cuentas.mover(t.cuenta_destino_id, m, cero);
                    reservas.mover(t.reserva_id, m, cero);
                }
                TipoTxIfpe::Retiro | TipoTxIfpe::TraspasoASofom => {
                    cuentas.mover(t.cuenta_origen_id, -m, cero);
                    reservas.mover(t.reserva_id, -m, cero);
                }
                TipoTxIfpe::Transferencia => {
                    cuentas.mover(t.cuenta_origen_id, -m, cero);
                    cuentas.mover(t.cuenta_destino_id, m, cero);
                }
                TipoTxIfpe::Retencion => cuentas.mover(t.cuenta_origen_id, -m, m),
                TipoTxIfpe::LiberacionRetencion => cuentas.mover(t.cuenta_origen_id, m, -m),
            }
        }
    }
    let mut r = v.resultado("IFPE", &cabeza, falla);
    r.discrepancias.append(&mut discrepancias);
    if r.falla.is_none() {
        r.anclas_verificadas = anclas_ok;
        verificar_conteo(&mut r, conteo_ledger(&mut tx, "ifpe.transacciones").await?, &cabeza);
        // Anclas que apuntan más allá de la cabeza: el ledger fue truncado.
        let mut maxima = 0_i64;
        for (sec, h, corte) in &anclas {
            if *sec > r.ultima_secuencia {
                r.discrepancias.push(format!(
                    "el corte {corte} ancla la secuencia {sec}, que ya no existe (cabeza en {}): ledger truncado",
                    r.ultima_secuencia
                ));
            } else if *sec == 0 {
                if h != GENESIS {
                    r.discrepancias
                        .push(format!("el corte {corte} ancla la secuencia 0 con un hash distinto al génesis"));
                } else {
                    r.anclas_verificadas += 1;
                }
            }
            if *sec < maxima {
                r.discrepancias.push(format!("el corte {corte} ancla la secuencia {sec}, anterior a un corte previo"));
            }
            maxima = maxima.max(*sec);
        }
        let vivos: Vec<(Uuid, Decimal, Decimal)> =
            sqlx::query_as("SELECT id, saldo_disponible, saldo_bloqueado FROM ifpe.cuentas")
                .fetch_all(&mut *tx)
                .await?;
        cuentas.conciliar("cuenta IFPE", &vivos, &mut r);
        let sombra: Vec<(Uuid, Decimal, Decimal)> =
            sqlx::query_as("SELECT cuenta_id, disponible, bloqueado FROM ifpe.saldos_ledger")
                .fetch_all(&mut *tx)
                .await?;
        cuentas.conciliar("sombra de cuenta IFPE", &sombra, &mut r);
        let reserva: Vec<(Uuid, Decimal, Decimal)> =
            sqlx::query_as("SELECT id, saldo, 0::numeric FROM ifpe.reserva_custodia").fetch_all(&mut *tx).await?;
        reservas.conciliar("reserva en custodia", &reserva, &mut r);
        let sombra: Vec<(Uuid, Decimal, Decimal)> =
            sqlx::query_as("SELECT reserva_id, saldo, 0::numeric FROM ifpe.reserva_ledger").fetch_all(&mut *tx).await?;
        reservas.conciliar("sombra de reserva", &sombra, &mut r);
    }
    tx.commit().await?;
    Ok(r.cerrar())
}

/// Recorre todo el ledger SOFOM (foto consistente): cadena de hashes,
/// reproducción de cuentas de inversión, caja y créditos, hash de los términos
/// de cada mutuo y movimientos espejo con la IFPE.
pub async fn verificar_cadena_sofom(pool: &PgPool) -> AppResult<ResultadoVerificacion> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Sofom).await?;
    let cabeza: LedgerCabeza =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM sofom.ledger_cabeza").fetch_one(&mut *tx).await?;
    let mut v = VerificadorCadena::default();
    let mut cuentas = Reproduccion::default();
    let mut cajas = Reproduccion::default();
    let mut creditos = Reproduccion::default();
    let mut falla = None;
    let mut desde = i64::MIN;
    'lotes: loop {
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
                falla = Some(f);
                break 'lotes;
            }
            let (m, cero) = (t.monto, Decimal::ZERO);
            match t.tipo {
                TipoTxSofom::AportacionMutuo => {
                    cuentas.mover(t.cuenta_id, cero, m);
                    cajas.mover(t.caja_id, m, cero);
                }
                TipoTxSofom::VencimientoMutuo => cuentas.mover(t.cuenta_id, m, -m),
                TipoTxSofom::PagoInteresMutuo => cuentas.mover(t.cuenta_id, m, cero),
                TipoTxSofom::RetornoAIfpe => {
                    cuentas.mover(t.cuenta_id, -m, cero);
                    cajas.mover(t.caja_id, -m, cero);
                }
                TipoTxSofom::DesembolsoCredito => {
                    cajas.mover(t.caja_id, -m, cero);
                    creditos.mover(t.credito_id, m, cero);
                }
                TipoTxSofom::CobroCredito | TipoTxSofom::AportacionCapital => cajas.mover(t.caja_id, m, cero),
            }
        }
    }
    let mut r = v.resultado("SOFOM", &cabeza, falla);
    if r.falla.is_none() {
        verificar_conteo(&mut r, conteo_ledger(&mut tx, "sofom.transacciones").await?, &cabeza);
        // El principal de cada cobro reduce el saldo del crédito (el movimiento lleva principal + interés).
        let cobros: Vec<(Uuid, Decimal)> =
            sqlx::query_as("SELECT credito_id, principal FROM sofom.cobros").fetch_all(&mut *tx).await?;
        for (credito, principal) in cobros {
            creditos.mover(Some(credito), -principal, Decimal::ZERO);
        }
        let consultas: [(&str, &str, &Reproduccion); 6] = [
            ("cuenta SOFOM", "SELECT id, saldo_disponible, saldo_bloqueado FROM sofom.cuentas", &cuentas),
            ("sombra de cuenta SOFOM", "SELECT cuenta_id, disponible, bloqueado FROM sofom.saldos_ledger", &cuentas),
            ("caja SOFOM", "SELECT id, saldo, 0::numeric FROM sofom.caja", &cajas),
            ("sombra de caja", "SELECT caja_id, saldo, 0::numeric FROM sofom.caja_ledger", &cajas),
            ("crédito", "SELECT id, saldo_insoluto, 0::numeric FROM sofom.creditos", &creditos),
            (
                "sombra de crédito",
                "SELECT credito_id, saldo_insoluto, 0::numeric FROM sofom.creditos_ledger",
                &creditos,
            ),
        ];
        for (nombre, sql, rep) in consultas {
            let saldos: Vec<(Uuid, Decimal, Decimal)> = sqlx::query_as(sql).fetch_all(&mut *tx).await?;
            rep.conciliar(nombre, &saldos, &mut r);
        }
        // Los términos vigentes de cada mutuo deben ser los que el cliente aceptó.
        let contratos: Vec<ContratoMutuo> =
            sqlx::query_as("SELECT * FROM sofom.contratos_mutuo ORDER BY folio").fetch_all(&mut *tx).await?;
        for c in &contratos {
            if puente::verificar_terminos(c).is_err() {
                r.discrepancias.push(format!("mutuo folio {}: los términos no coinciden con hash_terminos", c.folio));
            }
        }
    }
    tx.commit().await?;
    let mut espejos = verificar_espejos(pool).await?;
    r.discrepancias.append(&mut espejos);
    Ok(r.cerrar())
}

/// Cada movimiento del puente existe en ambos ledgers, con referencias
/// recíprocas, el mismo monto y el tipo correspondiente. Se lee con el rol del
/// puente (el único que ve ambos ledgers) en una sola foto consistente.
async fn verificar_espejos(pool: &PgPool) -> AppResult<Vec<String>> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Puente).await?;
    let rotos: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT 'IFPE', i.secuencia, i.tipo FROM ifpe.transacciones i \
           LEFT JOIN sofom.transacciones s ON s.id = i.ref_movimiento_espejo \
          WHERE i.tipo IN ('TRASPASO_A_SOFOM', 'RETORNO_DE_SOFOM') \
            AND (s.id IS NULL OR s.ref_movimiento_espejo IS DISTINCT FROM i.id OR s.monto <> i.monto \
                 OR s.tipo <> CASE i.tipo WHEN 'TRASPASO_A_SOFOM' THEN 'APORTACION_MUTUO' ELSE 'RETORNO_A_IFPE' END) \
         UNION ALL \
         SELECT 'SOFOM', s.secuencia, s.tipo FROM sofom.transacciones s \
           LEFT JOIN ifpe.transacciones i ON i.id = s.ref_movimiento_espejo \
          WHERE s.tipo IN ('APORTACION_MUTUO', 'RETORNO_A_IFPE') \
            AND (i.id IS NULL OR i.ref_movimiento_espejo IS DISTINCT FROM s.id OR i.monto <> s.monto \
                 OR i.tipo <> CASE s.tipo WHEN 'APORTACION_MUTUO' THEN 'TRASPASO_A_SOFOM' ELSE 'RETORNO_DE_SOFOM' END)",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rotos
        .into_iter()
        .map(|(ledger, sec, tipo)| format!("{ledger} secuencia {sec} ({tipo}) sin movimiento espejo válido"))
        .collect())
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
