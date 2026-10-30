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
//! - sellado de la bitácora en lotes encadenados y verificación incremental
//!   de la cadena (desde el último sello verificado);
//! - entrega de la bandeja de salida de notificaciones al log.
//!
//! Toda la lógica vive en funciones definer del esquema `aml` (§2.10); aquí
//! solo se encadenan, cada paso en su propia transacción corta `sc_aml`
//! (READ COMMITTED, como exige `aml.calcular`). Un arrendamiento con ficha en
//! `aml.monitoreo_estado` garantiza una sola instancia a la vez: se renueva
//! entre pasos y cada `TURNO_RENOVAR_CADA` clientes, y si otra instancia lo
//! retomó el ciclo se detiene sin tocar su turno. La marca de agua del ledger
//! (que arranca en las cabezas sembradas por 0009) solo avanza, hasta el rango
//! que registró el turno, cuando todos los clientes del rango se revisaron o
//! quedaron en cuarentena (tres ciclos fallidos: aviso COBERTURA; se
//! reintentan aparte). Las reglas por cliente se evalúan en los instantes de
//! sus movimientos del rango, no en el reloj del ciclo, y las funciones del
//! esquema usan el reloj del servidor: el instante de [`ciclo_en`] solo lo
//! respeta la base de datos cuando quien llama es el dueño del esquema.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::ConfigAml,
    db::{self, Rol},
    error::{AppError, AppResult},
};

/// Duración del arrendamiento del ciclo (la base de datos acepta a lo sumo
/// 600 s). Si el proceso muere a mitad del ciclo, otra instancia puede tomar
/// el turno cuando vence.
const TURNO_SEG: i32 = 600;
/// Clientes barridos entre dos renovaciones del arrendamiento.
const TURNO_RENOVAR_CADA: usize = 200;
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
    /// Clientes cuyo barrido falló en este ciclo y clientes en cuarentena.
    pub fallidos: usize,
    pub cuarentena: usize,
    pub alertas_lote: i64,
    pub alertas_redes: i64,
    pub reevaluadas: usize,
    pub plazos: i64,
    pub sellados: i64,
    /// Resultado de la verificación incremental de la bitácora.
    pub bitacora_valida: bool,
    pub notificaciones: usize,
    pub errores: usize,
}

/// Un ciclo completo con el reloj de la base de datos.
pub async fn ciclo(pool: &PgPool, cfg: &ConfigAml) -> AppResult<Ciclo> {
    ciclo_en(pool, cfg, None).await
}

/// Un ciclo completo con un instante propuesto. La base de datos lo ignora
/// para `sc_aml` (usa su propio reloj: nadie adelanta plazos ni ventanas);
/// solo lo respeta cuando quien llama es el dueño del esquema.
///
/// Solo falla si no se puede tomar o liberar el turno: el error de un paso se
/// registra, cuenta en `errores` y no impide los pasos siguientes (un error
/// en las reglas por lotes de un cliente impide avanzar la marca de agua
/// hasta que el cliente queda en cuarentena). Si el turno se pierde (otra
/// instancia lo retomó), el ciclo se detiene.
pub async fn ciclo_en(pool: &PgPool, cfg: &ConfigAml, ahora: Option<DateTime<Utc>>) -> AppResult<Ciclo> {
    // 1. Turno.
    let Some((turno, desde_ifpe, desde_sofom)) = tomar_turno(pool).await? else {
        return Ok(Ciclo::default());
    };
    let mut c = Ciclo { turno: true, hasta_ifpe: desde_ifpe, hasta_sofom: desde_sofom, ..Ciclo::default() };

    // 2-5. Clientes en cuarentena, reglas por lotes de los clientes que se
    // movieron y marca de agua.
    if let Err(err) = barrer_rango(pool, cfg, ahora, turno, &mut c).await {
        registrar_error(&mut c, "reglas por lotes", &err);
    }
    if !renovar_turno(pool, turno).await? {
        tracing::warn!(target: "aml", "otra instancia retomó el turno del monitoreo: el ciclo se detiene");
        return Ok(c);
    }

    // 6. Redes entre clientes (R26/R27); idempotente por semana.
    match escalar_i32(pool, "SELECT aml.barrer_redes($1::timestamptz)", ahora).await {
        Ok(n) => c.alertas_redes = i64::from(n),
        Err(err) => registrar_error(&mut c, "redes entre clientes", &err),
    }

    // 7. Marcas DIFERIDA y evaluaciones fallidas.
    if let Err(err) = reevaluar_marcas(pool, &mut c).await {
        registrar_error(&mut c, "reevaluación de marcas", &err);
    }

    // 8. Plazos (cada aviso una sola vez, por los índices únicos).
    match escalar_i32(pool, "SELECT aml.revisar_plazos($1::timestamptz)", ahora).await {
        Ok(n) => c.plazos = i64::from(n),
        Err(err) => registrar_error(&mut c, "revisión de plazos", &err),
    }

    // 9. Sellado de la bitácora (transacciones ya terminadas) y verificación
    // incremental desde el último sello verificado.
    match sellar(pool).await {
        Ok(n) => c.sellados = i64::from(n),
        Err(err) => registrar_error(&mut c, "sellado de la bitácora", &err),
    }
    match verificar_bitacora(pool).await {
        Ok(valida) => {
            c.bitacora_valida = valida;
            if !valida {
                tracing::error!(target: "aml", "la verificación de la bitácora de cumplimiento falló");
            }
        }
        Err(err) => registrar_error(&mut c, "verificación de la bitácora", &err),
    }

    // 10. Entrega de notificaciones.
    match entregar_notificaciones(pool, NOTIFICACIONES_POR_CICLO).await {
        Ok(n) => c.notificaciones = n,
        Err(err) => registrar_error(&mut c, "entrega de notificaciones", &err),
    }

    // 11. Libera el turno con el resultado (solo si sigue siendo nuestro).
    liberar_turno(pool, turno, &c).await?;
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

async fn tomar_turno(pool: &PgPool) -> AppResult<Option<(Uuid, i64, i64)>> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let marcas: Option<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT turno, ultima_secuencia_ifpe, ultima_secuencia_sofom FROM aml.monitoreo_tomar_turno($1)",
    )
    .bind(TURNO_SEG)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(marcas)
}

/// Renueva el arrendamiento; `false` si el turno ya no es de esta instancia.
async fn renovar_turno(pool: &PgPool, turno: Uuid) -> AppResult<bool> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let vigente: bool = sqlx::query_scalar("SELECT aml.monitoreo_renovar_turno($1, $2)")
        .bind(turno)
        .bind(TURNO_SEG)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(vigente)
}

async fn liberar_turno(pool: &PgPool, turno: Uuid, c: &Ciclo) -> AppResult<()> {
    let resultado = serde_json::to_value(c).map_err(anyhow::Error::from)?;
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let liberado: bool = sqlx::query_scalar("SELECT aml.monitoreo_liberar_turno($1, $2)")
        .bind(turno)
        .bind(resultado)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    if !liberado {
        tracing::warn!(target: "aml", "el turno del monitoreo ya era de otra instancia: no se libera");
    }
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

async fn verificar_bitacora(pool: &PgPool) -> AppResult<bool> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let valida: bool = sqlx::query_scalar("SELECT aml.registrar_verificacion_bitacora()").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(valida)
}

/// Pasos 2 a 5: reintento de los clientes en cuarentena, rango del ledger,
/// clientes candidatos (con sus anclas), reglas R20–R25 por cliente y, si
/// todos se revisaron o quedaron en cuarentena, avance de la marca de agua.
async fn barrer_rango(
    pool: &PgPool,
    cfg: &ConfigAml,
    ahora: Option<DateTime<Utc>>,
    turno: Uuid,
    c: &mut Ciclo,
) -> AppResult<()> {
    // 2. Clientes en cuarentena: se reintentan con el reloj del servidor; un
    // barrido exitoso los saca (aml.barrer_cliente borra su registro de fallos).
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let en_cuarentena: Vec<Uuid> = sqlx::query_scalar("SELECT aml.monitoreo_cuarentena()").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    for cliente in en_cuarentena {
        match barrer_cliente(pool, cliente, ahora, &[]).await {
            Ok(n) if n >= 0 => c.alertas_lote += i64::from(n),
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(target: "aml", %cliente, codigo = err.codigo(), "cliente en cuarentena: el barrido sigue fallando");
                registrar_fallo(pool, turno, cliente, &err).await?;
            }
        }
    }

    // 3. Rango (queda registrado con el turno) y candidatos con sus anclas.
    let lote = i32::try_from(cfg.monitoreo_lote.clamp(1, 100_000)).unwrap_or(5_000);
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let (hasta_ifpe, hasta_sofom): (i64, i64) =
        sqlx::query_as("SELECT hasta_ifpe, hasta_sofom FROM aml.monitoreo_rango($1, $2)")
            .bind(turno)
            .bind(lote)
            .fetch_one(&mut *tx)
            .await?;
    let candidatos: Vec<(Uuid, Vec<DateTime<Utc>>)> =
        sqlx::query_as("SELECT cliente_id, anclas FROM aml.monitoreo_candidatos($1)")
            .bind(turno)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    c.hasta_ifpe = hasta_ifpe;
    c.hasta_sofom = hasta_sofom;
    c.clientes = candidatos.len();

    // 4. Un cliente ocupado (otra transacción tiene su fila o su candado PLD
    // mientras se alertaba o congelaba) se reintenta una vez al final;
    // barrer_cliente nunca espera un candado. Uno que falla suma un fallo:
    // al tercer ciclo queda en cuarentena y deja de detener la marca.
    let mut sin_cuarentena = 0usize;
    let mut ocupados = Vec::new();
    for (i, (cliente, anclas)) in candidatos.iter().enumerate() {
        if i > 0 && i % TURNO_RENOVAR_CADA == 0 && !renovar_turno(pool, turno).await? {
            return Err(AppError::Integridad("el turno del monitoreo lo retomó otra instancia".into()));
        }
        match barrer_cliente(pool, *cliente, ahora, anclas).await {
            Ok(n) if n < 0 => ocupados.push((*cliente, anclas)),
            Ok(n) => c.alertas_lote += i64::from(n),
            Err(err) => {
                c.fallidos += 1;
                registrar_error(c, "reglas por lotes de un cliente", &err);
                if !registrar_fallo(pool, turno, *cliente, &err).await? {
                    sin_cuarentena += 1;
                }
            }
        }
    }
    let mut siguen_ocupados = 0usize;
    for (cliente, anclas) in ocupados {
        match barrer_cliente(pool, cliente, ahora, anclas).await {
            Ok(n) if n < 0 => siguen_ocupados += 1,
            Ok(n) => c.alertas_lote += i64::from(n),
            Err(err) => {
                c.fallidos += 1;
                registrar_error(c, "reglas por lotes de un cliente", &err);
                if !registrar_fallo(pool, turno, cliente, &err).await? {
                    sin_cuarentena += 1;
                }
            }
        }
    }
    c.ocupados = siguen_ocupados;
    let mut tx = db::begin(pool, Rol::Aml).await?;
    c.cuarentena = usize::try_from(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM aml.monitoreo_cuarentena()").fetch_one(&mut *tx).await?,
    )
    .unwrap_or(0);
    tx.commit().await?;

    // 5. Sin avanzar la marca, el siguiente ciclo revisa el mismo rango: es
    // seguro porque las alertas por lotes no se duplican (una abierta por
    // cliente, regla y semana; una cerrada solo cede ante evidencia posterior).
    if siguen_ocupados > 0 || sin_cuarentena > 0 {
        tracing::warn!(target: "aml", ocupados = siguen_ocupados, fallidos = sin_cuarentena, "la marca de agua del monitoreo no avanza en este ciclo");
        return Ok(());
    }
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let avanzo: bool =
        sqlx::query_scalar("SELECT aml.monitoreo_avanzar_marca($1)").bind(turno).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    if !avanzo {
        return Err(AppError::Integridad("el turno del monitoreo lo retomó otra instancia".into()));
    }
    Ok(())
}

/// Reglas por lotes de un cliente en su propia transacción, en sus anclas
/// (vacío: el reloj del servidor). `-1`: ocupado.
async fn barrer_cliente(
    pool: &PgPool,
    cliente: Uuid,
    ahora: Option<DateTime<Utc>>,
    anclas: &[DateTime<Utc>],
) -> AppResult<i32> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let n: i32 = sqlx::query_scalar("SELECT aml.barrer_cliente($1, $2, $3)")
        .bind(cliente)
        .bind(ahora)
        .bind(anclas)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(n)
}

/// Registra el fallo de un cliente; `true` si quedó (o ya estaba) en cuarentena.
async fn registrar_fallo(pool: &PgPool, turno: Uuid, cliente: Uuid, err: &AppError) -> AppResult<bool> {
    let mut tx = db::begin(pool, Rol::Aml).await?;
    let cuarentena: bool = sqlx::query_scalar("SELECT aml.monitoreo_registrar_fallo($1, $2, $3)")
        .bind(turno)
        .bind(cliente)
        .bind(format!("{}: {}", err.codigo(), err.detalle()))
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(cuarentena)
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
