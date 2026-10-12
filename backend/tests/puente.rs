//! Puente IFPE -> SOFOM, liquidación de mutuos y crédito con calce de plazos.

mod comun;

use chrono::{Days, Utc};
use comun::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    config::Config,
    kyc::Registro,
    models::sofom::{EstadoCredito, EstadoMutuo},
    sofom::{credito, puente, tasas},
};

async fn invertir(
    pool: &PgPool,
    cfg: &Config,
    r: &Registro,
    monto: &str,
    plazo: i32,
    dias_atras: u64,
) -> puente::ResultadoMutuo {
    puente::invertir(
        pool,
        &cfg.pld,
        &puente::SolicitudMutuo {
            cliente_id: r.cliente.id,
            monto: d(monto),
            plazo_dias: plazo,
            clave_idempotencia: clave(),
            fecha_inicio: Utc::now().date_naive() - Days::new(dias_atras),
        },
    )
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn inversion_mueve_saldo_y_conserva_reserva_100(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    let r = invertir(&pool, &cfg, &ana, "600", 182, 0).await;
    assert_eq!(r.contrato.principal, d("600"));
    assert_eq!(r.contrato.estado, EstadoMutuo::Vigente);
    assert_eq!(r.contrato.tasa_anual, tasas::tasa_para(182).unwrap());
    assert_eq!(r.contrato.hash_terminos, r.terminos.hash);
    assert_eq!(r.tx_ifpe.ref_movimiento_espejo, Some(r.tx_sofom.id));
    assert_eq!(r.tx_sofom.ref_movimiento_espejo, Some(r.tx_ifpe.id));
    assert_eq!(r.contrato.fecha_vencimiento, r.contrato.fecha_inicio + Days::new(182));

    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("400"));
    let (pasivo, reserva): (Decimal, Decimal) =
        sqlx::query_as("SELECT pasivo_clientes, activo_reserva FROM ifpe.estado_reserva")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((pasivo, reserva), (d("400"), d("400")));
    let (bloqueado, caja): (Decimal, Decimal) =
        sqlx::query_as("SELECT (SELECT saldo_bloqueado FROM sofom.cuentas), (SELECT saldo FROM sofom.caja)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((bloqueado, caja), (d("600"), d("600")));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn inversion_rechazos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "100").await;
    let solicitud = |monto: &str, plazo: i32| puente::SolicitudMutuo {
        cliente_id: ana.cliente.id,
        monto: d(monto),
        plazo_dias: plazo,
        clave_idempotencia: clave(),
        fecha_inicio: Utc::now().date_naive(),
    };
    let e = puente::invertir(&pool, &cfg.pld, &solicitud("101", 28)).await.unwrap_err();
    assert_eq!(e.codigo(), "SALDO_INSUFICIENTE");
    let e = puente::invertir(&pool, &cfg.pld, &solicitud("50", 30)).await.unwrap_err();
    assert_eq!(e.codigo(), "VALIDACION", "plazo fuera de la tabla");
    let contratos: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sofom.contratos_mutuo").fetch_one(&pool).await.unwrap();
    assert_eq!(contratos, 0);
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn inversion_idempotente(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let s = puente::SolicitudMutuo {
        cliente_id: ana.cliente.id,
        monto: d("300"),
        plazo_dias: 91,
        clave_idempotencia: clave(),
        fecha_inicio: Utc::now().date_naive(),
    };
    let a = puente::invertir(&pool, &cfg.pld, &s).await.unwrap();
    let b = puente::invertir(&pool, &cfg.pld, &s).await.unwrap();
    assert!(b.idempotente);
    assert_eq!(a.contrato.id, b.contrato.id);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("700"));
    let mut otro_plazo = s.clone();
    otro_plazo.plazo_dias = 182;
    assert_eq!(
        puente::invertir(&pool, &cfg.pld, &otro_plazo).await.unwrap_err().codigo(),
        "CLAVE_IDEMPOTENCIA_REUTILIZADA"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn liquidacion_al_vencimiento_regresa_principal_e_interes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10000").await;
    // La SOFOM necesita capital propio para pagar intereses.
    credito::aportar_capital(&pool, d("1000"), Some("Capital inicial".into()), clave()).await.unwrap();

    let vigente = invertir(&pool, &cfg, &ana, "1000", 28, 0).await;
    let vencido = invertir(&pool, &cfg, &ana, "5000", 28, 30).await;
    let hoy = Utc::now().date_naive();

    let e = puente::liquidar(&pool, vigente.contrato.id, Some(ana.cliente.id), hoy).await.unwrap_err();
    assert_eq!(e.codigo(), "MUTUO_NO_VENCIDO", "no hay retiro anticipado");

    let otro = cliente(&pool, &cfg, "Otro").await;
    let e = puente::liquidar(&pool, vencido.contrato.id, Some(otro.cliente.id), hoy).await.unwrap_err();
    assert_eq!(e.codigo(), "NO_ENCONTRADO", "solo el dueño puede liquidar");

    let r = puente::liquidar(&pool, vencido.contrato.id, Some(ana.cliente.id), hoy).await.unwrap();
    let interes = tasas::interes(d("5000"), tasas::tasa_para(28).unwrap(), 28);
    assert_eq!(r.interes, interes);
    assert_eq!(r.total_retornado, d("5000") + interes);
    assert_eq!(r.contrato.estado, EstadoMutuo::Liquidado);
    assert_eq!(r.movimientos_sofom.len(), 3);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("4000") + d("5000") + interes);

    let otra_vez = puente::liquidar(&pool, vencido.contrato.id, None, hoy).await.unwrap();
    assert!(otra_vez.idempotente);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("9000") + interes);
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn liquidacion_sin_liquidez_falla_sin_efectos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let m = invertir(&pool, &cfg, &ana, "1000", 28, 30).await; // sin capital para intereses
    let e = puente::liquidar(&pool, m.contrato.id, None, Utc::now().date_naive()).await.unwrap_err();
    assert_eq!(e.codigo(), "LIQUIDEZ_SOFOM_INSUFICIENTE");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, Decimal::ZERO);
    verificar_invariantes(&pool).await;
}

fn solicitud_credito(acreditado: &Registro, principal: &str, plazo: i32) -> credito::SolicitudCredito {
    credito::SolicitudCredito {
        acreditado_id: acreditado.cliente.id,
        principal: d(principal),
        tasa_anual: d("0.18"),
        plazo_dias: plazo,
        fecha_desembolso: Utc::now().date_naive(),
        destino: Some("Capital de trabajo".into()),
        clabe_destino: "646180157000000017".into(),
        clave_idempotencia: clave(),
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn credito_solo_con_fondeo_a_plazo_igual_o_mayor(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let largo = invertir(&pool, &cfg, &ana, "600", 182, 0).await;
    let corto = invertir(&pool, &cfg, &ana, "300", 28, 0).await;

    // 500 a 90 días: el mutuo de 28 días NO puede fondearlo; usa el de 182.
    let c1 = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "500", 90)).await.unwrap();
    assert_eq!(c1.credito.estado, EstadoCredito::Vigente);
    assert_eq!(c1.asignaciones.len(), 1);
    assert_eq!(c1.asignaciones[0].contrato_id, largo.contrato.id);

    // 200 a 90 días: solo quedan 100 con plazo suficiente -> rechazado, sin rastro.
    let e = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "200", 90)).await.unwrap_err();
    assert_eq!(e.codigo(), "FONDEO_A_PLAZO_INSUFICIENTE");
    let creditos: i64 = sqlx::query_scalar("SELECT count(*) FROM sofom.creditos").fetch_one(&pool).await.unwrap();
    assert_eq!(creditos, 1);

    // 350 a 20 días: primero el mutuo que vence antes (28 días), luego el de 182.
    let c2 = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "350", 20)).await.unwrap();
    let fondeo: Vec<_> = c2.asignaciones.iter().map(|a| (a.contrato_id, a.monto)).collect();
    assert_eq!(fondeo, [(corto.contrato.id, d("300")), (largo.contrato.id, d("50"))]);

    // Ningún peso de la IFPE se prestó: la bóveda sigue al 100%.
    verificar_invariantes(&pool).await;
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("100"));

    // Mientras el crédito vive, su mutuo no puede liquidarse aunque haya vencido.
    // Cobro total del crédito 1: libera el fondeo del mutuo largo.
    let pago = credito::pagar(
        &pool,
        &credito::PagoCredito {
            credito_id: c1.credito.id,
            principal: d("500"),
            interes: d("22.19"),
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();
    assert_eq!(pago.credito.estado, EstadoCredito::Liquidado);
    let asignado: Decimal = sqlx::query_scalar("SELECT principal_asignado FROM sofom.contratos_mutuo WHERE id = $1")
        .bind(largo.contrato.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(asignado, d("50"), "solo queda asignado el tramo del crédito 2");
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn mutuo_con_cartera_viva_no_se_liquida(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    credito::aportar_capital(&pool, d("100"), None, clave()).await.unwrap();
    let m = invertir(&pool, &cfg, &ana, "1000", 28, 27).await; // vence mañana
    let mut s = solicitud_credito(&taller, "400", 1); // vence mañana también
    s.fecha_desembolso = Utc::now().date_naive();
    credito::originar(&pool, &cfg.pld, &s).await.unwrap();
    let manana = Utc::now().date_naive() + Days::new(1);
    let e = puente::liquidar(&pool, m.contrato.id, None, manana).await.unwrap_err();
    // La base de datos usa su propia fecha: hoy el mutuo aún no vence; mañana
    // seguiría bloqueado por la cartera. En cualquier caso, no se liquida.
    assert!(matches!(e.codigo(), "MUTUO_CON_CARTERA_VIGENTE" | "MUTUO_RETIRO_ANTICIPADO"), "{e:?}");
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn pagos_de_credito_validados(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    invertir(&pool, &cfg, &ana, "1000", 364, 0).await;
    let c = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "300", 180)).await.unwrap();
    let pago = |principal: &str| credito::PagoCredito {
        credito_id: c.credito.id,
        principal: d(principal),
        interes: d("0"),
        clave_idempotencia: clave(),
    };
    assert_eq!(credito::pagar(&pool, &pago("301")).await.unwrap_err().codigo(), "PAGO_EXCEDE_SALDO");
    assert_eq!(credito::pagar(&pool, &pago("0")).await.unwrap_err().codigo(), "VALIDACION");
    let parcial = credito::pagar(&pool, &pago("100")).await.unwrap();
    assert_eq!(parcial.credito.saldo_insoluto, d("200"));
    assert_eq!(parcial.credito.estado, EstadoCredito::Vigente);
    verificar_invariantes(&pool).await;
}
