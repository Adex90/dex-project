# batch_swap (N = 4) proving artifacts (DEV KEYS)

The automator proves each batch of `circuits/batch_swap` (N = 4 slots, the testnet pool's `batchSize`) with these
files (`prove --wasm … --zkey …`; the deployed config points at `/opt/automator/keys/`):

| file | what |
| --- | --- |
| `main_n4.wasm` | the witness generator compiled from `circuits/batch_swap/main_n4.circom` |
| `batch_swap_n4.zkey` | the Groth16 proving key |
| `batch_swap_n4.vkey.json` | its verification key |
| `SHA256SUMS` | their hashes (`sha256sum -c SHA256SUMS`; `go test ./internal/prover` checks them) |

**⚠ DEV-ONLY KEYS.** They come from the single-party local setup (`circuits/scripts/setup-dev.mjs`): whoever held that
setup's randomness could forge batch proofs. They match `contracts/test/fixtures/BatchSwapGroth16Verifier.dev.sol`
(`internal/prover/zk_artifacts_test.go` checks every constant), the verifier `script/DeployTestnet.s.sol` deploys on
Arbitrum Sepolia under the trusted-bootstrap disclosure (`TestnetDisclosure`, the frontend's banner). They are fit for
CI and that testnet, never for value: mainnet needs keys from a multi-party ceremony and a verifier generated from them.

The setup is not reproducible (it draws fresh randomness), so these files are committed rather than rebuilt: a rebuilt
key would not match the committed verifier, and the testnet pool could no longer settle a batch. The join-split key
the browser proves with is committed the same way (`frontend/public/zk/`), and the oracle's in `oracle/zk/`. To
replace them, run the setup, then regenerate the verifiers, the fixtures and these files together.
