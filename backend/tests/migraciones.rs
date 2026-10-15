//! Migraciones sobre bases de datos con datos heredados.

use sqlx::PgPool;

const M0001: &str = include_str!("../../migrations/0001_initial_schema.sql");
const M0002: &str = include_str!("../../migrations/0002_kyc_capital_y_cortes.sql");
const M0003: &str = include_str!("../../migrations/0003_endurecimiento.sql");
const M0004: &str = include_str!("../../migrations/0004_saldos_ligados_al_ledger.sql");

/// Depósito como lo registraba el código anterior a 0004: clave
/// `spei:<rastreo>` y la clave de rastreo tal cual en `referencia_externa`.
async fn deposito_heredado(pool: &PgPool, rastreo: &str) {
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
    .bind(format!("spei:{rastreo}"))
    .bind(rastreo)
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
