//! Interfaz con el participante liquidador (STP) y una pasarela simulada.
//!
//! El cliente real de STP (registro de órdenes firmadas con RSA sobre la
//! "cadena original", consulta de estado y catálogo de instituciones para la
//! clave de participante de 5 dígitos del banco receptor) se conecta
//! implementando [`Pasarela`].
//!
//! Supuestos que el despachador NO hace sobre el proveedor:
//! - que reenviar una orden sea idempotente: un reenvío con la misma clave de
//!   rastreo puede volver "rechazado por duplicado" aunque la primera sí vaya a
//!   liquidarse;
//! - que una orden registrada aparezca de inmediato en la consulta;
//! - que el rechazo por duplicado valga entre días: como en Banxico, la clave
//!   de rastreo es única por emisor y día de operación, así que un reenvío
//!   registrado en otra fecha de operación puede aceptarse como un pago nuevo.
//!
//! La [`PasarelaSimulada`] reproduce esos casos y lleva el saldo de la cuenta
//! en el custodio para comprobar la conciliación.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Mutex};

use chrono::{Datelike, Days, NaiveDate, Weekday};
use rust_decimal::Decimal;
use serde::Serialize;

pub type Futuro<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Orden tal como se envía al proveedor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrdenPasarela {
    pub clave_rastreo: String,
    pub fecha_solicitud: NaiveDate,
    pub monto: Decimal,
    pub ordenante_nombre: String,
    pub ordenante_clabe: String,
    pub ordenante_rfc_curp: Option<String>,
    /// Código de banco (3 dígitos) de la CLABE beneficiaria; el cliente real
    /// lo traduce a clave de participante con el catálogo del proveedor.
    pub banco_receptor: String,
    pub beneficiario_clabe: String,
    pub beneficiario_nombre: String,
    pub beneficiario_rfc_curp: Option<String>,
    pub concepto: String,
    pub referencia_numerica: i32,
}

/// Respuesta al registrar una orden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registro {
    /// El proveedor aceptó la orden (aún no está liquidada) para la fecha de
    /// operación indicada.
    Aceptada { id: String, fecha_operacion: Option<NaiveDate> },
    /// El proveedor rechazó esta llamada. Solo es definitivo en el primer
    /// intento: en un reenvío puede significar "ya la tengo".
    Rechazada { causa: i16, motivo: String },
}

/// Consulta de una orden. Nuestra clave de rastreo es única para siempre;
/// las fechas candidatas sirven a proveedores que consultan por día.
#[derive(Debug, Clone)]
pub struct ConsultaOrden {
    pub clave_rastreo: String,
    pub id_proveedor: Option<String>,
    pub fechas_candidatas: Vec<NaiveDate>,
}

/// Estado de una orden según el proveedor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EstadoProveedor {
    /// El proveedor no tiene la orden (o aún no la muestra).
    NoExiste,
    EnProceso {
        fecha_operacion: Option<NaiveDate>,
    },
    /// Liquidada en Banxico: el dinero salió.
    Liquidada {
        fecha_operacion: Option<NaiveDate>,
    },
    /// Liquidada y luego devuelta por el banco receptor: el dinero regresó.
    Devuelta {
        causa: i16,
        motivo: String,
    },
    /// Cancelada antes de liquidarse: el dinero no salió.
    Cancelada {
        causa: i16,
        motivo: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorPasarela {
    /// Resultado desconocido: la orden pudo o no registrarse.
    #[error("tiempo de espera agotado con el proveedor SPEI")]
    Tiempo,
    #[error("error transitorio del proveedor SPEI: {0}")]
    Transitorio(String),
}

pub trait Pasarela: Send + Sync {
    fn nombre(&self) -> &'static str;

    fn registrar<'a>(&'a self, orden: &'a OrdenPasarela) -> Futuro<'a, Result<Registro, ErrorPasarela>>;

    fn consultar<'a>(&'a self, consulta: &'a ConsultaOrden) -> Futuro<'a, Result<EstadoProveedor, ErrorPasarela>>;
}

// -----------------------------------------------------------------------------
// Pasarela simulada
// -----------------------------------------------------------------------------

/// Comportamiento programado para las órdenes hacia una CLABE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comportamiento {
    /// El proveedor rechaza la orden al registrarla.
    RechazarAlRegistrar { causa: i16 },
    /// Las primeras `veces` llamadas a `registrar` agotan el tiempo. Si
    /// `registra` es `true`, la orden sí queda registrada en la primera llamada
    /// aunque la respuesta se pierda (resultado ambiguo).
    TiempoAgotadoAlRegistrar { veces: u32, registra: bool },
    /// La consulta responde "no existe" las primeras `veces` aunque la orden
    /// esté registrada (consistencia eventual del proveedor).
    OcultarEnConsulta { veces: u32 },
    /// Aceptada y cancelada antes de liquidarse.
    Cancelar { causa: i16 },
    /// Se liquida y en la consulta siguiente aparece devuelta.
    DevolverTrasLiquidar { causa: i16 },
    /// Las primeras `veces` consultas agotan el tiempo.
    TiempoAgotadoAlConsultar { veces: u32 },
}

#[derive(Debug, Clone)]
enum EstadoSimulado {
    EnProceso { consultas: u32 },
    Liquidada,
    Devuelta { causa: i16 },
    Cancelada { causa: i16 },
}

#[derive(Debug)]
struct OrdenSimulada {
    orden: OrdenPasarela,
    fecha_operacion: NaiveDate,
    estado: EstadoSimulado,
}

#[derive(Debug, Default)]
struct Simulacion {
    /// Por clave de rastreo: un registro por fecha de operación (el
    /// proveedor solo rechaza el duplicado del mismo día).
    ordenes: HashMap<String, Vec<OrdenSimulada>>,
    /// Fecha de operación vigente del proveedor; `None`: la que corresponde a
    /// la fecha de solicitud de la orden.
    fecha_operacion_vigente: Option<NaiveDate>,
    reglas: HashMap<String, Vec<Comportamiento>>,
    llamadas_registro: HashMap<String, u32>,
    llamadas_consulta: HashMap<String, u32>,
    siguiente_id: u64,
    saldo_custodio: Decimal,
    /// Veces que se liquidó cada orden (debe ser 0 o 1).
    liquidaciones: HashMap<String, u32>,
}

/// Pasarela en memoria. Cada orden queda en proceso hasta su
/// `consultas_para_liquidar`-ésima consulta, salvo regla en contrario. Como
/// STP, un reenvío con la misma clave de rastreo se rechaza por duplicado si
/// cae en el mismo día de operación; en otro día se registra otra vez.
#[derive(Debug)]
pub struct PasarelaSimulada {
    estado: Mutex<Simulacion>,
    consultas_para_liquidar: u32,
}

impl Default for PasarelaSimulada {
    fn default() -> Self {
        Self::new(1)
    }
}

/// Fecha de operación que asigna el sistema de pagos: sábado y domingo pasan
/// al lunes (sin calendario de días inhábiles en la simulación).
pub fn fecha_operacion_simulada(fecha: NaiveDate) -> NaiveDate {
    match fecha.weekday() {
        Weekday::Sat => fecha + Days::new(2),
        Weekday::Sun => fecha + Days::new(1),
        _ => fecha,
    }
}

pub const CAUSA_DUPLICADO: i16 = 99;

impl PasarelaSimulada {
    pub fn new(consultas_para_liquidar: u32) -> Self {
        Self { estado: Mutex::new(Simulacion::default()), consultas_para_liquidar }
    }

    fn sim(&self) -> std::sync::MutexGuard<'_, Simulacion> {
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Programa un comportamiento para las órdenes hacia `clabe` (se pueden
    /// combinar varios).
    pub fn programar(&self, clabe: &str, c: Comportamiento) {
        self.sim().reglas.entry(clabe.to_owned()).or_default().push(c);
    }

    /// Saldo inicial de la cuenta en el custodio (conciliación).
    pub fn fijar_saldo_custodio(&self, saldo: Decimal) {
        self.sim().saldo_custodio = saldo;
    }

    /// Un SPEI recibido llegó a la cuenta en el custodio.
    pub fn recibir(&self, monto: Decimal) {
        self.sim().saldo_custodio += monto;
    }

    pub fn saldo_custodio(&self) -> Decimal {
        self.sim().saldo_custodio
    }

    /// Máximo de veces que una misma orden se liquidó (1 si todo va bien).
    pub fn max_liquidaciones_por_orden(&self) -> u32 {
        self.sim().liquidaciones.values().copied().max().unwrap_or(0)
    }

    /// Registros aceptados (un reenvío aceptado en otro día cuenta dos veces).
    pub fn ordenes_registradas(&self) -> usize {
        self.sim().ordenes.values().map(Vec::len).sum()
    }

    /// El proveedor pasa a otro día de operación (p. ej. tras el corte).
    pub fn fijar_fecha_operacion(&self, fecha: NaiveDate) {
        self.sim().fecha_operacion_vigente = Some(fecha);
    }

    /// Estado actual de una orden en el proveedor (para pruebas).
    pub fn liquidada(&self, clave_rastreo: &str) -> bool {
        self.sim().liquidaciones.get(clave_rastreo).copied().unwrap_or(0) > 0
    }

    /// Fuerza la cancelación de una orden en proceso.
    pub fn cancelar(&self, clave_rastreo: &str, causa: i16) {
        for o in self.sim().ordenes.get_mut(clave_rastreo).into_iter().flatten() {
            if matches!(o.estado, EstadoSimulado::EnProceso { .. }) {
                o.estado = EstadoSimulado::Cancelada { causa };
            }
        }
    }

    /// Fuerza la liquidación (y opcionalmente la devolución) de una orden.
    pub fn liquidar(&self, clave_rastreo: &str) {
        let mut g = self.sim();
        let s = &mut *g;
        for o in s.ordenes.get_mut(clave_rastreo).into_iter().flatten() {
            if matches!(o.estado, EstadoSimulado::EnProceso { .. }) {
                o.estado = EstadoSimulado::Liquidada;
                s.saldo_custodio -= o.orden.monto;
                *s.liquidaciones.entry(clave_rastreo.to_owned()).or_default() += 1;
            }
        }
    }

    pub fn devolver(&self, clave_rastreo: &str, causa: i16) {
        let mut g = self.sim();
        let s = &mut *g;
        for o in s.ordenes.get_mut(clave_rastreo).into_iter().flatten() {
            if matches!(o.estado, EstadoSimulado::Liquidada) {
                o.estado = EstadoSimulado::Devuelta { causa };
                s.saldo_custodio += o.orden.monto;
            }
        }
    }

    fn registrar_sync(&self, orden: &OrdenPasarela) -> Result<Registro, ErrorPasarela> {
        let mut s = self.sim();
        let clave = orden.clave_rastreo.clone();
        let llamada = {
            let n = s.llamadas_registro.entry(clave.clone()).or_default();
            *n += 1;
            *n
        };
        let fecha = s.fecha_operacion_vigente.unwrap_or_else(|| fecha_operacion_simulada(orden.fecha_solicitud));
        if s.ordenes.get(&clave).is_some_and(|v| v.iter().any(|o| o.fecha_operacion == fecha)) {
            return Ok(Registro::Rechazada { causa: CAUSA_DUPLICADO, motivo: "clave de rastreo duplicada".into() });
        }
        let reglas = s.reglas.get(&orden.beneficiario_clabe).cloned().unwrap_or_default();
        for regla in reglas {
            match regla {
                Comportamiento::RechazarAlRegistrar { causa } => {
                    return Ok(Registro::Rechazada { causa, motivo: "rechazada por el proveedor".into() })
                }
                Comportamiento::TiempoAgotadoAlRegistrar { veces, registra } if llamada <= veces => {
                    if registra && llamada == 1 {
                        Self::alta(&mut s, orden, fecha);
                    }
                    return Err(ErrorPasarela::Tiempo);
                }
                _ => {}
            }
        }
        let id = Self::alta(&mut s, orden, fecha);
        Ok(Registro::Aceptada { id, fecha_operacion: Some(fecha) })
    }

    fn alta(s: &mut Simulacion, orden: &OrdenPasarela, fecha: NaiveDate) -> String {
        s.siguiente_id += 1;
        let id = format!("STP{:010}", s.siguiente_id);
        s.ordenes.entry(orden.clave_rastreo.clone()).or_default().push(OrdenSimulada {
            orden: orden.clone(),
            fecha_operacion: fecha,
            estado: EstadoSimulado::EnProceso { consultas: 0 },
        });
        id
    }

    fn consultar_sync(&self, consulta: &ConsultaOrden) -> Result<EstadoProveedor, ErrorPasarela> {
        let mut guardia = self.sim();
        let s = &mut *guardia;
        let clave = consulta.clave_rastreo.clone();
        let llamada = {
            let n = s.llamadas_consulta.entry(clave.clone()).or_default();
            *n += 1;
            *n
        };
        // La consulta muestra el primer registro de la clave de rastreo.
        let Some(o) = s.ordenes.get_mut(&clave).and_then(|v| v.first_mut()) else {
            return Ok(EstadoProveedor::NoExiste);
        };
        let reglas = s.reglas.get(&o.orden.beneficiario_clabe).cloned().unwrap_or_default();
        for regla in &reglas {
            match regla {
                Comportamiento::TiempoAgotadoAlConsultar { veces } if llamada <= *veces => {
                    return Err(ErrorPasarela::Tiempo)
                }
                Comportamiento::OcultarEnConsulta { veces } if llamada <= *veces => {
                    return Ok(EstadoProveedor::NoExiste)
                }
                _ => {}
            }
        }
        let cancelar = reglas.iter().find_map(|r| match r {
            Comportamiento::Cancelar { causa } => Some(*causa),
            _ => None,
        });
        let devolver = reglas.iter().find_map(|r| match r {
            Comportamiento::DevolverTrasLiquidar { causa } => Some(*causa),
            _ => None,
        });
        let siguiente = match (&o.estado, cancelar, devolver) {
            (EstadoSimulado::EnProceso { .. }, Some(causa), _) => EstadoSimulado::Cancelada { causa },
            (EstadoSimulado::EnProceso { consultas }, None, _) if consultas + 1 >= self.consultas_para_liquidar => {
                s.saldo_custodio -= o.orden.monto;
                *s.liquidaciones.entry(clave.clone()).or_default() += 1;
                EstadoSimulado::Liquidada
            }
            (EstadoSimulado::EnProceso { consultas }, None, _) => {
                EstadoSimulado::EnProceso { consultas: consultas + 1 }
            }
            (EstadoSimulado::Liquidada, _, Some(causa)) => {
                s.saldo_custodio += o.orden.monto;
                EstadoSimulado::Devuelta { causa }
            }
            (otro, _, _) => otro.clone(),
        };
        o.estado = siguiente;
        let fecha = Some(o.fecha_operacion);
        Ok(match &o.estado {
            EstadoSimulado::EnProceso { .. } => EstadoProveedor::EnProceso { fecha_operacion: fecha },
            EstadoSimulado::Liquidada => EstadoProveedor::Liquidada { fecha_operacion: fecha },
            EstadoSimulado::Devuelta { causa } => {
                EstadoProveedor::Devuelta { causa: *causa, motivo: "devuelta por el banco receptor".into() }
            }
            EstadoSimulado::Cancelada { causa } => {
                EstadoProveedor::Cancelada { causa: *causa, motivo: "cancelada por el proveedor".into() }
            }
        })
    }
}

impl Pasarela for PasarelaSimulada {
    fn nombre(&self) -> &'static str {
        "simulada"
    }

    fn registrar<'a>(&'a self, orden: &'a OrdenPasarela) -> Futuro<'a, Result<Registro, ErrorPasarela>> {
        Box::pin(async move { self.registrar_sync(orden) })
    }

    fn consultar<'a>(&'a self, consulta: &'a ConsultaOrden) -> Futuro<'a, Result<EstadoProveedor, ErrorPasarela>> {
        Box::pin(async move { self.consultar_sync(consulta) })
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn orden(clabe: &str, rastreo: &str) -> OrdenPasarela {
        OrdenPasarela {
            clave_rastreo: rastreo.into(),
            fecha_solicitud: NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
            monto: Decimal::new(10_000, 2),
            ordenante_nombre: "ANA".into(),
            ordenante_clabe: "646180157000000017".into(),
            ordenante_rfc_curp: None,
            banco_receptor: clabe[..3].into(),
            beneficiario_clabe: clabe.into(),
            beneficiario_nombre: "BETO".into(),
            beneficiario_rfc_curp: None,
            concepto: "PAGO".into(),
            referencia_numerica: 1,
        }
    }

    fn consulta(rastreo: &str) -> ConsultaOrden {
        ConsultaOrden { clave_rastreo: rastreo.into(), id_proveedor: None, fechas_candidatas: vec![] }
    }

    #[tokio::test]
    async fn ciclo_normal_y_reenvio_rechazado_por_duplicado() {
        let p = PasarelaSimulada::new(2);
        p.fijar_saldo_custodio(Decimal::new(1_000, 0));
        let o = orden("012180001234567899", "SC1");
        assert!(matches!(p.registrar(&o).await.unwrap(), Registro::Aceptada { .. }));
        // Como STP: un reenvío con la misma clave de rastreo es "duplicado".
        assert_eq!(
            p.registrar(&o).await.unwrap(),
            Registro::Rechazada { causa: CAUSA_DUPLICADO, motivo: "clave de rastreo duplicada".into() }
        );
        assert!(matches!(p.consultar(&consulta("SC1")).await.unwrap(), EstadoProveedor::EnProceso { .. }));
        assert!(matches!(p.consultar(&consulta("SC1")).await.unwrap(), EstadoProveedor::Liquidada { .. }));
        assert!(matches!(p.consultar(&consulta("SC1")).await.unwrap(), EstadoProveedor::Liquidada { .. }));
        assert_eq!(p.saldo_custodio(), Decimal::new(900, 0));
        assert_eq!(p.max_liquidaciones_por_orden(), 1);
        assert_eq!(p.consultar(&consulta("NADA")).await.unwrap(), EstadoProveedor::NoExiste);
    }

    #[tokio::test]
    async fn tiempo_agotado_ambiguo_y_consulta_atrasada() {
        let p = PasarelaSimulada::new(1);
        let o = orden("012180001234567899", "SC2");
        p.programar(&o.beneficiario_clabe, Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: true });
        assert_eq!(p.registrar(&o).await, Err(ErrorPasarela::Tiempo));
        // La orden sí quedó registrada.
        assert!(matches!(p.consultar(&consulta("SC2")).await.unwrap(), EstadoProveedor::Liquidada { .. }));

        let o = orden("002010077777777771", "SC3");
        p.programar(&o.beneficiario_clabe, Comportamiento::OcultarEnConsulta { veces: 2 });
        p.registrar(&o).await.unwrap();
        assert_eq!(p.consultar(&consulta("SC3")).await.unwrap(), EstadoProveedor::NoExiste);
    }

    /// El riesgo que el despachador debe evitar: tras el cambio de día de
    /// operación, el proveedor acepta otra vez la misma clave de rastreo.
    #[tokio::test]
    async fn reenvio_en_otro_dia_de_operacion_se_acepta() {
        let p = PasarelaSimulada::new(5);
        let o = orden("012180001234567899", "SC4");
        assert!(matches!(p.registrar(&o).await.unwrap(), Registro::Aceptada { .. }));
        assert_eq!(
            p.registrar(&o).await.unwrap(),
            Registro::Rechazada { causa: CAUSA_DUPLICADO, motivo: "clave de rastreo duplicada".into() }
        );
        p.fijar_fecha_operacion(fecha_operacion_simulada(o.fecha_solicitud) + Days::new(1));
        assert!(matches!(p.registrar(&o).await.unwrap(), Registro::Aceptada { .. }));
        assert_eq!(p.ordenes_registradas(), 2);
        p.liquidar("SC4");
        assert_eq!(p.max_liquidaciones_por_orden(), 2, "pago doble");
    }

    #[test]
    fn fin_de_semana_pasa_al_lunes() {
        let sabado = NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        assert_eq!(fecha_operacion_simulada(sabado), NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    }
}
