//! Cuentas vinculadas: lectura (a nombre del cliente, con RLS) y la cuenta de
//! demostración de `MODO_DEMO`.

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::modelos::{CuentaVinculada, VistaCuentaVinculada};
use crate::{
    db::{self, Rol},
    error::{AppError, AppResult},
    ledger,
    spei::clabe::{digito_verificador, Clabe},
};

/// Las cuentas vinculadas activas del cliente, la más antigua primero.
/// Lectura `sc_cliente`: RLS (0016) solo le deja ver las suyas.
pub async fn activas(pool: &PgPool, cliente_id: Uuid) -> AppResult<Vec<CuentaVinculada>> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let cuentas = sqlx::query_as(
        "SELECT * FROM ifpe.cuentas_vinculadas WHERE cliente_id = $1 AND estado = 'ACTIVA' ORDER BY creado_en, id",
    )
    .bind(cliente_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(cuentas)
}

/// La cuenta vinculada `id` del cliente, activa o dada de baja (el motor SPEI
/// decide si un retiro puede ir a ella; una repetición idempotente de un
/// retiro anterior a la baja sigue respondiendo). Ajena o inexistente: 404.
pub async fn del_cliente(pool: &PgPool, cliente_id: Uuid, id: Uuid) -> AppResult<CuentaVinculada> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let cuenta: Option<CuentaVinculada> =
        sqlx::query_as("SELECT * FROM ifpe.cuentas_vinculadas WHERE id = $1 AND cliente_id = $2")
            .bind(id)
            .bind(cliente_id)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    cuenta.ok_or_else(|| AppError::no_encontrado("la cuenta vinculada no existe"))
}

/// Cuántas CLABEs de demostración se prueban para un cliente antes de
/// rendirse (cada una ya tomada por otro cliente lleva a la siguiente).
const INTENTOS_DEMO: u32 = 8;

/// `MODO_DEMO`: si el cliente puede operar y no tiene ninguna cuenta
/// vinculada activa, le vincula la de demostración, a su nombre (el de
/// kyc.clientes, en mayúsculas, como lo registra un banco). Idempotente: dos
/// peticiones a la vez vinculan una sola (las serializa el candado de la
/// cuenta IFPE, el primero que toma también un retiro).
pub async fn asegurar_demo(pool: &PgPool, cliente_id: Uuid) -> AppResult<()> {
    let ya_tiene = !activas(pool, cliente_id).await?.is_empty();
    if ya_tiene {
        return Ok(());
    }
    db::con_reintentos(|| asegurar_demo_tx(pool, cliente_id)).await
}

async fn asegurar_demo_tx(pool: &PgPool, cliente_id: Uuid) -> AppResult<()> {
    let mut tx = db::begin(pool, Rol::Ifpe).await?;
    // Las condiciones del trigger de alta, para no provocar su rechazo: el
    // cliente opera y tiene un nombre que se puede comparar (y que cabe).
    let apto: Option<bool> = sqlx::query_scalar(
        "SELECT kyc.puede_operar(c.id) AND ifpe.nombre_comparable(c.nombre_completo) <> '' \
                AND char_length(c.nombre_completo) <= 120 \
           FROM (SELECT id, concat_ws(' ', nombre, apellido_paterno, apellido_materno) AS nombre_completo \
                   FROM kyc.clientes WHERE id = $1) c",
    )
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    if apto != Some(true) {
        return Ok(());
    }
    let cuenta_id = match ledger::id_cuenta_ifpe_de_cliente(&mut tx, cliente_id).await {
        Ok(id) => id,
        Err(AppError::NoEncontrado(_)) => return Ok(()),
        Err(e) => return Err(e),
    };
    ledger::bloquear_cuenta_ifpe(&mut tx, cuenta_id).await?;
    let ya_tiene: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ifpe.cuentas_vinculadas WHERE cliente_id = $1 AND estado = 'ACTIVA')",
    )
    .bind(cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    if ya_tiene {
        tx.commit().await?;
        return Ok(());
    }
    for intento in 0..INTENTOS_DEMO {
        let clabe = clabe_demo(cliente_id, intento);
        // Una CLABE ya activa para otro cliente (o, por azar, de Synthetic
        // Capital) no se toma: se prueba la siguiente.
        let vinculada: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO ifpe.cuentas_vinculadas (cliente_id, clabe, banco, titular, origen) \
             SELECT c.id, $2, $3, upper(concat_ws(' ', c.nombre, c.apellido_paterno, c.apellido_materno)), 'DEMO' \
               FROM kyc.clientes c \
              WHERE c.id = $1 AND NOT EXISTS (SELECT 1 FROM ifpe.clabes_asignadas WHERE clabe = $2) \
             ON CONFLICT (clabe) WHERE estado = 'ACTIVA' DO NOTHING \
             RETURNING id",
        )
        .bind(cliente_id)
        .bind(clabe.as_str())
        .bind(clabe.banco())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = vinculada {
            tx.commit().await?;
            tracing::info!(%cliente_id, cuenta_vinculada = %id, "cuenta de demostración vinculada");
            return Ok(());
        }
    }
    tx.commit().await?;
    tracing::warn!(%cliente_id, "no se vinculó la cuenta de demostración: todas las CLABEs probadas estaban tomadas");
    Ok(())
}

/// CLABE de demostración del cliente: BBVA México (012), plaza 180 (Ciudad de
/// México), siete dígitos del SHA-256 de su id (y del intento), un dígito
/// libre y «123»; el dígito libre se elige para que el verificador sea 4, así
/// que termina en «1234». La misma para el mismo cliente, siempre.
pub fn clabe_demo(cliente_id: Uuid, intento: u32) -> Clabe {
    let mut h = Sha256::new();
    h.update(b"synthetic-capital/cuenta-demo/");
    h.update(cliente_id.as_bytes());
    h.update(intento.to_be_bytes());
    let resumen = h.finalize();
    let semilla = u64::from_be_bytes(resumen[..8].try_into().expect("8 bytes")) % 10_000_000;
    // El dígito libre ocupa la posición 14 (peso 7): sus diez valores dan
    // diez verificadores distintos, uno de ellos 4.
    (0..10)
        .map(|libre| format!("012180{semilla:07}{libre}123"))
        .find(|primeros| digito_verificador(primeros) == Some(4))
        .and_then(|primeros| Clabe::parse(&format!("{primeros}4")).ok())
        .expect("siempre hay un dígito libre con verificador 4")
}

/// Nombre de un banco por su clave de 3 dígitos (la de la CLABE). Un catálogo
/// corto: los bancos más comunes y el liquidador; el resto, `None`.
pub fn nombre_banco(clave: &str) -> Option<&'static str> {
    match clave {
        "002" => Some("Banamex"),
        "012" => Some("BBVA México"),
        "014" => Some("Santander"),
        "021" => Some("HSBC"),
        "072" => Some("Banorte"),
        "646" => Some("STP"),
        _ => None,
    }
}

/// La cuenta como la ve su cliente.
pub fn vista(c: &CuentaVinculada) -> VistaCuentaVinculada {
    let clabe = c.clabe.trim();
    let banco = c.banco.trim().to_owned();
    VistaCuentaVinculada {
        id: c.id,
        nombre_banco: nombre_banco(&banco),
        banco,
        clabe_final: clabe[clabe.len().saturating_sub(4)..].to_owned(),
        titular: c.titular.clone(),
        vinculada_en: c.creado_en,
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_clabe_de_demostracion_es_valida_de_bbva_y_termina_en_1234() {
        for n in 0..500u128 {
            let cliente = Uuid::from_u128(n * 7_919 + 1);
            for intento in 0..3 {
                let c = clabe_demo(cliente, intento);
                assert!(Clabe::parse(c.as_str()).is_ok(), "{c}");
                assert_eq!((c.banco(), c.plaza()), ("012", "180"));
                assert!(c.as_str().ends_with("1234"), "{c}");
            }
        }
    }

    #[test]
    fn la_clabe_de_demostracion_es_la_misma_para_el_mismo_cliente() {
        let cliente = Uuid::from_u128(42);
        assert_eq!(clabe_demo(cliente, 0), clabe_demo(cliente, 0));
        assert_ne!(clabe_demo(cliente, 0), clabe_demo(cliente, 1));
        assert_ne!(clabe_demo(cliente, 0), clabe_demo(Uuid::from_u128(43), 0));
    }

    #[test]
    fn la_vista_muestra_solo_los_ultimos_cuatro_digitos() {
        let c = CuentaVinculada {
            id: Uuid::nil(),
            cliente_id: Uuid::nil(),
            clabe: "012180001234567899".into(),
            banco: "012".into(),
            titular: "ANA PRUEBA".into(),
            origen: "DEMO".into(),
            estado: "ACTIVA".into(),
            creado_en: chrono::Utc::now(),
            baja_en: None,
        };
        let v = vista(&c);
        assert_eq!((v.clabe_final.as_str(), v.banco.as_str(), v.nombre_banco), ("7899", "012", Some("BBVA México")));
        let json = serde_json::to_value(&v).unwrap();
        assert!(!json.to_string().contains("012180001234567899"), "{json}");
        assert_eq!(json["bank_name"], "BBVA México");
        assert_eq!(nombre_banco("999"), None);
    }
}
