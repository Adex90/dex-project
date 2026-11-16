//! Command-line plumbing shared by the three services: `--flag value` parsing, the salt file, the shutdown signal and
//! the testnet disclosure every service prints at start.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::str::FromStr;

use alloy::primitives::U256;

use crate::body::{SYMBOL_LEN, parse_symbol};
use crate::commit::modulus;

/// Printed by every service at start (decision 5: centralized trust, fully disclosed).
pub const TRUSTED_BOOTSTRAP: &str = "TESTNET TRUSTED BOOTSTRAP: Rothbard operates the price endpoint and the notary, \
so a price on the feed is only as honest as both. Not for mainnet value. See docs/ORACLE.md.";

/// `--flag value` pairs and bare `--switch`es, each at most once, from a fixed set.
#[derive(Debug, Default)]
pub struct Flags {
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}

impl Flags {
    /// Parses the arguments (without the program name). `Err(None)` asks for the usage text.
    pub fn parse(
        args: impl IntoIterator<Item = String>,
        valued: &[&str],
        switches: &[&str],
    ) -> Result<Self, Option<String>> {
        let mut flags = Self::default();
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            if flag == "-h" || flag == "--help" {
                return Err(None);
            }
            if switches.contains(&flag.as_str()) {
                if !flags.switches.insert(flag.clone()) {
                    return Err(Some(format!("{flag} given twice")));
                }
            } else if valued.contains(&flag.as_str()) {
                let value = it.next().ok_or_else(|| Some(format!("{flag} needs a value")))?;
                if flags.values.insert(flag.clone(), value).is_some() {
                    return Err(Some(format!("{flag} given twice")));
                }
            } else {
                return Err(Some(format!("unknown argument {flag}")));
            }
        }
        Ok(flags)
    }

    pub fn get(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }

    pub fn switch(&self, flag: &str) -> bool {
        self.switches.contains(flag)
    }

    /// A required flag's value.
    pub fn required(&self, flag: &str, what: &str) -> Result<&str, String> {
        self.get(flag).ok_or_else(|| format!("{flag} is required: {what}"))
    }

    /// A flag's parsed value, if given.
    pub fn parsed<T: FromStr>(&self, flag: &str) -> Result<Option<T>, String> {
        self.get(flag).map(|text| text.parse().map_err(|_| format!("{flag}: {text:?} is not valid"))).transpose()
    }

    /// A flag's parsed value, or the default.
    pub fn parsed_or<T: FromStr>(&self, flag: &str, default: T) -> Result<T, String> {
        Ok(self.parsed(flag)?.unwrap_or(default))
    }
}

/// An instrument symbol from the command line.
pub fn symbol(text: &str) -> Result<[u8; SYMBOL_LEN], String> {
    parse_symbol(text.as_bytes()).map_err(|_| format!("{text:?}: a symbol is 6 uppercase letters or digits"))
}

/// Reads the query salt: one non-zero field element, decimal or 0x-hex, from a file no one but its owner can read. The
/// salt hides the feed's instrument on chain, so it is never taken from the command line or the environment.
pub fn read_salt(path: &Path) -> Result<U256, String> {
    let shown = path.display();
    let mode = fs::metadata(path).map_err(|e| format!("{shown}: {e}"))?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!("{shown}: readable by group or others (mode {:o}); chmod 600 it", mode & 0o777));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    let salt: U256 = text.trim().parse().map_err(|_| format!("{shown}: not a decimal or 0x-hex number"))?;
    if salt.is_zero() || salt >= modulus() {
        return Err(format!("{shown}: the salt must be a non-zero BN254 field element"));
    }
    Ok(salt)
}

/// Resolves on SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_parse_values_switches_and_refuse_the_rest() {
        let flags = Flags::parse(args(&["--a", "1", "--s", "--b", "x"]), &["--a", "--b"], &["--s"]).unwrap();
        assert_eq!(flags.parsed_or::<u64>("--a", 7).unwrap(), 1);
        assert_eq!(flags.parsed_or::<u64>("--c", 7).unwrap(), 7);
        assert_eq!(flags.required("--b", "").unwrap(), "x");
        assert!(flags.switch("--s"));
        assert!(flags.parsed::<u64>("--b").is_err());
        assert!(flags.required("--c", "needed").unwrap_err().contains("needed"));

        assert_eq!(Flags::parse(args(&["--help"]), &[], &[]).unwrap_err(), None);
        for bad in [&["--a"][..], &["--z", "1"], &["--a", "1", "--a", "2"], &["--s", "--s"], &["a"]] {
            assert!(Flags::parse(args(bad), &["--a"], &["--s"]).unwrap_err().is_some(), "{bad:?}");
        }
    }

    #[test]
    fn symbols_follow_the_circuit_rule() {
        assert_eq!(&symbol("ETHUSD").unwrap(), b"ETHUSD");
        for bad in ["ethusd", "ETHUS", "ETHUSDT", "ETH/US"] {
            assert!(symbol(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_salt_file_must_be_private_and_a_field_element() {
        let dir = std::env::temp_dir().join(format!("zk-oracle-salt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("salt");
        let write = |text: &str, mode: u32| {
            fs::write(&file, text).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        };

        write("0x2a\n", 0o600);
        assert_eq!(read_salt(&file).unwrap(), U256::from(42));
        write("42", 0o400);
        assert_eq!(read_salt(&file).unwrap(), U256::from(42));
        write("42", 0o640);
        assert!(read_salt(&file).unwrap_err().contains("chmod 600"));
        write("0", 0o600);
        assert!(read_salt(&file).is_err());
        write(&modulus().to_string(), 0o600);
        assert!(read_salt(&file).is_err());
        write("salt", 0o600);
        assert!(read_salt(&file).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
