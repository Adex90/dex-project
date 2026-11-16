//! The relayer against a recording chain, with the real notary (over KMS) and the real prover (snarkjs on the
//! committed artifacts): what it pushes is exactly what ZkPriceFeed checks, and it pushes nothing the feed would
//! refuse.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use axum::http::StatusCode;
use common::{FakeKms, MockChain, Provider, address_of, ethusd_commitment, provider_id, salt, test_key};
use zk_oracle::body::{CanonicalBody, Price};
use zk_oracle::chain::{ChainError, FeeCaps, RpcFeed};
use zk_oracle::commit::body_commitment;
use zk_oracle::kms;
use zk_oracle::notary::{PROOF_LEN, notary_digest, recover};
use zk_oracle::notary_service::{self, Notary, NotaryConfig};
use zk_oracle::prover::{Prover, ProverError};
use zk_oracle::relayer::{self, Outcome, RelayerConfig};
use zk_oracle::unix_now;

struct Pipeline {
    provider: Provider,
    _kms: FakeKms,
    notary: Address,
    config: RelayerConfig,
    http: reqwest::Client,
    prover: Prover,
}

async fn pipeline(label: &str) -> Pipeline {
    let key = test_key(label);
    let kms = FakeKms::start(&[("notary-key", key.clone())]).await;
    let provider = Provider::start().await;
    let signer = kms::signer(&kms.config("notary-key"), None).await.unwrap();
    let notary = Notary::new(
        NotaryConfig {
            provider_url: provider.url(),
            provider_id: provider_id(),
            symbol: *b"ETHUSD",
            max_age_secs: 60,
            max_ahead_secs: 2,
            request_timeout: Duration::from_secs(2),
            allow_http: true,
        },
        signer,
    )
    .unwrap();
    let notary_url = format!("http://{}", common::serve(notary_service::router(Arc::new(notary))).await);
    Pipeline {
        provider,
        _kms: kms,
        notary: address_of(&key),
        config: RelayerConfig {
            notary_url,
            symbol: *b"ETHUSD",
            salt: salt(),
            notary_timeout: Duration::from_secs(5),
            interval: Duration::from_millis(1_500),
        },
        http: reqwest::Client::new(),
        prover: common::prover(),
    }
}

impl Pipeline {
    async fn cycle(&self, chain: &MockChain) -> Outcome {
        relayer::cycle(&self.config, &chain.setup, chain, &self.http, &self.prover).await
    }
}

fn wad(price: &str) -> U256 {
    Price::parse_quote(price).unwrap().wad()
}

#[tokio::test]
async fn a_cycle_proves_and_pushes_exactly_what_the_feed_checks() {
    let p = pipeline("relayer test: push").await;
    let ts = unix_now();
    let served = CanonicalBody::new("ETHUSD", Price::parse_quote("3456.78").unwrap(), ts).unwrap();
    p.provider.serve(served.text());
    let chain = MockChain::new(p.notary);
    assert_eq!(relayer::check_setup(&p.config, &chain, &p.http).await.unwrap(), chain.setup);

    let outcome = p.cycle(&chain).await;
    let Outcome::Pushed { observed_at, price, pushed, .. } = outcome else { panic!("{outcome}") };
    assert_eq!((observed_at, price), (ts, wad("3456.78")));
    assert_eq!(pushed.gas_used, 259_974);

    let submitted = chain.submitted();
    assert_eq!(submitted.len(), 1);
    let (attestation, proof) = &submitted[0];
    assert_eq!(attestation.providerId, provider_id());
    assert_eq!(attestation.queryCommitment, ethusd_commitment());
    assert_eq!(attestation.price, wad("3456.78"));
    assert_eq!(attestation.observedAt, ts);
    // ZkDecoOracle's wire format: snark ‖ bodyCommitment ‖ r ‖ s ‖ v, signed by the registered notary.
    assert_eq!(proof.len(), PROOF_LEN);
    let commitment = body_commitment(&served.padded());
    assert_eq!(&proof[256..288], commitment.as_slice());
    assert_eq!(recover(&proof[288..], notary_digest(provider_id(), commitment)).unwrap(), p.notary);

    // The same observation again: nothing to push.
    let outcome = p.cycle(&chain).await;
    assert!(
        matches!(outcome, Outcome::NothingNew { observed_at, latest } if observed_at == ts && latest == ts),
        "{outcome}"
    );
    assert_eq!(chain.submitted().len(), 1);
}

#[tokio::test]
async fn the_setup_check_catches_a_wrong_salt_feed_or_notary() {
    let p = pipeline("relayer test: setup").await;
    let mut wrong_salt = p.config.clone();
    wrong_salt.salt = salt() + U256::from(1);
    let error = relayer::check_setup(&wrong_salt, &MockChain::new(p.notary), &p.http).await.unwrap_err();
    assert!(error.contains("wrong salt"), "{error}");

    let error = relayer::check_setup(&p.config, &MockChain::new(Address::repeat_byte(7)), &p.http).await.unwrap_err();
    assert!(error.contains("registered"), "{error}");

    let error = relayer::check_setup(&p.config, &MockChain::new(Address::ZERO), &p.http).await.unwrap_err();
    assert!(error.contains("no notary registered"), "{error}");

    let mut nowhere = p.config.clone();
    nowhere.notary_url = "http://127.0.0.1:9".into();
    let error = relayer::check_setup(&nowhere, &MockChain::new(p.notary), &p.http).await.unwrap_err();
    assert!(error.contains("notary info"), "{error}");
}

#[tokio::test]
async fn nothing_is_pushed_that_the_feed_would_refuse() {
    let p = pipeline("relayer test: refusals").await;

    // The endpoint has no price: the notary attests nothing, the cycle fails before proving.
    p.provider.fail(StatusCode::SERVICE_UNAVAILABLE);
    let chain = MockChain::new(p.notary);
    let outcome = p.cycle(&chain).await;
    assert!(matches!(&outcome, Outcome::Failed(m) if m.contains("502")), "{outcome}");

    // A notary other than the registered one: refused before proving, as ZkDecoOracle would.
    p.provider.live("3456.78");
    let impostor = MockChain::new(Address::repeat_byte(7));
    let outcome = p.cycle(&impostor).await;
    assert!(matches!(&outcome, Outcome::Refused(m) if m.contains("notarized body")), "{outcome}");
    assert_eq!(impostor.preflights.load(Ordering::SeqCst), 0);

    // The feed's preflight refuses: no transaction.
    chain.answer_preflight(Err(ChainError::Reverted("PriceOutOfBounds".into())));
    let outcome = p.cycle(&chain).await;
    assert!(matches!(&outcome, Outcome::Refused(m) if m.contains("PriceOutOfBounds")), "{outcome}");

    assert!(chain.submitted().is_empty() && impostor.submitted().is_empty());
}

#[tokio::test]
async fn the_cycle_waits_for_the_chains_clock_then_pushes() {
    let p = pipeline("relayer test: clock").await;
    p.provider.live("3456.78");
    let chain = MockChain::new(p.notary);
    let ts = unix_now();
    chain.answer_preflight(Err(ChainError::FromTheFuture { observed_at: ts, block_time: ts - 1 }));
    let started = Instant::now();
    let outcome = p.cycle(&chain).await;
    assert!(matches!(outcome, Outcome::Pushed { .. }), "{outcome}");
    assert_eq!(chain.preflights.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn the_loop_pushes_on_its_heartbeat_and_stops_on_shutdown() {
    let p = pipeline("relayer test: loop").await;
    p.provider.live("3456.78");
    let chain = MockChain::new(p.notary);
    let stats =
        relayer::run(&p.config, &chain.setup, &chain, &p.http, &p.prover, Some(2), std::future::pending()).await;
    assert_eq!((stats.cycles, stats.pushed), (2, 2), "{stats:?}");
    let submitted = chain.submitted();
    assert!(submitted[1].0.observedAt > submitted[0].0.observedAt);

    // A shutdown stops the loop between cycles.
    let stats = relayer::run(
        &p.config,
        &chain.setup,
        &chain,
        &p.http,
        &p.prover,
        None,
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await;
    assert_eq!(stats.cycles, 1, "{stats:?}");
}

#[tokio::test]
async fn the_prover_checks_its_public_signals_and_its_deadline() {
    let prover = common::prover();
    let body = CanonicalBody::new("ETHUSD", Price::parse_quote("3456.78").unwrap(), 1_767_225_600).unwrap();
    let commitment = body_commitment(&body.padded());
    let witness = body.witness(provider_id(), ethusd_commitment(), commitment, salt());
    let word = |b: alloy::primitives::B256| U256::from_be_bytes(b.0);
    let expected = [
        word(provider_id()),
        word(ethusd_commitment()),
        wad("3456.78"),
        U256::from(1_767_225_600u64),
        word(commitment),
    ];
    assert_eq!(prover.prove(&witness, &expected).await.unwrap().len(), 256);

    let mut other = expected;
    other[2] = wad("3456.79");
    assert!(matches!(prover.prove(&witness, &other).await, Err(ProverError::PublicSignals)));

    let mut wrong_salt = witness.clone();
    wrong_salt["salt"] = serde_json::json!("1");
    assert!(matches!(prover.prove(&wrong_salt, &expected).await, Err(ProverError::Failed(_))), "no witness");

    let hurried = Prover { timeout: Duration::from_millis(1), ..prover.clone() };
    assert!(matches!(hurried.prove(&witness, &expected).await, Err(ProverError::Timeout)));

    let no_node = Prover { node: "/nonexistent/node".into(), ..prover };
    assert!(matches!(no_node.prove(&witness, &expected).await, Err(ProverError::Spawn(_))));
}

#[tokio::test]
async fn the_relayer_never_shows_its_rpc_url() {
    let kms = FakeKms::start(&[("relayer-key", test_key("relayer test: rpc"))]).await;
    let signer = || async { kms::signer(&kms.config("relayer-key"), Some(421_614)).await.unwrap() };
    let feed = Address::repeat_byte(1);
    for url in ["http://127.0.0.1:9/v2/very-secret-api-key", "http://user:very-secret-api-key@127.0.0.1:9/"] {
        let error = RpcFeed::connect(url, feed, signer().await, 421_614, FeeCaps::TESTNET).await.err().unwrap();
        let shown = error.to_string();
        assert!(shown.starts_with("rpc: ") && !shown.contains("very-secret-api-key"), "{shown}");
    }
    let error = RpcFeed::connect("not a url very-secret-api-key", feed, signer().await, 421_614, FeeCaps::TESTNET)
        .await
        .err()
        .unwrap();
    assert!(!error.to_string().contains("very-secret-api-key"), "{error}");
}
