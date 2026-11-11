//! Órdenes límite y stop (migración 0015): esperan a su precio con lo que
//! usarían comprometido, y el motor de disparo las ejecuta al precio vigente.
//!
//! **Colocar** ([`colocar`], `POST /api/v1/market/order` con `order_type`
//! `limit` o `stop`), rol `sc_mercado`, una transacción:
//! 1. La cuenta IFPE del cliente `FOR NO KEY UPDATE`, el primer candado de
//!    toda orden ([`crate::ledger`]); la idempotencia después, con el
//!    candado: la repetición devuelve la orden en su estado actual y una clave
//!    usada para otra cosa (otra orden, de mercado o pendiente) es 409.
//! 2. Reglas: KYC operable, cuenta activa, activo abierto con precio real (o
//!    MODO_DEMO), un stop que el precio aún no alcanza y a lo más
//!    [`MAXIMO_PENDIENTES`] órdenes abiertas por cliente.
//! 3. Compra: todo el monto pasa del disponible al comprometido con un
//!    COMPROMISO_ORDEN del ledger IFPE (`ord:<orden>:compromiso`); debe
//!    alcanzar para un incremento al precio de la orden. Venta: los títulos
//!    que el monto vende al precio de la orden, de los libres (como una venta
//!    de mercado), que la base de datos compromete en la posición. Nada sale
//!    de la custodia: el pasivo y la reserva del 100 % no cambian.
//! 4. La orden nace PENDIENTE. Una orden límite que ya se puede ejecutar
//!    (comprar a 1 600 o menos con el precio en 1 590) se ejecuta en seguida,
//!    en su propia transacción y como la ejecutaría el motor: la respuesta
//!    trae su estado final.
//!
//! **Ejecutar** ([`ejecutar`]; [`disparar`] recorre las disparadas, de la más
//! antigua a la más nueva): con la cuenta y la orden bloqueadas, y solo si
//! sigue pendiente, su activo opera y el precio vigente la dispara (una
//! límite, a su precio o mejor; una stop, alcanzado su precio). Compra: PLD
//! antes de escribir, como una orden de mercado; luego la orden se cierra
//! EJECUTADA, su efectivo regresa al disponible (LIBERACION_ORDEN) y se
//! compra con todo él al precio vigente: el resto queda disponible. Venta: se
//! cierra (sus títulos se liberan) y vende exactamente esos títulos. La orden
//! del ledger del mercado lleva la clave `ord:<orden>:ejecucion` y la base de
//! datos la revisa contra los términos de la pendiente. Si una regla la
//! rechaza (KYC, cuenta congelada, PLD, un monto que ya no alcanza un
//! incremento), la orden se cierra RECHAZADA con el código y lo comprometido
//! regresa. Un error transitorio la deja pendiente para la siguiente pasada.
//!
//! **Cancelar** ([`cancelar`]): solo su cliente y solo pendiente; lo
//! comprometido regresa (también con la cuenta congelada: devolver siempre
//! se puede). Cancelar dos veces devuelve la misma orden cancelada.
//!
//! Varias réplicas del backend pueden disparar a la vez: el candado de la
//! cuenta y el de la orden las serializan, y la segunda la encuentra cerrada.

use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    calculo, exigir_precio_real_o_demo, leer_activo,
    modelos::{EstadoPendiente, Lado, Orden, OrdenPendiente, TipoPendiente, VistaPendiente},
    orden::{self, NuevaOrden},
    se_ofrece, ticker_valido,
};
use crate::{
    aml::{self, Canal, Contraparte, Decision, Operacion},
    config::Config,
    db::{self, Rol, Tx},
    dinero,
    error::{AppError, AppResult},
    ledger::{self, NuevaTxIfpe},
    models::ifpe::TipoTxIfpe,
    pld,
};

/// Órdenes abiertas por cliente: el motor las revisa en cada pasada.
pub const MAXIMO_PENDIENTES: i64 = 50;

/// Órdenes disparadas que ejecuta una pasada del motor (la siguiente sigue).
const LOTE_DISPARO: i64 = 500;

/// La orden límite o stop que pide el cliente.
#[derive(Debug, Clone)]
pub struct SolicitudPendiente {
    pub cliente_id: Uuid,
    pub ticker: String,
    pub lado: Lado,
    pub tipo: TipoPendiente,
    /// Pesos con centavos: cuánto invertir (compra) o cuánto vender.
    pub monto: Decimal,
    /// Su precio límite o su precio stop.
    pub precio: Decimal,
    /// Clave ya acotada al cliente (`Usuario::clave`).
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone)]
pub struct ResultadoPendiente {
    pub orden: VistaPendiente,
    /// `true` si la clave ya existía y se devolvió la orden original.
    pub idempotente: bool,
}

/// Lo que hizo un intento de ejecución.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desenlace {
    Ejecutada,
    /// Cerrada por una regla, con su código; lo comprometido regresó.
    Rechazada(String),
    /// Nada: ya estaba cerrada, el precio no la dispara o su activo no opera.
    SinCambio,
}

/// Una pasada del motor de disparo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResumenDisparo {
    pub revisadas: usize,
    pub ejecutadas: usize,
    pub rechazadas: usize,
    pub errores: usize,
}

/// Coloca una orden límite o stop; si ya se puede ejecutar, la ejecuta.
pub async fn colocar(pool: &PgPool, config: &Config, s: &SolicitudPendiente) -> AppResult<ResultadoPendiente> {
    if !ticker_valido(&s.ticker) {
        return Err(AppError::no_encontrado(format!("el activo {} no existe", s.ticker)));
    }
    let monto = orden::validar_monto(s.monto)?;
    let precio = validar_precio(s.precio, s.tipo)?;
    let (p, idempotente, vigente) = db::con_reintentos(|| colocar_tx(pool, config, s, monto, precio)).await?;
    if !idempotente && p.disparada(vigente) {
        // Ya colocada (y confirmada): si la ejecución falla por algo
        // transitorio, la orden queda pendiente y el motor la toma.
        if let Err(e) = ejecutar(pool, config, p.id).await {
            tracing::warn!(orden = %p.id, codigo = e.codigo(), error = %e.detalle(),
                "la orden límite no se ejecutó al colocarse; queda pendiente para el motor");
        }
    }
    Ok(ResultadoPendiente { orden: leer(pool, p.id).await?, idempotente })
}

/// La orden colocada (o la previa con esa clave), si se repitió, y el precio
/// vigente con el que se colocó.
async fn colocar_tx(
    pool: &PgPool,
    config: &Config,
    s: &SolicitudPendiente,
    monto: Decimal,
    precio: Decimal,
) -> AppResult<(OrdenPendiente, bool, Decimal)> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;

    // 1. Cuenta IFPE del cliente (primer candado) e idempotencia.
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    let cuenta = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    if let Some(previa) = por_clave(&mut tx, &s.clave_idempotencia).await? {
        if previa.cliente_id != s.cliente_id
            || previa.ticker != s.ticker
            || previa.lado != s.lado
            || previa.tipo != s.tipo
            || previa.monto != monto
            || previa.precio() != precio
        {
            return Err(orden::clave_reutilizada());
        }
        tx.commit().await?;
        return Ok((previa, true, Decimal::ZERO));
    }
    if orden::orden_por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
        || ledger::buscar_por_clave(&mut tx, &s.clave_idempotencia).await?.is_some()
    {
        return Err(orden::clave_reutilizada());
    }

    // 2. Reglas de negocio. Congelada (preventivo u operador) o cerrada: no
    // coloca; el preventivo responde el error constante, sin aviso.
    pld::cargar_perfil(&mut tx, s.cliente_id).await?.exigir_operable()?;
    ledger::exigir_debitable(&cuenta)?;
    let activo = leer_activo(&mut tx, &s.ticker).await?;
    exigir_precio_real_o_demo(&activo, config)?;
    if !activo.operable {
        return Err(orden::mercado_cerrado(&activo));
    }
    if s.tipo == TipoPendiente::Stop && super::modelos::disparada(s.tipo, s.lado, precio, activo.precio) {
        return Err(AppError::negocio(
            "PRECIO_STOP_INVALIDO",
            match s.lado {
                Lado::Compra => format!(
                    "un stop de compra va por encima del precio vigente de {} ({})",
                    activo.ticker,
                    dinero::con_escala(activo.precio)
                ),
                Lado::Venta => format!(
                    "un stop de venta va por debajo del precio vigente de {} ({})",
                    activo.ticker,
                    dinero::con_escala(activo.precio)
                ),
            },
        ));
    }
    let abiertas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mercado.ordenes_pendientes WHERE cliente_id = $1 AND estado = 'PENDIENTE'",
    )
    .bind(s.cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    if abiertas >= MAXIMO_PENDIENTES {
        return Err(AppError::negocio(
            "LIMITE_ORDENES_PENDIENTES",
            format!("ya tienes {MAXIMO_PENDIENTES} órdenes pendientes; cancela alguna para colocar otra"),
        ));
    }

    // 3. Lo comprometido y la orden.
    let id = Uuid::new_v4();
    let p = match s.lado {
        Lado::Compra => {
            ledger::exigir_saldo(&cuenta, monto)?;
            // Al precio de la orden, el monto debe comprar al menos un incremento.
            calculo::compra(monto, precio, activo.decimales()).map_err(|r| orden::rechazo(r, &activo))?;
            let tx_compromiso = Uuid::new_v4();
            let p =
                insertar(&mut tx, id, s, cuenta.id, monto, precio, monto, Decimal::ZERO, Some(tx_compromiso)).await?;
            sqlx::query(
                "UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - $1, \
                 saldo_comprometido = saldo_comprometido + $1 WHERE id = $2",
            )
            .bind(monto)
            .bind(cuenta.id)
            .execute(&mut *tx)
            .await?;
            let concepto = format!("Orden {} de compra de {}", nombre_tipo(s.tipo), activo.ticker);
            ledger::insertar_tx_ifpe(
                &mut tx,
                NuevaTxIfpe {
                    id: tx_compromiso,
                    tipo: TipoTxIfpe::CompromisoOrden,
                    origen: Some(cuenta.id),
                    destino: Some(cuenta.id),
                    reserva: None,
                    monto,
                    concepto: Some(&concepto),
                    referencia_externa: None,
                    clave: &format!("ord:{id}:compromiso"),
                    espejo: Some(id),
                },
            )
            .await?;
            p
        }
        Lado::Venta => {
            // Los títulos que el monto vende al precio de la orden, de los libres.
            let t = orden::tenencia(&mut tx, s.cliente_id, &activo.ticker).await?;
            let e = calculo::venta(monto, precio, activo.decimales(), t.libre(), t.cantidad, t.costo)
                .map_err(|r| orden::rechazo(r, &activo))?;
            insertar(&mut tx, id, s, cuenta.id, monto, precio, Decimal::ZERO, e.cantidad, None).await?
        }
    };
    tx.commit().await?;
    tracing::info!(orden = %p.id, ticker = %p.ticker, lado = p.lado.as_str(), tipo = p.tipo.as_str(),
        "orden pendiente colocada");
    Ok((p, false, activo.precio))
}

#[allow(clippy::too_many_arguments)]
async fn insertar(
    tx: &mut Tx,
    id: Uuid,
    s: &SolicitudPendiente,
    cuenta_id: Uuid,
    monto: Decimal,
    precio: Decimal,
    efectivo: Decimal,
    titulos: Decimal,
    tx_compromiso: Option<Uuid>,
) -> AppResult<OrdenPendiente> {
    let (limite, stop) = match s.tipo {
        TipoPendiente::Limite => (Some(precio), None),
        TipoPendiente::Stop => (None, Some(precio)),
    };
    Ok(sqlx::query_as(
        "INSERT INTO mercado.ordenes_pendientes \
         (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite, precio_stop, \
          efectivo_comprometido, titulos_comprometidos, tx_compromiso_id, clave_idempotencia) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING *",
    )
    .bind(id)
    .bind(s.cliente_id)
    .bind(cuenta_id)
    .bind(&s.ticker)
    .bind(s.lado)
    .bind(s.tipo)
    .bind(monto)
    .bind(limite)
    .bind(stop)
    .bind(efectivo)
    .bind(titulos)
    .bind(tx_compromiso)
    .bind(&s.clave_idempotencia)
    .fetch_one(&mut **tx)
    .await?)
}

/// Ejecuta la orden si sigue pendiente y el precio vigente la dispara; si una
/// regla lo impide, la rechaza y devuelve lo comprometido. Un error
/// transitorio (o de integridad) la deja pendiente y se devuelve.
pub async fn ejecutar(pool: &PgPool, config: &Config, id: Uuid) -> AppResult<Desenlace> {
    match db::con_reintentos(|| ejecutar_tx(pool, config, id)).await {
        Err(e) if rechaza(&e) => {
            let motivo = motivo_de(&e);
            let cierre = Cierre::Rechazo(&motivo);
            Ok(match db::con_reintentos(|| cerrar_tx(pool, id, None, &cierre)).await? {
                Some(_) => {
                    tracing::warn!(orden = %id, motivo = %motivo, "orden pendiente rechazada al dispararse");
                    Desenlace::Rechazada(motivo)
                }
                // Otra petición la cerró primero.
                None => Desenlace::SinCambio,
            })
        }
        otro => otro,
    }
}

/// Una regla de negocio (no un error transitorio ni de integridad) que
/// impide ejecutarla: la orden se rechaza. Un activo suspendido no la
/// rechaza: la orden espera a que vuelva a operar.
fn rechaza(e: &AppError) -> bool {
    match e {
        AppError::OperacionNoDisponible => true,
        AppError::Negocio { codigo, .. } => codigo != "MERCADO_CERRADO",
        _ => false,
    }
}

/// El código del error, como lo acepta `mercado.ordenes_pendientes.motivo`.
fn motivo_de(e: &AppError) -> String {
    let codigo = e.codigo();
    let valido = codigo.len() <= 64
        && codigo.starts_with(|c: char| c.is_ascii_uppercase())
        && codigo.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if valido {
        codigo.to_string()
    } else {
        "RECHAZADA".to_string()
    }
}

async fn ejecutar_tx(pool: &PgPool, config: &Config, id: Uuid) -> AppResult<Desenlace> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    // La cuenta primero (orden global de candados), luego la orden.
    let cuenta_id: Option<Uuid> =
        sqlx::query_scalar("SELECT cuenta_ifpe_id FROM mercado.ordenes_pendientes WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(cuenta_id) = cuenta_id else { return Ok(Desenlace::SinCambio) };
    let cuenta = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let p = bloquear(&mut tx, id).await?;
    if p.estado != EstadoPendiente::Pendiente {
        tx.commit().await?;
        return Ok(Desenlace::SinCambio);
    }
    let activo = leer_activo(&mut tx, &p.ticker).await?;
    if !se_ofrece(&activo, config) || !activo.operable || !p.disparada(activo.precio) {
        tx.commit().await?;
        return Ok(Desenlace::SinCambio);
    }

    // Reglas de quien opera, como en una orden de mercado: un error aquí la rechaza.
    pld::cargar_perfil(&mut tx, p.cliente_id).await?.exigir_operable()?;
    ledger::exigir_debitable(&cuenta)?;

    let orden_id = Uuid::new_v4();
    let tx_ifpe_id = Uuid::new_v4();
    let clave = format!("ord:{}:ejecucion", p.id);
    let ejecucion = match p.lado {
        Lado::Compra => {
            // Todo el efectivo comprometido, al precio vigente.
            let e =
                calculo::compra(p.monto, activo.precio, activo.decimales()).map_err(|r| orden::rechazo(r, &activo))?;
            // PLD antes de cualquier escritura; un BLOQUEAR confirma solo la evaluación.
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
            e
        }
        Lado::Venta => {
            let t = orden::tenencia(&mut tx, p.cliente_id, &p.ticker).await?;
            calculo::venta_de_titulos(p.titulos_comprometidos, activo.precio, t.cantidad, t.costo)
                .map_err(|r| orden::rechazo(r, &activo))?
        }
    };

    // La reserva en custodia antes que cualquier escritura: la liberación del
    // efectivo inserta en el ledger IFPE (cuya cabeza va después de la
    // reserva en el orden global de candados, `crate::ledger`), y
    // `escribir_orden` la vuelve a tomar sin esperar.
    ledger::bloquear_reserva(&mut tx).await?;

    // La pendiente se cierra antes que la orden que la ejecuta: la base de
    // datos la busca por `orden_ejecutada_id` (llave diferida) y le exige su
    // precio y sus títulos. Cerrar una venta libera sus títulos.
    let tx_liberacion = (p.lado == Lado::Compra).then(Uuid::new_v4);
    sqlx::query(
        "UPDATE mercado.ordenes_pendientes SET estado = 'EJECUTADA', tx_liberacion_id = $2, orden_ejecutada_id = $3 \
         WHERE id = $1",
    )
    .bind(p.id)
    .bind(tx_liberacion)
    .bind(orden_id)
    .execute(&mut *tx)
    .await?;
    if let Some(t) = tx_liberacion {
        liberar_efectivo(&mut tx, &p, t, "ejecutada").await?;
    }
    let ejecutada = orden::escribir_orden(
        &mut tx,
        NuevaOrden {
            id: orden_id,
            tx_ifpe_id,
            cliente_id: p.cliente_id,
            cuenta_id: cuenta.id,
            activo: &activo,
            lado: p.lado,
            monto_solicitado: p.monto,
            ejecucion,
            clave: &clave,
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(orden = %p.id, ejecutada = %ejecutada.id, ticker = %p.ticker, lado = p.lado.as_str(),
        precio = %ejecutada.precio, "orden pendiente ejecutada");
    Ok(Desenlace::Ejecutada)
}

/// Una pasada del motor: ejecuta (o rechaza) las órdenes que el precio
/// vigente dispara, de la más antigua a la más nueva. Solo las de activos que
/// operan y se ofrecen (precio real, o simulado con MODO_DEMO).
pub async fn disparar(pool: &PgPool, config: &Config) -> AppResult<ResumenDisparo> {
    let mut tx = db::begin_lectura(pool, Rol::Mercado).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM mercado.ordenes_pendientes p JOIN mercado.activos a ON a.ticker = p.ticker \
          WHERE p.estado = 'PENDIENTE' AND a.operable AND (a.fuente_precio <> 'SIMULADO' OR $1) \
            AND CASE WHEN p.tipo = 'LIMITE' AND p.lado = 'COMPRA' THEN a.precio <= p.precio_limite \
                     WHEN p.tipo = 'LIMITE' THEN a.precio >= p.precio_limite \
                     WHEN p.lado = 'COMPRA' THEN a.precio >= p.precio_stop \
                     ELSE a.precio <= p.precio_stop END \
          ORDER BY p.creado_en, p.id LIMIT $2",
    )
    .bind(config.modo_demo)
    .bind(LOTE_DISPARO)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut r = ResumenDisparo::default();
    for id in ids {
        r.revisadas += 1;
        match ejecutar(pool, config, id).await {
            Ok(Desenlace::Ejecutada) => r.ejecutadas += 1,
            Ok(Desenlace::Rechazada(_)) => r.rechazadas += 1,
            Ok(Desenlace::SinCambio) => {}
            Err(e) => {
                r.errores += 1;
                tracing::error!(orden = %id, codigo = e.codigo(), error = %e.detalle(),
                    "no se pudo ejecutar una orden disparada; sigue pendiente");
            }
        }
    }
    Ok(r)
}

/// Cancela una orden pendiente del cliente y devuelve lo comprometido.
/// Repetirla devuelve la misma orden cancelada; una ya ejecutada o rechazada
/// es 409 `ORDEN_NO_PENDIENTE`; la de otro cliente no existe (404).
pub async fn cancelar(pool: &PgPool, cliente_id: Uuid, id: Uuid) -> AppResult<VistaPendiente> {
    let cierre = Cierre::Cancelacion;
    if db::con_reintentos(|| cerrar_tx(pool, id, Some(cliente_id), &cierre)).await?.is_none() {
        return Err(AppError::Interno(anyhow::anyhow!("la cancelación de {id} no devolvió la orden")));
    }
    leer(pool, id).await
}

/// Las órdenes límite y stop que esperan, de la más nueva a la más antigua.
/// Con el rol del cliente y RLS: solo las suyas.
pub async fn pendientes(pool: &PgPool, cliente_id: Uuid) -> AppResult<Vec<VistaPendiente>> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let filas: Vec<OrdenPendiente> = sqlx::query_as(
        "SELECT * FROM mercado.ordenes_pendientes WHERE cliente_id = $1 AND estado = 'PENDIENTE' \
         ORDER BY creado_en DESC, id DESC",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(filas.iter().map(|p| VistaPendiente::de(p, None)).collect())
}

/// La orden (y la que la ejecutó, si se ejecutó) como la lee el frontend.
async fn leer(pool: &PgPool, id: Uuid) -> AppResult<VistaPendiente> {
    let mut tx = db::begin_lectura(pool, Rol::Mercado).await?;
    let p: OrdenPendiente =
        sqlx::query_as("SELECT * FROM mercado.ordenes_pendientes WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
    let ejecucion: Option<Orden> = match p.orden_ejecutada_id {
        Some(orden_id) => Some(
            sqlx::query_as("SELECT * FROM mercado.ordenes WHERE id = $1").bind(orden_id).fetch_one(&mut *tx).await?,
        ),
        None => None,
    };
    tx.commit().await?;
    Ok(VistaPendiente::de(&p, ejecucion.as_ref()))
}

pub(super) async fn por_clave(tx: &mut Tx, clave: &str) -> AppResult<Option<OrdenPendiente>> {
    Ok(sqlx::query_as("SELECT * FROM mercado.ordenes_pendientes WHERE clave_idempotencia = $1")
        .bind(clave)
        .fetch_optional(&mut **tx)
        .await?)
}

/// La orden, bloqueada hasta el COMMIT. Después del candado de su cuenta.
async fn bloquear(tx: &mut Tx, id: Uuid) -> AppResult<OrdenPendiente> {
    sqlx::query_as("SELECT * FROM mercado.ordenes_pendientes WHERE id = $1 FOR NO KEY UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("la orden no existe"))
}

enum Cierre<'a> {
    Cancelacion,
    Rechazo(&'a str),
}

/// Cierra una orden pendiente (cancelada o rechazada) y devuelve su efectivo
/// comprometido; los títulos de una venta los libera la base de datos.
/// `cliente`: solo si es suya. `None` si ya estaba cerrada (salvo cancelar:
/// una cancelada se devuelve igual y cualquier otro estado es 409).
async fn cerrar_tx(
    pool: &PgPool,
    id: Uuid,
    cliente: Option<Uuid>,
    cierre: &Cierre<'_>,
) -> AppResult<Option<OrdenPendiente>> {
    let mut tx = db::begin(pool, Rol::Mercado).await?;
    let fila: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT cliente_id, cuenta_ifpe_id FROM mercado.ordenes_pendientes WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    // La orden de otro cliente no existe para este.
    let Some((_, cuenta_id)) = fila.filter(|(dueno, _)| cliente.map_or(true, |c| c == *dueno)) else {
        return Err(AppError::no_encontrado("la orden no existe"));
    };
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let p = bloquear(&mut tx, id).await?;
    if p.estado != EstadoPendiente::Pendiente {
        tx.commit().await?;
        return match cierre {
            Cierre::Cancelacion if p.estado == EstadoPendiente::Cancelada => Ok(Some(p)),
            Cierre::Cancelacion => Err(AppError::conflicto(
                "ORDEN_NO_PENDIENTE",
                match p.estado {
                    EstadoPendiente::Ejecutada => "la orden ya se ejecutó",
                    _ => "la orden ya se rechazó",
                },
            )),
            Cierre::Rechazo(_) => Ok(None),
        };
    }
    let (estado, motivo, como) = match cierre {
        Cierre::Cancelacion => (EstadoPendiente::Cancelada, None, "cancelada"),
        Cierre::Rechazo(m) => (EstadoPendiente::Rechazada, Some(*m), "rechazada"),
    };
    let tx_liberacion = (p.lado == Lado::Compra).then(Uuid::new_v4);
    let cerrada: OrdenPendiente = sqlx::query_as(
        "UPDATE mercado.ordenes_pendientes SET estado = $2, motivo = $3, tx_liberacion_id = $4 WHERE id = $1 \
         RETURNING *",
    )
    .bind(p.id)
    .bind(estado)
    .bind(motivo)
    .bind(tx_liberacion)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(t) = tx_liberacion {
        liberar_efectivo(&mut tx, &p, t, como).await?;
    }
    tx.commit().await?;
    tracing::info!(orden = %p.id, estado = estado.as_str(), "orden pendiente cerrada");
    Ok(Some(cerrada))
}

/// El efectivo de una compra que se cierra regresa al disponible, con su
/// LIBERACION_ORDEN. La cuenta ya está bloqueada.
async fn liberar_efectivo(tx: &mut Tx, p: &OrdenPendiente, tx_id: Uuid, como: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE ifpe.cuentas SET saldo_comprometido = saldo_comprometido - $1, \
         saldo_disponible = saldo_disponible + $1 WHERE id = $2",
    )
    .bind(p.efectivo_comprometido)
    .bind(p.cuenta_ifpe_id)
    .execute(&mut **tx)
    .await?;
    let concepto = format!("Orden {} de compra de {} {como}", nombre_tipo(p.tipo), p.ticker);
    ledger::insertar_tx_ifpe(
        tx,
        NuevaTxIfpe {
            id: tx_id,
            tipo: TipoTxIfpe::LiberacionOrden,
            origen: Some(p.cuenta_ifpe_id),
            destino: Some(p.cuenta_ifpe_id),
            reserva: None,
            monto: p.efectivo_comprometido,
            concepto: Some(&concepto),
            referencia_externa: None,
            clave: &format!("ord:{}:liberacion", p.id),
            espejo: Some(p.id),
        },
    )
    .await?;
    Ok(())
}

const fn nombre_tipo(tipo: TipoPendiente) -> &'static str {
    match tipo {
        TipoPendiente::Limite => "límite",
        TipoPendiente::Stop => "stop",
    }
}

/// Un precio límite o stop: positivo, con a lo más 8 decimales y dentro de
/// `NUMERIC(18, 8)`.
fn validar_precio(precio: Decimal, tipo: TipoPendiente) -> AppResult<Decimal> {
    let nombre = match tipo {
        TipoPendiente::Limite => "el precio límite",
        TipoPendiente::Stop => "el precio stop",
    };
    if precio <= Decimal::ZERO {
        return Err(AppError::validacion(format!("{nombre} debe ser mayor a cero")));
    }
    if precio.normalize().scale() > dinero::ESCALA {
        return Err(AppError::validacion(format!("{nombre} admite como máximo 8 decimales")));
    }
    if precio > dinero::maximo() {
        return Err(AppError::validacion(format!("{nombre} está fuera del rango permitido")));
    }
    Ok(dinero::con_escala(precio))
}

#[cfg(test)]
mod pruebas {
    use std::str::FromStr;

    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn el_precio_de_una_orden() {
        assert_eq!(validar_precio(d("1600"), TipoPendiente::Limite).unwrap().to_string(), "1600.00000000");
        assert_eq!(validar_precio(d("9.9439"), TipoPendiente::Stop).unwrap().to_string(), "9.94390000");
        assert!(validar_precio(d("0"), TipoPendiente::Limite).is_err());
        assert!(validar_precio(d("-1"), TipoPendiente::Stop).is_err());
        assert!(validar_precio(d("1.123456789"), TipoPendiente::Limite).is_err(), "9 decimales");
        assert!(validar_precio(d("10000000000"), TipoPendiente::Limite).is_err(), "no cabe en NUMERIC(18, 8)");
    }

    #[test]
    fn lo_que_dispara_cada_orden() {
        use super::super::modelos::disparada;
        let (limite, stop) = (TipoPendiente::Limite, TipoPendiente::Stop);
        // Límite: a su precio o mejor.
        assert!(disparada(limite, Lado::Compra, d("1600"), d("1590")));
        assert!(disparada(limite, Lado::Compra, d("1600"), d("1600")));
        assert!(!disparada(limite, Lado::Compra, d("1600"), d("1600.01")));
        assert!(disparada(limite, Lado::Venta, d("1700"), d("1710")));
        assert!(!disparada(limite, Lado::Venta, d("1700"), d("1699.99")));
        // Stop: alcanzado su precio.
        assert!(disparada(stop, Lado::Compra, d("1650"), d("1650")));
        assert!(!disparada(stop, Lado::Compra, d("1650"), d("1612.40")));
        assert!(disparada(stop, Lado::Venta, d("1550"), d("1540")));
        assert!(!disparada(stop, Lado::Venta, d("1550"), d("1590")));
    }

    #[test]
    fn que_rechaza_una_orden_disparada() {
        assert!(rechaza(&AppError::OperacionNoDisponible));
        assert!(rechaza(&AppError::negocio("MONTO_INSUFICIENTE", "x")));
        assert!(rechaza(&AppError::negocio("KYC_NO_VERIFICADO", "x")));
        assert!(!rechaza(&AppError::negocio("MERCADO_CERRADO", "x")), "espera a que el activo vuelva a operar");
        assert!(!rechaza(&AppError::Reintentable));
        assert!(!rechaza(&AppError::Integridad("x".into())));
        assert_eq!(motivo_de(&AppError::OperacionNoDisponible), "OPERACION_NO_DISPONIBLE");
        assert_eq!(motivo_de(&AppError::negocio("CUENTA_CONGELADA", "x")), "CUENTA_CONGELADA");
        assert_eq!(motivo_de(&AppError::negocio("raro: x", "x")), "RECHAZADA");
    }
}
