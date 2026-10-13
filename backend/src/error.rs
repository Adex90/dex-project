//! Errores de la aplicación y su traducción a HTTP.
//!
//! Toda respuesta de error usa el mismo sobre JSON:
//! `{"error": {"codigo": "...", "mensaje": "..."}}`, también para rutas
//! inexistentes, métodos no permitidos y cuerpos que no se pueden leer.
//!
//! Los errores que levanta la base de datos al violar una regla de negocio
//! (mensajes `SC_*` de los triggers, CHECKs de saldo, unicidad de claves de
//! idempotencia) se convierten en respuestas 4xx con un código estable. Los
//! `SC_*` que solo pueden aparecer si el backend tiene un error o alguien
//! alteró la base de datos (reserva del 100%, ledger inmutable, saldos sin
//! ledger, cortes inválidos...) son fallas de integridad: 500 y log ERROR.
//! Cualquier `permission denied` es un error de programación: significa que el
//! código intentó cruzar el aislamiento IFPE/SOFOM, y se registra como tal.

use axum::{
    extract::rejection::{JsonRejection, PathRejection, QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("no autenticado")]
    NoAutenticado,
    #[error("{0}")]
    CredencialesInvalidas(String),
    #[error("no tienes permiso para esta operación")]
    Prohibido,
    #[error("{0}")]
    NoEncontrado(String),
    #[error("{0}")]
    Validacion(String),
    /// Regla de negocio rechazada (por el backend o por la base de datos).
    #[error("{mensaje}")]
    Negocio { codigo: String, mensaje: String },
    #[error("{mensaje}")]
    Conflicto { codigo: String, mensaje: String },
    #[error("{0}")]
    Bloqueado(String),
    /// Error de protocolo: ruta o método inexistente, tipo de contenido,
    /// cuerpo demasiado grande o demasiado lento.
    #[error("{mensaje}")]
    Peticion { status: StatusCode, codigo: &'static str, mensaje: String },
    /// Contención transitoria (deadlock, serialización, lock_timeout): reintentable.
    #[error("el sistema está ocupado; reintenta la operación")]
    Reintentable,
    /// El hash recalculado no coincide con el de la base de datos, o un
    /// agregado no cuadra. Nunca debe ocurrir; la transacción se aborta.
    #[error("integridad comprometida: {0}")]
    Integridad(String),
    #[error(transparent)]
    Interno(#[from] anyhow::Error),
}

impl AppError {
    pub fn negocio(codigo: &str, mensaje: impl Into<String>) -> Self {
        Self::Negocio { codigo: codigo.into(), mensaje: mensaje.into() }
    }

    pub fn conflicto(codigo: &str, mensaje: impl Into<String>) -> Self {
        Self::Conflicto { codigo: codigo.into(), mensaje: mensaje.into() }
    }

    pub fn validacion(mensaje: impl Into<String>) -> Self {
        Self::Validacion(mensaje.into())
    }

    pub fn no_encontrado(mensaje: impl Into<String>) -> Self {
        Self::NoEncontrado(mensaje.into())
    }

    pub fn peticion(status: StatusCode, codigo: &'static str, mensaje: impl Into<String>) -> Self {
        Self::Peticion { status, codigo, mensaje: mensaje.into() }
    }

    /// Descripción completa para logs, con la cadena de causas de un error
    /// interno (el `Display` de la respuesta HTTP la oculta a propósito).
    pub fn detalle(&self) -> String {
        match self {
            Self::Interno(e) => format!("{e:#}"),
            otro => otro.to_string(),
        }
    }

    /// Código estable para clientes de la API.
    pub fn codigo(&self) -> &str {
        match self {
            Self::NoAutenticado => "NO_AUTENTICADO",
            Self::CredencialesInvalidas(_) => "CREDENCIALES_INVALIDAS",
            Self::Prohibido => "PROHIBIDO",
            Self::NoEncontrado(_) => "NO_ENCONTRADO",
            Self::Validacion(_) => "VALIDACION",
            Self::Negocio { codigo, .. } | Self::Conflicto { codigo, .. } => codigo,
            Self::Bloqueado(_) => "BLOQUEADO",
            Self::Peticion { codigo, .. } => codigo,
            Self::Reintentable => "REINTENTAR",
            Self::Integridad(_) => "INTEGRIDAD",
            Self::Interno(_) => "ERROR_INTERNO",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::NoAutenticado | Self::CredencialesInvalidas(_) => StatusCode::UNAUTHORIZED,
            Self::Prohibido => StatusCode::FORBIDDEN,
            Self::NoEncontrado(_) => StatusCode::NOT_FOUND,
            Self::Validacion(_) => StatusCode::BAD_REQUEST,
            Self::Negocio { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Conflicto { .. } => StatusCode::CONFLICT,
            Self::Bloqueado(_) => StatusCode::LOCKED,
            Self::Peticion { status, .. } => *status,
            Self::Reintentable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Integridad(_) | Self::Interno(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::no_encontrado("recurso no encontrado"),
            sqlx::Error::Database(ref db) => {
                let codigo_sql = db.code().map(|c| c.into_owned()).unwrap_or_default();
                let mensaje = db.message().to_string();
                let restriccion = db.constraint().unwrap_or_default().to_string();
                traducir_error_bd(&codigo_sql, &mensaje, &restriccion)
                    .unwrap_or_else(|| AppError::Interno(anyhow::Error::new(e).context("error de base de datos")))
            }
            sqlx::Error::PoolTimedOut => AppError::Reintentable,
            otro => AppError::Interno(anyhow::Error::new(otro)),
        }
    }
}

/// Códigos `SC_*` que solo aparecen si el backend tiene un error o si alguien
/// alteró la base de datos por fuera de la aplicación. Nunca son una respuesta
/// normal a la petición de un cliente.
fn es_invariante(codigo: &str) -> bool {
    matches!(
        codigo,
        "RESERVA_100"
            | "LEDGER_INMUTABLE"
            | "SALDO_SIN_LEDGER"
            | "MUTUO_DESCUADRE"
            | "CORTE_INCOMPLETO"
            | "CORTE_INVALIDO"
            | "FONDEO_INCOMPLETO"
            | "COBRO_INVALIDO"
    ) || codigo.ends_with("_INMUTABLE")
}

/// Traduce un error de PostgreSQL a un error de dominio. `None` = error interno.
fn traducir_error_bd(codigo_sql: &str, mensaje: &str, restriccion: &str) -> Option<AppError> {
    // Reglas levantadas por los triggers: "SC_CODIGO: mensaje".
    if let Some(resto) = mensaje.strip_prefix("SC_") {
        let (codigo, detalle) = resto.split_once(':').unwrap_or((resto, ""));
        let detalle = detalle.trim();
        let detalle = if detalle.is_empty() { mensaje } else { detalle };
        if es_invariante(codigo) {
            return Some(AppError::Integridad(format!("SC_{codigo}: {detalle}")));
        }
        // El backend valida antes de escribir: que la base de datos tenga que
        // rechazar la operación merece quedar registrado.
        tracing::warn!(codigo, %detalle, "regla rechazada por la base de datos");
        return Some(AppError::negocio(codigo, detalle));
    }
    match codigo_sql {
        // check_violation
        "23514" => Some(if restriccion.contains("caja_saldo") {
            AppError::negocio(
                "LIQUIDEZ_SOFOM_INSUFICIENTE",
                "la caja de la SOFOM no tiene efectivo suficiente para esta operación",
            )
        } else if restriccion.contains("reserva_saldo") {
            AppError::negocio("RESERVA_INSUFICIENTE", "la reserva en custodia no cubre esta operación")
        } else if restriccion.contains("saldo_disponible") || restriccion.contains("saldo_bloqueado") {
            AppError::negocio("SALDO_INSUFICIENTE", "saldo insuficiente")
        } else {
            AppError::negocio("RESTRICCION_VIOLADA", format!("la operación viola la restricción {restriccion}"))
        }),
        // unique_violation
        "23505" => Some(if restriccion.contains("deposito_spei") {
            AppError::conflicto("SPEI_DUPLICADO", "ese SPEI (clave de rastreo) ya fue acreditado")
        } else if restriccion.contains("clave_idempotencia") {
            AppError::conflicto("OPERACION_DUPLICADA", "ya existe una operación con esa clave de idempotencia")
        } else if restriccion.contains("email") {
            AppError::conflicto("EMAIL_REGISTRADO", "ese email ya está registrado")
        } else if restriccion.contains("curp") || restriccion.contains("rfc") {
            AppError::conflicto("IDENTIDAD_REGISTRADA", "ese CURP/RFC ya está registrado")
        } else if restriccion.contains("cliente_moneda") {
            AppError::conflicto("CUENTA_EXISTENTE", "el cliente ya tiene una cuenta en esa moneda")
        } else {
            AppError::conflicto("DUPLICADO", "registro duplicado")
        }),
        // foreign_key_violation
        "23503" => Some(AppError::validacion("referencia a un registro inexistente")),
        // Datos que no caben en la columna: entrada inválida, no un error interno.
        "22001" | "22003" | "22P02" | "22007" | "22008" | "23502" => {
            tracing::warn!(codigo_sql, %mensaje, "dato rechazado por la base de datos");
            Some(AppError::validacion(match codigo_sql {
                "22001" => "un campo excede la longitud permitida",
                "22003" => "un monto o número está fuera del rango permitido",
                "23502" => "falta un campo obligatorio",
                _ => "un campo tiene un formato inválido",
            }))
        }
        // serialization_failure, deadlock_detected, lock_not_available, query_canceled (timeout)
        "40001" | "40P01" | "55P03" | "57014" => Some(AppError::Reintentable),
        // insufficient_privilege: el código intentó cruzar el aislamiento de roles.
        "42501" => {
            tracing::error!(%mensaje, "VIOLACIÓN DE AISLAMIENTO: una transacción intentó acceder fuera de su rol");
            None
        }
        _ => None,
    }
}

/// Rechazo de un extractor (cuerpo, query o ruta) en el formato de la API.
fn rechazo(status: StatusCode, texto: String, causa: &(dyn std::error::Error + 'static)) -> AppError {
    let lento = std::iter::successors(Some(causa), |e| e.source()).any(|e| e.is::<tower_http::timeout::TimeoutError>());
    if lento {
        return AppError::peticion(
            StatusCode::REQUEST_TIMEOUT,
            "TIEMPO_AGOTADO",
            "el cuerpo de la petición no llegó a tiempo",
        );
    }
    match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            AppError::peticion(status, "TIPO_NO_SOPORTADO", "el cuerpo debe ser JSON (Content-Type: application/json)")
        }
        StatusCode::PAYLOAD_TOO_LARGE => {
            AppError::peticion(status, "CUERPO_DEMASIADO_GRANDE", "el cuerpo de la petición excede el máximo permitido")
        }
        _ => AppError::Validacion(texto),
    }
}

impl From<JsonRejection> for AppError {
    fn from(r: JsonRejection) -> Self {
        rechazo(r.status(), r.body_text(), &r)
    }
}

impl From<QueryRejection> for AppError {
    fn from(r: QueryRejection) -> Self {
        rechazo(r.status(), r.body_text(), &r)
    }
}

impl From<PathRejection> for AppError {
    fn from(r: PathRejection) -> Self {
        rechazo(r.status(), r.body_text(), &r)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let mensaje = match &self {
            AppError::Interno(e) => {
                tracing::error!(error = %format!("{e:#}"), "error interno");
                "error interno del servidor".to_string()
            }
            AppError::Integridad(detalle) => {
                tracing::error!(%detalle, "FALLA DE INTEGRIDAD");
                "falla de integridad detectada; la operación fue revertida".to_string()
            }
            otro => otro.to_string(),
        };
        let cuerpo = json!({ "error": { "codigo": self.codigo(), "mensaje": mensaje } });
        (status, Json(cuerpo)).into_response()
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn traduce_reglas_de_triggers() {
        let e = traducir_error_bd("P0001", "SC_CALCE_PLAZOS: el mutuo vence antes que el crédito", "").unwrap();
        assert_eq!(e.codigo(), "CALCE_PLAZOS");
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(e.to_string(), "el mutuo vence antes que el crédito");
    }

    #[test]
    fn invariantes_de_la_base_son_fallas_de_integridad() {
        for mensaje in [
            "SC_RESERVA_100: pasivo (1) <> reserva (0).",
            "SC_SALDO_SIN_LEDGER: la cuenta x no coincide con el ledger",
            "SC_LEDGER_INMUTABLE: el ledger es de solo inserción",
            "SC_CORTE_INVALIDO: el ancla retrocede",
            "SC_MUTUO_INMUTABLE: los términos no se modifican",
        ] {
            let e = traducir_error_bd("P0001", mensaje, "").unwrap();
            assert_eq!(e.codigo(), "INTEGRIDAD", "{mensaje}");
            assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert!(e.to_string().contains(mensaje.split(':').next().unwrap()));
        }
    }

    #[test]
    fn datos_fuera_de_rango_son_validacion() {
        for codigo in ["22001", "22003", "22P02", "23502"] {
            let e = traducir_error_bd(codigo, "value too long for type character varying(13)", "").unwrap();
            assert_eq!(e.codigo(), "VALIDACION", "{codigo}");
            assert_eq!(e.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn traduce_saldo_y_duplicados() {
        let e = traducir_error_bd("23514", "x", "ck_ifpe_cuentas_saldo_disponible_no_negativo").unwrap();
        assert_eq!(e.codigo(), "SALDO_INSUFICIENTE");
        let e = traducir_error_bd("23514", "x", "ck_sofom_caja_saldo_no_negativo").unwrap();
        assert_eq!(e.codigo(), "LIQUIDEZ_SOFOM_INSUFICIENTE");
        let e = traducir_error_bd("23505", "x", "transacciones_clave_idempotencia_key").unwrap();
        assert_eq!(e.status(), StatusCode::CONFLICT);
        assert!(traducir_error_bd("42501", "permission denied", "").is_none());
        assert!(matches!(traducir_error_bd("40P01", "deadlock", ""), Some(AppError::Reintentable)));
    }
}
