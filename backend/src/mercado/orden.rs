//! Una orden de mercado (rol `sc_mercado`): se ejecuta completa al precio
//! vigente, con su efectivo, en una sola transacción.
//!
//! 1. Cuenta IFPE del cliente, `FOR NO KEY UPDATE`: el primer candado del
//!    orden global (encabezado de [`crate::ledger`]). Toda orden del cliente
//!    lo toma, así que su posición no cambia hasta el COMMIT.
//! 2. Idempotencia, ya con el candado: la repetición de una orden espera a la
//!    primera y devuelve su recibo; una clave usada para otra cosa (también
//!    una orden límite o stop) es 409.
//! 3. Reglas: KYC operable, el activo abierto y con precio real (o MODO_DEMO);
//!    compra: saldo disponible para el monto; venta: los títulos libres (los
//!    comprometidos en ventas límite o stop pendientes no se venden a mercado).
//! 4. Compra: PLD antes de cualquier escritura, en el canal COMPRA_MERCADO
//!    (como la inversión en mutuo); un BLOQUEAR confirma solo la evaluación.
//!    Venta: no se evalúa (dinero del cliente que regresa, como un mutuo
//!    vencido), pero un cliente congelado no vende.
//! 5. [`escribir_orden`]: reserva en custodia, saldos y el movimiento IFPE
//!    (COMPRA_MERCADO o VENTA_MERCADO); luego la orden, que la base de datos
//!    revisa contra el catálogo y la posición, encadena y aplica a la
//!    posición. Ambos hashes se recalculan aquí. La ejecución de una orden
//!    límite o stop ([`super::pendiente`]) escribe con la misma función.
//!
//! El cambio de saldo sube la versión de la cuenta en esta transacción: al
//! confirmar, `sc_boveda` avisa y el WebSocket envía la bóveda nueva.

use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    calculo::{self, Ejecucion, Rechazo},
    exigir_precio_real_o_demo, leer_activo, pendiente, ticker_valido, Activo, Lado, Orden, Recibo,
};
use crate::{
    aml::{self, Canal, Contraparte, Decision, Operacion},
    config::Config,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    ledger::{self, hash, NuevaTxIfpe},
    models::ifpe::TipoTxIfpe,
    pld,
};

/// La orden que pide el cliente.
#[derive(Debug, Clone)]
pub struct SolicitudOrden {
    pub cliente_id: Uuid,
    pub ticker: String,
    pub lado: Lado,
    /// Pesos con centavos: cuánto invertir (compra) o cuánto vender.
    pub monto: Decimal,
    /// Clave ya acotada al cliente (`Usuario::clave`).
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone)]
pub struct ResultadoOrden {
    pub recibo: Recibo,
    /// `true` si la clave ya existía y se devolvió la orden original.
    pub idempotente: bool,
}

/// El máximo de una orden, como el del frontend (`frontend/src/lib/market.ts`):
/// 999 999 999.99 MXN.
fn maximo() -> Decimal {
    Decimal::new(99_999_999_999, 2)
}

/// Pesos con centavos, de 0.01 al máximo de una orden (también las límite y stop).
pub(super) fn validar_monto(monto: Decimal) -> AppResult<Decimal> {
    let monto = dinero::validar_monto(monto)?;
    if monto.normalize().scale() > 2 {
        return Err(AppError::validacion("el monto admite como máximo 2 decimales (centavos)"));
    }
    if monto > maximo() {
        return Err(AppError::validacion("el monto excede el máximo de una orden (999,999,999.99)"));
    }
    Ok(monto)
}

pub async fn colocar(pool: &PgPool, config: &Config, s: &SolicitudOrden) -> AppResult<ResultadoOrden> {
    if !ticker_valido(&s.ticker) {
        return Err(AppError::no_encontrado(format!("el activo {} no existe", s.ticker)));
    }
    let monto = validar_monto(s.monto)?;
    // Un precio que cambia bajo la orden (SC_MERCADO_PRECIO_CAMBIO) también es
    // Reintentable: se vuelve a calcular todo con el precio nuevo.
    db::con_reintentos(|| colocar_tx(pool, config, s, monto)).await
}

async fn colocar_tx(pool: &PgPool, config: &Config, s: &SolicitudOrden, monto: Decimal) -> AppResult<ResultadoOrden> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;

    // 1. Cuenta IFPE del cliente (primer candado).
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    let cuenta = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;

    // 2. Idempotencia.
    if let Some(previa) = orden_por_clave(&mut tx, &s.clave_idempotencia).await? {
        if previa.cliente_id != s.cliente_id
            || previa.ticker != s.ticker
            || previa.lado != s.lado
            || previa.monto_solicitado != monto
        {
            return Err(clave_reutilizada());
        }
        let recibo = Recibo::from(&previa);
        tx.commit().await?;
        return Ok(ResultadoOrden { recibo, idempotente: true });
    }
    if ledger::buscar_por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
        || pendiente::por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
    {
        return Err(clave_reutilizada());
    }

    // 3. Reglas de negocio.
    pld::cargar_perfil(&mut tx, s.cliente_id).await?.exigir_operable()?;
    let activo = leer_activo(&mut tx, &s.ticker).await?;
    exigir_precio_real_o_demo(&activo, config)?;
    if !activo.operable {
        return Err(mercado_cerrado(&activo));
    }

    let orden_id = Uuid::new_v4();
    let tx_ifpe_id = Uuid::new_v4();
    let ejecucion = match s.lado {
        Lado::Compra => {
            // Deja pasar el congelamiento preventivo: PLD registra el intento y bloquea.
            ledger::exigir_debitable_salvo_aml(&cuenta)?;
            ledger::exigir_saldo(&cuenta, monto)?;
            let e = calculo::compra(monto, activo.precio, activo.decimales()).map_err(|r| rechazo(r, &activo))?;
            // 4. PLD antes de cualquier escritura.
            let d = aml::evaluar(
                &mut tx,
                &Operacion {
                    cliente_id: s.cliente_id,
                    canal: Canal::CompraMercado,
                    monto: e.monto,
                    transaccion_id: tx_ifpe_id,
                    clave: &s.clave_idempotencia,
                    contraparte: Contraparte::default(),
                },
            )
            .await?;
            if d.decision == Decision::Bloquear {
                return aml::confirmar_bloqueo(tx, &d).await;
            }
            e
        }
        Lado::Venta => {
            // Congelado (preventivo u operador) o cerrada: no vende. El
            // preventivo responde el error constante, sin aviso.
            ledger::exigir_debitable(&cuenta)?;
            let t = tenencia(&mut tx, s.cliente_id, &activo.ticker).await?;
            calculo::venta(monto, activo.precio, activo.decimales(), t.libre(), t.cantidad, t.costo)
                .map_err(|r| rechazo(r, &activo))?
        }
    };

    // 5. Reserva, saldos, movimiento IFPE y la orden.
    let orden = escribir_orden(
        &mut tx,
        NuevaOrden {
            id: orden_id,
            tx_ifpe_id,
            cliente_id: s.cliente_id,
            cuenta_id: cuenta.id,
            activo: &activo,
            lado: s.lado,
            monto_solicitado: monto,
            ejecucion,
            clave: &s.clave_idempotencia,
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(orden = %orden.id, ticker = %orden.ticker, lado = orden.lado.as_str(), "orden de mercado ejecutada");
    Ok(ResultadoOrden { recibo: Recibo::from(&orden), idempotente: false })
}

/// Lo que escribe la ejecución de una orden, ya decidida (cantidad, importe y
/// costo) y ya evaluada en PLD si compra.
pub(super) struct NuevaOrden<'a> {
    pub id: Uuid,
    pub tx_ifpe_id: Uuid,
    pub cliente_id: Uuid,
    pub cuenta_id: Uuid,
    pub activo: &'a Activo,
    pub lado: Lado,
    /// El monto que pidió el cliente (el de la orden pendiente, si ejecuta una).
    pub monto_solicitado: Decimal,
    pub ejecucion: Ejecucion,
    /// La clave de la orden y de su movimiento IFPE (la del cliente, o
    /// `ord:<pendiente>:ejecucion`).
    pub clave: &'a str,
}

/// Reserva, saldo, movimiento IFPE y orden: el efectivo sale de la custodia a
/// liquidar la compra, o regresa con la venta. El pasivo y la reserva se
/// mueven juntos (SC_RESERVA_100). La cuenta del cliente debe estar bloqueada
/// por el llamador (primer candado); aquí se toma la reserva.
pub(super) async fn escribir_orden(tx: &mut Tx, n: NuevaOrden<'_>) -> AppResult<Orden> {
    let reserva = ledger::bloquear_reserva(tx).await?;
    let (sql_cuenta, sql_reserva, tipo, concepto, origen, destino) = match n.lado {
        Lado::Compra => (
            "UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1 WHERE id = $2",
            "UPDATE ifpe.reserva_custodia SET saldo = saldo - $1 WHERE id = $2",
            TipoTxIfpe::CompraMercado,
            format!("Compra de {}", n.activo.ticker),
            Some(n.cuenta_id),
            None,
        ),
        Lado::Venta => (
            "UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $1 WHERE id = $2",
            "UPDATE ifpe.reserva_custodia SET saldo = saldo + $1 WHERE id = $2",
            TipoTxIfpe::VentaMercado,
            format!("Venta de {}", n.activo.ticker),
            None,
            Some(n.cuenta_id),
        ),
    };
    sqlx::query(sql_cuenta).bind(n.ejecucion.monto).bind(n.cuenta_id).execute(&mut **tx).await?;
    sqlx::query(sql_reserva).bind(n.ejecucion.monto).bind(reserva.id).execute(&mut **tx).await?;
    let movimiento = ledger::insertar_tx_ifpe(
        tx,
        NuevaTxIfpe {
            id: n.tx_ifpe_id,
            tipo,
            origen,
            destino,
            reserva: Some(reserva.id),
            monto: n.ejecucion.monto,
            concepto: Some(&concepto),
            referencia_externa: None,
            clave: n.clave,
            espejo: Some(n.id),
        },
    )
    .await?;

    // La orden: la base de datos la revisa, la encadena y mueve la posición.
    let orden: Orden = sqlx::query_as(
        "INSERT INTO mercado.ordenes \
         (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad, precio, monto, costo_liberado, \
          fuente_precio, tx_ifpe_id, clave_idempotencia) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING *",
    )
    .bind(n.id)
    .bind(n.cliente_id)
    .bind(n.cuenta_id)
    .bind(&n.activo.ticker)
    .bind(n.lado)
    .bind(n.monto_solicitado)
    .bind(n.ejecucion.cantidad)
    .bind(n.activo.precio)
    .bind(n.ejecucion.monto)
    .bind(n.ejecucion.costo_liberado)
    .bind(n.activo.fuente_precio)
    .bind(movimiento.id)
    .bind(n.clave)
    .fetch_one(&mut **tx)
    .await?;
    let recalculado = hash::hash_mercado(&orden);
    if recalculado != orden.tx_hash {
        return Err(AppError::Integridad(format!(
            "MERCADO secuencia {}: tx_hash de la base de datos {} != recalculado {}",
            orden.secuencia, orden.tx_hash, recalculado
        )));
    }
    Ok(orden)
}

pub(super) async fn orden_por_clave(tx: &mut Tx, clave: &str) -> AppResult<Option<Orden>> {
    Ok(sqlx::query_as("SELECT * FROM mercado.ordenes WHERE clave_idempotencia = $1")
        .bind(clave)
        .fetch_optional(&mut **tx)
        .await?)
}

/// La posición del cliente en un activo: todos sus títulos, los comprometidos
/// en ventas pendientes y su costo (ceros si no tiene).
#[derive(Debug, Clone, Copy, Default, sqlx::FromRow)]
pub(super) struct Tenencia {
    pub cantidad: Decimal,
    pub cantidad_comprometida: Decimal,
    pub costo: Decimal,
}

impl Tenencia {
    /// Los títulos que una venta de mercado (o una venta pendiente nueva) puede usar.
    pub fn libre(&self) -> Decimal {
        self.cantidad - self.cantidad_comprometida
    }
}

/// Sin candado propio: toda orden del cliente toma antes su fila IFPE, que
/// esta transacción ya tiene.
pub(super) async fn tenencia(tx: &mut Tx, cliente_id: Uuid, ticker: &str) -> AppResult<Tenencia> {
    let fila: Option<Tenencia> = sqlx::query_as(
        "SELECT cantidad, cantidad_comprometida, costo FROM mercado.posiciones WHERE cliente_id = $1 AND ticker = $2",
    )
    .bind(cliente_id)
    .bind(ticker)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(fila.unwrap_or_default())
}

pub(super) fn clave_reutilizada() -> AppError {
    AppError::conflicto(
        "CLAVE_IDEMPOTENCIA_REUTILIZADA",
        "la clave de idempotencia ya se usó para una operación distinta",
    )
}

pub(super) fn mercado_cerrado(activo: &Activo) -> AppError {
    AppError::negocio("MERCADO_CERRADO", format!("{} no opera en este momento", activo.ticker))
}

pub(super) fn rechazo(r: Rechazo, activo: &Activo) -> AppError {
    match r {
        Rechazo::MontoInsuficiente => AppError::negocio(
            "MONTO_INSUFICIENTE",
            format!(
                "el monto no alcanza para el incremento mínimo de {} ({} al precio de la orden)",
                activo.ticker,
                Decimal::new(1, activo.decimales())
            ),
        ),
        Rechazo::SinPosicion => {
            AppError::negocio("POSICION_INSUFICIENTE", format!("no tienes {} que vender", activo.ticker))
        }
        Rechazo::TitulosComprometidos => AppError::negocio(
            "POSICION_INSUFICIENTE",
            format!("tus títulos de {} están comprometidos en órdenes pendientes", activo.ticker),
        ),
        Rechazo::PosicionInsuficiente => AppError::negocio(
            "POSICION_INSUFICIENTE",
            format!("el monto excede lo que valen tus títulos libres de {}", activo.ticker),
        ),
        Rechazo::FueraDeRango => AppError::validacion("el monto está fuera del rango permitido"),
    }
}

#[cfg(test)]
mod pruebas {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn el_monto_es_de_pesos_con_centavos() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        assert_eq!(validar_monto(d("1250")).unwrap().to_string(), "1250.00000000");
        assert_eq!(validar_monto(d("0.01")).unwrap().to_string(), "0.01000000");
        assert!(validar_monto(d("1250.005")).is_err(), "fracciones de centavo");
        assert!(validar_monto(d("0")).is_err());
        assert!(validar_monto(d("-5")).is_err());
        assert!(validar_monto(d("999999999.99")).is_ok());
        assert!(validar_monto(d("1000000000")).is_err(), "más que el máximo de una orden");
    }
}
