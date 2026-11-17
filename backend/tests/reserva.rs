//! Proof of Reserve: cortes y pruebas de inclusión verificables.

mod comun;

use comun::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    config::Config,
    crypto::merkle::{self, Hoja, HojaPosicion},
    dinero,
    kyc::Registro,
    mercado::{self, Lado, SolicitudOrden},
    reserva::{self, PasoJson, SolicitudPosicion, SolicitudVerificacion},
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
        let p = reserva::prueba_de_cliente(&pool, &Default::default(), c.cliente.id, 0).await.unwrap();
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
        let solicitud = SolicitudVerificacion {
            corte_id: corte.id,
            hoja: p.hoja.clone(),
            ruta: p.ruta.clone(),
            posiciones: vec![],
        };
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
    let p = reserva::prueba_de_cliente(&pool, &Default::default(), beto.cliente.id, 0).await.unwrap();
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
    // Bajar solo el total ya no pasa: el desglose de la hoja (0018) debe sumarlo.
    let e = sqlx::query("UPDATE ifpe.cortes_reserva_hojas SET saldo_total = saldo_total - 5 WHERE cuenta_id = $1")
        .bind(beto.cuenta_ifpe.id)
        .execute(&mut *conn)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("ck_cortes_hojas_desglose"), "{e}");
    // Alterado con su desglose, lo detecta la reconstrucción del árbol.
    sqlx::query(
        "UPDATE ifpe.cortes_reserva_hojas SET saldo_total = saldo_total - 5, saldo_disponible = saldo_disponible - 5 \
         WHERE cuenta_id = $1",
    )
    .bind(beto.cuenta_ifpe.id)
    .execute(&mut *conn)
    .await
    .unwrap();
    let e = reserva::prueba_de_cliente(&pool, &Default::default(), ana.cliente.id, 0).await.unwrap_err();
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
    let e = reserva::prueba_de_cliente(&pool, &Default::default(), beto.cliente.id, 3600).await.unwrap_err();
    assert_eq!(e.codigo(), "PRUEBA_PENDIENTE");
    let cortes: i64 = sqlx::query_scalar("SELECT count(*) FROM ifpe.cortes_reserva").fetch_one(&pool).await.unwrap();
    assert_eq!(cortes, 1, "no se generó un corte extra");
    // Ana, que sí está en el corte, obtiene su prueba sin generar nada.
    assert!(reserva::prueba_de_cliente(&pool, &Default::default(), ana.cliente.id, 3600).await.unwrap().valida);
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

// -----------------------------------------------------------------------------
// Ronda 2 de revisión
// -----------------------------------------------------------------------------

/// Ejecuta SQL como dueño del esquema con un trigger desactivado (lo que haría
/// alguien con acceso directo a la base de datos).
async fn como_dueno(pool: &PgPool, tabla: &str, trigger: &str, sentencias: &[(&str, Vec<uuid::Uuid>)]) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(&format!("ALTER TABLE {tabla} DISABLE TRIGGER {trigger}")).execute(&mut *tx).await.unwrap();
    for (sql, ids) in sentencias {
        let mut q = sqlx::query(sql);
        for id in ids {
            q = q.bind(*id);
        }
        q.execute(&mut *tx).await.unwrap();
    }
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.unwrap();
    sqlx::query(&format!("ALTER TABLE {tabla} ENABLE TRIGGER {trigger}")).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

/// Hallazgo #2: borrar la cola del ledger y retroceder la cabeza (cuadrando
/// también saldos y sombras) ya no pasa la auditoría: los cortes publicados
/// anclan una secuencia que dejó de existir. Tampoco se puede publicar un
/// corte nuevo anclado más atrás.
#[sqlx::test(migrations = "../migrations")]
async fn truncar_el_ledger_lo_detectan_las_anclas(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let corte = reserva::generar_corte(&pool).await.unwrap();

    let (ultima, monto, reserva_id): (i64, Decimal, uuid::Uuid) =
        sqlx::query_as("SELECT secuencia, monto, reserva_id FROM ifpe.transacciones ORDER BY secuencia DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ultima, corte.secuencia_ledger);
    let cuenta = ana.cuenta_ifpe.id;
    let ajuste = format!("{monto}");
    como_dueno(
        &pool,
        "ifpe.transacciones",
        "trg_ifpe_tx_inmutable",
        &[
            // El abono también tiene su registro SPEI (inmutable): el atacante lo borra.
            ("ALTER TABLE ifpe.spei_inbound DISABLE TRIGGER trg_spei_inbound_inmutable", vec![]),
            (
                "DELETE FROM ifpe.spei_inbound WHERE transaccion_id = \
                   (SELECT id FROM ifpe.transacciones ORDER BY secuencia DESC LIMIT 1)",
                vec![],
            ),
            ("ALTER TABLE ifpe.spei_inbound ENABLE TRIGGER trg_spei_inbound_inmutable", vec![]),
            (
                "DELETE FROM ifpe.transacciones WHERE secuencia = (SELECT max(secuencia) FROM ifpe.transacciones)",
                vec![],
            ),
            (
                "UPDATE ifpe.ledger_cabeza SET ultima_secuencia = t.secuencia, ultimo_hash = t.tx_hash \
                   FROM (SELECT secuencia, tx_hash FROM ifpe.transacciones ORDER BY secuencia DESC LIMIT 1) t",
                vec![],
            ),
            (
                &format!("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - {ajuste} WHERE id = $1"),
                vec![cuenta],
            ),
            (
                &format!("UPDATE ifpe.saldos_ledger SET disponible = disponible - {ajuste} WHERE cuenta_id = $1"),
                vec![cuenta],
            ),
            (&format!("UPDATE ifpe.reserva_custodia SET saldo = saldo - {ajuste} WHERE id = $1"), vec![reserva_id]),
            (
                &format!("UPDATE ifpe.reserva_ledger SET saldo = saldo - {ajuste} WHERE reserva_id = $1"),
                vec![reserva_id],
            ),
        ],
    )
    .await;

    let r = synthetic_capital::ledger::hash::verificar_cadena_ifpe(&pool).await.unwrap();
    assert!(r.falla.is_none() && r.cabeza_coincide, "la cadena restante es consistente por sí sola: {r:?}");
    assert!(!r.valido, "pero las anclas publicadas la delatan: {r:?}");
    assert!(r.discrepancias.iter().any(|d| d.contains("ledger truncado")), "{r:?}");
    let e = reserva::generar_corte(&pool).await.unwrap_err();
    assert_eq!(e.codigo(), "INTEGRIDAD");
}

/// Hallazgo #2: una fila con secuencia <= 0 quedaría fuera del recorrido del
/// verificador; la base de datos ya no la admite.
#[sqlx::test(migrations = "../migrations")]
async fn secuencia_no_positiva_rechazada(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE ifpe.transacciones DISABLE TRIGGER trg_ifpe_tx_inmutable")
        .execute(&mut *tx)
        .await
        .unwrap();
    let e = sqlx::query("UPDATE ifpe.transacciones SET secuencia = 0").execute(&mut *tx).await.unwrap_err();
    assert!(e.to_string().contains("ck_ifpe_tx_secuencia_positiva"), "{e}");
}

/// Inserta un corte con hojas arbitrarias en una sola transacción.
async fn publicar_corte_falso(
    pool: &PgPool,
    hojas: &[(uuid::Uuid, Decimal, Option<String>)],
    ancla: Option<(i64, String)>,
) -> Result<(), sqlx::Error> {
    let (secuencia, hash): (i64, String) = match ancla {
        Some(a) => a,
        None => sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM ifpe.ledger_cabeza").fetch_one(pool).await?,
    };
    let total: Decimal = hojas.iter().map(|h| h.1).sum();
    // Desde 0018 un corte también ancla el ledger del mercado.
    let (secuencia_mercado, hash_mercado): (i64, String) =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM mercado.ledger_cabeza").fetch_one(pool).await?;
    let mut tx = pool.begin().await?;
    let corte: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO ifpe.cortes_reserva (merkle_root, num_hojas, total_pasivo, total_reserva, secuencia_ledger, hash_ledger, \
                                          secuencia_mercado, hash_mercado) \
         VALUES (repeat('a', 64), $1, $2, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(hojas.len() as i32)
    .bind(total)
    .bind(secuencia)
    .bind(hash)
    .bind(secuencia_mercado)
    .bind(hash_mercado)
    .fetch_one(&mut *tx)
    .await?;
    for (i, (cuenta, saldo, hash)) in hojas.iter().enumerate() {
        let sal = [7u8; 32];
        let real = Hoja { cuenta_id: *cuenta, saldo: dinero::a_unidades(*saldo).unwrap(), sal }.hash();
        // El desglose, todo disponible (las cuentas de estas pruebas no tienen retenciones ni órdenes).
        sqlx::query(
            "INSERT INTO ifpe.cortes_reserva_hojas (corte_id, indice, cuenta_id, saldo_total, sal, hoja_hash, \
                                                    saldo_disponible, saldo_bloqueado, saldo_comprometido) \
             VALUES ($1, $2, $3, $4, $5, $6, $4, 0, 0)",
        )
        .bind(corte)
        .bind(i as i32)
        .bind(cuenta)
        .bind(saldo)
        .bind(hex::encode(sal))
        .bind(hash.clone().unwrap_or_else(|| hex::encode(real)))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Hallazgo #3: la base de datos rechaza cortes que no reflejan el estado
/// real: hojas distintas a los saldos vivos, ancla que no es la cabeza del
/// ledger u hojas cuyo hash no corresponde a su contenido.
#[sqlx::test(migrations = "../migrations")]
async fn corte_falso_rechazado_por_la_base_de_datos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "20").await;
    let (a, b) = (ana.cuenta_ifpe.id, beto.cuenta_ifpe.id);
    let invalido = |r: Result<(), sqlx::Error>| {
        let e = r.unwrap_err().to_string();
        assert!(e.contains("SC_CORTE_INVALIDO"), "{e}");
    };

    // Beto omitido y su saldo atribuido a Ana: misma suma, otra distribución.
    let falso = publicar_corte_falso(&pool, &[(a, d("30"), None)], None).await;
    assert!(falso.is_err(), "un corte que omite a un cliente no se publica");
    // Pasivo correcto pero distribución falsa.
    invalido(publicar_corte_falso(&pool, &[(a, d("20"), None), (b, d("10"), None)], None).await);
    // Hojas correctas, ancla en una secuencia anterior a la cabeza.
    let hash1: String = sqlx::query_scalar("SELECT tx_hash FROM ifpe.transacciones WHERE secuencia = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    invalido(publicar_corte_falso(&pool, &[(a, d("10"), None), (b, d("20"), None)], Some((1, hash1))).await);
    // Hojas correctas, hash de hoja inventado.
    invalido(publicar_corte_falso(&pool, &[(a, d("10"), Some("c".repeat(64))), (b, d("20"), None)], None).await);
    // El corte honesto sí se publica.
    publicar_corte_falso(&pool, &[(a, d("10"), None), (b, d("20"), None)], None).await.unwrap();
}

/// Hallazgo #5: el verificador público no redondea montos de la prueba.
#[sqlx::test(migrations = "../migrations")]
async fn verificacion_publica_rechaza_montos_no_canonicos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10000202.1").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "5").await;
    reserva::generar_corte(&pool).await.unwrap();
    let p = reserva::prueba_de_cliente(&pool, &Default::default(), ana.cliente.id, 0).await.unwrap();
    let base =
        SolicitudVerificacion { corte_id: p.corte.id, hoja: p.hoja.clone(), ruta: p.ruta.clone(), posiciones: vec![] };
    assert!(reserva::verificar_prueba(&pool, &base).await.unwrap().valida);

    let mut nueve_decimales = base.clone();
    nueve_decimales.hoja.saldo = d("10000202.100000004");
    assert_eq!(reserva::verificar_prueba(&pool, &nueve_decimales).await.unwrap_err().codigo(), "VALIDACION");
    let mut negativa = base.clone();
    negativa.ruta[0].suma = d("-0.000000001");
    assert_eq!(reserva::verificar_prueba(&pool, &negativa).await.unwrap_err().codigo(), "VALIDACION");
}

/// Hallazgo #9: el árbol de un corte se construye una sola vez, por muchas
/// pruebas que se pidan (también en paralelo), y se reconstruye con el
/// siguiente corte.
#[sqlx::test(migrations = "../migrations")]
async fn arbol_del_corte_se_construye_una_vez(pool: PgPool) {
    use std::sync::Arc;
    let cfg = preparar(&pool).await;
    cuentas_vacias(&pool, &cfg, 40).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
    let primero = reserva::generar_corte(&pool).await.unwrap();
    let cache: Arc<reserva::CacheCortes> = Arc::default();

    let mut tareas = Vec::new();
    for i in 0..12 {
        let (pool, cache) = (pool.clone(), cache.clone());
        let cliente = if i % 2 == 0 { ana.cliente.id } else { beto.cliente.id };
        tareas.push(tokio::spawn(async move { reserva::prueba_de_cliente(&pool, &cache, cliente, 3600).await }));
    }
    for t in tareas {
        let p = t.await.unwrap().unwrap();
        assert!(p.valida);
        assert_eq!(p.corte.id, primero.id);
    }
    assert_eq!(cache.construcciones(), 1);

    let segundo = reserva::generar_corte(&pool).await.unwrap();
    let p = reserva::prueba_de_cliente(&pool, &cache, ana.cliente.id, 3600).await.unwrap();
    assert_eq!(p.corte.id, segundo.id);
    assert_eq!(cache.construcciones(), 2);
    reserva::prueba_de_cliente(&pool, &cache, beto.cliente.id, 3600).await.unwrap();
    assert_eq!(cache.construcciones(), 2);
}

// -----------------------------------------------------------------------------
// Posiciones del mercado en el corte (0018)
// -----------------------------------------------------------------------------

/// Una compra a mercado de `monto` pesos de `ticker`.
async fn comprar(pool: &PgPool, cfg: &Config, r: &Registro, ticker: &str, monto: &str) {
    let s = SolicitudOrden {
        cliente_id: r.cliente.id,
        ticker: ticker.into(),
        lado: Lado::Compra,
        monto: d(monto),
        clave_idempotencia: format!("cli:{}:compra-{ticker}-{monto}", r.cliente.id),
    };
    mercado::colocar(pool, cfg, &s).await.unwrap();
}

fn a_pasos(ruta: &[PasoJson]) -> Vec<merkle::Paso> {
    ruta.iter()
        .map(|paso| merkle::Paso {
            lado: paso.lado,
            hermano: merkle::Nodo {
                hash: hex::decode(&paso.hash).unwrap().try_into().unwrap(),
                suma: dinero::a_unidades(paso.suma).unwrap(),
            },
        })
        .collect()
}

async fn cantidad(pool: &PgPool, cliente: uuid::Uuid, ticker: &str) -> Decimal {
    sqlx::query_scalar("SELECT cantidad FROM mercado.posiciones WHERE cliente_id = $1 AND ticker = $2")
        .bind(cliente)
        .bind(ticker)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Cada posición abierta entra en el árbol de su activo, con el ancla del
/// ledger del mercado; el desglose del efectivo acompaña a la hoja, y la
/// prueba de un cliente lleva la ruta de cada posición.
#[sqlx::test(migrations = "../migrations")]
async fn las_posiciones_entran_en_el_corte(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let carla = cliente(&pool, &cfg, "Carla").await;
    for c in [&ana, &beto, &carla] {
        depositar(&pool, &cfg, c.cuenta_ifpe.id, "20000").await;
    }
    comprar(&pool, &cfg, &ana, "AUG1", "1250.00").await;
    comprar(&pool, &cfg, &beto, "AUG1", "500.00").await;
    comprar(&pool, &cfg, &beto, "AGT1", "300.00").await;

    let corte = reserva::generar_corte(&pool).await.unwrap();
    let (secuencia, hash): (i64, String) =
        sqlx::query_as("SELECT ultima_secuencia, ultimo_hash FROM mercado.ledger_cabeza")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(secuencia, 3);
    assert_eq!((corte.secuencia_mercado, corte.hash_mercado.clone()), (Some(secuencia), Some(hash)));
    // Un árbol por activo con posiciones (Carla no tiene ninguna), con su total.
    let arboles: Vec<(&str, i32)> = corte.activos.iter().map(|a| (a.ticker.as_str(), a.num_hojas)).collect();
    assert_eq!(arboles, vec![("AGT1", 1), ("AUG1", 2)]);
    for a in &corte.activos {
        let total: Decimal = sqlx::query_scalar("SELECT sum(cantidad) FROM mercado.posiciones WHERE ticker = $1")
            .bind(&a.ticker)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(a.total_cantidad, total, "{}", a.ticker);
    }
    // La copia pública lleva las mismas raíces.
    assert_eq!(reserva::corte_por_id(&pool, corte.id).await.unwrap().activos, corte.activos);
    assert_eq!(reserva::ultimo_corte(&pool).await.unwrap().unwrap().activos, corte.activos);
    let historial = reserva::listar_cortes(&pool, None, 10).await.unwrap();
    assert_eq!(historial[0].activos, corte.activos);

    let p = reserva::prueba_de_cliente(&pool, &Default::default(), beto.cliente.id, 0).await.unwrap();
    assert!(p.valida);
    let desglose = p.desglose.as_ref().expect("un corte 0018 trae el desglose");
    assert_eq!(desglose.disponible + desglose.bloqueado + desglose.comprometido, p.hoja.saldo);
    assert_eq!(desglose.disponible, p.saldo_actual);
    let tickers: Vec<&str> = p.posiciones.iter().map(|x| x.hoja.ticker.as_str()).collect();
    assert_eq!(tickers, vec!["AGT1", "AUG1"]);
    for x in &p.posiciones {
        assert!(x.valida, "{}", x.hoja.ticker);
        assert_eq!(x.hoja.cuenta_id, beto.cuenta_ifpe.id);
        assert_eq!(x.hoja.cantidad, cantidad(&pool, beto.cliente.id, &x.hoja.ticker).await);
        // Verificación independiente, solo con la prueba y la raíz publicada del activo.
        let hoja = HojaPosicion {
            ticker: x.hoja.ticker.clone(),
            cuenta_id: x.hoja.cuenta_id,
            cantidad: dinero::a_unidades(x.hoja.cantidad).unwrap(),
            sal: hex::decode(&x.hoja.sal).unwrap().try_into().unwrap(),
        };
        let raiz = merkle::raiz_desde_ruta(&hoja.nodo(), &a_pasos(&x.ruta)).unwrap();
        let publicado = corte.activos.iter().find(|a| a.ticker == x.hoja.ticker).unwrap();
        assert_eq!(hex::encode(raiz.hash), publicado.merkle_root);
        assert_eq!(dinero::desde_unidades(raiz.suma).unwrap(), publicado.total_cantidad);
    }
    // Sin posiciones, la prueba solo trae el efectivo.
    let pc = reserva::prueba_de_cliente(&pool, &Default::default(), carla.cliente.id, 0).await.unwrap();
    assert!(pc.valida && pc.posiciones.is_empty());

    // Verificación pública con las posiciones.
    let solicitud = SolicitudVerificacion {
        corte_id: corte.id,
        hoja: p.hoja.clone(),
        ruta: p.ruta.clone(),
        posiciones: p
            .posiciones
            .iter()
            .map(|x| SolicitudPosicion { hoja: x.hoja.clone(), ruta: x.ruta.clone() })
            .collect(),
    };
    let r = reserva::verificar_prueba(&pool, &solicitud).await.unwrap();
    assert!(r.valida && r.posiciones.len() == 2 && r.posiciones.iter().all(|x| x.valida), "{r:?}");
    // Una unidad de más en una posición: esa falla y la prueba entera también.
    let mut falsa = solicitud.clone();
    falsa.posiciones[1].hoja.cantidad += d("0.00000001");
    let r = reserva::verificar_prueba(&pool, &falsa).await.unwrap();
    assert!(!r.valida && r.posiciones[0].valida && !r.posiciones[1].valida, "{r:?}");
    // La hoja de la plata no pasa por oro.
    let mut otro_activo = solicitud.clone();
    otro_activo.posiciones[0].hoja.ticker = "AUG1".into();
    assert!(!reserva::verificar_prueba(&pool, &otro_activo).await.unwrap().valida);
    // Un activo sin árbol en el corte.
    let mut sin_arbol = solicitud.clone();
    sin_arbol.posiciones[0].hoja.ticker = "CETES28".into();
    let r = reserva::verificar_prueba(&pool, &sin_arbol).await.unwrap();
    assert!(!r.valida && r.posiciones[0].raiz_publicada.is_none(), "{r:?}");
    // Un ticker mal formado es un error de validación.
    let mut mal = solicitud.clone();
    mal.posiciones[0].hoja.ticker = "agt1".into();
    assert_eq!(reserva::verificar_prueba(&pool, &mal).await.unwrap_err().codigo(), "VALIDACION");
}

/// Una posición alterada después de publicar se detecta al reconstruir el
/// árbol de su activo (también para la prueba de otro cliente).
#[sqlx::test(migrations = "../migrations")]
async fn posicion_alterada_se_detecta(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    for c in [&ana, &beto] {
        depositar(&pool, &cfg, c.cuenta_ifpe.id, "5000").await;
        comprar(&pool, &cfg, c, "AUG1", "1000.00").await;
    }
    reserva::generar_corte(&pool).await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("ALTER TABLE ifpe.cortes_reserva_posiciones DISABLE TRIGGER trg_cortes_posiciones_inmutable")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE ifpe.cortes_reserva_posiciones SET cantidad = cantidad / 2 WHERE cuenta_id = $1")
        .bind(beto.cuenta_ifpe.id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let e = reserva::prueba_de_cliente(&pool, &Default::default(), ana.cliente.id, 0).await.unwrap_err();
    assert_eq!(e.codigo(), "INTEGRIDAD");
}

/// La base de datos no publica un corte que omite las posiciones abiertas,
/// aunque el efectivo cuadre.
#[sqlx::test(migrations = "../migrations")]
async fn corte_sin_posiciones_rechazado_por_la_base_de_datos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let cuenta = ana.cuenta_ifpe.id;
    depositar(&pool, &cfg, cuenta, "5000").await;
    // Sin órdenes, el corte honesto del efectivo se publica.
    publicar_corte_falso(&pool, &[(cuenta, d("5000"), None)], None).await.unwrap();

    comprar(&pool, &cfg, &ana, "AUG1", "1000.00").await;
    let saldo: Decimal = sqlx::query_scalar("SELECT saldo_disponible FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta)
        .fetch_one(&pool)
        .await
        .unwrap();
    let e = publicar_corte_falso(&pool, &[(cuenta, saldo, None)], None).await.unwrap_err().to_string();
    assert!(e.contains("SC_CORTE_INVALIDO") && e.contains("posiciones"), "{e}");
    // El corte del backend sí las incluye.
    let corte = reserva::generar_corte(&pool).await.unwrap();
    assert_eq!(corte.activos.len(), 1);
}
