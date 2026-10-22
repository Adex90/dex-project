//! Tipos del monitoreo PLD/FT: hechos de una operación que se pasan al motor
//! de la base de datos (nunca un veredicto), su dictamen y las solicitudes de
//! la consola de cumplimiento (§4.2 del contrato HTTP).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{models::Entidad, spei::inbound::Origen};

/// Sentido del dinero desde la perspectiva del cliente evaluado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sentido {
    Entrada,
    Salida,
}

impl Sentido {
    pub const fn as_str(self) -> &'static str {
        match self {
            Sentido::Entrada => "ENTRADA",
            Sentido::Salida => "SALIDA",
        }
    }
}

/// Canal de la operación (columna `aml.evaluaciones.canal`). Cada canal tiene
/// una sola entidad y un solo sentido válidos; la base de datos rechaza
/// cualquier otra combinación (`SC_AML_PARAMETRO_INVALIDO`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canal {
    // ENTRADA (IFPE)
    SpeiWebhook,
    SpeiOperador,
    SpeiDemo,
    TransferenciaRecibida,
    // SALIDA (IFPE)
    TransferenciaEnviada,
    SpeiEnviado,
    InversionMutuo,
    // SALIDA (SOFOM)
    DesembolsoCredito,
    // ENTRADA (SOFOM)
    CobroCredito,
}

impl Canal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Canal::SpeiWebhook => "SPEI_WEBHOOK",
            Canal::SpeiOperador => "SPEI_OPERADOR",
            Canal::SpeiDemo => "SPEI_DEMO",
            Canal::TransferenciaRecibida => "TRANSFERENCIA_RECIBIDA",
            Canal::TransferenciaEnviada => "TRANSFERENCIA_ENVIADA",
            Canal::SpeiEnviado => "SPEI_ENVIADO",
            Canal::InversionMutuo => "INVERSION_MUTUO",
            Canal::DesembolsoCredito => "DESEMBOLSO_CREDITO",
            Canal::CobroCredito => "COBRO_CREDITO",
        }
    }

    pub const fn sentido(self) -> Sentido {
        match self {
            Canal::SpeiWebhook
            | Canal::SpeiOperador
            | Canal::SpeiDemo
            | Canal::TransferenciaRecibida
            | Canal::CobroCredito => Sentido::Entrada,
            Canal::TransferenciaEnviada | Canal::SpeiEnviado | Canal::InversionMutuo | Canal::DesembolsoCredito => {
                Sentido::Salida
            }
        }
    }

    pub const fn entidad(self) -> Entidad {
        match self {
            Canal::DesembolsoCredito | Canal::CobroCredito => Entidad::Sofom,
            _ => Entidad::Ifpe,
        }
    }

    pub const fn de_origen_spei(o: Origen) -> Canal {
        match o {
            Origen::Webhook => Canal::SpeiWebhook,
            Origen::Operador(_) => Canal::SpeiOperador,
            Origen::Demo => Canal::SpeiDemo,
        }
    }
}

/// Contraparte de la operación. Se serializa siempre con las cuatro claves
/// (`null` si falta): es la forma de `aml.evaluaciones.contraparte`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Contraparte {
    pub nombre: Option<String>,
    pub cuenta: Option<String>,
    pub rfc_curp: Option<String>,
    pub cliente_id: Option<Uuid>,
}

/// Hechos de una operación que el camino del dinero está a punto de escribir.
pub struct Operacion<'a> {
    pub cliente_id: Uuid,
    pub canal: Canal,
    pub monto: Decimal,
    /// Id del renglón del ledger que este camino VA a insertar (IFPE o SOFOM
    /// según `canal.entidad()`).
    pub transaccion_id: Uuid,
    /// La `clave_idempotencia` que se escribirá en el ledger (ya acotada por
    /// `Usuario::clave`).
    pub clave: &'a str,
    pub contraparte: Contraparte,
}

/// Decisión del motor (`ifpe.aml_evaluar` / `sofom.aml_evaluar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Decision {
    Permitir,
    Retener,
    Bloquear,
}

/// Un renglón de `ifpe.aml_evaluar` / `sofom.aml_evaluar`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Dictamen {
    pub evaluacion_id: Uuid,
    pub decision: Decision,
    pub puntaje: Option<Decimal>,
    pub nivel: Option<String>,
    pub monto_retener: Decimal,
}

// -----------------------------------------------------------------------------
// Solicitudes de la consola de cumplimiento (§4.2). Los textos se recortan y
// validan en Rust antes de llegar a la base de datos, que los vuelve a validar.
// -----------------------------------------------------------------------------

macro_rules! enum_texto {
    ($(#[$doc:meta])* $nombre:ident { $($variante:ident => $texto:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $nombre {
            $(#[serde(rename = $texto)] $variante),+
        }

        impl $nombre {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($nombre::$variante => $texto),+
                }
            }
        }
    };
}

enum_texto!(
    /// Acción de un oficial sobre una alerta.
    AccionResolucion {
        Tomar => "TOMAR",
        Liberar => "LIBERAR",
        Escalar => "ESCALAR",
        Confirmar => "CONFIRMAR",
        FalsoPositivo => "FALSO_POSITIVO",
    }
);

enum_texto!(
    /// Tipo de reporte a la UIF de una alerta confirmada.
    TipoReporte {
        Inusual => "INUSUAL",
        InternaPreocupante => "INTERNA_PREOCUPANTE",
    }
);

enum_texto!(
    /// Solicitud de cuatro ojos sobre un congelamiento.
    TipoSolicitud {
        LevantarYLiberar => "LEVANTAR_Y_LIBERAR",
        LevantarConservandoRetenciones => "LEVANTAR_CONSERVANDO_RETENCIONES",
        LiberarRetenciones => "LIBERAR_RETENCIONES",
    }
);

enum_texto!(
    /// Propuesta de cambio de una regla o de los parámetros.
    TipoPropuesta {
        Regla => "REGLA",
        Parametros => "PARAMETROS",
    }
);

enum_texto!(
    /// Nivel de cliente al que aplica una versión de regla.
    NivelClienteRegla {
        Bajo => "BAJO",
        Medio => "MEDIO",
        Alto => "ALTO",
        Todos => "TODOS",
    }
);

/// `POST /api/v1/aml/alertas/{id}/resolver`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudResolucion {
    pub accion: AccionResolucion,
    #[serde(default)]
    pub dictamen: Option<String>,
    #[serde(default)]
    pub tipo_reporte: Option<TipoReporte>,
    #[serde(default)]
    pub urgente: Option<bool>,
    #[serde(default)]
    pub congelar: Option<bool>,
    #[serde(default)]
    pub tipo_levantamiento: Option<TipoSolicitud>,
    pub clave_idempotencia: String,
}

/// `POST /api/v1/aml/alertas/{id}/reporte-uif`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudReporteUif {
    pub folio_uif: String,
    pub reportada_en: chrono::NaiveDate,
    pub clave_idempotencia: String,
}

/// `POST /api/v1/aml/clientes/{id}/congelar`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudCongelamiento {
    pub motivo: String,
    #[serde(default)]
    pub alerta_id: Option<Uuid>,
    pub clave_idempotencia: String,
}

/// `POST /api/v1/aml/congelamientos/{id}/solicitudes`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudLevantamiento {
    pub tipo: TipoSolicitud,
    pub justificacion: String,
    pub clave_idempotencia: String,
}

/// `POST /api/v1/aml/solicitudes/{id}/decision` y `/propuestas/{id}/decision`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudDecision {
    pub aprobar: bool,
    pub comentario: String,
    pub clave_idempotencia: String,
}

/// `POST /api/v1/aml/propuestas`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolicitudPropuesta {
    pub tipo: TipoPropuesta,
    #[serde(default)]
    pub regla_codigo: Option<String>,
    #[serde(default)]
    pub nivel_cliente: Option<NivelClienteRegla>,
    pub cambios: serde_json::Map<String, serde_json::Value>,
    pub justificacion: String,
    pub clave_idempotencia: String,
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn canal_entidad_y_sentido() {
        use Canal::*;
        let todos = [
            SpeiWebhook,
            SpeiOperador,
            SpeiDemo,
            TransferenciaRecibida,
            TransferenciaEnviada,
            SpeiEnviado,
            InversionMutuo,
            DesembolsoCredito,
            CobroCredito,
        ];
        let entradas: Vec<_> = todos.iter().filter(|c| c.sentido() == Sentido::Entrada).map(|c| c.as_str()).collect();
        assert_eq!(entradas, ["SPEI_WEBHOOK", "SPEI_OPERADOR", "SPEI_DEMO", "TRANSFERENCIA_RECIBIDA", "COBRO_CREDITO"]);
        let sofom: Vec<_> = todos.iter().filter(|c| c.entidad() == Entidad::Sofom).map(|c| c.as_str()).collect();
        assert_eq!(sofom, ["DESEMBOLSO_CREDITO", "COBRO_CREDITO"]);
        assert_eq!(Canal::de_origen_spei(Origen::Operador(Uuid::nil())), SpeiOperador);
    }

    #[test]
    fn contraparte_con_cuatro_claves() {
        let v = serde_json::to_value(Contraparte { cuenta: Some("012".into()), ..Default::default() }).unwrap();
        assert_eq!(v, serde_json::json!({"nombre": null, "cuenta": "012", "rfc_curp": null, "cliente_id": null}));
    }
}
