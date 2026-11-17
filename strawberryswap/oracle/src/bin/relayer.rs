//! The relayer: every ~15 s, a notarized body, its Groth16 proof and a push to ZkPriceFeed, signed by the relayer's
//! KMS key. See zk_oracle::relayer and docs/ORACLE.md.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::signers::Signer;
use zk_oracle::chain::{FeeCaps, RpcFeed};
use zk_oracle::cli::{self, Flags, TRUSTED_BOOTSTRAP};
use zk_oracle::kms::{self, KmsConfig};
use zk_oracle::prover::Prover;
use zk_oracle::relayer::{self, RelayerConfig};

const USAGE: &str = "usage: relayer --rpc-url URL --chain-id ID --feed ADDRESS --notary-url URL --salt-file PATH
               --kms-key-id KEY [--symbol SYMBOL] [--kms-region REGION] [--kms-endpoint URL]
               [--interval-ms MS] [--max-cycles N] [--zk-dir DIR] [--node PATH] [--prover-script PATH]
               [--prove-timeout-secs S] [--notary-timeout-ms MS] [--max-fee-wei WEI]
               [--max-priority-fee-wei WEI] [--gas-limit GAS]

Keeps ZkPriceFeed fresh: every interval, a notarized body from the notary, a Groth16 proof of it, a preflight and a
push signed by the relayer's AWS KMS key. Relaying is permissionless: the relayer is trusted for nothing.

  --rpc-url URL               the chain's JSON-RPC URL (required; never printed: it may hold an API key)
  --chain-id ID               the chain id, e.g. 421614 for Arbitrum Sepolia (required)
  --feed ADDRESS              the ZkPriceFeed contract (required)
  --notary-url URL            the notary's base URL (required)
  --salt-file PATH            the query salt, a field element, in a file only its owner can read (required)
  --kms-key-id KEY            the relayer's gas key in KMS (ECC_SECG_P256K1, SIGN_VERIFY) (required)
  --kms-region REGION         default: the standard AWS configuration
  --kms-endpoint URL          a KMS endpoint other than the region's default (a VPC endpoint)
  --symbol SYMBOL             default ETHUSD
  --interval-ms MS            one cycle per interval (default 15000)
  --max-cycles N              stop after N cycles (default: run until SIGTERM)
  --zk-dir DIR                zkdeco_oracle.{wasm,zkey,vkey.json} (default zk)
  --node PATH                 the node executable (default node)
  --prover-script PATH        default ../circuits/scripts/prove-zkdeco.mjs
  --prove-timeout-secs S      default 60
  --notary-timeout-ms MS      default 5000
  --max-fee-wei WEI           max fee per gas cap (default 2000000000)
  --max-priority-fee-wei WEI  priority fee cap (default 100000000)
  --gas-limit GAS             gas cap, Arbitrum's L1 data cost included (default 2000000)";

const VALUED: &[&str] = &[
    "--rpc-url",
    "--chain-id",
    "--feed",
    "--notary-url",
    "--salt-file",
    "--kms-key-id",
    "--kms-region",
    "--kms-endpoint",
    "--symbol",
    "--interval-ms",
    "--max-cycles",
    "--zk-dir",
    "--node",
    "--prover-script",
    "--prove-timeout-secs",
    "--notary-timeout-ms",
    "--max-fee-wei",
    "--max-priority-fee-wei",
    "--gas-limit",
];

struct Args {
    rpc_url: String,
    chain_id: u64,
    feed: Address,
    relayer: RelayerConfig,
    kms: KmsConfig,
    prover: Prover,
    caps: FeeCaps,
    max_cycles: Option<u64>,
}

fn parse(flags: &Flags) -> Result<Args, String> {
    let feed: Address = flags
        .required("--feed", "the ZkPriceFeed address")?
        .parse()
        .map_err(|_| "--feed: not an address".to_string())?;
    if feed.is_zero() {
        return Err("--feed: the zero address is not a contract".into());
    }
    let chain_id = flags.parsed::<u64>("--chain-id")?.ok_or("--chain-id is required: the relayer never guesses")?;
    let interval_ms: u64 = flags.parsed_or("--interval-ms", 15_000)?;
    if interval_ms < 1_000 {
        return Err("--interval-ms: at least 1000 (a body's timestamp has 1 s resolution)".into());
    }
    let salt_file = PathBuf::from(flags.required("--salt-file", "the query salt's file")?);
    let relayer = RelayerConfig {
        notary_url: flags.required("--notary-url", "the notary's base URL")?.to_string(),
        symbol: cli::symbol(flags.get("--symbol").unwrap_or("ETHUSD"))?,
        salt: cli::read_salt(&salt_file)?,
        notary_timeout: Duration::from_millis(flags.parsed_or("--notary-timeout-ms", 5_000)?),
        interval: Duration::from_millis(interval_ms),
    };
    let kms = KmsConfig {
        key_id: flags.required("--kms-key-id", "the relayer's KMS key")?.to_string(),
        endpoint: flags.get("--kms-endpoint").map(str::to_string),
        region: flags.get("--kms-region").map(str::to_string),
        static_credentials: None,
    };
    let zk_dir = PathBuf::from(flags.get("--zk-dir").unwrap_or("zk"));
    let prover = Prover::new(
        PathBuf::from(flags.get("--node").unwrap_or("node")),
        PathBuf::from(flags.get("--prover-script").unwrap_or("../circuits/scripts/prove-zkdeco.mjs")),
        &zk_dir,
        Duration::from_secs(flags.parsed_or("--prove-timeout-secs", 60)?),
    );
    for path in [&prover.script, &prover.wasm, &prover.zkey, &prover.vkey] {
        if !path.is_file() {
            return Err(format!("{}: no such file", path.display()));
        }
    }
    let caps = FeeCaps {
        max_fee_per_gas: flags.parsed_or("--max-fee-wei", FeeCaps::TESTNET.max_fee_per_gas)?,
        max_priority_fee_per_gas: flags
            .parsed_or("--max-priority-fee-wei", FeeCaps::TESTNET.max_priority_fee_per_gas)?,
        gas_limit: flags.parsed_or("--gas-limit", FeeCaps::TESTNET.gas_limit)?,
    };
    Ok(Args {
        rpc_url: flags.required("--rpc-url", "the chain's JSON-RPC URL")?.to_string(),
        chain_id,
        feed,
        relayer,
        kms,
        prover,
        caps,
        max_cycles: flags.parsed("--max-cycles")?,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let parsed = Flags::parse(std::env::args().skip(1), VALUED, &[]).and_then(|flags| parse(&flags).map_err(Some));
    let args = match parsed {
        Ok(args) => args,
        Err(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(Some(message)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let signer = match kms::signer(&args.kms, Some(args.chain_id)).await {
        Ok(signer) => signer,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let address = signer.address();
    let chain = match RpcFeed::connect(&args.rpc_url, args.feed, signer, args.chain_id, args.caps).await {
        Ok(chain) => chain,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let http = match reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build() {
        Ok(http) => http,
        Err(e) => {
            eprintln!("error: HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };
    let setup = match relayer::check_setup(&args.relayer, &chain, &http).await {
        Ok(setup) => setup,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let balance = chain.balance().await.unwrap_or(U256::ZERO);
    eprintln!("{TRUSTED_BOOTSTRAP}");
    eprintln!(
        "relayer {address} (KMS key {}, balance {balance} wei) on chain {}: feed {}, provider {}, notary {}, every {} ms",
        args.kms.key_id,
        args.chain_id,
        args.feed,
        setup.provider_id,
        setup.notary,
        args.relayer.interval.as_millis()
    );
    if balance.is_zero() {
        eprintln!("WARNING: the relayer has no balance: its pushes cannot pay for gas");
    }
    let stats =
        relayer::run(&args.relayer, &setup, &chain, &http, &args.prover, args.max_cycles, cli::shutdown_signal()).await;
    eprintln!(
        "relayer stopped: {} cycles, {} pushed, {} nothing new, {} refused, {} failed",
        stats.cycles, stats.pushed, stats.nothing_new, stats.refused, stats.failed
    );
    ExitCode::SUCCESS
}
