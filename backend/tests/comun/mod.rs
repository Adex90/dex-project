//! Utilidades compartidas por las pruebas de integración.
//!
//! Cada prueba `#[sqlx::test]` recibe una base de datos nueva con todas las
//! migraciones aplicadas (requiere `DATABASE_URL` con un usuario que tenga
//! CREATEDB y CREATEROLE).

#![allow(dead_code)]

use std::str::FromStr;

use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    config::Config,
    kyc::{self, DatosRegistro, Registro},
    ledger::{self, hash, Movimiento},
    models::kyc::TipoPersona,
};
use uuid::Uuid;

pub fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

pub async fn preparar(pool: &PgPool) -> Config {
    let cfg = Config::para_pruebas("");
    bootstrap::asegurar_infraestructura(pool, &cfg).await.unwrap();
    cfg
}

/// Cliente persona física con KYC verificado (modo demo) y cuenta IFPE.
pub async fn cliente(pool: &PgPool, cfg: &Config, nombre: &str) -> Registro {
    kyc::registrar(
        pool,
        &cfg.clabe_prefijo,
        true,
        DatosRegistro {
            tipo_persona: TipoPersona::Fisica,
            nombre: nombre.into(),
            apellido_paterno: Some("Prueba".into()),
            apellido_materno: None,
            email: format!("{}@example.mx", nombre.to_lowercase()),
            password: "contraseña-de-prueba-123".into(),
            curp: None,
            rfc: None,
            telefono: None,
            actividad_economica: None,
            ingreso_mensual_estimado: None,
        },
    )
    .await
    .unwrap()
}

/// SPEI entrante con una clave de rastreo única.
pub async fn depositar(pool: &PgPool, cfg: &Config, cuenta_id: Uuid, monto: &str) -> Movimiento {
    let spei = ledger::IdentidadSpei {
        banco_emisor: "40012".into(),
        fecha_operacion: chrono::Utc::now().date_naive(),
        clave_rastreo: format!("PRUEBA{}", &Uuid::new_v4().simple().to_string()[..20]),
    };
    ledger::depositar(pool, &cfg.pld, &ledger::Deposito::spei(cuenta_id, d(monto), &spei, None).unwrap()).await.unwrap()
}

/// Simula el paso del tiempo para un mutuo: recorre `fecha_inicio` hacia
/// atrás. Solo el dueño del esquema puede hacerlo, desactivando el trigger
/// que protege los términos (la aplicación nunca puede).
pub async fn envejecer_mutuo(pool: &PgPool, contrato_id: Uuid, dias: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE sofom.contratos_mutuo DISABLE TRIGGER trg_mutuo_proteger")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE sofom.contratos_mutuo SET fecha_inicio = fecha_inicio - $2 WHERE id = $1")
        .bind(contrato_id)
        .bind(dias)
        .execute(&mut *tx)
        .await
        .unwrap();
    // Dispara ya los triggers diferidos: ALTER TABLE no admite eventos pendientes.
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE sofom.contratos_mutuo ENABLE TRIGGER trg_mutuo_proteger").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

/// Igual que [`envejecer_mutuo`], para la fecha de desembolso de un crédito.
pub async fn envejecer_credito(pool: &PgPool, credito_id: Uuid, dias: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE sofom.creditos DISABLE TRIGGER trg_credito_proteger").execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE sofom.creditos SET fecha_desembolso = fecha_desembolso - $2 WHERE id = $1")
        .bind(credito_id)
        .bind(dias)
        .execute(&mut *tx)
        .await
        .unwrap();
    // Dispara ya los triggers diferidos: ALTER TABLE no admite eventos pendientes.
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE sofom.creditos ENABLE TRIGGER trg_credito_proteger").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

pub fn clave() -> String {
    format!("prueba:{}", Uuid::new_v4())
}

pub async fn saldo_ifpe(pool: &PgPool, cuenta_id: Uuid) -> (Decimal, Decimal) {
    sqlx::query_as("SELECT saldo_disponible, saldo_bloqueado FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Invariantes globales que deben cumplirse después de cualquier secuencia de operaciones.
pub async fn verificar_invariantes(pool: &PgPool) {
    // Regla 1: reserva del 100%, por tres caminos independientes.
    let (pasivo, activo): (Decimal, Decimal) =
        sqlx::query_as("SELECT pasivo_clientes, activo_reserva FROM ifpe.estado_reserva")
            .fetch_one(pool)
            .await
            .unwrap();
    let suma_cuentas: Decimal =
        sqlx::query_scalar("SELECT coalesce(sum(saldo_disponible + saldo_bloqueado), 0) FROM ifpe.cuentas")
            .fetch_one(pool)
            .await
            .unwrap();
    let suma_custodia: Decimal =
        sqlx::query_scalar("SELECT coalesce(sum(saldo), 0) FROM ifpe.reserva_custodia").fetch_one(pool).await.unwrap();
    assert_eq!(pasivo, activo, "pasivo IFPE != reserva");
    assert_eq!(pasivo, suma_cuentas, "estado_reserva.pasivo no cuadra con las cuentas");
    assert_eq!(activo, suma_custodia, "estado_reserva.activo no cuadra con la custodia");

    // Los saldos de cada cuenta IFPE se pueden reconstruir solo con el ledger.
    let descuadres: i64 = sqlx::query_scalar(
        "WITH efectos AS ( \
           SELECT cuenta_destino_id AS cuenta, \
                  CASE WHEN tipo IN ('RETENCION','LIBERACION_RETENCION') THEN 0 ELSE monto END AS delta \
             FROM ifpe.transacciones WHERE cuenta_destino_id IS NOT NULL \
           UNION ALL \
           SELECT cuenta_origen_id, \
                  CASE WHEN tipo IN ('RETENCION','LIBERACION_RETENCION') THEN 0 ELSE -monto END \
             FROM ifpe.transacciones WHERE cuenta_origen_id IS NOT NULL) \
         SELECT count(*) FROM ifpe.cuentas c \
          LEFT JOIN (SELECT cuenta, sum(delta) AS total FROM efectos GROUP BY cuenta) e ON e.cuenta = c.id \
         WHERE coalesce(e.total, 0) <> c.saldo_disponible + c.saldo_bloqueado",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(descuadres, 0, "hay cuentas IFPE cuyo saldo no se reconstruye desde el ledger");

    // Regla 2: el saldo bloqueado SOFOM es exactamente el principal de los mutuos vigentes,
    // y ningún mutuo está sobreasignado.
    let (bloqueado, vigente): (Decimal, Decimal) = sqlx::query_as(
        "SELECT (SELECT coalesce(sum(saldo_bloqueado), 0) FROM sofom.cuentas), \
                (SELECT coalesce(sum(principal), 0) FROM sofom.contratos_mutuo WHERE estado = 'VIGENTE')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(bloqueado, vigente, "saldo bloqueado SOFOM != principal de mutuos vigentes");
    let mal_calzados: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sofom.asignaciones_fondeo a \
         JOIN sofom.creditos c ON c.id = a.credito_id JOIN sofom.contratos_mutuo m ON m.id = a.contrato_id \
         WHERE m.fecha_vencimiento < c.fecha_vencimiento",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(mal_calzados, 0, "hay créditos fondeados con mutuos de menor plazo");

    // Cadenas de hash íntegras.
    let ifpe = hash::verificar_cadena_ifpe(pool).await.unwrap();
    assert!(ifpe.valido, "cadena IFPE inválida: {ifpe:?}");
    let sofom = hash::verificar_cadena_sofom(pool).await.unwrap();
    assert!(sofom.valido, "cadena SOFOM inválida: {sofom:?}");
}

/// Crea `n` cuentas IFPE vacías directamente (como dueño del esquema), sin
/// el costo de Argon2 del alta normal. Para pruebas de carga.
pub async fn cuentas_vacias(pool: &PgPool, cfg: &Config, n: usize) {
    for i in 0..n {
        let cliente: Uuid = sqlx::query_scalar(
            "INSERT INTO kyc.clientes (tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) \
             VALUES ('FISICA', 'Carga', 'Prueba', $1, 'VERIFICADO', now(), 'BAJO') RETURNING id",
        )
        .bind(format!("carga-{i}-{}@example.mx", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO ifpe.cuentas (cliente_id, clabe) VALUES ($1, $2)")
            .bind(cliente)
            .bind(synthetic_capital::clabe::generar(&cfg.clabe_prefijo))
            .execute(pool)
            .await
            .unwrap();
    }
}
