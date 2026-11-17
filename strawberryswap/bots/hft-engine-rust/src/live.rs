//! The live chain, over JSON-RPC (Phase 3).
//!
//! Each tick is **one batched RPC request**:
//!
//! 1. `eth_call` of EngineLens's creation code (contracts/src/periphery/EngineLens.sol): the oracle reading (through
//!    the pool, as DarkPool reads it), the pool's reserves and virtual reserves, the executor's balances and the
//!    block's number, timestamp and base fee, all from **one execution at one block**;
//! 2. the operator's nonce at `latest` and at `pending`;
//! 3. the operator's ether balance, which pays for the gas;
//! 4. while a transaction is in flight, its receipt and whether the node knows it.
//!
//! A rebalance is simulated (`eth_call` from the vault's address) before the vault is asked to sign it, and broadcast
//! with `eth_sendRawTransaction`. At start, `connect` checks the chain id and the contracts' wiring: the vault's
//! address must be the SearcherExecutor's operator, or every rebalance would revert `NotOperator`.
//!
//! The RPC URL may carry an API key: it is never printed, and every error is redacted of it.

use alloy::consensus::TxEip1559;
use alloy::primitives::{Address, B256, Bytes, U64, U256, hex};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::ErrorPayload;
use alloy::sol_types::{Panic, Revert, SolValue};
use alloy::transports::TransportError;
use serde_json::{Value, json};

use crate::chain::{BlockInfo, Chain, ChainState, PoolWei, Receipt, TxStatus, Watched};
use crate::model::{EngineError, OracleReading};

/// EngineLens's creation code, compiled with contracts/foundry.toml's settings (solc 0.8.26, via-IR, 200 runs, paris,
/// no metadata hash). CI checks this pin against `forge build`.
const LENS_INITCODE_HEX: &str = include_str!("lens.hex");

alloy::sol! {
    /// EngineLens's snapshot: eleven words.
    #[derive(Debug, PartialEq, Eq)]
    struct Snapshot {
        uint256 blockNumber;
        uint256 timestamp;
        uint256 baseFee;
        uint256 price;
        uint256 updatedAt;
        uint256 reserve0;
        uint256 reserve1;
        uint256 virtual0;
        uint256 virtual1;
        uint256 balance0;
        uint256 balance1;
    }

    #[sol(rpc)]
    #[derive(Debug)]
    interface ISearcherExecutor {
        function operator() external view returns (address);
        function pool() external view returns (address);
        function token0() external view returns (address);
        function token1() external view returns (address);

        error NotOperator();
        error NotPool();
        error NotInitiator();
        error NoActiveRebalance();
        error ZeroAddress();
    }

    #[sol(rpc)]
    #[derive(Debug)]
    interface IDarkPool {
        function oracle() external view returns (address);
        function token0() external view returns (address);
        function token1() external view returns (address);

        error InvariantBroken();
        error ReserveOverflow();
        error InsufficientLiquidity();
        error InsufficientInput();
        error StaleOracle();
        error BadPrice();
        error DeviationNotReduced();
        error ZeroAddress();
    }
}

/// The lens's creation code with its constructor arguments: what each tick's first `eth_call` sends.
fn lens_input(pool: Address, executor: Address) -> Bytes {
    let mut input = hex::decode(LENS_INITCODE_HEX.trim()).expect("src/lens.hex is hex");
    input.extend_from_slice(&(pool, executor).abi_encode_params());
    input.into()
}

/// Why a reverting call reverted: a DarkPool or SearcherExecutor custom error, a Solidity panic or reason string, or the
/// node's message.
fn revert_reason(payload: &ErrorPayload) -> String {
    if let Some(e) = payload.as_decoded_interface_error::<IDarkPool::IDarkPoolErrors>() {
        return format!("DarkPool: {e:?}");
    }
    if let Some(e) =
        payload.as_decoded_interface_error::<ISearcherExecutor::ISearcherExecutorErrors>()
    {
        return format!("SearcherExecutor: {e:?}");
    }
    if let Some(Revert { reason }) = payload.as_decoded_error::<Revert>() {
        return format!("reverted: {reason}");
    }
    if let Some(panic) = payload.as_decoded_error::<Panic>() {
        return format!("panic {:#x}", panic.code);
    }
    payload.message.to_string()
}

/// Whether an RPC error says the call reverted (as opposed to a node or transport failure).
fn is_revert(payload: &ErrorPayload) -> bool {
    payload.code == 3 || payload.message.contains("revert") || payload.as_revert_data().is_some()
}

/// The parts of an RPC URL that may hold a credential: the whole URL, its path, its query and its user info. Longest
/// first, so that a part never hides inside a longer one already replaced.
pub(crate) fn url_secrets(given: &str) -> Vec<String> {
    let mut secrets = vec![given.to_string()];
    if let Ok(url) = given.parse::<alloy::transports::http::reqwest::Url>() {
        secrets.push(url.as_str().to_string());
        if url.path().len() > 1 {
            secrets.push(url.path().to_string());
        }
        secrets.extend(url.query().map(str::to_string));
        // A key may show on its own: each query value, and each long path segment (`/v2/<key>`).
        secrets.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
        secrets.extend(
            url.path_segments()
                .into_iter()
                .flatten()
                .filter(|segment| segment.len() >= 8)
                .map(str::to_string),
        );
        secrets.extend(url.password().map(str::to_string));
        if !url.username().is_empty() {
            secrets.push(url.username().to_string());
        }
    }
    secrets.retain(|s| s.len() > 1);
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    secrets
}

pub(crate) fn redact(text: String, secrets: &[String]) -> String {
    secrets.iter().fold(text, |text, secret| {
        text.replace(secret.as_str(), "<redacted>")
    })
}

/// A `0x` quantity in a JSON-RPC object.
fn quantity(v: &Value, field: &str) -> Result<u64, EngineError> {
    v[field]
        .as_str()
        .and_then(|q| u64::from_str_radix(q.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| EngineError::Feed(format!("receipt without a valid {field}")))
}

fn parse_receipt(v: &Value) -> Result<Receipt, EngineError> {
    let tx_hash = v["transactionHash"]
        .as_str()
        .and_then(|h| h.parse().ok())
        .ok_or_else(|| EngineError::Feed("receipt without a transaction hash".into()))?;
    let from = v["from"]
        .as_str()
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| EngineError::Feed("receipt without a sender".into()))?;
    let status = match quantity(v, "status")? {
        1 => TxStatus::Success,
        _ => TxStatus::Reverted("reverted on chain (receipt status 0)".into()),
    };
    Ok(Receipt {
        tx_hash,
        from,
        status,
        block: quantity(v, "blockNumber")?,
        gas_used: quantity(v, "gasUsed")?,
    })
}

/// The live chain.
pub(crate) struct LiveChain {
    client: RpcClient,
    lens: Bytes,
    operator: Address,
    executor: Address,
    secrets: Vec<String>,
}

impl LiveChain {
    pub fn new(
        client: RpcClient,
        pool: Address,
        executor: Address,
        operator: Address,
        secrets: Vec<String>,
    ) -> Self {
        Self {
            client,
            lens: lens_input(pool, executor),
            operator,
            executor,
            secrets,
        }
    }

    fn rpc_error(&self, e: impl std::fmt::Display) -> EngineError {
        EngineError::Feed(redact(e.to_string(), &self.secrets))
    }

    fn snapshot_to_state(
        &self,
        snapshot: &Snapshot,
        nonce: u64,
        pending_nonce: u64,
        gas_balance: U256,
        watched: Watched,
    ) -> Result<ChainState, EngineError> {
        let small = |v: U256, what: &str| {
            u64::try_from(v).map_err(|_| EngineError::Conversion(format!("{what} out of range")))
        };
        Ok(ChainState {
            oracle: OracleReading {
                price: snapshot.price,
                updated_at: small(snapshot.updatedAt, "the oracle's timestamp")?,
            },
            token0: snapshot.balance0,
            token1: snapshot.balance1,
            pool: PoolWei {
                r0: snapshot.reserve0,
                r1: snapshot.reserve1,
                v0: snapshot.virtual0,
                v1: snapshot.virtual1,
            },
            block: BlockInfo {
                number: small(snapshot.blockNumber, "the block number")?,
                timestamp: small(snapshot.timestamp, "the block timestamp")?,
                base_fee: u128::try_from(snapshot.baseFee)
                    .map_err(|_| EngineError::Conversion("the base fee is out of range".into()))?,
            },
            nonce,
            pending_nonce,
            gas_balance,
            watched,
        })
    }
}

impl Chain for LiveChain {
    async fn read(&mut self, watch: Option<B256>) -> Result<ChainState, EngineError> {
        let mut batch = self.client.new_batch();
        let lens = batch
            .add_call::<_, Bytes>("eth_call", &(json!({ "input": self.lens }), "latest"))
            .map_err(|e| self.rpc_error(e))?;
        let nonce = batch
            .add_call::<_, U64>("eth_getTransactionCount", &(self.operator, "latest"))
            .map_err(|e| self.rpc_error(e))?;
        let pending = batch
            .add_call::<_, U64>("eth_getTransactionCount", &(self.operator, "pending"))
            .map_err(|e| self.rpc_error(e))?;
        let balance = batch
            .add_call::<_, U256>("eth_getBalance", &(self.operator, "latest"))
            .map_err(|e| self.rpc_error(e))?;
        let watched = match watch {
            None => None,
            Some(hash) => Some((
                batch
                    .add_call::<_, Option<Value>>("eth_getTransactionReceipt", &(hash,))
                    .map_err(|e| self.rpc_error(e))?,
                batch
                    .add_call::<_, Option<Value>>("eth_getTransactionByHash", &(hash,))
                    .map_err(|e| self.rpc_error(e))?,
            )),
        };
        batch.send().await.map_err(|e| self.rpc_error(e))?;

        let raw = lens.await.map_err(|e| self.rpc_error(e))?;
        let snapshot = Snapshot::abi_decode(&raw).map_err(|e| {
            EngineError::Feed(format!("the lens returned {} bytes: {e}", raw.len()))
        })?;
        let nonce = nonce.await.map_err(|e| self.rpc_error(e))?.to::<u64>();
        let pending_nonce = pending.await.map_err(|e| self.rpc_error(e))?.to::<u64>();
        let gas_balance = balance.await.map_err(|e| self.rpc_error(e))?;
        let watched = match watched {
            None => Watched::Nothing,
            Some((receipt, tx)) => {
                let receipt = receipt.await.map_err(|e| self.rpc_error(e))?;
                let tx = tx.await.map_err(|e| self.rpc_error(e))?;
                match (receipt, tx) {
                    (Some(receipt), _) => Watched::Mined(parse_receipt(&receipt)?),
                    (None, Some(_)) => Watched::Pending,
                    (None, None) => Watched::Unknown,
                }
            }
        };
        self.snapshot_to_state(&snapshot, nonce, pending_nonce, gas_balance, watched)
    }

    async fn simulate(&mut self, from: Address, tx: &TxEip1559) -> Result<(), EngineError> {
        let call = json!({
            "from": from,
            "to": self.executor,
            "input": tx.input,
            "gas": format!("{:#x}", tx.gas_limit),
            "value": "0x0",
        });
        match self
            .client
            .request::<_, Bytes>("eth_call", (call, "latest"))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => match e.as_error_resp() {
                Some(payload) if is_revert(payload) => Err(EngineError::Simulation(redact(
                    revert_reason(payload),
                    &self.secrets,
                ))),
                _ => Err(self.rpc_error(e)),
            },
        }
    }

    async fn send_raw_transaction(&mut self, raw: &[u8]) -> Result<B256, EngineError> {
        self.client
            .request::<_, B256>("eth_sendRawTransaction", (hex::encode_prefixed(raw),))
            .await
            .map_err(|e| self.rpc_error(e))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------------------------------------------------------

/// The deployment the engine trades on, as read and checked at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Deployment {
    pub chain_id: u64,
    pub executor: Address,
    /// The SearcherExecutor's operator: the vault's address.
    pub operator: Address,
    pub pool: Address,
    /// DarkPool's oracle: the ZkPriceFeed.
    pub feed: Address,
    pub token0: Address,
    pub token1: Address,
}

fn startup_error(e: impl std::fmt::Display, secrets: &[String]) -> String {
    redact(e.to_string(), secrets)
}

/// Reads the deployment and checks that the engine can trade on it: the chain id, a contract at the executor, the vault
/// as its operator, the executor and the pool on the same tokens, and (if given) the feed the pool reads.
pub(crate) async fn check_deployment(
    client: &RpcClient,
    chain_id: u64,
    executor: Address,
    vault: Address,
    feed: Option<Address>,
    secrets: &[String],
) -> Result<Deployment, String> {
    let provider: DynProvider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .connect_client(client.clone())
        .erased();
    let err = |e: TransportError| startup_error(e, secrets);
    let actual = provider.get_chain_id().await.map_err(err)?;
    if actual != chain_id {
        return Err(format!(
            "the RPC is on chain {actual}, the engine was started for chain {chain_id}"
        ));
    }
    if provider
        .get_code_at(executor)
        .await
        .map_err(err)?
        .is_empty()
    {
        return Err(format!("no contract at the executor address {executor}"));
    }
    let call_err = |e: alloy::contract::Error| startup_error(e, secrets);
    let exec = ISearcherExecutor::new(executor, &provider);
    let operator = exec.operator().call().await.map_err(call_err)?;
    if operator != vault {
        return Err(format!(
            "the SearcherExecutor's operator is {operator}, but the vault signs as {vault}: every rebalance would \
             revert NotOperator"
        ));
    }
    let pool = exec.pool().call().await.map_err(call_err)?;
    let (token0, token1) = (
        exec.token0().call().await.map_err(call_err)?,
        exec.token1().call().await.map_err(call_err)?,
    );
    let darkpool = IDarkPool::new(pool, &provider);
    let pool_tokens = (
        darkpool.token0().call().await.map_err(call_err)?,
        darkpool.token1().call().await.map_err(call_err)?,
    );
    if pool_tokens != (token0, token1) {
        return Err(format!(
            "the executor trades {token0}/{token1}, its pool {}/{}",
            pool_tokens.0, pool_tokens.1
        ));
    }
    let oracle = darkpool.oracle().call().await.map_err(call_err)?;
    if let Some(expected) = feed
        && oracle != expected
    {
        return Err(format!(
            "the pool reads its price from {oracle}, not from the feed {expected}"
        ));
    }
    Ok(Deployment {
        chain_id,
        executor,
        operator,
        pool,
        feed: oracle,
        token0,
        token1,
    })
}

/// Connects to the RPC at `rpc_url` and checks the deployment (see `check_deployment`).
pub(crate) async fn connect(
    rpc_url: &str,
    chain_id: u64,
    executor: Address,
    vault: Address,
    feed: Option<Address>,
) -> Result<(LiveChain, Deployment), String> {
    let secrets = url_secrets(rpc_url);
    let url = rpc_url.parse().map_err(|_| {
        "--rpc-url does not parse as a URL (not shown: it may hold a key)".to_string()
    })?;
    let client = RpcClient::new_http(url);
    let deployment = check_deployment(&client, chain_id, executor, vault, feed, &secrets).await?;
    let chain = LiveChain::new(client, deployment.pool, executor, vault, secrets);
    Ok((chain, deployment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use alloy::primitives::address;
    use alloy::rpc::json_rpc::{RequestPacket, ResponsePacket};
    use alloy::sol_types::SolError;
    use alloy::transports::TransportFut;
    use alloy::transports::mock::{Asserter, MockTransport};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    /// One JSON-RPC request as the node received it.
    #[derive(Clone, Debug)]
    struct Seen {
        method: String,
        params: Value,
        /// Part of a batch (an array of requests in one HTTP body).
        batched: bool,
    }

    /// alloy's mock transport, recording what it is sent.
    #[derive(Clone)]
    struct Recorder {
        inner: MockTransport,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl tower_service::Service<RequestPacket> for Recorder {
        type Response = ResponsePacket;
        type Error = TransportError;
        type Future = TransportFut<'static>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        fn call(&mut self, packet: RequestPacket) -> Self::Future {
            let (requests, batched) = match &packet {
                RequestPacket::Single(request) => (vec![request], false),
                RequestPacket::Batch(requests) => (requests.iter().collect(), true),
            };
            let mut seen = self.seen.lock().unwrap();
            for request in requests {
                seen.push(Seen {
                    method: request.method().to_string(),
                    params: request
                        .params()
                        .map_or(Value::Null, |p| serde_json::from_str(p.get()).unwrap()),
                    batched,
                });
            }
            self.inner.call(packet)
        }
    }

    /// The requests so far, emptied.
    fn take(seen: &Arc<Mutex<Vec<Seen>>>) -> Vec<Seen> {
        std::mem::take(&mut *seen.lock().unwrap())
    }

    const POOL: Address = address!("0x00000000000000000000000000000000000000d0");
    const FEED: Address = address!("0x00000000000000000000000000000000000000f0");
    const TOKEN0: Address = address!("0x0000000000000000000000000000000000000a00");
    const TOKEN1: Address = address!("0x0000000000000000000000000000000000000b00");

    fn mock() -> (RpcClient, Asserter) {
        let (client, asserter, _) = recorded();
        (client, asserter)
    }

    fn recorded() -> (RpcClient, Asserter, Arc<Mutex<Vec<Seen>>>) {
        let asserter = Asserter::new();
        let seen = Arc::default();
        let recorder = Recorder {
            inner: MockTransport::new(asserter.clone()),
            seen: Arc::clone(&seen),
        };
        (RpcClient::new(recorder, true), asserter, seen)
    }

    fn snapshot() -> Snapshot {
        let w = crate::model::wad;
        Snapshot {
            blockNumber: U256::from(77u64),
            timestamp: U256::from(1_767_225_600u64),
            baseFee: U256::from(100_000_000u64),
            price: w("3456.78"),
            updatedAt: U256::from(1_767_225_590u64),
            reserve0: w("1000"),
            reserve1: w("3456780"),
            virtual0: w("1000"),
            virtual1: w("3456780"),
            balance0: w("20"),
            balance1: w("70000"),
        }
    }

    #[test]
    fn the_pinned_lens_is_creation_code_that_takes_the_pool_and_the_executor() {
        let input = lens_input(POOL, fixtures::EXECUTOR);
        let code = hex::decode(LENS_INITCODE_HEX.trim()).unwrap();
        assert!(code.len() > 500, "{} bytes", code.len());
        assert_eq!(
            &code[..2],
            &[0x60, 0x80],
            "solc's creation code: PUSH1 0x80 (the free memory pointer)"
        );
        assert_eq!(input.len(), code.len() + 64);
        assert_eq!(&input[code.len() + 12..code.len() + 32], POOL.as_slice());
        assert_eq!(&input[code.len() + 44..], fixtures::EXECUTOR.as_slice());
    }

    /// One batch per tick: the lens, the two nonces and the gas balance; the transaction in flight's receipt and
    /// status when there is one. Every value lands where the engine expects it.
    #[tokio::test]
    async fn a_tick_is_one_batched_request_parsed_into_the_chain_state() {
        let (client, asserter, seen) = recorded();
        let mut chain = LiveChain::new(
            client,
            POOL,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            vec![],
        );
        let encoded = Bytes::from(snapshot().abi_encode());
        assert_eq!(encoded.len(), 11 * 32);
        asserter.push_success(&encoded);
        asserter.push_success(&U64::from(5));
        asserter.push_success(&U64::from(5));
        asserter.push_success(&(U256::from(3u64) * U256::from(10u64).pow(U256::from(17u64))));
        let reads = chain.read(None).await.unwrap();
        let requests = take(&seen);
        assert_eq!(
            requests
                .iter()
                .map(|r| r.method.as_str())
                .collect::<Vec<_>>(),
            [
                "eth_call",
                "eth_getTransactionCount",
                "eth_getTransactionCount",
                "eth_getBalance"
            ]
        );
        assert!(
            requests.iter().all(|r| r.batched),
            "one batch: {requests:?}"
        );
        assert_eq!(
            requests[0].params,
            json!([{ "input": lens_input(POOL, fixtures::EXECUTOR) }, "latest"])
        );
        assert_eq!(
            requests[1].params,
            json!([fixtures::VAULT_ADDRESS, "latest"])
        );
        assert_eq!(
            requests[2].params,
            json!([fixtures::VAULT_ADDRESS, "pending"])
        );
        assert_eq!(
            requests[3].params,
            json!([fixtures::VAULT_ADDRESS, "latest"])
        );

        let s = snapshot();
        assert_eq!(reads.oracle.price, s.price);
        assert_eq!(reads.oracle.updated_at, 1_767_225_590);
        assert_eq!((reads.token0, reads.token1), (s.balance0, s.balance1));
        assert_eq!(
            (reads.pool.r0, reads.pool.r1, reads.pool.v0, reads.pool.v1),
            (s.reserve0, s.reserve1, s.virtual0, s.virtual1)
        );
        assert_eq!(
            reads.block,
            BlockInfo {
                number: 77,
                timestamp: 1_767_225_600,
                base_fee: 100_000_000
            }
        );
        assert_eq!((reads.nonce, reads.pending_nonce), (5, 5));
        assert_eq!(reads.watched, Watched::Nothing);

        // With a transaction in flight: its receipt, then its status.
        let hash = B256::repeat_byte(0xab);
        for (receipt, tx, expected) in [
            (Value::Null, Value::Null, Watched::Unknown),
            (Value::Null, json!({ "hash": hash }), Watched::Pending),
            (
                json!({
                    "transactionHash": hash,
                    "from": fixtures::VAULT_ADDRESS,
                    "status": "0x1",
                    "blockNumber": "0x4e",
                    "gasUsed": "0x249f0",
                    "l1BlockNumber": "0x1",
                }),
                Value::Null,
                Watched::Mined(Receipt {
                    tx_hash: hash,
                    from: fixtures::VAULT_ADDRESS,
                    status: TxStatus::Success,
                    block: 78,
                    gas_used: 150_000,
                }),
            ),
        ] {
            asserter.push_success(&encoded);
            asserter.push_success(&U64::from(5));
            asserter.push_success(&U64::from(6));
            asserter.push_success(&U256::ZERO);
            asserter.push_success(&receipt);
            asserter.push_success(&tx);
            let reads = chain.read(Some(hash)).await.unwrap();
            assert_eq!(reads.watched, expected);
            assert_eq!(reads.pending_nonce, 6);
            let requests = take(&seen);
            assert_eq!(requests.len(), 6, "{requests:?}");
            assert!(
                requests.iter().all(|r| r.batched),
                "still one batch: {requests:?}"
            );
            assert_eq!(
                requests[4..]
                    .iter()
                    .map(|r| r.method.as_str())
                    .collect::<Vec<_>>(),
                ["eth_getTransactionReceipt", "eth_getTransactionByHash"]
            );
            assert_eq!(requests[4].params, json!([hash]));
        }
    }

    #[tokio::test]
    async fn a_short_lens_answer_or_a_failed_batch_is_an_error_not_a_reading() {
        let (client, asserter) = mock();
        let mut chain = LiveChain::new(
            client,
            POOL,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            vec![],
        );
        asserter.push_success(&Bytes::from(vec![0u8; 64]));
        asserter.push_success(&U64::from(0));
        asserter.push_success(&U64::from(0));
        asserter.push_success(&U256::ZERO);
        assert!(matches!(chain.read(None).await, Err(EngineError::Feed(m)) if m.contains("lens")));
        asserter.push_failure_msg("upstream unavailable");
        assert!(chain.read(None).await.is_err());
    }

    /// The simulation tells a revert (with the contract's reason) from a node failure.
    #[tokio::test]
    async fn the_simulation_names_the_revert() {
        let (client, asserter, seen) = recorded();
        let mut chain = LiveChain::new(
            client,
            POOL,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            vec![],
        );
        let tx = fixtures::scenario_tx();

        asserter.push_success(&Bytes::new());
        chain.simulate(fixtures::VAULT_ADDRESS, &tx).await.unwrap();
        let requests = take(&seen);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "eth_call");
        let call = &requests[0].params;
        assert_eq!(call[0]["from"], json!(fixtures::VAULT_ADDRESS));
        assert_eq!(call[0]["to"], json!(fixtures::EXECUTOR));
        assert_eq!(call[0]["input"], json!(tx.input));
        assert_eq!(call[0]["gas"], "0x493e0");
        assert_eq!(call[1], "latest");

        let revert = |data: Vec<u8>| {
            serde_json::from_value::<ErrorPayload>(json!({
                "code": 3,
                "message": "execution reverted",
                "data": hex::encode_prefixed(data),
            }))
            .unwrap()
        };
        asserter.push_failure(revert(IDarkPool::DeviationNotReduced {}.abi_encode()));
        let beaten = chain
            .simulate(fixtures::VAULT_ADDRESS, &tx)
            .await
            .unwrap_err();
        assert_eq!(
            beaten,
            EngineError::Simulation("DarkPool: DeviationNotReduced(DeviationNotReduced)".into())
        );
        asserter.push_failure(revert(ISearcherExecutor::NotOperator {}.abi_encode()));
        let refused = chain
            .simulate(fixtures::VAULT_ADDRESS, &tx)
            .await
            .unwrap_err();
        assert!(matches!(refused, EngineError::Simulation(m) if m.contains("NotOperator")));
        asserter.push_failure(revert(
            Panic {
                code: U256::from(0x11),
            }
            .abi_encode(),
        ));
        let panic = chain
            .simulate(fixtures::VAULT_ADDRESS, &tx)
            .await
            .unwrap_err();
        assert_eq!(panic, EngineError::Simulation("panic 0x11".into()));

        asserter.push_failure_msg("header not found");
        let down = chain
            .simulate(fixtures::VAULT_ADDRESS, &tx)
            .await
            .unwrap_err();
        assert!(matches!(down, EngineError::Feed(_)), "{down:?}");
    }

    #[tokio::test]
    async fn the_broadcast_returns_the_hash_and_errors_never_show_the_rpc_url() {
        let secret_url = "https://arb-sepolia.example/v2/sk-very-secret-key";
        let (client, asserter, seen) = recorded();
        let mut chain = LiveChain::new(
            client,
            POOL,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            url_secrets(secret_url),
        );
        let raw = hex::decode(fixtures::SIGNED_REBALANCE).unwrap();
        let hash = alloy::primitives::keccak256(&raw);
        asserter.push_success(&hash);
        assert_eq!(chain.send_raw_transaction(&raw).await.unwrap(), hash);
        let requests = take(&seen);
        assert_eq!(requests[0].method, "eth_sendRawTransaction");
        assert_eq!(requests[0].params, json!([hex::encode_prefixed(&raw)]));

        asserter.push_failure_msg(format!("error sending request for url ({secret_url})"));
        let error = chain
            .send_raw_transaction(&raw)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("sk-very-secret-key"), "{error}");
        assert!(error.contains("<redacted>"), "{error}");
        assert!(
            redact(
                "auth sk-very-secret-key".into(),
                &url_secrets("https://h/x?key=sk-very-secret-key")
            )
            .contains("<redacted>")
        );
    }

    fn word(address: Address) -> Bytes {
        Bytes::from(address.abi_encode())
    }

    /// The start-up checks, answered in order: chain id, code at the executor, then the contracts' views.
    fn push_deployment(asserter: &Asserter, chain_id: u64, operator: Address, oracle: Address) {
        asserter.push_success(&U64::from(chain_id));
        asserter.push_success(&Bytes::from(vec![0x60, 0x80]));
        asserter.push_success(&word(operator));
        asserter.push_success(&word(POOL));
        asserter.push_success(&word(TOKEN0));
        asserter.push_success(&word(TOKEN1));
        asserter.push_success(&word(TOKEN0));
        asserter.push_success(&word(TOKEN1));
        asserter.push_success(&word(oracle));
    }

    #[tokio::test]
    async fn the_engine_trades_only_where_the_vault_is_the_operator() {
        let (client, asserter) = mock();
        push_deployment(&asserter, 421_614, fixtures::VAULT_ADDRESS, FEED);
        let deployment = check_deployment(
            &client,
            421_614,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            Some(FEED),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            deployment,
            Deployment {
                chain_id: 421_614,
                executor: fixtures::EXECUTOR,
                operator: fixtures::VAULT_ADDRESS,
                pool: POOL,
                feed: FEED,
                token0: TOKEN0,
                token1: TOKEN1,
            }
        );

        let (client, asserter) = mock();
        push_deployment(&asserter, 421_614, Address::repeat_byte(0x11), FEED);
        let error = check_deployment(
            &client,
            421_614,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("NotOperator") && error.contains(&fixtures::VAULT_ADDRESS.to_string()),
            "{error}"
        );

        let (client, asserter) = mock();
        push_deployment(&asserter, 1, fixtures::VAULT_ADDRESS, FEED);
        let error = check_deployment(
            &client,
            421_614,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert!(error.contains("chain 1"), "{error}");

        let (client, asserter) = mock();
        push_deployment(
            &asserter,
            421_614,
            fixtures::VAULT_ADDRESS,
            Address::repeat_byte(0x22),
        );
        let error = check_deployment(
            &client,
            421_614,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            Some(FEED),
            &[],
        )
        .await
        .unwrap_err();
        assert!(error.contains("not from the feed"), "{error}");

        let (client, asserter) = mock();
        asserter.push_success(&U64::from(421_614));
        asserter.push_success(&Bytes::new());
        let error = check_deployment(
            &client,
            421_614,
            fixtures::EXECUTOR,
            fixtures::VAULT_ADDRESS,
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert!(error.contains("no contract"), "{error}");
    }
}
