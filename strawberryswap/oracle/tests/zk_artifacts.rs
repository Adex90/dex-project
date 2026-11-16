//! The committed proving artifacts are the ones the on-chain verifier checks: every constant of
//! contracts/test/fixtures/OracleGroth16Verifier.dev.sol comes from zk/zkdeco_oracle.vkey.json. (CI also checks
//! zk/SHA256SUMS, and the prover checks each proof against this vkey, so the zkey must match it as well.)

mod common;

use std::collections::BTreeMap;

use serde_json::Value;

/// `uint256 constant NAME = DIGITS;` lines of the verifier.
fn verifier_constants() -> BTreeMap<String, String> {
    let source = common::read(&common::crate_dir().join("../contracts/test/fixtures/OracleGroth16Verifier.dev.sol"));
    source
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("uint256 constant ")?;
            let (name, value) = rest.split_once('=')?;
            Some((name.trim().to_string(), value.trim().trim_end_matches(';').trim().to_string()))
        })
        .collect()
}

#[test]
fn the_verifier_is_built_from_the_committed_verification_key() {
    let vkey: Value = serde_json::from_str(&common::read(&common::zk_dir().join("zkdeco_oracle.vkey.json"))).unwrap();
    assert_eq!(vkey["protocol"], "groth16");
    assert_eq!(vkey["curve"], "bn128");
    assert_eq!(vkey["nPublic"], 5, "providerId, queryCommitment, price, observedAt, bodyCommitment");

    let s = |v: &Value| v.as_str().unwrap().to_string();
    let mut expected: BTreeMap<String, String> = BTreeMap::new();
    expected.insert("alphax".into(), s(&vkey["vk_alpha_1"][0]));
    expected.insert("alphay".into(), s(&vkey["vk_alpha_1"][1]));
    // snarkjs's template writes each G2 coordinate as (c1, c0), the order of the pairing precompile.
    for (name, key) in [("beta", "vk_beta_2"), ("gamma", "vk_gamma_2"), ("delta", "vk_delta_2")] {
        expected.insert(format!("{name}x1"), s(&vkey[key][0][1]));
        expected.insert(format!("{name}x2"), s(&vkey[key][0][0]));
        expected.insert(format!("{name}y1"), s(&vkey[key][1][1]));
        expected.insert(format!("{name}y2"), s(&vkey[key][1][0]));
    }
    let ic = vkey["IC"].as_array().unwrap();
    assert_eq!(ic.len(), 6);
    for (i, point) in ic.iter().enumerate() {
        expected.insert(format!("IC{i}x"), s(&point[0]));
        expected.insert(format!("IC{i}y"), s(&point[1]));
    }

    let mut constants = verifier_constants();
    for field in ["r", "q"] {
        assert!(constants.remove(field).is_some(), "the verifier declares the field size {field}");
    }
    assert_eq!(constants.len(), 26);
    assert_eq!(constants, expected);
}

#[test]
fn the_checksums_cover_every_artifact() {
    let sums = common::read(&common::zk_dir().join("SHA256SUMS"));
    let files: Vec<&str> = sums.lines().map(|l| l.split_whitespace().nth(1).unwrap()).collect();
    assert_eq!(files, ["zkdeco_oracle.wasm", "zkdeco_oracle.zkey", "zkdeco_oracle.vkey.json"]);
    for line in sums.lines() {
        let hash = line.split_whitespace().next().unwrap();
        assert!(hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()), "{line}");
    }
}
