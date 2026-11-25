//! Aclaraciones SPEI (roadmap 17, migración 0025): el cliente abre una sobre
//! uno de sus SPEI cuando se retrasa, no llega o regresa; el operador SPEI la
//! revisa y la resuelve o la rechaza con una nota que el cliente lee.
//!
//! - **Abrir** ([`abrir`], `POST /api/v1/wealth/spei/disputes`): rol
//!   `sc_ifpe`, una transacción. La cuenta IFPE del cliente primero (el primer
//!   candado del orden global), luego la idempotencia: la misma clave con los
//!   mismos términos devuelve la aclaración como está (200), con otros es 409.
//!   Reglas, antes de escribir: KYC verificado; el SPEI es suyo (si no, 404:
//!   para él no existe); a lo más [`PLAZO_DIAS`] días desde la operación; sin
//!   otra aclaración activa del mismo SPEI (409); a lo más [`MAXIMO_ACTIVAS`]
//!   activas. La base de datos las vuelve a exigir, pone el folio, la clave de
//!   rastreo y las fechas, y escribe el evento en la bitácora del cliente con
//!   su sesión, IP y dispositivo.
//! - **Listar** ([`listar`], `GET /api/v1/wealth/spei/disputes`): con el rol
//!   del cliente y RLS, la más reciente primero, cada una con su historia: los
//!   pasos que guarda la bitácora inmutable.
//! - **El operador** ([`cola`], [`cambiar_estado`], `/api/v1/operaciones/spei/aclaraciones`):
//!   rol `sc_spei`; la cola de las activas, la más antigua primero, y el paso
//!   a EN_REVISION, RESUELTA o RECHAZADA (con la cuenta del cliente bloqueada
//!   primero y la aclaración después). Cerrarla lleva una nota de 10 a 500
//!   caracteres, que la base de datos rechaza si nombra la prevención de
//!   lavado de dinero (el cliente la lee).
//!
//! No mueven dinero: un cliente con la cuenta congelada abre aclaraciones como
//! cualquiera (negarlo delataría el congelamiento).

use std::collections::HashMap;
use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::transferencias::Direccion;
use crate::{
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    ledger,
};

/// Aclaraciones activas por cliente, a lo más (`ifpe.maximo_aclaraciones_activas()`).
pub const MAXIMO_ACTIVAS: i64 = 10;
/// Días naturales desde la operación para abrir una (`ifpe.plazo_aclaracion_dias()`).
pub const PLAZO_DIAS: i64 = 90;
/// La descripción del cliente y la nota del operador, a lo más.
pub const MAXIMO_TEXTO: usize = 500;
/// Aclaraciones que lista el cliente, a lo más (las más recientes).
pub const MAXIMO_LISTA: i64 = 200;

/// Por qué la abre el cliente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Motivo {
    /// Se tarda más de lo normal.
    #[serde(rename = "delayed")]
    Retraso,
    /// Liquidado, pero el beneficiario no lo ve.
    #[serde(rename = "not_received")]
    NoRecibido,
    /// Regresó (una orden devuelta o rechazada, un depósito devuelto).
    #[serde(rename = "returned")]
    Devolucion,
    #[serde(rename = "wrong_amount")]
    MontoIncorrecto,
    /// Un depósito que el cliente no reconoce.
    #[serde(rename = "not_recognized")]
    NoReconocido,
    #[serde(rename = "other")]
    Otro,
}

impl Motivo {
    fn bd(self) -> &'static str {
        match self {
            Motivo::Retraso => "RETRASO",
            Motivo::NoRecibido => "NO_RECIBIDO",
            Motivo::Devolucion => "DEVOLUCION",
            Motivo::MontoIncorrecto => "MONTO_INCORRECTO",
            Motivo::NoReconocido => "NO_RECONOCIDO",
            Motivo::Otro => "OTRO",
        }
    }

    fn de_bd(valor: &str) -> AppResult<Self> {
        Ok(match valor {
            "RETRASO" => Motivo::Retraso,
            "NO_RECIBIDO" => Motivo::NoRecibido,
            "DEVOLUCION" => Motivo::Devolucion,
            "MONTO_INCORRECTO" => Motivo::MontoIncorrecto,
            "NO_RECONOCIDO" => Motivo::NoReconocido,
            "OTRO" => Motivo::Otro,
            otro => return Err(AppError::Integridad(format!("aclaración: motivo desconocido {otro}"))),
        })
    }
}

/// Dónde va: abierta, en revisión, resuelta o rechazada (`declined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EstadoAclaracion {
    #[serde(rename = "open")]
    Abierta,
    #[serde(rename = "in_review")]
    EnRevision,
    #[serde(rename = "resolved")]
    Resuelta,
    #[serde(rename = "declined")]
    Rechazada,
}

impl EstadoAclaracion {
    pub fn bd(self) -> &'static str {
        match self {
            EstadoAclaracion::Abierta => "ABIERTA",
            EstadoAclaracion::EnRevision => "EN_REVISION",
            EstadoAclaracion::Resuelta => "RESUELTA",
            EstadoAclaracion::Rechazada => "RECHAZADA",
        }
    }

    pub fn de_bd(valor: &str) -> AppResult<Self> {
        Ok(match valor {
            "ABIERTA" => EstadoAclaracion::Abierta,
            "EN_REVISION" => EstadoAclaracion::EnRevision,
            "RESUELTA" => EstadoAclaracion::Resuelta,
            "RECHAZADA" => EstadoAclaracion::Rechazada,
            otro => return Err(AppError::Integridad(format!("aclaración: estado desconocido {otro}"))),
        })
    }

    pub fn activa(self) -> bool {
        matches!(self, EstadoAclaracion::Abierta | EstadoAclaracion::EnRevision)
    }

    /// La máquina de estados de 0025.
    fn puede_pasar_a(self, nuevo: EstadoAclaracion) -> bool {
        matches!(
            (self, nuevo),
            (
                EstadoAclaracion::Abierta,
                EstadoAclaracion::EnRevision | EstadoAclaracion::Resuelta | EstadoAclaracion::Rechazada
            ) | (EstadoAclaracion::EnRevision, EstadoAclaracion::Resuelta | EstadoAclaracion::Rechazada)
        )
    }
}

/// Un paso de su historia, de la bitácora.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Paso {
    pub status: EstadoAclaracion,
    #[serde(rename = "at")]
    pub en: DateTime<Utc>,
}

/// Una aclaración como la ve su cliente (`SpeiClarification`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VistaAclaracion {
    #[serde(rename = "clarification_id")]
    pub id: Uuid,
    /// Su folio: el cliente lo usa al hablar de ella.
    #[serde(rename = "number")]
    pub numero: i32,
    #[serde(rename = "transfer_id")]
    pub transferencia_id: Uuid,
    #[serde(rename = "direction")]
    pub direccion: Direccion,
    #[serde(rename = "tracking_key")]
    pub clave_rastreo: String,
    #[serde(rename = "reason")]
    pub motivo: Motivo,
    #[serde(rename = "description")]
    pub descripcion: Option<String>,
    pub status: EstadoAclaracion,
    /// La nota del operador (neutral: la base de datos lo exige).
    #[serde(rename = "resolution_note")]
    pub nota: Option<String>,
    #[serde(rename = "opened_at")]
    pub creada_en: DateTime<Utc>,
    #[serde(rename = "updated_at")]
    pub actualizada_en: DateTime<Utc>,
    #[serde(rename = "closed_at")]
    pub cerrada_en: Option<DateTime<Utc>>,
    /// Abierta, en revisión, cerrada: cuándo, de la bitácora.
    #[serde(rename = "history")]
    pub historia: Vec<Paso>,
}

/// `GET /api/v1/wealth/spei/disputes` (`SpeiClarifications`).
#[derive(Debug, Clone, Serialize)]
pub struct Aclaraciones {
    #[serde(rename = "clarifications")]
    pub aclaraciones: Vec<VistaAclaracion>,
}

/// Registro de `ifpe.aclaraciones_spei`.
#[derive(Debug, Clone, FromRow)]
pub struct Aclaracion {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub numero: i32,
    pub spei_outbound_id: Option<Uuid>,
    pub spei_inbound_id: Option<Uuid>,
    pub clave_rastreo: String,
    pub motivo: String,
    pub descripcion: Option<String>,
    pub estado: String,
    pub nota_resolucion: Option<String>,
    pub clave_idempotencia: String,
    pub creada_en: DateTime<Utc>,
    pub actualizada_en: DateTime<Utc>,
    pub cerrada_en: Option<DateTime<Utc>>,
}

impl Aclaracion {
    fn vista(&self, historia: Vec<Paso>) -> AppResult<VistaAclaracion> {
        let (transferencia_id, direccion) = match (self.spei_outbound_id, self.spei_inbound_id) {
            (Some(id), None) => (id, Direccion::Saliente),
            (None, Some(id)) => (id, Direccion::Entrante),
            _ => return Err(AppError::Integridad(format!("aclaración {}: sin un solo movimiento", self.id))),
        };
        Ok(VistaAclaracion {
            id: self.id,
            numero: self.numero,
            transferencia_id,
            direccion,
            clave_rastreo: self.clave_rastreo.clone(),
            motivo: Motivo::de_bd(&self.motivo)?,
            descripcion: self.descripcion.clone(),
            status: EstadoAclaracion::de_bd(&self.estado)?,
            nota: self.nota_resolucion.clone(),
            creada_en: self.creada_en,
            actualizada_en: self.actualizada_en,
            cerrada_en: self.cerrada_en,
            historia,
        })
    }
}

/// Un texto libre para el otro lado: sin espacios de más ni caracteres de
/// control, a lo más [`MAXIMO_TEXTO`] caracteres. Vacío: nada.
fn texto(valor: Option<&str>, que: &str) -> AppResult<Option<String>> {
    let Some(valor) = valor.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    if valor.chars().count() > MAXIMO_TEXTO {
        return Err(AppError::validacion(format!("{que} admite como máximo {MAXIMO_TEXTO} caracteres")));
    }
    if valor.chars().any(|c| c.is_control() && c != '\n') {
        return Err(AppError::validacion(format!("{que} no admite caracteres de control")));
    }
    Ok(Some(valor.to_owned()))
}

/// Las aclaraciones del cliente, la más reciente primero, con su historia.
pub async fn listar(pool: &PgPool, cliente_id: Uuid) -> AppResult<Aclaraciones> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let filas: Vec<Aclaracion> =
        sqlx::query_as("SELECT * FROM ifpe.aclaraciones_spei WHERE cliente_id = $1 ORDER BY numero DESC LIMIT $2")
            .bind(cliente_id)
            .bind(MAXIMO_LISTA)
            .fetch_all(&mut *tx)
            .await?;
    let pasos: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT detalle, evento, ocurrido_en FROM kyc.eventos_autenticacion \
          WHERE cliente_id = $1 AND evento LIKE 'ACLARACION\\_SPEI\\_%' ORDER BY id",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut historias: HashMap<String, Vec<Paso>> = HashMap::new();
    for (folio, evento, en) in pasos {
        historias.entry(folio).or_default().push(paso(&evento, en)?);
    }
    let aclaraciones = filas
        .iter()
        .map(|a| a.vista(historias.remove(&a.numero.to_string()).unwrap_or_default()))
        .collect::<AppResult<Vec<_>>>()?;
    Ok(Aclaraciones { aclaraciones })
}

/// Lo que pide el cliente (ya validado en la ruta: el motivo y la dirección).
#[derive(Debug, Clone)]
pub struct SolicitudAclaracion {
    pub cliente_id: Uuid,
    pub transferencia_id: Uuid,
    pub direccion: Direccion,
    pub motivo: Motivo,
    pub descripcion: Option<String>,
    /// Con el prefijo del rol (`cli:...`).
    pub clave_idempotencia: String,
    pub sesion_id: Option<Uuid>,
    pub ip: Option<IpAddr>,
    pub dispositivo: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResultadoAclaracion {
    pub aclaracion: VistaAclaracion,
    /// `true` si la clave ya existía y se devolvió la aclaración original.
    pub idempotente: bool,
}

/// Abre una aclaración sobre un SPEI del cliente.
pub async fn abrir(pool: &PgPool, s: &SolicitudAclaracion) -> AppResult<ResultadoAclaracion> {
    let descripcion = texto(s.descripcion.as_deref(), "la descripción")?;
    if s.motivo == Motivo::Otro && descripcion.as_ref().map_or(true, |d| d.chars().count() < 10) {
        return Err(AppError::validacion("con el motivo «other», cuéntanos qué pasó (al menos 10 caracteres)"));
    }
    let (id, idempotente) = db::con_reintentos(|| abrir_tx(pool, s, descripcion.as_deref())).await?;
    // Como la lee el cliente: con su rol y RLS, y la historia de su bitácora.
    let aclaracion = obtener(pool, s.cliente_id, id).await?;
    Ok(ResultadoAclaracion { aclaracion, idempotente })
}

/// Una aclaración del cliente, con su historia; 404 si no es suya.
pub async fn obtener(pool: &PgPool, cliente_id: Uuid, id: Uuid) -> AppResult<VistaAclaracion> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let fila: Option<Aclaracion> =
        sqlx::query_as("SELECT * FROM ifpe.aclaraciones_spei WHERE id = $1 AND cliente_id = $2")
            .bind(id)
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(fila) = fila else { return Err(AppError::no_encontrado("la aclaración no existe")) };
    let historia = historia_de(&mut tx, &fila).await?;
    tx.commit().await?;
    fila.vista(historia)
}

/// Abre la aclaración (o encuentra la de la misma clave): su id, y si ya existía.
async fn abrir_tx(pool: &PgPool, s: &SolicitudAclaracion, descripcion: Option<&str>) -> AppResult<(Uuid, bool)> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    // La cuenta primero (orden global de candados): las aclaraciones de un
    // cliente se abren de una en una.
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;

    let (salida, entrada) = match s.direccion {
        Direccion::Saliente => (Some(s.transferencia_id), None),
        Direccion::Entrante => (None, Some(s.transferencia_id)),
    };
    let previa: Option<Aclaracion> =
        sqlx::query_as("SELECT * FROM ifpe.aclaraciones_spei WHERE clave_idempotencia = $1")
            .bind(&s.clave_idempotencia)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(previa) = previa {
        if previa.cliente_id != s.cliente_id
            || (previa.spei_outbound_id, previa.spei_inbound_id) != (salida, entrada)
            || previa.motivo != s.motivo.bd()
            || previa.descripcion.as_deref() != descripcion
        {
            return Err(AppError::conflicto(
                "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                "la clave de idempotencia ya se usó para otra aclaración",
            ));
        }
        tx.commit().await?;
        return Ok((previa.id, true));
    }

    let estatus: Option<String> = sqlx::query_scalar("SELECT estatus_kyc FROM kyc.clientes WHERE id = $1")
        .bind(s.cliente_id)
        .fetch_optional(&mut *tx)
        .await?;
    if estatus.as_deref() != Some("VERIFICADO") {
        return Err(AppError::negocio("KYC_NO_VERIFICADO", "abre una aclaración cuando tu identidad esté verificada"));
    }
    // El SPEI: suyo, y de cuándo. Para el cliente, uno ajeno no existe.
    let operacion: Option<DateTime<Utc>> = match s.direccion {
        Direccion::Saliente => {
            sqlx::query_scalar("SELECT creado_en FROM ifpe.spei_outbound WHERE id = $1 AND cliente_id = $2")
                .bind(s.transferencia_id)
                .bind(s.cliente_id)
                .fetch_optional(&mut *tx)
                .await?
        }
        Direccion::Entrante => {
            sqlx::query_scalar(
                "SELECT i.recibido_en FROM ifpe.spei_inbound i JOIN ifpe.cuentas c ON c.id = i.cuenta_id \
                  WHERE i.id = $1 AND c.cliente_id = $2 AND i.estado IN ('ABONADO', 'DEVUELTO')",
            )
            .bind(s.transferencia_id)
            .bind(s.cliente_id)
            .fetch_optional(&mut *tx)
            .await?
        }
    };
    let Some(operacion) = operacion else { return Err(AppError::no_encontrado("el SPEI no existe")) };
    if operacion < Utc::now() - Duration::days(PLAZO_DIAS) {
        return Err(AppError::negocio(
            "ACLARACION_VENCIDA",
            format!("una aclaración se abre dentro de los {PLAZO_DIAS} días naturales siguientes a la operación"),
        ));
    }
    let activa: Option<i32> = sqlx::query_scalar(
        "SELECT numero FROM ifpe.aclaraciones_spei \
          WHERE estado IN ('ABIERTA', 'EN_REVISION') AND (spei_outbound_id = $1 OR spei_inbound_id = $2)",
    )
    .bind(salida)
    .bind(entrada)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(numero) = activa {
        return Err(AppError::conflicto(
            "ACLARACION_ACTIVA",
            format!("ese SPEI ya tiene una aclaración activa (folio {numero})"),
        ));
    }
    let activas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ifpe.aclaraciones_spei WHERE cliente_id = $1 AND estado IN ('ABIERTA', 'EN_REVISION')",
    )
    .bind(s.cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    if activas >= MAXIMO_ACTIVAS {
        return Err(AppError::negocio(
            "LIMITE_ACLARACIONES",
            format!("ya tienes {MAXIMO_ACTIVAS} aclaraciones activas; espera a que se resuelva alguna"),
        ));
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_outbound_id, spei_inbound_id, motivo, descripcion, \
                                             clave_idempotencia, sesion_id, ip, dispositivo) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::inet, $10) RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(s.cliente_id)
    .bind(salida)
    .bind(entrada)
    .bind(s.motivo.bd())
    .bind(descripcion)
    .bind(&s.clave_idempotencia)
    .bind(s.sesion_id)
    .bind(s.ip.map(|i| i.to_string()))
    .bind(s.dispositivo.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((id, false))
}

/// La historia de una aclaración, de la bitácora del cliente (con su rol).
async fn historia_de(tx: &mut Tx, a: &Aclaracion) -> AppResult<Vec<Paso>> {
    let pasos: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT evento, ocurrido_en FROM kyc.eventos_autenticacion \
          WHERE cliente_id = $1 AND detalle = $2 AND evento LIKE 'ACLARACION\\_SPEI\\_%' ORDER BY id",
    )
    .bind(a.cliente_id)
    .bind(a.numero.to_string())
    .fetch_all(&mut **tx)
    .await?;
    pasos.into_iter().map(|(evento, en)| paso(&evento, en)).collect()
}

/// Un evento de la bitácora como paso de la historia.
fn paso(evento: &str, en: DateTime<Utc>) -> AppResult<Paso> {
    let estado = evento
        .strip_prefix("ACLARACION_SPEI_")
        .ok_or_else(|| AppError::Integridad(format!("aclaración: evento desconocido {evento}")))?;
    Ok(Paso { status: EstadoAclaracion::de_bd(estado)?, en })
}

// -----------------------------------------------------------------------------
// El operador SPEI
// -----------------------------------------------------------------------------

/// Una aclaración en la cola del operador.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AclaracionOperador {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub numero: i32,
    pub spei_outbound_id: Option<Uuid>,
    pub spei_inbound_id: Option<Uuid>,
    pub clave_rastreo: String,
    pub motivo: String,
    pub descripcion: Option<String>,
    pub estado: String,
    pub nota_resolucion: Option<String>,
    pub creada_en: DateTime<Utc>,
    pub actualizada_en: DateTime<Utc>,
    pub cerrada_en: Option<DateTime<Utc>>,
}

/// Las aclaraciones activas, la más antigua primero.
pub async fn cola(pool: &PgPool) -> AppResult<Vec<AclaracionOperador>> {
    let mut tx = db::begin_lectura(pool, Rol::Spei).await?;
    let filas = sqlx::query_as(
        "SELECT id, cliente_id, numero, spei_outbound_id, spei_inbound_id, clave_rastreo, motivo, descripcion, estado, \
                nota_resolucion, creada_en, actualizada_en, cerrada_en \
           FROM ifpe.aclaraciones_spei WHERE estado IN ('ABIERTA', 'EN_REVISION') ORDER BY creada_en, id LIMIT 500",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(filas)
}

/// La pasa a EN_REVISION, RESUELTA o RECHAZADA. Cerrarla lleva nota.
pub async fn cambiar_estado(
    pool: &PgPool,
    id: Uuid,
    nuevo: EstadoAclaracion,
    nota: Option<&str>,
) -> AppResult<AclaracionOperador> {
    let nota = texto(nota, "la nota")?;
    if !nuevo.activa() && nota.as_ref().map_or(true, |n| n.chars().count() < 10) {
        return Err(AppError::validacion("cerrar una aclaración lleva una nota para el cliente (10 a 500 caracteres)"));
    }
    if nota.as_ref().is_some_and(|n| n.chars().count() < 10) {
        return Err(AppError::validacion("la nota para el cliente lleva de 10 a 500 caracteres"));
    }
    db::con_reintentos(|| cambiar_estado_tx(pool, id, nuevo, nota.as_deref())).await
}

async fn cambiar_estado_tx(
    pool: &PgPool,
    id: Uuid,
    nuevo: EstadoAclaracion,
    nota: Option<&str>,
) -> AppResult<AclaracionOperador> {
    let mut tx = db::begin(pool, Rol::Spei).await?;
    let cliente_id: Option<Uuid> = sqlx::query_scalar("SELECT cliente_id FROM ifpe.aclaraciones_spei WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(cliente_id) = cliente_id else { return Err(AppError::no_encontrado("la aclaración no existe")) };
    // La cuenta del cliente primero (orden global de candados), luego la aclaración.
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, cliente_id).await?;
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let actual: String = sqlx::query_scalar("SELECT estado FROM ifpe.aclaraciones_spei WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let actual = EstadoAclaracion::de_bd(&actual)?;
    if !actual.activa() {
        return Err(AppError::conflicto("ACLARACION_CERRADA", "la aclaración ya se cerró"));
    }
    if !actual.puede_pasar_a(nuevo) {
        return Err(AppError::conflicto(
            "ACLARACION_TRANSICION",
            format!("la aclaración no pasa de {} a {}", actual.bd(), nuevo.bd()),
        ));
    }
    let fila: AclaracionOperador = sqlx::query_as(
        "UPDATE ifpe.aclaraciones_spei SET estado = $2, nota_resolucion = coalesce($3, nota_resolucion) WHERE id = $1 \
         RETURNING id, cliente_id, numero, spei_outbound_id, spei_inbound_id, clave_rastreo, motivo, descripcion, \
                   estado, nota_resolucion, creada_en, actualizada_en, cerrada_en",
    )
    .bind(id)
    .bind(nuevo.bd())
    .bind(nota)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fila)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_maquina_de_estados_es_la_de_la_base_de_datos() {
        use EstadoAclaracion::*;
        for (de, a, puede) in [
            (Abierta, EnRevision, true),
            (Abierta, Resuelta, true),
            (Abierta, Rechazada, true),
            (EnRevision, Resuelta, true),
            (EnRevision, Rechazada, true),
            (EnRevision, EnRevision, false),
            (EnRevision, Abierta, false),
            (Abierta, Abierta, false),
            (Resuelta, Rechazada, false),
            (Rechazada, EnRevision, false),
        ] {
            assert_eq!(de.puede_pasar_a(a), puede, "{de:?} -> {a:?}");
        }
        for e in [Abierta, EnRevision, Resuelta, Rechazada] {
            assert_eq!(EstadoAclaracion::de_bd(e.bd()).unwrap(), e);
        }
        assert!(Abierta.activa() && EnRevision.activa() && !Resuelta.activa() && !Rechazada.activa());
    }

    #[test]
    fn los_motivos_y_su_json() {
        for m in [
            Motivo::Retraso,
            Motivo::NoRecibido,
            Motivo::Devolucion,
            Motivo::MontoIncorrecto,
            Motivo::NoReconocido,
            Motivo::Otro,
        ] {
            assert_eq!(Motivo::de_bd(m.bd()).unwrap(), m);
        }
        assert_eq!(serde_json::to_value(Motivo::NoRecibido).unwrap(), "not_received");
        assert_eq!(serde_json::to_value(EstadoAclaracion::Rechazada).unwrap(), "declined");
        assert_eq!(serde_json::from_value::<Motivo>("wrong_amount".into()).unwrap(), Motivo::MontoIncorrecto);
        assert!(serde_json::from_value::<Motivo>("fraud".into()).is_err());
    }

    #[test]
    fn el_texto_se_recorta_y_tiene_limite() {
        assert_eq!(texto(Some("  Lo mandé ayer.  "), "x").unwrap().as_deref(), Some("Lo mandé ayer."));
        assert_eq!(texto(Some("   "), "x").unwrap(), None);
        assert_eq!(texto(None, "x").unwrap(), None);
        assert!(texto(Some(&"a".repeat(501)), "x").is_err());
        assert!(texto(Some(&"ñ".repeat(500)), "x").is_ok(), "caracteres, no bytes");
        assert!(texto(Some("con\u{0007}campana"), "x").is_err());
        assert_eq!(texto(Some("dos\nrenglones"), "x").unwrap().as_deref(), Some("dos\nrenglones"));
    }
}
