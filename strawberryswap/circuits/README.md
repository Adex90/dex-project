# circuits — StrawberrySwap zero-knowledge circuits (Circom 2.1.9)

| Circuit | Constraints | Public inputs | Status |
|---|---|---|---|
| `joinsplit/joinsplit.circom` | 12,596 | 9 | complete; 20 adversarial witness tests; real proofs verified on-chain |
| `zkdeco_oracle/zkdeco_oracle.circom` | 21,637 | 5 | **skeleton** (see trust boundary); 22 witness tests; real proof verified on-chain |
| `batch_swap/` | N=2: 14,982 · N=4: 28,736 · N=8: 56,244 · N=16: 111,260 | 8 + 2N | complete; 38 adversarial witness tests; **real proof settled on-chain end-to-end**; 9 circuit mutations (see below) |

## JoinSplit statement (one circuit for shield / transferZK / unshield, any `assetId`)
Public: `[root, nullifier0, nullifier1, commitment0, commitment1, publicIn, publicOut, extDataHash, assetId]`
(the order `NoteTree._applyJoinSplit` passes; asserted by `scripts/gen-fixtures.mjs`).
Proves ownership, Merkle membership (skipped for zero-value dummy inputs), correct nullifiers
(`n0 != n1`), 128-bit-range-checked value conservation `Σin + publicIn = Σout + publicOut`, well-formed
outputs of the same `assetId`, and constrains `extDataHash`. Hidden: keys, values, blindings, recipients,
and the Merkle path (so which leaf is spent). Note model and tree layout match `NoteTree.sol` exactly.

## zk-DECO oracle — trust boundary (read before relying on it)
The circuit does **not** verify TLS. It proves what follows from a committed response body: the hidden query
(`queryCommitment`), a price field, a timestamp field. Authenticity of the TLS plaintext rests on a registered
**notary** signature (`ZkDecoOracle.sol`) until in-circuit TLS verification exists. Also a skeleton in the
parser: fixed JSON layout and key names, prices must contain a decimal point, a body that echoes
attacker-controlled text could carry a second `"price":`. Details in the circuit header.

## batch_swap
Aggregates N shielded swap intents into one net state transition at a single closed-form clearing price (header of
`batch_swap/batch_swap.circom`; full design and security argument in `docs/BATCH_SETTLEMENT.md`). Public inputs:
`[root, tNow, X, Y, netIn0, netIn1, netOut0, netOut1, nullifier[N], commitment[N]]`. `main_n{2,4,8,16}.circom` are the
instantiations; N must equal `DarkPool.batchSize` and the verifier key.
`scripts/mutate-batch.sh` mutates the circuit one line at a time (drop `minOut`, deadline, net-flow, nullifier,
membership, remainder bounds, …) and requires the adversarial suite to fail for each; the first run found two
survivors (remainder bounds) which led to the *consistent-remainder* attack tests now in `test/batch.test.mjs`.

## Commands
```bash
npm ci
CIRCOM=/path/to/circom npm run compile          # needs circom >= 2.1.9
node test/joinsplit.test.mjs && node test/zkdeco.test.mjs && node test/batch.test.mjs   # batch: compile main_n4 with --wasm first
PTAU_POWER=15 node scripts/setup-dev.mjs joinsplit zkdeco_oracle   # DEV ONLY keys
node scripts/gen-fixtures.mjs                    # JoinSplit + oracle fixtures
node scripts/gen-golden.mjs                      # golden vectors for the Go Automator
AUTOMATOR_BIN=… node scripts/gen-batch-fixtures.mjs   # end-to-end batch: JS users + Go Automator + real proofs
```

## ⚠ Trusted setup
`setup-dev.mjs` is a **single-party local setup**: whoever ran it holds the toxic waste and can forge
proofs. The verifiers in `contracts/test/fixtures/` are generated from those keys and are test fixtures only.
Production requires a public multi-party phase-2 ceremony on a reputable phase-1 ptau, and a fresh verifier.

## Not yet done (honest list)
Independent circuit audit; an under-constrained-signal analyser run (e.g. circomspect / Picus); formal
verification; in-circuit TLS; a ceremony; batch proofs for N=8/16 (compiled, not proven — they need a 2^17/2^18 ptau).
