//! Monitoreo PLD/FT (prevención de lavado de dinero y financiamiento al
//! terrorismo) sobre el esquema `aml` (migración 0009).
//!
//! La base de datos es el **único** calificador: `aml.evaluar_operacion`
//! calcula el puntaje con datos del ledger, aplica la matriz de riesgo
//! versionada y decide (PERMITIR / RETENER / BLOQUEAR). Rust nunca calcula ni
//! propone un veredicto; solo:
//!
//! 1. toma los candados en el orden global (ver el encabezado de
//!    [`crate::ledger`]): fila IFPE del cliente → candado consultivo PLD →
//!    reserva → SOFOM → caja → contrato o crédito;
//! 2. pasa los hechos de la operación ([`Operacion`]: cliente, canal, monto, el
//!    id y la clave del renglón que VA a escribir, contraparte) ANTES de
//!    cualquier escritura en el ledger;
//! 3. traduce la decisión a HTTP: un BLOQUEAR confirma la transacción sin
//!    renglones del ledger y responde `OPERACION_NO_DISPONIBLE` (mensaje
//!    constante, sin aviso al cliente);
//! 4. vuelve a calcular el hash de cada renglón del ledger que PLD provoca
//!    (retenciones y liberaciones).
//!
//! Los roles transaccionales no tienen USAGE sobre `aml`: llegan al motor solo
//! por las envolturas SECURITY DEFINER `ifpe.aml_*` / `sofom.aml_*`, que solo
//! llama [`evaluacion`].

pub mod consultas;
pub mod evaluacion;
pub mod modelos;
pub mod monitoreo;
pub mod oficial;

pub use evaluacion::{bloquear_cliente_sofom, confirmar_bloqueo, evaluar, retener};
pub use modelos::*;
