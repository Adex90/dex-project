//! StrawberrySwap HFT execution engine, iteration 1: Avellaneda–Stoikov on a 100 ms tokio loop, rebalancing the
//! DarkPool through `SearcherExecutor.rebalance` transactions that the threshold vault signs.
//!
//! Every tick:
//!
//! 1. read the chain within FETCH_BUDGET: the zk oracle, the executor's inventory, the pool and the operator's nonce
//!    (`chain`);
//! 2. quote r ∓ δ/2 for the inventory, with the Python lab's closed forms (`model`);
//! 3. plan the rebalance those prices make worth its gas against the pool: bots/inventory-cpp's closed form, ported bit
//!    for bit (`planner`);
//! 4. build the unsigned EIP-1559 transaction (`tx`);
//! 5. have the vault sign it over its Unix socket, within the vault deadline, and check what comes back: the same
//!    transaction, signed by the vault's address (`vault`);
//! 6. broadcast it.
//!
//! The engine holds no key and links no signer crate. A rebalance whose signature does not come back in time, or does
//! not check out, is withdrawn: nothing is broadcast for it, and the next tick plans afresh on new reads.
//!
//! MOCKED in this iteration: the chain (`chain::MockChain`, the C++ engine's SimChain behind a transaction boundary).
//! The vault is the real daemon (strawberryswap/vaults/threshold-signer).

#![forbid(unsafe_code)]

mod chain;
mod model;
mod planner;
mod tx;
mod vault;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod lab_golden;
#[cfg(test)]
mod planner_golden;

use std::future::Future;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use alloy::consensus::TxEip1559;
use alloy::primitives::{Address, B256, address, hex};
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::chain::{Chain, ChainState, MockChain, Receipt, TxStatus};
use crate::model::{AsParams, EngineError, fresh_price, unix_now, wad_to_f64};
use crate::planner::{Plan, PlannerParams, plan};
use crate::tx::{Rebalance, TxConfig, spendable, unsigned_tx};
use crate::vault::{SignedTx, TxSigner, VaultClient, VaultError};

/// Tick period: one read, one decision, at most one signed transaction.
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// Budget of a tick's reads, and of a broadcast. A late read skips the tick.
pub(crate) const FETCH_BUDGET: Duration = Duration::from_millis(80);
/// Default deadline of a signature: the vault is local, so a round trip takes well under a millisecond.
const VAULT_TIMEOUT: Duration = Duration::from_millis(15);

// ---------------------------------------------------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------------------------------------------------

/// A rebalance ready for the vault.
#[derive(Clone, Debug, PartialEq)]
struct Order {
    plan: Plan,
    rebalance: Rebalance,
    tx: TxEip1559,
}

/// The engine's state between ticks: the model, the planner's costs, the transaction template, the inventory target,
/// what the last tick saw, and the counters.
struct HFTState {
    params: AsParams,
    /// Gas cost and minimum net profit; the inventory limits are set every tick from the executor's balances.
    planner: PlannerParams,
    tx: TxConfig,
    /// The executor's token0 target: the model's inventory q is the balance minus this.
    target_token0: f64,
    /// The last fresh oracle price and inventory.
    mid: Option<f64>,
    inventory: Option<f64>,
    stats: Stats,
}

impl HFTState {
    fn new(
        params: AsParams,
        planner: PlannerParams,
        tx: TxConfig,
        target_token0: f64,
    ) -> Result<Self, EngineError> {
        params.validate()?;
        let non_negative = |x: f64| x.is_finite() && x >= 0.0;
        if !non_negative(target_token0) {
            return Err(EngineError::Params(
                "the token0 target must be non-negative and finite",
            ));
        }
        if !non_negative(planner.gas_cost_quote) || !non_negative(planner.min_net) {
            return Err(EngineError::Params(
                "the gas cost and the minimum net must be non-negative and finite",
            ));
        }
        if tx.max_priority_fee_per_gas > tx.max_fee_per_gas {
            return Err(EngineError::Params(
                "the priority fee cannot exceed the max fee",
            ));
        }
        Ok(Self {
            params,
            planner,
            tx,
            target_token0,
            mid: None,
            inventory: None,
            stats: Stats::default(),
        })
    }

    /// The rebalance worth sending on this tick's reads, if any (no I/O). Errors when the reads cannot be acted on: a
    /// stale oracle, a value that does not convert, a non-finite quote.
    fn decide(&mut self, reads: &ChainState, now: u64) -> Result<Option<Order>, EngineError> {
        let mid = wad_to_f64(fresh_price(reads.oracle, now)?)?;
        if mid <= 0.0 {
            return Err(EngineError::Conversion("the oracle price is zero".into()));
        }
        let (token0, token1) = (wad_to_f64(reads.token0)?, wad_to_f64(reads.token1)?);
        let inventory = token0 - self.target_token0;
        self.mid = Some(mid);
        self.inventory = Some(inventory);
        let (r, half) = self.params.quote(mid, inventory)?;
        let pool = reads.pool.to_tokens()?;
        let limits = PlannerParams {
            max_inv0: spendable(token0),
            max_inv1: spendable(token1),
            ..self.planner
        };
        let Some(plan) = plan(&pool, mid, r - half, r + half, &limits) else {
            return Ok(None);
        };
        let rebalance = Rebalance::from_plan(&plan)?;
        let tx = unsigned_tx(&self.tx, reads.nonce, &rebalance);
        Ok(Some(Order {
            plan,
            rebalance,
            tx,
        }))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stats {
    ticks: u64,
    /// Ticks whose reads were fresh but offered no rebalance worth its gas.
    idle: u64,
    /// Ticks skipped before planning: a late or failed read, a stale oracle, a value that did not convert.
    skipped: u64,
    /// Rebalances withdrawn because the vault did not sign in time, refused, or answered with something other than
    /// our transaction signed by it. Nothing was broadcast for them.
    withdrawn: u64,
    /// Rebalances signed, checked and included.
    sent: u64,
    /// Of those, the ones that reverted.
    reverted: u64,
    /// Broadcasts the chain did not include (rejected, or no answer within the budget).
    rejected: u64,
    /// The slowest tick, microseconds.
    slowest_us: u64,
}

/// What one tick did.
#[derive(Debug)]
enum Outcome {
    Idle,
    Skipped(EngineError),
    Withdrawn {
        nonce: u64,
        reason: VaultError,
    },
    Sent {
        nonce: u64,
        receipt: Receipt,
        raw: Vec<u8>,
    },
    Rejected {
        nonce: u64,
        hash: B256,
        error: EngineError,
    },
}

/// How the loop runs.
#[derive(Clone, Copy, Debug)]
struct RunOptions {
    /// Stop after this many ticks (None: until shutdown).
    max_ticks: Option<u64>,
    /// Deadline of each signature.
    vault_timeout: Duration,
    /// A status line every this many ticks (0: none).
    log_every: u64,
    /// Print each broadcast transaction in full.
    log_txs: bool,
    /// Print events (sent, withdrawn, skipped); off in the tests.
    verbose: bool,
}

/// One tick: read, decide, sign within the deadline, check, broadcast.
async fn tick<C: Chain, S: TxSigner>(
    state: &mut HFTState,
    chain: &mut C,
    signer: &mut S,
    opts: &RunOptions,
) -> Outcome {
    let reads = match time::timeout(FETCH_BUDGET, chain.read()).await {
        Ok(Ok(reads)) => reads,
        Ok(Err(e)) => return Outcome::Skipped(e),
        Err(_) => return Outcome::Skipped(EngineError::Timeout),
    };
    let order = match state.decide(&reads, unix_now()) {
        Ok(Some(order)) => order,
        Ok(None) => return Outcome::Idle,
        Err(e) => return Outcome::Skipped(e),
    };
    let nonce = order.tx.nonce;
    // The deadline is enforced here for any signer; the vault client also bounds its own calls, and is safe to cancel.
    let signed: SignedTx = match time::timeout(opts.vault_timeout, signer.sign(&order.tx)).await {
        Ok(Ok(signed)) => signed,
        Ok(Err(reason)) => return Outcome::Withdrawn { nonce, reason },
        Err(_) => {
            return Outcome::Withdrawn {
                nonce,
                reason: VaultError::Timeout,
            };
        }
    };
    match time::timeout(FETCH_BUDGET, chain.send_raw_transaction(&signed.raw)).await {
        Ok(Ok(receipt)) => Outcome::Sent {
            nonce,
            receipt,
            raw: signed.raw,
        },
        Ok(Err(error)) => Outcome::Rejected {
            nonce,
            hash: signed.hash,
            error,
        },
        Err(_) => Outcome::Rejected {
            nonce,
            hash: signed.hash,
            error: EngineError::Timeout,
        },
    }
}

/// The event loop: a tick every TICK until `max_ticks` or `shutdown`. A tick that overruns skips the ticks it missed
/// instead of bunching them. Repeated identical notes (the same skip or withdrawal reason) are printed once.
async fn run<C: Chain, S: TxSigner>(
    state: &mut HFTState,
    chain: &mut C,
    signer: &mut S,
    opts: RunOptions,
    shutdown: impl Future<Output = ()>,
) {
    let mut interval = time::interval(TICK);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tokio::pin!(shutdown);
    let start = Instant::now();
    let mut last_note = String::new();
    while opts.max_ticks.is_none_or(|n| state.stats.ticks < n) {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            _ = interval.tick() => {}
        }
        let tick_start = Instant::now();
        let outcome = tick(state, chain, signer, &opts).await;
        let stats = &mut state.stats;
        stats.ticks += 1;
        let note = match &outcome {
            Outcome::Idle => {
                stats.idle += 1;
                None
            }
            Outcome::Skipped(e) => {
                stats.skipped += 1;
                Some(format!("SKIPPED: {e}"))
            }
            Outcome::Withdrawn { nonce, reason } => {
                stats.withdrawn += 1;
                Some(format!("WITHDRAWN nonce={nonce}: {reason}"))
            }
            Outcome::Rejected { nonce, hash, error } => {
                stats.rejected += 1;
                Some(format!("REJECTED nonce={nonce} tx={hash}: {error}"))
            }
            Outcome::Sent {
                nonce,
                receipt,
                raw,
            } => {
                stats.sent += 1;
                let status = match &receipt.status {
                    TxStatus::Success => "success".to_string(),
                    TxStatus::Reverted(reason) => {
                        stats.reverted += 1;
                        format!("reverted ({reason})")
                    }
                };
                if opts.verbose {
                    let raw = if opts.log_txs {
                        format!(" raw={}", hex::encode_prefixed(raw))
                    } else {
                        String::new()
                    };
                    println!(
                        "SENT nonce={nonce} tx={} signer={} status={status}{raw}",
                        receipt.tx_hash, receipt.signer
                    );
                }
                last_note.clear();
                None
            }
        };
        if let Some(note) = note
            && note != last_note
        {
            if opts.verbose {
                println!("{note}");
            }
            last_note = note;
        }
        let elapsed = u64::try_from(tick_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats.slowest_us = stats.slowest_us.max(elapsed);
        if opts.verbose && opts.log_every > 0 && stats.ticks.is_multiple_of(opts.log_every) {
            log_status(start.elapsed(), state);
        }
    }
}

fn log_status(elapsed: Duration, state: &HFTState) {
    let s = &state.stats;
    let show = |x: Option<f64>, digits: usize| {
        x.map_or_else(|| "—".to_string(), |x| format!("{x:.digits$}"))
    };
    println!(
        "t={:>7.1}s  mid {}  q {}  · sent {} (reverted {}) · withdrawn {} · skipped {} · idle {}",
        elapsed.as_secs_f64(),
        show(state.mid, 6),
        show(state.inventory, 1),
        s.sent,
        s.reverted,
        s.withdrawn,
        s.skipped,
        s.idle
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------------------------------------------------

const USAGE: &str = "usage: hft-engine [--vault-socket PATH] [--vault-timeout-ms MS] [--chain-id ID] [--executor ADDRESS]
                  [--target-token0 TOKENS] [--calibration PATH] [--ticks N] [--mid PRICE] [--seed N] [--log-txs]

Runs the Avellaneda–Stoikov engine (100 ms ticks) against a MOCK chain. Every rebalance is signed by
the threshold vault over its Unix socket; the engine holds no key. A rebalance the vault does not sign
within the deadline is withdrawn.

  --vault-socket PATH     the vault's socket (default: the vault's own default,
                          $XDG_RUNTIME_DIR/strawberryswap-vault/vault.sock)
  --vault-timeout-ms MS   deadline of each signature (default: 15)
  --chain-id ID           chain id of the transactions (default: 31337)
  --executor ADDRESS      the SearcherExecutor contract (default: 0x5FbDB2315678afecb367f032d93F642f64180aa3)
  --target-token0 TOKENS  the executor's token0 target; the model's inventory is the balance minus it
                          (default: 20000, the mock executor's starting balance)
  --calibration PATH      the Python lab's calibration.json (its `engine` block); default: mock parameters
  --ticks N               stop after N ticks (default: run until Ctrl-C or SIGTERM)
  --mid PRICE             starting price of the mock chain (default: 2)
  --seed N                seed of the mock chain (default: 7)
  --log-txs               print every broadcast transaction in full (hex)";

/// The mock executor's address: the first contract Anvil's default deployer creates.
const MOCK_EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");
/// Mock transaction fields: well inside the vault's default caps (1 000 000 gas, 500 gwei).
const MOCK_GAS_LIMIT: u64 = 300_000;
const MOCK_MAX_FEE_PER_GAS: u128 = 30_000_000_000;
const MOCK_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;
/// Mock costs, token1: the gas of a rebalance, and the least net profit worth sending one for.
const MOCK_GAS_COST: f64 = 0.01;
const MOCK_MIN_NET: f64 = 0.01;

#[derive(Debug, PartialEq)]
struct Args {
    vault_socket: Option<PathBuf>,
    vault_timeout: Duration,
    chain_id: u64,
    executor: Address,
    target_token0: f64,
    calibration: Option<PathBuf>,
    ticks: Option<u64>,
    mid: f64,
    seed: u64,
    log_txs: bool,
}

impl Args {
    /// Parses the arguments; Err(None) asks for the usage text.
    fn parse(mut it: impl Iterator<Item = String>) -> Result<Self, Option<String>> {
        fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, Option<String>> {
            text.parse()
                .map_err(|_| Some(format!("{flag}: {text} is not a valid number")))
        }
        let mut args = Self {
            vault_socket: None,
            vault_timeout: VAULT_TIMEOUT,
            chain_id: 31_337,
            executor: MOCK_EXECUTOR,
            target_token0: 20_000.0,
            calibration: None,
            ticks: None,
            mid: 2.0,
            seed: 7,
            log_txs: false,
        };
        while let Some(flag) = it.next() {
            match flag.as_str() {
                "-h" | "--help" => return Err(None),
                "--log-txs" => {
                    args.log_txs = true;
                    continue;
                }
                _ => {}
            }
            let value = it
                .next()
                .ok_or_else(|| Some(format!("{flag} needs a value")))?;
            match flag.as_str() {
                "--vault-socket" => args.vault_socket = Some(PathBuf::from(value)),
                "--vault-timeout-ms" => {
                    let ms: u64 = number(&flag, &value)?;
                    if !(1..=10_000).contains(&ms) {
                        return Err(Some("--vault-timeout-ms must be in [1, 10000]".into()));
                    }
                    args.vault_timeout = Duration::from_millis(ms);
                }
                "--chain-id" => args.chain_id = number(&flag, &value)?,
                "--executor" => {
                    let executor: Address = value
                        .parse()
                        .map_err(|_| Some(format!("--executor: {value} is not an address")))?;
                    if executor.is_zero() {
                        return Err(Some(
                            "--executor: the zero address is not a contract".into(),
                        ));
                    }
                    args.executor = executor;
                }
                "--target-token0" => {
                    let target: f64 = number(&flag, &value)?;
                    if !(target.is_finite() && target >= 0.0) {
                        return Err(Some("--target-token0 must be a non-negative amount".into()));
                    }
                    args.target_token0 = target;
                }
                "--calibration" => args.calibration = Some(PathBuf::from(value)),
                "--ticks" => args.ticks = Some(number(&flag, &value)?),
                "--mid" => {
                    let mid: f64 = number(&flag, &value)?;
                    if !(mid.is_finite() && mid > 0.0) {
                        return Err(Some("--mid must be a positive price".into()));
                    }
                    args.mid = mid;
                }
                "--seed" => args.seed = number(&flag, &value)?,
                other => return Err(Some(format!("unknown argument {other}"))),
            }
        }
        Ok(args)
    }
}

/// The vault's default socket (vaults/threshold-signer, `default_socket`): `$XDG_RUNTIME_DIR/strawberryswap-vault/
/// vault.sock`, or `<tmp>/strawberryswap-vault-<uid>/vault.sock`.
fn default_vault_socket() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir)
            .join("strawberryswap-vault")
            .join("vault.sock"),
        _ => {
            // The uid owns /proc/self (Linux; no libc call, so no unsafe code).
            let uid = std::fs::metadata("/proc/self").map_or(0, |m| m.uid());
            std::env::temp_dir()
                .join(format!("strawberryswap-vault-{uid}"))
                .join("vault.sock")
        }
    }
}

/// Ctrl-C or SIGTERM. A handler that cannot be installed never fires (the engine then runs until --ticks).
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(Some(message)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (params, source) = match &args.calibration {
        Some(path) => match AsParams::from_calibration(path) {
            Ok(params) => (params, path.display().to_string()),
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        },
        None => (AsParams::MOCK, "mock parameters".into()),
    };
    let tx = TxConfig {
        chain_id: args.chain_id,
        executor: args.executor,
        gas_limit: MOCK_GAS_LIMIT,
        max_fee_per_gas: MOCK_MAX_FEE_PER_GAS,
        max_priority_fee_per_gas: MOCK_PRIORITY_FEE_PER_GAS,
    };
    let planner = PlannerParams {
        gas_cost_quote: MOCK_GAS_COST,
        min_net: MOCK_MIN_NET,
        ..PlannerParams::default()
    };
    let mut state = match HFTState::new(params, planner, tx, args.target_token0) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let socket = args
        .vault_socket
        .clone()
        .unwrap_or_else(default_vault_socket);
    let mut vault = VaultClient::new(socket.clone(), args.vault_timeout);
    // The mock SearcherExecutor is deployed with the vault's address as its operator, as the real one is configured.
    // Without a vault the engine still runs, and withdraws every rebalance until the vault answers.
    let operator = match vault.fetch_address().await {
        Ok(address) => address,
        Err(e) => {
            eprintln!("warning: {e}; every rebalance will be withdrawn until the vault answers");
            Address::ZERO
        }
    };
    let mut chain = MockChain::new(
        args.mid,
        params.sigma2_per_sec,
        args.seed,
        args.chain_id,
        args.executor,
        operator,
    );

    println!(
        "hft-engine: MOCK chain, no RPC; {} ms ticks, {} ms read budget; no key in this process",
        TICK.as_millis(),
        FETCH_BUDGET.as_millis()
    );
    println!(
        "vault: {} · signer {} · {} ms deadline",
        socket.display(),
        vault
            .address()
            .map_or_else(|| "unknown (unavailable)".to_string(), |a| a.to_string()),
        args.vault_timeout.as_millis()
    );
    println!(
        "model: γ {} · σ² {} /s · τ {} s · κ {}  ({source})",
        params.gamma, params.sigma2_per_sec, params.horizon_sec, params.kappa
    );
    println!(
        "executor: {} on chain {} · target {} token0 · gas {} · max fee {} gwei",
        args.executor,
        args.chain_id,
        args.target_token0,
        MOCK_GAS_LIMIT,
        MOCK_MAX_FEE_PER_GAS / 1_000_000_000
    );

    let opts = RunOptions {
        max_ticks: args.ticks,
        vault_timeout: args.vault_timeout,
        log_every: 10,
        log_txs: args.log_txs,
        verbose: true,
    };
    run(&mut state, &mut chain, &mut vault, opts, shutdown_signal()).await;
    let s = state.stats;
    println!(
        "summary ticks={} idle={} skipped={} withdrawn={} sent={} reverted={} rejected={} slowest_ms={:.3} \
         pool={:.6} oracle={:.6} treasury={:.6}",
        s.ticks,
        s.idle,
        s.skipped,
        s.withdrawn,
        s.sent,
        s.reverted,
        s.rejected,
        s.slowest_us as f64 / 1e3,
        chain.pool.spot(),
        chain.oracle,
        chain.treasury
    );
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::model::{OracleReading, wad};
    use alloy::consensus::SignableTransaction;
    use alloy::eips::eip2718::Encodable2718;
    use alloy::primitives::Signature;

    /// The loop tests' stand-in for the vault: the vault's signing step, in process, with the end-to-end tests' public
    /// key (fixtures::VAULT_ADDRESS). Test code only: k256 is a dev-dependency, never linked into the engine binary.
    struct InProcessSigner(k256::ecdsa::SigningKey);

    impl InProcessSigner {
        fn new() -> Self {
            Self(k256::ecdsa::SigningKey::from_slice(fixtures::test_key().as_slice()).unwrap())
        }
    }

    impl TxSigner for InProcessSigner {
        async fn sign(&mut self, tx: &TxEip1559) -> Result<SignedTx, VaultError> {
            let (signature, recovery) = self
                .0
                .sign_prehash_recoverable(tx.signature_hash().as_slice())
                .unwrap();
            let signed = tx.clone().into_signed(Signature::from_signature_and_parity(
                signature,
                recovery.is_y_odd(),
            ));
            Ok(SignedTx {
                raw: signed.encoded_2718(),
                hash: *signed.hash(),
            })
        }
    }

    /// A vault that is too slow, refuses, or never answers.
    enum BadSigner {
        Slow(Duration),
        Refusing,
        Silent,
    }

    impl TxSigner for BadSigner {
        async fn sign(&mut self, tx: &TxEip1559) -> Result<SignedTx, VaultError> {
            match self {
                Self::Slow(delay) => {
                    time::sleep(*delay).await;
                    InProcessSigner::new().sign(tx).await
                }
                Self::Refusing => Err(VaultError::Refused {
                    code: "policy".into(),
                    message: "no".into(),
                }),
                Self::Silent => std::future::pending().await,
            }
        }
    }

    fn mock_state() -> HFTState {
        let planner = PlannerParams {
            gas_cost_quote: MOCK_GAS_COST,
            min_net: MOCK_MIN_NET,
            ..PlannerParams::default()
        };
        HFTState::new(AsParams::MOCK, planner, fixtures::CONFIG, 20_000.0).unwrap()
    }

    fn mock_chain(seed: u64) -> MockChain {
        MockChain::new(
            2.0,
            AsParams::MOCK.sigma2_per_sec,
            seed,
            31_337,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
        )
    }

    fn quiet(max_ticks: u64) -> RunOptions {
        RunOptions {
            max_ticks: Some(max_ticks),
            vault_timeout: VAULT_TIMEOUT,
            log_every: 0,
            log_txs: false,
            verbose: false,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_loop_ticks_every_100_ms() {
        let (mut state, mut chain, mut signer) =
            (mock_state(), mock_chain(7), InProcessSigner::new());
        let start = Instant::now();
        run(
            &mut state,
            &mut chain,
            &mut signer,
            quiet(50),
            std::future::pending(),
        )
        .await;
        // The first tick fires at once; the 50th, 49 ticks later.
        assert_eq!(start.elapsed().as_millis() / 100, 49);
        assert_eq!(state.stats.ticks, 50);
    }

    /// The whole path on the mock chain: thousands of ticks, every rebalance signed, included from the operator and
    /// accepted by the contract's rules; the pool tracks the oracle, the inventory stays near its target, and the
    /// Treasury is paid.
    #[tokio::test(start_paused = true)]
    async fn rebalances_are_signed_included_and_keep_the_pool_on_the_oracle() {
        let (mut state, mut chain, mut signer) =
            (mock_state(), mock_chain(7), InProcessSigner::new());
        let (mut deviation, mut worst) = (0.0, 0.0f64);
        for chunk in 1..=50 {
            run(
                &mut state,
                &mut chain,
                &mut signer,
                quiet(chunk * 100),
                std::future::pending(),
            )
            .await;
            let d = (chain.pool.spot() / chain.oracle - 1.0).abs();
            deviation += d / 50.0;
            worst = worst.max(d);
            assert!(
                (10_000.0..=30_000.0).contains(&chain.token0),
                "token0 {}",
                chain.token0
            );
        }
        let s = state.stats;
        assert_eq!(s.ticks, 5_000);
        assert!(s.sent > 100, "{s:?}");
        assert_eq!(
            (s.reverted, s.rejected, s.withdrawn, s.skipped),
            (0, 0, 0, 0),
            "{s:?}"
        );
        assert_eq!(s.idle + s.sent, s.ticks);
        assert_eq!(
            (chain.nonce, chain.included, chain.reverted),
            (s.sent, s.sent, 0)
        );
        assert!(chain.treasury > 0.0);
        assert!(
            deviation < 0.01 && worst < 0.03,
            "mean |pool/oracle − 1| {deviation}, worst {worst}"
        );
    }

    /// The vault is the only way to sign: if it is late, refuses or never answers, the rebalance is withdrawn within
    /// the deadline, nothing reaches the chain, and the loop keeps its pace.
    #[tokio::test(start_paused = true)]
    async fn a_late_refusing_or_silent_vault_withdraws_every_rebalance() {
        for mut signer in [
            BadSigner::Slow(VAULT_TIMEOUT * 2),
            BadSigner::Refusing,
            BadSigner::Silent,
        ] {
            let (mut state, mut chain) = (mock_state(), mock_chain(7));
            let start = Instant::now();
            run(
                &mut state,
                &mut chain,
                &mut signer,
                quiet(1_000),
                std::future::pending(),
            )
            .await;
            let s = state.stats;
            assert!(s.withdrawn > 50, "{s:?}");
            assert_eq!(
                (s.sent, s.rejected, chain.nonce, chain.included),
                (0, 0, 0, 0),
                "{s:?}"
            );
            assert_eq!(s.idle + s.withdrawn, s.ticks);
            assert!(
                start.elapsed() < TICK * 1_000,
                "the deadline keeps the loop on its 100 ms pace"
            );
        }
    }

    /// A vault that answers in time again after an outage: quoting resumes on fresh plans, at the chain's nonce.
    #[tokio::test(start_paused = true)]
    async fn quoting_resumes_when_the_vault_recovers() {
        let (mut state, mut chain) = (mock_state(), mock_chain(7));
        run(
            &mut state,
            &mut chain,
            &mut BadSigner::Silent,
            quiet(300),
            std::future::pending(),
        )
        .await;
        let withdrawn = state.stats.withdrawn;
        assert!(withdrawn > 0 && state.stats.sent == 0);
        run(
            &mut state,
            &mut chain,
            &mut InProcessSigner::new(),
            quiet(600),
            std::future::pending(),
        )
        .await;
        assert!(state.stats.sent > 0 && state.stats.reverted == 0);
        assert_eq!(state.stats.withdrawn, withdrawn);
        assert_eq!(chain.nonce, state.stats.sent);
    }

    #[tokio::test(start_paused = true)]
    async fn late_reads_skip_the_tick() {
        let (mut state, mut chain, mut signer) =
            (mock_state(), mock_chain(7), InProcessSigner::new());
        chain.latency = FETCH_BUDGET * 2;
        run(
            &mut state,
            &mut chain,
            &mut signer,
            quiet(20),
            std::future::pending(),
        )
        .await;
        assert_eq!((state.stats.skipped, state.stats.sent), (20, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_the_loop() {
        let (mut state, mut chain, mut signer) =
            (mock_state(), mock_chain(7), InProcessSigner::new());
        let shutdown = time::sleep(Duration::from_millis(1_050));
        run(
            &mut state,
            &mut chain,
            &mut signer,
            RunOptions {
                max_ticks: None,
                ..quiet(0)
            },
            shutdown,
        )
        .await;
        assert_eq!(state.stats.ticks, 11);
    }

    #[tokio::test(start_paused = true)]
    async fn runs_are_reproducible_for_a_seed() {
        let mut runs = Vec::new();
        for _ in 0..2 {
            let (mut state, mut chain, mut signer) =
                (mock_state(), mock_chain(11), InProcessSigner::new());
            run(
                &mut state,
                &mut chain,
                &mut signer,
                quiet(500),
                std::future::pending(),
            )
            .await;
            runs.push((
                state.stats.sent,
                state.stats.idle,
                chain.pool,
                chain.token0,
                chain.token1,
                chain.treasury,
            ));
        }
        assert_eq!(runs[0], runs[1]);
    }

    /// The decision on the C++ engine's scenario: the pool at 2000 over an oracle at 1985 is sold token0 into.
    #[test]
    fn decide_sells_token0_into_an_expensive_pool_and_respects_the_balances() {
        let mut state = mock_state();
        state.target_token0 = 50.0;
        let now = unix_now();
        let reads = ChainState {
            oracle: OracleReading {
                price: wad("1985"),
                updated_at: now,
            },
            ..fixtures::scenario_reads(now)
        };
        let order = state.decide(&reads, now).unwrap().expect("an opportunity");
        assert!(order.plan.sells_token0);
        assert!(order.plan.pay0 <= 50.0 && order.plan.amount1_out > 0.0);
        assert_eq!(order.tx.nonce, reads.nonce);
        assert_eq!(
            order.tx.to,
            alloy::primitives::TxKind::Call(fixtures::EXECUTOR)
        );
        assert_eq!(Rebalance::decode(&order.tx.input), Some(order.rebalance));
        // With no token0 to sell, nothing is planned.
        let empty = ChainState {
            token0: alloy::primitives::U256::ZERO,
            ..reads
        };
        assert_eq!(state.decide(&empty, now), Ok(None));
    }

    #[test]
    fn decide_refuses_stale_or_unconvertible_reads() {
        let mut state = mock_state();
        let now = unix_now();
        let stale = fixtures::scenario_reads(now - 60);
        assert!(matches!(
            state.decide(&stale, now),
            Err(EngineError::StaleOracle { .. })
        ));
        let future = fixtures::scenario_reads(now + 60);
        assert!(matches!(
            state.decide(&future, now),
            Err(EngineError::StaleOracle { .. })
        ));
        let zero = ChainState {
            oracle: OracleReading {
                price: alloy::primitives::U256::ZERO,
                updated_at: now,
            },
            ..fixtures::scenario_reads(now)
        };
        assert!(matches!(
            state.decide(&zero, now),
            Err(EngineError::Conversion(_))
        ));
    }

    #[test]
    fn the_state_rejects_parameters_outside_their_domain() {
        let planner = PlannerParams::default();
        assert!(
            HFTState::new(
                AsParams {
                    gamma: 0.0,
                    ..AsParams::MOCK
                },
                planner,
                fixtures::CONFIG,
                0.0
            )
            .is_err()
        );
        assert!(HFTState::new(AsParams::MOCK, planner, fixtures::CONFIG, -1.0).is_err());
        assert!(
            HFTState::new(
                AsParams::MOCK,
                PlannerParams {
                    min_net: f64::NAN,
                    ..planner
                },
                fixtures::CONFIG,
                0.0
            )
            .is_err()
        );
        let fees = TxConfig {
            max_priority_fee_per_gas: 2,
            max_fee_per_gas: 1,
            ..fixtures::CONFIG
        };
        assert!(HFTState::new(AsParams::MOCK, planner, fees, 0.0).is_err());
    }

    #[test]
    fn the_command_line() {
        let parse = |args: &[&str]| Args::parse(args.iter().map(|s| (*s).to_string()));
        let defaults = parse(&[]).unwrap();
        assert_eq!(
            (defaults.vault_timeout, defaults.chain_id, defaults.executor),
            (VAULT_TIMEOUT, 31_337, MOCK_EXECUTOR)
        );
        assert!(!defaults.log_txs && defaults.vault_socket.is_none());
        let args = parse(&[
            "--vault-socket",
            "/run/v.sock",
            "--log-txs",
            "--vault-timeout-ms",
            "250",
            "--ticks",
            "9",
            "--target-token0",
            "50",
        ])
        .unwrap();
        assert_eq!(args.vault_socket, Some(PathBuf::from("/run/v.sock")));
        assert_eq!(
            (
                args.log_txs,
                args.vault_timeout,
                args.ticks,
                args.target_token0
            ),
            (true, Duration::from_millis(250), Some(9), 50.0)
        );
        for bad in [
            &["--vault-timeout-ms", "0"][..],
            &["--executor", "0x0000000000000000000000000000000000000000"],
            &["--executor", "nope"],
            &["--mid", "-1"],
            &["--target-token0", "inf"],
            &["--ticks"],
            &["--lot", "1"],
        ] {
            assert!(matches!(parse(bad), Err(Some(_))), "{bad:?}");
        }
        assert_eq!(parse(&["--help"]), Err(None));
    }
}
