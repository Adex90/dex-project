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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alloy::consensus::TxEnvelope;
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, TxKind, U256, address, hex, keccak256};

/// keccak256 of this phrase is the INSECURE test key (the engine's fixtures and the vault's own test use it too).
const TEST_KEY_PHRASE: &str = "strawberryswap insecure test key: end-to-end tests only";
/// The test key's address, computed independently with viem (`privateKeyToAccount`).
const VAULT_ADDRESS: Address = address!("0xf8e44d4517A0de6610113165659DaAd8cF9327B2");
const EXECUTOR: Address = address!("0x5FbDB2315678afecb367f032d93F642f64180aa3");
const CHAIN_ID: u64 = 31_337;
/// `rebalance(uint256,uint256,uint256,uint256,uint256)`.
const REBALANCE_SELECTOR: [u8; 4] = [0xc0, 0x66, 0x17, 0x37];

/// Both binaries are debug builds here, so the vault's deadline is generous: these tests are about who signs, the
/// unit tests about the deadline.
const VAULT_TIMEOUT_MS: &str = "2000";
const STARTUP: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------------------------------------------------

/// The vault binary, built once per test run with the test-key feature (debug only: the feature refuses to compile
/// in an optimized build).
fn vault_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        if let Some(path) = std::env::var_os("THRESHOLD_SIGNER_BIN") {
            return PathBuf::from(path);
        }
        let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vaults/threshold-signer");
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--locked",
                "--quiet",
                "--features",
                "insecure-test-key",
                "--manifest-path",
            ])
            .arg(vault.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(vault.join("target"))
            .status()
            .expect("cargo runs");
        assert!(
            status.success(),
            "building the vault with --features insecure-test-key failed"
        );
        vault.join("target/debug/threshold-signer")
    })
}

/// A child process whose output is collected line by line. Killed if the test ends without stopping it.
struct Proc {
    name: &'static str,
    child: Child,
    lines: Receiver<String>,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<Vec<String>>>,
    readers: Vec<JoinHandle<()>>,
}

impl Proc {
    fn spawn(name: &'static str, command: &mut Command) -> Self {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let (sender, lines) = mpsc::channel();
        let (stdout, stderr) = (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (out, all) = (child.stdout.take().unwrap(), Arc::clone(&stdout));
        let out = std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                all.lock().unwrap().push(line.clone());
                let _ = sender.send(line);
            }
        });
        let (err, all) = (child.stderr.take().unwrap(), Arc::clone(&stderr));
        let err = std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                all.lock().unwrap().push(line);
            }
        });
        Self {
            name,
            child,
            lines,
            stdout,
            stderr,
            readers: vec![out, err],
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stdout(&self) -> String {
        self.stdout.lock().unwrap().join("\n")
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }

    /// The next stdout line that satisfies `pred`, within `within`.
    fn wait_for(&self, within: Duration, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + within;
        loop {
            match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) if pred(&line) => return line,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    panic!("{}: timed out\n{}", self.name, self.transcript())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("{}: exited\n{}", self.name, self.transcript())
                }
            }
        }
    }

    /// SIGTERM, then the exit status and the end of the output, within `within`. (std has no signal API, and the
    /// tests forbid unsafe code, so the signal goes through kill(1).)
    fn terminate(&mut self, within: Duration) -> ExitStatus {
        let sent = Command::new("kill")
            .args(["-TERM", &self.pid().to_string()])
            .status()
            .expect("kill(1) runs");
        assert!(sent.success(), "{}: kill -TERM failed", self.name);
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "{}: still running after SIGTERM\n{}",
                self.name,
                self.transcript()
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // The pipes close with the process: the readers finish with the last lines.
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        status
    }

    fn transcript(&self) -> String {
        format!(
            "--- {} stdout ---\n{}\n--- {} stderr ---\n{}",
            self.name,
            self.stdout(),
            self.name,
            self.stderr()
        )
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A private temporary directory (0700, as the vault requires for its socket), removed at the end.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hft-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        Self(dir)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("vault.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts the vault with the test key on `dir`'s socket, and deletes the key file as soon as it listens: from then on
/// the key exists only in the vault's memory.
fn spawn_vault(dir: &TempDir, extra: &[&str]) -> Proc {
    let key_file = dir.0.join("test.key");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_file)
        .unwrap();
    writeln!(file, "{}", keccak256(TEST_KEY_PHRASE)).unwrap();
    drop(file);
    let vault = Proc::spawn(
        "vault",
        Command::new(vault_binary())
            .args([
                "--chain-id",
                &CHAIN_ID.to_string(),
                "--executor",
                &EXECUTOR.to_string(),
                "--socket",
            ])
            .arg(dir.socket())
            .arg("--insecure-test-key-file")
            .arg(&key_file)
            .args(extra),
    );
    let banner = vault.wait_for(STARTUP, |l| l.starts_with("threshold-signer:"));
    assert!(
        banner.contains("INSECURE TEST KEY") && banner.contains(&VAULT_ADDRESS.to_string()),
        "{banner}"
    );
    vault.wait_for(STARTUP, |l| l.starts_with("listening on"));
    fs::remove_file(&key_file).unwrap();
    vault
}

fn engine(socket: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hft-engine"));
    command
        .arg("--vault-socket")
        .arg(socket)
        .args(["--vault-timeout-ms", VAULT_TIMEOUT_MS])
        .args(args);
    command
}

/// The engine's closing line, `summary ticks=… sent=…`, as numbers.
fn summary(stdout: &str) -> BTreeMap<String, f64> {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("summary "))
        .unwrap_or_else(|| panic!("no summary in\n{stdout}"));
    line.split_whitespace()
        .skip(1)
        .filter_map(|field| field.split_once('='))
        .map(|(k, v)| (k.to_string(), v.parse().unwrap_or(f64::NAN)))
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------------------------------------------------

/// The key as it could sit in memory: its 32 bytes in either order (k256 keeps a scalar in little-endian limbs), and
/// its hex text in either case.
fn key_encodings() -> Vec<(&'static str, Vec<u8>)> {
    let key = keccak256(TEST_KEY_PHRASE);
    let mut reversed = key.to_vec();
    reversed.reverse();
    vec![
        ("big-endian bytes", key.to_vec()),
        ("little-endian bytes", reversed),
        ("lowercase hex", hex::encode(key).into_bytes()),
        ("uppercase hex", hex::encode_upper(key).into_bytes()),
    ]
}

#[derive(Debug)]
struct Scan {
    /// Bytes read, and bytes of readable mappings the kernel would not hand over.
    bytes: u64,
    unreadable: u64,
    mappings: usize,
    found: BTreeSet<&'static str>,
}

/// Reads every readable mapping of `pid` through /proc/<pid>/mem, in chunks that overlap by a needle's length, and
/// reports which needles occur anywhere.
fn scan_memory(pid: u32, needles: &[(&'static str, Vec<u8>)]) -> Scan {
    const CHUNK: usize = 8 << 20;
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let mem = File::open(format!("/proc/{pid}/mem")).unwrap_or_else(|e| {
        panic!(
            "/proc/{pid}/mem: {e} (the test must be the process's parent, or hold CAP_SYS_PTRACE)"
        )
    });
    let finders: Vec<_> = needles
        .iter()
        .map(|(name, n)| (*name, memchr::memmem::Finder::new(n)))
        .collect();
    let overlap = needles.iter().map(|(_, n)| n.len()).max().unwrap_or(1) - 1;
    let mut scan = Scan {
        bytes: 0,
        unreadable: 0,
        mappings: 0,
        found: BTreeSet::new(),
    };
    let mut buffer = vec![0u8; overlap + CHUNK];
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let (Some(range), Some(perms)) = (fields.next(), fields.next()) else {
            continue;
        };
        let name = fields.nth(3).unwrap_or("");
        // [vvar] and [vsyscall] are kernel pages that /proc/<pid>/mem does not serve.
        if !perms.starts_with('r') || name.starts_with("[vvar") || name == "[vsyscall]" {
            continue;
        }
        let (start, end) = range.split_once('-').unwrap();
        let (start, end) = (
            u64::from_str_radix(start, 16).unwrap(),
            u64::from_str_radix(end, 16).unwrap(),
        );
        scan.mappings += 1;
        let (mut at, mut carried) = (start, 0);
        while at < end {
            let want = usize::try_from(end - at).unwrap_or(usize::MAX).min(CHUNK);
            match mem.read_at(&mut buffer[carried..carried + want], at) {
                Ok(read) if read > 0 => {
                    let window = &buffer[..carried + read];
                    for (name, finder) in &finders {
                        if finder.find(window).is_some() {
                            scan.found.insert(name);
                        }
                    }
                    scan.bytes += read as u64;
                    at += read as u64;
                    carried = overlap.min(window.len());
                    let tail = window.len() - carried;
                    buffer.copy_within(tail..tail + carried, 0);
                }
                _ => {
                    scan.unreadable += end - at;
                    break;
                }
            }
        }
    }
    scan
}

// ---------------------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------------------

/// One broadcast transaction from the engine's `SENT nonce=… tx=… signer=… status=… raw=0x…` line.
struct Sent {
    nonce: u64,
    hash: String,
    status: String,
    raw: Vec<u8>,
}

fn parse_sent(line: &str) -> Sent {
    let field = |key: &str| {
        let start = line
            .find(&format!(" {key}="))
            .unwrap_or_else(|| panic!("no {key} in {line}"))
            + key.len()
            + 2;
        line[start..].split(' ').next().unwrap().to_string()
    };
    let status_start = line.find(" status=").unwrap() + " status=".len();
    let status_end = line.find(" raw=").unwrap_or(line.len());
    Sent {
        nonce: field("nonce").parse().unwrap(),
        hash: field("tx"),
        status: line[status_start..status_end].to_string(),
        raw: hex::decode(field("raw")).unwrap(),
    }
}

#[test]
fn the_engine_trades_through_the_vault_and_never_holds_its_key() {
    let dir = TempDir::new("trade");
    let mut vault = spawn_vault(&dir, &[]);
    let mut engine = Proc::spawn(
        "engine",
        &mut engine(&dir.socket(), &["--log-txs", "--seed", "7"]),
    );
    let vault_line = engine.wait_for(STARTUP, |l| l.starts_with("vault:"));
    assert!(
        vault_line.contains(&VAULT_ADDRESS.to_string()),
        "the engine reports the vault's signer: {vault_line}"
    );

    // 1. Five rebalances, each checked here without the engine's help.
    let mut sent = Vec::new();
    while sent.len() < 5 {
        // (A read can miss its budget on a loaded machine: a SKIPPED tick is not a failure, and is not waited for.)
        let line = engine.wait_for(Duration::from_secs(120), |l| {
            ["SENT ", "WITHDRAWN", "REJECTED"]
                .iter()
                .any(|event| l.starts_with(event))
        });
        assert!(
            line.starts_with("SENT "),
            "with the vault up nothing is withdrawn or rejected: {line}"
        );
        sent.push(parse_sent(&line));
    }
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
        assert_eq!(
            tx.status, "success",
            "nonce {}: the mock chain executed it from the operator",
            tx.nonce
        );
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
    assert!(s["sent"] >= 5.0, "{s:?}");
    assert_eq!(
        (s["withdrawn"], s["rejected"], s["reverted"]),
        (0.0, 0.0, 0.0),
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
    let mut vault = spawn_vault(&dir, &["--max-fee-gwei", "1"]);
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
