//! The notary service over KMS: it signs only a fresh canonical body of its instrument, fetched by itself, and every
//! signature it hands out verifies the way ZkDecoOracle verifies it.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::B256;
use alloy::signers::aws::AwsSigner;
use axum::http::StatusCode;
use common::{FakeKms, Provider, address_of, provider_id, test_key};
use serde_json::Value;
use zk_oracle::body::{CanonicalBody, Price};
use zk_oracle::commit::body_commitment;
use zk_oracle::kms;
use zk_oracle::notary::{NotarizedBody, notary_digest};
use zk_oracle::notary_service::{self, AttestError, Notary, NotaryConfig};
use zk_oracle::unix_now;

fn config(provider: &Provider) -> NotaryConfig {
    NotaryConfig {
        provider_url: provider.url(),
        provider_id: provider_id(),
        symbol: *b"ETHUSD",
        max_age_secs: 5,
        max_ahead_secs: 2,
        request_timeout: Duration::from_secs(2),
        allow_http: true,
    }
}

async fn notary(label: &str) -> (Notary<AwsSigner>, Provider, FakeKms, alloy::primitives::Address) {
    let key = test_key(label);
    let address = address_of(&key);
    let kms = FakeKms::start(&[("notary-key", key)]).await;
    let provider = Provider::start().await;
    let signer = kms::signer(&kms.config("notary-key"), None).await.unwrap();
    (Notary::new(config(&provider), signer).unwrap(), provider, kms, address)
}

fn body(symbol: &str, price: &str, ts: u64) -> String {
    CanonicalBody::new(symbol, Price::parse_quote(price).unwrap(), ts).unwrap().text()
}

#[tokio::test]
async fn a_fresh_canonical_body_is_committed_and_signed_by_kms() {
    let (notary, provider, kms, address) = notary("notary test: attest").await;
    assert_eq!(notary.address(), address);
    let served = body("ETHUSD", "3456.78", unix_now());
    provider.serve(served.clone());

    let notarized = notary.attest().await.unwrap();
    assert_eq!(notarized.body, served);
    assert_eq!(notarized.provider_id, provider_id());
    assert_eq!(notarized.notary, address);
    let parsed = notarized.verify(provider_id(), address).expect("verifies as the contract does");
    assert_eq!(notarized.body_commitment, body_commitment(&parsed.padded()));
    assert_eq!(kms.signed(), vec![("notary-key".to_string(), notary_digest(provider_id(), notarized.body_commitment))]);

    // KMS may answer high s; what the notary hands out is still what the contract accepts.
    kms.set_high_s(true);
    tokio::time::sleep(notary_service::REUSE_WITHIN).await;
    let again = notary.attest().await.unwrap();
    again.verify(provider_id(), address).unwrap();
    assert_eq!(kms.signed().len(), 2);
}

#[tokio::test]
async fn kms_is_asked_at_most_once_a_second() {
    let (notary, provider, kms, _) = notary("notary test: reuse").await;
    provider.serve(body("ETHUSD", "3456.78", unix_now()));
    let notary = Arc::new(notary);
    let calls: Vec<_> = (0..8)
        .map(|_| {
            let notary = Arc::clone(&notary);
            tokio::spawn(async move { notary.attest().await.unwrap() })
        })
        .collect();
    let mut answers = Vec::new();
    for call in calls {
        answers.push(call.await.unwrap());
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "one attestation, handed out eight times");
    assert_eq!(kms.signed().len(), 1);
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn nothing_but_a_fresh_canonical_body_of_the_instrument_is_signed() {
    let (notary, provider, kms, _) = notary("notary test: refusals").await;
    let now = unix_now();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("stale", body("ETHUSD", "3456.78", now - 60).into_bytes()),
        ("from the future", body("ETHUSD", "3456.78", now + 60).into_bytes()),
        ("another instrument", body("BTCUSD", "91234.5", now).into_bytes()),
        ("zero padding on the wire", [body("ETHUSD", "3456.78", now).as_bytes(), &[0]].concat()),
        ("a trailing newline", format!("{}\n", body("ETHUSD", "3456.78", now)).into_bytes()),
        ("a non-canonical price", format!("{{\"symbol\":\"ETHUSD\",\"price\":3456.780,\"ts\":{now}}}").into_bytes()),
        (
            "a JSON number with an exponent",
            format!("{{\"symbol\":\"ETHUSD\",\"price\":3.4e3,\"ts\":{now}}}").into_bytes(),
        ),
        ("reordered keys", format!("{{\"price\":3456.78,\"symbol\":\"ETHUSD\",\"ts\":{now}}}").into_bytes()),
        ("longer than 96 bytes", vec![b' '; 200]),
    ];
    for (case, served) in cases {
        provider.serve(served);
        tokio::time::sleep(notary_service::REUSE_WITHIN).await;
        let error = notary.attest().await.expect_err(case);
        assert!(
            matches!(error, AttestError::Body(_) | AttestError::Symbol | AttestError::Stale { .. }),
            "{case}: {error}"
        );
    }

    provider.fail(StatusCode::SERVICE_UNAVAILABLE);
    assert!(matches!(notary.attest().await, Err(AttestError::Fetch(_))), "the endpoint has no price");
    provider.redirect("http://127.0.0.1:9/v1/ETHUSD");
    assert!(matches!(notary.attest().await, Err(AttestError::Fetch(_))), "redirects are not followed");

    assert!(kms.signed().is_empty(), "KMS signed nothing");
}

#[tokio::test]
async fn the_provider_must_be_fetched_over_https() {
    let key = test_key("notary test: https");
    let kms = FakeKms::start(&[("notary-key", key)]).await;
    let provider = Provider::start().await;
    let signer = kms::signer(&kms.config("notary-key"), None).await.unwrap();
    let mut plain = config(&provider);
    plain.allow_http = false;
    let error = Notary::new(plain, signer).err().expect("a plain-http provider is refused");
    assert!(error.contains("https"), "{error}");
}

#[tokio::test]
async fn the_http_api_attests_and_names_the_notary() {
    let (notary, provider, _kms, address) = notary("notary test: api").await;
    let base = format!("http://{}", common::serve(notary_service::router(Arc::new(notary))).await);
    let http = reqwest::Client::new();

    let info: Value = http.get(format!("{base}/v1/info")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["notary"].as_str().unwrap().parse::<alloy::primitives::Address>().unwrap(), address);
    assert_eq!(info["providerId"].as_str().unwrap().parse::<B256>().unwrap(), provider_id());
    assert_eq!(info["symbol"], "ETHUSD");

    provider.fail(StatusCode::SERVICE_UNAVAILABLE);
    let response = http.post(format!("{base}/v1/attest")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    provider.serve(body("ETHUSD", "3456.78", unix_now() - 60));
    let response = http.post(format!("{base}/v1/attest")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refusal: Value = response.json().await.unwrap();
    assert!(refusal["error"].as_str().unwrap().contains("stamped"), "{refusal}");

    provider.serve(body("ETHUSD", "3456.78", unix_now()));
    let response = http.post(format!("{base}/v1/attest")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let notarized: NotarizedBody = response.json().await.unwrap();
    notarized.verify(provider_id(), address).unwrap();
}
