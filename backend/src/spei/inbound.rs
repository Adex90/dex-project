//! SPEI recibidos: notificación del proveedor (webhook firmado) o captura
//! manual del operador. Ambos caminos pasan por [`abonar`], así que comparten
//! una sola identidad de pago y un pago nunca se abona dos veces.
//!
//! Una sola transacción con el rol `sc_ifpe`:
//!
//! 1. candado consultivo sobre la identidad del pago (emisor + fecha + rastreo):
//!    todas las notificaciones del mismo pago se procesan de una en una, sin
//!    importar a qué CLABE digan ir;
//! 2. si el pago ya se registró, se devuelve el resultado original (o un
//!    conflicto si la misma identidad llega con otro monto o CLABE). Si el
//!    registro vigente es una captura anulada por el operador, el pago se
//!    registra como su reemplazo (`reemplaza_a`);
//! 3. se resuelve la CLABE receptora en `ifpe.clabes_asignadas` y se bloquea la
//!    cuenta; si no puede recibir (inexistente, cancelada, cliente bloqueado o
//!    sin KYC) el pago se registra DEVUELTO con su causa Banxico y no toca
//!    saldos: el proveedor regresa el dinero al emisor;
//! 4. si puede recibir: saldo del cliente y reserva en custodia suben juntos,
//!    se inserta el DEPOSITO (con su `tx_hash`) y el registro ABONADO ligado a
//!    él, se evalúan reglas PLD y COMMIT. La base de datos verifica al COMMIT
//!    la reserva del 100%, la sombra del ledger y la liga SPEI <-> DEPOSITO.
//!
//! Además:
//! - una **devolución de una orden nuestra** (cualquier tipo de pago de
//!   devolución: 0, 16, 17, 18, 23, 24, o una notificación con clave de
//!   rastreo de devolución) no se abona como depósito: se aplica a la orden,
//!   que reembolsa una sola vez aunque el proveedor también avise por el
//!   webhook de estado. La base de datos rechaza un ABONADO que sea una
//!   devolución (`ck_spei_inbound_abonado_no_es_devolucion`);
//! - antes de abonar se buscan **posibles duplicados con otra identidad**
//!   (mismo rastreo, monto y cuenta, emisor con los mismos 3 últimos dígitos,
//!   fecha a ±7 días; o un abono anterior a 0005 con la misma clave de
//!   rastreo): una captura manual con la fecha o la clave de emisor
//!   equivocadas, o un abono anterior a 0005, no se vuelve a acreditar. Dos
//!   identidades informadas por el proveedor nunca son duplicado una de otra.
//!   El operador puede declarar que un pago es distinto ([`abonar_pago_distinto`]);
//! - el webhook nunca responde un 409 sin registro: un posible duplicado o una
//!   identidad reutilizada se retiene ([`retener`]) y se confirma; el
//!   operador lo resuelve;
//! - una captura manual errónea se anula ([`anular`]: corrección total) y su
//!   identidad queda libre para el pago real;
//! - todo corre con el rol `sc_spei`, que ninguna ruta del cliente usa.

use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, value::RawValue, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{clabe, outbound, validar_monto_spei, CausaDevolucion, IdentidadSpei};
use crate::{
    config::{ConfigSpei, UmbralesPld},
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    ledger,
    models::{ifpe::TransaccionIfpe, Entidad, EstadoCuenta},
    pld::{self, EventoPld},
};

/// Registro de `ifpe.spei_inbound`.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SpeiInbound {
    pub id: Uuid,
    pub institucion_emisora: String,
    pub clave_rastreo: String,
    pub fecha_operacion: NaiveDate,
    pub institucion_receptora: Option<String>,
    pub receptor_clabe: String,
    pub cuenta_id: Option<Uuid>,
    pub monto: Decimal,
    pub nombre_ordenante: Option<String>,
    pub cuenta_ordenante: Option<String>,
    pub rfc_curp_ordenante: Option<String>,
    pub referencia_numerica: Option<i32>,
    pub concepto: Option<String>,
    pub tipo_pago: Option<i16>,
    pub estado: String,
    pub causa_devolucion: Option<i16>,
    pub motivo: Option<String>,
    pub transaccion_id: Option<Uuid>,
    pub spei_outbound_id: Option<Uuid>,
    pub origen: String,
    pub registrado_por: Option<Uuid>,
    pub id_proveedor: Option<String>,
    #[serde(skip)]
    pub raw_payload: Value,
    pub recibido_en: DateTime<Utc>,
    /// Abono previo parecido del que el operador declaró distinto este pago.
    pub distinto_de_tx: Option<Uuid>,
    /// Registro anulado de la misma identidad al que este reemplaza.
    pub reemplaza_a: Option<Uuid>,
}

/// SPEI entrante ya validado.
#[derive(Debug, Clone)]
pub struct Abono {
    pub identidad: IdentidadSpei,
    pub institucion_receptora: Option<String>,
    /// Cuenta beneficiaria tal como llegó (dígitos). Si no es una CLABE nuestra
    /// válida, el pago se devuelve.
    pub receptor_clabe: String,
    pub monto: Decimal,
    pub nombre_ordenante: Option<String>,
    pub cuenta_ordenante: Option<String>,
    pub rfc_curp_ordenante: Option<String>,
    pub referencia_numerica: Option<i32>,
    pub concepto: Option<String>,
    /// Tipo de pago SPEI informado por el participante (ver
    /// [`TIPOS_PAGO_DEVOLUCION`]).
    pub tipo_pago: Option<i16>,
    /// En una devolución: la clave de rastreo de nuestra orden original.
    pub rastreo_devuelto: Option<String>,
    /// En una devolución: causa Banxico.
    pub causa_devolucion: Option<i16>,
    pub id_proveedor: Option<String>,
    /// Notificación completa, para auditoría.
    pub raw_payload: Value,
}

/// Tipos de pago SPEI de devolución (catálogo Banxico): 0 no acreditada, 16
/// extemporánea no acreditada, 17 acreditada, 18 extemporánea acreditada, 23
/// especial acreditada, 24 extemporánea especial acreditada.
pub const TIPOS_PAGO_DEVOLUCION: [i16; 6] = [0, 16, 17, 18, 23, 24];

impl Abono {
    /// Una devolución (de una orden nuestra) nunca se abona como depósito:
    /// cualquier tipo de pago de devolución, o la clave de rastreo del pago
    /// devuelto.
    pub fn es_devolucion(&self) -> bool {
        self.tipo_pago.is_some_and(|t| TIPOS_PAGO_DEVOLUCION.contains(&t)) || self.rastreo_devuelto.is_some()
    }
}

/// Devolución que no corresponde a ninguna orden (el webhook la pone en
/// cuarentena y la confirma: SPEI no permite devolver una devolución).
pub const DEVOLUCION_SIN_ORDEN: &str = "SPEI_DEVOLUCION_SIN_ORDEN";
/// Posible duplicado con otra identidad (el webhook lo retiene y confirma).
pub const POSIBLE_DUPLICADO: &str = "SPEI_POSIBLE_DUPLICADO";
/// Identidad ya registrada con otro monto o CLABE (el webhook la retiene y
/// confirma).
pub const IDENTIDAD_REUTILIZADA: &str = "SPEI_IDENTIDAD_REUTILIZADA";

/// El operador declara que el pago es distinto de un abono previo parecido.
#[derive(Debug, Clone)]
pub struct PagoDistinto {
    /// El DEPOSITO previo que señaló la búsqueda de posibles duplicados.
    pub transaccion_id: Uuid,
    pub motivo: String,
}

/// Datos del registro que no vienen de la notificación.
#[derive(Debug, Clone, Copy, Default)]
struct Vinculos<'a> {
    reemplaza_a: Option<Uuid>,
    distinto: Option<&'a PagoDistinto>,
}

/// Quién registra el pago.
#[derive(Debug, Clone, Copy)]
pub enum Origen {
    Webhook,
    /// Captura manual (conciliación) por el operador indicado.
    Operador(Uuid),
    /// Depósito simulado de `MODO_DEMO`.
    Demo,
}

/// Qué hacer si la cuenta destino no puede recibir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiNoAcreditable {
    /// Webhook: registrar DEVUELTO e indicar al proveedor que devuelva el pago.
    Devolver,
    /// Operador: rechazar la captura con el error de negocio (no se registra).
    Rechazar,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoAbono {
    pub spei: SpeiInbound,
    /// El DEPOSITO del ledger (ausente si el pago se devolvió).
    pub transaccion: Option<TransaccionIfpe>,
    /// `true` si el pago ya estaba registrado y se devolvió el resultado original.
    pub idempotente: bool,
}

impl ResultadoAbono {
    pub fn abonado(&self) -> bool {
        self.spei.estado == "ABONADO"
    }

    /// Respuesta al proveedor en el formato de STP: `confirmar` (abonado o
    /// devolución de una orden aplicada) o `devolver` con la causa.
    pub fn respuesta_proveedor(&self) -> Value {
        let mut r = if self.spei.estado == "DEVUELTO" {
            json!({ "mensaje": "devolver", "id": self.spei.causa_devolucion })
        } else {
            json!({ "mensaje": "confirmar" })
        };
        r["spei_id"] = json!(self.spei.id);
        r["estado"] = json!(self.spei.estado);
        r["idempotente"] = json!(self.idempotente);
        r
    }
}

// -----------------------------------------------------------------------------
// Notificación del proveedor
// -----------------------------------------------------------------------------

/// Notificación de abono con los campos de STP (`camelCase`). Los campos
/// desconocidos se aceptan (se conservan en `raw_payload`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificacionAbono {
    #[serde(default)]
    id: Option<Value>,
    fecha_operacion: Value,
    institucion_ordenante: Value,
    #[serde(default)]
    institucion_beneficiaria: Option<Value>,
    clave_rastreo: String,
    /// Texto JSON exacto del monto: nunca pasa por f64.
    monto: Box<RawValue>,
    #[serde(default)]
    nombre_ordenante: Option<String>,
    #[serde(default)]
    cuenta_ordenante: Option<Value>,
    #[serde(default)]
    rfc_curp_ordenante: Option<String>,
    cuenta_beneficiario: Value,
    #[serde(default)]
    concepto_pago: Option<String>,
    #[serde(default)]
    referencia_numerica: Option<Value>,
    #[serde(default)]
    tipo_pago: Option<Value>,
    /// Devoluciones: clave de rastreo del pago original que se devuelve.
    #[serde(default)]
    clave_rastreo_devolucion: Option<String>,
    #[serde(default)]
    causa_devolucion: Option<Value>,
}

/// Texto de un campo que el proveedor puede mandar como número o como cadena.
fn como_texto(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) if n.is_u64() => Some(n.to_string()),
        _ => None,
    }
}

/// Fecha de operación: `20261013`, `"20261013"` o `"2026-10-13"`.
fn fecha_operacion(v: &Value) -> AppResult<NaiveDate> {
    let texto = como_texto(v).ok_or_else(|| AppError::validacion("fechaOperacion inválida"))?;
    NaiveDate::parse_from_str(&texto, "%Y%m%d")
        .or_else(|_| NaiveDate::parse_from_str(&texto, "%Y-%m-%d"))
        .map_err(|_| AppError::validacion("fechaOperacion inválida (AAAAMMDD)"))
}

/// Monto exacto desde el texto JSON (`1500.25` o `"1500.25"`).
fn monto_exacto(crudo: &RawValue) -> AppResult<Decimal> {
    let texto = crudo.get().trim();
    let texto = match serde_json::from_str::<String>(texto) {
        Ok(cadena) => cadena,
        Err(_) => texto.to_owned(),
    };
    if texto.contains(['e', 'E']) {
        return Err(AppError::validacion("monto en notación científica no admitido"));
    }
    let monto = Decimal::from_str(&texto).map_err(|_| AppError::validacion("monto inválido"))?;
    validar_monto_spei(monto)
}

/// Texto libre del ordenante: sin caracteres de control y acotado.
fn texto_libre(v: Option<String>, max: usize) -> Option<String> {
    let limpio: String = v?.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let limpio = limpio.trim();
    (!limpio.is_empty()).then(|| limpio.chars().take(max).collect())
}

fn solo_digitos(v: Option<String>, max: usize) -> Option<String> {
    v.filter(|s| !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit()))
}

/// Interpreta y valida la notificación (ya autenticada). Identidad, monto y
/// cuenta beneficiaria son obligatorios; los datos informativos inválidos se
/// descartan (quedan en `raw_payload`) en vez de impedir el abono.
pub fn interpretar_notificacion(cuerpo: &[u8]) -> AppResult<Abono> {
    let raw_payload: Value =
        serde_json::from_slice(cuerpo).map_err(|e| AppError::validacion(format!("notificación SPEI ilegible: {e}")))?;
    let n: NotificacionAbono = serde_json::from_slice(cuerpo)
        .map_err(|e| AppError::validacion(format!("notificación SPEI incompleta: {e}")))?;
    let emisor =
        como_texto(&n.institucion_ordenante).ok_or_else(|| AppError::validacion("institucionOrdenante inválida"))?;
    let identidad = IdentidadSpei::nueva(&emisor, fecha_operacion(&n.fecha_operacion)?, &n.clave_rastreo)?;
    let receptor_clabe = como_texto(&n.cuenta_beneficiario)
        .filter(|c| !c.is_empty() && c.len() <= 20 && c.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| AppError::validacion("cuentaBeneficiario inválida"))?;
    Ok(Abono {
        identidad,
        institucion_receptora: n
            .institucion_beneficiaria
            .as_ref()
            .and_then(como_texto)
            .filter(|c| c.len() == 5 && c.bytes().all(|b| b.is_ascii_digit())),
        receptor_clabe,
        monto: monto_exacto(&n.monto)?,
        nombre_ordenante: texto_libre(n.nombre_ordenante, 120),
        cuenta_ordenante: solo_digitos(n.cuenta_ordenante.as_ref().and_then(como_texto), 20),
        rfc_curp_ordenante: texto_libre(n.rfc_curp_ordenante, 18).map(|s| s.to_uppercase()),
        referencia_numerica: n
            .referencia_numerica
            .as_ref()
            .and_then(como_texto)
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|r| (0..=9_999_999).contains(r)),
        concepto: texto_libre(n.concepto_pago, 210),
        tipo_pago: n
            .tipo_pago
            .as_ref()
            .and_then(como_texto)
            .and_then(|s| s.parse::<i16>().ok())
            .filter(|t| (0..=99).contains(t)),
        rastreo_devuelto: n
            .clave_rastreo_devolucion
            .map(|r| r.trim().to_ascii_uppercase())
            .filter(|r| !r.is_empty() && r.len() <= 30 && r.bytes().all(|b| b.is_ascii_alphanumeric())),
        causa_devolucion: n
            .causa_devolucion
            .as_ref()
            .and_then(como_texto)
            .and_then(|s| s.parse::<i16>().ok())
            .filter(|c| (1..=99).contains(c)),
        id_proveedor: n.id.as_ref().and_then(como_texto).filter(|s| s.len() <= 64),
        raw_payload,
    })
}

// -----------------------------------------------------------------------------
// Abono
// -----------------------------------------------------------------------------

pub async fn abonar(
    pool: &PgPool,
    umbrales: &UmbralesPld,
    cfg: &ConfigSpei,
    abono: &Abono,
    origen: Origen,
    si_no_acreditable: SiNoAcreditable,
) -> AppResult<ResultadoAbono> {
    validar_monto_spei(abono.monto)?;
    db::con_reintentos(|| abonar_tx(pool, umbrales, cfg, abono, origen, si_no_acreditable, None)).await
}

/// Captura del operador de un pago que la búsqueda de posibles duplicados
/// confunde con un abono previo (`distinto.transaccion_id`): el operador
/// confirmó que es otro pago. Solo se ignora ese abono; queda auditado en el
/// registro (`distinto_de_tx`, `motivo`, `registrado_por`) y la base de datos
/// exige que el abono previo sea de la misma cuenta, monto y clave de rastreo.
pub async fn abonar_pago_distinto(
    pool: &PgPool,
    umbrales: &UmbralesPld,
    cfg: &ConfigSpei,
    abono: &Abono,
    operador_id: Uuid,
    distinto: &PagoDistinto,
) -> AppResult<ResultadoAbono> {
    validar_monto_spei(abono.monto)?;
    let motivo = distinto.motivo.trim();
    if !(5..=200).contains(&motivo.chars().count()) || motivo.chars().any(char::is_control) {
        return Err(AppError::validacion("el motivo debe tener de 5 a 200 caracteres"));
    }
    let distinto = PagoDistinto { transaccion_id: distinto.transaccion_id, motivo: motivo.to_owned() };
    db::con_reintentos(|| {
        abonar_tx(pool, umbrales, cfg, abono, Origen::Operador(operador_id), SiNoAcreditable::Rechazar, Some(&distinto))
    })
    .await
}

/// Por qué la cuenta destino no puede recibir.
enum Destino {
    Acreditable { cuenta_id: Uuid, perfil: pld::PerfilCliente },
    NoAcreditable { cuenta_id: Option<Uuid>, causa: CausaDevolucion, error: AppError },
}

async fn resolver_destino(tx: &mut Tx, abono: &Abono) -> AppResult<Destino> {
    let no = |cuenta_id, causa, error| Ok(Destino::NoAcreditable { cuenta_id, causa, error });
    if !clabe::es_valida(&abono.receptor_clabe) {
        return no(None, CausaDevolucion::CuentaInexistente, AppError::validacion("CLABE receptora inválida"));
    }
    let asignada: Option<(Uuid, bool)> =
        sqlx::query_as("SELECT cuenta_id, activa FROM ifpe.clabes_asignadas WHERE clabe = $1")
            .bind(&abono.receptor_clabe)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((cuenta_id, activa)) = asignada else {
        return no(None, CausaDevolucion::CuentaInexistente, AppError::no_encontrado("CLABE no encontrada"));
    };
    // Orden global de bloqueo: la cuenta antes que la reserva.
    let cuenta = ledger::bloquear_cuenta_ifpe(tx, cuenta_id).await?;
    if !activa || cuenta.estado == EstadoCuenta::Cerrada {
        return no(
            Some(cuenta_id),
            CausaDevolucion::CuentaCancelada,
            AppError::negocio("CUENTA_CERRADA", "la cuenta IFPE está cerrada o la CLABE fue dada de baja"),
        );
    }
    let perfil = pld::cargar_perfil(tx, cuenta.cliente_id).await?;
    if let Err(error) = perfil.exigir_operable() {
        return no(Some(cuenta_id), CausaDevolucion::CuentaBloqueada, error);
    }
    Ok(Destino::Acreditable { cuenta_id, perfil })
}

/// Todas las notificaciones (y anulaciones) del mismo pago, de una en una.
async fn bloquear_identidad(tx: &mut Tx, identidad: &str) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("spei-inbound:{identidad}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn abonar_tx(
    pool: &PgPool,
    umbrales: &UmbralesPld,
    cfg: &ConfigSpei,
    abono: &Abono,
    origen: Origen,
    si_no_acreditable: SiNoAcreditable,
    distinto: Option<&PagoDistinto>,
) -> AppResult<ResultadoAbono> {
    let identidad = abono.identidad.canonica();
    let mut tx = db::begin(pool, Rol::Spei).await?;
    bloquear_identidad(&mut tx, &identidad).await?;
    let mut vinculos = Vinculos { reemplaza_a: None, distinto };
    if let Some(vigente) = buscar(&mut tx, &abono.identidad).await? {
        if !anulado(&mut tx, vigente.id).await? {
            return repeticion(&mut tx, vigente, abono, &identidad).await;
        }
        // La captura con esta identidad se anuló: este pago la reemplaza.
        vinculos.reemplaza_a = Some(vigente.id);
    }
    if abono.es_devolucion() {
        return devolucion_de_orden(tx, cfg, abono, origen, &identidad, vinculos).await;
    }

    let (cuenta_id, perfil) = match resolver_destino(&mut tx, abono).await? {
        Destino::Acreditable { cuenta_id, perfil } => (cuenta_id, perfil),
        Destino::NoAcreditable { error, .. } if si_no_acreditable == SiNoAcreditable::Rechazar => return Err(error),
        Destino::NoAcreditable { cuenta_id, causa, error } => {
            let motivo = format!("{} ({error})", causa.descripcion());
            let spei = insertar(&mut tx, abono, origen, vinculos, cuenta_id, None, Some((causa, &motivo))).await?;
            tx.commit().await?;
            tracing::warn!(%identidad, causa = causa.codigo(), %motivo, "SPEI recibido devuelto al emisor");
            return Ok(ResultadoAbono { spei, transaccion: None, idempotente: false });
        }
    };

    let nuevo_es_webhook = matches!(origen, Origen::Webhook);
    exigir_sin_duplicado(&mut tx, abono, cuenta_id, &identidad, nuevo_es_webhook, distinto.map(|d| d.transaccion_id))
        .await?;
    let reserva = ledger::bloquear_reserva(&mut tx).await?;
    let concepto = concepto_ledger(abono);
    // Igual que `ifpe.referencia_abono_spei`: el reemplazo de un registro
    // anulado lleva el id del anulado (su DEPOSITO ya tiene la identidad sola).
    let referencia = match vinculos.reemplaza_a {
        Some(anulado) => format!("{identidad}/{anulado}"),
        None => identidad.clone(),
    };
    let t = ledger::abonar_efectivo(
        &mut tx,
        cuenta_id,
        reserva.id,
        abono.monto,
        Some(&concepto),
        Some(&referencia),
        &format!("spei:{referencia}"),
    )
    .await?;
    let spei = insertar(&mut tx, abono, origen, vinculos, Some(cuenta_id), Some(t.id), None).await?;
    let evento = EventoPld {
        entidad: Entidad::Ifpe,
        cliente: &perfil,
        transaccion_id: t.id,
        operacion: "DEPOSITO",
        monto: abono.monto,
        cuenta_deposito: Some(cuenta_id),
    };
    pld::evaluar(&mut tx, &evento, umbrales).await?;
    tx.commit().await?;
    tracing::info!(%identidad, monto = %abono.monto, cuenta = %cuenta_id, "SPEI recibido abonado");
    Ok(ResultadoAbono { spei, transaccion: Some(t), idempotente: false })
}

/// Concepto del DEPOSITO en el ledger (cabe en 140 caracteres).
fn concepto_ledger(abono: &Abono) -> String {
    let mut c = String::from("SPEI recibido");
    if let Some(n) = &abono.nombre_ordenante {
        c.push_str(" de ");
        c.push_str(n);
    }
    if let Some(con) = &abono.concepto {
        c.push_str(": ");
        c.push_str(con);
    }
    c.chars().take(140).collect()
}

/// Registro vigente de la identidad: el último de su cadena de reemplazos (a
/// lo sumo uno; la base de datos solo admite reemplazar un registro anulado).
async fn buscar(tx: &mut Tx, id: &IdentidadSpei) -> AppResult<Option<SpeiInbound>> {
    Ok(sqlx::query_as(
        "SELECT * FROM ifpe.spei_inbound s \
         WHERE institucion_emisora = $1 AND clave_rastreo = $2 AND fecha_operacion = $3 \
           AND NOT EXISTS (SELECT 1 FROM ifpe.spei_inbound r WHERE r.reemplaza_a = s.id)",
    )
    .bind(&id.institucion_emisora)
    .bind(&id.clave_rastreo)
    .bind(id.fecha_operacion)
    .fetch_optional(&mut **tx)
    .await?)
}

async fn anulado(tx: &mut Tx, spei_inbound_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ifpe.spei_anulaciones WHERE spei_inbound_id = $1)")
        .bind(spei_inbound_id)
        .fetch_one(&mut **tx)
        .await?)
}

/// El mismo pago llegó otra vez: misma identidad, mismo monto y misma cuenta
/// = repetición (se devuelve el resultado original). Misma identidad con otros
/// datos = conflicto que requiere revisión; nunca se abona. Si el registro
/// previo es una captura errónea, el operador la anula y el pago real se
/// registra como su reemplazo.
async fn repeticion(tx: &mut Tx, previa: SpeiInbound, abono: &Abono, identidad: &str) -> AppResult<ResultadoAbono> {
    if previa.monto != abono.monto || previa.receptor_clabe != abono.receptor_clabe {
        tracing::error!(
            %identidad,
            monto_previo = %previa.monto,
            monto_nuevo = %abono.monto,
            clabe_previa = %previa.receptor_clabe,
            clabe_nueva = %abono.receptor_clabe,
            "SPEI con identidad repetida y datos distintos: requiere revisión"
        );
        return Err(AppError::conflicto(
            IDENTIDAD_REUTILIZADA,
            format!(
                "ese SPEI (emisor, fecha y clave de rastreo) ya se registró con otro monto o cuenta (registro {}); \
                 si esa captura fue un error, anúlela y vuelva a capturar el pago",
                previa.id
            ),
        ));
    }
    let transaccion = match previa.transaccion_id {
        Some(id) => Some(
            sqlx::query_as::<_, TransaccionIfpe>("SELECT * FROM ifpe.transacciones WHERE id = $1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?,
        ),
        None => None,
    };
    Ok(ResultadoAbono { spei: previa, transaccion, idempotente: true })
}

async fn insertar(
    tx: &mut Tx,
    abono: &Abono,
    origen: Origen,
    vinculos: Vinculos<'_>,
    cuenta_id: Option<Uuid>,
    transaccion_id: Option<Uuid>,
    devolucion: Option<(CausaDevolucion, &str)>,
) -> AppResult<SpeiInbound> {
    let estado = if devolucion.is_some() { "DEVUELTO" } else { "ABONADO" };
    // El motivo de un ABONADO es el del operador que lo declaró distinto.
    let motivo = devolucion.map(|(_, m)| m).or(vinculos.distinto.map(|d| d.motivo.as_str()));
    insertar_registro(
        tx,
        abono,
        origen,
        vinculos,
        cuenta_id,
        transaccion_id,
        None,
        estado,
        devolucion.map(|(c, _)| c.codigo()),
        motivo,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insertar_registro(
    tx: &mut Tx,
    abono: &Abono,
    origen: Origen,
    vinculos: Vinculos<'_>,
    cuenta_id: Option<Uuid>,
    transaccion_id: Option<Uuid>,
    spei_outbound_id: Option<Uuid>,
    estado: &str,
    causa_devolucion: Option<i16>,
    motivo: Option<&str>,
) -> AppResult<SpeiInbound> {
    let (origen_txt, registrado_por) = match origen {
        Origen::Webhook => ("WEBHOOK", None),
        Origen::Operador(id) => ("OPERADOR", Some(id)),
        Origen::Demo => ("DEMO", None),
    };
    Ok(sqlx::query_as(
        "INSERT INTO ifpe.spei_inbound \
         (institucion_emisora, clave_rastreo, fecha_operacion, institucion_receptora, receptor_clabe, cuenta_id, \
          monto, nombre_ordenante, cuenta_ordenante, rfc_curp_ordenante, referencia_numerica, concepto, estado, \
          causa_devolucion, motivo, transaccion_id, origen, registrado_por, id_proveedor, raw_payload, tipo_pago, \
          spei_outbound_id, reemplaza_a, distinto_de_tx) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, \
                 $23, $24) \
         RETURNING *",
    )
    .bind(&abono.identidad.institucion_emisora)
    .bind(&abono.identidad.clave_rastreo)
    .bind(abono.identidad.fecha_operacion)
    .bind(&abono.institucion_receptora)
    .bind(&abono.receptor_clabe)
    .bind(cuenta_id)
    .bind(abono.monto)
    .bind(&abono.nombre_ordenante)
    .bind(&abono.cuenta_ordenante)
    .bind(&abono.rfc_curp_ordenante)
    .bind(abono.referencia_numerica)
    .bind(&abono.concepto)
    .bind(estado)
    .bind(causa_devolucion)
    .bind(motivo)
    .bind(transaccion_id)
    .bind(origen_txt)
    .bind(registrado_por)
    .bind(&abono.id_proveedor)
    .bind(&abono.raw_payload)
    .bind(abono.tipo_pago)
    .bind(spei_outbound_id)
    .bind(vinculos.reemplaza_a)
    .bind(vinculos.distinto.map(|d| d.transaccion_id))
    .fetch_one(&mut **tx)
    .await?)
}

/// La devolución de una orden nuestra se aplica a la orden (que reembolsa una
/// sola vez) y se registra ligada a ella. Nunca se abona como depósito nuevo
/// ni se "devuelve" al emisor. Si no corresponde a ninguna orden no se escribe
/// nada aquí: el webhook la pone en cuarentena y la confirma; el operador
/// recibe el 409.
async fn devolucion_de_orden(
    mut tx: Tx,
    cfg: &ConfigSpei,
    abono: &Abono,
    origen: Origen,
    identidad: &str,
    vinculos: Vinculos<'_>,
) -> AppResult<ResultadoAbono> {
    let sin_orden = |detalle: String| {
        tracing::error!(%identidad, %detalle, "devolución SPEI sin orden que le corresponda: requiere revisión");
        AppError::conflicto(DEVOLUCION_SIN_ORDEN, detalle)
    };
    let Some(rastreo) = abono.rastreo_devuelto.as_deref() else {
        return Err(sin_orden("la devolución no trae la clave de rastreo del pago original".into()));
    };
    let Some(orden) = outbound::buscar_por_rastreo_en(&mut tx, rastreo).await? else {
        return Err(sin_orden(format!("no hay una orden con clave de rastreo {rastreo}")));
    };
    // El emisor de la devolución es el banco al que se envió, y el monto, el enviado.
    if orden.monto != abono.monto || !abono.identidad.institucion_emisora.ends_with(&orden.banco_receptor) {
        return Err(sin_orden(format!("la devolución no coincide con la orden {rastreo} (monto o banco)")));
    }
    let causa = abono.causa_devolucion.unwrap_or(99);
    let evento = outbound::Evento::Devuelta { causa, motivo: "devolución recibida del banco receptor".into() };
    let t = outbound::aplicar_en(&mut tx, cfg, orden.id, &evento).await?;
    if t.incidente.is_some() {
        // Se conserva la marca de revisión; la devolución no se registra.
        tx.commit().await?;
        return Err(sin_orden(t.incidente.unwrap_or_default()));
    }
    let spei = insertar_registro(
        &mut tx,
        abono,
        origen,
        vinculos,
        Some(orden.cuenta_id),
        None,
        Some(orden.id),
        "DEVOLUCION_DE_ORDEN",
        Some(causa),
        None,
    )
    .await?;
    tx.commit().await?;
    tracing::warn!(%identidad, %rastreo, "devolución de una orden SPEI aplicada");
    Ok(ResultadoAbono { spei, transaccion: None, idempotente: false })
}

/// Busca un abono que muy probablemente sea el mismo pago con otra identidad:
/// misma cuenta, monto y clave de rastreo; emisor con los mismos 3 últimos
/// dígitos (código de banco); fecha a ±7 días. También los abonos anteriores
/// a 0005 sin registro en `spei_inbound` (clave de rastreo sola, emisor de 3
/// dígitos, claves `op:`). No cuentan los registros anulados, el abono que el
/// operador declaró distinto (`ignorar`) ni, si el pago nuevo lo informa el
/// proveedor, los que también informó el proveedor: dos identidades del
/// proveedor son dos pagos (la misma clave de rastreo otro día o de otro
/// emisor es legítima).
async fn exigir_sin_duplicado(
    tx: &mut Tx,
    abono: &Abono,
    cuenta_id: Uuid,
    identidad: &str,
    nuevo_es_webhook: bool,
    ignorar: Option<Uuid>,
) -> AppResult<()> {
    let previo: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, referencia_externa FROM ( \
           SELECT t.id, t.referencia_externa, t.secuencia \
             FROM ifpe.spei_inbound s JOIN ifpe.transacciones t ON t.id = s.transaccion_id \
            WHERE s.estado = 'ABONADO' AND s.cuenta_id = $1 AND s.monto = $2 AND s.clave_rastreo = $3 \
              AND right(s.institucion_emisora, 3) = right($4, 3) \
              AND s.fecha_operacion BETWEEN $5::date - 7 AND $5::date + 7 \
              AND NOT EXISTS (SELECT 1 FROM ifpe.spei_anulaciones a WHERE a.spei_inbound_id = s.id) \
              AND NOT ($6::bool AND s.origen = 'WEBHOOK') \
           UNION ALL \
           SELECT t.id, t.referencia_externa, t.secuencia \
             FROM ifpe.transacciones t \
            WHERE t.tipo = 'DEPOSITO' AND t.cuenta_destino_id = $1 AND t.monto = $2 \
              AND t.clave_idempotencia NOT LIKE 'spei-reverso:%' \
              AND NOT EXISTS (SELECT 1 FROM ifpe.spei_inbound s WHERE s.transaccion_id = t.id) \
              AND (upper(btrim(t.referencia_externa)) = $3 \
                   OR (t.referencia_externa ~ '^[0-9]{3,5}:[0-9]{8}:[A-Za-z0-9]{1,30}$' \
                       AND upper(split_part(t.referencia_externa, ':', 3)) = $3 \
                       AND right(split_part(t.referencia_externa, ':', 1), 3) = right($4, 3) \
                       AND split_part(t.referencia_externa, ':', 2) \
                           BETWEEN to_char($5::date - 7, 'YYYYMMDD') AND to_char($5::date + 7, 'YYYYMMDD'))) \
         ) c WHERE id IS DISTINCT FROM $7 ORDER BY secuencia LIMIT 1",
    )
    .bind(cuenta_id)
    .bind(abono.monto)
    .bind(&abono.identidad.clave_rastreo)
    .bind(&abono.identidad.institucion_emisora)
    .bind(abono.identidad.fecha_operacion)
    .bind(nuevo_es_webhook)
    .bind(ignorar)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id, referencia)) = previo {
        let previo = referencia.unwrap_or_default();
        tracing::error!(%identidad, %previo, transaccion = %id, monto = %abono.monto, "posible SPEI duplicado con otra identidad: no se abona");
        return Err(AppError::conflicto(
            POSIBLE_DUPLICADO,
            format!(
                "ya se abonó un pago con la misma clave de rastreo, monto y cuenta ({previo}, transacción {id}); \
                 si es un pago distinto, captúrelo con distinto_de={id} y un motivo"
            ),
        ));
    }
    Ok(())
}

/// Guarda una notificación autenticada que no se pudo interpretar (o una
/// devolución sin orden).
pub async fn cuarentena(pool: &PgPool, tipo: &str, cuerpo: &[u8], error: &str) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Spei).await?;
    sqlx::query("INSERT INTO ifpe.spei_cuarentena (tipo, cuerpo, error) VALUES ($1, $2, $3)")
        .bind(tipo)
        .bind(cuerpo)
        .bind(error.chars().take(500).collect::<String>())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::error!(tipo, %error, "notificación SPEI autenticada en cuarentena");
    Ok(())
}

/// Retiene, sin abonar, una notificación autenticada que necesita revisión
/// (`POSIBLE_DUPLICADO` o `IDENTIDAD_REUTILIZADA`), con su identidad. Una fila
/// por cuerpo: los reintentos del proveedor no se acumulan. La cola es
/// `ifpe.v_spei_en_revision`.
pub async fn retener(pool: &PgPool, tipo: &str, abono: &Abono, cuerpo: &[u8], error: &str) -> AppResult<()> {
    let identidad = abono.identidad.canonica();
    let mut tx = db::begin(pool, Rol::Spei).await?;
    sqlx::query(
        "INSERT INTO ifpe.spei_cuarentena (tipo, cuerpo, error, identidad) VALUES ($1, $2, $3, $4) \
         ON CONFLICT DO NOTHING",
    )
    .bind(tipo)
    .bind(cuerpo)
    .bind(error.chars().take(500).collect::<String>())
    .bind(&identidad)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::error!(tipo, %identidad, %error, "SPEI retenido para revisión: confirmado al proveedor, sin abonar");
    Ok(())
}

// -----------------------------------------------------------------------------
// Corrección de un abono erróneo
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SolicitudCorreccion {
    pub spei_inbound_id: Uuid,
    pub monto: Decimal,
    pub motivo: String,
    pub operador_id: Uuid,
    /// Clave ya acotada al operador.
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Correccion {
    pub id: Uuid,
    pub spei_inbound_id: Uuid,
    pub cuenta_id: Uuid,
    pub monto: Decimal,
    pub motivo: String,
    pub autorizado_por: Uuid,
    #[serde(skip)]
    pub clave_idempotencia: String,
    pub tx_id: Uuid,
    pub creado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoCorreccion {
    pub correccion: Correccion,
    pub transaccion: TransaccionIfpe,
    pub idempotente: bool,
}

fn validar_motivo(motivo: &str, que: &str) -> AppResult<String> {
    let motivo = motivo.trim();
    if !(5..=200).contains(&motivo.chars().count()) || motivo.chars().any(char::is_control) {
        return Err(AppError::validacion(format!("el motivo de la {que} debe tener de 5 a 200 caracteres")));
    }
    Ok(motivo.to_owned())
}

/// Revierte (total o parcialmente) un abono SPEI erróneo: cargo al saldo
/// disponible del cliente y a la reserva, ligado al abono original. La base de
/// datos impide corregir más que el monto abonado.
pub async fn corregir(pool: &PgPool, s: &SolicitudCorreccion) -> AppResult<ResultadoCorreccion> {
    let monto = validar_monto_spei(s.monto)?;
    let motivo = validar_motivo(&s.motivo, "corrección")?;
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Spei).await?;
        let abono: SpeiInbound = sqlx::query_as("SELECT * FROM ifpe.spei_inbound WHERE id = $1")
            .bind(s.spei_inbound_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::no_encontrado("SPEI recibido no encontrado"))?;
        let (Some(cuenta_id), "ABONADO") = (abono.cuenta_id, abono.estado.as_str()) else {
            return Err(AppError::negocio("SPEI_NO_ABONADO", "solo se corrige un SPEI que se abonó"));
        };
        ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
        let previa: Option<Correccion> =
            sqlx::query_as("SELECT * FROM ifpe.spei_correcciones WHERE clave_idempotencia = $1")
                .bind(&s.clave_idempotencia)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(previa) = previa {
            if previa.spei_inbound_id != s.spei_inbound_id || previa.monto != monto || previa.motivo != motivo {
                return Err(AppError::conflicto(
                    "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                    "la clave de idempotencia ya se usó para otra corrección",
                ));
            }
            let transaccion = sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE id = $1")
                .bind(previa.tx_id)
                .fetch_one(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(ResultadoCorreccion { correccion: previa, transaccion, idempotente: true });
        }
        let (correccion, t) = corregir_en(&mut tx, &abono, monto, &motivo, s.operador_id, &s.clave_idempotencia).await?;
        tx.commit().await?;
        tracing::warn!(spei = %abono.id, %monto, operador = %s.operador_id, "abono SPEI corregido");
        Ok(ResultadoCorreccion { correccion, transaccion: t, idempotente: false })
    })
    .await
}

/// Cargo de corrección ligado al abono, dentro de una transacción `sc_spei`
/// que ya bloqueó la cuenta del abono.
async fn corregir_en(
    tx: &mut Tx,
    abono: &SpeiInbound,
    monto: Decimal,
    motivo: &str,
    operador_id: Uuid,
    clave_idempotencia: &str,
) -> AppResult<(Correccion, TransaccionIfpe)> {
    let (Some(cuenta_id), "ABONADO") = (abono.cuenta_id, abono.estado.as_str()) else {
        return Err(AppError::negocio("SPEI_NO_ABONADO", "solo se corrige un SPEI que se abonó"));
    };
    let cuenta = ledger::bloquear_cuenta_ifpe(tx, cuenta_id).await?;
    let corregido: Decimal =
        sqlx::query_scalar("SELECT coalesce(sum(monto), 0) FROM ifpe.spei_correcciones WHERE spei_inbound_id = $1")
            .bind(abono.id)
            .fetch_one(&mut **tx)
            .await?;
    if corregido + monto > abono.monto {
        return Err(AppError::negocio(
            "CORRECCION_EXCEDE_ABONO",
            format!("el abono fue de {} y ya se corrigieron {corregido}", abono.monto),
        ));
    }
    ledger::exigir_saldo(&cuenta, monto)?;
    // Mismo custodio al que entró el abono.
    let reserva_id: Uuid = sqlx::query_scalar("SELECT reserva_id FROM ifpe.transacciones WHERE id = $1")
        .bind(abono.transaccion_id)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("SELECT 1 FROM ifpe.reserva_custodia WHERE id = $1 FOR NO KEY UPDATE")
        .bind(reserva_id)
        .execute(&mut **tx)
        .await?;
    let id = Uuid::new_v4();
    let concepto = format!("Corrección de SPEI recibido {}: {motivo}", abono.clave_rastreo);
    let concepto: String = concepto.chars().take(140).collect();
    let t = ledger::cargar_efectivo(
        tx,
        cuenta_id,
        reserva_id,
        monto,
        Some(&concepto),
        Some(&format!("correccion:{}", abono.id)),
        &format!("spei-correccion:{id}"),
    )
    .await?;
    let correccion: Correccion = sqlx::query_as(
        "INSERT INTO ifpe.spei_correcciones \
         (id, spei_inbound_id, cuenta_id, monto, motivo, autorizado_por, clave_idempotencia, tx_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(id)
    .bind(abono.id)
    .bind(cuenta_id)
    .bind(monto)
    .bind(motivo)
    .bind(operador_id)
    .bind(clave_idempotencia)
    .bind(t.id)
    .fetch_one(&mut **tx)
    .await?;
    Ok((correccion, t))
}

// -----------------------------------------------------------------------------
// Anulación de una captura errónea
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SolicitudAnulacion {
    pub spei_inbound_id: Uuid,
    pub motivo: String,
    pub operador_id: Uuid,
    /// Clave ya acotada al operador.
    pub clave_idempotencia: String,
}

/// Registro de `ifpe.spei_anulaciones`.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Anulacion {
    pub id: Uuid,
    pub spei_inbound_id: Uuid,
    pub correccion_id: Option<Uuid>,
    pub motivo: String,
    pub autorizado_por: Uuid,
    #[serde(skip)]
    pub clave_idempotencia: String,
    pub creado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoAnulacion {
    pub anulacion: Anulacion,
    /// La corrección que completó la reversión (ausente si ya estaba
    /// corregido por completo).
    pub correccion: Option<Correccion>,
    pub idempotente: bool,
}

/// Anula una captura manual errónea (CLABE o monto equivocados): corrige lo
/// que falte del abono (cargo `spei-correccion:` ligado a él, como
/// [`corregir`]) y libera su identidad. El pago real (webhook o nueva captura)
/// se registra después como reemplazo del anulado y se abona a la cuenta
/// correcta. Solo capturas del operador o anteriores a 0005: lo que informa
/// el proveedor no se anula. La base de datos exige la corrección total al
/// COMMIT y que el operador no sea el titular de la cuenta.
pub async fn anular(pool: &PgPool, s: &SolicitudAnulacion) -> AppResult<ResultadoAnulacion> {
    let motivo = validar_motivo(&s.motivo, "anulación")?;
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Spei).await?;
        let abono: SpeiInbound = sqlx::query_as("SELECT * FROM ifpe.spei_inbound WHERE id = $1")
            .bind(s.spei_inbound_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::no_encontrado("SPEI recibido no encontrado"))?;
        // El mismo candado que las notificaciones de este pago.
        let identidad = IdentidadSpei {
            institucion_emisora: abono.institucion_emisora.clone(),
            fecha_operacion: abono.fecha_operacion,
            clave_rastreo: abono.clave_rastreo.clone(),
        }
        .canonica();
        bloquear_identidad(&mut tx, &identidad).await?;

        let previa: Option<Anulacion> =
            sqlx::query_as("SELECT * FROM ifpe.spei_anulaciones WHERE clave_idempotencia = $1 OR spei_inbound_id = $2")
                .bind(&s.clave_idempotencia)
                .bind(abono.id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(previa) = previa {
            if previa.clave_idempotencia != s.clave_idempotencia {
                return Err(AppError::conflicto("SPEI_YA_ANULADO", "ese SPEI recibido ya fue anulado"));
            }
            if previa.spei_inbound_id != abono.id || previa.motivo != motivo {
                return Err(AppError::conflicto(
                    "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                    "la clave de idempotencia ya se usó para otra anulación",
                ));
            }
            let correccion = match previa.correccion_id {
                Some(id) => Some(
                    sqlx::query_as("SELECT * FROM ifpe.spei_correcciones WHERE id = $1").bind(id).fetch_one(&mut *tx).await?,
                ),
                None => None,
            };
            tx.commit().await?;
            return Ok(ResultadoAnulacion { anulacion: previa, correccion, idempotente: true });
        }
        let (Some(cuenta_id), "ABONADO", "OPERADOR" | "LEGADO") =
            (abono.cuenta_id, abono.estado.as_str(), abono.origen.as_str())
        else {
            return Err(AppError::negocio(
                "SPEI_NO_ANULABLE",
                "solo se anula un abono capturado por el operador (o anterior a 0005); lo que informó el proveedor se corrige",
            ));
        };
        let titular: Uuid = sqlx::query_scalar("SELECT cliente_id FROM ifpe.cuentas WHERE id = $1")
            .bind(cuenta_id)
            .fetch_one(&mut *tx)
            .await?;
        if titular == s.operador_id {
            return Err(AppError::negocio("SPEI_AUTORESOLUCION", "un operador no anula abonos a su propia cuenta"));
        }
        ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
        let corregido: Decimal =
            sqlx::query_scalar("SELECT coalesce(sum(monto), 0) FROM ifpe.spei_correcciones WHERE spei_inbound_id = $1")
                .bind(abono.id)
                .fetch_one(&mut *tx)
                .await?;
        let resto = abono.monto - corregido;
        let correccion = if resto > Decimal::ZERO {
            let clave = format!("{}:anulacion", s.clave_idempotencia);
            let (c, _) = corregir_en(&mut tx, &abono, resto, &motivo, s.operador_id, &clave).await?;
            Some(c)
        } else {
            None
        };
        let anulacion: Anulacion = sqlx::query_as(
            "INSERT INTO ifpe.spei_anulaciones (spei_inbound_id, correccion_id, motivo, autorizado_por, clave_idempotencia) \
             VALUES ($1, $2, $3, $4, $5) RETURNING *",
        )
        .bind(abono.id)
        .bind(correccion.as_ref().map(|c| c.id))
        .bind(&motivo)
        .bind(s.operador_id)
        .bind(&s.clave_idempotencia)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::warn!(spei = %abono.id, %identidad, %resto, operador = %s.operador_id, "captura SPEI anulada: identidad liberada");
        Ok(ResultadoAnulacion { anulacion, correccion, idempotente: false })
    })
    .await
}
/// SPEI recibidos de un cliente (más recientes primero).
pub async fn de_cliente(pool: &PgPool, cliente_id: Uuid, limite: i64) -> AppResult<Vec<SpeiInbound>> {
    let mut tx = db::begin_lectura(pool, Rol::Ifpe).await?;
    let filas = sqlx::query_as(
        "SELECT s.* FROM ifpe.spei_inbound s JOIN ifpe.cuentas c ON c.id = s.cuenta_id \
         WHERE c.cliente_id = $1 ORDER BY s.recibido_en DESC LIMIT $2",
    )
    .bind(cliente_id)
    .bind(limite.clamp(1, 200))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(filas)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn notificacion(monto: &str) -> Vec<u8> {
        format!(
            r#"{{"id":987654,"fechaOperacion":20261013,"institucionOrdenante":40012,"institucionBeneficiaria":90646,
                "claveRastreo":"mban01002610130000123","monto":{monto},"nombreOrdenante":"JUAN PEREZ",
                "cuentaOrdenante":"012180001234567899","rfcCurpOrdenante":"pexj800101xx1",
                "cuentaBeneficiario":"646180157000000017","conceptoPago":"RENTA OCTUBRE",
                "referenciaNumerica":1234567,"tsLiquidacion":1791900000000,"campoNuevo":"x"}}"#
        )
        .into_bytes()
    }

    #[test]
    fn interpreta_la_notificacion_de_stp() {
        let a = interpretar_notificacion(&notificacion("1500.25")).unwrap();
        assert_eq!(a.identidad.canonica(), "40012:20261013:MBAN01002610130000123");
        assert_eq!(a.monto, Decimal::from_str("1500.25").unwrap());
        assert_eq!(a.receptor_clabe, "646180157000000017");
        assert_eq!(a.institucion_receptora.as_deref(), Some("90646"));
        assert_eq!(a.referencia_numerica, Some(1_234_567));
        assert_eq!(a.rfc_curp_ordenante.as_deref(), Some("PEXJ800101XX1"));
        assert_eq!(a.id_proveedor.as_deref(), Some("987654"));
        assert_eq!(a.raw_payload["campoNuevo"], "x");
    }

    #[test]
    fn el_monto_se_lee_exacto_sin_f64() {
        for (texto, esperado) in [("0.1", "0.1"), ("\"9999999999.99\"", "9999999999.99"), ("1234567.89", "1234567.89")]
        {
            let a = interpretar_notificacion(&notificacion(texto)).unwrap();
            assert_eq!(a.monto, Decimal::from_str(esperado).unwrap(), "{texto}");
        }
        for invalido in ["1.001", "0", "-5", "1e3", "\"abc\"", "null"] {
            assert!(interpretar_notificacion(&notificacion(invalido)).is_err(), "{invalido}");
        }
    }

    #[test]
    fn rechaza_notificaciones_sin_identidad() {
        let mala = br#"{"fechaOperacion":20261013,"institucionOrdenante":"012","claveRastreo":"A","monto":1,"cuentaBeneficiario":"646180157000000017"}"#;
        assert!(interpretar_notificacion(mala).is_err(), "emisor de 3 dígitos");
        let sin_monto = br#"{"fechaOperacion":20261013,"institucionOrdenante":40012,"claveRastreo":"A","cuentaBeneficiario":"646180157000000017"}"#;
        assert!(interpretar_notificacion(sin_monto).is_err());
        assert!(interpretar_notificacion(b"no es json").is_err());
    }
}
