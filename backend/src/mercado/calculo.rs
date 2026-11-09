//! Aritmética exacta de una orden de mercado, en enteros de 10^-8 (`i128`).
//!
//! La base de datos aplica las mismas reglas con `NUMERIC` (trigger
//! `mercado.fn_validar_orden`, migración 0013) y rechaza cualquier diferencia;
//! este módulo existe para elegir la cantidad antes de escribir y responder con
//! un error claro. Ningún paso usa punto flotante, y los productos caben en
//! `i128`: un monto o precio de `NUMERIC(18, 8)` es como mucho 10^18 unidades,
//! y un producto de dos, 10^36 (el máximo de `i128` es 1.7 × 10^38).
//!
//! - **Compra**: todo lo que el monto compra en el incremento del activo
//!   (10^-decimales): `cantidad = piso(monto / precio)`. Se cobra
//!   `cantidad × precio` al centavo (mitad hacia arriba), que nunca excede el
//!   monto; el resto se queda en efectivo.
//! - **Venta**: un monto que excede, al centavo hacia arriba, lo que vale la
//!   posición se rechaza. Uno que llega, al centavo hacia abajo, a lo que vale
//!   la vende completa (el botón «Max» del frontend, sin residuos). Si no, la
//!   cantidad que cubre el monto, como en la compra. Se abona
//!   `cantidad × precio` al centavo.
//! - **Costo**: la compra suma su importe al costo de la posición; la venta
//!   libera `costo × vendido / tenido` (costo promedio) al 10^-8, mitad hacia
//!   arriba, o todo el costo si vende todo.

use rust_decimal::Decimal;

use crate::dinero;

/// 10^8: unidades de 10^-8 en una unidad.
const UNIDAD: i128 = 100_000_000;
/// Unidades de 10^-16 (producto de dos montos) en un centavo.
const CENTAVO_16: i128 = 100_000_000_000_000;
/// Unidades de 10^-8 en un centavo.
const CENTAVO_8: i128 = 1_000_000;
/// El máximo de `NUMERIC(18, 8)` en unidades de 10^-8.
const MAXIMO: i128 = 999_999_999_999_999_999;

/// Lo que ejecuta una orden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ejecucion {
    /// Unidades del activo (múltiplo del incremento).
    pub cantidad: Decimal,
    /// Efectivo que se carga (compra) o abona (venta), al centavo.
    pub monto: Decimal,
    /// Venta: costo de las unidades vendidas. `None` en compras.
    pub costo_liberado: Option<Decimal>,
}

/// Por qué no se puede ejecutar una orden con esos números.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rechazo {
    /// El monto no alcanza para un incremento del activo (o su importe es 0.00).
    MontoInsuficiente,
    /// El cliente no tiene ese activo.
    SinPosicion,
    /// El monto excede lo que vale la posición.
    PosicionInsuficiente,
    /// Un número fuera de `NUMERIC(18, 8)` (no debería llegar aquí: los montos
    /// se validan antes y los demás vienen de la base de datos).
    FueraDeRango,
}

/// Unidades de 10^-8 de un decimal con a lo más 8 decimales (los de la base
/// de datos y los montos ya validados).
fn unidades(d: Decimal) -> Result<i128, Rechazo> {
    if d.normalize().scale() > dinero::ESCALA {
        return Err(Rechazo::FueraDeRango);
    }
    Ok(dinero::con_escala(d).mantissa())
}

/// Un monto de orden: pesos con centavos exactos, dentro de `NUMERIC(18, 8)`.
fn centavos_exactos(d: Decimal) -> Result<i128, Rechazo> {
    let u = unidades(d)?;
    if u <= 0 || u % CENTAVO_8 != 0 {
        return Err(Rechazo::FueraDeRango);
    }
    Ok(u)
}

/// De unidades a decimal, solo si cabe en `NUMERIC(18, 8)` (si no, la base de
/// datos rechazaría la orden).
fn decimal(u: i128) -> Result<Decimal, Rechazo> {
    if !(0..=MAXIMO).contains(&u) {
        return Err(Rechazo::FueraDeRango);
    }
    Decimal::try_from_i128_with_scale(u, dinero::ESCALA).map_err(|_| Rechazo::FueraDeRango)
}

fn por(a: i128, b: i128) -> Result<i128, Rechazo> {
    a.checked_mul(b).ok_or(Rechazo::FueraDeRango)
}

/// 10^-decimales en unidades de 10^-8, y 10^decimales.
fn incremento(decimales: u32) -> Result<(i128, i128), Rechazo> {
    if decimales > dinero::ESCALA {
        return Err(Rechazo::FueraDeRango);
    }
    Ok((10_i128.pow(dinero::ESCALA - decimales), 10_i128.pow(decimales)))
}

/// La mayor cantidad (en el incremento) cuyo costo no excede `monto`.
fn cantidad_que_cubre(monto: i128, precio: i128, decimales: u32) -> Result<i128, Rechazo> {
    let (paso, escala) = incremento(decimales)?;
    // piso(monto / precio) en incrementos: monto × 10^d / precio (enteros).
    let pasos = por(monto, escala)? / precio;
    por(pasos, paso)
}

/// `cantidad × precio` al centavo, mitad hacia arriba, en unidades de 10^-8.
fn importe(cantidad: i128, precio: i128) -> Result<i128, Rechazo> {
    let producto = por(cantidad, precio)?; // 10^-16
    let centavos = (producto + CENTAVO_16 / 2) / CENTAVO_16;
    por(centavos, CENTAVO_8)
}

/// Compra con `monto` pesos (centavos exactos) a `precio`.
pub fn compra(monto: Decimal, precio: Decimal, decimales: u32) -> Result<Ejecucion, Rechazo> {
    let (m, p) = (centavos_exactos(monto)?, unidades(precio)?);
    if p <= 0 {
        return Err(Rechazo::FueraDeRango);
    }
    let cantidad = cantidad_que_cubre(m, p, decimales)?;
    let cobro = importe(cantidad, p)?;
    if cantidad == 0 || cobro == 0 {
        return Err(Rechazo::MontoInsuficiente);
    }
    // cantidad × precio <= monto, y el monto es un centavo exacto: redondeado
    // al centavo, el cobro no lo excede. Se comprueba de todos modos.
    if cobro > m {
        return Err(Rechazo::FueraDeRango);
    }
    Ok(Ejecucion { cantidad: decimal(cantidad)?, monto: decimal(cobro)?, costo_liberado: None })
}

/// Venta de `monto` pesos de una posición de `tenido` unidades que costó `costo`.
pub fn venta(
    monto: Decimal,
    precio: Decimal,
    decimales: u32,
    tenido: Decimal,
    costo: Decimal,
) -> Result<Ejecucion, Rechazo> {
    let (m, p, t, c) = (centavos_exactos(monto)?, unidades(precio)?, unidades(tenido)?, unidades(costo)?);
    if p <= 0 || t < 0 || c < 0 {
        return Err(Rechazo::FueraDeRango);
    }
    if t == 0 {
        return Err(Rechazo::SinPosicion);
    }
    let valor = por(t, p)?; // 10^-16
    let piso = valor / CENTAVO_16;
    let techo = (valor + CENTAVO_16 - 1) / CENTAVO_16;
    let pedido = m / CENTAVO_8; // centavos exactos
    if pedido > techo {
        return Err(Rechazo::PosicionInsuficiente);
    }
    let cantidad = if pedido >= piso { t } else { cantidad_que_cubre(m, p, decimales)? };
    if cantidad == 0 {
        return Err(Rechazo::MontoInsuficiente);
    }
    let liberado = costo_liberado(c, cantidad, t)?;
    let abono = importe(cantidad, p)?;
    if abono == 0 {
        return Err(Rechazo::MontoInsuficiente);
    }
    Ok(Ejecucion { cantidad: decimal(cantidad)?, monto: decimal(abono)?, costo_liberado: Some(decimal(liberado)?) })
}

/// Costo promedio de `vendido` unidades de una posición de `tenido` que costó
/// `costo`: todo el costo si se vende todo; si no, `costo × vendido / tenido`
/// a 10^-8, mitad hacia arriba: piso((2·costo·vendido + tenido) / (2·tenido)).
fn costo_liberado(costo: i128, vendido: i128, tenido: i128) -> Result<i128, Rechazo> {
    if tenido <= 0 || vendido <= 0 || vendido > tenido {
        return Err(Rechazo::PosicionInsuficiente);
    }
    if vendido == tenido {
        return Ok(costo);
    }
    Ok((por(2 * costo, vendido)? + tenido) / (2 * tenido))
}

/// [`costo_liberado`] en decimales: el verificador del ledger lo recalcula
/// con la posición reproducida antes de cada venta.
pub fn costo_de_venta(costo: Decimal, vendido: Decimal, tenido: Decimal) -> Result<Decimal, Rechazo> {
    decimal(costo_liberado(unidades(costo)?, unidades(vendido)?, unidades(tenido)?)?)
}

/// `cantidad × precio` truncado a 10^-8: el valor de una posición en la bóveda.
pub fn valor(cantidad: Decimal, precio: Decimal) -> Decimal {
    let calcular = || -> Result<Decimal, Rechazo> { decimal(por(unidades(cantidad)?, unidades(precio)?)? / UNIDAD) };
    calcular().unwrap_or(Decimal::ZERO)
}

/// Variación porcentual de `desde` a `hasta`, a 2 decimales (solo para mostrar).
pub fn cambio_pct(hasta: Decimal, desde: Decimal) -> Decimal {
    if desde.is_zero() {
        return Decimal::new(0, 2);
    }
    let mut v = ((hasta - desde) * Decimal::ONE_HUNDRED / desde).round_dp(2);
    v.rescale(2);
    v
}

#[cfg(test)]
mod pruebas {
    use std::str::FromStr;

    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn ejecucion(cantidad: &str, monto: &str, costo: Option<&str>) -> Ejecucion {
        Ejecucion { cantidad: d(cantidad), monto: d(monto), costo_liberado: costo.map(d) }
    }

    // Los mismos números que tests/db/0013_mercado_valores.sql.
    const ORO: &str = "1612.40";

    #[test]
    fn compra_todo_lo_que_cubre_el_monto_y_cobra_al_centavo() {
        assert_eq!(compra(d("1250"), d(ORO), 4), Ok(ejecucion("0.7752", "1249.93", None)));
        // Un incremento más costaría más de lo pedido.
        assert!(d("0.7753") * d(ORO) > d("1250"));
        assert_eq!(compra(d("100"), d(ORO), 4), Ok(ejecucion("0.0620", "99.97", None)));
        // Sin decimales: certificados enteros.
        assert_eq!(compra(d("10"), d("9.92"), 0), Ok(ejecucion("1", "9.92", None)));
        assert_eq!(compra(d("19.84"), d("9.92"), 0), Ok(ejecucion("2", "19.84", None)), "exacto");
    }

    #[test]
    fn el_cobro_nunca_excede_el_monto() {
        for monto in ["0.17", "1", "99.99", "1250", "1250.01", "15000", "999999999.99"] {
            let e = compra(d(monto), d(ORO), 4).unwrap();
            assert!(e.monto <= d(monto), "{monto}: cobro {}", e.monto);
            assert!(e.cantidad * d(ORO) <= d(monto));
            assert!((e.cantidad + d("0.0001")) * d(ORO) > d(monto));
        }
    }

    #[test]
    fn un_monto_que_no_alcanza_un_incremento() {
        assert_eq!(compra(d("0.16"), d(ORO), 4), Err(Rechazo::MontoInsuficiente));
        assert_eq!(compra(d("9.91"), d("9.92"), 0), Err(Rechazo::MontoInsuficiente));
    }

    #[test]
    fn venta_parcial_libera_el_costo_promedio() {
        // Posición de la compra de 1 250: 0.7752 que costaron 1 249.93.
        assert_eq!(
            venta(d("500"), d(ORO), 4, d("0.7752"), d("1249.93")),
            Ok(ejecucion("0.3100", "499.84", Some("499.84300826")))
        );
    }

    #[test]
    fn lo_que_vale_la_posicion_la_vende_completa() {
        // Quedan 0.4652 (costo 750.08699174), que valen 750.088480.
        let todo = ejecucion("0.4652", "750.09", Some("750.08699174"));
        assert_eq!(venta(d("750.08"), d(ORO), 4, d("0.4652"), d("750.08699174")), Ok(todo), "piso");
        assert_eq!(venta(d("750.09"), d(ORO), 4, d("0.4652"), d("750.08699174")), Ok(todo), "techo");
        assert_eq!(
            venta(d("750.10"), d(ORO), 4, d("0.4652"), d("750.08699174")),
            Err(Rechazo::PosicionInsuficiente),
            "un centavo de más"
        );
        // Al mismo precio, comprar y vender todo devuelve exactamente lo pagado.
        assert_eq!(d("499.84") + d("750.09"), d("1249.93"));
    }

    #[test]
    fn el_costo_promedio_por_separado() {
        assert_eq!(costo_de_venta(d("1249.93"), d("0.3100"), d("0.7752")), Ok(d("499.84300826")));
        assert_eq!(costo_de_venta(d("750.08699174"), d("0.4652"), d("0.4652")), Ok(d("750.08699174")), "todo");
        // Mitad hacia arriba: 1 × 1 / 3 = 0.333333333… y 2 × 1 / 3 = 0.666666666…
        assert_eq!(costo_de_venta(d("1"), d("1"), d("3")), Ok(d("0.33333333")));
        assert_eq!(costo_de_venta(d("1"), d("2"), d("3")), Ok(d("0.66666667")));
        assert_eq!(costo_de_venta(d("1"), d("4"), d("3")), Err(Rechazo::PosicionInsuficiente));
    }

    #[test]
    fn sin_posicion_o_monto_minimo() {
        assert_eq!(venta(d("100"), d(ORO), 4, d("0"), d("0")), Err(Rechazo::SinPosicion));
        assert_eq!(venta(d("0.10"), d(ORO), 4, d("0.7752"), d("1249.93")), Err(Rechazo::MontoInsuficiente));
    }

    #[test]
    fn numeros_fuera_de_rango() {
        // Un monto con fracciones de centavo no es un monto de orden.
        assert_eq!(compra(d("1.001"), d(ORO), 4), Err(Rechazo::FueraDeRango));
        assert_eq!(compra(d("1.123456789"), d(ORO), 4), Err(Rechazo::FueraDeRango));
        assert_eq!(venta(d("1.001"), d(ORO), 4, d("1"), d("1")), Err(Rechazo::FueraDeRango));
        assert_eq!(compra(d("0"), d(ORO), 4), Err(Rechazo::FueraDeRango));
        assert_eq!(compra(d("10"), d("0"), 4), Err(Rechazo::FueraDeRango));
        assert_eq!(compra(d("10"), d(ORO), 9), Err(Rechazo::FueraDeRango));
        // Una cantidad que no cabe en NUMERIC(18, 8): el máximo de una orden a un precio ínfimo.
        assert_eq!(compra(d("999999999.99"), d("0.00000001"), 8), Err(Rechazo::FueraDeRango));
        // El máximo de una orden con 8 decimales de cantidad: los productos caben en i128.
        assert!(compra(d("999999999.99"), d(ORO), 8).is_ok());
    }

    #[test]
    fn valor_y_cambio_para_la_boveda() {
        assert_eq!(valor(d("0.7752"), d(ORO)), d("1249.93248"));
        // 0.12345678² = 0.0152415765279684: truncado, no redondeado.
        assert_eq!(valor(d("0.12345678"), d("0.12345678")), d("0.01524157"));
        assert_eq!(cambio_pct(d("1612.40"), d("1598.97")).to_string(), "0.84");
        assert_eq!(cambio_pct(d("548.20"), d("551.06")).to_string(), "-0.52");
        assert_eq!(cambio_pct(d("9.92"), d("9.92")).to_string(), "0.00");
        assert_eq!(cambio_pct(d("1"), Decimal::ZERO).to_string(), "0.00");
    }
}
