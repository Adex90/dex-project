//! What the vault reads from the chain over JSON-RPC: at start, the chain id, the code at the role's contract and the
//! contracts' configuration (views); then, for the nonce rule, its own next nonce and the receipts of what it signed.
//!
//! The RPC URL may hold an API key: it is never printed, and no error carries it (reqwest's errors are stripped of
//! their URL; the node's own error messages do not contain it).

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy_primitives::{Address, B256, hex};
use alloy_sol_types::{SolCall, sol};
use serde_json::{Value, json};

sol! {
    // SearcherExecutor: the operator's key.
    function operator() external view returns (address);
    // ZkPriceFeed: what the relayer's attestations must say.
    function providerId() external view returns (bytes32);
    function queryCommitment() external view returns (bytes32);
    function minPrice() external view returns (uint256);
    function maxPrice() external view returns (uint256);
    // DarkPool: the notes per batch.
    function batchSize() external view returns (uint256);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainError(pub String);

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The chain as the vault sees it.
pub trait Chain: Send + Sync + 'static {
    fn chain_id(&self) -> impl Future<Output = Result<u64, ChainError>> + Send;

    /// The size of the code at `at` (0 for an account with none).
    fn code_size(&self, at: Address) -> impl Future<Output = Result<usize, ChainError>> + Send;

    /// `eth_call` at the latest block.
    fn call(
        &self,
        to: Address,
        input: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, ChainError>> + Send;

    /// The account's next nonce at `latest` and at `pending`.
    fn nonces(&self, of: Address) -> impl Future<Output = Result<(u64, u64), ChainError>> + Send;

    /// The account's next nonce at `latest`, then which of `txs` have a receipt: one batch, the nonce first, so the
    /// receipts are read at least as late as the nonce.
    fn next_nonce_and_mined(
        &self,
        of: Address,
        txs: &[B256],
    ) -> impl Future<Output = Result<(u64, Vec<B256>), ChainError>> + Send;
}

/// A view call, decoded.
pub async fn view<C: SolCall>(
    chain: &impl Chain,
    to: Address,
    call: C,
) -> Result<C::Return, ChainError> {
    let output = chain.call(to, call.abi_encode()).await?;
    C::abi_decode_returns(&output).map_err(|_| {
        ChainError(format!(
            "{to}: {} returned {} bytes that do not decode (is it the right contract?)",
            C::SIGNATURE,
            output.len()
        ))
    })
}

/// JSON-RPC over HTTP(S).
pub struct Rpc {
    client: reqwest::Client,
    url: reqwest::Url,
    next_id: AtomicU64,
}

fn quantity(value: &Value, what: &str) -> Result<u64, ChainError> {
    value
        .as_str()
        .and_then(|text| text.strip_prefix("0x"))
        .and_then(|digits| u64::from_str_radix(digits, 16).ok())
        .ok_or_else(|| ChainError(format!("{what}: not a quantity: {value}")))
}

fn data(value: &Value, what: &str) -> Result<Vec<u8>, ChainError> {
    value
        .as_str()
        .and_then(|text| hex::decode(text).ok())
        .ok_or_else(|| ChainError(format!("{what}: not hex data")))
}

impl Rpc {
    /// The URL is checked here and never shown: a parse error says only that it does not parse.
    pub fn new(url: &str, timeout: Duration) -> Result<Self, ChainError> {
        let url: reqwest::Url = url.parse().map_err(|_| {
            ChainError("the RPC URL does not parse (not shown: it may hold a key)".into())
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ChainError("the RPC URL must be http(s)".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ChainError(format!("HTTP client: {}", e.without_url())))?;
        Ok(Self {
            client,
            url,
            next_id: AtomicU64::new(1),
        })
    }

    /// One batch of calls; the results in the calls' order. A call the node answers with an error fails the batch.
    async fn batch(&self, calls: &[(&str, Value)]) -> Result<Vec<Value>, ChainError> {
        let first = self
            .next_id
            .fetch_add(calls.len() as u64, Ordering::Relaxed);
        let requests: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (method, params))| {
                json!({ "jsonrpc": "2.0", "id": first + i as u64, "method": method, "params": params })
            })
            .collect();
        let response = self
            .client
            .post(self.url.clone())
            .header("content-type", "application/json")
            .body(Value::Array(requests).to_string())
            .send()
            .await
            .map_err(|e| ChainError(format!("RPC: {}", e.without_url())))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| ChainError(format!("RPC: {}", e.without_url())))?;
        if !status.is_success() {
            return Err(ChainError(format!("RPC: HTTP {status}")));
        }
        let replies: Vec<Value> = serde_json::from_slice(&body)
            .map_err(|_| ChainError("RPC: the reply is not a JSON-RPC batch".into()))?;
        let mut results = vec![Value::Null; calls.len()];
        let mut seen = vec![false; calls.len()];
        for reply in replies {
            let index = reply["id"]
                .as_u64()
                .and_then(|id| id.checked_sub(first))
                .map(|i| i as usize)
                .filter(|&i| i < calls.len())
                .ok_or_else(|| ChainError("RPC: a reply to no request".into()))?;
            if let Some(error) = reply.get("error") {
                return Err(ChainError(format!(
                    "RPC: {} failed: {}",
                    calls[index].0,
                    error["message"].as_str().unwrap_or("error")
                )));
            }
            results[index] = reply["result"].clone();
            seen[index] = true;
        }
        if seen.iter().any(|seen| !seen) {
            return Err(ChainError("RPC: a request went unanswered".into()));
        }
        Ok(results)
    }

    async fn one(&self, method: &str, params: Value) -> Result<Value, ChainError> {
        Ok(self.batch(&[(method, params)]).await?.remove(0))
    }
}

impl Chain for Rpc {
    async fn chain_id(&self) -> Result<u64, ChainError> {
        quantity(&self.one("eth_chainId", json!([])).await?, "eth_chainId")
    }

    async fn code_size(&self, at: Address) -> Result<usize, ChainError> {
        let code = self
            .one("eth_getCode", json!([at.to_string(), "latest"]))
            .await?;
        Ok(data(&code, "eth_getCode")?.len())
    }

    async fn call(&self, to: Address, input: Vec<u8>) -> Result<Vec<u8>, ChainError> {
        let output = self
            .one(
                "eth_call",
                json!([{ "to": to.to_string(), "data": hex::encode_prefixed(input) }, "latest"]),
            )
            .await?;
        data(&output, "eth_call")
    }

    async fn nonces(&self, of: Address) -> Result<(u64, u64), ChainError> {
        let results = self
            .batch(&[
                ("eth_getTransactionCount", json!([of.to_string(), "latest"])),
                (
                    "eth_getTransactionCount",
                    json!([of.to_string(), "pending"]),
                ),
            ])
            .await?;
        Ok((
            quantity(&results[0], "nonce (latest)")?,
            quantity(&results[1], "nonce (pending)")?,
        ))
    }

    async fn next_nonce_and_mined(
        &self,
        of: Address,
        txs: &[B256],
    ) -> Result<(u64, Vec<B256>), ChainError> {
        let mut calls = vec![("eth_getTransactionCount", json!([of.to_string(), "latest"]))];
        calls.extend(
            txs.iter()
                .map(|tx| ("eth_getTransactionReceipt", json!([tx.to_string()]))),
        );
        let results = self.batch(&calls).await?;
        let next = quantity(&results[0], "nonce (latest)")?;
        let mined = txs
            .iter()
            .zip(&results[1..])
            .filter(|(_, receipt)| !receipt.is_null())
            .map(|(tx, _)| *tx)
            .collect();
        Ok((next, mined))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    /// A JSON-RPC node that answers each method from a table and records what it was asked.
    async fn node(answers: Vec<(&'static str, Value)>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&asked);
        let app = Router::new().route(
            "/{*key}",
            post(move |body: Bytes| {
                let log = Arc::clone(&log);
                let answers = answers.clone();
                async move {
                    let batch: Vec<Value> = serde_json::from_slice(&body).unwrap();
                    log.lock().unwrap().extend(batch.clone());
                    // Replies in reverse order: the client must match them by id.
                    let replies: Vec<Value> = batch
                        .iter()
                        .rev()
                        .map(|request| {
                            let method = request["method"].as_str().unwrap();
                            let tx = request["params"][0].as_str().unwrap_or_default();
                            let result = answers
                                .iter()
                                .find(|(m, _)| *m == method || *m == tx)
                                .map(|(_, v)| v.clone())
                                .unwrap_or(Value::Null);
                            json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
                        })
                        .collect();
                    Value::Array(replies).to_string()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/v2/SECRET-API-KEY"), asked)
    }

    #[tokio::test]
    async fn reads_are_batched_and_matched_by_id() {
        let mined = B256::repeat_byte(1);
        let lost = B256::repeat_byte(2);
        let (url, asked) = node(vec![
            ("eth_chainId", json!("0x66eee")),
            ("eth_getTransactionCount", json!("0x2a")),
            ("eth_getCode", json!("0x6080")),
            (
                "eth_call",
                json!(format!("0x{}", "00".repeat(12) + &"ab".repeat(20))),
            ),
            (
                Box::leak(mined.to_string().into_boxed_str()),
                json!({ "status": "0x1" }),
            ),
        ])
        .await;
        let rpc = Rpc::new(&url, Duration::from_secs(5)).unwrap();
        assert_eq!(rpc.chain_id().await, Ok(421_614));
        assert_eq!(rpc.code_size(Address::ZERO).await, Ok(2));
        assert_eq!(rpc.nonces(Address::ZERO).await, Ok((42, 42)));
        assert_eq!(
            view(&rpc, Address::ZERO, operatorCall {}).await,
            Ok(Address::repeat_byte(0xab))
        );
        let (next, found) = rpc
            .next_nonce_and_mined(Address::ZERO, &[mined, lost])
            .await
            .unwrap();
        assert_eq!((next, found), (42, vec![mined]));
        // The nonce is asked first, in the same batch as the receipts.
        let asked = asked.lock().unwrap();
        let last: Vec<&str> = asked[asked.len() - 3..]
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            last,
            [
                "eth_getTransactionCount",
                "eth_getTransactionReceipt",
                "eth_getTransactionReceipt"
            ]
        );
    }

    #[tokio::test]
    async fn errors_never_carry_the_url() {
        // Nothing listens there: the connection fails.
        let rpc = Rpc::new(
            "http://127.0.0.1:9/v2/SECRET-API-KEY?key=SECRET",
            Duration::from_secs(2),
        )
        .unwrap();
        let error = rpc.chain_id().await.unwrap_err().to_string();
        assert!(
            !error.contains("SECRET") && !error.contains("127.0.0.1"),
            "{error}"
        );
        let error = Rpc::new("not a url SECRET", Duration::from_secs(1))
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("SECRET"), "{error}");
        // A view that does not decode names the function, not the URL.
        let (url, _) = node(vec![("eth_call", json!("0x01"))]).await;
        let rpc = Rpc::new(&url, Duration::from_secs(5)).unwrap();
        let error = view(&rpc, Address::ZERO, batchSizeCall {})
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("batchSize()") && !error.contains("SECRET"),
            "{error}"
        );
    }
}
