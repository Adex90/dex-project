//! Alertas de precio (migración 0024): avisar al cliente cuando el precio de
//! un activo llegue a un objetivo, al subir (`above`) o al bajar (`below`).
//!
//! - **Crear** ([`crear`], `POST /api/v1/market/alerts`): rol `sc_mercado`,
//!   una transacción. La cuenta IFPE del cliente primero (el primer candado
//!   del orden global: el trigger sube su versión y la bóveda avisa) y la
//!   idempotencia con el candado: la misma clave con los mismos términos
//!   devuelve la alerta como está (200), con otros es 409. Reglas: el activo
//!   existe y se ofrece (precio real, o simulado con MODO_DEMO), el objetivo es
//!   un precio válido del lado que aún no se alcanza (subir: por encima del
//!   precio vigente; bajar: por debajo) y a lo más [`MAXIMO_ALERTAS`] activas.
//!   La base de datos pone la referencia (el precio del catálogo) y las fechas.
//! - **Listar** ([`listar`], `GET /api/v1/market/alerts`): las activas y las
//!   disparadas en los últimos [`DIAS_DISPARADAS`] días, de la más reciente a
//!   la más antigua; con el rol del cliente y RLS.
//! - **Cancelar** ([`cancelar`], `POST /api/v1/market/alerts/{id}/cancel`):
//!   solo su cliente. Repetirlo devuelve la misma alerta cancelada; una ya
//!   disparada es 409 `AVISO_DISPARADO`; la de otro cliente no existe (404).
//! - **Disparar** ([`evaluar`], en la pasada del motor de cada 5 segundos):
//!   las alertas activas de los activos que se ofrecen cuyo objetivo alcanzó
//!   el precio vigente se disparan, cliente por cliente, cada uno en su
//!   transacción y con su cuenta bloqueada primero. La base de datos anota el
//!   precio del catálogo que la cumplió; si el oráculo lo movió desde que el
//!   motor la eligió, la transacción se repite con el precio nuevo. Disparar
//!   sube la versión de la bóveda: el WebSocket avisa y la pantalla anuncia la
//!   alerta disparada.
//!
//! No exigen KYC ni se niegan a una cuenta congelada: no mueven dinero, y
//! negarlas delataría el congelamiento. Los códigos de error que llegan al
//! cliente dicen AVISO, nunca ALERTA (una palabra de PLD que el cliente no
//! debe leer).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{exigir_precio_real_o_demo, leer_activo, ticker_valido};
use crate::{
    config::Config,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    ledger,
};

/// Alertas activas por cliente, a lo más (`mercado.maximo_alertas()`).
pub const MAXIMO_ALERTAS: i64 = 50;

/// Las disparadas se listan durante este tiempo (las activas, siempre).
pub const DIAS_DISPARADAS: i32 = 30;

/// Clientes con alertas por disparar que atiende una pasada del motor (la siguiente sigue).
const LOTE_CLIENTES: i64 = 500;

/// Hacia dónde mira el objetivo: `ARRIBA`/`ABAJO` en la base de datos,
/// `above`/`below` en la API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum Direccion {
    /// Se dispara con el precio en o por encima del objetivo.
    #[sqlx(rename = "ARRIBA")]
    #[serde(rename = "above")]
    Arriba,
    /// Se dispara con el precio en o por debajo del objetivo.
    #[sqlx(rename = "ABAJO")]
    #[serde(rename = "below")]
    Abajo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum EstadoAlerta {
    #[sqlx(rename = "ACTIVA")]
    #[serde(rename = "active")]
    Activa,
    #[sqlx(rename = "DISPARADA")]
    #[serde(rename = "triggered")]
    Disparada,
    #[sqlx(rename = "CANCELADA")]
    #[serde(rename = "cancelled")]
    Cancelada,
}

/// `mercado.alertas_precio`.
#[derive(Debug, Clone, FromRow)]
pub struct Alerta {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub ticker: String,
    pub precio_objetivo: Decimal,
    pub direccion: Direccion,
    pub estado: EstadoAlerta,
    pub precio_referencia: Decimal,
    pub precio_disparo: Option<Decimal>,
    pub disparada_en: Option<DateTime<Utc>>,
    pub cancelada_en: Option<DateTime<Utc>>,
    pub clave_idempotencia: String,
    pub creada_en: DateTime<Utc>,
}

/// Una alerta como la lee el frontend (`PriceAlert`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VistaAlerta {
    #[serde(rename = "alert_id")]
    pub id: Uuid,
    pub ticker: String,
    #[serde(rename = "direction")]
    pub direccion: Direccion,
    #[serde(rename = "target_price")]
    pub precio_objetivo: Decimal,
    pub status: EstadoAlerta,
    /// El precio cuando se creó.
    #[serde(rename = "reference_price")]
    pub precio_referencia: Decimal,
    /// El precio que la disparó.
    #[serde(rename = "triggered_price")]
    pub precio_disparo: Option<Decimal>,
    #[serde(rename = "created_at")]
    pub creada_en: DateTime<Utc>,
    #[serde(rename = "triggered_at")]
    pub disparada_en: Option<DateTime<Utc>>,
    #[serde(rename = "cancelled_at")]
    pub cancelada_en: Option<DateTime<Utc>>,
}

impl From<&Alerta> for VistaAlerta {
    fn from(a: &Alerta) -> Self {
        Self {
            id: a.id,
            ticker: a.ticker.clone(),
            direccion: a.direccion,
            precio_objetivo: dinero::con_escala(a.precio_objetivo),
            status: a.estado,
            precio_referencia: dinero::con_escala(a.precio_referencia),
            precio_disparo: a.precio_disparo.map(dinero::con_escala),
            creada_en: a.creada_en,
            disparada_en: a.disparada_en,
            cancelada_en: a.cancelada_en,
        }
    }
}

/// `GET /api/v1/market/alerts` (`PriceAlerts`).
#[derive(Debug, Clone, Serialize)]
pub struct Alertas {
    #[serde(rename = "alerts")]
    pub alertas: Vec<VistaAlerta>,
}

/// La alerta que pide el cliente.
#[derive(Debug, Clone)]
pub struct SolicitudAlerta {
    pub cliente_id: Uuid,
    pub ticker: String,
    pub precio_objetivo: Decimal,
    pub direccion: Direccion,
    /// Clave ya acotada al cliente (`Usuario::clave`).
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone)]
pub struct ResultadoAlerta {
    pub alerta: VistaAlerta,
    /// `true` si la clave ya existía y se devolvió la alerta original.
    pub idempotente: bool,
}

/// Un precio objetivo: positivo, con a lo más 8 decimales y dentro de `NUMERIC(18, 8)`.
fn validar_objetivo(precio: Decimal) -> AppResult<Decimal> {
    if precio <= Decimal::ZERO {
        return Err(AppError::validacion("el precio objetivo debe ser mayor a cero"));
    }
    if precio.normalize().scale() > dinero::ESCALA {
        return Err(AppError::validacion("el precio objetivo admite como máximo 8 decimales"));
    }
    if precio > dinero::maximo() {
        return Err(AppError::validacion("el precio objetivo está fuera del rango permitido"));
    }
    Ok(dinero::con_escala(precio))
}

pub async fn crear(pool: &PgPool, config: &Config, s: &SolicitudAlerta) -> AppResult<ResultadoAlerta> {
    if !ticker_valido(&s.ticker) {
        return Err(AppError::no_encontrado(format!("el activo {} no existe", s.ticker)));
    }
    let objetivo = validar_objetivo(s.precio_objetivo)?;
    db::con_reintentos(|| crear_tx(pool, config, s, objetivo)).await
}

async fn crear_tx(
    pool: &PgPool,
    config: &Config,
    s: &SolicitudAlerta,
    objetivo: Decimal,
) -> AppResult<ResultadoAlerta> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let previa: Option<Alerta> = sqlx::query_as("SELECT * FROM mercado.alertas_precio WHERE clave_idempotencia = $1")
        .bind(&s.clave_idempotencia)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(previa) = previa {
        if previa.cliente_id != s.cliente_id
            || previa.ticker != s.ticker
            || previa.precio_objetivo != objetivo
            || previa.direccion != s.direccion
        {
            return Err(AppError::conflicto(
                "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                "la clave de idempotencia ya se usó para una operación distinta",
            ));
        }
        tx.commit().await?;
        return Ok(ResultadoAlerta { alerta: VistaAlerta::from(&previa), idempotente: true });
    }

    let activo = leer_activo(&mut tx, &s.ticker).await?;
    exigir_precio_real_o_demo(&activo, config)?;
    let alcanzado = match s.direccion {
        Direccion::Arriba => objetivo <= activo.precio,
        Direccion::Abajo => objetivo >= activo.precio,
    };
    if alcanzado {
        return Err(AppError::negocio(
            "AVISO_YA_ALCANZADO",
            match s.direccion {
                Direccion::Arriba => format!(
                    "{} ya está en {}: un objetivo al alza va por encima del precio vigente",
                    activo.ticker,
                    dinero::con_escala(activo.precio)
                ),
                Direccion::Abajo => format!(
                    "{} ya está en {}: un objetivo a la baja va por debajo del precio vigente",
                    activo.ticker,
                    dinero::con_escala(activo.precio)
                ),
            },
        ));
    }
    let activas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mercado.alertas_precio WHERE cliente_id = $1 AND estado = 'ACTIVA'")
            .bind(s.cliente_id)
            .fetch_one(&mut *tx)
            .await?;
    if activas >= MAXIMO_ALERTAS {
        return Err(AppError::negocio(
            "LIMITE_AVISOS_PRECIO",
            format!("ya tienes {MAXIMO_ALERTAS} avisos de precio activos; cancela alguno para crear otro"),
        ));
    }

    let a: Alerta = sqlx::query_as(
        "INSERT INTO mercado.alertas_precio (id, cliente_id, ticker, precio_objetivo, direccion, clave_idempotencia) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(s.cliente_id)
    .bind(&s.ticker)
    .bind(objetivo)
    .bind(s.direccion)
    .bind(&s.clave_idempotencia)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(alerta = %a.id, ticker = %a.ticker, "alerta de precio creada");
    Ok(ResultadoAlerta { alerta: VistaAlerta::from(&a), idempotente: false })
}

/// Las alertas activas del cliente y las disparadas en los últimos
/// [`DIAS_DISPARADAS`] días, de la más reciente a la más antigua.
pub async fn listar(pool: &PgPool, cliente_id: Uuid) -> AppResult<Alertas> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let filas: Vec<Alerta> = sqlx::query_as(
        "SELECT * FROM mercado.alertas_precio \
          WHERE cliente_id = $1 \
            AND (estado = 'ACTIVA' OR (estado = 'DISPARADA' AND disparada_en > now() - make_interval(days => $2))) \
          ORDER BY coalesce(disparada_en, creada_en) DESC, id DESC",
    )
    .bind(cliente_id)
    .bind(DIAS_DISPARADAS)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Alertas { alertas: filas.iter().map(VistaAlerta::from).collect() })
}

/// Cancela una alerta activa del cliente. Repetirlo devuelve la misma.
pub async fn cancelar(pool: &PgPool, cliente_id: Uuid, id: Uuid) -> AppResult<VistaAlerta> {
    db::con_reintentos(|| cancelar_tx(pool, cliente_id, id)).await
}

async fn cancelar_tx(pool: &PgPool, cliente_id: Uuid, id: Uuid) -> AppResult<VistaAlerta> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    let dueno: Option<Uuid> = sqlx::query_scalar("SELECT cliente_id FROM mercado.alertas_precio WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    // La de otro cliente no existe para este.
    if dueno != Some(cliente_id) {
        return Err(no_existe());
    }
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, cliente_id).await?;
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let a = bloquear(&mut tx, id).await?;
    let cancelada = match a.estado {
        EstadoAlerta::Cancelada => a,
        EstadoAlerta::Disparada => {
            return Err(AppError::conflicto("AVISO_DISPARADO", "el aviso de precio ya se disparó"));
        }
        EstadoAlerta::Activa => {
            sqlx::query_as("UPDATE mercado.alertas_precio SET estado = 'CANCELADA' WHERE id = $1 RETURNING *")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    tx.commit().await?;
    Ok(VistaAlerta::from(&cancelada))
}

fn no_existe() -> AppError {
    AppError::no_encontrado("el aviso de precio no existe")
}

/// La alerta, bloqueada hasta el COMMIT. Después del candado de la cuenta.
async fn bloquear(tx: &mut Tx, id: Uuid) -> AppResult<Alerta> {
    sqlx::query_as("SELECT * FROM mercado.alertas_precio WHERE id = $1 FOR NO KEY UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(no_existe)
}

/// Una pasada del motor sobre las alertas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResumenAlertas {
    /// Clientes con alguna alerta que el precio vigente alcanzó.
    pub clientes: usize,
    pub disparadas: usize,
    pub errores: usize,
}

/// La condición de disparo sobre `a` (la alerta) y `act` (su activo), como la
/// revisa la base de datos al dispararla. `$1`: MODO_DEMO.
const DISPARABLE: &str = "a.estado = 'ACTIVA' AND act.ticker = a.ticker \
    AND (act.fuente_precio <> 'SIMULADO' OR $1) \
    AND CASE WHEN a.direccion = 'ARRIBA' THEN act.precio >= a.precio_objetivo \
             ELSE act.precio <= a.precio_objetivo END";

/// Dispara cada alerta activa que el precio vigente alcanzó (solo de activos
/// que se ofrecen), cliente por cliente.
pub async fn evaluar(pool: &PgPool, config: &Config) -> AppResult<ResumenAlertas> {
    let mut tx = db::begin_lectura(pool, Rol::Mercado).await?;
    let clientes: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT DISTINCT a.cliente_id FROM mercado.alertas_precio a, mercado.activos act WHERE {DISPARABLE} LIMIT $2"
    ))
    .bind(config.modo_demo)
    .bind(LOTE_CLIENTES)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut r = ResumenAlertas { clientes: clientes.len(), ..Default::default() };
    for cliente_id in clientes {
        match db::con_reintentos(|| disparar_de(pool, config, cliente_id)).await {
            Ok(n) => r.disparadas += n,
            Err(e) => {
                r.errores += 1;
                tracing::error!(cliente = %cliente_id, codigo = e.codigo(), error = %e.detalle(),
                    "no se pudieron disparar las alertas de precio de un cliente; siguen activas");
            }
        }
    }
    Ok(r)
}

/// Las alertas del cliente que el precio vigente alcanzó, disparadas en una
/// transacción con su cuenta bloqueada primero (la base de datos anota el
/// precio y sube la versión de la bóveda). Cuántas.
async fn disparar_de(pool: &PgPool, config: &Config, cliente_id: Uuid) -> AppResult<usize> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, cliente_id).await?;
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let disparadas: Vec<(Uuid, String)> = sqlx::query_as(&format!(
        "UPDATE mercado.alertas_precio a SET estado = 'DISPARADA' FROM mercado.activos act \
          WHERE a.cliente_id = $2 AND {DISPARABLE} RETURNING a.id, a.ticker"
    ))
    .bind(config.modo_demo)
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for (id, ticker) in &disparadas {
        tracing::info!(alerta = %id, ticker = %ticker, "alerta de precio disparada");
    }
    Ok(disparadas.len())
}

#[cfg(test)]
mod pruebas {
    use std::str::FromStr;

    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn el_precio_objetivo() {
        assert_eq!(validar_objetivo(d("1700")).unwrap().to_string(), "1700.00000000");
        assert_eq!(validar_objetivo(d("9.9439")).unwrap().to_string(), "9.94390000");
        assert!(validar_objetivo(d("0")).is_err());
        assert!(validar_objetivo(d("-1")).is_err());
        assert!(validar_objetivo(d("1.123456789")).is_err(), "9 decimales");
        assert!(validar_objetivo(d("10000000000")).is_err(), "no cabe en NUMERIC(18, 8)");
    }

    #[test]
    fn la_direccion_en_el_contrato() {
        assert_eq!(serde_json::from_str::<Direccion>("\"above\"").unwrap(), Direccion::Arriba);
        assert_eq!(serde_json::to_string(&Direccion::Abajo).unwrap(), "\"below\"");
        assert!(serde_json::from_str::<Direccion>("\"ARRIBA\"").is_err());
        assert!(serde_json::from_str::<Direccion>("\"cross\"").is_err());
    }
}
