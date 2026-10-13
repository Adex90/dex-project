//! Proof of Reserve: cortes del árbol Merkle de sumas y pruebas de inclusión.
//!
//! Un corte se genera en una foto consistente (REPEATABLE READ) con el rol
//! `sc_auditor`: saldos de todas las cuentas IFPE, estado agregado de la
//! reserva, suma de la reserva en custodia y cabeza del ledger. Antes de
//! publicar se comprueba que
//!
//! `Σ hojas == pasivo_clientes == activo_reserva == Σ reserva_custodia`,
//!
//! y la raíz se ancla al último `tx_hash` del ledger (`hash_ledger`), de modo
//! que el corte prueba el estado exacto tras ese movimiento. Las hojas (con su
//! sal aleatoria) se guardan para emitir pruebas de inclusión después.
//!
//! **Privacidad.** Un árbol de sumas solo es verificable si la prueba incluye
//! la suma de cada hermano; el primer hermano puede ser la hoja de otra cuenta,
//! así que una prueba revela *un* saldo ajeno. Lo que se protege es el
//! vínculo con la identidad: el `cuenta_id` va dentro del hash con una sal
//! aleatoria de 32 bytes, y el orden de las hojas se baraja en cada corte
//! (se ordenan por su hash salado), de modo que el vecino cambia de corte a
//! corte y no puede seguirse en el tiempo. Partir saldos en varias hojas
//! ocultaría también el monto exacto; queda como mejora futura.
//!
//! **Costo acotado.** Un corte recorre todas las cuentas. Solo puede haber uno
//! en curso (advisory lock) y una petición de cliente solo dispara uno si el
//! último tiene más de `CORTE_MIN_INTERVALO_SEG`. El árbol de cada corte se
//! verifica y construye una sola vez, fuera del ejecutor async, y se comparte
//! entre peticiones ([`CacheCortes`]): una prueba cuesta una consulta por
//! índice y O(log N) en memoria.
//!
//! **Anclas.** Cada corte publica `(secuencia_ledger, hash_ledger)`. El
//! historial completo de anclas es público (`GET /reserva/cortes`), de modo que
//! terceros pueden guardar copias: un ledger truncado o reescrito ya no
//! contiene esos hashes y el verificador de la cadena lo detecta.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use chrono::{DateTime, Utc};
use rand::RngCore;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    crypto::merkle::{self, ArbolSuma, Hoja, Lado, Nodo, Paso},
    db::{self, Rol},
    dinero,
    error::{AppError, AppResult},
    models::ifpe::{CorteReserva, EstadoReserva, HojaCorte, LedgerCabeza, ReservaCustodia},
};

/// Estado de la reserva en tiempo real (público).
#[derive(Debug, Clone, Serialize)]
pub struct EstadoReservaVista {
    pub pasivo_clientes: Decimal,
    pub activo_reserva: Decimal,
    pub coeficiente_reserva: Decimal,
    pub reserva_100: bool,
    pub custodia: Vec<CustodiaVista>,
    pub ledger: LedgerCabeza,
    pub actualizado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustodiaVista {
    pub institucion: String,
    pub tipo: String,
    pub saldo: Decimal,
}

pub async fn estado_actual(pool: &PgPool) -> AppResult<EstadoReservaVista> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Auditor).await?;
    let estado: EstadoReserva = sqlx::query_as(
        "SELECT pasivo_clientes, activo_reserva, coeficiente_reserva, actualizado_en FROM ifpe.estado_reserva",
    )
    .fetch_one(&mut *tx)
    .await?;
    let custodia: Vec<ReservaCustodia> =
        sqlx::query_as("SELECT * FROM ifpe.reserva_custodia WHERE activa OR saldo > 0 ORDER BY creado_en, id")
            .fetch_all(&mut *tx)
            .await?;
    let ledger: LedgerCabeza =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM ifpe.ledger_cabeza").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(EstadoReservaVista {
        reserva_100: estado.pasivo_clientes == estado.activo_reserva,
        pasivo_clientes: estado.pasivo_clientes,
        activo_reserva: estado.activo_reserva,
        coeficiente_reserva: estado.coeficiente_reserva,
        custodia: custodia
            .into_iter()
            .map(|r| CustodiaVista { institucion: r.institucion, tipo: r.tipo, saldo: r.saldo })
            .collect(),
        ledger,
        actualizado_en: estado.actualizado_en,
    })
}

#[derive(sqlx::FromRow)]
struct FilaHoja {
    cuenta_id: Uuid,
    saldo_total: Decimal,
}

fn aleatorio_32() -> [u8; 32] {
    let mut sal = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut sal);
    sal
}

fn unidades(monto: Decimal) -> AppResult<u128> {
    dinero::a_unidades(monto).ok_or_else(|| AppError::Integridad(format!("saldo inválido en árbol: {monto}")))
}

fn monto(unidades: u128) -> AppResult<Decimal> {
    dinero::desde_unidades(unidades).ok_or_else(|| AppError::Integridad("suma fuera de rango".into()))
}

/// Ejecuta trabajo de CPU (hashes del árbol) en el pool bloqueante de tokio.
async fn bloqueante<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Integridad(format!("cálculo del árbol Merkle abortado: {e}")))?
}

/// Reproduce el ledger IFPE completo en SQL (agregado por cuenta y por reserva)
/// y cuenta los saldos vivos que no coinciden. Se ejecuta en la misma foto que
/// el corte, así que las hojas publicadas son saldos que el ledger justifica.
const SQL_SALDOS_SIN_LEDGER: &str = "\
    WITH mov AS ( \
        SELECT cuenta_destino_id AS id, monto AS d, 0::numeric AS b FROM ifpe.transacciones \
         WHERE tipo IN ('DEPOSITO', 'RETORNO_DE_SOFOM', 'TRANSFERENCIA') \
        UNION ALL SELECT cuenta_origen_id, -monto, 0 FROM ifpe.transacciones \
         WHERE tipo IN ('RETIRO', 'TRASPASO_A_SOFOM', 'TRANSFERENCIA') \
        UNION ALL SELECT cuenta_origen_id, -monto, monto FROM ifpe.transacciones WHERE tipo = 'RETENCION' \
        UNION ALL SELECT cuenta_origen_id, monto, -monto FROM ifpe.transacciones WHERE tipo = 'LIBERACION_RETENCION' \
    ), cuentas AS (SELECT id, sum(d) AS d, sum(b) AS b FROM mov GROUP BY id), \
    reservas AS ( \
        SELECT reserva_id AS id, sum(CASE WHEN tipo IN ('DEPOSITO', 'RETORNO_DE_SOFOM') THEN monto ELSE -monto END) AS s \
          FROM ifpe.transacciones WHERE reserva_id IS NOT NULL GROUP BY reserva_id \
    ) \
    SELECT (SELECT count(*) FROM ifpe.cuentas c FULL JOIN cuentas r ON r.id = c.id \
             WHERE coalesce(c.saldo_disponible, 0) <> coalesce(r.d, 0) \
                OR coalesce(c.saldo_bloqueado, 0) <> coalesce(r.b, 0)) \
         + (SELECT count(*) FROM ifpe.reserva_custodia c FULL JOIN reservas r ON r.id = c.id \
             WHERE coalesce(c.saldo, 0) <> coalesce(r.s, 0))";

/// Clave del advisory lock que serializa la generación de cortes.
const LOCK_CORTE: i64 = 0x5343_504f_5200_0001; // "SCPOR\0\0\1"

/// Genera y publica un corte de Proof of Reserve.
pub async fn generar_corte(pool: &PgPool) -> AppResult<CorteReserva> {
    db::con_reintentos(|| generar_corte_tx(pool)).await
}

async fn generar_corte_tx(pool: &PgPool) -> AppResult<CorteReserva> {
    let mut tx = db::begin_instantanea(pool, Rol::Auditor).await?;
    let libre: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)").bind(LOCK_CORTE).fetch_one(&mut *tx).await?;
    if !libre {
        return Err(AppError::conflicto("CORTE_EN_CURSO", "ya se está generando un corte de reserva"));
    }
    let filas: Vec<FilaHoja> =
        sqlx::query_as("SELECT cuenta_id, saldo_total FROM ifpe.v_hojas_reserva ORDER BY cuenta_id")
            .fetch_all(&mut *tx)
            .await?;
    let estado: EstadoReserva = sqlx::query_as(
        "SELECT pasivo_clientes, activo_reserva, coeficiente_reserva, actualizado_en FROM ifpe.estado_reserva",
    )
    .fetch_one(&mut *tx)
    .await?;
    let reserva_custodia: Decimal =
        sqlx::query_scalar("SELECT coalesce(sum(saldo), 0) FROM ifpe.reserva_custodia").fetch_one(&mut *tx).await?;
    let cabeza: LedgerCabeza =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM ifpe.ledger_cabeza").fetch_one(&mut *tx).await?;

    // El ledger nunca retrocede respecto de un ancla ya publicada.
    let ancla_maxima: i64 = sqlx::query_scalar("SELECT coalesce(max(secuencia_ledger), 0) FROM ifpe.cortes_reserva")
        .fetch_one(&mut *tx)
        .await?;
    if cabeza.ultima_secuencia < ancla_maxima {
        return Err(AppError::Integridad(format!(
            "la cabeza del ledger ({}) está detrás de un corte ya publicado ({ancla_maxima}): ledger truncado",
            cabeza.ultima_secuencia
        )));
    }
    // Cada saldo de la foto debe ser exactamente el que el ledger justifica.
    let sin_ledger: i64 = sqlx::query_scalar(SQL_SALDOS_SIN_LEDGER).fetch_one(&mut *tx).await?;
    if sin_ledger > 0 {
        return Err(AppError::Integridad(format!(
            "{sin_ledger} saldos (cuentas o reserva) no coinciden con la reproducción del ledger"
        )));
    }

    let hojas: Vec<Hoja> = filas
        .iter()
        .map(|f| Ok(Hoja { cuenta_id: f.cuenta_id, saldo: unidades(f.saldo_total)?, sal: aleatorio_32() }))
        .collect::<AppResult<_>>()?;
    // Hash de cada hoja y árbol completo: CPU pura, fuera del ejecutor async.
    let (hojas, hashes, arbol) = bloqueante(move || {
        let mut con_hash: Vec<(merkle::Hash, Hoja)> = hojas.into_iter().map(|h| (h.hash(), h)).collect();
        // Orden barajado en cada corte (el hash depende de la sal aleatoria).
        con_hash.sort_unstable_by_key(|(hash, _)| *hash);
        let nodos = con_hash.iter().map(|(hash, h)| Nodo { hash: *hash, suma: h.saldo }).collect();
        let arbol = ArbolSuma::desde_nodos(nodos).map_err(|e| AppError::Integridad(e.to_string()))?;
        let (hashes, hojas) = con_hash.into_iter().unzip::<_, _, Vec<_>, Vec<_>>();
        Ok((hojas, hashes, arbol))
    })
    .await?;
    let raiz = arbol.raiz();
    let total_hojas = monto(raiz.suma)?;

    if total_hojas != estado.pasivo_clientes
        || estado.pasivo_clientes != estado.activo_reserva
        || estado.activo_reserva != reserva_custodia
    {
        return Err(AppError::Integridad(format!(
            "el corte no cuadra: Σhojas={total_hojas} pasivo={} activo={} Σcustodia={reserva_custodia}",
            estado.pasivo_clientes, estado.activo_reserva
        )));
    }

    let corte: CorteReserva = sqlx::query_as(
        "INSERT INTO ifpe.cortes_reserva \
         (id, merkle_root, num_hojas, total_pasivo, total_reserva, secuencia_ledger, hash_ledger, algoritmo) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(hex::encode(raiz.hash))
    .bind(i32::try_from(hojas.len()).map_err(|_| AppError::Integridad("demasiadas hojas".into()))?)
    .bind(estado.pasivo_clientes)
    .bind(reserva_custodia)
    .bind(cabeza.ultima_secuencia)
    .bind(&cabeza.ultimo_hash)
    .bind(merkle::ALGORITMO)
    .fetch_one(&mut *tx)
    .await?;

    // Inserción masiva de hojas en lotes (UNNEST de arreglos paralelos).
    for (n, lote) in hojas.chunks(5_000).enumerate() {
        let base = n * 5_000;
        let indices: Vec<i32> = (0..lote.len()).map(|i| (base + i) as i32).collect();
        let cuentas: Vec<Uuid> = lote.iter().map(|h| h.cuenta_id).collect();
        let saldos: Vec<Decimal> = lote.iter().map(|h| monto(h.saldo)).collect::<AppResult<_>>()?;
        let sales: Vec<String> = lote.iter().map(|h| hex::encode(h.sal)).collect();
        let hashes: Vec<String> = hashes[base..base + lote.len()].iter().map(hex::encode).collect();
        sqlx::query(
            "INSERT INTO ifpe.cortes_reserva_hojas (corte_id, indice, cuenta_id, saldo_total, sal, hoja_hash) \
             SELECT $1, * FROM UNNEST($2::int[], $3::uuid[], $4::numeric[], $5::text[], $6::text[])",
        )
        .bind(corte.id)
        .bind(&indices)
        .bind(&cuentas)
        .bind(&saldos)
        .bind(&sales)
        .bind(&hashes)
        .execute(&mut *tx)
        .await?;
    }
    // El constraint trigger diferido comprueba Σhojas == total_pasivo y num_hojas al COMMIT.
    tx.commit().await?;
    tracing::info!(corte = %corte.id, raiz = %corte.merkle_root, hojas = corte.num_hojas, pasivo = %corte.total_pasivo, "corte de Proof of Reserve publicado");
    Ok(corte)
}

pub async fn ultimo_corte(pool: &PgPool) -> AppResult<Option<CorteReserva>> {
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let corte = sqlx::query_as("SELECT * FROM ifpe.cortes_reserva ORDER BY creado_en DESC, id DESC LIMIT 1")
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(corte)
}

/// Historial público de anclas, del más reciente al más antiguo. `antes`
/// pagina por fecha de publicación.
pub async fn listar_cortes(pool: &PgPool, antes: Option<DateTime<Utc>>, limite: i64) -> AppResult<Vec<CorteReserva>> {
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let cortes = sqlx::query_as(
        "SELECT * FROM ifpe.cortes_reserva WHERE $1::timestamptz IS NULL OR creado_en < $1 \
         ORDER BY creado_en DESC, id DESC LIMIT $2",
    )
    .bind(antes)
    .bind(limite.clamp(1, 500))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(cortes)
}

pub async fn corte_por_id(pool: &PgPool, id: Uuid) -> AppResult<CorteReserva> {
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let corte =
        sqlx::query_as("SELECT * FROM ifpe.cortes_reserva WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    corte.ok_or_else(|| AppError::no_encontrado("corte no encontrado"))
}

// -----------------------------------------------------------------------------
// Árbol de cada corte, construido una sola vez
// -----------------------------------------------------------------------------

/// Árbol verificado del corte más reciente que se ha pedido. Los cortes son
/// inmutables, así que el árbol se reconstruye (y se comprueba contra la raíz
/// publicada) una vez por corte en lugar de una vez por petición.
///
/// La construcción corre en una tarea propia que conserva el candado: si el
/// cliente que la inició se desconecta, el trabajo no se pierde ni se repite.
/// Memoria: unos 100 bytes por cuenta (todos los niveles del árbol).
#[derive(Default)]
pub struct CacheCortes {
    ultimo: Arc<Mutex<Option<ArbolCorte>>>,
    construcciones: AtomicU64,
}

#[derive(Clone)]
struct ArbolCorte {
    corte_id: Uuid,
    creado_en: DateTime<Utc>,
    arbol: Arc<ArbolSuma>,
}

impl CacheCortes {
    /// Veces que se ha reconstruido un árbol (para pruebas y métricas).
    pub fn construcciones(&self) -> u64 {
        self.construcciones.load(Ordering::Relaxed)
    }

    async fn arbol(self: &Arc<Self>, pool: &PgPool, corte: &CorteReserva) -> AppResult<Arc<ArbolSuma>> {
        let guardia = Arc::clone(&self.ultimo).lock_owned().await;
        if let Some(c) = guardia.as_ref().filter(|c| c.corte_id == corte.id) {
            return Ok(Arc::clone(&c.arbol));
        }
        let (cache, pool, corte) = (Arc::clone(self), pool.clone(), corte.clone());
        tokio::spawn(async move {
            let mut guardia = guardia;
            let arbol = Arc::new(construir_verificado(&pool, &corte).await?);
            cache.construcciones.fetch_add(1, Ordering::Relaxed);
            // Nunca se reemplaza un corte más reciente por uno anterior.
            if guardia.as_ref().map_or(true, |c| c.creado_en <= corte.creado_en) {
                *guardia =
                    Some(ArbolCorte { corte_id: corte.id, creado_en: corte.creado_en, arbol: Arc::clone(&arbol) });
            }
            Ok(arbol)
        })
        .await
        .map_err(|e| AppError::Integridad(format!("construcción del árbol abortada: {e}")))?
    }
}

/// Lee las hojas almacenadas de un corte, recalcula cada hash y el árbol, y
/// exige que la raíz y la suma coincidan con las publicadas: una hoja alterada
/// después de publicar se detecta aquí.
async fn construir_verificado(pool: &PgPool, corte: &CorteReserva) -> AppResult<ArbolSuma> {
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let hojas: Vec<HojaCorte> =
        sqlx::query_as("SELECT * FROM ifpe.cortes_reserva_hojas WHERE corte_id = $1 ORDER BY indice")
            .bind(corte.id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    let corte = corte.clone();
    bloqueante(move || {
        if usize::try_from(corte.num_hojas).ok() != Some(hojas.len()) {
            return Err(AppError::Integridad(format!("el corte {} declara {} hojas", corte.id, corte.num_hojas)));
        }
        let mut nodos = Vec::with_capacity(hojas.len());
        for (i, h) in hojas.iter().enumerate() {
            if usize::try_from(h.indice).ok() != Some(i) {
                return Err(AppError::Integridad(format!("hojas del corte {} no contiguas", corte.id)));
            }
            let hoja = hoja_de_fila(h)?;
            let hash = hoja.hash();
            if hex::encode(hash) != h.hoja_hash {
                return Err(AppError::Integridad(format!("hash de hoja alterado en corte {} índice {i}", corte.id)));
            }
            nodos.push(Nodo { hash, suma: hoja.saldo });
        }
        let arbol = ArbolSuma::desde_nodos(nodos).map_err(|e| AppError::Integridad(e.to_string()))?;
        let raiz = arbol.raiz();
        if hex::encode(raiz.hash) != corte.merkle_root || monto(raiz.suma)? != corte.total_pasivo {
            return Err(AppError::Integridad(format!("la raíz recalculada del corte {} no coincide", corte.id)));
        }
        Ok(arbol)
    })
    .await
}

fn hoja_de_fila(h: &HojaCorte) -> AppResult<Hoja> {
    Ok(Hoja { cuenta_id: h.cuenta_id, saldo: unidades(h.saldo_total)?, sal: hash_desde_hex(&h.sal)? })
}

// -----------------------------------------------------------------------------
// Pruebas de inclusión (formato JSON, verificable fuera del banco)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HojaJson {
    pub cuenta_id: Uuid,
    /// Saldo con 8 decimales (las unidades del árbol son este valor × 10^8).
    #[serde(with = "rust_decimal::serde::str")]
    pub saldo: Decimal,
    pub sal: String,
    #[serde(default)]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasoJson {
    pub lado: Lado,
    pub hash: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub suma: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct PruebaReserva {
    pub corte: CorteReserva,
    pub indice: usize,
    pub hoja: HojaJson,
    pub ruta: Vec<PasoJson>,
    /// Verificación hecha por el servidor (el cliente debe repetirla por su cuenta).
    pub valida: bool,
    /// Saldo actual de la cuenta, para comparar con el saldo del corte.
    pub saldo_actual: Decimal,
}

fn hash_desde_hex(s: &str) -> AppResult<merkle::Hash> {
    let bytes = hex::decode(s).map_err(|_| AppError::validacion("hash hexadecimal inválido"))?;
    bytes.try_into().map_err(|_| AppError::validacion("el hash debe tener 32 bytes"))
}

impl HojaJson {
    fn a_hoja(&self) -> AppResult<Hoja> {
        Ok(Hoja {
            cuenta_id: self.cuenta_id,
            saldo: dinero::a_unidades_estricto(self.saldo)
                .ok_or_else(|| AppError::validacion("saldo inválido: no negativo y con 8 decimales como máximo"))?,
            sal: hash_desde_hex(&self.sal)?,
        })
    }
}

fn ruta_a_json(ruta: &[Paso]) -> AppResult<Vec<PasoJson>> {
    ruta.iter()
        .map(|p| Ok(PasoJson { lado: p.lado, hash: hex::encode(p.hermano.hash), suma: monto(p.hermano.suma)? }))
        .collect()
}

fn ruta_desde_json(ruta: &[PasoJson]) -> AppResult<Vec<Paso>> {
    ruta.iter()
        .map(|p| {
            Ok(Paso {
                lado: p.lado,
                hermano: Nodo {
                    hash: hash_desde_hex(&p.hash)?,
                    suma: dinero::a_unidades_estricto(p.suma).ok_or_else(|| {
                        AppError::validacion("suma inválida: no negativa y con 8 decimales como máximo")
                    })?,
                },
            })
        })
        .collect()
}

/// Prueba de inclusión de la cuenta IFPE del cliente en el último corte.
///
/// Si la cuenta aún no está en un corte (cuenta nueva), se genera uno solo si
/// el último tiene al menos `min_intervalo_seg` de antigüedad y no hay otro en
/// curso; si no, responde `PRUEBA_PENDIENTE` y el siguiente corte la incluirá.
pub async fn prueba_de_cliente(
    pool: &PgPool,
    cache: &Arc<CacheCortes>,
    cliente_id: Uuid,
    min_intervalo_seg: u64,
) -> AppResult<PruebaReserva> {
    if let Some(p) = prueba_en_ultimo_corte(pool, cache, cliente_id).await? {
        return Ok(p);
    }
    let pendiente = || {
        AppError::negocio(
            "PRUEBA_PENDIENTE",
            "tu cuenta se incluirá en el próximo corte de reserva; inténtalo de nuevo en un momento",
        )
    };
    if let Some(ultimo) = ultimo_corte(pool).await? {
        let antiguedad = Utc::now().signed_duration_since(ultimo.creado_en).num_seconds();
        if antiguedad < i64::try_from(min_intervalo_seg).unwrap_or(i64::MAX) {
            return Err(pendiente());
        }
    }
    match generar_corte(pool).await {
        Ok(_) => {}
        Err(AppError::Conflicto { codigo, .. }) if codigo == "CORTE_EN_CURSO" => return Err(pendiente()),
        Err(e) => return Err(e),
    }
    prueba_en_ultimo_corte(pool, cache, cliente_id).await?.ok_or_else(pendiente)
}

async fn prueba_en_ultimo_corte(
    pool: &PgPool,
    cache: &Arc<CacheCortes>,
    cliente_id: Uuid,
) -> AppResult<Option<PruebaReserva>> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Auditor).await?;
    let cuenta: Option<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT id, saldo_disponible + saldo_bloqueado FROM ifpe.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'",
    )
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((cuenta_id, saldo_actual)) = cuenta else {
        return Err(AppError::no_encontrado("el cliente no tiene cuenta en la bóveda IFPE"));
    };
    let corte: Option<CorteReserva> =
        sqlx::query_as("SELECT * FROM ifpe.cortes_reserva ORDER BY creado_en DESC, id DESC LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?;
    let Some(corte) = corte else { return Ok(None) };
    // Solo la hoja de esta cuenta (índice único por corte y cuenta).
    let fila: Option<HojaCorte> =
        sqlx::query_as("SELECT * FROM ifpe.cortes_reserva_hojas WHERE corte_id = $1 AND cuenta_id = $2")
            .bind(corte.id)
            .bind(cuenta_id)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    let Some(fila) = fila else { return Ok(None) };

    let arbol = cache.arbol(pool, &corte).await?;
    let hoja = hoja_de_fila(&fila)?;
    let indice = usize::try_from(fila.indice).map_err(|_| AppError::Integridad("índice de hoja inválido".into()))?;
    let prueba = arbol.prueba(indice).ok_or_else(|| AppError::Integridad("índice fuera del árbol".into()))?;
    // La fila leída ahora debe ser la misma hoja con la que se verificó el árbol.
    if prueba.hoja != hoja.nodo() || hex::encode(prueba.hoja.hash) != fila.hoja_hash {
        return Err(AppError::Integridad(format!("hoja alterada en corte {} índice {indice}", corte.id)));
    }
    let raiz = arbol.raiz();
    let valida = merkle::verificar(&hoja, &prueba.ruta, &raiz) && hex::encode(raiz.hash) == corte.merkle_root;
    Ok(Some(PruebaReserva {
        indice,
        hoja: HojaJson {
            cuenta_id: fila.cuenta_id,
            saldo: dinero::con_escala(fila.saldo_total),
            sal: fila.sal,
            hash: Some(fila.hoja_hash),
        },
        ruta: ruta_a_json(&prueba.ruta)?,
        valida,
        saldo_actual: dinero::con_escala(saldo_actual),
        corte,
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct SolicitudVerificacion {
    pub corte_id: Uuid,
    pub hoja: HojaJson,
    pub ruta: Vec<PasoJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoVerificacionPrueba {
    pub valida: bool,
    pub raiz_calculada: String,
    pub suma_calculada: Decimal,
    pub raiz_publicada: String,
    pub total_pasivo_publicado: Decimal,
    pub total_reserva_publicado: Decimal,
}

/// Verifica una prueba contra la raíz publicada de un corte (servicio público).
pub async fn verificar_prueba(pool: &PgPool, s: &SolicitudVerificacion) -> AppResult<ResultadoVerificacionPrueba> {
    if s.ruta.len() > 64 {
        return Err(AppError::validacion("ruta demasiado larga"));
    }
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let corte: CorteReserva = sqlx::query_as("SELECT * FROM ifpe.cortes_reserva WHERE id = $1")
        .bind(s.corte_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("corte no encontrado"))?;
    tx.commit().await?;

    let hoja = s.hoja.a_hoja()?;
    let ruta = ruta_desde_json(&s.ruta)?;
    let calculada = merkle::raiz_desde_ruta(&hoja.nodo(), &ruta).map_err(|e| AppError::validacion(e.to_string()))?;
    let raiz_calculada = hex::encode(calculada.hash);
    let suma_calculada = monto(calculada.suma)?;
    Ok(ResultadoVerificacionPrueba {
        valida: raiz_calculada == corte.merkle_root && suma_calculada == corte.total_pasivo,
        raiz_calculada,
        suma_calculada,
        raiz_publicada: corte.merkle_root,
        total_pasivo_publicado: corte.total_pasivo,
        total_reserva_publicado: corte.total_reserva,
    })
}
