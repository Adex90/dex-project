//! Proof of Reserve: cortes y pruebas de inclusión verificables.

mod comun;

use comun::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    crypto::merkle::{self, Hoja},
    dinero,
    reserva::{self, SolicitudVerificacion},
};

#[sqlx::test(migrations = "../migrations")]
async fn corte_y_pruebas_para_todos_los_clientes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let mut clientes = Vec::new();
    for (i, nombre) in ["Ana", "Beto", "Carla", "Dario", "Elena"].iter().enumerate() {
        let c = cliente(&pool, &cfg, nombre).await;
        depositar(&pool, &cfg, c.cuenta_ifpe.id, &format!("{}.25", (i + 1) * 1000)).await;
        clientes.push(c);
    }
    let corte = reserva::generar_corte(&pool).await.unwrap();
    assert_eq!(corte.num_hojas, 5);
    assert_eq!(corte.total_pasivo, d("15001.25"));
    assert_eq!(corte.total_pasivo, corte.total_reserva);

    for c in &clientes {
        let p = reserva::prueba_de_cliente(&pool, c.cliente.id, 0).await.unwrap();
        assert!(p.valida);
        assert_eq!(p.corte.id, corte.id);
        assert_eq!(p.hoja.cuenta_id, c.cuenta_ifpe.id);
        assert_eq!(p.hoja.saldo, p.saldo_actual);

        // Verificación independiente, solo con la prueba y la raíz publicada.
        let hoja = Hoja {
            cuenta_id: p.hoja.cuenta_id,
            saldo: dinero::a_unidades(p.hoja.saldo).unwrap(),
            sal: hex::decode(&p.hoja.sal).unwrap().try_into().unwrap(),
        };
        let ruta: Vec<merkle::Paso> = p
            .ruta
            .iter()
            .map(|paso| merkle::Paso {
                lado: paso.lado,
                hermano: merkle::Nodo {
                    hash: hex::decode(&paso.hash).unwrap().try_into().unwrap(),
                    suma: dinero::a_unidades(paso.suma).unwrap(),
                },
            })
            .collect();
        let raiz = merkle::raiz_desde_ruta(&hoja.nodo(), &ruta).unwrap();
        assert_eq!(hex::encode(raiz.hash), corte.merkle_root);
        assert_eq!(dinero::desde_unidades(raiz.suma).unwrap(), corte.total_pasivo);

        // Servicio público de verificación.
        let solicitud = SolicitudVerificacion { corte_id: corte.id, hoja: p.hoja.clone(), ruta: p.ruta.clone() };
        assert!(reserva::verificar_prueba(&pool, &solicitud).await.unwrap().valida);
        let mut falsa = solicitud.clone();
        falsa.hoja.saldo += Decimal::ONE;
        assert!(!reserva::verificar_prueba(&pool, &falsa).await.unwrap().valida);
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn cuenta_nueva_genera_corte_nuevo(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    let primero = reserva::generar_corte(&pool).await.unwrap();
    let beto = cliente(&pool, &cfg, "Beto").await;
    let p = reserva::prueba_de_cliente(&pool, beto.cliente.id, 0).await.unwrap();
    assert_ne!(p.corte.id, primero.id, "Beto no estaba en el primer corte");
    assert!(p.valida);
    assert_eq!(p.hoja.saldo, d("0"));
}

#[sqlx::test(migrations = "../migrations")]
async fn hoja_alterada_se_detecta(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "20").await;
    reserva::generar_corte(&pool).await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("ALTER TABLE ifpe.cortes_reserva_hojas DISABLE TRIGGER trg_cortes_hojas_inmutable")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE ifpe.cortes_reserva_hojas SET saldo_total = saldo_total - 5 WHERE cuenta_id = $1")
        .bind(beto.cuenta_ifpe.id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let e = reserva::prueba_de_cliente(&pool, ana.cliente.id, 0).await.unwrap_err();
    assert_eq!(e.codigo(), "INTEGRIDAD");
}

#[sqlx::test(migrations = "../migrations")]
async fn corte_incompleto_rechazado_por_la_base_de_datos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    // Un corte que declara 1 hoja pero no la registra no puede publicarse.
    let r = sqlx::query(
        "INSERT INTO ifpe.cortes_reserva (merkle_root, num_hojas, total_pasivo, total_reserva, secuencia_ledger, hash_ledger) \
         VALUES (repeat('a', 64), 1, 10, 10, 1, repeat('b', 64))",
    )
    .execute(&pool)
    .await;
    let e = r.unwrap_err();
    assert!(e.to_string().contains("SC_CORTE_INCOMPLETO"), "{e}");
}

/// Un corte (REPEATABLE READ) debe publicarse aunque haya tráfico IFPE
/// concurrente sobre las mismas cuentas. Con `FOR UPDATE` en las cuentas, la
/// verificación de llave foránea de las hojas (KEY SHARE) fallaba con 40001.
#[sqlx::test(migrations = "../migrations")]
async fn corte_se_publica_con_trafico_concurrente(pool: PgPool) {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use synthetic_capital::ledger;

    let cfg = Arc::new(preparar(&pool).await);
    cuentas_vacias(&pool, &cfg, 150).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "100000").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "100000").await;

    let parar = Arc::new(AtomicBool::new(false));
    let transferencias = Arc::new(AtomicUsize::new(0));
    let mut trafico = Vec::new();
    for i in 0..4 {
        let (pool, cfg, parar, n) = (pool.clone(), cfg.clone(), parar.clone(), transferencias.clone());
        let (origen, destino) = if i % 2 == 0 { (ana.clone(), beto.clone()) } else { (beto.clone(), ana.clone()) };
        trafico.push(tokio::spawn(async move {
            while !parar.load(Ordering::Relaxed) {
                let t = ledger::Transferencia {
                    cliente_origen_id: origen.cliente.id,
                    clabe_destino: destino.cuenta_ifpe.clabe.clone(),
                    monto: d("1"),
                    concepto: None,
                    clave_idempotencia: clave(),
                };
                ledger::transferir(&pool, &cfg.pld, &t).await.unwrap();
                n.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let antes = transferencias.load(Ordering::Relaxed);
    let mut publicados = 0;
    for _ in 0..5 {
        match reserva::generar_corte(&pool).await {
            Ok(_) => publicados += 1,
            Err(e) => eprintln!("corte fallido: {e:?}"),
        }
    }
    let durante = transferencias.load(Ordering::Relaxed) - antes;
    parar.store(true, Ordering::Relaxed);
    for t in trafico {
        t.await.unwrap();
    }
    assert!(durante > 0, "debe haber transferencias concurrentes con los cortes");
    assert_eq!(publicados, 5, "todos los cortes deben publicarse bajo tráfico");
    verificar_invariantes(&pool).await;
}

/// Hallazgo de la revisión: una cuenta nueva no puede forzar un corte por
/// petición; solo si el último corte tiene la antigüedad mínima.
#[sqlx::test(migrations = "../migrations")]
async fn cuenta_nueva_no_fuerza_cortes_seguidos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    reserva::generar_corte(&pool).await.unwrap();
    let beto = cliente(&pool, &cfg, "Beto").await;
    let e = reserva::prueba_de_cliente(&pool, beto.cliente.id, 3600).await.unwrap_err();
    assert_eq!(e.codigo(), "PRUEBA_PENDIENTE");
    let cortes: i64 = sqlx::query_scalar("SELECT count(*) FROM ifpe.cortes_reserva").fetch_one(&pool).await.unwrap();
    assert_eq!(cortes, 1, "no se generó un corte extra");
    // Ana, que sí está en el corte, obtiene su prueba sin generar nada.
    assert!(reserva::prueba_de_cliente(&pool, ana.cliente.id, 3600).await.unwrap().valida);
}

/// El orden de las hojas se baraja en cada corte (privacidad del vecino).
#[sqlx::test(migrations = "../migrations")]
async fn orden_de_hojas_cambia_entre_cortes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    cuentas_vacias(&pool, &cfg, 30).await;
    let orden = |corte: uuid::Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT cuenta_id FROM ifpe.cortes_reserva_hojas WHERE corte_id = $1 ORDER BY indice",
            )
            .bind(corte)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    let a = reserva::generar_corte(&pool).await.unwrap();
    let b = reserva::generar_corte(&pool).await.unwrap();
    assert_ne!(orden(a.id).await, orden(b.id).await);
}
