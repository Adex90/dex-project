//! The role's key in AWS KMS: a secp256k1 key (`ECC_SECG_P256K1`, `SIGN_VERIFY`) generated inside KMS, whose HSMs
//! never export it. This process holds a KMS client and a key id, never key material. KMS signs a 32-byte digest
//! (`MessageType` DIGEST, `ECDSA_SHA_256`) and returns a DER signature; alloy's `AwsSigner` normalizes it to low s and
//! recovers the parity against the key's public key.
//!
//! This is the testnet's interim custody: a threshold (MPC) signer replaces it before mainnet, behind the same
//! `alloy_signer::Signer` trait, so neither the policy nor the socket protocol changes.

use alloy_signer_aws::AwsSigner;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_kms::config::Credentials;

#[derive(Clone, Debug, Default)]
pub struct KmsConfig {
    /// Key id or key ARN (preferred: the signer caches the address, which an alias could later stop matching), or an
    /// alias.
    pub key_id: String,
    /// The KMS endpoint, if not the region's default (a VPC endpoint; a local stand-in in tests).
    pub endpoint: Option<String>,
    pub region: Option<String>,
    /// Static credentials, for tests against a local stand-in only. Otherwise the standard chain applies:
    /// environment, profile, web identity, ECS, then the EC2 instance role.
    pub static_credentials: Option<(String, String)>,
}

/// A signer for the key. Reads the public key (`GetPublicKey`) once, so a wrong key id, a missing permission or a key
/// of the wrong type fails at start.
pub async fn signer(config: &KmsConfig) -> Result<AwsSigner, String> {
    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Some(region) = &config.region {
        loader = loader.region(Region::new(region.clone()));
    }
    if let Some(endpoint) = &config.endpoint {
        loader = loader.endpoint_url(endpoint.clone());
    }
    if let Some((id, secret)) = &config.static_credentials {
        loader = loader.credentials_provider(Credentials::new(id, secret, None, None, "static"));
    }
    let shared = loader.load().await;
    // No chain id on the signer: the vault signs digests it computed itself, after its own chain id check.
    AwsSigner::new(
        aws_sdk_kms::Client::new(&shared),
        config.key_id.clone(),
        None,
    )
    .await
    .map_err(|e| format!("KMS key {}: {e}", config.key_id))
}
