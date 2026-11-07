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
│   │   └── dex/DarkPool.sol        # MODULE 2  adjusted x·y=k, batch settle, flashRebalance
│   ├── test/                       # (Phase 2) invariant + fuzz suites, mock verifiers
│   └── script/                     # (Phase 2) deterministic deploy
├── circuits/                       # (Phase 2) Circom/Noir
│   ├── joinsplit/                  #   2-in/2-out note circuit (shield/transfer/unshield, assetId)
│   ├── batch_swap/                 #   aggregated intent settlement
│   └── zkdeco_oracle/              #   TLS-session attestation for price feeds
├── frontend/                       # Next.js + TypeScript + Tailwind, `output: "export"` (static → IPFS)
│   └── src/{app,components,lib,styles}
├── bots/
│   ├── automator-go/               # intent mempool, batching, proving orchestration, relayer (Go)
│   ├── searcher-rust/              # flashRebalance searcher: LVR detection, tx building (Rust)
│   └── inventory-cpp/              # low-latency inventory control / quoting core (C++20)
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
| No naked options | *(Phase 3)* | option vault will only mint against fully escrowed collateral |
| Hidden amounts/parties | `NoteTree`, `settleBatch` | join-split + batch proofs, stealth-addressed ciphertexts |

## Known limitations (read before Phase 2)

1. **Privacy edges leak.** `shield`/`unshield`/`depositNote`/`withdrawNote` reveal their amount. Berrycoin ↔
   DarkPool currently crosses through a public hop; a unified shielded pool is the Phase 2 fix.
2. **Batch aggregates are public** (`in0,in1,out0,out1`). Small batches leak by subtraction; a minimum
   anonymity-set rule is needed.
3. **Trusted setup / circuit soundness** are assumed. The contracts bound the damage (supply cap, custody
   underflow) but a forged proof could still steal pooled funds.
4. **Oracle trust** is delegated to `IZKOracle`; its zk-DECO verification is not implemented here.
5. **LP shares are not an ERC-20/ERC-1155** yet and LP positions are public.
