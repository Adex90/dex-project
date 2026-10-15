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
//! hoja  = SHA256("SC-POR-v1:hoja" || 0x00 || cuenta_id[16] || saldo_u128_be[16] || sal[32])
//! nodo  = SHA256("SC-POR-v1:nodo" || 0x01 || izq.hash[32] || izq.suma_be[16] || der.hash[32] || der.suma_be[16])
//! vacío = SHA256("SC-POR-v1:vacio"), suma 0
//! ```
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

    /// Vector fijo para implementaciones externas (p. ej. el verificador del frontend).
    #[test]
    fn vector_de_referencia() {
        let hs = vec![
            Hoja { cuenta_id: Uuid::from_bytes([0x11; 16]), saldo: 100_000_000_000, sal: [0xAA; 32] },
            Hoja { cuenta_id: Uuid::from_bytes([0x22; 16]), saldo: 50_000_000, sal: [0xBB; 32] },
            Hoja { cuenta_id: Uuid::from_bytes([0x33; 16]), saldo: 0, sal: [0xCC; 32] },
        ];
        let a = ArbolSuma::construir(&hs).unwrap();
        assert_eq!(a.raiz().suma, 100_050_000_000);
        assert_eq!(hex::encode(a.raiz().hash).len(), 64);
        // La hoja 2 sube sin paso en el primer nivel.
        assert_eq!(a.prueba(2).unwrap().ruta.len(), 1);
        assert_eq!(a.prueba(0).unwrap().ruta.len(), 2);
    }
}
