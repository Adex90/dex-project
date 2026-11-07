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
│   ├── test/                       # (Phase 2) invariant + fuzz suites, mock verifiers
│   └── script/                     # (Phase 2) deterministic deploy
├── circuits/                       # (Phase 2) Circom/Noir
│   ├── joinsplit/                  #   2-in/2-out note circuit (shield/transfer/unshield, assetId)
│   ├── batch_swap/                 #   aggregated intent settlement
│   └── zkdeco_oracle/              #   TLS-session attestation for price feeds
├── frontend/                       # Next.js + TypeScript + Tailwind, `output: "export"` (static → IPFS)
│   └── src/{app/{,trade},components,hooks,lib,styles}
├── bots/
│   ├── automator-go/               # intent mempool, batching, proving orchestration, relayer (Go)
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

## Known limitations (read before Phase 2)

1. **Privacy edges leak.** `shield`/`unshield`/`depositNote`/`withdrawNote` reveal their amount. Berrycoin ↔
   DarkPool currently crosses through a public hop; a unified shielded pool is the Phase 2 fix.
2. **Batch aggregates are public** (`in0,in1,out0,out1`). Small batches leak by subtraction; a minimum
   anonymity-set rule is needed.
3. **Trusted setup / circuit soundness** are assumed. The contracts bound the damage (supply cap, custody
   underflow) but a forged proof could still steal pooled funds.
4. **Oracle trust** is delegated to `IZKOracle`; its zk-DECO verification is not implemented here.
5. **LP shares are not an ERC-20/ERC-1155** yet and LP positions are public.
