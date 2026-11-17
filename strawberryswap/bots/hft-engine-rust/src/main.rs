//! StrawberrySwap HFT execution engine: Avellaneda–Stoikov on a 100 ms tokio loop, rebalancing the DarkPool through
//! `SearcherExecutor.rebalance` transactions that the threshold vault signs.
//!
//! Every tick:
//!
//! 1. read the chain in one batched RPC request, within FETCH_BUDGET: the zk oracle, the pool, the executor's
//!    inventory, the block, the operator's nonces and gas balance, and the receipt of the transaction in flight
//!    (`chain`, `live`);
//! 2. with a transaction in flight, wait for its receipt: one transaction at a time, at the next nonce;
//! 3. quote r ∓ δ/2 for the inventory, with the Python lab's closed forms (`model`);
//! 4. plan the rebalance those prices make worth its gas against the pool: bots/inventory-cpp's closed form, ported bit
//!    for bit (`planner`), and build the unsigned EIP-1559 transaction (`tx`);
//! 5. simulate it (`eth_call` from the vault's address): a rebalance that would revert, because a competing searcher
//!    moved the pool first or the plan went stale, is dropped before any signature is asked for;
//! 6. have the vault sign it over its Unix socket, within the vault deadline, and check what comes back: the same
//!    transaction, signed by the vault's address (`vault`);
//! 7. broadcast it.
//!
//! The engine holds no key and links no signer crate. A rebalance whose signature does not come back in time, or does
//! not check out, is withdrawn: nothing is broadcast for it, and the next tick plans afresh on new reads.
//!
//! With `--rpc-url` the chain is live: at start the engine checks that the vault's address is the SearcherExecutor's
//! operator and that its own transaction fields are within the vault's policy. Without it, the chain is the mock
//! (`chain::MockChain`, the C++ engine's SimChain behind a transaction boundary). Events, alerts and a metrics snapshot
//! are printed as text, or as JSON lines with `--json` (`metrics`).

#![forbid(unsafe_code)]

mod chain;
mod live;
mod metrics;
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
use alloy::primitives::{Address, B256, U256, address, hex};
use serde_json::{Value, json};
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::chain::{Chain, ChainState, MockChain, Receipt, TxStatus, Watched};
use crate::metrics::{Alert, AlertChange, Alerts, Latencies};
use crate::model::{AsParams, EngineError, fresh_price, unix_now, wad_to_f64};
use crate::planner::{Plan, PlannerParams, plan};
use crate::tx::{Rebalance, TxConfig, spendable, unsigned_tx};
use crate::vault::{SignedTx, TxSigner, VaultClient, VaultError};

/// Tick period: one read, one decision, at most one signed transaction.
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// Budget of a tick's reads, of a simulation and of a broadcast. A late read skips the tick.
pub(crate) const FETCH_BUDGET: Duration = Duration::from_millis(80);
/// Default deadline of a signature: the vault is local, so a round trip takes well under a millisecond.
const VAULT_TIMEOUT: Duration = Duration::from_millis(15);
/// A transaction that the node still does not know this long after its broadcast never arrived, or was dropped.
const DROP_GRACE: Duration = Duration::from_secs(3);
/// A transaction waiting this long for its receipt raises `tx_stuck`.
const STUCK_AFTER: Duration = Duration::from_secs(30);
/// The operator should hold the gas of this many rebalances at the max fee, or `low_gas_balance` is raised.
const GAS_RESERVE_TXS: u64 = 10;

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

/// The engine's transaction on its way: nothing else is sent until it is mined, or its nonce is spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InFlight {
    hash: B256,
    nonce: u64,
    sent_at: Instant,
}

/// What the last good read showed, for the metrics.
#[derive(Clone, Debug, Default, PartialEq)]
struct Gauges {
    block: u64,
    base_fee: u128,
    oracle_age_sec: Option<u64>,
    /// |pool spot / oracle − 1|, basis points.
    deviation_bps: Option<f64>,
    token0: Option<f64>,
    token1: Option<f64>,
    gas_balance: Option<f64>,
    nonce: u64,
    pending_nonce: u64,
}

/// The engine's state between ticks: the model, the planner's costs, the transaction template, the inventory target,
/// what the last tick saw, the transaction in flight, and the counters, latencies and alerts.
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
    in_flight: Option<InFlight>,
    /// The operator's next nonce as the engine knows it: a node's count can lag a tick behind the receipt it served.
    expected_nonce: Option<u64>,
    latencies: Latencies,
    alerts: Alerts,
    gauges: Gauges,
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
            in_flight: None,
            expected_nonce: None,
            latencies: Latencies::default(),
            alerts: Alerts::default(),
            gauges: Gauges::default(),
        })
    }

    /// The rebalance worth sending on this tick's reads, at `nonce`, if any (no I/O). Errors when the reads cannot be
    /// acted on: a stale oracle, a value that does not convert, a non-finite quote.
    fn decide(
        &mut self,
        reads: &ChainState,
        nonce: u64,
        now: u64,
    ) -> Result<Option<Order>, EngineError> {
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
        let tx = unsigned_tx(&self.tx, nonce, &rebalance);
        Ok(Some(Order {
            plan,
            rebalance,
            tx,
        }))
    }

    /// The gauges of a read.
    fn observe(&mut self, reads: &ChainState) {
        let now = unix_now();
        let g = &mut self.gauges;
        g.block = reads.block.number;
        g.base_fee = reads.block.base_fee;
        g.oracle_age_sec =
            (reads.oracle.updated_at > 0).then(|| now.saturating_sub(reads.oracle.updated_at));
        g.deviation_bps = match (reads.pool.to_tokens(), wad_to_f64(reads.oracle.price)) {
            (Ok(pool), Ok(oracle)) if oracle > 0.0 && pool.r0 + pool.v0 > 0.0 => {
                Some((pool.spot() / oracle - 1.0).abs() * 1e4)
            }
            _ => None,
        };
        g.token0 = wad_to_f64(reads.token0).ok();
        g.token1 = wad_to_f64(reads.token1).ok();
        g.gas_balance = wad_to_f64(reads.gas_balance).ok();
        g.nonce = reads.nonce;
        g.pending_nonce = reads.pending_nonce;
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
    /// Ticks skipped before signing: a late or failed read, a stale oracle, a value that did not convert, fees above
    /// the cap, a simulation the node could not run.
    skipped: u64,
    /// Ticks spent waiting: for the transaction in flight, or for one from the operator that is not the engine's.
    waiting: u64,
    /// Rebalances whose simulation reverted (beaten by a competing searcher, or stale): no signature was asked for.
    beaten: u64,
    /// Rebalances withdrawn because the vault did not sign in time, refused, or answered with something other than
    /// our transaction signed by it. Nothing was broadcast for them.
    withdrawn: u64,
    /// Rebalances signed, checked and broadcast.
    sent: u64,
    /// Of those, the ones mined, and of these the ones that reverted.
    mined: u64,
    reverted: u64,
    /// Broadcasts the node refused.
    rejected: u64,
    /// Transactions the node did not know after DROP_GRACE, or whose nonce another transaction spent.
    dropped: u64,
    /// The slowest tick, microseconds.
    slowest_us: u64,
}

/// What a tick that sent nothing was waiting for.
#[derive(Clone, Debug, PartialEq)]
enum WaitingFor {
    /// The engine's own transaction.
    InFlight {
        nonce: u64,
        hash: B256,
        age: Duration,
    },
    /// A pending transaction from the operator that the engine did not send.
    Foreign { mined: u64, pending: u64 },
}

/// What one tick did.
#[derive(Debug)]
enum Outcome {
    Idle,
    Skipped(EngineError),
    Waiting(WaitingFor),
    Beaten {
        nonce: u64,
        reason: String,
    },
    Withdrawn {
        nonce: u64,
        reason: VaultError,
    },
    Sent {
        nonce: u64,
        hash: B256,
        raw: Vec<u8>,
        /// False when the node did not answer within the budget: the transaction is tracked all the same, and the
        /// next reads tell whether the node has it.
        acknowledged: bool,
    },
    Rejected {
        nonce: u64,
        hash: B256,
        error: EngineError,
    },
}

/// What became of the transaction in flight this tick.
#[derive(Clone, Debug, PartialEq)]
enum Resolution {
    Mined {
        nonce: u64,
        receipt: Receipt,
        after: Duration,
    },
    /// Another transaction spent its nonce (replaced), or its receipt lags the node's nonce: either way it is over.
    Spent { nonce: u64, hash: B256 },
    /// The node does not know it DROP_GRACE after the broadcast.
    Dropped { nonce: u64, hash: B256 },
}

/// One tick, as the loop accounts for it.
#[derive(Debug)]
struct TickReport {
    outcome: Outcome,
    resolution: Option<Resolution>,
    /// The tick's reads, when they arrived.
    reads: Option<ChainState>,
    /// The operator's nonce moved past what the engine expected, outside the engine: (expected, now).
    nonce_moved: Option<(u64, u64)>,
    read: Option<Duration>,
    simulate: Option<Duration>,
    sign: Option<Duration>,
    broadcast: Option<Duration>,
}

impl TickReport {
    fn new() -> Self {
        Self {
            outcome: Outcome::Idle,
            resolution: None,
            reads: None,
            nonce_moved: None,
            read: None,
            simulate: None,
            sign: None,
            broadcast: None,
        }
    }

    fn with(mut self, outcome: Outcome) -> Self {
        self.outcome = outcome;
        self
    }
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
    /// A metrics snapshot every this many ticks (0: none).
    metrics_every: u64,
    /// Print each broadcast transaction in full.
    log_txs: bool,
    /// Print events (sent, withdrawn, skipped, alerts…); off in the tests.
    verbose: bool,
    /// Events as JSON lines instead of text.
    json: bool,
}

/// One tick: read, settle the transaction in flight, decide, simulate, sign within the deadline, check, broadcast.
async fn tick<C: Chain, S: TxSigner>(
    state: &mut HFTState,
    chain: &mut C,
    signer: &mut S,
    opts: &RunOptions,
) -> TickReport {
    let mut report = TickReport::new();
    let watch = state.in_flight.map(|flight| flight.hash);
    let started = Instant::now();
    let reads = match time::timeout(FETCH_BUDGET, chain.read(watch)).await {
        Ok(Ok(reads)) => reads,
        Ok(Err(e)) => return report.with(Outcome::Skipped(e)),
        Err(_) => return report.with(Outcome::Skipped(EngineError::Timeout)),
    };
    report.read = Some(started.elapsed());

    // 1. The transaction in flight: one at a time.
    if let Some(flight) = state.in_flight {
        let age = flight.sent_at.elapsed();
        match &reads.watched {
            Watched::Mined(receipt) if receipt.tx_hash == flight.hash => {
                report.resolution = Some(Resolution::Mined {
                    nonce: flight.nonce,
                    receipt: receipt.clone(),
                    after: age,
                });
                state.expected_nonce = Some(flight.nonce + 1);
                state.in_flight = None;
            }
            _ if reads.nonce > flight.nonce => {
                report.resolution = Some(Resolution::Spent {
                    nonce: flight.nonce,
                    hash: flight.hash,
                });
                state.expected_nonce = Some(reads.nonce);
                state.in_flight = None;
            }
            Watched::Unknown if age >= DROP_GRACE => {
                report.resolution = Some(Resolution::Dropped {
                    nonce: flight.nonce,
                    hash: flight.hash,
                });
                state.in_flight = None;
            }
            _ => {
                report.reads = Some(reads);
                return report.with(Outcome::Waiting(WaitingFor::InFlight {
                    nonce: flight.nonce,
                    hash: flight.hash,
                    age,
                }));
            }
        }
    }

    // 2. The nonce: the node's, never below what the engine knows it spent.
    let expected = state.expected_nonce.unwrap_or(reads.nonce);
    if reads.nonce > expected {
        report.nonce_moved = Some((expected, reads.nonce));
    }
    let nonce = reads.nonce.max(expected);
    state.expected_nonce = Some(nonce);
    if reads.pending_nonce > nonce {
        let waiting = WaitingFor::Foreign {
            mined: reads.nonce,
            pending: reads.pending_nonce,
        };
        report.reads = Some(reads);
        return report.with(Outcome::Waiting(waiting));
    }

    // 3. Fees: a base fee above the max fee means a transaction that would wait unmined.
    if reads.block.base_fee > state.tx.max_fee_per_gas {
        let error = EngineError::FeesAboveCap {
            base_fee: reads.block.base_fee,
            cap: state.tx.max_fee_per_gas,
        };
        report.reads = Some(reads);
        return report.with(Outcome::Skipped(error));
    }

    // 4. The decision.
    let decision = state.decide(&reads, nonce, unix_now());
    report.reads = Some(reads);
    let order = match decision {
        Ok(Some(order)) => order,
        Ok(None) => return report.with(Outcome::Idle),
        Err(e) => return report.with(Outcome::Skipped(e)),
    };

    // 5. The simulation, from the vault's address: a rebalance that would revert is never signed.
    if let Some(from) = signer.address() {
        let started = Instant::now();
        let simulated = time::timeout(FETCH_BUDGET, chain.simulate(from, &order.tx)).await;
        report.simulate = Some(started.elapsed());
        match simulated {
            Ok(Ok(())) => {}
            Ok(Err(EngineError::Simulation(reason))) => {
                return report.with(Outcome::Beaten { nonce, reason });
            }
            Ok(Err(e)) => return report.with(Outcome::Skipped(e)),
            Err(_) => return report.with(Outcome::Skipped(EngineError::Timeout)),
        }
    }

    // 6. The signature. The deadline is enforced here for any signer; the vault client also bounds its own calls, and
    // is safe to cancel.
    let started = Instant::now();
    let signed = time::timeout(opts.vault_timeout, signer.sign(&order.tx)).await;
    report.sign = Some(started.elapsed());
    let signed: SignedTx = match signed {
        Ok(Ok(signed)) => signed,
        Ok(Err(reason)) => return report.with(Outcome::Withdrawn { nonce, reason }),
        Err(_) => {
            return report.with(Outcome::Withdrawn {
                nonce,
                reason: VaultError::Timeout,
            });
        }
    };

    // 7. The broadcast.
    let started = Instant::now();
    let sent = time::timeout(FETCH_BUDGET, chain.send_raw_transaction(&signed.raw)).await;
    report.broadcast = Some(started.elapsed());
    match sent {
        Ok(Ok(hash)) => {
            state.in_flight = Some(InFlight {
                hash,
                nonce,
                sent_at: Instant::now(),
            });
            report.with(Outcome::Sent {
                nonce,
                hash,
                raw: signed.raw,
                acknowledged: true,
            })
        }
        Ok(Err(error)) => report.with(Outcome::Rejected {
            nonce,
            hash: signed.hash,
            error,
        }),
        Err(_) => {
            // No answer in time: the node may have it. Track it; its receipt, its nonce being spent, or the node not
            // knowing it settles it.
            state.in_flight = Some(InFlight {
                hash: signed.hash,
                nonce,
                sent_at: Instant::now(),
            });
            report.with(Outcome::Sent {
                nonce,
                hash: signed.hash,
                raw: signed.raw,
                acknowledged: false,
            })
        }
    }
}

/// One printed line: text, or a JSON object with `--json`.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    text: String,
    json: Value,
    /// A note repeated tick after tick (a skip, a withdrawal): printed once until something else is printed.
    note: bool,
}

impl Line {
    fn event(text: String, json: Value) -> Self {
        Self {
            text,
            json,
            note: false,
        }
    }

    fn note(text: String, json: Value) -> Self {
        Self {
            text,
            json,
            note: true,
        }
    }

    fn alert(change: &AlertChange) -> Self {
        match change {
            AlertChange::Raised { alert, detail } => Self::event(
                format!("ALERT {}: {detail}", alert.name()),
                json!({ "event": "alert", "alert": alert.name(), "detail": detail }),
            ),
            AlertChange::Resolved { alert } => Self::event(
                format!("RESOLVED {}", alert.name()),
                json!({ "event": "resolved", "alert": alert.name() }),
            ),
        }
    }

    /// A one-off alert (it has no "resolved").
    fn one_off(name: &str, detail: String) -> Self {
        Self::event(
            format!("ALERT {name}: {detail}"),
            json!({ "event": "alert", "alert": name, "detail": detail }),
        )
    }
}

impl HFTState {
    /// Accounts for one tick: counters, latencies, gauges and alerts. Returns the lines to print.
    fn account(&mut self, report: TickReport, elapsed: Duration, opts: &RunOptions) -> Vec<Line> {
        let mut lines = Vec::new();
        let l = &mut self.latencies;
        l.tick.record(elapsed);
        for (latency, observed) in [
            (&mut l.read, report.read),
            (&mut l.simulate, report.simulate),
            (&mut l.sign, report.sign),
            (&mut l.broadcast, report.broadcast),
        ] {
            if let Some(d) = observed {
                latency.record(d);
            }
        }
        let s = &mut self.stats;
        s.ticks += 1;
        s.slowest_us = s
            .slowest_us
            .max(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX));

        // The transaction in flight.
        match &report.resolution {
            Some(Resolution::Mined {
                nonce,
                receipt,
                after,
            }) => {
                s.mined += 1;
                self.latencies.confirm.record(*after);
                let (status, ok) = match &receipt.status {
                    TxStatus::Success => ("success".to_string(), true),
                    TxStatus::Reverted(reason) => (format!("reverted ({reason})"), false),
                };
                lines.push(Line::event(
                    format!(
                        "MINED nonce={nonce} tx={} from={} status={status} block={} gas={} after_ms={}",
                        receipt.tx_hash,
                        receipt.from,
                        receipt.block,
                        receipt.gas_used,
                        after.as_millis()
                    ),
                    json!({
                        "event": "mined", "nonce": nonce, "tx": receipt.tx_hash, "from": receipt.from,
                        "status": if ok { "success" } else { "reverted" }, "block": receipt.block,
                        "gas_used": receipt.gas_used, "after_ms": after.as_millis() as u64,
                    }),
                ));
                if !ok {
                    s.reverted += 1;
                    lines.push(Line::one_off(
                        "tx_reverted",
                        format!(
                            "nonce {nonce} tx {} reverted on chain after its simulation passed",
                            receipt.tx_hash
                        ),
                    ));
                }
            }
            Some(Resolution::Spent { nonce, hash }) => {
                s.dropped += 1;
                lines.push(Line::one_off(
                    "nonce_spent",
                    format!("nonce {nonce} was spent without the receipt of tx {hash}: replaced, or the node lags"),
                ));
            }
            Some(Resolution::Dropped { nonce, hash }) => {
                s.dropped += 1;
                lines.push(Line::one_off(
                    "tx_dropped",
                    format!(
                        "the node does not know tx {hash} (nonce {nonce}) {} s after its broadcast",
                        DROP_GRACE.as_secs()
                    ),
                ));
            }
            None => {}
        }
        if let Some((expected, now)) = report.nonce_moved {
            lines.push(Line::one_off(
                "nonce_moved",
                format!("the operator's nonce went from {expected} to {now} outside the engine"),
            ));
        }

        // The outcome.
        match &report.outcome {
            Outcome::Idle => s.idle += 1,
            Outcome::Waiting(waiting) => {
                s.waiting += 1;
                if let WaitingFor::Foreign { mined, pending } = waiting {
                    lines.push(Line::note(
                        format!("WAITING: a transaction from the operator that is not the engine's is pending (mined {mined}, pending {pending})"),
                        json!({ "event": "waiting", "reason": "foreign_transaction", "mined": mined, "pending": pending }),
                    ));
                }
            }
            Outcome::Skipped(e) => {
                s.skipped += 1;
                lines.push(Line::note(
                    format!("SKIPPED: {e}"),
                    json!({ "event": "skipped", "reason": e.to_string() }),
                ));
            }
            Outcome::Beaten { nonce, reason } => {
                s.beaten += 1;
                lines.push(Line::note(
                    format!("BEATEN nonce={nonce}: {reason}"),
                    json!({ "event": "beaten", "nonce": nonce, "reason": reason }),
                ));
            }
            Outcome::Withdrawn { nonce, reason } => {
                s.withdrawn += 1;
                lines.push(Line::note(
                    format!("WITHDRAWN nonce={nonce}: {reason}"),
                    json!({ "event": "withdrawn", "nonce": nonce, "reason": reason.to_string() }),
                ));
            }
            Outcome::Rejected { nonce, hash, error } => {
                s.rejected += 1;
                lines.push(Line::note(
                    format!("REJECTED nonce={nonce} tx={hash}: {error}"),
                    json!({ "event": "rejected", "nonce": nonce, "tx": hash, "reason": error.to_string() }),
                ));
            }
            Outcome::Sent {
                nonce,
                hash,
                raw,
                acknowledged,
            } => {
                s.sent += 1;
                let raw_text = if opts.log_txs {
                    format!(" raw={}", hex::encode_prefixed(raw))
                } else {
                    String::new()
                };
                let ack = if *acknowledged {
                    ""
                } else {
                    " (no answer in time; tracked)"
                };
                lines.push(Line::event(
                    format!("SENT nonce={nonce} tx={hash}{raw_text}{ack}"),
                    json!({
                        "event": "sent", "nonce": nonce, "tx": hash, "acknowledged": acknowledged,
                        "raw": opts.log_txs.then(|| hex::encode_prefixed(raw)),
                    }),
                ));
            }
        }

        // Gauges and alerts.
        if let Some(reads) = &report.reads {
            self.observe(reads);
        }
        let changes = self.alert_changes(&report);
        lines.extend(changes.iter().map(Line::alert));
        lines
    }

    /// The alerts this tick raises or resolves.
    fn alert_changes(&mut self, report: &TickReport) -> Vec<AlertChange> {
        let mut changes = Vec::new();
        let mut observe = |alerts: &mut Alerts, alert: Alert, holds: Option<String>| {
            changes.extend(alerts.observe(alert, holds));
        };
        let alerts = &mut self.alerts;
        match &report.reads {
            None => {
                if let Outcome::Skipped(e) = &report.outcome {
                    observe(alerts, Alert::RpcDown, Some(e.to_string()));
                }
            }
            Some(reads) => {
                observe(alerts, Alert::RpcDown, None);
                let stale = fresh_price(reads.oracle, unix_now())
                    .err()
                    .map(|e| e.to_string());
                observe(alerts, Alert::OracleStale, stale);
                let fees = (reads.block.base_fee > self.tx.max_fee_per_gas).then(|| {
                    format!(
                        "base fee {} wei above the max fee {} wei",
                        reads.block.base_fee, self.tx.max_fee_per_gas
                    )
                });
                observe(alerts, Alert::FeesAboveCap, fees);
                let reserve = U256::from(self.tx.gas_limit)
                    * U256::from(self.tx.max_fee_per_gas)
                    * U256::from(GAS_RESERVE_TXS);
                let low = (reads.gas_balance < reserve).then(|| {
                    format!(
                        "the operator holds {} wei, less than {GAS_RESERVE_TXS} rebalances at the max fee ({reserve} wei)",
                        reads.gas_balance
                    )
                });
                observe(alerts, Alert::LowGasBalance, low);
                let foreign = matches!(
                    report.outcome,
                    Outcome::Waiting(WaitingFor::Foreign { .. })
                )
                .then(|| {
                    "a transaction from the operator that the engine did not send is pending"
                        .to_string()
                });
                observe(alerts, Alert::ForeignTransaction, foreign);
            }
        }
        match &report.outcome {
            Outcome::Withdrawn {
                reason: reason @ (VaultError::Unavailable(_) | VaultError::Timeout),
                ..
            } => observe(alerts, Alert::VaultDown, Some(reason.to_string())),
            Outcome::Sent { .. } => observe(alerts, Alert::VaultDown, None),
            _ => {}
        }
        let stuck = match &report.outcome {
            Outcome::Waiting(WaitingFor::InFlight { nonce, hash, age }) if *age >= STUCK_AFTER => {
                Some(format!(
                    "tx {hash} (nonce {nonce}) has waited {} s for its receipt",
                    age.as_secs()
                ))
            }
            _ => None,
        };
        if stuck.is_some() || self.in_flight.is_none() {
            observe(alerts, Alert::TxStuck, stuck);
        }
        changes
    }

    /// A snapshot of the counters, latencies and gauges.
    fn metrics(&self) -> Value {
        let s = &self.stats;
        let g = &self.gauges;
        json!({
            "event": "metrics",
            "counters": {
                "ticks": s.ticks, "idle": s.idle, "skipped": s.skipped, "waiting": s.waiting, "beaten": s.beaten,
                "withdrawn": s.withdrawn, "sent": s.sent, "mined": s.mined, "reverted": s.reverted,
                "rejected": s.rejected, "dropped": s.dropped,
            },
            "latency": self.latencies.to_json(),
            "gauges": {
                "block": g.block, "base_fee_wei": g.base_fee.to_string(), "oracle_age_sec": g.oracle_age_sec,
                "deviation_bps": g.deviation_bps, "mid": self.mid, "inventory": self.inventory,
                "token0": g.token0, "token1": g.token1, "gas_balance": g.gas_balance, "nonce": g.nonce,
                "pending_nonce": g.pending_nonce,
                "in_flight": self.in_flight.map(|f| json!({ "nonce": f.nonce, "tx": f.hash, "age_ms": f.sent_at.elapsed().as_millis() as u64 })),
            },
            "alerts": self.alerts.active(),
        })
    }
}

fn print_line(line: &Line, json: bool) {
    if json {
        let mut value = line.json.clone();
        value["ts_ms"] = json!(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64)
        );
        println!("{value}");
    } else {
        println!("{}", line.text);
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
        let report = tick(state, chain, signer, &opts).await;
        let lines = state.account(report, tick_start.elapsed(), &opts);
        if opts.verbose {
            for line in &lines {
                if line.note {
                    if line.text == last_note {
                        continue;
                    }
                    last_note.clone_from(&line.text);
                } else {
                    last_note.clear();
                }
                print_line(line, opts.json);
            }
            let ticks = state.stats.ticks;
            if opts.log_every > 0 && ticks.is_multiple_of(opts.log_every) && !opts.json {
                log_status(start.elapsed(), state);
            }
            if opts.metrics_every > 0 && ticks.is_multiple_of(opts.metrics_every) {
                let metrics = state.metrics();
                if opts.json {
                    print_line(&Line::event(String::new(), metrics), true);
                } else {
                    println!("METRICS {metrics}");
                }
            }
        }
    }
}

fn log_status(elapsed: Duration, state: &HFTState) {
    let s = &state.stats;
    let show = |x: Option<f64>, digits: usize| {
        x.map_or_else(|| "—".to_string(), |x| format!("{x:.digits$}"))
    };
    println!(
        "t={:>7.1}s  mid {}  q {}  · sent {} (mined {}, reverted {}) · beaten {} · withdrawn {} · skipped {} · idle {}",
        elapsed.as_secs_f64(),
        show(state.mid, 6),
        show(state.inventory, 1),
        s.sent,
        s.mined,
        s.reverted,
        s.beaten,
        s.withdrawn,
        s.skipped,
        s.idle
    );
}

/// The closing line: `summary ticks=… sent=… …`, each field a number.
fn summary_fields(state: &HFTState) -> String {
    let s = &state.stats;
    format!(
        "ticks={} idle={} skipped={} waiting={} beaten={} withdrawn={} sent={} mined={} reverted={} rejected={} \
         dropped={} slowest_ms={:.3}",
        s.ticks,
        s.idle,
        s.skipped,
        s.waiting,
        s.beaten,
        s.withdrawn,
        s.sent,
        s.mined,
        s.reverted,
        s.rejected,
        s.dropped,
        s.slowest_us as f64 / 1e3
    )
}

// ---------------------------------------------------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------------------------------------------------

const USAGE: &str = "usage: hft-engine [--rpc-url URL] [--feed ADDRESS] [--vault-socket PATH] [--vault-timeout-ms MS]
                  [--chain-id ID] [--executor ADDRESS] [--target-token0 TOKENS] [--calibration PATH]
                  [--gas-limit GAS] [--max-fee-wei WEI] [--priority-fee-wei WEI] [--gas-cost TOKENS1]
                  [--min-net TOKENS1] [--ticks N] [--json] [--metrics-every TICKS] [--log-txs]
                  [--mid PRICE] [--seed N]

Runs the Avellaneda–Stoikov engine (100 ms ticks). Every rebalance is simulated, then signed by the
threshold vault over its Unix socket; the engine holds no key. A rebalance that would revert is never
signed; one the vault does not sign within the deadline is withdrawn. One transaction is in flight at
a time.

  --rpc-url URL           the chain's JSON-RPC (LIVE mode; never printed: it may hold a key). Without
                          it, the chain is a MOCK
  --feed ADDRESS          live: the ZkPriceFeed the pool must read (checked at start)
  --vault-socket PATH     the vault's socket (default: the vault's own default,
                          $XDG_RUNTIME_DIR/strawberryswap-vault/vault.sock)
  --vault-timeout-ms MS   deadline of each signature (default: 15)
  --chain-id ID           chain id of the transactions (default: 31337)
  --executor ADDRESS      the SearcherExecutor contract (default: 0x5FbDB2315678afecb367f032d93F642f64180aa3)
  --target-token0 TOKENS  the executor's token0 target; the model's inventory is the balance minus it
                          (default: 20000, the mock executor's starting balance)
  --calibration PATH      the Python lab's calibration.json (its `engine` block); default: mock parameters
  --gas-limit GAS         gas limit of a rebalance (default: 300000)
  --max-fee-wei WEI       max fee per gas (default: 30 gwei); with the gas limit, within the vault's caps
  --priority-fee-wei WEI  priority fee per gas (default: 1 gwei)
  --gas-cost TOKENS1      a rebalance's gas, in token1, for the planner (default: 0.01)
  --min-net TOKENS1       the least net profit worth a rebalance, token1 (default: 0.01)
  --ticks N               stop after N ticks (default: run until Ctrl-C or SIGTERM)
  --json                  events, alerts and metrics as JSON lines
  --metrics-every TICKS   a metrics snapshot every TICKS ticks (default: 100, 10 s; 0: none)
  --log-txs               print every broadcast transaction in full (hex)
  --mid PRICE             mock: starting price (default: 2)
  --seed N                mock: seed (default: 7)";

/// The mock executor's address: the first contract Anvil's default deployer creates.
const MOCK_EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");
/// Default transaction fields: well inside the vault's default caps (1 000 000 gas, 500 gwei).
const DEFAULT_GAS_LIMIT: u64 = 300_000;
const DEFAULT_MAX_FEE_PER_GAS: u128 = 30_000_000_000;
const DEFAULT_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;
/// Default costs, token1: the gas of a rebalance, and the least net profit worth sending one for.
const DEFAULT_GAS_COST: f64 = 0.01;
const DEFAULT_MIN_NET: f64 = 0.01;

#[derive(Debug, PartialEq)]
struct Args {
    rpc_url: Option<String>,
    feed: Option<Address>,
    vault_socket: Option<PathBuf>,
    vault_timeout: Duration,
    chain_id: u64,
    executor: Address,
    target_token0: f64,
    calibration: Option<PathBuf>,
    gas_limit: u64,
    max_fee_per_gas: u128,
    priority_fee_per_gas: u128,
    gas_cost: f64,
    min_net: f64,
    ticks: Option<u64>,
    json: bool,
    metrics_every: u64,
    log_txs: bool,
    /// Mock only.
    mid: Option<f64>,
    seed: Option<u64>,
}

impl Args {
    /// Parses the arguments; Err(None) asks for the usage text.
    fn parse(mut it: impl Iterator<Item = String>) -> Result<Self, Option<String>> {
        fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, Option<String>> {
            text.parse()
                .map_err(|_| Some(format!("{flag}: {text} is not a valid number")))
        }
        fn address(flag: &str, text: &str) -> Result<Address, Option<String>> {
            let address: Address = text
                .parse()
                .map_err(|_| Some(format!("{flag}: {text} is not an address")))?;
            if address.is_zero() {
                return Err(Some(format!("{flag}: the zero address is not a contract")));
            }
            Ok(address)
        }
        fn amount(flag: &str, text: &str) -> Result<f64, Option<String>> {
            let x: f64 = number(flag, text)?;
            if !(x.is_finite() && x >= 0.0) {
                return Err(Some(format!("{flag} must be a non-negative amount")));
            }
            Ok(x)
        }
        let mut args = Self {
            rpc_url: None,
            feed: None,
            vault_socket: None,
            vault_timeout: VAULT_TIMEOUT,
            chain_id: 31_337,
            executor: MOCK_EXECUTOR,
            target_token0: 20_000.0,
            calibration: None,
            gas_limit: DEFAULT_GAS_LIMIT,
            max_fee_per_gas: DEFAULT_MAX_FEE_PER_GAS,
            priority_fee_per_gas: DEFAULT_PRIORITY_FEE_PER_GAS,
            gas_cost: DEFAULT_GAS_COST,
            min_net: DEFAULT_MIN_NET,
            ticks: None,
            json: false,
            metrics_every: 100,
            log_txs: false,
            mid: None,
            seed: None,
        };
        while let Some(flag) = it.next() {
            match flag.as_str() {
                "-h" | "--help" => return Err(None),
                "--log-txs" => {
                    args.log_txs = true;
                    continue;
                }
                "--json" => {
                    args.json = true;
                    continue;
                }
                _ => {}
            }
            let value = it
                .next()
                .ok_or_else(|| Some(format!("{flag} needs a value")))?;
            match flag.as_str() {
                "--rpc-url" => args.rpc_url = Some(value),
                "--feed" => args.feed = Some(address(&flag, &value)?),
                "--vault-socket" => args.vault_socket = Some(PathBuf::from(value)),
                "--vault-timeout-ms" => {
                    let ms: u64 = number(&flag, &value)?;
                    if !(1..=10_000).contains(&ms) {
                        return Err(Some("--vault-timeout-ms must be in [1, 10000]".into()));
                    }
                    args.vault_timeout = Duration::from_millis(ms);
                }
                "--chain-id" => args.chain_id = number(&flag, &value)?,
                "--executor" => args.executor = address(&flag, &value)?,
                "--target-token0" => args.target_token0 = amount(&flag, &value)?,
                "--calibration" => args.calibration = Some(PathBuf::from(value)),
                "--gas-limit" => {
                    args.gas_limit = number(&flag, &value)?;
                    if args.gas_limit < 21_000 {
                        return Err(Some("--gas-limit must be at least 21000".into()));
                    }
                }
                "--max-fee-wei" => args.max_fee_per_gas = number(&flag, &value)?,
                "--priority-fee-wei" => args.priority_fee_per_gas = number(&flag, &value)?,
                "--gas-cost" => args.gas_cost = amount(&flag, &value)?,
                "--min-net" => args.min_net = amount(&flag, &value)?,
                "--ticks" => args.ticks = Some(number(&flag, &value)?),
                "--metrics-every" => args.metrics_every = number(&flag, &value)?,
                "--mid" => {
                    let mid: f64 = number(&flag, &value)?;
                    if !(mid.is_finite() && mid > 0.0) {
                        return Err(Some("--mid must be a positive price".into()));
                    }
                    args.mid = Some(mid);
                }
                "--seed" => args.seed = Some(number(&flag, &value)?),
                other => return Err(Some(format!("unknown argument {other}"))),
            }
        }
        if args.priority_fee_per_gas > args.max_fee_per_gas {
            return Err(Some(
                "--priority-fee-wei cannot exceed --max-fee-wei".into(),
            ));
        }
        if args.rpc_url.is_some() && (args.mid.is_some() || args.seed.is_some()) {
            return Err(Some(
                "--mid and --seed drive the mock chain; they mean nothing with --rpc-url".into(),
            ));
        }
        if args.rpc_url.is_none() && args.feed.is_some() {
            return Err(Some(
                "--feed is checked on a live chain: it needs --rpc-url".into(),
            ));
        }
        Ok(args)
    }

    fn tx_config(&self) -> TxConfig {
        TxConfig {
            chain_id: self.chain_id,
            executor: self.executor,
            gas_limit: self.gas_limit,
            max_fee_per_gas: self.max_fee_per_gas,
            max_priority_fee_per_gas: self.priority_fee_per_gas,
        }
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

/// An error that stops the engine before its first tick.
fn fatal(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
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
    let tx = args.tx_config();
    let planner = PlannerParams {
        gas_cost_quote: args.gas_cost,
        min_net: args.min_net,
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
    let opts = RunOptions {
        max_ticks: args.ticks,
        vault_timeout: args.vault_timeout,
        log_every: 10,
        metrics_every: args.metrics_every,
        log_txs: args.log_txs,
        verbose: true,
        json: args.json,
    };
    let model_line = format!(
        "model: γ {} · σ² {} /s · τ {} s · κ {}  ({source})",
        params.gamma, params.sigma2_per_sec, params.horizon_sec, params.kappa
    );
    let tx_line = format!(
        "executor: {} on chain {} · target {} token0 · gas {} · max fee {} wei · priority {} wei",
        tx.executor,
        tx.chain_id,
        args.target_token0,
        tx.gas_limit,
        tx.max_fee_per_gas,
        tx.max_priority_fee_per_gas
    );

    if let Some(rpc_url) = &args.rpc_url {
        // LIVE: the vault must answer, sign for this chain and executor within its caps, and be the operator.
        let operator = match vault.fetch_address().await {
            Ok(address) => address,
            Err(e) => {
                return fatal(format!(
                    "the vault at {} did not answer ({e}): the live engine does not start without its signer",
                    socket.display()
                ));
            }
        };
        let policy = match vault.fetch_policy().await {
            Ok(policy) => policy,
            Err(e) => return fatal(format!("the vault's policy: {e}")),
        };
        if let Some(refusal) = policy.refuses(&tx) {
            return fatal(format!("the vault would refuse every rebalance: {refusal}"));
        }
        let (mut chain, deployment) =
            match live::connect(rpc_url, args.chain_id, args.executor, operator, args.feed).await {
                Ok(connected) => connected,
                Err(e) => return fatal(e),
            };
        println!(
            "hft-engine: LIVE on chain {}, one batched read per {} ms tick ({} ms budget); no key in this process",
            deployment.chain_id,
            TICK.as_millis(),
            FETCH_BUDGET.as_millis()
        );
        println!(
            "vault: {} · signer {operator} = the executor's operator · {} ms deadline · policy: gas ≤ {}, max fee ≤ {} wei",
            socket.display(),
            args.vault_timeout.as_millis(),
            policy.max_gas,
            policy.max_fee_per_gas
        );
        println!(
            "deployment: pool {} · feed {} · token0 {} · token1 {}",
            deployment.pool, deployment.feed, deployment.token0, deployment.token1
        );
        println!("{model_line}");
        println!("{tx_line}");
        run(&mut state, &mut chain, &mut vault, opts, shutdown_signal()).await;
        println!("summary {}", summary_fields(&state));
        return ExitCode::SUCCESS;
    }

    // MOCK: the SearcherExecutor is deployed with the vault's address as its operator, as the real one is configured.
    // Without a vault the engine still runs, and withdraws every rebalance until the vault answers.
    let operator = match vault.fetch_address().await {
        Ok(address) => address,
        Err(e) => {
            eprintln!("warning: {e}; every rebalance will be withdrawn until the vault answers");
            Address::ZERO
        }
    };
    let mut chain = MockChain::new(
        args.mid.unwrap_or(2.0),
        params.sigma2_per_sec,
        args.seed.unwrap_or(7),
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
    println!("{model_line}");
    println!("{tx_line}");
    run(&mut state, &mut chain, &mut vault, opts, shutdown_signal()).await;
    println!(
        "summary {} pool={:.6} oracle={:.6} treasury={:.6}",
        summary_fields(&state),
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
    struct InProcessSigner {
        key: k256::ecdsa::SigningKey,
        calls: u64,
    }

    impl InProcessSigner {
        fn new() -> Self {
            Self {
                key: k256::ecdsa::SigningKey::from_slice(fixtures::test_key().as_slice()).unwrap(),
                calls: 0,
            }
        }
    }

    impl TxSigner for InProcessSigner {
        fn address(&self) -> Option<Address> {
            Some(fixtures::VAULT_ADDRESS)
        }

        async fn sign(&mut self, tx: &TxEip1559) -> Result<SignedTx, VaultError> {
            self.calls += 1;
            let (signature, recovery) = self
                .key
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
        fn address(&self) -> Option<Address> {
            Some(fixtures::VAULT_ADDRESS)
        }

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

    /// The mock chain with the node's misbehaviours the live chain can show: a competing searcher that wins every
    /// race, transactions that take ticks to be mined, a node that drops what it accepted, a foreign pending
    /// transaction, a nonce read one block behind.
    struct Node {
        inner: MockChain,
        /// Every simulation reverts, as when a competing searcher moved the pool first.
        beaten: bool,
        /// Reads that report a transaction pending before its receipt shows.
        mine_after_reads: u32,
        hidden: Option<(B256, u32)>,
        /// The next broadcasts are accepted and then lost.
        drop_next: u32,
        /// A foreign transaction from the operator is pending.
        foreign_pending: bool,
        /// The next read reports the nonce one behind.
        lag_next_read: bool,
        broadcasts: Vec<u64>,
    }

    impl Node {
        fn new(inner: MockChain) -> Self {
            Self {
                inner,
                beaten: false,
                mine_after_reads: 0,
                hidden: None,
                drop_next: 0,
                foreign_pending: false,
                lag_next_read: false,
                broadcasts: Vec::new(),
            }
        }
    }

    impl Chain for Node {
        async fn read(&mut self, watch: Option<B256>) -> Result<ChainState, EngineError> {
            let mut reads = self.inner.read(watch).await?;
            if let Some((hash, left)) = self.hidden
                && watch == Some(hash)
            {
                if left > 0 {
                    self.hidden = Some((hash, left - 1));
                    reads.watched = Watched::Pending;
                    reads.nonce -= 1;
                } else {
                    self.hidden = None;
                }
            }
            if self.foreign_pending {
                reads.pending_nonce = reads.nonce + 1;
            }
            if std::mem::take(&mut self.lag_next_read) {
                reads.nonce -= 1;
                reads.pending_nonce = reads.nonce;
            }
            Ok(reads)
        }

        async fn simulate(&mut self, from: Address, tx: &TxEip1559) -> Result<(), EngineError> {
            if self.beaten {
                return Err(EngineError::Simulation(
                    "DarkPool: DeviationNotReduced".into(),
                ));
            }
            self.inner.simulate(from, tx).await
        }

        async fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<B256, EngineError> {
            let tx = alloy::consensus::TxEnvelope::decode_2718(&mut &raw[..]).unwrap();
            self.broadcasts
                .push(alloy::consensus::Transaction::nonce(&tx));
            if self.drop_next > 0 {
                self.drop_next -= 1;
                return Ok(*tx.tx_hash());
            }
            let hash = self.inner.send_raw_transaction(raw).await?;
            if self.mine_after_reads > 0 {
                self.hidden = Some((hash, self.mine_after_reads));
            }
            Ok(hash)
        }
    }

    use alloy::eips::eip2718::Decodable2718;

    fn mock_state() -> HFTState {
        let planner = PlannerParams {
            gas_cost_quote: DEFAULT_GAS_COST,
            min_net: DEFAULT_MIN_NET,
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
            metrics_every: 0,
            log_txs: false,
            verbose: false,
            json: false,
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

    /// The whole path on the mock chain: thousands of ticks, every rebalance simulated, signed, included from the
    /// operator and accepted by the contract's rules, one at a time; the pool tracks the oracle, the inventory stays
    /// near its target, and the Treasury is paid.
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
            (
                s.reverted,
                s.rejected,
                s.withdrawn,
                s.skipped,
                s.beaten,
                s.dropped,
                s.waiting
            ),
            (0, 0, 0, 0, 0, 0, 0),
            "{s:?}"
        );
        assert_eq!(s.idle + s.sent, s.ticks);
        assert_eq!(s.mined + u64::from(state.in_flight.is_some()), s.sent);
        assert_eq!(signer.calls, s.sent, "one signature per broadcast");
        assert_eq!(
            (chain.nonce, chain.included, chain.reverted),
            (s.sent, s.sent, 0)
        );
        assert!(chain.treasury > 0.0);
        assert!(
            deviation < 0.01 && worst < 0.03,
            "mean |pool/oracle − 1| {deviation}, worst {worst}"
        );
        assert_eq!(state.latencies.read.count(), 5_000);
        assert_eq!(state.latencies.simulate.count(), s.sent);
        assert_eq!(state.latencies.confirm.count(), s.mined);
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
        assert!(state.alerts.active().contains(&"vault_down"));
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
        assert!(
            !state.alerts.active().contains(&"vault_down"),
            "resolved by the first signature"
        );
    }

    /// The pre-send simulation: a rebalance a competing searcher already made unprofitable (it would revert) is
    /// dropped before the vault is asked, so no gas is spent on it.
    #[tokio::test(start_paused = true)]
    async fn a_rebalance_that_would_revert_is_never_signed() {
        let (mut state, mut signer) = (mock_state(), InProcessSigner::new());
        let mut node = Node::new(mock_chain(7));
        node.beaten = true;
        run(
            &mut state,
            &mut node,
            &mut signer,
            quiet(1_000),
            std::future::pending(),
        )
        .await;
        let s = state.stats;
        assert!(s.beaten > 50, "{s:?}");
        assert_eq!(
            (s.sent, signer.calls, node.inner.included),
            (0, 0, 0),
            "{s:?}"
        );
        assert_eq!(s.idle + s.beaten, s.ticks);
    }

    /// One transaction in flight: while it waits for its receipt nothing else is signed or sent, and the next one
    /// takes the next nonce.
    #[tokio::test(start_paused = true)]
    async fn one_transaction_is_in_flight_at_a_time() {
        let (mut state, mut signer) = (mock_state(), InProcessSigner::new());
        let mut node = Node::new(mock_chain(7));
        node.mine_after_reads = 5;
        run(
            &mut state,
            &mut node,
            &mut signer,
            quiet(2_000),
            std::future::pending(),
        )
        .await;
        let s = state.stats;
        assert!(s.sent > 20, "{s:?}");
        assert!(s.waiting >= 5 * (s.sent - 1), "{s:?}");
        let expected: Vec<u64> = (0..s.sent).collect();
        assert_eq!(
            node.broadcasts, expected,
            "consecutive nonces, each sent once"
        );
        assert_eq!(signer.calls, s.sent);
        assert_eq!((s.reverted, s.dropped, s.rejected), (0, 0, 0));
    }

    /// A transaction the node accepted and then lost: after DROP_GRACE the engine forgets it and reuses its nonce.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_transaction_is_forgotten_and_its_nonce_reused() {
        let (mut state, mut signer) = (mock_state(), InProcessSigner::new());
        let mut node = Node::new(mock_chain(7));
        node.drop_next = 1;
        run(
            &mut state,
            &mut node,
            &mut signer,
            quiet(500),
            std::future::pending(),
        )
        .await;
        let s = state.stats;
        assert_eq!(s.dropped, 1, "{s:?}");
        assert_eq!(
            &node.broadcasts[..2],
            [0, 0],
            "the lost nonce is sent again"
        );
        assert!(s.mined > 0 && s.reverted == 0, "{s:?}");
        assert!(s.waiting >= 29, "it waited DROP_GRACE first: {s:?}");
    }

    /// A pending transaction from the operator that the engine did not send: the engine sends nothing on top of it,
    /// and says so.
    #[tokio::test(start_paused = true)]
    async fn a_foreign_pending_transaction_holds_the_engine() {
        let (mut state, mut signer) = (mock_state(), InProcessSigner::new());
        let mut node = Node::new(mock_chain(7));
        node.foreign_pending = true;
        run(
            &mut state,
            &mut node,
            &mut signer,
            quiet(100),
            std::future::pending(),
        )
        .await;
        assert_eq!((state.stats.waiting, state.stats.sent), (100, 0));
        assert!(state.alerts.active().contains(&"foreign_transaction"));
    }

    /// A node a block behind on the nonce right after a receipt: the engine does not reuse a spent nonce.
    #[tokio::test(start_paused = true)]
    async fn the_nonce_never_goes_back_when_the_node_lags() {
        let (mut state, mut signer) = (mock_state(), InProcessSigner::new());
        let mut node = Node::new(mock_chain(7));
        while state.stats.mined == 0 {
            let next = state.stats.ticks + 1;
            run(
                &mut state,
                &mut node,
                &mut signer,
                quiet(next),
                std::future::pending(),
            )
            .await;
        }
        node.lag_next_read = true;
        let sent = state.stats.sent;
        let until = state.stats.ticks + 300;
        run(
            &mut state,
            &mut node,
            &mut signer,
            quiet(until),
            std::future::pending(),
        )
        .await;
        assert!(state.stats.sent > sent);
        let expected: Vec<u64> = (0..state.stats.sent).collect();
        assert_eq!(node.broadcasts, expected);
        assert_eq!(state.stats.rejected, 0);
    }

    /// A base fee above the max fee: no transaction (it would wait unmined), and an alert after a second of it.
    #[tokio::test(start_paused = true)]
    async fn fees_above_the_cap_hold_the_engine_and_raise_an_alert() {
        let (mut state, mut chain, mut signer) =
            (mock_state(), mock_chain(7), InProcessSigner::new());
        chain.base_fee = fixtures::CONFIG.max_fee_per_gas + 1;
        run(
            &mut state,
            &mut chain,
            &mut signer,
            quiet(50),
            std::future::pending(),
        )
        .await;
        assert_eq!((state.stats.skipped, state.stats.sent), (50, 0));
        assert!(state.alerts.active().contains(&"fees_above_cap"));
        chain.base_fee = crate::chain::MOCK_BASE_FEE;
        run(
            &mut state,
            &mut chain,
            &mut signer,
            quiet(60),
            std::future::pending(),
        )
        .await;
        assert!(!state.alerts.active().contains(&"fees_above_cap"));
    }

    #[tokio::test(start_paused = true)]
    async fn late_reads_skip_the_tick_and_raise_rpc_down() {
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
        assert_eq!(state.alerts.active(), ["rpc_down"]);
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

    /// The structured output: event lines in both formats, and a metrics snapshot with counters, latencies, gauges and
    /// the active alerts.
    #[tokio::test(start_paused = true)]
    async fn events_and_metrics_are_structured() {
        let (mut state, mut chain, mut signer) =
            (mock_state(), mock_chain(7), InProcessSigner::new());
        run(
            &mut state,
            &mut chain,
            &mut signer,
            quiet(300),
            std::future::pending(),
        )
        .await;
        let metrics = state.metrics();
        assert_eq!(metrics["event"], "metrics");
        assert_eq!(metrics["counters"]["ticks"], 300);
        assert_eq!(metrics["counters"]["sent"], state.stats.sent);
        assert_eq!(metrics["latency"]["read"]["count"], 300);
        assert!(metrics["latency"]["tick"]["p99_us"].is_u64());
        assert!(metrics["gauges"]["deviation_bps"].as_f64().unwrap() < 300.0);
        assert_eq!(metrics["gauges"]["base_fee_wei"], "1000000000");
        assert_eq!(metrics["alerts"], json!([]));

        let receipt = Receipt {
            tx_hash: B256::repeat_byte(7),
            from: fixtures::VAULT_ADDRESS,
            status: TxStatus::Success,
            block: 9,
            gas_used: 150_000,
        };
        let report = TickReport {
            resolution: Some(Resolution::Mined {
                nonce: 4,
                receipt,
                after: Duration::from_millis(250),
            }),
            ..TickReport::new().with(Outcome::Beaten {
                nonce: 5,
                reason: "DarkPool: DeviationNotReduced".into(),
            })
        };
        let lines = state.account(report, Duration::from_millis(3), &quiet(0));
        assert_eq!(
            lines[0].text,
            format!(
                "MINED nonce=4 tx={} from={} status=success block=9 gas=150000 after_ms=250",
                B256::repeat_byte(7),
                fixtures::VAULT_ADDRESS
            )
        );
        assert_eq!(lines[0].json["event"], "mined");
        assert_eq!(lines[0].json["gas_used"], 150_000);
        assert_eq!(
            lines[1].text,
            "BEATEN nonce=5: DarkPool: DeviationNotReduced"
        );
        assert!(lines[1].note);
        assert_eq!(lines[1].json["event"], "beaten");
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
        let order = state
            .decide(&reads, 7, now)
            .unwrap()
            .expect("an opportunity");
        assert!(order.plan.sells_token0);
        assert!(order.plan.pay0 <= 50.0 && order.plan.amount1_out > 0.0);
        assert_eq!(order.tx.nonce, 7);
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
        assert_eq!(state.decide(&empty, 7, now), Ok(None));
    }

    #[test]
    fn decide_refuses_stale_or_unconvertible_reads() {
        let mut state = mock_state();
        let now = unix_now();
        let stale = fixtures::scenario_reads(now - 60);
        assert!(matches!(
            state.decide(&stale, 0, now),
            Err(EngineError::StaleOracle { .. })
        ));
        let future = fixtures::scenario_reads(now + 60);
        assert!(matches!(
            state.decide(&future, 0, now),
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
            state.decide(&zero, 0, now),
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
        assert!(!defaults.log_txs && !defaults.json && defaults.vault_socket.is_none());
        assert_eq!(defaults.rpc_url, None);
        assert_eq!(defaults.tx_config(), fixtures::CONFIG);
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
        let live = parse(&[
            "--rpc-url",
            "http://127.0.0.1:8545",
            "--feed",
            "0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0",
            "--chain-id",
            "421614",
            "--gas-limit",
            "500000",
            "--max-fee-wei",
            "2000000000",
            "--priority-fee-wei",
            "0",
            "--gas-cost",
            "0.5",
            "--min-net",
            "1",
            "--json",
            "--metrics-every",
            "50",
        ])
        .unwrap();
        assert_eq!(live.rpc_url.as_deref(), Some("http://127.0.0.1:8545"));
        assert!(live.json && live.feed.is_some());
        assert_eq!(
            live.tx_config(),
            TxConfig {
                chain_id: 421_614,
                gas_limit: 500_000,
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 0,
                ..fixtures::CONFIG
            }
        );
        assert_eq!(
            (live.gas_cost, live.min_net, live.metrics_every),
            (0.5, 1.0, 50)
        );
        for bad in [
            &["--vault-timeout-ms", "0"][..],
            &["--executor", "0x0000000000000000000000000000000000000000"],
            &["--executor", "nope"],
            &["--mid", "-1"],
            &["--target-token0", "inf"],
            &["--ticks"],
            &["--lot", "1"],
            &["--gas-limit", "20000"],
            &["--max-fee-wei", "1", "--priority-fee-wei", "2"],
            &["--rpc-url", "http://x", "--seed", "1"],
            &["--feed", "0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0"],
            &["--gas-cost", "-1"],
        ] {
            assert!(matches!(parse(bad), Err(Some(_))), "{bad:?}");
        }
        assert_eq!(parse(&["--help"]), Err(None));
    }
}
