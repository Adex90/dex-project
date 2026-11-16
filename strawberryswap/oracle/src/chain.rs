//! The relayer's view of the chain: `ZkPriceFeed`'s configuration and latest observation, a preflight of `submit`
//! (`eth_call` from the relayer's address), and the push itself, an EIP-1559 transaction signed by the relayer's KMS
//! key, with capped fees and gas.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use alloy::contract::Error as ContractError;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::Signer;
use alloy::signers::aws::AwsSigner;

alloy::sol! {
    #[sol(rpc)]
    #[derive(Debug, PartialEq, Eq)]
    interface IZkPriceFeed {
        struct Attestation {
            bytes32 providerId;
            bytes32 queryCommitment;
            uint256 price;
            uint64 observedAt;
        }

        function submit(Attestation calldata a, bytes calldata proof) external;
        function latestVerifiedPrice() external view returns (uint256 price1e18, uint64 updatedAt);
        function providerId() external view returns (bytes32);
        function queryCommitment() external view returns (bytes32);
        function verifier() external view returns (address);
        function maxSubmissionDelay() external view returns (uint64);

        error BadConfig();
        error WrongProvider(bytes32 providerId);
        error WrongInstrument(bytes32 queryCommitment);
        error NotNewer(uint64 observedAt, uint64 latest);
        error FromTheFuture(uint64 observedAt, uint256 blockTime);
        error TooOld(uint64 observedAt, uint256 blockTime);
        error PriceOutOfBounds(uint256 price);
        error InvalidAttestation();
    }

    #[sol(rpc)]
    #[derive(Debug, PartialEq, Eq)]
    interface IZkDecoRegistry {
        function notaryOf(bytes32 providerId) external view returns (address);
    }
}

pub use IZkPriceFeed::Attestation;

/// What the feed is configured to accept, read once at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedSetup {
    pub provider_id: B256,
    pub query_commitment: B256,
    /// The notary ZkDecoOracle registered for the provider.
    pub notary: Address,
    pub max_submission_delay: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pushed {
    pub tx_hash: B256,
    pub gas_used: u64,
    pub block: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChainError {
    Rpc(String),
    /// The feed refuses the push (decoded to its custom error where possible).
    Reverted(String),
    /// The chain's clock is behind the observation: the feed's FromTheFuture. The sequencer's clock catches up within
    /// seconds, so the relayer waits rather than drops the observation.
    FromTheFuture {
        observed_at: u64,
        block_time: u64,
    },
    /// The network's fees are above the relayer's cap: no push, rather than a fee burn.
    FeesAboveCap {
        max_fee: u128,
        cap: u128,
    },
    /// The push needs more gas than the cap: no push, rather than a transaction bound to run out of gas.
    GasAboveCap {
        estimate: u64,
        cap: u64,
    },
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rpc(m) => write!(f, "rpc: {m}"),
            Self::Reverted(m) => write!(f, "refused by the feed: {m}"),
            Self::FromTheFuture { observed_at, block_time } => {
                write!(f, "refused by the feed: observed at {observed_at}, after the chain's clock {block_time}")
            }
            Self::FeesAboveCap { max_fee, cap } => write!(f, "max fee {max_fee} wei/gas above the cap {cap}"),
            Self::GasAboveCap { estimate, cap } => write!(f, "gas estimate {estimate} above the cap {cap}"),
        }
    }
}

/// The chain operations the relayer needs (a trait, so the cycle can be tested without a chain).
pub trait FeedChain: Sync {
    fn setup(&self) -> impl Future<Output = Result<FeedSetup, ChainError>> + Send;
    /// The stored price and observation time.
    fn latest(&self) -> impl Future<Output = Result<(U256, u64), ChainError>> + Send;
    /// `submit` as an eth_call from the relayer: Ok if the push would be accepted now.
    fn preflight(
        &self,
        attestation: &Attestation,
        proof: &Bytes,
    ) -> impl Future<Output = Result<(), ChainError>> + Send;
    fn submit(
        &self,
        attestation: &Attestation,
        proof: &Bytes,
    ) -> impl Future<Output = Result<Pushed, ChainError>> + Send;
}

#[derive(Clone, Copy, Debug)]
pub struct FeeCaps {
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub gas_limit: u64,
}

impl FeeCaps {
    /// Arbitrum Sepolia's base fee is a fraction of a gwei. A push executes in ~260 k gas, but Arbitrum's gas also
    /// prices the calldata's L1 cost, which moves with L1 fees: 2 M gas at up to 2 gwei bounds a push at 0.004 ETH.
    pub const TESTNET: Self =
        Self { max_fee_per_gas: 2_000_000_000, max_priority_fee_per_gas: 100_000_000, gas_limit: 2_000_000 };
}

/// The feed over JSON-RPC, pushing with the relayer's KMS key.
pub struct RpcFeed {
    provider: DynProvider,
    feed: Address,
    relayer: Address,
    caps: FeeCaps,
    receipt_timeout: Duration,
    /// The RPC URL and its parts that may hold an API key: never in an error message.
    secrets: Vec<String>,
}

/// The parts of an RPC URL that may hold a credential (the whole URL, its path, query and userinfo), longest first.
fn url_secrets(given: &str, url: &reqwest::Url) -> Vec<String> {
    let mut secrets = vec![given.to_string(), url.as_str().to_string()];
    if url.path().len() > 1 {
        secrets.push(url.path().to_string());
    }
    secrets.extend(url.query().map(str::to_string));
    secrets.extend(url.password().map(str::to_string));
    if !url.username().is_empty() {
        secrets.push(url.username().to_string());
    }
    secrets.retain(|s| s.len() > 1);
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    secrets
}

/// The error with every secret replaced by `<redacted>`.
fn redact(error: ChainError, secrets: &[String]) -> ChainError {
    let clean = |text: String| secrets.iter().fold(text, |text, secret| text.replace(secret.as_str(), "<redacted>"));
    match error {
        ChainError::Rpc(m) => ChainError::Rpc(clean(m)),
        ChainError::Reverted(m) => ChainError::Reverted(clean(m)),
        other => other,
    }
}

impl RpcFeed {
    /// Connects and checks the chain id (the signer refuses a transaction for another chain as well).
    pub async fn connect(
        rpc_url: &str,
        feed: Address,
        signer: AwsSigner,
        chain_id: u64,
        caps: FeeCaps,
    ) -> Result<Self, ChainError> {
        let relayer = signer.address();
        let url: reqwest::Url = rpc_url
            .parse()
            .map_err(|_| ChainError::Rpc("the RPC URL does not parse (not shown: it may hold a key)".into()))?;
        let secrets = url_secrets(rpc_url, &url);
        let provider = ProviderBuilder::new().wallet(EthereumWallet::from(signer)).connect_http(url).erased();
        let actual = provider.get_chain_id().await.map_err(|e| redact(rpc(e), &secrets))?;
        if actual != chain_id {
            return Err(ChainError::Rpc(format!("chain id {actual}, expected {chain_id}")));
        }
        Ok(Self { provider, feed, relayer, caps, receipt_timeout: Duration::from_secs(30), secrets })
    }

    pub fn relayer(&self) -> Address {
        self.relayer
    }

    /// The relayer's balance, wei: what pays for the pushes.
    pub async fn balance(&self) -> Result<U256, ChainError> {
        self.provider.get_balance(self.relayer).await.map_err(|e| redact(rpc(e), &self.secrets))
    }
}

fn rpc(e: impl fmt::Display) -> ChainError {
    ChainError::Rpc(e.to_string())
}

/// A contract error, decoded to the feed's custom error when the revert data is one.
fn revert(e: ContractError) -> ChainError {
    use IZkPriceFeed::IZkPriceFeedErrors as Feed;
    match e.as_decoded_interface_error::<Feed>() {
        Some(Feed::FromTheFuture(early)) => {
            ChainError::FromTheFuture { observed_at: early.observedAt, block_time: early.blockTime.saturating_to() }
        }
        Some(decoded) => ChainError::Reverted(format!("{decoded:?}")),
        None => match e {
            ContractError::TransportError(t) if t.as_error_resp().is_none() => ChainError::Rpc(t.to_string()),
            other => ChainError::Reverted(other.to_string()),
        },
    }
}

/// The chain calls, whose errors may still carry the RPC URL: the trait implementation below redacts them.
impl RpcFeed {
    async fn setup_unredacted(&self) -> Result<FeedSetup, ChainError> {
        let feed = IZkPriceFeed::new(self.feed, &self.provider);
        let provider_id = feed.providerId().call().await.map_err(rpc)?;
        let query_commitment = feed.queryCommitment().call().await.map_err(rpc)?;
        let max_submission_delay = feed.maxSubmissionDelay().call().await.map_err(rpc)?;
        let verifier = feed.verifier().call().await.map_err(rpc)?;
        let notary = IZkDecoRegistry::new(verifier, &self.provider).notaryOf(provider_id).call().await.map_err(rpc)?;
        Ok(FeedSetup { provider_id, query_commitment, notary, max_submission_delay })
    }

    async fn latest_unredacted(&self) -> Result<(U256, u64), ChainError> {
        let latest = IZkPriceFeed::new(self.feed, &self.provider).latestVerifiedPrice().call().await.map_err(rpc)?;
        Ok((latest.price1e18, latest.updatedAt))
    }

    async fn preflight_unredacted(&self, attestation: &Attestation, proof: &Bytes) -> Result<(), ChainError> {
        let feed = IZkPriceFeed::new(self.feed, &self.provider);
        feed.submit(attestation.clone(), proof.clone()).from(self.relayer).call().await.map(|_| ()).map_err(revert)
    }

    async fn submit_unredacted(&self, attestation: &Attestation, proof: &Bytes) -> Result<Pushed, ChainError> {
        let feed = IZkPriceFeed::new(self.feed, &self.provider);
        let call = feed.submit(attestation.clone(), proof.clone()).from(self.relayer);
        let fees = self.provider.estimate_eip1559_fees().await.map_err(rpc)?;
        if fees.max_fee_per_gas > self.caps.max_fee_per_gas {
            return Err(ChainError::FeesAboveCap { max_fee: fees.max_fee_per_gas, cap: self.caps.max_fee_per_gas });
        }
        let estimate = call.estimate_gas().await.map_err(revert)?;
        if estimate > self.caps.gas_limit {
            return Err(ChainError::GasAboveCap { estimate, cap: self.caps.gas_limit });
        }
        let gas = (estimate.saturating_mul(6) / 5).min(self.caps.gas_limit);
        let priority = fees.max_priority_fee_per_gas.min(self.caps.max_priority_fee_per_gas).min(fees.max_fee_per_gas);
        let pending = call
            .gas(gas)
            .max_fee_per_gas(fees.max_fee_per_gas)
            .max_priority_fee_per_gas(priority)
            .send()
            .await
            .map_err(revert)?;
        let receipt = pending.with_timeout(Some(self.receipt_timeout)).get_receipt().await.map_err(rpc)?;
        if !receipt.status() {
            return Err(ChainError::Reverted(format!("transaction {} reverted on chain", receipt.transaction_hash)));
        }
        Ok(Pushed {
            tx_hash: receipt.transaction_hash,
            gas_used: receipt.gas_used,
            block: receipt.block_number.unwrap_or_default(),
        })
    }
}

impl FeedChain for RpcFeed {
    async fn setup(&self) -> Result<FeedSetup, ChainError> {
        self.setup_unredacted().await.map_err(|e| redact(e, &self.secrets))
    }

    async fn latest(&self) -> Result<(U256, u64), ChainError> {
        self.latest_unredacted().await.map_err(|e| redact(e, &self.secrets))
    }

    async fn preflight(&self, attestation: &Attestation, proof: &Bytes) -> Result<(), ChainError> {
        self.preflight_unredacted(attestation, proof).await.map_err(|e| redact(e, &self.secrets))
    }

    async fn submit(&self, attestation: &Attestation, proof: &Bytes) -> Result<Pushed, ChainError> {
        self.submit_unredacted(attestation, proof).await.map_err(|e| redact(e, &self.secrets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_never_carry_the_rpc_urls_credentials() {
        let given = "https://user:pa55@arb-sepolia.example/v2/k3y-in-the-path?apikey=k3y-in-the-query";
        let secrets = url_secrets(given, &given.parse().unwrap());
        for text in [
            format!("error sending request for url ({given})"),
            "HTTP 401 for /v2/k3y-in-the-path".to_string(),
            "bad query apikey=k3y-in-the-query".to_string(),
            "auth pa55 refused".to_string(),
        ] {
            for error in [ChainError::Rpc(text.clone()), ChainError::Reverted(text.clone())] {
                let shown = redact(error, &secrets).to_string();
                for secret in ["k3y-in-the-path", "k3y-in-the-query", "pa55"] {
                    assert!(!shown.contains(secret), "{shown}");
                }
                assert!(shown.contains("<redacted>"), "{shown}");
            }
        }
        // A plain URL has nothing to hide but itself.
        let plain = url_secrets("http://127.0.0.1:8545", &"http://127.0.0.1:8545".parse().unwrap());
        assert_eq!(plain, ["http://127.0.0.1:8545/", "http://127.0.0.1:8545"]);
    }
}
