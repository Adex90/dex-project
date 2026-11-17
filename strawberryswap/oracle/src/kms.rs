//! Signing keys in AWS KMS: the notary's key and the relayer's gas key are secp256k1 KMS keys (ECC_SECG_P256K1,
//! SIGN_VERIFY). These processes hold a KMS client and a key id, never key material. KMS signs a 32-byte digest
//! (MessageType DIGEST) and returns a DER signature; alloy's AwsSigner normalizes it to low s and recovers the parity
//! against the key's public key.
//!
//! This is the testnet's interim custody (decision 3): a threshold (MPC) signer replaces it before mainnet, behind the
//! same `Signer` trait.

use alloy::signers::aws::AwsSigner;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_kms::config::Credentials;

#[derive(Clone, Debug, Default)]
pub struct KmsConfig {
    /// Key id, key ARN, alias name or alias ARN. A key id or ARN is safer: the signer caches the address.
    pub key_id: String,
    /// The KMS endpoint, if not the region's default (a VPC endpoint; or a local stand-in in tests).
    pub endpoint: Option<String>,
    pub region: Option<String>,
    /// Static credentials, for tests against a local stand-in only. Otherwise the standard chain applies:
    /// environment, profile, web identity, ECS, then the EC2 instance role.
    pub static_credentials: Option<(String, String)>,
}

/// A signer for one KMS key. Fetches the public key (`GetPublicKey`) once, so it fails fast on a wrong key.
pub async fn signer(config: &KmsConfig, chain_id: Option<u64>) -> Result<AwsSigner, String> {
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
    AwsSigner::new(aws_sdk_kms::Client::new(&shared), config.key_id.clone(), chain_id)
        .await
        .map_err(|e| format!("KMS key {}: {e}", config.key_id))
}
