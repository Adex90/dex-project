//! End to end on a live chain (anvil), with the real contracts, the oracle pipeline, the vault and the engine:
//!
//! ```text
//! stand-in exchanges ─► price-endpoint ─► notary (KMS) ─► relayer: Groth16 proof, KMS-signed push, every 2 s
//!                                                                    │
//!                                                                    ▼
//!  engine ── one batched read per tick ──► anvil: ZkPriceFeed ◄─ ZkDecoOracle ◄─ dev Groth16 verifier
//!    │ └── eth_call simulation                  DarkPool (WETH/tUSD) ◄── SearcherExecutor (operator: the vault)
//!    └── unsigned EIP-1559 ──► vault (Unix socket; INSECURE test key) ──► signed ──► eth_sendRawTransaction
//! ```
//!
//! The market moves from 3,456.78 to 3,500: the relayer proves and pushes the new price, the pool is now 1.25 % below
//! it, and the engine rebalances it through the vault. The test checks the rebalance on chain (the vault signed it, the
//! pool moved toward the oracle, the Treasury was paid), that nothing reverted, and scans the engine's memory for the
//! vault's key while it trades.
//!
//! CI only (`--ignored`): it needs anvil (ANVIL, default `anvil`), the contracts' forge build (contracts/out), node with
//! circuits/node_modules (the relayer's prover), the oracle crate's binaries and its `stand_ins` example
//! (ORACLE_BIN_DIR, e.g. oracle/target/debug) and the vault built with insecure-test-key (THRESHOLD_SIGNER_BIN).

mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, U256, b256, keccak256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, TransactionRequest};
use alloy::sol;
use alloy::sol_types::{SolCall, SolConstructor, SolEvent};
use common::{
    Proc, STARTUP, TempDir, VAULT_ADDRESS, VAULT_TIMEOUT_MS, key_encodings, scan_memory,
    spawn_vault, summary,
};
use serde_json::Value;

const CHAIN_ID: u64 = 421_614;
/// The testnet fixtures' provider and ETHUSD instrument (contracts/test/fixtures/FeedFixtures.sol).
const PROVIDER_ID: B256 =
    b256!("0x08abf07e8766aa49bf37ef22bd0341a2953a237b4933784bd9fee44bda8be771");
const ETHUSD_QUERY_COMMITMENT: B256 =
    b256!("0x16deddae37e6a51b4a9b7e8ee87c29e94b8f38e75a2ad04d1d883956ffe5a162");
const SALT_LABEL: &str = "StrawberrySwap.testnet.ETHUSD.salt";
/// BN254's scalar field: the salt is keccak256(label) reduced into it.
const BN254_R: U256 = U256::from_limbs([
    0x43e1f593f0000001,
    0x2833e84879b97091,
    0xb85045b68181585d,
    0x30644e72e131a029,
]);

sol! {
    #[sol(rpc)]
    contract MockERC20 {
        constructor(string n);
        function mint(address to, uint256 a) external;
        function approve(address s, uint256 a) external returns (bool);
        function balanceOf(address) external view returns (uint256);
    }

    #[sol(rpc)]
    contract MockVerifier {
        constructor(bool accept_);
    }

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
    }

    #[sol(rpc)]
    contract DarkPool {
        constructor(
            address token0_,
            address token1_,
            uint256 virtual0_,
            uint256 virtual1_,
            address joinSplitVerifier_,
            address batchVerifier_,
            address hasher_,
            address oracle_,
            address treasury_,
            uint256 batchSize_
        );
        function addLiquidity(uint256 amount0, uint256 amount1, address to) external returns (uint256 minted);
        function spotPrice() external view returns (uint256);
        event Rebalanced(address indexed searcher, uint256 harvest, uint256 treasuryPaid, uint256 priceBefore);
    }

    #[sol(rpc)]
    contract SearcherExecutor {
        constructor(address operator_, address pool_);
    }
}

fn wad(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle_bin(name: &str) -> PathBuf {
    let dir = std::env::var_os("ORACLE_BIN_DIR")
        .map(PathBuf::from)
        .expect("ORACLE_BIN_DIR: the oracle crate's target/debug (cargo build --bins --example stand_ins)");
    if name == "stand_ins" {
        dir.join("examples").join(name)
    } else {
        dir.join(name)
    }
}

/// The creation bytecode of a forge build artifact (contracts/out/<file>/<contract>.json).
fn bytecode(file: &str, contract: &str) -> Vec<u8> {
    let path = repo()
        .join("contracts/out")
        .join(file)
        .join(format!("{contract}.json"));
    let artifact: Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run forge build)", path.display())),
    )
    .unwrap();
    let hex = artifact["bytecode"]["object"]
        .as_str()
        .unwrap_or_else(|| panic!("{}: no bytecode", path.display()));
    alloy::primitives::hex::decode(hex).unwrap()
}

async fn deploy(provider: &DynProvider, from: Address, code: Vec<u8>) -> Address {
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_deploy_code(code);
    let receipt = provider
        .send_transaction(tx)
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "a deployment reverted");
    receipt.contract_address.unwrap()
}

/// Sends a call from an unlocked anvil account and waits for its success.
async fn transact(provider: &DynProvider, from: Address, to: Address, input: Vec<u8>) {
    let tx = TransactionRequest::default()
        .with_from(from)
        .with_to(to)
        .with_input(input);
    let receipt = provider
        .send_transaction(tx)
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "a set-up call to {to} reverted");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A service with test credentials and region for the stand-in KMS, and no AWS configuration of the runner's.
fn service(program: PathBuf) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("AWS_ACCESS_KEY_ID", "AKIDFAKEKMSTEST")
        .env("AWS_SECRET_ACCESS_KEY", "fake-kms-test-secret")
        .env("AWS_REGION", "us-east-1")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_CONFIG_FILE", "/nonexistent/aws-config")
        .env(
            "AWS_SHARED_CREDENTIALS_FILE",
            "/nonexistent/aws-credentials",
        );
    command
}

/// The value of `key=` in a line such as `LISTENING kms=http://… exchanges=http://…`.
fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        .to_string()
}

/// The engine's JSON event lines of one kind.
fn events(stdout: &str, kind: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == kind)
        .collect()
}

#[tokio::test]
#[ignore = "needs anvil, the forge build, node, the oracle binaries and the vault: CI runs it with --ignored"]
async fn the_engine_rebalances_the_live_pool_through_the_vault() {
    let dir = TempDir::new("live");

    // ---- anvil
    let port = free_port();
    let rpc = format!("http://127.0.0.1:{port}");
    let anvil_bin = std::env::var("ANVIL").unwrap_or_else(|_| "anvil".into());
    let _anvil = Proc::spawn(
        "anvil",
        Command::new(&anvil_bin).args([
            "--port",
            &port.to_string(),
            "--chain-id",
            &CHAIN_ID.to_string(),
            "--block-time",
            "1",
            "--silent",
        ]),
    );
    let provider = ProviderBuilder::new()
        .connect_http(rpc.parse().unwrap())
        .erased();
    let deadline = Instant::now() + STARTUP;
    while provider.get_chain_id().await.is_err() {
        assert!(Instant::now() < deadline, "anvil did not start");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let deployer = provider.get_accounts().await.unwrap()[0];

    // ---- the stand-ins: KMS (the notary's and the relayer's keys) and the exchanges at 3456.78
    let stand_ins = Proc::spawn(
        "stand-ins",
        Command::new(oracle_bin("stand_ins")).args([
            "--price",
            "3456.78",
            "--key",
            "notary-key=e2e: notary",
            "--key",
            "relayer-key=e2e: relayer",
        ]),
    );
    let notary_line = stand_ins.wait_for(STARTUP, |l| l.starts_with("KEY notary-key "));
    let notary: Address = notary_line
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    let relayer_line = stand_ins.wait_for(STARTUP, |l| l.starts_with("KEY relayer-key "));
    let relayer: Address = relayer_line
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    let listening = stand_ins.wait_for(STARTUP, |l| l.starts_with("LISTENING "));
    let (kms, exchanges, admin) = (
        field(&listening, "kms"),
        field(&listening, "exchanges"),
        field(&listening, "admin"),
    );

    // ---- the contracts
    let token_a = deploy(
        &provider,
        deployer,
        [
            bytecode("Mocks.sol", "MockERC20"),
            MockERC20::constructorCall { n: "WETH".into() }.abi_encode(),
        ]
        .concat(),
    )
    .await;
    let token_b = deploy(
        &provider,
        deployer,
        [
            bytecode("Mocks.sol", "MockERC20"),
            MockERC20::constructorCall { n: "tUSD".into() }.abi_encode(),
        ]
        .concat(),
    )
    .await;
    // The oracle prices token0 in token1, so WETH must sort first (Phase 5 must deploy tUSD above WETH's address).
    assert!(token_a < token_b, "WETH must be token0 for an ETHUSD price");
    let (weth, tusd) = (token_a, token_b);
    let treasury = deploy(&provider, deployer, bytecode("Mocks.sol", "MockTreasury")).await;
    let accept = MockVerifier::constructorCall { accept_: true }.abi_encode();
    let join_split = deploy(
        &provider,
        deployer,
        [bytecode("Mocks.sol", "MockVerifier"), accept.clone()].concat(),
    )
    .await;
    let batch = deploy(
        &provider,
        deployer,
        [bytecode("Mocks.sol", "MockVerifier"), accept].concat(),
    )
    .await;
    let hasher = deploy(&provider, deployer, bytecode("Mocks.sol", "MockHasher")).await;
    let verifier = deploy(
        &provider,
        deployer,
        bytecode("OracleGroth16Verifier.dev.sol", "OracleGroth16Verifier"),
    )
    .await;
    let args = ZkDecoOracle::constructorCall {
        verifier_: verifier,
        verifierGas_: U256::from(600_000u64),
        providerIds: vec![PROVIDER_ID],
        notaries: vec![notary],
    };
    let zkdeco = deploy(
        &provider,
        deployer,
        [
            bytecode("ZkDecoOracle.sol", "ZkDecoOracle"),
            args.abi_encode(),
        ]
        .concat(),
    )
    .await;
    let args = ZkPriceFeed::constructorCall {
        c: ZkPriceFeed::Config {
            verifier: zkdeco,
            providerId: PROVIDER_ID,
            queryCommitment: ETHUSD_QUERY_COMMITMENT,
            maxSubmissionDelay: 30,
            minPrice: wad(100),
            maxPrice: wad(100_000),
        },
    };
    let feed = deploy(
        &provider,
        deployer,
        [
            bytecode("ZkPriceFeed.sol", "ZkPriceFeed"),
            args.abi_encode(),
        ]
        .concat(),
    )
    .await;
    // The pool at the starting price: 1000 WETH and 3,456,780 tUSD, real and virtual alike.
    let args = DarkPool::constructorCall {
        token0_: weth,
        token1_: tusd,
        virtual0_: wad(1_000),
        virtual1_: wad(3_456_780),
        joinSplitVerifier_: join_split,
        batchVerifier_: batch,
        hasher_: hasher,
        oracle_: feed,
        treasury_: treasury,
        batchSize_: U256::from(4u64),
    };
    let pool = deploy(
        &provider,
        deployer,
        [bytecode("DarkPool.sol", "DarkPool"), args.abi_encode()].concat(),
    )
    .await;
    let args = SearcherExecutor::constructorCall {
        operator_: VAULT_ADDRESS,
        pool_: pool,
    };
    let executor = deploy(
        &provider,
        deployer,
        [
            bytecode("SearcherExecutor.sol", "SearcherExecutor"),
            args.abi_encode(),
        ]
        .concat(),
    )
    .await;
    for (token, amount) in [(weth, wad(1_000)), (tusd, wad(3_456_780))] {
        transact(
            &provider,
            deployer,
            token,
            MockERC20::mintCall {
                to: deployer,
                a: amount,
            }
            .abi_encode(),
        )
        .await;
        transact(
            &provider,
            deployer,
            token,
            MockERC20::approveCall {
                s: pool,
                a: U256::MAX,
            }
            .abi_encode(),
        )
        .await;
    }
    let add = DarkPool::addLiquidityCall {
        amount0: wad(1_000),
        amount1: wad(3_456_780),
        to: deployer,
    };
    transact(&provider, deployer, pool, add.abi_encode()).await;
    // The executor's inventory: 20 WETH and 100,000 tUSD. Gas for the vault's and the relayer's addresses.
    transact(
        &provider,
        deployer,
        weth,
        MockERC20::mintCall {
            to: executor,
            a: wad(20),
        }
        .abi_encode(),
    )
    .await;
    transact(
        &provider,
        deployer,
        tusd,
        MockERC20::mintCall {
            to: executor,
            a: wad(100_000),
        }
        .abi_encode(),
    )
    .await;
    for account in [VAULT_ADDRESS, relayer] {
        let _: Value = provider
            .raw_request("anvil_setBalance".into(), (account, wad(10)))
            .await
            .unwrap();
    }
    let darkpool = DarkPool::new(pool, &provider);
    let spot_before = darkpool.spotPrice().call().await.unwrap();
    assert_eq!(spot_before, U256::from(3_456_780_000_000_000_000_000u128));

    // ---- the oracle pipeline: endpoint, notary, relayer (a push every 2 s)
    let sources = ["coinbase", "kraken", "bitstamp"]
        .map(|e| format!("{e}={exchanges}"))
        .join(",");
    let mut command = service(oracle_bin("price-endpoint"));
    command.args([
        "--listen",
        "127.0.0.1:0",
        "--sources",
        &sources,
        "--poll-ms",
        "200",
    ]);
    let endpoint = Proc::spawn("price-endpoint", &mut command);
    let endpoint_line = endpoint.wait_for(STARTUP, |l| l.starts_with("LISTENING "));
    let endpoint_url = format!(
        "http://{}",
        endpoint_line.trim_start_matches("LISTENING ").trim()
    );
    let mut command = service(oracle_bin("notary"));
    command.args([
        "--listen",
        "127.0.0.1:0",
        "--provider-url",
        &format!("{endpoint_url}/v1/ETHUSD"),
        "--provider-id",
        &PROVIDER_ID.to_string(),
        "--kms-key-id",
        "notary-key",
        "--kms-endpoint",
        &kms,
        "--kms-region",
        "us-east-1",
        "--allow-http",
    ]);
    let notary_proc = Proc::spawn("notary", &mut command);
    let notary_line = notary_proc.wait_for(STARTUP, |l| l.starts_with("LISTENING "));
    let notary_url = format!(
        "http://{}",
        notary_line.trim_start_matches("LISTENING ").trim()
    );

    let salt = U256::from_be_bytes(keccak256(SALT_LABEL).0) % BN254_R;
    let salt_file = dir.0.join("ethusd.salt");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&salt_file)
        .unwrap();
    writeln!(file, "{salt:#x}").unwrap();
    drop(file);
    let oracle_dir = repo().join("oracle");
    let mut command = service(oracle_bin("relayer"));
    command.args([
        "--rpc-url",
        &rpc,
        "--chain-id",
        &CHAIN_ID.to_string(),
        "--feed",
        &feed.to_string(),
        "--notary-url",
        &notary_url,
        "--salt-file",
        salt_file.to_str().unwrap(),
        "--kms-key-id",
        "relayer-key",
        "--kms-endpoint",
        &kms,
        "--kms-region",
        "us-east-1",
        "--zk-dir",
        oracle_dir.join("zk").to_str().unwrap(),
        "--prover-script",
        repo()
            .join("circuits/scripts/prove-zkdeco.mjs")
            .to_str()
            .unwrap(),
        "--interval-ms",
        "2000",
        "--max-fee-wei",
        "100000000000",
    ]);
    let relayer_proc = Proc::spawn("relayer", &mut command);
    relayer_proc.wait_for(Duration::from_secs(120), |l| l.starts_with("PUSHED "));

    // ---- the vault (operator) and the engine, live
    let mut vault = spawn_vault(&dir, CHAIN_ID, executor, &[]);
    let mut engine = Proc::spawn(
        "engine",
        Command::new(env!("CARGO_BIN_EXE_hft-engine"))
            .arg("--vault-socket")
            .arg(dir.socket())
            .args([
                "--vault-timeout-ms",
                VAULT_TIMEOUT_MS,
                "--rpc-url",
                &rpc,
                "--chain-id",
                &CHAIN_ID.to_string(),
                "--executor",
                &executor.to_string(),
                "--feed",
                &feed.to_string(),
                "--target-token0",
                "20",
                "--gas-limit",
                "500000",
                "--json",
                "--metrics-every",
                "20",
                "--log-txs",
            ]),
    );
    let banner = engine.wait_for(STARTUP, |l| l.starts_with("hft-engine: LIVE"));
    assert!(banner.contains(&CHAIN_ID.to_string()), "{banner}");
    let deployment = engine.wait_for(STARTUP, |l| l.starts_with("deployment:"));
    assert!(
        deployment.contains(&pool.to_string()) && deployment.contains(&feed.to_string()),
        "{deployment}"
    );
    // On the starting price the pool sits on the oracle: nothing worth a rebalance.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        events(&engine.stdout(), "sent").is_empty(),
        "{}",
        engine.transcript()
    );

    // ---- the market moves to 3500: the relayer proves and pushes it, the engine rebalances the pool toward it
    let moved = Command::new("curl")
        .args([
            "-s",
            "--noproxy",
            "*",
            "-X",
            "POST",
            "-d",
            "3500.00",
            &format!("{admin}/price"),
        ])
        .status()
        .unwrap();
    assert!(moved.success());
    relayer_proc.wait_for(Duration::from_secs(60), |l| {
        l.starts_with("PUSHED ") && l.contains("price=3500000000000000000000")
    });
    let mined = engine.wait_for(Duration::from_secs(60), |l| {
        l.contains("\"event\":\"mined\"")
    });
    let mined: Value = serde_json::from_str(&mined).unwrap();
    assert_eq!(mined["status"], "success", "{mined}");
    assert_eq!(
        mined["from"].as_str().unwrap().parse::<Address>().unwrap(),
        VAULT_ADDRESS
    );
    let tx: B256 = mined["tx"].as_str().unwrap().parse().unwrap();

    // ---- while it trades: the key is in the vault's memory, nowhere in the engine's
    let needles = key_encodings();
    let in_vault = scan_memory(vault.pid(), &needles);
    assert!(
        !in_vault.found.is_empty(),
        "positive control: the scan finds the key in the vault: {in_vault:?}"
    );
    let in_engine = scan_memory(engine.pid(), &needles);
    assert!(
        in_engine.bytes > 4 << 20 && in_engine.mappings > 10,
        "the scan read the engine: {in_engine:?}"
    );
    assert!(
        in_engine.found.is_empty(),
        "the key is in the live engine's memory as {:?}",
        in_engine.found
    );
    eprintln!(
        "memory scan (live): vault key found as {:?}; engine {} MiB in {} mappings, key found as {:?}",
        in_vault.found,
        in_engine.bytes >> 20,
        in_engine.mappings,
        in_engine.found
    );

    // ---- on chain
    let receipt = provider
        .get_transaction_receipt(tx)
        .await
        .unwrap()
        .expect("the rebalance's receipt");
    assert!(receipt.status());
    assert_eq!((receipt.from, receipt.to), (VAULT_ADDRESS, Some(executor)));
    let rebalanced = provider
        .get_logs(
            &Filter::new()
                .address(pool)
                .event_signature(DarkPool::Rebalanced::SIGNATURE_HASH)
                .from_block(0),
        )
        .await
        .unwrap();
    assert!(!rebalanced.is_empty());
    for log in &rebalanced {
        let event = log.log_decode::<DarkPool::Rebalanced>().unwrap().inner.data;
        assert_eq!(event.searcher, executor);
        assert!(event.treasuryPaid > U256::ZERO);
    }
    let spot_after = darkpool.spotPrice().call().await.unwrap();
    let oracle = wad(3_500);
    let gap = |p: U256| if p > oracle { p - oracle } else { oracle - p };
    assert!(
        gap(spot_after) < gap(spot_before),
        "the pool moved toward the oracle: {spot_before} → {spot_after}"
    );
    let paid = MockERC20::new(tusd, &provider)
        .balanceOf(treasury)
        .call()
        .await
        .unwrap();
    assert!(paid > U256::ZERO, "the Treasury was paid its cut");

    // ---- the engine stops cleanly and accounts for everything
    assert!(
        engine.terminate(Duration::from_secs(10)).success(),
        "{}",
        engine.transcript()
    );
    let stdout = engine.stdout();
    let s = summary(&stdout);
    assert!(s["sent"] >= 1.0 && s["mined"] >= 1.0, "{s:?}");
    assert_eq!(
        (s["reverted"], s["rejected"], s["withdrawn"]),
        (0.0, 0.0, 0.0),
        "{s:?}\n{}",
        engine.transcript()
    );
    let metrics = events(&stdout, "metrics");
    let last = metrics.last().expect("a metrics snapshot");
    assert!(last["latency"]["read"]["count"].as_u64().unwrap() >= 20);
    assert!(
        last["latency"]["simulate"]["count"].as_u64().unwrap()
            >= last["counters"]["sent"].as_u64().unwrap(),
        "every broadcast was simulated first: {last}"
    );
    eprintln!(
        "live engine: {} sent, {} mined, {} beaten; spot {spot_before} → {spot_after}; treasury paid {paid}",
        s["sent"], s["mined"], s["beaten"]
    );
    assert!(vault.terminate(Duration::from_secs(10)).success());
    assert!(
        vault.stderr().contains(&format!("tx {tx}")),
        "the vault's audit log has the rebalance"
    );
}
