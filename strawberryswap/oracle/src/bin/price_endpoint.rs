//! The testnet's data provider: polls the exchanges and serves `GET /v1/<SYMBOL>`, the circuit's canonical body of
//! their aggregate price. See zk_oracle::endpoint and docs/ORACLE.md.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use zk_oracle::cli::{self, Flags, TRUSTED_BOOTSTRAP};
use zk_oracle::endpoint::{self, EndpointConfig, Quotes};
use zk_oracle::sources::{Exchange, Rules};

const USAGE: &str = "usage: price-endpoint [--listen ADDR] [--symbol SYMBOL] [--sources LIST] [--poll-ms MS]
                      [--timeout-ms MS] [--max-age-secs S] [--max-spread-bps BPS] [--min-sources N]

Serves GET /v1/<SYMBOL>: the canonical body {\"symbol\":…,\"price\":…,\"ts\":…} of the exchanges' aggregate
price, or 503 when they give none (fail closed). TLS is terminated in front of it.

  --listen ADDR          default 127.0.0.1:8080 (port 0: any free port, printed as LISTENING <addr> on stdout)
  --symbol SYMBOL        default ETHUSD
  --sources LIST         comma-separated exchange[=base URL] among coinbase, kraken, bitstamp (default: all three)
  --poll-ms MS           poll interval (default 2000)
  --timeout-ms MS        per-request timeout (default 2000)
  --max-age-secs S       a quote older than this is ignored (default 10)
  --max-spread-bps BPS   a quote farther than this from the median is ignored (default 100)
  --min-sources N        quotes that must remain for a price to be served (default 2)";

const VALUED: &[&str] = &[
    "--listen",
    "--symbol",
    "--sources",
    "--poll-ms",
    "--timeout-ms",
    "--max-age-secs",
    "--max-spread-bps",
    "--min-sources",
];

fn sources(list: &str) -> Result<Vec<(Exchange, String)>, String> {
    let mut out: Vec<(Exchange, String)> = Vec::new();
    for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, base) = item.split_once('=').map_or((item, None), |(n, b)| (n, Some(b)));
        let exchange = Exchange::from_name(name).ok_or_else(|| format!("--sources: unknown exchange {name:?}"))?;
        if out.iter().any(|(e, _)| *e == exchange) {
            return Err(format!("--sources: {name} given twice"));
        }
        out.push((exchange, base.unwrap_or(exchange.default_base()).to_string()));
    }
    if out.is_empty() {
        return Err("--sources: no exchange".into());
    }
    Ok(out)
}

fn parse(flags: &Flags) -> Result<(SocketAddr, EndpointConfig), String> {
    let listen = flags.parsed_or("--listen", SocketAddr::from(([127, 0, 0, 1], 8080)))?;
    let defaults = EndpointConfig::testnet();
    let symbol = flags.get("--symbol").unwrap_or(&defaults.symbol).to_string();
    cli::symbol(&symbol)?;
    let sources = match flags.get("--sources") {
        Some(list) => sources(list)?,
        None => defaults.sources,
    };
    let rules = Rules {
        max_age_secs: flags.parsed_or("--max-age-secs", Rules::TESTNET.max_age_secs)?,
        max_spread_bps: flags.parsed_or("--max-spread-bps", Rules::TESTNET.max_spread_bps)?,
        min_sources: flags.parsed_or("--min-sources", Rules::TESTNET.min_sources)?,
    };
    if rules.max_spread_bps > 10_000 || rules.max_age_secs == 0 || rules.max_age_secs > 60 {
        return Err("--max-spread-bps must be at most 10000 and --max-age-secs between 1 and 60".into());
    }
    if rules.min_sources == 0 || rules.min_sources > sources.len() {
        return Err(format!("--min-sources must be between 1 and the {} sources", sources.len()));
    }
    let poll_ms: u64 = flags.parsed_or("--poll-ms", 2_000)?;
    let timeout_ms: u64 = flags.parsed_or("--timeout-ms", 2_000)?;
    if poll_ms == 0 || timeout_ms == 0 {
        return Err("--poll-ms and --timeout-ms must be positive".into());
    }
    let config = EndpointConfig {
        symbol,
        sources,
        poll_interval: Duration::from_millis(poll_ms),
        request_timeout: Duration::from_millis(timeout_ms),
        rules,
    };
    Ok((listen, config))
}

#[tokio::main]
async fn main() -> ExitCode {
    let parsed = Flags::parse(std::env::args().skip(1), VALUED, &[]).and_then(|flags| parse(&flags).map_err(Some));
    let (listen, config) = match parsed {
        Ok(parsed) => parsed,
        Err(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(Some(message)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let client = match reqwest::Client::builder().timeout(config.request_timeout).build() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("error: HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("error: {listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let address = listener.local_addr().map_or(listen, |a| a);
    let names: Vec<&str> = config.sources.iter().map(|(e, _)| e.name()).collect();
    eprintln!("{TRUSTED_BOOTSTRAP}");
    eprintln!(
        "price-endpoint: GET /v1/{} on http://{address}: median of {}, quotes within {} bps of it, at least {} no older \
         than {} s",
        config.symbol,
        names.join(", "),
        config.rules.max_spread_bps,
        config.rules.min_sources,
        config.rules.max_age_secs
    );
    println!("LISTENING {address}");

    let quotes = Arc::new(Quotes::default());
    tokio::spawn(endpoint::poll_sources(Arc::clone(&quotes), client, config.clone()));
    let app = endpoint::router(quotes, &config);
    match axum::serve(listener, app).with_graceful_shutdown(cli::shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
