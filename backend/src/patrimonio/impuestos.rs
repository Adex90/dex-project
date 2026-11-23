//! Centro fiscal del cliente (migración 0021, módulo 26 del roadmap): su
//! perfil fiscal, el W-8BEN, el resumen del ejercicio y la exportación
//! contable. La API usa los nombres en inglés del frontend
//! (`frontend/src/types/taxes.ts`).
//!
//! **Resumen del ejercicio** (el año calendario de la Ciudad de México, que es
//! el ejercicio fiscal; el del año en curso, a la fecha). Se deriva al leerlo,
//! en una sola foto (REPEATABLE READ, el rol del cliente y RLS), de los
//! ledgers encadenados por hash, como los documentos (`documentos.rs`):
//!
//! - **Mercado** (`mercado.ordenes`): las compras por su importe; de cada
//!   venta, su importe, el costo que liberó (el costo promedio, exacto a
//!   10^-8, que comprueba la base de datos) y su resultado. Comisión: cero.
//! - **Plazo fijo** (`sofom.contratos_mutuo`): el interés de cada mutuo en el
//!   ejercicio en que se pagó (su RETORNO_DE_SOFOM en el ledger IFPE), y los
//!   que siguen abiertos al cierre del periodo.
//! - **ISR.** El retenido es el que registran los ledgers: hoy ninguno (la
//!   SOFOM paga el interés íntegro; DEPLOYMENT.md lo lista como pendiente).
//!   La estimación usa las tasas del ejercicio (`kyc.parametros_fiscales`);
//!   sin ellas no se estima:
//!   - intereses: capital × tasa anual de la LIF del año ÷ 365 × días del
//!     plazo en ese año (LISR art. 54), en el ejercicio del pago;
//!   - acciones y fondos (RENTA_VARIABLE, FONDO): la tasa del art. 129 LISR
//!     sobre la ganancia neta del ejercicio; una pérdida neta no paga;
//!   - materias primas y renta fija: su resultado se acumula en la
//!     declaración anual; se informa, no se estima.
//!
//!   Es una estimación para el cliente, no la determinación del impuesto.
//!
//! **W-8BEN.** Cada certificación es una versión inmutable y encadenada
//! (`SC-W8BEN-v1`) cuya Parte I copia la base de datos del expediente. El
//! estado: `pending` sin ninguna; `outdated` si el expediente ya no coincide
//! con la Parte I firmada (un cambio de circunstancias pide una nueva);
//! `expired` pasado el 31 de diciembre del tercer año siguiente a la firma;
//! si no, `active`. El backend recalcula la cadena al leerla.
//!
//! **Exportación contable** (RFC 4180, CRLF): cada movimiento del ejercicio
//! que cambia el saldo, con su operación (activo, cantidad, precio, costo y
//! resultado) o su interés, el saldo después y su secuencia y hash en el
//! ledger IFPE. Las retenciones y los compromisos de órdenes son internos: no
//! se listan, como en los estados de cuenta (sin aviso al cliente).

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, TimeZone, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::documentos::{asiento, Movimiento, TipoAsiento, COLUMNAS_MOVIMIENTO};
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    kyc::beneficiarios::Autor,
    ledger::hash::{sha256_hex, GENESIS},
};

/// Desfase de la Ciudad de México (sin horario de verano desde 2022), como `spei::hoy_cdmx`.
const DESFASE_CDMX_HORAS: i64 = 6;
/// El primer ejercicio que se puede consultar.
pub const PRIMER_EJERCICIO: i32 = 2000;
/// La tasa anual de retención se prorratea por días naturales (365).
const DIAS_PRORRATEO: i64 = 365;
const VERSION_W8BEN: &str = "SC-W8BEN-v1";
const VERSION_RESUMEN: &str = "SC-FISCAL-v1";
const FIRMA_MAXIMA: usize = 200;

/// Las columnas de la exportación contable, en orden.
pub const ENCABEZADO_CSV: [&str; 16] = [
    "date",
    "type",
    "reference",
    "ticker",
    "asset_name",
    "asset_class",
    "quantity",
    "price",
    "amount",
    "commission",
    "cost_basis",
    "realized_result",
    "interest",
    "balance",
    "ledger_sequence",
    "ledger_hash",
];

// -----------------------------------------------------------------------------
// Lo que se lee
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct Perfil {
    tipo_persona: String,
    /// `kyc.nombre_legal`: como lo firma el W-8BEN.
    nombre_legal: String,
    rfc: Option<String>,
    curp: Option<String>,
    nacionalidad: String,
    pais_residencia: String,
    fecha_nacimiento: Option<NaiveDate>,
    /// `kyc.domicilio_w8ben`: el domicilio permanente en una línea.
    domicilio: String,
    domicilio_completo: bool,
    estatus_kyc: String,
}

const CONSULTA_PERFIL: &str =
    "SELECT tipo_persona, kyc.nombre_legal(nombre, apellido_paterno, apellido_materno) AS nombre_legal, \
            rfc::text AS rfc, curp::text AS curp, nacionalidad::text AS nacionalidad, \
            pais_residencia::text AS pais_residencia, fecha_nacimiento, \
            kyc.domicilio_w8ben(domicilio) AS domicilio, \
            (coalesce(btrim(domicilio ->> 'calle'), '') <> '' \
             AND coalesce(btrim(domicilio ->> 'codigo_postal'), '') <> '') AS domicilio_completo, \
            estatus_kyc \
     FROM kyc.clientes WHERE id = $1";

#[derive(Debug, Clone, sqlx::FromRow)]
struct Orden {
    id: Uuid,
    secuencia: i64,
    ticker: String,
    nombre_activo: String,
    clase: String,
    lado: String,
    cantidad: Decimal,
    precio: Decimal,
    monto: Decimal,
    comision: Decimal,
    costo_liberado: Option<Decimal>,
    creado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Mutuo {
    folio: i64,
    principal: Decimal,
    fecha_inicio: NaiveDate,
    plazo_dias: i32,
    interes_pagado: Decimal,
    creado_en: DateTime<Utc>,
    /// El RETORNO_DE_SOFOM que lo pagó (ledger IFPE), si ya se pagó.
    pagado_en: Option<DateTime<Utc>>,
    secuencia_pago: Option<i64>,
    /// El TRASPASO_A_SOFOM que lo fondeó.
    secuencia_aportacion: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct FilaW8ben {
    cliente_id: Uuid,
    version: i32,
    nombre_legal: String,
    pais_ciudadania: String,
    pais_residencia: String,
    domicilio: String,
    tin_extranjero: String,
    fecha_nacimiento: NaiveDate,
    pais_tratado: String,
    beneficiario_efectivo: bool,
    no_persona_eua: bool,
    residente_tratado: bool,
    firma: String,
    firmado_en: DateTime<Utc>,
    vigente_hasta: NaiveDate,
    sesion_id: Option<Uuid>,
    ip: Option<String>,
    dispositivo: Option<String>,
    hash_anterior: String,
    hash: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Parametros {
    ejercicio: i16,
    tasa_retencion_intereses: Decimal,
    fundamento_intereses: String,
    tasa_ganancia_acciones: Decimal,
    fundamento_acciones: String,
    tasa_dividendos_eua_tratado: Decimal,
    tasa_dividendos_eua_sin_w8ben: Decimal,
    fundamento_dividendos_eua: String,
}

/// Todo lo del cliente, en una sola foto.
struct Datos {
    cliente_id: Uuid,
    perfil: Perfil,
    cuenta: Option<Uuid>,
    movimientos: Vec<Movimiento>,
    ordenes: Vec<Orden>,
    mutuos: Vec<Mutuo>,
    formularios: Vec<FilaW8ben>,
    parametros: Vec<Parametros>,
}

async fn leer(pool: &PgPool, cliente_id: Uuid) -> AppResult<Datos> {
    let mut tx = db::begin_lectura_instantanea_cliente(pool, cliente_id).await?;
    let perfil: Perfil = sqlx::query_as(CONSULTA_PERFIL)
        .bind(cliente_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NoAutenticado)?;
    let cuenta: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM ifpe.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'")
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?;
    let movimientos: Vec<Movimiento> = match cuenta {
        Some(c) => {
            sqlx::query_as(&format!(
                "SELECT {COLUMNAS_MOVIMIENTO} FROM ifpe.transacciones \
                 WHERE cuenta_origen_id = $1 OR cuenta_destino_id = $1 ORDER BY secuencia"
            ))
            .bind(c)
            .fetch_all(&mut *tx)
            .await?
        }
        None => Vec::new(),
    };
    let ordenes: Vec<Orden> = sqlx::query_as(
        "SELECT o.id, o.secuencia, o.ticker, a.nombre AS nombre_activo, a.clase, o.lado, o.cantidad, o.precio, \
                o.monto, o.comision, o.costo_liberado, o.creado_en \
         FROM mercado.ordenes o JOIN mercado.activos a ON a.ticker = o.ticker \
         WHERE o.cliente_id = $1 ORDER BY o.secuencia",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    let mutuos: Vec<Mutuo> = sqlx::query_as(
        "SELECT c.folio, c.principal, c.fecha_inicio, c.plazo_dias, c.interes_pagado, c.creado_en, \
                r.creado_en AS pagado_en, r.secuencia AS secuencia_pago, \
                (SELECT a.secuencia FROM ifpe.transacciones a WHERE a.id = c.tx_aportacion_ifpe_id) \
                    AS secuencia_aportacion \
         FROM sofom.contratos_mutuo c \
         LEFT JOIN LATERAL ( \
             SELECT t.creado_en, t.secuencia FROM sofom.transacciones s \
             JOIN ifpe.transacciones t ON t.ref_movimiento_espejo = s.id AND t.tipo = 'RETORNO_DE_SOFOM' \
             WHERE s.contrato_id = c.id AND s.tipo = 'RETORNO_A_IFPE' \
             ORDER BY t.secuencia LIMIT 1) r ON true \
         WHERE c.cliente_id = $1 ORDER BY c.folio",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    let formularios = leer_formularios(&mut tx, cliente_id).await?;
    let parametros: Vec<Parametros> = sqlx::query_as(
        "SELECT ejercicio, tasa_retencion_intereses, fundamento_intereses, tasa_ganancia_acciones, \
                fundamento_acciones, tasa_dividendos_eua_tratado, tasa_dividendos_eua_sin_w8ben, \
                fundamento_dividendos_eua \
         FROM kyc.parametros_fiscales ORDER BY ejercicio",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Datos { cliente_id, perfil, cuenta, movimientos, ordenes, mutuos, formularios, parametros })
}

async fn leer_formularios(tx: &mut sqlx::PgConnection, cliente_id: Uuid) -> AppResult<Vec<FilaW8ben>> {
    Ok(sqlx::query_as(
        "SELECT cliente_id, version, nombre_legal, pais_ciudadania::text AS pais_ciudadania, \
                pais_residencia::text AS pais_residencia, domicilio, tin_extranjero::text AS tin_extranjero, \
                fecha_nacimiento, pais_tratado::text AS pais_tratado, beneficiario_efectivo, no_persona_eua, \
                residente_tratado, firma, firmado_en, vigente_hasta, sesion_id, host(ip) AS ip, dispositivo, \
                hash_anterior::text AS hash_anterior, hash::text AS hash \
         FROM kyc.formularios_w8ben WHERE cliente_id = $1 ORDER BY version",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?)
}

// -----------------------------------------------------------------------------
// El ejercicio (año calendario de la Ciudad de México)
// -----------------------------------------------------------------------------

fn local(t: DateTime<Utc>) -> DateTime<Utc> {
    t - Duration::hours(DESFASE_CDMX_HORAS)
}

/// El ejercicio de un instante.
fn ejercicio_de(t: DateTime<Utc>) -> i32 {
    local(t).year()
}

/// Inicio (incluido) y fin (excluido) de un ejercicio, en UTC.
fn limites(anio: i32) -> (DateTime<Utc>, DateTime<Utc>) {
    let a_utc = |a: i32| {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(a, 1, 1).expect("1 de enero").and_hms_opt(0, 0, 0).expect("medianoche"),
        ) + Duration::hours(DESFASE_CDMX_HORAS)
    };
    (a_utc(anio), a_utc(anio + 1))
}

/// Días del plazo `[inicio, inicio + dias)` que caen en cada año.
fn dias_por_anio(inicio: NaiveDate, dias: i32) -> Vec<(i32, i64)> {
    let fin = inicio + Days::new(u64::try_from(dias.max(0)).unwrap_or(0));
    let mut d = inicio;
    let mut tramos = Vec::new();
    while d < fin {
        let siguiente = NaiveDate::from_ymd_opt(d.year() + 1, 1, 1).expect("1 de enero");
        let hasta = fin.min(siguiente);
        tramos.push((d.year(), (hasta - d).num_days()));
        d = hasta;
    }
    tramos
}

fn centavos(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

// -----------------------------------------------------------------------------
// La vista (`TaxCenter`)
// -----------------------------------------------------------------------------

/// `TaxProfile`: lo fiscal del expediente.
#[derive(Debug, Clone, Serialize)]
pub struct PerfilFiscal {
    /// El nombre legal, como lo firma el W-8BEN.
    pub name: String,
    pub rfc: Option<String>,
    pub curp: Option<String>,
    /// "individual" o "business".
    pub person_type: &'static str,
    /// País de ciudadanía (ISO 3166-1).
    pub nationality: String,
    /// País de residencia fiscal.
    pub tax_residence: String,
    pub identity_verified: bool,
    /// El domicilio permanente en una línea.
    pub address: String,
    pub birth_date: Option<NaiveDate>,
}

/// Una versión del W-8BEN, completa (`W8benForm`).
#[derive(Debug, Clone, Serialize)]
pub struct FormularioW8ben {
    pub version: i32,
    pub signed_at: DateTime<Utc>,
    /// Último día de vigencia (incluido).
    pub valid_until: NaiveDate,
    pub name: String,
    pub citizenship: String,
    pub residence: String,
    pub address: String,
    pub foreign_tin: String,
    pub birth_date: NaiveDate,
    pub treaty_country: String,
    pub beneficial_owner: bool,
    pub not_us_person: bool,
    pub treaty_resident: bool,
    pub signature: String,
    pub device: Option<String>,
    pub ip: Option<String>,
    pub hash: String,
    pub previous_hash: String,
}

/// Una versión del historial (`W8benSummary`).
#[derive(Debug, Clone, Serialize)]
pub struct ResumenW8ben {
    pub version: i32,
    pub signed_at: DateTime<Utc>,
    pub valid_until: NaiveDate,
    pub device: Option<String>,
    pub hash: String,
}

/// `W8benState`.
#[derive(Debug, Clone, Serialize)]
pub struct EstadoW8ben {
    /// "active", "expired", "outdated" o "pending".
    pub status: &'static str,
    pub current: Option<FormularioW8ben>,
    /// La más reciente primero.
    pub history: Vec<ResumenW8ben>,
    /// El backend recalculó el hash y el enlace de cada versión.
    pub chain_verified: bool,
    pub can_certify: bool,
    /// Por qué no puede certificar: "identity_unverified", "business",
    /// "us_person", "non_resident" o "missing_data".
    pub blocked_reason: Option<&'static str>,
    /// Lo que EUA retiene hoy sobre dividendos de fuente estadounidense (%):
    /// el del tratado con un W-8BEN vigente, si no la tasa legal.
    pub us_dividend_rate: Option<String>,
    pub us_dividend_rate_treaty: Option<String>,
    pub us_dividend_rate_statutory: Option<String>,
    pub us_dividend_basis: Option<String>,
}

/// Lo de un activo en el ejercicio (`AssetTaxLine`).
#[derive(Debug, Clone, Serialize)]
pub struct PorActivo {
    pub ticker: String,
    pub asset_name: String,
    /// "equity", "fund", "commodity" o "fixed_income".
    pub asset_class: &'static str,
    /// "listed_shares" (LISR art. 129) o "annual_return" (se acumula).
    pub treatment: &'static str,
    pub buys: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub bought: Decimal,
    pub sells: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub proceeds: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cost_basis: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub net_result: Decimal,
}

/// `MarketTaxSummary`.
#[derive(Debug, Clone, Serialize)]
pub struct Mercado {
    pub buys: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub bought: Decimal,
    pub sells: i64,
    /// Importe bruto de las ventas.
    #[serde(with = "rust_decimal::serde::str")]
    pub proceeds: Decimal,
    /// Costo promedio liberado por las ventas.
    #[serde(with = "rust_decimal::serde::str")]
    pub cost_basis: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub gains: Decimal,
    /// En positivo.
    #[serde(with = "rust_decimal::serde::str")]
    pub losses: Decimal,
    /// `proceeds - cost_basis`.
    #[serde(with = "rust_decimal::serde::str")]
    pub net_result: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub commissions: Decimal,
    pub by_asset: Vec<PorActivo>,
}

/// `FixedTermTaxSummary`.
#[derive(Debug, Clone, Serialize)]
pub struct PlazoFijo {
    pub contracts_paid: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub principal_returned: Decimal,
    /// Interés nominal pagado en el ejercicio.
    #[serde(with = "rust_decimal::serde::str")]
    pub interest_paid: Decimal,
    /// Abiertos al cierre del periodo: pagan al vencer.
    pub contracts_open: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub principal_open: Decimal,
}

/// Las tasas del ejercicio (`TaxRates`), en %.
#[derive(Debug, Clone, Serialize)]
pub struct Tasas {
    #[serde(with = "rust_decimal::serde::str")]
    pub interest_withholding_annual: Decimal,
    pub interest_basis: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub listed_shares: Decimal,
    pub listed_shares_basis: String,
}

/// `IsrEstimate`: al centavo.
#[derive(Debug, Clone, Serialize)]
pub struct Estimacion {
    /// Retención sobre el capital de los mutuos pagados en el ejercicio.
    #[serde(with = "rust_decimal::serde::str")]
    pub interest_withholding: Decimal,
    /// Ganancia neta de acciones y fondos (la base del art. 129).
    #[serde(with = "rust_decimal::serde::str")]
    pub listed_shares_net: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub listed_shares_tax: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Decimal,
}

/// `IsrSummary`.
#[derive(Debug, Clone, Serialize)]
pub struct Isr {
    /// El ISR que registran los ledgers como retenido en el ejercicio.
    #[serde(with = "rust_decimal::serde::str")]
    pub withheld: Decimal,
    pub estimated: Option<Estimacion>,
    pub rates: Option<Tasas>,
    /// Por qué no hay estimación: "no_rates" (el ejercicio, o un año del
    /// plazo de un mutuo, no tiene tasas publicadas).
    pub not_estimated_reason: Option<&'static str>,
    /// Resultado neto de materias primas y renta fija: se acumula en la
    /// declaración anual; no se estima.
    #[serde(with = "rust_decimal::serde::str")]
    pub annual_return_result: Decimal,
}

/// `TaxYearSummary`.
#[derive(Debug, Clone, Serialize)]
pub struct ResumenFiscal {
    pub year: i32,
    /// El año en curso: las cifras cambian hasta que cierre.
    pub in_progress: bool,
    pub period_start: DateTime<Utc>,
    /// El último instante incluido (ahora, en el año en curso).
    pub period_end: DateTime<Utc>,
    pub market: Mercado,
    pub fixed_term: PlazoFijo,
    pub isr: Isr,
    /// SHA-256 del resumen (`SC-FISCAL-v1`).
    pub reference: String,
}

/// `TaxCenter`: `GET /api/v1/wealth/impuestos`.
#[derive(Debug, Clone, Serialize)]
pub struct CentroFiscal {
    pub profile: PerfilFiscal,
    pub w8ben: EstadoW8ben,
    /// Ejercicios con actividad, del más reciente al más antiguo (siempre el actual).
    pub years: Vec<i32>,
    pub summary: ResumenFiscal,
}

fn clase_y_tratamiento(clase: &str) -> (&'static str, &'static str) {
    match clase {
        "RENTA_VARIABLE" => ("equity", "listed_shares"),
        "FONDO" => ("fund", "listed_shares"),
        "MATERIA_PRIMA" => ("commodity", "annual_return"),
        _ => ("fixed_income", "annual_return"),
    }
}

fn perfil_fiscal(p: &Perfil) -> PerfilFiscal {
    PerfilFiscal {
        name: p.nombre_legal.clone(),
        rfc: p.rfc.clone(),
        curp: p.curp.clone(),
        person_type: if p.tipo_persona == "FISICA" { "individual" } else { "business" },
        nationality: p.nacionalidad.clone(),
        tax_residence: p.pais_residencia.clone(),
        identity_verified: p.estatus_kyc == "VERIFICADO",
        address: p.domicilio.clone(),
        birth_date: p.fecha_nacimiento,
    }
}

// -----------------------------------------------------------------------------
// W-8BEN
// -----------------------------------------------------------------------------

/// Quién puede certificar, y si no, por qué.
fn bloqueo(p: &Perfil) -> Option<&'static str> {
    if p.estatus_kyc != "VERIFICADO" {
        Some("identity_unverified")
    } else if p.tipo_persona != "FISICA" {
        Some("business")
    } else if p.nacionalidad == "US" || p.pais_residencia == "US" {
        Some("us_person")
    } else if p.pais_residencia != "MX" {
        Some("non_resident")
    } else if p.rfc.is_none() || p.fecha_nacimiento.is_none() || !p.domicilio_completo {
        Some("missing_data")
    } else {
        None
    }
}

fn error_de_bloqueo(motivo: &str) -> AppError {
    match motivo {
        "identity_unverified" => {
            AppError::negocio("KYC_NO_VERIFICADO", "verifica tu identidad (KYC) para certificar el W-8BEN")
        }
        "business" => AppError::negocio(
            "W8BEN_PERSONA_MORAL",
            "el W-8BEN es de personas físicas; una empresa presenta el W-8BEN-E",
        ),
        "us_person" => {
            AppError::negocio("W8BEN_PERSONA_EUA", "un ciudadano o residente de EUA presenta el W-9, no el W-8BEN")
        }
        "non_resident" => {
            AppError::negocio("W8BEN_RESIDENCIA", "aquí certifica un residente de México (tratado México–EUA)")
        }
        _ => AppError::negocio(
            "W8BEN_DATOS_INCOMPLETOS",
            "tu expediente no tiene RFC, fecha de nacimiento o domicilio completos",
        ),
    }
}

/// La Parte I firmada ya no coincide con el expediente: un cambio de
/// circunstancias pide un formulario nuevo.
fn desactualizado(f: &FilaW8ben, p: &Perfil) -> bool {
    f.nombre_legal != p.nombre_legal
        || f.pais_ciudadania != p.nacionalidad
        || f.pais_residencia != p.pais_residencia
        || f.domicilio != p.domicilio
        || Some(&f.tin_extranjero) != p.rfc.as_ref()
        || Some(f.fecha_nacimiento) != p.fecha_nacimiento
}

/// La firma como la compara la base de datos (`kyc.firma_normalizada`).
pub fn firma_normalizada(firma: &str) -> String {
    firma.split_whitespace().collect::<Vec<_>>().join(" ").nfc().collect::<String>().to_uppercase()
}

fn campo(p: &mut String, valor: Option<&str>) {
    if !p.is_empty() {
        p.push('|');
    }
    match valor {
        None => p.push('~'),
        Some(v) => {
            p.push_str(&v.len().to_string());
            p.push(':');
            p.push_str(v);
        }
    }
}

/// `SC-W8BEN-v1` (ver la migración 0021).
fn payload_w8ben(f: &FilaW8ben) -> String {
    let mut p = String::new();
    campo(&mut p, Some(VERSION_W8BEN));
    campo(&mut p, Some(&f.cliente_id.to_string()));
    campo(&mut p, Some(&f.version.to_string()));
    campo(&mut p, Some(&f.firmado_en.timestamp_micros().to_string()));
    campo(&mut p, Some(&f.nombre_legal));
    campo(&mut p, Some(&f.pais_ciudadania));
    campo(&mut p, Some(&f.pais_residencia));
    campo(&mut p, Some(&f.domicilio));
    campo(&mut p, Some(&f.tin_extranjero));
    campo(&mut p, Some(&f.fecha_nacimiento.format("%Y-%m-%d").to_string()));
    campo(&mut p, Some(&f.pais_tratado));
    campo(&mut p, Some(if f.beneficiario_efectivo { "true" } else { "false" }));
    campo(&mut p, Some(if f.no_persona_eua { "true" } else { "false" }));
    campo(&mut p, Some(if f.residente_tratado { "true" } else { "false" }));
    campo(&mut p, Some(&f.firma));
    campo(&mut p, Some(&f.vigente_hasta.format("%Y-%m-%d").to_string()));
    campo(&mut p, f.sesion_id.map(|s| s.to_string()).as_deref());
    campo(&mut p, f.ip.as_deref());
    campo(&mut p, f.dispositivo.as_deref());
    campo(&mut p, Some(&f.hash_anterior));
    p
}

/// Recalcula cada hash y cada enlace de la cadena, desde el génesis.
fn cadena_verificada(formularios: &[FilaW8ben]) -> bool {
    let mut anterior = GENESIS.to_owned();
    for (i, f) in formularios.iter().enumerate() {
        if usize::try_from(f.version).ok() != Some(i + 1)
            || f.hash_anterior != anterior
            || sha256_hex(payload_w8ben(f).as_bytes()) != f.hash
        {
            return false;
        }
        anterior = f.hash.clone();
    }
    true
}

fn tasa(d: Decimal) -> String {
    let mut t = d.normalize();
    if t.scale() < 2 {
        t.rescale(2);
    }
    t.to_string()
}

fn estado_w8ben(datos: &Datos, hoy: NaiveDate) -> EstadoW8ben {
    let actual = datos.formularios.last();
    let status = match actual {
        None => "pending",
        Some(f) if desactualizado(f, &datos.perfil) => "outdated",
        Some(f) if hoy > f.vigente_hasta => "expired",
        Some(_) => "active",
    };
    let bloqueado = bloqueo(&datos.perfil);
    let parametros = datos.parametros.iter().find(|p| i32::from(p.ejercicio) == hoy.year());
    EstadoW8ben {
        status,
        current: actual.map(|f| FormularioW8ben {
            version: f.version,
            signed_at: f.firmado_en,
            valid_until: f.vigente_hasta,
            name: f.nombre_legal.clone(),
            citizenship: f.pais_ciudadania.clone(),
            residence: f.pais_residencia.clone(),
            address: f.domicilio.clone(),
            foreign_tin: f.tin_extranjero.clone(),
            birth_date: f.fecha_nacimiento,
            treaty_country: f.pais_tratado.clone(),
            beneficial_owner: f.beneficiario_efectivo,
            not_us_person: f.no_persona_eua,
            treaty_resident: f.residente_tratado,
            signature: f.firma.clone(),
            device: f.dispositivo.clone(),
            ip: f.ip.clone(),
            hash: f.hash.clone(),
            previous_hash: f.hash_anterior.clone(),
        }),
        history: datos
            .formularios
            .iter()
            .rev()
            .map(|f| ResumenW8ben {
                version: f.version,
                signed_at: f.firmado_en,
                valid_until: f.vigente_hasta,
                device: f.dispositivo.clone(),
                hash: f.hash.clone(),
            })
            .collect(),
        chain_verified: cadena_verificada(&datos.formularios),
        can_certify: bloqueado.is_none(),
        blocked_reason: bloqueado,
        us_dividend_rate: parametros.map(|p| {
            tasa(if status == "active" { p.tasa_dividendos_eua_tratado } else { p.tasa_dividendos_eua_sin_w8ben })
        }),
        us_dividend_rate_treaty: parametros.map(|p| tasa(p.tasa_dividendos_eua_tratado)),
        us_dividend_rate_statutory: parametros.map(|p| tasa(p.tasa_dividendos_eua_sin_w8ben)),
        us_dividend_basis: parametros.map(|p| p.fundamento_dividendos_eua.clone()),
    }
}

// -----------------------------------------------------------------------------
// El resumen del ejercicio
// -----------------------------------------------------------------------------

/// Retención estimada sobre el capital de un mutuo: la tasa anual de cada año
/// del plazo ÷ 365 × los días de ese año (al centavo). `None` si algún año no
/// tiene tasas publicadas.
fn retencion_mutuo(m: &Mutuo, parametros: &[Parametros]) -> Option<Decimal> {
    let mut total = Decimal::ZERO;
    for (anio, dias) in dias_por_anio(m.fecha_inicio, m.plazo_dias) {
        let p = parametros.iter().find(|p| i32::from(p.ejercicio) == anio)?;
        total += m.principal * p.tasa_retencion_intereses / Decimal::ONE_HUNDRED * Decimal::from(dias)
            / Decimal::from(DIAS_PRORRATEO);
    }
    Some(centavos(total))
}

fn resumen(datos: &Datos, anio: i32, ahora: DateTime<Utc>) -> ResumenFiscal {
    let (inicio, fin) = limites(anio);
    let en_curso = ahora < fin;
    let en_periodo = |t: DateTime<Utc>| t >= inicio && t < fin;

    // Mercado.
    let mut por_activo: Vec<PorActivo> = Vec::new();
    let mut mercado = Mercado {
        buys: 0,
        bought: Decimal::ZERO,
        sells: 0,
        proceeds: Decimal::ZERO,
        cost_basis: Decimal::ZERO,
        gains: Decimal::ZERO,
        losses: Decimal::ZERO,
        net_result: Decimal::ZERO,
        commissions: Decimal::ZERO,
        by_asset: Vec::new(),
    };
    let mut neto_acciones = Decimal::ZERO;
    let mut neto_acumulable = Decimal::ZERO;
    for o in datos.ordenes.iter().filter(|o| en_periodo(o.creado_en)) {
        let (clase, tratamiento) = clase_y_tratamiento(&o.clase);
        let i = match por_activo.iter().position(|a| a.ticker == o.ticker) {
            Some(i) => i,
            None => {
                por_activo.push(PorActivo {
                    ticker: o.ticker.clone(),
                    asset_name: o.nombre_activo.clone(),
                    asset_class: clase,
                    treatment: tratamiento,
                    buys: 0,
                    bought: Decimal::ZERO,
                    sells: 0,
                    proceeds: Decimal::ZERO,
                    cost_basis: Decimal::ZERO,
                    net_result: Decimal::ZERO,
                });
                por_activo.len() - 1
            }
        };
        let activo = &mut por_activo[i];
        mercado.commissions += o.comision;
        if o.lado == "COMPRA" {
            mercado.buys += 1;
            mercado.bought += o.monto;
            activo.buys += 1;
            activo.bought += o.monto;
        } else {
            let costo = o.costo_liberado.unwrap_or_default();
            let resultado = o.monto - costo;
            mercado.sells += 1;
            mercado.proceeds += o.monto;
            mercado.cost_basis += costo;
            if resultado > Decimal::ZERO {
                mercado.gains += resultado;
            } else {
                mercado.losses -= resultado;
            }
            activo.sells += 1;
            activo.proceeds += o.monto;
            activo.cost_basis += costo;
            activo.net_result += resultado;
            if tratamiento == "listed_shares" {
                neto_acciones += resultado;
            } else {
                neto_acumulable += resultado;
            }
        }
    }
    mercado.net_result = mercado.proceeds - mercado.cost_basis;
    por_activo.sort_by(|a, b| a.ticker.cmp(&b.ticker));
    mercado.by_asset = por_activo;

    // Plazo fijo: lo pagado en el ejercicio y lo abierto al cierre del periodo.
    let corte = if en_curso { ahora } else { fin };
    let mut plazo = PlazoFijo {
        contracts_paid: 0,
        principal_returned: Decimal::ZERO,
        interest_paid: Decimal::ZERO,
        contracts_open: 0,
        principal_open: Decimal::ZERO,
    };
    let mut retencion = Some(Decimal::ZERO);
    for m in &datos.mutuos {
        match m.pagado_en {
            Some(t) if en_periodo(t) => {
                plazo.contracts_paid += 1;
                plazo.principal_returned += m.principal;
                plazo.interest_paid += m.interes_pagado;
                retencion = match (retencion, retencion_mutuo(m, &datos.parametros)) {
                    (Some(a), Some(b)) => Some(a + b),
                    _ => None,
                };
            }
            _ => {}
        }
        let abierto = m.creado_en < corte && m.pagado_en.map_or(true, |t| t >= corte);
        if abierto {
            plazo.contracts_open += 1;
            plazo.principal_open += m.principal;
        }
    }

    // ISR.
    let parametros = datos.parametros.iter().find(|p| i32::from(p.ejercicio) == anio);
    let estimado = match (parametros, retencion) {
        (Some(p), Some(retencion)) => {
            let impuesto_acciones = if neto_acciones > Decimal::ZERO {
                centavos(neto_acciones * p.tasa_ganancia_acciones / Decimal::ONE_HUNDRED)
            } else {
                Decimal::ZERO
            };
            Some(Estimacion {
                interest_withholding: retencion,
                listed_shares_net: neto_acciones,
                listed_shares_tax: impuesto_acciones,
                total: retencion + impuesto_acciones,
            })
        }
        _ => None,
    };
    let isr = Isr {
        // Ningún movimiento de los ledgers retiene ISR todavía.
        withheld: Decimal::ZERO,
        not_estimated_reason: if estimado.is_none() { Some("no_rates") } else { None },
        estimated: estimado,
        rates: parametros.map(|p| Tasas {
            interest_withholding_annual: p.tasa_retencion_intereses,
            interest_basis: p.fundamento_intereses.clone(),
            listed_shares: p.tasa_ganancia_acciones,
            listed_shares_basis: p.fundamento_acciones.clone(),
        }),
        annual_return_result: neto_acumulable,
    };

    let fin_periodo = if en_curso { ahora } else { fin - Duration::microseconds(1) };
    let mut r = ResumenFiscal {
        year: anio,
        in_progress: en_curso,
        period_start: inicio,
        period_end: fin_periodo,
        market: mercado,
        fixed_term: plazo,
        isr,
        reference: String::new(),
    };
    r.reference = sha256_hex(payload_resumen(datos.cliente_id, &r).as_bytes());
    r
}

/// `SC-FISCAL-v1`: todas las cifras del resumen, en orden.
fn payload_resumen(cliente_id: Uuid, r: &ResumenFiscal) -> String {
    let d = |v: Decimal| v.normalize().to_string();
    let mut p = String::new();
    campo(&mut p, Some(VERSION_RESUMEN));
    campo(&mut p, Some(&cliente_id.to_string()));
    campo(&mut p, Some(&r.year.to_string()));
    campo(&mut p, Some(if r.in_progress { "true" } else { "false" }));
    campo(&mut p, Some(&r.period_end.timestamp_micros().to_string()));
    let m = &r.market;
    for v in [&m.buys.to_string(), &d(m.bought), &m.sells.to_string(), &d(m.proceeds), &d(m.cost_basis)] {
        campo(&mut p, Some(v));
    }
    for v in [d(m.gains), d(m.losses), d(m.net_result), d(m.commissions)] {
        campo(&mut p, Some(&v));
    }
    for a in &m.by_asset {
        for v in [&a.ticker, &a.sells.to_string(), &d(a.proceeds), &d(a.cost_basis), &d(a.net_result)] {
            campo(&mut p, Some(v));
        }
    }
    let f = &r.fixed_term;
    for v in [&f.contracts_paid.to_string(), &d(f.principal_returned), &d(f.interest_paid)] {
        campo(&mut p, Some(v));
    }
    campo(&mut p, Some(&d(r.isr.withheld)));
    match &r.isr.estimated {
        Some(e) => {
            for v in [d(e.interest_withholding), d(e.listed_shares_tax), d(e.total)] {
                campo(&mut p, Some(&v));
            }
        }
        None => campo(&mut p, None),
    }
    campo(&mut p, Some(&d(r.isr.annual_return_result)));
    p
}

/// Los ejercicios con actividad, del más reciente al más antiguo, siempre con el actual.
fn ejercicios(datos: &Datos, actual: i32) -> Vec<i32> {
    let mut anios: Vec<i32> = datos
        .movimientos
        .iter()
        .map(|m| ejercicio_de(m.creado_en))
        .chain(datos.ordenes.iter().map(|o| ejercicio_de(o.creado_en)))
        .chain(datos.mutuos.iter().map(|m| ejercicio_de(m.creado_en)))
        .chain(datos.mutuos.iter().filter_map(|m| m.pagado_en.map(ejercicio_de)))
        .filter(|a| (PRIMER_EJERCICIO..=actual).contains(a))
        .collect();
    anios.push(actual);
    anios.sort_unstable_by(|a, b| b.cmp(a));
    anios.dedup();
    anios
}

fn ejercicio_pedido(anio: Option<i32>, ahora: DateTime<Utc>) -> AppResult<i32> {
    let actual = ejercicio_de(ahora);
    let anio = anio.unwrap_or(actual);
    if !(PRIMER_EJERCICIO..=actual).contains(&anio) {
        return Err(AppError::validacion(format!("year: de {PRIMER_EJERCICIO} al ejercicio actual ({actual})")));
    }
    Ok(anio)
}

/// `GET /api/v1/wealth/impuestos?year=`: el perfil fiscal, el W-8BEN y el
/// resumen del ejercicio (el actual si no se pide otro).
pub async fn consultar(
    pool: &PgPool,
    cliente_id: Uuid,
    anio: Option<i32>,
    ahora: DateTime<Utc>,
) -> AppResult<CentroFiscal> {
    let anio = ejercicio_pedido(anio, ahora)?;
    let datos = leer(pool, cliente_id).await?;
    Ok(CentroFiscal {
        profile: perfil_fiscal(&datos.perfil),
        w8ben: estado_w8ben(&datos, local(ahora).date_naive()),
        years: ejercicios(&datos, ejercicio_de(ahora)),
        summary: resumen(&datos, anio, ahora),
    })
}

// -----------------------------------------------------------------------------
// Certificar (o renovar) el W-8BEN
// -----------------------------------------------------------------------------

/// `W8benCertification`: lo que el cliente certifica y su firma. La Parte I
/// la copia la base de datos de su expediente.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudW8ben {
    pub beneficial_owner: bool,
    pub not_us_person: bool,
    pub treaty_resident: bool,
    /// Su nombre legal completo.
    pub signature: String,
}

/// Guarda una versión nueva del W-8BEN y devuelve su estado (`W8benState`).
pub async fn certificar_w8ben(
    pool: &PgPool,
    cliente_id: Uuid,
    s: &SolicitudW8ben,
    autor: &Autor<'_>,
    ahora: DateTime<Utc>,
) -> AppResult<EstadoW8ben> {
    if s.signature.chars().count() > FIRMA_MAXIMA {
        return Err(AppError::validacion(format!("signature: a lo más {FIRMA_MAXIMA} caracteres")));
    }
    let mut tx = db::begin(pool, Rol::Kyc).await?;
    let perfil: Perfil = sqlx::query_as(CONSULTA_PERFIL)
        .bind(cliente_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NoAutenticado)?;
    if let Some(motivo) = bloqueo(&perfil) {
        return Err(error_de_bloqueo(motivo));
    }
    if !(s.beneficial_owner && s.not_us_person && s.treaty_resident) {
        return Err(AppError::negocio(
            "W8BEN_CERTIFICACIONES",
            "confirma las tres certificaciones: beneficiario efectivo, no eres persona de EUA y resides en México",
        ));
    }
    if firma_normalizada(&s.signature) != perfil.nombre_legal {
        return Err(AppError::negocio("W8BEN_FIRMA", "firma con tu nombre legal completo, como en tu expediente"));
    }
    let version: i32 = sqlx::query_scalar(
        "INSERT INTO kyc.formularios_w8ben \
           (id, cliente_id, beneficiario_efectivo, no_persona_eua, residente_tratado, firma, sesion_id, ip, dispositivo) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::inet, $9) RETURNING version",
    )
    .bind(Uuid::new_v4())
    .bind(cliente_id)
    .bind(s.beneficial_owner)
    .bind(s.not_us_person)
    .bind(s.treaty_resident)
    .bind(&s.signature)
    .bind(autor.sesion_id)
    .bind(autor.ip.map(|i| i.to_string()))
    .bind(autor.dispositivo)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(target: "kyc", %cliente_id, version, "W-8BEN certificado");
    let datos = leer(pool, cliente_id).await?;
    Ok(estado_w8ben(&datos, local(ahora).date_naive()))
}

// -----------------------------------------------------------------------------
// Exportación contable (RFC 4180)
// -----------------------------------------------------------------------------

/// Un decimal para la contabilidad: exacto, con al menos dos decimales.
fn decimal_csv(d: Decimal) -> String {
    let mut n = d.normalize();
    if n.scale() < 2 {
        n.rescale(2);
    }
    n.to_string()
}

/// Un campo RFC 4180: entre comillas si lleva coma, comillas o salto de línea.
fn campo_csv(v: &str) -> String {
    if v.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_owned()
    }
}

/// La hora de la Ciudad de México, con su desfase: "2026-11-23T16:24:05-06:00".
fn fecha_csv(t: DateTime<Utc>) -> String {
    format!("{}-06:00", local(t).format("%Y-%m-%dT%H:%M:%S"))
}

/// El CSV del ejercicio: `(nombre del archivo, contenido)`.
pub async fn exportar_csv(
    pool: &PgPool,
    cliente_id: Uuid,
    anio: Option<i32>,
    ahora: DateTime<Utc>,
) -> AppResult<(String, String)> {
    let anio = ejercicio_pedido(anio, ahora)?;
    let datos = leer(pool, cliente_id).await?;
    Ok((format!("synthetic-capital-taxes-{anio}.csv"), csv(&datos, anio)))
}

fn csv(datos: &Datos, anio: i32) -> String {
    let (inicio, fin) = limites(anio);
    let ordenes: HashMap<Uuid, &Orden> = datos.ordenes.iter().map(|o| (o.id, o)).collect();
    let pagos: HashMap<i64, &Mutuo> = datos.mutuos.iter().filter_map(|m| m.secuencia_pago.map(|s| (s, m))).collect();
    let aportaciones: HashMap<i64, &Mutuo> =
        datos.mutuos.iter().filter_map(|m| m.secuencia_aportacion.map(|s| (s, m))).collect();

    let mut out = String::new();
    out.push_str(&ENCABEZADO_CSV.join(","));
    out.push_str("\r\n");
    let Some(cuenta) = datos.cuenta else {
        return out;
    };
    let mut saldo = Decimal::ZERO;
    for m in &datos.movimientos {
        let Some((tipo, efecto)) = asiento(m, cuenta) else {
            continue;
        };
        saldo += efecto;
        if m.creado_en < inicio || m.creado_en >= fin {
            continue;
        }
        let orden = m
            .ref_movimiento_espejo
            .and_then(|id| ordenes.get(&id))
            .filter(|_| matches!(tipo, TipoAsiento::MarketBuy | TipoAsiento::MarketSell));
        let mutuo = match tipo {
            TipoAsiento::FromFixedTerm => pagos.get(&m.secuencia),
            TipoAsiento::ToFixedTerm => aportaciones.get(&m.secuencia),
            _ => None,
        };
        let referencia = match (orden, mutuo) {
            (Some(o), _) => format!("CO-{:08}", o.secuencia),
            (None, Some(c)) => format!("MUTUO-{}", c.folio),
            _ => m.referencia_externa.clone().unwrap_or_default(),
        };
        let venta = orden.filter(|o| o.lado == "VENTA");
        let fila = [
            fecha_csv(m.creado_en),
            tipo.como_texto().to_owned(),
            referencia,
            orden.map(|o| o.ticker.clone()).unwrap_or_default(),
            orden.map(|o| o.nombre_activo.clone()).unwrap_or_default(),
            orden.map(|o| clase_y_tratamiento(&o.clase).0.to_owned()).unwrap_or_default(),
            orden.map(|o| decimal_csv(o.cantidad)).unwrap_or_default(),
            orden.map(|o| decimal_csv(o.precio)).unwrap_or_default(),
            decimal_csv(efecto),
            orden.map(|o| decimal_csv(o.comision)).unwrap_or_default(),
            venta.map(|o| decimal_csv(o.costo_liberado.unwrap_or_default())).unwrap_or_default(),
            venta.map(|o| decimal_csv(o.monto - o.costo_liberado.unwrap_or_default())).unwrap_or_default(),
            match (tipo, mutuo) {
                (TipoAsiento::FromFixedTerm, Some(c)) => decimal_csv(c.interes_pagado),
                _ => String::new(),
            },
            decimal_csv(saldo),
            m.secuencia.to_string(),
            m.tx_hash.clone(),
        ];
        out.push_str(&fila.iter().map(|v| campo_csv(v)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn fecha(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn el_ejercicio_es_el_anio_de_la_ciudad_de_mexico() {
        let (inicio, fin) = limites(2026);
        assert_eq!(inicio.to_rfc3339(), "2026-01-01T06:00:00+00:00");
        assert_eq!(fin.to_rfc3339(), "2027-01-01T06:00:00+00:00");
        // 03:00 UTC del 1 de enero aún es 31 de diciembre en la Ciudad de México.
        assert_eq!(ejercicio_de("2027-01-01T03:00:00Z".parse().unwrap()), 2026);
        assert_eq!(ejercicio_de("2027-01-01T06:00:00Z".parse().unwrap()), 2027);
    }

    #[test]
    fn los_dias_del_plazo_se_reparten_por_anio() {
        assert_eq!(dias_por_anio(fecha("2026-03-01"), 28), vec![(2026, 28)]);
        // Del 20 de diciembre, 28 días: 12 en 2025 y 16 en 2026.
        assert_eq!(dias_por_anio(fecha("2025-12-20"), 28), vec![(2025, 12), (2026, 16)]);
        assert_eq!(dias_por_anio(fecha("2026-01-01"), 0), vec![]);
    }

    fn parametros(anio: i16, intereses: &str) -> Parametros {
        Parametros {
            ejercicio: anio,
            tasa_retencion_intereses: d(intereses),
            fundamento_intereses: "LIF".into(),
            tasa_ganancia_acciones: d("10"),
            fundamento_acciones: "LISR art. 129".into(),
            tasa_dividendos_eua_tratado: d("10"),
            tasa_dividendos_eua_sin_w8ben: d("30"),
            fundamento_dividendos_eua: "Tratado".into(),
        }
    }

    fn mutuo(principal: &str, inicio: &str, dias: i32) -> Mutuo {
        Mutuo {
            folio: 1,
            principal: d(principal),
            fecha_inicio: fecha(inicio),
            plazo_dias: dias,
            interes_pagado: d("0"),
            creado_en: Utc::now(),
            pagado_en: None,
            secuencia_pago: None,
            secuencia_aportacion: None,
        }
    }

    #[test]
    fn la_retencion_sobre_el_capital_usa_la_tasa_de_cada_anio_del_plazo() {
        let tasas = [parametros(2025, "0.50"), parametros(2026, "0.90")];
        // 100,000 por 28 días de 2026 al 0.90% anual: 100000 × 0.009 × 28 / 365 = 69.04.
        assert_eq!(retencion_mutuo(&mutuo("100000", "2026-03-01", 28), &tasas), Some(d("69.04")));
        // 12 días al 0.50% y 16 al 0.90%: 16.438… + 39.452… = 55.89.
        assert_eq!(retencion_mutuo(&mutuo("100000", "2025-12-20", 28), &tasas), Some(d("55.89")));
        // Sin tasas para un año del plazo, no se estima.
        assert_eq!(retencion_mutuo(&mutuo("100000", "2026-12-20", 28), &tasas), None);
    }

    #[test]
    fn la_firma_se_compara_como_en_la_base_de_datos() {
        assert_eq!(firma_normalizada("  ana  rojas\tnúñez "), "ANA ROJAS NÚÑEZ");
        // Una vocal con acento combinado (NFD) se compone (NFC).
        assert_eq!(firma_normalizada("nu\u{301}n\u{303}ez"), "NÚÑEZ");
    }

    #[test]
    fn el_csv_sigue_rfc_4180() {
        assert_eq!(campo_csv("Synthetic Tesla, 1 share"), "\"Synthetic Tesla, 1 share\"");
        assert_eq!(campo_csv("dice \"hola\""), "\"dice \"\"hola\"\"\"");
        assert_eq!(campo_csv("STSLA"), "STSLA");
        assert_eq!(decimal_csv(d("999.62000000")), "999.62");
        assert_eq!(decimal_csv(d("-10000")), "-10000.00");
        assert_eq!(decimal_csv(d("0.22640000")), "0.2264");
        assert_eq!(decimal_csv(d("0.00000001")), "0.00000001");
        assert_eq!(fecha_csv("2026-11-23T22:24:05Z".parse().unwrap()), "2026-11-23T16:24:05-06:00");
        assert_eq!(ENCABEZADO_CSV.len(), 16);
    }

    fn perfil() -> Perfil {
        Perfil {
            tipo_persona: "FISICA".into(),
            nombre_legal: "ANA ROJAS NÚÑEZ".into(),
            rfc: Some("RONA850315AB1".into()),
            curp: None,
            nacionalidad: "MX".into(),
            pais_residencia: "MX".into(),
            fecha_nacimiento: Some(fecha("1985-03-15")),
            domicilio: "Av. Reforma 222, Juárez, Cuauhtémoc, Ciudad de México, C.P. 06600, México".into(),
            domicilio_completo: true,
            estatus_kyc: "VERIFICADO".into(),
        }
    }

    fn formulario(p: &Perfil, vigente_hasta: &str) -> FilaW8ben {
        FilaW8ben {
            cliente_id: Uuid::nil(),
            version: 1,
            nombre_legal: p.nombre_legal.clone(),
            pais_ciudadania: p.nacionalidad.clone(),
            pais_residencia: p.pais_residencia.clone(),
            domicilio: p.domicilio.clone(),
            tin_extranjero: p.rfc.clone().unwrap(),
            fecha_nacimiento: p.fecha_nacimiento.unwrap(),
            pais_tratado: "MX".into(),
            beneficiario_efectivo: true,
            no_persona_eua: true,
            residente_tratado: true,
            firma: p.nombre_legal.clone(),
            firmado_en: Utc::now(),
            vigente_hasta: fecha(vigente_hasta),
            sesion_id: None,
            ip: None,
            dispositivo: None,
            hash_anterior: GENESIS.to_owned(),
            hash: String::new(),
        }
    }

    fn datos(perfil: Perfil, formularios: Vec<FilaW8ben>) -> Datos {
        Datos {
            cliente_id: Uuid::nil(),
            perfil,
            cuenta: None,
            movimientos: Vec::new(),
            ordenes: Vec::new(),
            mutuos: Vec::new(),
            formularios,
            parametros: vec![parametros(2026, "0.90")],
        }
    }

    #[test]
    fn el_estado_del_w8ben() {
        let hoy = fecha("2026-11-23");
        let p = perfil();
        assert_eq!(estado_w8ben(&datos(p.clone(), vec![]), hoy).status, "pending");
        let vigente = datos(p.clone(), vec![formulario(&p, "2029-12-31")]);
        let e = estado_w8ben(&vigente, hoy);
        assert_eq!((e.status, e.us_dividend_rate.as_deref()), ("active", Some("10.00")));
        // Vigente hasta ese día, incluido.
        assert_eq!(estado_w8ben(&vigente, fecha("2029-12-31")).status, "active");
        assert_eq!(estado_w8ben(&vigente, fecha("2030-01-01")).status, "expired");
        let e = estado_w8ben(&datos(p.clone(), vec![formulario(&p, "2025-12-31")]), hoy);
        assert_eq!((e.status, e.us_dividend_rate.as_deref()), ("expired", Some("30.00")));
        // Otro domicilio en el expediente: un cambio de circunstancias.
        let mut mudanza = p.clone();
        mudanza.domicilio = "Calle Nueva 1, Centro, Puebla, Puebla, C.P. 72000, México".into();
        let e = estado_w8ben(&datos(mudanza, vec![formulario(&p, "2029-12-31")]), hoy);
        assert_eq!((e.status, e.us_dividend_rate.as_deref()), ("outdated", Some("30.00")));
    }

    #[test]
    fn quien_no_puede_certificar_y_por_que() {
        let mut p = perfil();
        assert_eq!(bloqueo(&p), None);
        p.pais_residencia = "ES".into();
        assert_eq!(bloqueo(&p), Some("non_resident"));
        p.nacionalidad = "US".into();
        assert_eq!(bloqueo(&p), Some("us_person"));
        p.tipo_persona = "MORAL".into();
        assert_eq!(bloqueo(&p), Some("business"));
        p.estatus_kyc = "EN_REVISION".into();
        assert_eq!(bloqueo(&p), Some("identity_unverified"));
        let mut sin_rfc = perfil();
        sin_rfc.rfc = None;
        assert_eq!(bloqueo(&sin_rfc), Some("missing_data"));
    }

    #[test]
    fn el_tratamiento_depende_de_la_clase_del_activo() {
        assert_eq!(clase_y_tratamiento("RENTA_VARIABLE"), ("equity", "listed_shares"));
        assert_eq!(clase_y_tratamiento("FONDO"), ("fund", "listed_shares"));
        assert_eq!(clase_y_tratamiento("MATERIA_PRIMA"), ("commodity", "annual_return"));
        assert_eq!(clase_y_tratamiento("RENTA_FIJA"), ("fixed_income", "annual_return"));
    }
}
