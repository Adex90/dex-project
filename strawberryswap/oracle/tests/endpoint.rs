//! The price endpoint over stand-in exchanges: it serves the canonical body of the median of fresh, agreeing quotes,
//! and nothing (503) when the quotes do not support a price.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{StatusCode, header};
use common::FakeExchanges;
use zk_oracle::body::CanonicalBody;
use zk_oracle::endpoint::{self, EndpointConfig, Quotes};
use zk_oracle::sources::Rules;
use zk_oracle::unix_now;

struct Endpoint {
    url: String,
    exchanges: FakeExchanges,
    http: reqwest::Client,
}

async fn endpoint(max_age_secs: u64) -> Endpoint {
    let exchanges = FakeExchanges::start().await;
    let config = EndpointConfig {
        symbol: "ETHUSD".into(),
        sources: exchanges.sources(),
        poll_interval: Duration::from_millis(50),
        request_timeout: Duration::from_secs(1),
        rules: Rules { max_age_secs, ..Rules::TESTNET },
    };
    let quotes = Arc::new(Quotes::default());
    tokio::spawn(endpoint::poll_sources(Arc::clone(&quotes), reqwest::Client::new(), config.clone()));
    let address = common::serve(endpoint::router(quotes, &config)).await;
    Endpoint { url: format!("http://{address}"), exchanges, http: reqwest::Client::new() }
}

impl Endpoint {
    async fn get(&self, path: &str) -> reqwest::Response {
        self.http.get(format!("{}{path}", self.url)).send().await.unwrap()
    }

    /// Polls until the endpoint's answer matches `want` (the pollers run every 50 ms), or fails after 5 s.
    async fn until(&self, want: impl Fn(StatusCode, &str) -> bool) -> (StatusCode, String) {
        let mut last = (StatusCode::IM_A_TEAPOT, String::new());
        for _ in 0..100 {
            let response = self.get("/v1/ETHUSD").await;
            let status = response.status();
            let text = response.text().await.unwrap();
            if want(status, &text) {
                return (status, text);
            }
            last = (status, text);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the endpoint kept answering {last:?}");
    }

    async fn price(&self, want: &str) -> CanonicalBody {
        let want = format!("\"price\":{want},");
        let (_, text) = self.until(|status, text| status == StatusCode::OK && text.contains(&want)).await;
        CanonicalBody::parse(text.as_bytes()).unwrap()
    }

    async fn no_price(&self) -> String {
        self.until(|status, _| status == StatusCode::SERVICE_UNAVAILABLE).await.1
    }
}

#[tokio::test]
async fn it_serves_the_canonical_body_of_the_median() {
    let endpoint = endpoint(10).await;
    endpoint.exchanges.set_all(Some("3456.70"), Some("3456.78000"), Some("3456.90"));
    let body = endpoint.price("3456.78").await;
    assert_eq!(body.symbol_str(), "ETHUSD");
    assert!(body.observed_at().abs_diff(unix_now()) <= 1);
    assert_eq!(body.text(), format!("{{\"symbol\":\"ETHUSD\",\"price\":3456.78,\"ts\":{}}}", body.observed_at()));

    let response = endpoint.get("/v1/ETHUSD").await;
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(endpoint.get("/v1/BTCUSD").await.status(), StatusCode::NOT_FOUND);
    assert_eq!(endpoint.get("/healthz").await.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn two_of_three_suffice_and_an_outlier_is_ignored() {
    let endpoint = endpoint(10).await;
    // Kraken down: the median of two is their midpoint.
    endpoint.exchanges.set_all(Some("3456.00"), None, Some("3457.00"));
    endpoint.price("3456.5").await;
    // Bitstamp 10 % off the median (3456.20): dropped, and the price is the median of the two that agree.
    endpoint.exchanges.set_all(Some("3456.00"), Some("3456.20"), Some("3801.60"));
    endpoint.price("3456.1").await;
}

#[tokio::test]
async fn it_fails_closed() {
    let endpoint = endpoint(1).await;
    endpoint.exchanges.set_all(Some("3456.00"), Some("3456.20"), Some("3456.40"));
    endpoint.price("3456.2").await;

    // One source left: no price, rather than one exchange's word.
    endpoint.exchanges.set_all(Some("3456.00"), None, None);
    let reason = endpoint.no_price().await;
    assert!(reason.starts_with("no price"), "{reason}");

    // Two sources that disagree by more than 1 %: no price.
    endpoint.exchanges.set_all(Some("3456.00"), None, Some("3600.00"));
    endpoint.no_price().await;

    // Malformed tickers count as down.
    endpoint.exchanges.set_all(Some("3456.00"), Some("-1"), Some("NaN"));
    endpoint.no_price().await;

    // And everything comes back once two sources agree again.
    endpoint.exchanges.set_all(Some("3456.00"), Some("3456.10"), None);
    endpoint.price("3456.05").await;
}
