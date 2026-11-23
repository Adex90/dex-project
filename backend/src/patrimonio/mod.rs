//! Patrimonio: las cuentas vinculadas del cliente (la lista blanca de
//! retiros, migración 0016) y el retiro de su efectivo a una de ellas.
//!
//! - **Retiro**: un SPEI a cuenta propia del motor de [`crate::spei::outbound`].
//!   El disponible y la reserva bajan al instante con un RETIRO del ledger y la
//!   orden queda PENDIENTE: para el cliente, «en proceso» hasta que la red SPEI
//!   (STP/Banxico) la liquida, o se le reembolsa si el banco la rechaza o la
//!   devuelve. Solo cuenta `saldo_disponible`: el efectivo comprometido en
//!   órdenes límite y stop (0015) no se retira. El cambio de saldo sube la
//!   versión de la bóveda (`seq`, 0012) y el WebSocket lo avisa.
//! - **Lista blanca**: el destino es una cuenta vinculada activa a nombre del
//!   cliente. El motor lo exige en todo retiro a cuenta propia (también en
//!   `/ifpe/retiros`), después de PLD; la base de datos, en toda orden ligada
//!   a una cuenta vinculada.
//! - **Demostración**: con `MODO_DEMO`, un cliente que puede operar y no tiene
//!   cuentas vinculadas recibe una al consultarlas: BBVA México, a su nombre,
//!   con una CLABE válida que termina en 1234. Fuera de `MODO_DEMO` no se
//!   vincula ninguna: falta el flujo que verifica con el banco que la cuenta
//!   es del cliente (docs/FRONTEND_ROADMAP.md, módulo 16).

pub mod documentos;
pub mod impuestos;
pub mod modelos;
pub mod vinculadas;

use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use self::modelos::{CuentasVinculadas, EstadoRetiro, ReciboRetiro, VistaRetiro};
use crate::{
    config::Config,
    error::AppResult,
    spei::outbound::{self, SolicitudSpei, SpeiOutbound},
};

/// Concepto SPEI de un retiro (el juego de caracteres de SPEI).
pub const CONCEPTO_RETIRO: &str = "RETIRO A CUENTA PROPIA";

/// Las cuentas vinculadas activas del cliente (`LinkedAccounts`). Con
/// `MODO_DEMO`, antes vincula la de demostración si no tiene ninguna.
pub async fn cuentas_vinculadas(pool: &PgPool, config: &Config, cliente_id: Uuid) -> AppResult<CuentasVinculadas> {
    if config.modo_demo {
        vinculadas::asegurar_demo(pool, cliente_id).await?;
    }
    let cuentas = vinculadas::activas(pool, cliente_id).await?;
    Ok(CuentasVinculadas { cuentas: cuentas.iter().map(vinculadas::vista).collect() })
}

/// Un retiro: `monto` del disponible a la cuenta vinculada elegida.
#[derive(Debug, Clone)]
pub struct SolicitudRetiro {
    pub cliente_id: Uuid,
    pub cuenta_vinculada_id: Uuid,
    pub monto: Decimal,
    /// Clave ya acotada al cliente (`cli:<id>:<clave>`).
    pub clave_idempotencia: String,
}

pub struct ResultadoRetiro {
    pub recibo: ReciboRetiro,
    pub idempotente: bool,
}

/// Retira efectivo disponible a una cuenta vinculada. La cuenta se busca
/// entre las del cliente (una ajena o inexistente: 404); el motor SPEI exige
/// que siga activa y a su nombre (`CUENTA_NO_VINCULADA`), el saldo
/// disponible (`SALDO_INSUFICIENTE`) y PLD (`OPERACION_NO_DISPONIBLE`). Una
/// repetición con la misma clave devuelve el mismo retiro en su estado
/// actual.
pub async fn retirar(pool: &PgPool, config: &Config, s: &SolicitudRetiro) -> AppResult<ResultadoRetiro> {
    let cuenta = vinculadas::del_cliente(pool, s.cliente_id, s.cuenta_vinculada_id).await?;
    let r = outbound::solicitar(
        pool,
        &config.pld,
        &config.spei,
        &SolicitudSpei {
            cliente_id: s.cliente_id,
            clabe_destino: cuenta.clabe.trim().to_owned(),
            beneficiario_nombre: None,
            beneficiario_rfc_curp: None,
            monto: s.monto,
            concepto: Some(CONCEPTO_RETIRO.into()),
            referencia_numerica: None,
            clave_idempotencia: s.clave_idempotencia.clone(),
            cuenta_vinculada_id: Some(cuenta.id),
        },
    )
    .await?;
    Ok(ResultadoRetiro {
        recibo: ReciboRetiro {
            retiro: VistaRetiro {
                id: r.orden.id,
                estado: estado_retiro(&r.orden),
                monto: r.orden.monto,
                cuenta: vinculadas::vista(&cuenta),
                clave_rastreo: r.orden.clave_rastreo.clone(),
                solicitado_en: r.orden.creado_en,
            },
        },
        idempotente: r.idempotente,
    })
}

/// El estado de la orden como lo ve el cliente.
pub fn estado_retiro(orden: &SpeiOutbound) -> EstadoRetiro {
    match orden.estado.as_str() {
        "LIQUIDADA" => EstadoRetiro::Liquidado,
        "RECHAZADA" | "DEVUELTA" => EstadoRetiro::Devuelto,
        // PENDIENTE y ENVIADA: el dinero viaja por la red SPEI.
        _ => EstadoRetiro::EnProceso,
    }
}
