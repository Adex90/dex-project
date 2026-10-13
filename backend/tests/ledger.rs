//! Motor de la IFPE: depósitos, retiros, transferencias, idempotencia,
//! concurrencia y cadena de hashes.

mod comun;

use std::sync::Arc;

use comun::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    ledger::{self, hash},
    models::ifpe::TipoTxIfpe,
    AppError,
};

#[sqlx::test(migrations = "../migrations")]
async fn deposito_transferencia_y_retiro(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;

    let dep = depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    assert_eq!(dep.transaccion.tipo, TipoTxIfpe::Deposito);
    assert_eq!(dep.transaccion.secuencia, 1);
    assert_eq!(dep.transaccion.hash_anterior, hash::GENESIS);

    let t = ledger::transferir(
        &pool,
        &cfg.pld,
        &ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: beto.cuenta_ifpe.clabe.clone(),
            monto: d("250.5"),
            concepto: Some("Renta | octubre ñ".into()),
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();
    assert_eq!(t.transaccion.hash_anterior, dep.transaccion.tx_hash);

    ledger::retirar(
        &pool,
        &cfg.pld,
        &ledger::Retiro {
            cliente_id: beto.cliente.id,
            monto: d("50"),
            clabe_destino: "646180157000000017".into(),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();

    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("749.5"));
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("200.5"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn sobregiro_rechazado_sin_efectos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "100").await;

    let err = ledger::transferir(
        &pool,
        &cfg.pld,
        &ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: beto.cuenta_ifpe.clabe.clone(),
            monto: d("100.00000001"),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.codigo(), "SALDO_INSUFICIENTE");
    let err = ledger::retirar(
        &pool,
        &cfg.pld,
        &ledger::Retiro {
            cliente_id: ana.cliente.id,
            monto: d("101"),
            clabe_destino: "646180157000000017".into(),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.codigo(), "SALDO_INSUFICIENTE");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("100"));
    let movimientos: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ifpe.transacciones").fetch_one(&pool).await.unwrap();
    assert_eq!(movimientos, 1, "los intentos rechazados no dejan rastro en el ledger");
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn validaciones_de_entrada(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "100").await;
    let transferir = |clabe: &str, monto: &str| {
        let t = ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: clabe.into(),
            monto: d(monto),
            concepto: None,
            clave_idempotencia: clave(),
        };
        let pool = pool.clone();
        let pld = cfg.pld.clone();
        async move { ledger::transferir(&pool, &pld, &t).await }
    };
    assert!(matches!(transferir(&ana.cuenta_ifpe.clabe, "1").await, Err(AppError::Validacion(_))), "a sí mismo");
    assert!(matches!(transferir("646180157000000018", "1").await, Err(AppError::Validacion(_))), "CLABE inválida");
    assert!(matches!(transferir("646180157000000017", "1").await, Err(AppError::NoEncontrado(_))), "CLABE ajena");
    assert!(matches!(transferir(&ana.cuenta_ifpe.clabe, "0").await, Err(AppError::Validacion(_))));
    assert!(matches!(transferir(&ana.cuenta_ifpe.clabe, "0.000000001").await, Err(AppError::Validacion(_))));
}

#[sqlx::test(migrations = "../migrations")]
async fn idempotencia(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    let t = ledger::Transferencia {
        cliente_origen_id: ana.cliente.id,
        clabe_destino: beto.cuenta_ifpe.clabe.clone(),
        monto: d("100"),
        concepto: None,
        clave_idempotencia: "cli:ana:op-1".into(),
    };
    let primera = ledger::transferir(&pool, &cfg.pld, &t).await.unwrap();
    let segunda = ledger::transferir(&pool, &cfg.pld, &t).await.unwrap();
    assert!(!primera.idempotente);
    assert!(segunda.idempotente);
    assert_eq!(primera.transaccion.id, segunda.transaccion.id);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"));

    // Misma clave, operación distinta: conflicto, sin efectos.
    let mut otra = t.clone();
    otra.monto = d("200");
    let err = ledger::transferir(&pool, &cfg.pld, &otra).await.unwrap_err();
    assert_eq!(err.codigo(), "CLAVE_IDEMPOTENCIA_REUTILIZADA");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"));
    verificar_invariantes(&pool).await;
}

/// 20 transferencias concurrentes de 100 desde una cuenta con 1000:
/// exactamente 10 deben pasar y el saldo nunca es negativo.
#[sqlx::test(migrations = "../migrations")]
async fn concurrencia_no_permite_gastar_dos_veces(pool: PgPool) {
    let cfg = Arc::new(preparar(&pool).await);
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    let mut tareas = Vec::new();
    for _ in 0..20 {
        let (pool, cfg) = (pool.clone(), cfg.clone());
        let t = ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: beto.cuenta_ifpe.clabe.clone(),
            monto: d("100"),
            concepto: None,
            clave_idempotencia: clave(),
        };
        tareas.push(tokio::spawn(async move { ledger::transferir(&pool, &cfg.pld, &t).await }));
    }
    let mut exitos = 0;
    for t in tareas {
        match t.await.unwrap() {
            Ok(_) => exitos += 1,
            Err(e) => assert_eq!(e.codigo(), "SALDO_INSUFICIENTE", "{e:?}"),
        }
    }
    assert_eq!(exitos, 10);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, Decimal::ZERO);
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("1000"));
    verificar_invariantes(&pool).await;
}

/// Transferencias cruzadas A<->B concurrentes: el orden global de bloqueo evita deadlocks.
#[sqlx::test(migrations = "../migrations")]
async fn concurrencia_cruzada_sin_deadlocks(pool: PgPool) {
    let cfg = Arc::new(preparar(&pool).await);
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "1000").await;

    let mut tareas = Vec::new();
    for i in 0..40 {
        let (pool, cfg) = (pool.clone(), cfg.clone());
        let (origen, destino) = if i % 2 == 0 { (&ana, &beto) } else { (&beto, &ana) };
        let t = ledger::Transferencia {
            cliente_origen_id: origen.cliente.id,
            clabe_destino: destino.cuenta_ifpe.clabe.clone(),
            monto: d("10"),
            concepto: None,
            clave_idempotencia: clave(),
        };
        // Depósitos intercalados también compiten por la reserva y estado_reserva.
        let deposito_cuenta = destino.cuenta_ifpe.id;
        tareas.push(tokio::spawn(async move {
            ledger::transferir(&pool, &cfg.pld, &t).await?;
            ledger::depositar(
                &pool,
                &cfg.pld,
                &ledger::Deposito {
                    cuenta_id: deposito_cuenta,
                    monto: d("1"),
                    referencia_externa: None,
                    concepto: None,
                    clave_idempotencia: clave(),
                },
            )
            .await
        }));
    }
    for t in tareas {
        t.await.unwrap().unwrap();
    }
    let total = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0 + saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0;
    assert_eq!(total, d("2040"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn retencion_y_liberacion(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "500").await;
    let retener = |monto: &str, liberar: bool| ledger::Retencion {
        cuenta_id: ana.cuenta_ifpe.id,
        monto: d(monto),
        motivo: "Requerimiento de autoridad".into(),
        liberar,
        clave_idempotencia: clave(),
    };
    ledger::retener(&pool, &retener("200", false)).await.unwrap();
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("300"), d("200")));
    assert_eq!(ledger::retener(&pool, &retener("201", true)).await.unwrap_err().codigo(), "SALDO_INSUFICIENTE");
    ledger::retener(&pool, &retener("200", true)).await.unwrap();
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("500"), d("0")));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn kyc_pendiente_no_opera(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    sqlx::query("UPDATE kyc.clientes SET estatus_kyc = 'PENDIENTE' WHERE id = $1")
        .bind(ana.cliente.id)
        .execute(&pool)
        .await
        .unwrap();
    let err = ledger::depositar(
        &pool,
        &cfg.pld,
        &ledger::Deposito {
            cuenta_id: ana.cuenta_ifpe.id,
            monto: d("10"),
            referencia_externa: None,
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.codigo(), "KYC_NO_VERIFICADO");
}

#[sqlx::test(migrations = "../migrations")]
async fn alertas_pld_relevante_y_fraccionamiento(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "150000").await; // R01
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "80000").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "80000").await; // R02: 160,000 en 24 h
    let reglas: Vec<String> =
        sqlx::query_scalar("SELECT regla FROM kyc.alertas_pld ORDER BY folio").fetch_all(&pool).await.unwrap();
    assert_eq!(reglas, ["R01_OPERACION_RELEVANTE", "R02_FRACCIONAMIENTO"]);
}

/// Si alguien altera un movimiento saltándose los triggers (p. ej. con acceso
/// de superusuario al disco), el verificador lo detecta.
#[sqlx::test(migrations = "../migrations")]
async fn verificador_detecta_manipulacion(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "100").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "200").await;
    assert!(hash::verificar_cadena_ifpe(&pool).await.unwrap().valido);

    // El usuario de pruebas es dueño de la tabla: desactiva el trigger para simular el ataque.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("ALTER TABLE ifpe.transacciones DISABLE TRIGGER trg_ifpe_tx_inmutable")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE ifpe.transacciones SET monto = 1000 WHERE secuencia = 1").execute(&mut *conn).await.unwrap();
    let r = hash::verificar_cadena_ifpe(&pool).await.unwrap();
    assert!(!r.valido);
    assert_eq!(r.falla.unwrap().secuencia, 1);
}

/// Hallazgo de la revisión: el mismo SPEI (clave de rastreo) no puede
/// acreditarse dos veces, lo registre quien lo registre. Un doble registro
/// inflaría pasivo y reserva por igual sin que el efectivo exista.
#[sqlx::test(migrations = "../migrations")]
async fn spei_no_se_acredita_dos_veces(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let spei = ledger::Deposito::spei(ana.cuenta_ifpe.id, d("50000"), "ABC123", None).unwrap();
    let primero = ledger::depositar(&pool, &cfg.pld, &spei).await.unwrap();
    let repetido = ledger::depositar(&pool, &cfg.pld, &spei).await.unwrap();
    assert!(repetido.idempotente);
    assert_eq!(primero.transaccion.id, repetido.transaccion.id);

    // Mismo SPEI por otra vía (otra clave de idempotencia): lo frena el índice único.
    let mut otra_via = spei.clone();
    otra_via.clave_idempotencia = "otra-clave-cualquiera".into();
    let e = ledger::depositar(&pool, &cfg.pld, &otra_via).await.unwrap_err();
    assert_eq!(e.codigo(), "SPEI_DUPLICADO");

    // Misma clave con otro SPEI: conflicto, no se descarta en silencio.
    let mut otro_spei = spei.clone();
    otro_spei.referencia_externa = Some("XYZ999".into());
    let e = ledger::depositar(&pool, &cfg.pld, &otro_spei).await.unwrap_err();
    assert_eq!(e.codigo(), "CLAVE_IDEMPOTENCIA_REUTILIZADA");

    assert!(ledger::Deposito::spei(ana.cuenta_ifpe.id, d("1"), "con espacios", None).is_err());
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("50000"));
    verificar_invariantes(&pool).await;
}
