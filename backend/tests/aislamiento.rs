//! Aislamiento IFPE / SOFOM por rol de PostgreSQL.

mod comun;

use std::{fs, path::Path};

use comun::*;
use sqlx::PgPool;
use synthetic_capital::db::{self, Rol};

async fn ejecutar_como(pool: &PgPool, rol: Rol, sql: &str) -> Result<(), String> {
    let mut tx = db::begin(pool, rol).await.unwrap();
    let r = sqlx::query(sql).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    r.map(|_| ()).map_err(|e| e.to_string())
}

fn denegado(r: Result<(), String>) -> bool {
    matches!(r, Err(e) if e.contains("permission denied"))
}

#[sqlx::test(migrations = "../migrations")]
async fn cada_rol_solo_ve_lo_suyo(pool: PgPool) {
    preparar(&pool).await;

    // IFPE no ve la SOFOM.
    for tabla in ["sofom.cuentas", "sofom.caja", "sofom.contratos_mutuo", "sofom.creditos", "sofom.transacciones"] {
        assert!(denegado(ejecutar_como(&pool, Rol::Ifpe, &format!("SELECT 1 FROM {tabla}")).await), "{tabla}");
    }
    // SOFOM no ve la IFPE.
    for tabla in ["ifpe.cuentas", "ifpe.reserva_custodia", "ifpe.transacciones", "ifpe.estado_reserva"] {
        assert!(denegado(ejecutar_como(&pool, Rol::Sofom, &format!("SELECT 1 FROM {tabla}")).await), "{tabla}");
    }
    // El puente no puede originar créditos ni asignar fondeo.
    assert!(denegado(ejecutar_como(&pool, Rol::Puente, "SELECT 1 FROM sofom.creditos").await));
    assert!(denegado(ejecutar_como(&pool, Rol::Puente, "SELECT 1 FROM sofom.asignaciones_fondeo").await));
    // Nadie toca los agregados ni la cabeza del ledger directamente.
    for rol in [Rol::Ifpe, Rol::Puente, Rol::Auditor] {
        assert!(denegado(ejecutar_como(&pool, rol, "UPDATE ifpe.estado_reserva SET activo_reserva = 0").await));
        assert!(denegado(ejecutar_como(&pool, rol, "UPDATE ifpe.ledger_cabeza SET ultima_secuencia = 0").await));
    }
    // KYC no puede fijar saldos al abrir una cuenta, ni crear operadores.
    assert!(denegado(
        ejecutar_como(
            &pool,
            Rol::Kyc,
            "INSERT INTO ifpe.cuentas (cliente_id, clabe, saldo_disponible) VALUES (gen_random_uuid(), '646180157000000017', 1000)"
        )
        .await
    ));
    assert!(denegado(ejecutar_como(&pool, Rol::Kyc, "UPDATE kyc.credenciales SET rol = 'OPERADOR'").await));
    // El auditor solo lee la bóveda.
    assert!(denegado(ejecutar_como(&pool, Rol::Auditor, "UPDATE ifpe.cuentas SET saldo_disponible = 0").await));
    // Ningún rol puede borrar el ledger.
    assert!(denegado(ejecutar_como(&pool, Rol::Ifpe, "DELETE FROM ifpe.transacciones").await));
}

#[sqlx::test(migrations = "../migrations")]
async fn el_rol_no_se_filtra_entre_transacciones(pool: PgPool) {
    let mut tx = db::begin(&pool, Rol::Ifpe).await.unwrap();
    let rol: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(rol, "sc_ifpe");
    tx.commit().await.unwrap();
    // La misma conexión (pool de 1 en sqlx::test no está garantizado, así que
    // se comprueba sobre el pool completo varias veces).
    for _ in 0..5 {
        let rol: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&pool).await.unwrap();
        assert_ne!(rol, "sc_ifpe", "SET LOCAL ROLE debe revertirse al terminar la transacción");
    }
}

/// Solo `db.rs` abre transacciones: así toda transacción pasa por `SET LOCAL ROLE`.
#[test]
fn solo_db_abre_transacciones() {
    fn recorrer(dir: &Path, hallazgos: &mut Vec<String>) {
        for entrada in fs::read_dir(dir).unwrap() {
            let ruta = entrada.unwrap().path();
            if ruta.is_dir() {
                recorrer(&ruta, hallazgos);
            } else if ruta.extension().is_some_and(|e| e == "rs") && !ruta.ends_with("db.rs") {
                let codigo = fs::read_to_string(&ruta).unwrap();
                if codigo.contains(".begin()") {
                    hallazgos.push(ruta.display().to_string());
                }
            }
        }
    }
    let mut hallazgos = Vec::new();
    recorrer(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hallazgos);
    assert!(hallazgos.is_empty(), "abren transacciones sin rol: {hallazgos:?}");
}
