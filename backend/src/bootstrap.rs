//! Tareas administrativas de arranque, idempotentes.
//!
//! Se ejecutan con el usuario dueño del esquema (sin `SET ROLE`) porque crean
//! infraestructura que ningún rol transaccional puede crear: cuentas de
//! reserva en custodia, caja de la SOFOM, el operador inicial y (solo en
//! MODO_DEMO) los oficiales de cumplimiento. Ninguna mueve saldos: las cuentas
//! nacen en cero. `synthetic-capital migrar` además aprovisiona el login de la
//! aplicación ([`asegurar_login_aplicacion`]).

use anyhow::{bail, Context};
use sqlx::PgPool;

use crate::{
    auth::password,
    config::{Config, LoginAplicacion},
    crypto::cifrado::Cifrador,
    crypto::scram,
    db::{self, Rol},
    error::AppResult,
    models::kyc::RolUsuario,
};

/// Candado consultivo que serializa dos `migrar` simultáneos sobre la misma
/// base de datos mientras aprovisionan el login.
const CANDADO_LOGIN: i64 = 0x5343_5F41_5050_4C47;

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
    // Rotación de la clave de los secretos TOTP: con las dos claves, todo queda
    // cifrado con la vigente y la anterior puede retirarse.
    if let (Some(clave), Some(anterior)) = (&cfg.mfa.clave, &cfg.mfa.clave_anterior) {
        let n = crate::auth::mfa::recifrar_secretos(pool, &Cifrador::nuevo(clave, Some(anterior))).await?;
        tracing::info!(
            recifrados = n,
            "secretos TOTP cifrados con la clave vigente; MFA_CLAVE_CIFRADO_ANTERIOR ya puede retirarse"
        );
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

/// Crea o actualiza el login con el que corre el servicio: `LOGIN NOINHERIT`,
/// con la contraseña como verificador SCRAM-SHA-256 calculado aquí (el texto
/// en claro nunca llega al servidor ni a sus bitácoras) y miembro de cada rol
/// de [`Rol::TODOS`] sin heredar sus privilegios: solo actúa con
/// `SET LOCAL ROLE` (ver [`crate::db`]). Idempotente: repetirlo rota la
/// contraseña y repara las concesiones.
///
/// Falla cerrado, revirtiendo todo, si el login resultante es superusuario,
/// tiene CREATEROLE, CREATEDB, REPLICATION o BYPASSRLS, hereda privilegios de
/// un rol `sc_*` o es miembro del dueño del esquema: con cualquiera de ellos
/// el servicio podría saltarse el aislamiento por rol.
///
/// Las sentencias no fijan NOSUPERUSER/NOREPLICATION/NOBYPASSRLS: un dueño sin
/// superusuario (el usuario maestro de RDS) no puede mencionarlas siquiera.
pub async fn asegurar_login_aplicacion(pool: &PgPool, login: &LoginAplicacion) -> anyhow::Result<()> {
    let usuario = login.usuario();
    let verificador = scram::verificador(login.password());
    let mut tx = db::begin_administrativa(pool).await?;
    // Repetir una concesión existente avisa con un NOTICE por rol.
    sqlx::raw_sql("SET LOCAL client_min_messages = warning").execute(&mut *tx).await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(CANDADO_LOGIN).execute(&mut *tx).await?;
    let (propietario, version, existe): (String, i32, bool) = sqlx::query_as(
        "SELECT current_user::text, current_setting('server_version_num')::int, \
                EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)",
    )
    .bind(usuario)
    .fetch_one(&mut *tx)
    .await?;
    if usuario == propietario {
        bail!("DB_APP_USUARIO ({usuario}) es el usuario con que corre migrar; el servicio debe usar otro login");
    }
    // Las sentencias de roles no admiten parámetros. `usuario` es un
    // identificador validado y el verificador solo contiene [A-Za-z0-9+/=$:-]:
    // ninguno puede cerrar las comillas. `raw_sql` usa el protocolo simple, así
    // el texto tampoco queda en la caché de sentencias preparadas.
    let accion = if existe { "ALTER" } else { "CREATE" };
    sqlx::raw_sql(&format!("{accion} ROLE \"{usuario}\" LOGIN NOINHERIT PASSWORD '{verificador}'"))
        .execute(&mut *tx)
        .await
        .with_context(|| format!("{accion} ROLE {usuario}"))?;
    // Desde PostgreSQL 16 la herencia es de cada concesión (NOINHERIT solo fija
    // la opción por omisión): se fija explícitamente, lo que también corrige
    // concesiones anteriores del mismo otorgante.
    let roles = Rol::TODOS.map(Rol::nombre).join(", ");
    let opciones = if version >= 160_000 { " WITH INHERIT FALSE, SET TRUE" } else { "" };
    sqlx::raw_sql(&format!("GRANT {roles} TO \"{usuario}\"{opciones}"))
        .execute(&mut *tx)
        .await
        .with_context(|| format!("GRANT {roles} TO {usuario}"))?;
    verificar_login_aplicacion(&mut tx, usuario, version).await?;
    tx.commit().await?;
    tracing::info!(%usuario, creado = !existe, "login de la aplicación listo: NOINHERIT y miembro de {roles}");
    Ok(())
}

async fn verificar_login_aplicacion(tx: &mut db::Tx, usuario: &str, version: i32) -> anyhow::Result<()> {
    let roles = Rol::TODOS.map(Rol::nombre);
    // Antes de PostgreSQL 16, ser miembro basta para SET ROLE; desde 16 es una
    // opción de cada concesión.
    let cambiar = if version >= 160_000 { "SET" } else { "MEMBER" };
    let (privilegiado, inherit, puede_entrar, puede_cambiar, hereda, del_dueno): (bool, bool, bool, bool, bool, bool) =
        sqlx::query_as(
            "SELECT r.rolsuper OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication OR r.rolbypassrls, \
                    r.rolinherit, r.rolcanlogin, \
                    (SELECT bool_and(pg_has_role(r.oid, g, $3)) FROM unnest($2::name[]) g), \
                    (SELECT bool_or(pg_has_role(r.oid, g, 'USAGE')) FROM unnest($2::name[]) g), \
                    EXISTS (SELECT 1 FROM pg_namespace n \
                             WHERE n.nspname IN ('nucleo', 'kyc', 'ifpe', 'sofom', 'aml') \
                               AND pg_has_role(r.oid, n.nspowner, 'MEMBER')) \
               FROM pg_roles r WHERE r.rolname = $1",
        )
        .bind(usuario)
        .bind(&roles[..])
        .bind(cambiar)
        .fetch_one(&mut **tx)
        .await?;
    if privilegiado {
        bail!(
            "el login {usuario} tiene SUPERUSER, CREATEROLE, CREATEDB, REPLICATION o BYPASSRLS; el servicio debe \
             correr sin ellos: quítalos (ALTER ROLE) y vuelve a ejecutar migrar"
        );
    }
    if inherit || !puede_entrar {
        bail!("el login {usuario} no quedó LOGIN NOINHERIT");
    }
    if !puede_cambiar {
        bail!("el login {usuario} no puede cambiar (SET ROLE) a todos los roles {}", roles.join(", "));
    }
    if hereda {
        bail!(
            "el login {usuario} hereda privilegios de un rol sc_* (una concesión WITH INHERIT TRUE de otro \
             otorgante); revócala: debe actuar solo con SET LOCAL ROLE"
        );
    }
    if del_dueno {
        bail!("el login {usuario} es miembro del dueño del esquema: podría desactivar triggers");
    }
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
