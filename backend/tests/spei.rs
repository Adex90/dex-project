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

    /// GET autenticado a la API.
    async fn obtener(&self, token: &str, ruta: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .uri(ruta)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
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

// -----------------------------------------------------------------------------
// Regresiones de la revisión de 0005 (migración 0007)
// -----------------------------------------------------------------------------

/// Pasarela cuyo primer registro se detiene hasta que la prueba lo libera: un
/// despachador lento cuyo arrendamiento vence a media llamada.
struct PasarelaLenta {
    interna: Arc<PasarelaSimulada>,
    frenar: AtomicBool,
    detenida: tokio::sync::Notify,
    liberar: tokio::sync::Notify,
}

impl Pasarela for PasarelaLenta {
    fn nombre(&self) -> &'static str {
        "lenta"
    }

    fn registrar<'a>(&'a self, orden: &'a OrdenPasarela) -> Futuro<'a, Result<RegistroPasarela, ErrorPasarela>> {
        Box::pin(async move {
            if self.frenar.swap(false, Ordering::SeqCst) {
                self.detenida.notify_one();
                self.liberar.notified().await;
            }
            self.interna.registrar(orden).await
        })
    }

    fn consultar<'a>(&'a self, consulta: &'a ConsultaOrden) -> Futuro<'a, Result<EstadoProveedor, ErrorPasarela>> {
        self.interna.consultar(consulta)
    }
}

async fn contar_reversos(pool: &PgPool) -> i64 {
    contar(pool, "SELECT count(*) FROM ifpe.transacciones WHERE clave_idempotencia LIKE 'spei-reverso:%'").await
}

/// Notificación de un SPEI entrante con tipo de pago y, si se indica, la
/// clave de rastreo del pago que se devuelve.
fn cuerpo_devolucion(clabe: &str, monto: &str, emisor: &str, rastreo: &str, tipo: i32, devuelto: Option<&str>) -> String {
    let devuelto = devuelto.map(|r| format!(r#","claveRastreoDevolucion":"{r}""#)).unwrap_or_default();
    format!(
        r#"{{"fechaOperacion":{f},"institucionOrdenante":"{emisor}","claveRastreo":"{rastreo}","monto":{monto},
            "cuentaBeneficiario":"{clabe}","tipoPago":{tipo},"causaDevolucion":1{devuelto}}}"#,
        f = hoy_cdmx().format("%Y%m%d"),
    )
}

/// Suma, sobre todas las reservas, de una columna entera de la conciliación.
async fn conciliacion_total(pool: &PgPool, campo: fn(&outbound::Conciliacion) -> i64) -> i64 {
    outbound::conciliacion(pool).await.unwrap().iter().map(campo).sum()
}

/// outbound-money-1: el despachador A reclama la orden (intento 1) y tarda
/// en llamar al proveedor; su arrendamiento vence, B la reclama (intento 2),
/// consulta "no existe", reenvía y el proveedor la acepta. El "rechazado por
/// duplicado" que A recibe después ya no reembolsa (antes: RECHAZADA con el
/// cliente reembolsado mientras el proveedor pagaba = pago doble).
#[sqlx::test(migrations = "../migrations")]
async fn arrendamiento_vencido_rechazo_tardio_no_reembolsa(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, &externa(10), "100").await;
    let lenta = Arc::new(PasarelaLenta {
        interna: b.pasarela.clone(),
        frenar: AtomicBool::new(true),
        detenida: tokio::sync::Notify::new(),
        liberar: tokio::sync::Notify::new(),
    });
    let despachador_a = {
        let (pool, cfg, lenta) = (pool.clone(), b.cfg.spei.clone(), lenta.clone());
        tokio::spawn(async move { outbound::despachar(&pool, &*lenta, &cfg).await.unwrap() })
    };
    lenta.detenida.notified().await;
    assert_eq!(b.orden(o.id).await.intentos_envio, 1, "A reclamó la orden");

    b.despachar(1).await; // B: el arrendamiento de A venció
    let tomada = b.orden(o.id).await;
    assert_eq!((tomada.estado.as_str(), tomada.intentos_envio), ("ENVIADA", 2));
    lenta.liberar.notify_one();
    let a = despachador_a.await.unwrap();
    assert_eq!(a.errores, 0, "{a:?}");

    let o2 = b.orden(o.id).await;
    assert_eq!(o2.estado, "ENVIADA", "{o2:?}");
    assert!(o2.revision_manual && o2.tx_reverso_id.is_none(), "{o2:?}");
    assert!(o2.ultimo_error.as_deref().unwrap_or_default().starts_with("REVISION_MANUAL"), "{o2:?}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"), "sin reembolso");
    assert_eq!(contar_reversos(&pool).await, 0);

    b.despachar(3).await;
    let final_ = b.orden(o.id).await;
    assert_eq!(final_.estado, "LIQUIDADA");
    assert!(!final_.revision_manual);
    assert_eq!(b.pasarela.ordenes_registradas(), 1);
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"));
    verificar_invariantes(&pool).await;
}

/// outbound-money-1, el otro orden de llegada: B ya reclamó la orden (y aún
/// no llama al proveedor) cuando llega el rechazo tardío de A. Va a revisión;
/// la aceptación de B la manda después a ENVIADA, sin reembolso ni incidente.
#[sqlx::test(migrations = "../migrations")]
async fn rechazo_tardio_mientras_otro_reclamo_esta_en_vuelo(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_A, "100").await;
    sqlx::query("UPDATE ifpe.spei_outbound SET intentos_envio = 2 WHERE id = $1").bind(o.id).execute(&pool).await.unwrap();
    let tardio = Evento::RechazoRegistro { causa: 99, motivo: "clave de rastreo duplicada".into(), intentos_envio: 1 };
    let t = outbound::aplicar(&pool, &b.cfg.spei, o.id, &tardio).await.unwrap();
    assert_eq!(t.orden.estado, "PENDIENTE");
    assert!(t.orden.revision_manual && t.orden.tx_reverso_id.is_none());
    let aceptada = Evento::Aceptada { id_proveedor: Some("X1".into()), fecha_operacion: None };
    let t = outbound::aplicar(&pool, &b.cfg.spei, o.id, &aceptada).await.unwrap();
    assert_eq!(t.orden.estado, "ENVIADA");
    // Un rechazo tardío sobre una orden ya liquidada no cambia nada.
    let t = outbound::aplicar(&pool, &b.cfg.spei, o.id, &Evento::Liquidada { fecha_operacion: None }).await.unwrap();
    assert_eq!(t.orden.estado, "LIQUIDADA");
    let t = outbound::aplicar(&pool, &b.cfg.spei, o.id, &tardio).await.unwrap();
    assert!(!t.cambio && !t.orden.revision_manual);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"));
    assert_eq!(contar_reversos(&pool).await, 0);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_incidentes").await, 0);
    verificar_invariantes(&pool).await;
}

/// outbound-money-2: una orden en revisión manual que el proveedor nunca
/// recibió (caída del proveedor; o rechazo real en el reenvío) ya no queda
/// varada: el operador la reembolsa, auditado, por la ruta de operaciones. La
/// causa original de la revisión se conserva.
#[sqlx::test(migrations = "../migrations")]
async fn revision_manual_se_resuelve_con_reembolso_del_operador(pool: PgPool) {
    let b = banco_con(&pool, |c| c.spei.espera_resolucion_seg = 0).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let op = operador(&pool).await;

    // (a) El proveedor no responde durante todos los intentos.
    let caida = externa(20);
    b.pasarela.programar(&caida, Comportamiento::TiempoAgotadoAlRegistrar { veces: 10_000, registra: false });
    let o = b.enviar_spei(&ana, &caida, "100").await;
    b.despachar(b.cfg.spei.max_intentos as usize).await;
    let marcada = b.orden(o.id).await;
    assert_eq!(marcada.estado, "PENDIENTE");
    assert!(marcada.revision_manual, "{marcada:?}");
    let causa = marcada.ultimo_error.clone().unwrap();
    assert!(causa.starts_with("REVISION_MANUAL: sin resultado definitivo tras 20 intentos"), "{causa}");
    b.despachar(3).await;
    assert_eq!(b.orden(o.id).await.ultimo_error.as_deref(), Some(causa.as_str()), "la causa original se conserva");

    // (b) Sin registro en el primer intento y rechazo real en el reenvío.
    let rechazo = externa(21);
    b.pasarela.programar(&rechazo, Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: false });
    b.pasarela.programar(&rechazo, Comportamiento::RechazarAlRegistrar { causa: 1 });
    let o_b = b.enviar_spei(&ana, &rechazo, "200").await;
    b.despachar(2).await;
    let marcada_b = b.orden(o_b.id).await;
    assert!(marcada_b.revision_manual && marcada_b.estado == "PENDIENTE", "{marcada_b:?}");
    b.despachar(3).await;
    let causa_b = b.orden(o_b.id).await.ultimo_error.unwrap();
    assert!(causa_b.contains("causa 1"), "{causa_b}");
    assert_eq!(b.pasarela.ordenes_registradas(), 0);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("700"), "dinero congelado, sin salir del custodio");
    let custodio_antes: Decimal =
        outbound::conciliacion(&pool).await.unwrap().iter().map(|c| c.saldo_esperado_custodio).sum();

    let ruta = |id: Uuid| format!("/api/v1/operaciones/spei/enviados/{id}/reembolso");
    let cuerpo = json!({ "motivo": "STP confirma que nunca recibió la orden", "clave_idempotencia": "reembolso-0001" });
    let (s, _) = b.llamar(&b.token(ana.cliente.id, RolUsuario::Cliente), &ruta(o.id), cuerpo.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "solo operadores");
    let token = b.token(op, RolUsuario::Operador);
    let (s, r) = b.llamar(&token, &ruta(o.id), cuerpo.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["orden"]["estado"], "RECHAZADA");
    assert_eq!(r["resolucion"]["error_previo"], json!(causa));
    assert_eq!(r["resolucion"]["autorizado_por"], json!(op));
    let (s, r) = b.llamar(&token, &ruta(o.id), cuerpo).await;
    assert_eq!((s, &r["idempotente"]), (StatusCode::OK, &json!(true)), "{r}");
    let otra = json!({ "motivo": "STP confirma que nunca recibió la orden", "clave_idempotencia": "reembolso-0002" });
    let (s, r) = b.llamar(&token, &ruta(o.id), otra).await;
    assert_eq!((s, &r["error"]["codigo"]), (StatusCode::CONFLICT, &json!("SPEI_YA_RESUELTA")), "{r}");

    let cuerpo_b = json!({ "motivo": "rechazo real confirmado con STP", "clave_idempotencia": "reembolso-0003" });
    let (s, r) = b.llamar(&token, &ruta(o_b.id), cuerpo_b).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert!(r["resolucion"]["error_previo"].as_str().unwrap().contains("causa 1"), "{r}");

    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    assert_eq!(contar_reversos(&pool).await, 2);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_resoluciones").await, 2);
    b.despachar(3).await;
    assert_eq!(b.orden(o.id).await.estado, "RECHAZADA");
    let custodio: Decimal = outbound::conciliacion(&pool).await.unwrap().iter().map(|c| c.saldo_esperado_custodio).sum();
    assert_eq!(custodio, custodio_antes, "el dinero nunca salió del custodio");
    verificar_invariantes(&pool).await;
}

/// outbound-money-2: el reembolso del operador exige revisión manual, reposo
/// del despachador y que el proveedor, consultado en ese momento, no tenga la
/// orden. Nunca reembolsa una orden que el proveedor sí registró.
#[sqlx::test(migrations = "../migrations")]
async fn reembolso_del_operador_exige_revision_reposo_y_proveedor_sin_orden(pool: PgPool) {
    let b = banco_con(&pool, |c| c.spei.max_intentos = 2).await; // reposo por omisión: 900 s
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let op = operador(&pool).await;
    let resolver = |id: Uuid| {
        let (pool, pasarela, cfg) = (pool.clone(), b.pasarela.clone(), b.cfg.spei.clone());
        async move {
            let s = SolicitudResolucion {
                orden_id: id,
                motivo: "STP confirma que no tiene la orden".into(),
                operador_id: op,
                clave_idempotencia: clave(),
            };
            outbound::reembolsar_revision(&pool, &*pasarela, &cfg, &s).await.unwrap_err().codigo().to_owned()
        }
    };
    let nueva = b.enviar_spei(&ana, EXTERNA_A, "100").await;
    assert_eq!(resolver(nueva.id).await, "SPEI_SIN_REVISION");

    // Registrada pero oculta: el reenvío da "duplicado" (revisión) y la
    // consulta del operador ya la encuentra.
    let ambigua = externa(22);
    b.pasarela.programar(&ambigua, Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: true });
    b.pasarela.programar(&ambigua, Comportamiento::OcultarEnConsulta { veces: 1 });
    let o = b.enviar_spei(&ana, &ambigua, "300").await;
    let caida = externa(23);
    b.pasarela.programar(&caida, Comportamiento::TiempoAgotadoAlRegistrar { veces: 10_000, registra: false });
    let o2 = b.enviar_spei(&ana, &caida, "200").await;
    b.despachar(2).await;
    assert!(b.orden(o.id).await.revision_manual);
    assert!(b.orden(o2.id).await.revision_manual);
    assert_eq!(resolver(o.id).await, "SPEI_PROVEEDOR_TIENE_ORDEN");
    assert_eq!(resolver(o2.id).await, "SPEI_ORDEN_RECIENTE", "el despachador la tocó hace menos de 900 s");
    assert_eq!(contar_reversos(&pool).await, 0);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_resoluciones").await, 0);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("400"));
    b.despachar(1).await;
    assert_eq!(b.orden(o.id).await.estado, "LIQUIDADA");
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);

    // La base de datos tampoco acepta una resolución sin su reembolso.
    let e = como(
        &pool,
        Rol::Spei,
        &format!(
            "INSERT INTO ifpe.spei_resoluciones (spei_outbound_id, accion, motivo, error_previo, autorizado_por, \
             clave_idempotencia) SELECT id, 'REEMBOLSO', 'sin reembolso en esta transacción', ultimo_error, '{op}', \
             'x-1' FROM ifpe.spei_outbound WHERE id = '{}'",
            o2.id
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("SC_SPEI_DESCUADRE"), "{e}");
    verificar_invariantes(&pool).await;
}

/// outbound-money-3: el primer registro agota el tiempo pero el proveedor sí
/// lo registró para el día D; la consulta aún no lo muestra y el proveedor ya
/// pasó al día D+1. Reenviar se aceptaría como un pago nuevo (pago doble):
/// ahora va a revisión manual y se liquida una sola vez.
#[sqlx::test(migrations = "../migrations")]
async fn reenvio_tras_cambio_de_dia_va_a_revision(pool: PgPool) {
    // El corte cae dentro de la ventana [creación - margen, ahora + margen].
    let b = banco_con(&pool, |c| c.spei.hora_corte = ahora_cdmx() + chrono::Duration::minutes(2)).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let destino = externa(30);
    b.pasarela.programar(&destino, Comportamiento::TiempoAgotadoAlRegistrar { veces: 1, registra: true });
    b.pasarela.programar(&destino, Comportamiento::OcultarEnConsulta { veces: 3 });
    let o = b.enviar_spei(&ana, &destino, "400").await;
    b.despachar(1).await;
    assert_eq!(b.orden(o.id).await.intentos_envio, 1);
    assert_eq!(b.pasarela.ordenes_registradas(), 1);
    b.pasarela.fijar_fecha_operacion(fecha_operacion_simulada(o.fecha_solicitud) + Days::new(1));

    b.despachar(1).await;
    let o2 = b.orden(o.id).await;
    assert_eq!(o2.estado, "PENDIENTE", "{o2:?}");
    assert!(o2.revision_manual, "{o2:?}");
    assert!(o2.ultimo_error.as_deref().unwrap_or_default().contains("día de operación"), "{o2:?}");
    assert_eq!(b.pasarela.ordenes_registradas(), 1, "ningún reenvío llegó al proveedor");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("600"));

    b.despachar(4).await; // la consulta acaba por mostrar el registro original
    let final_ = b.orden(o.id).await;
    assert_eq!(final_.estado, "LIQUIDADA", "{final_:?}");
    assert!(!final_.revision_manual && final_.tx_reverso_id.is_none());
    assert_eq!(b.pasarela.ordenes_registradas(), 1);
    assert_eq!(b.pasarela.max_liquidaciones_por_orden(), 1);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("600"));
    verificar_invariantes(&pool).await;
}

/// outbound-money-4: una devolución que no corresponde a ninguna orden (sin
/// clave de rastreo original, clave desconocida, otro monto u otro banco) ya
/// no recibe un 409 sin registro: queda en cuarentena y se confirma (SPEI no
/// permite devolver una devolución). Nunca reembolsa por sí sola.
#[sqlx::test(migrations = "../migrations")]
async fn devolucion_sin_orden_va_a_cuarentena_y_se_confirma(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_A, "300").await;
    b.despachar(2).await;
    assert_eq!(b.orden(o.id).await.estado, "LIQUIDADA");
    let reserva = reserva_total(&pool).await;
    let recibidos = contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound").await;
    let clabe = &ana.cuenta_ifpe.clabe;
    let casos = [
        cuerpo_devolucion(clabe, "300.00", "40012", "DEVSINRASTREO1", 0, None),
        cuerpo_devolucion(clabe, "300.00", "40012", "DEVDESCONOCIDA1", 0, Some("NOEXISTE0001")),
        cuerpo_devolucion(clabe, "299.99", "40012", "DEVOTROMONTO1", 0, Some(&o.clave_rastreo)),
        cuerpo_devolucion(clabe, "300.00", "40014", "DEVOTROBANCO1", 0, Some(&o.clave_rastreo)),
    ];
    for cuerpo in casos.iter().chain(std::iter::once(&casos[0])) {
        let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo.clone()).await;
        assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("CUARENTENA")), "{r}");
    }
    assert_eq!(
        contar(
            &pool,
            "SELECT count(*) FROM ifpe.spei_cuarentena WHERE tipo = 'ABONO' AND error LIKE 'SPEI_DEVOLUCION_SIN_ORDEN%'"
        )
        .await,
        5,
        "cada entrega autenticada queda registrada"
    );
    assert_eq!(
        contar(&pool, "SELECT count(*) FROM ifpe.spei_cuarentena WHERE error LIKE '%NOEXISTE0001%'").await,
        1
    );
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound").await, recibidos);
    let o2 = b.orden(o.id).await;
    assert_eq!(o2.estado, "LIQUIDADA");
    assert!(o2.tx_reverso_id.is_none());
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("700"));
    assert_eq!(reserva_total(&pool).await, reserva);
    // El proveedor confirma la devolución por el webhook de estado: un solo reembolso.
    let estado = json!({ "claveRastreo": o.clave_rastreo, "estado": "DEVUELTA", "causaDevolucion": 1 }).to_string();
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(true)), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    verificar_invariantes(&pool).await;
}

/// outbound-money-5: la conciliación cuenta las órdenes LIQUIDADA marcadas
/// para revisión (antes las ocultaba) sin mover las columnas de dinero.
#[sqlx::test(migrations = "../migrations")]
async fn conciliacion_cuenta_liquidadas_en_revision(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let o = b.enviar_spei(&ana, EXTERNA_B, "100").await;
    b.despachar(1).await;
    let estado = |e: &str| json!({ "claveRastreo": o.clave_rastreo, "estado": e, "causaDevolucion": 3 }).to_string();
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado("LIQUIDADA")).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(true)), "{r}");
    let base = outbound::conciliacion(&pool).await.unwrap();
    assert_eq!(conciliacion_total(&pool, |c| c.ordenes_en_revision).await, 0);

    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado("CANCELADA")).await;
    assert_eq!((s, &r["error"]["codigo"]), (StatusCode::CONFLICT, &json!("SPEI_ESTADO_INCONSISTENTE")), "{r}");
    let o2 = b.orden(o.id).await;
    assert!(o2.estado == "LIQUIDADA" && o2.revision_manual, "{o2:?}");
    assert_eq!(conciliacion_total(&pool, |c| c.ordenes_en_revision).await, 1, "la orden en revisión se ve");
    let ahora = outbound::conciliacion(&pool).await.unwrap();
    for (a, c) in base.iter().zip(&ahora) {
        assert_eq!(
            (a.pendiente_de_envio, a.enviado_sin_confirmar, a.ordenes_en_transito, a.saldo_esperado_custodio),
            (c.pendiente_de_envio, c.enviado_sin_confirmar, c.ordenes_en_transito, c.saldo_esperado_custodio),
            "las columnas de dinero no cambian"
        );
    }
    b.despachar(1).await; // vigilancia: sigue marcada
    assert_eq!(conciliacion_total(&pool, |c| c.ordenes_en_revision).await, 1);
    let (s, _) = b.webhook("/api/v1/spei/webhook/estado", estado("DEVUELTA")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(conciliacion_total(&pool, |c| c.ordenes_en_revision).await, 0);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    verificar_invariantes(&pool).await;
}

/// outbound-money-6: "liquidada" de una orden ya reembolsada (RECHAZADA) era
/// solo una línea de log. Ahora queda en `ifpe.spei_incidentes` (una vez,
/// aunque el proveedor reintente) y en la conciliación; no se mueve dinero.
#[sqlx::test(migrations = "../migrations")]
async fn incidente_en_orden_rechazada_queda_registrado(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let destino = externa(40);
    b.pasarela.programar(&destino, Comportamiento::RechazarAlRegistrar { causa: 1 });
    let o = b.enviar_spei(&ana, &destino, "100").await;
    b.despachar(2).await;
    let rechazada = b.orden(o.id).await;
    assert_eq!(rechazada.estado, "RECHAZADA");
    for e in ["LIQUIDADA", "LIQUIDADA", "Exito", "LQ"] {
        let cuerpo = json!({ "claveRastreo": o.clave_rastreo, "estado": e }).to_string();
        let (s, r) = b.webhook("/api/v1/spei/webhook/estado", cuerpo).await;
        assert_eq!((s, &r["error"]["codigo"]), (StatusCode::CONFLICT, &json!("SPEI_ESTADO_INCONSISTENTE")), "{r}");
    }
    // Lo mismo por el camino del despachador (una foto vieja: "aceptada").
    let aceptada = Evento::Aceptada { id_proveedor: Some("X".into()), fecha_operacion: None };
    let e = outbound::aplicar(&pool, &b.cfg.spei, o.id, &aceptada).await.unwrap_err();
    assert_eq!(e.codigo(), "SPEI_ESTADO_INCONSISTENTE");
    let o2 = b.orden(o.id).await;
    assert_eq!((o2.estado.as_str(), o2.tx_reverso_id, o2.id_proveedor), ("RECHAZADA", rechazada.tx_reverso_id, None));
    let incidentes: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT estado_orden, evento, detalle FROM ifpe.spei_incidentes WHERE spei_outbound_id = $1 ORDER BY evento",
    )
    .bind(o.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(incidentes.len(), 2, "{incidentes:?}");
    assert_eq!((incidentes[0].0.as_str(), incidentes[0].1.as_str()), ("RECHAZADA", "ACEPTADA"));
    assert_eq!((incidentes[1].0.as_str(), incidentes[1].1.as_str()), ("RECHAZADA", "LIQUIDADA"));
    assert!(incidentes[1].2.contains("Liquidada"));
    assert_eq!(conciliacion_total(&pool, |c| c.incidentes_conciliacion).await, 1);
    let op = operador(&pool).await;
    let (_, cuerpo) = b.obtener(&b.token(op, RolUsuario::Operador), "/api/v1/spei/conciliacion").await;
    let total: i64 = cuerpo.as_array().unwrap().iter().map(|c| c["incidentes_conciliacion"].as_i64().unwrap()).sum();
    assert_eq!(total, 1, "{cuerpo}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"), "no se movió dinero");

    // Solo sc_spei registra incidentes, y nadie los modifica.
    let denegado = |r: Result<(), String>| matches!(r, Err(e) if e.contains("permission denied"));
    let insertar = format!(
        "INSERT INTO ifpe.spei_incidentes (spei_outbound_id, estado_orden, evento, detalle) \
         VALUES ('{}', 'RECHAZADA', 'DEVUELTA', 'x')",
        o.id
    );
    assert!(denegado(como(&pool, Rol::Ifpe, &insertar).await));
    assert!(denegado(como(&pool, Rol::Sofom, "SELECT 1 FROM ifpe.spei_incidentes").await));
    assert!(denegado(como(&pool, Rol::Spei, "UPDATE ifpe.spei_incidentes SET detalle = 'y'").await));
    let e = sqlx::query("DELETE FROM ifpe.spei_incidentes").execute(&pool).await.unwrap_err().to_string();
    assert!(e.contains("SC_LEDGER_INMUTABLE") || e.contains("inmutable"), "{e}");
    verificar_invariantes(&pool).await;
}

/// inbound-security-1: la devolución de una orden nuestra con un tipo de pago
/// de devolución distinto de 0 (17, 18...) se abonaba como depósito nuevo y
/// luego la orden se reembolsaba también (cliente acreditado dos veces).
/// Ahora se aplica a la orden en cualquier orden de llegada, también cuando la
/// captura el operador, y la base de datos rechaza un ABONADO que sea devolución.
#[sqlx::test(migrations = "../migrations")]
async fn devolucion_de_cualquier_tipo_reembolsa_una_vez(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    depositar(&pool, &b.cfg, ana.cuenta_ifpe.id, "1000").await;
    let clabe = ana.cuenta_ifpe.clabe.clone();
    let o1 = b.enviar_spei(&ana, EXTERNA_A, "300").await;
    let o2 = b.enviar_spei(&ana, EXTERNA_A, "200").await;
    let o3 = b.enviar_spei(&ana, EXTERNA_A, "100").await;
    b.despachar(2).await;
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("400"));
    let estado = |o: &SpeiOutbound| {
        json!({ "claveRastreo": o.clave_rastreo, "estado": "DEVUELTA", "causaDevolucion": 1 }).to_string()
    };

    // Tipo 17: primero el abono de devolución, luego el webhook de estado.
    let dev1 = cuerpo_devolucion(&clabe, "300.00", "40012", "DEVBBVA17", 17, Some(&o1.clave_rastreo));
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", dev1.clone()).await;
    assert_eq!(
        (s, &r["mensaje"], &r["estado"]),
        (StatusCode::OK, &json!("confirmar"), &json!("DEVOLUCION_DE_ORDEN")),
        "{r}"
    );
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado(&o1)).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(false)), "{r}");
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", dev1).await;
    assert_eq!((s, &r["idempotente"]), (StatusCode::OK, &json!(true)), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("700"), "un solo reembolso de 300");

    // Tipo 18: primero el webhook de estado, luego el abono de devolución.
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado(&o2)).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(true)), "{r}");
    let dev2 = cuerpo_devolucion(&clabe, "200.00", "40012", "DEVBBVA18", 18, Some(&o2.clave_rastreo));
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", dev2).await;
    assert_eq!((s, &r["estado"]), (StatusCode::OK, &json!("DEVOLUCION_DE_ORDEN")), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("900"));

    // Captura del operador de la devolución de la tercera orden.
    let op = operador(&pool).await;
    let token = b.token(op, RolUsuario::Operador);
    let captura = |devuelto: &str| {
        json!({
            "clabe": clabe, "monto": "100", "banco_emisor": "40012", "fecha_operacion": hoy_cdmx(),
            "clave_rastreo": "DEVBBVAOPER", "clave_rastreo_devolucion": devuelto,
        })
    };
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", captura("MAL-FORMADA")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{r}");
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", captura(&o3.clave_rastreo)).await;
    assert_eq!((s, &r["spei"]["estado"]), (StatusCode::CREATED, &json!("DEVOLUCION_DE_ORDEN")), "{r}");
    let (s, r) = b.webhook("/api/v1/spei/webhook/estado", estado(&o3)).await;
    assert_eq!((s, &r["cambio"]), (StatusCode::OK, &json!(false)), "{r}");

    for o in [&o1, &o2, &o3] {
        assert_eq!(b.orden(o.id).await.estado, "DEVUELTA");
    }
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"), "cada orden se reembolsó una vez");
    assert_eq!(contar_reversos(&pool).await, 3);
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_inbound WHERE estado = 'DEVOLUCION_DE_ORDEN'").await, 3);

    // La base de datos rechaza un ABONADO que sea una devolución.
    for (tipo, payload) in [("17", "{}"), ("1", r#"{"claveRastreoDevolucion":"ABC123"}"#)] {
        let e = como(
            &pool,
            Rol::Spei,
            &format!(
                "INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, tipo_pago, \
                 receptor_clabe, cuenta_id, monto, estado, transaccion_id, origen, raw_payload) \
                 VALUES ('40012', 'DEVCOMOABONO', current_date, {tipo}, '{clabe}', '{}', 1, 'ABONADO', \
                         gen_random_uuid(), 'WEBHOOK', '{payload}')",
                ana.cuenta_ifpe.id
            ),
        )
        .await
        .unwrap_err();
        assert!(e.contains("ck_spei_inbound_abonado_no_es_devolucion"), "{e}");
    }
    verificar_invariantes(&pool).await;
}

/// inbound-security-2: un pagador que reutiliza su clave de rastreo cada
/// semana (o cada día) recibía 409 SPEI_POSIBLE_DUPLICADO en el segundo pago,
/// sin registro ni salida. Dos identidades informadas por el proveedor son
/// dos pagos: se abonan.
#[sqlx::test(migrations = "../migrations")]
async fn webhooks_recurrentes_con_mismo_rastreo_se_abonan(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let clabe = &ana.cuenta_ifpe.clabe;
    let reserva = reserva_total(&pool).await;
    let pagos = [
        ("90646", "PAGOSEMANAL", hoy_cdmx() - Days::new(7)),
        ("90646", "PAGOSEMANAL", hoy_cdmx()),
        ("90646", "PAGODIARIO", hoy_cdmx() - Days::new(1)),
        ("90646", "PAGODIARIO", hoy_cdmx()),
        ("40646", "PAGODIARIO", hoy_cdmx()),
    ];
    for (emisor, rastreo, fecha) in pagos {
        let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo_abono(clabe, "500", emisor, rastreo, fecha)).await;
        assert_eq!(
            (s, &r["mensaje"], &r["estado"], &r["idempotente"]),
            (StatusCode::OK, &json!("confirmar"), &json!("ABONADO"), &json!(false)),
            "{emisor} {rastreo} {fecha}: {r}"
        );
    }
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("2500"));
    assert_eq!(reserva_total(&pool).await - reserva, d("2500"));
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.spei_cuarentena").await, 0);
    verificar_invariantes(&pool).await;
}

/// inbound-security-2: frente a una captura del operador, el posible
/// duplicado se retiene (confirmado y registrado, nunca un 409 sin registro)
/// y el operador, auditado, puede declarar que el pago es distinto.
#[sqlx::test(migrations = "../migrations")]
async fn operador_confirma_pago_distinto(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let op = operador(&pool).await;
    let token = b.token(op, RolUsuario::Operador);
    let clabe = ana.cuenta_ifpe.clabe.clone();
    let semana = |fecha: NaiveDate, extra: Value| {
        let mut c = json!({
            "clabe": clabe, "monto": "500", "banco_emisor": "90646", "fecha_operacion": fecha,
            "clave_rastreo": "NOMINA01",
        });
        c.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        c
    };
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", semana(hoy_cdmx() - Days::new(7), json!({}))).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let semana1 = r["transaccion"]["id"].as_str().unwrap().to_owned();

    let real = cuerpo_abono(&clabe, "500", "90646", "NOMINA01", hoy_cdmx());
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", real.clone()).await;
    assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("EN_REVISION")), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("500"));
    let (s, _) = b.obtener(&b.token(ana.cliente.id, RolUsuario::Cliente), "/api/v1/spei/en-revision").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, cola) = b.obtener(&token, "/api/v1/spei/en-revision").await;
    assert_eq!(s, StatusCode::OK, "{cola}");
    let identidad = format!("90646:{}:NOMINA01", hoy_cdmx().format("%Y%m%d"));
    assert_eq!(cola.as_array().map(|c| c.len()), Some(1), "{cola}");
    assert_eq!((&cola[0]["identidad"], &cola[0]["tipo"]), (&json!(identidad), &json!("POSIBLE_DUPLICADO")), "{cola}");

    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", semana(hoy_cdmx(), json!({}))).await;
    assert_eq!((s, &r["error"]["codigo"]), (StatusCode::CONFLICT, &json!("SPEI_POSIBLE_DUPLICADO")), "{r}");
    assert!(r["error"]["mensaje"].as_str().unwrap_or_default().contains(&semana1), "{r}");
    let (s, r) = b
        .llamar(&token, "/api/v1/operaciones/depositos", semana(hoy_cdmx(), json!({ "distinto_de": semana1, "motivo": "no" })))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "motivo demasiado corto: {r}");
    let distinto = json!({ "distinto_de": semana1, "motivo": "nomina semanal, rastreo reutilizado" });
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", semana(hoy_cdmx(), distinto)).await;
    assert_eq!((s, &r["spei"]["estado"]), (StatusCode::CREATED, &json!("ABONADO")), "{r}");
    assert_eq!(r["spei"]["distinto_de_tx"], json!(semana1));
    assert_eq!(r["spei"]["registrado_por"], json!(op));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));

    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", real).await;
    assert_eq!((s, &r["mensaje"], &r["idempotente"]), (StatusCode::OK, &json!("confirmar"), &json!(true)), "{r}");
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.v_spei_en_revision").await, 0);

    // La base de datos solo admite la declaración en una captura del operador.
    let e = como(
        &pool,
        Rol::Spei,
        &format!(
            "INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, \
             cuenta_id, monto, estado, transaccion_id, origen, raw_payload, distinto_de_tx, motivo) \
             VALUES ('90646', 'NOMINA01', current_date - 1, '{clabe}', '{}', 500, 'ABONADO', gen_random_uuid(), \
                     'WEBHOOK', '{{}}', '{semana1}', 'motivo suficiente')",
            ana.cuenta_ifpe.id
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("ck_spei_inbound_distinto"), "{e}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    verificar_invariantes(&pool).await;
}

/// DEPOSITO anterior a 0005 (sin registro en `ifpe.spei_inbound`), como lo
/// dejaba el código de entonces. Hoy la lista blanca de 0005 lo rechazaría:
/// se desactiva solo para fabricarlo.
async fn deposito_anterior_a_0005(pool: &PgPool, cuenta_id: Uuid, monto: &str, clave: &str, referencia: &str) {
    sqlx::query("ALTER TABLE ifpe.transacciones DISABLE TRIGGER trg_ifpe_tx_spei_registrado")
        .execute(pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let reserva: Uuid = sqlx::query_scalar("SELECT id FROM ifpe.reserva_custodia WHERE activa ORDER BY creado_en LIMIT 1")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $2 WHERE id = $1")
        .bind(cuenta_id)
        .bind(d(monto))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE ifpe.reserva_custodia SET saldo = saldo + $2 WHERE id = $1")
        .bind(reserva)
        .bind(d(monto))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ifpe.transacciones (tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa) \
         VALUES ('DEPOSITO', $1, $2, $3, $4, $5)",
    )
    .bind(cuenta_id)
    .bind(reserva)
    .bind(d(monto))
    .bind(clave)
    .bind(referencia)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("ALTER TABLE ifpe.transacciones ENABLE TRIGGER trg_ifpe_tx_spei_registrado")
        .execute(pool)
        .await
        .unwrap();
}

/// inbound-security-3: abonos anteriores a 0005. (a) El de identidad
/// canónica se registra con origen LEGADO (lo hace 0007 al aplicarse): una
/// nueva notificación del mismo pago es una repetición (`confirmar`), no un
/// 409. (b) El de clave de rastreo sola (sin emisor ni fecha) se abonaba otra
/// vez: ahora es un posible duplicado (retenido sin abonar).
#[sqlx::test(migrations = "../migrations")]
async fn abonos_anteriores_a_0005_no_se_abonan_dos_veces(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let clabe = ana.cuenta_ifpe.clabe.clone();
    let ayer = hoy_cdmx() - Days::new(1);
    let identidad = format!("40012:{}:LEGADOA", ayer.format("%Y%m%d"));
    deposito_anterior_a_0005(&pool, ana.cuenta_ifpe.id, "800", &format!("spei:{identidad}"), &identidad).await;
    deposito_anterior_a_0005(&pool, ana.cuenta_ifpe.id, "900", "spei:LEGADOB", "LEGADOB").await;
    // Lo que hace 0007 al aplicarse sobre una base con abonos anteriores.
    let registrados: i32 = sqlx::query_scalar("SELECT ifpe.registrar_abonos_legados()").fetch_one(&pool).await.unwrap();
    assert_eq!(registrados, 1, "solo el de identidad canónica");
    let saldo = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0;
    assert_eq!(saldo, d("1700"));

    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo_abono(&clabe, "800", "40012", "LEGADOA", ayer)).await;
    assert_eq!(
        (s, &r["mensaje"], &r["estado"], &r["idempotente"]),
        (StatusCode::OK, &json!("confirmar"), &json!("ABONADO"), &json!(true)),
        "{r}"
    );
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", cuerpo_abono(&clabe, "900", "40012", "LEGADOB", ayer)).await;
    assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("EN_REVISION")), "{r}");
    let op = operador(&pool).await;
    let mut captura = abono(&clabe, "900", "LEGADOB");
    captura.identidad = synthetic_capital::spei::IdentidadSpei::nueva("40012", ayer, "LEGADOB").unwrap();
    let e = inbound::abonar(&pool, &b.cfg.pld, &b.cfg.spei, &captura, Origen::Operador(op), SiNoAcreditable::Rechazar)
        .await
        .unwrap_err();
    assert_eq!(e.codigo(), "SPEI_POSIBLE_DUPLICADO");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, saldo, "ningún abono legado se acreditó dos veces");
    // Nadie más registra abonos LEGADO.
    let e = como(
        &pool,
        Rol::Spei,
        &format!(
            "INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, \
             monto, estado, causa_devolucion, origen, raw_payload) \
             VALUES ('40012', 'FALSOLEGADO', current_date, '{clabe}', 1, 'DEVUELTO', 1, 'LEGADO', '{{}}')"
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("SC_SPEI_LEGADO"), "{e}");
    verificar_invariantes(&pool).await;
}

/// inbound-security-4: el operador capturó el pago a la CLABE equivocada
/// (Beto en vez de Ana). La notificación real se retiene (antes: 409 para
/// siempre); el operador anula la captura (corrección total, auditada) y la
/// identidad queda libre: el pago se abona a Ana como reemplazo del anulado.
#[sqlx::test(migrations = "../migrations")]
async fn captura_equivocada_se_anula_y_libera_la_identidad(pool: PgPool) {
    let b = banco(&pool).await;
    let ana = cliente(&pool, &b.cfg, "Ana").await;
    let beto = cliente(&pool, &b.cfg, "Beto").await;
    let op = operador(&pool).await;
    let token = b.token(op, RolUsuario::Operador);
    let captura = |clabe: &str| {
        json!({
            "clabe": clabe, "monto": "500", "banco_emisor": "40012", "fecha_operacion": hoy_cdmx(),
            "clave_rastreo": "MBAN01REAL",
        })
    };
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", captura(&beto.cuenta_ifpe.clabe)).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let erronea = r["spei"]["id"].as_str().unwrap().to_owned();
    let real = cuerpo_abono(&ana.cuenta_ifpe.clabe, "500", "40012", "MBAN01REAL", hoy_cdmx());
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", real.clone()).await;
    assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("EN_REVISION")), "{r}");
    assert_eq!(r["motivo"], "IDENTIDAD_REUTILIZADA");
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.v_spei_en_revision").await, 1);

    // Solo un operador anula, y solo capturas (no lo que informó el proveedor).
    let anular = |id: &str| format!("/api/v1/operaciones/spei/recibidos/{id}/anulacion");
    let cuerpo = json!({ "motivo": "abonado a la CLABE equivocada", "clave_idempotencia": "anulacion-0001" });
    let (s, _) = b.llamar(&b.token(beto.cliente.id, RolUsuario::Cliente), &anular(&erronea), cuerpo.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, r) = b.llamar(&token, &anular(&erronea), cuerpo.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(d(r["correccion"]["monto"].as_str().unwrap()), d("500"), "corrección total: {r}");
    let (s, r) = b.llamar(&token, &anular(&erronea), cuerpo).await;
    assert_eq!((s, &r["idempotente"]), (StatusCode::OK, &json!(true)), "{r}");
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("0"));

    // El operador captura el pago real (la notificación retenida ya se confirmó).
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", captura(&ana.cuenta_ifpe.clabe)).await;
    assert_eq!((s, &r["spei"]["estado"]), (StatusCode::CREATED, &json!("ABONADO")), "{r}");
    assert_eq!(r["spei"]["reemplaza_a"], json!(erronea));
    let reemplazo = r["spei"]["id"].as_str().unwrap().to_owned();
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("500"));
    // Una nueva entrega del proveedor es ahora una repetición.
    let (s, r) = b.webhook("/api/v1/spei/webhook/inbound", real).await;
    assert_eq!((s, &r["idempotente"], &r["spei_id"]), (StatusCode::OK, &json!(true), &json!(reemplazo)), "{r}");
    assert_eq!(contar(&pool, "SELECT count(*) FROM ifpe.v_spei_en_revision").await, 0);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("500"));
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("0"));

    // Si la anulación llega antes que la notificación, el webhook abona a Ana.
    let otra_captura = json!({
        "clabe": beto.cuenta_ifpe.clabe, "monto": "70", "banco_emisor": "40012", "fecha_operacion": hoy_cdmx(),
        "clave_rastreo": "MBAN02REAL",
    });
    let (s, r) = b.llamar(&token, "/api/v1/operaciones/depositos", otra_captura).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let erronea2 = r["spei"]["id"].as_str().unwrap().to_owned();
    let cuerpo2 = json!({ "motivo": "abonado a la CLABE equivocada", "clave_idempotencia": "anulacion-0003" });
    let (s, r) = b.llamar(&token, &anular(&erronea2), cuerpo2).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let (s, r) = b.webhook(
        "/api/v1/spei/webhook/inbound",
        cuerpo_abono(&ana.cuenta_ifpe.clabe, "70", "40012", "MBAN02REAL", hoy_cdmx()),
    )
    .await;
    assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("ABONADO")), "{r}");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("570"));
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("0"));

    // Lo que informó el proveedor no se anula.
    let (s, r) = b.webhook(
        "/api/v1/spei/webhook/inbound",
        cuerpo_abono(&ana.cuenta_ifpe.clabe, "50", "40012", "MBAN01OTRO", hoy_cdmx()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let del_proveedor = r["spei_id"].as_str().unwrap().to_owned();
    let otra = json!({ "motivo": "no es anulable por el operador", "clave_idempotencia": "anulacion-0002" });
    let (s, r) = b.llamar(&token, &anular(&del_proveedor), otra).await;
    assert_eq!((s, &r["error"]["codigo"]), (StatusCode::UNPROCESSABLE_ENTITY, &json!("SPEI_NO_ANULABLE")), "{r}");
    verificar_invariantes(&pool).await;
}
