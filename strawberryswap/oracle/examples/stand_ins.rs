//! Local stand-ins for the oracle pipeline's outside services, for end-to-end tests of whole stacks (the engine's live
//! end to end in CI): AWS KMS, over its wire protocol, and the three exchanges' tickers, with an admin endpoint that
//! moves the market. They are this crate's test stand-ins (tests/common), served as one process.
//!
//! **Tests only.** The KMS keys are derived from public labels; nothing here is a key anyone should use. It is an
//! example, so it is never part of the crate's binaries.
//!
//! ```text
//! stand_ins --price 3456.78 --key notary-key="e2e: notary" --key relayer-key="e2e: relayer"
//! KEY notary-key 0x…            (one line per key: its id and its address)
//! LISTENING kms=http://127.0.0.1:… exchanges=http://127.0.0.1:… admin=http://127.0.0.1:…
//! ```
//!
//! `POST <admin>/price` with a decimal body (e.g. `3500.00`) sets all three exchanges' price.

#[path = "../tests/common/mod.rs"]
mod common;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use common::{FakeExchanges, FakeKms, address_of, serve, test_key};
use zk_oracle::body::Price;

async fn set_price(State(exchanges): State<FakeExchanges>, body: String) -> StatusCode {
    let price = body.trim();
    if Price::parse_quote(price).is_err() {
        return StatusCode::BAD_REQUEST;
    }
    exchanges.set_all(Some(price), Some(price), Some(price));
    StatusCode::NO_CONTENT
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (mut price, mut keys) = ("3456.78".to_string(), Vec::new());
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--price" => price = value,
            "--key" => {
                let (id, label) = value.split_once('=').expect("--key ID=LABEL");
                keys.push((id.to_string(), label.to_string()));
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let signing: Vec<_> = keys.iter().map(|(id, label)| (id.as_str(), test_key(label))).collect();
    let kms = FakeKms::start(&signing).await;
    let exchanges = FakeExchanges::start().await;
    exchanges.set_all(Some(&price), Some(&price), Some(&price));
    let admin = serve(Router::new().route("/price", post(set_price)).with_state(exchanges.clone())).await;
    for (id, key) in &signing {
        println!("KEY {id} {}", address_of(key));
    }
    println!("LISTENING kms={} exchanges={} admin=http://{admin}", kms.url, exchanges.base);
    let _ = tokio::signal::ctrl_c().await;
}
