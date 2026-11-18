//! The chain-state rules: which nonces the vault signs, and how much it signs per window.
//!
//! **Nonce sequencing.** A signature is worth something only for a nonce the chain has not used. The vault signs a
//! nonce only from the chain's next one (`eth_getTransactionCount` at `latest`) up to `max_pending` − 1 beyond it, and
//! never past the first nonce it has not signed. So no signature can be stockpiled for later: by the time a held-back
//! transaction could be sent, its nonce is spent. Signing the same nonce again (a replacement) is allowed, and counted.
//!
//! **Every spent nonce is the vault's.** Each time the chain's next nonce moves, the vault checks that each nonce it
//! moved past was spent by a transaction it signed (one of them has a receipt). A nonce spent by anything else means the
//! key signs somewhere else too: a shared key, a leaked one, or a second vault. The vault then stops signing until it is
//! restarted (fail closed), and says so in its audit log.
//!
//! **Per-window caps.** Over a sliding window, the signatures, the fee budget they commit (gas limit × max fee) and the
//! tokens they move are capped. A compromised client gets at most one window's worth before the caps stop it.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::Duration;

use alloy_primitives::{B256, U256};
use tokio::time::Instant;

use crate::policy::Size;

// ---------------------------------------------------------------------------------------------------------------------
// Nonce sequencing
// ---------------------------------------------------------------------------------------------------------------------

/// Why a nonce is not signed.
#[derive(Debug, PartialEq, Eq)]
pub enum NonceDenied {
    /// The chain has used it.
    Used { nonce: u64, next: u64 },
    /// Too far beyond the chain's next nonce: a signature for it could be held back and sent later.
    Ahead {
        nonce: u64,
        next: u64,
        max_pending: u64,
    },
    /// It would leave an unsigned nonce below it.
    Gap { nonce: u64, unsigned: u64 },
    /// Signing stopped: a nonce was spent by a transaction this vault did not sign.
    Halted(String),
}

impl NonceDenied {
    /// The error code the client sees.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Halted(_) => "halted",
            _ => "nonce",
        }
    }
}

impl fmt::Display for NonceDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Used { nonce, next } => {
                write!(
                    f,
                    "nonce {nonce} is spent: the chain's next nonce is {next}"
                )
            }
            Self::Ahead {
                nonce,
                next,
                max_pending,
            } => write!(
                f,
                "nonce {nonce} is ahead of the chain: it is at {next}, and the vault signs at most {max_pending} \
                 nonce(s) from there (no signature is held back for later)"
            ),
            Self::Gap { nonce, unsigned } => write!(
                f,
                "nonce {nonce} would skip {unsigned}, which the vault has not signed"
            ),
            Self::Halted(reason) => write!(
                f,
                "signing stopped: {reason}; the key is in use outside this vault (restart it once resolved)"
            ),
        }
    }
}

/// The vault's view of its own nonces.
#[derive(Debug)]
pub struct NonceGuard {
    max_pending: u64,
    /// Whether the chain is read at all (only the insecure test mode runs without one).
    chain: bool,
    /// The chain's next nonce, as last read; it only grows. Without a chain: the lowest nonce still signed.
    next: u64,
    /// Every nonce below this is accounted for: used before the vault started, or signed by it.
    accounted: u64,
    /// Every nonce below this was found spent by a transaction this vault signed, or before it started.
    verified: u64,
    /// The transactions signed for nonces in [verified, accounted).
    signed: BTreeMap<u64, Vec<B256>>,
    /// Without a chain, the first signature sets where the vault starts.
    started: bool,
    halted: Option<String>,
}

impl NonceGuard {
    /// At start: the chain's next nonce (`latest`) and its count with pending transactions. Pending transactions from
    /// before the start are accepted as they are: their nonces are accounted for.
    pub fn with_chain(latest: u64, pending: u64, max_pending: u64) -> Self {
        let accounted = pending.max(latest);
        Self {
            max_pending: max_pending.max(1),
            chain: true,
            next: latest,
            accounted,
            verified: accounted,
            signed: BTreeMap::new(),
            started: true,
            halted: None,
        }
    }

    /// The insecure test mode, with no chain: the vault follows its own signatures only (the next one, or the last
    /// one again).
    #[cfg_attr(not(feature = "insecure-test-key"), allow(dead_code))]
    pub fn without_chain() -> Self {
        Self {
            max_pending: 1,
            chain: false,
            next: 0,
            accounted: 0,
            verified: 0,
            signed: BTreeMap::new(),
            started: false,
            halted: None,
        }
    }

    pub const fn max_pending(&self) -> u64 {
        self.max_pending
    }

    pub const fn reads_chain(&self) -> bool {
        self.chain
    }

    /// The chain's next nonce as last read.
    pub const fn next(&self) -> u64 {
        self.next
    }

    pub fn halted(&self) -> Option<&str> {
        self.halted.as_deref()
    }

    /// Whether deciding about `nonce` needs a fresh read: it is beyond what the last read allows, and the chain may
    /// have moved since. (A nonce below the last read is spent whatever the chain says now: nonces only grow.)
    pub const fn needs_read(&self, nonce: u64) -> bool {
        self.chain && self.halted.is_none() && nonce >= self.next + self.max_pending
    }

    /// The transactions whose receipts the next read must look up: those signed for nonces not yet verified.
    pub fn unverified(&self) -> Vec<B256> {
        self.signed.values().flatten().copied().collect()
    }

    /// Checks a read (the chain's next nonce, and which of `unverified()` were mined) against what the vault signed:
    /// every nonce the chain moved past must have been spent by one of the vault's transactions.
    pub fn audit(&self, next: u64, mined: &[B256]) -> Result<(), String> {
        for nonce in self.verified..next {
            if nonce >= self.accounted {
                return Err(format!(
                    "nonce {nonce} was spent, and this vault never signed it"
                ));
            }
            let ours = self
                .signed
                .get(&nonce)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if !ours.iter().any(|tx| mined.contains(tx)) {
                return Err(format!(
                    "nonce {nonce} was spent by a transaction this vault did not sign (it signed {})",
                    ours.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Applies a read that passed `audit`. A read behind the last one (a lagging node) changes nothing.
    pub fn advance(&mut self, next: u64) {
        if next <= self.next {
            return;
        }
        self.next = next;
        self.verified = self.verified.max(next);
        self.signed = self.signed.split_off(&self.verified);
    }

    pub fn halt(&mut self, reason: String) {
        if self.halted.is_none() {
            self.halted = Some(reason);
        }
    }

    /// Whether `nonce` may be signed now, given the last read.
    pub fn check(&self, nonce: u64) -> Result<(), NonceDenied> {
        if let Some(reason) = &self.halted {
            return Err(NonceDenied::Halted(reason.clone()));
        }
        if !self.started {
            return Ok(());
        }
        if nonce < self.next {
            return Err(NonceDenied::Used {
                nonce,
                next: self.next,
            });
        }
        if self.chain && nonce >= self.next + self.max_pending {
            return Err(NonceDenied::Ahead {
                nonce,
                next: self.next,
                max_pending: self.max_pending,
            });
        }
        if nonce > self.accounted {
            return Err(NonceDenied::Gap {
                nonce,
                unsigned: self.accounted,
            });
        }
        Ok(())
    }

    /// A signature was given for `nonce`: the transaction `tx`.
    pub fn record(&mut self, nonce: u64, tx: B256) {
        if self.chain {
            if nonce >= self.verified {
                self.signed.entry(nonce).or_default().push(tx);
            }
        } else {
            self.started = true;
            self.next = self.next.max(nonce);
        }
        self.accounted = self.accounted.max(nonce + 1);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Per-window caps
// ---------------------------------------------------------------------------------------------------------------------

/// What a window may sign. `None` is no cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caps {
    pub signs: u64,
    /// The sum of gas limit × max fee per gas: what the signed transactions may cost at most, wei.
    pub fee_wei: U256,
    pub volume0: Option<U256>,
    pub volume1: Option<U256>,
    pub notes: Option<u64>,
}

/// What signatures commit to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub signs: u64,
    pub fee_wei: U256,
    pub volume0: U256,
    pub volume1: U256,
    pub notes: u64,
}

impl Usage {
    /// One signature: its fee budget and what it moves.
    pub fn of(gas_limit: u64, max_fee_per_gas: u128, size: Size) -> Self {
        Self {
            signs: 1,
            fee_wei: U256::from(gas_limit) * U256::from(max_fee_per_gas),
            volume0: size.volume0,
            volume1: size.volume1,
            notes: size.notes,
        }
    }

    fn plus(&self, other: &Self) -> Self {
        Self {
            signs: self.signs.saturating_add(other.signs),
            fee_wei: self.fee_wei.saturating_add(other.fee_wei),
            volume0: self.volume0.saturating_add(other.volume0),
            volume1: self.volume1.saturating_add(other.volume1),
            notes: self.notes.saturating_add(other.notes),
        }
    }

    fn minus(&self, other: &Self) -> Self {
        Self {
            signs: self.signs.saturating_sub(other.signs),
            fee_wei: self.fee_wei.saturating_sub(other.fee_wei),
            volume0: self.volume0.saturating_sub(other.volume0),
            volume1: self.volume1.saturating_sub(other.volume1),
            notes: self.notes.saturating_sub(other.notes),
        }
    }
}

/// Why a signature would pass a cap.
#[derive(Debug, PartialEq, Eq)]
pub struct WindowDenied {
    pub what: &'static str,
    pub used: U256,
    pub adding: U256,
    pub cap: U256,
    pub span_secs: u64,
}

impl fmt::Display for WindowDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} signed in the last {} s: {} more would pass the cap of {}",
            self.used, self.what, self.span_secs, self.adding, self.cap
        )
    }
}

/// The signatures of the last `span`, against the caps.
#[derive(Debug)]
pub struct Window {
    span: Duration,
    caps: Caps,
    entries: VecDeque<(Instant, Usage)>,
    total: Usage,
}

impl Window {
    pub fn new(span: Duration, caps: Caps) -> Self {
        Self {
            span,
            caps,
            entries: VecDeque::new(),
            total: Usage::default(),
        }
    }

    pub const fn span(&self) -> Duration {
        self.span
    }

    pub const fn caps(&self) -> &Caps {
        &self.caps
    }

    fn expire(&mut self, now: Instant) {
        while let Some((at, usage)) = self.entries.front() {
            if now.saturating_duration_since(*at) < self.span {
                break;
            }
            self.total = self.total.minus(usage);
            self.entries.pop_front();
        }
    }

    /// What the window holds now.
    pub fn used(&mut self, now: Instant) -> Usage {
        self.expire(now);
        self.total
    }

    /// Whether `add` fits under every cap.
    pub fn check(&mut self, now: Instant, add: &Usage) -> Result<(), WindowDenied> {
        self.expire(now);
        let after = self.total.plus(add);
        let span_secs = self.span.as_secs();
        let over = |what, used: U256, adding: U256, after: U256, cap: Option<U256>| match cap {
            Some(cap) if after > cap => Err(WindowDenied {
                what,
                used,
                adding,
                cap,
                span_secs,
            }),
            _ => Ok(()),
        };
        over(
            "signatures",
            U256::from(self.total.signs),
            U256::from(add.signs),
            U256::from(after.signs),
            Some(U256::from(self.caps.signs)),
        )?;
        over(
            "wei of fees",
            self.total.fee_wei,
            add.fee_wei,
            after.fee_wei,
            Some(self.caps.fee_wei),
        )?;
        over(
            "token0 (base units)",
            self.total.volume0,
            add.volume0,
            after.volume0,
            self.caps.volume0,
        )?;
        over(
            "token1 (base units)",
            self.total.volume1,
            add.volume1,
            after.volume1,
            self.caps.volume1,
        )?;
        over(
            "notes",
            U256::from(self.total.notes),
            U256::from(add.notes),
            U256::from(after.notes),
            self.caps.notes.map(U256::from),
        )
    }

    pub fn record(&mut self, now: Instant, add: Usage) {
        self.expire(now);
        self.total = self.total.plus(&add);
        self.entries.push_back((now, add));
    }
}

/// A decimal amount of ether, to wei: `0.5`, `2`, `0.000001`.
pub fn parse_eth(text: &str) -> Result<U256, String> {
    let bad = || format!("{text:?} is not an amount of ETH (e.g. 0.5)");
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty()
        || fraction.len() > 18
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(bad());
    }
    let wei = U256::from(10u64).pow(U256::from(18u64));
    let whole = if whole.is_empty() {
        U256::ZERO
    } else {
        whole.parse::<U256>().map_err(|_| bad())?
    };
    let fraction = if fraction.is_empty() {
        U256::ZERO
    } else {
        fraction.parse::<U256>().map_err(|_| bad())?
            * U256::from(10u64).pow(U256::from(18 - fraction.len()))
    };
    whole
        .checked_mul(wei)
        .and_then(|w| w.checked_add(fraction))
        .ok_or_else(bad)
}

/// A token amount in base units: an integer, or `<integer>e<exponent>` (`1000e18` is 1,000 tokens of 18 decimals).
pub fn parse_amount(text: &str) -> Result<U256, String> {
    let bad = || format!("{text:?} is not an amount in base units (e.g. 1000e18)");
    let (digits, exponent) = text.split_once('e').unwrap_or((text, "0"));
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let exponent: u8 = exponent.parse().map_err(|_| bad())?;
    let digits: U256 = digits.parse().map_err(|_| bad())?;
    U256::from(10u64)
        .checked_pow(U256::from(exponent))
        .and_then(|scale| digits.checked_mul(scale))
        .ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    #[test]
    fn the_vault_signs_the_chains_next_nonce_only() {
        let mut nonces = NonceGuard::with_chain(5, 5, 1);
        assert_eq!(nonces.check(5), Ok(()));
        assert_eq!(
            nonces.check(4),
            Err(NonceDenied::Used { nonce: 4, next: 5 })
        );
        assert_eq!(
            nonces.check(6),
            Err(NonceDenied::Ahead {
                nonce: 6,
                next: 5,
                max_pending: 1
            })
        );
        nonces.record(5, tx(1));
        // A replacement of 5 is signed; 6 is not until the chain moves past 5.
        assert_eq!(nonces.check(5), Ok(()));
        assert!(nonces.needs_read(6));
        assert_eq!(nonces.unverified(), vec![tx(1)]);
        // The chain spent 5 with the vault's transaction: 6 is next.
        assert_eq!(nonces.audit(6, &[tx(1)]), Ok(()));
        nonces.advance(6);
        assert!(nonces.unverified().is_empty());
        assert_eq!(nonces.check(6), Ok(()));
        assert_eq!(
            nonces.check(5),
            Err(NonceDenied::Used { nonce: 5, next: 6 })
        );
    }

    #[test]
    fn up_to_max_pending_nonces_ahead_and_never_a_gap() {
        let mut nonces = NonceGuard::with_chain(10, 10, 3);
        assert_eq!(nonces.check(10), Ok(()));
        assert_eq!(
            nonces.check(11),
            Err(NonceDenied::Gap {
                nonce: 11,
                unsigned: 10
            })
        );
        nonces.record(10, tx(1));
        nonces.record(11, tx(2));
        assert_eq!(nonces.check(12), Ok(()));
        nonces.record(12, tx(3));
        assert_eq!(
            nonces.check(13),
            Err(NonceDenied::Ahead {
                nonce: 13,
                next: 10,
                max_pending: 3
            })
        );
        // Two of them mined: two more may be signed.
        assert_eq!(nonces.audit(12, &[tx(1), tx(2)]), Ok(()));
        nonces.advance(12);
        assert_eq!(nonces.check(13), Ok(()));
        assert_eq!(nonces.unverified(), vec![tx(3)]);
    }

    #[test]
    fn a_nonce_spent_by_anything_else_stops_the_vault() {
        // A nonce the vault never signed.
        let mut nonces = NonceGuard::with_chain(5, 5, 1);
        nonces.record(5, tx(1));
        let reason = nonces.audit(7, &[tx(1)]).unwrap_err();
        assert!(
            reason.contains("nonce 6") && reason.contains("never signed"),
            "{reason}"
        );
        // The vault's nonce, spent by another transaction.
        let reason = nonces.audit(6, &[]).unwrap_err();
        assert!(
            reason.contains("nonce 5") && reason.contains(&tx(1).to_string()),
            "{reason}"
        );
        nonces.halt(reason);
        let denied = nonces.check(5).unwrap_err();
        assert!(matches!(denied, NonceDenied::Halted(_)) && denied.code() == "halted");
        assert!(!nonces.needs_read(9));
        // Pending transactions from before the start are not the vault's, and not foreign either.
        let nonces = NonceGuard::with_chain(5, 7, 1);
        assert_eq!(nonces.audit(7, &[]), Ok(()));
        assert_eq!(
            nonces.check(5),
            Ok(()),
            "a replacement of a pending nonce is signed"
        );
    }

    #[test]
    fn a_lagging_read_changes_nothing() {
        let mut nonces = NonceGuard::with_chain(8, 8, 1);
        nonces.record(8, tx(1));
        assert_eq!(nonces.audit(9, &[tx(1)]), Ok(()));
        nonces.advance(9);
        // Another node, a block behind.
        assert_eq!(nonces.audit(8, &[]), Ok(()));
        nonces.advance(8);
        assert_eq!(nonces.next(), 9);
    }

    #[test]
    fn without_a_chain_the_vault_follows_its_own_signatures() {
        let mut nonces = NonceGuard::without_chain();
        assert_eq!(nonces.check(41), Ok(()));
        nonces.record(41, tx(1));
        assert_eq!(nonces.check(41), Ok(()), "a replacement");
        assert_eq!(nonces.check(42), Ok(()));
        assert_eq!(
            nonces.check(40),
            Err(NonceDenied::Used {
                nonce: 40,
                next: 41
            })
        );
        assert_eq!(
            nonces.check(43),
            Err(NonceDenied::Gap {
                nonce: 43,
                unsigned: 42
            })
        );
        assert!(!nonces.needs_read(100));
    }

    fn caps() -> Caps {
        Caps {
            signs: 3,
            fee_wei: U256::from(1_000u32),
            volume0: Some(U256::from(100u32)),
            volume1: None,
            notes: Some(5),
        }
    }

    fn usage(fee: u32, volume0: u32, notes: u64) -> Usage {
        Usage {
            signs: 1,
            fee_wei: U256::from(fee),
            volume0: U256::from(volume0),
            volume1: U256::from(10u32).pow(U256::from(30u32)),
            notes,
        }
    }

    #[test]
    fn the_window_caps_signatures_fees_volumes_and_notes() {
        let t0 = Instant::now();
        let mut window = Window::new(Duration::from_secs(60), caps());
        let one = usage(300, 40, 2);
        for _ in 0..2 {
            assert_eq!(window.check(t0, &one), Ok(()));
            window.record(t0, one);
        }
        // The third would pass the token0 cap (120 > 100) before the signature cap.
        let denied = window.check(t0, &one).unwrap_err();
        assert_eq!(
            (denied.what, denied.cap),
            ("token0 (base units)", U256::from(100u32))
        );
        assert!(denied.to_string().contains("in the last 60 s"), "{denied}");
        let small = usage(300, 10, 1);
        assert_eq!(window.check(t0, &small), Ok(()));
        window.record(t0, small);
        assert_eq!(
            window.check(t0, &usage(1, 0, 0)).unwrap_err().what,
            "signatures"
        );
        assert_eq!(window.used(t0).signs, 3);
        // The fee budget and the notes, separately.
        let mut window = Window::new(Duration::from_secs(60), caps());
        assert_eq!(
            window.check(t0, &usage(1_001, 0, 0)).unwrap_err().what,
            "wei of fees"
        );
        assert_eq!(window.check(t0, &usage(1, 0, 6)).unwrap_err().what, "notes");
        // token1 has no cap.
        assert_eq!(window.check(t0, &usage(1, 0, 0)), Ok(()));
    }

    #[test]
    fn signatures_leave_the_window_as_it_slides() {
        let t0 = Instant::now();
        let mut window = Window::new(Duration::from_secs(60), caps());
        for i in 0..3 {
            window.record(t0 + Duration::from_secs(i * 10), usage(100, 10, 1));
        }
        let later = t0 + Duration::from_secs(59);
        assert_eq!(
            window.check(later, &usage(1, 0, 0)).unwrap_err().what,
            "signatures"
        );
        let slid = t0 + Duration::from_secs(60);
        assert_eq!(window.used(slid).signs, 2);
        assert_eq!(window.check(slid, &usage(1, 0, 0)), Ok(()));
        assert_eq!(
            window.used(t0 + Duration::from_secs(80)).fee_wei,
            U256::from(0u32)
        );
    }

    #[test]
    fn amounts_parse_exactly() {
        let e18 = U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(parse_eth("0.5"), Ok(e18 / U256::from(2u8)));
        assert_eq!(parse_eth("2"), Ok(e18 * U256::from(2u8)));
        assert_eq!(parse_eth(".000000000000000001"), Ok(U256::from(1u8)));
        for bad in [
            "",
            ".",
            "1.2.3",
            "-1",
            "1e18",
            "0.0000000000000000001",
            "abc",
        ] {
            assert!(parse_eth(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_amount("1000e18"), Ok(e18 * U256::from(1_000u32)));
        assert_eq!(parse_amount("12345"), Ok(U256::from(12_345u32)));
        for bad in ["", "e18", "1.5e18", "-1", "1e", "1e999", "2e77"] {
            assert!(parse_amount(bad).is_err(), "{bad:?}");
        }
    }
}
