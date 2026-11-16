//! The canonical response body that `circuits/zkdeco_oracle` proves statements about:
//!
//! ```text
//! {"symbol":"<SYM>","price":<I>.<F>,"ts":<10 digits>}      zero-padded to 96 bytes
//! ```
//!
//! The circuit pins every offset (rule B6), so a body is either exactly this object or unprovable. `SYM` is 6 bytes;
//! `I` is 1 to 12 digits, followed by a mandatory `.`; `F` is 1 to 10 digits; the timestamp is exactly 10 digits. The
//! endpoint formats prices with [`CanonicalBody::new`]. The notary and the relayer accept only what [`CanonicalBody::parse`]
//! accepts, which is what the circuit can prove, so a notarized body is always provable.

use std::fmt;

use alloy::primitives::{B256, U256};
use serde_json::{Value, json};

/// Bytes the circuit commits to (the body, zero-padded).
pub const BODY_LEN: usize = 96;
/// Bytes of the instrument's symbol.
pub const SYMBOL_LEN: usize = 6;
pub const MAX_INT_DIGITS: usize = 12;
pub const MAX_FRAC_DIGITS: usize = 10;

const SYMBOL_KEY: &[u8] = b"{\"symbol\":\"";
const PRICE_KEY: &[u8] = b"\",\"price\":";
const TS_KEY: &[u8] = b",\"ts\":";
/// Offsets the circuit pins: `"symbol":"` right after `{`, and `"price":` right after the symbol's `",`.
const OFF_SYM_KEY: usize = 1;
const OFF_PRICE_KEY: usize = 1 + 10 + SYMBOL_LEN + 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyError {
    /// The symbol is not 6 characters from A–Z and 0–9.
    Symbol,
    /// The bytes are not the canonical object.
    Layout(&'static str),
    /// The price has no digits where they are required, too many digits, or is zero.
    Price(&'static str),
    /// The timestamp is not 10 digits.
    Timestamp,
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbol => f.write_str("the symbol must be 6 characters from A-Z and 0-9"),
            Self::Layout(m) => write!(f, "not the canonical body: {m}"),
            Self::Price(m) => write!(f, "price: {m}"),
            Self::Timestamp => f.write_str("the timestamp must be exactly 10 digits"),
        }
    }
}

impl std::error::Error for BodyError {}

/// A non-negative decimal with at most 10 fractional digits, held exactly as a count of 1e-10 units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Price(u128);

impl Price {
    /// Units per whole token.
    pub const ONE: u128 = 10_000_000_000;

    pub const fn from_units(units: u128) -> Self {
        Self(units)
    }

    pub const fn units(self) -> u128 {
        self.0
    }

    /// An upstream quote such as `"3456.78"` or `"3456.78000"`: digits, then optionally a `.` and digits. Fractional
    /// digits beyond the tenth are truncated (toward zero). No sign, exponent or spaces.
    pub fn parse_quote(text: &str) -> Result<Self, BodyError> {
        let (int, frac) = text.split_once('.').unwrap_or((text, ""));
        if int.is_empty() || int.len() > MAX_INT_DIGITS || !all_digits(int.as_bytes()) {
            return Err(BodyError::Price("1 to 12 integer digits expected"));
        }
        if !all_digits(frac.as_bytes()) || (text.contains('.') && frac.is_empty()) {
            return Err(BodyError::Price("digits expected after the point"));
        }
        let kept = &frac[..frac.len().min(MAX_FRAC_DIGITS)];
        let int_units = digits_value(int.as_bytes()) * Self::ONE;
        let frac_units = digits_value(kept.as_bytes()) * 10u128.pow((MAX_FRAC_DIGITS - kept.len()) as u32);
        Ok(Self(int_units + frac_units))
    }

    /// The canonical digits: the integer part without leading zeros (a single `0` below 1), the fractional part
    /// without trailing zeros but at least one digit (`3456.0`).
    pub fn canonical_digits(self) -> (String, String) {
        let int = (self.0 / Self::ONE).to_string();
        let frac = format!("{:010}", self.0 % Self::ONE);
        let frac = frac.trim_end_matches('0');
        (int, if frac.is_empty() { "0".to_string() } else { frac.to_string() })
    }

    /// The 1e18-scaled value the circuit attests and `ZkPriceFeed` stores.
    pub fn wad(self) -> U256 {
        U256::from(self.0) * U256::from(100_000_000u64)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (int, frac) = self.canonical_digits();
        write!(f, "{int}.{frac}")
    }
}

fn all_digits(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_digit)
}

/// The value of at most 12 (integer) or 10 (fractional) ASCII digits: it fits in u128 with room to spare.
fn digits_value(bytes: &[u8]) -> u128 {
    bytes.iter().fold(0u128, |acc, &b| acc * 10 + u128::from(b - b'0'))
}

/// The canonical body. Holds the digits exactly as written, so that [`CanonicalBody::text`] reproduces a parsed body
/// byte for byte (the circuit accepts leading or trailing zeros; the endpoint never writes them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalBody {
    symbol: [u8; SYMBOL_LEN],
    int_digits: String,
    frac_digits: String,
    ts: u64,
}

impl CanonicalBody {
    /// The body the endpoint serves: canonical digits, a non-zero price, a 10-digit timestamp.
    pub fn new(symbol: &str, price: Price, ts: u64) -> Result<Self, BodyError> {
        let symbol = parse_symbol(symbol.as_bytes())?;
        if price.units() == 0 {
            return Err(BodyError::Price("zero"));
        }
        if !(1_000_000_000..=9_999_999_999).contains(&ts) {
            return Err(BodyError::Timestamp);
        }
        let (int_digits, frac_digits) = price.canonical_digits();
        if int_digits.len() > MAX_INT_DIGITS {
            return Err(BodyError::Price("more than 12 integer digits"));
        }
        Ok(Self { symbol, int_digits, frac_digits, ts })
    }

    /// Parses a body under the circuit's exact grammar (rules B3 to B6). Trailing zero bytes (the padding) are
    /// allowed; anything else after the object is not.
    pub fn parse(bytes: &[u8]) -> Result<Self, BodyError> {
        let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        let text = &bytes[..end];
        if text.len() > BODY_LEN {
            return Err(BodyError::Layout("longer than 96 bytes"));
        }
        let rest = text.strip_prefix(SYMBOL_KEY).ok_or(BodyError::Layout("must start with {\"symbol\":\""))?;
        if rest.len() < SYMBOL_LEN {
            return Err(BodyError::Symbol);
        }
        let symbol = parse_symbol(&rest[..SYMBOL_LEN])?;
        let rest = rest[SYMBOL_LEN..]
            .strip_prefix(PRICE_KEY)
            .ok_or(BodyError::Layout("the symbol must be followed by \",\"price\":"))?;

        let int_len = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=MAX_INT_DIGITS).contains(&int_len) {
            return Err(BodyError::Price("1 to 12 integer digits expected"));
        }
        let int_digits = &rest[..int_len];
        let rest = rest[int_len..].strip_prefix(b".").ok_or(BodyError::Price("a decimal point is required"))?;
        let frac_len = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=MAX_FRAC_DIGITS).contains(&frac_len) {
            return Err(BodyError::Price("1 to 10 fractional digits expected"));
        }
        let frac_digits = &rest[..frac_len];
        let rest =
            rest[frac_len..].strip_prefix(TS_KEY).ok_or(BodyError::Layout("the price must be followed by ,\"ts\":"))?;
        if rest.len() != 11 || !all_digits(&rest[..10]) || rest[10] != b'}' {
            return Err(BodyError::Timestamp);
        }
        let ts = digits_value(&rest[..10]) as u64;
        let body = Self {
            symbol,
            int_digits: String::from_utf8_lossy(int_digits).into_owned(),
            frac_digits: String::from_utf8_lossy(frac_digits).into_owned(),
            ts,
        };
        if body.price_wad().is_zero() {
            return Err(BodyError::Price("zero"));
        }
        Ok(body)
    }

    /// The body's text, as served and notarized.
    pub fn text(&self) -> String {
        format!(
            "{{\"symbol\":\"{}\",\"price\":{}.{},\"ts\":{:010}}}",
            String::from_utf8_lossy(&self.symbol),
            self.int_digits,
            self.frac_digits,
            self.ts
        )
    }

    /// The 96 bytes the commitment covers: the text, then zeros.
    pub fn padded(&self) -> [u8; BODY_LEN] {
        let mut out = [0u8; BODY_LEN];
        let text = self.text();
        out[..text.len()].copy_from_slice(text.as_bytes());
        out
    }

    pub fn symbol(&self) -> [u8; SYMBOL_LEN] {
        self.symbol
    }

    pub fn symbol_str(&self) -> &str {
        std::str::from_utf8(&self.symbol).unwrap_or("?")
    }

    /// The attested observation time (Unix seconds).
    pub fn observed_at(&self) -> u64 {
        self.ts
    }

    /// The attested price, 1e18-scaled: I·1e18 + F·10^(18 − |F|).
    pub fn price_wad(&self) -> U256 {
        let int = U256::from(digits_value(self.int_digits.as_bytes()));
        let frac = U256::from(digits_value(self.frac_digits.as_bytes()));
        int * U256::from(10u64).pow(U256::from(18u8))
            + frac * U256::from(10u64).pow(U256::from(18 - self.frac_digits.len()))
    }

    /// The circuit's private inputs offSymKey, offPriceKey and offTsKey.
    pub fn offsets(&self) -> (usize, usize, usize) {
        let off_ts_key = OFF_PRICE_KEY + 8 + self.int_digits.len() + self.frac_digits.len() + 2;
        (OFF_SYM_KEY, OFF_PRICE_KEY, off_ts_key)
    }

    /// The full witness of `circuits/zkdeco_oracle` for this body (the relayer's prover input). It contains the query
    /// salt: it is passed to the prover on stdin and never written to disk or logged.
    pub fn witness(&self, provider_id: B256, query_commitment: B256, body_commitment: B256, salt: U256) -> Value {
        let (off_sym, off_price, off_ts) = self.offsets();
        let field = |b: B256| U256::from_be_bytes(b.0).to_string();
        json!({
            "providerId": field(provider_id),
            "queryCommitment": field(query_commitment),
            "price": self.price_wad().to_string(),
            "observedAt": self.ts.to_string(),
            "bodyCommitment": field(body_commitment),
            "body": self.padded().iter().map(|b| b.to_string()).collect::<Vec<_>>(),
            "symbol": self.symbol.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
            "salt": salt.to_string(),
            "offSymKey": off_sym.to_string(),
            "offPriceKey": off_price.to_string(),
            "offTsKey": off_ts.to_string(),
            "intLen": self.int_digits.len().to_string(),
            "fracLen": self.frac_digits.len().to_string(),
        })
    }
}

/// An instrument symbol: exactly 6 bytes, uppercase ASCII letters or digits (rule B3).
pub fn parse_symbol(bytes: &[u8]) -> Result<[u8; SYMBOL_LEN], BodyError> {
    let symbol: [u8; SYMBOL_LEN] = bytes.try_into().map_err(|_| BodyError::Symbol)?;
    if symbol.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
        Ok(symbol)
    } else {
        Err(BodyError::Symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_767_225_600;

    #[test]
    fn quotes_parse_exactly_and_truncate_beyond_ten_digits() {
        assert_eq!(Price::parse_quote("3456.78").unwrap().units(), 34_567_800_000_000);
        assert_eq!(Price::parse_quote("3456.78000").unwrap(), Price::parse_quote("3456.78").unwrap());
        assert_eq!(Price::parse_quote("3456").unwrap().units(), 3456 * Price::ONE);
        assert_eq!(Price::parse_quote("0.123456789012").unwrap().units(), 1_234_567_890, "two digits truncated");
        for bad in ["", ".5", "3456.", "-1.0", "1e3", " 1.0", "1.0 ", "1,0", "1234567890123.0", "0x10"] {
            assert!(Price::parse_quote(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn canonical_digits_have_no_leading_or_trailing_zeros_but_always_a_fraction() {
        let p = |s| Price::parse_quote(s).unwrap();
        assert_eq!(p("3456.78").to_string(), "3456.78");
        assert_eq!(p("3456.7800").to_string(), "3456.78");
        assert_eq!(p("3456").to_string(), "3456.0");
        assert_eq!(p("0003456.5").to_string(), "3456.5");
        assert_eq!(p("0.0000000001").to_string(), "0.0000000001");
        assert_eq!(p("3456.789012").wad(), U256::from(3_456_789_012u64) * U256::from(10u64).pow(U256::from(12u8)));
    }

    /// The bodies of circuits/scripts/gen-feed-fixtures.mjs: same text, same offsets and lengths as its witness.
    #[test]
    fn bodies_match_the_fixture_generator() {
        for (price, ts, int_len, frac_len) in [
            ("3456.789012", T, 4, 6),
            ("3457.1205", T + 15, 4, 4),
            ("3455.9", T + 30, 4, 1),
            ("3458.0000000001", T + 45, 4, 10),
        ] {
            let body = CanonicalBody::new("ETHUSD", Price::parse_quote(price).unwrap(), ts).unwrap();
            let text = format!("{{\"symbol\":\"ETHUSD\",\"price\":{price},\"ts\":{ts}}}");
            assert_eq!(body.text(), text);
            assert_eq!(
                body.offsets(),
                (text.find("\"symbol\":\"").unwrap(), text.find("\"price\":").unwrap(), text.find("\"ts\":").unwrap())
            );
            let w = body.witness(B256::ZERO, B256::ZERO, B256::ZERO, U256::ZERO);
            assert_eq!(
                (w["intLen"].as_str().unwrap(), w["fracLen"].as_str().unwrap()),
                (&*int_len.to_string(), &*frac_len.to_string())
            );
            assert_eq!(CanonicalBody::parse(&body.padded()).unwrap(), body);
        }
        let b = CanonicalBody::new("ETHUSD", Price::parse_quote("3458.0000000001").unwrap(), T + 45).unwrap();
        assert_eq!(b.price_wad().to_string(), "3458000000000100000000");
    }

    #[test]
    fn parse_accepts_exactly_the_circuits_grammar() {
        let ok = |s: &str| CanonicalBody::parse(s.as_bytes());
        assert!(ok(r#"{"symbol":"ETHUSD","price":3456.78,"ts":1767225600}"#).is_ok());
        assert!(
            ok(r#"{"symbol":"ETHUSD","price":999999999999.9999999999,"ts":9999999999}"#).is_ok(),
            "the widest body"
        );
        let text = r#"{"symbol":"ETHUSD","price":0003456.7800,"ts":1767225600}"#;
        assert_eq!(ok(text).unwrap().text(), text, "non-canonical digits are kept exactly");
        for bad in [
            r#"{"symbol":"ETHUSD","price":3456,"ts":1767225600}"#, // no decimal point
            r#"{"symbol":"ETHUSD","price":3456.,"ts":1767225600}"#, // no fractional digit
            r#"{"symbol":"ETHUSD","price":.5,"ts":1767225600}"#,   // no integer digit
            r#"{"symbol":"ETHUSD","price":1234567890123.0,"ts":1767225600}"#, // 13 integer digits
            r#"{"symbol":"ETHUSD","price":1.12345678901,"ts":1767225600}"#, // 11 fractional digits
            r#"{"symbol":"ETHUSD","price":0.0,"ts":1767225600}"#,  // zero
            r#"{"symbol":"ETHUSD","price":3456.78,"ts":176722560}"#, // 9-digit timestamp
            r#"{"symbol":"ETHUSD","price":3456.78,"ts":17672256000}"#, // 11-digit timestamp
            r#"{"symbol":"ETHUSDT","price":3456.78,"ts":1767225600}"#, // 7-character symbol
            r#"{"symbol":"ethusd","price":3456.78,"ts":1767225600}"#, // lower case
            r#"{"symbol":"ETHUSD", "price":3456.78,"ts":1767225600}"#, // whitespace
            r#"{"price":3456.78,"symbol":"ETHUSD","ts":1767225600}"#, // key order
            r#"{"symbol":"ETHUSD","price":3456.78,"ts":1767225600,"x":1}"#, // a second field
            r#"{"symbol":"ETHUSD","price":3456.78,"ts":1767225600}}"#, // trailing bytes
            r#"{"symbol":"ETHUSD","price":-3456.78,"ts":1767225600}"#, // sign
            r#"{"symbol":"ETHUSD","price":"3456.78","ts":1767225600}"#, // quoted price
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
        let mut long = br#"{"symbol":"ETHUSD","price":3456.78,"ts":1767225600}"#.to_vec();
        long.resize(BODY_LEN + 1, b' ');
        assert!(CanonicalBody::parse(&long).is_err());
    }

    #[test]
    fn new_refuses_what_the_circuit_cannot_prove() {
        let p = Price::parse_quote("3456.78").unwrap();
        assert_eq!(CanonicalBody::new("ETH", p, T), Err(BodyError::Symbol));
        assert_eq!(CanonicalBody::new("ETH\"SD", p, T), Err(BodyError::Symbol));
        assert_eq!(CanonicalBody::new("ETHUSD", Price::from_units(0), T), Err(BodyError::Price("zero")));
        assert_eq!(CanonicalBody::new("ETHUSD", p, 999_999_999), Err(BodyError::Timestamp));
        assert!(CanonicalBody::new("ETHUSD", Price::from_units(1_000_000_000_000 * Price::ONE), T).is_err());
    }
}
