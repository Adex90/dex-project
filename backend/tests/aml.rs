//! Consola de cumplimiento PLD/FT (`/api/v1/aml`) y monitoreo por lotes.

mod comun;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use comun::*;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{
    aml::{monitoreo, oficial, AccionResolucion, SolicitudCongelamiento, SolicitudDecision, SolicitudResolucion},
    auth::Usuario,
    bootstrap,
    config::Config,
    db::Rol,
    error::AppResult,
    kyc::Registro,
    ledger::{self, hash, Movimiento},
    models::kyc::RolUsuario,
    routes,
    sofom::{credito, puente},
    spei::{
        clabe, firma, hoy_cdmx,
        inbound::{self, Origen},
        outbound::{self, Evento},
        pasarela::PasarelaSimulada,
    },
    AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

struct Api {
    app: Router,
}

impl Api {
    /// Petición con el cuerpo de la respuesta tal cual (bytes).
    async fn llamar_crudo(
        &self,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
    ) -> (StatusCode, Vec<u8>) {
        let mut req = Request::builder().method(metodo).uri(ruta);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let body = match cuerpo {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        (status, resp.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    async fn llamar(
        &self,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, bytes) = self.llamar_crudo(metodo, ruta, token, cuerpo).await;
        let valor = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
        (status, valor)
    }

    /// Notificación de abono del proveedor, firmada con el secreto vigente.
    async fn webhook(&self, cfg: &Config, cuerpo: String) -> (StatusCode, Value) {
        let f =
            firma::firmar(cfg.spei.webhook_secretos[0].as_bytes(), chrono::Utc::now().timestamp(), cuerpo.as_bytes());
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/spei/webhook/inbound")
            .header("content-type", "application/json")
            .header(firma::CABECERA, f)
            .body(Body::from(cuerpo))
            .unwrap();
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn get(&self, ruta: &str, token: &str) -> (StatusCode, Value) {
        self.llamar("GET", ruta, Some(token), None).await
    }

    async fn post(&self, ruta: &str, token: &str, cuerpo: Value) -> (StatusCode, Value) {
        self.llamar("POST", ruta, Some(token), Some(cuerpo)).await
    }

    async fn token(&self, email: &str, password: &str) -> String {
        let (s, v) = self
            .llamar("POST", "/api/v1/auth/login", None, Some(json!({ "email": email, "password": password })))
            .await;
        assert_eq!(s, StatusCode::OK, "login {email}: {v}");
        v["token"].as_str().unwrap().to_string()
    }
}

async fn api(pool: &PgPool) -> (Api, Config) {
    let cfg = preparar(pool).await;
    bootstrap::asegurar_operador(pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    (Api { app: routes::router(AppState::nuevo(pool.clone(), cfg.clone())) }, cfg)
}

/// Cuerpo de una notificación de abono al estilo STP (como en `spei.rs`), con
/// una clave de rastreo única.
fn cuerpo_abono(clabe: &str, monto: &str) -> String {
    let rastreo = format!("STP{}", &Uuid::new_v4().simple().to_string()[..20]).to_uppercase();
    format!(
        r#"{{"id":{id},"fechaOperacion":{fecha},"institucionOrdenante":40012,"institucionBeneficiaria":90646,
            "claveRastreo":"{rastreo}","monto":{monto},"nombreOrdenante":"JUAN PEREZ","tipoPago":1,
            "cuentaOrdenante":"012180001234567899","cuentaBeneficiario":"{clabe}","conceptoPago":"RENTA",
            "referenciaNumerica":1234567}}"#,
        id = rastreo.len(),
        fecha = hoy_cdmx().format("%Y%m%d"),
    )
}

/// Token de un oficial nuevo.
async fn token_oficial(api: &Api, pool: &PgPool, nombre: &str) -> (Uuid, String) {
    let (id, email) = oficial(pool, nombre).await;
    (id, api.token(&email, PASSWORD_INTERNO).await)
}

/// Token de un cliente creado con `comun::cliente`.
async fn token_cliente(api: &Api, nombre: &str) -> String {
    api.token(&format!("{}@example.mx", nombre.to_lowercase()), "contraseña-de-prueba-123").await
}

/// Id de la alerta del cliente con la regla indicada.
async fn alerta_de(pool: &PgPool, cliente: Uuid, regla: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM aml.alertas WHERE cliente_id = $1 AND regla_codigo = $2")
        .bind(cliente)
        .bind(regla)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn error(v: &Value) -> &str {
    v["error"]["codigo"].as_str().unwrap_or_default()
}

const DICTAMEN: &str = "Depósitos del mismo ordenante con nómina documentada; sin indicios de estructuración.";
const MOTIVO: &str = "Instrucción de la autoridad competente recibida por oficio; congelamiento preventivo.";
const JUSTIFICACION: &str = "El cliente acreditó el origen de los recursos con documentación suficiente.";

/// Respuesta constante de toda operación saliente bloqueada (§4.3).
const NO_DISPONIBLE: &str = r#"{"error":{"codigo":"OPERACION_NO_DISPONIBLE","mensaje":"la operación no está disponible en este momento; si necesitas ayuda, contacta a soporte"}}"#;

/// CLABE externa válida (banco 072) distinta por `n`.
fn externa(n: u64) -> String {
    let primeros = format!("072180{n:011}");
    let dv = clabe::digito_verificador(&primeros).unwrap();
    format!("{primeros}{dv}")
}

/// Clave de idempotencia válida en la API (sin el prefijo `prueba:` de `comun::clave`).
fn clave_api() -> String {
    Uuid::new_v4().to_string()
}

/// Oficial de cumplimiento como `Usuario` (para llamar a `aml::oficial` directamente).
async fn usuario_oficial(pool: &PgPool, nombre: &str) -> Usuario {
    Usuario { cliente_id: oficial(pool, nombre).await.0, rol: RolUsuario::Cumplimiento }
}

/// Operador de mesa (capturas, correcciones, retenciones).
async fn operador_id(pool: &PgPool) -> Uuid {
    bootstrap::asegurar_operador(pool, "operador@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = 'operador@synthetic.mx'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Ejecuta `sql` como dueño del esquema y confirma; el texto del error si falla.
async fn como_dueno(pool: &PgPool, sql: &str) -> Result<(), String> {
    let mut tx = pool.begin().await.unwrap();
    if let Err(e) = sqlx::raw_sql(sql).execute(&mut *tx).await {
        tx.rollback().await.unwrap();
        return Err(e.to_string());
    }
    tx.commit().await.map_err(|e| e.to_string())
}

async fn contar(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// Puntaje (texto con 2 decimales) de la alerta del cliente con la regla indicada.
async fn puntaje_de(pool: &PgPool, cliente: Uuid, regla: &str) -> String {
    sqlx::query_scalar("SELECT puntaje::text FROM aml.alertas WHERE cliente_id = $1 AND regla_codigo = $2")
        .bind(cliente)
        .bind(regla)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Total retenido por PLD (retenciones VIGENTE) en la cuenta.
async fn retenido(pool: &PgPool, cuenta_id: Uuid) -> Decimal {
    sqlx::query_scalar(
        "SELECT coalesce(sum(monto), 0) FROM aml.retenciones WHERE cuenta_id = $1 AND estado = 'VIGENTE'",
    )
    .bind(cuenta_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Tipos de notificación del cliente, en orden de id.
async fn notificaciones_de(pool: &PgPool, cliente: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT tipo FROM aml.notificaciones WHERE cliente_id = $1 ORDER BY id")
        .bind(cliente)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Transferencia entre clientes de la bóveda (camino del cliente).
async fn transferir(pool: &PgPool, cfg: &Config, de: &Registro, a: &Registro, monto: &str) -> AppResult<Movimiento> {
    ledger::transferir(
        pool,
        &cfg.pld,
        &ledger::Transferencia {
            cliente_origen_id: de.cliente.id,
            clabe_destino: a.cuenta_ifpe.clabe.clone(),
            monto: d(monto),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
}

async fn invertir(
    pool: &PgPool,
    cfg: &Config,
    r: &Registro,
    monto: &str,
    plazo: i32,
) -> AppResult<puente::ResultadoMutuo> {
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
}

fn solicitud_credito(acreditado: Uuid, principal: &str) -> credito::SolicitudCredito {
    credito::SolicitudCredito {
        acreditado_id: acreditado,
        principal: d(principal),
        tasa_anual: d("0.18"),
        plazo_dias: 90,
        destino: Some("Capital de trabajo".into()),
        clabe_destino: "646180157000000017".into(),
        clave_idempotencia: clave(),
    }
}

/// Levantamiento de cuatro ojos de un congelamiento con una sola alerta
/// abierta (p. ej. R30): `c1` toma y propone FALSO_POSITIVO (crea la
/// solicitud) y `c2` aprueba.
async fn levantar_con_cuatro_ojos(pool: &PgPool, c1: &Usuario, c2: &Usuario, alerta: Uuid) {
    for (accion, dictamen) in [(AccionResolucion::Tomar, None), (AccionResolucion::FalsoPositivo, Some(DICTAMEN))] {
        let s = SolicitudResolucion {
            accion,
            dictamen: dictamen.map(Into::into),
            tipo_reporte: None,
            urgente: None,
            congelar: None,
            tipo_levantamiento: None,
            clave_idempotencia: clave_api(),
        };
        oficial::resolver_alerta(pool, c1, alerta, &s).await.unwrap();
    }
    let solicitud: Uuid = sqlx::query_scalar(
        "SELECT s.id FROM aml.solicitudes_levantamiento s JOIN aml.alertas a ON a.congelamiento_id = s.congelamiento_id \
          WHERE a.id = $1 AND s.estado = 'PENDIENTE'",
    )
    .bind(alerta)
    .fetch_one(pool)
    .await
    .unwrap();
    let decision = SolicitudDecision {
        aprobar: true,
        comentario: "Revisado el expediente completo.".into(),
        clave_idempotencia: clave_api(),
    };
    oficial::decidir_levantamiento(pool, c2, solicitud, &decision).await.unwrap();
}

/// Congelamiento manual por un oficial (sin HTTP).
async fn congelar_manual(pool: &PgPool, oficial_: &Usuario, cliente: Uuid) -> Value {
    let s = SolicitudCongelamiento { motivo: MOTIVO.into(), alerta_id: None, clave_idempotencia: clave_api() };
    oficial::congelar_manual(pool, oficial_, cliente, &s).await.unwrap()
}

/// Propuesta de cambio aprobada con cuatro ojos (por HTTP).
async fn aprobar_propuesta(api: &Api, c1: &str, c2: &str, cuerpo: Value) -> Value {
    let mut cuerpo = cuerpo;
    cuerpo["justificacion"] = json!("Calibración trimestral aprobada por el comité de comunicación y control.");
    cuerpo["clave_idempotencia"] = json!(clave_api());
    let (s, v) = api.post("/api/v1/aml/propuestas", c1, cuerpo).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let decision = format!("/api/v1/aml/propuestas/{}/decision", v["propuesta"]["id"].as_str().unwrap());
    let (s, v) = api
        .post(
            &decision,
            c2,
            json!({ "aprobar": true, "comentario": "Revisada la calibración.", "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["propuesta"]["estado"], "APROBADA");
    v
}

// -----------------------------------------------------------------------------
// §6.1: detección en línea, congelamiento y retención
// -----------------------------------------------------------------------------

/// §6.1 prueba 1: la calibración de §2.5 no congela ninguno de los escenarios
/// de las pruebas existentes.
#[sqlx::test(migrations = "../migrations")]
async fn calibracion_sin_regresiones(pool: PgPool) {
    let cfg = preparar(&pool).await;
    // ledger.rs: 150k (registro de relevantes, sin alerta), 80k, 80k (R02).
    let ana = cliente(&pool, &cfg, "Ana").await;
    let relevante = depositar(&pool, &cfg, ana.cuenta_ifpe.id, "150000").await;
    assert!(alertas_de(&pool, ana.cliente.id).await.is_empty());
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "80000").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "80000").await;
    let alertas = alertas_de(&pool, ana.cliente.id).await;
    assert_eq!(alertas, [("R02_FRACCIONAMIENTO".into(), "ALTO".into(), "PENDIENTE".into(), "REGISTRADA".into(), 1)]);
    assert_eq!(puntaje_de(&pool, ana.cliente.id, "R02_FRACCIONAMIENTO").await, "65.00");
    let registro: Vec<(Uuid, Decimal, String)> =
        sqlx::query_as("SELECT transaccion_id, monto, sentido FROM aml.operaciones_relevantes WHERE cliente_id = $1")
            .bind(ana.cliente.id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(registro, [(relevante.transaccion.id, d("150000"), "ENTRADA".to_string())]);

    // reserva.rs: un depósito de 10,000,202.10 solo dispara R07 (MEDIO, 40).
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "10000202.10").await;
    let alertas = alertas_de(&pool, beto.cliente.id).await;
    assert_eq!(alertas, [("R07_VOLUMEN_DIARIO".into(), "MEDIO".into(), "PENDIENTE".into(), "REGISTRADA".into(), 1)]);
    assert_eq!(puntaje_de(&pool, beto.cliente.id, "R07_VOLUMEN_DIARIO").await, "40.00");

    // puente.rs: depósito de 100k e inversión de 100k: nada.
    let carla = cliente(&pool, &cfg, "Carla").await;
    depositar(&pool, &cfg, carla.cuenta_ifpe.id, "100000").await;
    invertir(&pool, &cfg, &carla, "100000", 28).await.unwrap();
    assert!(alertas_de(&pool, carla.cliente.id).await.is_empty());
    let canales: Vec<String> = evaluaciones_de(&pool, carla.cliente.id).await.into_iter().map(|e| e.0).collect();
    assert_eq!(canales, ["SPEI_WEBHOOK", "INVERSION_MUTUO"]);

    assert_eq!(contar(&pool, "SELECT count(*) FROM aml.congelamientos").await, 0);
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 2 (TAREA 4): tres depósitos de 145,000 en 24 horas. El segundo
/// alerta por fraccionamiento; el tercero completa la estructuración: alerta
/// crítica, congelamiento preventivo y retención de todo el patrón.
#[sqlx::test(migrations = "../migrations")]
async fn estructuracion_alerta_congela_y_retiene(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "145000").await;
    assert!(alertas_de(&pool, ana.cliente.id).await.is_empty());
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "145000").await;
    assert_eq!(
        alertas_de(&pool, ana.cliente.id).await,
        [("R02_FRACCIONAMIENTO".into(), "ALTO".into(), "PENDIENTE".into(), "REGISTRADA".into(), 1)]
    );
    assert!(congelamiento_activo(&pool, ana.cliente.id).await.is_none());

    let tercero = depositar(&pool, &cfg, ana.cuenta_ifpe.id, "145000").await;
    let alertas = alertas_de(&pool, ana.cliente.id).await;
    assert_eq!(alertas.len(), 2, "{alertas:?}");
    assert_eq!(alertas[1], ("R05_ESTRUCTURACION".into(), "CRITICO".into(), "PENDIENTE".into(), "RETENIDA".into(), 1));
    assert_eq!(puntaje_de(&pool, ana.cliente.id, "R05_ESTRUCTURACION").await, "100.00");
    let (congelamiento, origen, intentos) = congelamiento_activo(&pool, ana.cliente.id).await.expect("congelado");
    assert_eq!((origen.as_str(), intentos), ("AUTOMATICO", 0));
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    // Todo el patrón queda retenido; la reserva no se mueve.
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("0"), d("435000")));
    let retenciones: Vec<(Uuid, Uuid, Decimal, Uuid)> = sqlx::query_as(
        "SELECT id, congelamiento_id, monto, tx_retencion_id FROM aml.retenciones WHERE cliente_id = $1",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(retenciones.len(), 1);
    let (retencion, de_congelamiento, monto, tx_retencion) = retenciones[0];
    assert_eq!((de_congelamiento, monto), (congelamiento, d("435000")));
    let t: synthetic_capital::models::ifpe::TransaccionIfpe =
        sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE id = $1")
            .bind(tx_retencion)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(t.clave_idempotencia, format!("ret:{retencion}"));
    assert_eq!(t.concepto.as_deref(), Some("Retención temporal"));
    assert_eq!(
        (t.cuenta_origen_id, t.cuenta_destino_id, t.reserva_id),
        (Some(ana.cuenta_ifpe.id), Some(ana.cuenta_ifpe.id), None)
    );
    assert_eq!(hash::hash_ifpe(&t), t.tx_hash);
    assert!(t.secuencia > tercero.transaccion.secuencia);
    // Los tres SPEI se abonaron (al proveedor se le confirmó cada uno).
    let abonados: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ifpe.spei_inbound WHERE cuenta_id = $1 AND estado = 'ABONADO'")
            .bind(ana.cuenta_ifpe.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(abonados, 3);

    let evaluaciones = evaluaciones_de(&pool, ana.cliente.id).await;
    let resultados: Vec<(&str, &str)> = evaluaciones.iter().map(|e| (e.1.as_str(), e.2.as_str())).collect();
    assert_eq!(resultados, [("PERMITIDA", "EN_LINEA"), ("ALERTADA", "EN_LINEA"), ("RETENIDA", "EN_LINEA")]);
    assert_eq!(evaluaciones[2].3.as_deref(), Some("CRITICO"));
    let avisos = notificaciones_de(&pool, ana.cliente.id).await;
    for tipo in ["ALERTA_NUEVA", "ALERTA_CRITICA", "CONGELAMIENTO"] {
        assert!(avisos.iter().any(|a| a == tipo), "{avisos:?}");
    }
    let eventos = eventos_bitacora(&pool, ana.cliente.id).await;
    for evento in ["ALERTA_CREADA", "CONGELAMIENTO_APLICADO", "RETENCION_APLICADA"] {
        assert!(eventos.iter().any(|e| e == evento), "{eventos:?}");
    }
    assert!(!eventos.iter().any(|e| e == "EVALUACION_DIFERIDA"), "{eventos:?}");
    assert_eq!(contar(&pool, "SELECT count(*) FROM aml.evaluaciones WHERE origen = 'DIFERIDA'").await, 0);

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 3: la estructuración se mide por cliente a través de todos los
/// canales de entrada (webhook, captura del operador, demo y transferencia interna).
#[sqlx::test(migrations = "../migrations")]
async fn estructuracion_multicanal(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let operador = operador_id(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar_por(&pool, &cfg, ana.cuenta_ifpe.id, "140000", Origen::Webhook).await;
    depositar_por(&pool, &cfg, ana.cuenta_ifpe.id, "140000", Origen::Operador(operador)).await;
    depositar_por(&pool, &cfg, ana.cuenta_ifpe.id, "140000", Origen::Demo).await;
    assert!(congelamiento_activo(&pool, ana.cliente.id).await.is_some());
    let canales: Vec<String> = evaluaciones_de(&pool, ana.cliente.id).await.into_iter().map(|e| e.0).collect();
    assert_eq!(canales, ["SPEI_WEBHOOK", "SPEI_OPERADOR", "SPEI_DEMO"]);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("0"), d("420000")));

    // Carla: dos SPEI y una transferencia interna de Beto.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let carla = cliente(&pool, &cfg, "Carla").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "140000").await;
    depositar(&pool, &cfg, carla.cuenta_ifpe.id, "140000").await;
    depositar(&pool, &cfg, carla.cuenta_ifpe.id, "140000").await;
    let m = transferir(&pool, &cfg, &beto, &carla, "140000").await.unwrap();
    assert!(!m.idempotente);
    let (_, origen, _) = congelamiento_activo(&pool, carla.cliente.id).await.expect("Carla congelada");
    assert_eq!(origen, "AUTOMATICO");
    assert_eq!(saldo_ifpe(&pool, carla.cuenta_ifpe.id).await, (d("0"), d("420000")));
    assert_eq!(retenido(&pool, carla.cuenta_ifpe.id).await, d("420000"));
    // El emisor no se entera ni se congela.
    assert!(congelamiento_activo(&pool, beto.cliente.id).await.is_none());
    let beto_eval = evaluaciones_de(&pool, beto.cliente.id).await;
    assert_eq!(beto_eval.last().unwrap().0, "TRANSFERENCIA_ENVIADA");
    assert_eq!(beto_eval.last().unwrap().1, "PERMITIDA");
    let carla_eval = evaluaciones_de(&pool, carla.cliente.id).await;
    let ultima = carla_eval.last().unwrap();
    assert_eq!((ultima.0.as_str(), ultima.1.as_str()), ("TRANSFERENCIA_RECIBIDA", "RETENIDA"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 4: depósitos concurrentes del mismo cliente se ven entre sí
/// (candado por cliente y lectura fresca en READ COMMITTED).
#[sqlx::test(migrations = "../migrations")]
async fn depositos_concurrentes_se_ven(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let cuenta = ana.cuenta_ifpe.id;
    tokio::join!(
        depositar(&pool, &cfg, cuenta, "145000"),
        depositar(&pool, &cfg, cuenta, "145000"),
        depositar(&pool, &cfg, cuenta, "145000"),
    );
    let mut codigos: Vec<String> = alertas_de(&pool, ana.cliente.id).await.into_iter().map(|a| a.0).collect();
    codigos.sort();
    assert_eq!(codigos, ["R02_FRACCIONAMIENTO", "R05_ESTRUCTURACION"]);
    let congelamientos: i64 = sqlx::query_scalar("SELECT count(*) FROM aml.congelamientos WHERE cliente_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(congelamientos, 1);
    let retenidas = evaluaciones_de(&pool, ana.cliente.id).await.into_iter().filter(|e| e.1 == "RETENIDA").count();
    assert_eq!(retenidas, 1);
    assert_eq!(retenido(&pool, cuenta).await, d("435000"));

    // El cuarto se retiene completo y se agrupa en la misma alerta R05.
    depositar(&pool, &cfg, cuenta, "145000").await;
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("0"), d("580000")));
    let r05 = alertas_de(&pool, ana.cliente.id).await.into_iter().find(|a| a.0 == "R05_ESTRUCTURACION").unwrap();
    assert_eq!(r05.4, 2);
    assert_eq!(contar(&pool, "SELECT count(*) FROM aml.congelamientos").await, 1);

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 5: una salida estructurada se decide antes de escribir. El
/// bloqueo se confirma sin renglones del ledger ni orden SPEI.
#[sqlx::test(migrations = "../migrations")]
async fn salida_estructurada_bloquea_sin_ledger(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "450000").await;
    let destinos = ["002010077777777771".to_string(), externa(1), externa(2)];
    retirar(&pool, &cfg, ana.cliente.id, "145000", &destinos[0], None, &clave()).await.unwrap();
    retirar(&pool, &cfg, ana.cliente.id, "145000", &destinos[1], None, &clave()).await.unwrap();
    assert_eq!(
        alertas_de(&pool, ana.cliente.id).await,
        [("R07_VOLUMEN_DIARIO".into(), "MEDIO".into(), "PENDIENTE".into(), "REGISTRADA".into(), 2)]
    );

    let cabeza = cabeza_ifpe(&pool).await;
    let tercera = clave();
    let e = retirar(&pool, &cfg, ana.cliente.id, "145000", &destinos[2], None, &tercera).await.unwrap_err();
    assert_eq!(e.codigo(), "OPERACION_NO_DISPONIBLE");
    assert_eq!(cabeza_ifpe(&pool).await, cabeza, "el bloqueo no escribe en el ledger");
    let cuenta = ana.cuenta_ifpe.id;
    let retiros: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'RETIRO' AND cuenta_origen_id = $1")
            .bind(cuenta)
            .fetch_one(&pool)
            .await
            .unwrap();
    let ordenes: i64 = sqlx::query_scalar("SELECT count(*) FROM ifpe.spei_outbound WHERE cliente_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((retiros, ordenes), (2, 2));
    let bloqueadas: Vec<(String, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT motivo_bloqueo, transaccion_id, clave_operacion FROM aml.evaluaciones \
          WHERE cliente_id = $1 AND resultado = 'BLOQUEADA'",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(bloqueadas, [("PUNTAJE_CRITICO".to_string(), None, tercera.clone())]);
    let (origen, decision, nivel, transaccion): (String, String, String, Option<Uuid>) = sqlx::query_as(
        "SELECT origen, decision, nivel_riesgo::text, transaccion_id FROM aml.alertas \
          WHERE cliente_id = $1 AND regla_codigo = 'R05_ESTRUCTURACION'",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (origen.as_str(), decision.as_str(), nivel.as_str(), transaccion),
        ("INTENTO_BLOQUEADO", "BLOQUEADA", "CRITICO", None)
    );
    let (_, origen, intentos) = congelamiento_activo(&pool, ana.cliente.id).await.expect("congelada");
    assert_eq!((origen.as_str(), intentos), ("AUTOMATICO", 0));
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("160000"), d("0")));

    // Reintento con la misma clave: mismo error, sin otra evaluación ni intento.
    let evaluaciones = evaluaciones_de(&pool, ana.cliente.id).await.len();
    let e2 = retirar(&pool, &cfg, ana.cliente.id, "145000", &destinos[2], None, &tercera).await.unwrap_err();
    assert_eq!((e2.codigo(), e2.to_string()), (e.codigo(), e.to_string()));
    assert_eq!(evaluaciones_de(&pool, ana.cliente.id).await.len(), evaluaciones);
    assert_eq!(congelamiento_activo(&pool, ana.cliente.id).await.unwrap().2, 0);
    // Otra clave: mismo error y el intento queda contado.
    let e3 = retirar(&pool, &cfg, ana.cliente.id, "145000", &destinos[2], None, &clave()).await.unwrap_err();
    assert_eq!(e3.to_string(), e.to_string());
    assert_eq!(evaluaciones_de(&pool, ana.cliente.id).await.len(), evaluaciones + 1);
    assert_eq!(congelamiento_activo(&pool, ana.cliente.id).await.unwrap().2, 1);
    assert_eq!(cabeza_ifpe(&pool).await, cabeza);

    // El despachador no reclama las dos órdenes PENDIENTE del cliente congelado.
    vencer_ordenes(&pool).await;
    let despacho = outbound::despachar(&pool, &PasarelaSimulada::default(), &cfg.spei).await.unwrap();
    assert_eq!((despacho.reclamadas, despacho.errores), (0, 0));
    let intentos: Vec<(String, i32)> =
        sqlx::query_as("SELECT estado, intentos_envio FROM ifpe.spei_outbound WHERE cliente_id = $1")
            .bind(ana.cliente.id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(intentos, [("PENDIENTE".to_string(), 0), ("PENDIENTE".to_string(), 0)]);

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 6: el paso rápido (R11) bloquea y congela; los factores de
/// perfil (PEP, riesgo ALTO, fuera de perfil) solo alertan.
#[sqlx::test(migrations = "../migrations")]
async fn paso_rapido_bloquea_y_perfil_no_congela(pool: PgPool) {
    let cfg = preparar(&pool).await;
    // (a) Beto recibe 200,000 y saca 185,000 de inmediato.
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "200000").await;
    let cabeza = cabeza_ifpe(&pool).await;
    let e = retirar(&pool, &cfg, beto.cliente.id, "185000", &externa(4), None, &clave()).await.unwrap_err();
    assert_eq!(e.codigo(), "OPERACION_NO_DISPONIBLE");
    assert_eq!(cabeza_ifpe(&pool).await, cabeza);
    assert_eq!(
        alertas_de(&pool, beto.cliente.id).await,
        [("R11_PASO_RAPIDO".into(), "CRITICO".into(), "PENDIENTE".into(), "BLOQUEADA".into(), 1)]
    );
    assert_eq!(puntaje_de(&pool, beto.cliente.id, "R11_PASO_RAPIDO").await, "85.00");
    assert!(congelamiento_activo(&pool, beto.cliente.id).await.is_some());
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await, (d("200000"), d("0")));

    // (b) Dani: PEP, riesgo ALTO e ingreso de 20,000. Todo pasa: alertas
    // CRITICO sin congelamiento (ninguna regla disparada permite congelar).
    let dani = cliente(&pool, &cfg, "Dani").await;
    sqlx::query(
        "UPDATE kyc.clientes SET nivel_riesgo_pld = 'ALTO', es_pep = TRUE, ingreso_mensual_estimado = 20000 WHERE id = $1",
    )
    .bind(dani.cliente.id)
    .execute(&pool)
    .await
    .unwrap();
    depositar(&pool, &cfg, dani.cuenta_ifpe.id, "200000").await;
    retirar(&pool, &cfg, dani.cliente.id, "160000", &externa(5), None, &clave()).await.unwrap();
    let alertas = alertas_de(&pool, dani.cliente.id).await;
    assert_eq!(alertas.len(), 2, "{alertas:?}");
    assert!(alertas.iter().all(|a| a.1 == "CRITICO" && a.3 == "REGISTRADA"), "{alertas:?}");
    assert!(congelamiento_activo(&pool, dani.cliente.id).await.is_none());
    assert_eq!(saldo_ifpe(&pool, dani.cuenta_ifpe.id).await, (d("40000"), d("0")));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// §6.1: el congelamiento en cada camino del dinero
// -----------------------------------------------------------------------------

/// §6.1 prueba 7: un cliente congelado no debita por ninguna vía, ni desde
/// Rust ni con DML directo (ningún rol, ni el dueño del esquema).
#[sqlx::test(migrations = "../migrations")]
async fn congelado_no_debita_por_ninguna_via(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    // Fondeo de la SOFOM con un mutuo de Beto.
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "50000").await;
    invertir(&pool, &cfg, &beto, "50000", 182).await.unwrap();
    // Ana tiene 10,000 libres; la retención toma solo el patrón de 435,000.
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10000").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("10000"), d("435000")));

    let renglones = "SELECT (SELECT count(*) FROM ifpe.transacciones) + (SELECT count(*) FROM sofom.transacciones)";
    let antes = contar(&pool, renglones).await;
    let errores = [
        transferir(&pool, &cfg, &ana, &beto, "5000").await.unwrap_err(),
        retirar(&pool, &cfg, ana.cliente.id, "5000", "002010077777777771", None, &clave()).await.unwrap_err(),
        invertir(&pool, &cfg, &ana, "5000", 28).await.unwrap_err(),
        credito::originar(&pool, &cfg.pld, &solicitud_credito(ana.cliente.id, "5000")).await.unwrap_err(),
    ];
    for e in &errores {
        assert_eq!(e.codigo(), "OPERACION_NO_DISPONIBLE", "{e:?}");
        assert_eq!(e.to_string(), errores[0].to_string());
    }
    assert_eq!(contar(&pool, renglones).await, antes);
    let creditos: i64 = sqlx::query_scalar("SELECT count(*) FROM sofom.creditos WHERE acreditado_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(creditos, 0);
    assert_eq!(congelamiento_activo(&pool, ana.cliente.id).await.unwrap().2, 4);
    let bloqueadas: Vec<(String, String)> = sqlx::query_as(
        "SELECT canal, motivo_bloqueo FROM aml.evaluaciones WHERE cliente_id = $1 AND resultado = 'BLOQUEADA' \
          ORDER BY creado_en",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let esperado: Vec<(String, String)> =
        ["TRANSFERENCIA_ENVIADA", "SPEI_ENVIADO", "INVERSION_MUTUO", "DESEMBOLSO_CREDITO"]
            .into_iter()
            .map(|c| (c.to_string(), "CONGELADO".to_string()))
            .collect();
    assert_eq!(bloqueadas, esperado);

    // Base de datos: la guardia BEFORE INSERT rechaza a cualquier rol.
    let (a, b) = (ana.cuenta_ifpe.id, beto.cuenta_ifpe.id);
    let transferencia = format!(
        "INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia) \
         VALUES ('TRANSFERENCIA', '{a}', '{b}', 1, 'cli:{}')",
        Uuid::new_v4()
    );
    let traspaso = format!(
        "INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, monto, clave_idempotencia) \
         VALUES ('TRASPASO_A_SOFOM', '{a}', 1, 'pte:{}')",
        Uuid::new_v4()
    );
    for r in [
        como(&pool, Rol::Ifpe, &transferencia).await,
        como(&pool, Rol::Puente, &traspaso).await,
        como_dueno(&pool, &transferencia).await,
        como_dueno(&pool, &traspaso).await,
    ] {
        assert!(matches!(&r, Err(e) if e.contains("SC_OPERACION_NO_DISPONIBLE")), "{r:?}");
    }
    // Solo las funciones definer cambian el estado; el espejo protege al dueño.
    let activar = format!("UPDATE ifpe.cuentas SET estado = 'ACTIVA' WHERE id = '{a}'");
    let r = como(&pool, Rol::Ifpe, &activar).await;
    assert!(matches!(&r, Err(e) if e.contains("permission denied")), "{r:?}");
    let r = como_dueno(&pool, &activar).await;
    assert!(matches!(&r, Err(e) if e.contains("SC_AML_ESPEJO")), "{r:?}");
    assert_eq!(estado_cuenta(&pool, a).await, "CONGELADO_PREVENTIVO");
    assert_eq!(saldo_ifpe(&pool, a).await, (d("10000"), d("435000")));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 8: el congelamiento es linealizable con los débitos (ambos
/// toman primero la fila IFPE del cliente).
#[sqlx::test(migrations = "../migrations")]
async fn congelamiento_concurrente_con_debito(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let c1 = usuario_oficial(&pool, "Uno").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    for ronda in 0..10 {
        let ana = cliente(&pool, &cfg, &format!("Ana{ronda}")).await;
        depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
        let (congelado, debito) =
            tokio::join!(congelar_manual(&pool, &c1, ana.cliente.id), transferir(&pool, &cfg, &ana, &beto, "100"));
        assert_eq!(congelado["congelamiento"]["estado"], "ACTIVO");
        let congelado_en: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT congelado_en FROM aml.congelamientos WHERE cliente_id = $1")
                .bind(ana.cliente.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        match debito {
            Ok(m) => assert!(m.transaccion.creado_en < congelado_en, "ronda {ronda}: débito después del congelamiento"),
            Err(e) => assert_eq!(e.codigo(), "OPERACION_NO_DISPONIBLE", "ronda {ronda}: {e:?}"),
        }
        let tardias: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'TRANSFERENCIA' AND cuenta_origen_id = $1 \
                AND creado_en > $2",
        )
        .bind(ana.cuenta_ifpe.id)
        .bind(congelado_en)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tardias, 0, "ronda {ronda}");
    }
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 9: el dinero que entra a un cliente congelado se abona,
/// se confirma al emisor y se retiene completo.
#[sqlx::test(migrations = "../migrations")]
async fn entrante_a_congelado_se_retiene(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "5000").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let disponible = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0;
    let evaluaciones_antes = evaluaciones_de(&pool, ana.cliente.id).await.len();

    let (s, r) = api.webhook(&cfg, cuerpo_abono(&ana.cuenta_ifpe.clabe, "1000")).await;
    assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("ABONADO")), "{r}");
    let operador = api.token("operador@synthetic.mx", "operador-de-pruebas-123").await;
    let (s, r) = api
        .post(
            "/api/v1/operaciones/depositos",
            &operador,
            json!({
                "clabe": ana.cuenta_ifpe.clabe, "monto": "2000", "banco_emisor": "40012",
                "fecha_operacion": hoy_cdmx(), "clave_rastreo": "MBAN01CAPTURA",
            }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let tok_beto = token_cliente(&api, "Beto").await;
    let (s, r) = api
        .post(
            "/api/v1/ifpe/transferencias",
            &tok_beto,
            json!({ "clabe_destino": ana.cuenta_ifpe.clabe, "monto": "3000", "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!((r["transaccion"]["tipo"].as_str(), r["idempotente"].as_bool()), (Some("TRANSFERENCIA"), Some(false)));
    assert_eq!(r["transaccion"]["monto"], "3000.00000000");

    // Cada abono tiene su retención por el monto completo.
    let retenidos: Vec<(Decimal, Decimal)> = sqlx::query_as(
        "SELECT t.monto, r.monto FROM aml.retenciones r JOIN ifpe.transacciones t ON t.id = r.transaccion_origen_id \
          WHERE r.cliente_id = $1 AND t.monto < 145000 ORDER BY t.secuencia",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(retenidos, [(d("1000"), d("1000")), (d("2000"), d("2000")), (d("3000"), d("3000"))]);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (disponible, d("441000")));
    let nuevas: Vec<(String, String)> = evaluaciones_de(&pool, ana.cliente.id).await[evaluaciones_antes..]
        .iter()
        .map(|e| (e.0.clone(), e.1.clone()))
        .collect();
    assert_eq!(
        nuevas,
        [
            ("SPEI_WEBHOOK".to_string(), "RETENIDA".to_string()),
            ("SPEI_OPERADOR".to_string(), "RETENIDA".to_string()),
            ("TRANSFERENCIA_RECIBIDA".to_string(), "RETENIDA".to_string()),
        ]
    );
    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 10: el despachador no reclama las órdenes nunca intentadas de
/// un cliente congelado (y no se atasca); al levantar el congelamiento salen.
#[sqlx::test(migrations = "../migrations")]
async fn despacho_omite_ordenes_de_congelados(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let c1 = usuario_oficial(&pool, "Uno").await;
    let c2 = usuario_oficial(&pool, "Dos").await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let mut ordenes = Vec::new();
    for r in [&ana, &beto] {
        depositar(&pool, &cfg, r.cuenta_ifpe.id, "5000").await;
        ordenes.push(retirar(&pool, &cfg, r.cliente.id, "1000", &externa(7), None, &clave()).await.unwrap().orden.id);
    }
    let congelado = congelar_manual(&pool, &c1, ana.cliente.id).await;
    let orden = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, i32)>("SELECT estado, intentos_envio FROM ifpe.spei_outbound WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    vencer_ordenes(&pool).await;
    let despacho = outbound::despachar(&pool, &PasarelaSimulada::default(), &cfg.spei).await.unwrap();
    assert_eq!((despacho.reclamadas, despacho.errores), (1, 0), "{despacho:?}");
    assert_eq!(orden(ordenes[1]).await.1, 1, "la orden de Beto se reclamó");
    assert_eq!(orden(ordenes[0]).await, ("PENDIENTE".to_string(), 0), "la orden de Ana espera");
    // El trigger es el respaldo: ni el dueño puede reclamarla.
    let afectadas = sqlx::query("UPDATE ifpe.spei_outbound SET intentos_envio = 1 WHERE id = $1")
        .bind(ordenes[0])
        .execute(&pool)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(afectadas, 0);

    // Levantamiento aprobado con cuatro ojos: la orden sale en el siguiente ciclo.
    let alerta: Uuid = congelado["alerta"]["id"].as_str().unwrap().parse().unwrap();
    levantar_con_cuatro_ojos(&pool, &c1, &c2, alerta).await;
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "ACTIVA");
    vencer_ordenes(&pool).await;
    let despacho = outbound::despachar(&pool, &PasarelaSimulada::default(), &cfg.spei).await.unwrap();
    assert_eq!(despacho.errores, 0, "{despacho:?}");
    assert_eq!(orden(ordenes[0]).await.1, 1, "la orden de Ana se reclamó");

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 11: congelado no es cerrado. Los reembolsos de sus órdenes y
/// los vencimientos de sus mutuos se abonan sin retener; las correcciones del
/// operador siguen (auditadas) pero nunca alcanzan lo retenido.
#[sqlx::test(migrations = "../migrations")]
async fn reembolsos_vencimientos_y_correcciones_siguen(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let operador = operador_id(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let cuenta = ana.cuenta_ifpe.id;
    let libre = depositar(&pool, &cfg, cuenta, "10000").await;
    let mutuo = invertir(&pool, &cfg, &ana, "2000", 28).await.unwrap();
    let orden = retirar(&pool, &cfg, ana.cliente.id, "1000", &externa(3), None, &clave()).await.unwrap().orden;
    vencer_ordenes(&pool).await;
    outbound::despachar(&pool, &PasarelaSimulada::default(), &cfg.spei).await.unwrap();
    let estado: String = sqlx::query_scalar("SELECT estado FROM ifpe.spei_outbound WHERE id = $1")
        .bind(orden.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(estado, "ENVIADA");
    congelar_por_estructuracion(&pool, &cfg, cuenta).await;
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("7000"), d("435000")));

    // El proveedor rechaza la orden: el reembolso llega al disponible, sin retener.
    let t = outbound::aplicar(
        &pool,
        &cfg.spei,
        orden.id,
        &Evento::Rechazada { causa: 1, motivo: "Cuenta inexistente".into() },
    )
    .await
    .unwrap();
    assert!(t.cambio, "{t:?}");
    assert_eq!(saldo_ifpe(&pool, cuenta).await, (d("8000"), d("435000")));

    // El mutuo vence y se liquida: RETORNO_DE_SOFOM al disponible, sin retener.
    envejecer_mutuo(&pool, mutuo.contrato.id, 30).await;
    credito::aportar_capital(&pool, d("1000"), Some("Capital inicial".into()), clave()).await.unwrap();
    let liquidacion = puente::liquidar(&pool, mutuo.contrato.id, None).await.unwrap();
    let (disponible, bloqueado) = saldo_ifpe(&pool, cuenta).await;
    assert_eq!(disponible, d("8000") + liquidacion.tx_ifpe.monto, "{liquidacion:?}");
    assert!(liquidacion.tx_ifpe.monto >= d("2000"));
    assert_eq!(bloqueado, d("435000"));
    assert_eq!(retenido(&pool, cuenta).await, d("435000"));

    // Corrección de un abono libre: procede, se registra y se avisa con prioridad ALTA.
    let spei_libre: Uuid = sqlx::query_scalar("SELECT id FROM ifpe.spei_inbound WHERE transaccion_id = $1")
        .bind(libre.transaccion.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let corregir = |spei_inbound_id: Uuid, monto: &str| inbound::SolicitudCorreccion {
        spei_inbound_id,
        monto: d(monto),
        motivo: "Abono duplicado por error del banco emisor".into(),
        operador_id: operador,
        clave_idempotencia: clave(),
    };
    inbound::corregir(&pool, &corregir(spei_libre, "5000")).await.unwrap();
    assert!(eventos_bitacora(&pool, ana.cliente.id).await.iter().any(|e| e == "CARGO_EN_CUENTA_CONGELADA"));
    let prioridad: String = sqlx::query_scalar(
        "SELECT prioridad FROM aml.notificaciones WHERE cliente_id = $1 AND tipo = 'CARGO_EN_CUENTA_CONGELADA'",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(prioridad, "ALTA");
    // Corregir un abono retenido: el disponible no alcanza.
    let spei_retenido: Uuid = sqlx::query_scalar(
        "SELECT s.id FROM ifpe.spei_inbound s WHERE s.cuenta_id = $1 AND s.monto = 145000 ORDER BY s.recibido_en DESC LIMIT 1",
    )
    .bind(cuenta)
    .fetch_one(&pool)
    .await
    .unwrap();
    let e = inbound::corregir(&pool, &corregir(spei_retenido, "145000")).await.unwrap_err();
    assert_eq!(e.codigo(), "SALDO_INSUFICIENTE");
    assert_eq!(saldo_ifpe(&pool, cuenta).await.1, d("435000"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 12: lo retenido por PLD no se libera por ninguna vía del
/// operador ni se puede falsificar una retención.
#[sqlx::test(migrations = "../migrations")]
async fn retencion_aml_protegida(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let operador = operador_id(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let cuenta = ana.cuenta_ifpe.id;
    congelar_por_estructuracion(&pool, &cfg, cuenta).await;
    // Una captura del operador a la cuenta congelada: se retiene completa.
    let captura = depositar_por(&pool, &cfg, cuenta, "3000", Origen::Operador(operador)).await;
    let saldo = saldo_ifpe(&pool, cuenta).await;
    assert_eq!(saldo, (d("0"), d("438000")));

    // LIBERACION genérica del operador sobre lo retenido.
    let e = ledger::retener(
        &pool,
        &ledger::Retencion {
            cuenta_id: cuenta,
            monto: d("438000"),
            motivo: "Liberación solicitada por el cliente".into(),
            liberar: true,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "RETENCION_PROTEGIDA", "{e:?}");

    // Retención falsificada por el dueño con una clave reservada.
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "1000").await;
    let b = beto.cuenta_ifpe.id;
    let r = como_dueno(
        &pool,
        &format!(
            "UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100, saldo_bloqueado = saldo_bloqueado + 100 \
              WHERE id = '{b}'; \
             INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, concepto, clave_idempotencia) \
             VALUES ('RETENCION', '{b}', '{b}', 100, 'Retención temporal', 'ret:{}')",
            Uuid::new_v4()
        ),
    )
    .await;
    assert!(matches!(&r, Err(e) if e.contains("SC_AML_CLAVE_RESERVADA")), "{r:?}");
    assert_eq!(saldo_ifpe(&pool, b).await, (d("1000"), d("0")));

    // Anular la captura retenida: el disponible no alcanza.
    let spei: Uuid = sqlx::query_scalar("SELECT id FROM ifpe.spei_inbound WHERE transaccion_id = $1")
        .bind(captura.transaccion.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let e = inbound::anular(
        &pool,
        &inbound::SolicitudAnulacion {
            spei_inbound_id: spei,
            motivo: "Captura con CLABE equivocada".into(),
            operador_id: operador,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "SALDO_INSUFICIENTE", "{e:?}");
    assert_eq!(saldo_ifpe(&pool, cuenta).await, saldo);
    assert_eq!(retenido(&pool, cuenta).await, d("438000"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// §6.1: resolución por cumplimiento (cuatro ojos), segregación y sin aviso
// -----------------------------------------------------------------------------

/// §6.1 prueba 13: un oficial propone FALSO_POSITIVO sobre las alertas del
/// cliente congelado; otro aprueba (cuatro ojos). Se levanta el congelamiento,
/// se liberan las retenciones y la marca de agua suprime el patrón revisado.
#[sqlx::test(migrations = "../migrations")]
async fn falso_positivo_cuatro_ojos_libera_y_suprime(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let (c1_id, c1) = token_oficial(&api, &pool, "Uno").await;
    let (c2_id, c2) = token_oficial(&api, &pool, "Dos").await;
    let r05 = alerta_de(&pool, ana.cliente.id, "R05_ESTRUCTURACION").await;
    let r02 = alerta_de(&pool, ana.cliente.id, "R02_FRACCIONAMIENTO").await;
    let resolver = |id: Uuid| format!("/api/v1/aml/alertas/{id}/resolver");

    // TOMAR: 201; la misma clave, 200 idempotente; tomarla otra vez es un éxito sin efecto.
    let toma = json!({ "accion": "TOMAR", "clave_idempotencia": "toma-r05-1" });
    let (s, v) = api.post(&resolver(r05), &c1, toma.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["alerta"]["estado"].as_str(), v["idempotente"].as_bool()), (Some("EN_REVISION"), Some(false)));
    assert!(v["congelamiento"].is_object() && v["solicitud"].is_null(), "{v}");
    let (s, v) = api.post(&resolver(r05), &c1, toma).await;
    assert_eq!((s, v["idempotente"].as_bool()), (StatusCode::OK, Some(true)));
    let (s, _) = api.post(&resolver(r05), &c1, json!({ "accion": "TOMAR", "clave_idempotencia": "toma-r05-2" })).await;
    assert_eq!(s, StatusCode::CREATED);
    // Otro oficial no puede actuar sobre una alerta ajena.
    let (s, v) =
        api.post(&resolver(r05), &c2, json!({ "accion": "LIBERAR", "clave_idempotencia": "libera-c2-1" })).await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "AML_ASIGNADA_A_OTRO"));

    // Campos fuera de lugar o desconocidos: 400. Dictamen corto: 422.
    for cuerpo in [
        json!({ "accion": "TOMAR", "urgente": false, "clave_idempotencia": "mal-0001" }),
        json!({ "accion": "LIBERAR", "dictamen": DICTAMEN, "clave_idempotencia": "mal-0002" }),
        json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "congelar": true, "clave_idempotencia": "mal-0003" }),
        json!({ "accion": "TOMAR", "otro": 1, "clave_idempotencia": "mal-0004" }),
        json!({ "accion": "ARCHIVAR", "clave_idempotencia": "mal-0005" }),
        json!({ "accion": "TOMAR", "clave_idempotencia": "corta" }),
    ] {
        let (s, v) = api.post(&resolver(r05), &c1, cuerpo.clone()).await;
        assert_eq!((s, error(&v)), (StatusCode::BAD_REQUEST, "VALIDACION"), "{cuerpo}: {v}");
    }
    let (s, v) = api
        .post(
            &resolver(r05),
            &c1,
            json!({ "accion": "FALSO_POSITIVO", "dictamen": "corto", "clave_idempotencia": "fp-corto-1" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_DICTAMEN_INVALIDO"));

    // FALSO_POSITIVO con el cliente congelado: propuesta + solicitud PENDIENTE.
    let (s, v) = api
        .post(
            &resolver(r05),
            &c1,
            json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "clave_idempotencia": "fp-r05-1" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["alerta"]["estado"], "EN_REVISION");
    assert_eq!(v["alerta"]["resolucion_propuesta"], "FALSO_POSITIVO");
    assert_eq!(
        (v["solicitud"]["estado"].as_str(), v["solicitud"]["tipo"].as_str()),
        (Some("PENDIENTE"), Some("LEVANTAR_Y_LIBERAR"))
    );
    let solicitud = v["solicitud"]["id"].as_str().unwrap().to_string();
    let congelamiento = v["congelamiento"]["id"].as_str().unwrap().to_string();
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO", "la propuesta sola no levanta");
    let (_, q) = api.get("/api/v1/aml/solicitudes", &c2).await;
    assert_eq!(q["solicitudes"].as_array().unwrap().len(), 1, "{q}");
    assert_eq!(q["solicitudes"][0]["id"], solicitud.as_str());

    let decision = format!("/api/v1/aml/solicitudes/{solicitud}/decision");
    let aprobar = json!({ "aprobar": true, "comentario": "Revisado el expediente completo.", "clave_idempotencia": "aprueba-c2-1" });
    // La alerta R02 (ALTO) sigue abierta sin propuesta.
    let (s, v) = api
        .post(
            &decision,
            &c2,
            json!({ "aprobar": true, "comentario": "Revisado el expediente.", "clave_idempotencia": "aprueba-c2-0" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "AML_ALERTAS_ABIERTAS"), "{v}");
    for (clave, accion) in [
        ("toma-r02-1", json!({ "accion": "TOMAR" })),
        ("fp-r02-1", json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN })),
    ] {
        let mut cuerpo = accion;
        cuerpo["clave_idempotencia"] = json!(clave);
        let (s, v) = api.post(&resolver(r02), &c1, cuerpo).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        // La solicitud ya existe: la segunda propuesta no crea otra.
        assert!(v["solicitud"].is_null() || v["solicitud"]["id"] == solicitud.as_str(), "{v}");
    }
    let pendientes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM aml.solicitudes_levantamiento WHERE congelamiento_id = $1::uuid")
            .bind(&congelamiento)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pendientes, 1);
    // Comentario corto: 422. El solicitante no decide: cuatro ojos.
    let (s, v) = api
        .post(&decision, &c2, json!({ "aprobar": true, "comentario": "ok", "clave_idempotencia": "aprueba-c2-x" }))
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_DICTAMEN_INVALIDO"));
    let (s, v) = api
        .post(
            &decision,
            &c1,
            json!({ "aprobar": true, "comentario": "Me apruebo a mí mismo.", "clave_idempotencia": "aprueba-c1-1" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_CUATRO_OJOS"));

    let (s, v) = api.post(&decision, &c2, aprobar.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["solicitud"]["estado"], "APROBADA");
    assert_eq!(v["congelamiento"]["estado"], "LEVANTADO");
    assert_eq!(v["congelamiento"]["retenido_vigente"], "0.00000000");
    assert_eq!(v["alertas_resueltas"].as_array().unwrap().len(), 2);
    let liberaciones = v["liberaciones"].as_array().unwrap();
    assert_eq!(liberaciones.len(), 1);
    assert_eq!(liberaciones[0]["monto"], "435000.00000000");
    let (clave_lib, concepto): (String, Option<String>) =
        sqlx::query_as("SELECT clave_idempotencia, concepto FROM ifpe.transacciones WHERE id = $1::uuid")
            .bind(liberaciones[0]["transaccion_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(clave_lib.starts_with("lib:"), "{clave_lib}");
    assert_eq!(concepto.as_deref(), Some("Liberación de retención"));
    let t: synthetic_capital::models::ifpe::TransaccionIfpe =
        sqlx::query_as("SELECT * FROM ifpe.transacciones WHERE id = $1::uuid")
            .bind(liberaciones[0]["transaccion_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(hash::hash_ifpe(&t), t.tx_hash);
    // Las dos alertas: resueltas por C1 (quien propuso) y aprobadas por C2.
    let resueltas: Vec<(String, String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT regla_codigo, estado::text, resuelta_por, aprobada_por FROM aml.alertas WHERE cliente_id = $1 ORDER BY folio",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        resueltas,
        [
            ("R02_FRACCIONAMIENTO".to_string(), "FALSO_POSITIVO".to_string(), Some(c1_id), Some(c2_id)),
            ("R05_ESTRUCTURACION".to_string(), "FALSO_POSITIVO".to_string(), Some(c1_id), Some(c2_id)),
        ]
    );
    assert!(congelamiento_activo(&pool, ana.cliente.id).await.is_none());
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "ACTIVA");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("435000"), d("0")));
    // Ana vuelve a operar.
    transferir(&pool, &cfg, &ana, &beto, "1000").await.unwrap();
    // Repetición: 200 con el resultado guardado; otra solicitud con la misma clave: 409.
    let (s, v2) = api.post(&decision, &c2, aprobar).await;
    assert_eq!((s, v2["idempotente"].as_bool()), (StatusCode::OK, Some(true)));
    assert_eq!(v2["liberaciones"], v["liberaciones"]);
    let (s, v) = api
        .post(&decision, &c2, json!({ "aprobar": false, "comentario": "Revisado el expediente completo.", "clave_idempotencia": "aprueba-c2-1" }))
        .await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "CLAVE_IDEMPOTENCIA_REUTILIZADA"));
    let (s, v) = api
        .post(
            &decision,
            &c2,
            json!({ "aprobar": true, "comentario": "Otra vez lo mismo.", "clave_idempotencia": "aprueba-c2-2" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "AML_SOLICITUD_NO_PENDIENTE"));

    // Marca de agua: un cuarto depósito dentro de las 24 horas ya no dispara
    // R05 ni R02 (se revisaron) y no congela ni retiene; R07 sí (MEDIO).
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "145000").await;
    let abiertas: Vec<(String, String, String)> = alertas_de(&pool, ana.cliente.id)
        .await
        .into_iter()
        .filter(|a| a.2 == "PENDIENTE" || a.2 == "EN_REVISION")
        .map(|a| (a.0, a.1, a.3))
        .collect();
    assert_eq!(abiertas, [("R07_VOLUMEN_DIARIO".to_string(), "MEDIO".to_string(), "REGISTRADA".to_string())]);
    assert!(congelamiento_activo(&pool, ana.cliente.id).await.is_none());
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("579000"), d("0")));

    // Solicitudes sobre un congelamiento ya levantado.
    let solicitudes = format!("/api/v1/aml/congelamientos/{congelamiento}/solicitudes");
    let justificacion = JUSTIFICACION;
    let (s, v) = api
        .post(
            &solicitudes,
            &c1,
            json!({ "tipo": "LEVANTAR_Y_LIBERAR", "justificacion": justificacion, "clave_idempotencia": "sol-0001" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "AML_CONGELAMIENTO_NO_ACTIVO"));
    let (s, v) = api
        .post(
            &solicitudes,
            &c1,
            json!({ "tipo": "LIBERAR_RETENCIONES", "justificacion": justificacion, "clave_idempotencia": "sol-0002" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "AML_SIN_RETENCIONES"));
    let (s, v) = api
        .post(
            &format!("/api/v1/aml/congelamientos/{}/solicitudes", Uuid::new_v4()),
            &c1,
            json!({ "tipo": "LEVANTAR_Y_LIBERAR", "justificacion": justificacion, "clave_idempotencia": "sol-0003" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::NOT_FOUND, "NO_ENCONTRADO"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 14: levantamiento conservando las retenciones y liberación
/// posterior con otra solicitud de cuatro ojos.
#[sqlx::test(migrations = "../migrations")]
async fn levantamiento_parcial_y_liberacion_posterior(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10000").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let (_, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, c2) = token_oficial(&api, &pool, "Dos").await;
    let r05 = alerta_de(&pool, ana.cliente.id, "R05_ESTRUCTURACION").await;
    let r02 = alerta_de(&pool, ana.cliente.id, "R02_FRACCIONAMIENTO").await;
    let mut solicitud = Value::Null;
    for (alerta, cuerpo) in [
        (r05, json!({ "accion": "TOMAR" })),
        (
            r05,
            json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "tipo_levantamiento": "LEVANTAR_CONSERVANDO_RETENCIONES" }),
        ),
        (r02, json!({ "accion": "TOMAR" })),
        (r02, json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN })),
    ] {
        let mut cuerpo = cuerpo;
        cuerpo["clave_idempotencia"] = json!(clave_api());
        let (s, v) = api.post(&format!("/api/v1/aml/alertas/{alerta}/resolver"), &c1, cuerpo).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        if !v["solicitud"].is_null() {
            solicitud = v["solicitud"].clone();
        }
    }
    assert_eq!(solicitud["tipo"], "LEVANTAR_CONSERVANDO_RETENCIONES", "{solicitud}");
    let decidir = |id: &Value| format!("/api/v1/aml/solicitudes/{}/decision", id.as_str().unwrap());
    let aprobar = || json!({ "aprobar": true, "comentario": "Revisado el expediente completo.", "clave_idempotencia": clave_api() });
    let (s, v) = api.post(&decidir(&solicitud["id"]), &c2, aprobar()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["liberaciones"], json!([]));
    assert_eq!(v["congelamiento"]["estado"], "LEVANTADO");
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "ACTIVA");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("10000"), d("435000")));
    assert_eq!(retenido(&pool, ana.cuenta_ifpe.id).await, d("435000"));
    // Opera con lo disponible; lo retenido sigue protegido.
    transferir(&pool, &cfg, &ana, &beto, "5000").await.unwrap();
    assert_eq!(transferir(&pool, &cfg, &ana, &beto, "6000").await.unwrap_err().codigo(), "SALDO_INSUFICIENTE");

    // Liberación posterior: C1 solicita, C2 aprueba.
    let congelamiento = v["congelamiento"]["id"].as_str().unwrap().to_string();
    let (s, v) = api
        .post(
            &format!("/api/v1/aml/congelamientos/{congelamiento}/solicitudes"),
            &c1,
            json!({ "tipo": "LIBERAR_RETENCIONES", "justificacion": JUSTIFICACION, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = api.post(&decidir(&v["solicitud"]["id"]), &c1, aprobar()).await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_CUATRO_OJOS"));
    let (s, v2) = api
        .post(
            &format!("/api/v1/aml/congelamientos/{congelamiento}/solicitudes"),
            &c1,
            json!({ "tipo": "LIBERAR_RETENCIONES", "justificacion": JUSTIFICACION, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!((s, error(&v2)), (StatusCode::CONFLICT, "AML_SOLICITUD_PENDIENTE"));
    let pendiente: String = sqlx::query_scalar(
        "SELECT id::text FROM aml.solicitudes_levantamiento WHERE congelamiento_id = $1::uuid AND estado = 'PENDIENTE'",
    )
    .bind(&congelamiento)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (s, v) = api.post(&decidir(&json!(pendiente)), &c2, aprobar()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["liberaciones"].as_array().unwrap().len(), 1);
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("440000"), d("0")));
    assert_eq!(retenido(&pool, ana.cuenta_ifpe.id).await, d("0"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 15: CONFIRMAR (reporte de operación inusual) anula la solicitud
/// de levantamiento pendiente, conserva y confirma el congelamiento, eleva el
/// riesgo del cliente y abre el plazo de reporte a la UIF.
#[sqlx::test(migrations = "../migrations")]
async fn confirmar_conserva_congelamiento_y_reporta_uif(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let (_, c1) = token_oficial(&api, &pool, "Uno").await;
    let r05 = alerta_de(&pool, ana.cliente.id, "R05_ESTRUCTURACION").await;
    let r02 = alerta_de(&pool, ana.cliente.id, "R02_FRACCIONAMIENTO").await;
    let resolver = |id: Uuid| format!("/api/v1/aml/alertas/{id}/resolver");
    for (alerta, cuerpo) in [
        (r05, json!({ "accion": "TOMAR" })),
        (r05, json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN })),
        (r02, json!({ "accion": "TOMAR" })),
    ] {
        let mut cuerpo = cuerpo;
        cuerpo["clave_idempotencia"] = json!(clave_api());
        let (s, v) = api.post(&resolver(alerta), &c1, cuerpo).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let solicitud: Uuid = sqlx::query_scalar(
        "SELECT id FROM aml.solicitudes_levantamiento WHERE estado = 'PENDIENTE' AND solicitada_por IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let (s, v) = api
        .post(
            &resolver(r02),
            &c1,
            json!({ "accion": "CONFIRMAR", "dictamen": DICTAMEN, "urgente": true, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["alerta"]["estado"], "CONFIRMADO");
    assert_eq!(v["congelamiento"]["confirmado"], true, "{v}");
    let (estado, comentario): (String, Option<String>) =
        sqlx::query_as("SELECT estado, comentario FROM aml.solicitudes_levantamiento WHERE id = $1")
            .bind(solicitud)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(estado, "ANULADA", "{comentario:?}");
    let (estado_r05, propuesta): (String, Option<String>) =
        sqlx::query_as("SELECT estado::text, resolucion_propuesta FROM aml.alertas WHERE id = $1")
            .bind(r05)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((estado_r05.as_str(), propuesta), ("EN_REVISION", None), "la propuesta FP se retira");
    let (_, origen, _) = congelamiento_activo(&pool, ana.cliente.id).await.expect("sigue congelada");
    assert_eq!(origen, "AUTOMATICO");
    let confirmado: bool = sqlx::query_scalar("SELECT confirmado FROM aml.congelamientos WHERE cliente_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(confirmado);
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    let nivel: String = sqlx::query_scalar("SELECT nivel_riesgo_pld::text FROM kyc.clientes WHERE id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(nivel, "ALTO");
    let (urgente, a_tiempo): (bool, bool) = sqlx::query_as(
        "SELECT urgente, fecha_limite_reporte <= now() + interval '24 hours' FROM aml.alertas WHERE id = $1",
    )
    .bind(r02)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(urgente && a_tiempo);

    // Folio SITI de la UIF: una sola vez.
    let reporte = format!("/api/v1/aml/alertas/{r02}/reporte-uif");
    let (s, v) = api
        .post(
            &reporte,
            &c1,
            json!({ "folio_uif": "SITI-2026-0001", "reportada_en": hoy_cdmx(), "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["alerta"]["folio_uif"], "SITI-2026-0001");
    let (s, v) = api
        .post(
            &reporte,
            &c1,
            json!({ "folio_uif": "SITI-2026-0001", "reportada_en": hoy_cdmx(), "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_FOLIO_REGISTRADO"));

    // El cliente no ve su nivel de riesgo.
    let tok_ana = token_cliente(&api, "Ana").await;
    let (s, yo) = api.get("/api/v1/yo", &tok_ana).await;
    assert_eq!(s, StatusCode::OK, "{yo}");
    for campo in ["nivel_riesgo_pld", "es_pep", "riesgo_evaluado_en"] {
        assert!(yo.get(campo).is_none(), "{campo}: {yo}");
    }

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 16: congelamiento manual (un solo oficial puede endurecer) y
/// escalamiento con congelamiento.
#[sqlx::test(migrations = "../migrations")]
async fn congelamiento_manual_y_escalamiento(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let carla = cliente(&pool, &cfg, "Carla").await;
    let (c1_id, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, c2) = token_oficial(&api, &pool, "Dos").await;

    let congelar = format!("/api/v1/aml/clientes/{}/congelar", beto.cliente.id);
    let (s, v) = api.post(&congelar, &c1, json!({ "motivo": MOTIVO, "clave_idempotencia": clave_api() })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["ya_congelado"].as_bool(), v["idempotente"].as_bool()), (Some(false), Some(false)));
    assert_eq!(v["alerta"]["regla"]["codigo"], "R30_CONGELAMIENTO_MANUAL");
    assert_eq!(v["congelamiento"]["origen"], "MANUAL");
    let congelado_por: Option<Uuid> =
        sqlx::query_scalar("SELECT congelado_por FROM aml.congelamientos WHERE cliente_id = $1")
            .bind(beto.cliente.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(congelado_por, Some(c1_id));
    assert_eq!(estado_cuenta(&pool, beto.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    // Otra clave (otro oficial): 200 con el congelamiento existente.
    let (s, v2) = api.post(&congelar, &c2, json!({ "motivo": MOTIVO, "clave_idempotencia": clave_api() })).await;
    assert_eq!(s, StatusCode::OK, "{v2}");
    assert_eq!((v2["ya_congelado"].as_bool(), v2["idempotente"].as_bool()), (Some(true), Some(true)));
    assert_eq!(v2["congelamiento"]["id"], v["congelamiento"]["id"]);
    let congelamientos: i64 = sqlx::query_scalar("SELECT count(*) FROM aml.congelamientos WHERE cliente_id = $1")
        .bind(beto.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(congelamientos, 1);
    // Motivo corto: 422; cliente inexistente: 404.
    let (s, v) = api.post(&congelar, &c1, json!({ "motivo": "breve", "clave_idempotencia": clave_api() })).await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_DICTAMEN_INVALIDO"));
    let (s, v) = api
        .post(
            &format!("/api/v1/aml/clientes/{}/congelar", Uuid::new_v4()),
            &c1,
            json!({ "motivo": MOTIVO, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::NOT_FOUND, "NO_ENCONTRADO"));

    // ESCALAR con congelamiento sobre una alerta ALTO (R02).
    depositar(&pool, &cfg, carla.cuenta_ifpe.id, "140000").await;
    depositar(&pool, &cfg, carla.cuenta_ifpe.id, "140000").await;
    let r02 = alerta_de(&pool, carla.cliente.id, "R02_FRACCIONAMIENTO").await;
    let resolver = format!("/api/v1/aml/alertas/{r02}/resolver");
    let (s, _) = api.post(&resolver, &c1, json!({ "accion": "TOMAR", "clave_idempotencia": clave_api() })).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = api
        .post(
            &resolver,
            &c1,
            json!({ "accion": "ESCALAR", "dictamen": DICTAMEN, "congelar": true, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(
        (v["alerta"]["escalada"].as_bool(), v["alerta"]["nivel_riesgo"].as_str()),
        (Some(true), Some("CRITICO"))
    );
    assert_eq!(v["congelamiento"]["origen"], "MANUAL", "{v}");
    let (_, origen, _) = congelamiento_activo(&pool, carla.cliente.id).await.expect("Carla congelada");
    assert_eq!(origen, "MANUAL");
    assert_eq!(estado_cuenta(&pool, carla.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    assert!(notificaciones_de(&pool, carla.cliente.id).await.iter().any(|n| n == "ALERTA_CRITICA"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 17: segregación de funciones en la API y en la base de datos.
#[sqlx::test(migrations = "../migrations")]
async fn segregacion_de_funciones(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let tok_ana = token_cliente(&api, "Ana").await;
    let operador = api.token("operador@synthetic.mx", "operador-de-pruebas-123").await;
    let operador_id = operador_id(&pool).await;
    let (_, aud_email) = auditor(&pool, "Auditora").await;
    let aud = api.token(&aud_email, PASSWORD_INTERNO).await;
    let (_, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, c2) = token_oficial(&api, &pool, "Dos").await;
    let (c3_id, c3) = token_oficial(&api, &pool, "Tres").await;
    // C1 congela a C3 (un oficial también puede ser objeto de un expediente).
    let (s, v) = api
        .post(
            &format!("/api/v1/aml/clientes/{c3_id}/congelar"),
            &c1,
            json!({ "motivo": MOTIVO, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let r30 = v["alerta"]["id"].as_str().unwrap().to_string();
    let r05 = alerta_de(&pool, ana.cliente.id, "R05_ESTRUCTURACION").await;
    let congelamiento = congelamiento_activo(&pool, ana.cliente.id).await.unwrap().0;

    let lecturas = [
        "/api/v1/aml/resumen".to_string(),
        "/api/v1/aml/alertas".into(),
        format!("/api/v1/aml/alertas/{r05}"),
        format!("/api/v1/aml/clientes/{}", ana.cliente.id),
        "/api/v1/aml/congelamientos".into(),
        "/api/v1/aml/solicitudes".into(),
        "/api/v1/aml/notificaciones".into(),
        "/api/v1/aml/reglas".into(),
        "/api/v1/aml/propuestas".into(),
        "/api/v1/aml/relevantes".into(),
        "/api/v1/aml/bitacora/verificacion".into(),
    ];
    let escrituras = [
        (
            format!("/api/v1/aml/alertas/{r05}/resolver"),
            json!({ "accion": "TOMAR", "clave_idempotencia": clave_api() }),
        ),
        (
            format!("/api/v1/aml/alertas/{r05}/reporte-uif"),
            json!({ "folio_uif": "SITI-2026-0009", "reportada_en": hoy_cdmx(), "clave_idempotencia": clave_api() }),
        ),
        (
            format!("/api/v1/aml/clientes/{}/congelar", ana.cliente.id),
            json!({ "motivo": MOTIVO, "clave_idempotencia": clave_api() }),
        ),
        (
            format!("/api/v1/aml/congelamientos/{congelamiento}/solicitudes"),
            json!({ "tipo": "LEVANTAR_Y_LIBERAR", "justificacion": JUSTIFICACION, "clave_idempotencia": clave_api() }),
        ),
        (
            format!("/api/v1/aml/solicitudes/{}/decision", Uuid::new_v4()),
            json!({ "aprobar": true, "comentario": "Revisado el expediente.", "clave_idempotencia": clave_api() }),
        ),
        (
            "/api/v1/aml/propuestas".into(),
            json!({ "tipo": "PARAMETROS", "cambios": { "dias_aviso_plazo": 5 }, "justificacion": JUSTIFICACION, "clave_idempotencia": clave_api() }),
        ),
        (
            format!("/api/v1/aml/propuestas/{}/decision", Uuid::new_v4()),
            json!({ "aprobar": false, "comentario": "Revisada la propuesta.", "clave_idempotencia": clave_api() }),
        ),
    ];
    assert_eq!(lecturas.len() + escrituras.len(), 18);
    for ruta in &lecturas {
        let (s, v) = api.get(ruta, &aud).await;
        assert_eq!(s, StatusCode::OK, "AUDITOR GET {ruta}: {v}");
        for token in [&tok_ana, &operador] {
            let (s, v) = api.get(ruta, token).await;
            assert_eq!((s, error(&v)), (StatusCode::FORBIDDEN, "PROHIBIDO"), "GET {ruta}");
        }
        let (s, _) = api.llamar("GET", ruta, None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{ruta}");
    }
    let bitacora_antes = contar(&pool, "SELECT count(*) FROM aml.bitacora WHERE evento <> 'CONSULTA_EXPEDIENTE'").await;
    for (ruta, cuerpo) in &escrituras {
        for token in [&aud, &tok_ana, &operador] {
            let (s, v) = api.post(ruta, token, cuerpo.clone()).await;
            assert_eq!((s, error(&v)), (StatusCode::FORBIDDEN, "PROHIBIDO"), "POST {ruta}: {v}");
        }
    }
    assert_eq!(
        contar(&pool, "SELECT count(*) FROM aml.bitacora WHERE evento <> 'CONSULTA_EXPEDIENTE'").await,
        bitacora_antes,
        "ningún POST rechazado escribe"
    );

    // Autorresolución: C3 no toma la alerta sobre sí mismo.
    let (s, v) = api
        .post(
            &format!("/api/v1/aml/alertas/{r30}/resolver"),
            &c3,
            json!({ "accion": "TOMAR", "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_AUTORESOLUCION"), "{v}");
    // Una alerta tomada por C1 no la resuelve C2.
    let resolver = format!("/api/v1/aml/alertas/{r05}/resolver");
    let (s, _) = api.post(&resolver, &c1, json!({ "accion": "TOMAR", "clave_idempotencia": clave_api() })).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = api
        .post(
            &resolver,
            &c2,
            json!({ "accion": "FALSO_POSITIVO", "dictamen": DICTAMEN, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::CONFLICT, "AML_ASIGNADA_A_OTRO"));
    // La bandeja PLD heredada ya no existe.
    let (s, v) = api.get("/api/v1/operaciones/alertas", &operador).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");

    // Base de datos: la función definer vuelve a exigir el rol del actor.
    let r = como(
        &pool,
        Rol::Aml,
        &format!("SELECT aml.resolver_alerta('{operador_id}', '{r05}', 'TOMAR', NULL, NULL, NULL, NULL, NULL, 'cum:{operador_id}:toma-0001')"),
    )
    .await;
    assert!(matches!(&r, Err(e) if e.contains("SC_AML_NO_AUTORIZADO")), "{r:?}");
    for rol in [Rol::Ifpe, Rol::Spei, Rol::Puente, Rol::Sofom, Rol::Kyc] {
        let r = como(&pool, rol, "SELECT 1 FROM aml.alertas").await;
        assert!(matches!(&r, Err(e) if e.contains("permission denied")), "{r:?}");
    }
    for sql in ["UPDATE aml.bitacora SET motivo = 'x'", "DELETE FROM aml.bitacora", "TRUNCATE aml.bitacora"] {
        let r = como_dueno(&pool, sql).await;
        assert!(matches!(&r, Err(e) if e.contains("SC_AML_BITACORA_INMUTABLE")), "{sql}: {r:?}");
    }

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 18: sin aviso al cliente. Ninguna respuesta de las rutas del
/// cliente revela la alerta, el congelamiento ni el riesgo; todo bloqueo
/// responde los mismos bytes.
#[sqlx::test(migrations = "../migrations")]
async fn sin_aviso_al_cliente(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let tok_ana = token_cliente(&api, "Ana").await;
    let tok_beto = token_cliente(&api, "Beto").await;
    // Ana: 10,000 libres y luego la estructuración, todo por el webhook del proveedor.
    for monto in ["10000", "145000", "145000", "145000"] {
        let (s, r) = api.webhook(&cfg, cuerpo_abono(&ana.cuenta_ifpe.clabe, monto)).await;
        assert_eq!((s, &r["mensaje"], &r["estado"]), (StatusCode::OK, &json!("confirmar"), &json!("ABONADO")), "{r}");
    }
    assert!(congelamiento_activo(&pool, ana.cliente.id).await.is_some());

    // Quien envía a un cliente congelado ve un éxito normal.
    let (s, r) = api.webhook(&cfg, cuerpo_abono(&beto.cuenta_ifpe.clabe, "450000")).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (s, r) = api
        .post(
            "/api/v1/ifpe/transferencias",
            &tok_beto,
            json!({ "clabe_destino": ana.cuenta_ifpe.clabe, "monto": "1000", "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");

    // Ana intenta transferir: bloqueo por congelamiento.
    let (s, bloqueo_ana) = api
        .llamar_crudo(
            "POST",
            "/api/v1/ifpe/transferencias",
            Some(&tok_ana),
            Some(
                json!({ "clabe_destino": beto.cuenta_ifpe.clabe, "monto": "5000", "clave_idempotencia": clave_api() }),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    // Beto dispersa en tres SPEI: el tercero se bloquea por puntaje crítico.
    let mut bloqueo_beto = Vec::new();
    for n in 0..3 {
        let (s, cuerpo) = api
            .llamar_crudo(
                "POST",
                "/api/v1/spei/transfer",
                Some(&tok_beto),
                Some(json!({
                    "clabe_destino": externa(20 + n), "beneficiario_nombre": "Proveedor Externo",
                    "monto": "145000", "clave_idempotencia": clave_api(),
                })),
            )
            .await;
        if n < 2 {
            assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&cuerpo));
        } else {
            assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
            bloqueo_beto = cuerpo;
        }
    }
    assert!(congelamiento_activo(&pool, beto.cliente.id).await.is_some());
    assert_eq!(bloqueo_ana, bloqueo_beto, "los bloqueos son idénticos byte a byte");
    assert_eq!(String::from_utf8(bloqueo_ana).unwrap(), NO_DISPONIBLE);

    // Ninguna lectura del cliente delata la situación.
    let prohibidas = [
        "aml",
        "pld",
        "alerta",
        "cumplimiento",
        "uif",
        "sospech",
        "congelado",
        "preventiv",
        "nivel_riesgo",
        "es_pep",
        "riesgo_evaluado",
    ];
    for ruta in [
        "/api/v1/yo",
        "/api/v1/tablero",
        "/api/v1/ifpe/cuenta",
        "/api/v1/ifpe/movimientos",
        "/api/v1/sofom/posicion",
        "/api/v1/spei/transferencias",
        "/api/v1/spei/recibidos",
    ] {
        let (s, cuerpo) = api.llamar_crudo("GET", ruta, Some(&tok_ana), None).await;
        assert_eq!(s, StatusCode::OK, "{ruta}");
        let texto = String::from_utf8(cuerpo).unwrap().to_lowercase();
        for palabra in prohibidas {
            assert!(!texto.contains(palabra), "{ruta} contiene «{palabra}»: {texto}");
        }
    }
    let (_, cuenta) = api.get("/api/v1/ifpe/cuenta", &tok_ana).await;
    assert_eq!(cuenta["estado"], "ACTIVA", "{cuenta}");
    // Lo retenido se ve como saldo bloqueado (sigue respaldado al 100%).
    assert_eq!(
        (cuenta["saldo_disponible"].as_str(), cuenta["saldo_bloqueado"].as_str()),
        (Some("10000.00000000"), Some("436000.00000000")),
        "{cuenta}"
    );
    let (_, tablero) = api.get("/api/v1/tablero", &tok_ana).await;
    assert_eq!(tablero["boveda"]["estado"], "ACTIVA", "{tablero}");
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 19: el desembolso y el cobro de un crédito se evalúan; el
/// acreditado congelado no recibe crédito nuevo pero sí puede pagar.
#[sqlx::test(migrations = "../migrations")]
async fn credito_y_cobro_evaluados(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let c1 = usuario_oficial(&pool, "Uno").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let taller = cliente(&pool, &cfg, "Taller").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "50000").await;
    invertir(&pool, &cfg, &beto, "50000", 182).await.unwrap();

    let c = credito::originar(&pool, &cfg.pld, &solicitud_credito(taller.cliente.id, "5000")).await.unwrap();
    let pago = |principal: &str| credito::PagoCredito {
        credito_id: c.credito.id,
        principal: d(principal),
        interes: d("0"),
        clave_idempotencia: clave(),
    };
    let cobro = credito::pagar(&pool, &pago("1000")).await.unwrap();
    let evaluaciones: Vec<(String, String, String, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT canal, entidad, sentido, resultado, transaccion_id FROM aml.evaluaciones WHERE cliente_id = $1 \
          ORDER BY creado_en",
    )
    .bind(taller.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        evaluaciones,
        [
            ("DESEMBOLSO_CREDITO".into(), "SOFOM".into(), "SALIDA".into(), "PERMITIDA".into(), Some(c.desembolso.id)),
            ("COBRO_CREDITO".into(), "SOFOM".into(), "ENTRADA".into(), "PERMITIDA".into(), Some(cobro.cobro.id)),
        ]
    );

    congelar_manual(&pool, &c1, taller.cliente.id).await;
    let e = credito::originar(&pool, &cfg.pld, &solicitud_credito(taller.cliente.id, "5000")).await.unwrap_err();
    assert_eq!(e.codigo(), "OPERACION_NO_DISPONIBLE");
    assert_eq!(contar(&pool, "SELECT count(*) FROM sofom.creditos").await, 1);
    assert_eq!(congelamiento_activo(&pool, taller.cliente.id).await.unwrap().2, 1);
    // La cobranza sigue.
    let segundo = credito::pagar(&pool, &pago("1000")).await.unwrap();
    assert_eq!(segundo.credito.saldo_insoluto, d("3000"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// §6.1: LPB, reglas versionadas, fallos de evaluación y monitoreo por lotes
// -----------------------------------------------------------------------------

/// §6.1 prueba 20: la Lista de Personas Bloqueadas congela de inmediato,
/// aun en modo de observación (obligación legal), con reporte urgente.
#[sqlx::test(migrations = "../migrations")]
async fn lista_bloqueadas_congela_urgente(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let (_, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, c2) = token_oficial(&api, &pool, "Dos").await;
    let v = aprobar_propuesta(&api, &c1, &c2, json!({ "tipo": "PARAMETROS", "cambios": { "modo_observacion": true } }))
        .await;
    assert_eq!(v["parametros"]["modo_observacion"], true);

    // El espejo KYC de 0006 exige el expediente RECHAZADO para un cliente SUSPENDIDO.
    como_dueno(
        &pool,
        &format!(
            "UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'Lista de Personas Bloqueadas' \
              WHERE cliente_id = '{id}'; \
             UPDATE kyc.clientes SET en_lista_bloqueadas = TRUE, estatus_kyc = 'SUSPENDIDO' WHERE id = '{id}'",
            id = ana.cliente.id
        ),
    )
    .await
    .unwrap();
    let (regla, origen, tipo, urgente, a_tiempo, nivel): (String, String, String, bool, bool, String) = sqlx::query_as(
        "SELECT regla_codigo, origen, tipo_reporte, urgente, fecha_limite_reporte <= now() + interval '24 hours', \
                nivel_riesgo::text \
           FROM aml.alertas WHERE cliente_id = $1",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (regla.as_str(), origen.as_str(), tipo.as_str(), urgente, a_tiempo, nivel.as_str()),
        ("R09_LISTA_BLOQUEADAS", "LISTA_BLOQUEADAS", "INUSUAL", true, true, "CRITICO")
    );
    let (_, origen, _) = congelamiento_activo(&pool, ana.cliente.id).await.expect("congelada");
    assert_eq!(origen, "LISTA_BLOQUEADAS");
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 21: las reglas y los parámetros cambian solo con propuesta y
/// aprobación de otro oficial; las versiones son inmutables y cada evaluación
/// cita la versión que aplicó. El modo de observación alerta sin congelar.
#[sqlx::test(migrations = "../migrations")]
async fn reglas_y_parametros_cuatro_ojos(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let (_, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, c2) = token_oficial(&api, &pool, "Dos").await;
    let justificacion = "Calibración trimestral aprobada por el comité de comunicación y control.";

    // C1 propone R05 MEDIO con frecuencia mínima 4; él mismo no puede aprobar.
    let (s, v) = api
        .post(
            "/api/v1/aml/propuestas",
            &c1,
            json!({ "tipo": "REGLA", "regla_codigo": "R05_ESTRUCTURACION", "nivel_cliente": "MEDIO",
                    "cambios": { "frecuencia_min": 4 }, "justificacion": justificacion, "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let decision = format!("/api/v1/aml/propuestas/{}/decision", v["propuesta"]["id"].as_str().unwrap());
    let (s, v) = api
        .post(
            &decision,
            &c1,
            json!({ "aprobar": true, "comentario": "Aprobada por mí.", "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_CUATRO_OJOS"));
    let (s, v) = api
        .post(
            &decision,
            &c2,
            json!({ "aprobar": true, "comentario": "Revisada la calibración.", "clave_idempotencia": clave_api() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["regla"]["version"].as_i64(), v["regla"]["frecuencia_min"].as_i64()), (Some(2), Some(4)), "{v}");
    let versiones: Vec<(i32, bool, bool, Uuid)> = sqlx::query_as(
        "SELECT version, activa, desactivada_en IS NOT NULL, id FROM aml.reglas_riesgo \
          WHERE codigo = 'R05_ESTRUCTURACION' AND nivel_cliente = 'MEDIO' ORDER BY version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(versiones.iter().map(|v| (v.0, v.1, v.2)).collect::<Vec<_>>(), [(1, false, true), (2, true, false)]);
    let v2 = versiones[1].3;

    // Tres depósitos ya no son estructuración; el cuarto sí, con la versión 2.
    let eva = cliente(&pool, &cfg, "Eva").await;
    for _ in 0..3 {
        depositar(&pool, &cfg, eva.cuenta_ifpe.id, "145000").await;
    }
    assert!(!alertas_de(&pool, eva.cliente.id).await.iter().any(|a| a.0 == "R05_ESTRUCTURACION"));
    depositar(&pool, &cfg, eva.cuenta_ifpe.id, "145000").await;
    assert!(alertas_de(&pool, eva.cliente.id).await.iter().any(|a| a.0 == "R05_ESTRUCTURACION"));
    let aplicadas: Vec<Uuid> = sqlx::query_scalar(
        "SELECT reglas_aplicadas FROM aml.evaluaciones WHERE cliente_id = $1 ORDER BY creado_en DESC LIMIT 1",
    )
    .bind(eva.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(aplicadas.contains(&v2), "{aplicadas:?}");
    assert!(congelamiento_activo(&pool, eva.cliente.id).await.is_some());

    // Modo de observación: el mismo ataque alerta CRITICO pero no congela ni retiene.
    aprobar_propuesta(&api, &c1, &c2, json!({ "tipo": "PARAMETROS", "cambios": { "modo_observacion": true } })).await;
    let fer = cliente(&pool, &cfg, "Fer").await;
    for _ in 0..4 {
        depositar(&pool, &cfg, fer.cuenta_ifpe.id, "145000").await;
    }
    let r05 = alertas_de(&pool, fer.cliente.id).await.into_iter().find(|a| a.0 == "R05_ESTRUCTURACION").expect("R05");
    assert_eq!((r05.1.as_str(), r05.3.as_str()), ("CRITICO", "REGISTRADA"));
    assert!(congelamiento_activo(&pool, fer.cliente.id).await.is_none());
    assert_eq!(saldo_ifpe(&pool, fer.cuenta_ifpe.id).await, (d("580000"), d("0")));

    // Las versiones no se editan; claves desconocidas y reglas de sistema no se proponen.
    let r =
        como_dueno(&pool, "UPDATE aml.reglas_riesgo SET umbral_monto = 1 WHERE codigo = 'R02_FRACCIONAMIENTO'").await;
    assert!(matches!(&r, Err(e) if e.contains("SC_AML_REGLA_INMUTABLE")), "{r:?}");
    for (regla, nivel, cambios) in [
        ("R05_ESTRUCTURACION", "MEDIO", json!({ "no_existe": 1 })),
        ("R09_LISTA_BLOQUEADAS", "TODOS", json!({ "permite_congelar": false })),
    ] {
        let (s, v) = api
            .post(
                "/api/v1/aml/propuestas",
                &c1,
                json!({ "tipo": "REGLA", "regla_codigo": regla, "nivel_cliente": nivel, "cambios": cambios,
                        "justificacion": justificacion, "clave_idempotencia": clave_api() }),
            )
            .await;
        assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_CAMBIO_INVALIDO"), "{regla}: {v}");
    }

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 22: si la calificación falla, la entrada se abona (o se
/// retiene completa si el cliente está congelado) y queda marcada para
/// reevaluar; la salida se rechaza. Un renglón del ledger escrito sin
/// evaluación queda marcado DIFERIDA. El ciclo por lotes reevalúa todo.
#[sqlx::test(migrations = "../migrations")]
async fn fallo_de_evaluacion_y_cobertura_diferida(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let c1 = usuario_oficial(&pool, "Uno").await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let carla = cliente(&pool, &cfg, "Carla").await;
    let dani = cliente(&pool, &cfg, "Dani").await;
    let eva = cliente(&pool, &cfg, "Eva").await;
    congelar_manual(&pool, &c1, beto.cliente.id).await;
    depositar(&pool, &cfg, carla.cuenta_ifpe.id, "5000").await;
    depositar(&pool, &cfg, dani.cuenta_ifpe.id, "1000").await;

    // Se rompe la fuente de velocidad del motor.
    let funcion = "aml.movimientos_cliente(uuid, timestamptz, timestamptz, uuid)";
    como_dueno(&pool, &format!("ALTER FUNCTION {funcion} RENAME TO movimientos_cliente_roto")).await.unwrap();
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "2000").await;
    let e = retirar(&pool, &cfg, carla.cliente.id, "1000", &externa(6), None, &clave()).await;
    como_dueno(&pool, "ALTER FUNCTION aml.movimientos_cliente_roto(uuid, timestamptz, timestamptz, uuid) RENAME TO movimientos_cliente")
        .await
        .unwrap();

    // Entrada: abonada y marcada para reevaluar (fail-open), con aviso ALTA.
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("1000"), d("0")));
    let ultima = evaluaciones_de(&pool, ana.cliente.id).await.pop().unwrap();
    assert_eq!((ultima.1.as_str(), ultima.2.as_str()), ("PENDIENTE_REEVALUACION", "EN_LINEA"));
    assert!(eventos_bitacora(&pool, ana.cliente.id).await.iter().any(|e| e == "EVALUACION_FALLIDA"));
    let prioridad: String = sqlx::query_scalar(
        "SELECT prioridad FROM aml.notificaciones WHERE cliente_id = $1 AND tipo = 'EVALUACION_FALLIDA'",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(prioridad, "ALTA");
    // Cliente congelado: se retiene completa.
    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await, (d("0"), d("2000")));
    assert_eq!(retenido(&pool, beto.cuenta_ifpe.id).await, d("2000"));
    assert_eq!(evaluaciones_de(&pool, beto.cliente.id).await.pop().unwrap().1, "RETENIDA");
    // Salida: falla cerrada, sin renglones.
    assert!(e.is_err(), "{e:?}");
    let retiros: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'RETIRO' AND cuenta_origen_id = $1")
            .bind(carla.cuenta_ifpe.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retiros, 0);
    assert_eq!(saldo_ifpe(&pool, carla.cuenta_ifpe.id).await, (d("5000"), d("0")));

    // Transferencia escrita por el dueño sin evaluación en línea: la cobertura
    // la marca DIFERIDA (una por perspectiva) y avisa; nunca falla.
    let mut tx = pool.begin().await.unwrap();
    for (cuenta, delta) in [(dani.cuenta_ifpe.id, d("-300")), (eva.cuenta_ifpe.id, d("300"))] {
        sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $2 WHERE id = $1")
            .bind(cuenta)
            .bind(delta)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia) \
         VALUES ('TRANSFERENCIA', $1, $2, 300, $3)",
    )
    .bind(dani.cuenta_ifpe.id)
    .bind(eva.cuenta_ifpe.id)
    .bind(format!("cli:{}", Uuid::new_v4()))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let marcas: Vec<String> =
        sqlx::query_scalar("SELECT canal FROM aml.evaluaciones WHERE origen = 'DIFERIDA' ORDER BY canal")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(marcas, ["TRANSFERENCIA_ENVIADA", "TRANSFERENCIA_RECIBIDA"]);
    assert!(contar(&pool, "SELECT count(*) FROM aml.notificaciones WHERE tipo = 'COBERTURA'").await >= 1);

    // El ciclo reevalúa las tres marcas (la entrada fallida y las dos diferidas).
    let c = ciclo_aml(&pool, &cfg).await;
    assert_eq!((c.reevaluadas, c.errores), (3, 0), "{c:?}");
    let reevaluadas: i64 = contar(
        &pool,
        "SELECT count(*) FROM aml.evaluaciones e JOIN aml.evaluaciones o ON o.id = e.evaluacion_origen_id \
          WHERE e.origen = 'REEVALUACION' AND e.resultado = 'REEVALUADA' AND o.resultado = 'PENDIENTE_REEVALUACION'",
    )
    .await;
    assert_eq!(reevaluadas, 3);
    // Una segunda pasada no vuelve a reevaluar; todo aviso quedó entregado.
    let c2 = ciclo_aml(&pool, &cfg).await;
    assert_eq!((c2.reevaluadas, c2.errores), (0, 0), "{c2:?}");
    assert_eq!(contar(&pool, "SELECT count(*) FROM aml.notificaciones WHERE entregada_en IS NULL").await, 0);

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// §6.1 prueba 23.
#[sqlx::test(migrations = "../migrations")]
async fn monitoreo_lote_redes_plazos_y_notificaciones(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    for _ in 0..8 {
        depositar(&pool, &cfg, ana.cuenta_ifpe.id, "40000").await;
    }
    let mut red = Vec::new();
    for nombre in ["Beto", "Carla", "Dani"] {
        let c = cliente(&pool, &cfg, nombre).await;
        depositar(&pool, &cfg, c.cuenta_ifpe.id, "100000").await;
        red.push(c.cliente.id);
    }
    // En línea solo se ve el fraccionamiento de 24 h.
    let codigos: Vec<String> = alertas_de(&pool, ana.cliente.id).await.into_iter().map(|a| a.0).collect();
    assert_eq!(codigos, ["R02_FRACCIONAMIENTO"]);
    for c in &red {
        assert!(alertas_de(&pool, *c).await.is_empty());
    }

    let c = ciclo_aml(&pool, &cfg).await;
    assert!(c.turno && c.errores == 0 && c.ocupados == 0, "{c:?}");
    assert_eq!(c.clientes, 4, "{c:?}");
    assert_eq!((c.alertas_lote, c.alertas_redes), (1, 3), "{c:?}");
    let (puntaje, origen, nivel): (String, String, String) = sqlx::query_as(
        "SELECT puntaje::text, origen, nivel_riesgo::text FROM aml.alertas WHERE cliente_id = $1 AND regla_codigo LIKE 'R20%'",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((puntaje.as_str(), origen.as_str(), nivel.as_str()), ("85.00", "LOTE", "CRITICO"));
    let (_, origen, _) = congelamiento_activo(&pool, ana.cliente.id).await.expect("congelamiento por lotes");
    assert_eq!(origen, "LOTE");
    assert_eq!(estado_cuenta(&pool, ana.cuenta_ifpe.id).await, "CONGELADO_PREVENTIVO");
    // Sin retención: los abonos ya son pasado.
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await, (d("320000"), d("0")));
    for c in &red {
        let alertas = alertas_de(&pool, *c).await;
        assert_eq!(alertas.len(), 1, "{alertas:?}");
        assert!(alertas[0].0.starts_with("R26"), "{alertas:?}");
        assert!(congelamiento_activo(&pool, *c).await.is_none());
    }
    // Marcas de agua en las cabezas del ledger; todo aviso entregado.
    let (marca_ifpe, marca_sofom, cabeza_ifpe, cabeza_sofom, ultimo): (i64, i64, i64, i64, Option<Value>) =
        sqlx::query_as(
            "SELECT m.ultima_secuencia_ifpe, m.ultima_secuencia_sofom, \
                (SELECT ultima_secuencia FROM ifpe.ledger_cabeza), (SELECT ultima_secuencia FROM sofom.ledger_cabeza), \
                m.ultimo_resultado \
           FROM aml.monitoreo_estado m",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((marca_ifpe, marca_sofom), (cabeza_ifpe, cabeza_sofom));
    assert_eq!((c.hasta_ifpe, c.hasta_sofom), (cabeza_ifpe, cabeza_sofom));
    assert_eq!(ultimo.unwrap()["alertas_lote"], 1);
    let sin_entregar: i64 = sqlx::query_scalar("SELECT count(*) FROM aml.notificaciones WHERE entregada_en IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sin_entregar, 0);
    assert!(c.notificaciones > 0);

    // Segundo ciclo: nada nuevo (deduplicación por cliente, regla y semana).
    let c2 = ciclo_aml(&pool, &cfg).await;
    assert_eq!((c2.alertas_lote + c2.alertas_redes, c2.errores, c2.clientes), (0, 0, 0), "{c2:?}");

    // Plazos: cada aviso una sola vez.
    let n1: i32 =
        sqlx::query_scalar("SELECT aml.revisar_plazos(now() + interval '73 hours')").fetch_one(&pool).await.unwrap();
    let n2: i32 =
        sqlx::query_scalar("SELECT aml.revisar_plazos(now() + interval '73 hours')").fetch_one(&pool).await.unwrap();
    assert!(n1 >= 2, "{n1}");
    assert_eq!(n2, 0);
    let avisos: Vec<(String, i64)> = sqlx::query_as(
        "SELECT tipo, count(*) FROM aml.notificaciones \
          WHERE tipo IN ('SLA_CONGELAMIENTO', 'ESCALAMIENTO') AND cliente_id = $1 GROUP BY tipo ORDER BY tipo",
    )
    .bind(ana.cliente.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(avisos, [("ESCALAMIENTO".to_string(), 1), ("SLA_CONGELAMIENTO".to_string(), 1)]);
    let entregadas = monitoreo::entregar_notificaciones(&pool, 100).await.unwrap();
    assert_eq!(entregadas as i32, n1);

    // Con el turno tomado por otra instancia, el ciclo no hace nada.
    sqlx::query("UPDATE aml.monitoreo_estado SET en_curso_hasta = now() + interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();
    let alertas_antes: i64 = sqlx::query_scalar("SELECT count(*) FROM aml.alertas").fetch_one(&pool).await.unwrap();
    let c3 = ciclo_aml(&pool, &cfg).await;
    assert!(!c3.turno, "{c3:?}");
    let alertas_despues: i64 = sqlx::query_scalar("SELECT count(*) FROM aml.alertas").fetch_one(&pool).await.unwrap();
    assert_eq!(alertas_antes, alertas_despues);

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// Consola: formas de las lecturas y validación de las solicitudes
// -----------------------------------------------------------------------------

/// Formas de las lecturas de la consola (§4.2), filtros, cursores y
/// validación de parámetros. Los roles se prueban en `segregacion_de_funciones`.
#[sqlx::test(migrations = "../migrations")]
async fn consola_lecturas_y_validacion(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    congelar_por_estructuracion(&pool, &cfg, ana.cuenta_ifpe.id).await;
    let (c1_id, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, aud_email) = auditor(&pool, "Auditora").await;
    let aud = api.token(&aud_email, PASSWORD_INTERNO).await;
    let r05 = alerta_de(&pool, ana.cliente.id, "R05_ESTRUCTURACION").await;

    // 1. Resumen.
    let (s, r) = api.get("/api/v1/aml/resumen", &c1).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!((r["pendientes"].as_i64(), r["congelamientos_activos"].as_i64()), (Some(2), Some(1)), "{r}");
    assert_eq!(r["criticas_abiertas"], 1, "{r}");
    assert!(r["servidor_en"].is_string(), "{r}");

    // 2. Bandeja (folio descendente): R05 es la más reciente.
    let (s, b) = api.get("/api/v1/aml/alertas", &c1).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let codigos: Vec<&str> =
        b["alertas"].as_array().unwrap().iter().map(|a| a["regla"]["codigo"].as_str().unwrap()).collect();
    assert_eq!(codigos, ["R05_ESTRUCTURACION", "R02_FRACCIONAMIENTO"]);

    // 3. Expediente: estado real de la cuenta, retención y movimientos del patrón.
    let (s, x) = api.get(&format!("/api/v1/aml/alertas/{r05}"), &c1).await;
    assert_eq!(s, StatusCode::OK, "{x}");
    assert!(x["alerta"]["detalles"].is_object(), "{x}");
    assert!(x["alerta"]["dictamen_propuesto"].is_null());
    assert_eq!(x["alerta"]["puntaje"], "100.00");
    assert!(!x["evaluaciones"].as_array().unwrap().is_empty());
    assert_eq!(x["cuentas"]["ifpe"]["estado"], "CONGELADO_PREVENTIVO");
    assert_eq!(x["cuentas"]["ifpe"]["retenido_aml"], "435000.00000000");
    assert_eq!(x["congelamiento"]["estado"], "ACTIVO");
    assert_eq!(x["congelamiento"]["retenido_vigente"], "435000.00000000");
    assert_eq!(x["congelamiento"]["retenciones"].as_array().unwrap().len(), 1);
    let movimientos = x["movimientos"].as_array().unwrap();
    assert_eq!(movimientos.len(), 3, "{x}");
    assert_eq!(movimientos.iter().filter(|m| m["retenido"] == true).count(), 1, "{x}");
    assert_eq!(x["otras_alertas"].as_array().unwrap().len(), 1);
    assert!(!x["historial"].as_array().unwrap().is_empty());
    assert_eq!(x["cliente"]["id"], json!(ana.cliente.id));

    // 6. Revisión del cliente.
    let (s, c) = api.get(&format!("/api/v1/aml/clientes/{}", ana.cliente.id), &c1).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["agregados"]["h24"]["entradas_n"], 3, "{c}");
    assert_eq!(c["agregados"]["d30"]["entradas_monto"], "435000.00000000", "{c}");
    assert_eq!(c["agregados"]["h24"]["salidas_n"], 0);
    assert_eq!(c["alertas"].as_array().unwrap().len(), 2);
    assert_eq!(c["congelamientos"].as_array().unwrap().len(), 1);
    assert_eq!(c["movimientos"].as_array().unwrap().len(), 3);
    assert_eq!(c["relevantes_n"], 0);
    // Cada consulta de expediente queda en la bitácora con el actor.
    let consultas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM aml.bitacora WHERE evento = 'CONSULTA_EXPEDIENTE' AND actor_id = $1 AND cliente_id = $2",
    )
    .bind(c1_id)
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(consultas, 2);

    // 8. Congelamientos: ACTIVO por omisión, filtros y cursor.
    let (s, f) = api.get("/api/v1/aml/congelamientos", &c1).await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!(f["congelamientos"].as_array().unwrap().len(), 1);
    assert_eq!(f["congelamientos"][0]["origen"], "AUTOMATICO");
    assert!(f["siguiente_antes_de_folio"].is_null());
    let (_, f) = api.get("/api/v1/aml/congelamientos?estado=LEVANTADO", &c1).await;
    assert_eq!(f["congelamientos"].as_array().unwrap().len(), 0);
    let (_, f) =
        api.get(&format!("/api/v1/aml/congelamientos?estado=TODOS&cliente_id={}&limite=1", ana.cliente.id), &c1).await;
    assert_eq!(f["congelamientos"].as_array().unwrap().len(), 1);
    assert_eq!(f["siguiente_antes_de_folio"], f["congelamientos"][0]["folio"], "página llena: {f}");
    let (_, f) = api.get(&format!("/api/v1/aml/congelamientos?cliente_id={}", Uuid::new_v4()), &c1).await;
    assert_eq!(f["congelamientos"].as_array().unwrap().len(), 0);

    // 10. Cola de cuatro ojos vacía.
    let (s, q) = api.get("/api/v1/aml/solicitudes", &c1).await;
    assert_eq!((s, q), (StatusCode::OK, json!({ "solicitudes": [] })));

    // 12. Notificaciones en orden de id, con cursor.
    let (s, n) = api.get("/api/v1/aml/notificaciones", &c1).await;
    assert_eq!(s, StatusCode::OK, "{n}");
    let lista = n["notificaciones"].as_array().unwrap();
    let tipos: Vec<&str> = lista.iter().map(|x| x["tipo"].as_str().unwrap()).collect();
    for t in ["ALERTA_NUEVA", "ALERTA_CRITICA", "CONGELAMIENTO"] {
        assert!(tipos.contains(&t), "{tipos:?}");
    }
    let ids: Vec<i64> = lista.iter().map(|x| x["id"].as_i64().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    assert_eq!(n["ultimo_id"].as_i64(), ids.last().copied());
    assert!(lista.iter().all(|x| x.get("monto").is_none() && x["entregada_en"].is_null()));
    let (_, n) = api.get(&format!("/api/v1/aml/notificaciones?despues_de={}", ids.last().unwrap()), &c1).await;
    assert_eq!(n, json!({ "notificaciones": [], "ultimo_id": null }));

    // 13, 14, 17, 18.
    let (s, g) = api.get("/api/v1/aml/reglas", &c1).await;
    assert_eq!(s, StatusCode::OK, "{g}");
    assert_eq!(g["parametros"]["modo_observacion"], false);
    let reglas = g["reglas"].as_array().unwrap();
    assert!(reglas.iter().all(|r| r["activa"] == true) && !reglas.is_empty());
    assert_eq!(g["propuestas_pendientes"], json!([]));
    let (_, todas) = api.get("/api/v1/aml/reglas?incluir_inactivas=true", &c1).await;
    let todas = todas["reglas"].as_array().unwrap();
    assert!(todas.len() >= reglas.len());
    assert_eq!(todas.iter().filter(|r| r["activa"] == true).count(), reglas.len());
    let (s, p) = api.get("/api/v1/aml/propuestas?estado=TODOS", &c1).await;
    assert_eq!((s, p), (StatusCode::OK, json!({ "propuestas": [] })));
    let (s, rel) = api.get("/api/v1/aml/relevantes?trimestre=2026-T4", &c1).await;
    assert_eq!((s, rel), (StatusCode::OK, json!({ "relevantes": [], "siguiente_antes_de": null })));
    let (s, v) = api.get("/api/v1/aml/bitacora/verificacion", &c1).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["valido"], true, "{v}");
    assert!(v["filas"].as_i64().unwrap() > 0);
    assert!(v["primer_error"].is_null());

    // Validación: 400 antes de la base de datos; 404 para ids inexistentes.
    for ruta in [
        "/api/v1/aml/alertas/no-es-uuid",
        "/api/v1/aml/alertas?limite=0",
        "/api/v1/aml/alertas?estado=ABIERTA",
        "/api/v1/aml/congelamientos?estado=VIGENTE",
        "/api/v1/aml/congelamientos?limite=201",
        "/api/v1/aml/solicitudes?desconocido=1",
        "/api/v1/aml/notificaciones?despues_de=-1",
        "/api/v1/aml/relevantes?trimestre=2026-T9",
        "/api/v1/aml/relevantes?limite=501",
        "/api/v1/aml/reglas?incluir_inactivas=quiza",
    ] {
        let (s, v) = api.get(ruta, &c1).await;
        assert_eq!((s, error(&v)), (StatusCode::BAD_REQUEST, "VALIDACION"), "{ruta}: {v}");
    }
    for ruta in [format!("/api/v1/aml/alertas/{}", Uuid::new_v4()), format!("/api/v1/aml/clientes/{}", Uuid::new_v4())]
    {
        let (s, v) = api.get(&ruta, &aud).await;
        assert_eq!((s, error(&v)), (StatusCode::NOT_FOUND, "NO_ENCONTRADO"), "{ruta}: {v}");
    }

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

/// Folio UIF (endpoint 5) y propuestas de parámetros (endpoints 13–16): estados,
/// formatos (400 en Rust) y validaciones de la base de datos (422).
#[sqlx::test(migrations = "../migrations")]
async fn reporte_uif_y_propuestas_por_http(pool: PgPool) {
    let (api, cfg) = api(&pool).await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "145000").await;
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "145000").await;
    let (_, c1) = token_oficial(&api, &pool, "Uno").await;
    let (_, c2) = token_oficial(&api, &pool, "Dos").await;
    let r02 = alerta_de(&pool, beto.cliente.id, "R02_FRACCIONAMIENTO").await;
    let resolver = format!("/api/v1/aml/alertas/{r02}/resolver");
    let reporte = format!("/api/v1/aml/alertas/{r02}/reporte-uif");
    let hoy: String =
        sqlx::query_scalar("SELECT to_char((now() AT TIME ZONE 'America/Mexico_City')::date, 'YYYY-MM-DD')")
            .fetch_one(&pool)
            .await
            .unwrap();
    let pasado: String =
        sqlx::query_scalar("SELECT to_char((now() AT TIME ZONE 'America/Mexico_City')::date + 2, 'YYYY-MM-DD')")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Antes de confirmar no hay reporte.
    let (s, v) = api
        .post(
            &reporte,
            &c1,
            json!({ "folio_uif": "SITI-2026-0001", "reportada_en": hoy, "clave_idempotencia": "uif-0000" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_TRANSICION_INVALIDA"));

    let (s, _) = api.post(&resolver, &c1, json!({ "accion": "TOMAR", "clave_idempotencia": "toma-0001" })).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = api
        .post(&resolver, &c1, json!({ "accion": "CONFIRMAR", "dictamen": DICTAMEN, "tipo_reporte": "INUSUAL", "urgente": true, "clave_idempotencia": "confirma-01" }))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["alerta"]["estado"].as_str(), v["alerta"]["urgente"].as_bool()), (Some("CONFIRMADO"), Some(true)));
    assert!(v["congelamiento"].is_null(), "{v}");

    // Folio con formato inválido: 400. Fecha futura: 422.
    let (s, v) = api
        .post(&reporte, &c1, json!({ "folio_uif": "siti-1", "reportada_en": hoy, "clave_idempotencia": "uif-0001" }))
        .await;
    assert_eq!((s, error(&v)), (StatusCode::BAD_REQUEST, "VALIDACION"));
    let (s, v) = api
        .post(
            &reporte,
            &c1,
            json!({ "folio_uif": "SITI-2026-0001", "reportada_en": pasado, "clave_idempotencia": "uif-0002" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_DICTAMEN_INVALIDO"));
    let cuerpo = json!({ "folio_uif": "SITI-2026-0001", "reportada_en": hoy, "clave_idempotencia": "uif-0003" });
    let (s, v) = api.post(&reporte, &c1, cuerpo.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(
        (v["alerta"]["folio_uif"].as_str(), v["alerta"]["reportada_uif_en"].as_str()),
        (Some("SITI-2026-0001"), Some(hoy.as_str()))
    );
    let (s, v) = api.post(&reporte, &c1, cuerpo).await;
    assert_eq!((s, v["idempotente"].as_bool()), (StatusCode::OK, Some(true)));
    let (s, v) = api
        .post(
            &reporte,
            &c2,
            json!({ "folio_uif": "SITI-2026-0002", "reportada_en": hoy, "clave_idempotencia": "uif-0004" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_FOLIO_REGISTRADO"));

    // Propuestas de cambio con cuatro ojos.
    let justificacion = "Calibración trimestral: ampliar el aviso de plazos de reporte a la UIF.";
    let (s, v) = api
        .post("/api/v1/aml/propuestas", &c1, json!({ "tipo": "PARAMETROS", "cambios": { "dias_aviso_plazo": 5 }, "justificacion": justificacion, "clave_idempotencia": "prop-0001" }))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["propuesta"]["estado"], "PENDIENTE");
    let propuesta = v["propuesta"]["id"].as_str().unwrap().to_string();
    let (_, g) = api.get("/api/v1/aml/reglas", &c2).await;
    assert_eq!(g["propuestas_pendientes"].as_array().unwrap().len(), 1, "{g}");
    let decision = format!("/api/v1/aml/propuestas/{propuesta}/decision");
    let (s, v) = api
        .post(
            &decision,
            &c1,
            json!({ "aprobar": true, "comentario": "Aprobada por mí.", "clave_idempotencia": "dec-0001" }),
        )
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_CUATRO_OJOS"));
    let (s, v) = api
        .post(
            &decision,
            &c2,
            json!({ "aprobar": true, "comentario": "Revisada la calibración.", "clave_idempotencia": "dec-0002" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(
        (v["propuesta"]["estado"].as_str(), v["parametros"]["dias_aviso_plazo"].as_i64()),
        (Some("APROBADA"), Some(5))
    );
    assert!(v["regla"].is_null());
    let (_, p) = api.get("/api/v1/aml/propuestas?estado=APROBADA", &c1).await;
    assert_eq!(p["propuestas"].as_array().unwrap().len(), 1);
    // Validación de la forma (400) y de las claves (422, en la base de datos).
    for cuerpo in [
        json!({ "tipo": "REGLA", "cambios": { "puntos": "50" }, "justificacion": justificacion, "clave_idempotencia": "prop-0002" }),
        json!({ "tipo": "PARAMETROS", "cambios": {}, "justificacion": justificacion, "clave_idempotencia": "prop-0003" }),
        json!({ "tipo": "PARAMETROS", "regla_codigo": "R05_ESTRUCTURACION", "cambios": { "dias_aviso_plazo": 4 }, "justificacion": justificacion, "clave_idempotencia": "prop-0004" }),
    ] {
        let (s, v) = api.post("/api/v1/aml/propuestas", &c1, cuerpo.clone()).await;
        assert_eq!((s, error(&v)), (StatusCode::BAD_REQUEST, "VALIDACION"), "{cuerpo}: {v}");
    }
    let (s, v) = api
        .post("/api/v1/aml/propuestas", &c1, json!({ "tipo": "PARAMETROS", "cambios": { "no_existe": 1 }, "justificacion": justificacion, "clave_idempotencia": "prop-0005" }))
        .await;
    assert_eq!((s, error(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "AML_CAMBIO_INVALIDO"));

    verificar_invariantes(&pool).await;
    verificar_invariantes_aml(&pool).await;
}

// -----------------------------------------------------------------------------
// §3.3: escaneo del código fuente
// -----------------------------------------------------------------------------

/// §3.3: todo camino de salida pasa por el interceptor PLD y solo
/// `backend/src/aml/` llama a las envolturas de la base de datos.
#[test]
fn fuente_intercepta_toda_salida() {
    use std::{fs, path::Path};
    fn recorrer(dir: &Path, archivos: &mut Vec<(String, String)>) {
        for entrada in fs::read_dir(dir).unwrap() {
            let ruta = entrada.unwrap().path();
            if ruta.is_dir() {
                recorrer(&ruta, archivos);
            } else if ruta.extension().is_some_and(|e| e == "rs") {
                archivos.push((ruta.display().to_string(), fs::read_to_string(&ruta).unwrap()));
            }
        }
    }
    let raiz = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut archivos = Vec::new();
    recorrer(&raiz, &mut archivos);
    let relativa = |ruta: &str| ruta.strip_prefix(&format!("{}/", raiz.display())).unwrap_or(ruta).to_string();

    let legado: Vec<String> =
        archivos.iter().filter(|(_, c)| c.contains("pld::evaluar")).map(|(r, _)| relativa(r)).collect();
    assert!(legado.is_empty(), "pld::evaluar sigue en {legado:?}");

    let mut llamadas: Vec<(String, usize)> = archivos
        .iter()
        .map(|(r, c)| (relativa(r), c.matches("aml::evaluar(").count()))
        .filter(|(_, n)| *n > 0)
        .collect();
    llamadas.sort();
    let esperado: Vec<(String, usize)> = [
        ("ledger/mod.rs", 2),
        ("sofom/credito.rs", 2),
        ("sofom/puente.rs", 1),
        ("spei/inbound.rs", 1),
        ("spei/outbound.rs", 1),
    ]
    .into_iter()
    .map(|(r, n)| (r.to_string(), n))
    .collect();
    assert_eq!(llamadas, esperado);

    let fuera: Vec<String> = archivos
        .iter()
        .filter(|(r, c)| !relativa(r).starts_with("aml/") && (c.contains("ifpe.aml_") || c.contains("sofom.aml_")))
        .map(|(r, _)| relativa(r))
        .collect();
    assert!(fuera.is_empty(), "envolturas PLD llamadas fuera de backend/src/aml: {fuera:?}");
}
