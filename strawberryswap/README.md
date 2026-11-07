# StrawberrySwap — Phase 1

DeFi SuperApp / institutional dark-pool broker. This directory is a self-contained monorepo that lives
beside the unrelated `Synthetic Capital` code at the repo root (nothing outside `strawberryswap/` was
touched).

## Phase 1 deliverables

| # | Deliverable | Status |
|---|---|---|
| 1 | Monorepo architecture (tree below) | scaffolded, empty dirs hold `.gitkeep` |
| 2 | `contracts/src/tokens/Berrycoin.sol` — ZK-ERC20 | written, compiles (solc 0.8.26, via-IR) |
| 3 | `contracts/src/dex/DarkPool.sol` — AMM + `flashRebalance()` | written, compiles |
| – | Shared: `privacy/NoteTree.sol`, `utils/*`, `interfaces/*` | written, compiles |

## Phase 2 deliverables

| # | Deliverable | Status |
|---|---|---|
| 4 | `contracts/src/derivatives/OptionsMarket.sol` — ZK-ERC1155 European calls/puts, 100 % collateral | written, compiles |
| 5 | `contracts/src/derivatives/SyntheticRWA.sol` — fully-funded long/short index pairs + `IZKDecoOracle` | written, compiles |

**Not done in Phase 1 (deliberately):** circuits and verifier keys, Poseidon contract deployment, tests,
frontend, bots, Terraform. **Nothing here is audited. Do not deploy.**

## Monorepo tree

```
strawberryswap/
├── README.md
├── .gitignore
├── contracts/                      # Foundry project (Solidity 0.8.26, evm=paris, via-IR)
│   ├── foundry.toml
│   ├── src/
│   │   ├── interfaces/             # IZKVerifier, IPoseidonHasher, IZKOracle, ITreasury,
│   │   │                           #   IFlashRebalancer, IERC20Min
│   │   ├── privacy/NoteTree.sol    # incremental Poseidon Merkle tree + nullifiers + join-split gate
│   │   ├── utils/                  # ReentrancyGuard, SafeTransfer, Math
│   │   ├── tokens/Berrycoin.sol    # MODULE 1  ZK-ERC20, fixed genesis supply, no mint
│   │   ├── dex/DarkPool.sol        # MODULE 2  adjusted x·y=k, batch settle, flashRebalance
│   │   ├── banking/SovereignBank.sol   # MODULE 5  DemandDeposits | TimeDeposits | facade
│   │   ├── funds/StrategyVault.sol     # MODULE 9  hedge-fund vault, HWM fee
│   │   └── derivatives/
│   │       ├── OptionsMarket.sol   # MODULE 3  ZK-ERC1155 options, total coverage, European exercise
│   │       └── SyntheticRWA.sol    # MODULE 4  capped-index long/short pairs + IZKDecoOracle
│   ├── test/                       # unit / fuzz / invariant / real-proof integration suites + mocks + fixtures
│   └── script/                     # (Phase 2) deterministic deploy
├── circuits/                       # Circom 2.1.9 (+ test harness, dev setup, fixture generator)
│   ├── joinsplit/                  #   2-in/2-out note circuit (shield/transfer/unshield, assetId)
│   ├── batch_swap/                 #   aggregated shielded intent settlement (N = 2/4/8/16)
│   └── zkdeco_oracle/              #   TLS-session attestation for price feeds
├── frontend/                       # Next.js + TypeScript + Tailwind, `output: "export"` (static → IPFS)
│   └── src/{app/{,trade},components,hooks,lib,styles}
├── bots/
│   ├── automator/                  # Go relayer: intents → batch → proof → settleBatch; Tor-only metadata layer
│   ├── searcher-rust/              # (reserved) production RPC / signer adapters
│   └── inventory-cpp/              # Searcher core: LOB signals, Avellaneda–Stoikov, curve planner (C++20)
├── mpc/tss-vault/                  # (Phase 4) threshold-ECDSA/EdDSA corporate vault coordinator
├── infra/
│   ├── terraform/{modules/{network,ec2-bots,rds,s3-ipfs-pin},envs/prod}
│   └── scripts/                    # PowerShell + AWS CLI helpers
├── .github/workflows/              # CI: forge test, slither, circuit checks, IPFS publish
└── docs/
```

## How the axioms map to code

| Axiom | Where | Mechanism |
|---|---|---|
| Fixed supply, no mint | `Berrycoin` | `MAX_SUPPLY` constant credited only in the constructor; no mint/burn/owner/proxy |
| Supply cannot be inflated even by a broken circuit | `Berrycoin.unshield` | `amount <= shieldedSupply` checked independently of the proof |
| 100% backing | `DarkPool._checkBalances` | token balance ≥ reserve + note custody after every entrypoint |
| 0% retail fee | `DarkPool.RETAIL_FEE_BPS = 0` | curve rounds in the pool's favour so K never decreases |
| Gas subsidy via MEV | `DarkPool.flashRebalance` | searcher must pay ≥ 80% of harvested value to the Treasury in-tx |
| No naked options | `OptionsMarket.mintOptionToken` | only mint path; pulls 100 % collateral, verified by balance delta; solvency check `balance >= liabilities` |
| Demand money can never be lent | `DemandDeposits` | separate contract; only external calls are the custodied ERC-20's; no owner/approve/adapter hook (opcode-scanned: no DELEGATECALL/CALLCODE/SELFDESTRUCT/CREATE) |
| Only time deposits earn yield | `TimeDeposits` | no early-withdrawal function; routing is per-deposit, exact amount, no standing allowance |
| Manager cannot withdraw | `StrategyVault` | only `executeTrade` (immutable adapter allowlist, trade-size cap, per-trade NAV-loss cap); manager paid only in fee shares |
| Fee only on new profit | `StrategyVault._crystallise` | per-share high-water mark; fee 0 at or below the mark |
| Synthetics stay 100 % backed | `SyntheticRWA` | capped payoff: long + short payouts sum to exactly the locked collateral |
| Hidden amounts/parties | `NoteTree`, `settleBatch` | join-split + batch proofs, stealth-addressed ciphertexts |

## Phase 3 deliverables

| # | Deliverable | Status |
|---|---|---|
| 6 | `contracts/src/banking/SovereignBank.sol` — `DemandDeposits` (pure custody) + `TimeDeposits` (fixed term, optional routing) + registry facade | written, compiles |
| 7 | `contracts/src/funds/StrategyVault.sol` — non-custodial fund, `executeTrade()` only, per-share high-water-mark performance fee | written, compiles |

## Phase 4 deliverables

| # | Deliverable | Status |
|---|---|---|
| 8 | `frontend/` — Next.js 15 + TS + Tailwind, `/trade` dashboard, hooks for `DarkPool` and `StrategyVault`, **static export** | `next build` + `tsc` pass; export checked for third-party origins |
| 9 | `bots/inventory-cpp/` — C++20 Searcher: LOB signals, Avellaneda–Stoikov, curve planner, `flashRebalance` caller | builds with `-Werror`; self-test: 178k checks pass |
| 10 | `contracts/src/periphery/SearcherExecutor.sol` — on-chain callback the bot drives | compiles |
| 11 | `infra/terraform/` — VPC, EC2 (t3.micro), RDS Postgres (db.t3.micro), S3 metadata bucket, budget alarm | `terraform validate` passes; **never applied** |

### Run it
```bash
# contracts (compile check used in this repo: solc 0.8.26, evm paris, via-IR)
cd contracts && forge build

# frontend: static export for IPFS (addresses are inlined at build time)
cd frontend && npm ci && NEXT_PUBLIC_DARKPOOL_ADDRESS=0x… NEXT_PUBLIC_VAULT_ADDRESS=0x… npm run build && npm run verify:static
#   ./out is the directory to `ipfs add -r`; serve from an origin root (ipfs://, <cid>.ipfs.<gw>, ENS/DNSLink)

# searcher: unit/property tests + dry run (no network, no keys)
cd bots/inventory-cpp && cmake -S . -B build && cmake --build build && ./build/selftest && ./build/searcher_dryrun

# infra
cd infra/terraform/envs/prod && cp terraform.tfvars.example terraform.tfvars && terraform init && terraform plan
```

## Phase 5 deliverables

| # | Deliverable | Status |
|---|---|---|
| 12 | `circuits/joinsplit` — Circom shielded-pool circuit (12,596 constraints) | 20 adversarial witness tests; **real Groth16 proofs verified on-chain** |
| 13 | `circuits/zkdeco_oracle` — oracle circuit **skeleton** (21,637 constraints) | 22 witness tests; real proof settles a `SyntheticRWA` series. **Trust boundary: notary, not TLS** |
| 14 | `contracts/src/verifiers/*`, `src/oracle/ZkDecoOracle.sol` — Groth16 + Poseidon adapters, notary-checked oracle | gas-capped verifier calls |
| 15 | `contracts/test/` — 77 Foundry tests: unit, fuzz, **stateful invariants**, real-proof integration | all pass; 5/5 mutations caught |
| 16 | `docs/INVARIANTS.md` — algebraic proofs of I-SUPPLY and I-K + findings | — |

**Not delivered:** the `batch_swap` circuit (DarkPool's batch proofs are only mocked), an MPC ceremony (keys are
single-party DEV keys), in-circuit TLS, an external audit. See `circuits/README.md` and `docs/INVARIANTS.md`.

```bash
# contracts: needs forge-std once, then everything runs offline
cd contracts && forge install foundry-rs/forge-std --no-git && forge test
# circuits (circom >= 2.1.9 on PATH or $CIRCOM)
cd circuits && npm ci && npm run compile && node test/joinsplit.test.mjs && node test/zkdeco.test.mjs
```

## Phase 6 deliverables

| # | Deliverable | Status |
|---|---|---|
| 17 | `circuits/batch_swap` — N shielded intents → one net transition at a uniform curve price | N=4 proven end-to-end; 38 witness tests; 9 circuit mutations |
| 18 | `DarkPool.settleBatch` rewired to the circuit (public-input layout, clock window, net flows, `batchSize`) + `GenericGroth16Adapter` | 22 pool tests + 11 real-proof E2E tests |
| 19 | `bots/automator/` — Go relayer: tree mirror, batcher, prover wrapper, chain client, HTTP intake, epoch loop | `go test -race` green; golden-checked against the JS reference |
| 20 | Metadata-privacy layer: onion-only intake, redacting HTTP/log layers, Tor egress, hardened systemd unit, `torrc` | tested properties G1–G5 (`internal/privacy`) |
| 21 | `docs/BATCH_SETTLEMENT.md` — design, K proof, threat table, measured costs | — |

**Open:** the frontend still posts a legacy plaintext intent (client-side proving not wired); `settleBatch` costs ≈3.05M
gas at N=4, dominated by on-chain Merkle insertions (SNARK verification is 297k); keys are single-party DEV keys.

## Known limitations (read before Phase 2)

1. **Privacy edges leak.** `shield`/`unshield`/`depositNote`/`withdrawNote` reveal their amount. Berrycoin ↔
   DarkPool currently crosses through a public hop; a unified shielded pool is the Phase 2 fix.
2. **Batch aggregates are public** (`in0,in1,out0,out1`). Small batches leak by subtraction; a minimum
   anonymity-set rule is needed.
3. **Trusted setup / circuit soundness** are assumed. The contracts bound the damage (supply cap, custody
   underflow) but a forged proof could still steal pooled funds.
4. **Oracle trust** is delegated to `IZKOracle`; its zk-DECO verification is not implemented here.
5. **LP shares are not an ERC-20/ERC-1155** yet and LP positions are public.
