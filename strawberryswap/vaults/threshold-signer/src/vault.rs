//! The vault: one role's policy, its chain-state rules and its key, behind the socket protocol.
//!
//! Protocol: newline-delimited JSON, one request per line and one response per line (README.md):
//! `{"id": 1, "method": "address"}`, `{"id": 2, "method": "policy"}`,
//! `{"id": 3, "method": "sign_transaction", "params": {"tx": "0x02…"}}`.
//!
//! A signature passes, in order: the role's policy (the transaction itself), the rate limit, the window's caps and the
//! nonce rule (the chain), and only then is the key asked. What was signed is counted in the window and recorded for
//! the nonce rule; every signature and every refusal is in the audit log (stderr).

use std::sync::Arc;
use std::time::Duration;

use alloy_consensus::SignableTransaction;
use alloy_primitives::{Address, B256, hex};
use alloy_signer::Signer;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::chain::Chain;
use crate::policy::{Calls, Policy};
use crate::rules::{NonceDenied, NonceGuard, Usage, Window};

/// Before stopping on a nonce that looks spent by another signer, the chain is read once more after this: a
/// load-balanced RPC can answer one batch from two nodes at different heights.
const CONFIRM_DELAY: Duration = Duration::from_millis(750);

/// A token bucket: at most `per_sec` signatures per second, in bursts of at most that many.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(per_sec: f64, now: Instant) -> Self {
        let capacity = per_sec.max(1.0);
        Self {
            capacity,
            tokens: capacity,
            per_sec,
            last: now,
        }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
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

struct State {
    bucket: TokenBucket,
    nonces: NonceGuard,
    window: Window,
    /// The background check's last failure, logged once until it changes.
    watch_error: Option<String>,
}

pub struct Vault<C> {
    policy: Policy,
    /// The key. Nothing here can read it back: KMS holds it (or, in the insecure test mode, the signer does).
    signer: Arc<dyn Signer + Send + Sync>,
    address: Address,
    /// Where the key is, for the `policy` method: "aws-kms" (or "insecure-test-key").
    key_store: &'static str,
    /// The only uid allowed to connect (checked with SO_PEERCRED, which the kernel fills in).
    allowed_uid: u32,
    signs_per_sec: f64,
    /// The chain the nonce rule reads; none only in the insecure test mode.
    chain: Option<C>,
    /// One signature at a time: the rules decide against the state the previous signature left.
    state: Mutex<State>,
}

/// One request line.
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

pub fn error(id: Option<u64>, code: &str, message: impl Into<String>) -> Value {
    json!({ "id": id, "error": { "code": code, "message": message.into() } })
}

type Refusal = (&'static str, String);

impl<C: Chain> Vault<C> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policy: Policy,
        signer: Arc<dyn Signer + Send + Sync>,
        key_store: &'static str,
        allowed_uid: u32,
        signs_per_sec: f64,
        window: Window,
        nonces: NonceGuard,
        chain: Option<C>,
    ) -> Self {
        let address = signer.address();
        Self {
            policy,
            signer,
            address,
            key_store,
            allowed_uid,
            signs_per_sec,
            chain,
            state: Mutex::new(State {
                bucket: TokenBucket::new(signs_per_sec, Instant::now()),
                nonces,
                window,
                watch_error: None,
            }),
        }
    }

    pub const fn address(&self) -> Address {
        self.address
    }

    pub const fn allowed_uid(&self) -> u32 {
        self.allowed_uid
    }

    #[cfg(test)]
    pub fn set_allowed_uid(&mut self, uid: u32) {
        self.allowed_uid = uid;
    }

    /// What the vault signs and what its rules have left, for its client to check its own transactions against before
    /// asking. Read-only, and nothing in it is secret: the same rules are in the vault's start-up lines.
    async fn describe(&self) -> Value {
        let mut state = self.state.lock().await;
        let used = state.window.used(Instant::now());
        let caps = state.window.caps().clone();
        let role = self.policy.role();
        let mut policy = json!({
            "role": role.name(),
            "chain_id": self.policy.chain_id,
            "to": self.policy.to.to_string(),
            "contract": role.contract(),
            "function": role.function(),
            "selector": hex::encode_prefixed(role.selector()),
            "address": self.address.to_string(),
            "key": self.key_store,
            "max_gas": self.policy.max_gas,
            // Decimal strings: a u128 or a U256 does not fit a JSON number exactly.
            "max_fee_per_gas": self.policy.max_fee_per_gas.to_string(),
            "max_signs_per_sec": self.signs_per_sec,
            "nonce": {
                "reads_chain": state.nonces.reads_chain(),
                "next": state.nonces.next(),
                "max_pending": state.nonces.max_pending(),
                "halted": state.nonces.halted(),
            },
            "window": {
                "secs": state.window.span().as_secs(),
                "caps": {
                    "signatures": caps.signs,
                    "fee_wei": caps.fee_wei.to_string(),
                    "volume0": caps.volume0.map(|v| v.to_string()),
                    "volume1": caps.volume1.map(|v| v.to_string()),
                    "notes": caps.notes,
                },
                "used": {
                    "signatures": used.signs,
                    "fee_wei": used.fee_wei.to_string(),
                    "volume0": used.volume0.to_string(),
                    "volume1": used.volume1.to_string(),
                    "notes": used.notes,
                },
            },
        });
        match &self.policy.calls {
            Calls::Rebalance => {}
            Calls::Submit {
                provider_id,
                query_commitment,
                min_price,
                max_price,
            } => {
                policy["provider_id"] = json!(provider_id.to_string());
                policy["query_commitment"] = json!(query_commitment.to_string());
                policy["min_price"] = json!(min_price.to_string());
                policy["max_price"] = json!(max_price.to_string());
            }
            Calls::SettleBatch { max_notes } => policy["max_notes"] = json!(max_notes),
        }
        policy
    }

    /// One request line in, one response out.
    pub async fn handle(&self, line: &[u8]) -> Value {
        let request: Request = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(e) => return error(None, "parse", format!("not a valid request: {e}")),
        };
        let id = request.id;
        match (request.method.as_str(), request.params) {
            ("address", None) => {
                json!({ "id": id, "result": { "address": self.address.to_string() } })
            }
            ("sign_transaction", Some(params)) => match self.sign(&params.tx).await {
                Ok(result) => json!({ "id": id, "result": result }),
                Err((code, message)) => error(Some(id), code, message),
            },
            ("policy", None) => json!({ "id": id, "result": self.describe().await }),
            ("address", Some(_)) => error(Some(id), "parse", "address takes no params"),
            ("policy", Some(_)) => error(Some(id), "parse", "policy takes no params"),
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

    async fn sign(&self, tx_hex: &str) -> Result<Value, Refusal> {
        let raw = tx_hex
            .strip_prefix("0x")
            .and_then(|digits| hex::decode(digits).ok())
            .ok_or(("parse", "params.tx must be 0x-prefixed hex".to_string()))?;
        let (tx, size) = self.policy.check(&raw).map_err(|denied| {
            eprintln!("audit: denied: {denied}");
            ("policy", denied.to_string())
        })?;
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if !state.bucket.try_take(now) {
            eprintln!("audit: denied: rate limit (nonce {})", tx.nonce);
            return Err(("rate_limited", "too many signatures per second".to_string()));
        }
        let usage = Usage::of(tx.gas_limit, tx.max_fee_per_gas, size);
        if let Err(denied) = state.window.check(now, &usage) {
            eprintln!("audit: denied: window cap: {denied}");
            return Err(("window_cap", denied.to_string()));
        }
        if let Some(chain) = &self.chain
            && state.nonces.needs_read(tx.nonce)
        {
            self.read_chain(chain, &mut state.nonces).await?;
        }
        if let Err(denied) = state.nonces.check(tx.nonce) {
            eprintln!("audit: denied: {denied}");
            return Err((denied.code(), denied.to_string()));
        }

        let signing_hash = tx.signature_hash();
        let signature = self
            .signer
            .sign_hash(&signing_hash)
            .await
            .map_err(|e| ("signer", e.to_string()))?;
        let (nonce, gas_limit, max_fee) = (tx.nonce, tx.gas_limit, tx.max_fee_per_gas);
        let signed = tx.into_signed(signature);
        let mut signed_tx = Vec::with_capacity(signed.eip2718_encoded_length());
        signed.eip2718_encode(&mut signed_tx);
        let tx_hash = *signed.hash();
        state.window.record(now, usage);
        state.nonces.record(nonce, tx_hash);
        let function = self.policy.role().function();
        let name = function.split('(').next().unwrap_or(function);
        eprintln!(
            "audit: signed {name} nonce {nonce} gas {gas_limit} tx {tx_hash} max fee {max_fee}"
        );
        Ok(json!({
            "r": B256::from(signature.r()).to_string(),
            "s": B256::from(signature.s()).to_string(),
            "y_parity": u8::from(signature.v()),
            "signing_hash": signing_hash.to_string(),
            "tx_hash": tx_hash.to_string(),
            "signed_tx": hex::encode_prefixed(&signed_tx),
        }))
    }

    /// Reads the vault's next nonce and the receipts of what it signed, and checks that every nonce the chain moved
    /// past was spent by the vault. A nonce spent by anything else stops the vault, after a second read confirms it.
    async fn read_chain(&self, chain: &C, nonces: &mut NonceGuard) -> Result<(), Refusal> {
        let mut suspicion = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(CONFIRM_DELAY).await;
            }
            let (next, mined) = chain
                .next_nonce_and_mined(self.address, &nonces.unverified())
                .await
                .map_err(|e| {
                    (
                        "chain",
                        format!("cannot read the chain for the nonce rule: {e}"),
                    )
                })?;
            match nonces.audit(next, &mined) {
                Ok(()) => {
                    nonces.advance(next);
                    return Ok(());
                }
                Err(reason) => suspicion = reason,
            }
        }
        eprintln!(
            "audit: ALERT: {suspicion}: the key signs outside this vault; signing stopped until restart"
        );
        nonces.halt(suspicion.clone());
        Err(("halted", NonceDenied::Halted(suspicion).to_string()))
    }

    /// Checks the chain every `every` while idle too, so a key used elsewhere is caught before the next signature.
    pub async fn watch(self: Arc<Self>, every: Duration) {
        let Some(chain) = &self.chain else {
            return;
        };
        let mut ticker = tokio::time::interval(every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let mut state = self.state.lock().await;
            if state.nonces.halted().is_some() {
                continue;
            }
            let result = self.read_chain(chain, &mut state.nonces).await;
            let failure = match result {
                Err(("chain", message)) => Some(message),
                _ => None,
            };
            if failure != state.watch_error {
                match &failure {
                    Some(message) => eprintln!("vault: {message}"),
                    None => eprintln!("vault: the chain answers again"),
                }
                state.watch_error = failure;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chain::ChainError;
    use crate::policy::tests::{
        EXECUTOR, automator, batch, operator, raw, rebalance_tx, relayer, settle_tx, submit_tx,
    };
    use crate::rules::Caps;
    use alloy_consensus::TxEip1559;
    use alloy_consensus::transaction::RlpEcdsaDecodableTx;
    use alloy_primitives::{U256, keccak256};
    use alloy_signer_local::PrivateKeySigner;
    use std::collections::HashMap;

    /// A chain in memory: the vault's next nonce, the mined transactions, code and view answers.
    #[derive(Default)]
    pub struct MockChain {
        pub chain_id: u64,
        pub next: std::sync::Mutex<u64>,
        pub pending: u64,
        pub mined: std::sync::Mutex<Vec<B256>>,
        pub code: HashMap<Address, usize>,
        pub views: HashMap<(Address, [u8; 4]), Vec<u8>>,
        pub reads: std::sync::atomic::AtomicUsize,
        pub down: std::sync::atomic::AtomicBool,
    }

    impl MockChain {
        pub fn mine(&self, tx: B256) {
            self.mined.lock().unwrap().push(tx);
            *self.next.lock().unwrap() += 1;
        }

        fn up(&self) -> Result<(), ChainError> {
            if self.down.load(std::sync::atomic::Ordering::SeqCst) {
                Err(ChainError("RPC: connection refused".into()))
            } else {
                Ok(())
            }
        }
    }

    impl Chain for Arc<MockChain> {
        async fn chain_id(&self) -> Result<u64, ChainError> {
            self.up()?;
            Ok(self.chain_id)
        }

        async fn code_size(&self, at: Address) -> Result<usize, ChainError> {
            self.up()?;
            Ok(self.code.get(&at).copied().unwrap_or(0))
        }

        async fn call(&self, to: Address, input: Vec<u8>) -> Result<Vec<u8>, ChainError> {
            self.up()?;
            let selector: [u8; 4] = input[..4].try_into().unwrap();
            self.views
                .get(&(to, selector))
                .cloned()
                .ok_or_else(|| ChainError("execution reverted".into()))
        }

        async fn nonces(&self, _: Address) -> Result<(u64, u64), ChainError> {
            self.up()?;
            let next = *self.next.lock().unwrap();
            Ok((next, self.pending.max(next)))
        }

        async fn next_nonce_and_mined(
            &self,
            _: Address,
            txs: &[B256],
        ) -> Result<(u64, Vec<B256>), ChainError> {
            self.up()?;
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mined = self.mined.lock().unwrap();
            let found = txs
                .iter()
                .filter(|tx| mined.contains(tx))
                .copied()
                .collect();
            Ok((*self.next.lock().unwrap(), found))
        }
    }

    pub fn caps() -> Caps {
        Caps {
            signs: 100,
            fee_wei: U256::from(10u64).pow(U256::from(18u64)),
            volume0: Some(U256::from(10u64).pow(U256::from(21u64))),
            volume1: Some(U256::from(10u64).pow(U256::from(24u64))),
            notes: Some(1_000),
        }
    }

    pub fn vault_with(
        policy: Policy,
        signs_per_sec: f64,
        caps: Caps,
        chain: Arc<MockChain>,
    ) -> Vault<Arc<MockChain>> {
        let next = *chain.next.lock().unwrap();
        Vault::new(
            policy,
            Arc::new(PrivateKeySigner::random()),
            "aws-kms",
            0,
            signs_per_sec,
            Window::new(Duration::from_secs(3_600), caps),
            NonceGuard::with_chain(next, next, 1),
            Some(chain),
        )
    }

    pub fn vault(policy: Policy) -> Vault<Arc<MockChain>> {
        vault_with(policy, 20.0, caps(), Arc::new(MockChain::default()))
    }

    pub fn sign_request(id: u64, tx: &TxEip1559) -> Vec<u8> {
        json!({ "id": id, "method": "sign_transaction", "params": { "tx": hex::encode_prefixed(raw(tx)) } })
            .to_string()
            .into_bytes()
    }

    fn with_nonce(tx: TxEip1559, nonce: u64) -> TxEip1559 {
        TxEip1559 { nonce, ..tx }
    }

    fn code(v: &Value) -> String {
        v["error"]["code"].as_str().unwrap_or("").to_string()
    }

    #[tokio::test]
    async fn signatures_recover_to_the_vault_and_the_signed_tx_is_valid() {
        for (policy, tx) in [
            (operator(), rebalance_tx()),
            (relayer(), submit_tx()),
            (automator(), settle_tx(batch(2))),
        ] {
            let chain = Arc::new(MockChain::default());
            *chain.next.lock().unwrap() = 7;
            let vault = vault_with(policy, 20.0, caps(), chain);
            let response = vault.handle(&sign_request(9, &tx)).await;
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
                vault.address()
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
                vault.address()
            );
            assert_eq!(
                field("tx_hash").parse::<B256>().unwrap(),
                keccak256(&signed_tx)
            );
        }
    }

    #[tokio::test]
    async fn requests_are_parsed_strictly() {
        let vault = vault(operator());
        assert_eq!(code(&vault.handle(b"not json").await), "parse");
        assert_eq!(
            code(
                &vault
                    .handle(br#"{"id":1,"method":"address","extra":true}"#)
                    .await
            ),
            "parse"
        );
        assert_eq!(
            code(&vault.handle(br#"{"id":1,"method":"export_key"}"#).await),
            "unknown_method"
        );
        assert_eq!(
            code(
                &vault
                    .handle(br#"{"id":1,"method":"sign_transaction"}"#)
                    .await
            ),
            "parse"
        );
        assert_eq!(
            code(
                &vault
                    .handle(br#"{"id":1,"method":"sign_transaction","params":{"tx":"02f8"}}"#)
                    .await
            ),
            "parse"
        );
        let address = vault.handle(br#"{"id":2,"method":"address"}"#).await;
        assert_eq!(address["result"]["address"], vault.address().to_string());
        assert_eq!(
            code(
                &vault
                    .handle(br#"{"id":3,"method":"policy","params":{"tx":"0x"}}"#)
                    .await
            ),
            "parse"
        );
    }

    #[tokio::test]
    async fn the_policy_tells_the_client_what_will_be_signed() {
        let vault = vault(operator());
        let reply = vault.handle(br#"{"id":4,"method":"policy"}"#).await;
        assert_eq!(reply["id"], 4);
        let policy = &reply["result"];
        assert_eq!(policy["role"], "operator");
        assert_eq!(policy["to"], EXECUTOR.to_string());
        assert_eq!(policy["contract"], "SearcherExecutor");
        assert_eq!(policy["selector"], "0xc0661737");
        assert_eq!(policy["address"], vault.address().to_string());
        assert_eq!(policy["key"], "aws-kms");
        assert_eq!(policy["max_gas"], 2_000_000);
        assert_eq!(policy["max_fee_per_gas"], "500000000000");
        assert_eq!(policy["nonce"]["max_pending"], 1);
        assert_eq!(policy["nonce"]["halted"], Value::Null);
        assert_eq!(policy["window"]["caps"]["signatures"], 100);
        assert_eq!(policy["window"]["used"]["signatures"], 0);
        // After a signature, the window shows it.
        let signed = vault
            .handle(&sign_request(5, &with_nonce(rebalance_tx(), 0)))
            .await;
        assert!(signed["result"].is_object(), "{signed}");
        let after = vault.handle(br#"{"id":6,"method":"policy"}"#).await;
        assert_eq!(after["result"]["window"]["used"]["signatures"], 1);
        assert_eq!(
            after["result"]["window"]["used"]["fee_wei"],
            "9000000000000000"
        );
        // The relayer's and the automator's policies say what their calldata must hold.
        let relayer = self::vault(relayer());
        let policy = relayer.handle(br#"{"id":1,"method":"policy"}"#).await["result"].clone();
        assert_eq!(policy["role"], "relayer");
        assert!(policy["provider_id"].is_string() && policy["min_price"].is_string());
        let automator = self::vault(automator());
        let policy = automator.handle(br#"{"id":1,"method":"policy"}"#).await["result"].clone();
        assert_eq!(
            (policy["role"].clone(), policy["max_notes"].clone()),
            (json!("automator"), json!(4))
        );
    }

    #[tokio::test]
    async fn the_nonce_rule_follows_the_chain() {
        let chain = Arc::new(MockChain::default());
        *chain.next.lock().unwrap() = 7;
        let vault = vault_with(operator(), 20.0, caps(), Arc::clone(&chain));
        // The chain's next nonce is 7: 7 is signed, 6 is spent, 8 is ahead.
        let first = vault.handle(&sign_request(1, &rebalance_tx())).await;
        let tx7: B256 = first["result"]["tx_hash"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            code(
                &vault
                    .handle(&sign_request(2, &with_nonce(rebalance_tx(), 6)))
                    .await
            ),
            "nonce"
        );
        let ahead = vault
            .handle(&sign_request(3, &with_nonce(rebalance_tx(), 8)))
            .await;
        assert_eq!(code(&ahead), "nonce", "{ahead}");
        assert!(
            ahead["error"]["message"]
                .as_str()
                .unwrap()
                .contains("ahead")
        );
        // A replacement of 7 is signed.
        assert!(vault.handle(&sign_request(4, &rebalance_tx())).await["result"].is_object());
        // Once 7 is mined (by the vault's transaction), 8 is signed.
        chain.mine(tx7);
        assert!(
            vault
                .handle(&sign_request(5, &with_nonce(rebalance_tx(), 8)))
                .await["result"]
                .is_object()
        );
    }

    #[tokio::test]
    async fn a_nonce_spent_by_another_signer_stops_the_vault() {
        let chain = Arc::new(MockChain::default());
        *chain.next.lock().unwrap() = 3;
        let vault = vault_with(operator(), 20.0, caps(), Arc::clone(&chain));
        assert!(
            vault
                .handle(&sign_request(1, &with_nonce(rebalance_tx(), 3)))
                .await["result"]
                .is_object()
        );
        // Nonce 3 is spent by a transaction the vault did not sign.
        chain.mine(B256::repeat_byte(0xee));
        let refused = vault
            .handle(&sign_request(2, &with_nonce(rebalance_tx(), 4)))
            .await;
        assert_eq!(code(&refused), "halted", "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("nonce 3")
        );
        // Read twice (the second read confirms), and stopped for good: even a valid nonce is refused, unread.
        assert_eq!(chain.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            code(
                &vault
                    .handle(&sign_request(3, &with_nonce(rebalance_tx(), 4)))
                    .await
            ),
            "halted"
        );
        assert_eq!(chain.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        let policy = vault.handle(br#"{"id":4,"method":"policy"}"#).await;
        assert!(
            policy["result"]["nonce"]["halted"]
                .as_str()
                .unwrap()
                .contains("nonce 3")
        );
    }

    #[tokio::test]
    async fn a_chain_that_cannot_be_read_refuses_rather_than_guesses() {
        let chain = Arc::new(MockChain::default());
        let vault = vault_with(operator(), 20.0, caps(), Arc::clone(&chain));
        chain.down.store(true, std::sync::atomic::Ordering::SeqCst);
        // Nonce 0 is the cached next nonce: no read needed. Nonce 1 needs one, and the chain is down.
        assert!(
            vault
                .handle(&sign_request(1, &with_nonce(rebalance_tx(), 0)))
                .await["result"]
                .is_object()
        );
        let refused = vault
            .handle(&sign_request(2, &with_nonce(rebalance_tx(), 1)))
            .await;
        assert_eq!(code(&refused), "chain", "{refused}");
        assert!(
            !refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("halt")
        );
    }

    #[tokio::test]
    async fn the_window_caps_answer_window_cap() {
        let mut tight = caps();
        tight.signs = 2;
        let vault = vault_with(operator(), 20.0, tight, Arc::new(MockChain::default()));
        for id in 0..2 {
            assert!(
                vault
                    .handle(&sign_request(id, &with_nonce(rebalance_tx(), 0)))
                    .await["result"]
                    .is_object()
            );
        }
        let refused = vault
            .handle(&sign_request(3, &with_nonce(rebalance_tx(), 0)))
            .await;
        assert_eq!(code(&refused), "window_cap", "{refused}");
        // The volume caps: a rebalance moving more token1 than a window allows.
        let mut small = caps();
        small.volume1 = Some(U256::from(10u64).pow(U256::from(18u64)));
        let vault = vault_with(operator(), 20.0, small, Arc::new(MockChain::default()));
        let refused = vault
            .handle(&sign_request(1, &with_nonce(rebalance_tx(), 0)))
            .await;
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("token1"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn the_rate_limit_answers_rate_limited() {
        let vault = vault_with(operator(), 1.0, caps(), Arc::new(MockChain::default()));
        let tx = with_nonce(rebalance_tx(), 0);
        assert!(vault.handle(&sign_request(3, &tx)).await["result"].is_object());
        assert_eq!(
            code(&vault.handle(&sign_request(4, &tx)).await),
            "rate_limited"
        );
    }

    #[tokio::test]
    async fn refusals_by_the_policy_name_the_rule() {
        let vault = vault(operator());
        let refused = vault
            .handle(&sign_request(1, &with_nonce(submit_tx(), 0)))
            .await;
        assert_eq!(code(&refused), "policy");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("relayer role's function"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn the_watcher_catches_a_foreign_nonce_while_idle() {
        let chain = Arc::new(MockChain::default());
        let vault = Arc::new(vault_with(operator(), 20.0, caps(), Arc::clone(&chain)));
        chain.mine(B256::repeat_byte(0x01));
        let watcher = tokio::spawn(Arc::clone(&vault).watch(Duration::from_millis(20)));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let policy = vault.handle(br#"{"id":1,"method":"policy"}"#).await;
            if policy["result"]["nonce"]["halted"].is_string() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the watcher never stopped the vault"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        watcher.abort();
    }
}
