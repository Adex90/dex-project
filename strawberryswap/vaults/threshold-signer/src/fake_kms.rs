//! Tests only: AWS KMS over its awsJson1.1 protocol (GetPublicKey with a DER SubjectPublicKeyInfo; Sign with
//! MessageType DIGEST and ECDSA_SHA_256, a DER signature), with KMS's error shape for anything else. The same stand-in
//! as the oracle crate's (oracle/tests/common), so the vault's KMS path runs without an AWS account. Keys are derived
//! from public labels: nothing here is a key anyone should use.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, keccak256};
use axum::Router;
use axum::body::Bytes as Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use k256::pkcs8::EncodePublicKey;
use serde_json::{Value, json};

use crate::kms::KmsConfig;

/// A deterministic key for tests, derived from a label.
pub fn test_key(label: &str) -> SigningKey {
    SigningKey::from_bytes(&keccak256(label.as_bytes()).0.into()).unwrap()
}

pub fn address_of(key: &SigningKey) -> Address {
    Address::from_public_key(key.verifying_key())
}

pub struct FakeKms {
    pub url: String,
    state: Arc<KmsState>,
}

struct KmsState {
    keys: HashMap<String, SigningKey>,
    /// Answer with the high-s twin of each signature, as KMS may: the signer must normalize it.
    high_s: AtomicBool,
    signed: Mutex<Vec<(String, B256)>>,
}

impl FakeKms {
    pub async fn start(keys: &[(&str, SigningKey)]) -> Self {
        let state = Arc::new(KmsState {
            keys: keys
                .iter()
                .map(|(id, key)| (id.to_string(), key.clone()))
                .collect(),
            high_s: AtomicBool::new(false),
            signed: Mutex::new(Vec::new()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(kms))
            .with_state(Arc::clone(&state));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{address}"),
            state,
        }
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
}

fn kms_reply(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/x-amz-json-1.1")],
        body.to_string(),
    )
        .into_response()
}

fn kms_error(kind: &str, message: &str) -> Response {
    let mut response = kms_reply(
        StatusCode::BAD_REQUEST,
        json!({ "__type": kind, "message": message }),
    );
    response
        .headers_mut()
        .insert("x-amzn-errortype", kind.parse().unwrap());
    response
}

async fn kms(State(state): State<Arc<KmsState>>, headers: HeaderMap, body: Body) -> Response {
    let target = headers
        .get("x-amz-target")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if headers.get(header::AUTHORIZATION).is_none() {
        return kms_error("MissingAuthenticationTokenException", "unsigned request");
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return kms_error("SerializationException", "not JSON");
    };
    let key_id = request["KeyId"].as_str().unwrap_or_default();
    let Some(key) = state.keys.get(key_id) else {
        return kms_error(
            "NotFoundException",
            &format!("Key '{key_id}' does not exist"),
        );
    };
    let arn = format!("arn:aws:kms:us-east-1:000000000000:key/{key_id}");
    match target {
        "TrentService.GetPublicKey" => {
            let der = k256::PublicKey::from(key.verifying_key())
                .to_public_key_der()
                .unwrap();
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
            if request["MessageType"] != "DIGEST" || request["SigningAlgorithm"] != "ECDSA_SHA_256"
            {
                return kms_error(
                    "ValidationException",
                    "expected a DIGEST signed with ECDSA_SHA_256",
                );
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
            state
                .signed
                .lock()
                .unwrap()
                .push((key_id.to_string(), B256::from_slice(&digest)));
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
