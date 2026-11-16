//! The commitments `circuits/zkdeco_oracle` recomputes, with Poseidon over BN254 and circomlib's parameters
//! (light-poseidon's `new_circom`):
//!
//! * the body commitment: Poseidon of the 96-byte padded body, as four 24-byte chunks packed little-endian (byte i
//!   worth 256^i). The notary signs it.
//! * the query commitment: Poseidon(pack(symbol), salt), which hides the instrument on chain. `ZkPriceFeed` pins it.

use alloy::primitives::{B256, U256, keccak256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use light_poseidon::{Poseidon, PoseidonHasher};

use crate::body::{BODY_LEN, SYMBOL_LEN};

/// Bytes per field element in the body commitment (24 < 31: a chunk is always a field element).
const CHUNK: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotAFieldElement;

impl std::fmt::Display for NotAFieldElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("value is not below the BN254 scalar field modulus")
    }
}

impl std::error::Error for NotAFieldElement {}

/// The BN254 scalar field's modulus p.
pub fn modulus() -> U256 {
    U256::from_be_slice(&Fr::MODULUS.to_bytes_be())
}

fn to_fr(x: U256) -> Result<Fr, NotAFieldElement> {
    if x >= modulus() {
        return Err(NotAFieldElement);
    }
    Ok(Fr::from_be_bytes_mod_order(&x.to_be_bytes::<32>()))
}

fn from_fr(f: Fr) -> U256 {
    U256::from_be_slice(&f.into_bigint().to_bytes_be())
}

/// Poseidon of 1 to 12 field elements (circomlib's `Poseidon(n)`). Inputs at or above p are refused, not reduced.
pub fn poseidon(inputs: &[U256]) -> Result<U256, NotAFieldElement> {
    let elements = inputs.iter().map(|&x| to_fr(x)).collect::<Result<Vec<_>, _>>()?;
    let mut hasher = Poseidon::<Fr>::new_circom(elements.len()).map_err(|_| NotAFieldElement)?;
    hasher.hash(&elements).map(from_fr).map_err(|_| NotAFieldElement)
}

/// Little-endian packing, byte i worth 256^i: the circuit's chunk and symbol packing.
pub fn pack_le(bytes: &[u8]) -> U256 {
    U256::from_le_slice(bytes)
}

/// Poseidon of the padded body's four 24-byte chunks.
pub fn body_commitment(padded: &[u8; BODY_LEN]) -> B256 {
    let chunks: Vec<U256> = padded.chunks(CHUNK).map(pack_le).collect();
    let value = poseidon(&chunks).expect("24-byte chunks are below the modulus");
    B256::from(value.to_be_bytes::<32>())
}

/// Poseidon(pack(symbol), salt). The salt must be a field element.
pub fn query_commitment(symbol: &[u8; SYMBOL_LEN], salt: U256) -> Result<B256, NotAFieldElement> {
    poseidon(&[pack_le(symbol), salt]).map(|v| B256::from(v.to_be_bytes::<32>()))
}

/// keccak256(label) reduced mod p: the fixture generators' deterministic values (`det` in circuits/scripts), used for
/// test provider ids and test salts. A deployment's salt is random and secret.
pub fn det(label: &str) -> U256 {
    U256::from_be_bytes(keccak256(label).0) % modulus()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{CanonicalBody, Price};
    use alloy::primitives::b256;

    #[test]
    fn the_modulus_is_bn254s_scalar_field() {
        assert_eq!(
            modulus().to_string(),
            "21888242871839275222246405745257275088548364400416034343698204186575808495617"
        );
        assert_eq!(poseidon(&[modulus()]), Err(NotAFieldElement));
        assert!(poseidon(&[modulus() - U256::from(1u8)]).is_ok());
    }

    /// Values from contracts/test/fixtures/FeedFixtures.sol, written by circuits/scripts/gen-feed-fixtures.mjs with
    /// circomlibjs: the Rust commitments are the JavaScript ones, bit for bit.
    #[test]
    fn commitments_match_the_ethusd_fixtures() {
        assert_eq!(
            B256::from(det("StrawberrySwap.testnet.price-endpoint").to_be_bytes::<32>()),
            b256!("0x08abf07e8766aa49bf37ef22bd0341a2953a237b4933784bd9fee44bda8be771")
        );
        assert_eq!(
            query_commitment(b"ETHUSD", det("StrawberrySwap.testnet.ETHUSD.salt")).unwrap(),
            b256!("0x16deddae37e6a51b4a9b7e8ee87c29e94b8f38e75a2ad04d1d883956ffe5a162")
        );
        let t = 1_767_225_600u64;
        for (price, ts, commitment) in [
            ("3456.789012", t, b256!("0x22466b4995874955bcf708559ead200c9ad778d93f04317df7f9cabcfca0e1a3")),
            ("3457.1205", t + 15, b256!("0x13f6abb8c9b3446b36b24e39f1d903ddfd5b833feb7994ca7400d4504ba0e655")),
            ("3455.9", t + 30, b256!("0x1396cdfb81e1268332a55e95426dfa57cc71e17a3ca792fd2cddc2cdf2708468")),
            ("3458.0000000001", t + 45, b256!("0x2639decd2f1e4d13e62ec7bef5b23953c63c5b8af681cf936634d79739e4180b")),
        ] {
            let body = CanonicalBody::new("ETHUSD", Price::parse_quote(price).unwrap(), ts).unwrap();
            assert_eq!(body_commitment(&body.padded()), commitment, "{price}");
        }
    }

    /// The repo's original XAUUSD fixture (contracts/test/fixtures/Fixtures.sol, gen-fixtures.mjs).
    #[test]
    fn commitments_match_the_xauusd_fixture() {
        assert_eq!(
            B256::from(det("StrawberrySwap.provider.example").to_be_bytes::<32>()),
            b256!("0x1df8bd17f7fc1f7608d26a0ece0c595a011219fa0652a3e006bbf039a2f7504d")
        );
        assert_eq!(
            query_commitment(b"XAUUSD", det("oracle.salt")).unwrap(),
            b256!("0x18e179f084667b1770b652dc8589ced69641f809a6115c2e000aa18ce77c2ff2")
        );
        let body = CanonicalBody::parse(br#"{"symbol":"XAUUSD","price":2345.678912,"ts":1767225600}"#).unwrap();
        assert_eq!(
            body_commitment(&body.padded()),
            b256!("0x273d1010da07220c5819ea81d8c6280c20cd05d763f8ccb9c160fdcb58d3d8bc")
        );
    }

    #[test]
    fn a_salt_outside_the_field_is_refused() {
        assert_eq!(query_commitment(b"ETHUSD", modulus()), Err(NotAFieldElement));
        assert_eq!(query_commitment(b"ETHUSD", U256::MAX), Err(NotAFieldElement));
    }
}
