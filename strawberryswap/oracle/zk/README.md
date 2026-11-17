# zkdeco_oracle proving artifacts (DEV KEYS)

The relayer's prover (`circuits/scripts/prove-zkdeco.mjs`) proves `circuits/zkdeco_oracle.circom` with these files:

| file | what |
| --- | --- |
| `zkdeco_oracle.wasm` | the witness generator compiled from the circuit |
| `zkdeco_oracle.zkey` | the Groth16 proving key |
| `zkdeco_oracle.vkey.json` | its verification key; the prover checks every proof against it before handing it out |
| `SHA256SUMS` | their hashes (`sha256sum -c SHA256SUMS`; CI checks them) |

**⚠ DEV-ONLY KEYS.** They come from the single-party local setup (`circuits/scripts/setup-dev.mjs`): whoever held that
setup's randomness could forge proofs. They match `contracts/test/fixtures/OracleGroth16Verifier.dev.sol`
(`tests/zk_artifacts.rs` checks every constant), which makes them fit for local tests, CI and the testnet's trusted
bootstrap (docs/ORACLE.md), where Rothbard operates the whole pipeline anyway. Mainnet needs keys from a multi-party
ceremony, and a verifier generated from them.

The setup is not reproducible (it draws fresh randomness), so these files are committed rather than rebuilt: a rebuilt
key would not match the committed verifier. To replace them, run the setup, then regenerate the verifier, the fixtures
(`circuits/scripts/gen-feed-fixtures.mjs`) and these files together.
