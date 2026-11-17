//! The notary service: on `POST /v1/attest` it fetches the provider's body over TLS, checks it is a fresh canonical
//! body of the expected instrument, commits to it and has KMS sign `ZkDecoOracle`'s digest.
//!
//! This is model (a) of circuits/zkdeco_oracle: the notary is trusted to have seen the body come from the provider,
//! and the circuit proves everything that follows from the committed body. The notary fetches the body itself, so a
//! relayer can never hand it a body to sign. On testnet Rothbard operates it: the trusted bootstrap of
//! docs/ORACLE.md.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use alloy::signers::Signer;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::json;
use tokio::sync::Mutex;

use crate::body::{BODY_LEN, BodyError, CanonicalBody, SYMBOL_LEN};
use crate::commit::body_commitment;
use crate::notary::{NotarizedBody, encode_signature, notary_digest, recover};
use crate::{error_chain, unix_now};

#[derive(Clone, Debug)]
pub struct NotaryConfig {
    /// The provider's URL, e.g. https://price.testnet.<domain>/v1/ETHUSD.
    pub provider_url: String,
    /// The provider's registry key in ZkDecoOracle.
    pub provider_id: B256,
    pub symbol: [u8; SYMBOL_LEN],
    /// A body stamped more than this many seconds before the notary's clock is refused.
    pub max_age_secs: u64,
    /// And so is one stamped more than this many seconds after it.
    pub max_ahead_secs: u64,
    pub request_timeout: Duration,
    /// Plain-http provider URLs are refused unless this is set (local tests only: the notary's point is TLS).
    pub allow_http: bool,
}

#[derive(Debug)]
pub enum AttestError {
    Fetch(String),
    Body(BodyError),
    /// The provider served a valid body for another instrument.
    Symbol,
    /// The body's timestamp is outside the notary's window.
    Stale {
        ts: u64,
        now: u64,
    },
    Sign(String),
}

impl fmt::Display for AttestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(m) => write!(f, "provider: {m}"),
            Self::Body(e) => write!(f, "provider body: {e}"),
            Self::Symbol => f.write_str("provider body: another instrument"),
            Self::Stale { ts, now } => write!(f, "provider body stamped {ts}, notary clock {now}"),
            Self::Sign(m) => write!(f, "signing: {m}"),
        }
    }
}

/// A request within this long of the last attestation gets that attestation again: at most one KMS call per second,
/// however often the notary is asked (a body's timestamp has 1 s resolution anyway).
pub const REUSE_WITHIN: Duration = Duration::from_secs(1);

pub struct Notary<S> {
    config: NotaryConfig,
    signer: S,
    client: reqwest::Client,
    /// One attestation at a time, and the last one with when it was made.
    last: Mutex<Option<(Instant, NotarizedBody)>>,
}

impl<S: Signer + Send + Sync> Notary<S> {
    pub fn new(config: NotaryConfig, signer: S) -> Result<Self, String> {
        if !config.allow_http && !config.provider_url.starts_with("https://") {
            return Err(format!("{}: the provider must be fetched over https", config.provider_url));
        }
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!config.allow_http)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { config, signer, client, last: Mutex::new(None) })
    }

    /// The notary's address: what ZkDecoOracle must have registered for the provider.
    pub fn address(&self) -> Address {
        self.signer.address()
    }

    pub fn config(&self) -> &NotaryConfig {
        &self.config
    }

    /// The body exactly as served, at most 96 bytes.
    async fn fetch(&self) -> Result<Vec<u8>, AttestError> {
        let mut response =
            self.client.get(&self.config.provider_url).send().await.map_err(|e| AttestError::Fetch(error_chain(&e)))?;
        if !response.status().is_success() {
            return Err(AttestError::Fetch(format!("HTTP {}", response.status())));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| AttestError::Fetch(error_chain(&e)))? {
            body.extend_from_slice(&chunk);
            if body.len() > BODY_LEN {
                return Err(AttestError::Body(BodyError::Layout("longer than 96 bytes")));
            }
        }
        Ok(body)
    }

    /// Fetch, check, commit and sign; or the last attestation, if it is less than a second old.
    pub async fn attest(&self) -> Result<NotarizedBody, AttestError> {
        let mut last = self.last.lock().await;
        if let Some((at, notarized)) = last.as_ref()
            && at.elapsed() < REUSE_WITHIN
        {
            return Ok(notarized.clone());
        }
        let notarized = self.attest_now().await?;
        *last = Some((Instant::now(), notarized.clone()));
        Ok(notarized)
    }

    async fn attest_now(&self) -> Result<NotarizedBody, AttestError> {
        let raw = self.fetch().await?;
        let body = CanonicalBody::parse(&raw).map_err(AttestError::Body)?;
        if body.text().as_bytes() != raw.as_slice() {
            return Err(AttestError::Body(BodyError::Layout("padding or other bytes around the object")));
        }
        if body.symbol() != self.config.symbol {
            return Err(AttestError::Symbol);
        }
        let (ts, now) = (body.observed_at(), unix_now());
        if ts.saturating_add(self.config.max_age_secs) < now || ts > now.saturating_add(self.config.max_ahead_secs) {
            return Err(AttestError::Stale { ts, now });
        }
        let commitment = body_commitment(&body.padded());
        let digest = notary_digest(self.config.provider_id, commitment);
        let signature = self.signer.sign_hash(&digest).await.map_err(|e| AttestError::Sign(e.to_string()))?;
        let encoded = encode_signature(&signature);
        // Never hand out a signature the contract would not accept from this notary.
        match recover(&encoded, digest) {
            Ok(address) if address == self.signer.address() => {}
            Ok(address) => return Err(AttestError::Sign(format!("the signature recovers to {address}"))),
            Err(e) => return Err(AttestError::Sign(e.to_string())),
        }
        Ok(NotarizedBody {
            provider_id: self.config.provider_id,
            body: body.text(),
            body_commitment: commitment,
            signature: encoded.to_vec().into(),
            notary: self.signer.address(),
        })
    }
}

/// `POST /v1/attest` and `GET /v1/info`.
pub fn router<S: Signer + Send + Sync + 'static>(notary: Arc<Notary<S>>) -> Router {
    Router::new().route("/v1/attest", post(attest::<S>)).route("/v1/info", get(info::<S>)).with_state(notary)
}

async fn attest<S: Signer + Send + Sync + 'static>(State(notary): State<Arc<Notary<S>>>) -> Response {
    match notary.attest().await {
        Ok(notarized) => Json(notarized).into_response(),
        Err(e) => {
            let status = match e {
                AttestError::Fetch(_) => StatusCode::BAD_GATEWAY,
                AttestError::Body(_) | AttestError::Symbol | AttestError::Stale { .. } => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                AttestError::Sign(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            eprintln!("attest refused: {e}");
            (status, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

async fn info<S: Signer + Send + Sync + 'static>(State(notary): State<Arc<Notary<S>>>) -> Response {
    Json(json!({
        "notary": notary.address(),
        "providerId": notary.config.provider_id,
        "symbol": String::from_utf8_lossy(&notary.config.symbol),
    }))
    .into_response()
}
