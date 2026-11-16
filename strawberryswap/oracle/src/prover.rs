//! Groth16 proving of `circuits/zkdeco_oracle`, by `circuits/scripts/prove-zkdeco.mjs` (snarkjs) in a child process.
//!
//! The witness holds the query salt, so it goes to the child on stdin only, never to disk or the command line. The
//! child gets an empty environment (PATH aside), a hard timeout, and is killed if the relayer drops the call. Its
//! output is checked here: exactly the expected five public signals, and a 256-byte proof.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use alloy::primitives::{U256, hex};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::notary::SNARK_LEN;

#[derive(Clone, Debug)]
pub struct Prover {
    /// The node executable.
    pub node: PathBuf,
    /// circuits/scripts/prove-zkdeco.mjs (it resolves snarkjs from circuits/node_modules).
    pub script: PathBuf,
    pub wasm: PathBuf,
    pub zkey: PathBuf,
    pub vkey: PathBuf,
    pub timeout: Duration,
}

#[derive(Debug)]
pub enum ProverError {
    Spawn(String),
    Timeout,
    Failed(String),
    Output(String),
    /// The proof is for other public inputs than the attestation's.
    PublicSignals,
}

impl fmt::Display for ProverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "prover: cannot start: {m}"),
            Self::Timeout => f.write_str("prover: timed out"),
            Self::Failed(m) => write!(f, "prover: failed: {m}"),
            Self::Output(m) => write!(f, "prover: unexpected output: {m}"),
            Self::PublicSignals => f.write_str("prover: the public signals are not the attestation's"),
        }
    }
}

#[derive(Deserialize)]
struct Output {
    snark: String,
    #[serde(rename = "publicSignals")]
    public_signals: Vec<String>,
}

impl Prover {
    /// The artifacts of a directory holding zkdeco_oracle.{wasm,zkey,vkey.json} (strawberryswap/oracle/zk).
    pub fn new(node: PathBuf, script: PathBuf, zk_dir: &Path, timeout: Duration) -> Self {
        Self {
            node,
            script,
            wasm: zk_dir.join("zkdeco_oracle.wasm"),
            zkey: zk_dir.join("zkdeco_oracle.zkey"),
            vkey: zk_dir.join("zkdeco_oracle.vkey.json"),
            timeout,
        }
    }

    /// A Groth16 proof of `witness`, whose public signals must be `expected`: providerId, queryCommitment, price,
    /// observedAt, bodyCommitment.
    pub async fn prove(&self, witness: &Value, expected: &[U256; 5]) -> Result<[u8; SNARK_LEN], ProverError> {
        let mut child = Command::new(&self.node)
            .arg(&self.script)
            .arg(&self.wasm)
            .arg(&self.zkey)
            .arg(&self.vkey)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ProverError::Spawn(format!("{}: {e}", self.node.display())))?;
        let mut stdin = child.stdin.take().ok_or_else(|| ProverError::Spawn("no stdin".into()))?;
        let input = witness.to_string();
        let run = async move {
            stdin.write_all(input.as_bytes()).await.map_err(|e| ProverError::Spawn(e.to_string()))?;
            drop(stdin); // end of input
            child.wait_with_output().await.map_err(|e| ProverError::Spawn(e.to_string()))
        };
        let output = tokio::time::timeout(self.timeout, run).await.map_err(|_| ProverError::Timeout)??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            return Err(ProverError::Failed(format!("{}: {last}", output.status)));
        }
        let parsed: Output = serde_json::from_slice(&output.stdout).map_err(|e| ProverError::Output(format!("{e}")))?;
        let signals: Vec<U256> = parsed
            .public_signals
            .iter()
            .map(|s| s.parse::<U256>().map_err(|_| ProverError::Output(format!("signal {s:?}"))))
            .collect::<Result<_, _>>()?;
        if signals.as_slice() != expected.as_slice() {
            return Err(ProverError::PublicSignals);
        }
        let digits = parsed.snark.strip_prefix("0x").ok_or_else(|| ProverError::Output("snark not 0x-hex".into()))?;
        let bytes = hex::decode(digits).map_err(|e| ProverError::Output(e.to_string()))?;
        bytes.try_into().map_err(|b: Vec<u8>| ProverError::Output(format!("{} proof bytes, expected 256", b.len())))
    }
}
