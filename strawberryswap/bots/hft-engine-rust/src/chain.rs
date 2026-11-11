//! The engine's view of the chain (reads and broadcast) and a deterministic mock of it.
//!
//! The mock is the C++ engine's `SimChain` (bots/inventory-cpp/include/strawberry/sim.hpp) behind a transaction
//! boundary. It accepts only signed EIP-1559 transactions, recovers the sender from the signature, consumes the nonce,
//! and executes `SearcherExecutor.rebalance` with the contract's checks: the sender must be the executor's operator,
//! and the rebalance must pass DarkPool's acceptance rules (`planner::chain_rules`).

use std::time::Duration;

use alloy::consensus::TxEnvelope;
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, B256, TxKind, U256};
use tokio::time;

use crate::model::{EngineError, OracleReading, Round, to_wei, unix_now, wad_to_f64};
use crate::planner::{PoolState, chain_rules};
use crate::tx::Rebalance;

/// DarkPool's reserves (R0, R1) and virtual reserves (v0, v1), 18-decimal fixed point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PoolWei {
    pub r0: U256,
    pub r1: U256,
    pub v0: U256,
    pub v1: U256,
}

impl PoolWei {
    pub fn to_tokens(self) -> Result<PoolState, EngineError> {
        Ok(PoolState {
            r0: wad_to_f64(self.r0)?,
            r1: wad_to_f64(self.r1)?,
            v0: wad_to_f64(self.v0)?,
            v1: wad_to_f64(self.v1)?,
        })
    }
}

/// One tick's reads, as the contracts return them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChainState {
    /// IZKOracle.latestVerifiedPrice().
    pub oracle: OracleReading,
    /// The SearcherExecutor's balances (IERC20.balanceOf), wei.
    pub token0: U256,
    pub token1: U256,
    pub pool: PoolWei,
    /// The operator's next nonce (eth_getTransactionCount).
    pub nonce: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TxStatus {
    Success,
    Reverted(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub tx_hash: B256,
    /// The sender recovered from the signature.
    pub signer: Address,
    pub status: TxStatus,
}

/// Reads and broadcast. Mocked in this iteration; live, `read` is a batch of eth_calls through an alloy provider and
/// `send_raw_transaction` is eth_sendRawTransaction.
pub(crate) trait Chain {
    async fn read(&mut self) -> Result<ChainState, EngineError>;
    async fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<Receipt, EngineError>;
}

/// SplitMix64 (Steele, Lea and Flood, 2014): small, fast and well tested; deterministic for a given seed.
pub(crate) struct SplitMix64(pub u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform on [0, 1), from 53 random bits.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
    }

    /// Standard normal (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = 1.0 - self.uniform(); // in (0, 1], so the logarithm is finite
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Simulated RPC round trip of each mock call.
const MOCK_LATENCY: Duration = Duration::from_millis(2);

/// The mock world: a DarkPool, the zk oracle, the SearcherExecutor's inventory and the operator's nonce, in tokens.
/// Each read advances it by one tick: the oracle follows dS = σ dW, and with some probability a batch of user trades
/// moves the pool along its curve, away from the oracle. The engine's rebalances pull it back.
pub(crate) struct MockChain {
    pub chain_id: u64,
    pub executor: Address,
    /// SearcherExecutor.operator: the only sender `rebalance` accepts (the vault's address).
    pub operator: Address,
    pub pool: PoolState,
    pub oracle: f64,
    pub token0: f64,
    pub token1: f64,
    /// token1 the pool pulled for the Treasury.
    pub treasury: f64,
    pub nonce: u64,
    /// Transactions included, and how many of those reverted.
    pub included: u64,
    pub reverted: u64,
    pub latency: Duration,
    step_sd: f64,
    flow_probability: f64,
    flow_size: f64,
    flow_reversion: f64,
    rng: SplitMix64,
}

impl MockChain {
    /// A pool of 1 000 000 token0 (and as much again in virtual reserves) at `mid`, an executor holding 20 000 token0
    /// and their value in token1, and an oracle on the pool. Each tick, an oracle step of σ√(0.1 s) with the model's
    /// σ², and a 30 % chance of a user batch: noise of 0.1 % of the pool's token0 (normally distributed, about 2000
    /// token0, which moves the price about 0.2 %), plus informed flow that trades 5 % of the way back to the oracle.
    /// Without the informed part the pool's deviation would be a random walk that no finite inventory can absorb; with
    /// it, the pool strays about 0.5 % from the oracle and the engine's inventory stays within ±30 % of its target.
    pub fn new(
        mid: f64,
        sigma2_per_sec: f64,
        seed: u64,
        chain_id: u64,
        executor: Address,
        operator: Address,
    ) -> Self {
        let r0 = 1_000_000.0;
        Self {
            chain_id,
            executor,
            operator,
            pool: PoolState {
                r0,
                r1: r0 * mid,
                v0: r0,
                v1: r0 * mid,
            },
            oracle: mid,
            token0: 20_000.0,
            token1: 20_000.0 * mid,
            treasury: 0.0,
            nonce: 0,
            included: 0,
            reverted: 0,
            latency: MOCK_LATENCY,
            step_sd: (sigma2_per_sec * crate::TICK.as_secs_f64()).sqrt(),
            flow_probability: 0.3,
            flow_size: 0.001,
            flow_reversion: 0.05,
            rng: SplitMix64(seed),
        }
    }

    fn advance(&mut self) {
        self.oracle = (self.oracle + self.step_sd * self.rng.normal()).max(1e-9);
        if self.rng.uniform() < self.flow_probability {
            // A user batch settles against the curve: dx token0 in (out if negative), K unchanged.
            let (x, y, k) = (self.pool.x(), self.pool.y(), self.pool.k());
            let x_at_oracle = (k / self.oracle).sqrt();
            let dx =
                self.flow_size * x * self.rng.normal() + self.flow_reversion * (x_at_oracle - x);
            let dy = k / (x + dx) - y;
            if self.pool.r0 + dx > 0.0 && self.pool.r1 + dy > 0.0 {
                self.pool.r0 += dx;
                self.pool.r1 += dy;
            }
        }
    }

    fn wei(tokens: f64) -> Result<U256, EngineError> {
        to_wei(tokens, Round::Down)
    }

    /// `SearcherExecutor.rebalance` then `DarkPool.flashRebalance`, as the C++ SimChain runs them: the executor must
    /// hold the repayment and the Treasury allowance, the pool's three acceptance rules must hold, and the pool pulls
    /// exactly the cut (80 % of the harvest at the oracle price), never the whole allowance.
    pub fn apply_rebalance(&mut self, r: &Rebalance) -> TxStatus {
        let amounts =
            [r.amount0_out, r.amount1_out, r.pay0, r.pay1, r.treasury_pay].map(wad_to_f64);
        let [Ok(o0), Ok(o1), Ok(p0), Ok(p1), Ok(tr)] = amounts else {
            return TxStatus::Reverted("amounts out of range".into());
        };
        if o0 == 0.0 && o1 == 0.0 {
            return TxStatus::Reverted("InsufficientInput: nothing out".into());
        }
        if o0 >= self.pool.r0 || o1 >= self.pool.r1 {
            return TxStatus::Reverted("InsufficientLiquidity".into());
        }
        if p0 > self.token0 + o0 || p1 + tr > self.token1 + o1 {
            return TxStatus::Reverted(
                "the executor cannot cover the repayment and the allowance".into(),
            );
        }
        let rules = chain_rules(&self.pool, self.oracle, o0, o1, p0, p1, tr);
        if !rules.k_ok {
            return TxStatus::Reverted("I1: K would decrease".into());
        }
        if !rules.deviation_ok {
            return TxStatus::Reverted("I3a: the pool does not move toward the oracle".into());
        }
        if !rules.treasury_ok {
            return TxStatus::Reverted("I3b: the Treasury allowance is below the cut".into());
        }
        self.pool.r0 += p0 - o0;
        self.pool.r1 += p1 - o1;
        self.token0 += o0 - p0;
        self.token1 += o1 - p1 - rules.cut;
        self.treasury += rules.cut;
        TxStatus::Success
    }
}

impl Chain for MockChain {
    async fn read(&mut self) -> Result<ChainState, EngineError> {
        time::sleep(self.latency).await;
        self.advance();
        Ok(ChainState {
            oracle: OracleReading {
                price: Self::wei(self.oracle)?,
                updated_at: unix_now(),
            },
            token0: Self::wei(self.token0)?,
            token1: Self::wei(self.token1)?,
            pool: PoolWei {
                r0: Self::wei(self.pool.r0)?,
                r1: Self::wei(self.pool.r1)?,
                v0: Self::wei(self.pool.v0)?,
                v1: Self::wei(self.pool.v1)?,
            },
            nonce: self.nonce,
        })
    }

    /// eth_sendRawTransaction on a chain that mines at once. A transaction that is not signed EIP-1559 on this chain at
    /// the account's nonce is rejected before inclusion; one that is included consumes the nonce, even if it reverts.
    async fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<Receipt, EngineError> {
        time::sleep(self.latency).await;
        let envelope = TxEnvelope::decode_2718(&mut &raw[..])
            .map_err(|e| EngineError::Feed(format!("undecodable transaction: {e}")))?;
        let TxEnvelope::Eip1559(signed) = envelope else {
            return Err(EngineError::Feed(
                "only EIP-1559 transactions are accepted".into(),
            ));
        };
        let tx = signed.tx();
        if tx.chain_id != self.chain_id {
            return Err(EngineError::Feed(format!(
                "chain id {} on chain {}",
                tx.chain_id, self.chain_id
            )));
        }
        let signer = signed
            .recover_signer()
            .map_err(|e| EngineError::Feed(format!("invalid signature: {e}")))?;
        if tx.nonce != self.nonce {
            return Err(EngineError::Feed(format!(
                "nonce {} but the account is at {}",
                tx.nonce, self.nonce
            )));
        }
        self.nonce += 1;
        self.included += 1;
        let status = if tx.to != TxKind::Call(self.executor) {
            TxStatus::Reverted("not a call to the SearcherExecutor".into())
        } else if !tx.value.is_zero() {
            TxStatus::Reverted("rebalance is not payable".into())
        } else if signer != self.operator {
            TxStatus::Reverted("NotOperator".into())
        } else {
            match Rebalance::decode(&tx.input) {
                Some(r) => self.apply_rebalance(&r),
                None => TxStatus::Reverted("unknown function".into()),
            }
        };
        if status != TxStatus::Success {
            self.reverted += 1;
        }
        Ok(Receipt {
            tx_hash: *signed.hash(),
            signer,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::planner::{PlannerParams, plan};
    use alloy::primitives::hex;

    fn chain(operator: Address) -> MockChain {
        MockChain::new(2.0, 2.5e-7, 9, 31_337, fixtures::EXECUTOR, operator)
    }

    /// Plans on the mock world for many ticks, rounds every plan to wei and executes it: every rebalance passes the
    /// contract's rules, moves the pool toward the oracle without crossing it, and leaves the inventory non-negative.
    #[test]
    fn every_rounded_plan_executes_on_the_mock_chain() {
        let mut c = chain(Address::ZERO);
        let mut executed = 0;
        for _ in 0..5_000 {
            c.advance();
            let (p, spot_before, k_before) = (c.oracle, c.pool.spot(), c.pool.k());
            let half = 0.002;
            let pp = PlannerParams {
                gas_cost_quote: 0.01,
                min_net: 0.01,
                max_inv0: crate::tx::spendable(c.token0),
                max_inv1: crate::tx::spendable(c.token1),
                ..PlannerParams::default()
            };
            let Some(g) = plan(&c.pool, p, p - half, p + half, &pp) else {
                continue;
            };
            let r = Rebalance::from_plan(&g).unwrap();
            assert_eq!(c.apply_rebalance(&r), TxStatus::Success, "{g:?}");
            executed += 1;
            assert!(c.pool.k() >= k_before * (1.0 - 1e-12));
            assert!((c.pool.spot() - p).abs() < (spot_before - p).abs());
            assert!(
                (spot_before - p) * (c.pool.spot() - p) >= 0.0,
                "pushed past the oracle"
            );
            assert!(c.token0 >= 0.0 && c.token1 >= 0.0);
        }
        assert!(
            executed > 100,
            "the mock world must create opportunities ({executed})"
        );
        assert!(c.treasury > 0.0);
    }

    /// The C++ self-test's scenario: pool at 2000, oracle at 1985.
    #[test]
    fn the_cpp_scenario_moves_the_pool_toward_the_oracle_and_pays_the_treasury() {
        let mut c = fixtures::scenario_chain();
        let r = fixtures::scenario_rebalance();
        assert_eq!(c.apply_rebalance(&r), TxStatus::Success);
        assert!(c.pool.spot() < 2000.0 && c.pool.spot() >= 1985.0 * (1.0 - 1e-6));
        let harvest = c.treasury / 0.8;
        assert!(c.treasury > 0.0 && wad_to_f64(r.treasury_pay).unwrap() >= c.treasury);
        assert!(harvest > 0.0);
    }

    #[test]
    fn rebalances_the_contract_would_refuse_revert() {
        let mut c = fixtures::scenario_chain();
        let r = fixtures::scenario_rebalance();
        let none = Rebalance {
            amount0_out: U256::ZERO,
            amount1_out: U256::ZERO,
            ..r
        };
        assert!(
            matches!(c.apply_rebalance(&none), TxStatus::Reverted(m) if m.contains("InsufficientInput"))
        );
        let greedy = Rebalance {
            amount1_out: r.amount1_out * U256::from(2u8),
            ..r
        };
        assert!(matches!(c.apply_rebalance(&greedy), TxStatus::Reverted(m) if m.contains("I1")));
        let stingy = Rebalance {
            treasury_pay: U256::ZERO,
            ..r
        };
        assert!(matches!(c.apply_rebalance(&stingy), TxStatus::Reverted(m) if m.contains("I3b")));
        let poor = Rebalance {
            pay0: crate::model::wad("1000000"),
            ..r
        };
        assert!(
            matches!(c.apply_rebalance(&poor), TxStatus::Reverted(m) if m.contains("cannot cover"))
        );
    }

    /// A transaction signed by the vault (fixtures::SIGNED_REBALANCE) is accepted from the operator, consumes the nonce
    /// and executes; the same transaction is a NotOperator revert when the executor's operator is someone else.
    #[tokio::test]
    async fn a_vault_signed_rebalance_executes_only_from_the_operator() {
        let raw = hex::decode(fixtures::SIGNED_REBALANCE).unwrap();
        let mut c = fixtures::scenario_chain();
        let receipt = c.send_raw_transaction(&raw).await.unwrap();
        assert_eq!(
            (receipt.signer, receipt.status.clone()),
            (fixtures::VAULT_ADDRESS, TxStatus::Success)
        );
        assert_eq!(receipt.tx_hash, alloy::primitives::keccak256(&raw));
        assert_eq!((c.nonce, c.included, c.reverted), (1, 1, 0));
        // Replaying it: the nonce is spent.
        assert!(
            c.send_raw_transaction(&raw)
                .await
                .unwrap_err()
                .to_string()
                .contains("nonce 0 but the account is at 1")
        );

        let mut other = fixtures::scenario_chain();
        other.operator = Address::repeat_byte(0x11);
        let receipt = other.send_raw_transaction(&raw).await.unwrap();
        assert_eq!(receipt.status, TxStatus::Reverted("NotOperator".into()));
        assert_eq!((other.nonce, other.reverted), (1, 1));
    }

    #[tokio::test]
    async fn unsigned_tampered_or_foreign_transactions_are_rejected() {
        let raw = hex::decode(fixtures::SIGNED_REBALANCE).unwrap();
        let mut c = fixtures::scenario_chain();
        // A flipped byte in the calldata: the signature recovers to some other address (or none at all).
        let mut tampered = raw.clone();
        let at = tampered.len() - 80;
        tampered[at] ^= 0x01;
        match c.send_raw_transaction(&tampered).await {
            Ok(receipt) => assert_ne!(receipt.signer, fixtures::VAULT_ADDRESS),
            Err(e) => assert!(
                e.to_string().contains("invalid signature")
                    || e.to_string().contains("undecodable")
            ),
        }
        // The unsigned payload is not a transaction.
        let mut c = fixtures::scenario_chain();
        let unsigned = crate::tx::signing_payload(&fixtures::scenario_tx());
        assert!(c.send_raw_transaction(&unsigned).await.is_err());
        // Another chain.
        let mut c = fixtures::scenario_chain();
        c.chain_id = 1;
        assert!(
            c.send_raw_transaction(&raw)
                .await
                .unwrap_err()
                .to_string()
                .contains("chain id")
        );
    }
}
