//! Árbol Merkle de sumas para Proof of Reserve.
//!
//! Cada hoja es una cuenta de la IFPE con su saldo total (disponible +
//! retenido). Cada nodo guarda `(hash, suma)`; la suma de la raíz es el
//! pasivo total con clientes, que la base de datos obliga a ser igual a la
//! reserva líquida en custodia. Un cliente que recibe su prueba de inclusión
//! puede comprobar, sin confiar en el banco, que:
//!
//! 1. su saldo está incluido en la raíz publicada, y
//! 2. ningún nodo del camino tiene suma negativa ni inconsistente (las sumas
//!    de *ambos* hijos entran en el hash del padre, lo que evita el ataque
//!    clásico al árbol de Maxwell en que un nodo declara una suma menor).
//!
//! Codificación (v1), con separación de dominios para que una hoja nunca
//! pueda hacerse pasar por un nodo interno:
//!
//! ```text
//! hoja     = SHA256("SC-POR-v1:hoja" || 0x00 || cuenta_id[16] || saldo_u128_be[16] || sal[32])
//! posición = SHA256("SC-POR-v1:posicion" || 0x00 || ticker || 0x00 || cuenta_id[16] || cantidad_u128_be[16] || sal[32])
//! nodo     = SHA256("SC-POR-v1:nodo" || 0x01 || izq.hash[32] || izq.suma_be[16] || der.hash[32] || der.suma_be[16])
//! vacío    = SHA256("SC-POR-v1:vacio"), suma 0
//! ```
//!
//! Las posiciones del mercado (migración 0018) van en un árbol por activo,
//! aparte del efectivo, con la misma codificación de nodos: su hoja lleva su
//! propio dominio y el ticker (`[A-Z][A-Z0-9]{0,11}`, sin bytes nulos), así
//! que no puede pasar de un árbol a otro ni hacerse pasar por una del efectivo.
//!
//! Los saldos son enteros en unidades de 10^-8 MXN. Si un nivel tiene un número
//! impar de nodos, el último sube sin cambios al siguiente nivel (no se
//! duplica: duplicar contaría dos veces su saldo).
//!
//! Privacidad: la prueba revela la suma de cada hermano (sin ella no se puede
//! verificar el árbol), así que el primer paso puede mostrar el saldo de una
//! hoja vecina. La sal aleatoria de 32 bytes por hoja y corte oculta *de quién*
//! es ese saldo (el `cuenta_id` va dentro del hash), y `reserva` baraja el
//! orden de las hojas en cada corte para que el vecino no pueda seguirse.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const ALGORITMO: &str = "SHA-256 merkle-sum v1";

const DOMINIO_HOJA: &[u8] = b"SC-POR-v1:hoja\x00";
const DOMINIO_POSICION: &[u8] = b"SC-POR-v1:posicion\x00";
const DOMINIO_NODO: &[u8] = b"SC-POR-v1:nodo\x01";
const DOMINIO_VACIO: &[u8] = b"SC-POR-v1:vacio";

pub type Hash = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nodo {
    pub hash: Hash,
    /// Unidades de 10^-8 MXN.
    pub suma: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hoja {
    pub cuenta_id: Uuid,
    pub saldo: u128,
    pub sal: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MerkleError {
    #[error("la suma de saldos desborda u128")]
    Desbordamiento,
}

impl Hoja {
    pub fn hash(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(DOMINIO_HOJA);
        h.update(self.cuenta_id.as_bytes());
        h.update(self.saldo.to_be_bytes());
        h.update(self.sal);
        h.finalize().into()
    }

    pub fn nodo(&self) -> Nodo {
        Nodo { hash: self.hash(), suma: self.saldo }
    }
}

/// Hoja de un árbol de posiciones (0018): la cantidad de un activo que el
/// mercado debe a una cuenta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HojaPosicion {
    /// `[A-Z][A-Z0-9]{0,11}` (lo exigen la base de datos y [`es_ticker`]).
    pub ticker: String,
    pub cuenta_id: Uuid,
    /// Unidades de 10^-8 del activo.
    pub cantidad: u128,
    pub sal: [u8; 32],
}

impl HojaPosicion {
    pub fn hash(&self) -> Hash {
        let mut h = Sha256::new();
        h.update(DOMINIO_POSICION);
        h.update(self.ticker.as_bytes());
        h.update([0u8]);
        h.update(self.cuenta_id.as_bytes());
        h.update(self.cantidad.to_be_bytes());
        h.update(self.sal);
        h.finalize().into()
    }

    pub fn nodo(&self) -> Nodo {
        Nodo { hash: self.hash(), suma: self.cantidad }
    }
}

/// Un ticker del catálogo: la codificación de la hoja de posiciones depende
/// de que no tenga bytes nulos (el ticker termina en 0x00).
pub fn es_ticker(ticker: &str) -> bool {
    let mut letras = ticker.bytes();
    matches!(letras.next(), Some(b'A'..=b'Z'))
        && ticker.len() <= 12
        && letras.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

pub fn combinar(izq: &Nodo, der: &Nodo) -> Result<Nodo, MerkleError> {
    let suma = izq.suma.checked_add(der.suma).ok_or(MerkleError::Desbordamiento)?;
    let mut h = Sha256::new();
    h.update(DOMINIO_NODO);
    h.update(izq.hash);
    h.update(izq.suma.to_be_bytes());
    h.update(der.hash);
    h.update(der.suma.to_be_bytes());
    Ok(Nodo { hash: h.finalize().into(), suma })
}

pub fn raiz_vacia() -> Nodo {
    Nodo { hash: Sha256::digest(DOMINIO_VACIO).into(), suma: 0 }
}

/// Lado en que se encuentra el **hermano** respecto del nodo que se va subiendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Lado {
    Izquierda,
    Derecha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paso {
    pub lado: Lado,
    pub hermano: Nodo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruebaMerkle {
    pub indice: usize,
    pub hoja: Nodo,
    pub ruta: Vec<Paso>,
}

/// Árbol completo en memoria: `niveles[0]` son las hojas y el último nivel la raíz.
#[derive(Debug, Clone)]
pub struct ArbolSuma {
    niveles: Vec<Vec<Nodo>>,
}

impl ArbolSuma {
    pub fn construir(hojas: &[Hoja]) -> Result<Self, MerkleError> {
        Self::desde_nodos(hojas.iter().map(Hoja::nodo).collect())
    }

    pub fn desde_nodos(base: Vec<Nodo>) -> Result<Self, MerkleError> {
        let mut niveles = vec![base];
        while niveles.last().is_some_and(|n| n.len() > 1) {
            let actual = niveles.last().expect("hay al menos un nivel");
            let mut siguiente = Vec::with_capacity(actual.len().div_ceil(2));
            for par in actual.chunks(2) {
                match par {
                    [izq, der] => siguiente.push(combinar(izq, der)?),
                    [solo] => siguiente.push(*solo), // impar: sube sin cambios
                    _ => unreachable!("chunks(2)"),
                }
            }
            niveles.push(siguiente);
        }
        Ok(Self { niveles })
    }

    pub fn num_hojas(&self) -> usize {
        self.niveles[0].len()
    }

    pub fn raiz(&self) -> Nodo {
        match self.niveles.last().and_then(|n| n.first()) {
            Some(raiz) if self.num_hojas() > 0 => *raiz,
            _ => raiz_vacia(),
        }
    }

    pub fn prueba(&self, indice: usize) -> Option<PruebaMerkle> {
        let hoja = *self.niveles[0].get(indice)?;
        let mut ruta = Vec::new();
        let mut i = indice;
        for nivel in &self.niveles[..self.niveles.len() - 1] {
            let hermano = i ^ 1;
            if hermano < nivel.len() {
                let lado = if hermano < i { Lado::Izquierda } else { Lado::Derecha };
                ruta.push(Paso { lado, hermano: nivel[hermano] });
            }
            // Si no hay hermano (último de un nivel impar), el nodo sube sin paso.
            i /= 2;
        }
        Some(PruebaMerkle { indice, hoja, ruta })
    }
}

/// Recalcula la raíz a partir de una hoja y su ruta.
pub fn raiz_desde_ruta(hoja: &Nodo, ruta: &[Paso]) -> Result<Nodo, MerkleError> {
    ruta.iter().try_fold(*hoja, |actual, paso| match paso.lado {
        Lado::Izquierda => combinar(&paso.hermano, &actual),
        Lado::Derecha => combinar(&actual, &paso.hermano),
    })
}

/// Verificación completa de una prueba de inclusión contra una raíz publicada.
pub fn verificar(hoja: &Hoja, ruta: &[Paso], raiz: &Nodo) -> bool {
    matches!(raiz_desde_ruta(&hoja.nodo(), ruta), Ok(calculada) if calculada == *raiz)
}

/// Lo mismo para una posición contra la raíz del árbol de su activo.
pub fn verificar_posicion(hoja: &HojaPosicion, ruta: &[Paso], raiz: &Nodo) -> bool {
    es_ticker(&hoja.ticker) && matches!(raiz_desde_ruta(&hoja.nodo(), ruta), Ok(calculada) if calculada == *raiz)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn hoja(i: u8, saldo: u128) -> Hoja {
        Hoja { cuenta_id: Uuid::from_bytes([i; 16]), saldo, sal: [i.wrapping_mul(7); 32] }
    }

    fn hojas(n: usize) -> Vec<Hoja> {
        (0..n).map(|i| hoja(i as u8, (i as u128 + 1) * 100_000_000)).collect()
    }

    #[test]
    fn arbol_vacio() {
        let a = ArbolSuma::construir(&[]).unwrap();
        assert_eq!(a.raiz(), raiz_vacia());
        assert_eq!(a.raiz().suma, 0);
        assert!(a.prueba(0).is_none());
    }

    #[test]
    fn todas_las_pruebas_verifican_para_cualquier_tamano() {
        for n in 1..=33 {
            let hs = hojas(n);
            let arbol = ArbolSuma::construir(&hs).unwrap();
            let raiz = arbol.raiz();
            let total: u128 = hs.iter().map(|h| h.saldo).sum();
            assert_eq!(raiz.suma, total, "n={n}: la suma de la raíz es el pasivo total");
            for (i, h) in hs.iter().enumerate() {
                let p = arbol.prueba(i).unwrap();
                assert_eq!(p.hoja, h.nodo());
                assert!(p.ruta.len() <= (usize::BITS - n.leading_zeros()) as usize);
                assert!(verificar(h, &p.ruta, &raiz), "n={n} i={i}");
            }
        }
    }

    #[test]
    fn una_hoja_es_su_propia_raiz() {
        let hs = hojas(1);
        let a = ArbolSuma::construir(&hs).unwrap();
        assert_eq!(a.raiz(), hs[0].nodo());
        assert!(a.prueba(0).unwrap().ruta.is_empty());
    }

    #[test]
    fn detecta_saldo_alterado() {
        let hs = hojas(9);
        let a = ArbolSuma::construir(&hs).unwrap();
        let p = a.prueba(4).unwrap();
        let mut falsa = hs[4].clone();
        falsa.saldo += 1;
        assert!(!verificar(&falsa, &p.ruta, &a.raiz()));
        let mut otra_sal = hs[4].clone();
        otra_sal.sal[0] ^= 1;
        assert!(!verificar(&otra_sal, &p.ruta, &a.raiz()));
    }

    #[test]
    fn detecta_suma_de_hermano_reducida() {
        // El banco no puede "esconder" pasivo bajando la suma de un hermano:
        // la suma entra en el hash del padre.
        let hs = hojas(8);
        let a = ArbolSuma::construir(&hs).unwrap();
        let mut p = a.prueba(0).unwrap();
        p.ruta[1].hermano.suma -= 1;
        assert!(!verificar(&hs[0], &p.ruta, &a.raiz()));
    }

    #[test]
    fn detecta_lado_invertido_y_ruta_truncada() {
        let hs = hojas(8);
        let a = ArbolSuma::construir(&hs).unwrap();
        let mut p = a.prueba(3).unwrap();
        p.ruta[0].lado = Lado::Derecha;
        assert!(!verificar(&hs[3], &p.ruta, &a.raiz()));
        let p = a.prueba(3).unwrap();
        assert!(!verificar(&hs[3], &p.ruta[..2], &a.raiz()));
    }

    #[test]
    fn una_hoja_no_se_confunde_con_un_nodo() {
        let h = hoja(1, 5);
        let n = h.nodo();
        let combinado = combinar(&n, &n).unwrap();
        assert_ne!(combinado.hash, h.hash());
    }

    #[test]
    fn desbordamiento_detectado() {
        let a = Nodo { hash: [0; 32], suma: u128::MAX };
        let b = Nodo { hash: [1; 32], suma: 1 };
        assert_eq!(combinar(&a, &b), Err(MerkleError::Desbordamiento));
    }

    /// Vector fijo para implementaciones externas: el verificador del frontend
    /// (`frontend/src/lib/merkle-verifier.test.ts`) y el trigger de la base de
    /// datos llegan a los mismos hashes, calculados aparte desde la
    /// especificación.
    #[test]
    fn vector_de_referencia() {
        let hs = vec![
            Hoja { cuenta_id: Uuid::from_bytes([0x11; 16]), saldo: 100_000_000_000, sal: [0xAA; 32] },
            Hoja { cuenta_id: Uuid::from_bytes([0x22; 16]), saldo: 50_000_000, sal: [0xBB; 32] },
            Hoja { cuenta_id: Uuid::from_bytes([0x33; 16]), saldo: 0, sal: [0xCC; 32] },
        ];
        assert_eq!(hex::encode(hs[0].hash()), "892eb7b69d676ea2338d32d883b66a29a64aa8a7808f3ae2bcbf64e0987536f9");
        assert_eq!(hex::encode(hs[1].hash()), "b16d717b123a2f503c6a03043a64f5cab30b6e511f14d83377e179fb142ec036");
        assert_eq!(hex::encode(hs[2].hash()), "af52a7e97b3b63f1f60d4439215f5918199ee5e6000d2720a6a62f7aeeb255f0");
        let a = ArbolSuma::construir(&hs).unwrap();
        assert_eq!(a.raiz().suma, 100_050_000_000);
        assert_eq!(hex::encode(a.raiz().hash), "9b089a89d1de37751f63a455c8a9e8cd15540a028aa8654f96fd18b03ef9c3f0");
        // La hoja 2 sube sin paso en el primer nivel.
        assert_eq!(a.prueba(2).unwrap().ruta.len(), 1);
        let p = a.prueba(0).unwrap();
        assert_eq!(p.ruta.len(), 2);
        assert_eq!(p.ruta[0].lado, Lado::Derecha);
        assert_eq!(
            hex::encode(combinar(&hs[0].nodo(), &hs[1].nodo()).unwrap().hash),
            "99cc3e6d58053dddc683b16fbb0e8e8e30096429b9cd94dc4b9d120993e26b62"
        );
    }

    fn posicion(ticker: &str, i: u8, cantidad: u128, sal: u8) -> HojaPosicion {
        HojaPosicion { ticker: ticker.into(), cuenta_id: Uuid::from_bytes([i; 16]), cantidad, sal: [sal; 32] }
    }

    #[test]
    fn vector_de_referencia_de_posiciones() {
        let hs = [posicion("AUG1", 0x11, 250_000_000, 0xDD), posicion("AUG1", 0x22, 1, 0xEE)];
        assert_eq!(hex::encode(hs[0].hash()), "1c07aca64ba5dc119b5f606c9c9f7fca0c43285f44405805d40f8990b7721ac2");
        assert_eq!(hex::encode(hs[1].hash()), "af1a6c50dd4af636162a3cb32fd6b55d41492b2e1ea77e2a4ba26ac8ab2dbc9f");
        let a = ArbolSuma::desde_nodos(hs.iter().map(HojaPosicion::nodo).collect()).unwrap();
        assert_eq!(a.raiz().suma, 250_000_001);
        assert_eq!(hex::encode(a.raiz().hash), "d6028011a2ceeea4ed7e14a9861ea1aa06312f6a95676d9bbc0ac481f63a84c3");
        for (i, h) in hs.iter().enumerate() {
            assert!(verificar_posicion(h, &a.prueba(i).unwrap().ruta, &a.raiz()));
        }
    }

    #[test]
    fn una_posicion_no_pasa_de_un_activo_a_otro_ni_a_la_hoja_del_efectivo() {
        let oro = posicion("AUG1", 0x11, 250_000_000, 0xDD);
        let plata = HojaPosicion { ticker: "AGT1".into(), ..oro.clone() };
        assert_eq!(hex::encode(plata.hash()), "40d20497109c13fff04fd2bee899b18a25ee7f63137f6adc72b8c3cddddc3290");
        assert_ne!(oro.hash(), plata.hash());
        let efectivo = Hoja { cuenta_id: oro.cuenta_id, saldo: oro.cantidad, sal: oro.sal };
        assert_ne!(oro.hash(), efectivo.hash());
        // La prueba del oro no sirve para la plata, aunque los datos coincidan.
        let a = ArbolSuma::desde_nodos(vec![oro.nodo(), posicion("AUG1", 0x22, 1, 0xEE).nodo()]).unwrap();
        let ruta = a.prueba(0).unwrap().ruta;
        assert!(verificar_posicion(&oro, &ruta, &a.raiz()));
        assert!(!verificar_posicion(&plata, &ruta, &a.raiz()));
    }

    #[test]
    fn ticker_valido() {
        for t in ["AUG1", "A", "CETES28", "ABCDEFGHIJKL"] {
            assert!(es_ticker(t), "{t}");
        }
        for t in ["", "aug1", "1AUG", "AUG-1", "AUG\u{0}1", "ABCDEFGHIJKLM", "ÁUG"] {
            assert!(!es_ticker(t), "{t:?}");
        }
        // Un ticker inválido no verifica aunque su hash sea la raíz.
        let h = posicion("aug1", 0x11, 1, 0x01);
        let a = ArbolSuma::desde_nodos(vec![h.nodo()]).unwrap();
        assert_eq!(a.raiz(), h.nodo());
        assert!(!verificar_posicion(&h, &[], &a.raiz()));
    }
}
