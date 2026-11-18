//! Helpers shared by the end-to-end tests: child processes with their output collected line by line, the vault
//! started with the INSECURE test key, the engine's summary line, and the memory scan that looks for the key.
#![allow(dead_code)] // each test binary uses a part

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

use alloy::primitives::{Address, address, hex, keccak256};

/// keccak256 of this phrase is the INSECURE test key (the engine's fixtures and the vault's own test use it too).
pub const TEST_KEY_PHRASE: &str = "strawberryswap insecure test key: end-to-end tests only";
/// The test key's address, computed independently with viem (`privateKeyToAccount`).
pub const VAULT_ADDRESS: Address = address!("0xf8e44d4517A0de6610113165659DaAd8cF9327B2");

/// Both binaries are debug builds here, so the vault's deadline is generous: these tests are about who signs, the
/// unit tests about the deadline.
pub const VAULT_TIMEOUT_MS: &str = "2000";
pub const STARTUP: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------------------------------------------------

/// The vault binary, built once per test run with the test-key feature (debug only: the feature refuses to compile
/// in an optimized build).
pub fn vault_binary() -> &'static Path {
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
pub struct Proc {
    name: &'static str,
    child: Child,
    lines: Receiver<String>,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<Vec<String>>>,
    readers: Vec<JoinHandle<()>>,
}

impl Proc {
    pub fn spawn(name: &'static str, command: &mut Command) -> Self {
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

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn stdout(&self) -> String {
        self.stdout.lock().unwrap().join("\n")
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }

    /// The next stdout line that satisfies `pred`, within `within`.
    pub fn wait_for(&self, within: Duration, pred: impl Fn(&str) -> bool) -> String {
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
    pub fn terminate(&mut self, within: Duration) -> ExitStatus {
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

    pub fn transcript(&self) -> String {
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
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hft-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        Self(dir)
    }

    pub fn socket(&self) -> PathBuf {
        self.0.join("vault.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts an operator vault with the test key on `dir`'s socket, and deletes the key file as soon as it listens: from
/// then on the key exists only in the vault's memory. No chain (the engine's chain is a mock here): the vault's nonce
/// rule follows its own signatures, and its caps are set far above what these tests sign (the vault's own tests cover
/// them).
pub fn spawn_vault(dir: &TempDir, chain_id: u64, executor: Address, extra: &[&str]) -> Proc {
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
                "--role",
                "operator",
                "--chain-id",
                &chain_id.to_string(),
                "--executor",
                &executor.to_string(),
                "--max-fee-eth-per-window",
                "1000",
                "--max-volume0-per-window",
                "1e40",
                "--max-volume1-per-window",
                "1e40",
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

pub fn engine(socket: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hft-engine"));
    command
        .arg("--vault-socket")
        .arg(socket)
        .args(["--vault-timeout-ms", VAULT_TIMEOUT_MS])
        .args(args);
    command
}

/// The engine's closing line, `summary ticks=… sent=…`, as numbers.
pub fn summary(stdout: &str) -> BTreeMap<String, f64> {
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
pub fn key_encodings() -> Vec<(&'static str, Vec<u8>)> {
    key_encodings_of(keccak256(TEST_KEY_PHRASE))
}

/// The four encodings of a 32-byte key the memory scans look for.
pub fn key_encodings_of(key: alloy::primitives::B256) -> Vec<(&'static str, Vec<u8>)> {
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
pub struct Scan {
    /// Bytes read, and bytes of readable mappings the kernel would not hand over.
    pub bytes: u64,
    pub unreadable: u64,
    pub mappings: usize,
    pub found: BTreeSet<&'static str>,
}

/// Reads every readable mapping of `pid` through /proc/<pid>/mem, in chunks that overlap by a needle's length, and
/// reports which needles occur anywhere.
pub fn scan_memory(pid: u32, needles: &[(&'static str, Vec<u8>)]) -> Scan {
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
