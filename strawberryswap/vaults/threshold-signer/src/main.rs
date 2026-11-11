//! StrawberrySwap threshold signer, iteration 0: the vault daemon that signs for the HFT engine.
//!
//! The engine never holds a key. It builds an unsigned EIP-1559 transaction and sends it over a local Unix socket. This
//! process decodes it, checks it against a strict policy, signs it and returns the signature and the signed
//! transaction. The policy allows only `SearcherExecutor.rebalance`, on one chain, with bounded amounts, gas and fees,
//! no value, and never `sweep`.
//!
//! Two things are MOCKED in this iteration:
//! * the key is one secp256k1 key generated in memory at start, never stored; the threshold protocol replaces it
//!   behind the same `alloy_signer::Signer` trait;
//! * the policy is a static allowlist that does not read chain state.
//!
//! Protocol: newline-delimited JSON, one request per line and one response per line (README.md).

#![forbid(unsafe_code)]

// The insecure-test-key feature lets the end-to-end tests give the vault a key they know, so that they can look for it
// in process memory. It must never reach a deployable binary, so optimized builds refuse to compile with it.
#[cfg(all(feature = "insecure-test-key", not(debug_assertions)))]
compile_error!(
    "the insecure-test-key feature loads the key from a file: it is for the end-to-end tests and is refused in optimized builds"
);

use std::fmt;
use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{Address, B256, TxKind, U256, hex, keccak256};
use alloy_rlp::Decodable;
use alloy_signer::Signer;
use alloy_signer_local::PrivateKeySigner;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout};

/// Longest request line accepted, in bytes. A rebalance transaction is about 230 bytes, 460 in hex.
const MAX_LINE: usize = 16 * 1024;
/// A connection that sends nothing for this long is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Connections served at once; further ones are closed as they arrive.
const MAX_CONNECTIONS: usize = 8;
/// The only function the vault signs: `SearcherExecutor.rebalance`.
const REBALANCE: &str = "rebalance(uint256,uint256,uint256,uint256,uint256)";
/// Its arguments, for error messages.
const REBALANCE_ARGS: [&str; 5] = ["amount0Out", "amount1Out", "pay0", "pay1", "treasuryPay"];
/// The operator's other function, which moves inventory out of the executor. Never signed here.
const SWEEP: &str = "sweep(address,address,uint256)";
/// The intrinsic gas of any transaction.
const INTRINSIC_GAS: u64 = 21_000;

/// The 4-byte selector of a Solidity function signature.
fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

// ---------------------------------------------------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------------------------------------------------

/// What the vault signs. Static in this iteration (mocked): it does not read chain state.
#[derive(Clone, Debug)]
struct Policy {
    chain_id: u64,
    /// The SearcherExecutor contract, the only destination.
    executor: Address,
    /// Cap on the gas limit.
    max_gas: u64,
    /// Cap on `max_fee_per_gas`, wei.
    max_fee_per_gas: u128,
}

/// Why a payload is not signed.
#[derive(Debug, PartialEq)]
enum Denied {
    Empty,
    TxType(u8),
    Rlp(String),
    TrailingBytes(usize),
    NonCanonical,
    ChainId { got: u64, want: u64 },
    Create,
    To(Address),
    Value(U256),
    AccessList,
    GasLimit(u64),
    MaxFee(u128),
    PriorityFee,
    Sweep,
    Selector([u8; 4]),
    CalldataLength(usize),
    Amount(&'static str),
    NoOutput,
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
            Self::To(to) => write!(f, "destination {to} is not the SearcherExecutor"),
            Self::Value(v) => write!(f, "value {v} wei: the vault signs no ether transfer"),
            Self::AccessList => f.write_str("access lists are not signed"),
            Self::GasLimit(g) => write!(f, "gas limit {g} outside [{INTRINSIC_GAS}, cap]"),
            Self::MaxFee(fee) => write!(f, "max fee per gas {fee} wei above the cap"),
            Self::PriorityFee => f.write_str("priority fee above the max fee"),
            Self::Sweep => {
                f.write_str("sweep moves inventory out of the executor: never signed by this vault")
            }
            Self::Selector(s) => write!(
                f,
                "selector {}: only SearcherExecutor.rebalance is signed",
                hex::encode_prefixed(s)
            ),
            Self::CalldataLength(n) => write!(
                f,
                "calldata of {n} bytes: rebalance takes exactly 4 + 5 × 32 = 164"
            ),
            Self::Amount(arg) => write!(f, "{arg} does not fit in 128 bits"),
            Self::NoOutput => {
                f.write_str("amount0Out and amount1Out are both zero: the pool would revert")
            }
        }
    }
}

impl Policy {
    /// Decodes `raw` (the EIP-1559 signing payload, `0x02 ‖ rlp(fields)`) and checks it field by field. Returns the
    /// transaction only if every rule holds.
    fn check(&self, raw: &[u8]) -> Result<TxEip1559, Denied> {
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
        match tx.to {
            TxKind::Create => return Err(Denied::Create),
            TxKind::Call(to) if to != self.executor => return Err(Denied::To(to)),
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
        check_rebalance(&tx.input)?;
        Ok(tx)
    }
}

/// `rebalance(uint256 amount0Out, uint256 amount1Out, uint256 pay0, uint256 pay1, uint256 treasuryPay)`: exactly five
/// words, each below 2^128 (the pool's reserves are uint128, and the C++ bot encodes amounts the same way), and at
/// least one non-zero output, since `DarkPool.flashRebalance` reverts otherwise.
fn check_rebalance(input: &[u8]) -> Result<[U256; 5], Denied> {
    let Some((sel, args)) = input.split_first_chunk::<4>() else {
        return Err(Denied::CalldataLength(input.len()));
    };
    if *sel == selector(SWEEP) {
        return Err(Denied::Sweep);
    }
    if *sel != selector(REBALANCE) {
        return Err(Denied::Selector(*sel));
    }
    if args.len() != 5 * 32 {
        return Err(Denied::CalldataLength(input.len()));
    }
    let mut words = [U256::ZERO; 5];
    for (i, chunk) in args.chunks_exact(32).enumerate() {
        let word = U256::from_be_slice(chunk);
        if word > U256::from(u128::MAX) {
            return Err(Denied::Amount(REBALANCE_ARGS[i]));
        }
        words[i] = word;
    }
    if words[0].is_zero() && words[1].is_zero() {
        return Err(Denied::NoOutput);
    }
    Ok(words)
}

// ---------------------------------------------------------------------------------------------------------------------
// The vault
// ---------------------------------------------------------------------------------------------------------------------

/// A token bucket: at most `per_sec` signatures per second, in bursts of at most that many.
#[derive(Debug)]
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(per_sec: f64, now: Instant) -> Self {
        let capacity = per_sec.max(1.0);
        Self {
            capacity,
            tokens: capacity,
            per_sec,
            last: now,
        }
    }

    fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_sec).min(self.capacity);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Vault {
    policy: Policy,
    /// The signing backend. Mocked by an in-memory key in this iteration; nothing here can read the key back.
    signer: Arc<dyn Signer + Send + Sync>,
    /// The only uid allowed to connect (checked with SO_PEERCRED, which the kernel fills in).
    allowed_uid: u32,
    bucket: Mutex<TokenBucket>,
}

/// One request line: `{"id": 1, "method": "address"}` or
/// `{"id": 2, "method": "sign_transaction", "params": {"tx": "0x02…"}}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Option<SignParams>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignParams {
    /// The EIP-1559 signing payload `0x02 ‖ rlp([chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas,
    /// gas_limit, to, value, data, access_list])`, hex with a 0x prefix.
    tx: String,
}

fn error(id: Option<u64>, code: &str, message: impl Into<String>) -> Value {
    json!({ "id": id, "error": { "code": code, "message": message.into() } })
}

impl Vault {
    fn new(
        policy: Policy,
        signer: Arc<dyn Signer + Send + Sync>,
        allowed_uid: u32,
        signs_per_sec: f64,
    ) -> Self {
        let bucket = Mutex::new(TokenBucket::new(signs_per_sec, Instant::now()));
        Self {
            policy,
            signer,
            allowed_uid,
            bucket,
        }
    }

    /// One request line in, one response out.
    async fn handle(&self, line: &[u8]) -> Value {
        let request: Request = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(e) => return error(None, "parse", format!("not a valid request: {e}")),
        };
        let id = request.id;
        match (request.method.as_str(), request.params) {
            ("address", None) => {
                json!({ "id": id, "result": { "address": self.signer.address().to_string() } })
            }
            ("sign_transaction", Some(params)) => match self.sign(&params.tx).await {
                Ok(result) => json!({ "id": id, "result": result }),
                Err((code, message)) => error(Some(id), code, message),
            },
            ("address", Some(_)) => error(Some(id), "parse", "address takes no params"),
            ("sign_transaction", None) => {
                error(Some(id), "parse", "sign_transaction needs params.tx")
            }
            (other, _) => error(
                Some(id),
                "unknown_method",
                format!("unknown method {other:?}"),
            ),
        }
    }

    async fn sign(&self, tx_hex: &str) -> Result<Value, (&'static str, String)> {
        let raw = tx_hex
            .strip_prefix("0x")
            .and_then(|digits| hex::decode(digits).ok())
            .ok_or(("parse", "params.tx must be 0x-prefixed hex".to_string()))?;
        let tx = self.policy.check(&raw).map_err(|denied| {
            eprintln!("audit: denied: {denied}");
            ("policy", denied.to_string())
        })?;
        let allowed = self
            .bucket
            .lock()
            .map_err(|_| ("internal", "rate limiter unavailable".to_string()))?
            .try_take(Instant::now());
        if !allowed {
            eprintln!("audit: denied: rate limit (nonce {})", tx.nonce);
            return Err(("rate_limited", "too many signatures per second".to_string()));
        }
        let signing_hash = tx.signature_hash();
        let signature = self
            .signer
            .sign_hash(&signing_hash)
            .await
            .map_err(|e| ("signer", e.to_string()))?;
        let (nonce, gas_limit) = (tx.nonce, tx.gas_limit);
        let signed = tx.into_signed(signature);
        let mut signed_tx = Vec::with_capacity(signed.eip2718_encoded_length());
        signed.eip2718_encode(&mut signed_tx);
        let tx_hash = *signed.hash();
        eprintln!("audit: signed rebalance nonce {nonce} gas {gas_limit} tx {tx_hash}");
        Ok(json!({
            "r": B256::from(signature.r()).to_string(),
            "s": B256::from(signature.s()).to_string(),
            "y_parity": u8::from(signature.v()),
            "signing_hash": signing_hash.to_string(),
            "tx_hash": tx_hash.to_string(),
            "signed_tx": hex::encode_prefixed(&signed_tx),
        }))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The socket
// ---------------------------------------------------------------------------------------------------------------------

/// Reads a test key: 32 bytes as 64 hex digits (an optional 0x prefix), from a file only its owner can read.
#[cfg(feature = "insecure-test-key")]
fn load_test_key(path: &Path) -> io::Result<PrivateKeySigner> {
    let meta = fs::metadata(path)?;
    if meta.mode() & 0o077 != 0 {
        return Err(invalid(format!(
            "{}: a key file must be readable by its owner only (0600)",
            path.display()
        )));
    }
    let text = fs::read_to_string(path)?;
    let digits = text.trim();
    let digits = digits.strip_prefix("0x").unwrap_or(digits);
    let bytes: B256 = digits
        .parse()
        .map_err(|_| invalid(format!("{}: expected 64 hex digits", path.display())))?;
    PrivateKeySigner::from_bytes(&bytes).map_err(|e| {
        invalid(format!(
            "{}: not a valid secp256k1 key: {e}",
            path.display()
        ))
    })
}

/// The uid this process runs as. /proc/self is owned by it (Linux; no libc call, so no unsafe code).
fn own_uid() -> io::Result<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// `$XDG_RUNTIME_DIR/strawberryswap-vault/vault.sock`, or `<tmp>/strawberryswap-vault-<uid>/vault.sock`.
fn default_socket(uid: u32) -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir)
            .join("strawberryswap-vault")
            .join("vault.sock"),
        _ => std::env::temp_dir()
            .join(format!("strawberryswap-vault-{uid}"))
            .join("vault.sock"),
    }
}

/// Binds the socket in a private directory: the directory must belong to this uid and grant nothing to other users
/// (it is created 0700 if missing). The socket is 0600, or 0660 when another uid may connect (that uid then reaches
/// it through the directory's group, set up by the deployment). A stale socket from an earlier run is replaced; a
/// live one, or any other file, is not.
fn bind_socket(path: &Path, own_uid: u32, group_access: bool) -> io::Result<UnixListener> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "{}: the socket needs a parent directory",
                path.display()
            ))
        })?;
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(invalid(format!("{} is not a directory", dir.display())));
            }
            if meta.uid() != own_uid {
                return Err(invalid(format!(
                    "{} belongs to uid {}, not to this process (uid {own_uid})",
                    dir.display(),
                    meta.uid()
                )));
            }
            if meta.mode() & 0o007 != 0 {
                return Err(invalid(format!(
                    "{} is open to other users (mode {:o}): use 0700, or 0750 with a group",
                    dir.display(),
                    meta.mode() & 0o777
                )));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?,
        Err(e) => return Err(e),
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(invalid(format!(
                    "another process is listening on {}",
                    path.display()
                )));
            }
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(invalid(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if group_access { 0o660 } else { 0o600 }),
    )?;
    Ok(listener)
}

async fn write_line(write: &mut OwnedWriteHalf, value: &Value) -> io::Result<()> {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    write.write_all(&bytes).await
}

/// Serves one client: the kernel-reported uid must be the allowed one, then one JSON line per request, each at most
/// MAX_LINE bytes. A line that is too long, or cut short, ends the connection after an error reply.
async fn connection(stream: UnixStream, vault: &Vault) -> io::Result<()> {
    let uid = stream.peer_cred()?.uid();
    if uid != vault.allowed_uid {
        eprintln!(
            "audit: refused a connection from uid {uid} (only uid {} may connect)",
            vault.allowed_uid
        );
        return Ok(());
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        let limited = (&mut reader).take(MAX_LINE as u64 + 1);
        let n = match timeout(IDLE_TIMEOUT, read_line(limited, &mut line)).await {
            Ok(read) => read?,
            Err(_) => return Ok(()),
        };
        if n == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            let message = if line.len() > MAX_LINE {
                format!("request longer than {MAX_LINE} bytes")
            } else {
                "request not terminated by a newline".to_string()
            };
            return write_line(&mut write, &error(None, "parse", message)).await;
        }
        let response = vault.handle(&line).await;
        write_line(&mut write, &response).await?;
    }
}

async fn read_line(
    mut reader: impl tokio::io::AsyncBufRead + Unpin,
    line: &mut Vec<u8>,
) -> io::Result<usize> {
    reader.read_until(b'\n', line).await
}

/// Accepts connections until `shutdown` completes, at most MAX_CONNECTIONS at a time.
async fn serve(listener: UnixListener, vault: Arc<Vault>, shutdown: impl Future<Output = ()>) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        eprintln!("vault: {MAX_CONNECTIONS} connections already open; closing a new one");
                        continue;
                    };
                    let vault = Arc::clone(&vault);
                    tokio::spawn(async move {
                        if let Err(e) = connection(stream, &vault).await {
                            eprintln!("vault: connection ended: {e}");
                        }
                        drop(permit);
                    });
                }
                Err(e) => {
                    eprintln!("vault: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// Ctrl-C or SIGTERM (systemd's stop). A handler that cannot be installed never fires.
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------------------------------------------------

const USAGE: &str =
    "usage: threshold-signer --chain-id ID --executor ADDRESS [--socket PATH] [--allow-uid UID]
                        [--max-gas GAS] [--max-fee-gwei GWEI] [--max-signs-per-sec N]

Signs the HFT engine's SearcherExecutor.rebalance transactions over a local Unix socket.
The key is a MOCK: one secp256k1 key generated in memory at start, never written anywhere.

  --chain-id ID           the only chain id signed for (required)
  --executor ADDRESS      the SearcherExecutor contract, the only destination (required)
  --socket PATH           default: $XDG_RUNTIME_DIR/strawberryswap-vault/vault.sock
  --allow-uid UID         the uid allowed to connect (default: this process's uid)
  --max-gas GAS           gas limit cap (default: 1000000)
  --max-fee-gwei GWEI     max fee per gas cap (default: 500)
  --max-signs-per-sec N   signature rate limit (default: 20; the engine needs at most 10)";

struct Args {
    chain_id: u64,
    executor: Address,
    socket: Option<PathBuf>,
    allow_uid: Option<u32>,
    max_gas: u64,
    max_fee_gwei: u64,
    signs_per_sec: u32,
    #[cfg(feature = "insecure-test-key")]
    test_key_file: Option<PathBuf>,
}

impl Args {
    /// Parses the arguments; Err(None) asks for the usage text.
    fn parse(mut it: impl Iterator<Item = String>) -> Result<Self, Option<String>> {
        fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, Option<String>> {
            text.parse()
                .map_err(|_| Some(format!("{flag}: {text} is not a valid number")))
        }
        let (mut chain_id, mut executor) = (None, None);
        let mut args = Self {
            chain_id: 0,
            executor: Address::ZERO,
            socket: None,
            allow_uid: None,
            max_gas: 1_000_000,
            max_fee_gwei: 500,
            signs_per_sec: 20,
            #[cfg(feature = "insecure-test-key")]
            test_key_file: None,
        };
        while let Some(flag) = it.next() {
            if flag == "-h" || flag == "--help" {
                return Err(None);
            }
            let value = it
                .next()
                .ok_or_else(|| Some(format!("{flag} needs a value")))?;
            match flag.as_str() {
                "--chain-id" => chain_id = Some(number::<u64>(&flag, &value)?),
                "--executor" => {
                    let address: Address = value
                        .parse()
                        .map_err(|_| Some(format!("--executor: {value} is not an address")))?;
                    if address.is_zero() {
                        return Err(Some(
                            "--executor: the zero address is not a contract".into(),
                        ));
                    }
                    executor = Some(address);
                }
                "--socket" => args.socket = Some(PathBuf::from(value)),
                "--allow-uid" => args.allow_uid = Some(number(&flag, &value)?),
                "--max-gas" => args.max_gas = number(&flag, &value)?,
                "--max-fee-gwei" => args.max_fee_gwei = number(&flag, &value)?,
                "--max-signs-per-sec" => args.signs_per_sec = number(&flag, &value)?,
                #[cfg(feature = "insecure-test-key")]
                "--insecure-test-key-file" => args.test_key_file = Some(PathBuf::from(value)),
                other => return Err(Some(format!("unknown argument {other}"))),
            }
        }
        args.chain_id = chain_id.ok_or_else(|| {
            Some("--chain-id is required: the vault never guesses its chain".into())
        })?;
        args.executor = executor
            .ok_or_else(|| Some("--executor is required: the SearcherExecutor address".into()))?;
        if args.signs_per_sec == 0 || args.max_gas < INTRINSIC_GAS {
            return Err(Some(
                "--max-signs-per-sec must be positive and --max-gas at least 21000".into(),
            ));
        }
        Ok(args)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(None) => {
            println!("{USAGE}");
            #[cfg(feature = "insecure-test-key")]
            println!(
                "  --insecure-test-key-file PATH  INSECURE: the key from PATH (0600); end-to-end tests only"
            );
            return ExitCode::SUCCESS;
        }
        Err(Some(message)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let own_uid = match own_uid() {
        Ok(uid) => uid,
        Err(e) => {
            eprintln!("error: cannot read this process's uid from /proc/self: {e}");
            return ExitCode::FAILURE;
        }
    };
    let allowed_uid = args.allow_uid.unwrap_or(own_uid);
    let socket = args
        .socket
        .clone()
        .unwrap_or_else(|| default_socket(own_uid));
    let policy = Policy {
        chain_id: args.chain_id,
        executor: args.executor,
        max_gas: args.max_gas,
        max_fee_per_gas: u128::from(args.max_fee_gwei) * 1_000_000_000,
    };

    // MOCK key: generated here, held only by the signer (k256 zeroizes it on drop), never logged or stored.
    const MOCK_KEY: &str = "MOCK key, generated in memory at start and never stored";
    #[cfg(not(feature = "insecure-test-key"))]
    let (signer, key_origin) = (PrivateKeySigner::random(), MOCK_KEY.to_string());
    #[cfg(feature = "insecure-test-key")]
    let (signer, key_origin) = match args.test_key_file.as_deref() {
        None => (PrivateKeySigner::random(), MOCK_KEY.to_string()),
        Some(path) => match load_test_key(path) {
            Ok(signer) => {
                eprintln!(
                    "WARNING: INSECURE TEST KEY loaded from {}: end-to-end tests only, never a deployment",
                    path.display()
                );
                let origin = format!(
                    "INSECURE TEST KEY from {} (end-to-end tests only)",
                    path.display()
                );
                (signer, origin)
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let address = signer.address();
    let vault = Arc::new(Vault::new(
        policy.clone(),
        Arc::new(signer),
        allowed_uid,
        f64::from(args.signs_per_sec),
    ));
    let listener = match bind_socket(&socket, own_uid, allowed_uid != own_uid) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("threshold-signer: {key_origin}; signer {address}");
    println!(
        "policy: chain {} · to {} · {REBALANCE} [{}] only, never sweep · gas ≤ {} · max fee ≤ {} gwei · ≤ {} signatures/s",
        policy.chain_id,
        policy.executor,
        hex::encode_prefixed(selector(REBALANCE)),
        policy.max_gas,
        args.max_fee_gwei,
        args.signs_per_sec
    );
    println!(
        "listening on {} (mode {}; uid {allowed_uid} only)",
        socket.display(),
        if allowed_uid != own_uid {
            "0660"
        } else {
            "0600"
        }
    );

    serve(listener, vault, shutdown_signal()).await;
    if let Err(e) = fs::remove_file(&socket) {
        eprintln!("vault: could not remove {}: {e}", socket.display());
    }
    println!("threshold-signer: stopped");
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::transaction::RlpEcdsaDecodableTx;
    use alloy_eips::eip2930::{AccessList, AccessListItem};
    use alloy_primitives::{Bytes, address};

    const EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");

    fn policy() -> Policy {
        Policy {
            chain_id: 31_337,
            executor: EXECUTOR,
            max_gas: 1_000_000,
            max_fee_per_gas: 500_000_000_000,
        }
    }

    fn calldata(sel: [u8; 4], words: &[U256]) -> Bytes {
        let mut data = sel.to_vec();
        for word in words {
            data.extend_from_slice(&word.to_be_bytes::<32>());
        }
        data.into()
    }

    /// A plausible rebalance: the pool sends 1 token0 out, the executor returns 2 token1 and allows a 0.004 cut.
    fn rebalance_tx() -> TxEip1559 {
        let e18 = U256::from(10u64).pow(U256::from(18u64));
        TxEip1559 {
            chain_id: 31_337,
            nonce: 7,
            gas_limit: 300_000,
            max_fee_per_gas: 30_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call(EXECUTOR),
            value: U256::ZERO,
            access_list: AccessList::default(),
            input: calldata(
                selector(REBALANCE),
                &[
                    e18,
                    U256::ZERO,
                    U256::ZERO,
                    e18 * U256::from(2u8),
                    e18 / U256::from(250u16),
                ],
            ),
        }
    }

    fn raw(tx: &TxEip1559) -> Vec<u8> {
        let mut out = Vec::new();
        tx.encode_for_signing(&mut out);
        out
    }

    fn vault(allowed_uid: u32, signs_per_sec: f64) -> Vault {
        Vault::new(
            policy(),
            Arc::new(PrivateKeySigner::random()),
            allowed_uid,
            signs_per_sec,
        )
    }

    // ---- the policy ----

    #[test]
    fn the_rebalance_selector_is_the_one_the_cpp_bot_encodes() {
        // bots/inventory-cpp/include/strawberry/abi.hpp: 0xc0661737, checked there against solc's method identifiers.
        assert_eq!(selector(REBALANCE), [0xc0, 0x66, 0x17, 0x37]);
    }

    #[test]
    fn a_rebalance_passes_the_policy() {
        let tx = rebalance_tx();
        assert_eq!(policy().check(&raw(&tx)), Ok(tx));
    }

    #[test]
    fn every_other_transaction_is_denied() {
        let ok = rebalance_tx();
        let e18 = U256::from(10u64).pow(U256::from(18u64));
        let words = |w: [U256; 5]| calldata(selector(REBALANCE), &w);
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
                Denied::To(Address::repeat_byte(0x11)),
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
                    gas_limit: 1_000_001,
                    ..ok.clone()
                },
                Denied::GasLimit(1_000_001),
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
                    input: calldata(selector(SWEEP), &[U256::from(1u8), U256::from(2u8), e18]),
                    ..ok.clone()
                },
                Denied::Sweep,
            ),
            (
                "an ERC-20 transfer",
                TxEip1559 {
                    input: calldata(
                        selector("transfer(address,uint256)"),
                        &[U256::from(1u8), e18],
                    ),
                    ..ok.clone()
                },
                Denied::Selector(selector("transfer(address,uint256)")),
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
                    input: calldata(selector(REBALANCE), &[e18, U256::ZERO, U256::ZERO, e18]),
                    ..ok.clone()
                },
                Denied::CalldataLength(132),
            ),
            (
                "an amount above 2^128",
                TxEip1559 {
                    input: words([
                        e18,
                        U256::ZERO,
                        U256::ZERO,
                        U256::from(u128::MAX) + U256::from(1u8),
                        e18,
                    ]),
                    ..ok.clone()
                },
                Denied::Amount("pay1"),
            ),
            (
                "no output",
                TxEip1559 {
                    input: words([U256::ZERO, U256::ZERO, e18, e18, e18]),
                    ..ok.clone()
                },
                Denied::NoOutput,
            ),
        ];
        for (name, tx, want) in cases {
            assert_eq!(policy().check(&raw(&tx)), Err(want), "{name}");
        }
    }

    #[test]
    fn malformed_and_non_eip1559_payloads_are_denied() {
        let good = raw(&rebalance_tx());
        assert_eq!(policy().check(&[]), Err(Denied::Empty));
        for ty in [0x01, 0x03, 0x04, 0xf8] {
            let mut other = good.clone();
            other[0] = ty;
            assert_eq!(
                policy().check(&other),
                Err(Denied::TxType(ty)),
                "type {ty:#04x}"
            );
        }
        let truncated = &good[..good.len() - 1];
        assert!(matches!(policy().check(truncated), Err(Denied::Rlp(_))));
        let mut trailing = good.clone();
        trailing.push(0x00);
        assert_eq!(policy().check(&trailing), Err(Denied::TrailingBytes(1)));
        // The same list with a non-minimal length prefix (0xf9 0x00 len instead of 0xf8 len) is not accepted.
        assert_eq!(good[1], 0xf8, "a rebalance list has a one-byte length");
        let mut long_header = vec![0x02, 0xf9, 0x00];
        long_header.extend_from_slice(&good[2..]);
        assert!(matches!(policy().check(&long_header), Err(Denied::Rlp(_))));
    }

    #[test]
    fn signing_is_rate_limited() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::new(2.0, t0);
        assert!(bucket.try_take(t0) && bucket.try_take(t0));
        assert!(!bucket.try_take(t0));
        assert!(bucket.try_take(t0 + Duration::from_millis(500)));
        assert!(!bucket.try_take(t0 + Duration::from_millis(500)));
    }

    // ---- signing ----

    #[tokio::test]
    async fn signatures_recover_to_the_vault_and_the_signed_tx_is_valid() {
        let vault = vault(0, 20.0);
        let tx = rebalance_tx();
        let request = json!({ "id": 9, "method": "sign_transaction", "params": { "tx": hex::encode_prefixed(raw(&tx)) } });
        let response = vault.handle(request.to_string().as_bytes()).await;
        assert_eq!(response["id"], 9);
        let result = &response["result"];
        let field = |name: &str| {
            result[name]
                .as_str()
                .unwrap_or_else(|| panic!("{name} in {response}"))
                .to_string()
        };

        let signing_hash: B256 = field("signing_hash").parse().unwrap();
        assert_eq!(signing_hash, keccak256(raw(&tx)));
        let r: B256 = field("r").parse().unwrap();
        let s: B256 = field("s").parse().unwrap();
        let y_parity = result["y_parity"].as_u64().unwrap() == 1;
        let signature = alloy_primitives::Signature::new(r.into(), s.into(), y_parity);
        assert_eq!(
            signature
                .recover_address_from_prehash(&signing_hash)
                .unwrap(),
            vault.signer.address()
        );

        // The signed transaction decodes to the same transaction, recovers to the vault, and hashes to tx_hash.
        let signed_tx = hex::decode(field("signed_tx")).unwrap();
        assert_eq!(signed_tx[0], 0x02);
        let (decoded, decoded_signature) =
            TxEip1559::rlp_decode_with_signature(&mut &signed_tx[1..]).unwrap();
        assert_eq!(decoded, tx);
        assert_eq!(
            decoded_signature
                .recover_address_from_prehash(&decoded.signature_hash())
                .unwrap(),
            vault.signer.address()
        );
        assert_eq!(
            field("tx_hash").parse::<B256>().unwrap(),
            keccak256(&signed_tx)
        );
    }

    #[tokio::test]
    async fn requests_are_parsed_strictly() {
        let vault = vault(0, 20.0);
        let code = |v: Value| v["error"]["code"].as_str().unwrap_or("").to_string();
        assert_eq!(code(vault.handle(b"not json").await), "parse");
        assert_eq!(
            code(
                vault
                    .handle(br#"{"id":1,"method":"address","extra":true}"#)
                    .await
            ),
            "parse"
        );
        assert_eq!(
            code(vault.handle(br#"{"id":1,"method":"export_key"}"#).await),
            "unknown_method"
        );
        assert_eq!(
            code(
                vault
                    .handle(br#"{"id":1,"method":"sign_transaction"}"#)
                    .await
            ),
            "parse"
        );
        assert_eq!(
            code(
                vault
                    .handle(br#"{"id":1,"method":"sign_transaction","params":{"tx":"02f8"}}"#)
                    .await
            ),
            "parse"
        );
        let address = vault.handle(br#"{"id":2,"method":"address"}"#).await;
        assert_eq!(
            address["result"]["address"],
            vault.signer.address().to_string()
        );
    }

    // ---- the socket ----

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tsig-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Sends one line and reads one reply line ("" when the vault closes without replying). A refused peer may see
    /// its write fail with a broken pipe, which is part of being refused, so write errors are not fatal here.
    async fn ask(path: &Path, line: &[u8]) -> String {
        let mut stream = UnixStream::connect(path).await.unwrap();
        let _ = stream.write_all(&[line, b"\n"].concat()).await;
        let mut reply = String::new();
        let _ = BufReader::new(stream).read_line(&mut reply).await;
        reply
    }

    #[tokio::test]
    async fn the_socket_is_private_and_serves_the_protocol() {
        let dir = TempDir::new("serve");
        let path = dir.0.join("vault.sock");
        let uid = own_uid().unwrap();
        let listener = bind_socket(&path, uid, false).unwrap();
        assert_eq!(fs::metadata(&dir.0).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o777, 0o600);

        let vault = Arc::new(vault(uid, 20.0));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, Arc::clone(&vault), async {
            let _ = stopped.await;
        }));

        let reply: Value =
            serde_json::from_str(&ask(&path, br#"{"id":1,"method":"address"}"#).await).unwrap();
        assert_eq!(
            reply["result"]["address"],
            vault.signer.address().to_string()
        );

        let sign = |tx: &TxEip1559| {
            json!({ "id": 2, "method": "sign_transaction", "params": { "tx": hex::encode_prefixed(raw(tx)) } }).to_string()
        };
        let reply: Value =
            serde_json::from_str(&ask(&path, sign(&rebalance_tx()).as_bytes()).await).unwrap();
        assert!(reply["result"]["signed_tx"].is_string(), "{reply}");
        let sweep = TxEip1559 {
            input: calldata(selector(SWEEP), &[U256::ZERO, U256::ZERO, U256::ZERO]),
            ..rebalance_tx()
        };
        let reply: Value =
            serde_json::from_str(&ask(&path, sign(&sweep).as_bytes()).await).unwrap();
        assert_eq!(reply["error"]["code"], "policy");

        // A line longer than MAX_LINE gets one error and the connection is closed.
        let long = vec![b'x'; MAX_LINE + 10];
        let reply: Value = serde_json::from_str(&ask(&path, &long).await).unwrap();
        assert_eq!(reply["error"]["code"], "parse");

        // A second vault refuses to take over a live socket; files that are not sockets are left alone.
        assert!(
            bind_socket(&path, uid, false)
                .unwrap_err()
                .to_string()
                .contains("another process is listening")
        );
        let file = dir.0.join("not-a-socket");
        fs::write(&file, b"").unwrap();
        assert!(
            bind_socket(&file, uid, false)
                .unwrap_err()
                .to_string()
                .contains("not a socket")
        );

        stop.send(()).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_peer_with_another_uid_is_refused() {
        let dir = TempDir::new("uid");
        let path = dir.0.join("vault.sock");
        let uid = own_uid().unwrap();
        let listener = bind_socket(&path, uid, false).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(
            listener,
            Arc::new(vault(uid.wrapping_add(1), 20.0)),
            async {
                let _ = stopped.await;
            },
        ));
        // The connection is closed without a reply.
        assert_eq!(ask(&path, br#"{"id":1,"method":"address"}"#).await, "");
        stop.send(()).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_directory_open_to_other_users_is_refused() {
        let dir = TempDir::new("open");
        fs::create_dir_all(&dir.0).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o777)).unwrap();
        let err = bind_socket(&dir.0.join("vault.sock"), own_uid().unwrap(), false).unwrap_err();
        assert!(err.to_string().contains("open to other users"), "{err}");
    }

    #[tokio::test]
    async fn the_rate_limit_answers_rate_limited() {
        let vault = vault(0, 1.0);
        let request = json!({ "id": 3, "method": "sign_transaction", "params": { "tx": hex::encode_prefixed(raw(&rebalance_tx())) } });
        assert!(vault.handle(request.to_string().as_bytes()).await["result"].is_object());
        assert_eq!(
            vault.handle(request.to_string().as_bytes()).await["error"]["code"],
            "rate_limited"
        );
    }

    #[cfg(feature = "insecure-test-key")]
    #[test]
    fn a_test_key_file_must_be_private_and_well_formed() {
        let dir = TempDir::new("testkey");
        fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("key");
        // The end-to-end tests' key: keccak256 of a public phrase, so no key literal is stored anywhere. Its address
        // was computed independently with viem (privateKeyToAccount).
        let key = keccak256("strawberryswap insecure test key: end-to-end tests only");
        fs::write(&path, format!("{key}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_test_key(&path)
                .unwrap_err()
                .to_string()
                .contains("0600")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_test_key(&path).unwrap().address(),
            address!("0xf8e44d4517A0de6610113165659DaAd8cF9327B2")
        );
        fs::write(&path, "not hex").unwrap();
        assert!(load_test_key(&path).is_err());
    }

    #[test]
    fn the_command_line_requires_chain_and_executor() {
        let parse = |args: &[&str]| Args::parse(args.iter().map(|s| (*s).to_string()));
        assert!(parse(&["--executor", "0x5FbDB2315678afecb367f032d93F642f64180aa3"]).is_err());
        assert!(parse(&["--chain-id", "31337"]).is_err());
        assert!(
            parse(&[
                "--chain-id",
                "31337",
                "--executor",
                "0x0000000000000000000000000000000000000000"
            ])
            .is_err()
        );
        let args = parse(&[
            "--chain-id",
            "31337",
            "--executor",
            "0x5FbDB2315678afecb367f032d93F642f64180aa3",
        ])
        .unwrap();
        assert_eq!(
            (
                args.chain_id,
                args.executor,
                args.max_gas,
                args.signs_per_sec
            ),
            (31_337, EXECUTOR, 1_000_000, 20)
        );
    }
}
