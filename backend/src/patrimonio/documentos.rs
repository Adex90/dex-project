//! Buzón de documentos del cliente: comprobantes de operación y estados de
//! cuenta. Los de la API usan los nombres en inglés del frontend
//! (`frontend/src/types/documents.ts`).
//!
//! Ningún documento se guarda aparte: cada uno se deriva, al leerlo, de los
//! ledgers encadenados por hash, que son inmutables. Así un comprobante y el
//! estado de cuenta de un mes cerrado salen idénticos cada vez, cifra por
//! cifra, y su referencia lo demuestra:
//!
//! - **Comprobante de operación** (`co_<orden>`, folio `CO-<secuencia>`): uno
//!   por orden ejecutada en `mercado.ordenes` (de mercado, o límite o stop al
//!   dispararse). Cantidad, precio, importe, comisión (cero, por restricción
//!   de la base de datos), el movimiento IFPE que la liquidó y los hashes de
//!   los dos ledgers. Su referencia es el `tx_hash` de la orden.
//! - **Estado de cuenta** (`ec_<AAAA-MM>`, folio `EC-<AAAA-MM>-<CLABE final>`):
//!   uno por mes con actividad (meses de la Ciudad de México, UTC−6 como
//!   `spei::hoy_cdmx`). Saldo inicial, abonos, cargos y saldo final de la
//!   bóveda; cada movimiento que cambia el saldo, con el saldo después; el
//!   desglose final (disponible, retenido, comprometido en órdenes); las
//!   operaciones del mes y las posiciones al cierre. El del mes en curso es
//!   provisional. Su referencia es el SHA-256 de su contenido (SC-EDC-v1).
//!
//! **Sello de reserva:** el primer corte de Proof of Reserve publicado cuyas
//! anclas cubren el documento (el ledger IFPE y el del mercado llegan al menos
//! hasta sus movimientos): su raíz ya los incluye. Sin corte todavía, el sello
//! queda pendiente.
//!
//! Sin aviso al cliente: las retenciones son movimientos internos (no cambian
//! el saldo) y no se listan; el desglose final dice cuánto está retenido, con
//! la redacción neutral de toda pantalla del cliente.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    ledger::hash::sha256_hex,
};

/// Comprobantes que devuelve la lista (los más recientes).
pub const COMPROBANTES_EN_LISTA: usize = 200;
/// Desfase de la Ciudad de México (sin horario de verano desde 2022), como `spei::hoy_cdmx`.
const DESFASE_CDMX_HORAS: i64 = 6;
const VERSION_ESTADO: &str = "SC-EDC-v1";

// -----------------------------------------------------------------------------
// Lo que se lee de los ledgers
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct Movimiento {
    secuencia: i64,
    tipo: String,
    monto: Decimal,
    cuenta_destino_id: Option<Uuid>,
    referencia_externa: Option<String>,
    ref_movimiento_espejo: Option<Uuid>,
    tx_hash: String,
    creado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Orden {
    id: Uuid,
    secuencia: i64,
    ticker: String,
    nombre_activo: String,
    lado: String,
    monto_solicitado: Decimal,
    cantidad: Decimal,
    precio: Decimal,
    monto: Decimal,
    comision: Decimal,
    costo_liberado: Option<Decimal>,
    fuente_precio: String,
    tx_hash: String,
    hash_anterior: String,
    creado_en: DateTime<Utc>,
    tipo_pendiente: Option<String>,
    precio_limite: Option<Decimal>,
    precio_stop: Option<Decimal>,
    secuencia_ifpe: Option<i64>,
    hash_ifpe: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Titular {
    nombre: String,
    apellido_paterno: Option<String>,
    apellido_materno: Option<String>,
    rfc: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Cuenta {
    id: Uuid,
    clabe: String,
}

/// Todo lo del cliente, en una sola foto (REPEATABLE READ, su rol y RLS).
struct Datos {
    titular: Titular,
    cuenta: Option<Cuenta>,
    movimientos: Vec<Movimiento>,
    ordenes: Vec<Orden>,
}

async fn leer(pool: &PgPool, cliente_id: Uuid) -> AppResult<Datos> {
    let mut tx = db::begin_lectura_instantanea_cliente(pool, cliente_id).await?;
    let titular: Titular =
        sqlx::query_as("SELECT nombre, apellido_paterno, apellido_materno, rfc FROM kyc.clientes WHERE id = $1")
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::NoAutenticado)?;
    let cuenta: Option<Cuenta> =
        sqlx::query_as("SELECT id, clabe FROM ifpe.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'")
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?;
    let movimientos: Vec<Movimiento> = match &cuenta {
        Some(c) => {
            sqlx::query_as(
                "SELECT secuencia, tipo, monto, cuenta_destino_id, referencia_externa, \
                        ref_movimiento_espejo, tx_hash, creado_en \
                 FROM ifpe.transacciones WHERE cuenta_origen_id = $1 OR cuenta_destino_id = $1 \
                 ORDER BY secuencia",
            )
            .bind(c.id)
            .fetch_all(&mut *tx)
            .await?
        }
        None => Vec::new(),
    };
    let ordenes: Vec<Orden> = sqlx::query_as(
        "SELECT o.id, o.secuencia, o.ticker, a.nombre AS nombre_activo, o.lado, o.monto_solicitado, o.cantidad, \
                o.precio, o.monto, o.comision, o.costo_liberado, o.fuente_precio, o.tx_hash, o.hash_anterior, \
                o.creado_en, p.tipo AS tipo_pendiente, p.precio_limite, p.precio_stop, \
                t.secuencia AS secuencia_ifpe, t.tx_hash AS hash_ifpe \
         FROM mercado.ordenes o \
         JOIN mercado.activos a ON a.ticker = o.ticker \
         LEFT JOIN mercado.ordenes_pendientes p ON p.orden_ejecutada_id = o.id \
         LEFT JOIN ifpe.transacciones t ON t.id = o.tx_ifpe_id \
         WHERE o.cliente_id = $1 ORDER BY o.secuencia",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Datos { titular, cuenta, movimientos, ordenes })
}

// -----------------------------------------------------------------------------
// Periodos (meses de la Ciudad de México)
// -----------------------------------------------------------------------------

/// "2026-11": el mes de la Ciudad de México de un instante.
fn periodo_de(t: DateTime<Utc>) -> String {
    let local = (t - Duration::hours(DESFASE_CDMX_HORAS)).date_naive();
    format!("{:04}-{:02}", local.year(), local.month())
}

/// Inicio (incluido) y fin (excluido) de un periodo, en UTC.
fn limites(periodo: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let (a, m) = periodo.split_once('-')?;
    let (anio, mes): (i32, u32) = (a.parse().ok()?, m.parse().ok()?);
    if a.len() != 4 || m.len() != 2 {
        return None;
    }
    let inicio = NaiveDate::from_ymd_opt(anio, mes, 1)?;
    let siguiente =
        if mes == 12 { NaiveDate::from_ymd_opt(anio + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(anio, mes + 1, 1)? };
    let a_utc = |d: NaiveDate| {
        Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("medianoche")) + Duration::hours(DESFASE_CDMX_HORAS)
    };
    Some((a_utc(inicio), a_utc(siguiente)))
}

const MESES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn nombre_periodo(periodo: &str) -> String {
    let mes: usize = periodo[5..7].parse().unwrap_or(1);
    format!("{} {}", MESES[mes.clamp(1, 12) - 1], &periodo[..4])
}

// -----------------------------------------------------------------------------
// Movimientos en palabras
// -----------------------------------------------------------------------------

/// `StatementEntryKind`: qué fue un movimiento que cambia el saldo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TipoAsiento {
    Deposit,
    Withdrawal,
    TransferIn,
    TransferOut,
    ToFixedTerm,
    FromFixedTerm,
    MarketBuy,
    MarketSell,
}

/// Efecto de un movimiento en el saldo total de la cuenta (disponible +
/// retenido + comprometido), y qué fue. Las reclasificaciones internas
/// (retenciones y compromisos de órdenes) no cambian el total: `None`.
fn asiento(m: &Movimiento, cuenta: Uuid) -> Option<(TipoAsiento, Decimal)> {
    match m.tipo.as_str() {
        "DEPOSITO" => Some((TipoAsiento::Deposit, m.monto)),
        "RETIRO" => Some((TipoAsiento::Withdrawal, -m.monto)),
        "TRANSFERENCIA" if m.cuenta_destino_id == Some(cuenta) => Some((TipoAsiento::TransferIn, m.monto)),
        "TRANSFERENCIA" => Some((TipoAsiento::TransferOut, -m.monto)),
        "TRASPASO_A_SOFOM" => Some((TipoAsiento::ToFixedTerm, -m.monto)),
        "RETORNO_DE_SOFOM" => Some((TipoAsiento::FromFixedTerm, m.monto)),
        "COMPRA_MERCADO" => Some((TipoAsiento::MarketBuy, -m.monto)),
        "VENTA_MERCADO" => Some((TipoAsiento::MarketSell, m.monto)),
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Sello de reserva
// -----------------------------------------------------------------------------

/// `ReserveSeal`.
#[derive(Debug, Clone, Serialize)]
pub struct SelloReserva {
    /// "sealed": un corte publicado ya cubre el documento; "pending": el siguiente lo hará.
    pub status: &'static str,
    pub snapshot_id: Option<Uuid>,
    /// Raíz Merkle del efectivo del corte.
    pub root: Option<String>,
    pub taken_at: Option<DateTime<Utc>>,
    /// Reserva entre pasivo, en por ciento (la base de datos exige 100).
    pub reserve_ratio: Option<String>,
    /// Anclas del corte: los ledgers IFPE y del mercado hasta donde llega.
    pub ledger_anchor: Option<i64>,
    pub market_anchor: Option<i64>,
    /// Lo que el documento necesita que cubra.
    pub covers_ledger_sequence: i64,
    pub covers_market_sequence: i64,
}

/// Un corte publicado, lo que el sello necesita de él.
#[derive(sqlx::FromRow)]
struct FilaCorte {
    id: Uuid,
    merkle_root: String,
    creado_en: DateTime<Utc>,
    total_pasivo: Decimal,
    total_reserva: Decimal,
    secuencia_ledger: i64,
    secuencia_mercado: Option<i64>,
}

async fn sello(pool: &PgPool, secuencia_ifpe: i64, secuencia_mercado: i64) -> AppResult<SelloReserva> {
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let corte: Option<FilaCorte> = sqlx::query_as(
        "SELECT id, merkle_root, creado_en, total_pasivo, total_reserva, secuencia_ledger, secuencia_mercado \
         FROM ifpe.cortes_reserva \
         WHERE secuencia_ledger >= $1 AND coalesce(secuencia_mercado, 0) >= $2 \
         ORDER BY creado_en, id LIMIT 1",
    )
    .bind(secuencia_ifpe)
    .bind(secuencia_mercado)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(match corte {
        Some(c) => {
            let razon = if c.total_pasivo.is_zero() {
                Decimal::ONE_HUNDRED
            } else {
                (c.total_reserva * Decimal::ONE_HUNDRED / c.total_pasivo).trunc_with_scale(2)
            };
            SelloReserva {
                status: "sealed",
                snapshot_id: Some(c.id),
                root: Some(c.merkle_root),
                taken_at: Some(c.creado_en),
                reserve_ratio: Some(format!("{razon:.2}")),
                ledger_anchor: Some(c.secuencia_ledger),
                market_anchor: c.secuencia_mercado,
                covers_ledger_sequence: secuencia_ifpe,
                covers_market_sequence: secuencia_mercado,
            }
        }
        None => SelloReserva {
            status: "pending",
            snapshot_id: None,
            root: None,
            taken_at: None,
            reserve_ratio: None,
            ledger_anchor: None,
            market_anchor: None,
            covers_ledger_sequence: secuencia_ifpe,
            covers_market_sequence: secuencia_mercado,
        },
    })
}

// -----------------------------------------------------------------------------
// La lista
// -----------------------------------------------------------------------------

/// `DocumentSummary`: una fila del buzón.
#[derive(Debug, Clone, Serialize)]
pub struct ResumenDocumento {
    pub id: String,
    /// "trade_confirmation" o "account_statement".
    pub kind: &'static str,
    pub folio: String,
    pub title: String,
    /// Ejecución de la orden, o fin del periodo (ahora, si está en curso).
    pub date: DateTime<Utc>,
    /// Hash de referencia: el `tx_hash` de la orden, o el del estado de cuenta.
    pub reference: String,
    /// Importe de la orden, o saldo final del periodo.
    pub amount: Decimal,
    /// Estado de cuenta del mes en curso: sus cifras cambian hasta que cierre.
    pub provisional: bool,
}

/// `Documents`.
#[derive(Debug, Clone, Serialize)]
pub struct Documentos {
    /// El más reciente primero.
    pub documents: Vec<ResumenDocumento>,
    /// Hay más comprobantes que los listados (los más antiguos quedan fuera).
    pub truncated: bool,
}

fn titulo_orden(o: &Orden) -> String {
    let lado = if o.lado == "COMPRA" { "Buy" } else { "Sell" };
    format!("{lado} {} {}", o.cantidad.normalize(), o.ticker)
}

/// Los documentos del cliente: los estados de cuenta de cada mes con actividad
/// y los comprobantes más recientes.
pub async fn listar(pool: &PgPool, cliente_id: Uuid) -> AppResult<Documentos> {
    let datos = leer(pool, cliente_id).await?;
    let ahora = Utc::now();
    let mut documentos: Vec<ResumenDocumento> = estados(&datos, cliente_id, ahora)
        .into_iter()
        .map(|e| ResumenDocumento {
            id: format!("ec_{}", e.period),
            kind: "account_statement",
            title: format!("Account statement · {}", nombre_periodo(&e.period)),
            folio: e.folio.clone(),
            date: e.period_end,
            reference: e.reference.clone(),
            amount: e.closing_balance,
            provisional: e.provisional,
        })
        .collect();
    let truncated = datos.ordenes.len() > COMPROBANTES_EN_LISTA;
    documentos.extend(datos.ordenes.iter().rev().take(COMPROBANTES_EN_LISTA).map(|o| ResumenDocumento {
        id: format!("co_{}", o.id),
        kind: "trade_confirmation",
        folio: folio_orden(o),
        title: titulo_orden(o),
        date: o.creado_en,
        reference: o.tx_hash.clone(),
        amount: o.monto,
        provisional: false,
    }));
    documentos.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.kind.cmp(b.kind)));
    Ok(Documentos { documents: documentos, truncated })
}

fn folio_orden(o: &Orden) -> String {
    format!("CO-{:08}", o.secuencia)
}

// -----------------------------------------------------------------------------
// Comprobante de operación
// -----------------------------------------------------------------------------

/// `Holder`.
#[derive(Debug, Clone, Serialize)]
pub struct Titularidad {
    pub name: String,
    pub rfc: Option<String>,
}

fn titularidad(t: &Titular) -> Titularidad {
    let name = [Some(t.nombre.as_str()), t.apellido_paterno.as_deref(), t.apellido_materno.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    Titularidad { name, rfc: t.rfc.clone() }
}

/// `TradeConfirmation`.
#[derive(Debug, Clone, Serialize)]
pub struct Comprobante {
    pub id: String,
    pub kind: &'static str,
    pub folio: String,
    pub holder: Titularidad,
    pub executed_at: DateTime<Utc>,
    /// "buy" o "sell".
    pub side: &'static str,
    pub ticker: String,
    pub asset_name: String,
    /// "market", "limit" o "stop".
    pub order_type: &'static str,
    pub limit_price: Option<Decimal>,
    pub stop_price: Option<Decimal>,
    pub requested_amount: Decimal,
    pub quantity: Decimal,
    pub price: Decimal,
    /// Cantidad × precio, al centavo.
    pub gross_amount: Decimal,
    pub commission: Decimal,
    /// Lo que salió de (compra) o entró a (venta) la bóveda.
    pub net_amount: Decimal,
    /// Venta: costo promedio de lo vendido y la ganancia realizada.
    pub cost_basis: Option<Decimal>,
    pub realized_pnl: Option<Decimal>,
    /// "simulated" (demostración) u "oracle".
    pub price_source: &'static str,
    /// El ledger del mercado: secuencia, hash de la orden y el de la anterior.
    pub market_sequence: i64,
    pub reference: String,
    pub previous_hash: String,
    /// El movimiento IFPE que la liquidó.
    pub settlement_sequence: Option<i64>,
    pub settlement_hash: Option<String>,
    pub account_clabe_last4: Option<String>,
    pub reserve_seal: SelloReserva,
}

async fn comprobante(pool: &PgPool, datos: &Datos, orden_id: Uuid) -> AppResult<Comprobante> {
    let o = datos.ordenes.iter().find(|o| o.id == orden_id).ok_or_else(no_encontrado)?;
    let venta = o.lado == "VENTA";
    let sello = sello(pool, o.secuencia_ifpe.unwrap_or(0), o.secuencia).await?;
    Ok(Comprobante {
        id: format!("co_{}", o.id),
        kind: "trade_confirmation",
        folio: folio_orden(o),
        holder: titularidad(&datos.titular),
        executed_at: o.creado_en,
        side: if venta { "sell" } else { "buy" },
        ticker: o.ticker.clone(),
        asset_name: o.nombre_activo.clone(),
        order_type: match o.tipo_pendiente.as_deref() {
            Some("LIMITE") => "limit",
            Some("STOP") => "stop",
            _ => "market",
        },
        limit_price: o.precio_limite,
        stop_price: o.precio_stop,
        requested_amount: o.monto_solicitado,
        quantity: o.cantidad,
        price: o.precio,
        gross_amount: o.monto,
        commission: o.comision,
        net_amount: if venta { o.monto - o.comision } else { o.monto + o.comision },
        cost_basis: o.costo_liberado,
        realized_pnl: o.costo_liberado.map(|c| o.monto - o.comision - c),
        price_source: if o.fuente_precio == "ORACULO" { "oracle" } else { "simulated" },
        market_sequence: o.secuencia,
        reference: o.tx_hash.clone(),
        previous_hash: o.hash_anterior.clone(),
        settlement_sequence: o.secuencia_ifpe,
        settlement_hash: o.hash_ifpe.clone(),
        account_clabe_last4: datos.cuenta.as_ref().map(|c| c.clabe[c.clabe.len() - 4..].to_owned()),
        reserve_seal: sello,
    })
}

// -----------------------------------------------------------------------------
// Estado de cuenta
// -----------------------------------------------------------------------------

/// `StatementEntry`: un movimiento que cambió el saldo.
#[derive(Debug, Clone, Serialize)]
pub struct Asiento {
    pub date: DateTime<Utc>,
    pub kind: TipoAsiento,
    /// El activo de una compra o venta.
    pub ticker: Option<String>,
    /// Clave de rastreo SPEI, si la hay.
    pub tracking_key: Option<String>,
    /// Secuencia y hash del movimiento en el ledger IFPE.
    pub sequence: i64,
    pub hash: String,
    /// Con signo: positivo abona, negativo carga.
    pub amount: Decimal,
    /// Saldo total después del movimiento.
    pub balance: Decimal,
}

/// `ClosingBreakdown`: el saldo final, por disponibilidad.
#[derive(Debug, Clone, Serialize)]
pub struct Desglose {
    pub available: Decimal,
    pub on_hold: Decimal,
    pub held_for_orders: Decimal,
}

/// `TradeSummary`.
#[derive(Debug, Clone, Serialize)]
pub struct ResumenOperaciones {
    pub buys: usize,
    pub bought: Decimal,
    pub sells: usize,
    pub sold: Decimal,
    pub commission: Decimal,
}

/// `PositionLine`.
#[derive(Debug, Clone, Serialize)]
pub struct Posicion {
    pub ticker: String,
    pub asset_name: String,
    pub quantity: Decimal,
}

/// `AccountStatement`.
#[derive(Debug, Clone, Serialize)]
pub struct EstadoCuenta {
    pub id: String,
    pub kind: &'static str,
    pub folio: String,
    /// "2026-11".
    pub period: String,
    pub period_start: DateTime<Utc>,
    /// Fin del mes, o ahora si está en curso.
    pub period_end: DateTime<Utc>,
    pub provisional: bool,
    pub holder: Titularidad,
    pub account_clabe: String,
    pub opening_balance: Decimal,
    pub credits: Decimal,
    pub debits: Decimal,
    pub closing_balance: Decimal,
    pub closing_breakdown: Desglose,
    pub entries: Vec<Asiento>,
    pub trades: ResumenOperaciones,
    pub positions: Vec<Posicion>,
    /// SHA-256 del contenido (SC-EDC-v1).
    pub reference: String,
    /// Último movimiento IFPE y última orden incluidos (lo que el sello debe cubrir).
    #[serde(skip)]
    secuencia_ifpe: i64,
    #[serde(skip)]
    secuencia_mercado: i64,
    pub reserve_seal: Option<SelloReserva>,
}

fn sumar_hasta<'a>(movs: impl Iterator<Item = &'a Movimiento>, tipos: (&str, &str)) -> Decimal {
    movs.fold(Decimal::ZERO, |acc, m| {
        if m.tipo == tipos.0 {
            acc + m.monto
        } else if m.tipo == tipos.1 {
            acc - m.monto
        } else {
            acc
        }
    })
}

/// Los estados de cuenta de cada mes con actividad, el más antiguo primero
/// (sin sello: se busca al pedir uno).
fn estados(datos: &Datos, cliente_id: Uuid, ahora: DateTime<Utc>) -> Vec<EstadoCuenta> {
    let Some(cuenta) = &datos.cuenta else { return Vec::new() };
    let mut periodos: Vec<String> = datos
        .movimientos
        .iter()
        .map(|m| periodo_de(m.creado_en))
        .chain(datos.ordenes.iter().map(|o| periodo_de(o.creado_en)))
        .collect();
    periodos.sort();
    periodos.dedup();
    let en_curso = periodo_de(ahora);
    let tickers_de: BTreeMap<Uuid, &Orden> = datos.ordenes.iter().map(|o| (o.id, o)).collect();
    let final_clabe = &cuenta.clabe[cuenta.clabe.len() - 4..];

    periodos
        .into_iter()
        .filter_map(|periodo| {
            let (inicio, fin) = limites(&periodo)?;
            let provisional = periodo == en_curso;
            let antes = datos.movimientos.iter().filter(|m| m.creado_en < inicio);
            let apertura: Decimal = antes.filter_map(|m| asiento(m, cuenta.id)).map(|(_, e)| e).sum();
            let del_periodo: Vec<&Movimiento> =
                datos.movimientos.iter().filter(|m| m.creado_en >= inicio && m.creado_en < fin).collect();
            let mut saldo = apertura;
            let (mut abonos, mut cargos) = (Decimal::ZERO, Decimal::ZERO);
            let mut asientos = Vec::new();
            for m in &del_periodo {
                let Some((tipo, efecto)) = asiento(m, cuenta.id) else { continue };
                saldo += efecto;
                if efecto > Decimal::ZERO {
                    abonos += efecto;
                } else {
                    cargos -= efecto;
                }
                let ticker = match tipo {
                    TipoAsiento::MarketBuy | TipoAsiento::MarketSell => {
                        m.ref_movimiento_espejo.and_then(|id| tickers_de.get(&id)).map(|o| o.ticker.clone())
                    }
                    _ => None,
                };
                let tracking_key = match tipo {
                    TipoAsiento::Deposit | TipoAsiento::Withdrawal => m.referencia_externa.clone(),
                    _ => None,
                };
                asientos.push(Asiento {
                    date: m.creado_en,
                    kind: tipo,
                    ticker,
                    tracking_key,
                    sequence: m.secuencia,
                    hash: m.tx_hash.clone(),
                    amount: efecto,
                    balance: saldo,
                });
            }
            let hasta_fin = || datos.movimientos.iter().filter(|m| m.creado_en < fin);
            let retenido = sumar_hasta(hasta_fin(), ("RETENCION", "LIBERACION_RETENCION"));
            let comprometido = sumar_hasta(hasta_fin(), ("COMPROMISO_ORDEN", "LIBERACION_ORDEN"));
            let cierre = saldo;

            let ordenes_periodo: Vec<&Orden> =
                datos.ordenes.iter().filter(|o| o.creado_en >= inicio && o.creado_en < fin).collect();
            let compras: Vec<&&Orden> = ordenes_periodo.iter().filter(|o| o.lado == "COMPRA").collect();
            let ventas: Vec<&&Orden> = ordenes_periodo.iter().filter(|o| o.lado == "VENTA").collect();
            let trades = ResumenOperaciones {
                buys: compras.len(),
                bought: compras.iter().map(|o| o.monto).sum(),
                sells: ventas.len(),
                sold: ventas.iter().map(|o| o.monto).sum(),
                commission: ordenes_periodo.iter().map(|o| o.comision).sum(),
            };
            let mut tenencias: BTreeMap<&str, (Decimal, &str)> = BTreeMap::new();
            for o in datos.ordenes.iter().filter(|o| o.creado_en < fin) {
                let entrada = tenencias.entry(o.ticker.as_str()).or_insert((Decimal::ZERO, o.nombre_activo.as_str()));
                entrada.0 += if o.lado == "COMPRA" { o.cantidad } else { -o.cantidad };
            }
            let positions = tenencias
                .into_iter()
                .filter(|(_, (q, _))| *q > Decimal::ZERO)
                .map(|(t, (q, n))| Posicion { ticker: t.to_owned(), asset_name: n.to_owned(), quantity: q })
                .collect();

            let secuencia_ifpe = hasta_fin().map(|m| m.secuencia).max().unwrap_or(0);
            let secuencia_mercado =
                datos.ordenes.iter().filter(|o| o.creado_en < fin).map(|o| o.secuencia).max().unwrap_or(0);
            let desglose = Desglose {
                available: cierre - retenido - comprometido,
                on_hold: retenido,
                held_for_orders: comprometido,
            };

            // SC-EDC-v1: lo que dice el estado de cuenta, y los movimientos y
            // órdenes que lo respaldan (por su hash en cada ledger).
            let mut p = vec![
                VERSION_ESTADO.to_owned(),
                cliente_id.to_string(),
                cuenta.id.to_string(),
                periodo.clone(),
                apertura.normalize().to_string(),
                cierre.normalize().to_string(),
                desglose.available.normalize().to_string(),
                desglose.on_hold.normalize().to_string(),
                desglose.held_for_orders.normalize().to_string(),
            ];
            p.extend(del_periodo.iter().map(|m| format!("{}:{}", m.secuencia, m.tx_hash)));
            p.extend(ordenes_periodo.iter().map(|o| format!("{}:{}", o.secuencia, o.tx_hash)));
            let payload = p.iter().map(|c| format!("{}:{c}", c.len())).collect::<Vec<_>>().join("|");

            Some(EstadoCuenta {
                id: format!("ec_{periodo}"),
                kind: "account_statement",
                folio: format!("EC-{periodo}-{final_clabe}"),
                period_start: inicio,
                period_end: if provisional { ahora.min(fin) } else { fin },
                provisional,
                holder: titularidad(&datos.titular),
                account_clabe: cuenta.clabe.clone(),
                opening_balance: apertura,
                credits: abonos,
                debits: cargos,
                closing_balance: cierre,
                closing_breakdown: desglose,
                entries: asientos,
                trades,
                positions,
                reference: sha256_hex(payload.as_bytes()),
                secuencia_ifpe,
                secuencia_mercado,
                reserve_seal: None,
                period: periodo,
            })
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Un documento
// -----------------------------------------------------------------------------

/// `Document`: un comprobante o un estado de cuenta, completos.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Documento {
    Comprobante(Box<Comprobante>),
    EstadoCuenta(Box<EstadoCuenta>),
}

/// `co_<orden>` o `ec_<AAAA-MM>`; cualquier otro, o uno ajeno: 404.
pub async fn obtener(pool: &PgPool, cliente_id: Uuid, id: &str) -> AppResult<Documento> {
    if let Some(orden) = id.strip_prefix("co_") {
        let orden_id = Uuid::parse_str(orden).map_err(|_| no_encontrado())?;
        let datos = leer(pool, cliente_id).await?;
        return Ok(Documento::Comprobante(Box::new(comprobante(pool, &datos, orden_id).await?)));
    }
    if let Some(periodo) = id.strip_prefix("ec_") {
        limites(periodo).ok_or_else(no_encontrado)?;
        let datos = leer(pool, cliente_id).await?;
        let mut estado = estados(&datos, cliente_id, Utc::now())
            .into_iter()
            .find(|e| e.period == periodo)
            .ok_or_else(no_encontrado)?;
        estado.reserve_seal = Some(sello(pool, estado.secuencia_ifpe, estado.secuencia_mercado).await?);
        return Ok(Documento::EstadoCuenta(Box::new(estado)));
    }
    Err(no_encontrado())
}

fn no_encontrado() -> AppError {
    AppError::no_encontrado("el documento no existe")
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn en(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn los_meses_son_de_la_ciudad_de_mexico() {
        // 1 de diciembre a las 02:00 UTC: todavía 30 de noviembre en la CDMX.
        assert_eq!(periodo_de(en("2026-12-01T02:00:00Z")), "2026-11");
        assert_eq!(periodo_de(en("2026-12-01T06:00:00Z")), "2026-12");
        let (inicio, fin) = limites("2026-11").unwrap();
        assert_eq!((inicio, fin), (en("2026-11-01T06:00:00Z"), en("2026-12-01T06:00:00Z")));
        let (_, fin) = limites("2026-12").unwrap();
        assert_eq!(fin, en("2027-01-01T06:00:00Z"));
        for malo in ["2026-13", "2026-1", "26-11", "2026/11", "abcd-ef", ""] {
            assert!(limites(malo).is_none(), "{malo}");
        }
        assert_eq!(nombre_periodo("2026-11"), "November 2026");
    }

    fn mov(tipo: &str, monto: &str, _origen: Option<Uuid>, destino: Option<Uuid>) -> Movimiento {
        Movimiento {
            secuencia: 1,
            tipo: tipo.into(),
            monto: monto.parse().unwrap(),
            cuenta_destino_id: destino,
            referencia_externa: None,
            ref_movimiento_espejo: None,
            tx_hash: "a".repeat(64),
            creado_en: en("2026-11-10T12:00:00Z"),
        }
    }

    fn en_fecha(m: Movimiento, secuencia: i64, cuando: &str) -> Movimiento {
        Movimiento { secuencia, creado_en: en(cuando), tx_hash: format!("{secuencia:064}"), ..m }
    }

    #[test]
    fn estados_de_cuenta_de_dos_meses() {
        let cuenta = Uuid::from_u128(1);
        let cliente = Uuid::from_u128(9);
        let orden = Orden {
            id: Uuid::from_u128(7),
            secuencia: 3,
            ticker: "AUG1".into(),
            nombre_activo: "Oro".into(),
            lado: "COMPRA".into(),
            monto_solicitado: "1000".parse().unwrap(),
            cantidad: "0.62".parse().unwrap(),
            precio: "1612.40".parse().unwrap(),
            monto: "999.69".parse().unwrap(),
            comision: Decimal::ZERO,
            costo_liberado: None,
            fuente_precio: "SIMULADO".into(),
            tx_hash: "b".repeat(64),
            hash_anterior: "c".repeat(64),
            creado_en: en("2026-11-05T18:00:00Z"),
            tipo_pendiente: None,
            precio_limite: None,
            precio_stop: None,
            secuencia_ifpe: Some(4),
            hash_ifpe: None,
        };
        let mut compra = mov("COMPRA_MERCADO", "999.69", Some(cuenta), None);
        compra.ref_movimiento_espejo = Some(orden.id);
        let datos = Datos {
            titular: Titular {
                nombre: "Ana".into(),
                apellido_paterno: Some("Rojas".into()),
                apellido_materno: None,
                rfc: None,
            },
            cuenta: Some(Cuenta { id: cuenta, clabe: "646180157000000017".into() }),
            movimientos: vec![
                // 31 de octubre por la noche en la CDMX (1 de noviembre en UTC): octubre.
                en_fecha(mov("DEPOSITO", "5000", None, Some(cuenta)), 1, "2026-11-01T03:00:00Z"),
                en_fecha(mov("RETENCION", "300", Some(cuenta), Some(cuenta)), 2, "2026-11-02T12:00:00Z"),
                en_fecha(compra, 4, "2026-11-05T18:00:00Z"),
                en_fecha(mov("RETIRO", "100", Some(cuenta), None), 5, "2026-11-20T18:00:00Z"),
            ],
            ordenes: vec![orden],
        };
        let lista = estados(&datos, cliente, en("2026-11-23T12:00:00Z"));
        assert_eq!(lista.iter().map(|e| e.period.as_str()).collect::<Vec<_>>(), ["2026-10", "2026-11"]);
        let (octubre, noviembre) = (&lista[0], &lista[1]);
        assert!(!octubre.provisional && noviembre.provisional);
        assert_eq!(octubre.closing_balance.to_string(), "5000");
        assert_eq!(noviembre.opening_balance, octubre.closing_balance, "el saldo inicial es el final del mes anterior");
        assert_eq!(noviembre.closing_balance, "3900.31".parse::<Decimal>().unwrap());
        assert_eq!(
            noviembre.entries.iter().map(|a| a.kind).collect::<Vec<_>>(),
            [TipoAsiento::MarketBuy, TipoAsiento::Withdrawal],
            "la retención no es un asiento"
        );
        assert_eq!(noviembre.entries[0].ticker.as_deref(), Some("AUG1"));
        assert_eq!(noviembre.closing_breakdown.on_hold.to_string(), "300");
        assert_eq!(noviembre.closing_breakdown.available, "3600.31".parse::<Decimal>().unwrap());
        assert_eq!((noviembre.trades.buys, noviembre.positions.len()), (1, 1));
        assert_eq!(noviembre.folio, "EC-2026-11-0017");
        assert_eq!((noviembre.secuencia_ifpe, noviembre.secuencia_mercado), (5, 3));
        assert_eq!(octubre.period_end, en("2026-11-01T06:00:00Z"));
        // Mismos datos, misma referencia; otro movimiento, otra.
        assert_eq!(estados(&datos, cliente, en("2026-11-23T12:00:00Z"))[1].reference, noviembre.reference);
        let mut otros = Datos { movimientos: datos.movimientos.clone(), ordenes: datos.ordenes.clone(), ..datos };
        otros.movimientos[3].monto = "101".parse().unwrap();
        assert_ne!(estados(&otros, cliente, en("2026-11-23T12:00:00Z"))[1].reference, noviembre.reference);
        assert_eq!(estados(&otros, cliente, en("2026-11-23T12:00:00Z"))[0].reference, octubre.reference);
    }

    #[test]
    fn solo_lo_que_cambia_el_saldo_es_un_asiento() {
        let yo = Uuid::from_u128(1);
        let otro = Uuid::from_u128(2);
        let efecto = |m: Movimiento| asiento(&m, yo).map(|(t, e)| (t, e.to_string()));
        assert_eq!(efecto(mov("DEPOSITO", "100", None, Some(yo))), Some((TipoAsiento::Deposit, "100".into())));
        assert_eq!(efecto(mov("RETIRO", "40", Some(yo), None)), Some((TipoAsiento::Withdrawal, "-40".into())));
        assert_eq!(
            efecto(mov("TRANSFERENCIA", "5", Some(otro), Some(yo))),
            Some((TipoAsiento::TransferIn, "5".into()))
        );
        assert_eq!(
            efecto(mov("TRANSFERENCIA", "5", Some(yo), Some(otro))),
            Some((TipoAsiento::TransferOut, "-5".into()))
        );
        assert_eq!(
            efecto(mov("COMPRA_MERCADO", "1250", Some(yo), None)),
            Some((TipoAsiento::MarketBuy, "-1250".into()))
        );
        assert_eq!(efecto(mov("VENTA_MERCADO", "900", None, Some(yo))), Some((TipoAsiento::MarketSell, "900".into())));
        for interno in ["RETENCION", "LIBERACION_RETENCION", "COMPROMISO_ORDEN", "LIBERACION_ORDEN"] {
            assert_eq!(efecto(mov(interno, "10", Some(yo), Some(yo))), None, "{interno}");
        }
    }
}
