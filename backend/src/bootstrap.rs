//! Tareas administrativas de arranque, idempotentes.
//!
//! Se ejecutan con el usuario dueño del esquema (sin `SET ROLE`) porque crean
//! infraestructura que ningún rol transaccional puede crear: cuentas de
//! reserva en custodia, caja de la SOFOM, el operador inicial y (solo en
//! MODO_DEMO) los oficiales de cumplimiento. Ninguna mueve saldos: las cuentas
//! nacen en cero.

use sqlx::PgPool;

use crate::{auth::password, config::Config, db, error::AppResult, models::kyc::RolUsuario};

pub async fn ejecutar(pool: &PgPool, cfg: &Config) -> AppResult<()> {
    verificar_codificacion(pool).await?;
    asegurar_infraestructura(pool, cfg).await?;
    if let (Some(email), Some(pw)) = (&cfg.operador_email, &cfg.operador_password) {
        asegurar_operador(pool, email, pw).await?;
    }
    if cfg.modo_demo {
        if let Some(pw) = &cfg.pld.bootstrap_password {
            for email in &cfg.pld.bootstrap_emails {
                asegurar_usuario_interno(pool, email, pw, RolUsuario::Cumplimiento).await?;
            }
        }
    } else if !cfg.pld.bootstrap_emails.is_empty() || cfg.pld.bootstrap_password.is_some() {
        // En producción el DBA da de alta a los oficiales como dueño del esquema.
        tracing::warn!("BOOTSTRAP_CUMPLIMIENTO_* solo se usa con MODO_DEMO; se ignora");
    }
    Ok(())
}

/// El payload canónico mide longitudes en bytes UTF-8 (`octet_length`); la
/// base de datos debe usar UTF8 para que Rust y PostgreSQL coincidan.
async fn verificar_codificacion(pool: &PgPool) -> AppResult<()> {
    let codificacion: String = sqlx::query_scalar("SHOW server_encoding").fetch_one(pool).await?;
    if codificacion != "UTF8" {
        return Err(anyhow::anyhow!("la base de datos debe usar codificación UTF8 (usa {codificacion})").into());
    }
    Ok(())
}

pub async fn asegurar_infraestructura(pool: &PgPool, cfg: &Config) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO ifpe.reserva_custodia (institucion, tipo, numero_cuenta) \
         SELECT $1, 'BANXICO', $2 WHERE NOT EXISTS (SELECT 1 FROM ifpe.reserva_custodia WHERE activa)",
    )
    .bind(&cfg.reserva_institucion)
    .bind(&cfg.reserva_cuenta)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO sofom.caja (institucion, numero_cuenta) \
         SELECT $1, $2 WHERE NOT EXISTS (SELECT 1 FROM sofom.caja WHERE activa)",
    )
    .bind(&cfg.caja_institucion)
    .bind(&cfg.caja_cuenta)
    .execute(pool)
    .await?;
    // El proveedor simulado solo se admite en demostración (0007).
    sqlx::query("UPDATE kyc.parametros SET permite_proveedor_simulado = $1 WHERE id")
        .bind(cfg.modo_demo)
        .execute(pool)
        .await?;
    // Umbrales del KYC biométrico: viven en la base de datos (la que los hace
    // cumplir); las variables de entorno solo los fijan al arrancar.
    let k = &cfg.kyc;
    if k.score_minimo.is_some() || k.score_revision.is_some() || k.max_intentos.is_some() {
        sqlx::query(
            "UPDATE kyc.parametros SET umbral_score_minimo = coalesce($1, umbral_score_minimo), \
                    umbral_revision = coalesce($2, umbral_revision), max_intentos = coalesce($3, max_intentos) \
             WHERE id",
        )
        .bind(k.score_minimo)
        .bind(k.score_revision)
        .bind(k.max_intentos)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Crea (o deja intacto) un operador. Solo el dueño del esquema puede asignar
/// `rol = 'OPERADOR'`: `sc_kyc` no tiene privilegio sobre esa columna.
pub async fn asegurar_operador(pool: &PgPool, email: &str, pw: &str) -> AppResult<()> {
    asegurar_usuario_interno(pool, email, pw, RolUsuario::Operador).await
}

/// Crea (o deja intacta) una persona interna con el rol indicado (operador,
/// auditor u oficial de cumplimiento). Mismas reglas que [`asegurar_operador`]:
/// solo el dueño del esquema fija `kyc.credenciales.rol`.
///
/// El nombre visible es el rol y la parte local del email ("Cumplimiento
/// oficial1"): en la consola, el solicitante, el proponente y el aprobador de
/// una decisión de cuatro ojos deben distinguirse (un auditor verifica la
/// segregación de funciones por esos nombres).
pub async fn asegurar_usuario_interno(pool: &PgPool, email: &str, pw: &str, rol: RolUsuario) -> AppResult<()> {
    password::validar_politica(pw)?;
    let email = email.trim().to_lowercase();
    let existe: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kyc.clientes WHERE lower(email) = $1)")
        .bind(&email)
        .fetch_one(pool)
        .await?;
    if existe {
        return Ok(());
    }
    let nombre = match rol {
        RolUsuario::Operador => "Operador",
        RolUsuario::Auditor => "Auditoría",
        RolUsuario::Cumplimiento => "Cumplimiento",
        RolUsuario::Cliente => {
            return Err(anyhow::anyhow!("asegurar_usuario_interno no da de alta clientes").into());
        }
    };
    let apellido = nombre_visible(&email);
    let hash = password::hashear(pw.to_owned()).await?;
    let mut tx = db::begin_administrativa(pool).await?;
    let id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO kyc.clientes (tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) \
         VALUES ('FISICA', $2, $3, $1, 'VERIFICADO', now(), 'BAJO') RETURNING id",
    )
    .bind(&email)
    .bind(nombre)
    .bind(&apellido)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(&hash)
        .bind(rol)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(%email, ?rol, "usuario interno creado");
    Ok(())
}

/// Parte local del email (hasta 60 caracteres), que distingue a cada persona
/// interna en la consola; "Synthetic Capital" si el email no la tiene.
fn nombre_visible(email: &str) -> String {
    let local: String = email.split('@').next().unwrap_or_default().trim().chars().take(60).collect();
    if local.is_empty() {
        "Synthetic Capital".to_owned()
    } else {
        local
    }
}

#[cfg(test)]
mod pruebas {
    use super::nombre_visible;

    #[test]
    fn nombre_visible_distingue_por_email() {
        assert_eq!(nombre_visible("oficial1@cumplimiento.mx"), "oficial1");
        assert_ne!(nombre_visible("oficial1@cumplimiento.mx"), nombre_visible("oficial2@cumplimiento.mx"));
        assert_eq!(nombre_visible("@x.mx"), "Synthetic Capital");
        assert_eq!(nombre_visible(&format!("{}@x.mx", "a".repeat(80))).chars().count(), 60);
    }
}
