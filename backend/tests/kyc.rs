//! Onboarding digital y KYC biométrico: un cliente sin KYC VERIFICADO no
//! recibe dinero, no transfiere, no retira y no invierte, ni por la API ni
//! saltándose el backend (RLS); y solo el proveedor biométrico (resultado
//! firmado) o un operador con evidencia suficiente lo verifican.

mod comun;

use std::sync::Arc;

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
    bootstrap,
    config::Config,
    db::{self, Rol},
    error::AppError,
    kyc::{
        self,
        biometria::{ProveedorBiometrico, ProveedorSimulado, Regla},
        dictamen::DictamenKyc,
        expediente::{self, EstadoKyc, OrigenEnvio},
        ine::OcrIne,
        verificacion::Resultado,
    },
    ledger,
    models::kyc::TipoPersona,
    routes,
    sofom::{credito, puente},
    spei::{
        inbound::{self, Origen, SiNoAcreditable},
        outbound,
        pasarela::{Comportamiento, PasarelaSimulada},
    },
    AppState,
};
use tower::ServiceExt;
use uuid::Uuid;

// -----------------------------------------------------------------------------
// Utilidades
// -----------------------------------------------------------------------------

struct Api {
    app: Router,
}

impl Api {
    async fn llamar(
        &self,
        metodo: &str,
        ruta: &str,
        token: Option<&str>,
        cuerpo: Option<Value>,
        cabeceras: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(metodo).uri(ruta);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        for (k, v) in cabeceras {
            req = req.header(*k, *v);
        }
        let body = match cuerpo {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        self.enviar(req.body(body).unwrap()).await
    }

    async fn enviar(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn webhook(&self, cuerpo: &[u8], firma: Option<&str>) -> (StatusCode, Value) {
        let mut req = Request::builder().method("POST").uri("/api/v1/kyc/webhook").header("content-type", "application/json");
        if let Some(f) = firma {
            req = req.header("x-kyc-firma", f);
        }
        self.enviar(req.body(Body::from(cuerpo.to_vec())).unwrap()).await
    }
}

fn api_con(pool: &PgPool, cfg: &Config, proveedor: Arc<ProveedorSimulado>) -> Api {
    let estado = AppState::nuevo(pool.clone(), cfg.clone()).con_biometria(proveedor as Arc<dyn ProveedorBiometrico>);
    Api { app: routes::router(estado) }
}

async fn registrar_http(api: &Api, nombre: &str) -> (String, Uuid) {
    let (s, r) = api
        .llamar(
            "POST",
            "/api/v1/auth/registro",
            None,
            Some(json!({
                "tipo_persona": "FISICA",
                "nombre": nombre,
                "apellido_paterno": "Prueba",
                "email": format!("{}@example.mx", nombre.to_lowercase()),
                "password": "contraseña-segura-123"
            })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    (r["sesion"]["token"].as_str().unwrap().to_owned(), r["cliente"]["id"].as_str().unwrap().parse().unwrap())
}

async fn operador(pool: &PgPool) -> Uuid {
    bootstrap::asegurar_operador(pool, "cumplimiento@synthetic.mx", "operador-de-pruebas-123").await.unwrap();
    sqlx::query_scalar("SELECT id FROM kyc.clientes WHERE email = 'cumplimiento@synthetic.mx'").fetch_one(pool).await.unwrap()
}

fn dictamen(estado: EstadoKyc, motivo: Option<&str>) -> DictamenKyc {
    DictamenKyc {
        estatus_kyc: Some(estado),
        motivo: motivo.map(Into::into),
        verificacion_id: None,
        nivel_riesgo_pld: None,
        nivel_cuenta: None,
    }
}

/// Verificar el envío que el operador revisó (el vigente).
async fn verificar(pool: &PgPool, cliente_id: Uuid) -> DictamenKyc {
    DictamenKyc { verificacion_id: en_curso(pool, cliente_id).await, ..dictamen(EstadoKyc::Verificado, None) }
}

async fn en_curso(pool: &PgPool, cliente_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT verificacion_en_curso FROM kyc.kyc_perfiles WHERE cliente_id = $1")
        .bind(cliente_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn estado(pool: &PgPool, cliente_id: Uuid) -> (String, String, i16) {
    sqlx::query_as(
        "SELECT p.estado_kyc::text, c.estatus_kyc, p.intentos FROM kyc.kyc_perfiles p \
         JOIN kyc.clientes c ON c.id = p.cliente_id WHERE p.cliente_id = $1",
    )
    .bind(cliente_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn contar(pool: &PgPool, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql).bind(id).fetch_one(pool).await.unwrap()
}

/// Paso 1 + envío (sin procesar el resultado: proveedor asíncrono).
async fn enviar(pool: &PgPool, cfg: &Config, cliente_id: Uuid, id: &IdentidadPrueba, p: &ProveedorSimulado) -> kyc::EnvioVerificacion {
    onboarding(pool, cfg, cliente_id, id, p).await.unwrap()
}

fn spei_entrante(clabe: &str, monto: &str, rastreo: &str) -> inbound::Abono {
    abono(clabe, monto, rastreo)
}

// -----------------------------------------------------------------------------
// Bloqueo estricto: la aplicación
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn sin_kyc_no_entra_ni_sale_dinero(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente_sin_kyc(&pool, &cfg, "Beto").await;
    assert_eq!(beto.cliente.estatus_kyc, synthetic_capital::models::kyc::EstatusKyc::Pendiente);
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    // SPEI recibido: el webhook lo devuelve al emisor (causa 2, cuenta bloqueada).
    let r = inbound::abonar(
        &pool,
        &cfg.pld,
        &cfg.spei,
        &spei_entrante(&beto.cuenta_ifpe.clabe, "500", "SINKYC0001"),
        Origen::Webhook,
        SiNoAcreditable::Devolver,
    )
    .await
    .unwrap();
    assert_eq!(r.respuesta_proveedor()["mensaje"], "devolver");
    assert_eq!(r.respuesta_proveedor()["id"], 2);
    assert!(r.transaccion.is_none());

    // Captura del operador: rechazada.
    let e = inbound::abonar(
        &pool,
        &cfg.pld,
        &cfg.spei,
        &spei_entrante(&beto.cuenta_ifpe.clabe, "500", "SINKYC0002"),
        Origen::Operador(Uuid::new_v4()),
        SiNoAcreditable::Rechazar,
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "KYC_NO_VERIFICADO");

    // Transferencia interna hacia un cliente sin KYC.
    let e = ledger::transferir(
        &pool,
        &cfg.pld,
        &ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: beto.cuenta_ifpe.clabe.clone(),
            monto: d("100"),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "DESTINO_NO_ACREDITABLE", "sin revelar el estado KYC del beneficiario");

    // Inversión en mutuo y SPEI saliente del cliente sin KYC.
    let e = puente::invertir(
        &pool,
        &cfg.pld,
        &puente::SolicitudMutuo { cliente_id: beto.cliente.id, monto: d("10"), plazo_dias: 28, clave_idempotencia: clave() },
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "KYC_NO_VERIFICADO");
    let e = retirar(&pool, &cfg, beto.cliente.id, "10", "012180001234567899", None, &clave()).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_NO_VERIFICADO");

    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await, (d("0"), d("0")));
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn revocar_el_kyc_bloquea_al_instante(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Rechazado, Some("credencial reportada como robada")))
        .await
        .unwrap();
    assert_eq!(estado(&pool, ana.cliente.id).await, ("RECHAZADO".into(), "RECHAZADO".into(), 1));

    let e = inbound::abonar(
        &pool,
        &cfg.pld,
        &cfg.spei,
        &spei_entrante(&ana.cuenta_ifpe.clabe, "10", "REVOCADO01"),
        Origen::Operador(op),
        SiNoAcreditable::Rechazar,
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "KYC_NO_VERIFICADO");
    let e = ledger::transferir(
        &pool,
        &cfg.pld,
        &ledger::Transferencia {
            cliente_origen_id: ana.cliente.id,
            clabe_destino: beto.cuenta_ifpe.clabe.clone(),
            monto: d("100"),
            concepto: None,
            clave_idempotencia: clave(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "KYC_NO_VERIFICADO");
    // El saldo existente sigue respaldado al 100% (no se pierde ni se presta).
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("1000"));
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn reembolsos_y_vencimientos_no_dependen_del_kyc(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let pasarela = PasarelaSimulada::new(1);
    let ana = cliente(&pool, &cfg, "Ana").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;
    // Una orden SPEI que el proveedor rechazará y un mutuo que vencerá.
    pasarela.programar("012180001234567899", Comportamiento::RechazarAlRegistrar { causa: 1 });
    let orden = retirar(&pool, &cfg, ana.cliente.id, "300", "012180001234567899", None, &clave()).await.unwrap();
    let mutuo = puente::invertir(
        &pool,
        &cfg.pld,
        &puente::SolicitudMutuo { cliente_id: ana.cliente.id, monto: d("200"), plazo_dias: 28, clave_idempotencia: clave() },
    )
    .await
    .unwrap();
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("500"));

    kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Rechazado, Some("revisión de cumplimiento"))).await.unwrap();

    // El reembolso de su propia orden fallida ('spei-reverso:') sí entra.
    vencer_ordenes(&pool).await;
    outbound::despachar(&pool, &pasarela, &cfg.spei).await.unwrap();
    let o = outbound::buscar_por_rastreo(&pool, &orden.orden.clave_rastreo).await.unwrap();
    assert_eq!(o.estado, "RECHAZADA");
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("800"));

    // El vencimiento del contrato ya firmado (RETORNO_DE_SOFOM) también. La
    // SOFOM paga el interés con capital propio.
    credito::aportar_capital(&pool, d("100"), Some("Capital".into()), clave()).await.unwrap();
    envejecer_mutuo(&pool, mutuo.contrato.id, 28).await;
    puente::liquidar(&pool, mutuo.contrato.id, None).await.unwrap();
    let (disponible, _) = saldo_ifpe(&pool, ana.cuenta_ifpe.id).await;
    assert!(disponible > d("1000"), "principal + interés regresan: {disponible}");
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Bloqueo estricto: la base de datos (aunque el backend se equivoque)
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn rls_bloquea_aunque_el_backend_no_compruebe(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let ana = cliente(&pool, &cfg, "Ana").await;
    let beto = cliente_sin_kyc(&pool, &cfg, "Beto").await;
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "1000").await;

    // Transferencia a un cliente sin KYC escrita "a mano" con el rol sc_ifpe.
    let mut tx = db::begin(&pool, Rol::Ifpe).await.unwrap();
    for (delta, cuenta) in [("-100", ana.cuenta_ifpe.id), ("100", beto.cuenta_ifpe.id)] {
        sqlx::query("UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + $1::numeric WHERE id = $2")
            .bind(delta)
            .bind(cuenta)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    let err = sqlx::query(
        "INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia) \
         VALUES ('TRANSFERENCIA', $1, $2, 100, $3)",
    )
    .bind(ana.cuenta_ifpe.id)
    .bind(beto.cuenta_ifpe.id)
    .bind(clave())
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("row-level security"), "{err}");
    assert_eq!(AppError::from(err).codigo(), "KYC_NO_VERIFICADO");
    drop(tx);

    // Un mutuo para el cliente sin KYC, como lo escribiría el puente.
    let mut tx = db::begin(&pool, Rol::Puente).await.unwrap();
    let err = sqlx::query(
        "INSERT INTO sofom.contratos_mutuo (cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id, \
           principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos) \
         VALUES ($1, gen_random_uuid(), $2, gen_random_uuid(), 10, 0.05, 28, current_date, repeat('0', 64))",
    )
    .bind(beto.cliente.id)
    .bind(beto.cuenta_ifpe.id)
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("row-level security"), "{err}");
    drop(tx);

    // Ni el rol de captura (sc_kyc) puede verificar ni escribir el espejo.
    let mut tx = db::begin(&pool, Rol::Kyc).await.unwrap();
    let err = sqlx::query("UPDATE kyc.clientes SET estatus_kyc = 'VERIFICADO', kyc_verificado_en = now() WHERE id = $1")
        .bind(beto.cliente.id)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"), "{err}");
    drop(tx);
    let mut tx = db::begin(&pool, Rol::Kyc).await.unwrap();
    let err = sqlx::query("UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = $1")
        .bind(beto.cliente.id)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("SC_KYC_TRANSICION"), "{err}");
    drop(tx);

    assert_eq!(saldo_ifpe(&pool, beto.cuenta_ifpe.id).await.0, d("0"));
    verificar_invariantes(&pool).await;
}

// -----------------------------------------------------------------------------
// Pipeline biométrico
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn onboarding_completo_verifica_y_desbloquea(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    assert_eq!(r.estado_kyc, EstadoKyc::Verificado);
    let v = r.resultado.unwrap();
    assert_eq!(v.resultado, Resultado::Aprobado);
    assert_eq!(v.score_similitud, Some(d("0.93")));
    assert_eq!((v.intentos, v.intentos_restantes), (1, 2));

    // Espejo e identidad verificada en kyc.clientes; bitácora inmutable.
    let c = kyc::perfil(&pool, ana.cliente.id).await.unwrap();
    assert_eq!(c.curp.as_deref(), Some(id.curp.as_str()));
    assert_eq!(c.rfc.as_deref(), Some(id.rfc.as_str()));
    assert_eq!(c.documento_identidad_numero.as_deref(), Some(id.clave_elector.as_str()));
    assert!(c.kyc_verificado_en.is_some());
    let (curp_ocr, umbral, vida): (String, Decimal, bool) = sqlx::query_as(
        "SELECT curp_ocr, umbral_aplicado, prueba_vida_exitosa FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1",
    )
    .bind(ana.cliente.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((curp_ocr.as_str(), umbral, vida), (id.curp.as_str(), d("0.85"), true));
    let historial: Vec<(String, String)> =
        sqlx::query_as("SELECT estado_nuevo::text, rol FROM kyc.kyc_historial WHERE cliente_id = $1 ORDER BY id")
            .bind(ana.cliente.id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(historial, vec![("EN_REVISION".into(), "sc_kyc".into()), ("VERIFICADO".into(), "sc_kyc_verificador".into())]);

    // Ya opera.
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "250").await;
    assert_eq!(saldo_ifpe(&pool, ana.cuenta_ifpe.id).await.0, d("250"));

    // El expediente verificado ya no se edita.
    let e = expediente::guardar_datos(&pool, ana.cliente.id, &id.datos_personales(), chrono::Utc::now().date_naive())
        .await
        .unwrap_err();
    assert_eq!(e.codigo(), "KYC_EXPEDIENTE_NO_EDITABLE");
    verificar_invariantes(&pool).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn zona_gris_va_a_un_operador(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { score: Some(Some(d("0.78"))), ..Regla::default() });
    let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    assert_eq!(r.estado_kyc, EstadoKyc::EnRevision);
    assert_eq!(r.resultado.unwrap().resultado, Resultado::Revision);

    // Mientras tanto no opera.
    let e = inbound::abonar(
        &pool,
        &cfg.pld,
        &cfg.spei,
        &spei_entrante(&ana.cuenta_ifpe.clabe, "10", "GRIS0001"),
        Origen::Webhook,
        SiNoAcreditable::Rechazar,
    )
    .await
    .unwrap_err();
    assert_eq!(e.codigo(), "KYC_NO_VERIFICADO");

    // Cola del operador y dictamen.
    let cola = kyc::dictamen::pendientes(&pool, 10).await.unwrap();
    assert_eq!(cola.len(), 1);
    assert_eq!(cola[0].score_similitud, Some(d("0.78")));
    assert_eq!(cola[0].resultado.as_deref(), Some("REVISION"));
    let e = kyc::dictaminar(&pool, ana.cliente.id, ana.cliente.id, &verificar(&pool, ana.cliente.id).await).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_AUTODICTAMEN");
    // Sin decir qué envío revisó, no verifica.
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Verificado, None)).await.unwrap_err();
    assert_eq!(e.codigo(), "VALIDACION");
    let c = kyc::dictaminar(&pool, op, ana.cliente.id, &verificar(&pool, ana.cliente.id).await).await.unwrap();
    assert_eq!(c.estatus_kyc, synthetic_capital::models::kyc::EstatusKyc::Verificado);
    let revisor: Option<Uuid> = sqlx::query_scalar("SELECT revisado_por FROM kyc.kyc_perfiles WHERE cliente_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(revisor, Some(op));
    depositar(&pool, &cfg, ana.cuenta_ifpe.id, "10").await;
}

#[sqlx::test(migrations = "../migrations")]
async fn un_operador_no_verifica_sin_evidencia_suficiente(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;

    // PENDIENTE sin ningún envío: no hay envío que verificar.
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &verificar(&pool, ana.cliente.id).await).await.unwrap_err();
    assert_eq!(e.codigo(), "VALIDACION");
    let inventado = DictamenKyc { verificacion_id: Some(Uuid::new_v4()), ..dictamen(EstadoKyc::Verificado, None) };
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &inventado).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_DICTAMEN_DESACTUALIZADO");

    // Envío cuyo resultado del proveedor no llegó (asíncrono): sin evidencia.
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { asincrono: true, ..Regla::default() });
    let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    assert_eq!(r.estado_kyc, EstadoKyc::EnRevision);
    assert!(r.resultado.is_none());
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &verificar(&pool, ana.cliente.id).await).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_SIN_EVIDENCIA");

    // Con evidencia de prueba de vida fallida, tampoco.
    kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Pendiente, None)).await.unwrap();
    p.programar(&id.curp, Regla { vida_fallida: true, asincrono: false, ..Regla::default() });
    let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    assert_eq!(r.resultado.as_ref().unwrap().resultado, Resultado::Rechazado);
    assert_eq!(r.estado_kyc, EstadoKyc::Pendiente, "le queda un intento");
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &verificar(&pool, ana.cliente.id).await).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_TRANSICION");

    // Y rechazar exige motivo.
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Rechazado, None)).await.unwrap_err();
    assert_eq!(e.codigo(), "VALIDACION");
    assert_eq!(estado(&pool, ana.cliente.id).await.0, "PENDIENTE");
}

#[sqlx::test(migrations = "../migrations")]
async fn fallas_consumen_intentos_hasta_rechazar(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    fn otra_curp(o: &mut OcrIne) {
        o.curp = "LOOA531113HTCPBN07".into();
    }
    let reglas = [
        (Regla { score: Some(Some(d("0.42"))), ..Regla::default() }, "similitud facial"),
        (Regla { alterar_ocr: Some(otra_curp), ..Regla::default() }, "CURP de la credencial"),
        (Regla { documento_invalido: true, ..Regla::default() }, "credencial no válida"),
    ];
    for (i, (regla, esperado)) in reglas.into_iter().enumerate() {
        p.programar(&id.curp, regla);
        let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
        let v = r.resultado.unwrap();
        assert_eq!(v.resultado, Resultado::Rechazado);
        assert!(v.detalles.iter().any(|d| d.tipo == "ERROR" && d.mensaje.contains(esperado)), "{:?}", v.detalles);
        let ultimo = i == 2;
        assert_eq!(r.estado_kyc, if ultimo { EstadoKyc::Rechazado } else { EstadoKyc::Pendiente });
        assert_eq!(v.intentos_restantes, 2 - i as i16);
    }
    let (perfil, espejo, intentos) = estado(&pool, ana.cliente.id).await;
    assert_eq!((perfil.as_str(), espejo.as_str(), intentos), ("RECHAZADO", "RECHAZADO", 3));
    let motivo: Option<String> = sqlx::query_scalar("SELECT motivo_rechazo FROM kyc.kyc_perfiles WHERE cliente_id = $1")
        .bind(ana.cliente.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(motivo.unwrap().starts_with("intentos agotados"));
    assert_eq!(contar(&pool, "SELECT count(*) FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1", ana.cliente.id).await, 3);

    // Sin intentos, un nuevo envío ni siquiera llega al proveedor.
    let e = onboarding(&pool, &cfg, ana.cliente.id, &id, &p).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_EXPEDIENTE_NO_EDITABLE");
}

#[sqlx::test(migrations = "../migrations")]
async fn evidencia_ajena_o_imagenes_repetidas_se_rechazan(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");

    // El cliente envía la misma imagen como dos cuadros: rechazado antes del proveedor.
    expediente::guardar_datos(&pool, ana.cliente.id, &id.datos_personales(), chrono::Utc::now().date_naive())
        .await
        .unwrap();
    let mut s = id.solicitud();
    s.selfie.frames[1] = s.selfie.frames[0].clone();
    let e = kyc::verificar_identidad(&pool, &cfg.kyc, &p, ana.cliente.id, &s, &OrigenEnvio::default()).await.unwrap_err();
    assert_eq!(e.codigo(), "VALIDACION");
    assert_eq!(estado(&pool, ana.cliente.id).await.2, 0, "no consume intentos");

    // El proveedor dice haber analizado otras imágenes.
    p.programar(&id.curp, Regla { otra_evidencia: true, ..Regla::default() });
    let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    let v = r.resultado.unwrap();
    assert_eq!(v.resultado, Resultado::Rechazado);
    assert!(v.detalles.iter().any(|d| d.mensaje.contains("imágenes analizadas")), "{:?}", v.detalles);
}

#[sqlx::test(migrations = "../migrations")]
async fn umbral_configurable_en_la_base_de_datos(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    cfg.kyc.score_minimo = Some(d("0.95"));
    cfg.kyc.max_intentos = Some(5);
    bootstrap::asegurar_infraestructura(&pool, &cfg).await.unwrap();
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    let r = enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    let v = r.resultado.unwrap();
    assert_eq!(v.resultado, Resultado::Revision, "0.93 < 0.95");
    assert_eq!(v.intentos_restantes, 4);
    let umbral: Decimal =
        sqlx::query_scalar("SELECT umbral_aplicado FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1")
            .bind(ana.cliente.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(umbral, d("0.95"));

    // Un umbral fuera de rango no se acepta (CHECK de kyc.parametros).
    cfg.kyc.score_minimo = Some(d("0.40"));
    assert!(bootstrap::asegurar_infraestructura(&pool, &cfg).await.is_err());
}

#[sqlx::test(migrations = "../migrations")]
async fn identidad_declarada_no_bloquea_a_su_titular(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let id = IdentidadPrueba::nueva("Ana");
    // Un tercero se registra declarando la CURP y el RFC de Ana.
    let impostor = kyc::registrar(
        &pool,
        &cfg.clabe_prefijo,
        kyc::DatosRegistro {
            tipo_persona: TipoPersona::Fisica,
            nombre: "Impostor".into(),
            apellido_paterno: Some("X".into()),
            apellido_materno: None,
            email: "impostor@example.mx".into(),
            password: "contraseña-de-prueba-123".into(),
            curp: Some(id.curp.clone()),
            rfc: Some(id.rfc.clone()),
            telefono: None,
            actividad_economica: None,
            ingreso_mensual_estimado: None,
        },
    )
    .await
    .unwrap();
    // Ana se registra y verifica igual.
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    assert_eq!(enviar(&pool, &cfg, ana.cliente.id, &id, &p).await.estado_kyc, EstadoKyc::Verificado);
    // El impostor no puede enviar a revisión la identidad de Ana.
    let e = onboarding(&pool, &cfg, impostor.cliente.id, &id, &p).await.unwrap_err();
    assert_eq!(e.codigo(), "IDENTIDAD_EN_USO");
    assert_eq!(estado(&pool, impostor.cliente.id).await, ("PENDIENTE".into(), "PENDIENTE".into(), 0));
}

#[sqlx::test(migrations = "../migrations")]
async fn requisitos_del_expediente(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let hoy = chrono::Utc::now().date_naive();
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");

    // Sin datos personales no se envía.
    let e = kyc::verificar_identidad(&pool, &cfg.kyc, &p, ana.cliente.id, &id.solicitud(), &OrigenEnvio::default())
        .await
        .unwrap_err();
    assert_eq!(e.codigo(), "KYC_DATOS_INCOMPLETOS");
    expediente::guardar_datos(&pool, ana.cliente.id, &id.datos_personales(), hoy).await.unwrap();

    // Consentimiento expreso para datos biométricos.
    let mut s = id.solicitud();
    s.consentimiento_biometrico = false;
    let e = kyc::verificar_identidad(&pool, &cfg.kyc, &p, ana.cliente.id, &s, &OrigenEnvio::default()).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_SIN_CONSENTIMIENTO");

    // Credencial vencida o con una clave de elector de otra fecha de nacimiento.
    let mut s = id.solicitud();
    s.ine.emision = 2014;
    s.ine.vigencia = 2024;
    assert_eq!(
        kyc::verificar_identidad(&pool, &cfg.kyc, &p, ana.cliente.id, &s, &OrigenEnvio::default()).await.unwrap_err().codigo(),
        "VALIDACION"
    );
    let mut s = id.solicitud();
    s.ine.clave_elector = IdentidadPrueba::nueva("Ana").clave_elector;
    assert_eq!(
        kyc::verificar_identidad(&pool, &cfg.kyc, &p, ana.cliente.id, &s, &OrigenEnvio::default()).await.unwrap_err().codigo(),
        "VALIDACION"
    );
    assert_eq!(estado(&pool, ana.cliente.id).await.2, 0, "las validaciones previas no consumen intentos");

    // Persona moral: no aplica la identificación biométrica remota.
    let taller = kyc::registrar(
        &pool,
        &cfg.clabe_prefijo,
        kyc::DatosRegistro {
            tipo_persona: TipoPersona::Moral,
            nombre: "Taller Hayek SA de CV".into(),
            apellido_paterno: None,
            apellido_materno: None,
            email: "taller@example.mx".into(),
            password: "contraseña-de-prueba-123".into(),
            curp: None,
            rfc: None,
            telefono: None,
            actividad_economica: None,
            ingreso_mensual_estimado: None,
        },
    )
    .await
    .unwrap();
    let e = expediente::guardar_datos(&pool, taller.cliente.id, &id.datos_personales(), hoy).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_SOLO_PERSONA_FISICA");

    // Menor de edad.
    let nina = cliente_sin_kyc(&pool, &cfg, "Nina").await;
    let fecha = chrono::NaiveDate::from_ymd_opt(2012, 3, 4).unwrap();
    let curp = kyc::curp::construir("Nina", "Prueba", None, fecha, kyc::curp::Sexo::M, "DF", 1);
    let mut datos = IdentidadPrueba::nueva("Nina").datos_personales();
    datos.apellido_materno = None;
    datos.rfc = kyc::rfc::construir_desde_curp(&curp, "AB");
    datos.curp = curp;
    let e = expediente::guardar_datos(&pool, nina.cliente.id, &datos, hoy).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_MENOR_DE_EDAD");
}

// -----------------------------------------------------------------------------
// API: webhook del proveedor, IP de origen, límites y sesión
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn webhook_firmado_idempotente_y_solo_del_envio_vigente(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let p = Arc::new(proveedor(&cfg));
    let api = api_con(&pool, &cfg, p.clone());
    let (token, ana) = registrar_http(&api, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { asincrono: true, ..Regla::default() });

    let (s, _) = api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json()), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, r) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json()), &[]).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{r}");
    assert_eq!(r["estado_kyc"], "EN_REVISION");
    let (viejo, firma_vieja) = p.tomar_pendientes().pop().unwrap();

    // Cumplimiento pide un nuevo envío: el resultado del primero queda obsoleto.
    kyc::dictaminar(&pool, op, ana, &dictamen(EstadoKyc::Pendiente, None)).await.unwrap();
    let (s, _) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json()), &[]).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (cuerpo, firma) = p.tomar_pendientes().pop().unwrap();
    let (s, e) = api.webhook(&viejo, Some(&firma_vieja)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["error"]["codigo"], "KYC_VERIFICACION_AJENA");

    // Sin firma, con firma de otro secreto o con el cuerpo alterado: 401.
    let (s, _) = api.webhook(&cuerpo, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let ajena = synthetic_capital::spei::firma::firmar(b"secreto-de-otro-proveedor-0123456789abcdef", chrono::Utc::now().timestamp(), &cuerpo);
    let (s, _) = api.webhook(&cuerpo, Some(&ajena)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let mut alterado: Value = serde_json::from_slice(&cuerpo).unwrap();
    alterado["comparacion_facial"]["score"] = json!(0.99);
    let alterado = serde_json::to_vec(&alterado).unwrap();
    let (s, _) = api.webhook(&alterado, Some(&firma)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(estado(&pool, ana).await.0, "EN_REVISION");

    // El resultado vigente verifica; repetirlo es idempotente.
    let (s, v) = api.webhook(&cuerpo, Some(&firma)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["estado_kyc"].as_str(), v["idempotente"].as_bool()), (Some("VERIFICADO"), Some(false)));
    let (s, v) = api.webhook(&cuerpo, Some(&firma)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["idempotente"], true);
    // Otro resultado (bien firmado) para la misma verificación: conflicto.
    let firma_alterado = p.firmar(&alterado);
    let (s, e) = api.webhook(&alterado, Some(&firma_alterado)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["error"]["codigo"], "KYC_RESULTADO_DISTINTO");
    assert_eq!(contar(&pool, "SELECT count(*) FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1", ana).await, 1);
}

#[sqlx::test(migrations = "../migrations")]
async fn ip_de_origen_y_limite_de_cuerpo(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    let p = Arc::new(proveedor(&cfg));

    // Sin proxy configurado, X-Forwarded-For se ignora (la escribe cualquiera).
    let api = api_con(&pool, &cfg, p.clone());
    let (token, ana) = registrar_http(&api, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json()), &[]).await;
    let (s, r) = api
        .llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json()), &[("x-forwarded-for", "198.51.100.7")])
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let ip: Option<String> =
        sqlx::query_scalar("SELECT host(ip_origen) FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1")
            .bind(ana)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ip, None);

    // Detrás de un proxy propio: la última IP de la cadena (la que agregó el proxy).
    cfg.http.proxy_confiable = true;
    let api = api_con(&pool, &cfg, p.clone());
    let (token, beto) = registrar_http(&api, "Beto").await;
    let id = IdentidadPrueba::nueva("Beto");
    api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json()), &[]).await;
    let ua = [("x-forwarded-for", "10.9.9.9, 203.0.113.42"), ("user-agent", "Mozilla/5.0 prueba")];
    let (s, r) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json()), &ua).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (ip, agente): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT host(ip_origen), user_agent FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1",
    )
    .bind(beto)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((ip.as_deref(), agente.as_deref()), (Some("203.0.113.42"), Some("Mozilla/5.0 prueba")));

    // /kyc/verificar admite imágenes (más de 64 KB) pero no más de 12 MB.
    let (token, _) = registrar_http(&api, "Carla").await;
    let enorme = json!({ "relleno": "x".repeat(13 * 1024 * 1024) });
    let (s, e) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(enorme), &[]).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{e}");
    // El resto de la API conserva su límite.
    let (s, _) = api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(json!({ "relleno": "x".repeat(70_000) })), &[]).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[sqlx::test(migrations = "../migrations")]
async fn rechazado_entra_a_ver_su_estado_pero_no_opera(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let p = Arc::new(proveedor(&cfg));
    let api = api_con(&pool, &cfg, p);
    let (token, ana) = registrar_http(&api, "Ana").await;
    kyc::dictaminar(&pool, op, ana, &dictamen(EstadoKyc::Rechazado, Some("documento apócrifo"))).await.unwrap();

    let (s, sesion) = api
        .llamar(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "email": "ana@example.mx", "password": "contraseña-segura-123" })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{sesion}");
    let (s, exp) = api.llamar("GET", "/api/v1/kyc/expediente", Some(&token), None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((exp["estado_kyc"].as_str(), exp["motivo_rechazo"].as_str()), (Some("RECHAZADO"), Some("documento apócrifo")));
    let (s, e) = api
        .llamar(
            "POST",
            "/api/v1/ifpe/depositos/simulados",
            Some(&token),
            Some(json!({ "monto": "100", "clave_idempotencia": "dep-0001" })),
            &[],
        )
        .await;
    assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("KYC_NO_VERIFICADO")));

    // En la Lista de Personas Bloqueadas ya no entra.
    sqlx::query("UPDATE kyc.clientes SET en_lista_bloqueadas = true, estatus_kyc = 'SUSPENDIDO' WHERE id = $1")
        .bind(ana)
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = api.llamar("GET", "/api/v1/kyc/expediente", Some(&token), None, &[]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

// -----------------------------------------------------------------------------
// Endurecimiento (migración 0007)
// -----------------------------------------------------------------------------

#[sqlx::test(migrations = "../migrations")]
async fn sin_proveedor_certificado_falla_cerrado(pool: PgPool) {
    let mut cfg = preparar(&pool).await;
    // Fuera de MODO_DEMO no hay proveedor simulado: 503 sin consumir intentos.
    cfg.modo_demo = false;
    bootstrap::asegurar_infraestructura(&pool, &cfg).await.unwrap();
    let api = Api { app: routes::router(AppState::nuevo(pool.clone(), cfg.clone())) };
    let (token, ana) = registrar_http(&api, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    let (s, _) = api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json()), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, e) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json()), &[]).await;
    assert_eq!((s, e["error"]["codigo"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("KYC_PROVEEDOR_NO_DISPONIBLE")));
    assert_eq!(estado(&pool, ana).await, ("PENDIENTE".into(), "PENDIENTE".into(), 0));

    // Aunque alguien conecte el simulado, la base de datos rechaza su
    // resultado fuera de demostración; el intento se devuelve.
    let e = kyc::verificar_identidad(&pool, &cfg.kyc, &proveedor(&cfg), ana, &id.solicitud(), &OrigenEnvio::default())
        .await
        .unwrap_err();
    assert_eq!(e.codigo(), "KYC_PROVEEDOR_ERROR");
    assert_eq!(estado(&pool, ana).await, ("PENDIENTE".into(), "PENDIENTE".into(), 0));
    assert_eq!(contar(&pool, "SELECT count(*) FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = $1", ana).await, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn una_falla_del_proveedor_no_cuesta_intentos(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { falla: true, ..Regla::default() });
    for _ in 0..4 {
        let e = onboarding(&pool, &cfg, ana.cliente.id, &id, &p).await.unwrap_err();
        assert_eq!(e.codigo(), "KYC_PROVEEDOR_ERROR");
    }
    assert_eq!(estado(&pool, ana.cliente.id).await, ("PENDIENTE".into(), "PENDIENTE".into(), 0));
    p.programar(&id.curp, Regla::default());
    assert_eq!(enviar(&pool, &cfg, ana.cliente.id, &id, &p).await.estado_kyc, EstadoKyc::Verificado);
}

#[sqlx::test(migrations = "../migrations")]
async fn sondear_identidades_ajenas_tiene_limite(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    assert_eq!(enviar(&pool, &cfg, ana.cliente.id, &id, &p).await.estado_kyc, EstadoKyc::Verificado);
    let beto = cliente_sin_kyc(&pool, &cfg, "Beto").await;
    for _ in 0..3 {
        let e = onboarding(&pool, &cfg, beto.cliente.id, &id, &p).await.unwrap_err();
        assert_eq!(e.codigo(), "IDENTIDAD_EN_USO");
    }
    let e = onboarding(&pool, &cfg, beto.cliente.id, &id, &p).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_SIN_INTENTOS", "el cuarto sondeo ya no responde si la CURP es cliente");
}

#[sqlx::test(migrations = "../migrations")]
async fn una_curp_rechazada_en_otras_cuentas_se_bloquea(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = proveedor(&cfg);
    let victima = IdentidadPrueba::nueva("Ana");
    p.programar(&victima.curp, Regla { score: Some(Some(d("0.31"))), ..Regla::default() });
    let mallory = cliente_sin_kyc(&pool, &cfg, "Mallory").await;
    for _ in 0..3 {
        enviar(&pool, &cfg, mallory.cliente.id, &victima, &p).await;
    }
    assert_eq!(estado(&pool, mallory.cliente.id).await.0, "RECHAZADO");
    // Una cuenta nueva con la misma CURP ya no llega al proveedor.
    let trudy = cliente_sin_kyc(&pool, &cfg, "Trudy").await;
    let e = onboarding(&pool, &cfg, trudy.cliente.id, &victima, &p).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_IDENTIDAD_BLOQUEADA");
    assert_eq!(estado(&pool, trudy.cliente.id).await.2, 0);
}

#[sqlx::test(migrations = "../migrations")]
async fn el_dictamen_aplica_al_envio_revisado(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { score: Some(Some(d("0.80"))), ..Regla::default() });
    enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    // El operador A abre la cola y ve el envío X.
    let visto = verificar(&pool, ana.cliente.id).await;
    // Mientras tanto, otro operador pide un nuevo envío y el cliente reenvía (Y).
    kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Pendiente, None)).await.unwrap();
    p.programar(&id.curp, Regla { score: Some(Some(d("0.71"))), ..Regla::default() });
    assert_eq!(enviar(&pool, &cfg, ana.cliente.id, &id, &p).await.estado_kyc, EstadoKyc::EnRevision);
    // A verifica con su pantalla vieja: no se aplica a Y.
    let e = kyc::dictaminar(&pool, op, ana.cliente.id, &visto).await.unwrap_err();
    assert_eq!(e.codigo(), "KYC_DICTAMEN_DESACTUALIZADO");
    assert_eq!(estado(&pool, ana.cliente.id).await.0, "EN_REVISION");
}

#[sqlx::test(migrations = "../migrations")]
async fn reabrir_deja_al_menos_un_intento(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let op = operador(&pool).await;
    let p = proveedor(&cfg);
    let ana = cliente_sin_kyc(&pool, &cfg, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { score: Some(Some(d("0.40"))), ..Regla::default() });
    enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    enviar(&pool, &cfg, ana.cliente.id, &id, &p).await;
    // Tercer intento en zona gris; cumplimiento pide un nuevo envío.
    p.programar(&id.curp, Regla { score: Some(Some(d("0.80"))), ..Regla::default() });
    assert_eq!(enviar(&pool, &cfg, ana.cliente.id, &id, &p).await.estado_kyc, EstadoKyc::EnRevision);
    kyc::dictaminar(&pool, op, ana.cliente.id, &dictamen(EstadoKyc::Pendiente, None)).await.unwrap();
    assert_eq!(estado(&pool, ana.cliente.id).await.2, 2, "le queda un intento");
    p.programar(&id.curp, Regla::default());
    assert_eq!(enviar(&pool, &cfg, ana.cliente.id, &id, &p).await.estado_kyc, EstadoKyc::Verificado);
}

#[sqlx::test(migrations = "../migrations")]
async fn el_cliente_no_ve_el_score_exacto(pool: PgPool) {
    let cfg = preparar(&pool).await;
    let p = Arc::new(proveedor(&cfg));
    let api = api_con(&pool, &cfg, p.clone());
    let (token, ana) = registrar_http(&api, "Ana").await;
    let id = IdentidadPrueba::nueva("Ana");
    p.programar(&id.curp, Regla { score: Some(Some(d("0.78"))), ..Regla::default() });
    api.llamar("POST", "/api/v1/kyc/perfil", Some(&token), Some(id.datos_json()), &[]).await;
    let (s, v) = api.llamar("POST", "/api/v1/kyc/verificar", Some(&token), Some(id.solicitud_json()), &[]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["resultado"]["resultado"], "REVISION");
    assert_eq!(v["resultado"]["score_similitud"], Value::Null);
    let (_, exp) = api.llamar("GET", "/api/v1/kyc/expediente", Some(&token), None, &[]).await;
    assert_eq!(exp["ultimo_resultado"]["score_similitud"], Value::Null);
    assert_eq!(exp["ultimo_resultado"]["umbral_aplicado"], Value::Null);
    // Cumplimiento sí lo ve.
    let cola = kyc::dictamen::pendientes(&pool, 10).await.unwrap();
    assert_eq!(cola[0].expediente.cliente_id, ana);
    assert_eq!(cola[0].score_similitud, Some(d("0.78")));
}
