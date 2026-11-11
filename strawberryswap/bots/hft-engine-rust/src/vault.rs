//! The engine's side of the threshold signer's Unix socket (strawberryswap/vaults/threshold-signer).
//!
//! The engine holds no key. It sends the unsigned EIP-1559 payload and gets back the signed transaction, which it
//! checks before broadcasting: it must decode to exactly the transaction asked for, and its signature must recover to
//! the vault's address. Every call has one deadline (connect, write and read together); a late, refused or failed call
//! is an error the loop answers by withdrawing that tick's rebalance, never by retrying it blind.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy::consensus::{TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, B256, hex};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::tx::signing_payload;

/// Longest reply accepted, bytes (a signed rebalance is about 600 in hex).
const MAX_REPLY: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum VaultError {
    /// No vault on the socket, or the connection broke.
    Unavailable(String),
    /// The vault did not answer within the deadline.
    Timeout,
    /// The vault refused: its policy, its rate limit, or a malformed request.
    Refused { code: String, message: String },
    /// The reply is not the protocol.
    Protocol(String),
    /// The reply is well formed but is not a signature of our transaction by the vault.
    Mismatch(&'static str),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "vault unavailable: {m}"),
            Self::Timeout => f.write_str("vault timeout"),
            Self::Refused { code, message } => write!(f, "vault refused ({code}): {message}"),
            Self::Protocol(m) => write!(f, "vault protocol: {m}"),
            Self::Mismatch(m) => write!(f, "vault reply rejected: {m}"),
        }
    }
}

/// A transaction signed by the vault and checked: ready for eth_sendRawTransaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SignedTx {
    pub raw: Vec<u8>,
    pub hash: B256,
}

/// What the loop needs from a signer. The only implementation is the vault's socket.
pub(crate) trait TxSigner {
    async fn sign(&mut self, tx: &TxEip1559) -> Result<SignedTx, VaultError>;
}

/// One open connection to the vault: the reply reader and the request writer.
type Connection = (BufReader<OwnedReadHalf>, OwnedWriteHalf);

pub(crate) struct VaultClient {
    path: PathBuf,
    timeout: Duration,
    connection: Option<Connection>,
    next_id: u64,
    address: Option<Address>,
}

impl VaultClient {
    pub fn new(path: PathBuf, timeout: Duration) -> Self {
        Self {
            path,
            timeout,
            connection: None,
            next_id: 1,
            address: None,
        }
    }

    /// The vault's address, once known.
    pub fn address(&self) -> Option<Address> {
        self.address
    }

    /// Asks the vault for its address (the SearcherExecutor's operator) and remembers it.
    pub async fn fetch_address(&mut self) -> Result<Address, VaultError> {
        let result = self.call("address", None).await?;
        let address = result["address"]
            .as_str()
            .and_then(|a| a.parse::<Address>().ok())
            .ok_or_else(|| VaultError::Protocol("address reply without an address".into()))?;
        self.address = Some(address);
        Ok(address)
    }

    /// One request and its reply, within the deadline (connecting included).
    ///
    /// The connection is taken out for the exchange and put back only when the stream is known to be in step (a reply
    /// or a refusal was read), so a call that fails, times out or is cancelled half-way leaves no unread reply behind:
    /// the next call reconnects. A kept connection may have been closed by the vault in the meantime (it closes idle
    /// ones); a failure on it is retried once on a new connection, within the same deadline.
    async fn call(&mut self, method: &str, params: Option<Value>) -> Result<Value, VaultError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({ "id": id, "method": method });
        if let Some(params) = params {
            request["params"] = params;
        }
        let mut line = request.to_string().into_bytes();
        line.push(b'\n');

        let kept = self.connection.take();
        let path = &self.path;
        let attempt = async {
            let reused = kept.is_some();
            let (outcome, connection) = exchange(path, kept, id, &line).await;
            if reused && matches!(outcome, Err(VaultError::Unavailable(_))) {
                return exchange(path, None, id, &line).await;
            }
            (outcome, connection)
        };
        match tokio::time::timeout(self.timeout, attempt).await {
            Ok((outcome, connection)) => {
                self.connection = connection;
                outcome
            }
            Err(_) => Err(VaultError::Timeout),
        }
    }
}

/// Sends one request line on `connection` (a new one if None) and reads its reply. Returns the connection only if it
/// is still usable.
async fn exchange(
    path: &Path,
    connection: Option<Connection>,
    id: u64,
    line: &[u8],
) -> (Result<Value, VaultError>, Option<Connection>) {
    let mut connection = match connection {
        Some(connection) => connection,
        None => match UnixStream::connect(path).await {
            Ok(stream) => {
                let (read, write) = stream.into_split();
                (BufReader::new(read), write)
            }
            Err(e) => {
                return (
                    Err(VaultError::Unavailable(format!("{}: {e}", path.display()))),
                    None,
                );
            }
        },
    };
    let outcome = request(&mut connection, id, line).await;
    match outcome {
        Ok(_) | Err(VaultError::Refused { .. }) => (outcome, Some(connection)),
        _ => (outcome, None),
    }
}

async fn request(
    (reader, writer): &mut Connection,
    id: u64,
    line: &[u8],
) -> Result<Value, VaultError> {
    writer
        .write_all(line)
        .await
        .map_err(|e| VaultError::Unavailable(e.to_string()))?;
    let mut reply = Vec::new();
    let n = reader
        .take(MAX_REPLY as u64 + 1)
        .read_until(b'\n', &mut reply)
        .await
        .map_err(|e| VaultError::Unavailable(e.to_string()))?;
    if n == 0 {
        return Err(VaultError::Unavailable(
            "the vault closed the connection".into(),
        ));
    }
    if reply.last() != Some(&b'\n') {
        return Err(VaultError::Protocol("reply too long or cut short".into()));
    }
    let reply: Value =
        serde_json::from_slice(&reply).map_err(|e| VaultError::Protocol(e.to_string()))?;
    if reply["id"].as_u64() != Some(id) {
        return Err(VaultError::Protocol(format!(
            "reply to request {} instead of {id}",
            reply["id"]
        )));
    }
    if let Some(error) = reply.get("error") {
        return Err(VaultError::Refused {
            code: error["code"].as_str().unwrap_or("unknown").to_string(),
            message: error["message"].as_str().unwrap_or("").to_string(),
        });
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| VaultError::Protocol("reply without result or error".into()))
}

/// The checks on the vault's reply: an EIP-1559 transaction equal to the one requested, signed by `vault`.
pub(crate) fn verify_signed(
    raw: &[u8],
    requested: &TxEip1559,
    vault: Address,
) -> Result<B256, VaultError> {
    let envelope = TxEnvelope::decode_2718(&mut &raw[..])
        .map_err(|e| VaultError::Protocol(format!("signed_tx: {e}")))?;
    let TxEnvelope::Eip1559(signed) = envelope else {
        return Err(VaultError::Mismatch("not an EIP-1559 transaction"));
    };
    if signed.tx() != requested {
        return Err(VaultError::Mismatch(
            "the vault signed a different transaction",
        ));
    }
    let signer = signed
        .recover_signer()
        .map_err(|_| VaultError::Mismatch("the signature does not recover"))?;
    if signer != vault {
        return Err(VaultError::Mismatch(
            "signed by an address other than the vault's",
        ));
    }
    Ok(*signed.hash())
}

impl TxSigner for VaultClient {
    async fn sign(&mut self, tx: &TxEip1559) -> Result<SignedTx, VaultError> {
        let vault = match self.address {
            Some(address) => address,
            None => self.fetch_address().await?,
        };
        let params = json!({ "tx": hex::encode_prefixed(signing_payload(tx)) });
        let result = self.call("sign_transaction", Some(params)).await?;
        let raw = result["signed_tx"]
            .as_str()
            .and_then(|s| s.strip_prefix("0x"))
            .and_then(|digits| hex::decode(digits).ok())
            .ok_or_else(|| VaultError::Protocol("sign reply without a 0x-hex signed_tx".into()))?;
        let hash = verify_signed(&raw, tx, vault)?;
        Ok(SignedTx { raw, hash })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use tokio::net::UnixListener;

    /// A stand-in for the vault that answers every request line with `reply(request)`; None closes the connection.
    async fn fake_vault(
        path: &Path,
        reply: impl Fn(Value) -> Option<Value> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    match reply(request) {
                        Some(answer) => write
                            .write_all(format!("{answer}\n").as_bytes())
                            .await
                            .unwrap(),
                        None => break,
                    }
                }
            }
        })
    }

    /// A socket path in the temporary directory, removed when the test ends (passing or not).
    struct Socket(PathBuf);

    impl Socket {
        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for Socket {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn socket(name: &str) -> Socket {
        let path =
            std::env::temp_dir().join(format!("hft-vault-{}-{name}.sock", std::process::id()));
        Socket(path)
    }

    /// The real vault's answers for the fixture: its address, and its signature of the fixture transaction.
    fn honest(request: Value) -> Option<Value> {
        let id = request["id"].clone();
        Some(match request["method"].as_str() {
            Some("address") => {
                json!({ "id": id, "result": { "address": fixtures::VAULT_ADDRESS.to_string() } })
            }
            Some("sign_transaction") => {
                let expected = hex::encode_prefixed(signing_payload(&fixtures::scenario_tx()));
                if request["params"]["tx"] == expected {
                    json!({ "id": id, "result": { "signed_tx": format!("0x{}", fixtures::SIGNED_REBALANCE) } })
                } else {
                    json!({ "id": id, "error": { "code": "policy", "message": "not the fixture" } })
                }
            }
            _ => json!({ "id": id, "error": { "code": "unknown_method", "message": "" } }),
        })
    }

    #[tokio::test]
    async fn a_signature_by_the_vault_of_our_transaction_is_accepted() {
        let path = socket("honest");
        let server = fake_vault(&path.path(), honest).await;
        let mut client = VaultClient::new(path.path(), Duration::from_millis(500));
        let signed = client.sign(&fixtures::scenario_tx()).await.unwrap();
        assert_eq!(client.address(), Some(fixtures::VAULT_ADDRESS));
        assert_eq!(signed.raw, hex::decode(fixtures::SIGNED_REBALANCE).unwrap());
        assert_eq!(signed.hash, alloy::primitives::keccak256(&signed.raw));
        // A refusal comes back as Refused and the connection stays usable.
        let other = crate::tx::unsigned_tx(&fixtures::CONFIG, 1, &fixtures::scenario_rebalance());
        assert!(
            matches!(client.sign(&other).await, Err(VaultError::Refused { code, .. }) if code == "policy")
        );
        assert!(client.sign(&fixtures::scenario_tx()).await.is_ok());
        server.abort();
    }

    #[tokio::test]
    async fn a_reply_that_is_not_our_transaction_signed_by_the_vault_is_rejected() {
        // The vault's real signature, presented as coming from another vault address.
        let path = socket("impostor");
        let server = fake_vault(&path.path(), |request| {
            let id = request["id"].clone();
            Some(match request["method"].as_str() {
                Some("address") => json!({ "id": id, "result": { "address": Address::repeat_byte(0x22).to_string() } }),
                _ => json!({ "id": id, "result": { "signed_tx": format!("0x{}", fixtures::SIGNED_REBALANCE) } }),
            })
        })
        .await;
        let mut client = VaultClient::new(path.path(), Duration::from_millis(500));
        assert_eq!(
            client.sign(&fixtures::scenario_tx()).await,
            Err(VaultError::Mismatch(
                "signed by an address other than the vault's"
            ))
        );
        server.abort();

        // The right signer, but for a transaction we did not ask for.
        let other = crate::tx::unsigned_tx(&fixtures::CONFIG, 5, &fixtures::scenario_rebalance());
        assert_eq!(
            verify_signed(
                &hex::decode(fixtures::SIGNED_REBALANCE).unwrap(),
                &other,
                fixtures::VAULT_ADDRESS
            ),
            Err(VaultError::Mismatch(
                "the vault signed a different transaction"
            ))
        );
    }

    #[tokio::test]
    async fn a_missing_silent_or_closing_vault_is_an_error_within_the_deadline() {
        // Nobody listening.
        let mut client = VaultClient::new(socket("nobody").path(), Duration::from_millis(200));
        assert!(matches!(
            client.sign(&fixtures::scenario_tx()).await,
            Err(VaultError::Unavailable(_))
        ));

        // A vault that never answers: the deadline holds.
        let path = socket("silent");
        let listener = UnixListener::bind(path.path()).unwrap();
        let held = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut client = VaultClient::new(path.path(), Duration::from_millis(50));
        let started = std::time::Instant::now();
        assert_eq!(
            client.sign(&fixtures::scenario_tx()).await,
            Err(VaultError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        held.abort();

        // A vault that hangs up, then comes back: the client reconnects.
        let path = socket("flaky");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = std::sync::Arc::clone(&calls);
        let server = fake_vault(&path.path(), move |request| {
            if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                None
            } else {
                honest(request)
            }
        })
        .await;
        let mut client = VaultClient::new(path.path(), Duration::from_millis(500));
        assert!(matches!(
            client.fetch_address().await,
            Err(VaultError::Unavailable(_))
        ));
        assert_eq!(client.fetch_address().await, Ok(fixtures::VAULT_ADDRESS));
        server.abort();
    }

    /// The vault closes connections that stay idle; the next call must not lose its tick to that.
    #[tokio::test]
    async fn a_connection_the_vault_closed_in_between_is_replaced_within_the_call() {
        let path = socket("idle");
        let listener = UnixListener::bind(path.path()).unwrap();
        let server = tokio::spawn(async move {
            // One request per connection, then hang up: every kept connection is stale by the next call.
            while let Ok((stream, _)) = listener.accept().await {
                let (read, mut write) = stream.into_split();
                if let Ok(Some(line)) = BufReader::new(read).lines().next_line().await {
                    let answer = honest(serde_json::from_str(&line).unwrap()).unwrap();
                    write
                        .write_all(format!("{answer}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let mut client = VaultClient::new(path.path(), Duration::from_millis(500));
        assert_eq!(client.fetch_address().await, Ok(fixtures::VAULT_ADDRESS));
        for _ in 0..3 {
            assert!(client.sign(&fixtures::scenario_tx()).await.is_ok());
        }
        server.abort();
    }

    /// A call cancelled half-way (the loop's own deadline) leaves no unread reply that the next call would take for
    /// its own.
    #[tokio::test]
    async fn a_cancelled_call_does_not_desynchronise_the_next_one() {
        let path = socket("cancel");
        let slow_first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let listener = UnixListener::bind(path.path()).unwrap();
        let flag = std::sync::Arc::clone(&slow_first);
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let flag = std::sync::Arc::clone(&flag);
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut lines = BufReader::new(read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if flag.swap(false, std::sync::atomic::Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        let answer = honest(serde_json::from_str(&line).unwrap()).unwrap();
                        if write
                            .write_all(format!("{answer}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        let mut client = VaultClient::new(path.path(), Duration::from_millis(500));
        let cancelled =
            tokio::time::timeout(Duration::from_millis(20), client.fetch_address()).await;
        assert!(
            cancelled.is_err(),
            "the first reply is late, so the outer deadline cancels the call"
        );
        // The late reply to request 1 must not be read as the reply to request 2.
        assert_eq!(client.fetch_address().await, Ok(fixtures::VAULT_ADDRESS));
        assert!(client.sign(&fixtures::scenario_tx()).await.is_ok());
        server.abort();
    }
}
