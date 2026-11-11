//! From a plan to an unsigned transaction: wei amounts rounded in the pool's favour, `SearcherExecutor.rebalance`
//! calldata, and the EIP-1559 signing payload the vault receives. Nothing here signs.

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::sol_types::SolCall;

use crate::model::{EngineError, Round, to_wei};
use crate::planner::Plan;

alloy::sol! {
    /// SearcherExecutor.rebalance: the only call the engine makes (contracts/src/periphery/SearcherExecutor.sol).
    function rebalance(uint256 amount0Out, uint256 amount1Out, uint256 pay0, uint256 pay1, uint256 treasuryPay) external;
}

/// The plan in wei, rounded as bots/inventory-cpp does: the pool's payouts down, the executor's repayments up with a
/// 1e-9 relative margin, and the Treasury allowance (a cap) up. So the chain's exact checks pass whenever the plan's do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rebalance {
    pub amount0_out: U256,
    pub amount1_out: U256,
    pub pay0: U256,
    pub pay1: U256,
    pub treasury_pay: U256,
}

impl Rebalance {
    pub fn from_plan(plan: &Plan) -> Result<Self, EngineError> {
        Ok(Self {
            amount0_out: to_wei(plan.amount0_out, Round::Down)?,
            amount1_out: to_wei(plan.amount1_out, Round::Down)?,
            pay0: to_wei(plan.pay0 * (1.0 + 1e-9), Round::Up)?,
            pay1: to_wei(plan.pay1 * (1.0 + 1e-9), Round::Up)?,
            treasury_pay: to_wei(plan.treasury_pay, Round::Up)?,
        })
    }

    pub fn calldata(&self) -> Bytes {
        rebalanceCall {
            amount0Out: self.amount0_out,
            amount1Out: self.amount1_out,
            pay0: self.pay0,
            pay1: self.pay1,
            treasuryPay: self.treasury_pay,
        }
        .abi_encode()
        .into()
    }

    /// Decodes rebalance calldata (the mock chain's view of a transaction).
    pub fn decode(calldata: &[u8]) -> Option<Self> {
        let call = rebalanceCall::abi_decode(calldata).ok()?;
        Some(Self {
            amount0_out: call.amount0Out,
            amount1_out: call.amount1Out,
            pay0: call.pay0,
            pay1: call.pay1,
            treasury_pay: call.treasuryPay,
        })
    }
}

/// What the planner may commit out of a balance (tokens) so that the rounded transaction still fits in it: repayments
/// carry a relative margin of 1e-9 and every amount rounds up by at most one wei, so the planner sees the balance a few
/// parts per billion smaller, and 10⁻¹² token (10⁶ wei) smaller still for dust balances.
pub(crate) fn spendable(balance: f64) -> f64 {
    (balance * (1.0 - 4e-9) - 1e-12).max(0.0)
}

/// The fixed fields of every transaction the engine builds. The vault enforces its own caps on the same fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TxConfig {
    pub chain_id: u64,
    /// The SearcherExecutor contract.
    pub executor: Address,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

/// The unsigned EIP-1559 rebalance for a nonce.
pub(crate) fn unsigned_tx(cfg: &TxConfig, nonce: u64, rebalance: &Rebalance) -> TxEip1559 {
    TxEip1559 {
        chain_id: cfg.chain_id,
        nonce,
        gas_limit: cfg.gas_limit,
        max_fee_per_gas: cfg.max_fee_per_gas,
        max_priority_fee_per_gas: cfg.max_priority_fee_per_gas,
        to: TxKind::Call(cfg.executor),
        value: U256::ZERO,
        access_list: Default::default(),
        input: rebalance.calldata(),
    }
}

/// The EIP-1559 signing payload `0x02 ‖ rlp([chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas, gas_limit,
/// to, value, data, access_list])`: what the vault decodes, checks and signs.
pub(crate) fn signing_payload(tx: &TxEip1559) -> Vec<u8> {
    let mut out = Vec::with_capacity(tx.payload_len_for_signature());
    tx.encode_for_signing(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::wad;
    use alloy::primitives::address;

    #[test]
    fn the_selector_and_layout_are_the_cpp_bots() {
        // bots/inventory-cpp/include/strawberry/abi.hpp and its self-test: selector 0xc0661737, five 32-byte words.
        assert_eq!(rebalanceCall::SELECTOR, [0xc0, 0x66, 0x17, 0x37]);
        let r = Rebalance {
            amount0_out: wad("1"),
            amount1_out: U256::ZERO,
            pay0: U256::ZERO,
            pay1: U256::ZERO,
            treasury_pay: wad("2"),
        };
        let data = r.calldata();
        assert_eq!(data.len(), 4 + 5 * 32);
        assert_eq!(
            alloy::primitives::hex::encode(&data[4..36]),
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
        );
        assert_eq!(
            alloy::primitives::hex::encode(&data[4 + 4 * 32..]),
            "0000000000000000000000000000000000000000000000001bc16d674ec80000"
        );
        assert_eq!(Rebalance::decode(&data), Some(r));
        assert_eq!(Rebalance::decode(&data[..100]), None);
    }

    #[test]
    fn amounts_round_in_the_pools_favour() {
        let plan = Plan {
            sells_token0: true,
            amount0_out: 0.0,
            amount1_out: 0.1,
            pay0: 0.1,
            pay1: 0.0,
            treasury_pay: 0.1,
            harvest: 0.0,
            edge: 0.0,
            net: 0.0,
            target_price: 0.0,
        };
        let r = Rebalance::from_plan(&plan).unwrap();
        let w = |s: &str| U256::from_str_radix(s, 10).unwrap();
        assert_eq!(r.amount1_out, w("100000000000000005")); // the payout, down
        // The repayment, up, with the 1e-9 margin: 0.1 · (1 + 1e-9) is the double 0.10000000010000001, exactly
        // 100000000100000013.83 wei (Python's fractions.Fraction agrees).
        assert_eq!(r.pay0, w("100000000100000014"));
        assert_eq!(r.treasury_pay, w("100000000000000006")); // the allowance, up
        assert!(Rebalance::from_plan(&Plan { pay0: -1.0, ..plan }).is_err());
    }

    /// A plan that spends the whole spendable balance still fits once rounded to wei.
    #[test]
    fn a_plan_on_the_spendable_balance_fits_the_balance_once_rounded() {
        let mut rng = crate::chain::SplitMix64(5);
        for _ in 0..100_000 {
            let balance = 10f64.powf(rng.uniform() * 24.0 - 9.0); // 1e-9 to 1e15 tokens
            let balance_wei = to_wei(balance, Round::Down).unwrap();
            let pay = spendable(balance);
            let plan = Plan {
                pay0: pay,
                pay1: pay,
                treasury_pay: 0.0,
                ..zero_plan()
            };
            let r = Rebalance::from_plan(&plan).unwrap();
            assert!(
                r.pay0 <= balance_wei,
                "balance {balance}: pays {} of {balance_wei}",
                r.pay0
            );
        }
        assert_eq!(spendable(0.0), 0.0);
    }

    fn zero_plan() -> Plan {
        Plan {
            sells_token0: true,
            amount0_out: 0.0,
            amount1_out: 0.0,
            pay0: 0.0,
            pay1: 0.0,
            treasury_pay: 0.0,
            harvest: 0.0,
            edge: 0.0,
            net: 0.0,
            target_price: 0.0,
        }
    }

    #[test]
    fn the_signing_payload_is_an_eip1559_transaction_to_the_executor() {
        let cfg = TxConfig {
            chain_id: 31_337,
            executor: address!("0x5FbDB2315678afecb367f032d93F642f64180aa3"),
            gas_limit: 300_000,
            max_fee_per_gas: 30_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
        };
        let r = Rebalance {
            amount0_out: wad("1"),
            amount1_out: U256::ZERO,
            pay0: U256::ZERO,
            pay1: wad("2000"),
            treasury_pay: wad("1"),
        };
        let tx = unsigned_tx(&cfg, 7, &r);
        let payload = signing_payload(&tx);
        assert_eq!(payload[0], 0x02);
        let mut body = &payload[1..];
        let decoded = <TxEip1559 as alloy::rlp::Decodable>::decode(&mut body).unwrap();
        assert!(body.is_empty());
        assert_eq!(decoded, tx);
        assert_eq!(
            (decoded.nonce, decoded.to, decoded.value),
            (7, TxKind::Call(cfg.executor), U256::ZERO)
        );
        assert_eq!(Rebalance::decode(&decoded.input), Some(r));
    }
}
