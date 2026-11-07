# circuits — StrawberrySwap zero-knowledge circuits (Circom 2.1.9)

| Circuit | Constraints | Public inputs | Status |
|---|---|---|---|
| `joinsplit/joinsplit.circom` | 12,596 | 9 | complete; 20 adversarial witness tests; real proofs verified on-chain |
| `zkdeco_oracle/zkdeco_oracle.circom` | 21,637 | 5 | **skeleton** (see trust boundary); 22 witness tests; real proof verified on-chain |
| `batch_swap/` | — | — | **not written.** `DarkPool.settleBatch` is only tested against a mock batch verifier |

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

## Commands
```bash
npm ci
CIRCOM=/path/to/circom npm run compile          # needs circom >= 2.1.9
node test/joinsplit.test.mjs && node test/zkdeco.test.mjs
PTAU_POWER=15 node scripts/setup-dev.mjs joinsplit zkdeco_oracle   # DEV ONLY keys
node scripts/gen-fixtures.mjs                    # writes contracts/test/fixtures/*
```

## ⚠ Trusted setup
`setup-dev.mjs` is a **single-party local setup**: whoever ran it holds the toxic waste and can forge
proofs. The verifiers in `contracts/test/fixtures/` are generated from those keys and are test fixtures only.
Production requires a public multi-party phase-2 ceremony on a reputable phase-1 ptau, and a fresh verifier.

## Not yet done (honest list)
Independent circuit audit; an under-constrained-signal analyser run (e.g. circomspect / Picus); formal
verification; the `batch_swap` circuit; in-circuit TLS; a ceremony.
