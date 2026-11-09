//! Lecturas de la bóveda. Lo del cliente se lee con su rol (`sc_cliente`) y
//! RLS: un id equivocado no puede mostrar la bóveda de otro. La prueba de
//! reserva es pública y se lee con el rol de auditor, como `/api/v1/reserva`.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use super::modelos::{
    Boveda, Direccion, EstadoPrueba, EstadoTransferencia, Mensaje, PruebaReserva, SaldoBoveda, TransferenciaSpei,
};
use crate::{
    config::Config,
    db::{self, Rol},
    dinero,
    error::{AppError, AppResult},
};

/// La raíz que se informa mientras no se ha publicado ningún corte.
pub const RAIZ_SIN_CORTE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// La bóveda del cliente con la versión de la cuenta que la produjo.
#[derive(Debug, Clone)]
pub struct Lectura {
    pub boveda: Boveda,
    pub seq: i64,
    pub cuenta_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct FilaCuenta {
    id: Uuid,
    clabe: String,
    saldo_disponible: Decimal,
    saldo_bloqueado: Decimal,
    seq: i64,
}

/// Una sola foto (REPEATABLE READ): la versión, los saldos y el principal
/// invertido vienen de la misma confirmación. Si no, una inversión a medio
/// leer mostraría el dinero dos veces o ninguna.
pub async fn leer(pool: &PgPool, config: &Config, cliente_id: Uuid) -> AppResult<Lectura> {
    let mut tx = db::begin_lectura_instantanea_cliente(pool, cliente_id).await?;
    let cuenta: Option<FilaCuenta> = sqlx::query_as(
        "SELECT id, clabe, saldo_disponible, saldo_bloqueado, seq \
         FROM ifpe.cuentas WHERE cliente_id = $1 AND moneda = 'MXN'",
    )
    .bind(cliente_id)
    .fetch_optional(&mut *tx)
    .await?;
    // Un mutuo vencido sigue siendo del cliente hasta que se liquida a su
    // cuenta; esa liquidación mueve IFPE y SOFOM en la misma versión.
    let invertido: Decimal = sqlx::query_scalar(
        "SELECT coalesce(sum(principal), 0) FROM sofom.contratos_mutuo \
         WHERE cliente_id = $1 AND estado IN ('VIGENTE', 'VENCIDO')",
    )
    .bind(cliente_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let cuenta = cuenta.ok_or_else(|| AppError::NoEncontrado("la bóveda del cliente".into()))?;
    let prueba_reserva = prueba_reserva(pool).await?;
    // Escala 8, como todo importe de la API (sin mutuos, la suma es un 0 sin decimales).
    let saldo = SaldoBoveda {
        total: dinero::con_escala(cuenta.saldo_disponible + cuenta.saldo_bloqueado + invertido),
        disponible: dinero::con_escala(cuenta.saldo_disponible),
        invertido: dinero::con_escala(invertido),
        rendimiento_no_realizado: Decimal::new(0, 2),
    };
    Ok(Lectura {
        boveda: Boveda {
            cliente_id,
            clabe: cuenta.clabe,
            banco: config.spei.banco_nombre.clone(),
            saldo,
            prueba_reserva,
            posiciones: Vec::new(),
        },
        seq: cuenta.seq,
        cuenta_id: cuenta.id,
    })
}

/// La prueba de reserva vigente: la raíz del último corte publicado y si
/// cubre los depósitos (en el corte y en vivo).
pub async fn prueba_reserva(pool: &PgPool) -> AppResult<PruebaReserva> {
    let mut tx = db::begin_lectura(pool, Rol::Auditor).await?;
    let corte: Option<(String, Decimal, Decimal)> = sqlx::query_as(
        "SELECT merkle_root, total_pasivo, total_reserva FROM ifpe.cortes_reserva \
         ORDER BY creado_en DESC, id DESC LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let reserva_100: bool = sqlx::query_scalar("SELECT pasivo_clientes = activo_reserva FROM ifpe.estado_reserva")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(match corte {
        None => PruebaReserva { estado: EstadoPrueba::Pendiente, raiz: RAIZ_SIN_CORTE.into() },
        Some((raiz, pasivo, reserva)) => PruebaReserva {
            estado: if reserva >= pasivo && reserva_100 { EstadoPrueba::Verificada } else { EstadoPrueba::Fallida },
            raiz,
        },
    })
}

#[derive(sqlx::FromRow)]
struct FilaSpei {
    clave_rastreo: String,
    entrante: bool,
    monto: Decimal,
    contraparte: String,
    banco: String,
    ocurrio_en: DateTime<Utc>,
}

/// El SPEI que movió la cuenta en la transacción `xact` (la del aviso
/// `sc_boveda`), si fue un SPEI: un entrante acreditado o el reembolso de una
/// orden saliente (migración 0012 sella ambos con su transacción).
pub async fn transferencia_de(
    pool: &PgPool,
    cliente_id: Uuid,
    cuenta_id: Uuid,
    xact: &str,
) -> AppResult<Option<TransferenciaSpei>> {
    let mut tx = db::begin_lectura_cliente(pool, cliente_id).await?;
    let fila: Option<FilaSpei> = sqlx::query_as(
        "SELECT clave_rastreo::text, TRUE AS entrante, monto, coalesce(nombre_ordenante, '') AS contraparte, \
                right(institucion_emisora::text, 3) AS banco, recibido_en AS ocurrio_en \
           FROM ifpe.spei_inbound \
          WHERE cuenta_id = $1 AND xact = $2::xid8 AND estado = 'ABONADO' \
         UNION ALL \
         SELECT clave_rastreo::text, FALSE, monto, beneficiario_nombre, banco_receptor::text, \
                coalesce(finalizada_en, actualizado_en) \
           FROM ifpe.spei_outbound \
          WHERE cuenta_id = $1 AND xact_reverso = $2::xid8 \
         LIMIT 1",
    )
    .bind(cuenta_id)
    .bind(xact)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(fila.map(|f| TransferenciaSpei {
        clave_rastreo: f.clave_rastreo,
        direccion: if f.entrante { Direccion::Entrante } else { Direccion::Saliente },
        estado: if f.entrante { EstadoTransferencia::Liquidada } else { EstadoTransferencia::Devuelta },
        monto: dinero::con_escala(f.monto),
        contraparte: f.contraparte,
        banco_contraparte: f.banco.trim().to_string(),
        ocurrio_en: f.ocurrio_en,
    }))
}

/// El mensaje para un cambio de versión: `spei.transfer` si lo causó un SPEI,
/// `vault.snapshot` si no. Lleva la versión que se leyó, que puede ser mayor
/// que la del aviso si otra transacción confirmó entretanto: los valores
/// siempre corresponden a la versión que dice.
pub async fn mensaje_por_cambio(pool: &PgPool, config: &Config, cliente_id: Uuid, xact: &str) -> AppResult<Mensaje> {
    let lectura = leer(pool, config, cliente_id).await?;
    Ok(match transferencia_de(pool, cliente_id, lectura.cuenta_id, xact).await? {
        Some(transferencia) => Mensaje::Spei { seq: lectura.seq, transferencia, saldo: lectura.boveda.saldo },
        None => Mensaje::Instantanea { seq: lectura.seq, boveda: lectura.boveda },
    })
}
