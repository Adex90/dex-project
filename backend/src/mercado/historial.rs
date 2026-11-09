//! Historial de precios simulado, mientras no haya oráculo ni historia guardada.
//!
//! Lo que la gráfica del frontend necesita para 1D a All: puntos cada 15
//! minutos el último día, cada hora la última semana y diarios hasta un año.
//!
//! - **Determinista**: el mismo activo, precio y cierre dan la misma serie en
//!   cada petición del mismo intervalo (el rendimiento de cada punto sale de
//!   una semilla con el ticker y el instante del punto, nunca de un azar).
//! - **Coherente con la cotización**: termina exactamente en el precio vigente
//!   y pasa por el cierre anterior exactamente 24 horas antes, así que el rango
//!   1D de la gráfica empieza donde lo dice `change_24h_pct`.
//! - **Sin punto flotante**: cada paso divide por (1 + r) con r en puntos base
//!   enteros, en decimales exactos, y redondea al centavo.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::dinero;

/// Un precio en un instante (`BalancePoint` en frontend/src/lib/portfolio.ts).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PuntoPrecio {
    /// Milisegundos desde epoch.
    #[serde(rename = "at")]
    pub en_ms: i64,
    #[serde(rename = "value")]
    pub valor: Decimal,
}

const DIA: i64 = 24 * 60 * 60;
/// El último día: (segundos entre puntos, volatilidad en puntos base).
const ULTIMO_DIA: (i64, i64) = (15 * 60, 6);
/// Antes del cierre anterior: (segundos entre puntos, puntos, volatilidad).
const ANTES: [(i64, usize, i64); 2] = [
    (60 * 60, 144, 18), // del día 1 al 7
    (DIA, 358, 90),     // del día 7 al 365
];

/// Serie del más antiguo al más reciente: el último punto es `(ahora, precio)`
/// y el de hace exactamente 24 horas, `cierre_previo`.
pub fn simular(ticker: &str, precio: Decimal, cierre_previo: Decimal, ahora: DateTime<Utc>) -> Vec<PuntoPrecio> {
    let fin = ahora.timestamp();
    let ayer = fin - DIA;
    let mut puntos: Vec<(i64, Decimal)> = vec![(fin, precio)];

    // Último día, hacia atrás desde el precio vigente. Una corrección lineal
    // en el tiempo lleva la caminata al cierre anterior, sin saltos.
    let (paso, volatilidad) = ULTIMO_DIA;
    let mut crudos = Vec::new();
    let mut valor = precio;
    let mut instante = (fin - 1).div_euclid(paso) * paso;
    while instante > ayer {
        valor = paso_atras(valor, rendimiento_bps(ticker, instante, volatilidad));
        crudos.push((instante, valor));
        instante -= paso;
    }
    let llegada = paso_atras(valor, rendimiento_bps(ticker, ayer, volatilidad));
    let ajuste = cierre_previo / llegada - Decimal::ONE;
    for (t, v) in &mut crudos {
        let avance = Decimal::from(fin - *t) / Decimal::from(DIA);
        *v = centavos(*v * (Decimal::ONE + ajuste * avance));
    }
    puntos.extend(crudos);
    puntos.push((ayer, cierre_previo));

    // La semana y el año, hacia atrás desde el cierre anterior.
    let mut valor = cierre_previo;
    let mut siguiente = ayer;
    for (paso, n, volatilidad) in ANTES {
        let mut instante = (siguiente - 1).div_euclid(paso) * paso;
        for _ in 0..n {
            valor = paso_atras(valor, rendimiento_bps(ticker, instante, volatilidad));
            puntos.push((instante, valor));
            siguiente = instante;
            instante -= paso;
        }
    }

    puntos.reverse();
    puntos.into_iter().map(|(s, v)| PuntoPrecio { en_ms: s * 1000, valor: dinero::con_escala(v) }).collect()
}

/// El precio un intervalo antes: deshace un rendimiento de `r` puntos base.
fn paso_atras(valor: Decimal, r: i64) -> Decimal {
    centavos(valor * Decimal::from(10_000) / Decimal::from(10_000 + r))
}

/// Al centavo, nunca menos de un centavo.
fn centavos(v: Decimal) -> Decimal {
    v.round_dp(2).max(Decimal::new(1, 2))
}

/// Rendimiento entero en [-volatilidad, volatilidad] puntos base para el
/// intervalo que termina en `instante`: FNV-1a del ticker mezclado con el
/// instante (SplitMix64).
fn rendimiento_bps(ticker: &str, instante: i64, volatilidad: i64) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in ticker.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let mut z = h ^ (instante as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    let ancho = u64::try_from(2 * volatilidad + 1).unwrap_or(1);
    i64::try_from(z % ancho).unwrap_or(0) - volatilidad
}

#[cfg(test)]
mod pruebas {
    use std::str::FromStr;

    use chrono::TimeZone;

    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn a_las(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, h, m, s).unwrap()
    }

    #[test]
    fn un_anio_ordenado_que_termina_en_el_precio_vigente() {
        let ahora = a_las(12, 7, 31);
        let serie = simular("AUG1", d("1612.40"), d("1598.97"), ahora);
        assert!((598..=601).contains(&serie.len()), "{} puntos", serie.len());
        assert!(serie.windows(2).all(|w| w[0].en_ms < w[1].en_ms), "instantes estrictamente crecientes");
        let ultimo = serie.last().unwrap();
        assert_eq!((ultimo.en_ms, ultimo.valor), (ahora.timestamp() * 1000, d("1612.40")));
        let dias = (ultimo.en_ms - serie[0].en_ms) / (DIA * 1000);
        assert!((364..=366).contains(&dias), "{dias} días");
        assert!(serie.iter().all(|p| p.valor > Decimal::ZERO && p.valor.scale() == 8));
    }

    #[test]
    fn hace_exactamente_un_dia_estaba_en_el_cierre_anterior() {
        // También con la hora alineada a los 15 minutos (el caso borde).
        for ahora in [a_las(12, 7, 31), a_las(12, 15, 0), a_las(0, 0, 0)] {
            let serie = simular("AUG1", d("1612.40"), d("1598.97"), ahora);
            let hace_un_dia = (ahora.timestamp() - DIA) * 1000;
            // El precio vigente hace un día: el último punto en o antes de ese instante.
            let punto = serie.iter().rev().find(|p| p.en_ms <= hace_un_dia).unwrap();
            assert_eq!((punto.en_ms, punto.valor), (hace_un_dia, d("1598.97")), "{ahora}");
        }
    }

    #[test]
    fn determinista_y_distinta_por_activo() {
        let ahora = a_las(12, 7, 31);
        let a = simular("AUG1", d("1612.40"), d("1598.97"), ahora);
        assert_eq!(a, simular("AUG1", d("1612.40"), d("1598.97"), ahora));
        let b = simular("AGT1", d("1612.40"), d("1598.97"), ahora);
        let n = a.len() - 1;
        assert_ne!(a[..n], b[..n]);
    }

    #[test]
    fn el_dia_no_salta_y_los_rendimientos_quedan_en_su_banda() {
        let serie = simular("AUG1", d("1612.40"), d("1598.97"), a_las(12, 7, 31));
        let dia = &serie[serie.len() - 98..];
        for w in dia.windows(2) {
            let cambio = ((w[1].valor - w[0].valor) / w[0].valor).abs();
            assert!(cambio < d("0.002"), "{} -> {}", w[0].valor, w[1].valor);
        }
        for instante in (0..10_000).map(|i| i * 900) {
            assert!(rendimiento_bps("AUG1", instante, 6).abs() <= 6);
        }
    }
}
