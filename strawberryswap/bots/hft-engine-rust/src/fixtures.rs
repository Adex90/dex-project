//! Test fixtures: the C++ engine's end-to-end scenario, and the vault's signature of its rebalance.
//!
//! The scenario is bots/inventory-cpp's `testEndToEnd`: a pool of 1000 token0 (and 1000 virtual) at 2000 token1 each,
//! a zk oracle at 1985 (the pool is too expensive), an executor holding 50 token0 and 100 000 token1, bid and ask at
//! 1985 ∓ 0.1 and a gas cost of 0.5 token1. `SIGNED_REBALANCE` is that rebalance at nonce 0, as the real vault signed
//! it (strawberryswap/vaults/threshold-signer, built with `insecure-test-key` and given `test_key()`).
//!
//! The key is the end-to-end tests' INSECURE test key: keccak256 of a public phrase, so that no key is stored anywhere
//! and anyone can recompute it. It signs nothing outside the tests.

use alloy::consensus::TxEip1559;
use alloy::primitives::{Address, B256, U256, address, keccak256};

use crate::chain::{ChainState, MockChain, PoolWei};
use crate::model::{OracleReading, wad};
use crate::planner::{PlannerParams, PoolState};
use crate::tx::{Rebalance, TxConfig, unsigned_tx};

/// The mock SearcherExecutor (Anvil's first deployment address).
pub(crate) const EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");

/// The phrase whose keccak256 is the end-to-end tests' key (tests/vault_e2e.rs and the vault's own test use it too).
pub(crate) const TEST_KEY_PHRASE: &str = "strawberryswap insecure test key: end-to-end tests only";

/// The address of `test_key()`, computed independently with viem (`privateKeyToAccount`).
pub(crate) const VAULT_ADDRESS: Address = address!("0xf8e44d4517A0de6610113165659DaAd8cF9327B2");

/// INSECURE: the end-to-end tests' key, recomputed from its public phrase.
pub(crate) fn test_key() -> B256 {
    keccak256(TEST_KEY_PHRASE)
}

/// The transaction template of the engine's defaults.
pub(crate) const CONFIG: TxConfig = TxConfig {
    chain_id: 31_337,
    executor: EXECUTOR,
    gas_limit: 300_000,
    max_fee_per_gas: 30_000_000_000,
    max_priority_fee_per_gas: 1_000_000_000,
};

/// The scenario's pool, in tokens.
pub(crate) const SCENARIO_POOL: PoolState = PoolState {
    r0: 1000.0,
    r1: 2e6,
    v0: 1000.0,
    v1: 2e6,
};
pub(crate) const SCENARIO_ORACLE: f64 = 1985.0;
pub(crate) const SCENARIO_TOKEN0: f64 = 50.0;
pub(crate) const SCENARIO_TOKEN1: f64 = 100_000.0;

/// The planner's limits in the scenario.
pub(crate) fn scenario_planner() -> PlannerParams {
    PlannerParams {
        gas_cost_quote: 0.5,
        min_net: 0.0,
        max_inv0: SCENARIO_TOKEN0,
        max_inv1: SCENARIO_TOKEN1,
        ..PlannerParams::default()
    }
}

/// The scenario as the mock chain, with the vault as the executor's operator.
pub(crate) fn scenario_chain() -> MockChain {
    let mut chain = MockChain::new(
        SCENARIO_ORACLE,
        0.0,
        1,
        CONFIG.chain_id,
        EXECUTOR,
        VAULT_ADDRESS,
    );
    chain.pool = SCENARIO_POOL;
    chain.token0 = SCENARIO_TOKEN0;
    chain.token1 = SCENARIO_TOKEN1;
    chain
}

/// The scenario as one tick's reads, the oracle stamped `updated_at`.
pub(crate) fn scenario_reads(updated_at: u64) -> ChainState {
    ChainState {
        oracle: OracleReading {
            price: wad("1985"),
            updated_at,
        },
        token0: wad("50"),
        token1: wad("100000"),
        pool: PoolWei {
            r0: wad("1000"),
            r1: wad("2000000"),
            v0: wad("1000"),
            v1: wad("2000000"),
        },
        nonce: 0,
    }
}

/// The scenario's rebalance in wei: the planner's plan, rounded by `Rebalance::from_plan` (pinned by a test below).
pub(crate) fn scenario_rebalance() -> Rebalance {
    let w = |digits: &str| U256::from_str_radix(digits, 10).expect("a decimal literal");
    Rebalance {
        amount0_out: w("0"),
        amount1_out: w("14526376953423954546451"),
        pay0: w("7289661576957604084"),
        pay1: w("0"),
        treasury_pay: w("45141549601524992852"),
    }
}

/// The scenario's unsigned transaction, nonce 0.
pub(crate) fn scenario_tx() -> TxEip1559 {
    unsigned_tx(&CONFIG, 0, &scenario_rebalance())
}

/// `scenario_tx()` as the vault signed it: EIP-2718 bytes, hex. Transaction hash
/// 0x677f9f29e1823635c3a720201ce528fa4678a6cab65c6f601f516d92334bbc10; viem's `recoverTransactionAddress` gives
/// VAULT_ADDRESS.
pub(crate) const SIGNED_REBALANCE: &str = concat!(
    "02f90113827a6980843b9aca008506fc23ac00830493e0945fbdb2315678afecb367f032d93f642f64180aa380b8a4c066173700",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00031379fa1b186fe58f13000000000000000000000000000000000000000000000000652a1494fb5f30f4000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000027277003141",
    "92cb54c080a032c348899bf8b498e4e9de78c9a7d649464fc1943299e67aa527c3468939f25fa07332dd87f7ca00a65555ccf9e4",
    "83185ae6fbe5c12d901c007d30b940cb62abfe",
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::plan;
    use alloy::consensus::{SignableTransaction, TxEnvelope};
    use alloy::eips::eip2718::{Decodable2718, Encodable2718};
    use alloy::primitives::{Signature, hex};

    /// Prints what the vault must sign (run with --ignored --nocapture to regenerate the fixture).
    #[test]
    #[ignore]
    fn print_the_fixture() {
        let p = plan(
            &SCENARIO_POOL,
            SCENARIO_ORACLE,
            SCENARIO_ORACLE - 0.1,
            SCENARIO_ORACLE + 0.1,
            &scenario_planner(),
        )
        .unwrap();
        let r = Rebalance::from_plan(&p).unwrap();
        println!("plan {p:?}");
        println!(
            "amounts {} {} {} {} {}",
            r.amount0_out, r.amount1_out, r.pay0, r.pay1, r.treasury_pay
        );
        let tx = unsigned_tx(&CONFIG, 0, &r);
        println!(
            "payload {}",
            hex::encode_prefixed(crate::tx::signing_payload(&tx))
        );
    }

    #[test]
    fn the_fixture_is_the_planners_rebalance_for_the_cpp_scenario() {
        let p = plan(
            &SCENARIO_POOL,
            SCENARIO_ORACLE,
            SCENARIO_ORACLE - 0.1,
            SCENARIO_ORACLE + 0.1,
            &scenario_planner(),
        )
        .expect("the C++ scenario has an opportunity");
        assert!(p.sells_token0);
        assert_eq!(Rebalance::from_plan(&p).unwrap(), scenario_rebalance());
    }

    /// The vault's signature is the one RFC 6979 gives for this key and transaction: the fixture decodes to exactly
    /// the scenario transaction, recovers to VAULT_ADDRESS, and k256 signing in process produces the same bytes.
    #[test]
    fn the_fixture_is_the_vaults_signature_of_the_scenario_transaction() {
        let raw = hex::decode(SIGNED_REBALANCE).unwrap();
        let TxEnvelope::Eip1559(signed) = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap()
        else {
            panic!("not an EIP-1559 transaction");
        };
        assert_eq!(signed.tx(), &scenario_tx());
        assert_eq!(signed.recover_signer().unwrap(), VAULT_ADDRESS);

        let key = k256::ecdsa::SigningKey::from_slice(test_key().as_slice()).unwrap();
        let (sig, recovery) = key
            .sign_prehash_recoverable(scenario_tx().signature_hash().as_slice())
            .unwrap();
        let again = scenario_tx().into_signed(Signature::from_signature_and_parity(
            sig,
            recovery.is_y_odd(),
        ));
        assert_eq!(again.encoded_2718(), raw);
    }
}
