//! Endurecimiento de la entrada: ningún cliente cuela campos de más
//! (asignación masiva) en un cuerpo JSON ni en una query.

mod comun;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use axum::{http::StatusCode, Router};
use comun::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use synthetic_capital::{routes, AppState};

/// Campos abiertos a propósito dentro de un tipo de petición (`Tipo.campo`) y
/// por qué. Cualquier otro `Value` o `Map` en una petición hace fallar la
/// prueba: aceptarlo es una decisión que se revisa.
const ABIERTOS: &[(&str, &str)] = &[(
    "SolicitudPropuesta.cambios",
    "cambios a una regla PLD: la base de datos valida cada nombre y tipo (aml.proponer_cambio, AML_CAMBIO_INVALIDO)",
)];

struct Definicion {
    archivo: String,
    niega: bool,
    es_struct: bool,
    campos: Vec<(String, String)>,
}

fn fuentes() -> Vec<(String, String)> {
    fn recorrer(dir: &Path, raiz: &Path, archivos: &mut Vec<(String, String)>) {
        for entrada in fs::read_dir(dir).unwrap() {
            let ruta = entrada.unwrap().path();
            if ruta.is_dir() {
                recorrer(&ruta, raiz, archivos);
            } else if ruta.extension().is_some_and(|e| e == "rs") {
                let relativa = ruta.strip_prefix(raiz).unwrap().display().to_string();
                archivos.push((relativa, fs::read_to_string(&ruta).unwrap()));
            }
        }
    }
    let raiz = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut archivos = Vec::new();
    recorrer(&raiz, &raiz, &mut archivos);
    archivos
}

/// Todo `struct`/`enum` con `#[derive(.. Deserialize ..)]`, por nombre: si
/// niega campos desconocidos y los campos (nombre, tipo) de un struct.
fn definiciones(archivos: &[(String, String)]) -> BTreeMap<String, Vec<Definicion>> {
    let mut mapa: BTreeMap<String, Vec<Definicion>> = BTreeMap::new();
    for (archivo, texto) in archivos {
        let lineas: Vec<&str> = texto.lines().collect();
        for (i, linea) in lineas.iter().enumerate() {
            let t = linea.trim();
            if !(t.starts_with("#[derive(") && t.contains("Deserialize")) {
                continue;
            }
            let mut niega = false;
            let mut j = i + 1;
            while j < lineas.len() {
                let t = lineas[j].trim();
                niega |= t.contains("deny_unknown_fields");
                if t.starts_with("#[") || t.starts_with("//") || t.is_empty() {
                    j += 1;
                } else {
                    break;
                }
            }
            let Some(item) =
                lineas.get(j).map(|l| l.trim().trim_start_matches("pub(crate) ").trim_start_matches("pub "))
            else {
                continue;
            };
            let (es_struct, resto) = match (item.strip_prefix("struct "), item.strip_prefix("enum ")) {
                (Some(r), _) => (true, r),
                (_, Some(r)) => (false, r),
                _ => continue,
            };
            let nombre: String = resto.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if nombre.is_empty() {
                continue; // enum generado por macro
            }
            let mut campos = Vec::new();
            if es_struct && item.ends_with('{') {
                for l in lineas[j + 1..].iter().map(|l| l.trim()).take_while(|l| *l != "}") {
                    if l.starts_with("//") || l.starts_with("#[") {
                        continue;
                    }
                    if let Some((campo, tipo)) = l.trim_start_matches("pub ").split_once(':') {
                        campos.push((campo.trim().to_string(), tipo.trim().trim_end_matches(',').to_string()));
                    }
                }
            }
            mapa.entry(nombre).or_default().push(Definicion { archivo: archivo.clone(), niega, es_struct, campos });
        }
    }
    mapa
}

/// Tipos que los handlers reciben con `Json<T>`, `Query<T>` o `Form<T>`.
fn tipos_de_peticion(archivos: &[(String, String)]) -> BTreeSet<String> {
    let mut tipos = BTreeSet::new();
    for (archivo, texto) in archivos.iter().filter(|(a, _)| a.starts_with("routes/")) {
        for extractor in ["): Json<", "): Query<", "): Form<"] {
            for (i, _) in texto.match_indices(extractor) {
                let tipo: String = texto[i + extractor.len()..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                    .collect();
                let nombre = tipo.rsplit("::").next().unwrap_or_default().to_string();
                assert!(!nombre.is_empty(), "{archivo}: extractor sin tipo legible");
                tipos.insert(nombre);
            }
        }
    }
    tipos
}

/// Todo tipo de petición, y todo struct propio anidado en él, lleva
/// `#[serde(deny_unknown_fields)]`: un campo inyectado (`"rol"`, `"saldo"`,
/// `"cliente_id"`...) da 400 en vez de ignorarse en silencio.
#[test]
fn todo_cuerpo_y_query_rechaza_campos_desconocidos() {
    let archivos = fuentes();
    let defs = definiciones(&archivos);
    let peticiones = tipos_de_peticion(&archivos);
    assert!(peticiones.len() >= 30, "el escaneo encontró solo {} tipos de petición: {peticiones:?}", peticiones.len());

    let mut pendientes: Vec<String> = peticiones.iter().cloned().collect();
    let mut vistos = BTreeSet::new();
    let mut abiertos = Vec::new();
    let mut sin_definicion = Vec::new();
    while let Some(nombre) = pendientes.pop() {
        if !vistos.insert(nombre.clone()) {
            continue;
        }
        let Some(lista) = defs.get(&nombre) else {
            sin_definicion.push(nombre);
            continue;
        };
        for d in lista.iter().filter(|d| d.es_struct) {
            if !d.niega {
                abiertos.push(format!("{nombre} ({}) no tiene #[serde(deny_unknown_fields)]", d.archivo));
            }
            for (campo, tipo) in &d.campos {
                if tipo.contains("Value") || tipo.contains("Map<") {
                    let clave = format!("{nombre}.{campo}");
                    if !ABIERTOS.iter().any(|(c, _)| *c == clave) {
                        abiertos.push(format!("{clave}: {tipo} acepta cualquier JSON"));
                    }
                    continue;
                }
                for palabra in tipo.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                    if palabra.starts_with(|c: char| c.is_uppercase()) && defs.contains_key(palabra) {
                        pendientes.push(palabra.to_string());
                    }
                }
            }
        }
    }
    assert!(sin_definicion.is_empty(), "tipos de petición sin definición encontrada: {sin_definicion:?}");
    assert!(abiertos.is_empty(), "peticiones que aceptan campos de más:\n{}", abiertos.join("\n"));
}

// -----------------------------------------------------------------------------
// Por HTTP
// -----------------------------------------------------------------------------

async fn app(pool: &PgPool) -> Router {
    let cfg = preparar(pool).await;
    routes::router(AppState::nuevo(pool.clone(), cfg))
}

fn registro(email: &str) -> Value {
    json!({
        "tipo_persona": "FISICA",
        "nombre": "Ana",
        "apellido_paterno": "Prueba",
        "email": email,
        "password": "contraseña-segura-123"
    })
}

fn rechazo_por_campo(s: StatusCode, v: &Value, campo: &str) {
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["codigo"], "VALIDACION", "{v}");
    let mensaje = v["error"]["mensaje"].as_str().unwrap_or_default();
    assert!(mensaje.contains(&format!("unknown field `{campo}`")), "{mensaje}");
}

#[sqlx::test(migrations = "../migrations")]
async fn el_registro_rechaza_un_rol_inyectado(pool: PgPool) {
    let app = app(&pool).await;
    let mut cuerpo = registro("mallory@example.mx");
    cuerpo["rol"] = json!("CUMPLIMIENTO");
    let (s, v) = llamar_json(&app, "POST", "/api/v1/auth/registro", None, Some(cuerpo)).await;
    rechazo_por_campo(s, &v, "rol");
    let existe: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kyc.clientes WHERE email = 'mallory@example.mx')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!existe, "el registro rechazado no debe crear la cuenta");

    // Sin el campo inyectado el mismo cuerpo sí registra, como cliente.
    let (s, v) = llamar_json(&app, "POST", "/api/v1/auth/registro", None, Some(registro("mallory@example.mx"))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["sesion"]["rol"], "CLIENTE", "{v}");
}

#[sqlx::test(migrations = "../migrations")]
async fn una_transferencia_rechaza_campos_inyectados(pool: PgPool) {
    let app = app(&pool).await;
    let (_, r) = llamar_json(&app, "POST", "/api/v1/auth/registro", None, Some(registro("ana@example.mx"))).await;
    let token = r["sesion"]["token"].as_str().unwrap().to_string();
    for (campo, valor) in
        [("cuenta_origen_id", json!(uuid::Uuid::new_v4())), ("comision", json!("0")), ("estado", json!("LIQUIDADA"))]
    {
        let mut cuerpo = json!({
            "clabe_destino": "002180700000000016",
            "monto": "100.00",
            "clave_idempotencia": clave()
        });
        cuerpo[campo] = valor;
        let (s, v) = llamar_json(&app, "POST", "/api/v1/ifpe/transferencias", Some(&token), Some(cuerpo)).await;
        rechazo_por_campo(s, &v, campo);
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn una_query_rechaza_parametros_inyectados(pool: PgPool) {
    let app = app(&pool).await;
    let (_, r) = llamar_json(&app, "POST", "/api/v1/auth/registro", None, Some(registro("ana@example.mx"))).await;
    let token = r["sesion"]["token"].as_str().unwrap().to_string();
    let (s, v) = llamar_json(&app, "GET", "/api/v1/ifpe/movimientos?limite=5", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ruta = format!("/api/v1/ifpe/movimientos?limite=5&cliente_id={}", uuid::Uuid::new_v4());
    let (s, v) = llamar_json(&app, "GET", &ruta, Some(&token), None).await;
    rechazo_por_campo(s, &v, "cliente_id");
}

#[sqlx::test(migrations = "../migrations")]
async fn la_verificacion_de_una_prueba_rechaza_campos_inyectados(pool: PgPool) {
    let app = app(&pool).await;
    let hoja = json!({ "cuenta_id": uuid::Uuid::new_v4(), "saldo": "1.00000000", "sal": "00", "hash": null });
    let cuerpo = |hoja: Value, ruta: Value| json!({ "corte_id": uuid::Uuid::new_v4(), "hoja": hoja, "ruta": ruta });

    let mut con_extra = hoja.clone();
    con_extra["saldo_real"] = json!("1000000.00000000");
    let (s, v) = llamar_json(&app, "POST", "/api/v1/reserva/verificar", None, Some(cuerpo(con_extra, json!([])))).await;
    rechazo_por_campo(s, &v, "saldo_real");

    let paso = json!({ "lado": "IZQUIERDA", "hash": "00", "suma": "0", "peso": 1 });
    let (s, v) = llamar_json(&app, "POST", "/api/v1/reserva/verificar", None, Some(cuerpo(hoja, json!([paso])))).await;
    rechazo_por_campo(s, &v, "peso");
}
