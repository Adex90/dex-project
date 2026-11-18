//! The relayer's signer: the relayer vault (`vaults/threshold-signer --role relayer`) over its local Unix socket.
//!
//! The relayer holds no key and no KMS permission. The vault holds the relayer's gas key in AWS KMS and signs
//! `ZkPriceFeed.submit` only, for this feed's provider and instrument, within its caps and its nonce rule. Here the
//! vault is an alloy `TxSigner`: the provider fills the transaction (nonce, fees, gas) and the vault signs it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy::consensus::SignableTransaction;
use alloy::network::TxSigner;
use alloy::primitives::{Address, B256, Signature, U256, hex, keccak256};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::chain::FeeCaps;

/// What the vault reports it signs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultPolicy {
    pub chain_id: u64,
    /// The one contract it signs calls to: for the relayer, the ZkPriceFeed.
    pub to: Address,
    pub max_gas: u64,
    pub max_fee_per_gas: u128,
}

/// The relayer vault, as a transaction signer.
#[derive(Clone, Debug)]
pub struct VaultSigner {
    path: PathBuf,
    timeout: Duration,
    address: Address,
    policy: VaultPolicy,
}

/// One request, one reply, on a fresh connection, within `timeout`.
async fn call(path: &Path, timeout: Duration, method: &str, params: Option<Value>) -> Result<Value, String> {
    let exchange = async {
        let mut stream =
            UnixStream::connect(path).await.map_err(|e| format!("the vault at {}: {e}", path.display()))?;
        let mut request = json!({ "id": 1, "method": method });
        if let Some(params) = params {
            request["params"] = params;
        }
        let mut line = request.to_string().into_bytes();
        line.push(b'\n');
        stream.write_all(&line).await.map_err(|e| format!("the vault: {e}"))?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await.map_err(|e| format!("the vault: {e}"))?;
        serde_json::from_str::<Value>(&reply).map_err(|_| "the vault closed or answered garbage".to_string())
    };
    let reply = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| format!("the vault did not answer {method} within {} ms", timeout.as_millis()))??;
    if let Some(error) = reply.get("error") {
        return Err(format!(
            "the vault refused ({}): {}",
            error["code"].as_str().unwrap_or("unknown"),
            error["message"].as_str().unwrap_or("")
        ));
    }
    if reply["id"] != 1 {
        return Err("the vault answered another request".into());
    }
    Ok(reply["result"].clone())
}

impl VaultSigner {
    /// Asks the vault at `path` for its address and its policy. Only a relayer vault is accepted: a vault holds one
    /// role's key, and another role's would refuse every push.
    pub async fn connect(path: &Path, timeout: Duration) -> Result<Self, String> {
        let address = call(path, timeout, "address", None).await?["address"]
            .as_str()
            .and_then(|a| a.parse::<Address>().ok())
            .ok_or("the vault's address reply has no address")?;
        let policy = call(path, timeout, "policy", None).await?;
        let bad = |field: &str| format!("the vault's policy has no valid {field}");
        let role = policy["role"].as_str().ok_or_else(|| bad("role"))?;
        if role != "relayer" {
            return Err(format!(
                "the vault at {} holds the {role} key: the relayer signs through the relayer vault",
                path.display()
            ));
        }
        let policy = VaultPolicy {
            chain_id: policy["chain_id"].as_u64().ok_or_else(|| bad("chain_id"))?,
            to: policy["to"].as_str().and_then(|a| a.parse().ok()).ok_or_else(|| bad("to"))?,
            max_gas: policy["max_gas"].as_u64().ok_or_else(|| bad("max_gas"))?,
            max_fee_per_gas: policy["max_fee_per_gas"]
                .as_str()
                .and_then(|fee| fee.parse().ok())
                .ok_or_else(|| bad("max_fee_per_gas"))?,
        };
        Ok(Self { path: path.to_path_buf(), timeout, address, policy })
    }

    pub const fn policy(&self) -> &VaultPolicy {
        &self.policy
    }

    /// Why the vault would refuse every push the relayer makes with these settings, if it would.
    pub fn refuses(&self, chain_id: u64, feed: Address, caps: &FeeCaps) -> Option<String> {
        let p = &self.policy;
        if p.chain_id != chain_id {
            return Some(format!("the vault signs for chain {}, the relayer pushes on chain {chain_id}", p.chain_id));
        }
        if p.to != feed {
            return Some(format!("the vault signs pushes to {}, the relayer pushes to {feed}", p.to));
        }
        if caps.gas_limit > p.max_gas {
            return Some(format!("the relayer's gas cap {} is above the vault's {}", caps.gas_limit, p.max_gas));
        }
        if caps.max_fee_per_gas > p.max_fee_per_gas {
            return Some(format!(
                "the relayer's max fee {} wei is above the vault's {}",
                caps.max_fee_per_gas, p.max_fee_per_gas
            ));
        }
        None
    }
}

#[async_trait::async_trait]
impl TxSigner<Signature> for VaultSigner {
    fn address(&self) -> Address {
        self.address
    }

    /// The vault signs the transaction's signing payload; the signature must recover to the vault's address over the
    /// payload's hash, so a confused or impersonated vault cannot hand back a signature for anything else.
    async fn sign_transaction(&self, tx: &mut dyn SignableTransaction<Signature>) -> alloy::signers::Result<Signature> {
        let mut payload = Vec::new();
        tx.encode_for_signing(&mut payload);
        let result =
            call(&self.path, self.timeout, "sign_transaction", Some(json!({ "tx": hex::encode_prefixed(&payload) })))
                .await
                .map_err(alloy::signers::Error::other)?;
        let word = |name: &str| -> Result<U256, alloy::signers::Error> {
            result[name]
                .as_str()
                .and_then(|w| w.parse::<B256>().ok())
                .map(Into::into)
                .ok_or_else(|| alloy::signers::Error::other(format!("the vault's signature has no valid {name}")))
        };
        let y_parity = match result["y_parity"].as_u64() {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(alloy::signers::Error::other("the vault's signature has no valid y_parity")),
        };
        let signature = Signature::new(word("r")?, word("s")?, y_parity);
        let recovered = signature
            .recover_address_from_prehash(&keccak256(&payload))
            .map_err(|e| alloy::signers::Error::other(format!("the vault's signature does not recover: {e}")))?;
        if recovered != self.address {
            return Err(alloy::signers::Error::other(format!(
                "the vault's signature recovers to {recovered}, not to the vault ({})",
                self.address
            )));
        }
        Ok(signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::consensus::TxEip1559;
    use alloy::primitives::TxKind;
    use alloy::signers::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use tokio::net::UnixListener;

    const FEED: Address = Address::repeat_byte(0xfe);

    /// A vault on a socket: answers address and policy for `role`, and signs with `key` (or with another key).
    async fn fake_vault(path: PathBuf, role: &'static str, key: PrivateKeySigner, honest: bool) {
        let listener = UnixListener::bind(&path).unwrap();
        let address = key.address();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let key = key.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    BufReader::new(read).read_line(&mut line).await.unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "address" => json!({ "address": address.to_string() }),
                        "policy" => json!({
                            "role": role, "chain_id": 31_337, "to": FEED.to_string(),
                            "max_gas": 2_000_000, "max_fee_per_gas": "500000000000",
                        }),
                        _ => {
                            let payload = hex::decode(request["params"]["tx"].as_str().unwrap()).unwrap();
                            let signer = if honest { key } else { PrivateKeySigner::random() };
                            let signature = signer.sign_hash_sync(&keccak256(&payload)).unwrap();
                            json!({
                                "r": B256::from(signature.r()).to_string(),
                                "s": B256::from(signature.s()).to_string(),
                                "y_parity": u8::from(signature.v()),
                            })
                        }
                    };
                    let reply = json!({ "id": request["id"], "result": result }).to_string() + "\n";
                    write.write_all(reply.as_bytes()).await.unwrap();
                });
            }
        });
    }

    fn socket(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("oracle-vault-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn tx() -> TxEip1559 {
        TxEip1559 {
            chain_id: 31_337,
            nonce: 3,
            gas_limit: 400_000,
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            to: TxKind::Call(FEED),
            input: vec![0xab; 40].into(),
            ..TxEip1559::default()
        }
    }

    #[tokio::test]
    async fn the_relayer_vault_signs_its_transactions() {
        let key = PrivateKeySigner::random();
        let path = socket("honest");
        fake_vault(path.clone(), "relayer", key.clone(), true).await;
        let signer = VaultSigner::connect(&path, Duration::from_secs(2)).await.unwrap();
        assert_eq!(TxSigner::address(&signer), key.address());
        assert_eq!(signer.refuses(31_337, FEED, &FeeCaps::TESTNET), None);
        for (chain, feed, caps, why) in [
            (1, FEED, FeeCaps::TESTNET, "chain"),
            (31_337, Address::ZERO, FeeCaps::TESTNET, "pushes to"),
            (31_337, FEED, FeeCaps { gas_limit: 3_000_000, ..FeeCaps::TESTNET }, "gas cap"),
            (31_337, FEED, FeeCaps { max_fee_per_gas: 600_000_000_000, ..FeeCaps::TESTNET }, "max fee"),
        ] {
            assert!(signer.refuses(chain, feed, &caps).unwrap().contains(why), "{why}");
        }
        let mut tx = tx();
        let signature = signer.sign_transaction(&mut tx).await.unwrap();
        assert_eq!(signature.recover_address_from_prehash(&tx.signature_hash()).unwrap(), key.address());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn another_roles_vault_or_a_foreign_signature_is_refused() {
        let path = socket("operator");
        fake_vault(path.clone(), "operator", PrivateKeySigner::random(), true).await;
        let refused = VaultSigner::connect(&path, Duration::from_secs(2)).await.unwrap_err();
        assert!(refused.contains("holds the operator key"), "{refused}");
        let _ = std::fs::remove_file(&path);

        let path = socket("liar");
        fake_vault(path.clone(), "relayer", PrivateKeySigner::random(), false).await;
        let signer = VaultSigner::connect(&path, Duration::from_secs(2)).await.unwrap();
        let refused = signer.sign_transaction(&mut tx()).await.unwrap_err().to_string();
        assert!(refused.contains("not to the vault"), "{refused}");
        let _ = std::fs::remove_file(&path);

        let missing = VaultSigner::connect(&socket("missing"), Duration::from_secs(1)).await.unwrap_err();
        assert!(missing.contains("the vault at"), "{missing}");
    }
}
