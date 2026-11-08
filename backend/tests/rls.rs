//! Row-Level Security por cliente (migración 0011): una transacción a nombre de
//! un cliente (`db::begin_lectura_cliente`, rol `sc_cliente`) solo ve las filas
//! de ese cliente, aunque la consulta pida otras.

mod comun;

use comun::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    db::{self, Rol},
    ledger,
};
use uuid::Uuid;

/// Ana y Beto con dinero, una transferencia de Ana a Beto, y Carla sin nada.
struct Escenario {
    ana: Uuid,
    beto: Uuid,
    carla: Uuid,
    cuenta_beto: Uuid,
}

async fn escenario(pool: &PgPool) -> Escenario {
    let cfg = preparar(pool).await;
    let ana = cliente(pool, &cfg, "Ana").await;
    let beto = cliente(pool, &cfg, "Beto").await;
    let carla = cliente(pool, &cfg, "Carla").await;
    depositar(pool, &cfg, ana.cuenta_ifpe.id, "1000.00").await;
    depositar(pool, &cfg, beto.cuenta_ifpe.id, "500.00").await;
    let t = ledger::Transferencia {
        cliente_origen_id: ana.cliente.id,
        clabe_destino: beto.cuenta_ifpe.clabe.clone(),
        monto: Decimal::new(100, 0),
        concepto: Some("renta".into()),
        clave_idempotencia: clave(),
    };
    ledger::transferir(pool, &cfg.pld, &t).await.unwrap();
    Escenario { ana: ana.cliente.id, beto: beto.cliente.id, carla: carla.cliente.id, cuenta_beto: beto.cuenta_ifpe.id }
}

async fn contar(tx: &mut db::Tx, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&mut **tx).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn contar_con(tx: &mut db::Tx, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql).bind(id).fetch_one(&mut **tx).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[sqlx::test(migrations = "../migrations")]
async fn un_cliente_solo_ve_sus_filas(pool: PgPool) {
    let e = escenario(&pool).await;

    let mut tx = db::begin_lectura_cliente(&pool, e.ana).await.unwrap();
    let rol: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(rol, "sc_cliente");
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM kyc.clientes").await, 1);
    assert_eq!(contar_con(&mut tx, "SELECT count(*) FROM kyc.clientes WHERE id = $1", e.ana).await, 1);
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.cuentas").await, 1);
    // Su depósito y su transferencia a Beto; no el depósito de Beto.
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.transacciones").await, 2);
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.spei_inbound").await, 1);
    // Pedir lo de otro, como haría un error de la aplicación (IDOR), no devuelve nada.
    assert_eq!(contar_con(&mut tx, "SELECT count(*) FROM kyc.clientes WHERE id = $1", e.beto).await, 0);
    assert_eq!(contar_con(&mut tx, "SELECT count(*) FROM ifpe.cuentas WHERE cliente_id = $1", e.beto).await, 0);
    assert_eq!(
        contar_con(
            &mut tx,
            "SELECT count(*) FROM ifpe.transacciones WHERE cuenta_destino_id = $1 AND tipo = 'DEPOSITO'",
            e.cuenta_beto
        )
        .await,
        0
    );
    tx.commit().await.unwrap();

    // Beto ve su depósito y la transferencia que recibió de Ana: cada uno su lado.
    let mut tx = db::begin_lectura_cliente(&pool, e.beto).await.unwrap();
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.transacciones").await, 2);
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'TRANSFERENCIA'").await, 1);
    tx.commit().await.unwrap();

    // Carla no tiene movimientos: no ve ninguno.
    let mut tx = db::begin_lectura_cliente(&pool, e.carla).await.unwrap();
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.transacciones").await, 0);
    assert_eq!(contar(&mut tx, "SELECT count(*) FROM ifpe.cuentas").await, 1);
    tx.commit().await.unwrap();

    // Los roles de servicio no cambian: el de la bóveda ve todas las cuentas.
    let mut tx = db::begin_lectura(&pool, Rol::Ifpe).await.unwrap();
    assert!(contar(&mut tx, "SELECT count(*) FROM ifpe.cuentas").await >= 3);
    assert!(contar(&mut tx, "SELECT count(*) FROM ifpe.transacciones").await >= 3);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../migrations")]
async fn sin_cliente_no_ve_nada(pool: PgPool) {
    escenario(&pool).await;
    // El rol sin `sc.cliente_actual`: falla cerrado.
    let mut tx = db::begin_lectura(&pool, Rol::Cliente).await.unwrap();
    for tabla in [
        "kyc.clientes",
        "ifpe.cuentas",
        "ifpe.transacciones",
        "ifpe.spei_inbound",
        "ifpe.spei_outbound",
        "sofom.cuentas",
        "sofom.contratos_mutuo",
        "sofom.transacciones",
    ] {
        assert_eq!(contar(&mut tx, &format!("SELECT count(*) FROM {tabla}")).await, 0, "{tabla}");
    }
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../migrations")]
async fn un_cliente_que_no_es_uuid_hace_fallar_la_consulta(pool: PgPool) {
    escenario(&pool).await;
    let mut tx = db::begin_lectura(&pool, Rol::Cliente).await.unwrap();
    sqlx::query("SELECT set_config('sc.cliente_actual', 'todos', true)").execute(&mut *tx).await.unwrap();
    let r: Result<i64, _> = sqlx::query_scalar("SELECT count(*) FROM ifpe.cuentas").fetch_one(&mut *tx).await;
    let e = r.unwrap_err().to_string();
    assert!(e.contains("invalid input syntax for type uuid"), "{e}");
}

#[sqlx::test(migrations = "../migrations")]
async fn el_rol_del_cliente_solo_lee(pool: PgPool) {
    let e = escenario(&pool).await;
    // Transacción de escritura (no de solo lectura) para probar privilegios, no el modo.
    for sql in [
        "UPDATE kyc.clientes SET telefono = '5512345678' WHERE id = $1",
        "UPDATE ifpe.cuentas SET saldo_disponible = 0 WHERE cliente_id = $1",
        "DELETE FROM ifpe.transacciones WHERE cuenta_origen_id IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = $1)",
        "DELETE FROM kyc.clientes WHERE id = $1",
        "INSERT INTO ifpe.spei_outbound (cliente_id) VALUES ($1)",
    ] {
        let mut tx = db::begin(&pool, Rol::Cliente).await.unwrap();
        sqlx::query("SELECT set_config('sc.cliente_actual', $1, true)")
            .bind(e.ana.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        let r = sqlx::query(sql).bind(e.ana).execute(&mut *tx).await;
        let error = r.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(error.contains("permission denied"), "{sql}: {error}");
    }
    // Ni las tablas que no son del cliente.
    for tabla in ["kyc.credenciales", "sofom.creditos", "ifpe.reserva_custodia", "aml.alertas"] {
        let mut tx = db::begin_lectura_cliente(&pool, e.ana).await.unwrap();
        let r = sqlx::query(&format!("SELECT 1 FROM {tabla}")).execute(&mut *tx).await;
        let error = r.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(error.contains("permission denied"), "{tabla}: {error}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn las_lecturas_del_cliente_corren_con_su_rol(pool: PgPool) {
    let e = escenario(&pool).await;
    // Las funciones que usan las rutas del cliente pasan por sc_cliente y siguen
    // devolviendo lo suyo.
    let cuenta = ledger::cuenta_de_cliente(&pool, e.ana).await.unwrap().expect("cuenta");
    assert_eq!(cuenta.cliente_id, e.ana);
    let movimientos = ledger::movimientos_de_cliente(&pool, e.beto, 50, None).await.unwrap();
    assert_eq!(movimientos.len(), 2);
    let perfil = synthetic_capital::kyc::perfil(&pool, e.carla).await.unwrap();
    assert_eq!(perfil.id, e.carla);
}
