//! Planes de ahorro (migración 0023): el mismo monto de un activo cada
//! semana, cada dos semanas o cada mes, con el efectivo disponible de la
//! bóveda. Cada compra es una orden de mercado como cualquier otra: el mismo
//! ledger encadenado, el mismo movimiento IFPE, la misma reserva del 100 % y
//! la misma evaluación PLD ([`super::orden`]); solo que la coloca el
//! programador.
//!
//! **Crear** ([`crear`], `POST /api/v1/market/order` con `order_type`
//! `savings_plan`), rol `sc_mercado`, una transacción:
//! 1. La cuenta IFPE del cliente `FOR NO KEY UPDATE`, el primer candado de
//!    toda orden ([`crate::ledger`]); la idempotencia después, con el
//!    candado: la repetición devuelve el plan en su estado actual y una clave
//!    usada para otra cosa (una orden de mercado, límite o stop, u otro plan)
//!    es 409.
//! 2. Reglas: KYC operable, la bóveda activa (el congelamiento preventivo
//!    responde el error constante, sin aviso), el activo abierto y con precio
//!    real (o MODO_DEMO), un monto que al precio de hoy compra al menos un
//!    incremento y a lo más [`MAXIMO_PLANES`] planes abiertos (activos o
//!    pausados). No se compromete efectivo: cada compra usa el disponible de
//!    su día. La base de datos repite las reglas en `mercado.fn_validar_plan`.
//! 3. El plan nace ACTIVO con la primera compra vencida: se hace en seguida,
//!    en su propia transacción y como la haría el programador, así que la
//!    respuesta trae su resultado (hecha u omitida) y las fechas siguientes.
//!
//! **Comprar** ([`ejecutar`]; [`ejecutar_vencidos`] recorre los vencidos, del
//! más atrasado al más reciente): con la cuenta y el plan bloqueados, y solo
//! si sigue activo y vencido. Las reglas de una orden de mercado (KYC
//! operable, bóveda activa, el activo abierto y ofrecido, efectivo disponible
//! para el monto, un incremento al precio vigente) y PLD antes de escribir,
//! en el canal COMPRA_MERCADO. La orden lleva la clave
//! `plan:<plan>:<periodo>` y se escribe con [`orden::escribir_orden`]; el
//! plan registra la compra (EJECUTADA, con esa orden) y la base de datos fija
//! la siguiente fecha. Si una regla la impide (efectivo insuficiente, mercado
//! cerrado, KYC, bóveda congelada, PLD...), la compra se registra OMITIDA con
//! el código, en su propia transacción, y el plan sigue en su fecha
//! siguiente: nada queda comprometido ni se reintenta. Un error transitorio
//! la deja vencida para la siguiente pasada.
//!
//! **Pausar, reanudar, cancelar** ([`cambiar`]): solo su cliente. Repetir un
//! cambio devuelve el plan igual; un plan cancelado ya no cambia (409
//! `PLAN_CANCELADO`). Al reanudar, la siguiente compra es la primera fecha
//! del calendario que aún no llega: las que pasaron en pausa no se reponen.
//!
//! Varias réplicas del backend pueden procesar a la vez: el candado de la
//! cuenta y el del plan las serializan, y la segunda lo encuentra avanzado.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Row};
use uuid::Uuid;

use super::{
    calculo, exigir_precio_real_o_demo, leer_activo,
    modelos::{EjecucionPendiente, Lado, Orden},
    orden::{self, NuevaOrden},
    pendiente, ticker_valido,
};
use crate::{
    aml::{self, Canal, Contraparte, Decision, Operacion},
    config::Config,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    ledger, pld,
};

/// Planes abiertos (activos o pausados) por cliente; `mercado.maximo_planes()`.
pub const MAXIMO_PLANES: i64 = 20;

/// Planes vencidos que procesa una pasada del programador (la siguiente sigue).
const LOTE_PLANES: i64 = 500;

/// Fechas de compra que muestra un plan activo: la siguiente y dos más.
const PROXIMAS: i32 = 3;

/// Cada cuánto compra (`mercado.planes_ahorro.frecuencia`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum Frecuencia {
    #[sqlx(rename = "SEMANAL")]
    #[serde(rename = "weekly")]
    Semanal,
    #[sqlx(rename = "QUINCENAL")]
    #[serde(rename = "biweekly")]
    Quincenal,
    #[sqlx(rename = "MENSUAL")]
    #[serde(rename = "monthly")]
    Mensual,
}

impl Frecuencia {
    pub const fn as_str(self) -> &'static str {
        match self {
            Frecuencia::Semanal => "SEMANAL",
            Frecuencia::Quincenal => "QUINCENAL",
            Frecuencia::Mensual => "MENSUAL",
        }
    }
}

/// Estado de un plan: pausa y reanuda; CANCELADO es definitivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum EstadoPlan {
    #[sqlx(rename = "ACTIVO")]
    #[serde(rename = "active")]
    Activo,
    #[sqlx(rename = "PAUSADO")]
    #[serde(rename = "paused")]
    Pausado,
    #[sqlx(rename = "CANCELADO")]
    #[serde(rename = "cancelled")]
    Cancelado,
}

impl EstadoPlan {
    pub const fn as_str(self) -> &'static str {
        match self {
            EstadoPlan::Activo => "ACTIVO",
            EstadoPlan::Pausado => "PAUSADO",
            EstadoPlan::Cancelado => "CANCELADO",
        }
    }
}

/// Qué pasó con la última compra vencida.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum ResultadoCompra {
    #[sqlx(rename = "EJECUTADA")]
    #[serde(rename = "executed")]
    Ejecutada,
    #[sqlx(rename = "OMITIDA")]
    #[serde(rename = "skipped")]
    Omitida,
}

/// `mercado.planes_ahorro`: sus términos no cambian; su calendario es de la
/// base de datos.
#[derive(Debug, Clone, FromRow)]
pub struct Plan {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub cuenta_ifpe_id: Uuid,
    pub ticker: String,
    /// Cada compra, en pesos y centavos.
    pub monto: Decimal,
    pub frecuencia: Frecuencia,
    pub estado: EstadoPlan,
    /// El día del alta en la Ciudad de México: de él se cuentan las compras.
    pub fecha_ancla: NaiveDate,
    /// La compra que sigue (0, la del alta).
    pub periodo: i32,
    /// Cuándo; solo con el plan activo.
    pub proxima_ejecucion_en: Option<DateTime<Utc>>,
    pub ultima_ejecucion_en: Option<DateTime<Utc>>,
    pub ultimo_resultado: Option<ResultadoCompra>,
    /// Código de la regla que impidió la última compra (OMITIDA).
    pub motivo: Option<String>,
    pub ultima_orden_id: Option<Uuid>,
    pub clave_idempotencia: String,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
    pub cancelado_en: Option<DateTime<Utc>>,
}

/// La clave de la orden (y de su movimiento IFPE y su evaluación PLD) de la
/// compra `periodo`; la base de datos la exige (`mercado.fn_ejecucion_plan`).
fn clave_compra(plan: Uuid, periodo: i32) -> String {
    format!("plan:{plan}:{periodo}")
}

// -----------------------------------------------------------------------------
// Lo que lee el frontend
// -----------------------------------------------------------------------------

/// Un plan como lo lee el frontend (`SavingsPlan`): la respuesta de crearlo,
/// pausarlo, reanudarlo o cancelarlo y cada renglón de
/// `GET /api/v1/market/savings-plans`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VistaPlan {
    #[serde(rename = "plan_id")]
    pub id: Uuid,
    /// Siempre `savings_plan`: distingue el plan de un recibo o de una orden
    /// pendiente en la respuesta de `POST /market/order`.
    #[serde(rename = "order_type")]
    pub tipo: &'static str,
    pub status: EstadoPlan,
    pub ticker: String,
    /// Siempre `buy`.
    #[serde(rename = "side")]
    pub lado: Lado,
    /// Cada compra, en MXN.
    #[serde(rename = "amount")]
    pub monto: Decimal,
    #[serde(rename = "frequency")]
    pub frecuencia: Frecuencia,
    #[serde(rename = "anchor_date")]
    pub fecha_ancla: NaiveDate,
    /// La siguiente compra (activo); null en pausa o cancelado.
    #[serde(rename = "next_execution_at")]
    pub proxima_ejecucion_en: Option<DateTime<Utc>>,
    /// La siguiente y las dos que le siguen (activo); vacío si no.
    #[serde(rename = "upcoming")]
    pub proximas: Vec<DateTime<Utc>>,
    /// Cuándo se procesó la última compra vencida (hecha u omitida).
    #[serde(rename = "last_execution_at")]
    pub ultima_ejecucion_en: Option<DateTime<Utc>>,
    #[serde(rename = "last_result")]
    pub ultimo_resultado: Option<ResultadoCompra>,
    /// Código de la regla que impidió la última compra (`skipped`).
    #[serde(rename = "failure_reason")]
    pub motivo: Option<String>,
    /// La compra más reciente que se hizo (aunque la última se haya omitido).
    #[serde(rename = "last_purchase")]
    pub ultima_compra: Option<EjecucionPendiente>,
    /// Compras hechas y omitidas desde el alta.
    #[serde(rename = "purchases")]
    pub compras: i64,
    #[serde(rename = "skipped")]
    pub omitidas: i64,
    /// Lo que se cargó a la bóveda por todas sus compras, en MXN.
    #[serde(rename = "invested")]
    pub invertido: Decimal,
    /// Las unidades que compró.
    #[serde(rename = "quantity")]
    pub cantidad: Decimal,
    #[serde(rename = "created_at")]
    pub creado_en: DateTime<Utc>,
    #[serde(rename = "cancelled_at")]
    pub cancelado_en: Option<DateTime<Utc>>,
}

/// `GET /api/v1/market/savings-plans` (`SavingsPlans`): los planes activos y
/// pausados, del más nuevo al más antiguo.
#[derive(Debug, Clone, Serialize)]
pub struct PlanesAhorro {
    #[serde(rename = "plans")]
    pub planes: Vec<VistaPlan>,
}

/// Un plan con sus próximas fechas y la cuenta de sus compras.
#[derive(FromRow)]
struct FilaPlan {
    #[sqlx(flatten)]
    plan: Plan,
    proximas: Vec<DateTime<Utc>>,
    compras: i64,
    omitidas: i64,
    invertido: Decimal,
    cantidad: Decimal,
}

impl VistaPlan {
    fn de(f: &FilaPlan, ultima_compra: Option<&Orden>) -> Self {
        let p = &f.plan;
        Self {
            id: p.id,
            tipo: "savings_plan",
            status: p.estado,
            ticker: p.ticker.clone(),
            lado: Lado::Compra,
            monto: dinero::con_escala(p.monto),
            frecuencia: p.frecuencia,
            fecha_ancla: p.fecha_ancla,
            proxima_ejecucion_en: p.proxima_ejecucion_en,
            proximas: f.proximas.clone(),
            ultima_ejecucion_en: p.ultima_ejecucion_en,
            ultimo_resultado: p.ultimo_resultado,
            motivo: p.motivo.clone(),
            ultima_compra: ultima_compra.map(EjecucionPendiente::from),
            compras: f.compras,
            omitidas: f.omitidas,
            invertido: dinero::con_escala(f.invertido),
            cantidad: dinero::con_escala(f.cantidad),
            creado_en: p.creado_en,
            cancelado_en: p.cancelado_en,
        }
    }
}

/// Los planes que cumplen `condicion` (sobre `p`, con el parámetro `$2`) y su
/// compra más reciente. Las fechas las calcula la base de datos con el mismo
/// calendario que las fija.
async fn vistas(tx: &mut Tx, condicion: &str, valor: Uuid) -> AppResult<Vec<VistaPlan>> {
    let filas: Vec<FilaPlan> = sqlx::query_as(&format!(
        "SELECT p.*, \
                CASE WHEN p.estado = 'ACTIVO' THEN \
                     ARRAY[p.proxima_ejecucion_en] || ARRAY( \
                       SELECT mercado.momento_plan(p.frecuencia, p.fecha_ancla, p.periodo + k) \
                         FROM generate_series(1, $1 - 1) AS k ORDER BY k) \
                     ELSE ARRAY[]::timestamptz[] END AS proximas, \
                s.compras, s.omitidas, s.invertido, s.cantidad \
           FROM mercado.planes_ahorro p \
          CROSS JOIN LATERAL ( \
                SELECT count(*) FILTER (WHERE e.evento = 'EJECUTADO') AS compras, \
                       count(*) FILTER (WHERE e.evento = 'OMITIDO') AS omitidas, \
                       coalesce(sum(o.monto), 0) AS invertido, \
                       coalesce(sum(o.cantidad), 0) AS cantidad \
                  FROM mercado.eventos_plan e LEFT JOIN mercado.ordenes o ON o.id = e.orden_id \
                 WHERE e.plan_id = p.id AND e.evento IN ('EJECUTADO', 'OMITIDO')) s \
          WHERE {condicion} \
          ORDER BY p.creado_en DESC, p.id DESC"
    ))
    .bind(PROXIMAS)
    .bind(valor)
    .fetch_all(&mut **tx)
    .await?;

    let ids: Vec<Uuid> = filas.iter().filter(|f| f.compras > 0).map(|f| f.plan.id).collect();
    let mut ultimas: HashMap<Uuid, Orden> = HashMap::new();
    if !ids.is_empty() {
        let renglones = sqlx::query(
            "SELECT DISTINCT ON (e.plan_id) e.plan_id AS plan_de_la_compra, o.* \
               FROM mercado.eventos_plan e JOIN mercado.ordenes o ON o.id = e.orden_id \
              WHERE e.plan_id = ANY($1) AND e.evento = 'EJECUTADO' \
              ORDER BY e.plan_id, e.id DESC",
        )
        .bind(&ids[..])
        .fetch_all(&mut **tx)
        .await?;
        for r in &renglones {
            ultimas.insert(r.try_get("plan_de_la_compra")?, Orden::from_row(r)?);
        }
    }
    Ok(filas.iter().map(|f| VistaPlan::de(f, ultimas.get(&f.plan.id))).collect())
}

/// Los planes activos y pausados del cliente. Con su rol y RLS: solo los suyos.
pub async fn listar(pool: &PgPool, cliente_id: Uuid) -> AppResult<Vec<VistaPlan>> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let planes = vistas(&mut tx, "p.cliente_id = $2 AND p.estado <> 'CANCELADO'", cliente_id).await?;
    tx.commit().await?;
    Ok(planes)
}

/// Un plan del cliente, en cualquier estado (el de otro no existe: 404).
pub async fn leer(pool: &PgPool, cliente_id: Uuid, id: Uuid) -> AppResult<VistaPlan> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let plan = vistas(&mut tx, "p.id = $2", id).await?.into_iter().next();
    tx.commit().await?;
    plan.ok_or_else(no_existe)
}

fn no_existe() -> AppError {
    AppError::no_encontrado("el plan de ahorro no existe")
}

// -----------------------------------------------------------------------------
// Crear
// -----------------------------------------------------------------------------

/// El plan que pide el cliente.
#[derive(Debug, Clone)]
pub struct SolicitudPlan {
    pub cliente_id: Uuid,
    pub ticker: String,
    /// Cada compra, en pesos con centavos.
    pub monto: Decimal,
    pub frecuencia: Frecuencia,
    /// Clave ya acotada al cliente (`Usuario::clave`).
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone)]
pub struct ResultadoPlan {
    pub plan: VistaPlan,
    /// `true` si la clave ya existía y se devolvió el plan original.
    pub idempotente: bool,
}

/// Crea el plan y hace su primera compra.
pub async fn crear(pool: &PgPool, config: &Config, s: &SolicitudPlan) -> AppResult<ResultadoPlan> {
    if !ticker_valido(&s.ticker) {
        return Err(AppError::no_encontrado(format!("el activo {} no existe", s.ticker)));
    }
    let monto = orden::validar_monto(s.monto)?;
    let (id, idempotente) = db::con_reintentos(|| crear_tx(pool, config, s, monto)).await?;
    if !idempotente {
        // Ya creado (y confirmado): si la compra falla por algo transitorio,
        // sigue vencida y el programador la toma en su siguiente pasada.
        if let Err(e) = ejecutar(pool, config, id).await {
            tracing::warn!(plan = %id, codigo = e.codigo(), error = %e.detalle(),
                "la primera compra del plan de ahorro no se hizo al crearlo; queda para el programador");
        }
    }
    Ok(ResultadoPlan { plan: leer(pool, s.cliente_id, id).await?, idempotente })
}

/// El id del plan creado (o del previo con esa clave) y si se repitió.
async fn crear_tx(pool: &PgPool, config: &Config, s: &SolicitudPlan, monto: Decimal) -> AppResult<(Uuid, bool)> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;

    // 1. Cuenta IFPE del cliente (primer candado) e idempotencia.
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    let cuenta = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    if let Some(previo) = por_clave(&mut tx, &s.clave_idempotencia).await? {
        if previo.cliente_id != s.cliente_id
            || previo.ticker != s.ticker
            || previo.monto != monto
            || previo.frecuencia != s.frecuencia
        {
            return Err(orden::clave_reutilizada());
        }
        tx.commit().await?;
        return Ok((previo.id, true));
    }
    if orden::orden_por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
        || pendiente::por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
        || ledger::buscar_por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
    {
        return Err(orden::clave_reutilizada());
    }

    // 2. Reglas de negocio. Congelada (preventivo u operador) o cerrada: no
    // crea; el preventivo responde el error constante, sin aviso.
    pld::cargar_perfil(&mut tx, s.cliente_id).await?.exigir_operable()?;
    ledger::exigir_debitable(&cuenta)?;
    let activo = leer_activo(&mut tx, &s.ticker).await?;
    exigir_precio_real_o_demo(&activo, config)?;
    if !activo.operable {
        return Err(orden::mercado_cerrado(&activo));
    }
    // Al precio de hoy, cada compra alcanza al menos un incremento.
    calculo::compra(monto, activo.precio, activo.decimales()).map_err(|r| orden::rechazo(r, &activo))?;
    let abiertos: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mercado.planes_ahorro WHERE cliente_id = $1 AND estado <> 'CANCELADO'",
    )
    .bind(s.cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    if abiertos >= MAXIMO_PLANES {
        return Err(AppError::negocio(
            "LIMITE_PLANES",
            format!("ya tienes {MAXIMO_PLANES} planes de ahorro; cancela alguno para crear otro"),
        ));
    }

    // 3. El plan: la base de datos pone el ancla, la primera compra (ya) y su evento.
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO mercado.planes_ahorro (id, cliente_id, cuenta_ifpe_id, ticker, monto, frecuencia, \
         clave_idempotencia) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(s.cliente_id)
    .bind(cuenta.id)
    .bind(&s.ticker)
    .bind(monto)
    .bind(s.frecuencia)
    .bind(&s.clave_idempotencia)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(plan = %id, ticker = %s.ticker, frecuencia = s.frecuencia.as_str(), "plan de ahorro creado");
    Ok((id, false))
}

pub(super) async fn por_clave(tx: &mut Tx, clave: &str) -> AppResult<Option<Plan>> {
    Ok(sqlx::query_as("SELECT * FROM mercado.planes_ahorro WHERE clave_idempotencia = $1")
        .bind(clave)
        .fetch_optional(&mut **tx)
        .await?)
}

/// El plan, bloqueado hasta el COMMIT (después del candado de su cuenta), y
/// si su compra ya venció (con el reloj de la base de datos, el mismo con el
/// que la registra `mercado.fn_ejecucion_plan`).
#[derive(FromRow)]
struct PlanBloqueado {
    #[sqlx(flatten)]
    plan: Plan,
    vencido: bool,
}

async fn bloquear(tx: &mut Tx, id: Uuid) -> AppResult<PlanBloqueado> {
    sqlx::query_as(
        "SELECT *, coalesce(estado = 'ACTIVO' AND proxima_ejecucion_en <= now(), FALSE) AS vencido \
           FROM mercado.planes_ahorro WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(no_existe)
}

// -----------------------------------------------------------------------------
// Comprar
// -----------------------------------------------------------------------------

/// Lo que hizo un intento de compra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compra {
    Ejecutada,
    /// Una regla la impidió, con su código: el plan sigue en su fecha siguiente.
    Omitida(String),
    /// Nada: el plan no está activo o su compra no ha vencido (otra réplica la procesó).
    SinCambio,
}

/// Una pasada del programador.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResumenPlanes {
    pub revisados: usize,
    pub ejecutados: usize,
    pub omitidos: usize,
    pub errores: usize,
}

/// Hace la compra vencida del plan; si una regla lo impide, la registra
/// omitida. Un error transitorio (o de integridad) la deja vencida y se
/// devuelve.
pub async fn ejecutar(pool: &PgPool, config: &Config, id: Uuid) -> AppResult<Compra> {
    match db::con_reintentos(|| comprar_tx(pool, config, id)).await {
        Err(e) if omite(&e) => {
            let motivo = pendiente::motivo_de(&e);
            Ok(if db::con_reintentos(|| omitir_tx(pool, id, &motivo)).await? {
                tracing::info!(plan = %id, motivo = %motivo, "compra de plan de ahorro omitida");
                Compra::Omitida(motivo)
            } else {
                Compra::SinCambio
            })
        }
        otro => otro,
    }
}

/// Una regla de negocio (no un error transitorio ni de integridad) que
/// impide la compra de hoy: se omite y el plan sigue. También un activo
/// suspendido o sin precio real: la compra no espera a que vuelva.
fn omite(e: &AppError) -> bool {
    match e {
        AppError::OperacionNoDisponible | AppError::Negocio { .. } | AppError::Validacion(_) => true,
        AppError::Peticion { codigo, .. } => *codigo == "MERCADO_NO_DISPONIBLE",
        _ => false,
    }
}

async fn comprar_tx(pool: &PgPool, config: &Config, id: Uuid) -> AppResult<Compra> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    // La cuenta primero (orden global de candados), luego el plan.
    let cuenta_id: Option<Uuid> = sqlx::query_scalar("SELECT cuenta_ifpe_id FROM mercado.planes_ahorro WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(cuenta_id) = cuenta_id else { return Ok(Compra::SinCambio) };
    let cuenta = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let PlanBloqueado { plan: p, vencido } = bloquear(&mut tx, id).await?;
    if !vencido {
        tx.commit().await?;
        return Ok(Compra::SinCambio);
    }

    // Las reglas de una orden de mercado: un error aquí omite la compra.
    pld::cargar_perfil(&mut tx, p.cliente_id).await?.exigir_operable()?;
    ledger::exigir_debitable(&cuenta)?;
    let activo = leer_activo(&mut tx, &p.ticker).await?;
    exigir_precio_real_o_demo(&activo, config)?;
    if !activo.operable {
        return Err(orden::mercado_cerrado(&activo));
    }
    ledger::exigir_saldo(&cuenta, p.monto)?;
    let e = calculo::compra(p.monto, activo.precio, activo.decimales()).map_err(|r| orden::rechazo(r, &activo))?;

    // PLD antes de cualquier escritura; un BLOQUEAR confirma solo la evaluación.
    let orden_id = Uuid::new_v4();
    let tx_ifpe_id = Uuid::new_v4();
    let clave = clave_compra(p.id, p.periodo);
    let d = aml::evaluar(
        &mut tx,
        &Operacion {
            cliente_id: p.cliente_id,
            canal: Canal::CompraMercado,
            monto: e.monto,
            transaccion_id: tx_ifpe_id,
            clave: &clave,
            contraparte: Contraparte::default(),
        },
    )
    .await?;
    if d.decision == Decision::Bloquear {
        return aml::confirmar_bloqueo(tx, &d).await;
    }

    // La orden de mercado (reserva, saldo, movimiento IFPE y ledger del
    // mercado) y su registro en el plan; la base de datos fija la siguiente.
    let o = orden::escribir_orden(
        &mut tx,
        NuevaOrden {
            id: orden_id,
            tx_ifpe_id,
            cliente_id: p.cliente_id,
            cuenta_id: cuenta.id,
            activo: &activo,
            lado: Lado::Compra,
            monto_solicitado: p.monto,
            ejecucion: e,
            clave: &clave,
        },
    )
    .await?;
    sqlx::query(
        "UPDATE mercado.planes_ahorro SET ultimo_resultado = 'EJECUTADA', motivo = NULL, ultima_orden_id = $2 \
         WHERE id = $1",
    )
    .bind(p.id)
    .bind(o.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(plan = %p.id, orden = %o.id, ticker = %p.ticker, periodo = p.periodo, precio = %o.precio,
        "compra de plan de ahorro ejecutada");
    Ok(Compra::Ejecutada)
}

/// Registra omitida la compra vencida, con el código de la regla. `false` si
/// ya no estaba vencida (otra réplica la procesó, o el plan cambió).
async fn omitir_tx(pool: &PgPool, id: Uuid, motivo: &str) -> AppResult<bool> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    let cuenta_id: Option<Uuid> = sqlx::query_scalar("SELECT cuenta_ifpe_id FROM mercado.planes_ahorro WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(cuenta_id) = cuenta_id else { return Ok(false) };
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    if !bloquear(&mut tx, id).await?.vencido {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query(
        "UPDATE mercado.planes_ahorro SET ultimo_resultado = 'OMITIDA', motivo = $2, ultima_orden_id = NULL \
         WHERE id = $1",
    )
    .bind(id)
    .bind(motivo)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Una pasada del programador: hace (u omite) cada compra vencida, de la más
/// atrasada a la más reciente.
pub async fn ejecutar_vencidos(pool: &PgPool, config: &Config) -> AppResult<ResumenPlanes> {
    let mut tx = db::begin_lectura(pool, Rol::Mercado).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM mercado.planes_ahorro WHERE estado = 'ACTIVO' AND proxima_ejecucion_en <= now() \
          ORDER BY proxima_ejecucion_en, id LIMIT $1",
    )
    .bind(LOTE_PLANES)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut r = ResumenPlanes::default();
    for id in ids {
        r.revisados += 1;
        match ejecutar(pool, config, id).await {
            Ok(Compra::Ejecutada) => r.ejecutados += 1,
            Ok(Compra::Omitida(_)) => r.omitidos += 1,
            Ok(Compra::SinCambio) => {}
            Err(e) => {
                r.errores += 1;
                tracing::error!(plan = %id, codigo = e.codigo(), error = %e.detalle(),
                    "no se pudo hacer la compra de un plan de ahorro; sigue vencida");
            }
        }
    }
    Ok(r)
}

// -----------------------------------------------------------------------------
// Pausar, reanudar, cancelar
// -----------------------------------------------------------------------------

/// Lo que el cliente pide hacer con su plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cambio {
    Pausar,
    Reanudar,
    Cancelar,
}

impl Cambio {
    const fn destino(self) -> EstadoPlan {
        match self {
            Cambio::Pausar => EstadoPlan::Pausado,
            Cambio::Reanudar => EstadoPlan::Activo,
            Cambio::Cancelar => EstadoPlan::Cancelado,
        }
    }
}

/// Pausa, reanuda o cancela un plan del cliente. Repetirlo devuelve el plan
/// igual; uno cancelado no cambia (409 `PLAN_CANCELADO`); el de otro cliente
/// no existe (404). Pausar y cancelar siempre se puede (también con la
/// bóveda congelada): detienen compras, no mueven dinero.
pub async fn cambiar(pool: &PgPool, cliente_id: Uuid, id: Uuid, cambio: Cambio) -> AppResult<VistaPlan> {
    db::con_reintentos(|| cambiar_tx(pool, cliente_id, id, cambio)).await?;
    leer(pool, cliente_id, id).await
}

async fn cambiar_tx(pool: &PgPool, cliente_id: Uuid, id: Uuid, cambio: Cambio) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    let fila: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT cliente_id, cuenta_ifpe_id FROM mercado.planes_ahorro WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((_, cuenta_id)) = fila.filter(|(dueno, _)| *dueno == cliente_id) else {
        return Err(no_existe());
    };
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let p = bloquear(&mut tx, id).await?.plan;
    let destino = cambio.destino();
    if p.estado == destino {
        tx.commit().await?;
        return Ok(());
    }
    if p.estado == EstadoPlan::Cancelado {
        return Err(AppError::conflicto("PLAN_CANCELADO", "el plan de ahorro ya está cancelado"));
    }
    // La base de datos fija las fechas (en pausa ninguna; al reanudar, la
    // primera que aún no llega) y escribe el evento.
    sqlx::query("UPDATE mercado.planes_ahorro SET estado = $2 WHERE id = $1")
        .bind(id)
        .bind(destino)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(plan = %id, de = p.estado.as_str(), a = destino.as_str(), "plan de ahorro cambió de estado");
    Ok(())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_clave_de_cada_compra() {
        let plan = Uuid::parse_str("9a000000-0000-0000-0000-000000000001").unwrap();
        assert_eq!(clave_compra(plan, 0), "plan:9a000000-0000-0000-0000-000000000001:0");
        assert_eq!(clave_compra(plan, 12), "plan:9a000000-0000-0000-0000-000000000001:12");
    }

    #[test]
    fn que_omite_una_compra() {
        assert!(omite(&AppError::OperacionNoDisponible));
        assert!(omite(&AppError::negocio("SALDO_INSUFICIENTE", "x")));
        assert!(omite(&AppError::negocio("MERCADO_CERRADO", "x")), "no espera a que el activo vuelva a operar");
        assert!(omite(&AppError::negocio("KYC_NO_VERIFICADO", "x")));
        assert!(omite(&AppError::validacion("fuera de rango")));
        assert!(omite(&AppError::peticion(axum::http::StatusCode::SERVICE_UNAVAILABLE, "MERCADO_NO_DISPONIBLE", "x")));
        assert!(!omite(&AppError::Reintentable), "transitorio: sigue vencida");
        assert!(!omite(&AppError::Integridad("x".into())));
        assert!(!omite(&AppError::conflicto("OPERACION_DUPLICADA", "x")));
        assert!(!omite(&AppError::no_encontrado("x")));
    }

    #[test]
    fn la_frecuencia_en_el_contrato() {
        let f: Frecuencia = serde_json::from_str("\"biweekly\"").unwrap();
        assert_eq!(f, Frecuencia::Quincenal);
        assert_eq!(serde_json::to_string(&Frecuencia::Mensual).unwrap(), "\"monthly\"");
        assert!(serde_json::from_str::<Frecuencia>("\"daily\"").is_err());
        assert!(serde_json::from_str::<Frecuencia>("\"SEMANAL\"").is_err());
    }
}
