//! Puerta KYC y Lista de Personas Bloqueadas. El monitoreo transaccional vive
//! en `crate::aml` y en el esquema `aml`.
//!
//! [`PerfilCliente::exigir_operable`]: solo clientes con KYC verificado y fuera
//! de la Lista de Personas Bloqueadas pueden mover dinero.

use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    db::Tx,
    error::{AppError, AppResult},
    models::kyc::{EstatusKyc, NivelRiesgo},
};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PerfilCliente {
    pub id: Uuid,
    pub estatus_kyc: EstatusKyc,
    pub en_lista_bloqueadas: bool,
    pub nivel_riesgo_pld: NivelRiesgo,
    pub ingreso_mensual_estimado: Option<Decimal>,
    pub es_pep: bool,
}

/// Carga el perfil de riesgo. Todos los roles transaccionales pueden leer `kyc.clientes`.
pub async fn cargar_perfil(tx: &mut Tx, cliente_id: Uuid) -> AppResult<PerfilCliente> {
    sqlx::query_as(
        "SELECT id, estatus_kyc, en_lista_bloqueadas, nivel_riesgo_pld, ingreso_mensual_estimado, es_pep \
         FROM kyc.clientes WHERE id = $1",
    )
    .bind(cliente_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::no_encontrado("cliente no encontrado"))
}

impl PerfilCliente {
    pub fn exigir_operable(&self) -> AppResult<()> {
        if self.en_lista_bloqueadas {
            return Err(AppError::negocio(
                "CLIENTE_EN_LISTA_BLOQUEADAS",
                "el cliente está en la Lista de Personas Bloqueadas; operación rechazada",
            ));
        }
        if self.estatus_kyc != EstatusKyc::Verificado {
            return Err(AppError::negocio("KYC_NO_VERIFICADO", "el expediente KYC del cliente no está verificado"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn perfil(riesgo: NivelRiesgo, ingreso: Option<i64>) -> PerfilCliente {
        PerfilCliente {
            id: Uuid::nil(),
            estatus_kyc: EstatusKyc::Verificado,
            en_lista_bloqueadas: false,
            nivel_riesgo_pld: riesgo,
            ingreso_mensual_estimado: ingreso.map(Decimal::from),
            es_pep: false,
        }
    }

    #[test]
    fn exige_kyc_y_listas() {
        let mut p = perfil(NivelRiesgo::Bajo, None);
        assert!(p.exigir_operable().is_ok());
        p.estatus_kyc = EstatusKyc::Pendiente;
        assert_eq!(p.exigir_operable().unwrap_err().codigo(), "KYC_NO_VERIFICADO");
        p.estatus_kyc = EstatusKyc::Suspendido;
        p.en_lista_bloqueadas = true;
        assert_eq!(p.exigir_operable().unwrap_err().codigo(), "CLIENTE_EN_LISTA_BLOQUEADAS");
    }
}
