//! End to end: the engine binary against the real threshold signer, over a temporary Unix socket.
//!
//! Both run as child processes of this test. The vault is built with its `insecure-test-key` feature, so that the
//! test knows the key it has to look for: keccak256 of a public phrase, also used by the engine's fixtures. The tests
//! show four things.
//!
//! 1. Every transaction the engine broadcasts is the vault's: each one, decoded and recovered here independently of
//!    the engine's own checks, is signed by the vault's address, and the vault's audit log lists it.
//! 2. The engine never holds the key. While it trades, its whole readable memory (/proc/<pid>/mem) is scanned for the
//!    key in four encodings: none is there. The same scan finds the key in the vault's memory, so the scan works.
//! 3. The engine has no other way to sign: with no vault, or with a vault whose policy refuses its transactions, it
//!    broadcasts nothing and withdraws every rebalance.
//! 4. No crate that loads, stores or generates a wallet key is in the engine's dependency graph.
//!
//! Linux only (/proc). Reading a process's memory needs ptrace access: the test is the parent of both processes, which
//! Yama's default policy (ptrace_scope 1) allows. Set THRESHOLD_SIGNER_BIN to use a prebuilt vault (CI does);
//! otherwise the test builds it with cargo.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy::consensus::TxEnvelope;
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, TxKind, U256, address, hex, keccak256};
use common::{
    Proc, STARTUP, TempDir, VAULT_ADDRESS, engine, key_encodings, scan_memory, spawn_vault, summary,
};

const EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");
const CHAIN_ID: u64 = 31_337;
/// `rebalance(uint256,uint256,uint256,uint256,uint256)`.
const REBALANCE_SELECTOR: [u8; 4] = [0xc0, 0x66, 0x17, 0x37];

// ---------------------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------------------

/// A `key=value` field of an event line.
fn field(line: &str, key: &str) -> String {
    let start = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        + key.len()
        + 2;
    line[start..].split(' ').next().unwrap().to_string()
}

/// One broadcast transaction from the engine's `SENT nonce=… tx=… raw=0x…` line.
struct Sent {
    nonce: u64,
    hash: String,
    raw: Vec<u8>,
}

fn parse_sent(line: &str) -> Sent {
    Sent {
        nonce: field(line, "nonce").parse().unwrap(),
        hash: field(line, "tx"),
        raw: hex::decode(field(line, "raw")).unwrap(),
    }
}

/// Its receipt, from the `MINED nonce=… tx=… from=… status=… block=… gas=… after_ms=…` line.
struct Mined {
    nonce: u64,
    hash: String,
    from: String,
    status: String,
}

fn parse_mined(line: &str) -> Mined {
    Mined {
        nonce: field(line, "nonce").parse().unwrap(),
        hash: field(line, "tx"),
        from: field(line, "from"),
        status: field(line, "status"),
    }
}

#[test]
fn the_engine_trades_through_the_vault_and_never_holds_its_key() {
    let dir = TempDir::new("trade");
    let mut vault = spawn_vault(&dir, CHAIN_ID, EXECUTOR, &[]);
    let mut engine = Proc::spawn(
        "engine",
        &mut engine(&dir.socket(), &["--log-txs", "--seed", "7"]),
    );
    let vault_line = engine.wait_for(STARTUP, |l| l.starts_with("vault:"));
    assert!(
        vault_line.contains(&VAULT_ADDRESS.to_string()),
        "the engine reports the vault's signer: {vault_line}"
    );

    // 1. Five rebalances, each checked here without the engine's help, and their receipts. (A read can miss its budget
    // on a loaded machine: a SKIPPED tick is not a failure, and is not waited for.)
    let (mut sent, mut mined) = (Vec::new(), Vec::new());
    while mined.len() < 5 {
        let line = engine.wait_for(Duration::from_secs(120), |l| {
            ["SENT ", "MINED ", "WITHDRAWN", "REJECTED", "BEATEN"]
                .iter()
                .any(|event| l.starts_with(event))
        });
        if line.starts_with("SENT ") {
            sent.push(parse_sent(&line));
        } else {
            assert!(
                line.starts_with("MINED "),
                "with the vault up nothing is withdrawn, rejected or beaten: {line}"
            );
            mined.push(parse_mined(&line));
        }
    }
    sent.truncate(5);
    for (i, tx) in sent.iter().enumerate() {
        let TxEnvelope::Eip1559(signed) = TxEnvelope::decode_2718(&mut tx.raw.as_slice()).unwrap()
        else {
            panic!("nonce {}: not an EIP-1559 transaction", tx.nonce);
        };
        assert_eq!(
            signed.recover_signer().unwrap(),
            VAULT_ADDRESS,
            "nonce {}: signed by someone else",
            tx.nonce
        );
        assert_eq!(keccak256(&tx.raw).to_string(), tx.hash);
        assert_eq!(signed.hash().to_string(), tx.hash);
        let body = signed.tx();
        assert_eq!(
            (body.chain_id, body.nonce, body.to, body.value),
            (CHAIN_ID, i as u64, TxKind::Call(EXECUTOR), U256::ZERO)
        );
        assert_eq!(
            (tx.nonce, body.input.len(), &body.input[..4]),
            (i as u64, 4 + 5 * 32, &REBALANCE_SELECTOR[..])
        );
        // One transaction in flight: each is mined before the next is sent.
        let receipt = &mined[i];
        assert_eq!(
            (receipt.nonce, &receipt.hash, receipt.status.as_str()),
            (tx.nonce, &tx.hash, "success"),
            "nonce {}: the mock chain executed it from the operator",
            tx.nonce
        );
        assert_eq!(receipt.from, VAULT_ADDRESS.to_string());
    }

    // 2. The key: in the vault's memory (the scan works), in none of its encodings in the engine's.
    let needles = key_encodings();
    let in_vault = scan_memory(vault.pid(), &needles);
    assert!(
        in_vault.found.contains("big-endian bytes")
            || in_vault.found.contains("little-endian bytes"),
        "positive control: the scan must find the key in the vault, or it proves nothing about the engine: {in_vault:?}"
    );
    let in_engine = scan_memory(engine.pid(), &needles);
    assert!(
        in_engine.bytes > 4 << 20 && in_engine.mappings > 10,
        "the scan read the engine's memory: {in_engine:?}"
    );
    assert!(
        in_engine.found.is_empty(),
        "the key is in the engine's memory as {:?}",
        in_engine.found
    );
    eprintln!(
        "memory scan: vault {} MiB, key found as {:?}; engine {} MiB in {} mappings ({} B unreadable), key found as {:?}",
        in_vault.bytes >> 20,
        in_vault.found,
        in_engine.bytes >> 20,
        in_engine.mappings,
        in_engine.unreadable,
        in_engine.found
    );

    // The engine stops cleanly on SIGTERM and accounts for every rebalance.
    assert!(
        engine.terminate(Duration::from_secs(10)).success(),
        "{}",
        engine.transcript()
    );
    let stdout = engine.stdout();
    let s = summary(&stdout);
    assert!(s["sent"] >= 5.0 && s["mined"] >= 5.0, "{s:?}");
    assert_eq!(
        (s["withdrawn"], s["rejected"], s["reverted"], s["beaten"]),
        (0.0, 0.0, 0.0, 0.0),
        "{s:?}"
    );
    assert!(engine.stderr().is_empty(), "{}", engine.transcript());

    // Every transaction the engine broadcast is in the vault's audit log.
    let audit = vault.stderr();
    let broadcast: Vec<_> = stdout
        .lines()
        .filter(|l| l.starts_with("SENT "))
        .map(parse_sent)
        .collect();
    assert_eq!(broadcast.len() as f64, s["sent"]);
    for tx in &broadcast {
        assert!(
            audit.contains(&format!(
                "audit: signed rebalance nonce {} gas 300000 tx {}",
                tx.nonce, tx.hash
            )),
            "{}",
            vault.transcript()
        );
    }

    assert!(
        vault.terminate(Duration::from_secs(10)).success(),
        "{}",
        vault.transcript()
    );
    assert!(
        vault.stdout().ends_with("threshold-signer: stopped"),
        "{}",
        vault.transcript()
    );
}

/// (3a) No vault: every rebalance is withdrawn, nothing is broadcast, and the loop keeps ticking.
#[test]
fn without_the_vault_nothing_is_broadcast() {
    let dir = TempDir::new("novault");
    let output = engine(&dir.socket(), &["--ticks", "200", "--seed", "7"])
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.contains("vault unavailable")
            && stderr.contains("every rebalance will be withdrawn"),
        "{stderr}"
    );
    let s = summary(&stdout);
    assert_eq!(
        (s["ticks"], s["sent"], s["rejected"]),
        (200.0, 0.0, 0.0),
        "{s:?}"
    );
    assert!(
        s["withdrawn"] >= 1.0,
        "the mock chain offered rebalances and each was withdrawn: {s:?}"
    );
    assert!(!stdout.contains("SENT "), "{stdout}");
    assert!(
        stdout.contains("WITHDRAWN nonce=0: vault unavailable"),
        "{stdout}"
    );
}

/// (3b) A vault whose policy refuses the engine's transactions (here a max fee cap below the engine's 30 gwei): every
/// rebalance is withdrawn and the refusal is in the vault's audit log.
#[test]
fn a_vault_that_refuses_the_transactions_gets_them_withdrawn() {
    let dir = TempDir::new("policy");
    let mut vault = spawn_vault(&dir, CHAIN_ID, EXECUTOR, &["--max-fee-gwei", "1"]);
    let output = engine(&dir.socket(), &["--ticks", "200", "--seed", "7"])
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let s = summary(&stdout);
    assert_eq!(
        (s["ticks"], s["sent"], s["rejected"]),
        (200.0, 0.0, 0.0),
        "{s:?}"
    );
    assert!(s["withdrawn"] >= 1.0, "{s:?}");
    assert!(stdout.contains("WITHDRAWN nonce=0: vault refused (policy): max fee per gas 30000000000 wei above the cap"), "{stdout}");
    assert!(vault.terminate(Duration::from_secs(10)).success());
    assert!(
        vault
            .stderr()
            .contains("audit: denied: max fee per gas 30000000000 wei above the cap"),
        "{}",
        vault.transcript()
    );
    assert!(
        !vault.stderr().contains("audit: signed"),
        "{}",
        vault.transcript()
    );
}

/// (4) The engine's Cargo.lock (which also lists dev-dependencies, so it is a superset of what the binary links) holds
/// no crate made to load, store, derive or generate a wallet key. k256 is there for signature recovery: it is an
/// elliptic-curve library, and the memory scan above is what shows that no key is ever loaded into it.
#[test]
fn no_crate_that_can_hold_a_wallet_key_is_in_the_engines_graph() {
    const KEY_HOLDERS: &[&str] = &[
        "alloy-signer-local",
        "alloy-signer-ledger",
        "alloy-signer-trezor",
        "alloy-signer-aws",
        "alloy-signer-gcp",
        "alloy-signer-turnkey",
        "eth-keystore",
        "coins-bip32",
        "coins-bip39",
        "ethers-signers",
    ];
    let packages = |path: PathBuf| -> BTreeSet<String> {
        let lock = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        lock.lines()
            .filter_map(|l| {
                l.strip_prefix("name = \"")?
                    .strip_suffix('"')
                    .map(String::from)
            })
            .collect()
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let engine = packages(root.join("Cargo.lock"));
    let vault = packages(root.join("../../vaults/threshold-signer/Cargo.lock"));
    // Positive controls: the parser reads both lockfiles, and sees the vault's local signer.
    assert!(engine.contains("alloy") && engine.contains("tokio") && engine.len() > 100);
    assert!(
        vault.contains("alloy-signer-local"),
        "the vault's lockfile should list its signer"
    );
    let linked: Vec<_> = KEY_HOLDERS
        .iter()
        .filter(|c| engine.contains(**c))
        .collect();
    assert!(
        linked.is_empty(),
        "key-holding crates in the engine's graph: {linked:?}"
    );
}
