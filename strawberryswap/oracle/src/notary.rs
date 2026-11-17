//! What the notary signs, and the proof format `ZkDecoOracle.verifyAttestation` reads:
//!
//! ```text
//! digest = keccak256("\x19Ethereum Signed Message:\n32" ‖ keccak256(abi.encode(DOMAIN, providerId, bodyCommitment)))
//! proof  = Groth16 proof (256 bytes) ‖ bodyCommitment (32) ‖ r (32) ‖ s (32) ‖ v (1)          353 bytes
//! ```
//!
//! with DOMAIN = keccak256("StrawberrySwap.zkdeco.notary.v1"), s in the lower half of the curve order and v 27 or 28
//! (the contract rejects anything else).

use std::fmt;

use alloy::primitives::{Address, B256, Bytes, Signature, eip191_hash_message, keccak256};
use serde::{Deserialize, Serialize};

use crate::body::{BodyError, CanonicalBody};
use crate::commit::body_commitment;

/// ZkDecoOracle's signing domain.
pub const NOTARY_DOMAIN: &str = "StrawberrySwap.zkdeco.notary.v1";
/// abi.encode(uint256[2] a, uint256[2][2] b, uint256[2] c).
pub const SNARK_LEN: usize = 256;
/// The full `ZkDecoOracle` proof.
pub const PROOF_LEN: usize = SNARK_LEN + 32 + 65;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotaryError {
    Body(BodyError),
    /// The body commitment is not the body's.
    Commitment,
    /// The attestation is for another provider.
    Provider {
        got: B256,
        want: B256,
    },
    /// The signature is malformed, malleable or not the expected notary's.
    Signature(&'static str),
    Notary {
        got: Address,
        want: Address,
    },
}

impl fmt::Display for NotaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(e) => write!(f, "{e}"),
            Self::Commitment => f.write_str("the body commitment is not the body's"),
            Self::Provider { got, want } => write!(f, "provider {got}, expected {want}"),
            Self::Signature(m) => write!(f, "notary signature: {m}"),
            Self::Notary { got, want } => write!(f, "signed by {got}, expected the notary {want}"),
        }
    }
}

impl std::error::Error for NotaryError {}

impl From<BodyError> for NotaryError {
    fn from(e: BodyError) -> Self {
        Self::Body(e)
    }
}

/// keccak256(abi.encode(keccak256(DOMAIN), providerId, bodyCommitment)): the 32 bytes the notary signs (EIP-191).
pub fn notary_message(provider_id: B256, body_commitment: B256) -> B256 {
    let mut encoded = [0u8; 96];
    encoded[..32].copy_from_slice(keccak256(NOTARY_DOMAIN).as_slice());
    encoded[32..64].copy_from_slice(provider_id.as_slice());
    encoded[64..].copy_from_slice(body_commitment.as_slice());
    keccak256(encoded)
}

/// `ZkDecoOracle.notaryDigest(providerId, bodyCommitment)`.
pub fn notary_digest(provider_id: B256, body_commitment: B256) -> B256 {
    eip191_hash_message(notary_message(provider_id, body_commitment))
}

/// r ‖ s ‖ v as the contract reads it: s normalized to the lower half (the parity flips with it), v 27 or 28.
pub fn encode_signature(signature: &Signature) -> [u8; 65] {
    signature.normalize_s().unwrap_or(*signature).as_bytes()
}

/// The contract's checks on r ‖ s ‖ v, then recovery: the address that signed `digest`.
pub fn recover(signature: &[u8], digest: B256) -> Result<Address, NotaryError> {
    if signature.len() != 65 {
        return Err(NotaryError::Signature("65 bytes expected (r, s, v)"));
    }
    if signature[64] != 27 && signature[64] != 28 {
        return Err(NotaryError::Signature("v must be 27 or 28"));
    }
    let parsed = Signature::from_raw(signature).map_err(|_| NotaryError::Signature("not a signature"))?;
    if parsed.normalize_s().is_some() {
        return Err(NotaryError::Signature("high s: malleable, the contract refuses it"));
    }
    parsed.recover_address_from_prehash(&digest).map_err(|_| NotaryError::Signature("does not recover"))
}

/// The notary's answer: a body fetched from the provider, its commitment and the notary's signature over both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotarizedBody {
    pub provider_id: B256,
    /// The body's text, exactly as the provider served it.
    pub body: String,
    pub body_commitment: B256,
    /// r ‖ s ‖ v (65 bytes).
    pub signature: Bytes,
    /// The notary's address (as it claims; [`NotarizedBody::verify`] checks it against the expected one).
    pub notary: Address,
}

impl NotarizedBody {
    /// The relayer's checks before proving anything: the right provider, a canonical body, its commitment, and the
    /// expected notary's signature over both, in exactly the format the contract accepts.
    pub fn verify(&self, provider_id: B256, notary: Address) -> Result<CanonicalBody, NotaryError> {
        if self.provider_id != provider_id {
            return Err(NotaryError::Provider { got: self.provider_id, want: provider_id });
        }
        let body = CanonicalBody::parse(self.body.as_bytes())?;
        if body.text() != self.body || body_commitment(&body.padded()) != self.body_commitment {
            return Err(NotaryError::Commitment);
        }
        let signer = recover(&self.signature, notary_digest(provider_id, self.body_commitment))?;
        if signer != notary {
            return Err(NotaryError::Notary { got: signer, want: notary });
        }
        Ok(body)
    }

    /// The 353-byte `ZkDecoOracle` proof around a Groth16 proof of this body.
    pub fn proof(&self, snark: &[u8; SNARK_LEN]) -> Bytes {
        let mut out = Vec::with_capacity(PROOF_LEN);
        out.extend_from_slice(snark);
        out.extend_from_slice(self.body_commitment.as_slice());
        out.extend_from_slice(&self.signature);
        out.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::Price;
    use crate::commit::det;
    use alloy::primitives::{U256, b256};
    use k256::ecdsa::SigningKey;

    fn key(label: &str) -> SigningKey {
        SigningKey::from_slice(keccak256(label).as_slice()).unwrap()
    }

    fn address(key: &SigningKey) -> Address {
        Address::from_public_key(key.verifying_key())
    }

    fn sign(key: &SigningKey, digest: B256) -> Signature {
        let (sig, recid) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
        Signature::from_signature_and_parity(sig, recid.is_y_odd())
    }

    fn provider() -> B256 {
        B256::from(det("StrawberrySwap.testnet.price-endpoint").to_be_bytes::<32>())
    }

    fn notarize(key: &SigningKey, text: &str) -> NotarizedBody {
        let body = CanonicalBody::parse(text.as_bytes()).unwrap();
        let commitment = body_commitment(&body.padded());
        let sig = sign(key, notary_digest(provider(), commitment));
        NotarizedBody {
            provider_id: provider(),
            body: text.to_string(),
            body_commitment: commitment,
            signature: encode_signature(&sig).to_vec().into(),
            notary: address(key),
        }
    }

    const BODY: &str = r#"{"symbol":"ETHUSD","price":3456.789012,"ts":1767225600}"#;

    /// The digest, spelled out: the EIP-191 prefix over keccak256(DOMAIN ‖ providerId ‖ bodyCommitment). (CI's
    /// contracts job checks the same vector against ZkDecoOracle.notaryDigest: test/NotaryVector.t.sol.)
    #[test]
    fn the_digest_is_zkdeco_oracles() {
        let commitment = b256!("0x22466b4995874955bcf708559ead200c9ad778d93f04317df7f9cabcfca0e1a3");
        let inner =
            keccak256([keccak256(NOTARY_DOMAIN).as_slice(), provider().as_slice(), commitment.as_slice()].concat());
        let expected = keccak256([b"\x19Ethereum Signed Message:\n32".as_slice(), inner.as_slice()].concat());
        assert_eq!(notary_message(provider(), commitment), inner);
        assert_eq!(notary_digest(provider(), commitment), expected);
        assert_eq!(
            notary_digest(provider(), commitment),
            b256!("0x9d0788e643dfc235b812b96b689a3652ead96de3c8e94c5bb80ec9ee8ad2ee3b"),
            "pinned vector (update NotaryVector.t.sol together)"
        );
    }

    #[test]
    fn a_notarized_body_verifies_and_carries_the_contracts_proof_format() {
        let notary = key("test notary");
        let n = notarize(&notary, BODY);
        let body = n.verify(provider(), address(&notary)).unwrap();
        assert_eq!(body.price_wad(), Price::parse_quote("3456.789012").unwrap().wad());
        let proof = n.proof(&[7u8; SNARK_LEN]);
        assert_eq!(proof.len(), PROOF_LEN);
        assert_eq!(&proof[256..288], n.body_commitment.as_slice());
        assert!(proof[352] == 27 || proof[352] == 28);
        assert!(U256::from_be_slice(&proof[320..352]) <= U256::from_be_slice(&k256_half_order()));
    }

    fn k256_half_order() -> [u8; 32] {
        alloy::primitives::hex!("7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0")
    }

    #[test]
    fn verify_refuses_anything_the_contract_would() {
        let notary = key("test notary");
        let good = notarize(&notary, BODY);
        let want = address(&notary);

        let other = NotarizedBody { provider_id: B256::repeat_byte(1), ..good.clone() };
        assert!(matches!(other.verify(provider(), want), Err(NotaryError::Provider { .. })));

        let body = NotarizedBody { body: BODY.replace("3456.789012", "3456.789013"), ..good.clone() };
        assert_eq!(body.verify(provider(), want), Err(NotaryError::Commitment));

        let foreign = notarize(&key("someone else"), BODY);
        assert!(matches!(foreign.verify(provider(), want), Err(NotaryError::Notary { .. })));

        // The same signature with s mirrored to the upper half (still a valid ECDSA signature): the contract refuses it.
        let sig = Signature::from_raw(&good.signature).unwrap();
        let n = U256::from_be_slice(&alloy::primitives::hex!(
            "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141"
        ));
        let high = Signature::new(sig.r(), n - sig.s(), !sig.v());
        let malleable = NotarizedBody { signature: high.as_bytes().to_vec().into(), ..good.clone() };
        assert_eq!(
            malleable.verify(provider(), want),
            Err(NotaryError::Signature("high s: malleable, the contract refuses it"))
        );
        assert_eq!(encode_signature(&high), good.signature.as_ref(), "encode_signature normalizes it back");

        let mut v = good.signature.to_vec();
        v[64] -= 27; // 0/1 instead of 27/28
        assert!(matches!(
            NotarizedBody { signature: v.into(), ..good.clone() }.verify(provider(), want),
            Err(NotaryError::Signature(_))
        ));

        let not_canonical = NotarizedBody { body: BODY.replace(':', ": "), ..good.clone() };
        assert!(matches!(not_canonical.verify(provider(), want), Err(NotaryError::Body(_))));
    }

    #[test]
    fn the_wire_format_is_strict_json() {
        let n = notarize(&key("test notary"), BODY);
        let text = serde_json::to_string(&n).unwrap();
        assert!(text.contains("\"bodyCommitment\":\"0x") && text.contains("\"providerId\":\"0x"));
        assert_eq!(serde_json::from_str::<NotarizedBody>(&text).unwrap(), n);
        let extra = text.replacen('{', "{\"extra\":1,", 1);
        assert!(serde_json::from_str::<NotarizedBody>(&extra).is_err());
    }
}
