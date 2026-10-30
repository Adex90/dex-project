//! Correo saliente simulado (restablecimiento de contraseña).
//!
//! No hay proveedor de correo integrado. Con `MODO_DEMO` el mensaje se escribe
//! en la bitácora (`target: "correo"`) y se conserva en memoria (los últimos
//! [`CAPACIDAD`], para las pruebas). Fuera de `MODO_DEMO` el enlace **no** se
//! escribe en ningún lado: un token vigente en la bitácora permitiría tomar la
//! cuenta a quien la lea. Hasta integrar un proveedor, en producción los
//! restablecimientos los atiende soporte (DEPLOYMENT.md).

use std::{collections::VecDeque, sync::Mutex};

use uuid::Uuid;

const CAPACIDAD: usize = 100;

#[derive(Debug, Clone)]
pub struct CorreoSimulado {
    pub para: String,
    pub asunto: String,
    pub enlace: String,
}

#[derive(Debug)]
pub struct BuzonSimulado {
    modo_demo: bool,
    enviados: Mutex<VecDeque<CorreoSimulado>>,
}

impl BuzonSimulado {
    pub fn nuevo(modo_demo: bool) -> Self {
        Self { modo_demo, enviados: Mutex::new(VecDeque::new()) }
    }

    /// Enlace de restablecimiento para `para` (cliente `cliente_id`).
    pub fn restablecimiento(&self, cliente_id: Uuid, para: &str, enlace: String) {
        if !self.modo_demo {
            tracing::warn!(
                target: "correo",
                %cliente_id,
                "restablecimiento de contraseña solicitado, pero no hay proveedor de correo: el enlace no se envió"
            );
            return;
        }
        tracing::info!(target: "correo", %para, %enlace, "SIMULADO: enlace para restablecer la contraseña");
        let mut enviados = self.enviados.lock().unwrap_or_else(|e| e.into_inner());
        if enviados.len() == CAPACIDAD {
            enviados.pop_front();
        }
        enviados
            .push_back(CorreoSimulado { para: para.to_owned(), asunto: "Restablece tu contraseña".into(), enlace });
    }

    /// Último correo simulado para `para` (pruebas).
    pub fn ultimo_para(&self, para: &str) -> Option<CorreoSimulado> {
        let enviados = self.enviados.lock().unwrap_or_else(|e| e.into_inner());
        enviados.iter().rev().find(|c| c.para.eq_ignore_ascii_case(para)).cloned()
    }

    /// Correos simulados para `para` (pruebas).
    pub fn cuantos_para(&self, para: &str) -> usize {
        let enviados = self.enviados.lock().unwrap_or_else(|e| e.into_inner());
        enviados.iter().filter(|c| c.para.eq_ignore_ascii_case(para)).count()
    }
}
