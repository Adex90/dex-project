//! The price endpoint: `GET /v1/<SYMBOL>` serves the circuit's canonical object for the current aggregate price,
//! `{"symbol":"ETHUSD","price":3456.78,"ts":1767225600}`, or 503 when the sources give no price.
//!
//! On testnet this is the "data provider" the notary vouches for. It is operated by Rothbard, and that is part of the
//! testnet's trusted bootstrap (docs/ORACLE.md). TLS is terminated in front of it, and its domain and TLS key root
//! define the providerId registered in ZkDecoOracle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::RwLock;

use crate::body::CanonicalBody;
use crate::sources::{Exchange, Quote, Rules, SourceError, aggregate};
use crate::{error_chain, unix_now};

#[derive(Clone, Debug)]
pub struct EndpointConfig {
    /// The one instrument served (6 characters).
    pub symbol: String,
    /// The exchanges polled, with their base URLs (tests point them at local stand-ins).
    pub sources: Vec<(Exchange, String)>,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
    pub rules: Rules,
}

impl EndpointConfig {
    pub fn testnet() -> Self {
        Self {
            symbol: "ETHUSD".into(),
            sources: Exchange::ALL.iter().map(|&e| (e, e.default_base().to_string())).collect(),
            poll_interval: Duration::from_secs(2),
            request_timeout: Duration::from_secs(2),
            rules: Rules::TESTNET,
        }
    }
}

/// The latest quote of each exchange, shared by the pollers and the handler.
#[derive(Default)]
pub struct Quotes(RwLock<HashMap<Exchange, Quote>>);

impl Quotes {
    pub async fn record(&self, quote: Quote) {
        self.0.write().await.insert(quote.exchange, quote);
    }

    pub async fn snapshot(&self) -> Vec<Quote> {
        self.0.read().await.values().copied().collect()
    }
}

/// Fetches one exchange's ticker once.
pub async fn fetch_quote(client: &reqwest::Client, exchange: Exchange, base: &str) -> Result<Quote, SourceError> {
    let url = format!("{}{}", base.trim_end_matches('/'), exchange.ticker_path());
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SourceError::Upstream(format!("{}: {}", exchange.name(), error_chain(&e))))?;
    if !response.status().is_success() {
        return Err(SourceError::Upstream(format!("{}: HTTP {}", exchange.name(), response.status())));
    }
    let ticker: serde_json::Value =
        response.json().await.map_err(|e| SourceError::Format(format!("{}: {e}", exchange.name())))?;
    let price = exchange.parse_ticker(&ticker)?;
    Ok(Quote { exchange, price, received_at: unix_now() })
}

/// Polls every source every `poll_interval`, forever. A failing source keeps its last quote, which ages out under
/// the rules' max age.
pub async fn poll_sources(quotes: Arc<Quotes>, client: reqwest::Client, config: EndpointConfig) {
    let mut tick = tokio::time::interval(config.poll_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_error: HashMap<Exchange, String> = HashMap::new();
    loop {
        tick.tick().await;
        let polls = config.sources.iter().map(|(exchange, base)| {
            let (client, exchange, base) = (client.clone(), *exchange, base.clone());
            async move { (exchange, fetch_quote(&client, exchange, &base).await) }
        });
        for (exchange, result) in futures_join_all(polls).await {
            match result {
                Ok(quote) => {
                    quotes.record(quote).await;
                    last_error.remove(&exchange);
                }
                Err(e) => {
                    let message = e.to_string();
                    if last_error.get(&exchange) != Some(&message) {
                        eprintln!("source {}: {message}", exchange.name());
                        last_error.insert(exchange, message);
                    }
                }
            }
        }
    }
}

/// Runs the futures concurrently and collects their outputs in order (a small join_all without the futures crate).
async fn futures_join_all<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("a source poll panicked"));
    }
    out
}

#[derive(Clone)]
struct AppState {
    quotes: Arc<Quotes>,
    symbol: String,
    rules: Rules,
}

/// The endpoint's routes: `GET /v1/<symbol>` and `GET /healthz`.
pub fn router(quotes: Arc<Quotes>, config: &EndpointConfig) -> Router {
    Router::new().route("/v1/{symbol}", get(price)).route("/healthz", get(|| async { "ok" })).with_state(AppState {
        quotes,
        symbol: config.symbol.clone(),
        rules: config.rules,
    })
}

async fn price(State(state): State<AppState>, Path(symbol): Path<String>) -> Response {
    if symbol != state.symbol {
        return (StatusCode::NOT_FOUND, "unknown symbol").into_response();
    }
    let now = unix_now();
    let quotes = state.quotes.snapshot().await;
    let body = aggregate(&quotes, now, &state.rules)
        .map_err(|e| e.to_string())
        .and_then(|price| CanonicalBody::new(&state.symbol, price, now).map_err(|e| e.to_string()));
    match body {
        Ok(body) => {
            let mut response = body.text().into_response();
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(reason) => {
            (StatusCode::SERVICE_UNAVAILABLE, [(header::CACHE_CONTROL, "no-store")], format!("no price: {reason}"))
                .into_response()
        }
    }
}
