//! Monitoreo PLD por lotes (§3.4): lo que la evaluación en línea no ve.
//!
//! La evaluación en línea solo mira ventanas de hasta 24 h para no encarecer
//! los caminos del dinero. Este ciclo, que corre en segundo plano cada
//! `AML_MONITOREO_INTERVALO_SEG`, revisa los flujos agregados por día y por
//! mes de los clientes que se movieron en el ledger:
//!
//! - reglas por lotes R20–R25 (fraccionamiento en 7 y 30 días, volumen
//!   mensual contra perfil, abanicos de entrada y salida, paso rápido y flujos
//!   circulares entre clientes), con una alerta por cliente, regla y semana;
//! - redes entre clientes R26/R27 (un mismo ordenante o beneficiario para
//!   varios clientes);
//! - reevaluación de las marcas DIFERIDA (movimientos que llegaron al ledger
//!   sin evaluación en línea) y de las evaluaciones fallidas;
//! - plazos: alertas críticas sin atender, congelamientos sin decisión y
//!   plazos de reporte a la UIF próximos o vencidos;
//! - sellado de la bitácora en lotes encadenados;
//! - entrega de la bandeja de salida de notificaciones al log.
//!
//! Toda la lógica vive en funciones definer del esquema `aml` (§2.10); aquí
//! solo se encadenan, cada paso en su propia transacción corta `sc_aml`
//! (READ COMMITTED, como exige `aml.calcular`). Un arrendamiento en
//! `aml.monitoreo_estado` garantiza una sola instancia a la vez, y la marca
//! de agua del ledger (que arranca en las cabezas sembradas por 0009) solo
//! avanza cuando todos los clientes del rango se revisaron.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::ConfigAml,
    db::{self, Rol},
    error::{AppError, AppResult},
};

/// Duración del arrendamiento del ciclo. Si el proceso muere a mitad del
/// ciclo, otra instancia puede tomar el turno cuando vence.
const TURNO_SEG: i32 = 600;
/// Marcas DIFERIDA que se reevalúan por ciclo.
const MARCADORES_POR_CICLO: i32 = 100;
/// Notificaciones que se entregan por ciclo.
const NOTIFICACIONES_POR_CICLO: i64 = 100;

/// Resultado de un ciclo (se guarda en `aml.monitoreo_estado.ultimo_resultado`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Ciclo {
    /// `false`: otra instancia tiene el turno y no se ejecutó nada más.
    pub turno: bool,
    pub hasta_ifpe: i64,
    pub hasta_sofom: i64,
    /// Clientes con movimientos en el rango revisado.
    pub clientes: usize,
    /// Clientes ocupados (fila o candado PLD tomados) aun después de un reintento.
    pub ocupados: usize,
    pub alertas_lote: i64,
    pub alertas_redes: i64,
    pub reevaluadas: usize,
    pub plazos: i64,
    pub sellados: i64,
    pub notificaciones: usize,
    pub errores: usize,
}

/// Un ciclo completo con el reloj de la base de datos.
pub async fn ciclo(pool: &PgPool, cfg: &ConfigAml) -> AppResult<Ciclo> {
    ciclo_en(pool, cfg, None).await
}

/// Un ciclo completo. `ahora = None` usa el reloj de la base de datos; las
/// pruebas pasan un instante futuro para ejercitar los plazos.
///
/// Solo falla si no se puede tomar o liberar el turno: el error de un paso se
/// registra, cuenta en `errores` y no impide los pasos siguientes (un error
/// en las reglas por lotes sí impide avanzar la marca de agua, así que el
/// siguiente ciclo vuelve a revisar el mismo rango).
pub async fn ciclo_en(pool: &PgPool, cfg: &ConfigAml, ahora: Option<DateTime<Utc>>) -> AppResult<Ciclo> {
    // 1. Turno.
    let Some((desde_ifpe, desde_sofom)) = tomar_turno(pool).await? else {
        return Ok(Ciclo::default());
    };
    let mut c = Ciclo { turno: true, hasta_ifpe: desde_ifpe, hasta_sofom: desde_sofom, ..Ciclo::default() };

    // 2-5. Reglas por lotes de los clientes que se movieron y marca de agua.
    if let Err(err) = barrer_rango(pool, cfg, ahora, desde_ifpe, desde_sofom, &mut c).await {
        registrar_error(&mut c, "reglas por lotes", &err);
    }

    // 6. Redes entre clientes (R26/R27); idempotente por semana.
    match escalar_i32(pool, "SELECT aml.barrer_redes(coalesce($1::timestamptz, clock_timestamp()))", ahora).await {
        Ok(n) => c.alertas_redes = i64::from(n),
        Err(err) => registrar_error(&mut c, "redes entre clientes", &err),
    }

    // 7. Marcas DIFERIDA y evaluaciones fallidas.
    if let Err(err) = reevaluar_marcas(pool, &mut c).await {
        registrar_error(&mut c, "reevaluación de marcas", &err);
    }

    // 8. Plazos (cada aviso una sola vez, por los índices únicos).
    match escalar_i32(pool, "SELECT aml.revisar_plazos(coalesce($1::timestamptz, clock_timestamp()))", ahora).await {
        Ok(n) => c.plazos = i64::from(n),
        Err(err) => registrar_error(&mut c, "revisión de plazos", &err),
    }

    // 9. Sellado de la bitácora (renglones con más de 10 minutos).
    match sellar(pool).await {
        Ok(n) => c.sellados = i64::from(n),
        Err(err) => registrar_error(&mut c, "sellado de la bitácora", &err),
    }

    // 10. Entrega de notificaciones.
    match entregar_notificaciones(pool, NOTIFICACIONES_POR_CICLO).await {
        Ok(n) => c.notificaciones = n,
        Err(err) => registrar_error(&mut c, "entrega de notificaciones", &err),
    }

    // 11. Libera el turno con el resultado.
    liberar_turno(pool, &c).await?;
    Ok(c)
}

/// Entrega la bandeja de salida al log (`target: "aml"`) y marca cada
/// notificación como entregada, en una sola transacción: al menos una vez
/// (si el proceso muere antes del COMMIT, el lote se vuelve a registrar). El
/// resumen solo lleva códigos y folios. Es el punto de extensión para correo o
/// mensajería.
pub async fn entregar_notificaciones(pool: &PgPool, lote: i64) -> AppResult<usize> {
    let lote = i32::try_from(lote.clamp(1, 10_000)).unwrap_or(100);
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let pendientes: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT id, tipo, prioridad, resumen FROM aml.tomar_notificaciones($1)")
            .bind(lote)
            .fetch_all(&mut *tx)
            .await?;
    for (id, tipo, prioridad, resumen) in &pendientes {
        tracing::warn!(target: "aml", id, %tipo, %prioridad, %resumen, "notificación de cumplimiento");
        sqlx::query("SELECT aml.marcar_notificacion($1)").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(pendientes.len())
}

fn registrar_error(c: &mut Ciclo, paso: &str, err: &AppError) {
    c.errores += 1;
    tracing::error!(target: "aml", paso, codigo = err.codigo(), error = %err.detalle(), "falló un paso del monitoreo AML");
}

async fn tomar_turno(pool: &PgPool) -> AppResult<Option<(i64, i64)>> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let marcas: Option<(i64, i64)> =
        sqlx::query_as("SELECT ultima_secuencia_ifpe, ultima_secuencia_sofom FROM aml.monitoreo_tomar_turno($1)")
            .bind(TURNO_SEG)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(marcas)
}

async fn liberar_turno(pool: &PgPool, c: &Ciclo) -> AppResult<()> {
    let resultado = serde_json::to_value(c).map_err(anyhow::Error::from)?;
    let mut tx = db::begin(pool, Rol::Aml).await?;
    sqlx::query("SELECT aml.monitoreo_liberar_turno($1)").bind(resultado).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Una función entera con el instante opcional como único parámetro, en su
/// propia transacción.
async fn escalar_i32(pool: &PgPool, sql: &str, ahora: Option<DateTime<Utc>>) -> AppResult<i32> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let n: i32 = sqlx::query_scalar(sql).bind(ahora).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(n)
}

async fn sellar(pool: &PgPool) -> AppResult<i32> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let n: i32 = sqlx::query_scalar("SELECT aml.sellar_bitacora()").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(n)
}

/// Pasos 2 a 5: rango del ledger, clientes candidatos, reglas R20–R25 por
/// cliente y, si todos se revisaron sin error, avance de la marca de agua.
async fn barrer_rango(
    pool: &PgPool,
    cfg: &ConfigAml,
    ahora: Option<DateTime<Utc>>,
    desde_ifpe: i64,
    desde_sofom: i64,
    c: &mut Ciclo,
) -> AppResult<()> {
    let lote = i32::try_from(cfg.monitoreo_lote.clamp(1, 100_000)).unwrap_or(5_000);
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let (hasta_ifpe, hasta_sofom): (i64, i64) =
        sqlx::query_as("SELECT hasta_ifpe, hasta_sofom FROM aml.monitoreo_rango($1)")
            .bind(lote)
            .fetch_one(&mut *tx)
            .await?;
    let candidatos: Vec<Uuid> = sqlx::query_scalar("SELECT aml.monitoreo_candidatos($1, $2, $3, $4)")
        .bind(desde_ifpe)
        .bind(hasta_ifpe)
        .bind(desde_sofom)
        .bind(hasta_sofom)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    c.hasta_ifpe = hasta_ifpe;
    c.hasta_sofom = hasta_sofom;
    c.clientes = candidatos.len();

    // Un cliente ocupado (otra transacción tiene su fila o su candado PLD) se
    // reintenta una vez al final; barrer_cliente nunca espera un candado.
    let mut fallidos = 0usize;
    let mut ocupados = Vec::new();
    for cliente in candidatos {
        match barrer_cliente(pool, cliente, ahora).await {
            Ok(n) if n < 0 => ocupados.push(cliente),
            Ok(n) => c.alertas_lote += i64::from(n),
            Err(err) => {
                fallidos += 1;
                registrar_error(c, "reglas por lotes de un cliente", &err);
            }
        }
    }
    let mut siguen_ocupados = 0usize;
    for cliente in ocupados {
        match barrer_cliente(pool, cliente, ahora).await {
            Ok(n) if n < 0 => siguen_ocupados += 1,
            Ok(n) => c.alertas_lote += i64::from(n),
            Err(err) => {
                fallidos += 1;
                registrar_error(c, "reglas por lotes de un cliente", &err);
            }
        }
    }
    c.ocupados = siguen_ocupados;

    // Sin avanzar la marca, el siguiente ciclo revisa el mismo rango: es
    // seguro porque las alertas por lotes se deduplican por cliente, regla y
    // semana (volver a correr no crea nada).
    if siguen_ocupados > 0 || fallidos > 0 {
        tracing::warn!(target: "aml", ocupados = siguen_ocupados, fallidos, "la marca de agua del monitoreo no avanza en este ciclo");
        return Ok(());
    }
    let mut tx = db::begin(pool, Rol::Aml).await?;
    sqlx::query("SELECT aml.monitoreo_avanzar_marca($1, $2)")
        .bind(hasta_ifpe)
        .bind(hasta_sofom)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Reglas por lotes de un cliente en su propia transacción. `-1`: ocupado.
async fn barrer_cliente(pool: &PgPool, cliente: Uuid, ahora: Option<DateTime<Utc>>) -> AppResult<i32> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let n: i32 = sqlx::query_scalar("SELECT aml.barrer_cliente($1, coalesce($2::timestamptz, clock_timestamp()))")
        .bind(cliente)
        .bind(ahora)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(n)
}

/// Paso 7: una transacción por marca. Una segunda reevaluación de la misma
/// marca choca con el índice único de `evaluacion_origen_id` y se omite.
async fn reevaluar_marcas(pool: &PgPool, c: &mut Ciclo) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let marcas: Vec<Uuid> = sqlx::query_scalar("SELECT aml.marcadores_pendientes($1)")
        .bind(MARCADORES_POR_CICLO)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    for id in marcas {
        let resultado = async {
            let mut tx = db::begin(pool, Rol::Aml).await?;
            sqlx::query("SELECT aml.reevaluar($1)").bind(id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok::<_, AppError>(())
        }
        .await;
        match resultado {
            Ok(()) => c.reevaluadas += 1,
            Err(err @ AppError::Conflicto { .. }) => {
                tracing::debug!(target: "aml", evaluacion = %id, error = %err.detalle(), "marca ya reevaluada");
            }
            Err(err) => registrar_error(c, "reevaluación de una marca", &err),
        }
    }
    Ok(())
}
