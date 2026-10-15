//! Migraciones sobre bases de datos con datos heredados.

use sqlx::PgPool;

const M0001: &str = include_str!("../../migrations/0001_initial_schema.sql");
const M0002: &str = include_str!("../../migrations/0002_kyc_capital_y_cortes.sql");
const M0003: &str = include_str!("../../migrations/0003_endurecimiento.sql");
const M0004: &str = include_str!("../../migrations/0004_saldos_ligados_al_ledger.sql");
const M0005: &str = include_str!("../../migrations/0005_spei_integration.sql");
const M0006: &str = include_str!("../../migrations/0006_kyc_biometricos.sql");
const M0007: &str = include_str!("../../migrations/0007_spei_endurecimiento.sql");

/// Depósito como lo registraba el código anterior a 0004: clave
/// `spei:<rastreo>` y la clave de rastreo tal cual en `referencia_externa`.
async fn deposito_heredado(pool: &PgPool, rastreo: &str) {
    deposito_heredado_con(pool, &format!("spei:{rastreo}"), rastreo).await;
}

async fn deposito_heredado_con(pool: &PgPool, clave: &str, referencia: &str) {
    let mut tx = pool.begin().await.unwrap();
    for sql in [
        "UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'",
        "UPDATE ifpe.reserva_custodia SET saldo = saldo + 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    sqlx::query(
        "INSERT INTO ifpe.transacciones (tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa) \
         VALUES ('DEPOSITO', '1a000000-0000-0000-0000-00000000001a', 'e0000000-0000-0000-0000-00000000000e', 100, $1, $2)",
    )
    .bind(clave)
    .bind(referencia)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// Hallazgo #17: una migración no debe fallar por datos que el código anterior
/// permitía. Con 0003, la misma clave de rastreo en mayúsculas y minúsculas
/// se acreditaba dos veces (hallazgo #8); 0004 debe aplicarse igual sobre esa
/// base y proteger los depósitos nuevos.
#[sqlx::test(migrations = false)]
async fn migracion_0004_tolera_duplicados_heredados(pool: PgPool) {
    for m in [M0001, M0002, M0003] {
        sqlx::raw_sql(m).execute(&pool).await.unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) \
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'VERIFICADO', now(), 'BAJO'); \
         INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) \
           VALUES ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001'); \
         INSERT INTO ifpe.cuentas (id, cliente_id, clabe) \
           VALUES ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017');",
    )
    .execute(&pool)
    .await
    .unwrap();
    deposito_heredado(&pool, "rastreo123").await;
    deposito_heredado(&pool, "RASTREO123").await;

    sqlx::raw_sql(M0004).execute(&pool).await.expect("0004 debe aplicarse sobre datos heredados");

    // Las sombras reflejan el ledger heredado completo.
    let sombra: rust_decimal::Decimal = sqlx::query_scalar(
        "SELECT disponible FROM ifpe.saldos_ledger WHERE cuenta_id = '1a000000-0000-0000-0000-00000000001a'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(sombra, rust_decimal::Decimal::from(200));
    // Un SPEI nuevo (identidad canónica) no puede acreditarse dos veces.
    let canonico = |clave: &'static str| {
        let pool = pool.clone();
        async move {
            let mut tx = pool.begin().await.unwrap();
            sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'")
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo + 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'")
                .execute(&mut *tx)
                .await
                .unwrap();
            let r = sqlx::query(
                "INSERT INTO ifpe.transacciones (tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa) \
                 VALUES ('DEPOSITO', '1a000000-0000-0000-0000-00000000001a', 'e0000000-0000-0000-0000-00000000000e', 100, $1, \
                         '40012:20261001:RASTREO123')",
            )
            .bind(clave)
            .execute(&mut *tx)
            .await;
            match r {
                Ok(_) => tx.commit().await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
    };
    canonico("spei:40012:20261001:RASTREO123").await.unwrap();
    let e = canonico("spei:otra-clave-para-el-mismo-pago").await.unwrap_err();
    assert!(e.contains("ux_ifpe_tx_deposito_spei"), "{e}");
}

/// inbound-security-3: 0005-0007 se aplican sobre abonos heredados y 0007
/// registra con origen LEGADO los de identidad canónica (una nueva
/// notificación del mismo pago es una repetición, no un segundo abono). Los
/// de clave de rastreo sola quedan como historia (el backend los trata como
/// posibles duplicados).
#[sqlx::test(migrations = false)]
async fn migracion_0007_registra_abonos_anteriores(pool: PgPool) {
    for m in [M0001, M0002, M0003] {
        sqlx::raw_sql(m).execute(&pool).await.unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) \
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'VERIFICADO', now(), 'BAJO'); \
         INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) \
           VALUES ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001'); \
         INSERT INTO ifpe.cuentas (id, cliente_id, clabe) \
           VALUES ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017');",
    )
    .execute(&pool)
    .await
    .unwrap();
    deposito_heredado(&pool, "rastreo123").await;
    sqlx::raw_sql(M0004).execute(&pool).await.unwrap();
    deposito_heredado_con(&pool, "spei:40012:20261001:LEGADO1", "40012:20261001:LEGADO1").await;
    for m in [M0005, M0006, M0007] {
        sqlx::raw_sql(m).execute(&pool).await.expect("0005-0007 deben aplicarse sobre datos heredados");
    }
    let legados: Vec<(String, String, String, rust_decimal::Decimal)> = sqlx::query_as(
        "SELECT institucion_emisora, clave_rastreo, origen, monto FROM ifpe.spei_inbound ORDER BY clave_rastreo",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        legados,
        vec![("40012".into(), "LEGADO1".into(), "LEGADO".into(), rust_decimal::Decimal::from(100))],
        "solo el abono de identidad canónica"
    );
    // Ligado a su DEPOSITO, y la función es idempotente.
    let ligado: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ifpe.spei_inbound s JOIN ifpe.transacciones t ON t.id = s.transaccion_id \
          WHERE t.clave_idempotencia = 'spei:40012:20261001:LEGADO1')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(ligado);
    let otra_vez: i32 = sqlx::query_scalar("SELECT ifpe.registrar_abonos_legados()").fetch_one(&pool).await.unwrap();
    assert_eq!(otra_vez, 0);
}
