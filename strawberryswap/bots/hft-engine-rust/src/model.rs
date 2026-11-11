//! Avellaneda–Stoikov closed forms, the engine's parameters and errors, and fixed point.
//!
//! The closed forms are the Python lab's (`hft-engine/python-sim/as_sim/model.py`), which are written exactly as in
//! `bots/inventory-cpp` (`avellaneda_stoikov.hpp`):
//!
//! ```text
//! reservation price      r = s − q·γ·σ²·τ
//! optimal total spread   δ = γ·σ²·τ + (2/γ)·ln(1 + γ/κ)
//! bid, ask               r ∓ δ/2
//! ```
//!
//! Units are those of the `engine` block of the lab's `calibration.json`: s, r, bid and ask are token1 per token0 (s
//! is the zk-attested oracle price); q, the signed inventory, is whole token0 (the executor's balance minus its
//! target); σ² is price² per second; τ, the rolling horizon, is seconds; γ is 1/(price · token0); κ is 1/price.

use std::fmt;
use std::path::Path;

use alloy::primitives::U256;
use alloy::primitives::utils::{ParseUnits, format_units};
use serde::Deserialize;

/// Decimals of on-chain prices and token amounts.
pub(crate) const DECIMALS: u8 = 18;
/// Oldest oracle reading the engine acts on, seconds: the C++ engine's limit, inside the contract's
/// `DarkPool.MAX_ORACLE_AGE` (60 s) with headroom for latency.
pub(crate) const MAX_ORACLE_AGE_SEC: u64 = 45;
/// How far in the future an oracle timestamp may be (clock skew), seconds; beyond it the reading is rejected.
pub(crate) const MAX_CLOCK_SKEW_SEC: u64 = 5;

// ---------------------------------------------------------------------------------------------------------------------
// Closed forms: the lab's functions, same arguments in the same order (σ² instead of σ) and the same operation order,
// so the results are the lab's bit for bit (the golden tests compare them).
// ---------------------------------------------------------------------------------------------------------------------

/// r = s − q·γ·σ²·τ. A long inventory pulls r below the mid (keener to sell), a short one above it.
pub(crate) fn reservation_price(
    mid: f64,
    inventory: f64,
    gamma: f64,
    sigma2: f64,
    tau: f64,
) -> f64 {
    mid - inventory * gamma * sigma2 * tau
}

/// Total spread δa + δb = γσ²τ + (2/γ)·ln(1 + γ/κ), for γ > 0 and κ > 0. `ln_1p` keeps full precision when γ/κ is
/// small, where ln(1 + γ/κ) loses digits (and is exactly 0 below 2⁻⁵³); the spread then tends to 2/κ, the risk-neutral
/// spread.
pub(crate) fn optimal_spread(gamma: f64, sigma2: f64, tau: f64, kappa: f64) -> f64 {
    gamma * sigma2 * tau + (2.0 / gamma) * (gamma / kappa).ln_1p()
}

/// Avellaneda–Stoikov parameters in the engine's units. The names are those of the `engine` block of the lab's
/// `calibration.json` (and of `ASParams` in bots/inventory-cpp), so a calibration loads as is.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AsParams {
    /// γ, 1/(price · token0).
    pub gamma: f64,
    /// τ, seconds: a rolling horizon, re-armed every tick (the C++ engine's stationary approximation).
    pub horizon_sec: f64,
    /// σ², price² per second.
    pub sigma2_per_sec: f64,
    /// κ, 1/price.
    pub kappa: f64,
}

impl AsParams {
    /// Mock parameters for a USDB/BERRY-like market (mid ≈ 2): σ = 0.0005 per √s, a mean fill depth of 0.002 (κ = 500),
    /// γ = 0.1 and the C++ engine's 60 s horizon. A calibration from the lab replaces them (`--calibration`).
    pub const MOCK: Self = Self {
        gamma: 0.1,
        horizon_sec: 60.0,
        sigma2_per_sec: 2.5e-7,
        kappa: 500.0,
    };

    pub fn validate(&self) -> Result<(), EngineError> {
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
    pub fn from_calibration(path: &Path) -> Result<Self, String> {
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

    /// The A-S reservation price and half spread for a mid and an inventory (token0), as bots/inventory-cpp hands
    /// them to its planner: bid = r − half and ask = r + half, with no tick grid and no clamp to the mid, because
    /// they are the planner's prices for trading against the pool, not resting orders. Errors on a non-finite result.
    pub fn quote(&self, mid: f64, inventory: f64) -> Result<(f64, f64), EngineError> {
        let r = reservation_price(
            mid,
            inventory,
            self.gamma,
            self.sigma2_per_sec,
            self.horizon_sec,
        );
        let half = 0.5
            * optimal_spread(
                self.gamma,
                self.sigma2_per_sec,
                self.horizon_sec,
                self.kappa,
            );
        if r.is_finite() && half.is_finite() {
            Ok((r, half))
        } else {
            Err(EngineError::NonFinite)
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------------------------------

/// Why a tick produced no transaction, or why the engine stopped.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EngineError {
    /// A parameter is outside its domain.
    Params(&'static str),
    /// An on-chain value did not convert (a zero price, an amount out of range).
    Conversion(String),
    /// The model produced a non-finite number; nothing is planned on it.
    NonFinite,
    /// A chain read or a broadcast failed.
    Feed(String),
    /// The reads missed their budget.
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
            Self::Feed(m) => write!(f, "chain: {m}"),
            Self::Timeout => write!(
                f,
                "reads missed the {} ms budget",
                crate::FETCH_BUDGET.as_millis()
            ),
            Self::StaleOracle { updated_at, now } => write!(
                f,
                "oracle reading stamped {updated_at}, now {now}: outside [now − {MAX_ORACLE_AGE_SEC} s, now + \
                 {MAX_CLOCK_SKEW_SEC} s]"
            ),
        }
    }
}

impl std::error::Error for EngineError {}

// ---------------------------------------------------------------------------------------------------------------------
// Fixed point
// ---------------------------------------------------------------------------------------------------------------------

/// 18-decimal fixed point (unsigned or signed) to f64 with a single correct rounding: the exact decimal expansion is
/// parsed, and Rust's float parsing rounds to nearest. Every U256 fits (2²⁵⁶ / 10¹⁸ ≈ 1.2e59).
pub(crate) fn wad_to_f64(value: impl Into<ParseUnits>) -> Result<f64, EngineError> {
    let text = format_units(value, DECIMALS).map_err(|e| EngineError::Conversion(e.to_string()))?;
    text.parse()
        .map_err(|_| EngineError::Conversion(format!("{text} is not a decimal")))
}

/// A decimal literal in 18-decimal fixed point, for test constants (panics on a malformed literal).
#[cfg(test)]
pub(crate) fn wad(decimal: &str) -> U256 {
    alloy::primitives::utils::parse_units(decimal, DECIMALS)
        .expect("a valid decimal literal")
        .get_absolute()
}

/// Rounding direction of [`to_wei`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Round {
    Down,
    Up,
}

/// ⌊tokens · 10¹⁸⌋ or ⌈tokens · 10¹⁸⌉, exactly, for 0 ≤ tokens ≤ 3e20 (the C++ engine's range; amounts stay below
/// 2¹²⁸). A double is m · 2^e with an integer m, so m · 10¹⁸ is shifted by e in 256-bit integers and the rounding is
/// applied to the exact value. bots/inventory-cpp rounds the product in long double first; this is exact.
pub(crate) fn to_wei(tokens: f64, round: Round) -> Result<U256, EngineError> {
    if !(0.0..=3.0e20).contains(&tokens) {
        return Err(EngineError::Conversion(format!(
            "{tokens} tokens is out of range"
        )));
    }
    if tokens == 0.0 {
        return Ok(U256::ZERO);
    }
    let bits = tokens.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased - 1075)
    };
    let scaled = U256::from(mantissa) * U256::from(10u64).pow(U256::from(18u8));
    if exponent >= 0 {
        return Ok(scaled << exponent as usize);
    }
    let shift = exponent.unsigned_abs() as usize;
    if shift >= 256 {
        return Ok(if round == Round::Up {
            U256::from(1u8)
        } else {
            U256::ZERO
        });
    }
    let floor = scaled >> shift;
    let exact = (floor << shift) == scaled;
    Ok(if round == Round::Up && !exact {
        floor + U256::from(1u8)
    } else {
        floor
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------------------------------------------------

/// One oracle reading, in the shape `IZKOracle.latestVerifiedPrice()` returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OracleReading {
    /// token0 priced in token1, 18-decimal fixed point.
    pub price: U256,
    /// Unix time of the attested observation, seconds.
    pub updated_at: u64,
}

/// The price of a reading that is fresh at `now` (Unix seconds): not older than MAX_ORACLE_AGE_SEC and not stamped
/// more than MAX_CLOCK_SKEW_SEC in the future. The contract reverts a rebalance on an oracle older than 60 s.
pub(crate) fn fresh_price(reading: OracleReading, now: u64) -> Result<U256, EngineError> {
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
/// which makes every reading look stamped in the future: no action, rather than an action on a bad clock.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lab_golden;
    use alloy::primitives::utils::parse_units;

    /// Distance between two doubles in units in the last place (0 when equal, ±0 included).
    pub(crate) fn ulps(a: f64, b: f64) -> u64 {
        if a == b {
            return 0;
        }
        let key = |x: f64| {
            let bits = x.to_bits() as i64;
            i128::from(if bits < 0 { i64::MIN - bits } else { bits })
        };
        (key(a) - key(b)).unsigned_abs() as u64
    }

    /// The lab's `quotes`, kept here as the reference the golden vectors were made with: r and the bid and ask, with
    /// the lab's two guards (a quote never crosses the mid; with tick > 0 quotes are rounded outward, with a 1e-9-tick
    /// tolerance). The engine itself hands r ∓ δ/2 to the planner ([`AsParams::quote`]).
    fn lab_quotes(
        mid: f64,
        q: f64,
        gamma: f64,
        sigma2: f64,
        tau: f64,
        kappa: f64,
        tick: f64,
    ) -> (f64, f64, f64) {
        let r = reservation_price(mid, q, gamma, sigma2, tau);
        let half = 0.5 * optimal_spread(gamma, sigma2, tau, kappa);
        let mut bid = (r - half).min(mid);
        let mut ask = (r + half).max(mid);
        if tick > 0.0 {
            bid = (bid / tick + 1e-9).floor() * tick;
            ask = (ask / tick - 1e-9).ceil() * tick;
        }
        (r, bid, ask)
    }

    #[test]
    fn the_closed_forms_match_the_python_lab() {
        let mut worst = 0;
        for row in lab_golden::QUOTES {
            let [mid, q, gamma, sigma2, tau, kappa, tick, r, bid, ask, spread] = *row;
            let (got_r, got_bid, got_ask) = lab_quotes(mid, q, gamma, sigma2, tau, kappa, tick);
            let got_spread = optimal_spread(gamma, sigma2, tau, kappa);
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
    fn the_quote_is_r_plus_minus_half_the_spread() {
        let p = AsParams::MOCK;
        let (r, half) = p.quote(2.0, 100.0).unwrap();
        assert_eq!(
            r,
            reservation_price(2.0, 100.0, p.gamma, p.sigma2_per_sec, p.horizon_sec)
        );
        assert_eq!(
            half,
            0.5 * optimal_spread(p.gamma, p.sigma2_per_sec, p.horizon_sec, p.kappa)
        );
        // 100 token0 · 0.1 · 2.5e-7 /s · 60 s = 1.5e-4 below the mid: a long book wants to sell.
        assert!((2.0 - r - 1.5e-4).abs() < 1e-15);
        let huge = AsParams {
            gamma: 1e300,
            horizon_sec: 1e300,
            sigma2_per_sec: 1e300,
            kappa: 1.0,
        };
        assert_eq!(huge.quote(2.0, 1.0), Err(EngineError::NonFinite));
    }

    #[test]
    fn the_spread_keeps_full_precision_when_gamma_over_kappa_is_tiny() {
        let kappa = 1e4_f64;
        // γ/κ = 1e-17 is below 2^-53: 1 + γ/κ rounds to 1 and the naive logarithm loses the whole spread.
        let gamma = 1e-13_f64;
        assert_eq!((1.0 + gamma / kappa).ln(), 0.0);
        let spread = optimal_spread(gamma, 0.0, 60.0, kappa);
        assert!((spread * kappa / 2.0 - 1.0).abs() < 1e-15, "{spread:e}");
        // Towards the risk-neutral limit 2/κ: (κ/γ)·ln(1 + x) = 1 − x/2 + O(x²) with x = γ/κ.
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
    }

    #[test]
    fn to_wei_rounds_the_exact_value() {
        let w = |s: &str| U256::from_str_radix(s, 10).unwrap();
        assert_eq!(to_wei(1.0, Round::Down).unwrap(), w("1000000000000000000"));
        assert_eq!(to_wei(2.5, Round::Up).unwrap(), w("2500000000000000000"));
        // 0.1 is 0.1000000000000000055511151231257827… in binary: ×10¹⁸ lies between …005 and …006.
        assert_eq!(to_wei(0.1, Round::Down).unwrap(), w("100000000000000005"));
        assert_eq!(to_wei(0.1, Round::Up).unwrap(), w("100000000000000006"));
        // The double nearest 1e-18 is 1.0000000000000000715e-18: one wei and a bit.
        assert_eq!(to_wei(1e-18, Round::Down).unwrap(), U256::from(1u8));
        assert_eq!(to_wei(1e-18, Round::Up).unwrap(), U256::from(2u8));
        // Integers stay exact far beyond 2^53 wei.
        assert_eq!(
            to_wei(123_456_789.0, Round::Up).unwrap(),
            w("123456789000000000000000000")
        );
        assert_eq!(to_wei(0.0, Round::Up).unwrap(), U256::ZERO);
        assert_eq!(
            to_wei(f64::MIN_POSITIVE / 4.0, Round::Up).unwrap(),
            U256::from(1u8)
        );
        for bad in [-1e-30, f64::NAN, f64::INFINITY, 3.0e21] {
            assert!(to_wei(bad, Round::Down).is_err(), "{bad}");
        }
        // Down and up bracket the value and differ by at most one wei.
        for t in [1.9998, 0.3, 1234.5678, 7.0e-7, 2.0e20] {
            let (lo, hi) = (
                to_wei(t, Round::Down).unwrap(),
                to_wei(t, Round::Up).unwrap(),
            );
            assert!(hi >= lo && hi - lo <= U256::from(1u8), "{t}");
        }
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
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(m.validate().is_ok());
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

    #[test]
    fn only_fresh_oracle_readings_are_used() {
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
}
