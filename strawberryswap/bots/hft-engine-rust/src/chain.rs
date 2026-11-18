//! The engine's view of the chain (reads, simulation and broadcast) and a deterministic mock of it.
//!
//! Live, `live::LiveChain` implements it over JSON-RPC: one batched read per tick (the EngineLens snapshot, the
//! operator's nonces and gas balance, and what the node knows of the transaction in flight), an `eth_call` simulation
//! of each rebalance from the vault's address, and `eth_sendRawTransaction`.
//!
//! The mock is the C++ engine's `SimChain` (bots/inventory-cpp/include/strawberry/sim.hpp) behind a transaction
//! boundary. It accepts only signed EIP-1559 transactions, recovers the sender from the signature, consumes the nonce,
//! and executes `SearcherExecutor.rebalance` with the contract's checks: the sender must be the executor's operator,
//! and the rebalance must pass DarkPool's acceptance rules (`planner::chain_rules`). It mines a transaction as it is
//! broadcast; the engine learns the outcome from its receipt on the next read, as it does live.

use std::collections::HashMap;
use std::time::Duration;

use alloy::consensus::{TxEip1559, TxEnvelope};
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

/// The block a tick's snapshot was read at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockInfo {
    pub number: u64,
    pub timestamp: u64,
    /// EIP-1559 base fee, wei per gas.
    pub base_fee: u128,
}

/// What the node knows of the transaction the engine asked about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Watched {
    /// No transaction was asked about.
    #[default]
    Nothing,
    /// The node does not know it: never received, or dropped.
    Unknown,
    /// Received, not mined yet.
    Pending,
    Mined(Receipt),
}

/// One tick's reads, as the contracts and the node return them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChainState {
    /// IZKOracle.latestVerifiedPrice(), read through the pool as DarkPool reads it.
    pub oracle: OracleReading,
    /// The SearcherExecutor's balances (IERC20.balanceOf), wei.
    pub token0: U256,
    pub token1: U256,
    pub pool: PoolWei,
    pub block: BlockInfo,
    /// The operator's mined transactions (eth_getTransactionCount at `latest`): its next nonce when nothing is pending.
    pub nonce: u64,
    /// Mined or pending (at `pending`). Above `nonce` while a transaction from the operator waits to be mined.
    pub pending_nonce: u64,
    /// The operator's ether, wei: what pays for the rebalances' gas.
    pub gas_balance: U256,
    pub watched: Watched,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TxStatus {
    Success,
    Reverted(String),
}

/// A mined transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub tx_hash: B256,
    /// The sender (recovered from the signature).
    pub from: Address,
    pub status: TxStatus,
    pub block: u64,
    pub gas_used: u64,
}

/// Reads, simulation and broadcast: the live chain over JSON-RPC (`live::LiveChain`), or the mock below.
pub(crate) trait Chain {
    /// One tick's reads, in one round trip. `watch` asks what the node knows of that transaction as well.
    async fn read(&mut self, watch: Option<B256>) -> Result<ChainState, EngineError>;
    /// The transaction as an `eth_call` from `from` on the latest block: Ok if it would execute now, else why not
    /// (`EngineError::Simulation`).
    async fn simulate(&mut self, from: Address, tx: &TxEip1559) -> Result<(), EngineError>;
    /// eth_sendRawTransaction: the hash, once the node has accepted the transaction.
    async fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<B256, EngineError>;
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
/// The mock's base fee: 1 gwei.
pub(crate) const MOCK_BASE_FEE: u128 = 1_000_000_000;
/// The gas a mock rebalance uses.
const MOCK_GAS_USED: u64 = 150_000;

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
    /// The receipts of the transactions included, by hash.
    pub receipts: HashMap<B256, Receipt>,
    /// The base fee every block reports, wei per gas.
    pub base_fee: u128,
    /// Blocks so far: one per read.
    pub block: u64,
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
            receipts: HashMap::new(),
            base_fee: MOCK_BASE_FEE,
            block: 0,
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
    /// exactly the cut (80 % of the harvest at the oracle price), never the whole allowance. Nothing changes; Ok holds
    /// the state after it.
    fn evaluate(&self, r: &Rebalance) -> Result<Applied, String> {
        let amounts =
            [r.amount0_out, r.amount1_out, r.pay0, r.pay1, r.treasury_pay].map(wad_to_f64);
        let [Ok(o0), Ok(o1), Ok(p0), Ok(p1), Ok(tr)] = amounts else {
            return Err("amounts out of range".into());
        };
        if o0 == 0.0 && o1 == 0.0 {
            return Err("InsufficientInput: nothing out".into());
        }
        if o0 >= self.pool.r0 || o1 >= self.pool.r1 {
            return Err("InsufficientLiquidity".into());
        }
        if p0 > self.token0 + o0 || p1 + tr > self.token1 + o1 {
            return Err("the executor cannot cover the repayment and the allowance".into());
        }
        let rules = chain_rules(&self.pool, self.oracle, o0, o1, p0, p1, tr);
        if !rules.k_ok {
            return Err("I1: K would decrease".into());
        }
        if !rules.deviation_ok {
            return Err("I3a: the pool does not move toward the oracle".into());
        }
        if !rules.treasury_ok {
            return Err("I3b: the Treasury allowance is below the cut".into());
        }
        let mut pool = self.pool;
        pool.r0 += p0 - o0;
        pool.r1 += p1 - o1;
        Ok(Applied {
            pool,
            token0: self.token0 + o0 - p0,
            token1: self.token1 + o1 - p1 - rules.cut,
            cut: rules.cut,
        })
    }

    /// Executes the rebalance (as `evaluate`, then commits it).
    #[cfg(test)]
    pub fn apply_rebalance(&mut self, r: &Rebalance) -> TxStatus {
        match self.evaluate(r) {
            Ok(applied) => {
                self.pool = applied.pool;
                self.token0 = applied.token0;
                self.token1 = applied.token1;
                self.treasury += applied.cut;
                TxStatus::Success
            }
            Err(reason) => TxStatus::Reverted(reason),
        }
    }

    /// What a call from `from` would do: the executor's checks, then the rebalance's.
    fn check_call(&self, from: Address, tx: &TxEip1559) -> Result<Applied, String> {
        if tx.to != TxKind::Call(self.executor) {
            return Err("not a call to the SearcherExecutor".into());
        }
        if !tx.value.is_zero() {
            return Err("rebalance is not payable".into());
        }
        if from != self.operator {
            return Err("NotOperator".into());
        }
        let r = Rebalance::decode(&tx.input).ok_or("unknown function")?;
        self.evaluate(&r)
    }
}

/// The state after a rebalance.
struct Applied {
    pool: PoolState,
    token0: f64,
    token1: f64,
    cut: f64,
}

impl Chain for MockChain {
    async fn read(&mut self, watch: Option<B256>) -> Result<ChainState, EngineError> {
        time::sleep(self.latency).await;
        self.advance();
        self.block += 1;
        let watched = match watch {
            None => Watched::Nothing,
            // The mock mines as it accepts, so a transaction it knows is mined.
            Some(hash) => self
                .receipts
                .get(&hash)
                .map_or(Watched::Unknown, |receipt| Watched::Mined(receipt.clone())),
        };
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
            block: BlockInfo {
                number: self.block,
                timestamp: unix_now(),
                base_fee: self.base_fee,
            },
            nonce: self.nonce,
            pending_nonce: self.nonce,
            gas_balance: U256::from(10u64).pow(U256::from(20u64)),
            watched,
        })
    }

    async fn simulate(&mut self, from: Address, tx: &TxEip1559) -> Result<(), EngineError> {
        time::sleep(self.latency).await;
        self.check_call(from, tx)
            .map(|_| ())
            .map_err(EngineError::Simulation)
    }

    /// eth_sendRawTransaction on a chain that mines at once. A transaction that is not signed EIP-1559 on this chain at
    /// the account's nonce is rejected before inclusion; one that is included consumes the nonce, even if it reverts.
    async fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<B256, EngineError> {
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
        let status = match self.check_call(signer, tx) {
            Ok(applied) => {
                self.pool = applied.pool;
                self.token0 = applied.token0;
                self.token1 = applied.token1;
                self.treasury += applied.cut;
                TxStatus::Success
            }
            Err(reason) => {
                self.reverted += 1;
                TxStatus::Reverted(reason)
            }
        };
        let hash = *signed.hash();
        self.receipts.insert(
            hash,
            Receipt {
                tx_hash: hash,
                from: signer,
                status,
                block: self.block,
                gas_used: MOCK_GAS_USED,
            },
        );
        Ok(hash)
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
        let hash = c.send_raw_transaction(&raw).await.unwrap();
        assert_eq!(hash, alloy::primitives::keccak256(&raw));
        let receipt = c.receipts[&hash].clone();
        assert_eq!(
            (receipt.from, receipt.status.clone()),
            (fixtures::VAULT_ADDRESS, TxStatus::Success)
        );
        // The next read reports it mined.
        let reads = c.read(Some(hash)).await.unwrap();
        assert_eq!(reads.watched, Watched::Mined(receipt));
        assert_eq!(
            c.read(Some(B256::repeat_byte(1))).await.unwrap().watched,
            Watched::Unknown
        );
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
        let hash = other.send_raw_transaction(&raw).await.unwrap();
        assert_eq!(
            other.receipts[&hash].status,
            TxStatus::Reverted("NotOperator".into())
        );
        assert_eq!((other.nonce, other.reverted), (1, 1));
    }

    /// The simulation runs the executor's and the pool's checks without changing anything.
    #[tokio::test]
    async fn the_simulation_reports_what_the_transaction_would_do_and_changes_nothing() {
        let mut c = fixtures::scenario_chain();
        let tx = fixtures::scenario_tx();
        c.simulate(fixtures::VAULT_ADDRESS, &tx).await.unwrap();
        assert_eq!(
            (c.pool, c.treasury, c.nonce),
            (fixtures::SCENARIO_POOL, 0.0, 0)
        );
        let refused = |e: EngineError| match e {
            EngineError::Simulation(reason) => reason,
            other => panic!("{other:?}"),
        };
        let not_operator = c
            .simulate(Address::repeat_byte(0x11), &tx)
            .await
            .unwrap_err();
        assert_eq!(refused(not_operator), "NotOperator");
        // Beaten: someone else moved the pool to the oracle first, so the same rebalance no longer reduces the
        // deviation.
        c.pool = PoolState {
            r1: fixtures::SCENARIO_POOL.r1 * fixtures::SCENARIO_ORACLE / 2000.0,
            v1: fixtures::SCENARIO_POOL.v1 * fixtures::SCENARIO_ORACLE / 2000.0,
            ..fixtures::SCENARIO_POOL
        };
        let beaten = refused(c.simulate(fixtures::VAULT_ADDRESS, &tx).await.unwrap_err());
        assert!(beaten.starts_with("I"), "{beaten}");
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
            Ok(hash) => assert_ne!(c.receipts[&hash].from, fixtures::VAULT_ADDRESS),
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
