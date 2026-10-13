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
//! último tiene más de `CORTE_MIN_INTERVALO_SEG`.

use chrono::{DateTime, Utc};
use rand::RngCore;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
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

/// Genera y publica un corte de Proof of Reserve.
/// Clave del advisory lock que serializa la generación de cortes.
const LOCK_CORTE: i64 = 0x5343_504f_5200_0001; // "SCPOR\0\0\1"

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

    let mut hojas: Vec<Hoja> = filas
        .iter()
        .map(|f| Ok(Hoja { cuenta_id: f.cuenta_id, saldo: unidades(f.saldo_total)?, sal: aleatorio_32() }))
        .collect::<AppResult<_>>()?;
    // Orden barajado en cada corte (el hash depende de la sal aleatoria).
    hojas.sort_by_cached_key(Hoja::hash);
    let arbol = ArbolSuma::construir(&hojas).map_err(|e| AppError::Integridad(e.to_string()))?;
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
        let hashes: Vec<String> = lote.iter().map(|h| hex::encode(h.hash())).collect();
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

// -----------------------------------------------------------------------------
// Pruebas de inclusión (formato JSON, verificable fuera del banco)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HojaJson {
    pub cuenta_id: Uuid,
    /// Saldo con 8 decimales (las unidades del árbol son este valor × 10^8).
    pub saldo: Decimal,
    pub sal: String,
    #[serde(default)]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasoJson {
    pub lado: Lado,
    pub hash: String,
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
            saldo: dinero::a_unidades(self.saldo).ok_or_else(|| AppError::validacion("saldo inválido"))?,
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
                    suma: dinero::a_unidades(p.suma).ok_or_else(|| AppError::validacion("suma inválida"))?,
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
pub async fn prueba_de_cliente(pool: &PgPool, cliente_id: Uuid, min_intervalo_seg: u64) -> AppResult<PruebaReserva> {
    if let Some(p) = prueba_en_ultimo_corte(pool, cliente_id).await? {
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
    prueba_en_ultimo_corte(pool, cliente_id).await?.ok_or_else(pendiente)
}

async fn prueba_en_ultimo_corte(pool: &PgPool, cliente_id: Uuid) -> AppResult<Option<PruebaReserva>> {
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
    let hojas: Vec<HojaCorte> =
        sqlx::query_as("SELECT * FROM ifpe.cortes_reserva_hojas WHERE corte_id = $1 ORDER BY indice")
            .bind(corte.id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;

    let Some(posicion) = hojas.iter().position(|h| h.cuenta_id == cuenta_id) else { return Ok(None) };

    // Reconstruye el árbol desde las hojas almacenadas y exige que la raíz
    // coincida con la publicada: si alguien alteró una hoja, se detecta aquí.
    let mut nodos = Vec::with_capacity(hojas.len());
    for (i, h) in hojas.iter().enumerate() {
        if h.indice as usize != i {
            return Err(AppError::Integridad(format!("hojas del corte {} no contiguas", corte.id)));
        }
        let hoja = Hoja { cuenta_id: h.cuenta_id, saldo: unidades(h.saldo_total)?, sal: hash_desde_hex(&h.sal)? };
        if hex::encode(hoja.hash()) != h.hoja_hash {
            return Err(AppError::Integridad(format!("hash de hoja alterado en corte {} índice {i}", corte.id)));
        }
        nodos.push(hoja.nodo());
    }
    let arbol = ArbolSuma::desde_nodos(nodos).map_err(|e| AppError::Integridad(e.to_string()))?;
    let raiz = arbol.raiz();
    if hex::encode(raiz.hash) != corte.merkle_root || monto(raiz.suma)? != corte.total_pasivo {
        return Err(AppError::Integridad(format!("la raíz recalculada del corte {} no coincide", corte.id)));
    }

    let h = &hojas[posicion];
    let prueba = arbol.prueba(posicion).ok_or_else(|| AppError::Integridad("índice fuera del árbol".into()))?;
    let hoja = Hoja { cuenta_id: h.cuenta_id, saldo: unidades(h.saldo_total)?, sal: hash_desde_hex(&h.sal)? };
    let valida = merkle::verificar(&hoja, &prueba.ruta, &raiz);
    Ok(Some(PruebaReserva {
        indice: posicion,
        hoja: HojaJson {
            cuenta_id: h.cuenta_id,
            saldo: dinero::con_escala(h.saldo_total),
            sal: h.sal.clone(),
            hash: Some(h.hoja_hash.clone()),
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
