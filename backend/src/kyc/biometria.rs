//! Proveedor biométrico: prueba de vida, comparación facial (selfie contra la
//! fotografía de la INE) y lectura (OCR) de la credencial.
//!
//! El proveedor entrega su resultado como un documento JSON firmado con
//! HMAC-SHA256, con el mismo esquema que los webhooks SPEI
//! (`X-Kyc-Firma: t=<unix>,v1=<hex>` sobre `"<t>.<cuerpo>"`). El resultado se
//! procesa SIEMPRE por el mismo camino ([`super::verificacion::procesar`]),
//! llegue por `POST /api/v1/kyc/webhook` (proveedor asíncrono) o de inmediato
//! (proveedor síncrono, como el simulado): la firma se verifica en ambos casos.
//!
//! [`ProveedorSimulado`] sustituye a un proveedor certificado (liveness pasivo
//! / activo, comparación 1:1, validación contra la lista nominal del INE y
//! RENAPO). Comprueba lo que puede comprobar sin visión por computadora
//! (cuadros distintos de la cámara, imágenes distintas entre sí) y devuelve un
//! score configurable por CURP para las pruebas.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Mutex};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    expediente::{Evidencia, IdentidadPropia, Imagenes},
    ine::{self, OcrIne},
};
use crate::spei::firma;

/// Cabecera con la firma del resultado.
pub const CABECERA: &str = "x-kyc-firma";

pub type Futuro<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Resultado firmado del proveedor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultadoProveedor {
    pub verificacion_id: Uuid,
    pub cliente_id: Uuid,
    pub proveedor: String,
    pub referencia: String,
    pub emitido_en: DateTime<Utc>,
    pub prueba_vida: PruebaVida,
    pub comparacion_facial: ComparacionFacial,
    pub documento: Documento,
    /// Huellas SHA-256 de las imágenes que analizó (deben ser las enviadas).
    pub evidencia: Evidencia,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PruebaVida {
    pub exitosa: bool,
    #[serde(default)]
    pub motivo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparacionFacial {
    /// Similitud entre la selfie y la fotografía de la credencial, de 0 a 1.
    /// `None`: no se encontró un rostro comparable. Se lee del texto JSON
    /// exacto (nunca de un f64): 0.8499999999999998 no se redondea a 0.85.
    #[serde(default, with = "score_exacto")]
    pub score: Option<Decimal>,
}

mod score_exacto {
    use std::str::FromStr;

    use rust_decimal::Decimal;
    use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::value::RawValue;

    pub fn serialize<S: Serializer>(v: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(d) => RawValue::from_string(d.to_string()).map_err(S::Error::custom)?.serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Decimal>, D::Error> {
        let Some(crudo) = Option::<Box<RawValue>>::deserialize(d)? else { return Ok(None) };
        let texto = crudo.get().trim();
        if texto == "null" {
            return Ok(None);
        }
        if !texto.starts_with(|c: char| c == '-' || c.is_ascii_digit()) {
            return Err(D::Error::custom("el score debe ser un número JSON"));
        }
        Decimal::from_str(texto)
            .or_else(|_| Decimal::from_scientific(texto))
            .map(Some)
            .map_err(|_| D::Error::custom("score fuera del rango representable"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Documento {
    /// Credencial auténtica y vigente en la lista nominal.
    pub valido: bool,
    #[serde(default)]
    pub motivo: Option<String>,
    #[serde(default)]
    pub ocr: Option<OcrIne>,
}

/// Lo que se envía al proveedor.
#[derive(Debug, Clone)]
pub struct SolicitudProveedor {
    pub verificacion_id: Uuid,
    pub cliente_id: Uuid,
    pub identidad: IdentidadPropia,
    pub imagenes: Imagenes,
}

/// Cómo responde el proveedor.
#[derive(Debug, Clone)]
pub enum Entrega {
    /// Resultado firmado disponible de inmediato.
    Inmediata { cuerpo: Vec<u8>, firma: String },
    /// El resultado llegará por el webhook.
    Pendiente,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ErrorProveedor {
    #[error("el proveedor biométrico no respondió: {0}")]
    NoDisponible(String),
}

pub trait ProveedorBiometrico: Send + Sync {
    fn nombre(&self) -> &'static str;

    /// `false`: no hay proveedor real configurado. El onboarding responde 503
    /// antes de consumir un intento.
    fn disponible(&self) -> bool {
        true
    }

    fn verificar<'a>(&'a self, solicitud: &'a SolicitudProveedor) -> Futuro<'a, Result<Entrega, ErrorProveedor>>;
}

/// Sin proveedor certificado configurado (producción sin integración): nadie
/// se verifica de forma automática; el simulado solo existe en MODO_DEMO.
pub struct ProveedorNoConfigurado;

impl ProveedorBiometrico for ProveedorNoConfigurado {
    fn nombre(&self) -> &'static str {
        "no_configurado"
    }

    fn disponible(&self) -> bool {
        false
    }

    fn verificar<'a>(&'a self, _: &'a SolicitudProveedor) -> Futuro<'a, Result<Entrega, ErrorProveedor>> {
        Box::pin(async { Err(ErrorProveedor::NoDisponible("no hay proveedor biométrico configurado".into())) })
    }
}

// -----------------------------------------------------------------------------
// Proveedor simulado
// -----------------------------------------------------------------------------

/// Comportamiento programado para una CURP (pruebas).
#[derive(Debug, Clone, Default)]
pub struct Regla {
    /// Score de similitud (por omisión 0.93). `Some(None)`: sin rostro.
    pub score: Option<Option<Decimal>>,
    pub vida_fallida: bool,
    pub documento_invalido: bool,
    /// Modifica lo que "lee" de la credencial.
    pub alterar_ocr: Option<fn(&mut OcrIne)>,
    /// Firma con este secreto en lugar del configurado.
    pub secreto_ajeno: bool,
    /// Analiza otras imágenes (huellas distintas de las enviadas).
    pub otra_evidencia: bool,
    /// Falla sin entregar resultado (caída del proveedor).
    pub falla: bool,
    /// No responde de inmediato (el resultado llegaría por webhook).
    pub asincrono: bool,
}

pub const SCORE_SIMULADO: Decimal = Decimal::from_parts(93, 0, 0, false, 2);

pub struct ProveedorSimulado {
    secreto: Vec<u8>,
    reglas: Mutex<HashMap<String, Regla>>,
    /// Resultados que no se entregaron de inmediato (proveedor asíncrono).
    pendientes: Mutex<Vec<(Vec<u8>, String)>>,
}

impl ProveedorSimulado {
    pub fn nuevo(secreto: impl Into<Vec<u8>>) -> Self {
        Self { secreto: secreto.into(), reglas: Mutex::default(), pendientes: Mutex::default() }
    }

    pub fn programar(&self, curp: &str, regla: Regla) {
        self.reglas.lock().expect("mutex").insert(curp.to_owned(), regla);
    }

    /// Resultados firmados que un proveedor asíncrono enviaría por webhook.
    pub fn tomar_pendientes(&self) -> Vec<(Vec<u8>, String)> {
        std::mem::take(&mut *self.pendientes.lock().expect("mutex"))
    }

    /// Lo que concluiría el proveedor (sin firmar).
    pub fn resultado(&self, s: &SolicitudProveedor, regla: &Regla) -> ResultadoProveedor {
        let i = &s.imagenes;
        let mut cuadros: Vec<&str> = i.selfie.iter().map(|c| c.sha256.as_str()).collect();
        cuadros.sort_unstable();
        cuadros.dedup();
        let cuadros_distintos = cuadros.len() == i.selfie.len() && i.selfie.len() >= 3;
        let selfie_no_es_ine =
            !i.selfie.iter().any(|c| c.sha256 == i.ine_frente.sha256 || c.sha256 == i.ine_reverso.sha256);
        let vida = if regla.vida_fallida {
            PruebaVida { exitosa: false, motivo: Some("no se detectó un rostro vivo frente a la cámara".into()) }
        } else if !cuadros_distintos || !selfie_no_es_ine {
            PruebaVida { exitosa: false, motivo: Some("los cuadros de la cámara son idénticos (posible foto o repetición)".into()) }
        } else {
            PruebaVida { exitosa: true, motivo: None }
        };
        let documento = if regla.documento_invalido {
            Documento { valido: false, motivo: Some("la credencial no está vigente en la lista nominal".into()), ocr: None }
        } else if i.ine_frente.sha256 == i.ine_reverso.sha256 {
            Documento { valido: false, motivo: Some("anverso y reverso son la misma imagen".into()), ocr: None }
        } else {
            let mut ocr = ine::ocr_de(&s.identidad.como_declarada());
            if let Some(alterar) = regla.alterar_ocr {
                alterar(&mut ocr);
            }
            Documento { valido: true, motivo: None, ocr: Some(ocr) }
        };
        let mut evidencia = i.evidencia();
        if regla.otra_evidencia {
            evidencia.selfie[0] = "0".repeat(64);
        }
        ResultadoProveedor {
            verificacion_id: s.verificacion_id,
            cliente_id: s.cliente_id,
            proveedor: self.nombre().to_owned(),
            referencia: format!("sim-{}", s.verificacion_id.simple()),
            emitido_en: Utc::now(),
            prueba_vida: vida,
            comparacion_facial: ComparacionFacial { score: regla.score.unwrap_or(Some(SCORE_SIMULADO)) },
            documento,
            evidencia,
        }
    }

    /// Firma un resultado (también la usan las pruebas del webhook).
    pub fn firmar(&self, cuerpo: &[u8]) -> String {
        firma::firmar(&self.secreto, Utc::now().timestamp(), cuerpo)
    }
}

impl ProveedorBiometrico for ProveedorSimulado {
    fn nombre(&self) -> &'static str {
        "simulado"
    }

    fn verificar<'a>(&'a self, s: &'a SolicitudProveedor) -> Futuro<'a, Result<Entrega, ErrorProveedor>> {
        Box::pin(async move {
            let regla = self.reglas.lock().expect("mutex").get(&s.identidad.curp).cloned().unwrap_or_default();
            if regla.falla {
                return Err(ErrorProveedor::NoDisponible("proveedor simulado caído".into()));
            }
            let resultado = self.resultado(s, &regla);
            let cuerpo = serde_json::to_vec(&resultado).map_err(|e| ErrorProveedor::NoDisponible(e.to_string()))?;
            let firma = if regla.secreto_ajeno {
                firma::firmar(b"secreto-de-otro-proveedor-0123456789abcdef", Utc::now().timestamp(), &cuerpo)
            } else {
                self.firmar(&cuerpo)
            };
            if regla.asincrono {
                self.pendientes.lock().expect("mutex").push((cuerpo, firma));
                return Ok(Entrega::Pendiente);
            }
            Ok(Entrega::Inmediata { cuerpo, firma })
        })
    }
}
