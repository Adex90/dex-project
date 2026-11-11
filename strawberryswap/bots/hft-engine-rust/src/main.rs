//! StrawberrySwap HFT execution engine, iteration 0.
//!
//! Avellaneda–Stoikov quoting on a 100 ms tokio loop, fed by mock data sources, so that the arithmetic and the loop can
//! be verified before anything reads or writes a live contract. The closed forms are the Python lab's
//! (`strawberryswap/hft-engine/python-sim/as_sim/model.py`), which are written exactly as in `bots/inventory-cpp`
//! (`avellaneda_stoikov.hpp`):
//!
//! ```text
//! reservation price      r = s − q·γ·σ²·τ
//! optimal total spread   δ = γ·σ²·τ + (2/γ)·ln(1 + γ/κ)
//! quotes                 bid = min(r − δ/2, s),  ask = max(r + δ/2, s),  rounded outward to the tick
//! ```
//!
//! Units are the engine's, those of the `engine` block of the lab's `calibration.json`:
//!
//! * s, r, bid, ask: price of token0 in token1, in 18-decimal fixed point on chain (s is the zk-attested oracle price
//!   the DarkPool trusts, `IZKOracle.latestVerifiedPrice()`);
//! * q: signed inventory in whole token0, the executor's balance minus its target (long > 0);
//! * σ²: variance of the mid, price² per second; τ: rolling horizon in seconds, re-armed every tick;
//! * γ: risk aversion, 1/(price · token0); κ: decay of the fill intensity with distance from the mid, 1/price.
//!
//! On-chain amounts are `U256` in 18-decimal fixed point. They become `f64` with a single correct rounding, and quotes
//! go back on chain as exact multiples of the tick.

#![forbid(unsafe_code)]

#[cfg(test)]
mod lab_golden;

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use alloy::primitives::utils::{ParseUnits, format_units, parse_units};
use alloy::primitives::{I256, U256};
use serde::Deserialize;
use tokio::time::{self, Instant, MissedTickBehavior};

/// Period of the event loop.
const TICK: Duration = Duration::from_millis(100);
/// Both reads must land within this budget; a tick that misses it pulls the quote instead of quoting on stale data.
const FETCH_BUDGET: Duration = Duration::from_millis(80);
/// Decimals of on-chain prices and token0 amounts.
const DECIMALS: u8 = 18;
/// Tolerance of the outward tick rounding, in ticks, as in the lab: it absorbs the binary error of price / tick.
const TICK_EPS: f64 = 1e-9;
/// Slack on the inventory cap, as in the lab's simulator.
const CAP_EPS: f64 = 1e-12;
/// Largest tick count a quote may carry: every integer up to 2^53 is exact in f64.
const MAX_TICKS: f64 = 9_007_199_254_740_992.0;
/// Oldest oracle reading the engine quotes on, seconds: the C++ engine's limit, inside the contract's
/// `DarkPool.MAX_ORACLE_AGE` (60 s) with headroom for latency.
const MAX_ORACLE_AGE_SEC: u64 = 45;
/// How far in the future an oracle timestamp may be (clock skew), seconds; beyond it the reading is rejected.
const MAX_CLOCK_SKEW_SEC: u64 = 5;

// ---------------------------------------------------------------------------------------------------------------------
// Closed forms: the lab's functions, same arguments in the same order (σ² instead of σ) and the same operation order,
// so the results are the lab's bit for bit (the golden tests compare them).
// ---------------------------------------------------------------------------------------------------------------------

/// r = s − q·γ·σ²·τ. A long inventory pulls r below the mid (keener to sell), a short one above it.
fn reservation_price(mid: f64, inventory: f64, gamma: f64, sigma2: f64, tau: f64) -> f64 {
    mid - inventory * gamma * sigma2 * tau
}

/// Total spread δa + δb = γσ²τ + (2/γ)·ln(1 + γ/κ), for γ > 0 and κ > 0. `ln_1p` keeps full precision when γ/κ is
/// small, where ln(1 + γ/κ) loses digits (and is exactly 0 below 2⁻⁵³); the spread then tends to 2/κ, the risk-neutral
/// spread.
fn optimal_spread(gamma: f64, sigma2: f64, tau: f64, kappa: f64) -> f64 {
    gamma * sigma2 * tau + (2.0 / gamma) * (gamma / kappa).ln_1p()
}

/// The lab's `quotes`: r, and the bid and ask around it with the lab's two guards. A quote never crosses the mid (a
/// large inventory holds the near side at the mid, where it fills fastest), and with `tick > 0` the quotes are rounded
/// outward, bid down and ask up, so rounding never makes a quote more aggressive. Returns (r, bid, ask).
fn quotes(
    mid: f64,
    inventory: f64,
    gamma: f64,
    sigma2: f64,
    tau: f64,
    kappa: f64,
    tick: f64,
) -> (f64, f64, f64) {
    let r = reservation_price(mid, inventory, gamma, sigma2, tau);
    let half = 0.5 * optimal_spread(gamma, sigma2, tau, kappa);
    let mut bid = (r - half).min(mid);
    let mut ask = (r + half).max(mid);
    if tick > 0.0 {
        bid = bid_ticks(bid, tick) * tick;
        ask = ask_ticks(ask, tick) * tick;
    }
    (r, bid, ask)
}

/// Number of ticks of a bid, rounded down, with the lab's 1e-9-tick tolerance.
fn bid_ticks(price: f64, tick: f64) -> f64 {
    (price / tick + TICK_EPS).floor()
}

/// Number of ticks of an ask, rounded up, with the lab's 1e-9-tick tolerance.
fn ask_ticks(price: f64, tick: f64) -> f64 {
    (price / tick - TICK_EPS).ceil()
}

/// The lab's fill model: P(at least one market order reaches a quote at distance δ within dt)
/// = 1 − exp(−A·e^(−κ·max(δ, 0))·dt), with A the fill rate at the mid. Here it drives the mock venue.
fn fill_probability(delta: f64, a: f64, kappa: f64, dt: f64) -> f64 {
    let lam = a * (-kappa * delta.max(0.0)).exp();
    -(-lam * dt).exp_m1()
}

// ---------------------------------------------------------------------------------------------------------------------
// Parameters, errors and fixed point
// ---------------------------------------------------------------------------------------------------------------------

/// Avellaneda–Stoikov parameters in the engine's units. The names are those of the `engine` block of the lab's
/// `calibration.json` (and of `ASParams` in bots/inventory-cpp), so a calibration loads as is.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AsParams {
    /// γ, 1/(price · token0).
    gamma: f64,
    /// τ, seconds: a rolling horizon, re-armed every tick (the C++ engine's stationary approximation).
    horizon_sec: f64,
    /// σ², price² per second.
    sigma2_per_sec: f64,
    /// κ, 1/price.
    kappa: f64,
}

impl AsParams {
    /// Mock parameters for a USDB/BERRY-like market (mid ≈ 2): σ = 0.0005 per √s, a mean fill depth of 0.002 (κ = 500),
    /// γ = 0.1 and the C++ engine's 60 s horizon. A calibration from the lab replaces them (`--calibration`).
    const MOCK: Self = Self {
        gamma: 0.1,
        horizon_sec: 60.0,
        sigma2_per_sec: 2.5e-7,
        kappa: 500.0,
    };

    fn validate(&self) -> Result<(), EngineError> {
        let positive = |x: f64| x.is_finite() && x > 0.0;
        let non_negative = |x: f64| x.is_finite() && x >= 0.0;
        if !positive(self.gamma) {
            return Err(EngineError::Params("gamma must be positive and finite"));
        }
        if !positive(self.kappa) {
            return Err(EngineError::Params("kappa must be positive and finite"));
        }
        if !non_negative(self.sigma2_per_sec) {
            return Err(EngineError::Params(
                "sigma2PerSec must be non-negative and finite",
            ));
        }
        if !non_negative(self.horizon_sec) {
            return Err(EngineError::Params(
                "horizonSec must be non-negative and finite",
            ));
        }
        Ok(())
    }

    /// Reads the `engine` block of a `calibration.json` written by the lab (`run_sim.py`).
    fn from_calibration(path: &Path) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Calibration {
            engine: AsParams,
        }
        let at = |e: &dyn fmt::Display| format!("{}: {e}", path.display());
        let text = std::fs::read_to_string(path).map_err(|e| at(&e))?;
        let calibration: Calibration = serde_json::from_str(&text).map_err(|e| at(&e))?;
        calibration.engine.validate().map_err(|e| at(&e))?;
        Ok(calibration.engine)
    }
}

/// Why a tick produced no quote, or why the engine stopped.
#[derive(Clone, Debug, PartialEq)]
enum EngineError {
    /// A parameter is outside its domain.
    Params(&'static str),
    /// An on-chain value did not convert (a zero price, a balance beyond 2²⁵⁵).
    Conversion(String),
    /// The model produced a non-finite number; nothing is quoted on it.
    NonFinite,
    /// A data source or the venue failed.
    Feed(String),
    /// The reads missed FETCH_BUDGET.
    Timeout,
    /// The oracle reading is older than MAX_ORACLE_AGE_SEC, or stamped too far in the future.
    StaleOracle { updated_at: u64, now: u64 },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Params(m) => write!(f, "invalid parameters: {m}"),
            Self::Conversion(m) => write!(f, "conversion: {m}"),
            Self::NonFinite => f.write_str("the model produced a non-finite price"),
            Self::Feed(m) => write!(f, "data source: {m}"),
            Self::Timeout => write!(f, "reads missed the {} ms budget", FETCH_BUDGET.as_millis()),
            Self::StaleOracle { updated_at, now } => write!(
                f,
                "oracle reading stamped {updated_at}, now {now}: outside [now − {MAX_ORACLE_AGE_SEC} s, now + \
                 {MAX_CLOCK_SKEW_SEC} s]"
            ),
        }
    }
}

impl std::error::Error for EngineError {}

/// 18-decimal fixed point (unsigned or signed) to f64 with a single correct rounding: the exact decimal expansion is
/// parsed, and Rust's float parsing rounds to nearest. Every U256 fits (2²⁵⁶ / 10¹⁸ ≈ 1.2e59).
fn wad_to_f64(value: impl Into<ParseUnits>) -> Result<f64, EngineError> {
    let text = format_units(value, DECIMALS).map_err(|e| EngineError::Conversion(e.to_string()))?;
    text.parse()
        .map_err(|_| EngineError::Conversion(format!("{text} is not a decimal")))
}

/// A decimal literal in 18-decimal fixed point, for constants (panics on a malformed literal).
fn wad(decimal: &str) -> U256 {
    parse_units(decimal, DECIMALS)
        .expect("a valid decimal literal")
        .get_absolute()
}

/// A tick count as an on-chain price, or None if the count is not a whole number of ticks in [1, 2^53].
fn ticks_to_wad(count: f64, tick: U256) -> Option<U256> {
    if !(1.0..=MAX_TICKS).contains(&count) || count.fract() != 0.0 {
        return None;
    }
    // Exact: count is an integer below 2^53.
    U256::from(count as u64).checked_mul(tick)
}

// ---------------------------------------------------------------------------------------------------------------------
// Engine state
// ---------------------------------------------------------------------------------------------------------------------

/// The market as the chain sees it, in 18-decimal fixed point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarketSpec {
    /// Price tick, token1 per token0; quotes are whole multiples of it.
    tick: U256,
    /// Size of one quote, token0.
    lot: U256,
    /// token0 balance the executor aims to hold: q = balance − target.
    target_inventory: U256,
    /// Cap on |q|, token0: the side whose fill would breach it is not quoted (the lab's `--max-inventory`).
    max_inventory: U256,
}

impl MarketSpec {
    /// The mock market: a 0.0001 tick, lots of 100 token0, a 10 000 token0 target and a 1 000 token0 (ten-lot) cap.
    #[cfg(test)]
    fn mock() -> Self {
        Self::mock_with_lot(wad("100"))
    }

    /// The mock market with another lot: a cap of ten lots and a target of a hundred, so the balance never runs out.
    /// The lot must be at most 10^12 token0.
    fn mock_with_lot(lot: U256) -> Self {
        Self {
            tick: wad("0.0001"),
            lot,
            target_inventory: lot * U256::from(100u8),
            max_inventory: lot * U256::from(10u8),
        }
    }
}

/// One requote: the model's numbers and the prices that go on chain.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Quote {
    /// s, the oracle mid the quote was computed on.
    mid: f64,
    /// q, token0.
    inventory: f64,
    /// r.
    reservation: f64,
    /// δ, the model's total spread, before the guards and the rounding.
    spread: f64,
    /// Bid on chain, a multiple of the tick; None when that side is not quoted.
    bid: Option<U256>,
    /// Ask on chain, a multiple of the tick; None when that side is not quoted.
    ask: Option<U256>,
}

/// Everything the engine carries from one tick to the next.
#[derive(Debug)]
struct HFTState {
    params: AsParams,
    market: MarketSpec,
    /// f64 views of the market spec, converted once.
    tick: f64,
    lot: f64,
    max_inventory: f64,
    /// Last oracle mid and inventory read.
    mid: Option<f64>,
    inventory: Option<f64>,
    /// The quote resting on the venue, if any.
    quote: Option<Quote>,
    /// Ticks run; ticks that quoted; ticks that could not quote (stale or failed reads, model errors).
    ticks: u64,
    quoted: u64,
    pulled: u64,
    /// Slowest tick: reads, model and submission.
    max_latency: Duration,
}

impl HFTState {
    fn new(params: AsParams, market: MarketSpec) -> Result<Self, EngineError> {
        params.validate()?;
        if market.tick.is_zero() || market.lot.is_zero() {
            return Err(EngineError::Params("the tick and the lot must be positive"));
        }
        if market.max_inventory < market.lot {
            return Err(EngineError::Params(
                "the inventory cap must be at least one lot",
            ));
        }
        Ok(Self {
            params,
            market,
            tick: wad_to_f64(market.tick)?,
            lot: wad_to_f64(market.lot)?,
            max_inventory: wad_to_f64(market.max_inventory)?,
            mid: None,
            inventory: None,
            quote: None,
            ticks: 0,
            quoted: 0,
            pulled: 0,
            max_latency: Duration::ZERO,
        })
    }

    /// One requote from fresh reads, as the chain returns them: the oracle mid and the executor's token0 balance.
    /// Applies the closed form with the lab's guards, then the engine's own: a non-finite result is an error, never a
    /// quote; the side whose fill would breach the inventory cap is not quoted, nor a bid below one tick; and if the
    /// rounding leaves the bid and the ask on the same tick, they are moved one tick apart on the side the inventory
    /// wants to trade less (the bid when long, the ask otherwise).
    fn on_market(&mut self, spot: U256, balance: U256) -> Result<Quote, EngineError> {
        let mid = wad_to_f64(spot)?;
        if mid <= 0.0 {
            return Err(EngineError::Conversion("the oracle price is zero".into()));
        }
        let signed = |v: U256| {
            I256::try_from(v).map_err(|_| EngineError::Conversion(format!("{v} exceeds 2^255")))
        };
        let q = signed(balance)?
            .checked_sub(signed(self.market.target_inventory)?)
            .ok_or_else(|| EngineError::Conversion("inventory out of range".into()))?;
        let q = wad_to_f64(q)?;
        self.mid = Some(mid);
        self.inventory = Some(q);

        let p = self.params;
        let spread = optimal_spread(p.gamma, p.sigma2_per_sec, p.horizon_sec, p.kappa);
        let (r, bid, ask) = quotes(
            mid,
            q,
            p.gamma,
            p.sigma2_per_sec,
            p.horizon_sec,
            p.kappa,
            0.0,
        );
        // f64::min and max ignore a NaN operand, so r and the spread are checked themselves, not just the quotes.
        if ![r, spread, bid, ask].iter().all(|x| x.is_finite()) {
            return Err(EngineError::NonFinite);
        }
        let (mut bid_n, mut ask_n) = (bid_ticks(bid, self.tick), ask_ticks(ask, self.tick));
        let can_buy = q + self.lot <= self.max_inventory + CAP_EPS;
        let can_sell = q - self.lot >= -self.max_inventory - CAP_EPS;
        if can_buy && can_sell && bid_n >= ask_n {
            if q > 0.0 {
                bid_n = ask_n - 1.0;
            } else {
                ask_n = bid_n + 1.0;
            }
        }
        Ok(Quote {
            mid,
            inventory: q,
            reservation: r,
            spread,
            bid: if can_buy {
                ticks_to_wad(bid_n, self.market.tick)
            } else {
                None
            },
            ask: if can_sell {
                ticks_to_wad(ask_n, self.market.tick)
            } else {
                None
            },
        })
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Data sources and the event loop
// ---------------------------------------------------------------------------------------------------------------------

/// One oracle reading, in the shape `IZKOracle.latestVerifiedPrice()` returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OracleReading {
    /// token0 priced in token1, 18-decimal fixed point.
    price: U256,
    /// Unix time of the attested observation, seconds.
    updated_at: u64,
}

/// The price of a reading that is fresh at `now` (Unix seconds): not older than MAX_ORACLE_AGE_SEC and not stamped
/// more than MAX_CLOCK_SKEW_SEC in the future. The contract reverts a rebalance on an oracle older than 60 s.
fn fresh_price(reading: OracleReading, now: u64) -> Result<U256, EngineError> {
    let not_too_old = now.saturating_sub(reading.updated_at) <= MAX_ORACLE_AGE_SEC;
    let not_in_future = reading.updated_at <= now.saturating_add(MAX_CLOCK_SKEW_SEC);
    if not_too_old && not_in_future {
        Ok(reading.price)
    } else {
        Err(EngineError::StaleOracle {
            updated_at: reading.updated_at,
            now,
        })
    }
}

/// Wall-clock Unix time, seconds (oracle timestamps are block times, which follow it). A clock before 1970 reads 0,
/// which makes every reading look stamped in the future: no quote, rather than a quote on a bad clock.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Source of the mid: the zk-attested oracle the DarkPool trusts. Mocked in this iteration; live, it is
/// `IZKOracle(darkPool.oracle()).latestVerifiedPrice()` read through an alloy provider.
trait Oracle {
    async fn fetch_oracle_price(&mut self) -> Result<OracleReading, EngineError>;
}

/// Where the inventory sits and where the quotes go. Mocked in this iteration. Live, the inventory is the
/// SearcherExecutor's token0 balance (`IERC20.balanceOf`), and the quotes are not resting orders: as in
/// bots/inventory-cpp, the bid and the ask feed the curve planner, which sizes a flash rebalance of the DarkPool toward
/// the oracle price and sends `SearcherExecutor.rebalance` after a preflight.
trait Venue {
    /// The executor's token0 balance, 18-decimal fixed point.
    async fn fetch_onchain_inventory(&mut self) -> Result<U256, EngineError>;
    /// Hands the current quote to execution; None stands it down.
    async fn submit_quotes(&mut self, quote: Option<&Quote>) -> Result<(), EngineError>;
}

/// The event loop. Every TICK it reads the oracle and the inventory concurrently, within FETCH_BUDGET, requotes and
/// submits. A late or failed read, or a model error, pulls the resting quote rather than leave a stale one. A tick
/// that overruns skips the ticks it missed instead of bunching them. Returns after `max_ticks` ticks or when
/// `shutdown` completes, and pulls the quote either way. Prints a line every `log_every` ticks (0: silent) and
/// whenever quoting stops or resumes.
async fn run<O: Oracle, V: Venue>(
    state: &mut HFTState,
    oracle: &mut O,
    venue: &mut V,
    max_ticks: Option<u64>,
    log_every: u64,
    shutdown: impl Future<Output = ()>,
) -> Result<(), EngineError> {
    let mut interval = time::interval(TICK);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tokio::pin!(shutdown);
    let start = Instant::now();
    let mut was_quoting = true;
    while max_ticks.is_none_or(|n| state.ticks < n) {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            _ = interval.tick() => {}
        }
        let tick_start = Instant::now();
        let reads = time::timeout(FETCH_BUDGET, async {
            tokio::join!(oracle.fetch_oracle_price(), venue.fetch_onchain_inventory())
        })
        .await;
        let outcome = match reads {
            Ok((Ok(reading), Ok(balance))) => {
                fresh_price(reading, unix_now()).and_then(|spot| state.on_market(spot, balance))
            }
            Ok((Err(e), _) | (_, Err(e))) => Err(e),
            Err(_) => Err(EngineError::Timeout),
        };
        state.ticks += 1;
        match &outcome {
            Ok(quote) => {
                venue.submit_quotes(Some(quote)).await?;
                state.quote = Some(*quote);
                state.quoted += 1;
            }
            Err(_) => {
                if state.quote.take().is_some() {
                    venue.submit_quotes(None).await?;
                }
                state.pulled += 1;
            }
        }
        state.max_latency = state.max_latency.max(tick_start.elapsed());
        let quoting = outcome.is_ok();
        if log_every > 0 && (state.ticks.is_multiple_of(log_every) || quoting != was_quoting) {
            log_tick(start.elapsed(), &outcome);
        }
        was_quoting = quoting;
    }
    venue.submit_quotes(None).await?;
    state.quote = None;
    Ok(())
}

fn log_tick(elapsed: Duration, outcome: &Result<Quote, EngineError>) {
    let t = elapsed.as_secs_f64();
    match outcome {
        Ok(q) => {
            let side = |px: Option<U256>| match px.map(wad_to_f64) {
                Some(Ok(p)) => format!("{p:.4}"),
                _ => "  —   ".into(),
            };
            println!(
                "t={t:>8.1}s  mid {:.6}  q {:+9.1}  r {:.6}  δ {:.6}  bid {}  ask {}",
                q.mid,
                q.inventory,
                q.reservation,
                q.spread,
                side(q.bid),
                side(q.ask)
            );
        }
        Err(e) => println!("t={t:>8.1}s  no quote: {e}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Mocks: on-chain-shaped values, no network
// ---------------------------------------------------------------------------------------------------------------------

/// Simulated RPC round trip of each mock read.
const MOCK_LATENCY: Duration = Duration::from_millis(2);
/// Fill rate of a quote at the mid in the mock venue, per second (the lab's A).
const MOCK_FILLS_PER_SEC_AT_MID: f64 = 0.5;

/// SplitMix64 (Steele, Lea and Flood, 2014): small, fast and well tested; deterministic for a given seed.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform on [0, 1), from 53 random bits.
    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
    }

    /// Standard normal (Box–Muller).
    fn normal(&mut self) -> f64 {
        let u1 = 1.0 - self.uniform(); // in (0, 1], so the logarithm is finite
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Mock of `IZKOracle.latestVerifiedPrice()`: the model's own dynamics, dS = σ dW, one TICK step per read (floored at
/// one tick so the price stays positive), in 18-decimal fixed point and stamped with the current time, after a
/// simulated round trip.
struct MockOracle {
    mid: f64,
    step_sd: f64,
    floor: f64,
    latency: Duration,
    rng: SplitMix64,
}

impl MockOracle {
    fn new(mid: f64, sigma2_per_sec: f64, floor: f64, seed: u64) -> Self {
        let step_sd = (sigma2_per_sec * TICK.as_secs_f64()).sqrt();
        Self {
            mid,
            step_sd,
            floor,
            latency: MOCK_LATENCY,
            rng: SplitMix64(seed),
        }
    }
}

impl Oracle for MockOracle {
    async fn fetch_oracle_price(&mut self) -> Result<OracleReading, EngineError> {
        time::sleep(self.latency).await;
        self.mid = (self.mid + self.step_sd * self.rng.normal()).max(self.floor);
        let price = parse_units(&format!("{:.18}", self.mid), DECIMALS)
            .map(ParseUnits::get_absolute)
            .map_err(|e| EngineError::Feed(format!("mock price {}: {e}", self.mid)))?;
        Ok(OracleReading {
            price,
            updated_at: unix_now(),
        })
    }
}

/// Mock of the SearcherExecutor, with the Python lab's execution model: the quote handed over rests, and between reads
/// each side fills one lot with probability 1 − exp(−A·e^(−κδ)·dt), δ the side's distance from the mid it was quoted on
/// and dt one TICK. A sell the balance cannot cover is skipped. Nothing is sent anywhere.
struct MockVenue {
    balance: U256,
    lot: U256,
    fills_per_sec_at_mid: f64,
    kappa: f64,
    latency: Duration,
    rng: SplitMix64,
    resting: Option<Quote>,
    buys: u64,
    sells: u64,
}

impl MockVenue {
    fn new(balance: U256, lot: U256, kappa: f64, seed: u64) -> Self {
        Self {
            balance,
            lot,
            fills_per_sec_at_mid: MOCK_FILLS_PER_SEC_AT_MID,
            kappa,
            latency: MOCK_LATENCY,
            rng: SplitMix64(seed),
            resting: None,
            buys: 0,
            sells: 0,
        }
    }

    fn fills(&mut self, distance: f64) -> bool {
        self.rng.uniform()
            < fill_probability(
                distance,
                self.fills_per_sec_at_mid,
                self.kappa,
                TICK.as_secs_f64(),
            )
    }
}

impl Venue for MockVenue {
    async fn fetch_onchain_inventory(&mut self) -> Result<U256, EngineError> {
        time::sleep(self.latency).await;
        if let Some(quote) = self.resting {
            if let Some(ask) = quote.ask
                && self.fills(wad_to_f64(ask)? - quote.mid)
                && let Some(rest) = self.balance.checked_sub(self.lot)
            {
                self.balance = rest;
                self.sells += 1;
            }
            if let Some(bid) = quote.bid
                && self.fills(quote.mid - wad_to_f64(bid)?)
            {
                self.balance = self
                    .balance
                    .checked_add(self.lot)
                    .ok_or_else(|| EngineError::Feed("balance overflow".into()))?;
                self.buys += 1;
            }
        }
        Ok(self.balance)
    }

    async fn submit_quotes(&mut self, quote: Option<&Quote>) -> Result<(), EngineError> {
        self.resting = quote.copied();
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------------------------------------------------

const USAGE: &str = "usage: hft-engine [--calibration PATH] [--ticks N] [--mid PRICE] [--lot TOKENS] [--seed N]

Runs the Avellaneda–Stoikov quoting loop (100 ms ticks) against MOCK data sources:
nothing is read from or sent to any chain.

  --calibration PATH  the Python lab's calibration.json (its `engine` block); default: mock parameters
  --ticks N           stop after N ticks (default: run until Ctrl-C)
  --mid PRICE         starting mid of the mock oracle (default: 2)
  --lot TOKENS        quote size in token0; the cap is ten lots (default: 100). γ is per token0, so use
                      the lot the calibration was run with (the lab's --lot, 1 by default)
  --seed N            seed of the mock oracle and venue (default: 7)";

struct Args {
    calibration: Option<PathBuf>,
    ticks: Option<u64>,
    mid: f64,
    lot: U256,
    seed: u64,
}

impl Args {
    /// Parses the arguments; Err(None) asks for the usage text.
    fn parse(mut it: impl Iterator<Item = String>) -> Result<Self, Option<String>> {
        let mut args = Self {
            calibration: None,
            ticks: None,
            mid: 2.0,
            lot: wad("100"),
            seed: 7,
        };
        while let Some(flag) = it.next() {
            let mut value = || {
                it.next()
                    .ok_or_else(|| Some(format!("{flag} needs a value")))
            };
            match flag.as_str() {
                "--calibration" => args.calibration = Some(PathBuf::from(value()?)),
                "--ticks" => {
                    args.ticks = Some(
                        value()?
                            .parse()
                            .map_err(|e| Some(format!("--ticks: {e}")))?,
                    )
                }
                "--seed" => {
                    args.seed = value()?.parse().map_err(|e| Some(format!("--seed: {e}")))?
                }
                "--mid" => {
                    let mid: f64 = value()?.parse().map_err(|e| Some(format!("--mid: {e}")))?;
                    if !(mid.is_finite() && mid > 0.0) {
                        return Err(Some("--mid must be a positive price".into()));
                    }
                    args.mid = mid;
                }
                "--lot" => {
                    let text = value()?;
                    let lot = parse_units(&text, DECIMALS)
                        .ok()
                        .filter(|lot| {
                            !lot.is_negative()
                                && !lot.get_absolute().is_zero()
                                && lot.get_absolute() <= wad("1000000000000")
                        })
                        .ok_or_else(|| {
                            Some(format!(
                                "--lot: {text} is not an amount in (0, 10^12] token0"
                            ))
                        })?;
                    args.lot = lot.get_absolute();
                }
                "-h" | "--help" => return Err(None),
                other => return Err(Some(format!("unknown argument {other}"))),
            }
        }
        Ok(args)
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
    let market = MarketSpec::mock_with_lot(args.lot);
    let mut state = match HFTState::new(params, market) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let mut oracle = MockOracle::new(args.mid, params.sigma2_per_sec, state.tick, args.seed);
    let mut venue = MockVenue::new(
        market.target_inventory,
        market.lot,
        params.kappa,
        args.seed.wrapping_add(1),
    );

    let show = |v: U256| wad_to_f64(v).map_or_else(|e| e.to_string(), |x| x.to_string());
    println!(
        "hft-engine: MOCK data sources, no chain access; {} ms ticks, {} ms read budget",
        TICK.as_millis(),
        FETCH_BUDGET.as_millis()
    );
    println!(
        "model: γ {} · σ² {} /s · τ {} s · κ {}  ({source})",
        params.gamma, params.sigma2_per_sec, params.horizon_sec, params.kappa
    );
    println!(
        "market: tick {} · lot {} · target {} · cap ±{} token0",
        show(market.tick),
        show(market.lot),
        show(market.target_inventory),
        show(market.max_inventory)
    );

    let shutdown = async {
        // Without a signal handler, run until --ticks rather than stop at once.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let result = run(
        &mut state,
        &mut oracle,
        &mut venue,
        args.ticks,
        10,
        shutdown,
    )
    .await;
    println!(
        "{} ticks: {} quoted, {} pulled; slowest tick {:.3} ms; mock fills {} buys, {} sells; last mid {:.6}, inventory {:+.1} token0",
        state.ticks,
        state.quoted,
        state.pulled,
        state.max_latency.as_secs_f64() * 1e3,
        venue.buys,
        venue.sells,
        state.mid.unwrap_or(f64::NAN),
        state.inventory.unwrap_or(f64::NAN)
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;

    /// Distance between two doubles in units in the last place (0 when equal, ±0 included).
    fn ulps(a: f64, b: f64) -> u64 {
        if a == b {
            return 0;
        }
        let key = |x: f64| {
            let bits = x.to_bits() as i64;
            i128::from(if bits < 0 { i64::MIN - bits } else { bits })
        };
        (key(a) - key(b)).unsigned_abs() as u64
    }

    fn mock_state() -> HFTState {
        HFTState::new(AsParams::MOCK, MarketSpec::mock()).unwrap()
    }

    fn mocks(state: &HFTState, seed: u64) -> (MockOracle, MockVenue) {
        let oracle = MockOracle::new(2.0, state.params.sigma2_per_sec, state.tick, seed);
        let venue = MockVenue::new(
            state.market.target_inventory,
            state.market.lot,
            state.params.kappa,
            seed + 1,
        );
        (oracle, venue)
    }

    // ---- the closed forms against the Python lab ----

    #[test]
    fn quotes_match_the_python_lab() {
        let mut worst = 0;
        for row in lab_golden::QUOTES {
            let [mid, q, gamma, sigma2, tau, kappa, tick, r, bid, ask, spread] = *row;
            let (got_r, got_bid, got_ask) = quotes(mid, q, gamma, sigma2, tau, kappa, tick);
            let got_spread = optimal_spread(gamma, sigma2, tau, kappa);
            assert_eq!(
                reservation_price(mid, q, gamma, sigma2, tau).to_bits(),
                got_r.to_bits()
            );
            for (name, got, lab) in [
                ("r", got_r, r),
                ("bid", got_bid, bid),
                ("ask", got_ask, ask),
                ("δ", got_spread, spread),
            ] {
                let d = ulps(got, lab);
                assert!(
                    d <= 2,
                    "{name} = {got:e}, lab {lab:e} ({d} ulps) for {row:?}"
                );
                worst = worst.max(d);
            }
        }
        println!(
            "{} cases, worst difference from the lab: {worst} ulps",
            lab_golden::QUOTES.len()
        );
    }

    #[test]
    fn fill_probability_matches_the_python_lab() {
        let mut worst = 0;
        for row in lab_golden::FILL_PROBABILITY {
            let [delta, a, kappa, dt, p] = *row;
            let got = fill_probability(delta, a, kappa, dt);
            let d = ulps(got, p);
            assert!(d <= 2, "{got:e}, lab {p:e} ({d} ulps) for {row:?}");
            worst = worst.max(d);
        }
        println!(
            "{} cases, worst difference from the lab: {worst} ulps",
            lab_golden::FILL_PROBABILITY.len()
        );
    }

    // ---- the mathematics ----

    #[test]
    fn the_reservation_price_leans_against_the_inventory() {
        let p = AsParams::MOCK;
        let r = |q| reservation_price(2.0, q, p.gamma, p.sigma2_per_sec, p.horizon_sec);
        assert_eq!(r(0.0), 2.0);
        assert!(r(100.0) < 2.0 && r(-100.0) > 2.0);
        // 100 token0 · 0.1 · 2.5e-7 /s · 60 s = 1.5e-4
        assert!(
            (2.0 - r(100.0) - 1.5e-4).abs() < 1e-15 && (r(-100.0) - 2.0 - 1.5e-4).abs() < 1e-15
        );
    }

    #[test]
    fn the_spread_keeps_full_precision_when_gamma_over_kappa_is_tiny() {
        let kappa = 1e4_f64;
        // γ/κ = 1e-17 is below 2^-53: 1 + γ/κ rounds to 1 and the naive logarithm loses the whole spread.
        let gamma = 1e-13_f64;
        assert_eq!((1.0 + gamma / kappa).ln(), 0.0);
        let spread = optimal_spread(gamma, 0.0, 60.0, kappa);
        assert!((spread * kappa / 2.0 - 1.0).abs() < 1e-15, "{spread:e}");
        // Towards the risk-neutral limit 2/κ: (κ/γ)·ln(1 + x) = 1 − x/2 + O(x²) with x = γ/κ, so the relative gap is x/2
        // up to x², plus the rounding of the few operations (a few ε).
        for gamma in [1e-10, 1e-8, 1e-6, 1e-4] {
            let x = gamma / kappa;
            let gap = 1.0 - optimal_spread(gamma, 0.0, 60.0, kappa) * kappa / 2.0;
            assert!(
                (gap - x / 2.0).abs() <= x * x + 4.0 * f64::EPSILON,
                "γ = {gamma:e}: gap {gap:e}"
            );
        }
    }

    #[test]
    fn tick_rounding_is_outward_and_absorbs_binary_error() {
        // 0.3 / 0.0001 = 2999.9999999999995 in binary: a plain floor would drop a whole tick.
        assert_eq!((0.3f64 / 1e-4).floor(), 2999.0);
        assert_eq!(bid_ticks(0.3, 1e-4), 3000.0);
        assert_eq!(bid_ticks(19.99, 0.01), 1999.0);
        // 0.07 / 0.01 = 7.000000000000001: a plain ceil would add one.
        assert_eq!((0.07f64 / 0.01).ceil(), 8.0);
        assert_eq!(ask_ticks(0.07, 0.01), 7.0);
        // Between ticks, the bid goes down and the ask up.
        assert_eq!(bid_ticks(1.99795, 1e-4), 19979.0);
        assert_eq!(ask_ticks(1.99795, 1e-4), 19980.0);
    }

    #[test]
    fn fixed_point_conversions_are_correctly_rounded() {
        assert_eq!(wad_to_f64(wad("1")).unwrap(), 1.0);
        assert_eq!(wad_to_f64(wad("1.9998")).unwrap(), 1.9998);
        assert_eq!(wad_to_f64(U256::from(1u8)).unwrap(), 1e-18);
        assert_eq!(
            wad_to_f64(parse_units("-1.5", DECIMALS).unwrap().get_signed()).unwrap(),
            -1.5
        );
        assert!(wad_to_f64(U256::MAX).unwrap().is_finite());
        // In fixed point 0.1 + 0.2 is exactly 0.3, and converts to the double nearest 0.3 (in f64, 0.1 + 0.2 is not).
        assert_ne!(0.1 + 0.2, 0.3);
        assert_eq!(wad_to_f64(wad("0.1") + wad("0.2")).unwrap(), 0.3);
        // Tick counts go back on chain exactly.
        assert_eq!(ticks_to_wad(19_979.0, wad("0.0001")), Some(wad("1.9979")));
        for bad in [0.0, -1.0, 0.5, f64::NAN, f64::INFINITY, 1e300] {
            assert_eq!(ticks_to_wad(bad, wad("0.0001")), None, "{bad}");
        }
    }

    // ---- the engine state ----

    #[test]
    fn quotes_sit_on_the_tick_grid_and_lean_against_the_inventory() {
        let mut s = mock_state();
        let target = s.market.target_inventory;
        let flat = s.on_market(wad("2"), target).unwrap();
        assert_eq!(flat.inventory, 0.0);
        assert_eq!(flat.reservation, 2.0);
        let (bid, ask) = (flat.bid.unwrap(), flat.ask.unwrap());
        assert!(bid < wad("2") && ask > wad("2"));
        assert!((bid % s.market.tick).is_zero() && (ask % s.market.tick).is_zero());
        // Long 300 token0: r is 4.5 ticks below the mid, and both quotes move down.
        let long = s.on_market(wad("2"), target + wad("300")).unwrap();
        assert_eq!(long.inventory, 300.0);
        assert!(long.reservation < 2.0);
        assert!(long.bid.unwrap() < bid && long.ask.unwrap() < ask);
        assert_eq!((s.mid, s.inventory), (Some(2.0), Some(300.0)));
    }

    #[test]
    fn the_side_that_would_breach_the_cap_is_not_quoted() {
        let mut s = mock_state();
        let target = s.market.target_inventory;
        // Cap 1 000, lot 100: at +950 another buy would breach it; at +900 it would reach it exactly, which is allowed.
        let q = s.on_market(wad("2"), target + wad("950")).unwrap();
        assert!(q.bid.is_none() && q.ask.is_some());
        let q = s.on_market(wad("2"), target - wad("950")).unwrap();
        assert!(q.bid.is_some() && q.ask.is_none());
        let q = s.on_market(wad("2"), target + wad("900")).unwrap();
        assert!(q.bid.is_some() && q.ask.is_some());
    }

    #[test]
    fn a_spread_under_one_tick_is_widened_to_one_tick() {
        // σ² = 0 and κ = 1e14: δ ≈ 2/κ = 2e-14, far under the 0.0001 tick, so rounding puts both sides on the mid.
        let params = AsParams {
            gamma: 0.1,
            horizon_sec: 60.0,
            sigma2_per_sec: 0.0,
            kappa: 1e14,
        };
        let mut s = HFTState::new(params, MarketSpec::mock()).unwrap();
        let target = s.market.target_inventory;
        let flat = s.on_market(wad("2"), target).unwrap();
        assert_eq!((flat.bid, flat.ask), (Some(wad("2")), Some(wad("2.0001"))));
        // Long: the ask stays at the mid and the bid steps down.
        let long = s.on_market(wad("2"), target + wad("100")).unwrap();
        assert_eq!((long.bid, long.ask), (Some(wad("1.9999")), Some(wad("2"))));
    }

    #[test]
    fn bad_inputs_are_errors_never_quotes() {
        let mut s = mock_state();
        let target = s.market.target_inventory;
        assert!(matches!(
            s.on_market(U256::ZERO, target),
            Err(EngineError::Conversion(_))
        ));
        assert!(matches!(
            s.on_market(wad("2"), U256::MAX),
            Err(EngineError::Conversion(_))
        ));
        // γσ²τ overflows to infinity.
        let huge = AsParams {
            gamma: 1e300,
            horizon_sec: 1e300,
            sigma2_per_sec: 1e300,
            kappa: 1.0,
        };
        let mut s = HFTState::new(huge, MarketSpec::mock()).unwrap();
        assert_eq!(
            s.on_market(wad("2"), target + wad("1")),
            Err(EngineError::NonFinite)
        );
    }

    #[test]
    fn parameters_outside_their_domain_are_rejected() {
        let m = AsParams::MOCK;
        for bad in [
            AsParams { gamma: 0.0, ..m },
            AsParams {
                gamma: f64::INFINITY,
                ..m
            },
            AsParams { kappa: 0.0, ..m },
            AsParams { kappa: -1.0, ..m },
            AsParams {
                sigma2_per_sec: -1e-9,
                ..m
            },
            AsParams {
                sigma2_per_sec: f64::NAN,
                ..m
            },
            AsParams {
                horizon_sec: -1.0,
                ..m
            },
        ] {
            assert!(HFTState::new(bad, MarketSpec::mock()).is_err(), "{bad:?}");
        }
        let tight = MarketSpec {
            max_inventory: wad("50"),
            ..MarketSpec::mock()
        };
        assert!(HFTState::new(m, tight).is_err());
    }

    #[test]
    fn loads_the_engine_block_of_a_lab_calibration() {
        // The engine block as run_sim.py writes it (synthetic data run), with the rest of the file trimmed.
        let json = r#"{"label": "synthetic", "engine": {"gamma": 0.016, "horizonSec": 11670.0,
            "sigma2PerSec": 0.00039623631218582077, "kappa": 13.803083097931438}}"#;
        let path = std::env::temp_dir().join(format!(
            "hft-engine-calibration-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();
        let params = AsParams::from_calibration(&path);
        std::fs::write(
            &path,
            r#"{"engine": {"gamma": 0, "horizonSec": 1, "sigma2PerSec": 1, "kappa": 1}}"#,
        )
        .unwrap();
        let invalid = AsParams::from_calibration(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            params,
            Ok(AsParams {
                gamma: 0.016,
                horizon_sec: 11670.0,
                sigma2_per_sec: 0.00039623631218582077,
                kappa: 13.803083097931438
            })
        );
        assert!(invalid.unwrap_err().contains("gamma must be positive"));
    }

    // ---- the event loop, on paused virtual time ----

    /// Checks every quote and inventory the loop produces against the engine's invariants.
    struct Checked {
        inner: MockVenue,
        market: MarketSpec,
        tick: f64,
        submissions: Vec<Option<Quote>>,
        one_sided: u64,
    }

    impl Venue for Checked {
        async fn fetch_onchain_inventory(&mut self) -> Result<U256, EngineError> {
            let balance = self.inner.fetch_onchain_inventory().await?;
            let target = self.market.target_inventory;
            let q = if balance >= target {
                balance - target
            } else {
                target - balance
            };
            assert!(
                q <= self.market.max_inventory,
                "inventory {balance} breaches the cap"
            );
            Ok(balance)
        }

        async fn submit_quotes(&mut self, quote: Option<&Quote>) -> Result<(), EngineError> {
            if let Some(q) = quote {
                let slack = self.tick * 1e-6;
                if let Some(bid) = q.bid {
                    assert!((bid % self.market.tick).is_zero());
                    assert!(
                        wad_to_f64(bid)? <= q.mid + slack,
                        "bid above the mid: {q:?}"
                    );
                }
                if let Some(ask) = q.ask {
                    assert!((ask % self.market.tick).is_zero());
                    assert!(
                        wad_to_f64(ask)? >= q.mid - slack,
                        "ask below the mid: {q:?}"
                    );
                }
                match (q.bid, q.ask) {
                    (Some(bid), Some(ask)) => assert!(bid < ask, "crossed or locked: {q:?}"),
                    (None, None) => panic!("no side quoted: {q:?}"),
                    _ => self.one_sided += 1,
                }
            }
            self.submissions.push(quote.copied());
            self.inner.submit_quotes(quote).await
        }
    }

    fn checked(state: &HFTState, venue: MockVenue) -> Checked {
        Checked {
            inner: venue,
            market: state.market,
            tick: state.tick,
            submissions: Vec::new(),
            one_sided: 0,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_loop_ticks_every_100_ms() {
        let mut s = mock_state();
        let (mut oracle, mut venue) = mocks(&s, 1);
        let t0 = Instant::now();
        run(&mut s, &mut oracle, &mut venue, Some(50), 0, pending())
            .await
            .unwrap();
        // The first tick fires at once and the 50th at 4.9 s; it ends after one 2 ms mock round trip.
        assert_eq!(t0.elapsed(), Duration::from_millis(4_902));
        assert_eq!((s.ticks, s.quoted, s.pulled), (50, 50, 0));
        assert_eq!(s.max_latency, MOCK_LATENCY);
        assert!(
            venue.resting.is_none() && s.quote.is_none(),
            "the quote is pulled on exit"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn late_reads_pull_the_quote_and_quoting_resumes_when_they_recover() {
        let mut s = mock_state();
        let (mut oracle, venue) = mocks(&s, 2);
        let mut venue = checked(&s, venue);
        run(&mut s, &mut oracle, &mut venue, Some(5), 0, pending())
            .await
            .unwrap();
        oracle.latency = Duration::from_millis(150); // beyond the 80 ms budget
        run(&mut s, &mut oracle, &mut venue, Some(10), 0, pending())
            .await
            .unwrap();
        assert_eq!((s.quoted, s.pulled), (5, 5));
        oracle.latency = MOCK_LATENCY;
        run(&mut s, &mut oracle, &mut venue, Some(15), 0, pending())
            .await
            .unwrap();
        assert_eq!((s.quoted, s.pulled), (10, 5));
        // Five quotes and the pull on exit; nothing while the reads were late (nothing was resting); five quotes and
        // the pull on exit.
        let kinds: Vec<bool> = venue.submissions.iter().map(Option::is_some).collect();
        assert_eq!(
            kinds,
            [[true; 5].as_slice(), &[false, false], &[true; 5], &[false]].concat()
        );
    }

    #[test]
    fn only_fresh_oracle_readings_are_quoted_on() {
        let now = 1_800_000_000;
        let at = |updated_at| {
            fresh_price(
                OracleReading {
                    price: wad("2"),
                    updated_at,
                },
                now,
            )
        };
        for fresh in [now, now - 45, now + 5] {
            assert_eq!(at(fresh), Ok(wad("2")), "{fresh}");
        }
        for stale in [now - 46, now + 6, 0, u64::MAX] {
            assert_eq!(
                at(stale),
                Err(EngineError::StaleOracle {
                    updated_at: stale,
                    now
                }),
                "{stale}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_or_a_stale_oracle_pulls_the_quote() {
        // Read 3 fails, read 4 returns a reading a minute old; the others are fresh.
        struct Scripted(u64);
        impl Oracle for Scripted {
            async fn fetch_oracle_price(&mut self) -> Result<OracleReading, EngineError> {
                self.0 += 1;
                match self.0 {
                    3 => Err(EngineError::Feed("node unavailable".into())),
                    4 => Ok(OracleReading {
                        price: wad("2"),
                        updated_at: unix_now() - 60,
                    }),
                    _ => Ok(OracleReading {
                        price: wad("2"),
                        updated_at: unix_now(),
                    }),
                }
            }
        }
        let mut s = mock_state();
        let (_, venue) = mocks(&s, 3);
        let mut venue = checked(&s, venue);
        run(&mut s, &mut Scripted(0), &mut venue, Some(5), 0, pending())
            .await
            .unwrap();
        // Quote, quote, pull on the failed read, nothing on the stale one (nothing rests), quote, pull on exit.
        let kinds: Vec<bool> = venue.submissions.iter().map(Option::is_some).collect();
        assert_eq!(kinds, [true, true, false, true, false]);
        assert_eq!((s.quoted, s.pulled), (3, 2));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_the_loop_and_pulls_the_quote() {
        let mut s = mock_state();
        let (mut oracle, mut venue) = mocks(&s, 4);
        run(
            &mut s,
            &mut oracle,
            &mut venue,
            None,
            0,
            time::sleep(Duration::from_millis(1_050)),
        )
        .await
        .unwrap();
        assert_eq!(s.ticks, 11); // t = 0, 0.1, …, 1.0 s
        assert!(venue.resting.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_run_keeps_every_invariant() {
        // A three-lot cap and ten times the default fill rate, so that the cap is reached on both sides.
        let market = MarketSpec {
            max_inventory: wad("300"),
            ..MarketSpec::mock()
        };
        let mut s = HFTState::new(AsParams::MOCK, market).unwrap();
        let (mut oracle, mut venue) = mocks(&s, 42);
        venue.fills_per_sec_at_mid = 5.0;
        let mut venue = checked(&s, venue);
        run(&mut s, &mut oracle, &mut venue, Some(20_000), 0, pending())
            .await
            .unwrap();
        assert_eq!((s.quoted, s.pulled), (20_000, 0));
        assert!(venue.one_sided > 0, "the inventory cap was never reached");
        assert!(venue.inner.buys > 1_000 && venue.inner.sells > 1_000);
    }

    #[tokio::test(start_paused = true)]
    async fn runs_are_reproducible_for_a_seed() {
        let mut outcomes = Vec::new();
        for _ in 0..2 {
            let mut s = mock_state();
            let (mut oracle, mut venue) = mocks(&s, 7);
            run(&mut s, &mut oracle, &mut venue, Some(2_000), 0, pending())
                .await
                .unwrap();
            outcomes.push((oracle.mid.to_bits(), venue.balance, venue.buys, venue.sells));
        }
        assert_eq!(outcomes[0], outcomes[1]);
    }
}
