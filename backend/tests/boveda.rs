//! Bóveda Soberana (migración 0012, `boveda`, `routes/boveda.rs`): la versión
//! de la cuenta y sus avisos, la foto por REST y el WebSocket contra un
//! servidor real: subprotocolo con el JWT, foto inicial, cambios en orden de
//! versión, resincronización (pedida, o al volver a escuchar), cortes de
//! reserva, sesión revocada, latido, topes y apagado.

mod comun;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::http::StatusCode;
use comun::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::{postgres::PgListener, PgPool};
use synthetic_capital::{
    auth,
    boveda::{
        central::{self, MAX_SOCKETS_POR_CLIENTE},
        consulta::RAIZ_SIN_CORTE,
        socket::{Tiempos, CIERRE_SESION},
        CentralBoveda,
    },
    config::{Config, Cuota, LimitesPorIp},
    ledger,
    models::kyc::RolUsuario,
    reserva, routes, servidor,
    sofom::puente,
    spei::{
        clabe, outbound,
        pasarela::{Comportamiento, PasarelaSimulada},
    },
    AppState,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    task::JoinHandle,
    time::{timeout, timeout_at, Instant},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Error as ErrorWs, Message},
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// `Config::para_pruebas().frontend_origin`.
const ORIGEN: &str = "http://localhost:3000";
const ESPERA: Duration = Duration::from_secs(10);

// -----------------------------------------------------------------------------
// Servidor y cliente
// -----------------------------------------------------------------------------

struct Servidor {
    dir: SocketAddr,
    estado: AppState,
    apagar: oneshot::Sender<()>,
    tarea: JoinHandle<()>,
}

/// El servidor HTTP real (con `with_upgrades`), apagado como en `main.rs`:
/// primero se avisa a los sockets de la bóveda.
async fn arrancar(estado: AppState) -> Servidor {
    let limites = estado.config.http.clone();
    let app = routes::router(estado.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = listener.local_addr().unwrap();
    let (apagar, senal) = oneshot::channel::<()>();
    let boveda = estado.boveda.clone();
    let tarea = tokio::spawn(async move {
        servidor::servir(listener, app, &limites, async move {
            let _ = senal.await;
            boveda.cerrar();
        })
        .await;
    });
    Servidor { dir, estado, apagar, tarea }
}

/// El mismo estado con otros plazos de socket.
fn con_tiempos(mut estado: AppState, tiempos: Tiempos) -> AppState {
    estado.boveda = CentralBoveda::con_tiempos(estado.pool.clone(), estado.config.clone(), tiempos);
    estado
}

fn token_cliente(estado: &AppState, cliente_id: Uuid) -> String {
    estado.jwt.emitir(cliente_id, RolUsuario::Cliente, false).unwrap().0
}

/// Como un navegador: el JWT viaja como entrada del subprotocolo.
async fn abrir(dir: SocketAddr, token: &str) -> Result<Ws, ErrorWs> {
    abrir_con(dir, &format!("sc.vault.v1, bearer.{token}"), Some(ORIGEN), None).await
}

async fn abrir_con(
    dir: SocketAddr,
    protocolos: &str,
    origen: Option<&str>,
    autorizacion: Option<&str>,
) -> Result<Ws, ErrorWs> {
    let mut req = format!("ws://{dir}/api/v1/vault/ws").into_client_request().unwrap();
    let cabeceras = req.headers_mut();
    if !protocolos.is_empty() {
        cabeceras.insert("sec-websocket-protocol", HeaderValue::from_str(protocolos).unwrap());
    }
    if let Some(o) = origen {
        cabeceras.insert("origin", HeaderValue::from_str(o).unwrap());
    }
    if let Some(t) = autorizacion {
        cabeceras.insert("authorization", HeaderValue::from_str(&format!("Bearer {t}")).unwrap());
    }
    let (ws, respuesta) = connect_async(req).await?;
    // El servidor confirma solo el subprotocolo: el token nunca vuelve.
    assert_eq!(respuesta.headers()["sec-websocket-protocol"], "sc.vault.v1");
    Ok(ws)
}

/// Estado y código de error (`error.codigo`) de un handshake rechazado.
fn rechazo(r: Result<Ws, ErrorWs>) -> (u16, String) {
    match r {
        Err(ErrorWs::Http(respuesta)) => {
            let cuerpo: Value = serde_json::from_slice(respuesta.body().as_deref().unwrap_or_default()).unwrap();
            (respuesta.status().as_u16(), cuerpo["error"]["codigo"].as_str().unwrap_or_default().to_owned())
        }
        Err(e) => panic!("error inesperado en el handshake: {e}"),
        Ok(_) => panic!("el handshake debía rechazarse"),
    }
}

/// El siguiente mensaje JSON del servidor; los pings no cuentan.
async fn recibir(ws: &mut Ws) -> Value {
    loop {
        let m = timeout(ESPERA, ws.next()).await.expect("sin mensaje del servidor").expect("socket cerrado").unwrap();
        match m {
            Message::Text(texto) => return serde_json::from_str(texto.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            otro => panic!("mensaje inesperado: {otro:?}"),
        }
    }
}

/// El cierre que envía el servidor: código y razón.
async fn cierre(ws: &mut Ws) -> (u16, String) {
    loop {
        match timeout(ESPERA, ws.next()).await.expect("el servidor no cerró") {
            Some(Ok(Message::Close(Some(marco)))) => return (marco.code.into(), marco.reason.as_str().to_owned()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            otro => panic!("se esperaba el cierre: {otro:?}"),
        }
    }
}

/// Nada (salvo pings) llega durante `plazo`.
async fn nada_mas(ws: &mut Ws, plazo: Duration) {
    let fin = Instant::now() + plazo;
    loop {
        match timeout_at(fin, ws.next()).await {
            Err(_) => return,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Ok(otro) => panic!("no se esperaba nada: {otro:?}"),
        }
    }
}

const OPCODE_CIERRE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;

/// Las tramas que el servidor envió hasta cerrar la conexión, leídas del TCP
/// sin tungstenite (que contestaría los pings): `(opcode, carga)`. Las del
/// servidor no llevan máscara y estas son cortas (< 126 bytes).
async fn tramas_crudas(ws: &mut Ws) -> Vec<(u8, Vec<u8>)> {
    let mut bytes = Vec::new();
    timeout(ESPERA, ws.get_mut().read_to_end(&mut bytes)).await.expect("el servidor no cerró la conexión").unwrap();
    let mut tramas = Vec::new();
    let mut resto = bytes.as_slice();
    while let [cabeza, largo, cola @ ..] = resto {
        let largo = usize::from(*largo);
        assert!(largo < 126 && largo <= cola.len(), "trama inesperada: {bytes:?}");
        tramas.push((cabeza & 0x0F, cola[..largo].to_vec()));
        resto = &cola[largo..];
    }
    assert!(resto.is_empty(), "trama incompleta: {bytes:?}");
    tramas
}

async fn pedir_resync(ws: &mut Ws) {
    ws.send(Message::text(r#"{"type":"resync"}"#)).await.unwrap();
}

// -----------------------------------------------------------------------------
// Datos
// -----------------------------------------------------------------------------

async fn seq(pool: &PgPool, cuenta_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT seq FROM ifpe.cuentas WHERE id = $1").bind(cuenta_id).fetch_one(pool).await.unwrap()
}

/// La versión subió de `antes` a exactamente `antes + 1`.
async fn subio_una(pool: &PgPool, cuenta_id: Uuid, antes: i64) {
    assert_eq!(seq(pool, cuenta_id).await, antes + 1, "una transacción, una versión");
}

/// Como `tests/spei.rs`: el corte SPEI a 12 horas de ahora, para que el
/// despachador no vea un cambio de día de operación.
fn config_spei(mut cfg: Config) -> Config {
    cfg.spei.hora_corte = (chrono::Utc::now() - chrono::Duration::hours(6)).time() + chrono::Duration::hours(12);
    cfg
}

/// CLABE externa válida (banco 072).
fn externa(n: u64) -> String {
    let primeros = format!("072180{n:011}");
    let dv = clabe::digito_verificador(&primeros).unwrap();
    format!("{primeros}{dv}")
}

async fn transferir(pool: &PgPool, cfg: &Config, origen: Uuid, clabe_destino: &str, monto: &str) {
    ledger::transferir(
        pool,
        &cfg.pld,
        &ledger::Transferencia {
            cliente_origen_id: origen,
            clabe_destino: clabe_destino.into(),
            monto: d(monto),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();
}

async fn ultimo_rastreo_entrante(pool: &PgPool, cuenta_id: Uuid) -> String {
    sqlx::query_scalar(
        "SELECT clave_rastreo::text FROM ifpe.spei_inbound WHERE cuenta_id = $1 ORDER BY recibido_en DESC LIMIT 1",
    )
    .bind(cuenta_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn siguiente_aviso(oyente: &mut PgListener) -> Value {
    let aviso = timeout(ESPERA, oyente.recv()).await.expect("sin aviso de sc_boveda").unwrap();
    serde_json::from_str(aviso.payload()).unwrap()
}

// -----------------------------------------------------------------------------
// La versión de la cuenta y sus avisos (migración 0012)
// -----------------------------------------------------------------------------

/// En `tests/db/0012_boveda_tiempo_real.sql` todo ocurre en una transacción;
/// aquí, cada operación del backend confirma la suya.
#[sqlx::test(migrations = "../migrations")]
async fn la_version_sube_una_vez_por_transaccion_que_mueve_saldo(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let (a, b) = (ana.cuenta_ifpe.id, beto.cuenta_ifpe.id);
    assert_eq!((seq(&pool, a).await, seq(&pool, b).await), (0, 0), "una cuenta nueva empieza en 0");

    depositar(&pool, &cfg, a, "1000").await;
    subio_una(&pool, a, 0).await;
    depositar(&pool, &cfg, a, "500").await;
    subio_una(&pool, a, 1).await;

    // Una transferencia interna mueve las dos cuentas: cada una sube una vez.
    transferir(&pool, &cfg, ana.cliente.id, &beto.cuenta_ifpe.clabe, "250").await;
    subio_una(&pool, a, 2).await;
    subio_una(&pool, b, 0).await;

    // Una inversión saca el principal de la cuenta IFPE.
    puente::invertir(
        &pool,
        &cfg.pld,
        &puente::SolicitudMutuo {
            cliente_id: ana.cliente.id,
            monto: d("100"),
            plazo_dias: 28,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();
    subio_una(&pool, a, 3).await;

    // Un corte de reserva no mueve saldos.
    reserva::generar_corte(&pool).await.unwrap();
    assert_eq!(seq(&pool, a).await, 4);
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn los_avisos_llegan_al_confirmar_y_nunca_de_un_rollback(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let mut oyente = PgListener::connect_with(&pool).await.unwrap();
    oyente.listen(central::CANAL).await.unwrap();

    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let aviso = siguiente_aviso(&mut oyente).await;
    // La transacción del aviso es la que quedó sellada en el SPEI acreditado.
    let xact: String = sqlx::query_scalar("SELECT xact::text FROM ifpe.spei_inbound WHERE cuenta_id = $1")
        .bind(ana.cuenta_ifpe.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(aviso, json!({ "tipo": "saldo", "cliente_id": ana.cliente.id, "seq": 1, "xact": xact }));

    // Un cambio deshecho no avisa: los avisos salen en orden de confirmación,
    // así que el siguiente es el del corte.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 1 WHERE id = $1")
        .bind(ana.cuenta_ifpe.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let corte = reserva::generar_corte(&pool).await.unwrap();
    assert_eq!(siguiente_aviso(&mut oyente).await, json!({ "tipo": "reserva", "corte_id": corte.id }));
    assert_eq!(seq(&pool, ana.cuenta_ifpe.id).await, 1);
}

// -----------------------------------------------------------------------------
// GET /api/v1/vault
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn la_boveda_por_rest(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let estado = AppState::nuevo(pool.clone(), cfg.clone());
    let app = routes::router(estado.clone());
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    let token = token_cliente(&estado, ana.cliente.id);

    let (s, v) = llamar_json(&app, "GET", "/api/v1/vault", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!({
            "user_id": ana.cliente.id,
            "clabe": ana.cuenta_ifpe.clabe,
            "bank_name": "STP",
            "balance": {
                "total_mxn": "1000.00000000",
                "available_cash": "1000.00000000",
                "invested_assets": "0.00000000",
                "unrealized_pnl_percentage": "0.00",
            },
            // Ningún corte publicado todavía.
            "reserve_proof": { "status": "pending", "zk_root_hash": RAIZ_SIN_CORTE },
            "holdings": [],
        })
    );

    // Lo invertido sigue siendo patrimonio del cliente; el corte publicado
    // trae su raíz y cubre los depósitos.
    puente::invertir(
        &pool,
        &cfg.pld,
        &puente::SolicitudMutuo {
            cliente_id: ana.cliente.id,
            monto: d("400"),
            plazo_dias: 28,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap();
    let corte = reserva::generar_corte(&pool).await.unwrap();
    let (s, v) = llamar_json(&app, "GET", "/api/v1/vault", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["balance"],
        json!({
            "total_mxn": "1000.00000000",
            "available_cash": "600.00000000",
            "invested_assets": "400.00000000",
            "unrealized_pnl_percentage": "0.00",
        })
    );
    assert_eq!(v["reserve_proof"], json!({ "status": "verified", "zk_root_hash": corte.merkle_root }));

    // Solo clientes, y con sesión.
    let (s, v) = llamar_json(&app, "GET", "/api/v1/vault", None, None).await;
    assert_eq!((s, v["error"]["codigo"].as_str()), (StatusCode::UNAUTHORIZED, Some("NO_AUTENTICADO")));
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&estado, oficial_id).await;
    let token_oficial = estado.jwt.emitir(oficial_id, RolUsuario::Cumplimiento, true).unwrap().0;
    let (s, _) = llamar_json(&app, "GET", "/api/v1/vault", Some(&token_oficial), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

// -----------------------------------------------------------------------------
// WebSocket
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn websocket_de_principio_a_fin(pool: PgPool) {
    let cfg = config_spei(preparar(&pool).await);
    let pasarela = Arc::new(PasarelaSimulada::new(1));
    let srv = arrancar(AppState::con_pasarela(pool.clone(), cfg.clone(), pasarela.clone())).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let cuenta = ana.cuenta_ifpe.id;
    depositar(&pool, &cfg, cuenta, "1000").await;
    let token = token_cliente(&srv.estado, ana.cliente.id);

    // 1. Foto inicial con la versión de la cuenta.
    let mut ws = abrir(srv.dir, &token).await.unwrap();
    let foto = recibir(&mut ws).await;
    assert_eq!((foto["type"].as_str(), foto["seq"].as_i64()), (Some("vault.snapshot"), Some(1)), "{foto}");
    assert_eq!(foto["vault"]["clabe"], ana.cuenta_ifpe.clabe);
    assert_eq!(foto["vault"]["balance"]["available_cash"], "1000.00000000");
    assert_eq!(foto["vault"]["reserve_proof"]["status"], "pending");

    // 2. Al empezar a escuchar, cada socket recibe una foto: lo que confirmó
    //    antes del LISTEN no se avisará nunca. (Desde aquí, LISTEN está activo.)
    let oyente = tokio::spawn(central::escuchar(srv.estado.boveda.clone()));
    let foto = recibir(&mut ws).await;
    assert_eq!((foto["type"].as_str(), foto["seq"].as_i64()), (Some("vault.snapshot"), Some(1)), "{foto}");

    // 3. SPEI entrante: el abono, con la versión siguiente y el saldo con el que quedó.
    depositar(&pool, &cfg, cuenta, "2500").await;
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("spei.transfer"), Some(2)), "{m}");
    let ocurrio = m["transfer"]["occurred_at"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(ocurrio).is_ok(), "{ocurrio}");
    assert_eq!(
        m["transfer"],
        json!({
            "tracking_key": ultimo_rastreo_entrante(&pool, cuenta).await,
            "direction": "inbound",
            "status": "settled",
            "amount": "2500.00000000",
            "counterparty_name": "ORDENANTE DE PRUEBA",
            "counterparty_bank": "012",
            "occurred_at": ocurrio,
        })
    );
    assert_eq!(m["balance"]["available_cash"], "3500.00000000");
    assert_eq!(m["balance"]["total_mxn"], "3500.00000000");

    // 4. Una transferencia interna no es SPEI: foto completa con la versión siguiente.
    transferir(&pool, &cfg, ana.cliente.id, &beto.cuenta_ifpe.clabe, "500").await;
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("vault.snapshot"), Some(3)), "{m}");
    assert_eq!(m["vault"]["balance"]["available_cash"], "3000.00000000");

    // 5. SPEI saliente: la orden retiene el monto (foto) y, rechazada por el
    //    banco, el reembolso llega como transferencia saliente devuelta.
    let destino = externa(1);
    pasarela.programar(&destino, Comportamiento::RechazarAlRegistrar { causa: 1 });
    let orden = outbound::solicitar(
        &pool,
        &cfg.pld,
        &cfg.spei,
        &outbound::SolicitudSpei {
            cliente_id: ana.cliente.id,
            clabe_destino: destino.clone(),
            beneficiario_nombre: Some("Beneficiario Externo".into()),
            beneficiario_rfc_curp: None,
            monto: d("100"),
            concepto: Some("Pago de prueba".into()),
            referencia_numerica: Some(123),
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap()
    .orden;
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("vault.snapshot"), Some(4)), "{m}");
    assert_eq!(m["vault"]["balance"]["available_cash"], "2900.00000000");
    for _ in 0..4 {
        vencer_ordenes(&pool).await;
        outbound::despachar(&pool, &*pasarela, &cfg.spei).await.unwrap();
    }
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("spei.transfer"), Some(5)), "{m}");
    let rastreo: String = sqlx::query_scalar("SELECT clave_rastreo::text FROM ifpe.spei_outbound WHERE id = $1")
        .bind(orden.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(m["transfer"]["tracking_key"], rastreo);
    assert_eq!(m["transfer"]["direction"], "outbound");
    assert_eq!(m["transfer"]["status"], "returned");
    assert_eq!(m["transfer"]["amount"], "100.00000000");
    assert_eq!(m["transfer"]["counterparty_name"], "Beneficiario Externo");
    assert_eq!(m["transfer"]["counterparty_bank"], "072");
    assert_eq!(m["balance"]["available_cash"], "3000.00000000");

    // 6. El navegador pide resync: foto completa con la misma versión. Otro
    //    mensaje del navegador no tiene respuesta.
    pedir_resync(&mut ws).await;
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("vault.snapshot"), Some(5)), "{m}");
    ws.send(Message::text(r#"{"type":"otra-cosa"}"#)).await.unwrap();
    nada_mas(&mut ws, Duration::from_millis(300)).await;

    // 7. Un corte de reserva: la prueba nueva llega en una foto, misma versión.
    let corte = reserva::generar_corte(&pool).await.unwrap();
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("vault.snapshot"), Some(5)), "{m}");
    assert_eq!(m["vault"]["reserve_proof"], json!({ "status": "verified", "zk_root_hash": corte.merkle_root }));

    // 8. Lo de otro cliente nunca llega aquí.
    depositar(&pool, &cfg, beto.cuenta_ifpe.id, "10").await;
    nada_mas(&mut ws, Duration::from_millis(500)).await;

    // 9. Apagado: 1001 (el navegador se reconecta a otra tarea), LISTEN suelta
    //    su conexión y no queda ningún socket.
    srv.apagar.send(()).unwrap();
    assert_eq!(cierre(&mut ws).await, (1001, "el servidor se reinicia".to_owned()));
    timeout(ESPERA, srv.tarea).await.expect("el servidor no se apagó").unwrap();
    timeout(ESPERA, oyente).await.expect("LISTEN no terminó con el apagado").unwrap();
    assert!(srv.estado.boveda.esperar_cierre(ESPERA).await);
    assert_eq!(srv.estado.boveda.sockets(), 0);
}

/// Dos pestañas del mismo cliente reciben el mismo cambio, cada una con su
/// foto inicial.
#[sqlx::test(migrations = "../migrations")]
async fn todas_las_pestanas_del_cliente_reciben_el_cambio(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let srv = arrancar(AppState::nuevo(pool.clone(), cfg.clone())).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_cliente(&srv.estado, ana.cliente.id);
    let mut pestanas = Vec::new();
    for _ in 0..2 {
        let mut ws = abrir(srv.dir, &token).await.unwrap();
        assert_eq!(recibir(&mut ws).await["seq"], 0);
        pestanas.push(ws);
    }
    let oyente = tokio::spawn(central::escuchar(srv.estado.boveda.clone()));
    for ws in &mut pestanas {
        assert_eq!(recibir(ws).await["seq"], 0, "la foto al empezar a escuchar");
    }
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "75.5").await;
    for ws in &mut pestanas {
        let m = recibir(ws).await;
        assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("spei.transfer"), Some(1)), "{m}");
        assert_eq!(m["balance"]["available_cash"], "75.50000000");
    }
    oyente.abort();
}

/// Si se cae la conexión de LISTEN (un failover, un proxy que la corta), se
/// vuelve a escuchar y cada socket recibe una foto: lo que confirmó mientras
/// nadie escuchaba no se avisa nunca.
#[sqlx::test(migrations = "../migrations")]
async fn al_perder_listen_se_reconecta_y_resincroniza(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let srv = arrancar(AppState::nuevo(pool.clone(), cfg.clone())).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let mut ws = abrir(srv.dir, &token_cliente(&srv.estado, ana.cliente.id)).await.unwrap();
    assert_eq!(recibir(&mut ws).await["seq"], 0);
    let oyente = tokio::spawn(central::escuchar(srv.estado.boveda.clone()));
    assert_eq!(recibir(&mut ws).await["seq"], 0, "la foto al empezar a escuchar");

    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_stat_activity WHERE datname = current_database() AND query = 'LISTEN \"sc_boveda\"'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let terminado: bool =
        sqlx::query_scalar("SELECT pg_terminate_backend($1)").bind(pid).fetch_one(&pool).await.unwrap();
    assert!(terminado);
    let fin = Instant::now() + ESPERA;
    while sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
        .bind(pid)
        .fetch_one(&pool)
        .await
        .unwrap()
    {
        assert!(Instant::now() < fin, "el proceso de LISTEN no terminó");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;

    // La foto llega ya escuchando otra vez: con el abono, si confirmó antes
    // del LISTEN nuevo; si confirmó después, el abono llega tras ella.
    let foto = recibir(&mut ws).await;
    assert_eq!(foto["type"], "vault.snapshot", "{foto}");
    if foto["seq"] == 0 {
        let m = recibir(&mut ws).await;
        assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("spei.transfer"), Some(1)), "{m}");
    } else {
        assert_eq!(foto["seq"], 1, "{foto}");
        assert_eq!(foto["vault"]["balance"]["available_cash"], "10.00000000");
    }
    // Y los cambios se vuelven a avisar.
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "5").await;
    let m = recibir(&mut ws).await;
    assert_eq!((m["type"].as_str(), m["seq"].as_i64()), (Some("spei.transfer"), Some(2)), "{m}");
    oyente.abort();
}

#[sqlx::test(migrations = "../migrations")]
async fn el_handshake_exige_sesion_de_cliente_origen_y_subprotocolo(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let srv = arrancar(AppState::nuevo(pool.clone(), cfg.clone())).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_cliente(&srv.estado, ana.cliente.id);
    let sp = |t: &str| format!("sc.vault.v1, bearer.{t}");

    assert_eq!(rechazo(abrir_con(srv.dir, "sc.vault.v1", Some(ORIGEN), None).await), (401, "NO_AUTENTICADO".into()));
    assert_eq!(
        rechazo(abrir_con(srv.dir, &sp(&format!("{token}x")), Some(ORIGEN), None).await).0,
        401,
        "firma alterada"
    );
    // Cross-site WebSocket hijacking: CORS no protege un WebSocket.
    assert_eq!(
        rechazo(abrir_con(srv.dir, &sp(&token), Some("https://otro-sitio.example"), None).await),
        (403, "PROHIBIDO".into())
    );
    assert_eq!(
        rechazo(abrir_con(srv.dir, &format!("bearer.{token}"), Some(ORIGEN), None).await),
        (400, "PROTOCOLO_REQUERIDO".into())
    );
    // La bóveda es solo de clientes.
    let (oficial_id, _) = oficial(&pool, "Olga").await;
    activar_mfa(&srv.estado, oficial_id).await;
    let token_oficial = srv.estado.jwt.emitir(oficial_id, RolUsuario::Cumplimiento, true).unwrap().0;
    assert_eq!(rechazo(abrir_con(srv.dir, &sp(&token_oficial), Some(ORIGEN), None).await), (403, "PROHIBIDO".into()));
    // Una sesión revocada antes de abrir.
    let beto = cliente(&pool, &cfg, "Beto").await;
    let token_beto = token_cliente(&srv.estado, beto.cliente.id);
    tokio::time::sleep(Duration::from_millis(1100)).await; // la revocación cuenta en segundos
    auth::revocar_sesiones(&pool, beto.cliente.id).await.unwrap();
    assert_eq!(rechazo(abrir(srv.dir, &token_beto).await).0, 401);
    // Una petición sin Upgrade.
    let app = routes::router(srv.estado.clone());
    let (s, v) = llamar_json(&app, "GET", "/api/v1/vault/ws", Some(&token), None).await;
    assert_eq!((s, v["error"]["codigo"].as_str()), (StatusCode::BAD_REQUEST, Some("WEBSOCKET_REQUERIDO")), "{v}");

    // Un cliente que no es navegador: sin Origin, con Authorization (pero el subprotocolo, siempre).
    let mut ws = abrir_con(srv.dir, "sc.vault.v1", None, Some(&token)).await.unwrap();
    assert_eq!(recibir(&mut ws).await["type"], "vault.snapshot");
    assert_eq!(srv.estado.boveda.sockets(), 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn tope_de_sockets_por_cliente(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let srv = arrancar(AppState::nuevo(pool.clone(), cfg.clone())).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    let token = token_cliente(&srv.estado, ana.cliente.id);

    let mut abiertos = Vec::new();
    for _ in 0..MAX_SOCKETS_POR_CLIENTE {
        let mut ws = abrir(srv.dir, &token).await.unwrap();
        recibir(&mut ws).await;
        abiertos.push(ws);
    }
    assert_eq!(rechazo(abrir(srv.dir, &token).await), (429, "DEMASIADAS_CONEXIONES".into()));
    // Otro cliente no comparte ese tope.
    let mut de_beto = abrir(srv.dir, &token_cliente(&srv.estado, beto.cliente.id)).await.unwrap();
    recibir(&mut de_beto).await;

    // Al cerrar una pestaña se libera su lugar.
    let mut primera = abiertos.remove(0);
    primera.close(None).await.unwrap();
    // El servidor contesta el cierre: para el navegador es un cierre limpio.
    match timeout(ESPERA, primera.next()).await.expect("sin respuesta al cierre") {
        Some(Ok(Message::Close(_))) => {}
        otro => panic!("se esperaba la respuesta al cierre: {otro:?}"),
    }
    let fin = Instant::now() + ESPERA;
    while srv.estado.boveda.sockets() > MAX_SOCKETS_POR_CLIENTE {
        assert!(Instant::now() < fin, "el servidor no soltó el socket cerrado");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut otra = abrir(srv.dir, &token).await.unwrap();
    recibir(&mut otra).await;
}

/// El tope por tarea es el de conexiones HTTP (`HTTP_MAX_CONEXIONES`): un
/// socket ya abierto deja de contar para el servidor HTTP.
#[sqlx::test(migrations = "../migrations")]
async fn tope_de_sockets_por_tarea(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    cfg.http.max_conexiones = 2;
    let srv = arrancar(AppState::nuevo(pool.clone(), cfg.clone())).await;
    let mut abiertos = Vec::new();
    for nombre in ["Ana", "Beto"] {
        let r = cliente(&pool, &cfg, nombre).await;
        let mut ws = abrir(srv.dir, &token_cliente(&srv.estado, r.cliente.id)).await.unwrap();
        recibir(&mut ws).await;
        abiertos.push(ws);
    }
    let carla = cliente(&pool, &cfg, "Carla").await;
    assert_eq!(
        rechazo(abrir(srv.dir, &token_cliente(&srv.estado, carla.cliente.id)).await),
        (503, "REINTENTAR".into())
    );
    // Las peticiones HTTP siguen atendiéndose con dos sockets abiertos.
    let mut s = TcpStream::connect(srv.dir).await.unwrap();
    s.write_all(b"GET /salud HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut respuesta = String::new();
    timeout(ESPERA, s.read_to_string(&mut respuesta)).await.unwrap().unwrap();
    assert!(respuesta.starts_with("HTTP/1.1 200"), "{respuesta}");
}

/// El handshake está dentro del grupo `api` del límite por IP.
#[sqlx::test(migrations = "../migrations")]
async fn el_handshake_cuenta_en_el_limite_por_ip(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    cfg.http.por_ip = LimitesPorIp { activo: true, api: Cuota { por_minuto: 1, rafaga: 2 }, ..LimitesPorIp::default() };
    let srv = arrancar(AppState::nuevo(pool.clone(), cfg.clone())).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_cliente(&srv.estado, ana.cliente.id);
    let mut primero = abrir(srv.dir, &token).await.unwrap();
    recibir(&mut primero).await;
    let mut segundo = abrir(srv.dir, &token).await.unwrap();
    recibir(&mut segundo).await;
    match abrir(srv.dir, &token).await {
        Err(ErrorWs::Http(respuesta)) => {
            assert_eq!(respuesta.status().as_u16(), 429);
            assert!(respuesta.headers().contains_key("retry-after"));
        }
        otro => panic!("se esperaba 429 del límite por IP: {:?}", otro.map(|_| ())),
    }
}

/// Una sesión revocada (o un usuario bloqueado) cierra el socket abierto con
/// 4001 en la siguiente revalidación: el navegador vuelve a autenticarse.
#[sqlx::test(migrations = "../migrations")]
async fn una_sesion_revocada_cierra_el_socket(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let tiempos = Tiempos { revalidacion: Duration::from_millis(200), ..Tiempos::default() };
    let srv = arrancar(con_tiempos(AppState::nuevo(pool.clone(), cfg.clone()), tiempos)).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let mut ws = abrir(srv.dir, &token_cliente(&srv.estado, ana.cliente.id)).await.unwrap();
    recibir(&mut ws).await;
    // Varias revalidaciones con la sesión vigente: sigue abierto. La
    // revocación cuenta en segundos: debe caer en un segundo posterior al token.
    nada_mas(&mut ws, Duration::from_millis(1100)).await;
    auth::revocar_sesiones(&pool, ana.cliente.id).await.unwrap();
    assert_eq!(cierre(&mut ws).await, (CIERRE_SESION, "sesión revocada".to_owned()));
}

/// Un navegador que pide `resync` sin parar no ocupa la base de datos: tras
/// una foto, las siguientes peticiones se juntan en una sola, que llega al
/// cumplirse `resync_minimo`.
#[sqlx::test(migrations = "../migrations")]
async fn los_resync_seguidos_se_juntan_en_uno(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let tiempos = Tiempos { resync_minimo: Duration::from_millis(500), ..Tiempos::default() };
    let srv = arrancar(con_tiempos(AppState::nuevo(pool.clone(), cfg.clone()), tiempos)).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let mut ws = abrir(srv.dir, &token_cliente(&srv.estado, ana.cliente.id)).await.unwrap();
    recibir(&mut ws).await;

    pedir_resync(&mut ws).await;
    assert_eq!(recibir(&mut ws).await["type"], "vault.snapshot", "la primera, de inmediato");
    let primera = Instant::now();
    for _ in 0..10 {
        pedir_resync(&mut ws).await;
    }
    assert_eq!(recibir(&mut ws).await["type"], "vault.snapshot");
    assert!(primera.elapsed() >= Duration::from_millis(400), "llegó a los {:?}", primera.elapsed());
    nada_mas(&mut ws, Duration::from_millis(800)).await;
}

/// El latido: el navegador responde los pings (lo hace solo) y sigue
/// conectado; uno que deja de leer se cierra tras el silencio máximo.
#[sqlx::test(migrations = "../migrations")]
async fn el_latido_mantiene_al_que_responde_y_cierra_al_mudo(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let tiempos = Tiempos {
        latido: Duration::from_millis(100),
        silencio_maximo: Duration::from_millis(400),
        ..Tiempos::default()
    };
    let srv = arrancar(con_tiempos(AppState::nuevo(pool.clone(), cfg.clone()), tiempos)).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let token = token_cliente(&srv.estado, ana.cliente.id);
    let mut vivo = abrir(srv.dir, &token).await.unwrap();
    recibir(&mut vivo).await;
    let mut mudo = abrir(srv.dir, &token).await.unwrap();
    recibir(&mut mudo).await;

    // `vivo` lee sin parar: tungstenite contesta cada ping con un pong.
    let fin = Instant::now() + Duration::from_millis(1200);
    let mut pings = 0;
    loop {
        match timeout_at(fin, vivo.next()).await {
            Err(_) => break,
            Ok(Some(Ok(Message::Ping(_)))) => pings += 1,
            Ok(otro) => panic!("solo se esperaban pings: {otro:?}"),
        }
    }
    assert!(pings >= 5, "{pings} pings en 1.2 s");
    // `mudo` no leyó en ese tiempo, así que nunca contestó: el servidor lo
    // pingueó y lo cerró. Sus tramas se leen en crudo, sin tungstenite, que
    // al leer contestaría los pings (y fallaría al escribir en un socket ya
    // cerrado por el servidor).
    let tramas = tramas_crudas(&mut mudo).await;
    let (cierre, pings) = tramas.split_last().expect("ninguna trama");
    assert!(!pings.is_empty() && pings.iter().all(|(opcode, _)| *opcode == OPCODE_PING), "{tramas:?}");
    let mut esperado = 1001u16.to_be_bytes().to_vec();
    esperado.extend_from_slice("sin respuesta".as_bytes());
    assert_eq!(cierre, &(OPCODE_CIERRE, esperado));
    assert_eq!(srv.estado.boveda.sockets(), 1, "solo queda `vivo`");
    pedir_resync(&mut vivo).await;
    assert_eq!(recibir(&mut vivo).await["type"], "vault.snapshot");
}
