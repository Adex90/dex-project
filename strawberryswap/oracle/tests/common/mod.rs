//! Test stand-ins: AWS KMS (its real wire protocol, so the production AwsSigner code path runs), the three exchanges,
//! the price endpoint as the notary sees it, and a recording chain.
#![allow(dead_code)] // each test binary uses a part

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use axum::Router;
use axum::body::Bytes as Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use k256::pkcs8::EncodePublicKey;
use serde_json::{Value, json};
use zk_oracle::body::{CanonicalBody, Price};
use zk_oracle::chain::{Attestation, ChainError, FeedChain, FeedSetup, Pushed};
use zk_oracle::commit::{det, query_commitment};
use zk_oracle::kms::KmsConfig;
use zk_oracle::prover::Prover;
use zk_oracle::sources::Exchange;
use zk_oracle::unix_now;

/// The testnet fixtures' provider id and query salt (circuits/scripts/gen-feed-fixtures.mjs).
pub fn provider_id() -> B256 {
    B256::from(det("StrawberrySwap.testnet.price-endpoint"))
}

pub fn salt() -> U256 {
    det("StrawberrySwap.testnet.ETHUSD.salt")
}

pub fn ethusd_commitment() -> B256 {
    query_commitment(b"ETHUSD", salt()).unwrap()
}

/// Serves `app` on a free local port until the test's runtime ends.
pub async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A deterministic key for tests, derived from a label. Test-only: no real key is ever written down.
pub fn test_key(label: &str) -> SigningKey {
    SigningKey::from_bytes(&keccak256(label.as_bytes()).0.into()).unwrap()
}

pub fn address_of(key: &SigningKey) -> Address {
    Address::from_public_key(key.verifying_key())
}

// ---------------------------------------------------------------------------------------------------------------------
// AWS KMS
// ---------------------------------------------------------------------------------------------------------------------

/// AWS KMS over its awsJson1.1 protocol: GetPublicKey (DER SubjectPublicKeyInfo) and Sign (MessageType DIGEST,
/// ECDSA_SHA_256, DER signature), with KMS's error shape for anything else.
pub struct FakeKms {
    pub url: String,
    state: Arc<KmsState>,
}

struct KmsState {
    keys: HashMap<String, SigningKey>,
    /// Answer with the high-s twin of each signature, as KMS may: the signer must normalize it.
    high_s: AtomicBool,
    signed: Mutex<Vec<(String, B256)>>,
    public_key_reads: AtomicUsize,
}

impl FakeKms {
    pub async fn start(keys: &[(&str, SigningKey)]) -> Self {
        let state = Arc::new(KmsState {
            keys: keys.iter().map(|(id, key)| (id.to_string(), key.clone())).collect(),
            high_s: AtomicBool::new(false),
            signed: Mutex::new(Vec::new()),
            public_key_reads: AtomicUsize::new(0),
        });
        let address = serve(Router::new().route("/", post(kms)).with_state(Arc::clone(&state))).await;
        Self { url: format!("http://{address}"), state }
    }

    /// A client configuration for one of the keys (static test credentials, no AWS account involved).
    pub fn config(&self, key_id: &str) -> KmsConfig {
        KmsConfig {
            key_id: key_id.to_string(),
            endpoint: Some(self.url.clone()),
            region: Some("us-east-1".into()),
            static_credentials: Some(("AKIDFAKEKMSTEST".into(), "fake-kms-test-secret".into())),
        }
    }

    pub fn set_high_s(&self, on: bool) {
        self.state.high_s.store(on, Ordering::SeqCst);
    }

    /// The digests signed so far, with the key that signed each.
    pub fn signed(&self) -> Vec<(String, B256)> {
        self.state.signed.lock().unwrap().clone()
    }

    pub fn public_key_reads(&self) -> usize {
        self.state.public_key_reads.load(Ordering::SeqCst)
    }
}

fn kms_reply(status: StatusCode, body: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/x-amz-json-1.1")], body.to_string()).into_response()
}

fn kms_error(kind: &str, message: &str) -> Response {
    let mut response = kms_reply(StatusCode::BAD_REQUEST, json!({ "__type": kind, "message": message }));
    response.headers_mut().insert("x-amzn-errortype", kind.parse().unwrap());
    response
}

async fn kms(State(state): State<Arc<KmsState>>, headers: HeaderMap, body: Body) -> Response {
    let target = headers.get("x-amz-target").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if headers.get(header::AUTHORIZATION).is_none() {
        return kms_error("MissingAuthenticationTokenException", "unsigned request");
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return kms_error("SerializationException", "not JSON");
    };
    let key_id = request["KeyId"].as_str().unwrap_or_default();
    let Some(key) = state.keys.get(key_id) else {
        return kms_error("NotFoundException", &format!("Key '{key_id}' does not exist"));
    };
    let arn = format!("arn:aws:kms:us-east-1:000000000000:key/{key_id}");
    match target {
        "TrentService.GetPublicKey" => {
            state.public_key_reads.fetch_add(1, Ordering::SeqCst);
            let der = k256::PublicKey::from(key.verifying_key()).to_public_key_der().unwrap();
            kms_reply(
                StatusCode::OK,
                json!({
                    "KeyId": arn,
                    "PublicKey": B64.encode(der.as_bytes()),
                    "KeySpec": "ECC_SECG_P256K1",
                    "CustomerMasterKeySpec": "ECC_SECG_P256K1",
                    "KeyUsage": "SIGN_VERIFY",
                    "SigningAlgorithms": ["ECDSA_SHA_256"],
                }),
            )
        }
        "TrentService.Sign" => {
            if request["MessageType"] != "DIGEST" || request["SigningAlgorithm"] != "ECDSA_SHA_256" {
                return kms_error("ValidationException", "expected a DIGEST signed with ECDSA_SHA_256");
            }
            let digest = match request["Message"].as_str().map(|m| B64.decode(m)) {
                Some(Ok(digest)) if digest.len() == 32 => digest,
                _ => return kms_error("ValidationException", "a digest is 32 bytes"),
            };
            let signature: Signature = key.sign_prehash(&digest).unwrap();
            let signature = if state.high_s.load(Ordering::SeqCst) {
                Signature::from_scalars(signature.r(), -signature.s()).unwrap()
            } else {
                signature
            };
            state.signed.lock().unwrap().push((key_id.to_string(), B256::from_slice(&digest)));
            kms_reply(
                StatusCode::OK,
                json!({
                    "KeyId": arn,
                    "Signature": B64.encode(signature.to_der().as_bytes()),
                    "SigningAlgorithm": "ECDSA_SHA_256",
                }),
            )
        }
        other => kms_error("UnknownOperationException", other),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Exchanges
// ---------------------------------------------------------------------------------------------------------------------

/// The three exchanges' public ETH-USD tickers, on one local server. An exchange without a price answers 503.
#[derive(Clone)]
pub struct FakeExchanges {
    pub base: String,
    prices: Arc<Mutex<HashMap<Exchange, String>>>,
}

impl FakeExchanges {
    pub async fn start() -> Self {
        let prices: Arc<Mutex<HashMap<Exchange, String>>> = Arc::default();
        let quote = |exchange: Exchange| {
            move |State(prices): State<Arc<Mutex<HashMap<Exchange, String>>>>| async move {
                let Some(price) = prices.lock().unwrap().get(&exchange).cloned() else {
                    return (StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
                };
                axum::Json(match exchange {
                    Exchange::Coinbase => json!({ "price": price, "time": "2026-11-16T00:00:00Z" }),
                    Exchange::Kraken => json!({ "error": [], "result": { "XETHZUSD": { "c": [price, "0.010"] } } }),
                    Exchange::Bitstamp => json!({ "last": price, "timestamp": "1779000000" }),
                })
                .into_response()
            }
        };
        let app = Router::new()
            .route("/products/ETH-USD/ticker", get(quote(Exchange::Coinbase)))
            .route("/0/public/Ticker", get(quote(Exchange::Kraken)))
            .route("/api/v2/ticker/ethusd/", get(quote(Exchange::Bitstamp)))
            .with_state(Arc::clone(&prices));
        let address = serve(app).await;
        Self { base: format!("http://{address}"), prices }
    }

    pub fn set(&self, exchange: Exchange, price: Option<&str>) {
        let mut prices = self.prices.lock().unwrap();
        match price {
            Some(p) => prices.insert(exchange, p.to_string()),
            None => prices.remove(&exchange),
        };
    }

    pub fn set_all(&self, coinbase: Option<&str>, kraken: Option<&str>, bitstamp: Option<&str>) {
        self.set(Exchange::Coinbase, coinbase);
        self.set(Exchange::Kraken, kraken);
        self.set(Exchange::Bitstamp, bitstamp);
    }

    pub fn sources(&self) -> Vec<(Exchange, String)> {
        Exchange::ALL.iter().map(|&e| (e, self.base.clone())).collect()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The price endpoint, as the notary fetches it
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone)]
enum Served {
    Body(Vec<u8>),
    /// The canonical ETHUSD body of this price, stamped with the time of each request (as the endpoint serves it).
    Live(String),
    Status(StatusCode),
    Redirect(String),
}

/// Serves `GET /v1/<anything>` with whatever the test sets: a body, an error status or a redirect.
#[derive(Clone)]
pub struct Provider {
    pub base: String,
    served: Arc<Mutex<Served>>,
    pub requests: Arc<AtomicUsize>,
}

impl Provider {
    pub async fn start() -> Self {
        let served = Arc::new(Mutex::new(Served::Status(StatusCode::SERVICE_UNAVAILABLE)));
        let requests = Arc::new(AtomicUsize::new(0));
        let state = (Arc::clone(&served), Arc::clone(&requests));
        let app = Router::new()
            .route(
                "/v1/{symbol}",
                get(|State((served, requests)): State<(Arc<Mutex<Served>>, Arc<AtomicUsize>)>| async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    let served = served.lock().unwrap().clone();
                    match served {
                        Served::Body(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
                        Served::Live(price) => {
                            let price = Price::parse_quote(&price).unwrap();
                            let body = CanonicalBody::new("ETHUSD", price, unix_now()).unwrap().text();
                            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
                        }
                        Served::Status(status) => (status, "unavailable").into_response(),
                        Served::Redirect(to) => (StatusCode::FOUND, [(header::LOCATION, to)]).into_response(),
                    }
                }),
            )
            .with_state(state);
        let address = serve(app).await;
        Self { base: format!("http://{address}"), served, requests }
    }

    pub fn url(&self) -> String {
        format!("{}/v1/ETHUSD", self.base)
    }

    pub fn serve(&self, body: impl Into<Vec<u8>>) {
        *self.served.lock().unwrap() = Served::Body(body.into());
    }

    /// Serve `price`, stamped afresh on each request.
    pub fn live(&self, price: &str) {
        *self.served.lock().unwrap() = Served::Live(price.to_string());
    }

    pub fn fail(&self, status: StatusCode) {
        *self.served.lock().unwrap() = Served::Status(status);
    }

    pub fn redirect(&self, to: &str) {
        *self.served.lock().unwrap() = Served::Redirect(to.to_string());
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Prover and chain
// ---------------------------------------------------------------------------------------------------------------------

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The real prover: node, circuits/scripts/prove-zkdeco.mjs and the committed artifacts in zk/.
pub fn prover() -> Prover {
    let circuits = crate_dir().join("../circuits");
    assert!(
        circuits.join("node_modules/snarkjs").is_dir(),
        "snarkjs is missing: run `npm ci` in strawberryswap/circuits"
    );
    Prover::new(
        "node".into(),
        circuits.join("scripts/prove-zkdeco.mjs"),
        &crate_dir().join("zk"),
        Duration::from_secs(120),
    )
}

pub fn zk_dir() -> PathBuf {
    crate_dir().join("zk")
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A chain that records what the relayer does. Preflight answers are scripted (default: accepted).
pub struct MockChain {
    pub setup: FeedSetup,
    pub latest: Mutex<(U256, u64)>,
    pub preflight_answers: Mutex<VecDeque<Result<(), ChainError>>>,
    pub preflights: AtomicUsize,
    pub submitted: Mutex<Vec<(Attestation, Bytes)>>,
}

impl MockChain {
    pub fn new(notary: Address) -> Self {
        Self {
            setup: FeedSetup {
                provider_id: provider_id(),
                query_commitment: ethusd_commitment(),
                notary,
                max_submission_delay: 30,
            },
            latest: Mutex::new((U256::ZERO, 0)),
            preflight_answers: Mutex::new(VecDeque::new()),
            preflights: AtomicUsize::new(0),
            submitted: Mutex::new(Vec::new()),
        }
    }

    pub fn answer_preflight(&self, answer: Result<(), ChainError>) {
        self.preflight_answers.lock().unwrap().push_back(answer);
    }

    pub fn submitted(&self) -> Vec<(Attestation, Bytes)> {
        self.submitted.lock().unwrap().clone()
    }
}

impl FeedChain for MockChain {
    async fn setup(&self) -> Result<FeedSetup, ChainError> {
        Ok(self.setup)
    }

    async fn latest(&self) -> Result<(U256, u64), ChainError> {
        Ok(*self.latest.lock().unwrap())
    }

    async fn preflight(&self, _: &Attestation, _: &Bytes) -> Result<(), ChainError> {
        self.preflights.fetch_add(1, Ordering::SeqCst);
        self.preflight_answers.lock().unwrap().pop_front().unwrap_or(Ok(()))
    }

    async fn submit(&self, attestation: &Attestation, proof: &Bytes) -> Result<Pushed, ChainError> {
        let mut submitted = self.submitted.lock().unwrap();
        submitted.push((attestation.clone(), proof.clone()));
        *self.latest.lock().unwrap() = (attestation.price, attestation.observedAt);
        Ok(Pushed { tx_hash: keccak256(proof), gas_used: 259_974, block: submitted.len() as u64 })
    }
}
