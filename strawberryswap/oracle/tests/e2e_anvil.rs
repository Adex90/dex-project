//! End to end, every piece real but the exchanges and KMS: the three binaries and the relayer vault as separate
//! processes, the dev Groth16 verifier, ZkDecoOracle and ZkPriceFeed deployed on anvil, and the relayer's pushes,
//! signed by the relayer vault with the relayer's KMS key, landing on chain.
//!
//! ```text
//! stand-in exchanges ─► price-endpoint ─► notary (KMS) ─► relayer (proof) ─► relayer vault (KMS) ─► ZkPriceFeed
//! ```
//!
//! It needs anvil, the contracts' forge build (contracts/out) and the vault's binary, so it is ignored by default; CI
//! runs it: `forge build` in strawberryswap/contracts, the vault built, then `cargo test --test e2e_anvil -- --ignored`.
//! `ANVIL` overrides the anvil executable, `THRESHOLD_SIGNER_BIN` the vault's.

mod common;

use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, TransactionRequest};
use alloy::sol;
use alloy::sol_types::{SolConstructor, SolEvent};
use common::{FakeExchanges, FakeKms, address_of, ethusd_commitment, provider_id, salt, test_key};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use zk_oracle::body::Price;

sol! {
    #[sol(rpc)]
    contract ZkDecoOracle {
        constructor(address verifier_, uint256 verifierGas_, bytes32[] providerIds, address[] notaries);
    }

    #[sol(rpc)]
    contract ZkPriceFeed {
        struct Config {
            address verifier;
            bytes32 providerId;
            bytes32 queryCommitment;
            uint64 maxSubmissionDelay;
            uint256 minPrice;
            uint256 maxPrice;
        }

        constructor(Config c);
        function latestVerifiedPrice() external view returns (uint256 price1e18, uint64 updatedAt);
        event PriceUpdated(uint256 price, uint64 observedAt, address indexed relayer);
    }
}

const CHAIN_ID: u64 = 421_614; // Arbitrum Sepolia's id, as the testnet relayer runs with

fn contracts_out() -> PathBuf {
    common::crate_dir().join("../contracts/out")
}

/// The creation bytecode of a forge build artifact.
fn bytecode(file: &str, contract: &str) -> Vec<u8> {
    let path = contracts_out().join(file).join(format!("{contract}.json"));
    let artifact: Value = serde_json::from_str(&common::read(&path)).unwrap();
    let hex = artifact["bytecode"]["object"].as_str().unwrap_or_else(|| panic!("{}: no bytecode", path.display()));
    alloy::hex::decode(hex).unwrap()
}

/// Kills the process when the test ends, however it ends.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start_anvil() -> (Process, String) {
    let port = free_port();
    let anvil = std::env::var("ANVIL").unwrap_or_else(|_| "anvil".into());
    let child = Command::new(&anvil)
        .args(["--port", &port.to_string(), "--chain-id", &CHAIN_ID.to_string(), "--block-time", "1", "--silent"])
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|e| panic!("{anvil}: {e} (install Foundry, or set ANVIL)"));
    let url = format!("http://127.0.0.1:{port}");
    let provider = ProviderBuilder::new().connect_http(url.parse().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    while provider.get_chain_id().await.is_err() {
        assert!(Instant::now() < deadline, "anvil did not start");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    (Process(child), url)
}

async fn deploy(provider: &DynProvider, from: Address, code: Vec<u8>) -> Address {
    let tx = TransactionRequest::default().with_from(from).with_deploy_code(code);
    let receipt = provider.send_transaction(tx).await.unwrap().get_receipt().await.unwrap();
    assert!(receipt.status(), "deployment reverted");
    receipt.contract_address.unwrap()
}

/// The environment the services get: PATH (for node), test credentials and region for the stand-in KMS, and no
/// AWS configuration files or instance metadata.
fn service(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("AWS_ACCESS_KEY_ID", "AKIDFAKEKMSTEST")
        .env("AWS_SECRET_ACCESS_KEY", "fake-kms-test-secret")
        .env("AWS_REGION", "us-east-1")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_CONFIG_FILE", "/nonexistent/aws-config")
        .env("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/aws-credentials")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command
}

/// Starts a service and waits for its `LISTENING <address>` line.
async fn listening(mut command: Command) -> (Process, String) {
    let mut child = command.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await.unwrap().unwrap().unwrap();
    let address = line.strip_prefix("LISTENING ").unwrap_or_else(|| panic!("unexpected first line {line:?}"));
    (Process(child), format!("http://{address}"))
}

fn salt_file(dir: &Path, name: &str, salt: U256) -> PathBuf {
    let path = dir.join(name);
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
    writeln!(file, "{salt:#x}").unwrap();
    path
}

/// The relayer vault: the relayer's key in the stand-in KMS, `ZkPriceFeed.submit` only. Returns once it listens.
async fn relayer_vault(rpc: &str, feed: Address, kms: &FakeKms, socket: &Path) -> Process {
    let binary = std::env::var_os("THRESHOLD_SIGNER_BIN").map_or_else(
        || common::crate_dir().join("../vaults/threshold-signer/target/debug/threshold-signer"),
        PathBuf::from,
    );
    let mut command = service(binary.to_str().unwrap());
    command.args([
        "--role",
        "relayer",
        "--chain-id",
        &CHAIN_ID.to_string(),
        "--rpc-url",
        rpc,
        "--kms-key-id",
        "relayer-key",
        "--kms-endpoint",
        &kms.url,
        "--kms-region",
        "us-east-1",
        "--feed",
        &feed.to_string(),
        "--max-fee-eth-per-window",
        "100",
        "--socket",
        socket.to_str().unwrap(),
    ]);
    let mut child = command.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .expect("the vault did not start")
            .unwrap()
            .expect("the vault exited");
        println!("vault: {line}");
        if line.starts_with("listening on") {
            break;
        }
    }
    Process(child)
}

fn relayer(rpc: &str, feed: Address, notary: &str, salt_file: &Path, vault: &Path, cycles: u64) -> Command {
    let crate_dir = common::crate_dir();
    let mut command = service(env!("CARGO_BIN_EXE_relayer"));
    command.args([
        "--rpc-url",
        rpc,
        "--chain-id",
        &CHAIN_ID.to_string(),
        "--feed",
        &feed.to_string(),
        "--notary-url",
        notary,
        "--salt-file",
        salt_file.to_str().unwrap(),
        "--vault-socket",
        vault.to_str().unwrap(),
        "--zk-dir",
        crate_dir.join("zk").to_str().unwrap(),
        "--prover-script",
        crate_dir.join("../circuits/scripts/prove-zkdeco.mjs").to_str().unwrap(),
        "--interval-ms",
        "3000",
        "--max-cycles",
        &cycles.to_string(),
        "--max-fee-wei",
        "100000000000",
    ]);
    command
}

/// The relayer's PUSHED lines: (observedAt, price).
fn pushed(line: &str) -> Option<(u64, U256)> {
    let rest = line.strip_prefix("PUSHED ")?;
    let field = |key: &str| rest.split_whitespace().find_map(|kv| kv.strip_prefix(key)).map(str::to_string);
    Some((field("observedAt=")?.parse().ok()?, field("price=")?.parse().ok()?))
}

async fn next_line(lines: &mut Lines<BufReader<ChildStdout>>) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(120), lines.next_line()).await.expect("the relayer went quiet").unwrap()
}

#[tokio::test]
#[ignore = "needs anvil and the contracts' forge build: CI runs it with --ignored"]
async fn the_pipeline_keeps_the_feed_fresh_on_chain() {
    let wad = |p: &str| Price::parse_quote(p).unwrap().wad();
    let (_anvil, rpc) = start_anvil().await;
    let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap()).erased();
    let deployer = provider.get_accounts().await.unwrap()[0];

    // KMS: the notary's key and the relayer's gas key (the relayer vault's). Its address gets test ETH for gas.
    let notary_key = test_key("e2e: notary");
    let relayer_key = test_key("e2e: relayer");
    let kms = FakeKms::start(&[("notary-key", notary_key.clone()), ("relayer-key", relayer_key.clone())]).await;
    let relayer_address = address_of(&relayer_key);
    let _: Value = provider
        .raw_request("anvil_setBalance".into(), (relayer_address, U256::from(10u64).pow(U256::from(19))))
        .await
        .unwrap();

    // The contracts: the dev verifier, ZkDecoOracle registering the notary for the provider, and the feed.
    let verifier =
        deploy(&provider, deployer, bytecode("OracleGroth16Verifier.dev.sol", "OracleGroth16Verifier")).await;
    let args = ZkDecoOracle::constructorCall {
        verifier_: verifier,
        verifierGas_: U256::from(600_000),
        providerIds: vec![provider_id()],
        notaries: vec![address_of(&notary_key)],
    };
    let oracle =
        deploy(&provider, deployer, [bytecode("ZkDecoOracle.sol", "ZkDecoOracle"), args.abi_encode()].concat()).await;
    let args = ZkPriceFeed::constructorCall {
        c: ZkPriceFeed::Config {
            verifier: oracle,
            providerId: provider_id(),
            queryCommitment: ethusd_commitment(),
            maxSubmissionDelay: 30,
            minPrice: wad("100"),
            maxPrice: wad("100000"),
        },
    };
    let feed =
        deploy(&provider, deployer, [bytecode("ZkPriceFeed.sol", "ZkPriceFeed"), args.abi_encode()].concat()).await;
    let feed_contract = ZkPriceFeed::new(feed, &provider);
    let before = feed_contract.latestVerifiedPrice().call().await.unwrap();
    assert_eq!((before.price1e18, before.updatedAt), (U256::ZERO, 0));

    // The services.
    let exchanges = FakeExchanges::start().await;
    exchanges.set_all(Some("3456.70"), Some("3456.78"), Some("3456.90"));
    let sources = ["coinbase", "kraken", "bitstamp"].map(|e| format!("{e}={}", exchanges.base)).join(",");
    let mut command = service(env!("CARGO_BIN_EXE_price-endpoint"));
    command.args(["--listen", "127.0.0.1:0", "--sources", &sources, "--poll-ms", "200"]);
    let (_endpoint, endpoint_url) = listening(command).await;

    let mut command = service(env!("CARGO_BIN_EXE_notary"));
    command.args([
        "--listen",
        "127.0.0.1:0",
        "--provider-url",
        &format!("{endpoint_url}/v1/ETHUSD"),
        "--provider-id",
        &provider_id().to_string(),
        "--kms-key-id",
        "notary-key",
        "--kms-endpoint",
        &kms.url,
        "--kms-region",
        "us-east-1",
        "--allow-http",
    ]);
    let (_notary, notary_url) = listening(command).await;

    let dir = std::env::temp_dir().join(format!("zk-oracle-e2e-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("vault").join("relayer.sock");
    let _vault = relayer_vault(&rpc, feed, &kms, &socket).await;

    // A relayer with the wrong salt never starts: its instrument is not the feed's.
    let wrong = salt_file(&dir, "wrong-salt", salt() + U256::from(1));
    let output = relayer(&rpc, feed, &notary_url, &wrong, &socket, 1).stderr(Stdio::piped()).output().await.unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("wrong salt"), "{stderr}");

    // The relayer: four cycles, 3 s apart. After its first push the market moves to 3500, and the feed follows.
    let right = salt_file(&dir, "salt", salt());
    let mut child = relayer(&rpc, feed, &notary_url, &right, &socket, 4).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut relayer_process = Process(child);
    let mut pushes = Vec::new();
    while let Some(line) = next_line(&mut lines).await {
        println!("relayer: {line}");
        if let Some(push) = pushed(&line) {
            if pushes.is_empty() {
                assert_eq!(push.1, wad("3456.78"), "the median of the three exchanges");
                exchanges.set_all(Some("3500.00"), Some("3500.00"), Some("3500.00"));
            }
            pushes.push(push);
        }
    }
    let status = tokio::time::timeout(Duration::from_secs(30), relayer_process.0.wait()).await.unwrap().unwrap();
    assert!(status.success(), "the relayer exited with {status}");
    fs::remove_dir_all(&dir).unwrap();

    assert!(pushes.len() >= 3, "at least three of four cycles push a new observation: {pushes:?}");
    assert!(pushes.windows(2).all(|w| w[1].0 > w[0].0), "each push is newer: {pushes:?}");
    let (last_at, last_price) = *pushes.last().unwrap();
    assert_eq!(last_price, wad("3500"), "the feed follows the market");

    // On chain: the feed holds the last push, and every push is a PriceUpdated from the relayer's KMS address (the
    // relayer vault's).
    let latest = feed_contract.latestVerifiedPrice().call().await.unwrap();
    assert_eq!((latest.price1e18, latest.updatedAt), (last_price, last_at));
    let block = provider.get_block_number().await.unwrap();
    let logs = provider
        .get_logs(&Filter::new().address(feed).event_signature(ZkPriceFeed::PriceUpdated::SIGNATURE_HASH).from_block(0))
        .await
        .unwrap();
    let events: Vec<(u64, U256, Address)> = logs
        .iter()
        .map(|log| {
            let event = log.log_decode::<ZkPriceFeed::PriceUpdated>().unwrap().inner.data;
            (event.observedAt, event.price, event.relayer)
        })
        .collect();
    let expected: Vec<(u64, U256, Address)> = pushes.iter().map(|&(at, price)| (at, price, relayer_address)).collect();
    assert_eq!(events, expected);
    println!("{} pushes on chain by {relayer_address}, up to block {block}", events.len());

    // The notary signed one digest per attestation, and KMS never saw the relayer's key sign anything else than
    // the pushes: the vault signed once per transaction, and nothing for the refused relayer.
    let signed = kms.signed();
    let by_relayer = signed.iter().filter(|(key, _)| key == "relayer-key").count();
    assert_eq!(by_relayer, pushes.len());
    assert!(signed.iter().any(|(key, _)| key == "notary-key"));
}
