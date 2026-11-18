//! What a role signs: one function, on one contract, with bounded arguments. Everything else is denied, and a request
//! for another role's function is refused as such. A vault process holds one role and one key:
//!
//! | role | contract | function |
//! |---|---|---|
//! | operator | SearcherExecutor | `rebalance`, never `sweep` |
//! | relayer | ZkPriceFeed | `submit`, for the feed's own provider and instrument |
//! | automator | DarkPool | `settleBatch` |

use std::fmt;
use std::str::FromStr;

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{Address, B256, TxKind, U256, hex};
use alloy_rlp::Decodable;
use alloy_sol_types::abi::AbiDecoderConfig;
use alloy_sol_types::{SolCall, sol};

/// The intrinsic gas of any transaction.
pub const INTRINSIC_GAS: u64 = 21_000;
/// ZkDecoOracle's proof wire format: a Groth16 proof (8 words) ‖ the body commitment (1 word) ‖ the notary's signature
/// (r ‖ s ‖ v).
pub const ATTESTATION_PROOF_LEN: usize = 8 * 32 + 32 + 65;
/// The batch circuit's Groth16 proof: abi.encode(uint256[2] a, uint256[2][2] b, uint256[2] c).
pub const BATCH_PROOF_LEN: usize = 8 * 32;
/// A sealed output note is an AEAD nonce, 33 bytes of plaintext and a tag (about 61 bytes); this bounds it loosely.
pub const MAX_NOTE_LEN: usize = 256;

sol! {
    // SearcherExecutor (the operator's)
    function rebalance(uint256 amount0Out, uint256 amount1Out, uint256 pay0, uint256 pay1, uint256 treasuryPay);
    function sweep(address token, address to, uint256 amount);

    // ZkPriceFeed (the relayer's)
    struct Attestation {
        bytes32 providerId;
        bytes32 queryCommitment;
        uint256 price;
        uint64 observedAt;
    }
    function submit(Attestation a, bytes proof);

    // DarkPool (the automator's)
    struct Batch {
        bytes proof;
        bytes32 root;
        uint64 tNow;
        bytes32[] nullifiers;
        bytes32[] commitments;
        bytes[] encryptedNotes;
        uint256 in0;
        uint256 in1;
        uint256 out0;
        uint256 out1;
        uint256 X;
        uint256 Y;
    }
    function settleBatch(Batch b);
}

/// The three roles. Each has its own key, its own process and its own socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Operator,
    Relayer,
    Automator,
}

impl Role {
    pub const ALL: [Self; 3] = [Self::Operator, Self::Relayer, Self::Automator];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Relayer => "relayer",
            Self::Automator => "automator",
        }
    }

    /// The contract the role's transactions go to.
    pub const fn contract(self) -> &'static str {
        match self {
            Self::Operator => "SearcherExecutor",
            Self::Relayer => "ZkPriceFeed",
            Self::Automator => "DarkPool",
        }
    }

    /// The one function the role signs, as its Solidity signature.
    pub const fn function(self) -> &'static str {
        match self {
            Self::Operator => rebalanceCall::SIGNATURE,
            Self::Relayer => submitCall::SIGNATURE,
            Self::Automator => settleBatchCall::SIGNATURE,
        }
    }

    pub const fn selector(self) -> [u8; 4] {
        match self {
            Self::Operator => rebalanceCall::SELECTOR,
            Self::Relayer => submitCall::SELECTOR,
            Self::Automator => settleBatchCall::SELECTOR,
        }
    }

    /// The role whose function this selector is, if any.
    fn owning(selector: [u8; 4]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|role| role.selector() == selector)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|role| role.name() == text)
            .ok_or_else(|| format!("{text:?} is not a role: operator, relayer or automator"))
    }
}

/// The role's rules for the calldata, with what they were read from (the feed's configuration, the pool's batch size).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Calls {
    /// `rebalance`: five words below 2^128, at least one output.
    Rebalance,
    /// `submit`: for the feed's provider and instrument, a price within the feed's bounds, a wire-format proof.
    Submit {
        provider_id: B256,
        query_commitment: B256,
        min_price: U256,
        max_price: U256,
    },
    /// `settleBatch`: 1 to `max_notes` notes, consistent arrays, bounded ciphertexts, flows below 2^128.
    SettleBatch { max_notes: usize },
}

impl Calls {
    pub const fn role(&self) -> Role {
        match self {
            Self::Rebalance => Role::Operator,
            Self::Submit { .. } => Role::Relayer,
            Self::SettleBatch { .. } => Role::Automator,
        }
    }
}

/// What a signed transaction commits to, for the window's caps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    /// token0 moved, both ways: a rebalance's amount0Out + pay0, a batch's in0 + out0.
    pub volume0: U256,
    /// token1 moved, both ways: a rebalance's amount1Out + pay1 + treasuryPay, a batch's in1 + out1.
    pub volume1: U256,
    /// Notes settled (a batch's nullifiers).
    pub notes: u64,
}

/// What one role's vault signs. Fixed at start.
#[derive(Clone, Debug)]
pub struct Policy {
    pub chain_id: u64,
    /// The role's contract, the only destination.
    pub to: Address,
    /// Cap on the gas limit.
    pub max_gas: u64,
    /// Cap on `max_fee_per_gas`, wei.
    pub max_fee_per_gas: u128,
    pub calls: Calls,
}

/// Why a payload is not signed.
#[derive(Debug, PartialEq, Eq)]
pub enum Denied {
    Empty,
    TxType(u8),
    Rlp(String),
    TrailingBytes(usize),
    NonCanonical,
    ChainId {
        got: u64,
        want: u64,
    },
    Create,
    To {
        to: Address,
        role: Role,
    },
    Value(U256),
    AccessList,
    GasLimit(u64),
    MaxFee(u128),
    PriorityFee,
    Sweep,
    /// Another role's function: never signed with this role's key.
    OtherRole {
        owner: Role,
        role: Role,
    },
    Selector {
        selector: [u8; 4],
        role: Role,
    },
    CalldataLength(usize),
    /// Not a canonical ABI encoding of the role's function.
    Abi(Role),
    Amount(&'static str),
    NoOutput,
    Provider(B256),
    Instrument(B256),
    Price(U256),
    ObservedAt,
    ProofLength {
        len: usize,
        want: usize,
    },
    Notes {
        notes: usize,
        max: usize,
    },
    NoteArrays,
    NoteLength {
        index: usize,
        len: usize,
    },
    ZeroNote(usize),
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty payload"),
            Self::TxType(t) => write!(
                f,
                "transaction type {t:#04x}: only EIP-1559 (0x02) transactions are signed"
            ),
            Self::Rlp(e) => write!(f, "not a valid EIP-1559 transaction: {e}"),
            Self::TrailingBytes(n) => write!(f, "{n} bytes after the transaction"),
            Self::NonCanonical => f.write_str("the transaction is not canonically encoded"),
            Self::ChainId { got, want } => {
                write!(f, "chain id {got}, this vault signs for chain {want} only")
            }
            Self::Create => f.write_str("contract creation is never signed"),
            Self::To { to, role } => write!(
                f,
                "destination {to} is not the {} the {role} role signs for",
                role.contract()
            ),
            Self::Value(v) => write!(f, "value {v} wei: the vault signs no ether transfer"),
            Self::AccessList => f.write_str("access lists are not signed"),
            Self::GasLimit(g) => write!(f, "gas limit {g} outside [{INTRINSIC_GAS}, cap]"),
            Self::MaxFee(fee) => write!(f, "max fee per gas {fee} wei above the cap"),
            Self::PriorityFee => f.write_str("priority fee above the max fee"),
            Self::Sweep => {
                f.write_str("sweep moves inventory out of the executor: never signed by this vault")
            }
            Self::OtherRole { owner, role } => write!(
                f,
                "{} is the {owner} role's function: this vault holds the {role} key and signs {} only",
                owner.function(),
                role.function()
            ),
            Self::Selector { selector, role } => write!(
                f,
                "selector {}: the {role} role signs {} only",
                hex::encode_prefixed(selector),
                role.function()
            ),
            Self::CalldataLength(n) => write!(
                f,
                "calldata of {n} bytes: rebalance takes exactly 4 + 5 × 32 = 164"
            ),
            Self::Abi(role) => write!(
                f,
                "the calldata is not a canonical ABI encoding of {}",
                role.function()
            ),
            Self::Amount(arg) => write!(f, "{arg} does not fit in 128 bits"),
            Self::NoOutput => {
                f.write_str("amount0Out and amount1Out are both zero: the pool would revert")
            }
            Self::Provider(id) => write!(f, "provider {id} is not the feed's"),
            Self::Instrument(id) => write!(f, "instrument commitment {id} is not the feed's"),
            Self::Price(price) => write!(f, "price {price} outside the feed's bounds"),
            Self::ObservedAt => f.write_str("an observation time of 0"),
            Self::ProofLength { len, want } => {
                write!(f, "a proof of {len} bytes, the wire format has {want}")
            }
            Self::Notes { notes, max } => write!(f, "{notes} notes: a batch has 1 to {max}"),
            Self::NoteArrays => f.write_str(
                "the nullifiers, the commitments and the encrypted notes differ in number",
            ),
            Self::NoteLength { index, len } => write!(
                f,
                "encrypted note {index} has {len} bytes, more than {MAX_NOTE_LEN}"
            ),
            Self::ZeroNote(index) => write!(f, "note {index} has a zero nullifier or commitment"),
        }
    }
}

/// 2^128: every amount the roles sign stays below it (the pool's reserves are far smaller).
fn below_2_128(word: U256, name: &'static str) -> Result<U256, Denied> {
    if word > U256::from(u128::MAX) {
        Err(Denied::Amount(name))
    } else {
        Ok(word)
    }
}

/// Strict ABI decoding: canonical offsets, zero padding, no trailing bytes; and the bytes must be exactly what the
/// decoded call re-encodes to.
fn decode<C: SolCall>(input: &[u8], role: Role) -> Result<C, Denied> {
    let call = C::abi_decode_with_config(input, AbiDecoderConfig::new().strict(true))
        .map_err(|_| Denied::Abi(role))?;
    if call.abi_encode() != input {
        return Err(Denied::Abi(role));
    }
    Ok(call)
}

impl Policy {
    pub const fn role(&self) -> Role {
        self.calls.role()
    }

    /// Decodes `raw` (the EIP-1559 signing payload, `0x02 ‖ rlp(fields)`) and checks it field by field. Returns the
    /// transaction and what it commits to, only if every rule holds.
    pub fn check(&self, raw: &[u8]) -> Result<(TxEip1559, Size), Denied> {
        let (&ty, mut body) = raw.split_first().ok_or(Denied::Empty)?;
        if ty != 0x02 {
            return Err(Denied::TxType(ty));
        }
        let tx = TxEip1559::decode(&mut body).map_err(|e| Denied::Rlp(e.to_string()))?;
        if !body.is_empty() {
            return Err(Denied::TrailingBytes(body.len()));
        }
        // The decoder already rejects non-canonical RLP; re-encoding makes sure that what is signed is exactly the
        // payload that was received.
        let mut canonical = Vec::with_capacity(raw.len());
        tx.encode_for_signing(&mut canonical);
        if canonical != raw {
            return Err(Denied::NonCanonical);
        }

        if tx.chain_id != self.chain_id {
            return Err(Denied::ChainId {
                got: tx.chain_id,
                want: self.chain_id,
            });
        }
        // The function first: a request for another role's function says so, wherever it is addressed.
        let selector = tx.input.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok());
        if let Some(selector) = selector {
            if selector == sweepCall::SELECTOR && self.role() == Role::Operator {
                return Err(Denied::Sweep);
            }
            match Role::owning(selector) {
                Some(owner) if owner != self.role() => {
                    return Err(Denied::OtherRole {
                        owner,
                        role: self.role(),
                    });
                }
                Some(_) => {}
                None => {
                    return Err(Denied::Selector {
                        selector,
                        role: self.role(),
                    });
                }
            }
        }
        match tx.to {
            TxKind::Create => return Err(Denied::Create),
            TxKind::Call(to) if to != self.to => {
                return Err(Denied::To {
                    to,
                    role: self.role(),
                });
            }
            TxKind::Call(_) => {}
        }
        if !tx.value.is_zero() {
            return Err(Denied::Value(tx.value));
        }
        if !tx.access_list.0.is_empty() {
            return Err(Denied::AccessList);
        }
        if !(INTRINSIC_GAS..=self.max_gas).contains(&tx.gas_limit) {
            return Err(Denied::GasLimit(tx.gas_limit));
        }
        if tx.max_fee_per_gas > self.max_fee_per_gas {
            return Err(Denied::MaxFee(tx.max_fee_per_gas));
        }
        if tx.max_priority_fee_per_gas > tx.max_fee_per_gas {
            return Err(Denied::PriorityFee);
        }
        let size = match &self.calls {
            Calls::Rebalance => check_rebalance(&tx.input)?,
            Calls::Submit {
                provider_id,
                query_commitment,
                min_price,
                max_price,
            } => {
                let call: submitCall = decode(&tx.input, Role::Relayer)?;
                if call.a.providerId != *provider_id {
                    return Err(Denied::Provider(call.a.providerId));
                }
                if call.a.queryCommitment != *query_commitment {
                    return Err(Denied::Instrument(call.a.queryCommitment));
                }
                if call.a.price < *min_price || call.a.price > *max_price {
                    return Err(Denied::Price(call.a.price));
                }
                if call.a.observedAt == 0 {
                    return Err(Denied::ObservedAt);
                }
                if call.proof.len() != ATTESTATION_PROOF_LEN {
                    return Err(Denied::ProofLength {
                        len: call.proof.len(),
                        want: ATTESTATION_PROOF_LEN,
                    });
                }
                Size::default()
            }
            Calls::SettleBatch { max_notes } => check_batch(&tx.input, *max_notes)?,
        };
        Ok((tx, size))
    }
}

/// `rebalance(uint256 amount0Out, uint256 amount1Out, uint256 pay0, uint256 pay1, uint256 treasuryPay)`: exactly five
/// words, each below 2^128 (the pool's reserves are uint128, and the C++ bot encodes amounts the same way), and at
/// least one non-zero output, since `DarkPool.flashRebalance` reverts otherwise.
fn check_rebalance(input: &[u8]) -> Result<Size, Denied> {
    if input.len() != 4 + 5 * 32 {
        return Err(Denied::CalldataLength(input.len()));
    }
    let call: rebalanceCall = decode(input, Role::Operator)?;
    let amount0_out = below_2_128(call.amount0Out, "amount0Out")?;
    let amount1_out = below_2_128(call.amount1Out, "amount1Out")?;
    let pay0 = below_2_128(call.pay0, "pay0")?;
    let pay1 = below_2_128(call.pay1, "pay1")?;
    let treasury_pay = below_2_128(call.treasuryPay, "treasuryPay")?;
    if amount0_out.is_zero() && amount1_out.is_zero() {
        return Err(Denied::NoOutput);
    }
    Ok(Size {
        volume0: amount0_out + pay0,
        volume1: amount1_out + pay1 + treasury_pay,
        notes: 0,
    })
}

/// `settleBatch(Batch)`: what DarkPool checks first (1 to batchSize notes, three arrays of one length, no zero nullifier
/// or commitment), the proof's wire length, bounded ciphertexts, and flows below 2^128.
fn check_batch(input: &[u8], max_notes: usize) -> Result<Size, Denied> {
    let call: settleBatchCall = decode(input, Role::Automator)?;
    let b = call.b;
    let notes = b.nullifiers.len();
    if notes == 0 || notes > max_notes {
        return Err(Denied::Notes {
            notes,
            max: max_notes,
        });
    }
    if b.commitments.len() != notes || b.encryptedNotes.len() != notes {
        return Err(Denied::NoteArrays);
    }
    for (index, (nullifier, commitment)) in b.nullifiers.iter().zip(&b.commitments).enumerate() {
        if nullifier.is_zero() || commitment.is_zero() {
            return Err(Denied::ZeroNote(index));
        }
    }
    if let Some((index, note)) = b
        .encryptedNotes
        .iter()
        .enumerate()
        .find(|(_, note)| note.len() > MAX_NOTE_LEN)
    {
        return Err(Denied::NoteLength {
            index,
            len: note.len(),
        });
    }
    if b.proof.len() != BATCH_PROOF_LEN {
        return Err(Denied::ProofLength {
            len: b.proof.len(),
            want: BATCH_PROOF_LEN,
        });
    }
    let in0 = below_2_128(b.in0, "in0")?;
    let in1 = below_2_128(b.in1, "in1")?;
    let out0 = below_2_128(b.out0, "out0")?;
    let out1 = below_2_128(b.out1, "out1")?;
    below_2_128(b.X, "X")?;
    below_2_128(b.Y, "Y")?;
    Ok(Size {
        volume0: in0 + out0,
        volume1: in1 + out1,
        notes: notes as u64,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloy_eips::eip2930::{AccessList, AccessListItem};
    use alloy_primitives::{Bytes, address, b256};

    pub const EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");
    pub const FEED: Address = address!("0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512");
    pub const POOL: Address = address!("0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0");
    pub const PROVIDER: B256 =
        b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
    pub const INSTRUMENT: B256 =
        b256!("0x2222222222222222222222222222222222222222222222222222222222222222");

    pub fn e18() -> U256 {
        U256::from(10u64).pow(U256::from(18u64))
    }

    pub fn policy(calls: Calls) -> Policy {
        let to = match calls.role() {
            Role::Operator => EXECUTOR,
            Role::Relayer => FEED,
            Role::Automator => POOL,
        };
        Policy {
            chain_id: 31_337,
            to,
            max_gas: 2_000_000,
            max_fee_per_gas: 500_000_000_000,
            calls,
        }
    }

    pub fn operator() -> Policy {
        policy(Calls::Rebalance)
    }

    pub fn relayer() -> Policy {
        policy(Calls::Submit {
            provider_id: PROVIDER,
            query_commitment: INSTRUMENT,
            min_price: e18(),
            max_price: e18() * U256::from(1_000_000u32),
        })
    }

    pub fn automator() -> Policy {
        policy(Calls::SettleBatch { max_notes: 4 })
    }

    pub fn tx_to(to: Address, input: Vec<u8>) -> TxEip1559 {
        TxEip1559 {
            chain_id: 31_337,
            nonce: 7,
            gas_limit: 300_000,
            max_fee_per_gas: 30_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call(to),
            value: U256::ZERO,
            access_list: AccessList::default(),
            input: input.into(),
        }
    }

    pub fn raw(tx: &TxEip1559) -> Vec<u8> {
        let mut out = Vec::new();
        tx.encode_for_signing(&mut out);
        out
    }

    /// A plausible rebalance: the pool sends 1 token0 out, the executor returns 2 token1 and allows a 0.004 cut.
    pub fn rebalance_call() -> rebalanceCall {
        rebalanceCall {
            amount0Out: e18(),
            amount1Out: U256::ZERO,
            pay0: U256::ZERO,
            pay1: e18() * U256::from(2u8),
            treasuryPay: e18() / U256::from(250u16),
        }
    }

    pub fn rebalance_tx() -> TxEip1559 {
        tx_to(EXECUTOR, rebalance_call().abi_encode())
    }

    pub fn submit_call() -> submitCall {
        submitCall {
            a: Attestation {
                providerId: PROVIDER,
                queryCommitment: INSTRUMENT,
                price: e18() * U256::from(3_456u32),
                observedAt: 1_800_000_000,
            },
            proof: Bytes::from(vec![0x5a; ATTESTATION_PROOF_LEN]),
        }
    }

    pub fn submit_tx() -> TxEip1559 {
        tx_to(FEED, submit_call().abi_encode())
    }

    pub fn batch(notes: usize) -> Batch {
        Batch {
            proof: Bytes::from(vec![0x33; BATCH_PROOF_LEN]),
            root: B256::repeat_byte(0x44),
            tNow: 1_800_000_000,
            nullifiers: (0..notes)
                .map(|i| B256::repeat_byte(0x10 + i as u8))
                .collect(),
            commitments: (0..notes)
                .map(|i| B256::repeat_byte(0x80 + i as u8))
                .collect(),
            encryptedNotes: (0..notes).map(|_| Bytes::from(vec![0xee; 61])).collect(),
            in0: e18(),
            in1: U256::ZERO,
            out0: U256::ZERO,
            out1: e18() * U256::from(3_000u32),
            X: e18() * U256::from(1_000u32),
            Y: e18() * U256::from(3_456_780u32),
        }
    }

    pub fn settle_tx(b: Batch) -> TxEip1559 {
        tx_to(POOL, settleBatchCall { b }.abi_encode())
    }

    #[test]
    fn the_selectors_are_the_contracts() {
        // bots/inventory-cpp/include/strawberry/abi.hpp: 0xc0661737, checked there against solc's method identifiers.
        assert_eq!(Role::Operator.selector(), [0xc0, 0x66, 0x17, 0x37]);
        assert_eq!(
            Role::Operator.function(),
            "rebalance(uint256,uint256,uint256,uint256,uint256)"
        );
        assert_eq!(
            Role::Relayer.function(),
            "submit((bytes32,bytes32,uint256,uint64),bytes)"
        );
        // bots/automator/testdata/settleBatch.calldata.hex starts with it (checked there against Foundry's cast).
        assert_eq!(Role::Automator.selector(), [0x25, 0xa5, 0xb9, 0x41]);
        assert_eq!(
            sweepCall::SELECTOR,
            alloy_primitives::keccak256("sweep(address,address,uint256)")[..4]
        );
        for role in Role::ALL {
            assert_eq!(role.name().parse::<Role>(), Ok(role));
        }
        assert!("owner".parse::<Role>().is_err());
    }

    #[test]
    fn each_role_signs_its_function() {
        let (tx, size) = operator().check(&raw(&rebalance_tx())).unwrap();
        assert_eq!(tx, rebalance_tx());
        assert_eq!(
            size,
            Size {
                volume0: e18(),
                volume1: e18() * U256::from(2u8) + e18() / U256::from(250u16),
                notes: 0
            }
        );
        let (_, size) = relayer().check(&raw(&submit_tx())).unwrap();
        assert_eq!(size, Size::default());
        let (_, size) = automator().check(&raw(&settle_tx(batch(3)))).unwrap();
        assert_eq!(
            size,
            Size {
                volume0: e18(),
                volume1: e18() * U256::from(3_000u32),
                notes: 3
            }
        );
    }

    #[test]
    fn the_automators_golden_calldata_is_a_batch_the_automator_role_signs() {
        // The Go automator's encoder output, checked there against Foundry's cast: one note, a 256-byte proof.
        let text = include_str!("../../../bots/automator/testdata/settleBatch.calldata.hex");
        let input = hex::decode(text.trim()).unwrap();
        let (_, size) = policy(Calls::SettleBatch { max_notes: 64 })
            .check(&raw(&tx_to(POOL, input)))
            .unwrap();
        assert!(size.notes >= 1, "{size:?}");
    }

    #[test]
    fn a_role_never_signs_another_roles_function() {
        let cases = [
            (operator(), submit_tx(), Role::Relayer),
            (operator(), settle_tx(batch(1)), Role::Automator),
            (relayer(), rebalance_tx(), Role::Operator),
            (relayer(), settle_tx(batch(1)), Role::Automator),
            (automator(), rebalance_tx(), Role::Operator),
            (automator(), submit_tx(), Role::Relayer),
        ];
        for (policy, tx, owner) in cases {
            // Addressed to the role's own contract, or to the other role's: refused as the other role's function.
            for to in [policy.to, tx.to.to().copied().unwrap()] {
                let tx = TxEip1559 {
                    to: TxKind::Call(to),
                    ..tx.clone()
                };
                let denied = policy.check(&raw(&tx)).unwrap_err();
                assert_eq!(
                    denied,
                    Denied::OtherRole {
                        owner,
                        role: policy.role()
                    }
                );
                assert!(denied.to_string().contains(owner.function()), "{denied}");
            }
        }
    }

    #[test]
    fn every_other_transaction_is_denied() {
        let ok = rebalance_tx();
        let words = |w: [U256; 5]| {
            rebalanceCall {
                amount0Out: w[0],
                amount1Out: w[1],
                pay0: w[2],
                pay1: w[3],
                treasuryPay: w[4],
            }
            .abi_encode()
            .into()
        };
        let cases: Vec<(&str, TxEip1559, Denied)> = vec![
            (
                "another chain",
                TxEip1559 {
                    chain_id: 1,
                    ..ok.clone()
                },
                Denied::ChainId {
                    got: 1,
                    want: 31_337,
                },
            ),
            (
                "contract creation",
                TxEip1559 {
                    to: TxKind::Create,
                    ..ok.clone()
                },
                Denied::Create,
            ),
            (
                "another destination",
                TxEip1559 {
                    to: TxKind::Call(Address::repeat_byte(0x11)),
                    ..ok.clone()
                },
                Denied::To {
                    to: Address::repeat_byte(0x11),
                    role: Role::Operator,
                },
            ),
            (
                "ether attached",
                TxEip1559 {
                    value: U256::from(1u8),
                    ..ok.clone()
                },
                Denied::Value(U256::from(1u8)),
            ),
            (
                "an access list",
                TxEip1559 {
                    access_list: AccessList(vec![AccessListItem {
                        address: EXECUTOR,
                        storage_keys: vec![],
                    }]),
                    ..ok.clone()
                },
                Denied::AccessList,
            ),
            (
                "gas below intrinsic",
                TxEip1559 {
                    gas_limit: 20_999,
                    ..ok.clone()
                },
                Denied::GasLimit(20_999),
            ),
            (
                "gas above the cap",
                TxEip1559 {
                    gas_limit: 2_000_001,
                    ..ok.clone()
                },
                Denied::GasLimit(2_000_001),
            ),
            (
                "max fee above the cap",
                TxEip1559 {
                    max_fee_per_gas: 500_000_000_001,
                    ..ok.clone()
                },
                Denied::MaxFee(500_000_000_001),
            ),
            (
                "priority above max",
                TxEip1559 {
                    max_priority_fee_per_gas: 31_000_000_000,
                    ..ok.clone()
                },
                Denied::PriorityFee,
            ),
            (
                "sweep",
                TxEip1559 {
                    input: sweepCall {
                        token: Address::repeat_byte(1),
                        to: Address::repeat_byte(2),
                        amount: e18(),
                    }
                    .abi_encode()
                    .into(),
                    ..ok.clone()
                },
                Denied::Sweep,
            ),
            (
                "an ERC-20 transfer",
                TxEip1559 {
                    input: [&[0xa9, 0x05, 0x9c, 0xbb][..], &[0u8; 64]].concat().into(),
                    ..ok.clone()
                },
                Denied::Selector {
                    selector: [0xa9, 0x05, 0x9c, 0xbb],
                    role: Role::Operator,
                },
            ),
            (
                "no calldata",
                TxEip1559 {
                    input: Bytes::new(),
                    ..ok.clone()
                },
                Denied::CalldataLength(0),
            ),
            (
                "four words",
                TxEip1559 {
                    input: rebalance_tx().input[..4 + 4 * 32].to_vec().into(),
                    ..ok.clone()
                },
                Denied::CalldataLength(132),
            ),
            (
                "an amount above 2^128",
                TxEip1559 {
                    input: words([
                        e18(),
                        U256::ZERO,
                        U256::ZERO,
                        U256::from(u128::MAX) + U256::from(1u8),
                        e18(),
                    ]),
                    ..ok.clone()
                },
                Denied::Amount("pay1"),
            ),
            (
                "no output",
                TxEip1559 {
                    input: words([U256::ZERO, U256::ZERO, e18(), e18(), e18()]),
                    ..ok.clone()
                },
                Denied::NoOutput,
            ),
        ];
        for (name, tx, want) in cases {
            assert_eq!(operator().check(&raw(&tx)), Err(want), "{name}");
        }
    }

    #[test]
    fn the_relayer_signs_only_its_feeds_attestations() {
        let with = |f: &dyn Fn(&mut submitCall)| {
            let mut call = submit_call();
            f(&mut call);
            relayer().check(&raw(&tx_to(FEED, call.abi_encode())))
        };
        assert_eq!(
            with(&|c| c.a.providerId = B256::repeat_byte(9)),
            Err(Denied::Provider(B256::repeat_byte(9)))
        );
        assert_eq!(
            with(&|c| c.a.queryCommitment = B256::repeat_byte(9)),
            Err(Denied::Instrument(B256::repeat_byte(9)))
        );
        assert_eq!(
            with(&|c| c.a.price = U256::from(1u8)),
            Err(Denied::Price(U256::from(1u8)))
        );
        assert_eq!(with(&|c| c.a.observedAt = 0), Err(Denied::ObservedAt));
        assert_eq!(
            with(&|c| c.proof = Bytes::from(vec![1; 352])),
            Err(Denied::ProofLength {
                len: 352,
                want: ATTESTATION_PROOF_LEN
            })
        );
        // Not canonical: a trailing word, or a proof offset pointing elsewhere.
        let mut trailing = submit_call().abi_encode();
        trailing.extend_from_slice(&[0; 32]);
        assert_eq!(
            relayer().check(&raw(&tx_to(FEED, trailing))),
            Err(Denied::Abi(Role::Relayer))
        );
        let mut moved = submit_call().abi_encode();
        moved[4 + 4 * 32 + 31] += 32;
        moved.extend_from_slice(&[0; 32]);
        assert_eq!(
            relayer().check(&raw(&tx_to(FEED, moved))),
            Err(Denied::Abi(Role::Relayer))
        );
    }

    #[test]
    fn the_automator_signs_only_well_formed_batches() {
        let check = |b: Batch| automator().check(&raw(&settle_tx(b)));
        assert_eq!(check(batch(0)), Err(Denied::Notes { notes: 0, max: 4 }));
        assert_eq!(check(batch(5)), Err(Denied::Notes { notes: 5, max: 4 }));
        let mut b = batch(2);
        b.commitments.pop();
        assert_eq!(check(b), Err(Denied::NoteArrays));
        let mut b = batch(2);
        b.encryptedNotes.push(Bytes::new());
        assert_eq!(check(b), Err(Denied::NoteArrays));
        let mut b = batch(2);
        b.nullifiers[1] = B256::ZERO;
        assert_eq!(check(b), Err(Denied::ZeroNote(1)));
        let mut b = batch(2);
        b.encryptedNotes[0] = Bytes::from(vec![0; MAX_NOTE_LEN + 1]);
        assert_eq!(
            check(b),
            Err(Denied::NoteLength {
                index: 0,
                len: MAX_NOTE_LEN + 1
            })
        );
        let mut b = batch(2);
        b.proof = Bytes::from(vec![0; 288]);
        assert_eq!(
            check(b),
            Err(Denied::ProofLength {
                len: 288,
                want: BATCH_PROOF_LEN
            })
        );
        let mut b = batch(2);
        b.out1 = U256::MAX;
        assert_eq!(check(b), Err(Denied::Amount("out1")));
        let mut garbage = settleBatchCall { b: batch(2) }.abi_encode();
        garbage.truncate(garbage.len() - 1);
        assert_eq!(
            automator().check(&raw(&tx_to(POOL, garbage))),
            Err(Denied::Abi(Role::Automator))
        );
    }

    #[test]
    fn malformed_and_non_eip1559_payloads_are_denied() {
        let good = raw(&rebalance_tx());
        assert_eq!(operator().check(&[]), Err(Denied::Empty));
        for ty in [0x01, 0x03, 0x04, 0xf8] {
            let mut other = good.clone();
            other[0] = ty;
            assert_eq!(
                operator().check(&other),
                Err(Denied::TxType(ty)),
                "type {ty:#04x}"
            );
        }
        let truncated = &good[..good.len() - 1];
        assert!(matches!(operator().check(truncated), Err(Denied::Rlp(_))));
        let mut trailing = good.clone();
        trailing.push(0x00);
        assert_eq!(operator().check(&trailing), Err(Denied::TrailingBytes(1)));
        // The same list with a non-minimal length prefix (0xf9 0x00 len instead of 0xf8 len) is not accepted.
        assert_eq!(good[1], 0xf8, "a rebalance list has a one-byte length");
        let mut long_header = vec![0x02, 0xf9, 0x00];
        long_header.extend_from_slice(&good[2..]);
        assert!(matches!(
            operator().check(&long_header),
            Err(Denied::Rlp(_))
        ));
    }
}
