//! Resultado del proveedor biométrico -> dictamen automático (rol
//! `sc_kyc_verificador`).
//!
//! 1. Firma HMAC del cuerpo crudo (401 si no es del proveedor).
//! 2. El resultado debe ser del envío EN CURSO del expediente (la base de
//!    datos genera `verificacion_en_curso` en cada envío): un resultado viejo
//!    o de otro cliente responde 409. Repetir el mismo resultado es idempotente.
//! 3. [`evaluar`]: prueba de vida, documento, huellas de las imágenes, cotejo
//!    del OCR (CURP, clave de elector, fecha, sexo, MRZ, vigencia) y score
//!    contra los umbrales de `kyc.parametros`:
//!    - APROBADO: todo correcto, sin advertencias, score >= mínimo -> VERIFICADO.
//!    - REVISION: todo correcto pero score entre el umbral de revisión y el
//!      mínimo, o con advertencias -> sigue EN_REVISION para un operador.
//!    - RECHAZADO: cualquier error o score bajo el umbral de revisión ->
//!      PENDIENTE para reintentar, o RECHAZADO si agotó sus intentos.
//! 4. Bitácora inmutable (`kyc.kyc_auditoria_biometrica`) y transición en la
//!    misma transacción. La base de datos vuelve a exigir la evidencia.

use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    biometria::ResultadoProveedor,
    curp::Curp,
    expediente::{self, EstadoKyc, Evidencia, IdentidadPropia},
    ine, rfc,
};
use crate::{
    config::ConfigKyc,
    db::{self, Rol, Tx},
    error::{AppError, AppResult},
    spei::{self, firma},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Resultado {
    Aprobado,
    Revision,
    Rechazado,
}

impl Resultado {
    pub fn como_texto(self) -> &'static str {
        match self {
            Resultado::Aprobado => "APROBADO",
            Resultado::Revision => "REVISION",
            Resultado::Rechazado => "RECHAZADO",
        }
    }

    fn desde_texto(s: &str) -> Self {
        match s {
            "APROBADO" => Resultado::Aprobado,
            "REVISION" => Resultado::Revision,
            _ => Resultado::Rechazado,
        }
    }
}

/// Umbrales vigentes (`kyc.parametros`).
#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub struct Umbrales {
    pub umbral_score_minimo: Decimal,
    pub umbral_revision: Decimal,
    pub max_intentos: i16,
}

/// Una observación del dictamen (se guarda en `detalles_fallo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detalle {
    /// `ERROR` o `ADVERTENCIA`.
    pub tipo: String,
    pub mensaje: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Evaluacion {
    pub resultado: Resultado,
    pub score: Option<Decimal>,
    /// La credencial leída es la del cliente: el OCR coincide con lo
    /// declarado (CURP, clave de elector, nombre, fecha, sexo, MRZ), la
    /// credencial y el RFC son consistentes con la CURP, y el proveedor
    /// analizó las imágenes enviadas. No depende de la prueba de vida, del
    /// documento ni de la similitud facial (tienen su propia columna o su
    /// score).
    pub coincide_identidad: bool,
    pub errores: Vec<String>,
    pub advertencias: Vec<String>,
}

/// Hallazgos de similitud facial. Sin el score ni el umbral: el texto llega
/// al cliente (`detalles`), y conocer el número serviría para ajustar una
/// suplantación hasta pasar el umbral. Cumplimiento ve ambos en la bitácora.
pub const SIMILITUD_INSUFICIENTE: &str =
    "la similitud facial entre la selfie y la credencial no alcanza el mínimo aceptable";
pub const SIMILITUD_EN_REVISION: &str = "la similitud facial entre la selfie y la credencial requiere revisión humana";

impl Evaluacion {
    pub fn detalles(&self) -> Vec<Detalle> {
        let d = |tipo: &str, m: &String| Detalle { tipo: tipo.to_owned(), mensaje: m.clone() };
        self.errores
            .iter()
            .map(|m| d("ERROR", m))
            .chain(self.advertencias.iter().map(|m| d("ADVERTENCIA", m)))
            .collect()
    }
}

/// Dictamen sobre un resultado del proveedor (sin efectos).
pub fn evaluar(
    r: &ResultadoProveedor,
    identidad: &IdentidadPropia,
    evidencia_enviada: Option<&Evidencia>,
    u: &Umbrales,
    hoy: NaiveDate,
) -> Evaluacion {
    let (mut errores, mut advertencias) = (Vec::new(), Vec::new());
    let mut coincide_identidad = true;
    if evidencia_enviada != Some(&r.evidencia) {
        coincide_identidad = false;
        errores.push("las imágenes analizadas por el proveedor no son las que envió el cliente".to_owned());
    }
    if !r.prueba_vida.exitosa {
        errores.push(format!(
            "prueba de vida fallida: {}",
            r.prueba_vida.motivo.as_deref().unwrap_or("sin detalle del proveedor")
        ));
    }
    if !r.documento.valido {
        errores.push(format!(
            "credencial no válida: {}",
            r.documento.motivo.as_deref().unwrap_or("sin detalle del proveedor")
        ));
    }
    match &r.documento.ocr {
        None => {
            coincide_identidad = false;
            errores.push("el proveedor no entregó la lectura de la credencial".to_owned());
        }
        Some(ocr) => {
            let c = ine::cotejar(ocr, &identidad.como_declarada(), hoy);
            coincide_identidad &= c.errores.is_empty();
            errores.extend(c.errores);
            advertencias.extend(c.advertencias);
        }
    }
    match Curp::parse(&identidad.curp) {
        Err(e) => {
            coincide_identidad = false;
            errores.push(e.to_string());
        }
        Ok(curp) => {
            // Consistencia de la credencial con la CURP (fecha, sexo y entidad
            // de nacimiento en la clave de elector, vigencia).
            let (e, a) = ine::validar_credencial(&identidad.como_declarada().credencial, &curp, hoy);
            coincide_identidad &= e.is_empty();
            errores.extend(e);
            advertencias.extend(a);
            advertencias.extend(curp.advertencias_de_nombre(
                &identidad.nombres,
                &identidad.apellido_paterno,
                identidad.apellido_materno.as_deref(),
            ));
            let (e, a) = rfc::cotejar_con_curp(
                &identidad.rfc,
                &curp,
                &identidad.apellido_paterno,
                identidad.apellido_materno.as_deref(),
            );
            coincide_identidad &= e.is_empty();
            errores.extend(e);
            advertencias.extend(a);
        }
    }
    let score = match r.comparacion_facial.score {
        // Truncar (nunca redondear hacia arriba) a la escala de la columna.
        Some(s) if s >= Decimal::ZERO && s <= Decimal::ONE => {
            Some(s.round_dp_with_strategy(4, rust_decimal::RoundingStrategy::ToZero))
        }
        Some(_) => {
            errores.push("score de similitud fuera de rango".to_owned());
            None
        }
        None => None,
    };
    let resultado = match score {
        _ if !errores.is_empty() => Resultado::Rechazado,
        None => {
            errores.push("no se encontró un rostro comparable entre la selfie y la credencial".to_owned());
            Resultado::Rechazado
        }
        Some(s) if s < u.umbral_revision => {
            errores.push(SIMILITUD_INSUFICIENTE.to_owned());
            Resultado::Rechazado
        }
        Some(s) if s < u.umbral_score_minimo => {
            advertencias.push(SIMILITUD_EN_REVISION.to_owned());
            Resultado::Revision
        }
        Some(_) if !advertencias.is_empty() => Resultado::Revision,
        Some(_) => Resultado::Aprobado,
    };
    Evaluacion { resultado, score, coincide_identidad, errores, advertencias }
}

/// Lo que responde `POST /kyc/verificar` y el webhook.
#[derive(Debug, Clone, Serialize)]
pub struct ResultadoVerificacion {
    pub cliente_id: Uuid,
    pub verificacion_id: Uuid,
    pub resultado: Resultado,
    pub estado_kyc: EstadoKyc,
    pub score_similitud: Option<Decimal>,
    pub detalles: Vec<Detalle>,
    pub intentos: i16,
    pub intentos_restantes: i16,
    /// El mismo resultado ya se había procesado.
    pub idempotente: bool,
}

pub(crate) async fn umbrales(tx: &mut Tx) -> AppResult<Umbrales> {
    Ok(sqlx::query_as("SELECT umbral_score_minimo, umbral_revision, max_intentos FROM kyc.parametros WHERE id")
        .fetch_one(&mut **tx)
        .await?)
}

/// Verifica la firma y procesa un resultado del proveedor.
pub async fn procesar(
    pool: &PgPool,
    cfg: &ConfigKyc,
    cabecera: Option<&str>,
    cuerpo: &[u8],
) -> AppResult<ResultadoVerificacion> {
    let secretos: Vec<&[u8]> = cfg.webhook_secretos.iter().map(|s| s.as_bytes()).collect();
    firma::verificar(&secretos, cabecera, cuerpo, Utc::now().timestamp(), cfg.tolerancia_firma_seg).map_err(|e| {
        tracing::warn!(error = %e, "resultado biométrico rechazado por firma");
        AppError::peticion(axum::http::StatusCode::UNAUTHORIZED, "FIRMA_INVALIDA", e.to_string())
    })?;
    let crudo: serde_json::Value = serde_json::from_slice(cuerpo)
        .map_err(|e| AppError::validacion(format!("el resultado del proveedor no es JSON: {e}")))?;
    // Del texto, no del Value: el score se lee con todos sus dígitos.
    let r: ResultadoProveedor = serde_json::from_slice(cuerpo)
        .map_err(|e| AppError::validacion(format!("resultado del proveedor inválido: {e}")))?;
    let firma = cabecera.unwrap_or_default().chars().take(512).collect::<String>();

    let mut tx = db::begin(pool, Rol::KycVerificador).await?;
    let e: expediente::Expediente = sqlx::query_as(&format!(
        "SELECT {} FROM kyc.kyc_perfiles WHERE cliente_id = $1 FOR UPDATE",
        expediente::COLUMNAS
    ))
    .bind(r.cliente_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::no_encontrado("expediente KYC no encontrado"))?;
    let u = umbrales(&mut tx).await?;

    // Repetición del mismo resultado (el proveedor reintenta su webhook).
    let previo: Option<(String, serde_json::Value, Option<Decimal>, serde_json::Value)> = sqlx::query_as(
        "SELECT resultado, resultado_crudo, score_similitud, detalles_fallo \
         FROM kyc.kyc_auditoria_biometrica WHERE verificacion_id = $1",
    )
    .bind(r.verificacion_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((resultado, resultado_crudo, score, detalles)) = previo {
        if resultado_crudo != crudo {
            return Err(AppError::conflicto(
                "KYC_RESULTADO_DISTINTO",
                "ya se recibió otro resultado para esa verificación",
            ));
        }
        tx.commit().await?;
        return Ok(ResultadoVerificacion {
            cliente_id: e.cliente_id,
            verificacion_id: r.verificacion_id,
            resultado: Resultado::desde_texto(&resultado),
            estado_kyc: e.estado_kyc,
            score_similitud: score,
            detalles: serde_json::from_value(detalles).unwrap_or_default(),
            intentos: e.intentos,
            intentos_restantes: (u.max_intentos - e.intentos).max(0),
            idempotente: true,
        });
    }
    if e.estado_kyc != EstadoKyc::EnRevision || e.verificacion_en_curso != Some(r.verificacion_id) {
        return Err(AppError::conflicto(
            "KYC_VERIFICACION_AJENA",
            "el resultado no corresponde al envío vigente del expediente",
        ));
    }

    let identidad = IdentidadPropia::de(&e)?;
    let enviada: Option<Evidencia> = serde_json::from_value(e.evidencia.clone()).ok();
    let ev = evaluar(&r, &identidad, enviada.as_ref(), &u, spei::hoy_cdmx());
    let detalles = ev.detalles();
    let ocr = r.documento.ocr.as_ref();
    let corto = |s: &str| Some(s.trim().to_uppercase()).filter(|v| v.len() <= 18);
    sqlx::query(
        "INSERT INTO kyc.kyc_auditoria_biometrica \
         (cliente_id, verificacion_id, proveedor, referencia_proveedor, score_similitud, prueba_vida_exitosa, \
          documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, detalles_fallo, firma, \
          resultado_crudo) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(e.cliente_id)
    .bind(r.verificacion_id)
    .bind(r.proveedor.chars().take(64).collect::<String>())
    .bind(r.referencia.chars().take(128).collect::<String>())
    .bind(ev.score)
    .bind(r.prueba_vida.exitosa)
    .bind(r.documento.valido)
    .bind(ocr.and_then(|o| corto(&o.curp)))
    .bind(ocr.and_then(|o| corto(&o.clave_elector)))
    .bind(ev.coincide_identidad)
    .bind(ev.resultado.como_texto())
    .bind(serde_json::to_value(&detalles).map_err(|err| AppError::Interno(err.into()))?)
    .bind(&firma)
    .bind(&crudo)
    .execute(&mut *tx)
    .await?;

    let metadata = json!({
        "proveedor": r.proveedor,
        "referencia": r.referencia,
        "verificacion_id": r.verificacion_id,
        "resultado": ev.resultado,
        "score_similitud": ev.score,
        "evaluado_en": Utc::now(),
    });
    let agotado = e.intentos >= u.max_intentos;
    let (estado_nuevo, motivo): (Option<EstadoKyc>, Option<String>) = match ev.resultado {
        Resultado::Aprobado => (Some(EstadoKyc::Verificado), None),
        Resultado::Revision => (None, None),
        Resultado::Rechazado if agotado => (
            Some(EstadoKyc::Rechazado),
            Some(
                format!("intentos agotados: {}", ev.errores.first().map_or("", String::as_str))
                    .chars()
                    .take(500)
                    .collect(),
            ),
        ),
        Resultado::Rechazado => (Some(EstadoKyc::Pendiente), None),
    };
    let (estado_kyc, intentos): (EstadoKyc, i16) = sqlx::query_as(
        "UPDATE kyc.kyc_perfiles SET \
           estado_kyc = coalesce($2::kyc.estado_kyc, estado_kyc), \
           motivo_rechazo = CASE WHEN $2 = 'RECHAZADO' THEN $3 ELSE motivo_rechazo END, \
           metadata_proveedor = metadata_proveedor || jsonb_build_object('ultimo_resultado', $4::jsonb) \
         WHERE cliente_id = $1 RETURNING estado_kyc::text, intentos",
    )
    .bind(e.cliente_id)
    .bind(estado_nuevo.map(EstadoKyc::como_texto))
    .bind(motivo)
    .bind(metadata)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(
        cliente = %e.cliente_id,
        verificacion = %r.verificacion_id,
        resultado = ev.resultado.como_texto(),
        estado = estado_kyc.como_texto(),
        "resultado biométrico procesado"
    );
    Ok(ResultadoVerificacion {
        cliente_id: e.cliente_id,
        verificacion_id: r.verificacion_id,
        resultado: ev.resultado,
        estado_kyc,
        score_similitud: ev.score,
        detalles,
        intentos,
        intentos_restantes: (u.max_intentos - intentos).max(0),
        idempotente: false,
    })
}

/// El proveedor falló sin evaluar el envío (error, tiempo agotado o un
/// resultado que no se pudo procesar): el expediente vuelve a PENDIENTE y la
/// base de datos devuelve el intento, solo si ese envío no tiene resultado.
/// Si el expediente ya cambió (otro proceso registró el resultado), no hace nada.
pub async fn reembolsar_intento(
    pool: &PgPool,
    cliente_id: Uuid,
    verificacion_id: Uuid,
    motivo: &str,
) -> AppResult<bool> {
    let mut tx = db::begin(pool, Rol::KycVerificador).await?;
    sqlx::query("SELECT set_config('sc.reembolsar_intento', 'si', true)").execute(&mut *tx).await?;
    let n = sqlx::query(
        "UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE', \
                metadata_proveedor = metadata_proveedor || jsonb_build_object('ultima_falla_proveedor', $3::text) \
         WHERE cliente_id = $1 AND estado_kyc = 'EN_REVISION' AND verificacion_en_curso = $2 \
           AND NOT EXISTS (SELECT 1 FROM kyc.kyc_auditoria_biometrica WHERE verificacion_id = $2)",
    )
    .bind(cliente_id)
    .bind(verificacion_id)
    .bind(motivo.chars().take(300).collect::<String>())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    if n > 0 {
        tracing::warn!(cliente = %cliente_id, verificacion = %verificacion_id, %motivo, "falla del proveedor biométrico: intento devuelto");
    }
    Ok(n > 0)
}

#[cfg(test)]
mod pruebas {
    use super::*;
    use crate::kyc::{
        biometria::{ComparacionFacial, Documento, PruebaVida},
        curp::{self, Sexo},
    };

    fn identidad() -> IdentidadPropia {
        let fecha = NaiveDate::from_ymd_opt(1990, 7, 15).unwrap();
        let curp = curp::construir("Ludwig", "Von Mises", None, fecha, Sexo::H, "DF", 3);
        IdentidadPropia {
            nombres: "Ludwig".into(),
            apellido_paterno: "Von Mises".into(),
            apellido_materno: None,
            rfc: rfc::construir_desde_curp(&curp, "AB"),
            clave_elector: ine::construir_clave_elector("Ludwig", "Von Mises", None, fecha, 9, Sexo::H, 1),
            curp,
            fecha_nacimiento: fecha,
            sexo: Sexo::H,
            cic: "123456789".into(),
            identificador_ciudadano: None,
            emision: 2022,
            vigencia: 2032,
            codigo_postal: Some("06000".into()),
        }
    }

    fn evidencia() -> Evidencia {
        Evidencia { ine_frente: "a".repeat(64), ine_reverso: "b".repeat(64), selfie: vec!["c".repeat(64); 3] }
    }

    fn resultado(score: Option<Decimal>) -> ResultadoProveedor {
        let id = identidad();
        ResultadoProveedor {
            verificacion_id: Uuid::new_v4(),
            cliente_id: Uuid::new_v4(),
            proveedor: "simulado".into(),
            referencia: "r".into(),
            emitido_en: Utc::now(),
            prueba_vida: PruebaVida { exitosa: true, motivo: None },
            comparacion_facial: ComparacionFacial { score },
            documento: Documento { valido: true, motivo: None, ocr: Some(ine::ocr_de(&id.como_declarada())) },
            evidencia: evidencia(),
        }
    }

    const U: Umbrales = Umbrales {
        umbral_score_minimo: Decimal::from_parts(85, 0, 0, false, 2),
        umbral_revision: Decimal::from_parts(70, 0, 0, false, 2),
        max_intentos: 3,
    };

    fn hoy() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 13).unwrap()
    }

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn aprueba_revisa_o_rechaza_segun_el_score() {
        let id = identidad();
        let ev = |s: Option<&str>| evaluar(&resultado(s.map(d)), &id, Some(&evidencia()), &U, hoy());
        assert_eq!(ev(Some("0.93")).resultado, Resultado::Aprobado);
        assert_eq!(ev(Some("0.93")).score, Some(d("0.9300")));
        assert_eq!(ev(Some("0.85")).resultado, Resultado::Aprobado);
        assert_eq!(ev(Some("0.8499")).resultado, Resultado::Revision);
        // Justo debajo del umbral: se trunca, nunca se redondea hacia arriba.
        assert_eq!(ev(Some("0.8499999999999998")).resultado, Resultado::Revision);
        assert_eq!(ev(Some("0.70")).resultado, Resultado::Revision);
        assert_eq!(ev(Some("0.6999")).resultado, Resultado::Rechazado);
        assert_eq!(ev(None).resultado, Resultado::Rechazado);
        assert_eq!(ev(Some("1.5")).resultado, Resultado::Rechazado);
        assert_eq!(ev(Some("-0.1")).resultado, Resultado::Rechazado);
    }

    #[test]
    fn el_score_se_lee_del_texto_exacto() {
        let mut v = serde_json::to_value(resultado(Some(d("0.93")))).unwrap();
        v["comparacion_facial"]["score"] = serde_json::json!(0.93);
        let texto = serde_json::to_string(&v).unwrap().replace("0.93", "0.8499999999999998");
        let r: ResultadoProveedor = serde_json::from_str(&texto).unwrap();
        assert_eq!(r.comparacion_facial.score, Some(d("0.8499999999999998")));
        let r: ResultadoProveedor = serde_json::from_str(&texto.replace("0.8499999999999998", "8.5e-1")).unwrap();
        assert_eq!(r.comparacion_facial.score, Some(d("0.85")));
        assert!(serde_json::from_str::<ResultadoProveedor>(&texto.replace("0.8499999999999998", "\"0.9\"")).is_err());
    }

    #[test]
    fn entidad_de_la_clave_distinta_va_a_revision() {
        let mut id = identidad();
        // Misma fecha y sexo, entidad 14 (Jalisco) contra CURP de la CDMX.
        id.clave_elector =
            ine::construir_clave_elector("Ludwig", "Von Mises", None, id.fecha_nacimiento, 14, Sexo::H, 1);
        let mut r = resultado(Some(d("0.99")));
        r.documento.ocr = Some(ine::ocr_de(&id.como_declarada()));
        let e = evaluar(&r, &id, Some(&evidencia()), &U, hoy());
        assert_eq!(e.resultado, Resultado::Revision, "{e:?}");
    }

    #[test]
    fn rechaza_sin_prueba_de_vida_documento_o_evidencia() {
        let id = identidad();
        let mut r = resultado(Some(d("0.99")));
        r.prueba_vida = PruebaVida { exitosa: false, motivo: Some("foto de una pantalla".into()) };
        assert_eq!(evaluar(&r, &id, Some(&evidencia()), &U, hoy()).resultado, Resultado::Rechazado);

        let mut r = resultado(Some(d("0.99")));
        r.documento.valido = false;
        assert_eq!(evaluar(&r, &id, Some(&evidencia()), &U, hoy()).resultado, Resultado::Rechazado);

        let r = resultado(Some(d("0.99")));
        let mut otra = evidencia();
        otra.selfie[1] = "d".repeat(64);
        assert_eq!(evaluar(&r, &id, Some(&otra), &U, hoy()).resultado, Resultado::Rechazado);
        assert_eq!(evaluar(&r, &id, None, &U, hoy()).resultado, Resultado::Rechazado);

        let mut r = resultado(Some(d("0.99")));
        r.documento.ocr.as_mut().unwrap().curp = "LOOA531113HTCPBN07".into();
        let e = evaluar(&r, &id, Some(&evidencia()), &U, hoy());
        assert_eq!(e.resultado, Resultado::Rechazado);
        assert!(e.errores.iter().any(|m| m.contains("CURP")), "{e:?}");
    }

    #[test]
    fn advertencias_van_a_revision() {
        let id = identidad();
        let mut r = resultado(Some(d("0.99")));
        r.documento.ocr.as_mut().unwrap().nombres = "Luis".into();
        let e = evaluar(&r, &id, Some(&evidencia()), &U, hoy());
        assert_eq!(e.resultado, Resultado::Revision, "{e:?}");
        assert!(e.errores.is_empty());
    }

    #[test]
    fn los_hallazgos_no_revelan_score_ni_umbral() {
        let id = identidad();
        for (score, resultado_esperado, mensaje) in [
            (Some("0.42"), Resultado::Rechazado, SIMILITUD_INSUFICIENTE),
            (Some("0.7812"), Resultado::Revision, SIMILITUD_EN_REVISION),
        ] {
            let e = evaluar(&resultado(score.map(d)), &id, Some(&evidencia()), &U, hoy());
            assert_eq!(e.resultado, resultado_esperado);
            let detalles = e.detalles();
            assert!(detalles.iter().any(|x| x.mensaje == mensaje), "{e:?}");
            for x in &detalles {
                assert!(!x.mensaje.chars().any(|c| c.is_ascii_digit()), "un hallazgo con números: {}", x.mensaje);
            }
        }
    }

    #[test]
    fn coincide_identidad_no_depende_del_rostro_ni_de_la_prueba_de_vida() {
        let id = identidad();
        let coincide =
            |r: &ResultadoProveedor, ev: Option<&Evidencia>| evaluar(r, &id, ev, &U, hoy()).coincide_identidad;
        // Solo falla el rostro (score bajo o sin rostro): la identidad sí coincide.
        assert!(coincide(&resultado(Some(d("0.42"))), Some(&evidencia())));
        assert!(coincide(&resultado(None), Some(&evidencia())));
        let mut r = resultado(Some(d("0.99")));
        r.prueba_vida = PruebaVida { exitosa: false, motivo: None };
        assert!(coincide(&r, Some(&evidencia())));
        // La credencial no es la declarada, falta la lectura o se analizaron otras imágenes: no.
        let mut r = resultado(Some(d("0.99")));
        r.documento.ocr.as_mut().unwrap().curp = "LOOA531113HTCPBN07".into();
        assert!(!coincide(&r, Some(&evidencia())));
        let mut r = resultado(Some(d("0.99")));
        r.documento.ocr = None;
        assert!(!coincide(&r, Some(&evidencia())));
        assert!(!coincide(&resultado(Some(d("0.99"))), None));
        assert!(coincide(&resultado(Some(d("0.99"))), Some(&evidencia())));
    }
}
