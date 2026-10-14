//! Utilidades compartidas por las pruebas de integración.
//!
//! Cada prueba `#[sqlx::test]` recibe una base de datos nueva con todas las
//! migraciones aplicadas (requiere `DATABASE_URL` con un usuario que tenga
//! CREATEDB y CREATEROLE).

#![allow(dead_code)]

use std::{
    str::FromStr,
    sync::atomic::{AtomicU32, Ordering},
};

use base64::Engine;
use chrono::NaiveDate;

use rust_decimal::Decimal;
use sqlx::PgPool;
use synthetic_capital::{
    bootstrap,
    config::Config,
    db,
    error::AppResult,
    kyc::{
        self,
        biometria::ProveedorSimulado,
        curp::{self, Sexo},
        expediente::{self, DatosIne, DatosPersonales, Domicilio, OrigenEnvio, Selfie, SolicitudVerificacion},
        ine, rfc, DatosRegistro, EnvioVerificacion, Registro,
    },
    ledger::{hash, Movimiento},
    models::{kyc::TipoPersona, sofom::ContratoMutuo},
    sofom::puente,
    spei::{
        inbound::{self, Origen, SiNoAcreditable},
        outbound, IdentidadSpei,
    },
};
use uuid::Uuid;

pub fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

pub async fn preparar(pool: &PgPool) -> Config {
    let cfg = Config::para_pruebas("");
    bootstrap::asegurar_infraestructura(pool, &cfg).await.unwrap();
    cfg
}

/// Cliente persona física con cuenta IFPE y KYC VERIFICADO por el camino
/// real: datos personales, INE y selfie evaluados por el proveedor simulado.
pub async fn cliente(pool: &PgPool, cfg: &Config, nombre: &str) -> Registro {
    let mut r = cliente_sin_kyc(pool, cfg, nombre).await;
    let id = IdentidadPrueba::nueva(nombre);
    let envio = onboarding(pool, cfg, r.cliente.id, &id, &proveedor(cfg)).await.unwrap();
    assert_eq!(envio.estado_kyc, expediente::EstadoKyc::Verificado, "{envio:?}");
    r.cliente = kyc::perfil(pool, r.cliente.id).await.unwrap();
    r
}

/// Cliente recién registrado: expediente PENDIENTE, sin poder operar.
pub async fn cliente_sin_kyc(pool: &PgPool, cfg: &Config, nombre: &str) -> Registro {
    kyc::registrar(
        pool,
        &cfg.clabe_prefijo,
        DatosRegistro {
            tipo_persona: TipoPersona::Fisica,
            nombre: nombre.into(),
            apellido_paterno: Some("Prueba".into()),
            apellido_materno: None,
            email: format!("{}@example.mx", nombre.to_lowercase()),
            password: "contraseña-de-prueba-123".into(),
            curp: None,
            rfc: None,
            telefono: None,
            actividad_economica: None,
            ingreso_mensual_estimado: None,
        },
    )
    .await
    .unwrap()
}

/// Proveedor biométrico simulado que firma con el secreto de la configuración.
pub fn proveedor(cfg: &Config) -> ProveedorSimulado {
    ProveedorSimulado::nuevo(cfg.kyc.webhook_secretos[0].as_bytes())
}

/// Pasos 1 a 3 del onboarding con el proveedor indicado.
pub async fn onboarding(
    pool: &PgPool,
    cfg: &Config,
    cliente_id: Uuid,
    id: &IdentidadPrueba,
    proveedor: &ProveedorSimulado,
) -> AppResult<EnvioVerificacion> {
    expediente::guardar_datos(pool, cliente_id, &id.datos_personales(), chrono::Utc::now().date_naive()).await?;
    kyc::verificar_identidad(pool, &cfg.kyc, proveedor, cliente_id, &id.solicitud(), &OrigenEnvio::default()).await
}

/// Revoca el KYC (como cumplimiento): el cliente deja de poder operar.
pub async fn revocar_kyc(pool: &PgPool, cliente_id: Uuid) {
    sqlx::query("UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = $1")
        .bind(cliente_id)
        .execute(pool)
        .await
        .unwrap();
}

static SECUENCIA: AtomicU32 = AtomicU32::new(0);

/// Identidad mexicana válida y única en el proceso (CURP, RFC y clave de
/// elector con sus dígitos verificadores) para un nombre de pila.
#[derive(Debug, Clone)]
pub struct IdentidadPrueba {
    pub nombres: String,
    pub apellido_paterno: String,
    pub apellido_materno: Option<String>,
    pub curp: String,
    pub rfc: String,
    pub clave_elector: String,
    pub cic: String,
    pub semilla: u32,
}

impl IdentidadPrueba {
    pub fn nueva(nombre: &str) -> Self {
        let n = SECUENCIA.fetch_add(1, Ordering::Relaxed);
        let fecha = NaiveDate::from_ymd_opt(1960, 1, 1).unwrap() + chrono::Days::new(u64::from(n % 14_000) + 1);
        let (paterno, materno) = ("Prueba", Some("Sintética"));
        let curp = curp::construir(nombre, paterno, materno, fecha, Sexo::M, "DF", (n % 10) as u8);
        let rfc = rfc::construir_desde_curp(&curp, &format!("{:02}", n % 100));
        let clave_elector = ine::construir_clave_elector(nombre, paterno, materno, fecha, 9, Sexo::M, (n % 1000) as u16);
        Self {
            nombres: nombre.to_owned(),
            apellido_paterno: paterno.to_owned(),
            apellido_materno: materno.map(str::to_owned),
            curp,
            rfc,
            clave_elector,
            cic: format!("{:09}", 100_000_000 + n),
            semilla: n,
        }
    }

    pub fn datos_personales(&self) -> DatosPersonales {
        DatosPersonales {
            nombres: self.nombres.clone(),
            apellido_paterno: self.apellido_paterno.clone(),
            apellido_materno: self.apellido_materno.clone(),
            curp: self.curp.clone(),
            rfc: self.rfc.clone(),
            domicilio: Domicilio {
                calle: "Av. Paseo de la Reforma".into(),
                numero_exterior: "222".into(),
                numero_interior: None,
                colonia: "Juárez".into(),
                municipio: "Cuauhtémoc".into(),
                estado: "Ciudad de México".into(),
                codigo_postal: "06600".into(),
            },
        }
    }

    pub fn solicitud(&self) -> SolicitudVerificacion {
        SolicitudVerificacion {
            ine: DatosIne {
                clave_elector: self.clave_elector.clone(),
                cic: self.cic.clone(),
                identificador_ciudadano: None,
                emision: 2023,
                vigencia: 2033,
                frente: imagen_jpeg(self.semilla, 1),
                reverso: imagen_jpeg(self.semilla, 2),
            },
            selfie: Selfie { frames: (3..6).map(|i| imagen_jpeg(self.semilla, i)).collect(), reto: Some("PARPADEA".into()) },
            consentimiento_biometrico: true,
        }
    }

    pub fn datos_json(&self) -> serde_json::Value {
        let d = self.datos_personales();
        serde_json::json!({
            "nombres": d.nombres, "apellido_paterno": d.apellido_paterno, "apellido_materno": d.apellido_materno,
            "curp": d.curp, "rfc": d.rfc,
            "domicilio": serde_json::to_value(&d.domicilio).unwrap(),
        })
    }

    pub fn solicitud_json(&self) -> serde_json::Value {
        let s = self.solicitud();
        serde_json::json!({
            "ine": {
                "clave_elector": s.ine.clave_elector, "cic": s.ine.cic, "emision": s.ine.emision,
                "vigencia": s.ine.vigencia, "frente": s.ine.frente, "reverso": s.ine.reverso,
            },
            "selfie": { "frames": s.selfie.frames, "reto": s.selfie.reto },
            "consentimiento_biometrico": true,
        })
    }
}

/// "Imagen" JPEG (cabecera válida, contenido distinto por semilla e índice).
pub fn imagen_jpeg(semilla: u32, indice: u32) -> String {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend(format!("{semilla}:{indice}:").bytes());
    bytes.resize(4096, (indice % 251) as u8);
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// SPEI entrante (como lo notificaría el proveedor) con una clave de rastreo única.
pub async fn depositar(pool: &PgPool, cfg: &Config, cuenta_id: Uuid, monto: &str) -> Movimiento {
    // Con un rol (la prueba de usuario sin privilegios usa este ayudante).
    let mut tx = db::begin_lectura(pool, db::Rol::Ifpe).await.unwrap();
    let clabe: String = sqlx::query_scalar("SELECT clabe FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let rastreo = format!("PRUEBA{}", &Uuid::new_v4().simple().to_string()[..20]);
    let r = inbound::abonar(
        pool,
        &cfg.pld,
        &cfg.spei,
        &abono(&clabe, monto, &rastreo),
        Origen::Webhook,
        SiNoAcreditable::Rechazar,
    )
    .await
    .unwrap();
    Movimiento { transaccion: r.transaccion.expect("abonado"), idempotente: r.idempotente }
}

/// SPEI recibido de BBVA (40012) con fecha de hoy.
pub fn abono(clabe: &str, monto: &str, rastreo: &str) -> inbound::Abono {
    inbound::Abono {
        identidad: IdentidadSpei::nueva("40012", chrono::Utc::now().date_naive(), rastreo).unwrap(),
        institucion_receptora: Some("90646".into()),
        receptor_clabe: clabe.into(),
        monto: d(monto),
        nombre_ordenante: Some("ORDENANTE DE PRUEBA".into()),
        cuenta_ordenante: Some("012180001234567899".into()),
        rfc_curp_ordenante: None,
        referencia_numerica: Some(1),
        concepto: Some("PRUEBA".into()),
        tipo_pago: Some(1),
        rastreo_devuelto: None,
        causa_devolucion: None,
        id_proveedor: None,
        raw_payload: serde_json::json!({ "prueba": true }),
    }
}

/// Hace vencer el arrendamiento y la espera de todas las órdenes (como dueño
/// del esquema), para que el siguiente ciclo del despachador las procese.
pub async fn vencer_ordenes(pool: &PgPool) {
    sqlx::query("UPDATE ifpe.spei_outbound SET proximo_intento_en = now() - interval '1 second' WHERE estado IN ('PENDIENTE', 'ENVIADA', 'LIQUIDADA')")
        .execute(pool)
        .await
        .unwrap();
}

/// Retiro (SPEI saliente a cuenta propia).
pub async fn retirar(
    pool: &PgPool,
    cfg: &Config,
    cliente_id: Uuid,
    monto: &str,
    clabe_destino: &str,
    concepto: Option<&str>,
    clave_idempotencia: &str,
) -> AppResult<outbound::ResultadoOrden> {
    outbound::solicitar(
        pool,
        &cfg.pld,
        &cfg.spei,
        &outbound::SolicitudSpei {
            cliente_id,
            clabe_destino: clabe_destino.into(),
            beneficiario_nombre: None,
            beneficiario_rfc_curp: None,
            monto: d(monto),
            concepto: concepto.map(Into::into),
            referencia_numerica: None,
            clave_idempotencia: clave_idempotencia.into(),
        },
    )
    .await
}

/// Simula el paso del tiempo para un mutuo: recorre `fecha_inicio` hacia
/// atrás. Solo el dueño del esquema puede hacerlo, desactivando el trigger
/// que protege los términos (la aplicación nunca puede).
pub async fn envejecer_mutuo(pool: &PgPool, contrato_id: Uuid, dias: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE sofom.contratos_mutuo DISABLE TRIGGER trg_mutuo_proteger")
        .execute(&mut *tx)
        .await
        .unwrap();
    let contrato: ContratoMutuo =
        sqlx::query_as("UPDATE sofom.contratos_mutuo SET fecha_inicio = fecha_inicio - $2 WHERE id = $1 RETURNING *")
            .bind(contrato_id)
            .bind(dias)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    // Los términos (y su hash) cambian con la fecha de inicio; sin esto la
    // aplicación detectaría, con razón, un contrato alterado.
    sqlx::query("UPDATE sofom.contratos_mutuo SET hash_terminos = $2 WHERE id = $1")
        .bind(contrato_id)
        .bind(puente::terminos_de(&contrato).unwrap().hash)
        .execute(&mut *tx)
        .await
        .unwrap();
    // Dispara ya los triggers diferidos: ALTER TABLE no admite eventos pendientes.
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE sofom.contratos_mutuo ENABLE TRIGGER trg_mutuo_proteger").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

/// Igual que [`envejecer_mutuo`], para la fecha de desembolso de un crédito.
pub async fn envejecer_credito(pool: &PgPool, credito_id: Uuid, dias: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE sofom.creditos DISABLE TRIGGER trg_credito_proteger").execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE sofom.creditos SET fecha_desembolso = fecha_desembolso - $2 WHERE id = $1")
        .bind(credito_id)
        .bind(dias)
        .execute(&mut *tx)
        .await
        .unwrap();
    // Dispara ya los triggers diferidos: ALTER TABLE no admite eventos pendientes.
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE sofom.creditos ENABLE TRIGGER trg_credito_proteger").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

pub fn clave() -> String {
    format!("prueba:{}", Uuid::new_v4())
}

pub async fn saldo_ifpe(pool: &PgPool, cuenta_id: Uuid) -> (Decimal, Decimal) {
    sqlx::query_as("SELECT saldo_disponible, saldo_bloqueado FROM ifpe.cuentas WHERE id = $1")
        .bind(cuenta_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Invariantes globales que deben cumplirse después de cualquier secuencia de operaciones.
pub async fn verificar_invariantes(pool: &PgPool) {
    // Regla 1: reserva del 100%, por tres caminos independientes.
    let (pasivo, activo): (Decimal, Decimal) =
        sqlx::query_as("SELECT pasivo_clientes, activo_reserva FROM ifpe.estado_reserva")
            .fetch_one(pool)
            .await
            .unwrap();
    let suma_cuentas: Decimal =
        sqlx::query_scalar("SELECT coalesce(sum(saldo_disponible + saldo_bloqueado), 0) FROM ifpe.cuentas")
            .fetch_one(pool)
            .await
            .unwrap();
    let suma_custodia: Decimal =
        sqlx::query_scalar("SELECT coalesce(sum(saldo), 0) FROM ifpe.reserva_custodia").fetch_one(pool).await.unwrap();
    assert_eq!(pasivo, activo, "pasivo IFPE != reserva");
    assert_eq!(pasivo, suma_cuentas, "estado_reserva.pasivo no cuadra con las cuentas");
    assert_eq!(activo, suma_custodia, "estado_reserva.activo no cuadra con la custodia");

    // Los saldos de cada cuenta IFPE se pueden reconstruir solo con el ledger.
    let descuadres: i64 = sqlx::query_scalar(
        "WITH efectos AS ( \
           SELECT cuenta_destino_id AS cuenta, \
                  CASE WHEN tipo IN ('RETENCION','LIBERACION_RETENCION') THEN 0 ELSE monto END AS delta \
             FROM ifpe.transacciones WHERE cuenta_destino_id IS NOT NULL \
           UNION ALL \
           SELECT cuenta_origen_id, \
                  CASE WHEN tipo IN ('RETENCION','LIBERACION_RETENCION') THEN 0 ELSE -monto END \
             FROM ifpe.transacciones WHERE cuenta_origen_id IS NOT NULL) \
         SELECT count(*) FROM ifpe.cuentas c \
          LEFT JOIN (SELECT cuenta, sum(delta) AS total FROM efectos GROUP BY cuenta) e ON e.cuenta = c.id \
         WHERE coalesce(e.total, 0) <> c.saldo_disponible + c.saldo_bloqueado",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(descuadres, 0, "hay cuentas IFPE cuyo saldo no se reconstruye desde el ledger");

    // Regla 2: el saldo bloqueado SOFOM es exactamente el principal de los mutuos vigentes,
    // y ningún mutuo está sobreasignado.
    let (bloqueado, vigente): (Decimal, Decimal) = sqlx::query_as(
        "SELECT (SELECT coalesce(sum(saldo_bloqueado), 0) FROM sofom.cuentas), \
                (SELECT coalesce(sum(principal), 0) FROM sofom.contratos_mutuo WHERE estado = 'VIGENTE')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(bloqueado, vigente, "saldo bloqueado SOFOM != principal de mutuos vigentes");
    let mal_calzados: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sofom.asignaciones_fondeo a \
         JOIN sofom.creditos c ON c.id = a.credito_id JOIN sofom.contratos_mutuo m ON m.id = a.contrato_id \
         WHERE m.fecha_vencimiento < c.fecha_vencimiento",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(mal_calzados, 0, "hay créditos fondeados con mutuos de menor plazo");

    // Cadenas de hash íntegras.
    let ifpe = hash::verificar_cadena_ifpe(pool).await.unwrap();
    assert!(ifpe.valido, "cadena IFPE inválida: {ifpe:?}");
    let sofom = hash::verificar_cadena_sofom(pool).await.unwrap();
    assert!(sofom.valido, "cadena SOFOM inválida: {sofom:?}");
}

/// Crea `n` cuentas IFPE vacías directamente (como dueño del esquema), sin
/// el costo de Argon2 del alta normal. Para pruebas de carga.
pub async fn cuentas_vacias(pool: &PgPool, cfg: &Config, n: usize) {
    for i in 0..n {
        let cliente: Uuid = sqlx::query_scalar(
            "INSERT INTO kyc.clientes (tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) \
             VALUES ('FISICA', 'Carga', 'Prueba', $1, 'VERIFICADO', now(), 'BAJO') RETURNING id",
        )
        .bind(format!("carga-{i}-{}@example.mx", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO ifpe.cuentas (cliente_id, clabe) VALUES ($1, $2)")
            .bind(cliente)
            .bind(synthetic_capital::spei::clabe::generar(&cfg.clabe_prefijo).to_string())
            .execute(pool)
            .await
            .unwrap();
    }
}
