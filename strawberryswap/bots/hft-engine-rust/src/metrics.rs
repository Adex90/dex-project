//! Structured metrics and alerts: what the engine did, how fast, and what needs a human.
//!
//! Latencies go into fixed-bucket histograms (read, simulate, sign, broadcast, the whole tick, and broadcast to mined);
//! the engine prints a snapshot of them, its counters and its gauges every `--metrics-every` ticks. An alert is raised
//! once when its condition has held for its threshold of consecutive ticks, and resolved once when it clears, so a
//! condition that lasts an hour is two lines, not 36 000.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

/// Upper bounds of the latency buckets, microseconds. Above the last is the overflow bucket.
const BOUNDS_US: [u64; 12] = [
    100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 80_000, 100_000, 1_000_000,
];

/// A latency histogram.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Latency {
    counts: [u64; BOUNDS_US.len() + 1],
    n: u64,
    sum_us: u64,
    max_us: u64,
}

impl Latency {
    pub fn record(&mut self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        let bucket = BOUNDS_US
            .iter()
            .position(|&bound| us <= bound)
            .unwrap_or(BOUNDS_US.len());
        self.counts[bucket] += 1;
        self.n += 1;
        self.sum_us = self.sum_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
    }

    #[cfg(test)]
    pub fn count(&self) -> u64 {
        self.n
    }

    /// The bucket bound at or under which a fraction `q` of the observations fall (the maximum for the overflow
    /// bucket); None without observations.
    pub fn quantile_us(&self, q: f64) -> Option<u64> {
        if self.n == 0 {
            return None;
        }
        let rank = ((q * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0;
        for (i, &count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return Some(
                    BOUNDS_US
                        .get(i)
                        .map_or(self.max_us, |&b| b.min(self.max_us)),
                );
            }
        }
        Some(self.max_us)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "count": self.n,
            "mean_us": self.sum_us.checked_div(self.n),
            "p50_us": self.quantile_us(0.5),
            "p99_us": self.quantile_us(0.99),
            "max_us": (self.n > 0).then_some(self.max_us),
        })
    }
}

/// The engine's latency histograms.
#[derive(Clone, Debug, Default)]
pub(crate) struct Latencies {
    pub read: Latency,
    pub simulate: Latency,
    pub sign: Latency,
    pub broadcast: Latency,
    pub tick: Latency,
    /// From the broadcast to the receipt.
    pub confirm: Latency,
}

impl Latencies {
    pub fn to_json(&self) -> Value {
        json!({
            "read": self.read.to_json(),
            "simulate": self.simulate.to_json(),
            "sign": self.sign.to_json(),
            "broadcast": self.broadcast.to_json(),
            "tick": self.tick.to_json(),
            "confirm": self.confirm.to_json(),
        })
    }
}

/// The conditions that need a human.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Alert {
    /// The chain reads keep failing or missing their budget.
    RpcDown,
    /// The oracle reading is too old (or stamped in the future) to trade on: the relayer is not pushing.
    OracleStale,
    /// The vault keeps failing to answer.
    VaultDown,
    /// The base fee is above the engine's max fee: nothing it sends would be mined.
    FeesAboveCap,
    /// A transaction has waited for its receipt too long.
    TxStuck,
    /// A transaction from the operator that the engine did not send is pending.
    ForeignTransaction,
    /// The operator holds less ether than the gas of a few rebalances.
    LowGasBalance,
}

impl Alert {
    pub fn name(self) -> &'static str {
        match self {
            Self::RpcDown => "rpc_down",
            Self::OracleStale => "oracle_stale",
            Self::VaultDown => "vault_down",
            Self::FeesAboveCap => "fees_above_cap",
            Self::TxStuck => "tx_stuck",
            Self::ForeignTransaction => "foreign_transaction",
            Self::LowGasBalance => "low_gas_balance",
        }
    }

    /// Consecutive observations before the alert is raised: one second of ticks for what a single slow tick can
    /// cause, at once for the rest.
    fn threshold(self) -> u32 {
        match self {
            Self::RpcDown | Self::OracleStale | Self::VaultDown | Self::FeesAboveCap => 10,
            Self::TxStuck | Self::ForeignTransaction | Self::LowGasBalance => 1,
        }
    }
}

/// An alert that was raised or resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AlertChange {
    Raised { alert: Alert, detail: String },
    Resolved { alert: Alert },
}

/// The alerts' state.
#[derive(Clone, Debug, Default)]
pub(crate) struct Alerts {
    streak: BTreeMap<Alert, u32>,
    active: BTreeMap<Alert, String>,
}

impl Alerts {
    /// One observation of `alert`'s condition: Some(detail) while it holds, None once it does not.
    pub fn observe(&mut self, alert: Alert, holds: Option<String>) -> Option<AlertChange> {
        match holds {
            Some(detail) => {
                let streak = self.streak.entry(alert).or_default();
                *streak = streak.saturating_add(1);
                if *streak >= alert.threshold() && !self.active.contains_key(&alert) {
                    self.active.insert(alert, detail.clone());
                    return Some(AlertChange::Raised { alert, detail });
                }
                None
            }
            None => {
                self.streak.remove(&alert);
                self.active
                    .remove(&alert)
                    .map(|_| AlertChange::Resolved { alert })
            }
        }
    }

    pub fn active(&self) -> Vec<&'static str> {
        self.active.keys().map(|a| a.name()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latencies_land_in_their_buckets() {
        let mut l = Latency::default();
        assert_eq!(l.quantile_us(0.5), None);
        for us in [50, 90, 300, 300, 300, 4_000, 4_000, 4_000, 4_000, 120_000] {
            l.record(Duration::from_micros(us));
        }
        assert_eq!(l.count(), 10);
        assert_eq!(l.quantile_us(0.1), Some(100));
        assert_eq!(l.quantile_us(0.5), Some(500));
        assert_eq!(l.quantile_us(0.9), Some(5_000));
        assert_eq!(
            l.quantile_us(0.99),
            Some(120_000),
            "the overflow-free last bucket reports the max"
        );
        let json = l.to_json();
        assert_eq!(json["count"], 10);
        assert_eq!(json["max_us"], 120_000);
        assert_eq!(json["mean_us"], (50 + 90 + 900 + 16_000 + 120_000) / 10);
    }

    #[test]
    fn an_alert_is_raised_once_after_its_threshold_and_resolved_once() {
        let mut alerts = Alerts::default();
        for _ in 0..9 {
            assert_eq!(alerts.observe(Alert::RpcDown, Some("timeout".into())), None);
        }
        assert_eq!(
            alerts.observe(Alert::RpcDown, Some("timeout".into())),
            Some(AlertChange::Raised {
                alert: Alert::RpcDown,
                detail: "timeout".into()
            })
        );
        assert_eq!(
            alerts.observe(Alert::RpcDown, Some("timeout".into())),
            None,
            "once"
        );
        assert_eq!(alerts.active(), ["rpc_down"]);
        assert_eq!(
            alerts.observe(Alert::RpcDown, None),
            Some(AlertChange::Resolved {
                alert: Alert::RpcDown
            })
        );
        assert_eq!(alerts.observe(Alert::RpcDown, None), None, "once");
        // A streak broken before the threshold raises nothing.
        for _ in 0..9 {
            assert_eq!(alerts.observe(Alert::OracleStale, Some("old".into())), None);
        }
        assert_eq!(alerts.observe(Alert::OracleStale, None), None);
        assert_eq!(alerts.observe(Alert::OracleStale, Some("old".into())), None);
        // Immediate alerts.
        assert!(matches!(
            alerts.observe(Alert::TxStuck, Some("31 s".into())),
            Some(AlertChange::Raised { .. })
        ));
    }
}
