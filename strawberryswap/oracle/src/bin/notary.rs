//! The notary: on `POST /v1/attest` it fetches the price endpoint's body over TLS, checks it, and has AWS KMS sign
//! ZkDecoOracle's digest of its commitment. See zk_oracle::notary_service and docs/ORACLE.md.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::B256;
use alloy::signers::Signer;
use tokio::net::TcpListener;
use zk_oracle::cli::{self, Flags, TRUSTED_BOOTSTRAP};
use zk_oracle::kms::{self, KmsConfig};
use zk_oracle::notary_service::{self, Notary, NotaryConfig};

const USAGE: &str =
    "usage: notary --provider-url URL --provider-id ID --kms-key-id KEY [--symbol SYMBOL] [--listen ADDR]
              [--kms-region REGION] [--kms-endpoint URL] [--max-age-secs S] [--max-ahead-secs S]
              [--timeout-ms MS] [--allow-http]

Notarizes the price endpoint. POST /v1/attest fetches its body over TLS, checks it is a fresh canonical body of the
instrument, and has AWS KMS sign ZkDecoOracle's digest of its commitment. GET /v1/info names the notary's address,
which ZkDecoOracle must have registered for the provider. AWS credentials come from the standard chain.

  --provider-url URL    the endpoint's https URL, e.g. https://price.testnet.example/v1/ETHUSD (required)
  --provider-id ID      the provider's registry key in ZkDecoOracle, 0x-hex (required)
  --kms-key-id KEY      the notary's KMS key (ECC_SECG_P256K1, SIGN_VERIFY): id, ARN or alias (required)
  --kms-region REGION   default: the standard AWS configuration
  --kms-endpoint URL    a KMS endpoint other than the region's default (a VPC endpoint)
  --symbol SYMBOL       default ETHUSD
  --listen ADDR         default 127.0.0.1:8090 (port 0: any free port, printed as LISTENING <addr> on stdout)
  --max-age-secs S      refuse a body stamped more than S s before the notary's clock (default 5)
  --max-ahead-secs S    refuse a body stamped more than S s after it (default 2)
  --timeout-ms MS       the fetch's timeout (default 3000)
  --allow-http          accept a plain-http provider URL: local tests only";

const VALUED: &[&str] = &[
    "--provider-url",
    "--provider-id",
    "--kms-key-id",
    "--kms-region",
    "--kms-endpoint",
    "--symbol",
    "--listen",
    "--max-age-secs",
    "--max-ahead-secs",
    "--timeout-ms",
];

fn parse(flags: &Flags) -> Result<(SocketAddr, NotaryConfig, KmsConfig), String> {
    let provider_id: B256 = flags
        .required("--provider-id", "the provider's registry key in ZkDecoOracle")?
        .parse()
        .map_err(|_| "--provider-id: not a 32-byte 0x-hex value".to_string())?;
    if provider_id.is_zero() {
        return Err("--provider-id: zero is no provider".into());
    }
    let timeout_ms: u64 = flags.parsed_or("--timeout-ms", 3_000)?;
    let config = NotaryConfig {
        provider_url: flags.required("--provider-url", "the price endpoint's https URL")?.to_string(),
        provider_id,
        symbol: cli::symbol(flags.get("--symbol").unwrap_or("ETHUSD"))?,
        max_age_secs: flags.parsed_or("--max-age-secs", 5)?,
        max_ahead_secs: flags.parsed_or("--max-ahead-secs", 2)?,
        request_timeout: Duration::from_millis(timeout_ms.max(1)),
        allow_http: flags.switch("--allow-http"),
    };
    let kms = KmsConfig {
        key_id: flags.required("--kms-key-id", "the notary's KMS key")?.to_string(),
        endpoint: flags.get("--kms-endpoint").map(str::to_string),
        region: flags.get("--kms-region").map(str::to_string),
        static_credentials: None,
    };
    // ZkPriceFeed refuses an observation more than 60 s old: a wider window would only sign what cannot be pushed.
    if config.max_age_secs == 0 || config.max_age_secs > 60 || config.max_ahead_secs > 60 {
        return Err("--max-age-secs must be between 1 and 60, and --max-ahead-secs at most 60".into());
    }
    let listen = flags.parsed_or("--listen", SocketAddr::from(([127, 0, 0, 1], 8090)))?;
    Ok((listen, config, kms))
}

#[tokio::main]
async fn main() -> ExitCode {
    let parsed =
        Flags::parse(std::env::args().skip(1), VALUED, &["--allow-http"]).and_then(|flags| parse(&flags).map_err(Some));
    let (listen, config, kms_config) = match parsed {
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
    // Digests only: the notary key never signs a transaction, so it is bound to no chain.
    let signer = match kms::signer(&kms_config, None).await {
        Ok(signer) => signer,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let address = signer.address();
    if config.allow_http {
        eprintln!("WARNING: --allow-http: plain-http provider URLs accepted. Local tests only.");
    }
    let provider_id = config.provider_id;
    let notary = match Notary::new(config, signer) {
        Ok(notary) => Arc::new(notary),
        Err(e) => {
            eprintln!("error: {e}");
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
    let bound = listener.local_addr().map_or(listen, |a| a);
    eprintln!("{TRUSTED_BOOTSTRAP}");
    eprintln!(
        "notary {address} (KMS key {}) for provider {provider_id}: POST /v1/attest on http://{bound}",
        kms_config.key_id
    );
    println!("LISTENING {bound}");
    match axum::serve(listener, notary_service::router(notary)).with_graceful_shutdown(cli::shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
