//! SPEI enviados: órdenes, despachador y reversos.
//!
//! **Solicitud** (una transacción `sc_ifpe`): se bloquea la cuenta, se valida
//! saldo disponible (el dinero en mutuos vive en la SOFOM y nunca cuenta), el
//! saldo del cliente y la reserva en custodia bajan juntos (RETIRO con su
//! `tx_hash`) y se registra la orden PENDIENTE con una clave de rastreo única
//! para siempre. El dinero ya no es del cliente aunque siga en el custodio
//! hasta liquidarse: no puede gastarse dos veces.
//!
//! **Despacho** (rol `sc_spei`): el despachador reclama órdenes con un
//! arrendamiento (`FOR UPDATE SKIP LOCKED`) y, para las PENDIENTE, cuenta el
//! intento **antes** de llamar al proveedor (queda confirmado en la base de
//! datos). Luego habla con el proveedor fuera de toda transacción y aplica el
//! resultado con [`aplicar`]. Justo antes de cada `registrar`, el reclamo se
//! verifica y renueva contra la fila viva (mismo estado, mismos
//! `intentos_envio`, sin revisión manual): una foto vieja de un lote lento
//! nunca registra una orden que otro despachador retomó, que fue a revisión o
//! que el operador reembolsó. Cada llamada al proveedor dura a lo sumo
//! [`LLAMADA_PASARELA_SEG`], muy por debajo del arrendamiento.
//!
//! **Reglas que protegen el dinero** (un reembolso indebido paga dos veces):
//! - Solo el primer intento puede tomar un "rechazado" como definitivo, y solo
//!   si al aplicarlo (con la orden bloqueada) `intentos_envio` sigue siendo el
//!   del reclamo: si el arrendamiento venció y otro despachador la reclamó,
//!   ese otro pudo registrarla, y el rechazo tardío va a revisión manual.
//! - Si ya hubo un intento de resultado desconocido, primero se consulta; solo
//!   si el proveedor dice que no tiene la orden se reenvía, y un rechazo en
//!   ese reenvío (p. ej. "clave de rastreo duplicada") es ambiguo: revisión
//!   manual, nunca reembolso.
//! - Solo se reenvía si el día de operación del proveedor no pudo cambiar
//!   desde la primera llamada a `registrar` que pudo registrar la orden
//!   (`registro_incierto_desde`; el proveedor rechaza como duplicada la clave
//!   de rastreo del mismo día, no la de un día anterior); si pudo cambiar,
//!   revisión manual. Una llamada que prueba que no llegó al proveedor
//!   ([`ErrorPasarela::NoRecibida`]) no cuenta: tras una caída del proveedor
//!   la orden se reenvía aunque haya pasado el corte.
//! - Una orden en revisión manual que el proveedor nunca recibió la reembolsa
//!   el operador ([`reembolsar_revision`]), auditado en `ifpe.spei_resoluciones`.
//! - Tiempo agotado o error: nunca se reembolsa; se reintenta con espera y,
//!   tras `max_intentos`, revisión manual.
//! - Reembolso solo ante un rechazo del primer intento, una cancelación o una
//!   devolución que informa el proveedor. Las órdenes liquidadas se siguen
//!   consultando durante la ventana de devolución.
//!
//! El reverso es un DEPOSITO `spei-reverso:<orden>` que restituye cliente y
//! reserva en la misma transacción que cierra la orden. Un resultado del
//! proveedor incompatible con el estado final de la orden queda en
//! `ifpe.spei_incidentes` (evidencia permanente). La orden bloqueada
//! (`FOR UPDATE`) serializa despachador, webhook de estado y devoluciones
//! recibidas; la máquina de estados de la base de datos rechaza transiciones
//! inválidas y la clave única del reverso hace imposible reembolsar dos veces.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{
    clabe::Clabe,
    pasarela::{ConsultaOrden, ErrorPasarela, EstadoProveedor, OrdenPasarela, Pasarela, Registro},
    texto_spei, validar_monto_spei,
};
use crate::{
    aml::{self, Canal, Contraparte, Decision, Operacion},
    config::{ConfigAml, ConfigSpei},
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    ledger,
    models::{ifpe::TransaccionIfpe, EstadoCuenta},
    pld,
};

/// Registro de `ifpe.spei_outbound`.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SpeiOutbound {
    pub id: Uuid,
    pub cliente_id: Uuid,
    pub cuenta_id: Uuid,
    #[serde(skip)]
    pub clave_idempotencia: String,
    pub clave_rastreo: String,
    pub fecha_solicitud: NaiveDate,
    pub fecha_operacion: Option<NaiveDate>,
    pub ordenante_nombre: String,
    pub ordenante_clabe: String,
    pub ordenante_rfc_curp: Option<String>,
    pub banco_receptor: String,
    pub beneficiario_clabe: String,
    pub beneficiario_nombre: String,
    pub beneficiario_rfc_curp: Option<String>,
    pub monto: Decimal,
    pub concepto: String,
    pub referencia_numerica: i32,
    pub estado: String,
    pub tx_retiro_id: Uuid,
    pub tx_reverso_id: Option<Uuid>,
    pub id_proveedor: Option<String>,
    pub intentos_envio: i32,
    pub ultimo_error: Option<String>,
    pub revision_manual: bool,
    pub causa_devolucion: Option<i16>,
    pub motivo: Option<String>,
    pub proximo_intento_en: DateTime<Utc>,
    pub creado_en: DateTime<Utc>,
    pub actualizado_en: DateTime<Utc>,
    pub enviada_en: Option<DateTime<Utc>>,
    pub liquidada_en: Option<DateTime<Utc>>,
    pub finalizada_en: Option<DateTime<Utc>>,
    /// Inicio de la última llamada a `registrar`.
    #[serde(skip)]
    pub registro_intentado_en: Option<DateTime<Utc>>,
    /// Inicio de la primera llamada a `registrar` que pudo registrar la orden.
    #[serde(skip)]
    pub registro_incierto_desde: Option<DateTime<Utc>>,
}

/// Fecha calendario de la Ciudad de México (UTC-6, como `hoy_cdmx`).
fn fecha_cdmx(t: DateTime<Utc>) -> NaiveDate {
    (t - chrono::Duration::hours(6)).date_naive()
}

impl SpeiOutbound {
    fn orden_pasarela(&self) -> OrdenPasarela {
        OrdenPasarela {
            clave_rastreo: self.clave_rastreo.clone(),
            fecha_solicitud: self.fecha_solicitud,
            monto: self.monto,
            ordenante_nombre: self.ordenante_nombre.clone(),
            ordenante_clabe: self.ordenante_clabe.clone(),
            ordenante_rfc_curp: self.ordenante_rfc_curp.clone(),
            banco_receptor: self.banco_receptor.clone(),
            beneficiario_clabe: self.beneficiario_clabe.clone(),
            beneficiario_nombre: self.beneficiario_nombre.clone(),
            beneficiario_rfc_curp: self.beneficiario_rfc_curp.clone(),
            concepto: self.concepto.clone(),
            referencia_numerica: self.referencia_numerica,
        }
    }

    /// Consulta al proveedor. Sin fecha de operación confirmada, las fechas
    /// candidatas cubren toda fecha que pudo asignar alguna llamada a
    /// `registrar`: de la fecha de solicitud a la de la última llamada
    /// (`registro_intentado_en`) más [`super::DIAS_FECHA_OPERACION_FUTURA`].
    /// Un primer registro hecho días después de la solicitud (despachador
    /// caído, cola atrasada) también se encuentra.
    pub(crate) fn consulta(&self) -> ConsultaOrden {
        let mut fechas: Vec<NaiveDate> = self.fecha_operacion.into_iter().collect();
        let desde = self.fecha_solicitud;
        let ultima = self.registro_intentado_en.map(fecha_cdmx).unwrap_or(desde).max(desde);
        let hasta = ultima + chrono::Days::new(super::DIAS_FECHA_OPERACION_FUTURA);
        for f in desde.iter_days().take_while(|f| *f <= hasta) {
            if !fechas.contains(&f) {
                fechas.push(f);
            }
        }
        ConsultaOrden {
            clave_rastreo: self.clave_rastreo.clone(),
            id_proveedor: self.id_proveedor.clone(),
            fechas_candidatas: fechas,
        }
    }
}

// -----------------------------------------------------------------------------
// Solicitud
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SolicitudSpei {
    pub cliente_id: Uuid,
    pub clabe_destino: String,
    /// `None`: retiro a una cuenta propia (el beneficiario es el cliente).
    pub beneficiario_nombre: Option<String>,
    pub beneficiario_rfc_curp: Option<String>,
    pub monto: Decimal,
    pub concepto: Option<String>,
    pub referencia_numerica: Option<i32>,
    /// Clave ya acotada al cliente (`cli:<id>:<clave>`).
    pub clave_idempotencia: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoOrden {
    pub orden: SpeiOutbound,
    /// El RETIRO del ledger.
    pub transaccion: TransaccionIfpe,
    pub idempotente: bool,
}

struct SolicitudValida {
    clabe: Clabe,
    nombre: Option<String>,
    rfc: Option<String>,
    monto: Decimal,
    concepto: String,
    referencia: i32,
}

fn rfc_curp(valor: Option<&str>, campo: &str) -> AppResult<Option<String>> {
    match valor.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => {
            let r = r.to_uppercase();
            let n = r.chars().count();
            if !(12..=18).contains(&n) || !r.chars().all(|c| c.is_ascii_alphanumeric() || c == '&' || c == 'Ñ') {
                return Err(AppError::validacion(format!("{campo}: RFC (12-13) o CURP (18) inválido")));
            }
            Ok(Some(r))
        }
        None => Ok(None),
    }
}

fn validar(s: &SolicitudSpei) -> AppResult<SolicitudValida> {
    let clabe = Clabe::parse(s.clabe_destino.trim())
        .map_err(|e| AppError::validacion(format!("CLABE destino inválida: {e}")))?;
    let monto = validar_monto_spei(s.monto)?;
    let nombre = s.beneficiario_nombre.as_deref().map(|n| texto_spei("beneficiario_nombre", n, 40)).transpose()?;
    let rfc = rfc_curp(s.beneficiario_rfc_curp.as_deref(), "beneficiario_rfc_curp")?;
    let concepto = texto_spei("concepto", s.concepto.as_deref().unwrap_or("TRANSFERENCIA SPEI"), 40)?;
    let referencia = s.referencia_numerica.unwrap_or(1);
    if !(1..=9_999_999).contains(&referencia) {
        return Err(AppError::validacion("referencia_numerica debe tener de 1 a 7 dígitos"));
    }
    Ok(SolicitudValida { clabe, nombre, rfc, monto, concepto, referencia })
}

/// Datos del cliente como ordenante (nombre apto para SPEI y RFC/CURP).
struct Ordenante {
    nombre: String,
    rfc_curp: Option<String>,
}

async fn ordenante(tx: &mut Tx, cliente_id: Uuid) -> AppResult<Ordenante> {
    let (nombre, paterno, materno, rfc, curp): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as("SELECT nombre, apellido_paterno, apellido_materno, rfc, curp FROM kyc.clientes WHERE id = $1")
        .bind(cliente_id)
        .fetch_one(&mut **tx)
        .await?;
    let completo = [Some(nombre), paterno, materno].into_iter().flatten().collect::<Vec<_>>().join(" ");
    // Se omiten los caracteres que SPEI no admite en lugar de rechazar la operación.
    let apto: String = completo
        .chars()
        .map(|c| match texto_spei("nombre", &c.to_string(), 1) {
            Ok(t) => t,
            Err(_) => " ".to_string(),
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let apto: String = apto.chars().take(40).collect::<String>().trim().to_owned();
    Ok(Ordenante {
        nombre: if apto.is_empty() { "CLIENTE SYNTHETIC CAPITAL".into() } else { apto },
        rfc_curp: rfc_curp(rfc.or(curp).as_deref(), "rfc").unwrap_or(None),
    })
}

/// Los umbrales PLD viven en la base de datos (`aml.reglas_riesgo`); el
/// parámetro de configuración se conserva para que los llamadores no cambien.
pub async fn solicitar(
    pool: &PgPool,
    _aml: &ConfigAml,
    cfg: &ConfigSpei,
    s: &SolicitudSpei,
) -> AppResult<ResultadoOrden> {
    let v = validar(s)?;
    db::con_reintentos(|| solicitar_tx(pool, cfg, s, &v)).await
}

async fn solicitar_tx(
    pool: &PgPool,
    cfg: &ConfigSpei,
    s: &SolicitudSpei,
    v: &SolicitudValida,
) -> AppResult<ResultadoOrden> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    let cuenta_id = ledger::id_cuenta_ifpe_de_cliente(&mut tx, s.cliente_id).await?;
    let cuenta = ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let ord = ordenante(&mut tx, s.cliente_id).await?;
    let (nombre, rfc) = match &v.nombre {
        Some(n) => (n.clone(), v.rfc.clone()),
        // Retiro a cuenta propia: el beneficiario es el mismo cliente.
        None => (ord.nombre.clone(), v.rfc.clone().or_else(|| ord.rfc_curp.clone())),
    };

    // Idempotencia (después de bloquear: la segunda petición ve la primera).
    let previa: Option<SpeiOutbound> = sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE clave_idempotencia = $1")
        .bind(&s.clave_idempotencia)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(previa) = previa {
        if previa.cuenta_id != cuenta.id
            || previa.beneficiario_clabe != v.clabe.as_str()
            || previa.monto != v.monto
            || previa.beneficiario_nombre != nombre
            || previa.beneficiario_rfc_curp != rfc
            || previa.concepto != v.concepto
            || previa.referencia_numerica != v.referencia
        {
            return Err(AppError::conflicto(
                "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                "la clave de idempotencia ya se usó para una transferencia distinta",
            ));
        }
        let transaccion = transaccion(&mut tx, previa.tx_retiro_id).await?;
        tx.commit().await?;
        return Ok(ResultadoOrden { orden: previa, transaccion, idempotente: true });
    }

    let propia: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ifpe.clabes_asignadas WHERE clabe = $1)")
        .bind(v.clabe.as_str())
        .fetch_one(&mut *tx)
        .await?;
    if propia {
        return Err(AppError::negocio(
            "CLABE_PROPIA",
            "la CLABE destino es de Synthetic Capital: usa una transferencia interna (sin costo ni espera)",
        ));
    }
    let perfil = pld::cargar_perfil(&mut tx, cuenta.cliente_id).await?;
    perfil.exigir_operable()?;
    ledger::exigir_debitable_salvo_aml(&cuenta)?;
    ledger::exigir_saldo(&cuenta, v.monto)?;

    // PLD antes de cualquier escritura (y antes del candado de la reserva, del
    // consecutivo de rastreo y de la orden): un BLOQUEAR confirma sin RETIRO
    // ni orden.
    let retiro_id = Uuid::new_v4();
    let d = aml::evaluar(
        &mut tx,
        &Operacion {
            cliente_id: cuenta.cliente_id,
            canal: Canal::SpeiEnviado,
            monto: v.monto,
            transaccion_id: retiro_id,
            clave: &s.clave_idempotencia,
            contraparte: Contraparte {
                nombre: Some(nombre.clone()),
                cuenta: Some(v.clabe.as_str().into()),
                rfc_curp: rfc.clone(),
                cliente_id: None,
            },
        },
    )
    .await?;
    if d.decision == Decision::Bloquear {
        return aml::confirmar_bloqueo(tx, &d).await;
    }
    let reserva = ledger::bloquear_reserva(&mut tx).await?;

    // Pasivo y reserva bajan juntos: el dinero queda comprometido en la orden.
    let t = ledger::cargar_efectivo(
        &mut tx,
        retiro_id,
        cuenta.id,
        reserva.id,
        v.monto,
        Some(&v.concepto),
        Some(v.clabe.as_str()),
        &s.clave_idempotencia,
    )
    .await?;
    // Clave de rastreo única para siempre: <prefijo><AAAAMMDD><consecutivo>.
    let (clave_rastreo, fecha_solicitud): (String, NaiveDate) = sqlx::query_as(
        "SELECT $1 || to_char(f, 'YYYYMMDD') || lpad(nextval('ifpe.spei_rastreo_seq')::text, 12, '0'), f \
         FROM (SELECT (now() AT TIME ZONE 'America/Mexico_City')::date AS f) d",
    )
    .bind(&cfg.prefijo_rastreo)
    .fetch_one(&mut *tx)
    .await?;
    let orden: SpeiOutbound = sqlx::query_as(
        "INSERT INTO ifpe.spei_outbound \
         (cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud, ordenante_nombre, \
          ordenante_clabe, ordenante_rfc_curp, banco_receptor, beneficiario_clabe, beneficiario_nombre, \
          beneficiario_rfc_curp, monto, concepto, referencia_numerica, tx_retiro_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) RETURNING *",
    )
    .bind(s.cliente_id)
    .bind(cuenta.id)
    .bind(&s.clave_idempotencia)
    .bind(&clave_rastreo)
    .bind(fecha_solicitud)
    .bind(&ord.nombre)
    .bind(&cuenta.clabe)
    .bind(&ord.rfc_curp)
    .bind(v.clabe.banco())
    .bind(v.clabe.as_str())
    .bind(&nombre)
    .bind(&rfc)
    .bind(v.monto)
    .bind(&v.concepto)
    .bind(v.referencia)
    .bind(t.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(rastreo = %orden.clave_rastreo, monto = %orden.monto, banco = %orden.banco_receptor, "orden SPEI registrada");
    Ok(ResultadoOrden { orden, transaccion: t, idempotente: false })
}

async fn transaccion(tx: &mut Tx, id: Uuid) -> AppResult<TransaccionIfpe> {
    Ok(sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE id = $1").bind(id).fetch_one(&mut **tx).await?)
}

// -----------------------------------------------------------------------------
// Resultados del proveedor
// -----------------------------------------------------------------------------

/// Resultado del proveedor (o del despachador) para una orden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evento {
    /// El proveedor tiene la orden (aceptada o en proceso).
    Aceptada {
        id_proveedor: Option<String>,
        fecha_operacion: Option<NaiveDate>,
    },
    /// Sigue en proceso: volver a consultar.
    EnProceso {
        fecha_operacion: Option<NaiveDate>,
    },
    Liquidada {
        fecha_operacion: Option<NaiveDate>,
    },
    /// El proveedor informa (consulta o webhook) que canceló o rechazó la
    /// orden: el dinero no salió.
    Rechazada {
        causa: i16,
        motivo: String,
    },
    /// El proveedor rechazó el registro de un primer intento. Es definitivo
    /// solo si el reclamo sigue vigente: la orden sigue PENDIENTE con los
    /// mismos `intentos_envio` (nadie la reclamó después de vencer el
    /// arrendamiento); si no, revisión manual.
    RechazoRegistro {
        causa: i16,
        motivo: String,
        intentos_envio: i32,
    },
    /// Devuelta por el banco receptor después de liquidar: el dinero regresó.
    Devuelta {
        causa: i16,
        motivo: String,
    },
    /// Resultado desconocido: reintentar más tarde, sin reembolsar.
    Incierto {
        error: String,
    },
    /// La llamada a `registrar` no llegó al proveedor: como `Incierto`, pero
    /// si el reclamo sigue vigente (`intentos_envio`) y fue esta llamada la
    /// que fijó `registro_incierto_desde` (`registro_desde`), se borra: nada
    /// pudo registrarse y la regla del corte no aplica.
    NoRecibida {
        error: String,
        intentos_envio: i32,
        registro_desde: Option<DateTime<Utc>>,
    },
    /// Resultado ambiguo que una persona debe resolver; sin reembolso.
    RevisionManual {
        motivo: String,
    },
    /// Liquidada y sin novedad: seguir vigilando por si llega una devolución.
    SeguirVigilando,
}

impl Evento {
    /// Nombre del evento en `ifpe.spei_incidentes`.
    fn nombre(&self) -> &'static str {
        match self {
            Evento::Aceptada { .. } => "ACEPTADA",
            Evento::EnProceso { .. } => "EN_PROCESO",
            Evento::Liquidada { .. } => "LIQUIDADA",
            Evento::Rechazada { .. } => "RECHAZADA",
            Evento::Devuelta { .. } => "DEVUELTA",
            Evento::RechazoRegistro { .. } => "RECHAZO_REGISTRO",
            Evento::Incierto { .. } => "INCIERTO",
            Evento::NoRecibida { .. } => "NO_RECIBIDA",
            Evento::RevisionManual { .. } => "REVISION_MANUAL",
            Evento::SeguirVigilando => "SEGUIR_VIGILANDO",
        }
    }
}

/// Estado de la orden después de aplicar un evento.
#[derive(Debug, Clone, Serialize)]
pub struct Transicion {
    pub orden: SpeiOutbound,
    pub cambio: bool,
    /// Resultado incompatible con el estado final: no se movió dinero, el
    /// incidente quedó en `ifpe.spei_incidentes`, la orden quedó marcada para
    /// revisión (si no es final) y se responde 409.
    #[serde(skip)]
    pub incidente: Option<String>,
}

impl Transicion {
    pub fn exigir_sin_incidente(self) -> AppResult<Self> {
        match self.incidente {
            Some(detalle) => Err(AppError::conflicto("SPEI_ESTADO_INCONSISTENTE", detalle)),
            None => Ok(self),
        }
    }
}

/// Aplica un resultado a la orden en su propia transacción (rol `sc_spei`).
/// Si el reverso no se puede aplicar porque la cuenta está cerrada, la orden
/// queda marcada para revisión manual (en otra transacción) y se devuelve el
/// error.
pub async fn aplicar(pool: &PgPool, cfg: &ConfigSpei, orden_id: Uuid, evento: &Evento) -> AppResult<Transicion> {
    let r = db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Spei).await?;
        let t = aplicar_en(&mut tx, cfg, orden_id, evento).await?;
        // También se confirman el incidente y la marca de revisión.
        tx.commit().await?;
        Ok(t)
    })
    .await;
    match r {
        Err(e) if e.codigo() == CUENTA_CERRADA => {
            marcar_revision(pool, orden_id, &format!("reverso imposible: {}", e.detalle())).await?;
            Err(e)
        }
        r => r?.exigir_sin_incidente(),
    }
}

/// Código del error de un reverso hacia una cuenta cerrada.
pub const CUENTA_CERRADA: &str = "CUENTA_CERRADA";

/// Marca para revisión manual, en su propia transacción, una orden que aún
/// no es final (sin mover dinero).
pub async fn marcar_revision(pool: &PgPool, orden_id: Uuid, motivo: &str) -> AppResult<()> {
    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Spei).await?;
        let o: Option<SpeiOutbound> = sqlx::query_as(
            "SELECT * FROM ifpe.spei_outbound WHERE id = $1 AND estado IN ('PENDIENTE', 'ENVIADA', 'LIQUIDADA') \
             FOR UPDATE",
        )
        .bind(orden_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(o) = o {
            revision_manual(&mut tx, &o, motivo).await?;
        }
        tx.commit().await?;
        Ok(())
    })
    .await
}

/// ¿El proveedor informa un registro distinto del que la orden ya tiene
/// (otro id u otra fecha de operación)? Es un segundo registro aceptado de la
/// misma orden: posible pago doble.
fn segundo_registro(o: &SpeiOutbound, id_proveedor: Option<&str>, fecha_operacion: Option<NaiveDate>) -> bool {
    let otro_id = matches!((o.id_proveedor.as_deref(), id_proveedor), (Some(a), Some(b)) if a != b);
    let otra_fecha = matches!((o.fecha_operacion, fecha_operacion), (Some(a), Some(b)) if a != b);
    otro_id || otra_fecha
}

/// Aplica un resultado dentro de una transacción `sc_spei` abierta.
/// Idempotente: el mismo evento dos veces no cambia nada la segunda vez. Un
/// evento que contradice un estado final (p. ej. "liquidada" de una orden ya
/// reembolsada) es un incidente de conciliación: no se toca el dinero, se
/// registra en `ifpe.spei_incidentes`, se marca para revisión (si la orden no
/// es RECHAZADA/DEVUELTA, que ya no cambian) y se responde 409.
pub async fn aplicar_en(tx: &mut Tx, cfg: &ConfigSpei, orden_id: Uuid, evento: &Evento) -> AppResult<Transicion> {
    // La orden primero: serializa despachador, webhook y devoluciones.
    let o: SpeiOutbound = sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE id = $1 FOR UPDATE")
        .bind(orden_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("orden SPEI no encontrada"))?;
    let estado = o.estado.as_str();
    let nueva = match (estado, evento) {
        // Un segundo registro aceptado de una orden que el proveedor ya tiene
        // (p. ej. un reenvío tardío aceptado en otro día de operación): el
        // proveedor puede pagarla dos veces. Evidencia y revisión manual.
        ("ENVIADA" | "LIQUIDADA" | "DEVUELTA", Evento::Aceptada { id_proveedor, fecha_operacion })
            if segundo_registro(&o, id_proveedor.as_deref(), *fecha_operacion) =>
        {
            tracing::error!(
                rastreo = %o.clave_rastreo,
                estado,
                ?evento,
                "INCIDENTE DE CONCILIACIÓN SPEI: segundo registro aceptado de una orden (posible pago doble)"
            );
            registrar_incidente(tx, &o, evento).await?;
            let orden = if estado == "DEVUELTA" {
                o.clone()
            } else {
                revision_manual(tx, &o, &format!("el proveedor aceptó un segundo registro de la orden: {evento:?}"))
                    .await?
            };
            return Ok(Transicion {
                orden,
                cambio: false,
                incidente: Some(format!(
                    "la orden {} ya tenía un registro con el proveedor; el segundo requiere revisión manual",
                    o.clave_rastreo
                )),
            });
        }
        ("PENDIENTE", Evento::Aceptada { id_proveedor, fecha_operacion }) => {
            Some(enviada(tx, cfg, &o, id_proveedor.as_deref(), *fecha_operacion).await?)
        }
        ("ENVIADA", Evento::Aceptada { fecha_operacion, .. } | Evento::EnProceso { fecha_operacion }) => {
            Some(reprogramar(tx, &o, cfg.consulta_intervalo_seg as f64, *fecha_operacion).await?)
        }
        ("PENDIENTE", Evento::EnProceso { fecha_operacion }) => {
            Some(enviada(tx, cfg, &o, None, *fecha_operacion).await?)
        }
        ("PENDIENTE" | "ENVIADA", Evento::Liquidada { fecha_operacion }) => {
            Some(liquidar(tx, cfg, &o, *fecha_operacion).await?)
        }
        ("PENDIENTE" | "ENVIADA", Evento::Rechazada { causa, motivo }) => {
            Some(reembolsar(tx, &o, "RECHAZADA", *causa, motivo).await?)
        }
        // Rechazo del registro de un primer intento cuyo reclamo sigue
        // vigente: nadie más pudo registrarla, el rechazo es definitivo.
        ("PENDIENTE", Evento::RechazoRegistro { causa, motivo, intentos_envio })
            if o.intentos_envio == *intentos_envio && !o.revision_manual =>
        {
            Some(reembolsar(tx, &o, "RECHAZADA", *causa, motivo).await?)
        }
        // Reclamo vencido: otro despachador la reclamó y pudo registrarla.
        ("PENDIENTE" | "ENVIADA", Evento::RechazoRegistro { causa, motivo, intentos_envio }) => Some(
            revision_manual(
                tx,
                &o,
                &format!(
                    "rechazo de registro de un reclamo vencido (intento {intentos_envio}; la orden va en {} y está \
                     {estado}; causa {causa}: {motivo}); otro despachador pudo registrarla",
                    o.intentos_envio
                ),
            )
            .await?,
        ),
        // La orden ya tiene resultado del proveedor: un rechazo tardío de un
        // registro duplicado no dice nada del dinero.
        ("LIQUIDADA" | "RECHAZADA" | "DEVUELTA", Evento::RechazoRegistro { .. }) => {
            tracing::warn!(rastreo = %o.clave_rastreo, estado, ?evento, "rechazo de registro tardío ignorado");
            None
        }
        ("PENDIENTE" | "ENVIADA" | "LIQUIDADA", Evento::Devuelta { causa, motivo }) => {
            let liquidada = if estado == "LIQUIDADA" { o.clone() } else { liquidar(tx, cfg, &o, None).await? };
            Some(reembolsar(tx, &liquidada, "DEVUELTA", *causa, motivo).await?)
        }
        ("PENDIENTE" | "ENVIADA", Evento::Incierto { error }) => Some(incierto(tx, cfg, &o, error).await?),
        // La llamada no llegó al proveedor: si el reclamo sigue vigente y esa
        // misma llamada fijó `registro_incierto_desde`, se borra.
        ("PENDIENTE", Evento::NoRecibida { error, intentos_envio, registro_desde }) => {
            let vigente = match registro_desde {
                Some(desde) => sqlx::query_as(
                    "UPDATE ifpe.spei_outbound SET registro_incierto_desde = NULL \
                     WHERE id = $1 AND intentos_envio = $2 AND registro_incierto_desde = $3 RETURNING *",
                )
                .bind(o.id)
                .bind(intentos_envio)
                .bind(desde)
                .fetch_optional(&mut **tx)
                .await?
                .unwrap_or_else(|| o.clone()),
                None => o.clone(),
            };
            Some(incierto(tx, cfg, &vigente, error).await?)
        }
        ("ENVIADA", Evento::NoRecibida { error, .. }) => Some(incierto(tx, cfg, &o, error).await?),
        ("PENDIENTE" | "ENVIADA" | "LIQUIDADA", Evento::RevisionManual { motivo }) => {
            Some(revision_manual(tx, &o, motivo).await?)
        }
        (
            "LIQUIDADA",
            Evento::SeguirVigilando | Evento::Incierto { .. } | Evento::NoRecibida { .. } | Evento::EnProceso { .. },
        ) => Some(reprogramar(tx, &o, 6.0 * 3_600.0, None).await?),
        // Repeticiones de un estado ya registrado: sin cambio.
        ("LIQUIDADA" | "DEVUELTA", Evento::Liquidada { .. } | Evento::Aceptada { .. }) => None,
        ("RECHAZADA" | "DEVUELTA", Evento::Rechazada { .. } | Evento::Devuelta { .. }) => None,
        ("RECHAZADA" | "DEVUELTA", Evento::Incierto { .. } | Evento::SeguirVigilando | Evento::EnProceso { .. }) => {
            None
        }
        ("LIQUIDADA", Evento::Rechazada { .. }) | ("RECHAZADA", Evento::Liquidada { .. } | Evento::Aceptada { .. }) => {
            tracing::error!(
                rastreo = %o.clave_rastreo,
                estado,
                ?evento,
                "INCIDENTE DE CONCILIACIÓN SPEI: el proveedor informa un resultado incompatible con el estado final"
            );
            registrar_incidente(tx, &o, evento).await?;
            // Una orden RECHAZADA ya no cambia (su reverso está ligado a la
            // fila): el incidente queda solo en `ifpe.spei_incidentes`.
            let orden = if estado == "LIQUIDADA" {
                revision_manual(tx, &o, &format!("el proveedor informa {evento:?} de una orden liquidada")).await?
            } else {
                o.clone()
            };
            return Ok(Transicion {
                orden,
                cambio: false,
                incidente: Some(format!(
                    "la orden {} está {estado}; el resultado informado requiere revisión manual",
                    o.clave_rastreo
                )),
            });
        }
        (_, _) => None,
    };
    Ok(match nueva {
        Some(orden) => Transicion { orden, cambio: true, incidente: None },
        None => Transicion { orden: o, cambio: false, incidente: None },
    })
}

async fn enviada(
    tx: &mut Tx,
    cfg: &ConfigSpei,
    o: &SpeiOutbound,
    id_proveedor: Option<&str>,
    fecha_operacion: Option<NaiveDate>,
) -> AppResult<SpeiOutbound> {
    Ok(sqlx::query_as(
        "UPDATE ifpe.spei_outbound SET estado = 'ENVIADA', enviada_en = coalesce(enviada_en, now()), \
         id_proveedor = coalesce(id_proveedor, $2), fecha_operacion = coalesce(fecha_operacion, $3), \
         ultimo_error = NULL, proximo_intento_en = now() + make_interval(secs => $4) WHERE id = $1 RETURNING *",
    )
    .bind(o.id)
    .bind(id_proveedor)
    .bind(fecha_operacion)
    .bind(cfg.consulta_intervalo_seg as f64)
    .fetch_one(&mut **tx)
    .await?)
}

async fn reprogramar(
    tx: &mut Tx,
    o: &SpeiOutbound,
    espera_seg: f64,
    fecha_operacion: Option<NaiveDate>,
) -> AppResult<SpeiOutbound> {
    Ok(sqlx::query_as(
        "UPDATE ifpe.spei_outbound SET fecha_operacion = coalesce(fecha_operacion, $2), \
         proximo_intento_en = now() + make_interval(secs => $3) WHERE id = $1 RETURNING *",
    )
    .bind(o.id)
    .bind(fecha_operacion)
    .bind(espera_seg)
    .fetch_one(&mut **tx)
    .await?)
}

async fn liquidar(
    tx: &mut Tx,
    cfg: &ConfigSpei,
    o: &SpeiOutbound,
    fecha_operacion: Option<NaiveDate>,
) -> AppResult<SpeiOutbound> {
    if o.estado == "PENDIENTE" {
        enviada(tx, cfg, o, None, fecha_operacion).await?;
    }
    // Se sigue consultando por si el banco receptor la devuelve.
    let orden = sqlx::query_as(
        "UPDATE ifpe.spei_outbound SET estado = 'LIQUIDADA', liquidada_en = now(), ultimo_error = NULL, \
         revision_manual = FALSE, fecha_operacion = coalesce(fecha_operacion, $2), \
         proximo_intento_en = now() + interval '1 hour' WHERE id = $1 RETURNING *",
    )
    .bind(o.id)
    .bind(fecha_operacion)
    .fetch_one(&mut **tx)
    .await?;
    tracing::info!(rastreo = %o.clave_rastreo, monto = %o.monto, "orden SPEI liquidada");
    Ok(orden)
}

/// Reverso: el dinero vuelve a la cuenta y a la misma reserva de la que salió,
/// en la misma transacción que cierra la orden.
async fn reembolsar(tx: &mut Tx, o: &SpeiOutbound, estado: &str, causa: i16, motivo: &str) -> AppResult<SpeiOutbound> {
    // Orden global de bloqueo: cuenta -> reserva.
    let cuenta = ledger::bloquear_cuenta_ifpe(tx, o.cuenta_id).await?;
    if cuenta.estado == EstadoCuenta::Cerrada {
        tracing::error!(rastreo = %o.clave_rastreo, "reverso SPEI hacia una cuenta cerrada: requiere revisión manual");
        return Err(AppError::negocio(
            CUENTA_CERRADA,
            "la cuenta de la orden está cerrada; el reverso requiere revisión",
        ));
    }
    let reserva_id: Uuid = sqlx::query_scalar("SELECT reserva_id FROM ifpe.transacciones WHERE id = $1")
        .bind(o.tx_retiro_id)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("SELECT 1 FROM ifpe.reserva_custodia WHERE id = $1 FOR NO KEY UPDATE")
        .bind(reserva_id)
        .execute(&mut **tx)
        .await?;
    let motivo: String = motivo.chars().filter(|c| !c.is_control()).take(100).collect();
    let concepto = format!("Devolución SPEI {}: {}", o.clave_rastreo, motivo);
    let t = ledger::abonar_efectivo(
        tx,
        Uuid::new_v4(),
        o.cuenta_id,
        reserva_id,
        o.monto,
        Some(&concepto),
        Some(&format!("devolucion:{}", o.clave_rastreo)),
        &format!("spei-reverso:{}", o.id),
    )
    .await?;
    let orden = sqlx::query_as(
        "UPDATE ifpe.spei_outbound SET estado = $2, tx_reverso_id = $3, causa_devolucion = $4, motivo = $5, \
         revision_manual = FALSE, finalizada_en = now() WHERE id = $1 RETURNING *",
    )
    .bind(o.id)
    .bind(estado)
    .bind(t.id)
    .bind(causa.clamp(1, 99))
    .bind(&motivo)
    .fetch_one(&mut **tx)
    .await?;
    tracing::warn!(rastreo = %o.clave_rastreo, monto = %o.monto, estado, causa, %motivo, "orden SPEI reembolsada al cliente");
    Ok(orden)
}

/// Evidencia de un resultado incompatible con el estado final de la orden.
/// Los reintentos del proveedor no la duplican.
async fn registrar_incidente(tx: &mut Tx, o: &SpeiOutbound, evento: &Evento) -> AppResult<()> {
    let detalle: String = format!("{evento:?}").chars().filter(|c| !c.is_control()).take(500).collect();
    sqlx::query(
        "INSERT INTO ifpe.spei_incidentes (spei_outbound_id, estado_orden, evento, detalle) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (spei_outbound_id, estado_orden, evento) DO NOTHING",
    )
    .bind(o.id)
    .bind(&o.estado)
    .bind(evento.nombre())
    .bind(detalle)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Resultado desconocido. Las PENDIENTE ya contaron su intento al
/// reclamarse; las ENVIADA cuentan aquí cada consulta fallida. Una orden ya
/// en revisión manual se queda en el ciclo de una hora (sin perder la causa).
async fn incierto(tx: &mut Tx, cfg: &ConfigSpei, o: &SpeiOutbound, error: &str) -> AppResult<SpeiOutbound> {
    let intentos = if o.estado == "ENVIADA" { o.intentos_envio + 1 } else { o.intentos_envio };
    if o.revision_manual || intentos >= cfg.max_intentos {
        return revision_manual(tx, o, &format!("sin resultado definitivo tras {intentos} intentos: {error}")).await;
    }
    // Espera exponencial con piso: el proveedor necesita tiempo para mostrar
    // una orden que sí registró.
    let espera = (2f64.powi(intentos.clamp(0, 8))).max(cfg.espera_incierto_seg as f64).min(300.0);
    let error: String = error.chars().filter(|c| !c.is_control()).take(200).collect();
    Ok(sqlx::query_as(
        "UPDATE ifpe.spei_outbound SET intentos_envio = $2, ultimo_error = $3, \
         proximo_intento_en = now() + make_interval(secs => $4) WHERE id = $1 RETURNING *",
    )
    .bind(o.id)
    .bind(intentos)
    .bind(error)
    .bind(espera)
    .fetch_one(&mut **tx)
    .await?)
}

async fn revision_manual(tx: &mut Tx, o: &SpeiOutbound, motivo: &str) -> AppResult<SpeiOutbound> {
    let motivo: String = motivo.chars().filter(|c| !c.is_control()).take(300).collect();
    tracing::error!(
        rastreo = %o.clave_rastreo,
        monto = %o.monto,
        estado = %o.estado,
        %motivo,
        "orden SPEI en revisión manual: resultado ambiguo, NO se reembolsa automáticamente"
    );
    // Se sigue consultando cada hora: un resultado definitivo la resuelve. Se
    // conserva la primera causa (la que el operador necesita); las siguientes
    // quedan en el log. `reembolsar_revision` y `ifpe.spei_resoluciones`
    // exigen este prefijo 'REVISION_MANUAL:'.
    Ok(sqlx::query_as(
        "UPDATE ifpe.spei_outbound SET revision_manual = TRUE, \
         ultimo_error = CASE WHEN revision_manual AND ultimo_error LIKE 'REVISION_MANUAL:%' \
                             THEN ultimo_error ELSE $2 END, \
         proximo_intento_en = now() + interval '1 hour' WHERE id = $1 RETURNING *",
    )
    .bind(o.id)
    .bind(format!("REVISION_MANUAL: {motivo}"))
    .fetch_one(&mut **tx)
    .await?)
}

// -----------------------------------------------------------------------------
// Resolución del operador
// -----------------------------------------------------------------------------

/// Reembolso, por el operador, de una orden en revisión manual que el
/// proveedor nunca recibió.
#[derive(Debug, Clone)]
pub struct SolicitudResolucion {
    pub orden_id: Uuid,
    /// Qué confirmó el operador con el proveedor (10 a 300 caracteres).
    pub motivo: String,
    pub operador_id: Uuid,
    /// Clave ya acotada al operador.
    pub clave_idempotencia: String,
}

/// Registro de `ifpe.spei_resoluciones`.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Resolucion {
    pub id: Uuid,
    pub spei_outbound_id: Uuid,
    pub accion: String,
    pub motivo: String,
    /// Causa de la revisión manual al momento de resolver.
    pub error_previo: String,
    pub autorizado_por: Uuid,
    #[serde(skip)]
    pub clave_idempotencia: String,
    pub creado_en: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultadoResolucion {
    pub orden: SpeiOutbound,
    pub resolucion: Resolucion,
    /// El reverso (DEPOSITO `spei-reverso:<orden>`).
    pub transaccion: TransaccionIfpe,
    pub idempotente: bool,
}

/// Solo una orden PENDIENTE en revisión manual que el proveedor nunca aceptó
/// (sin id, fecha de operación ni envío confirmado). La base de datos exige
/// lo mismo al COMMIT (`ifpe.fn_verificar_spei_resolucion`).
fn exigir_reembolsable(o: &SpeiOutbound) -> AppResult<()> {
    let en_revision = o.ultimo_error.as_deref().is_some_and(|e| e.starts_with("REVISION_MANUAL:"));
    if o.estado != "PENDIENTE"
        || !o.revision_manual
        || !en_revision
        || o.id_proveedor.is_some()
        || o.fecha_operacion.is_some()
        || o.enviada_en.is_some()
    {
        return Err(AppError::negocio(
            "SPEI_SIN_REVISION",
            "solo se reembolsa una orden PENDIENTE en revisión manual que el proveedor nunca aceptó",
        ));
    }
    Ok(())
}

/// Resolución ya registrada: la misma solicitud (idempotente) o un conflicto.
async fn resolucion_previa(tx: &mut Tx, s: &SolicitudResolucion) -> AppResult<Option<ResultadoResolucion>> {
    let por_clave: Option<Resolucion> =
        sqlx::query_as("SELECT * FROM ifpe.spei_resoluciones WHERE clave_idempotencia = $1")
            .bind(&s.clave_idempotencia)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(r) = por_clave {
        if r.spei_outbound_id != s.orden_id || r.motivo != s.motivo.trim() {
            return Err(AppError::conflicto(
                "CLAVE_IDEMPOTENCIA_REUTILIZADA",
                "la clave de idempotencia ya se usó para otra resolución",
            ));
        }
        let orden: SpeiOutbound = sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE id = $1")
            .bind(s.orden_id)
            .fetch_one(&mut **tx)
            .await?;
        let reverso = orden.tx_reverso_id.ok_or_else(|| AppError::Integridad("resolución sin reverso".into()))?;
        let transaccion = transaccion(tx, reverso).await?;
        return Ok(Some(ResultadoResolucion { orden, resolucion: r, transaccion, idempotente: true }));
    }
    let resuelta: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ifpe.spei_resoluciones WHERE spei_outbound_id = $1)")
            .bind(s.orden_id)
            .fetch_one(&mut **tx)
            .await?;
    if resuelta {
        return Err(AppError::conflicto("SPEI_YA_RESUELTA", "la orden ya fue resuelta por un operador"));
    }
    Ok(None)
}

/// Reembolsa una orden en revisión manual después de confirmar, consultando
/// al proveedor en ese momento, que no la tiene. Exige que el despachador no
/// haya tocado la orden durante `espera_resolucion_seg` (ningún registro
/// tardío en vuelo: las órdenes en revisión nunca se reenvían, todo
/// `registrar` exige antes un reclamo vigente sin revisión manual y dura a lo
/// sumo [`LLAMADA_PASARELA_SEG`]). La consulta cubre todas las fechas de
/// operación que pudo asignar cualquier llamada a `registrar`. El reverso es
/// el de siempre (`spei-reverso:<orden>`), ligado a la resolución en la misma
/// transacción. Nunca reembolsa una orden que el proveedor aceptó.
pub async fn reembolsar_revision(
    pool: &PgPool,
    pasarela: &dyn Pasarela,
    cfg: &ConfigSpei,
    s: &SolicitudResolucion,
) -> AppResult<ResultadoResolucion> {
    let motivo = s.motivo.trim();
    if !(10..=300).contains(&motivo.chars().count()) || motivo.chars().any(char::is_control) {
        return Err(AppError::validacion("el motivo de la resolución debe tener de 10 a 300 caracteres"));
    }
    let mut tx = db::begin_lectura(pool, Rol::Spei).await?;
    if let Some(previa) = resolucion_previa(&mut tx, s).await? {
        tx.commit().await?;
        return Ok(previa);
    }
    let o: SpeiOutbound = sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE id = $1")
        .bind(s.orden_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::no_encontrado("orden SPEI no encontrada"))?;
    tx.commit().await?;
    exigir_reembolsable(&o)?;
    if o.cliente_id == s.operador_id {
        return Err(AppError::negocio("SPEI_AUTORESOLUCION", "un operador no resuelve sus propias órdenes"));
    }

    // Fuera de toda transacción: el proveedor debe decir, ahora, que no la tiene.
    match pasarela.consultar(&o.consulta()).await {
        Ok(EstadoProveedor::NoExiste) => {}
        Ok(estado) => {
            return Err(AppError::conflicto(
                "SPEI_PROVEEDOR_TIENE_ORDEN",
                format!("el proveedor informa {estado:?}: el despachador resolverá la orden; no se reembolsa"),
            ))
        }
        Err(e) => {
            return Err(AppError::peticion(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "SPEI_PROVEEDOR_NO_DISPONIBLE",
                format!("no se pudo consultar al proveedor: {e}"),
            ))
        }
    }

    db::con_reintentos(|| async {
        let mut tx = db::begin(pool, Rol::Spei).await?;
        let o: SpeiOutbound = sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE id = $1 FOR UPDATE")
            .bind(s.orden_id)
            .fetch_one(&mut *tx)
            .await?;
        // Una solicitud concurrente igual esperó el candado: repetición.
        if let Some(previa) = resolucion_previa(&mut tx, s).await? {
            tx.commit().await?;
            return Ok(previa);
        }
        exigir_reembolsable(&o)?;
        let en_reposo: bool = sqlx::query_scalar(
            "SELECT actualizado_en <= now() - make_interval(secs => $2) FROM ifpe.spei_outbound WHERE id = $1",
        )
        .bind(o.id)
        .bind(cfg.espera_resolucion_seg as f64)
        .fetch_one(&mut *tx)
        .await?;
        if !en_reposo {
            return Err(AppError::negocio(
                "SPEI_ORDEN_RECIENTE",
                format!(
                    "el despachador tocó la orden hace menos de {} s; reintente más tarde",
                    cfg.espera_resolucion_seg
                ),
            ));
        }
        let error_previo = o.ultimo_error.clone().unwrap_or_default();
        let orden =
            reembolsar(&mut tx, &o, "RECHAZADA", 99, &format!("reembolso por revisión manual: {motivo}")).await?;
        let resolucion: Resolucion = sqlx::query_as(
            "INSERT INTO ifpe.spei_resoluciones \
             (spei_outbound_id, accion, motivo, error_previo, autorizado_por, clave_idempotencia) \
             VALUES ($1, 'REEMBOLSO', $2, $3, $4, $5) RETURNING *",
        )
        .bind(o.id)
        .bind(motivo)
        .bind(&error_previo)
        .bind(s.operador_id)
        .bind(&s.clave_idempotencia)
        .fetch_one(&mut *tx)
        .await?;
        let reverso = orden.tx_reverso_id.ok_or_else(|| AppError::Integridad("reembolso sin reverso".into()))?;
        let transaccion = transaccion(&mut tx, reverso).await?;
        tx.commit().await?;
        tracing::warn!(
            rastreo = %orden.clave_rastreo,
            monto = %orden.monto,
            operador = %s.operador_id,
            "orden SPEI en revisión reembolsada por el operador"
        );
        Ok(ResultadoResolucion { orden, resolucion, transaccion, idempotente: false })
    })
    .await
}

// -----------------------------------------------------------------------------
// Despachador
// -----------------------------------------------------------------------------

/// Resumen de un ciclo del despachador.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Despacho {
    pub reclamadas: usize,
    pub cambios: usize,
    pub errores: usize,
}

/// Arrendamiento de una orden reclamada: tiempo para hablar con el proveedor
/// antes de que otro despachador pueda retomarla. Se renueva, verificando la
/// fila viva, justo antes de cada `registrar` ([`renovar_reclamo`]). Que venza
/// no reembolsa de más: el resultado de un primer intento se aplica solo si
/// `intentos_envio` sigue siendo el del reclamo (todo reclamo de una
/// PENDIENTE lo incrementa y lo confirma antes de llamar al proveedor; nadie
/// más llama a `registrar`).
const ARRENDAMIENTO_SEG: f64 = 60.0;

/// Tiempo máximo de una llamada al proveedor, muy por debajo de
/// [`ARRENDAMIENTO_SEG`]: un `registrar` iniciado con el reclamo renovado
/// termina antes de que otro despachador pueda retomar la orden. Agotarlo es
/// un resultado desconocido ([`ErrorPasarela::Tiempo`]).
pub const LLAMADA_PASARELA_SEG: u64 = 20;

async fn con_limite<T>(
    llamada: impl std::future::Future<Output = Result<T, ErrorPasarela>>,
) -> Result<T, ErrorPasarela> {
    tokio::time::timeout(std::time::Duration::from_secs(LLAMADA_PASARELA_SEG), llamada)
        .await
        .unwrap_or(Err(ErrorPasarela::Tiempo))
}

/// Un ciclo: reclama órdenes vencidas (contando el intento de las PENDIENTE
/// antes de llamar al proveedor), consulta o registra cada una y aplica el
/// resultado.
pub async fn despachar(pool: &PgPool, pasarela: &dyn Pasarela, cfg: &ConfigSpei) -> AppResult<Despacho> {
    let mut tx = db::begin(pool, Rol::Spei).await?;
    // Las órdenes nunca intentadas de un cliente con congelamiento preventivo
    // no se reclaman (`aml::evaluacion::FILTRO_DESPACHO`): el trigger que las
    // retiene las omitiría sin avanzar `proximo_intento_en` y acapararían el
    // LIMIT del lote.
    let sql = format!(
        "UPDATE ifpe.spei_outbound \
            SET proximo_intento_en = now() + make_interval(secs => $2), \
                intentos_envio = intentos_envio + CASE WHEN estado = 'PENDIENTE' THEN 1 ELSE 0 END \
          WHERE id IN (SELECT id FROM ifpe.spei_outbound \
                        WHERE proximo_intento_en <= now() \
                          AND (estado IN ('PENDIENTE', 'ENVIADA') \
                               OR (estado = 'LIQUIDADA' AND liquidada_en > now() - make_interval(days => $3))) \
                          {} \
                        ORDER BY proximo_intento_en LIMIT $1 FOR UPDATE SKIP LOCKED) \
         RETURNING *",
        aml::evaluacion::FILTRO_DESPACHO
    );
    let reclamadas: Vec<SpeiOutbound> = sqlx::query_as(&sql)
        .bind(cfg.lote)
        .bind(ARRENDAMIENTO_SEG)
        .bind(cfg.ventana_devolucion_dias)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    // Las más antiguas primero (RETURNING no garantiza orden).
    let mut reclamadas = reclamadas;
    reclamadas.sort_by_key(|o| (o.creado_en, o.id));

    let mut d = Despacho { reclamadas: reclamadas.len(), ..Despacho::default() };
    for o in reclamadas {
        let evento = match procesar(pool, pasarela, cfg, &o).await {
            Ok(Some(evento)) => evento,
            // El reclamo ya no es vigente: otro despachador (o el operador) la tiene.
            Ok(None) => continue,
            Err(e) => {
                d.errores += 1;
                tracing::error!(rastreo = %o.clave_rastreo, codigo = e.codigo(), error = %e.detalle(), "no se pudo renovar el reclamo SPEI");
                continue;
            }
        };
        match aplicar(pool, cfg, o.id, &evento).await {
            Ok(t) if t.cambio && t.orden.estado != o.estado => d.cambios += 1,
            Ok(_) => {}
            Err(e) => {
                d.errores += 1;
                tracing::error!(rastreo = %o.clave_rastreo, codigo = e.codigo(), error = %e.detalle(), "no se pudo aplicar el resultado SPEI");
            }
        }
    }
    Ok(d)
}

fn desde_consulta(estado: EstadoProveedor, o: &SpeiOutbound) -> Option<Evento> {
    Some(match estado {
        EstadoProveedor::NoExiste => return None,
        EstadoProveedor::EnProceso { fecha_operacion } if o.estado == "PENDIENTE" => {
            Evento::Aceptada { id_proveedor: None, fecha_operacion }
        }
        EstadoProveedor::EnProceso { fecha_operacion } => Evento::EnProceso { fecha_operacion },
        EstadoProveedor::Liquidada { fecha_operacion } if o.estado == "LIQUIDADA" => {
            let _ = fecha_operacion;
            Evento::SeguirVigilando
        }
        EstadoProveedor::Liquidada { fecha_operacion } => Evento::Liquidada { fecha_operacion },
        EstadoProveedor::Devuelta { causa, motivo } => Evento::Devuelta { causa, motivo },
        // El proveedor afirma que la orden se canceló: el dinero no salió.
        EstadoProveedor::Cancelada { causa, motivo } => Evento::Rechazada { causa, motivo },
    })
}

/// ¿Pudo cambiar el día de operación del proveedor entre `desde` y `hasta`?
/// Conservador: basta con que la hora de corte diaria (Ciudad de México,
/// UTC-6 como `hoy_cdmx`) caiga en `[desde - margen, hasta + margen]`, aunque
/// sea un día inhábil.
fn pudo_cambiar_dia_operacion(
    desde: DateTime<Utc>,
    hasta: DateTime<Utc>,
    corte: NaiveTime,
    margen: chrono::Duration,
) -> bool {
    let dia = |t: DateTime<Utc>| ((t - chrono::Duration::hours(6)).naive_utc() - (corte - NaiveTime::MIN)).date();
    dia(desde - margen) != dia(hasta + margen)
}

/// Verifica contra la fila viva que el reclamo de `o` sigue vigente (misma
/// orden PENDIENTE, mismos `intentos_envio`, sin revisión manual) y lo
/// renueva justo antes de llamar a `registrar`; fija `registro_intentado_en` y,
/// si aún no lo estaba, `registro_incierto_desde` (pesimista: la llamada puede
/// registrar la orden aunque el despachador muera antes de aplicar el
/// resultado). `None`: el reclamo se perdió, no se llama al proveedor.
/// `Some(Some(t))`: esta llamada fijó `registro_incierto_desde = t` (ninguna
/// llamada anterior pudo registrar la orden).
async fn renovar_reclamo(pool: &PgPool, o: &SpeiOutbound) -> AppResult<Option<Option<DateTime<Utc>>>> {
    let mut tx = db::begin(pool, Rol::Spei).await?;
    let fila: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
        "UPDATE ifpe.spei_outbound \
            SET proximo_intento_en = now() + make_interval(secs => $3), \
                registro_intentado_en = greatest(coalesce(registro_intentado_en, now()), now()), \
                registro_incierto_desde = coalesce(registro_incierto_desde, now()) \
          WHERE id = $1 AND estado = 'PENDIENTE' AND intentos_envio = $2 AND NOT revision_manual \
         RETURNING CASE WHEN registro_incierto_desde = now() THEN registro_incierto_desde END",
    )
    .bind(o.id)
    .bind(o.intentos_envio)
    .bind(ARRENDAMIENTO_SEG)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    if fila.is_none() {
        tracing::warn!(rastreo = %o.clave_rastreo, intentos = o.intentos_envio, "reclamo SPEI vencido: no se llama al proveedor");
    }
    Ok(fila.map(|(desde,)| desde))
}

/// Qué hacer con la orden según el proveedor. `None`: el reclamo ya no es
/// vigente y no se habló con el proveedor para registrarla.
async fn procesar(
    pool: &PgPool,
    pasarela: &dyn Pasarela,
    cfg: &ConfigSpei,
    o: &SpeiOutbound,
) -> AppResult<Option<Evento>> {
    let primer_intento = o.estado == "PENDIENTE" && o.intentos_envio <= 1 && !o.revision_manual;
    if !primer_intento {
        // Ya hubo contacto (o pudo haberlo): primero se pregunta.
        match con_limite(pasarela.consultar(&o.consulta())).await {
            Err(e) => return Ok(Some(Evento::Incierto { error: e.to_string() })),
            Ok(estado) => {
                if let Some(evento) = desde_consulta(estado, o) {
                    return Ok(Some(evento));
                }
            }
        }
        // El proveedor no tiene (o aún no muestra) la orden.
        match o.estado.as_str() {
            "LIQUIDADA" => return Ok(Some(Evento::SeguirVigilando)),
            "ENVIADA" => {
                return Ok(Some(Evento::Incierto { error: "aceptada pero el proveedor no la encuentra".into() }))
            }
            _ if o.revision_manual => {
                return Ok(Some(Evento::Incierto { error: "en revisión manual: no se reenvía".into() }))
            }
            _ => {}
        }
        // El reenvío solo es seguro si el proveedor lo rechazaría como
        // duplicado de un registro que aún no muestra: eso vale dentro del
        // mismo día de operación de la primera llamada que pudo registrarla.
        // Si ninguna pudo (todas probaron no llegar al proveedor), el
        // proveedor no tiene nada que duplicar.
        let margen = chrono::Duration::seconds(i64::try_from(cfg.margen_corte_seg).unwrap_or(86_400).min(86_400));
        if let Some(desde) = o.registro_incierto_desde {
            if pudo_cambiar_dia_operacion(desde, Utc::now(), cfg.hora_corte, margen) {
                return Ok(Some(Evento::RevisionManual {
                    motivo: format!(
                        "el día de operación pudo cambiar desde el primer intento de resultado desconocido (corte \
                         {}): el proveedor no rechazaría el reenvío como duplicado; no se reenvía",
                        cfg.hora_corte.format("%H:%M")
                    ),
                }));
            }
        }
    }
    // Reclamo vigente justo antes de registrar (una foto vieja no registra).
    let Some(marca) = renovar_reclamo(pool, o).await? else {
        return Ok(None);
    };
    Ok(Some(match con_limite(pasarela.registrar(&o.orden_pasarela())).await {
        Ok(Registro::Aceptada { id, fecha_operacion }) => Evento::Aceptada { id_proveedor: Some(id), fecha_operacion },
        // Ninguna llamada anterior pudo registrarla (esta fijó la marca): el
        // rechazo es definitivo si al aplicarlo nadie más la reclamó.
        Ok(Registro::Rechazada { causa, motivo }) if marca.is_some() => {
            Evento::RechazoRegistro { causa, motivo, intentos_envio: o.intentos_envio }
        }
        // Un rechazo en un reenvío es ambiguo ("duplicada").
        Ok(Registro::Rechazada { causa, motivo }) => Evento::RevisionManual {
            motivo: format!("rechazo en un reenvío (causa {causa}: {motivo}); la orden original pudo registrarse"),
        },
        Err(ErrorPasarela::NoRecibida(e)) => Evento::NoRecibida {
            error: ErrorPasarela::NoRecibida(e).to_string(),
            intentos_envio: o.intentos_envio,
            registro_desde: marca,
        },
        Err(e) => Evento::Incierto { error: e.to_string() },
    }))
}

// -----------------------------------------------------------------------------
// Consultas
// -----------------------------------------------------------------------------

/// Orden por clave de rastreo (única) para el rol indicado.
pub async fn buscar_por_rastreo_en(tx: &mut Tx, clave_rastreo: &str) -> AppResult<Option<SpeiOutbound>> {
    Ok(sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE clave_rastreo = $1")
        .bind(clave_rastreo.trim().to_ascii_uppercase())
        .fetch_optional(&mut **tx)
        .await?)
}

pub async fn buscar_por_rastreo(pool: &PgPool, clave_rastreo: &str) -> AppResult<SpeiOutbound> {
    let mut tx = db::begin_lectura(pool, Rol::Spei).await?;
    let orden = buscar_por_rastreo_en(&mut tx, clave_rastreo).await?;
    tx.commit().await?;
    orden.ok_or_else(|| AppError::no_encontrado("orden SPEI no encontrada"))
}

pub async fn buscar_por_id_proveedor(pool: &PgPool, id_proveedor: &str) -> AppResult<SpeiOutbound> {
    let mut tx = db::begin_lectura(pool, Rol::Spei).await?;
    let orden =
        sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE id_proveedor = $1 ORDER BY creado_en DESC LIMIT 1")
            .bind(id_proveedor)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    orden.ok_or_else(|| AppError::no_encontrado("orden SPEI no encontrada"))
}

pub async fn de_cliente(pool: &PgPool, cliente_id: Uuid, limite: i64) -> AppResult<Vec<SpeiOutbound>> {
    let mut tx = db::begin_lectura(pool, Rol::Ifpe).await?;
    let ordenes =
        sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE cliente_id = $1 ORDER BY creado_en DESC LIMIT $2")
            .bind(cliente_id)
            .bind(limite.clamp(1, 200))
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(ordenes)
}

/// Conciliación con el custodio, por cuenta de reserva (`ifpe.v_spei_conciliacion`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Conciliacion {
    pub reserva_id: Uuid,
    pub institucion: String,
    pub tipo: String,
    pub reserva_contable: Decimal,
    pub pendiente_de_envio: Decimal,
    pub enviado_sin_confirmar: Decimal,
    pub ordenes_en_transito: i64,
    pub ordenes_en_revision: i64,
    pub saldo_esperado_custodio: Decimal,
    /// Órdenes con un incidente registrado en `ifpe.spei_incidentes`.
    pub incidentes_conciliacion: i64,
}

pub async fn conciliacion(pool: &PgPool) -> AppResult<Vec<Conciliacion>> {
    let mut tx = db::begin_lectura_instantanea(pool, Rol::Spei).await?;
    let c = sqlx::query_as("SELECT * FROM ifpe.v_spei_conciliacion ORDER BY institucion").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(c)
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use std::str::FromStr;

    fn solicitud() -> SolicitudSpei {
        SolicitudSpei {
            cliente_id: Uuid::nil(),
            clabe_destino: "012180001234567899".into(),
            beneficiario_nombre: Some("José Núñez".into()),
            beneficiario_rfc_curp: Some("nuxj800101xx1".into()),
            monto: Decimal::from_str("1500.50").unwrap(),
            concepto: Some("Renta de octubre".into()),
            referencia_numerica: Some(1_234_567),
            clave_idempotencia: "cli:x:1".into(),
        }
    }

    /// Instante de la Ciudad de México (UTC-6) en UTC.
    fn cdmx(dia: u32, h: u32, m: u32) -> DateTime<Utc> {
        let local = NaiveDate::from_ymd_opt(2026, 10, dia).unwrap().and_hms_opt(h, m, 0).unwrap();
        (local + chrono::Duration::hours(6)).and_utc()
    }

    #[test]
    fn cambio_de_dia_de_operacion_por_corte() {
        let corte = NaiveTime::from_hms_opt(17, 30, 0).unwrap();
        let margen = chrono::Duration::minutes(10);
        let cambia = |a, b| pudo_cambiar_dia_operacion(a, b, corte, margen);
        // Lunes 12 de octubre de 2026.
        assert!(!cambia(cdmx(12, 10, 0), cdmx(12, 11, 0)), "misma mañana");
        assert!(cambia(cdmx(12, 17, 10), cdmx(12, 17, 50)), "cruza el corte");
        assert!(cambia(cdmx(12, 17, 21), cdmx(12, 17, 22)), "antes del corte, dentro del margen");
        assert!(!cambia(cdmx(12, 17, 45), cdmx(13, 9, 0)), "mismo día de operación: el corte, no la medianoche");
        assert!(cambia(cdmx(12, 10, 0), cdmx(13, 9, 0)), "lunes a martes");
    }

    #[test]
    fn validacion_de_la_solicitud() {
        let v = validar(&solicitud()).unwrap();
        assert_eq!(v.clabe.banco(), "012");
        assert_eq!(v.nombre.as_deref(), Some("Jose Nunez"));
        assert_eq!(v.rfc.as_deref(), Some("NUXJ800101XX1"));
        assert_eq!(v.concepto, "Renta de octubre");

        let mut s = solicitud();
        s.clabe_destino = "012180001234567890".into();
        let e = validar(&s).err().unwrap();
        assert!(e.to_string().contains("dígito verificador"), "{e}");
        let mut s = solicitud();
        s.monto = Decimal::from_str("10.001").unwrap();
        assert!(validar(&s).is_err());
        let mut s = solicitud();
        s.referencia_numerica = Some(10_000_000);
        assert!(validar(&s).is_err());
        let mut s = solicitud();
        s.concepto = Some("pago <script>".into());
        assert!(validar(&s).is_err());
        let mut s = solicitud();
        s.beneficiario_rfc_curp = Some("CORTO".into());
        assert!(validar(&s).is_err());
    }
}
