//! Acceso a PostgreSQL con aislamiento por rol.
//!
//! **Toda** transacción de negocio se abre con [`begin`] (o sus variantes),
//! que ejecuta `SET LOCAL ROLE <rol>` como primera sentencia. A partir de ahí
//! la base de datos solo permite lo que ese rol tiene concedido en las
//! migraciones: una transacción de la IFPE no puede leer ni escribir tablas de
//! la SOFOM, y el puente no puede originar créditos. `SET LOCAL` se revierte
//! al terminar la transacción, así que el rol nunca se filtra a otra petición
//! que reutilice la conexión del pool.
//!
//! Este es el único módulo que llama a `pool.begin()` (lo comprueba una prueba).

use std::{future::Future, time::Duration};

use sqlx::{postgres::PgPoolOptions, Executor, PgPool, Postgres, Transaction};

use crate::{config::Config, error::AppError};

pub type Tx = Transaction<'static, Postgres>;

/// Roles NOLOGIN definidos en las migraciones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rol {
    /// Bóveda a la vista: depósitos, retiros, transferencias, retenciones.
    Ifpe,
    /// Crédito: originación, asignación de fondeo, cobranza, capital.
    Sofom,
    /// Puente IFPE <-> mutuo. Sin acceso a créditos ni asignaciones.
    Puente,
    /// Proof of Reserve: lectura de la bóveda y registro de cortes.
    Auditor,
    /// Identidad: alta, autenticación, captura del expediente KYC, bandeja PLD.
    Kyc,
    /// Dictamen KYC: registra resultados del proveedor biométrico y verifica,
    /// rechaza o reabre expedientes. Solo el webhook firmado del proveedor y
    /// los operadores.
    KycVerificador,
    /// SPEI: registra lo que informa el proveedor (abonos, estados de órdenes,
    /// reembolsos) y las correcciones del operador. Solo lo usan los webhooks
    /// firmados, el despachador y el operador; nunca una ruta del cliente.
    Spei,
}

impl Rol {
    pub const fn nombre(self) -> &'static str {
        match self {
            Rol::Ifpe => "sc_ifpe",
            Rol::Sofom => "sc_sofom",
            Rol::Puente => "sc_puente",
            Rol::Auditor => "sc_auditor",
            Rol::Kyc => "sc_kyc",
            Rol::KycVerificador => "sc_kyc_verificador",
            Rol::Spei => "sc_spei",
        }
    }

    /// Sentencia literal (los identificadores de rol no admiten parámetros;
    /// al ser constantes no hay riesgo de inyección).
    const fn set_local(self) -> &'static str {
        match self {
            Rol::Ifpe => "SET LOCAL ROLE sc_ifpe",
            Rol::Sofom => "SET LOCAL ROLE sc_sofom",
            Rol::Puente => "SET LOCAL ROLE sc_puente",
            Rol::Auditor => "SET LOCAL ROLE sc_auditor",
            Rol::Kyc => "SET LOCAL ROLE sc_kyc",
            Rol::KycVerificador => "SET LOCAL ROLE sc_kyc_verificador",
            Rol::Spei => "SET LOCAL ROLE sc_spei",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Modo {
    Escritura,
    Lectura,
    /// REPEATABLE READ de solo lectura: una foto consistente de varias tablas.
    LecturaInstantanea,
    /// REPEATABLE READ con escritura (p. ej. registrar un corte de reserva).
    EscrituraInstantanea,
}

/// Transacción READ COMMITTED de lectura/escritura con el rol indicado.
pub async fn begin(pool: &PgPool, rol: Rol) -> Result<Tx, sqlx::Error> {
    begin_con(pool, rol, Modo::Escritura).await
}

/// Transacción de solo lectura con el rol indicado.
pub async fn begin_lectura(pool: &PgPool, rol: Rol) -> Result<Tx, sqlx::Error> {
    begin_con(pool, rol, Modo::Lectura).await
}

/// Foto consistente (REPEATABLE READ) de solo lectura.
pub async fn begin_lectura_instantanea(pool: &PgPool, rol: Rol) -> Result<Tx, sqlx::Error> {
    begin_con(pool, rol, Modo::LecturaInstantanea).await
}

/// Foto consistente (REPEATABLE READ) que además puede escribir.
pub async fn begin_instantanea(pool: &PgPool, rol: Rol) -> Result<Tx, sqlx::Error> {
    begin_con(pool, rol, Modo::EscrituraInstantanea).await
}

async fn begin_con(pool: &PgPool, rol: Rol, modo: Modo) -> Result<Tx, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // SET TRANSACTION debe preceder a cualquier consulta de la transacción.
    let caracteristicas = match modo {
        Modo::Escritura => None,
        Modo::Lectura => Some("SET TRANSACTION READ ONLY"),
        Modo::LecturaInstantanea => Some("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY"),
        Modo::EscrituraInstantanea => Some("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"),
    };
    if let Some(sql) = caracteristicas {
        (&mut *tx).execute(sql).await?;
    }
    (&mut *tx).execute(rol.set_local()).await?;
    Ok(tx)
}

/// Transacción con el usuario dueño del esquema, **sin** rol restringido.
/// Exclusiva de las tareas de arranque ([`crate::bootstrap`]), que crean
/// infraestructura (cuentas de custodia, caja, operador inicial) y nunca
/// mueven saldos. Ninguna ruta HTTP la usa.
pub async fn begin_administrativa(pool: &PgPool) -> Result<Tx, sqlx::Error> {
    pool.begin().await
}

/// Pool con límites de seguridad por conexión: ninguna transacción puede
/// quedarse esperando un lock o abierta indefinidamente.
pub async fn crear_pool(cfg: &Config) -> Result<PgPool, sqlx::Error> {
    crear_pool_con(&cfg.database_url, cfg.max_conexiones).await
}

/// Pool del dueño del esquema (`DATABASE_URL_PROPIETARIO`), solo para
/// migraciones y arranque.
pub async fn crear_pool_propietario(url: &str) -> Result<PgPool, sqlx::Error> {
    crear_pool_con(url, 2).await
}

/// Comprueba con qué privilegios corre la aplicación. Si el usuario de
/// `DATABASE_URL` es dueño del esquema (o superusuario) puede desactivar
/// triggers y saltarse los permisos por rol: la integridad depende entonces
/// solo de la detección (anclas, auditoría), no de la prevención.
pub async fn verificar_usuario_de_aplicacion(pool: &PgPool) -> Result<(), sqlx::Error> {
    let (usuario, superusuario, dueno, miembro): (String, bool, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, \
                (SELECT rolsuper FROM pg_roles WHERE rolname = current_user), \
                EXISTS (SELECT 1 FROM pg_namespace WHERE nspname IN ('ifpe', 'sofom', 'kyc', 'nucleo') \
                         AND pg_has_role(current_user, nspowner, 'USAGE')), \
                pg_has_role(current_user, 'sc_ifpe', 'MEMBER') AND pg_has_role(current_user, 'sc_sofom', 'MEMBER') \
                AND pg_has_role(current_user, 'sc_puente', 'MEMBER') AND pg_has_role(current_user, 'sc_auditor', 'MEMBER') \
                AND pg_has_role(current_user, 'sc_kyc', 'MEMBER') AND pg_has_role(current_user, 'sc_spei', 'MEMBER') \
                AND pg_has_role(current_user, 'sc_kyc_verificador', 'MEMBER')",
    )
    .fetch_one(pool)
    .await?;
    if !miembro {
        tracing::error!(%usuario, "el usuario de la aplicación no es miembro de sc_ifpe, sc_sofom, sc_puente, sc_auditor, sc_kyc, sc_kyc_verificador y sc_spei");
    }
    if superusuario || dueno {
        tracing::warn!(
            %usuario,
            "la aplicación corre como dueño del esquema: puede desactivar triggers. En producción usa \
             DATABASE_URL_PROPIETARIO para migrar y un usuario sin privilegios en DATABASE_URL (ver README)"
        );
    } else {
        tracing::info!(%usuario, "la aplicación corre sin privilegios de dueño: solo puede actuar mediante los roles sc_*");
    }
    Ok(())
}

async fn crear_pool_con(url: &str, max_conexiones: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_conexiones)
        .acquire_timeout(Duration::from_secs(10))
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute(
                    "SET application_name = 'synthetic-capital'; \
                     SET lock_timeout = '5s'; \
                     SET statement_timeout = '30s'; \
                     SET idle_in_transaction_session_timeout = '60s'; \
                     SET TIME ZONE 'UTC'",
                )
                .await?;
                Ok(())
            })
        })
        .connect(url)
        .await
}

/// Reintenta una operación transaccional completa ante contención transitoria
/// (deadlock, fallo de serialización, lock_timeout). Cada intento abre su
/// propia transacción, así que reintentar es seguro.
pub async fn con_reintentos<T, F, Fut>(mut operacion: F) -> Result<T, AppError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
{
    const INTENTOS: u32 = 3;
    let mut intento = 1;
    loop {
        match operacion().await {
            Err(AppError::Reintentable) if intento < INTENTOS => {
                tracing::warn!(intento, "contención transitoria; reintentando");
                tokio::time::sleep(Duration::from_millis(25 * u64::from(intento))).await;
                intento += 1;
            }
            resultado => return resultado,
        }
    }
}
