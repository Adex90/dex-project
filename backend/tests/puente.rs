//! Puente IFPE -> SOFOM, liquidación de mutuos y crédito con calce de plazos.

mod comun;

use chrono::Days;
use comun::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    config::Config,
    db::{self, Rol},
    kyc::Registro,
    models::sofom::{EstadoCredito, EstadoMutuo},
    sofom::{credito, puente, tasas},
};

async fn invertir(pool: &PgPool, cfg: &Config, r: &Registro, monto: &str, plazo: i32) -> puente::ResultadoMutuo {
    puente::invertir(
        pool,
        &cfg.pld,
        &puente::SolicitudMutuo {
            cliente_id: r.cliente.id,
            monto: d(monto),
            plazo_dias: plazo,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap()
}

/// Mutuo ya vencido: se contrata hoy y se envejece `plazo + 2` días.
async fn invertir_vencido(
    pool: &PgPool,
    cfg: &Config,
    r: &Registro,
    monto: &str,
    plazo: i32,
) -> puente::ResultadoMutuo {
    let m = invertir(pool, cfg, r, monto, plazo).await;
    envejecer_mutuo(pool, m.contrato.id, plazo + 2).await;
    m
}

fn solicitud_credito(acreditado: &Registro, principal: &str, plazo: i32) -> credito::SolicitudCredito {
    credito::SolicitudCredito {
        acreditado_id: acreditado.cliente.id,
        principal: d(principal),
        tasa_anual: d("0.18"),
        plazo_dias: plazo,
        destino: Some("Capital de trabajo".into()),
        clabe_destino: "646180157000000017".into(),
        clave_idempotencia: clave(),
    }
}

async fn caja(pool: &PgPool) -> Decimal {
    sqlx::query_scalar("SELECT saldo FROM sofom.caja").fetch_one(pool).await.unwrap()
}

// -----------------------------------------------------------------------------
// Inversión
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn inversion_mueve_saldo_y_conserva_reserva_100(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    let r = invertir(&pool, &cfg, &ana, "600", 182).await;
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
    let bloqueado: Decimal =
        sqlx::query_scalar("SELECT saldo_bloqueado FROM sofom.cuentas").fetch_one(&pool).await.unwrap();
    assert_eq!((bloqueado, caja(&pool).await), (d("600"), d("600")));
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

/// Un mutuo con fecha de inicio retroactiva vencería de inmediato y cobraría
/// intereses de todo el plazo: la base de datos lo rechaza aunque lo intente
/// el dueño del esquema.
#[sqlx::test(migrations = "../migrations")]
async fn mutuo_retroactivo_rechazado_por_la_base_de_datos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let m = invertir(&pool, &cfg, &ana, "100", 28).await;
    let e = sqlx::query(
        "INSERT INTO sofom.contratos_mutuo (cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id, \
           principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos) \
         VALUES ($1, $2, $3, gen_random_uuid(), 100, 0.045, 28, current_date - 400, repeat('a', 64))",
    )
    .bind(ana.cliente.id)
    .bind(m.contrato.cuenta_sofom_id)
    .bind(ana.cuenta_ifpe.id)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(e.to_string().contains("SC_FECHA_VALOR"), "{e}");
}

// -----------------------------------------------------------------------------
// Liquidación al vencimiento
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn liquidacion_al_vencimiento_regresa_principal_e_interes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10000").await;
    // La SOFOM paga intereses con capital propio.
    credito::aportar_capital(&pool, d("1000"), Some("Capital inicial".into()), clave()).await.unwrap();

    let vigente = invertir(&pool, &cfg, &ana, "1000", 28).await;
    let vencido = invertir_vencido(&pool, &cfg, &ana, "5000", 28).await;

    let e = puente::liquidar(&pool, vigente.contrato.id, Some(ana.cliente.id)).await.unwrap_err();
    assert_eq!(e.codigo(), "MUTUO_NO_VENCIDO", "no hay retiro anticipado");

    let otro = cliente(&pool, &cfg, "Otro").await;
    let e = puente::liquidar(&pool, vencido.contrato.id, Some(otro.cliente.id)).await.unwrap_err();
    assert_eq!(e.codigo(), "NO_ENCONTRADO", "solo el dueño puede liquidar");

    let r = puente::liquidar(&pool, vencido.contrato.id, Some(ana.cliente.id)).await.unwrap();
    let interes = tasas::interes(d("5000"), tasas::tasa_para(28).unwrap(), 28);
    assert_eq!(r.interes, interes);
    assert_eq!(r.total_retornado, d("5000") + interes);
    assert_eq!(r.contrato.estado, EstadoMutuo::Liquidado);
    assert_eq!(r.movimientos_sofom.len(), 3);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("4000") + d("5000") + interes);

    let otra_vez = puente::liquidar(&pool, vencido.contrato.id, None).await.unwrap();
    assert!(otra_vez.idempotente);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("9000") + interes);
    verificar_invariantes(&pool).await;
}

/// Hallazgo de la revisión: sin capital propio, el interés de un mutuo no
/// puede pagarse con el principal no prestado de OTROS inversionistas (eso
/// convertiría dinero a corto plazo en fondeo de créditos a largo).
#[sqlx::test(migrations = "../migrations")]
async fn interes_nunca_sale_del_dinero_de_otros_inversionistas(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "100000").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "100000").await;
    let m_ana = invertir_vencido(&pool, &cfg, &ana, "100000", 28).await;
    invertir(&pool, &cfg, &beto, "100000", 364).await;

    let e = puente::liquidar(&pool, m_ana.contrato.id, None).await.unwrap_err();
    assert_eq!(e.codigo(), "LIQUIDEZ_SOFOM_INSUFICIENTE");
    assert_eq!(caja(&pool).await, d("200000"), "el dinero de Beto sigue íntegro");

    // La base de datos también lo impide si alguien se salta el backend.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE sofom.caja SET saldo = saldo - 1").execute(&mut *tx).await.unwrap();
    let e = tx.commit().await.unwrap_err();
    assert!(e.to_string().contains("SC_LIQUIDEZ_MUTUOS"), "{e}");

    // Con capital propio para el interés, la liquidación procede.
    let interes = tasas::interes(d("100000"), tasas::tasa_para(28).unwrap(), 28);
    credito::aportar_capital(&pool, interes, None, clave()).await.unwrap();
    puente::liquidar(&pool, m_ana.contrato.id, None).await.unwrap();
    assert_eq!(caja(&pool).await, d("100000"), "queda exactamente el principal de Beto");
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Crédito con calce de plazos
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn credito_solo_con_fondeo_a_plazo_igual_o_mayor(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let largo = invertir(&pool, &cfg, &ana, "600", 182).await;
    let corto = invertir(&pool, &cfg, &ana, "300", 28).await;

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

/// Hallazgo de la revisión: un crédito incumplido congelaba para siempre el
/// mutuo que lo fondeó. Ahora: el mutuo no puede vencer mientras fondea
/// cartera viva; al castigar el crédito, la pérdida la absorbe el capital de
/// la SOFOM (si no alcanza, el castigo se rechaza) y el inversionista cobra completo.
#[sqlx::test(migrations = "../migrations")]
async fn castigo_libera_el_mutuo_y_la_perdida_la_absorbe_el_capital(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let m = invertir(&pool, &cfg, &ana, "1000", 28).await;
    let c = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "400", 20)).await.unwrap();

    // Pasan 30 días: ambos vencen; el crédito queda VENCIDO con saldo.
    envejecer_mutuo(&pool, m.contrato.id, 30).await;
    envejecer_credito(&pool, c.credito.id, 30).await;
    assert_eq!(credito::marcar_creditos_vencidos(&pool).await.unwrap(), 1);
    let e = puente::liquidar(&pool, m.contrato.id, None).await.unwrap_err();
    assert_eq!(e.codigo(), "MUTUO_CON_CARTERA_VIGENTE");
    assert_eq!(puente::contratos_bloqueados_por_cartera(&pool).await.unwrap(), [(m.contrato.folio, d("400"))]);

    // Recupera 300; los 100 restantes se castigan.
    let pago = |principal: &str| credito::PagoCredito {
        credito_id: c.credito.id,
        principal: d(principal),
        interes: d("0"),
        clave_idempotencia: clave(),
    };
    credito::pagar(&pool, &pago("300")).await.unwrap();

    // Sin capital, el castigo se rechaza: liberar 400 de fondeo exige que la
    // caja respalde todo el principal del mutuo (faltan los 100 perdidos).
    let e = credito::castigar(&pool, c.credito.id).await.unwrap_err();
    assert_eq!(e.codigo(), "CAPITAL_INSUFICIENTE_PARA_CASTIGO");

    credito::aportar_capital(&pool, d("110"), Some("Capital para pérdidas".into()), clave()).await.unwrap();
    let castigo = credito::castigar(&pool, c.credito.id).await.unwrap();
    assert_eq!(castigo.perdida, d("100"));
    assert_eq!(castigo.credito.estado, EstadoCredito::Castigado);

    // Recuperación posterior al castigo: ingreso para la SOFOM; sigue CASTIGADO.
    let rec = credito::pagar(&pool, &pago("40")).await.unwrap();
    assert_eq!(rec.credito.estado, EstadoCredito::Castigado);
    assert_eq!(rec.credito.saldo_insoluto, d("60"));

    // El inversionista recibe principal + interés completos.
    let r = puente::liquidar(&pool, m.contrato.id, None).await.unwrap();
    assert_eq!(r.total_retornado, d("1000") + tasas::interes(d("1000"), tasas::tasa_para(28).unwrap(), 28));
    assert!(puente::contratos_bloqueados_por_cartera(&pool).await.unwrap().is_empty());
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn pagos_de_credito_validados_e_idempotentes(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    invertir(&pool, &cfg, &ana, "1000", 364).await;
    let c = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "300", 180)).await.unwrap();
    let pago = |principal: &str, interes: &str, clave: &str| credito::PagoCredito {
        credito_id: c.credito.id,
        principal: d(principal),
        interes: d(interes),
        clave_idempotencia: clave.into(),
    };
    assert_eq!(credito::pagar(&pool, &pago("301", "0", &clave())).await.unwrap_err().codigo(), "PAGO_EXCEDE_SALDO");
    assert_eq!(credito::pagar(&pool, &pago("0", "0", &clave())).await.unwrap_err().codigo(), "VALIDACION");

    let k = clave();
    let parcial = credito::pagar(&pool, &pago("100", "0", &k)).await.unwrap();
    assert_eq!(parcial.credito.saldo_insoluto, d("200"));
    assert_eq!(parcial.credito.estado, EstadoCredito::Vigente);
    assert!(credito::pagar(&pool, &pago("100", "0", &k)).await.unwrap().idempotente);
    // Mismo total, distinto desglose: no es la misma operación.
    let e = credito::pagar(&pool, &pago("0", "100", &k)).await.unwrap_err();
    assert_eq!(e.codigo(), "CLAVE_IDEMPOTENCIA_REUTILIZADA");
    verificar_invariantes(&pool).await;
}

/// El rol de crédito ya no puede cerrar mutuos ni tocar cuentas de
/// inversionistas; y ni el puente puede vencer un mutuo que fondea cartera.
#[sqlx::test(migrations = "../migrations")]
async fn mutuos_protegidos_en_la_base_de_datos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let m = invertir(&pool, &cfg, &ana, "1000", 28).await;
    let c = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "400", 20)).await.unwrap();
    envejecer_mutuo(&pool, m.contrato.id, 30).await;
    envejecer_credito(&pool, c.credito.id, 30).await;

    let intentar = |rol: Rol, sql: String| {
        let pool = pool.clone();
        async move {
            let mut tx = db::begin(&pool, rol).await.unwrap();
            let r = sqlx::query(&sql).execute(&mut *tx).await;
            let r = match r {
                Ok(_) => tx.commit().await.map(|_| ()),
                Err(e) => Err(e),
            };
            r.map_err(|e| e.to_string())
        }
    };
    let id = m.contrato.id;
    let e =
        intentar(Rol::Sofom, format!("UPDATE sofom.contratos_mutuo SET estado = 'VENCIDO' WHERE id = '{id}'")).await;
    assert!(e.unwrap_err().contains("permission denied"));
    let e = intentar(Rol::Sofom, "UPDATE sofom.cuentas SET saldo_bloqueado = 0".into()).await;
    assert!(e.unwrap_err().contains("permission denied"));
    let e =
        intentar(Rol::Puente, format!("UPDATE sofom.contratos_mutuo SET estado = 'VENCIDO' WHERE id = '{id}'")).await;
    assert!(e.unwrap_err().contains("SC_MUTUO_CON_CARTERA_VIGENTE"));
    let e =
        intentar(Rol::Puente, format!("UPDATE sofom.contratos_mutuo SET interes_pagado = 999 WHERE id = '{id}'")).await;
    assert!(e.unwrap_err().contains("SC_MUTUO_INTERES"));
    verificar_invariantes(&pool).await;
}

/// Hallazgo (ronda 2): la regla de liquidez trataba como capital el principal
/// que un acreditado ya devolvió; con eso se pagaba interés ajeno y el último
/// pago del acreditado era rechazado. Ahora el último pago siempre procede y
/// el interés de otro mutuo exige capital propio.
#[sqlx::test(migrations = "../migrations")]
async fn ultimo_pago_del_acreditado_nunca_se_rechaza(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "500").await;
    invertir(&pool, &cfg, &ana, "1000", 364).await;
    let m_beto = invertir(&pool, &cfg, &beto, "500", 28).await;
    let c = credito::originar(&pool, &cfg.pld, &solicitud_credito(&taller, "1000", 182)).await.unwrap();
    let pago = |principal: &str| credito::PagoCredito {
        credito_id: c.credito.id,
        principal: d(principal),
        interes: d("0"),
        clave_idempotencia: clave(),
    };
    credito::pagar(&pool, &pago("900")).await.unwrap();

    // Beto vence: su interés NO puede salir de los 900 que devolvió el acreditado (son de Ana).
    envejecer_mutuo(&pool, m_beto.contrato.id, 30).await;
    let e = puente::liquidar(&pool, m_beto.contrato.id, None).await.unwrap_err();
    assert_eq!(e.codigo(), "LIQUIDEZ_SOFOM_INSUFICIENTE");
    let interes = tasas::interes(d("500"), tasas::tasa_para(28).unwrap(), 28);
    credito::aportar_capital(&pool, interes, None, clave()).await.unwrap();
    puente::liquidar(&pool, m_beto.contrato.id, None).await.unwrap();

    // El último pago del acreditado procede y libera el fondeo de Ana.
    let fin = credito::pagar(&pool, &pago("100")).await.unwrap();
    assert_eq!(fin.credito.estado, EstadoCredito::Liquidado);
    assert_eq!(caja(&pool).await, d("1000"), "la caja respalda exactamente el mutuo de Ana");
    verificar_invariantes(&pool).await;
}

/// Hallazgo (ronda 2): repetir la originación con otra tasa o CLABE de
/// desembolso es otra operación (409), no la confirmación de la primera.
#[sqlx::test(migrations = "../migrations")]
async fn originacion_repetida_con_otros_terminos_es_conflicto(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    invertir(&pool, &cfg, &ana, "1000", 364).await;
    let s = solicitud_credito(&taller, "300", 180);
    let a = credito::originar(&pool, &cfg.pld, &s).await.unwrap();
    assert_eq!(a.credito.clabe_desembolso.as_deref(), Some("646180157000000017"));
    assert!(credito::originar(&pool, &cfg.pld, &s).await.unwrap().idempotente);
    let mut otra_tasa = s.clone();
    otra_tasa.tasa_anual = d("0.05");
    assert_eq!(
        credito::originar(&pool, &cfg.pld, &otra_tasa).await.unwrap_err().codigo(),
        "CLAVE_IDEMPOTENCIA_REUTILIZADA"
    );
    let mut otra_clabe = s.clone();
    otra_clabe.clabe_destino = "646180157000000020".into();
    assert_eq!(
        credito::originar(&pool, &cfg.pld, &otra_clabe).await.unwrap_err().codigo(),
        "CLAVE_IDEMPOTENCIA_REUTILIZADA"
    );
    verificar_invariantes(&pool).await;
}
