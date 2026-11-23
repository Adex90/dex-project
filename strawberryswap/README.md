# StrawberrySwap

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
│   ├── hft-engine-rust/            # A-S execution engine: 100 ms loop, curve planner, vault-signed txs, live chain (iteration 2)
│   ├── searcher-rust/              # (reserved) production RPC / signer adapters
│   └── inventory-cpp/              # Searcher core: LOB signals, Avellaneda–Stoikov, curve planner (C++20)
├── vaults/threshold-signer/        # the testnet vault: one role per process, its key in AWS KMS, chain-state rules (iteration 1)
├── oracle/                         # price-proof pipeline: ETHUSD endpoint, KMS notary, ~15 s relayer → ZkPriceFeed
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
#   placeholders until launch: the footer links x.com, t.me and discord.gg /StrawberrySwap. Those handles are NOT
#   verified as ours: set NEXT_PUBLIC_SOCIAL_X / _TELEGRAM / _DISCORD=https://… to the official URLs before any public deployment

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

## Phase 7 deliverables

| # | Deliverable | Status |
|---|---|---|
| 22 | `frontend/src/zk/` — in-browser crypto core: note/intent model, Poseidon tree, JoinSplit witness builder, stateful wallet `ZkEngine` | 9 wallet tests + 5 cross-language golden tests (vs Go + circuits); real-proof test (3 Groth16 proofs verify against the vkey) |
| 23 | `prover.worker.ts` + typed `protocol.ts` / `client.ts` — Web Worker doing download, hash-verification, witness, proving; keys and notes never on the UI thread | static export builds clean; artifacts hash-pinned into the bundle |
| 24 | Hooks `useProver`, `useShieldedWallet`, `usePrivateSwap`; `PrivateSwapPanel` (§ 3); legacy plaintext intent path removed | typechecks strictly |
| 25 | `lib/automator.ts` — secure transport (URL policy, no credentials/referrer/redirects, batch-key check); Automator gains CORS | unit-tested; Go server tests |
| 26 | `e2e/run.mjs` — anvil + real contracts + static site in real Chromium (mock wallet) + the Go Automator, through to withdrawal | passes: browser proofs accepted on-chain; Automator settles; payout received |
| 27 | `docs/CLIENT_SIDE_PROVING.md` | — |

```bash
cd frontend && npm ci && npm run test:zk && npm run test:zk:proofs   # needs circuits/build (see circuits/README)
cd e2e && AUTOMATOR_BIN=… SOLC=… node run.mjs                         # full stack, real browser
```

## Phase 8 — adversarial audit and remediation

`docs/AUDIT_PHASE8.md` is an internal red-team review of the contracts, circuits and searcher (3 Critical, 7 High,
7 Medium findings, each reproduced by a working exploit). The fixes are merged:

| Area | What changed | Proven by |
|---|---|---|
| DarkPool | batches can never lower pool value at the oracle price (arbitrage must pay the Treasury); virtual reserves scale with shares; the Treasury cut is pulled per pool; batch proofs survive state changes for 64 blocks; zero entries rejected | `test/security/DarkPoolRegression.t.sol`, `RealProofRegression.t.sol` |
| NoteTree | one root per transaction, 1024 kept; a full tree rolls over (old roots stay valid) | same |
| SovereignBank / OptionsMarket | pro-rata exits instead of global freezes; pull-measured `TimeDeposits.settle` | `SovereignBank.t.sol`, `OptionsMarket.t.sol` (first suites for these modules) |
| Circuits | zk-DECO pinned to the canonical single-object layout (7,164 constraints); batch intent nullifier keyed by `destBlinding` | `circuits/test/*`, `npm run scan` (exact under-constraint scan: clean) |
| Go Automator, wallet | new nullifier, `Batch.(X, Y)`, oracle-aware batch selection, tree-rollover mirrors | `go test ./...`, `npm run test:zk`, `e2e/run.mjs` |
| C++ searcher | flat integer-tick book (3.6×), closed-form planner (~350×, and it fixes a profit-losing search bug), allocation-free calldata | `selftest` (1.67 M checks), `bench/` |

Test totals: Foundry 134, circuits 89 witness tests, Go all packages, wallet 17, C++ 1.67 M checks, full e2e.

### Mainnet blockers (not solved by this phase)
1. **Trusted setup.** Every verifier is built from single-party DEV keys; a public multi-party ceremony is required.
2. **No production price source yet.** `ZkPriceFeed` (Phase 1, `docs/ORACLE.md`) implements `IZKOracle`: it keeps the
   newest zk-DECO-attested price, with strict ordering and freshness rules. The pipeline that feeds it (Phase 2,
   `oracle/`) is built and tested end to end on anvil. Testnet runs it as a **trusted bootstrap**: we operate the
   endpoint and the notary, and this is disclosed. Mainnet needs independent providers and a decentralized notary (or
   in-circuit TLS). Batches and rebalances depend on the price being fresh (≤ 60 s).
3. **No external audit.** The Phase-8 review is internal; open items are listed in `docs/AUDIT_PHASE8.md` §7
   (operator sandwich residual, oracle-relative slippage, anonymity floor on-chain, deadline window, intent relaying,
   searcher RPC architecture, among others).

## Frontend — "Cyber-Strawberry" app shell

A mobile-first rebuild of the UI (presentation only: every hook, ABI, `lib/` and `zk/` file is byte-for-byte unchanged).

| Route | Screen |
|---|---|
| `/trade/` | Swap card (from/to token rows, flip, one primary action that names the next step, ending in **Shield & Swap (ZK)**) with the prover as a nested mini-card (real download progress, striped bar while proving, every step); shielded wallet + shield form; private orders (intents, notes, withdraw); live pool stats; market trackers |
| `/stocks/` | Market trackers and the synthetic-RWA explainer |
| `/earn/` | Strategy vault list card (NAV, high-water mark, share price, deposit / redeem / settle); Sovereign Bank products behind a Demand / Time segmented control |
| `/portfolio/` | Overview, Liquidity and History tabs over the shielded wallet and the vault position |

* **Shared state.** The `(app)` layout calls the existing hooks once (`components/AppState.tsx`), so an unlocked wallet and an
  in-flight proof survive switching tabs; the sticky bottom navigation (desktop: header pills) is client-side.
* **No invented numbers.** Prices come only from the chain. The vault's green/red pill is its share price against the 1.00
  start. SPY and NVDA show *feed pending* until a zk-DECO price feed exists; the bank shows its contract terms but no
  actions, because this build has no bank address.
* **Market chart** (`components/markets/`). TradingView-style candles or line, 1m to 1D, OHLC under the crosshair, built on
  [lightweight-charts](https://github.com/tradingview/lightweight-charts) (Apache-2.0; loaded on demand, attribution shown).
  History is the pool's own `spotPrice()` read at every block that emitted `Sync` in the last ~50,000 blocks (exact on
  anvil and archive RPCs; a node without historical state falls back to live points), then a live point per 12 s poll.
  The price only moves when reserves move, so a bucket without a change is a muted flat bar.
* **Wallet sheet** (`components/wallet/`). Browser wallet (named via EIP-6963, inline icons only), the native Shielded Wallet
  (connect, then unlock with one signature), hardware signers through the browser wallet, and WalletConnect shown as not in
  this build: it would route through a third-party relay. Focus is trapped, Esc closes, phones get a bottom sheet.
* **Language and currency** (`components/settings/`). English or Español and USD / EUR / MXN, remembered in
  `localStorage`. Navigation, dialogs and page headings are translated; trading screens follow later. Fiat totals use real
  rates only: USD through the pool's USD stablecoin (stated as such); EUR and MXN show no number until a rate feed exists.
* **Static and private.** Outfit and JetBrains Mono are bundled from npm (`@fontsource-variable/*`, OFL-1.1), the mascot and
  token icons are inline SVG: the export still talks only to the configured RPC and relay (`verify:static`, e2e).
* **Accessible.** Text meets WCAG AA on every surface (button fills use deeper brand shades for white labels), token pickers
  are native `<select>`s, tabs are ARIA tablists, and motion stops under `prefers-reduced-motion`. Skeletons are sized like
  the data they stand in for, so content fades in without layout shift.

## HFT execution engine (Rust), iteration 2: the live chain

`bots/hft-engine-rust/` runs the market-making loop end to end, every 100 ms:

1. **Read** the chain in one batched RPC request: the zk oracle (through the pool), the reserves, the executor's
   inventory, the block, the operator's nonces and gas balance. One `eth_call` of a never-deployed lens
   (`EngineLens`) returns the contract reads from a single block.
2. **Quote** with the Python lab's Avellaneda–Stoikov closed forms, using the `engine` block of `calibration.json`.
3. **Size** a DarkPool rebalance with the C++ engine's curve planner, ported bit for bit, and build the unsigned
   EIP-1559 `SearcherExecutor.rebalance` transaction.
4. **Simulate** it as an `eth_call` from the vault's address. One a competing searcher already made unprofitable is
   dropped before any signature.
5. **Sign** through the operator vault's Unix socket, within a deadline. Then check the signed transaction and
   broadcast it, with one transaction in flight at a time.

* **No key in the engine.** End-to-end tests recover every broadcast transaction to the vault's address. They scan
  the engine's memory for the signing key, on the mock chain (the vault's test key) and live on anvil (the operator's
  KMS key): it is never there.
* **Checked at start.** The vault is an operator vault, its address is the executor's operator, the engine's fees and
  gas are within its policy, and the chain and the pool's feed are the expected ones.
* **Structured output.** Events, alerts (`rpc_down`, `oracle_stale`, `tx_stuck`, …) and a metrics snapshot with
  latency histograms, as text or JSON lines.
* **The whole live path in CI.** On anvil, with the real contracts, the oracle pipeline pushing proven prices through
  the relayer vault, and the engine: when the market moves, the engine rebalances the pool through the operator
  vault.
* **Matches the references.**
  * Planner: bit-identical to the C++ on 311 golden cases.
  * A-S: within 1 ulp of the lab.
* **Safe and clean.** `forbid(unsafe_code)`; valgrind memcheck shows no errors.

```bash
cd bots/hft-engine-rust && cargo test     # unit tests, plus end to end with the vault binary
```

## The Arbitrum Sepolia testnet: testnet Phase 5

Deploying the whole stack on Arbitrum Sepolia, and soaking it. `docs/TESTNET.md` is the deployment, end to end;
`ops/soak/RUNBOOK.md` is the 72-hour soak.

* **Keys** (`infra/terraform/modules/signing-keys`). One secp256k1 key in KMS per role (operator, relayer,
  automator, notary; optionally deployer), each signable by one IAM role only, digests only. Administrators cannot sign.
  The bot host may assume exactly the signer roles; each signer runs as its own user, and only those users reach
  instance metadata. `terraform test` checks the rendered policies offline.
* **Deployment** (`contracts/script/DeployTestnet.s.sol`). Faucet-capped tWETH and tUSD, the clearly labelled dev-key
  verifiers, Poseidon, the notary registry, ZkPriceFeed, a minimal Treasury, DarkPool, SearcherExecutor bound to the
  operator's KMS address, and `TestnetDisclosure` on chain. It refuses any chain but 421614 and 31337, checks every
  wiring and writes the manifest; `CheckTestnet` re-checks it at any time. `script/verify-testnet.sh` source-verifies
  every contract.
* **Frontend.** Built from the manifest (`scripts/env-from-manifest.mjs`), with a trusted-bootstrap banner on every
  page that cannot be dismissed. Reproducible: the same commit gives the same IPFS CID. `ops/release/publish-ipfs.sh`
  publishes the site and the build artifacts (contract artifacts, manifest, the three dev proving keys) to IPFS.
* **Soak and drills** (`ops/`). Telemetry export in the calibration lab's formats and a comparison at γ = 0.0041; the
  service levels; an RPC failover proxy; and five drills (vault down, stale feed, RPC failover, chain reorg, competing
  searcher), each judged from the engine's own events.

```bash
python3 -m unittest discover -s ops/soak/tests -t ops/soak     # the soak harness, the proxy and the drills' scripts
cd infra/terraform/modules/signing-keys && terraform init -backend=false && terraform test
```

CI rehearses the release on anvil on every change: the deployment and CheckTestnet, the frontend built from that
manifest with its banner checked, and the IPFS CIDs.

## The vault (Rust), iteration 1: testnet Phase 4

`vaults/threshold-signer/` signs for the bots, so that no bot holds a key. **One role per process,** each with its own
key in **AWS KMS** (generated in its HSMs, never exported), its own socket and its own user. Each role signs **one
function on one contract** and refuses the others' by name:

| role | client | signs only |
|---|---|---|
| operator | HFT engine | `SearcherExecutor.rebalance` (never `sweep`) |
| relayer | oracle relayer | `ZkPriceFeed.submit`, for the feed's own provider and instrument |
| automator | batch automator | `DarkPool.settleBatch` |

* **The exchange.** A client sends an unsigned EIP-1559 transaction over a local Unix socket. The vault decodes it
  strictly, checks it, has KMS sign it, and returns the signed transaction. The socket sits in a private directory,
  and the kernel checks the caller's uid (`SO_PEERCRED`).
* **Checked against the chain at start.** The operator's key must be the executor's `operator()`, and no other role's
  key may be. The relayer reads its feed's configuration, and the automator its pool's batch size.
* **Nonce sequencing.** Only the chain's next nonce (or up to `--max-pending` beyond it) is signed, so no signature can
  be stockpiled. Every nonce the chain moves past must be the vault's own transaction (its receipt); anything else
  means the key signs elsewhere, and the vault stops until restarted.
* **Per-window caps** on signatures, fee budget, token volumes and notes.

The crate's README has the rules, the protocol, the IAM policy per key, and the path to t-of-n threshold ECDSA before
mainnet.

```bash
cd vaults/threshold-signer && cargo test     # 38 tests: policies, rules, KMS (local stand-in), socket
```

## Price-proof pipeline (Rust), testnet Phase 2

`oracle/` keeps `ZkPriceFeed` fresh. Three services, one crate (`docs/ORACLE.md` has the design and the trust model):

* **`price-endpoint`** serves `GET /v1/ETHUSD`: the circuit's canonical body of the median of Coinbase, Kraken and
  Bitstamp. It needs two quotes within 1 % of each other and no older than 10 s; otherwise it answers 503 (fail
  closed).
* **`notary`** fetches that body itself over HTTPS and checks it byte for byte. It then has **AWS KMS** sign
  `ZkDecoOracle`'s digest of its commitment, and checks that the signature recovers to it before handing it out.
* **`relayer`** runs every ~15 s. It checks the attestation as the contract will and proves it (Groth16, the committed
  DEV keys in `oracle/zk/`). It preflights `submit`, then pushes an EIP-1559 transaction, with capped fees, signed by
  the relayer vault (Phase 4): the relayer holds no key.

No process holds key material: the notary's key and the relayer's (behind its vault) are KMS keys, the interim custody
until the MPC vault. The tests run the production KMS signer against a stand-in that speaks KMS's protocol, and real
proofs throughout. CI's end-to-end test runs the three binaries and the relayer vault against `ZkPriceFeed` on anvil
while the market moves.

```bash
cd oracle && cargo test     # needs `npm ci` in circuits/ (snarkjs); the anvil end to end runs in CI (--ignored)
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
