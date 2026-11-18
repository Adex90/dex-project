//! StrawberrySwap vault, iteration 1 (testnet): one role, one AWS KMS key, one contract function.
//!
//! A vault process holds one role's key and signs that role's transactions, which its client builds and sends over a
//! local Unix socket. Clients never hold a key. Three roles, each in its own process, with its own key, socket and uid:
//!
//! * **operator** (the HFT engine): `SearcherExecutor.rebalance`, never `sweep`;
//! * **relayer** (the oracle relayer): `ZkPriceFeed.submit`, for the feed's own provider and instrument;
//! * **automator** (the batch automator): `DarkPool.settleBatch`.
//!
//! Each transaction passes the role's policy (policy.rs), a rate limit, per-window caps and the nonce rule (rules.rs),
//! then the key in KMS signs it (kms.rs). At start the vault checks its key against the chain: the operator's key must
//! be the executor's `operator()`, and no other role's key may be.
//!
//! Protocol: newline-delimited JSON, one request per line and one response per line (README.md).

#![forbid(unsafe_code)]

// The insecure-test-key feature lets the engine's end-to-end tests give the vault a key they know, so that they can
// look for it in the engine's memory. It must never reach a deployable binary, so optimized builds refuse it.
#[cfg(all(feature = "insecure-test-key", not(debug_assertions)))]
compile_error!(
    "the insecure-test-key feature loads the key from a file: it is for the end-to-end tests and is refused in optimized builds"
);

mod chain;
#[cfg(test)]
mod fake_kms;
mod kms;
mod policy;
mod rules;
mod server;
mod vault;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, hex};
use alloy_signer::Signer;

use crate::chain::{
    Chain, Rpc, batchSizeCall, maxPriceCall, minPriceCall, operatorCall, providerIdCall,
    queryCommitmentCall, view,
};
use crate::kms::KmsConfig;
use crate::policy::{Calls, INTRINSIC_GAS, Policy, Role};
use crate::rules::{Caps, NonceGuard, Window, parse_amount, parse_eth};
use crate::vault::Vault;

/// The chain is read again this often while idle, to catch a nonce spent outside the vault.
const WATCH_EVERY: Duration = Duration::from_secs(10);
/// Each JSON-RPC request's deadline.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

const USAGE: &str = "usage: threshold-signer --role ROLE --chain-id ID --rpc-url URL --kms-key-id KEY
                        --max-fee-eth-per-window ETH [options]

Signs one role's transactions with one AWS KMS key, over a local Unix socket. A role signs one
function on one contract; run one vault per role, each with its own key, socket and uid.

  --role ROLE                   operator  (SearcherExecutor.rebalance; never sweep)
                                relayer   (ZkPriceFeed.submit)
                                automator (DarkPool.settleBatch)                       (required)
  --chain-id ID                 the only chain id signed for                          (required)
  --rpc-url URL                 the chain, for the start-up checks and the nonce rule
                                (required; never printed: it may hold an API key)
  --kms-key-id KEY              the role's key in AWS KMS (ECC_SECG_P256K1, SIGN_VERIFY) (required)
  --kms-region REGION           default: the standard AWS configuration
  --kms-endpoint URL            a KMS endpoint other than the region's default (a VPC endpoint)

  --executor ADDRESS            the SearcherExecutor: the operator's destination (required for it);
                                for the other roles, the contract whose operator this key must not be
  --feed ADDRESS                the ZkPriceFeed                                       (relayer: required)
  --pool ADDRESS                the DarkPool                                          (automator: required)

  --socket PATH                 default: $XDG_RUNTIME_DIR/strawberryswap-vault/<role>.sock
  --allow-uid UID               the uid allowed to connect (default: this process's uid)

  --max-gas GAS                 gas limit cap (default: operator 1000000, relayer 2000000, automator 5000000)
  --max-fee-gwei GWEI           max fee per gas cap (default: 500)
  --max-signs-per-sec N         signature rate limit (default: 20)
  --max-pending N               nonces signed beyond the chain's next one, one included (default: 1:
                                one transaction in flight; no signature can be held back for later)
  --window-secs S               the caps' sliding window (default: 3600)
  --max-signs-per-window N      signatures per window (default: 3600)
  --max-fee-eth-per-window ETH  gas limit × max fee of everything signed in a window   (required)
  --max-volume0-per-window AMT  token0 moved per window, base units: 1000e18 is 1,000 tokens of 18
                                decimals (operator: required; automator: optional)
  --max-volume1-per-window AMT  token1, likewise
  --max-notes-per-window N      notes settled per window (automator; default: 3600)";

#[cfg(feature = "insecure-test-key")]
const TEST_KEY_USAGE: &str = "  --insecure-test-key-file PATH INSECURE: the key from PATH (0600) instead of KMS, and --rpc-url
                                optional (operator only); the engine's end-to-end tests only";

/// The command line, checked for consistency; the chain fills in the rest at start.
#[derive(Debug)]
struct Args {
    role: Role,
    chain_id: u64,
    rpc_url: Option<String>,
    kms: Option<KmsConfig>,
    #[cfg(feature = "insecure-test-key")]
    test_key_file: Option<PathBuf>,
    executor: Option<Address>,
    feed: Option<Address>,
    pool: Option<Address>,
    socket: Option<PathBuf>,
    allow_uid: Option<u32>,
    max_gas: u64,
    max_fee_gwei: u64,
    signs_per_sec: u32,
    max_pending: u64,
    window: Duration,
    caps: Caps,
}

const VALUED: &[&str] = &[
    "--role",
    "--chain-id",
    "--rpc-url",
    "--kms-key-id",
    "--kms-region",
    "--kms-endpoint",
    "--executor",
    "--feed",
    "--pool",
    "--socket",
    "--allow-uid",
    "--max-gas",
    "--max-fee-gwei",
    "--max-signs-per-sec",
    "--max-pending",
    "--window-secs",
    "--max-signs-per-window",
    "--max-fee-eth-per-window",
    "--max-volume0-per-window",
    "--max-volume1-per-window",
    "--max-notes-per-window",
    #[cfg(feature = "insecure-test-key")]
    "--insecure-test-key-file",
];

impl Args {
    /// Parses the arguments; Err(None) asks for the usage text.
    fn parse(it: impl IntoIterator<Item = String>) -> Result<Self, Option<String>> {
        let mut flags = BTreeMap::new();
        let mut it = it.into_iter();
        while let Some(flag) = it.next() {
            if flag == "-h" || flag == "--help" {
                return Err(None);
            }
            if !VALUED.contains(&flag.as_str()) {
                return Err(Some(format!("unknown argument {flag}")));
            }
            let value = it
                .next()
                .ok_or_else(|| Some(format!("{flag} needs a value")))?;
            if flags.insert(flag.clone(), value).is_some() {
                return Err(Some(format!("{flag} given twice")));
            }
        }
        Self::from_flags(&flags).map_err(Some)
    }

    fn from_flags(flags: &BTreeMap<String, String>) -> Result<Self, String> {
        let get = |flag: &str| flags.get(flag).map(String::as_str);
        fn number<T: std::str::FromStr>(
            flag: &str,
            text: Option<&str>,
            default: T,
        ) -> Result<T, String> {
            text.map_or(Ok(default), |text| {
                text.parse()
                    .map_err(|_| format!("{flag}: {text} is not a valid number"))
            })
        }
        let address = |flag: &str| -> Result<Option<Address>, String> {
            get(flag)
                .map(|text| match text.parse::<Address>() {
                    Ok(address) if address.is_zero() => {
                        Err(format!("{flag}: the zero address is not a contract"))
                    }
                    Ok(address) => Ok(address),
                    Err(_) => Err(format!("{flag}: {text} is not an address")),
                })
                .transpose()
        };
        let amount = |flag: &str| {
            get(flag)
                .map(parse_amount)
                .transpose()
                .map_err(|e| format!("{flag}: {e}"))
        };

        let role: Role = get("--role")
            .ok_or("--role is required: operator, relayer or automator")?
            .parse()?;
        let chain_id = get("--chain-id")
            .ok_or("--chain-id is required: the vault never guesses its chain")?
            .parse()
            .map_err(|_| "--chain-id is not a valid number".to_string())?;

        let kms = get("--kms-key-id").map(|key_id| KmsConfig {
            key_id: key_id.to_string(),
            endpoint: get("--kms-endpoint").map(str::to_string),
            region: get("--kms-region").map(str::to_string),
            static_credentials: None,
        });
        if kms.is_none() && (get("--kms-endpoint").is_some() || get("--kms-region").is_some()) {
            return Err("--kms-endpoint and --kms-region go with --kms-key-id".into());
        }
        #[cfg(feature = "insecure-test-key")]
        let test_key_file = get("--insecure-test-key-file").map(PathBuf::from);
        #[cfg(feature = "insecure-test-key")]
        let test_key = test_key_file.is_some();
        #[cfg(not(feature = "insecure-test-key"))]
        let test_key = false;
        match (kms.is_some(), test_key) {
            (true, true) => {
                return Err("--kms-key-id and --insecure-test-key-file: one key only".into());
            }
            (false, false) => {
                return Err("--kms-key-id is required: the role's key in AWS KMS".into());
            }
            _ => {}
        }
        let rpc_url = get("--rpc-url").map(str::to_string);
        if rpc_url.is_none() && !(test_key && role == Role::Operator) {
            return Err(
                "--rpc-url is required: the vault checks its key against the chain and follows its nonce".into(),
            );
        }

        let (executor, feed, pool) = (
            address("--executor")?,
            address("--feed")?,
            address("--pool")?,
        );
        match role {
            Role::Operator if executor.is_none() => {
                return Err("--executor is required for the operator: the SearcherExecutor".into());
            }
            Role::Relayer if feed.is_none() => {
                return Err("--feed is required for the relayer: the ZkPriceFeed".into());
            }
            Role::Automator if pool.is_none() => {
                return Err("--pool is required for the automator: the DarkPool".into());
            }
            _ => {}
        }
        for (flag, owner, given) in [
            ("--feed", Role::Relayer, feed.is_some()),
            ("--pool", Role::Automator, pool.is_some()),
        ] {
            if given && role != owner {
                return Err(format!(
                    "{flag} is the {owner}'s: this vault holds the {role} key"
                ));
            }
        }

        let default_gas = match role {
            Role::Operator => 1_000_000,
            Role::Relayer => 2_000_000,
            Role::Automator => 5_000_000,
        };
        let max_gas = number("--max-gas", get("--max-gas"), default_gas)?;
        let max_fee_gwei = number("--max-fee-gwei", get("--max-fee-gwei"), 500)?;
        let signs_per_sec = number("--max-signs-per-sec", get("--max-signs-per-sec"), 20u32)?;
        let max_pending = number("--max-pending", get("--max-pending"), 1u64)?;
        let window_secs = number("--window-secs", get("--window-secs"), 3_600u64)?;
        if signs_per_sec == 0
            || max_gas < INTRINSIC_GAS
            || window_secs == 0
            || !(1..=16).contains(&max_pending)
        {
            return Err(
                "--max-signs-per-sec and --window-secs must be positive, --max-gas at least 21000, --max-pending 1 to 16"
                    .into(),
            );
        }

        let fee_wei = parse_eth(get("--max-fee-eth-per-window").ok_or(
            "--max-fee-eth-per-window is required: what a window of signatures may cost at most",
        )?)
        .map_err(|e| format!("--max-fee-eth-per-window: {e}"))?;
        let (volume0, volume1) = (
            amount("--max-volume0-per-window")?,
            amount("--max-volume1-per-window")?,
        );
        let notes = get("--max-notes-per-window")
            .map(|text| number("--max-notes-per-window", Some(text), 0u64))
            .transpose()?;
        match role {
            Role::Operator if volume0.is_none() || volume1.is_none() => {
                return Err(
                    "--max-volume0-per-window and --max-volume1-per-window are required for the operator: the tokens a window may move".into(),
                );
            }
            Role::Relayer if volume0.is_some() || volume1.is_some() || notes.is_some() => {
                return Err(
                    "the relayer moves no tokens and settles no notes: no volume or notes caps"
                        .into(),
                );
            }
            Role::Operator if notes.is_some() => {
                return Err("--max-notes-per-window is the automator's".into());
            }
            _ => {}
        }
        let caps = Caps {
            signs: number(
                "--max-signs-per-window",
                get("--max-signs-per-window"),
                3_600,
            )?,
            fee_wei,
            volume0,
            volume1,
            notes: (role == Role::Automator).then(|| notes.unwrap_or(3_600)),
        };

        Ok(Self {
            role,
            chain_id,
            rpc_url,
            kms,
            #[cfg(feature = "insecure-test-key")]
            test_key_file,
            executor,
            feed,
            pool,
            socket: get("--socket").map(PathBuf::from),
            allow_uid: get("--allow-uid")
                .map(|text| number("--allow-uid", Some(text), 0u32))
                .transpose()?,
            max_gas,
            max_fee_gwei,
            signs_per_sec,
            max_pending,
            window: Duration::from_secs(window_secs),
            caps,
        })
    }

    /// The role's contract.
    fn to(&self) -> Address {
        match self.role {
            Role::Operator => self.executor,
            Role::Relayer => self.feed,
            Role::Automator => self.pool,
        }
        .expect("checked when parsed")
    }
}

/// The role's policy, from the command line and the chain. Refuses a key that holds the wrong role: the operator's key
/// must be the executor's `operator()`, and the other roles' keys must not be.
async fn read_policy(args: &Args, chain: &impl Chain, address: Address) -> Result<Policy, String> {
    let actual = chain.chain_id().await.map_err(|e| e.to_string())?;
    if actual != args.chain_id {
        return Err(format!(
            "the RPC is on chain {actual}, the vault signs for chain {}",
            args.chain_id
        ));
    }
    let to = args.to();
    if chain.code_size(to).await.map_err(|e| e.to_string())? == 0 {
        return Err(format!(
            "no contract at {to}: it is not the {} the {} role signs for",
            args.role.contract(),
            args.role
        ));
    }
    if let Some(executor) = args.executor {
        let operator = view(chain, executor, operatorCall {})
            .await
            .map_err(|e| format!("SearcherExecutor {executor}: {e}"))?;
        match args.role {
            Role::Operator if operator != address => {
                return Err(format!(
                    "the executor's operator is {operator}, not this key ({address}): it would revert every rebalance"
                ));
            }
            Role::Relayer | Role::Automator if operator == address => {
                return Err(format!(
                    "this key ({address}) is the executor's operator: the {} role needs its own key",
                    args.role
                ));
            }
            _ => {}
        }
    }
    let calls = match args.role {
        Role::Operator => Calls::Rebalance,
        Role::Relayer => {
            let feed = to;
            let read = |e: chain::ChainError| format!("ZkPriceFeed {feed}: {e}");
            Calls::Submit {
                provider_id: view(chain, feed, providerIdCall {}).await.map_err(read)?,
                query_commitment: view(chain, feed, queryCommitmentCall {})
                    .await
                    .map_err(read)?,
                min_price: view(chain, feed, minPriceCall {}).await.map_err(read)?,
                max_price: view(chain, feed, maxPriceCall {}).await.map_err(read)?,
            }
        }
        Role::Automator => {
            let pool = to;
            let size = view(chain, pool, batchSizeCall {})
                .await
                .map_err(|e| format!("DarkPool {pool}: {e}"))?;
            Calls::SettleBatch {
                max_notes: usize::try_from(size)
                    .ok()
                    .filter(|&n| (1..=1_024).contains(&n))
                    .ok_or_else(|| format!("DarkPool {pool}: a batch size of {size}"))?,
            }
        }
    };
    Ok(Policy {
        chain_id: args.chain_id,
        to,
        max_gas: args.max_gas,
        max_fee_per_gas: u128::from(args.max_fee_gwei) * 1_000_000_000,
        calls,
    })
}

/// Reads a test key: 32 bytes as 64 hex digits (an optional 0x prefix), from a file only its owner can read.
#[cfg(feature = "insecure-test-key")]
fn load_test_key(path: &std::path::Path) -> std::io::Result<alloy_signer_local::PrivateKeySigner> {
    use server::invalid;
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    if meta.mode() & 0o077 != 0 {
        return Err(invalid(format!(
            "{}: a key file must be readable by its owner only (0600)",
            path.display()
        )));
    }
    let text = fs::read_to_string(path)?;
    let digits = text.trim();
    let digits = digits.strip_prefix("0x").unwrap_or(digits);
    let bytes: alloy_primitives::B256 = digits
        .parse()
        .map_err(|_| invalid(format!("{}: expected 64 hex digits", path.display())))?;
    alloy_signer_local::PrivateKeySigner::from_bytes(&bytes).map_err(|e| {
        invalid(format!(
            "{}: not a valid secp256k1 key: {e}",
            path.display()
        ))
    })
}

fn describe_caps(caps: &Caps, window: Duration) -> String {
    let mut parts = vec![
        format!("≤ {} signatures", caps.signs),
        format!("≤ {} wei of fees", caps.fee_wei),
    ];
    if let Some(v) = caps.volume0 {
        parts.push(format!("≤ {v} token0"));
    }
    if let Some(v) = caps.volume1 {
        parts.push(format!("≤ {v} token1"));
    }
    if let Some(n) = caps.notes {
        parts.push(format!("≤ {n} notes"));
    }
    format!("window {} s: {}", window.as_secs(), parts.join(", "))
}

async fn run(args: Args) -> Result<(), String> {
    let own_uid = server::own_uid()
        .map_err(|e| format!("cannot read this process's uid from /proc/self: {e}"))?;
    let allowed_uid = args.allow_uid.unwrap_or(own_uid);
    let socket = args
        .socket
        .clone()
        .unwrap_or_else(|| server::default_socket(own_uid, args.role));

    // The key: in KMS (or, in the engine's end-to-end tests only, a test key from a file).
    let (signer, key_store, key_origin): (Arc<dyn Signer + Send + Sync>, &'static str, String) =
        match &args.kms {
            Some(config) => (
                Arc::new(kms::signer(config).await?),
                "aws-kms",
                format!("KMS key {}", config.key_id),
            ),
            #[cfg(feature = "insecure-test-key")]
            None => {
                let path = args.test_key_file.as_deref().expect("checked when parsed");
                let signer = load_test_key(path).map_err(|e| e.to_string())?;
                eprintln!(
                    "WARNING: INSECURE TEST KEY loaded from {}: end-to-end tests only, never a deployment",
                    path.display()
                );
                (
                    Arc::new(signer),
                    "insecure-test-key",
                    format!(
                        "INSECURE TEST KEY from {} (end-to-end tests only)",
                        path.display()
                    ),
                )
            }
            #[cfg(not(feature = "insecure-test-key"))]
            None => unreachable!("checked when parsed"),
        };
    let address = signer.address();

    // The chain: the key's role, the role's contract, and where the nonce stands.
    let (policy, nonces, chain) = match &args.rpc_url {
        Some(url) => {
            let rpc = Rpc::new(url, RPC_TIMEOUT).map_err(|e| e.to_string())?;
            let policy = read_policy(&args, &rpc, address).await?;
            let (latest, pending) = rpc.nonces(address).await.map_err(|e| e.to_string())?;
            (
                policy,
                NonceGuard::with_chain(latest, pending, args.max_pending),
                Some(rpc),
            )
        }
        None => {
            eprintln!(
                "WARNING: no --rpc-url (insecure test mode): no start-up checks against the chain, and the nonce rule follows the vault's own signatures only"
            );
            let policy = Policy {
                chain_id: args.chain_id,
                to: args.to(),
                max_gas: args.max_gas,
                max_fee_per_gas: u128::from(args.max_fee_gwei) * 1_000_000_000,
                calls: Calls::Rebalance,
            };
            (policy, NonceGuard::without_chain(), None)
        }
    };
    let nonce_line = if nonces.reads_chain() {
        format!(
            "nonce: the chain's next is {}; ≤ {} signed beyond it",
            nonces.next(),
            nonces.max_pending()
        )
    } else {
        "nonce: the vault's own signatures only (no chain)".to_string()
    };
    let caps_line = describe_caps(&args.caps, args.window);
    let vault = Arc::new(Vault::new(
        policy.clone(),
        signer,
        key_store,
        allowed_uid,
        f64::from(args.signs_per_sec),
        Window::new(args.window, args.caps.clone()),
        nonces,
        chain,
    ));
    let listener =
        server::bind_socket(&socket, own_uid, allowed_uid != own_uid).map_err(|e| e.to_string())?;

    let role = policy.role();
    println!(
        "threshold-signer: role {role} · {key_origin}; signer {}",
        vault.address()
    );
    let extra = match &policy.calls {
        Calls::Rebalance => ", never sweep".to_string(),
        Calls::Submit {
            provider_id,
            query_commitment,
            ..
        } => format!(", provider {provider_id}, instrument {query_commitment}"),
        Calls::SettleBatch { max_notes } => format!(", 1 to {max_notes} notes"),
    };
    println!(
        "policy: chain {} · to {} ({}) · {} [{}] only{extra} · gas ≤ {} · max fee ≤ {} gwei · ≤ {} signatures/s",
        policy.chain_id,
        policy.to,
        role.contract(),
        role.function(),
        hex::encode_prefixed(role.selector()),
        policy.max_gas,
        args.max_fee_gwei,
        args.signs_per_sec
    );
    println!("rules: {nonce_line} · {caps_line}");
    println!(
        "listening on {} (mode {}; uid {allowed_uid} only)",
        socket.display(),
        if allowed_uid != own_uid {
            "0660"
        } else {
            "0600"
        }
    );

    let watcher = tokio::spawn(Arc::clone(&vault).watch(WATCH_EVERY));
    server::serve(listener, vault, server::shutdown_signal()).await;
    watcher.abort();
    if let Err(e) = fs::remove_file(&socket) {
        eprintln!("vault: could not remove {}: {e}", socket.display());
    }
    println!("threshold-signer: stopped");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(None) => {
            println!("{USAGE}");
            #[cfg(feature = "insecure-test-key")]
            println!("{TEST_KEY_USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(Some(message)) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_kms::{FakeKms, address_of, test_key};
    use crate::policy::tests::{
        EXECUTOR, FEED, INSTRUMENT, POOL, PROVIDER, e18, raw, rebalance_tx,
    };
    use crate::vault::tests::{MockChain, sign_request};
    use alloy_consensus::TxEip1559;
    use alloy_primitives::{B256, U256};
    use alloy_sol_types::SolValue;
    use serde_json::Value;
    use std::collections::HashMap;

    fn parse(args: &[&str]) -> Result<Args, Option<String>> {
        Args::parse(args.iter().map(|s| (*s).to_string()))
    }

    const OPERATOR: &[&str] = &[
        "--role",
        "operator",
        "--chain-id",
        "31337",
        "--rpc-url",
        "http://127.0.0.1:8545",
        "--kms-key-id",
        "alias/strawberryswap-operator",
        "--executor",
        "0x5FbDB2315678afecb367f032d93F642f64180aa3",
        "--max-fee-eth-per-window",
        "0.5",
        "--max-volume0-per-window",
        "1000e18",
        "--max-volume1-per-window",
        "3500000e18",
    ];

    fn without(args: &[&'static str], flag: &str) -> Vec<&'static str> {
        let at = args.iter().position(|a| *a == flag).unwrap();
        [&args[..at], &args[at + 2..]].concat()
    }

    fn with(args: &[&'static str], more: &[&'static str]) -> Vec<&'static str> {
        [args, more].concat()
    }

    #[test]
    fn the_command_line_names_the_role_key_chain_and_caps() {
        let args = parse(OPERATOR).unwrap();
        assert_eq!(
            (
                args.role,
                args.chain_id,
                args.max_gas,
                args.signs_per_sec,
                args.max_pending
            ),
            (Role::Operator, 31_337, 1_000_000, 20, 1)
        );
        assert_eq!(args.caps.fee_wei, e18() / U256::from(2u8));
        assert_eq!(args.caps.volume0, Some(e18() * U256::from(1_000u32)));
        assert_eq!(args.caps.notes, None);
        assert_eq!(args.to(), EXECUTOR);
        for (flag, why) in [
            ("--role", "--role is required"),
            ("--chain-id", "--chain-id is required"),
            ("--rpc-url", "--rpc-url is required"),
            ("--kms-key-id", "--kms-key-id is required"),
            ("--executor", "--executor is required"),
            (
                "--max-fee-eth-per-window",
                "--max-fee-eth-per-window is required",
            ),
            ("--max-volume1-per-window", "required for the operator"),
        ] {
            let message = parse(&without(OPERATOR, flag)).unwrap_err().unwrap();
            assert!(message.contains(why), "{flag}: {message}");
        }
        for (more, why) in [
            (
                &["--feed", "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512"][..],
                "the relayer's",
            ),
            (&["--max-notes-per-window", "5"][..], "the automator's"),
            (&["--max-pending", "0"][..], "--max-pending 1 to 16"),
            (&["--role", "relayer"][..], "given twice"),
            (&["--key", "0x00"][..], "unknown argument --key"),
        ] {
            let message = parse(&with(OPERATOR, more)).unwrap_err().unwrap();
            assert!(message.contains(why), "{more:?}: {message}");
        }
        assert!(parse(&["--help"]).unwrap_err().is_none());
    }

    #[test]
    fn each_role_needs_its_contract_and_only_its_caps() {
        let relayer = [
            "--role",
            "relayer",
            "--chain-id",
            "1",
            "--rpc-url",
            "http://x",
            "--kms-key-id",
            "k",
            "--feed",
            "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512",
            "--max-fee-eth-per-window",
            "1",
        ];
        let args = parse(&relayer).unwrap();
        assert_eq!(
            (args.to(), args.max_gas, args.caps.volume0),
            (FEED, 2_000_000, None)
        );
        assert!(
            parse(&with(&relayer, &["--max-volume0-per-window", "1"]))
                .unwrap_err()
                .unwrap()
                .contains("moves no tokens")
        );
        let automator = [
            "--role",
            "automator",
            "--chain-id",
            "1",
            "--rpc-url",
            "http://x",
            "--kms-key-id",
            "k",
            "--pool",
            "0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0",
            "--max-fee-eth-per-window",
            "1",
        ];
        let args = parse(&automator).unwrap();
        assert_eq!(
            (args.to(), args.max_gas, args.caps.notes),
            (POOL, 5_000_000, Some(3_600))
        );
        assert!(
            parse(&without(&automator, "--pool"))
                .unwrap_err()
                .unwrap()
                .contains("--pool is required")
        );
    }

    /// A chain with the three contracts: the executor's operator is `operator`.
    fn chain_with(operator: Address) -> Arc<MockChain> {
        let word = |value: Vec<u8>| value;
        let mut views = HashMap::new();
        views.insert(
            (EXECUTOR, operatorCall::SELECTOR),
            word(operator.abi_encode()),
        );
        views.insert(
            (FEED, providerIdCall::SELECTOR),
            word(PROVIDER.abi_encode()),
        );
        views.insert(
            (FEED, queryCommitmentCall::SELECTOR),
            word(INSTRUMENT.abi_encode()),
        );
        views.insert((FEED, minPriceCall::SELECTOR), word(e18().abi_encode()));
        views.insert(
            (FEED, maxPriceCall::SELECTOR),
            word((e18() * U256::from(1_000_000u32)).abi_encode()),
        );
        views.insert(
            (POOL, batchSizeCall::SELECTOR),
            word(U256::from(16u8).abi_encode()),
        );
        Arc::new(MockChain {
            chain_id: 31_337,
            code: [(EXECUTOR, 100), (FEED, 100), (POOL, 100)]
                .into_iter()
                .collect(),
            views,
            ..MockChain::default()
        })
    }

    use alloy_sol_types::SolCall;

    #[tokio::test]
    async fn the_key_must_hold_its_role_on_chain() {
        let key = Address::repeat_byte(0x0a);
        let operator = parse(OPERATOR).unwrap();
        // The operator's key is the executor's operator: the policy is read.
        let policy = read_policy(&operator, &chain_with(key), key).await.unwrap();
        assert_eq!((policy.to, policy.calls), (EXECUTOR, Calls::Rebalance));
        // Any other key is refused: every rebalance would revert.
        let message = read_policy(&operator, &chain_with(Address::repeat_byte(1)), key)
            .await
            .unwrap_err();
        assert!(message.contains("not this key"), "{message}");

        // The relayer reads the feed's configuration, and its key may not be the operator's.
        let relayer = Args {
            role: Role::Relayer,
            feed: Some(FEED),
            ..parse(OPERATOR).unwrap()
        };
        let policy = read_policy(&relayer, &chain_with(Address::repeat_byte(1)), key)
            .await
            .unwrap();
        assert_eq!(
            policy.calls,
            Calls::Submit {
                provider_id: PROVIDER,
                query_commitment: INSTRUMENT,
                min_price: e18(),
                max_price: e18() * U256::from(1_000_000u32)
            }
        );
        let message = read_policy(&relayer, &chain_with(key), key)
            .await
            .unwrap_err();
        assert!(message.contains("needs its own key"), "{message}");

        // The automator reads the pool's batch size.
        let automator = Args {
            role: Role::Automator,
            pool: Some(POOL),
            ..parse(OPERATOR).unwrap()
        };
        let policy = read_policy(&automator, &chain_with(Address::repeat_byte(1)), key)
            .await
            .unwrap();
        assert_eq!(policy.calls, Calls::SettleBatch { max_notes: 16 });

        // The wrong chain, or no contract at the destination.
        let mut wrong = parse(OPERATOR).unwrap();
        wrong.chain_id = 1;
        assert!(
            read_policy(&wrong, &chain_with(key), key)
                .await
                .unwrap_err()
                .contains("chain 31337")
        );
        let empty = Arc::new(MockChain {
            chain_id: 31_337,
            ..MockChain::default()
        });
        assert!(
            read_policy(&operator, &empty, key)
                .await
                .unwrap_err()
                .contains("no contract")
        );
    }

    #[tokio::test]
    async fn the_vault_signs_with_its_kms_key() {
        let key = test_key("vault test: operator");
        let kms = FakeKms::start(&[("operator-key", key.clone())]).await;
        let signer = kms::signer(&kms.config("operator-key")).await.unwrap();
        assert_eq!(signer.address(), address_of(&key));
        let address = signer.address();
        let chain = chain_with(address);
        let args = parse(OPERATOR).unwrap();
        let policy = read_policy(&args, &chain, address).await.unwrap();
        let vault = Vault::new(
            policy,
            Arc::new(signer),
            "aws-kms",
            0,
            20.0,
            Window::new(args.window, args.caps.clone()),
            NonceGuard::with_chain(0, 0, 1),
            Some(chain),
        );
        let tx = TxEip1559 {
            nonce: 0,
            ..rebalance_tx()
        };
        // KMS may return a high-s signature: the signer normalizes it, so the vault's always recovers.
        for high_s in [false, true] {
            kms.set_high_s(high_s);
            let reply: Value = vault.handle(&sign_request(1, &tx)).await;
            let result = &reply["result"];
            assert!(result.is_object(), "{reply}");
            let signing_hash: B256 = result["signing_hash"].as_str().unwrap().parse().unwrap();
            assert_eq!(signing_hash, alloy_primitives::keccak256(raw(&tx)));
            let s: U256 = result["s"]
                .as_str()
                .unwrap()
                .parse::<B256>()
                .unwrap()
                .into();
            assert!(
                s <= U256::from_be_slice(
                    &hex::decode(
                        "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0"
                    )
                    .unwrap()
                )
            );
            let signature = alloy_primitives::Signature::new(
                result["r"]
                    .as_str()
                    .unwrap()
                    .parse::<B256>()
                    .unwrap()
                    .into(),
                s,
                result["y_parity"].as_u64() == Some(1),
            );
            assert_eq!(
                signature
                    .recover_address_from_prehash(&signing_hash)
                    .unwrap(),
                address
            );
        }
        // Each signature is one KMS Sign of the transaction's signing hash; the key never left the stand-in.
        assert_eq!(kms.signed().len(), 2);
        assert!(
            kms.signed().iter().all(|(id, digest)| id == "operator-key"
                && *digest == alloy_primitives::keccak256(raw(&tx)))
        );
    }

    #[tokio::test]
    async fn a_kms_key_that_does_not_exist_fails_at_start() {
        let kms = FakeKms::start(&[("operator-key", test_key("vault test: operator"))]).await;
        let message = kms::signer(&kms.config("another-key")).await.unwrap_err();
        assert!(message.contains("KMS key another-key"), "{message}");
    }

    #[cfg(feature = "insecure-test-key")]
    #[test]
    fn a_test_key_file_must_be_private_and_well_formed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tsig-{}-testkey", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        // The end-to-end tests' key: keccak256 of a public phrase, so no key literal is stored anywhere. Its address
        // was computed independently with viem (privateKeyToAccount).
        let key =
            alloy_primitives::keccak256("strawberryswap insecure test key: end-to-end tests only");
        fs::write(&path, format!("{key}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_test_key(&path)
                .unwrap_err()
                .to_string()
                .contains("0600")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_test_key(&path).unwrap().address(),
            alloy_primitives::address!("0xf8e44d4517A0de6610113165659DaAd8cF9327B2")
        );
        fs::write(&path, "not hex").unwrap();
        assert!(load_test_key(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
        // The test key replaces KMS, and lets the operator run without a chain; the other roles still need one.
        let args = parse(&[
            "--role",
            "operator",
            "--chain-id",
            "31337",
            "--insecure-test-key-file",
            "/k",
            "--executor",
            "0x5FbDB2315678afecb367f032d93F642f64180aa3",
            "--max-fee-eth-per-window",
            "1",
            "--max-volume0-per-window",
            "1",
            "--max-volume1-per-window",
            "1",
        ])
        .unwrap();
        assert!(args.rpc_url.is_none() && args.kms.is_none());
        assert!(
            parse(&[
                "--role",
                "relayer",
                "--chain-id",
                "1",
                "--insecure-test-key-file",
                "/k",
                "--feed",
                "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512",
                "--max-fee-eth-per-window",
                "1"
            ])
            .unwrap_err()
            .unwrap()
            .contains("--rpc-url is required")
        );
    }
}
