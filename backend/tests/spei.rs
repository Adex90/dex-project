//! Integración SPEI: webhooks firmados, duplicados concurrentes, despachador
//! con la pasarela simulada, autoridad de los roles, correcciones, las
//! regresiones de la revisión de 0005 (migración 0007) y un día de tráfico
//! interbancario intenso conciliado contra el custodio.

mod comun;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use chrono::{Days, NaiveDate, Utc};
use comun::*;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    config::Config,
    db::{self, Rol},
    kyc::Registro,
    models::kyc::RolUsuario,
    routes,
    sofom::puente,
    spei::{
        clabe, firma, hoy_cdmx,
        inbound::{self, Origen, SiNoAcreditable},
        outbound::{self, Evento, SolicitudResolucion, SolicitudSpei, SpeiOutbound},
        pasarela::{
            fecha_operacion_simulada, Comportamiento, ConsultaOrden, ErrorPasarela, EstadoProveedor, Futuro,
            OrdenPasarela, Pasarela, PasarelaSimulada, Registro as RegistroPasarela,
        },
    },
    AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

/// CLABEs externas válidas (bancos 012, 002, 014, 021, 072).
const EXTERNA_A: &str = "012180001234567899";
const EXTERNA_B: &str = "002010077777777771";

fn externa(n: u64) -> String {
    let primeros = format!("072180{n:011}");
    let dv = clabe::digito_verificador(&primeros).unwrap();
    format!("{primeros}{dv}")
}

struct Banco {
    pool: PgPool,
    cfg: Config,
    app: Router,
    estado: AppState,
    pasarela: Arc<PasarelaSimulada>,
}

/// Hora actual en la Ciudad de México (UTC-6, como `hoy_cdmx`).
fn ahora_cdmx() -> chrono::NaiveTime {
    (Utc::now() - chrono::Duration::hours(6)).time()
}

async fn banco(pool: &PgPool) -> Banco {
    banco_con(pool, |_| {}).await
}

/// Banco con la configuración ajustada. El corte SPEI queda a 12 horas de
/// ahora: ninguna prueba cruza por accidente el cambio de día de operación
/// (el despachador no reenvía si el día pudo cambiar).
async fn banco_con(pool: &PgPool, ajustar: impl FnOnce(&mut Config)) -> Banco {
    let mut cfg = preparar(pool).await;
    cfg.spei.hora_corte = ahora_cdmx() + chrono::Duration::hours(12);
    ajustar(&mut cfg);
    let pasarela = Arc::new(PasarelaSimulada::new(1));
    let estado = AppState::con_pasarela(pool.clone(), cfg.clone(), pasarela.clone());
    Banco { pool: pool.clone(), app: routes::router(estado.clone()), estado, cfg, pasarela }
}

/// Operador con credenciales reales (el JWT se valida contra la base de datos).
async fn operador(pool: &PgPool) -> Uuid {
    bootstrap::asegurar_operador(pool, "cumplimiento@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = 'cumplimiento@synthetic.mx'")
        .fetch_one(pool)
        .await
        .unwrap()
}

impl Banco {
    fn secreto(&self) -> &[u8] {
        self.cfg.spei.webhook_secretos[0].as_bytes()
    }

    async fn enviar(&self, ruta: &str, cuerpo: String, firma: Option<String>) -> (StatusCode, Value) {
        let mut req = Request::builder().method("POST").uri(ruta).header("content-type", "application/json");
        if let Some(f) = firma {
            req = req.header(firma::CABECERA, f);
        }
        let resp = self.app.clone().oneshot(req.body(Body::from(cuerpo)).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn token(&self, id: Uuid, rol: RolUsuario) -> String {
        self.estado.jwt.emitir(id, rol).unwrap().0
    }

    /// Petición autenticada a la API.
    async fn llamar(&self, token: &str, ruta: &str, cuerpo: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(ruta)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(cuerpo.to_string()))
            .unwrap();
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Webhook firmado con el secreto vigente y la hora actual.
    async fn webhook(&self, ruta: &str, cuerpo: String) -> (StatusCode, Value) {
        let f = firma::firmar(self.secreto(), chrono::Utc::now().timestamp(), cuerpo.as_bytes());
        self.enviar(ruta, cuerpo, Some(f)).await
    }

    async fn despachar(&self, ciclos: usize) {
        for _ in 0..ciclos {
            vencer_ordenes(&self.pool).await;
            outbound::despachar(&self.pool, &*self.pasarela, &self.cfg.spei).await.unwrap();
        }
    }

    async fn orden(&self, id: Uuid) -> SpeiOutbound {
        sqlx::query_as("SELECT * FROM ifpe.spei_outbound WHERE id = $1").bind(id).fetch_one(&self.pool).await.unwrap()
    }

    async fn enviar_spei(&self, r: &Registro, clabe_destino: &str, monto: &str) -> SpeiOutbound {
        outbound::solicitar(
            &self.pool,
            &self.cfg.pld,
            &self.cfg.spei,
            &SolicitudSpei {
                cliente_id: r.cliente.id,
                clabe_destino: clabe_destino.into(),
                beneficiario_nombre: Some("Beneficiario Externo".into()),
                beneficiario_rfc_curp: None,
                monto: d(monto),
                concepto: Some("Pago de prueba".into()),
                referencia_numerica: Some(123),
                clave_idempotencia: clave(),
            },
        )
        .await
        .unwrap()
        .orden
    }
}

/// Cuerpo de una notificación de abono al estilo STP (monto como número JSON
/// exacto, tal cual lo escribe el proveedor).
fn cuerpo_abono(clabe: &str, monto: &str, emisor: &str, rastreo: &str, fecha: NaiveDate) -> String {
    format!(
        r#"{{"id":{id},"fechaOperacion":{fecha},"institucionOrdenante":{emisor},"institucionBeneficiaria":90646,
            "claveRastreo":"{rastreo}","monto":{monto},"nombreOrdenante":"JUAN PEREZ","tipoPago":1,
            "cuentaOrdenante":"012180001234567899","cuentaBeneficiario":"{clabe}","conceptoPago":"RENTA",
            "referenciaNumerica":1234567}}"#,
        id = rastreo.len(),
        fecha = fecha.format("%Y%m%d"),
    )
}

async fn reserva_total(pool: &PgPool) -> Decimal {
    sqlx::query_scalar("SELECT coalesce(sum(saldo), 0) FROM ifpe.reserva_custodia").fetch_one(pool).await.unwrap()
}

async fn contar(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

// -----------------------------------------------------------------------------
// CLABE: Rust y PostgreSQL aplican la misma regla
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn clabe_misma_regla_en_rust_y_postgres(pool: PgPool) {
    let mut casos = Vec::new();
    for i in 0..400u64 {
        let c = clabe::generar("646180").to_string();
        // Variante con el último dígito alterado y otra con un dígito intermedio alterado.
        let mut b = c.clone().into_bytes();
        b[17] = b'0' + (b[17] - b'0' + 1 + (i % 9) as u8) % 10;
        let mut m = c.clone().into_bytes();
        let p = (i % 17) as usize;
        m[p] = b'0' + (m[p] - b'0' + 3) % 10;
        casos.extend([c, String::from_utf8(b).unwrap(), String::from_utf8(m).unwrap()]);
    }
    casos.extend(["032180000118359719".into(), "12345".into(), "64618015700000001a".into()]);
    for c in casos {
        let sql: bool = sqlx::query_scalar("SELECT nucleo.clabe_valida($1)").bind(&c).fetch_one(&pool).await.unwrap();
        assert_eq!(clabe::es_valida(&c), sql, "{c}");
    }
}

// -----------------------------------------------------------------------------
// Webhook de abono
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn webhook_abona_cliente_y_reserva_juntos(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let reserva_antes = reserva_total(&pool).await;
    let cuerpo = cuerpo_abono(&ana.cuenta_ifpe.clabe, "1500.25", "40012", "MBAN01002610130000123", hoy_cdmx());
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["mensaje"], "confirmar");
    assert_eq!(r["estado"], "ABONADO");

    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1500.25"));
    assert_eq!(reserva_total(&pool).await - reserva_antes, d("1500.25"), "la reserva sube 1:1");
    let (referencia, clave_tx): (String, String) =
        sqlx::query_as("SELECT referencia_externa, clave_idempotencia FROM ifpe.transacciones WHERE tipo = 'DEPOSITO'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let identidad = format!("40012:{}:MBAN01002610130000123", hoy_cdmx().format("%Y%m%d"));
    assert_eq!(referencia, identidad);
    assert_eq!(clave_tx, format!("spei:{identidad}"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn webhook_sin_firma_valida_se_rechaza(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let cuerpo = cuerpo_abono(&ana.cuenta_ifpe.clabe, "100", "40012", "FIRMA1", hoy_cdmx());
    let ahora = chrono::Utc::now().timestamp();
    let casos = [
        None,
        Some("t=1,v1=00".to_string()),
        Some(firma::firmar(b"otro-secreto-que-no-es-el-del-banco-0000", ahora, cuerpo.as_bytes())),
        Some(firma::firmar(b.secreto(), ahora - 3_600, cuerpo.as_bytes())),
        // Firma válida de otro cuerpo (otro monto).
        Some(firma::firmar(b.secreto(), ahora, cuerpo.replace("\"monto\":100", "\"monto\":900").as_bytes())),
    ];
    for f in casos {
        let (s, r) = b.enviar("/api/v1/spei/webhook/inbound", cuerpo.clone(), f.clone()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{f:?}: {r}");
        assert_eq!(r["error"]["codigo"], "FIRMA_INVALIDA");
    }
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound").await, 0);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("0"));
}

#[sqlx::test(migrations = "../migrations")]
async fn webhook_devuelve_lo_que_no_puede_abonar(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let beto = cliente(&pool, &b.cfg, "Beto").await;
    revocar_kyc(&pool, beto.cliente.id).await;
    let reserva_antes = reserva_total(&pool).await;
    let casos = [
        (externa(5), "1", "CLABE que no es nuestra"),
        ("646180157000000018".to_string(), "1", "dígito verificador inválido"),
        (beto.cuenta_ifpe.clabe.clone(), "2", "cliente sin KYC"),
    ];
    for (i, (clabe_destino, causa, caso)) in casos.iter().enumerate() {
        let cuerpo = cuerpo_abono(clabe_destino, "250", "40014", &format!("DEV{i}"), hoy_cdmx());
        let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo).await;
        assert_eq!(s, StatusCode::OK, "{caso}: {r}");
        assert_eq!(r["mensaje"], "devolver", "{caso}");
        assert_eq!(r["id"].to_string(), *causa, "{caso}");
        assert_eq!(r["estado"], "DEVUELTO");
    }
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound WHERE estado = 'DEVUELTO'").await, 3);
    assert_eq!(reserva_total(&pool).await, reserva_antes, "un pago devuelto no toca la reserva");
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("0"));

    // Autenticada pero inutilizable (3 decimales): cuarentena y devolución, nunca 4xx.
    let cuerpo = cuerpo_abono(&ana.cuenta_ifpe.clabe, "10.001", "40012", "TRESDEC", hoy_cdmx());
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo).await;
    assert_eq!((s, &r["mensaje"], &r["id"]), (StatusCode::OK, &json!("devolver"), &json!(14)), "{r}");
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_cuarentena").await, 1);
    verificar_invariantes(&pool).await;
}

/// Hallazgo de la revisión: un pago en fin de semana o puente lleva la fecha de
/// operación del siguiente día hábil; antes se rechazaba y quedaba sin dueño.
#[sqlx::test(migrations = "../migrations")]
async fn fecha_de_operacion_del_siguiente_dia_habil(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let martes = hoy_cdmx() + Days::new(4);
    let (s, r) = b
        .webhook(
            "/api/v1/spei/webhook/inbound",
            cuerpo_abono(&ana.cuenta_ifpe.clabe, "300", "40012", "PUENTE1", martes),
        )
        .await;
    assert_eq!((s, r["mensaje"].as_str()), (StatusCode::OK, Some("confirmar")), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("300"));
}

/// Regla 2: el mismo pago notificado muchas veces en paralelo (con variantes
/// de mayúsculas en la clave de rastreo) se abona exactamente una vez.
#[sqlx::test(migrations = "../migrations")]
async fn webhooks_duplicados_concurrentes_abonan_una_vez(pool: PgPool) {
    let b = Arc::new(banco(&pool).await);
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let reserva_antes = reserva_total(&pool).await;
    let mut tareas = Vec::new();
    for i in 0..60 {
        let b = b.clone();
        let rastreo = if i % 3 == 0 { "mban0100261013dup" } else { "MBAN0100261013DUP" };
        let cuerpo = cuerpo_abono(&ana.cuenta_ifpe.clabe, "5000.00", "40012", rastreo, hoy_cdmx());
        tareas.push(tokio::spawn(async move { b.webhook("/api/v1/spei/webhook/inbound", cuerpo).await }));
    }
    let mut idempotentes = 0;
    for t in tareas {
        let (s, r) = t.await.unwrap();
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(r["mensaje"], "confirmar");
        if r["idempotente"] == true {
            idempotentes += 1;
        }
    }
    assert_eq!(idempotentes, 59, "una sola notificación abona; las demás son repeticiones");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("5000"));
    assert_eq!(reserva_total(&pool).await - reserva_antes, d("5000"));
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound").await, 1);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'DEPOSITO'").await, 1);

    // Misma identidad con otro monto: se retiene para revisión (registrada y
    // confirmada al proveedor, nunca un 409 sin registro), sin abono.
    let cuerpo = cuerpo_abono(&ana.cuenta_ifpe.clabe, "5000.01", "40012", "MBAN0100261013DUP", hoy_cdmx());
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo).await;
    assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("EN_REVISION")), "{r}");
    assert_eq!(r["motivo"], "IDENTIDAD_REUTILIZADA");
    assert_eq!(
        contar(&pool, "SELECT count(*) FROM ifpe.spei_cuarentena WHERE tipo = 'IDENTIDAD_REUTILIZADA'").await,
        1
    );
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("5000"));
    verificar_invariantes(&pool).await;
}

/// Hallazgo de la revisión: el operador captura un pago con la fecha
/// calendario (o un código de emisor equivocado) y después llega el webhook
/// con la identidad real: no se abona dos veces. (inbound-security-2: el
/// webhook queda retenido y registrado, y se confirma; ya no es un 409.)
#[sqlx::test(migrations = "../migrations")]
async fn captura_manual_con_otra_identidad_no_duplica(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let op = operador(&pool).await;
    let mut manual = abono(&ana.cuenta_ifpe.clabe, "5000", "MBAN0100MANUAL");
    manual.identidad =
        synthetic_capital::spei::IdentidadSpei::nueva("90012", hoy_cdmx() - Days::new(2), "MBAN0100MANUAL").unwrap();
    inbound::abonar(&pool, &b.cfg.pld, &b.cfg.spei, &manual, Origen::Operador(op), SiNoAcreditable::Rechazar)
        .await
        .unwrap();
    let cuerpo = cuerpo_abono(&ana.cuenta_ifpe.clabe, "5000", "40012", "MBAN0100MANUAL", hoy_cdmx());
    for _ in 0..2 {
        let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo.clone()).await;
        assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("EN_REVISION")), "{r}");
        assert_eq!(r["motivo"], "POSIBLE_DUPLICADO");
    }
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("5000"));
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound").await, 1);
    let identidad = format!("40012:{}:MBAN0100MANUAL", hoy_cdmx().format("%Y%m%d"));
    let retenidas: Vec<String> =
        sqlx::query_scalar("SELECT identidad FROM ifpe.spei_cuarentena WHERE tipo = 'POSIBLE_DUPLICADO'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(retenidas, vec![identidad.clone()], "una fila aunque el proveedor reintente");
    let en_revision: Vec<String> =
        sqlx::query_scalar("SELECT identidad FROM ifpe.v_spei_en_revision").fetch_all(&pool).await.unwrap();
    assert_eq!(en_revision, vec![identidad]);
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Envío
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn transferencia_saliente_ciclo_completo(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    b.pasarela.fijar_saldo_custodio(reserva_total(&pool).await);
    let (token, _) = b.estado.jwt.emitir(ana.cliente.id, RolUsuario::Cliente).unwrap();
    let cuerpo = json!({
        "clabe_destino": EXTERNA_A,
        "beneficiario_nombre": "Beto Núñez",
        "monto": "250.50",
        "concepto": "Pago de renta",
        "clave_idempotencia": "transfer-0001",
    })
    .to_string();
    let llamar = |cuerpo: String| {
        let app = b.app.clone();
        let token = token.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri("/api/v1/spei/transfer")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(cuerpo))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            let s = resp.status();
            let v: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
            (s, v)
        }
    };
    let (s, r) = llamar(cuerpo.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["orden"]["estado"], "PENDIENTE");
    assert_eq!(r["orden"]["beneficiario_nombre"], "Beto Nunez");
    assert_eq!(r["orden"]["ordenante_clabe"], ana.cuenta_ifpe.clabe);
    let (s, r2) = llamar(cuerpo).await;
    assert_eq!(s, StatusCode::OK, "repetición idempotente");
    assert_eq!(r["orden"]["id"], r2["orden"]["id"]);
    // El dinero ya no es del cliente ni está en la reserva contable.
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("749.50"));
    let id: Uuid = r["orden"]["id"].as_str().unwrap().parse().unwrap();

    b.despachar(1).await;
    assert_eq!(b.orden(id).await.estado, "ENVIADA");
    b.despachar(1).await;
    let o = b.orden(id).await;
    assert_eq!(o.estado, "LIQUIDADA");
    assert!(o.fecha_operacion.is_some());
    b.despachar(2).await; // vigilancia de devolución: sin cambios
    assert_eq!(b.orden(id).await.estado, "LIQUIDADA");
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);
    assert_eq!(b.pasarela.saldo_custodio(), reserva_total(&pool).await, "custodio == reserva contable");
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn validaciones_de_la_transferencia(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let beto = cliente(&pool, &b.cfg, "Beto").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    // 600 a un mutuo SOFOM: no cuentan como saldo líquido para enviar.
    puente::invertir(
        &pool,
        &b.cfg.pld,
        &puente::SolicitudMutuo {
            cliente_id: ana.cliente.id,
            monto: d("600"),
            plazo_dias: 91,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();
    let intentar = |clabe_destino: &str, monto: &str| {
        let (pool, cfg) = (pool.clone(), b.cfg.clone());
        let s = SolicitudSpei {
            cliente_id: ana.cliente.id,
            clabe_destino: clabe_destino.into(),
            beneficiario_nombre: Some("Beto".into()),
            beneficiario_rfc_curp: None,
            monto: d(monto),
            concepto: None,
            referencia_numerica: None,
            clave_idempotencia: clave(),
        };
        async move {
            outbound::solicitar(&pool, &cfg.pld, &cfg.spei, &s).await.map(|_| ()).map_err(|e| e.codigo().to_string())
        }
    };
    assert_eq!(intentar(EXTERNA_A, "400.01").await.unwrap_err(), "SALDO_INSUFICIENTE");
    assert_eq!(intentar("012180001234567890", "10").await.unwrap_err(), "VALIDACION", "dígito verificador");
    assert_eq!(intentar(EXTERNA_A, "10.001").await.unwrap_err(), "VALIDACION", "centavos");
    assert_eq!(intentar(&beto.cuenta_ifpe.clabe, "10").await.unwrap_err(), "CLABE_PROPIA");
    intentar(EXTERNA_A, "400").await.unwrap();
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("0"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn rechazo_y_cancelacion_reembolsan_una_vez(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let rechazada = externa(1);
    let cancelada = externa(2);
    b.pasarela.programar(&rechazada, Comportamiento::RechazarAlRegistrar { causa: 1 });
    b.pasarela.programar(&cancelada, Comportamiento::Cancelar { causa: 3 });
    let o1 = b.enviar_spei(&ana, &rechazada, "100").await;
    let o2 = b.enviar_spei(&ana, &cancelada, "200").await;
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("700"));
    b.despachar(4).await;
    let (o1, o2) = (b.orden(o1.id).await, b.orden(o2.id).await);
    assert_eq!((o1.estado.as_str(), o1.causa_devolucion), ("RECHAZADA", Some(1)));
    assert_eq!((o2.estado.as_str(), o2.causa_devolucion), ("RECHAZADA", Some(3)));
    assert!(o1.tx_reverso_id.is_some() && o2.tx_reverso_id.is_some());
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"), "reembolso total, una sola vez");
    assert_eq!(
        contar(&pool, "SELECT count(*) FROM ifpe.transacciones WHERE clave_idempotencia LIKE 'spei-reverso:%'").await,
        2
    );
    verificar_invariantes(&pool).await;
}

/// Hallazgo crítico de la revisión: el registro agota el tiempo pero la orden
/// SÍ quedó registrada, y la consulta tarda en mostrarla. El reenvío vuelve
/// "rechazado por duplicado": antes eso reembolsaba al cliente mientras el
/// pago original se liquidaba (pago doble). Ahora va a revisión manual y,
/// cuando el proveedor confirma la liquidación, la orden se cierra sin reembolso.
#[sqlx::test(migrations = "../migrations")]
async fn tiempo_agotado_ambiguo_nunca_reembolsa(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let destino = externa(3);
    b.pasarela.programar(&destino, Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: true });
    b.pasarela.programar(&destino, Comportamiento::OcultarEnConsulta { veces: 1 });
    let o = b.enviar_spei(&ana, &destino, "400").await;

    b.despachar(1).await; // registro: tiempo agotado (pero registrada)
    let tras_timeout = b.orden(o.id).await;
    assert_eq!((tras_timeout.estado.as_str(), tras_timeout.intentos_envio), ("PENDIENTE", 1));
    b.despachar(1).await; // consulta: "no existe" -> reenvío -> duplicado
    let ambigua = b.orden(o.id).await;
    assert_eq!(ambigua.estado, "PENDIENTE");
    assert!(ambigua.revision_manual, "{ambigua:?}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("600"), "sin reembolso");
    b.despachar(1).await; // la consulta ya la muestra: liquidada
    let final_ = b.orden(o.id).await;
    assert_eq!(final_.estado, "LIQUIDADA");
    assert!(!final_.revision_manual);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("600"));
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);
    assert!(final_.tx_reverso_id.is_none());
    verificar_invariantes(&pool).await;
}

/// Tiempo agotado sin registro: se consulta, no existe, se reenvía con la
/// misma clave de rastreo y se liquida una sola vez.
#[sqlx::test(migrations = "../migrations")]
async fn tiempo_agotado_sin_registro_se_reenvia(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let destino = externa(4);
    b.pasarela.programar(&destino, Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: false });
    let o = b.enviar_spei(&ana, &destino, "300").await;
    b.despachar(4).await;
    let o = b.orden(o.id).await;
    assert_eq!(o.estado, "LIQUIDADA");
    assert_eq!(o.intentos_envio, 2);
    assert_eq!(b.pasarela.ordenes_registradas(), 1);
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn devolucion_tras_liquidar_reembolsa(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    b.pasarela.fijar_saldo_custodio(reserva_total(&pool).await);
    let destino = externa(6);
    b.pasarela.programar(&destino, Comportamiento::DevolverTrasLiquidar { causa: 1 });
    let o = b.enviar_spei(&ana, &destino, "250").await;
    b.despachar(2).await;
    assert_eq!(b.orden(o.id).await.estado, "LIQUIDADA");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("750"));
    b.despachar(1).await; // vigilancia: el banco receptor la devolvió
    let o = b.orden(o.id).await;
    assert_eq!((o.estado.as_str(), o.causa_devolucion), ("DEVUELTA", Some(1)));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    assert_eq!(b.pasarela.saldo_custodio(), reserva_total(&pool).await);
    verificar_invariantes(&pool).await;
}

/// Webhook de estado: idempotente, y un resultado incompatible con el estado
/// final es un incidente (409 + revisión), nunca un movimiento de dinero.
#[sqlx::test(migrations = "../migrations")]
async fn webhook_de_estado(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_B, "100").await;
    b.despachar(1).await;
    let estado = |e: &str| json!({ "claveRastreo": o.clave_rastreo, "estado": e, "causaDevolucion": 3 }).to_string();
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado("LIQUIDADA")).await;
    assert_eq!((s, &r["estado"], &r["cambio"]), (StatusCode::OK, &json!("LIQUIDADA"), &json!(true)), "{r}");
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado("Exito")).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(false)), "repetición sin efecto: {r}");
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado("CANCELADA")).await;
    assert_eq!(s, StatusCode::CONFLICT, "{r}");
    assert_eq!(r["error"]["codigo"], "SPEI_ESTADO_INCONSISTENTE");
    let o = b.orden(o.id).await;
    assert_eq!(o.estado, "LIQUIDADA");
    assert!(o.revision_manual);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"), "sin reembolso");
    let (s, _) = b.enviar("/api/v1/spei/webhook/estado", estado("DEVUELTA"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// Hallazgo de la revisión: la devolución de una orden nuestra que llega como
/// SPEI entrante (tipo de pago 0) se aplica a la orden. Con el webhook de
/// estado y el abono de devolución, el cliente recibe el reembolso una vez.
#[sqlx::test(migrations = "../migrations")]
async fn devolucion_recibida_como_abono_reembolsa_una_vez(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_A, "300").await;
    b.despachar(2).await;
    assert_eq!(b.orden(o.id).await.estado, "LIQUIDADA");

    let devolucion = format!(
        r#"{{"fechaOperacion":{f},"institucionOrdenante":"40012","claveRastreo":"DEVBBVA0001","monto":300.00,
            "cuentaBeneficiario":"{clabe}","tipoPago":0,"claveRastreoDevolucion":"{rastreo}","causaDevolucion":1}}"#,
        f = hoy_cdmx().format("%Y%m%d"),
        clabe = ana.cuenta_ifpe.clabe,
        rastreo = o.clave_rastreo,
    );
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", devolucion.clone()).await;
    assert_eq!(
        (s, &r["mensaje"], &r["estado"]),
        (StatusCode::OK, &json!("confirmar"), &json!("DEVOLUCION_DE_ORDEN")),
        "{r}"
    );
    let estado = json!({ "claveRastreo": o.clave_rastreo, "estado": "DEVUELTA", "causaDevolucion": 1 }).to_string();
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(false)), "{r}");
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", devolucion).await;
    assert_eq!((s, &r["idempotente"]), (StatusCode::OK, &json!(true)), "{r}");
    assert_eq!(b.orden(o.id).await.estado, "DEVUELTA");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"), "un solo reembolso");
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'DEPOSITO'").await, 2);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_cuarentena").await, 0);
    verificar_invariantes(&pool).await;
}

/// Dos despachadores en paralelo no registran ni liquidan una orden dos veces.
#[sqlx::test(migrations = "../migrations")]
async fn despachadores_concurrentes(pool: PgPool) {
    let b = Arc::new(banco(&pool).await);
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "10000").await;
    for i in 0..30 {
        b.enviar_spei(&ana, &externa(100 + i), "10").await;
    }
    for _ in 0..4 {
        vencer_ordenes(&pool).await;
        let tareas: Vec<_> = (0..4)
            .map(|_| {
                let b = b.clone();
                tokio::spawn(async move { outbound::despachar(&b.pool, &*b.pasarela, &b.cfg.spei).await.unwrap() })
            })
            .collect();
        for t in tareas {
            t.await.unwrap();
        }
    }
    assert_eq!(b.pasarela.ordenes_registradas(), 30);
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_outbound WHERE estado = 'LIQUIDADA'").await, 30);
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Autoridad de los roles (base de datos)
// -----------------------------------------------------------------------------

async fn como(pool: &PgPool, rol: Rol, sql: &str) -> Result<(), String> {
    let mut tx = db::begin(pool, rol).await.unwrap();
    let r = sqlx::query(sql).execute(&mut *tx).await;
    let r = match r {
        Ok(_) => tx.commit().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    r
}

#[sqlx::test(migrations = "../migrations")]
async fn las_rutas_del_cliente_no_pueden_fabricar_spei(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let beto = cliente(&pool, &b.cfg, "Beto").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_A, "100").await;
    let denegado = |r: Result<(), String>| matches!(r, Err(e) if e.contains("permission denied"));

    // sc_ifpe (rutas del cliente): ni abonos SPEI, ni estados de órdenes, ni dueños de cuentas.
    assert!(denegado(
        como(
            &pool,
            Rol::Ifpe,
            "INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, \
             receptor_clabe, monto, estado, causa_devolucion, origen, raw_payload) \
             VALUES ('40012', 'X', current_date, '1', 1, 'DEVUELTO', 1, 'WEBHOOK', '{}')"
        )
        .await
    ));
    assert!(denegado(
        como(&pool, Rol::Ifpe, &format!("UPDATE ifpe.spei_outbound SET estado = 'ENVIADA' WHERE id = '{}'", o.id))
            .await
    ));
    assert!(denegado(
        como(
            &pool,
            Rol::Ifpe,
            &format!("UPDATE ifpe.cuentas SET cliente_id = '{}' WHERE id = '{}'", beto.cliente.id, ana.cuenta_ifpe.id)
        )
        .await
    ));
    assert!(denegado(como(&pool, Rol::Ifpe, "UPDATE ifpe.reserva_custodia SET activa = false").await));
    // SOFOM y puente no ven SPEI.
    for rol in [Rol::Sofom, Rol::Puente] {
        assert!(denegado(como(&pool, rol, "SELECT 1 FROM ifpe.spei_outbound").await));
        assert!(denegado(como(&pool, rol, "SELECT 1 FROM ifpe.spei_inbound").await));
    }
    // Ni el dueño del esquema cambia el dueño de una cuenta.
    let e = sqlx::query("UPDATE ifpe.cuentas SET cliente_id = $1 WHERE id = $2")
        .bind(beto.cliente.id)
        .bind(ana.cuenta_ifpe.id)
        .execute(&pool)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("SC_CUENTA_INMUTABLE") || e.contains("ux_ifpe_cuentas_cliente_moneda"), "{e}");
    // Un RETIRO sin orden no se confirma (lista blanca), ni con la reserva cuadrada.
    let e = como(
        &pool,
        Rol::Ifpe,
        &format!(
            "WITH c AS (UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 1 WHERE id = '{a}' RETURNING id), \
                  r AS (UPDATE ifpe.reserva_custodia SET saldo = saldo - 1 WHERE activa RETURNING id) \
             INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia) \
             SELECT 'RETIRO', c.id, r.id, 1, 'retiro-sin-orden' FROM c, r",
            a = ana.cuenta_ifpe.id
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("SC_SPEI_SIN_REGISTRO"), "{e}");
    verificar_invariantes(&pool).await;
}

/// Hallazgo de la revisión: un RETIRO anterior (ya pagado) no puede adoptarse
/// en una orden nueva para luego "rechazarla" y reembolsarlo.
#[sqlx::test(migrations = "../migrations")]
async fn no_se_adopta_un_retiro_anterior(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_A, "100").await;
    let e = como(
        &pool,
        Rol::Ifpe,
        &format!(
            "INSERT INTO ifpe.spei_outbound (cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud, \
             ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe, beneficiario_nombre, monto, concepto, \
             referencia_numerica, tx_retiro_id) \
             SELECT cliente_id, cuenta_id, clave_idempotencia, 'ADOPTADA1', fecha_solicitud, ordenante_nombre, \
                    ordenante_clabe, banco_receptor, beneficiario_clabe, beneficiario_nombre, monto, concepto, \
                    referencia_numerica, tx_retiro_id FROM ifpe.spei_outbound WHERE id = '{}'",
            o.id
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("duplicate key") || e.contains("SC_SPEI_DESCUADRE"), "{e}");
}

// -----------------------------------------------------------------------------
// Corrección de un abono erróneo
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn correccion_de_un_abono_erroneo(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let operador = cliente(&pool, &b.cfg, "Operador").await;
    let r = inbound::abonar(
        &pool,
        &b.cfg.pld,
        &b.cfg.spei,
        &abono(&ana.cuenta_ifpe.clabe, "10000", "CAPTURAERRONEA"),
        Origen::Operador(operador.cliente.id),
        SiNoAcreditable::Rechazar,
    )
    .await
    .unwrap();
    let reserva_antes = reserva_total(&pool).await;
    let corregir = |monto: &str, clave_c: &str| {
        let pool = pool.clone();
        let s = inbound::SolicitudCorreccion {
            spei_inbound_id: r.spei.id,
            monto: d(monto),
            motivo: "El pago real fue de 1,000; se capturó 10,000".into(),
            operador_id: operador.cliente.id,
            clave_idempotencia: clave_c.into(),
        };
        async move { inbound::corregir(&pool, &s).await }
    };
    let c = corregir("9000", "op:x:correccion-1").await.unwrap();
    assert!(!c.idempotente);
    assert!(corregir("9000", "op:x:correccion-1").await.unwrap().idempotente);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    assert_eq!(reserva_antes - reserva_total(&pool).await, d("9000"), "la reserva baja con el pasivo");
    let e = corregir("1000.01", "op:x:correccion-2").await.unwrap_err();
    assert_eq!(e.codigo(), "CORRECCION_EXCEDE_ABONO");
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Un día de tráfico interbancario intenso
// -----------------------------------------------------------------------------

/// Generador determinista (LCG) para que la prueba sea reproducible.
struct Azar(u64);

impl Azar {
    fn siguiente(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

/// Regla 1 bajo presión: abonos (con duplicados y destinos inválidos), envíos
/// (con rechazos, cancelaciones, devoluciones y tiempos agotados ambiguos),
/// transferencias internas e inversiones, en paralelo y con el despachador
/// corriendo. Al cerrar el día: reserva del 100%, cadena íntegra, ninguna
/// orden liquidada dos veces y el custodio conciliado con los libros.
#[sqlx::test(migrations = "../migrations")]
async fn dia_de_trafico_interbancario_intenso(pool: PgPool) {
    let b = Arc::new(banco(&pool).await);
    let mut clientes = Vec::new();
    for i in 0..8 {
        clientes.push(cliente(&pool, &b.cfg, &format!("Cliente{i}")).await);
    }
    let clientes = Arc::new(clientes);
    // Destinos externos con distintos comportamientos del proveedor.
    let destinos: Vec<String> = (0..10).map(|i| externa(900 + i)).collect();
    b.pasarela.programar(&destinos[1], Comportamiento::RechazarAlRegistrar { causa: 1 });
    b.pasarela.programar(&destinos[2], Comportamiento::Cancelar { causa: 3 });
    b.pasarela.programar(&destinos[3], Comportamiento::DevolverTrasLiquidar { causa: 1 });
    b.pasarela.programar(&destinos[4], Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: true });
    b.pasarela.programar(&destinos[5], Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: false });
    b.pasarela.programar(&destinos[6], Comportamiento::TiempoAgotadoAlConsultar { veces: 2 });
    let destinos = Arc::new(destinos);
    b.pasarela.fijar_saldo_custodio(reserva_total(&pool).await);
    let barridos_sofom = Arc::new(tokio::sync::Mutex::new(Decimal::ZERO));

    let mut tareas = Vec::new();
    for hilo in 0..6u64 {
        let (b, clientes, destinos, barridos) = (b.clone(), clientes.clone(), destinos.clone(), barridos_sofom.clone());
        tareas.push(tokio::spawn(async move {
            let mut azar = Azar(0x5EED + hilo);
            for paso in 0..40u64 {
                let c = &clientes[azar.siguiente(clientes.len() as u64) as usize];
                let monto = format!("{}.{:02}", 10 + azar.siguiente(5_000), azar.siguiente(100));
                match azar.siguiente(10) {
                    // Abono SPEI (a veces repetido, a veces a una CLABE ajena).
                    0..=3 => {
                        let rastreo = format!("H{hilo}P{paso}");
                        let destino =
                            if azar.siguiente(8) == 0 { externa(5_000 + paso) } else { c.cuenta_ifpe.clabe.clone() };
                        let cuerpo = cuerpo_abono(&destino, &monto, "40014", &rastreo, hoy_cdmx());
                        let veces = 1 + azar.siguiente(3);
                        for _ in 0..veces {
                            let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo.clone()).await;
                            assert_eq!(s, StatusCode::OK, "{r}");
                            // El dinero llega al custodio una vez; si se pide devolverlo, el proveedor lo regresa.
                            if r["mensaje"] == "confirmar" && r["idempotente"] == false {
                                b.pasarela.recibir(d(&monto));
                            }
                        }
                    }
                    // SPEI saliente.
                    4..=6 => {
                        let destino = &destinos[azar.siguiente(destinos.len() as u64) as usize];
                        let s = SolicitudSpei {
                            cliente_id: c.cliente.id,
                            clabe_destino: destino.clone(),
                            beneficiario_nombre: Some("Proveedor Externo".into()),
                            beneficiario_rfc_curp: None,
                            monto: d(&monto),
                            concepto: None,
                            referencia_numerica: None,
                            clave_idempotencia: clave(),
                        };
                        match outbound::solicitar(&b.pool, &b.cfg.pld, &b.cfg.spei, &s).await {
                            Ok(_) => {}
                            Err(e) => assert_eq!(e.codigo(), "SALDO_INSUFICIENTE", "{e:?}"),
                        }
                    }
                    // Transferencia interna.
                    7 => {
                        let otro = &clientes[azar.siguiente(clientes.len() as u64) as usize];
                        if otro.cliente.id != c.cliente.id {
                            let t = synthetic_capital::ledger::Transferencia {
                                cliente_origen_id: c.cliente.id,
                                clabe_destino: otro.cuenta_ifpe.clabe.clone(),
                                monto: d(&monto),
                                concepto: None,
                                clave_idempotencia: clave(),
                            };
                            if let Err(e) = synthetic_capital::ledger::transferir(&b.pool, &b.cfg.pld, &t).await {
                                assert_eq!(e.codigo(), "SALDO_INSUFICIENTE", "{e:?}");
                            }
                        }
                    }
                    // Inversión en mutuo (el efectivo se barre a la caja de la SOFOM).
                    8 => {
                        let s = puente::SolicitudMutuo {
                            cliente_id: c.cliente.id,
                            monto: d(&monto),
                            plazo_dias: 91,
                            clave_idempotencia: clave(),
                        };
                        match puente::invertir(&b.pool, &b.cfg.pld, &s).await {
                            Ok(r) => *barridos.lock().await += r.contrato.principal,
                            Err(e) => assert_eq!(e.codigo(), "SALDO_INSUFICIENTE", "{e:?}"),
                        }
                    }
                    // El despachador corre en medio del tráfico.
                    _ => {
                        outbound::despachar(&b.pool, &*b.pasarela, &b.cfg.spei).await.unwrap();
                    }
                }
            }
        }));
    }
    for t in tareas {
        t.await.unwrap();
    }
    // Cierre del día: el despachador procesa todo lo pendiente.
    b.despachar(12).await;

    let barridos = *barridos_sofom.lock().await;
    let reserva = reserva_total(&pool).await;
    let en_transito: Decimal = sqlx::query_scalar(
        "SELECT coalesce(sum(monto), 0) FROM ifpe.spei_outbound WHERE estado IN ('PENDIENTE', 'ENVIADA')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        b.pasarela.saldo_custodio(),
        reserva + en_transito + barridos,
        "custodio = reserva contable + órdenes en tránsito + efectivo barrido a la SOFOM"
    );
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1, "ninguna orden se pagó dos veces");
    let estados: Vec<(String, i64)> =
        sqlx::query_as("SELECT estado, count(*) FROM ifpe.spei_outbound GROUP BY estado ORDER BY estado")
            .fetch_all(&pool)
            .await
            .unwrap();
    let reversos =
        contar(&pool, "SELECT count(*) FROM ifpe.transacciones WHERE clave_idempotencia LIKE 'spei-reverso:%'").await;
    let reembolsables =
        contar(&pool, "SELECT count(*) FROM ifpe.spei_outbound WHERE estado IN ('RECHAZADA', 'DEVUELTA')").await;
    assert_eq!(reversos, reembolsables, "un reverso por orden rechazada o devuelta: {estados:?}");
    let abonos = contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound WHERE estado = 'ABONADO'").await;
    let depositos =
        contar(&pool, "SELECT count(*) FROM ifpe.transacciones WHERE clave_idempotencia LIKE 'spei:%'").await;
    assert_eq!(abonos, depositos);
    let conciliacion = outbound::conciliacion(&pool).await.unwrap();
    assert_eq!(conciliacion.iter().map(|c| c.reserva_contable).sum::<Decimal>(), reserva);
    verificar_invariantes(&pool).await;
}
