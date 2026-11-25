//! El rastreador SPEI del cliente (roadmap 17): cada SPEI suyo, entrante o
//! saliente, con su liquidación y lo que Banxico pide para consultar su CEP
//! (Comprobante Electrónico de Pago).
//!
//! - **Entrantes** (`ifpe.spei_inbound` de su cuenta): abonados (liquidados)
//!   o devueltos al emisor (nunca abonados). La devolución de una orden
//!   nuestra no se lista aparte: es el desenlace de esa orden. De un depósito
//!   devuelto no sale la causa ni el motivo (pueden nombrar la Lista de
//!   Personas Bloqueadas: aviso indebido); la pantalla dice, sin más, que la
//!   cuenta no pudo recibirlo.
//! - **Salientes** (`ifpe.spei_outbound` del cliente): retiros a su cuenta
//!   vinculada y pagos a terceros. PENDIENTE (iniciada), ENVIADA (en la red
//!   SPEI), LIQUIDADA, DEVUELTA (el banco receptor la regresó) o RECHAZADA
//!   (no salió); en las dos últimas el dinero volvió a la bóveda. La causa
//!   Banxico sale como un código corto; el motivo y los errores del proveedor
//!   no salen.
//! - **Contraparte**: nombre, banco (el catálogo corto de
//!   [`super::vinculadas::nombre_banco`]) y los últimos cuatro dígitos de su
//!   cuenta: la CLABE completa de otro no sale de la API.
//! - **CEP**: de un SPEI liquidado (también de una orden devuelta después de
//!   liquidarse): la fecha de operación, la clave de rastreo, el banco emisor
//!   y el receptor, la cuenta beneficiaria (la CLABE del cliente, completa,
//!   si el beneficiario es él; de otro, sus últimos cuatro dígitos) y el
//!   monto. Un SPEI simulado (`MODO_DEMO`: los depósitos de demostración y la
//!   pasarela simulada) no tiene CEP en Banxico, y se dice (`available`).
//! - La última aclaración de cada movimiento (migración 0025), si la hay.
//! - Con el rol del cliente y RLS: solo lo suyo. Por páginas, del más reciente
//!   al más antiguo, con un cursor opaco como el del historial de órdenes; o
//!   uno, por su id o por el de un movimiento del ledger que hizo.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{aclaraciones::EstadoAclaracion, vinculadas::nombre_banco};
use crate::{
    db, dinero,
    error::{AppError, AppResult},
};

/// SPEI por página, si no se pide otra cosa.
pub const PAGINA: i64 = 50;
/// SPEI por página, a lo más.
pub const MAXIMO_PAGINA: i64 = 100;
/// Donde Banxico consulta un CEP.
pub const URL_CEP: &str = "https://www.banxico.org.mx/cep/";
/// La institución de las CLABEs de los clientes, si su clave no está en el catálogo.
pub const NOMBRE_PROPIO: &str = "Synthetic Capital";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direccion {
    #[serde(rename = "incoming")]
    Entrante,
    #[serde(rename = "outgoing")]
    Saliente,
}

/// Qué fue: un depósito, un retiro a la cuenta vinculada o un pago a otro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Clase {
    #[serde(rename = "deposit")]
    Deposito,
    #[serde(rename = "withdrawal")]
    Retiro,
    #[serde(rename = "payment")]
    Pago,
}

/// Para el cliente: en camino, liquidado o de regreso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EstadoTransferencia {
    #[serde(rename = "pending")]
    Pendiente,
    #[serde(rename = "settled")]
    Liquidada,
    #[serde(rename = "returned")]
    Devuelta,
}

/// Hasta dónde llegó: la línea de tiempo del detalle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Etapa {
    /// Una orden registrada que espera al despachador.
    #[serde(rename = "initiated")]
    Iniciada,
    /// El liquidador (STP) la tiene y viaja por Banxico.
    #[serde(rename = "sent")]
    Enviada,
    #[serde(rename = "settled")]
    Liquidada,
    /// Liquidada (o recibida) y regresada: la devolvió el banco receptor, o
    /// un depósito que la cuenta no pudo recibir volvió al emisor.
    #[serde(rename = "returned")]
    Devuelta,
    /// No salió: el liquidador la rechazó antes de liquidarla.
    #[serde(rename = "rejected")]
    Rechazada,
}

/// Por qué regresó un SPEI saliente, del catálogo de causas de Banxico.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CausaRegreso {
    /// 1: la cuenta no existe.
    #[serde(rename = "account_not_found")]
    CuentaInexistente,
    /// 2: el banco receptor tiene la cuenta bloqueada.
    #[serde(rename = "account_blocked")]
    CuentaBloqueada,
    /// 3: la cuenta está cancelada.
    #[serde(rename = "account_closed")]
    CuentaCancelada,
    /// 14: faltó información obligatoria.
    #[serde(rename = "missing_information")]
    FaltaInformacion,
    #[serde(rename = "other")]
    Otra,
}

impl CausaRegreso {
    pub fn de_codigo(codigo: Option<i16>) -> Self {
        match codigo {
            Some(1) => Self::CuentaInexistente,
            Some(2) => Self::CuentaBloqueada,
            Some(3) => Self::CuentaCancelada,
            Some(14) => Self::FaltaInformacion,
            _ => Self::Otra,
        }
    }
}

/// Un banco: la clave de 3 dígitos de su CLABE y su nombre, si está en el
/// catálogo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Banco {
    #[serde(rename = "code")]
    pub clave: Option<String>,
    #[serde(rename = "name")]
    pub nombre: Option<&'static str>,
}

impl Banco {
    fn de_clave(clave: Option<&str>) -> Self {
        let clave = clave.filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_digit()));
        Self { clave: clave.map(str::to_owned), nombre: clave.and_then(nombre_banco) }
    }

    /// De una CLABE: sus primeros 3 dígitos.
    fn de_clabe(clabe: &str) -> Self {
        Self::de_clave(clabe.get(..3))
    }

    /// El del cliente: su CLABE es de Synthetic Capital (por su liquidador,
    /// STP, en producción; con otra clave en `MODO_DEMO` y en pruebas).
    fn propio(clabe: &str) -> Self {
        let banco = Self::de_clabe(clabe);
        Self { nombre: banco.nombre.or(Some(NOMBRE_PROPIO)), ..banco }
    }

    /// De la clave de participante SPEI de 5 dígitos: sus últimos 3 son los
    /// del banco en la CLABE (40012 = 012, BBVA México; 90646 = 646, STP).
    fn de_participante(participante: Option<&str>) -> Self {
        Self::de_clave(participante.filter(|p| p.len() == 5).and_then(|p| p.get(2..)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Contraparte {
    #[serde(rename = "name")]
    pub nombre: Option<String>,
    #[serde(rename = "bank")]
    pub banco: Banco,
    #[serde(rename = "account_last4")]
    pub cuenta_final: Option<String>,
}

/// Lo que pide Banxico para consultar el CEP de un SPEI liquidado.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DatosCep {
    /// Hay CEP en Banxico: liquidado, no simulado y con fecha de operación.
    #[serde(rename = "available")]
    pub disponible: bool,
    #[serde(rename = "operation_date")]
    pub fecha_operacion: Option<NaiveDate>,
    #[serde(rename = "tracking_key")]
    pub clave_rastreo: String,
    #[serde(rename = "sender_bank")]
    pub banco_emisor: Banco,
    #[serde(rename = "receiver_bank")]
    pub banco_receptor: Banco,
    /// Completa solo si el beneficiario es el cliente (su propia CLABE).
    #[serde(rename = "beneficiary_account")]
    pub cuenta_beneficiaria: Option<String>,
    #[serde(rename = "beneficiary_account_last4")]
    pub cuenta_beneficiaria_final: Option<String>,
    #[serde(rename = "amount")]
    pub monto: Decimal,
    pub url: &'static str,
}

/// La última aclaración del movimiento.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumenAclaracion {
    #[serde(rename = "clarification_id")]
    pub id: Uuid,
    #[serde(rename = "number")]
    pub numero: i32,
    #[serde(rename = "status")]
    pub estado: EstadoAclaracion,
}

/// Un SPEI del cliente (`SpeiTransfer`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transferencia {
    /// La orden de envío o el registro del SPEI recibido.
    #[serde(rename = "transfer_id")]
    pub id: Uuid,
    #[serde(rename = "direction")]
    pub direccion: Direccion,
    #[serde(rename = "kind")]
    pub clase: Clase,
    pub status: EstadoTransferencia,
    #[serde(rename = "stage")]
    pub etapa: Etapa,
    #[serde(rename = "amount")]
    pub monto: Decimal,
    /// La clave de rastreo SPEI.
    #[serde(rename = "tracking_key")]
    pub clave_rastreo: String,
    /// El día de operación que asignó Banxico (null mientras no se sabe).
    #[serde(rename = "operation_date")]
    pub fecha_operacion: Option<NaiveDate>,
    /// El concepto del pago, tal como viajó por SPEI.
    #[serde(rename = "concept")]
    pub concepto: Option<String>,
    #[serde(rename = "numeric_reference")]
    pub referencia_numerica: Option<i32>,
    #[serde(rename = "counterparty")]
    pub contraparte: Contraparte,
    #[serde(rename = "initiated_at")]
    pub iniciada_en: DateTime<Utc>,
    #[serde(rename = "sent_at")]
    pub enviada_en: Option<DateTime<Utc>>,
    #[serde(rename = "settled_at")]
    pub liquidada_en: Option<DateTime<Utc>>,
    #[serde(rename = "returned_at")]
    pub devuelta_en: Option<DateTime<Utc>>,
    /// Solo de un SPEI saliente que regresó.
    #[serde(rename = "return_reason")]
    pub causa_regreso: Option<CausaRegreso>,
    /// El movimiento del ledger: el cargo de la orden o el abono del depósito.
    #[serde(rename = "transaction_id")]
    pub tx_id: Option<Uuid>,
    /// El reembolso de una orden que regresó.
    #[serde(rename = "refund_transaction_id")]
    pub tx_reverso_id: Option<Uuid>,
    /// Un SPEI simulado (`MODO_DEMO`): no pasó por Banxico.
    #[serde(rename = "simulated")]
    pub simulado: bool,
    pub cep: Option<DatosCep>,
    #[serde(rename = "clarification")]
    pub aclaracion: Option<ResumenAclaracion>,
}

/// `GET /api/v1/wealth/spei/transfers` (`SpeiTransfers`).
#[derive(Debug, Clone, Serialize)]
pub struct Transferencias {
    #[serde(rename = "transfers")]
    pub transferencias: Vec<Transferencia>,
    /// Para pedir la página siguiente (`?cursor=`); null en la última.
    pub next_cursor: Option<String>,
}

/// Una fila de la unión de órdenes y SPEI recibidos.
#[derive(Debug, FromRow)]
struct Fila {
    id: Uuid,
    direccion: String,
    clase: String,
    estado: String,
    monto: Decimal,
    clave_rastreo: String,
    fecha_operacion: Option<NaiveDate>,
    concepto: Option<String>,
    referencia_numerica: Option<i32>,
    contraparte_nombre: Option<String>,
    contraparte_banco: Option<String>,
    contraparte_participante: Option<String>,
    contraparte_cuenta: Option<String>,
    cuenta_propia: String,
    causa: Option<i16>,
    origen: Option<String>,
    iniciada_en: DateTime<Utc>,
    enviada_en: Option<DateTime<Utc>>,
    liquidada_en: Option<DateTime<Utc>>,
    devuelta_en: Option<DateTime<Utc>>,
    tx_id: Option<Uuid>,
    tx_reverso_id: Option<Uuid>,
    aclaracion_id: Option<Uuid>,
    aclaracion_numero: Option<i32>,
    aclaracion_estado: Option<String>,
}

/// Las órdenes del cliente y los SPEI que llegaron (abonados o devueltos) a
/// su cuenta, con la última aclaración de cada uno. `$1`: el cliente.
const SPEI_DEL_CLIENTE: &str = "\
    SELECT t.*, a.id AS aclaracion_id, a.numero AS aclaracion_numero, a.estado AS aclaracion_estado FROM ( \
        SELECT o.id, 'SALIENTE' AS direccion, \
               CASE WHEN o.cuenta_vinculada_id IS NOT NULL THEN 'RETIRO' ELSE 'PAGO' END AS clase, \
               o.estado, o.monto, o.clave_rastreo::text AS clave_rastreo, o.fecha_operacion, o.concepto, \
               o.referencia_numerica, o.beneficiario_nombre AS contraparte_nombre, \
               o.banco_receptor::text AS contraparte_banco, NULL::text AS contraparte_participante, \
               o.beneficiario_clabe::text AS contraparte_cuenta, o.ordenante_clabe::text AS cuenta_propia, \
               o.causa_devolucion AS causa, NULL::text AS origen, o.creado_en AS iniciada_en, o.enviada_en, \
               o.liquidada_en, \
               CASE WHEN o.estado IN ('DEVUELTA', 'RECHAZADA') THEN coalesce(o.finalizada_en, o.actualizado_en) END \
                 AS devuelta_en, \
               o.tx_retiro_id AS tx_id, o.tx_reverso_id \
          FROM ifpe.spei_outbound o \
         WHERE o.cliente_id = $1 \
        UNION ALL \
        SELECT i.id, 'ENTRANTE', 'DEPOSITO', i.estado, i.monto, i.clave_rastreo::text, i.fecha_operacion, i.concepto, \
               i.referencia_numerica, i.nombre_ordenante, NULL, i.institucion_emisora::text, i.cuenta_ordenante, \
               i.receptor_clabe::text, i.causa_devolucion, i.origen, i.recibido_en, NULL, \
               CASE WHEN i.estado = 'ABONADO' THEN i.recibido_en END, \
               CASE WHEN i.estado = 'DEVUELTO' THEN i.recibido_en END, i.transaccion_id, NULL \
          FROM ifpe.spei_inbound i JOIN ifpe.cuentas c ON c.id = i.cuenta_id \
         WHERE c.cliente_id = $1 AND i.estado IN ('ABONADO', 'DEVUELTO') \
    ) t \
    LEFT JOIN LATERAL ( \
        SELECT x.id, x.numero, x.estado FROM ifpe.aclaraciones_spei x \
         WHERE (t.direccion = 'SALIENTE' AND x.spei_outbound_id = t.id) \
            OR (t.direccion = 'ENTRANTE' AND x.spei_inbound_id = t.id) \
         ORDER BY x.numero DESC LIMIT 1 \
    ) a ON TRUE";

/// Los últimos cuatro dígitos de una cuenta; nada si no son dígitos.
fn ultimos_cuatro(cuenta: Option<&str>) -> Option<String> {
    let cuenta = cuenta?.trim();
    (cuenta.len() >= 4 && cuenta.bytes().all(|b| b.is_ascii_digit())).then(|| cuenta[cuenta.len() - 4..].to_owned())
}

impl Fila {
    fn transferencia(self, pasarela_simulada: bool) -> AppResult<Transferencia> {
        let saliente = match self.direccion.as_str() {
            "SALIENTE" => true,
            "ENTRANTE" => false,
            otra => return Err(AppError::Integridad(format!("rastreador SPEI: dirección desconocida {otra}"))),
        };
        let clase = match self.clase.as_str() {
            "RETIRO" => Clase::Retiro,
            "PAGO" => Clase::Pago,
            "DEPOSITO" => Clase::Deposito,
            otra => return Err(AppError::Integridad(format!("rastreador SPEI: clase desconocida {otra}"))),
        };
        let (status, etapa) = match (saliente, self.estado.as_str()) {
            (true, "PENDIENTE") => (EstadoTransferencia::Pendiente, Etapa::Iniciada),
            (true, "ENVIADA") => (EstadoTransferencia::Pendiente, Etapa::Enviada),
            (true, "LIQUIDADA") | (false, "ABONADO") => (EstadoTransferencia::Liquidada, Etapa::Liquidada),
            (true, "DEVUELTA") | (false, "DEVUELTO") => (EstadoTransferencia::Devuelta, Etapa::Devuelta),
            (true, "RECHAZADA") => (EstadoTransferencia::Devuelta, Etapa::Rechazada),
            (_, otro) => return Err(AppError::Integridad(format!("rastreador SPEI: estado desconocido {otro}"))),
        };
        let simulado = if saliente { pasarela_simulada } else { self.origen.as_deref() == Some("DEMO") };
        // Un depósito de demostración no trae fecha de operación real ni ordenante.
        let demo_entrante = !saliente && simulado;
        let fecha_operacion = if demo_entrante { None } else { self.fecha_operacion };
        let contraparte = if saliente {
            Contraparte {
                nombre: self.contraparte_nombre.clone(),
                banco: Banco::de_clave(self.contraparte_banco.as_deref()),
                cuenta_final: ultimos_cuatro(self.contraparte_cuenta.as_deref()),
            }
        } else if demo_entrante {
            Contraparte { nombre: None, banco: Banco::de_clave(None), cuenta_final: None }
        } else {
            Contraparte {
                nombre: self.contraparte_nombre.clone(),
                banco: Banco::de_participante(self.contraparte_participante.as_deref()),
                cuenta_final: ultimos_cuatro(self.contraparte_cuenta.as_deref()),
            }
        };
        // Liquidado alguna vez: también una orden devuelta después de liquidarse.
        let liquidado = self.liquidada_en.is_some();
        let cep = liquidado.then(|| {
            let propio = Banco::propio(&self.cuenta_propia);
            let (banco_emisor, banco_receptor) =
                if saliente { (propio, contraparte.banco.clone()) } else { (contraparte.banco.clone(), propio) };
            let (cuenta_beneficiaria, cuenta_beneficiaria_final) = if saliente {
                (None, ultimos_cuatro(self.contraparte_cuenta.as_deref()))
            } else {
                (Some(self.cuenta_propia.clone()), ultimos_cuatro(Some(&self.cuenta_propia)))
            };
            DatosCep {
                disponible: !simulado && fecha_operacion.is_some(),
                fecha_operacion,
                clave_rastreo: self.clave_rastreo.clone(),
                banco_emisor,
                banco_receptor,
                cuenta_beneficiaria,
                cuenta_beneficiaria_final,
                monto: dinero::con_escala(self.monto),
                url: URL_CEP,
            }
        });
        let aclaracion = match (self.aclaracion_id, self.aclaracion_numero, self.aclaracion_estado.as_deref()) {
            (Some(id), Some(numero), Some(estado)) => {
                Some(ResumenAclaracion { id, numero, estado: EstadoAclaracion::de_bd(estado)? })
            }
            _ => None,
        };
        Ok(Transferencia {
            id: self.id,
            direccion: if saliente { Direccion::Saliente } else { Direccion::Entrante },
            clase,
            status,
            etapa,
            monto: dinero::con_escala(self.monto),
            clave_rastreo: self.clave_rastreo,
            fecha_operacion,
            concepto: if demo_entrante { None } else { self.concepto },
            referencia_numerica: self.referencia_numerica,
            contraparte,
            iniciada_en: self.iniciada_en,
            enviada_en: self.enviada_en,
            liquidada_en: self.liquidada_en,
            devuelta_en: self.devuelta_en,
            causa_regreso: (saliente && status == EstadoTransferencia::Devuelta)
                .then(|| CausaRegreso::de_codigo(self.causa)),
            tx_id: self.tx_id,
            tx_reverso_id: self.tx_reverso_id,
            simulado,
            cep,
            aclaracion,
        })
    }
}

/// El cursor de la página que sigue al SPEI iniciado en `en` con `id`.
fn cursor_de(en: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", en.timestamp_micros(), id.simple())
}

fn leer_cursor(cursor: &str) -> AppResult<(DateTime<Utc>, Uuid)> {
    let invalido = || AppError::validacion("el cursor no es válido; pide la primera página sin cursor");
    let (micros, id) = cursor.split_once('_').ok_or_else(invalido)?;
    let micros: i64 = micros.parse().map_err(|_| invalido())?;
    let en = Utc.timestamp_micros(micros).single().ok_or_else(invalido)?;
    let id = Uuid::try_parse(id).map_err(|_| invalido())?;
    Ok((en, id))
}

/// Una página de los SPEI del cliente, del más reciente al más antiguo.
pub async fn listar(
    pool: &PgPool,
    cliente_id: Uuid,
    pasarela_simulada: bool,
    limite: Option<i64>,
    cursor: Option<&str>,
) -> AppResult<Transferencias> {
    let limite = limite.unwrap_or(PAGINA);
    if !(1..=MAXIMO_PAGINA).contains(&limite) {
        return Err(AppError::validacion(format!("limit va de 1 a {MAXIMO_PAGINA}")));
    }
    let desde = cursor.map(leer_cursor).transpose()?;
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let mut filas: Vec<Fila> = sqlx::query_as(&format!(
        "SELECT * FROM ({SPEI_DEL_CLIENTE}) s \
          WHERE $2::timestamptz IS NULL OR (s.iniciada_en, s.id) < ($2, $3) \
          ORDER BY s.iniciada_en DESC, s.id DESC LIMIT $4"
    ))
    .bind(cliente_id)
    .bind(desde.map(|(en, _)| en))
    .bind(desde.map(|(_, id)| id))
    .bind(limite + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    // Una de más dice si hay otra página.
    let hay_mas = filas.len() as i64 > limite;
    filas.truncate(limite as usize);
    let next_cursor = if hay_mas { filas.last().map(|f| cursor_de(f.iniciada_en, f.id)) } else { None };
    let transferencias =
        filas.into_iter().map(|f| f.transferencia(pasarela_simulada)).collect::<AppResult<Vec<_>>>()?;
    Ok(Transferencias { transferencias, next_cursor })
}

/// Un SPEI del cliente, por su id o por el de un movimiento del ledger que
/// hizo (el cargo de una orden, su reembolso, el abono de un depósito). 404 si
/// no es suyo.
pub async fn obtener(pool: &PgPool, cliente_id: Uuid, pasarela_simulada: bool, id: Uuid) -> AppResult<Transferencia> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let fila: Option<Fila> = sqlx::query_as(&format!(
        "SELECT * FROM ({SPEI_DEL_CLIENTE}) s WHERE s.id = $2 OR s.tx_id = $2 OR s.tx_reverso_id = $2 \
          ORDER BY (s.id = $2) DESC LIMIT 1"
    ))
    .bind(cliente_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    fila.ok_or_else(|| AppError::no_encontrado("el SPEI no existe"))?.transferencia(pasarela_simulada)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn los_bancos_salen_de_la_clabe_o_del_participante() {
        assert_eq!(
            Banco::de_clabe("012180001234567899"),
            Banco { clave: Some("012".into()), nombre: Some("BBVA México") }
        );
        assert_eq!(Banco::de_participante(Some("90646")).nombre, Some("STP"));
        assert_eq!(Banco::de_participante(Some("40014")).nombre, Some("Santander"));
        // Fuera del catálogo: la clave sin nombre; algo que no es una clave, nada.
        assert_eq!(Banco::de_participante(Some("40137")), Banco { clave: Some("137".into()), nombre: None });
        assert_eq!(Banco::de_participante(Some("012")), Banco { clave: None, nombre: None });
        assert_eq!(Banco::de_clave(Some("A12")), Banco { clave: None, nombre: None });
        // El del cliente: STP en producción; Synthetic Capital con otra clave.
        assert_eq!(Banco::propio("646180157000000017").nombre, Some("STP"));
        assert_eq!(
            Banco::propio("999180883632317555"),
            Banco { clave: Some("999".into()), nombre: Some("Synthetic Capital") }
        );
    }

    #[test]
    fn de_una_cuenta_ajena_solo_los_ultimos_cuatro() {
        assert_eq!(ultimos_cuatro(Some("012180001234567899")).as_deref(), Some("7899"));
        assert_eq!(ultimos_cuatro(Some("5512345678")).as_deref(), Some("5678"));
        assert_eq!(ultimos_cuatro(Some("123")), None);
        assert_eq!(ultimos_cuatro(Some("01218000123456789X")), None);
        assert_eq!(ultimos_cuatro(None), None);
    }

    #[test]
    fn las_causas_de_banxico_en_codigos_cortos() {
        assert_eq!(CausaRegreso::de_codigo(Some(1)), CausaRegreso::CuentaInexistente);
        assert_eq!(CausaRegreso::de_codigo(Some(2)), CausaRegreso::CuentaBloqueada);
        assert_eq!(CausaRegreso::de_codigo(Some(3)), CausaRegreso::CuentaCancelada);
        assert_eq!(CausaRegreso::de_codigo(Some(14)), CausaRegreso::FaltaInformacion);
        assert_eq!(CausaRegreso::de_codigo(Some(99)), CausaRegreso::Otra);
        assert_eq!(CausaRegreso::de_codigo(None), CausaRegreso::Otra);
    }

    #[test]
    fn el_cursor_va_y_vuelve() {
        let en = Utc.timestamp_micros(1_795_546_401_502_123).single().unwrap();
        let id = Uuid::parse_str("9a000000-0000-0000-0000-000000000001").unwrap();
        assert_eq!(leer_cursor(&cursor_de(en, id)).unwrap(), (en, id));
        for malo in ["", "123", "abc_9a000000000000000000000000000001", "123_no-es-un-uuid"] {
            assert!(leer_cursor(malo).is_err(), "{malo}");
        }
    }
}
