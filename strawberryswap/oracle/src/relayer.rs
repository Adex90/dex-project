//! The relayer: every ~15 s it asks the notary for a fresh notarized body, checks it, proves it, and pushes it to
//! `ZkPriceFeed`, decoupled from any rebalance (decision 4). Relaying is permissionless: the relayer is trusted for
//! nothing, it pays the gas, and anyone may run another one for liveness.
//!
//! One cycle:
//! 1. `POST <notary>/v1/attest`: the body, its commitment and the notary's signature.
//! 2. Check them as the contract will: provider, canonical body, commitment, the registered notary's low-s signature.
//! 3. Skip if the feed already holds this observation or a newer one.
//! 4. Prove (Groth16, ~1 s); the prover checks its five public signals against the attestation.
//! 5. Preflight `submit` with an eth_call; push it only if it would be accepted (waiting up to 4 s for the chain's
//!    clock to reach the observation's timestamp).
//! 6. Push: an EIP-1559 transaction signed by the relayer's KMS key, fees and gas capped.

use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};

use crate::body::SYMBOL_LEN;
use crate::chain::{Attestation, ChainError, FeedChain, FeedSetup, Pushed};
use crate::commit::query_commitment;
use crate::error_chain;
use crate::notary::NotarizedBody;
use crate::prover::Prover;

/// How long, and how many times, a cycle waits for the chain's clock to reach the observation's timestamp.
const CLOCK_WAIT: Duration = Duration::from_secs(1);
const MAX_CLOCK_WAITS: u32 = 4;

#[derive(Clone, Debug)]
pub struct RelayerConfig {
    /// The notary's base URL (it serves POST /v1/attest).
    pub notary_url: String,
    pub symbol: [u8; SYMBOL_LEN],
    /// The query salt: secret, a field element. Its commitment with the symbol must be the feed's queryCommitment.
    pub salt: U256,
    pub notary_timeout: Duration,
    /// The heartbeat: one cycle per interval (~15 s on testnet).
    pub interval: Duration,
}

/// What one cycle did.
#[derive(Debug)]
pub enum Outcome {
    Pushed {
        observed_at: u64,
        price: U256,
        pushed: Pushed,
        prove: Duration,
        total: Duration,
    },
    /// The feed already holds this observation or a newer one.
    NothingNew {
        observed_at: u64,
        latest: u64,
    },
    /// The notary's answer, or the feed's preflight, refused this observation.
    Refused(String),
    /// Something failed on the way (notary unreachable, prover, RPC).
    Failed(String),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pushed { observed_at, price, pushed, prove, total } => write!(
                f,
                "PUSHED observedAt={observed_at} price={price} tx={} block={} gas={} prove_ms={} total_ms={}",
                pushed.tx_hash,
                pushed.block,
                pushed.gas_used,
                prove.as_millis(),
                total.as_millis()
            ),
            Self::NothingNew { observed_at, latest } => {
                write!(f, "NOTHING_NEW observedAt={observed_at} latest={latest}")
            }
            Self::Refused(m) => write!(f, "REFUSED {m}"),
            Self::Failed(m) => write!(f, "FAILED {m}"),
        }
    }
}

/// Start-up checks: the salt's commitment is the feed's instrument, and the notary answering is the one the oracle
/// registered for the feed's provider.
pub async fn check_setup<C: FeedChain>(
    config: &RelayerConfig,
    chain: &C,
    http: &reqwest::Client,
) -> Result<FeedSetup, String> {
    let setup = chain.setup().await.map_err(|e| e.to_string())?;
    let ours = query_commitment(&config.symbol, config.salt).map_err(|e| format!("salt: {e}"))?;
    if ours != setup.query_commitment {
        return Err(format!(
            "the salt and symbol commit to {ours}, but the feed accepts {}: wrong salt, symbol or feed",
            setup.query_commitment
        ));
    }
    if setup.notary == Address::ZERO {
        return Err(format!("no notary registered for provider {}", setup.provider_id));
    }
    let info: serde_json::Value = http
        .get(format!("{}/v1/info", config.notary_url.trim_end_matches('/')))
        .timeout(config.notary_timeout)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("notary info: {e}"))?
        .json()
        .await
        .map_err(|e| format!("notary info: {e}"))?;
    let notary: Option<Address> = info["notary"].as_str().and_then(|a| a.parse().ok());
    let provider: Option<B256> = info["providerId"].as_str().and_then(|p| p.parse().ok());
    if notary != Some(setup.notary) || provider != Some(setup.provider_id) {
        return Err(format!(
            "the notary is {notary:?} for provider {provider:?}, but the feed's oracle registered {} for {}",
            setup.notary, setup.provider_id
        ));
    }
    Ok(setup)
}

async fn notarize(config: &RelayerConfig, http: &reqwest::Client) -> Result<NotarizedBody, String> {
    let response = http
        .post(format!("{}/v1/attest", config.notary_url.trim_end_matches('/')))
        .timeout(config.notary_timeout)
        .send()
        .await
        .map_err(|e| format!("notary: {}", error_chain(&e)))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!("notary: HTTP {status}: {}", text.chars().take(200).collect::<String>()));
    }
    response.json().await.map_err(|e| format!("notary: {e}"))
}

/// One cycle.
pub async fn cycle<C: FeedChain>(
    config: &RelayerConfig,
    setup: &FeedSetup,
    chain: &C,
    http: &reqwest::Client,
    prover: &Prover,
) -> Outcome {
    let started = Instant::now();
    let notarized = match notarize(config, http).await {
        Ok(n) => n,
        Err(e) => return Outcome::Failed(e),
    };
    let body = match notarized.verify(setup.provider_id, setup.notary) {
        Ok(body) => body,
        Err(e) => return Outcome::Refused(format!("notarized body: {e}")),
    };
    if body.symbol() != config.symbol {
        return Outcome::Refused(format!("the notary attested {}, not our instrument", body.symbol_str()));
    }
    let latest = match chain.latest().await {
        Ok((_, at)) => at,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    if body.observed_at() <= latest {
        return Outcome::NothingNew { observed_at: body.observed_at(), latest };
    }

    let word = |b: B256| U256::from_be_bytes(b.0);
    let expected = [
        word(setup.provider_id),
        word(setup.query_commitment),
        body.price_wad(),
        U256::from(body.observed_at()),
        word(notarized.body_commitment),
    ];
    let witness = body.witness(setup.provider_id, setup.query_commitment, notarized.body_commitment, config.salt);
    let proving = Instant::now();
    let snark = match prover.prove(&witness, &expected).await {
        Ok(snark) => snark,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let prove = proving.elapsed();

    let attestation = Attestation {
        providerId: setup.provider_id,
        queryCommitment: setup.query_commitment,
        price: body.price_wad(),
        observedAt: body.observed_at(),
    };
    let proof = notarized.proof(&snark);
    // The observation is stamped by the endpoint's clock; the chain's may trail it by a second or two. Wait for it
    // (bounded) rather than drop the observation.
    let mut clock_waits = 0;
    loop {
        match chain.preflight(&attestation, &proof).await {
            Ok(()) => break,
            Err(ChainError::FromTheFuture { .. }) if clock_waits < MAX_CLOCK_WAITS => {
                clock_waits += 1;
                tokio::time::sleep(CLOCK_WAIT).await;
            }
            Err(e) => return Outcome::Refused(e.to_string()),
        }
    }
    match chain.submit(&attestation, &proof).await {
        Ok(pushed) => Outcome::Pushed {
            observed_at: body.observed_at(),
            price: body.price_wad(),
            pushed,
            prove,
            total: started.elapsed(),
        },
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub cycles: u64,
    pub pushed: u64,
    pub nothing_new: u64,
    pub refused: u64,
    pub failed: u64,
}

/// Cycles every `interval` until `max_cycles` or `shutdown`, printing one line per cycle. A slow cycle delays the
/// next one rather than bunching them.
pub async fn run<C: FeedChain>(
    config: &RelayerConfig,
    setup: &FeedSetup,
    chain: &C,
    http: &reqwest::Client,
    prover: &Prover,
    max_cycles: Option<u64>,
    shutdown: impl Future<Output = ()>,
) -> Stats {
    let mut stats = Stats::default();
    let mut tick = tokio::time::interval(config.interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    while max_cycles.is_none_or(|n| stats.cycles < n) {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            _ = tick.tick() => {}
        }
        let outcome = cycle(config, setup, chain, http, prover).await;
        stats.cycles += 1;
        match &outcome {
            Outcome::Pushed { .. } => stats.pushed += 1,
            Outcome::NothingNew { .. } => stats.nothing_new += 1,
            Outcome::Refused(_) => stats.refused += 1,
            Outcome::Failed(_) => stats.failed += 1,
        }
        println!("{outcome}");
    }
    stats
}
