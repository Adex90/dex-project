//! The feed's deployment constants, for script/DeployTestnet.s.sol: QUERY_COMMITMENT = Poseidon(pack(symbol), salt)
//! from the relayer's salt file, and (with --provider-label) PROVIDER_ID = keccak256(label) mod p. With --new-salt it
//! first creates that salt file, mode 0600, from 31 bytes of the OS's randomness. The salt itself is never printed.

#![forbid(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;

use alloy::primitives::{B256, U256};
use zk_oracle::cli::{self, Flags};
use zk_oracle::commit::{det, query_commitment};

const USAGE: &str = "usage: query-commitment --salt-file PATH [--new-salt] [--symbol SYMBOL] [--provider-label LABEL]

Prints QUERY_COMMITMENT=0x… (Poseidon(pack(SYMBOL), salt), the instrument ZkPriceFeed pins) and, with
--provider-label, PROVIDER_ID=0x… (keccak256(LABEL) mod p, the notary's key in ZkDecoOracle), as KEY=VALUE lines for
the deployment's environment. The salt is never printed: it stays in its file, which only the relayer reads.

  --salt-file PATH        the query salt's file (the relayer's --salt-file)
  --new-salt              create PATH first (mode 0600; an existing file is never overwritten) with a random salt
  --symbol SYMBOL         default ETHUSD
  --provider-label LABEL  also print the provider id derived from LABEL";

/// A random non-zero salt: 31 bytes (248 bits) are always below the BN254 scalar field's modulus.
fn random_salt() -> Result<U256, String> {
    let mut bytes = [0u8; 31];
    File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| format!("/dev/urandom: {e}"))?;
    let salt = U256::from_be_slice(&bytes);
    if salt.is_zero() {
        return Err("the OS returned 31 zero bytes".into());
    }
    Ok(salt)
}

/// Creates the salt file, owner-only, refusing to replace one: a new salt is a new instrument commitment, so
/// overwriting the relayer's salt would silently break every proof against the deployed feed.
fn create_salt_file(path: &Path) -> Result<(), String> {
    let salt = random_salt()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e} (a salt file is never overwritten)", path.display()))?;
    file.write_all(format!("{salt:#x}\n").as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn run(flags: &Flags) -> Result<String, String> {
    let path = Path::new(flags.required("--salt-file", "the query salt's file")?);
    let symbol = cli::symbol(flags.get("--symbol").unwrap_or("ETHUSD"))?;
    if flags.switch("--new-salt") {
        create_salt_file(path)?;
    }
    let salt = cli::read_salt(path)?;
    let commitment = query_commitment(&symbol, salt).map_err(|e| e.to_string())?;
    let mut out = format!("QUERY_COMMITMENT={commitment}\n");
    if let Some(label) = flags.get("--provider-label") {
        let id = B256::from(det(label).to_be_bytes::<32>());
        out.push_str(&format!("PROVIDER_ID={id}\n"));
    }
    Ok(out)
}

fn main() -> ExitCode {
    let flags =
        match Flags::parse(std::env::args().skip(1), &["--salt-file", "--symbol", "--provider-label"], &["--new-salt"])
        {
            Ok(flags) => flags,
            Err(None) => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Err(Some(message)) => {
                eprintln!("error: {message}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
    match run(&flags) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn flags(list: &[&str]) -> Flags {
        Flags::parse(
            list.iter().map(|s| s.to_string()),
            &["--salt-file", "--symbol", "--provider-label"],
            &["--new-salt"],
        )
        .unwrap()
    }

    #[test]
    fn a_new_salt_is_private_never_printed_and_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("query-commitment-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("salt");
        let _ = std::fs::remove_file(&file);
        let path = file.to_str().unwrap();

        let out =
            run(&flags(&["--salt-file", path, "--new-salt", "--provider-label", "rothbard/ethusd/testnet"])).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let salt = cli::read_salt(&file).unwrap();
        let expected = query_commitment(b"ETHUSD", salt).unwrap();
        let provider = B256::from(det("rothbard/ethusd/testnet").to_be_bytes::<32>());
        assert_eq!(out, format!("QUERY_COMMITMENT={expected}\nPROVIDER_ID={provider}\n"));
        assert!(
            !out.contains(&format!("{salt:x}")) && !out.contains(&salt.to_string()),
            "the salt never leaves its file"
        );

        // Reading it back gives the same commitment; a second --new-salt is refused and leaves the salt as it was.
        assert_eq!(run(&flags(&["--salt-file", path])).unwrap(), format!("QUERY_COMMITMENT={expected}\n"));
        assert!(run(&flags(&["--salt-file", path, "--new-salt"])).unwrap_err().contains("never overwritten"));
        assert_eq!(cli::read_salt(&file).unwrap(), salt);
        // Another instrument, another commitment.
        let xau = run(&flags(&["--salt-file", path, "--symbol", "XAUUSD"])).unwrap();
        assert_ne!(xau, format!("QUERY_COMMITMENT={expected}\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn salts_are_random_field_elements() {
        let (a, b) = (random_salt().unwrap(), random_salt().unwrap());
        assert_ne!(a, b);
        assert!(a < U256::from(1u8) << 248 && b < U256::from(1u8) << 248);
    }
}
