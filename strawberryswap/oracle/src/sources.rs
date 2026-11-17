//! The endpoint's upstream prices: public ETH-USD tickers of three exchanges, and the rule that turns their quotes into
//! one price, or into no price at all.
//!
//! The rule fails closed. It takes the median of the fresh quotes, drops any quote farther than `max_spread_bps` from
//! that median, and requires `min_sources` quotes to remain. With fewer, the endpoint serves no price, so the notary
//! attests nothing and the feed goes stale, which DarkPool treats as "no trading". A wrong price would be worse.

use std::fmt;

use serde_json::Value;

use crate::body::Price;

/// An exchange with a public ETH-USD ticker (no API key).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Exchange {
    Coinbase,
    Kraken,
    Bitstamp,
}

impl Exchange {
    pub const ALL: [Self; 3] = [Self::Coinbase, Self::Kraken, Self::Bitstamp];

    pub fn name(self) -> &'static str {
        match self {
            Self::Coinbase => "coinbase",
            Self::Kraken => "kraken",
            Self::Bitstamp => "bitstamp",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == name)
    }

    /// The public API's base URL.
    pub fn default_base(self) -> &'static str {
        match self {
            Self::Coinbase => "https://api.exchange.coinbase.com",
            Self::Kraken => "https://api.kraken.com",
            Self::Bitstamp => "https://www.bitstamp.net",
        }
    }

    /// The ETH-USD ticker's path under the base URL.
    pub fn ticker_path(self) -> &'static str {
        match self {
            Self::Coinbase => "/products/ETH-USD/ticker",
            Self::Kraken => "/0/public/Ticker?pair=ETHUSD",
            Self::Bitstamp => "/api/v2/ticker/ethusd/",
        }
    }

    /// The last trade price in the ticker's JSON. Prices are strings in all three APIs, so they are parsed exactly.
    pub fn parse_ticker(self, ticker: &Value) -> Result<Price, SourceError> {
        let text = match self {
            // {"price":"3456.78","time":"…",…}
            Self::Coinbase => ticker["price"].as_str(),
            // {"error":[],"result":{"XETHZUSD":{"c":["3456.78000","0.010"],…}}}
            Self::Kraken => {
                if ticker["error"].as_array().is_some_and(|e| !e.is_empty()) {
                    return Err(SourceError::Upstream(format!("kraken: {}", ticker["error"])));
                }
                ticker["result"].as_object().and_then(|r| r.values().next()).and_then(|pair| pair["c"][0].as_str())
            }
            // {"last":"3456.78","timestamp":"…",…}
            Self::Bitstamp => ticker["last"].as_str(),
        };
        let text = text.ok_or_else(|| SourceError::Format(format!("{}: no price in the ticker", self.name())))?;
        Price::parse_quote(text).map_err(|e| SourceError::Format(format!("{}: {text:?}: {e}", self.name())))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    Upstream(String),
    Format(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upstream(m) | Self::Format(m) => f.write_str(m),
        }
    }
}

/// One exchange's last price and when the endpoint received it (Unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    pub exchange: Exchange,
    pub price: Price,
    pub received_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    /// A quote older than this is ignored.
    pub max_age_secs: u64,
    /// A quote farther than this from the median is ignored (basis points).
    pub max_spread_bps: u128,
    /// Quotes that must remain for a price to be served.
    pub min_sources: usize,
}

impl Rules {
    /// The testnet's rule: two of three exchanges, within 1 % of their median, received in the last 10 s.
    pub const TESTNET: Self = Self { max_age_secs: 10, max_spread_bps: 100, min_sources: 2 };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoPrice {
    /// Fewer fresh quotes than the rule requires.
    TooFewFresh { fresh: usize, required: usize },
    /// The fresh quotes disagree: too few lie near their median.
    Disagreement { agreeing: usize, required: usize },
}

impl fmt::Display for NoPrice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewFresh { fresh, required } => write!(f, "{fresh} fresh quotes, {required} required"),
            Self::Disagreement { agreeing, required } => {
                write!(f, "the sources disagree: {agreeing} near the median, {required} required")
            }
        }
    }
}

/// The median of the units (the lower middle's and upper middle's mean, rounded down, for an even count).
fn median(prices: &mut [u128]) -> u128 {
    prices.sort_unstable();
    let n = prices.len();
    if n % 2 == 1 {
        prices[n / 2]
    } else {
        prices[n / 2 - 1] / 2 + prices[n / 2] / 2 + (prices[n / 2 - 1] % 2 + prices[n / 2] % 2) / 2
    }
}

/// The price to serve at `now`, or why there is none.
pub fn aggregate(quotes: &[Quote], now: u64, rules: &Rules) -> Result<Price, NoPrice> {
    let required = rules.min_sources.max(1);
    let mut fresh: Vec<u128> = quotes
        .iter()
        .filter(|q| q.received_at <= now && now - q.received_at <= rules.max_age_secs && q.price.units() > 0)
        .map(|q| q.price.units())
        .collect();
    if fresh.len() < required {
        return Err(NoPrice::TooFewFresh { fresh: fresh.len(), required });
    }
    let center = median(&mut fresh);
    let mut agreeing: Vec<u128> = fresh
        .into_iter()
        .filter(|&p| p.abs_diff(center) * 10_000 <= center.saturating_mul(rules.max_spread_bps))
        .collect();
    if agreeing.len() < required {
        return Err(NoPrice::Disagreement { agreeing: agreeing.len(), required });
    }
    Ok(Price::from_units(median(&mut agreeing)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn q(exchange: Exchange, price: &str, received_at: u64) -> Quote {
        Quote { exchange, price: Price::parse_quote(price).unwrap(), received_at }
    }

    #[test]
    fn each_exchanges_ticker_format_parses_exactly() {
        let p = |e: Exchange, v: Value| e.parse_ticker(&v).map(|p| p.to_string());
        assert_eq!(
            p(Exchange::Coinbase, json!({"price": "3456.78", "time": "2026-01-01T00:00:00Z"})).unwrap(),
            "3456.78"
        );
        assert_eq!(
            p(Exchange::Kraken, json!({"error": [], "result": {"XETHZUSD": {"c": ["3456.78000", "0.010"]}}})).unwrap(),
            "3456.78"
        );
        assert_eq!(p(Exchange::Bitstamp, json!({"last": "3456.78", "timestamp": "1767225600"})).unwrap(), "3456.78");
        assert!(p(Exchange::Kraken, json!({"error": ["EService:Unavailable"], "result": {}})).is_err());
        assert!(
            p(Exchange::Coinbase, json!({"price": 3456.78})).is_err(),
            "a JSON number is refused: no float parsing"
        );
        assert!(p(Exchange::Bitstamp, json!({"last": "-1"})).is_err());
        assert!(p(Exchange::Coinbase, json!({"message": "NotFound"})).is_err());
        assert_eq!(Exchange::from_name("kraken"), Some(Exchange::Kraken));
        assert_eq!(Exchange::from_name("binance"), None);
    }

    #[test]
    fn the_median_of_fresh_agreeing_quotes_is_served() {
        let now = 1_767_225_600;
        let r = Rules::TESTNET;
        let quotes = [
            q(Exchange::Coinbase, "3456.78", now),
            q(Exchange::Kraken, "3457.10", now - 2),
            q(Exchange::Bitstamp, "3455.00", now - 9),
        ];
        assert_eq!(aggregate(&quotes, now, &r).unwrap().to_string(), "3456.78");
        // Two quotes: their mean.
        assert_eq!(aggregate(&quotes[..2], now, &r).unwrap().to_string(), "3456.94");
        // The mean of two prices rounds down at the tenth decimal.
        let odd = [q(Exchange::Coinbase, "1.0000000001", now), q(Exchange::Kraken, "1.0000000002", now)];
        assert_eq!(aggregate(&odd, now, &r).unwrap().to_string(), "1.0000000001");
    }

    #[test]
    fn stale_or_future_quotes_do_not_count() {
        let now = 1_767_225_600;
        let r = Rules::TESTNET;
        let quotes = [
            q(Exchange::Coinbase, "3456.78", now),
            q(Exchange::Kraken, "3457.10", now - 11),
            q(Exchange::Bitstamp, "3455.00", now + 1),
        ];
        assert_eq!(aggregate(&quotes, now, &r), Err(NoPrice::TooFewFresh { fresh: 1, required: 2 }));
        assert_eq!(aggregate(&[], now, &r), Err(NoPrice::TooFewFresh { fresh: 0, required: 2 }));
    }

    /// One exchange printing a wrong price is outvoted; two disagreeing exchanges and no third mean no price.
    #[test]
    fn an_outlier_is_dropped_and_a_split_serves_nothing() {
        let now = 1_767_225_600;
        let r = Rules::TESTNET;
        let outlier = [
            q(Exchange::Coinbase, "3456.78", now),
            q(Exchange::Kraken, "3457.10", now),
            q(Exchange::Bitstamp, "34.56", now),
        ];
        assert_eq!(aggregate(&outlier, now, &r).unwrap().to_string(), "3456.94");
        let split = [q(Exchange::Coinbase, "3456.78", now), q(Exchange::Kraken, "3600.00", now)];
        assert_eq!(aggregate(&split, now, &r), Err(NoPrice::Disagreement { agreeing: 0, required: 2 }));
        // Exactly 1 % away still agrees; just past it does not.
        let edge = [
            q(Exchange::Coinbase, "1000.0", now),
            q(Exchange::Kraken, "1000.0", now),
            q(Exchange::Bitstamp, "1010.0", now),
        ];
        assert_eq!(aggregate(&edge, now, &r).unwrap().to_string(), "1000.0");
        let wide = Rules { min_sources: 3, ..r };
        assert_eq!(aggregate(&edge, now, &wide).unwrap().to_string(), "1000.0");
        let past = [
            q(Exchange::Coinbase, "1000.0", now),
            q(Exchange::Kraken, "1000.0", now),
            q(Exchange::Bitstamp, "1010.0000000001", now),
        ];
        assert_eq!(aggregate(&past, now, &wide), Err(NoPrice::Disagreement { agreeing: 2, required: 3 }));
    }
}
