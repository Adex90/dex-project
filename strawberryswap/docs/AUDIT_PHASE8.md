# Phase 8 — Institutional Security Audit, Cryptographic Red Team and HFT Review

**Audited commit:** `a0a7eea` (Phase 7) · **Date:** 2026-11-07 · **Product code changed by this audit:** none

This is an adversarial review. Every Critical and High finding is backed by something that runs. A suspected break of
that severity counts only if I could reproduce it. Findings marked *analysis* are argued from the code instead:

* **12 exploit tests and 1 gas benchmark** in Foundry (`contracts/audit/`). All 13 **pass on the audited code**, which
  means the attacks work. 3 of them use real Groth16 proofs and real Poseidon.
* **3 circuit scripts** (`circuits/audit/`). They produce real forged zk-DECO proofs, de-anonymise batch slots from
  public data, and run an exact under-constraint scan.
* **Exact-integer economic models** (`contracts/audit/models/`).
* **C++ micro-benchmarks** (`bots/inventory-cpp/audit/bench.cpp`).

Every patch proposed here is in `docs/AUDIT_PHASE8.patch` (812 lines). It applies cleanly to this commit. On a fresh copy,
after applying it:

* **all 92 existing tests still pass**;
* **all 12 exploits fail**, each on the guard the patch adds;
* the patched circuits still satisfy their own test suites.

The patch is a reference fix. It is **not** applied here.

---

## 0. Verdict

The axioms rank as follows, from strongest to weakest:

1. **100 % reserve of user custody holds.** No path in DarkPool, NoteTree, OptionsMarket or SovereignBank mints unbacked
   notes, options or deposits, or releases more custody than was deposited, **provided proofs are sound**. Two
   qualifications:
   * Proofs are only as sound as a **single-party dev trusted setup** (§C-0).
   * The *liquidity providers'* money is a different story (C-2).
2. **Zero-fee sustainability is broken outright.** `settleBatch` is a permissionless, fee-free, **oracle-free** swap
   path. Any arbitrageur trades the pool back to the market through it and keeps 100 % of the profit. `flashRebalance`'s
   80 % Treasury cut is therefore optional, and rational searchers will never pay it (C-1). The "MEV internalisation"
   that is supposed to fund the subsidy collects **zero**.
3. **LPs can be drained in one transaction.** The pool's virtual reserves are immutable constants. As a result, a
   *proportional* liquidity deposit moves the price. Flash-borrow, deposit, arbitrage for free, withdraw: **55.9 % of
   an honest LP's position** is taken in one transaction (C-2).
4. **Absolute privacy is not achieved.** Intent nullifiers are computable by anyone, so every batch slot links to the
   wallet that created it: 4 of 4 slots de-anonymised from public data (H-7). The relay operator can sandwich users up
   to their whole slippage tolerance (H-4). That disproves the documentation's claim that "a malicious aggregator can
   at worst censor".
5. **Liveness is cheap to destroy:**
   * 64 junk transactions void every proof in flight (H-2).
   * A 1-wei reserve change voids every batch proof, including refunds (H-5).
   * Filling the 2²⁰-leaf tree freezes every shielded note **forever** (H-3).
6. **The zk-DECO oracle proves the wrong number.** Using real Groth16 proofs, I settled gold at the silver price
   (C-3).
7. **The HFT engine optimises the wrong thing.** Its quoting maths runs in microseconds, but every decision waits on
   four synchronous RPC round-trips. Its planner also has a bug that leaves up to **23 %** of the profit behind (Q-3).
   And while C-1 exists, competitors arbitrage the pool for 100 % of the profit against our 20 % (Q-1).

### Scoreboard

Columns: **Exploit** = a PoC demonstrates the attack on `a0a7eea`. **Fixed** = with the patch, the exploit fails and the
existing suite still passes.

| ID | Sev | Component | Finding | Exploit | Fixed |
|---|---|---|---|---|---|
| C-0 | Critical (acknowledged) | all SNARKs | Single-party dev Groth16 keys: whoever ran the setup can forge every proof and drain all custody | — (documented) | ceremony |
| C-1 | **Critical** | DarkPool | `settleBatch` is a fee-free, oracle-free arbitrage path, so the Treasury cut is optional | ✅ mock + ✅ real proofs | ✅ P-1 |
| C-2 | **Critical** | DarkPool | Fixed virtual reserves: flash-liquidity add, free arbitrage and remove drain honest LPs (−55.9 %) | ✅ | ✅ P-3 (and P-1) |
| C-3 | **Critical** | zk-DECO circuit | `price` and `ts` are not bound to the object that carries `symbol`: real proofs settle XAUUSD at 29.10 | ✅ real proofs | ✅ ZD-1 |
| H-1 | High | DarkPool | The Treasury payment is a shared balance delta, so nested rebalances of N pools pay once | ✅ | ✅ P-4 |
| H-2 | High | NoteTree | Root history counts leaves: 64 junk `transferNote`s (no funds needed) evict every root | ✅ | ✅ P-6 |
| H-3 | High | NoteTree | 2²⁰-leaf tree with no rollover; once full, every exit reverts forever (8.5·10¹¹ gas) | ✅ | ✅ P-5 |
| H-4 | High | DarkPool / Automator | The opening holder sandwiches an intent: victim filled at 98.8 of 100 bps slippage, Treasury gets 0 | ✅ | ◐ P-1 cuts it 96 %; see P-9 |
| H-5 | High | DarkPool | A batch proof is bound to exact X, Y: a 74 k-gas grief voids 40 s proofs, refunds included | ✅ real proofs | ✅ P-8 |
| H-6 | High | SovereignBank | `TimeDeposits.settle` credits the balance delta around an arbitrary call: theft of another depositor's principal | ✅ | ✅ P-H6 |
| H-7 | High | batch_swap | Intent nullifiers use a public key, so anyone links each batch slot to its creator (4 of 4 linked) | ✅ | ✅ ZK-P1 |
| M-1 | Medium | SovereignBank | A 1 bp balance loss freezes 100 % of demand deposits | ✅ | ✅ P-M1 |
| M-2 | Medium | OptionsMarket | A per-token shortfall freezes every series using that token | ✅ | ✅ P-M2 |
| M-3 | Medium | DarkPool | `add/removeLiquidity` have no slippage bounds | analysis | patch §2 |
| M-4 | Medium | frontend | `minOut` is computed from the manipulable pool quote, not the oracle | analysis | patch §2 |
| M-5 | Medium | batch_swap / DarkPool | Deadlines are extended by `BATCH_CLOCK_WINDOW` (300 s), and the prover's choice of `tNow` gives a swap-or-refund option | analysis | patch §3 |
| M-6 | Medium | batch_swap / DarkPool | The anonymity floor `MinSwaps` is off-chain only | analysis | patch §3 |
| M-7 | Medium | zk-DECO | Query salt: public means no privacy, secret means a settlement option for the creator | analysis | design §3 |
| L-1 | Low | DarkPool | Zero entries inside `n` burn nullifier 0 and insert leaf 0 | ✅ | ✅ P-7 |
| L-2…L-6, I-1…I-3 | Low / Info | various | Silent `uint128` casts, bps flooring, missing domain separation in `extDataHash`, late returns stranded, read-only reentrancy, untested modules | analysis | §2–3 |
| Q-1 … Q-9 | see §4 | C++ engine / SearcherExecutor | Strategic dominance, synchronous RPC, a planner bug (−23 % profit), 165–373× planner, 3.6× book, 5× ABI, +312 % variance bias | bench | ✅ code §4 |

---

## 1. Reproduce everything

```bash
# Exploits on the audited code: 13/13 PASS (12 attacks work + 1 gas measurement)
cd strawberryswap/contracts
FOUNDRY_TEST=audit forge test -vv            # DarkPoolExploits (7), RealProofExploits (3), BankOptionsExploits (3)
forge test                                   # the product suite is untouched: 92/92 pass

# Economic models (exact integers, mirror the contract and the circuit's clearing rule)
cd audit/models && python3 lp_attack.py && python3 sandwich.py && python3 sandwich2.py

# Circuits (from strawberryswap/circuits, artifacts from `npm run compile` / setup-dev)
node audit/zkdeco_crossfield.mjs             # 4 forged attestations, real Groth16 proofs (dev keys)
node audit/intent_linkability.mjs            # de-anonymises the real-proof E2E batch from public data
node audit/freewire.mjs                      # exact single-wire under-constraint scan of the 3 circuits

# C++ hot path
cd ../bots/inventory-cpp
g++ -std=c++20 -O3 -march=native -DNDEBUG -Iinclude audit/bench.cpp -o /tmp/bench && /tmp/bench

# The fixes: apply the reference patch on a copy, then
#   forge test                   -> 92/92 pass
#   FOUNDRY_TEST=audit forge test -> the 12 exploits FAIL; P3Check + gas pass
git apply -p1 --directory=strawberryswap strawberryswap/docs/AUDIT_PHASE8.patch   # (from the repo root)
```

**Mocks, and why they are legitimate here.**

* The exploit tests that use an accept-all verifier only ever submit statements an honest prover can produce. The batch
  flows are computed with the circuit's own clearing rule (`pLo = ⌊WAD·(Y+A1)/(X+A0)⌋`, identical to
  `circuits/test/batch.lib.mjs` and the Go batcher).
* The batch proving key is public, so the attacker proves exactly as the Automator does.
* `RealProofExploits` repeats C-1 and H-5 with no mocks on the proof path.

---

## 2. Mission 1 — Solvency and economic red team

Notation used throughout:

* Effective reserves: `x = r0 + v0`, `y = r1 + v1`.
* `P` is the zk-oracle price (token1 per token0, WAD-scaled).
* `V_P(x, y) = x·P + y` is the pool's value at `P`.
* A trade's **harvest** is `−ΔV_P`: the value the pool loses, measured at the oracle price.

### C-1 · `settleBatch` is a fee-free, oracle-free arbitrage path (Critical)

**Root cause.** The design routes LVR (loss-versus-rebalancing) through `flashRebalance`, which charges
`0.8·harvest`. But `settleBatch` executes *any* set of intents at the curve price:

* with **zero fee**;
* **without reading the oracle**;
* checking only `K' ≥ K`.

Anyone can create intent notes. An intent note is just a note owned by `BATCH_PUBKEY`, made with an ordinary
`transferNote`. Anyone can also prove a batch that contains only their own intents. A one-intent batch selling `A0`
pays exactly `A0·⌊WAD·Y/(X+A0)⌋/WAD`, which is the constant-product output. In other words, it *is* a zero-fee swap.

**Exploit.** When the pool drifts from the market, the arbitrageur runs, in one transaction (all roots, leaf indices
and X, Y are predictable, so every proof is precomputed):

1. `depositNote` the input token;
2. `transferNote` the note into an intent note;
3. `settleBatch` with that one intent;
4. `withdrawNote` the output.

The arbitrageur keeps the whole harvest. The Treasury receives nothing.

**Evidence.** The test pool has a 5 % deviation, virtual reserves 1 M/2 M and real reserves 100 k/200 k
(`test_C1_settleBatchBypassesFlashRebalanceTreasuryCut`):

| path | Treasury receives | arbitrageur keeps |
|---|---|---|
| `flashRebalance` (designed) | **1,128.39** token1 | 282.10 (20 %) |
| `settleBatch` (one intent) | **0** | **1,410.49 (100 %)** |

The real-proof test `test_C1_real_batchTradesAtCurveWithDeadOracleAndZeroTreasury` uses an oracle that has **never
published**. In that state `flashRebalance` reverts `StaleOracle`, yet the real 4-intent batch moves 849.9 token0 against
1,699.3 token1 at the curve. Treasury revenue is 0.

**Patch P-1** (`DarkPool.settleBatch`). No batch may make the pool lose value at the oracle price. The comparison is
exact integer arithmetic, with no division:

```solidity
uint32 firstLeaf = _consumeBatch(b);
_applyFlows(b, r0, r1, kBefore);
if ((b.in0 | b.in1 | b.out0 | b.out1) != 0) {          // pure-refund batches never need the oracle
    uint256 price = _freshPrice();
    if (b.in0 * price + b.in1 * WAD < b.out0 * price + b.out1 * WAD) revert ValueLeak();   // ΔV_P >= 0
}
```

**Why this is sufficient, and why it costs retail nothing.**

1. **Arbitrage is blocked.** Moving the pool *toward* `P` lowers `V_P`, because `V_P` restricted to the curve
   `xy = K` is convex with its minimum at spot = `P`. Every arbitrage batch is therefore rejected. Arbitrage must use
   `flashRebalance`, which pays.
2. **Retail is not blocked.** When spot = `P`, every curve trade has `ΔV_P ≥ 0`. The tangent line to the convex curve
   at the current point has slope `−P`. Every point with `K' ≥ K` lies on or above that line, so
   `Δy ≥ −P·Δx`. The only cost to retail is that, while the pool is off-oracle, a trade that would *improve* the price is
   held until searchers rebalance.
3. **Bounds.** `in0 < 2¹¹²` (in-circuit) and `P ≤ 10²⁴`, so every product is below `2¹⁹³`.

**Residual risk (stated honestly).** P-1 compares against a price that is up to 60 s old. When the market moves and the
oracle has not yet updated, a batch that trades *with* the move still has `ΔV_P ≥ 0` at the stale `P`. That
"ahead-of-the-oracle" LVR stays free. Closing it requires one of:

* oracle updates within one block;
* a fee on value-moving batches (this conflicts with axiom 2);
* a sealed-bid batch auction.

P-1 closes the dominant case, where the pool lags the oracle, and with it the **whole** C-1 attack as demonstrated.

**Verified.** `test_C1_*` → `ValueLeak()` and `StaleOracle()`. All 92 tests pass. The fuzzer
`testFuzz_settleBatchEnforcesKMonotonicity` now asserts `accepted ⇔ K' ≥ K ∧ ΔV_P ≥ 0`.

---

### C-2 · Immutable virtual reserves turn liquidity provision into a price weapon (Critical)

**Root cause.** `x = r0 + v0` and `y = r1 + v1` with `v0, v1` immutable. A *proportional* deposit `(f·r0, f·r1)`
mints `f·S` shares and moves the price:

```
p' = (r1(1+f) + v1) / (r0(1+f) + v0)   ≠   p = (r1 + v1)/(r0 + v0)     unless   r1/r0 = v1/v0
```

After any market move, `r1/r0` drifts far from `v1/v0`. In the test pool at 2400 with launch price 2000, the real ratio
is **98,600** while the virtual ratio is 2,000. With `f → ∞`, the price tends to `r1/r0`.

**Exploit** (atomic, flash-funded):

1. add `f·(r0, r1)`;
2. arbitrage the pool back to `P` through a free batch (C-1);
3. remove the shares;
4. arbitrage again: the removal moves the price toward `v1/v0`.

Let `V` be the pool's value before the attack, `V'` its value after the first arbitrage, and `H2` the second
arbitrage's harvest. The attacker's profit is

```
profit = V − V'/(1+f) + H2  ≥  0
```

with equality only when `r1/r0 = v1/v0`. The proof: `V'` is the minimum of `V_P` on the post-deposit curve. The honest
LPs bear exactly that profit.

**Evidence.** Pool: launch 2000, virtual reserves 1,000/2 M, an honest LP holding 100/200 k, market at 2400
(`lp_attack.py`, `test_C2_flashLiquidityAttackDrainsHonestLPs`):

| flash multiple `f` | 1 | 10 | 100 | 1000 |
|---|---|---|---|---|
| honest LP loss | 2.8 % | 23.4 % | **55.9 %** | 69.3 % |

At `f = 100`:

* The spot jumps from **2,400 to 30,570** on a *proportional* deposit.
* The attacker nets **+234,735.85** token1.
* The honest LP's value falls from 419,958.51 to 185,222.66.
* It needs about 42 M of flash liquidity. Even at a 0.05 % flash fee (≈ 21 k), about 214 k of profit remains.

**Patch P-3.** Virtual reserves scale with total shares. This is exactly a Uniswap-v3 single-range position, whose
virtual reserves are `L/√p_b` and `L·√p_a`, with `L ∝ shares`:

```solidity
uint256 public refShares;                                 // totalShares right after the first deposit
function virtual0() public view returns (uint256) { return refShares == 0 ? _v0 : _v0 * totalShares / refShares; }
function virtual1() public view returns (uint256) { return refShares == 0 ? _v1 : _v1 * totalShares / refShares; }
// addLiquidity:  if (refShares == 0) refShares = ts + minted;
```

* A proportional deposit gives `x' = (1+f)·x` and `y' = (1+f)·y`, so `p' = p`, and `K` scales by `(1+f)²`.
* Batches and rebalances do not change `totalShares`, so `K` is constant within them.
* The ABI is unchanged. The Go Automator (`rpc.go`) and the frontend already read `virtual0()` and `virtual1()` on every
  refresh.

**Verified.**

* The model gives profit `0` for every `f`.
* `P3Check.test_P3_proportionalLiquidityNoLongerMovesThePrice`: 100× add and remove leave the spot at
  `2400.000000000000000003` **to the wei**.
* With P-1 also applied, the attack already fails at its first free arbitrage.

P-3 is still needed. Without it, *honest* large withdrawals hand an arbitrage to searchers, and the remaining LPs lose
it.

---

### H-1 · The Treasury payment is a shared balance delta: N nested pools pay once (High)

**Root cause.** `flashRebalance` checks:

```
paid = token1.balanceOf(treasury) − tBefore
```

`tBefore` is read before the callback. Pools that share a Treasury and a quote token (USDC, for example) each see the
**same** payment. Each pool's `nonReentrant` guard is local, so pool B can be rebalanced *inside* pool A's callback.

**Exploit.** `NestedSearcher` does three things:

1. rebalances A;
2. inside A's callback, rebalances B;
3. pays `max(reqA, reqB)` once.

Both pools accept.

**Evidence.** `test_H1_nestedRebalancesPayTheTreasuryOnce`: 2,256.78 due, 1,128.39 paid. With `k` pools the attacker
pays `max(req)` instead of `Σ req`, which is `1/k` of what is owed when the pools are alike. Any unrelated `token1` inflow to the Treasury during the callback is also credited to the
searcher.

**Patch P-4.** Pull exactly what is owed, from the initiator, after every check:

```solidity
uint256 required = harvest * EXTRACTION_BPS / BPS;
reserve0 = uint128(n0); reserve1 = uint128(n1);           // effects
if (required > 0) token1.safeTransferFrom(msg.sender, address(treasury), required);
treasury.recordExtraction(address(token1), required, msg.sender);
```

**Side benefits.**

* Searchers no longer guess the cut, so the C++ planner's 5 bps over-payment margin disappears.
* `SearcherExecutor` approves `treasuryPay` before the call and 0 after. That turns the planned cut into a **maximum
  pay guard**: if the oracle moved against the plan, the pull reverts and nothing executes.

**Verified.** The Treasury receives `3,385.17 = pay + reqA + reqB`. Two existing tests now pin the new semantics: "the
pool pulls exactly the cut", and "a donation lowers the pulled cut by 80 % of its value".

---

### H-2 · 64 junk transactions void every proof in flight (High)

**Root cause.**

* `ROOT_HISTORY = 128` counts **leaves**, not transactions: `_insert` pushes one root per leaf.
* Every JoinSplit inserts 2 leaves, and a batch inserts `n`.
* A `transferNote` with two dummy inputs and two zero-value outputs needs **no funds**. Anyone can prove it.

**Evidence.**

* `test_H2_64JunkTransfersEvictEveryRoot`: after 64 junk transfers, the user's withdrawal reverts `UnknownRoot`.
* With real Poseidon and a real verifier (`test_gas_realTransferNote`), one `transferNote` costs **1,625,492 gas**. A full
  eviction costs **104 M gas**, which is a few dollars on an L2.
* A browser proof takes about 10 s and a batch proof about 40 s (Phase 7 measurements). A busy pool also evicts roots
  organically.

**Patch P-6.** One root per transaction, and `ROOT_HISTORY = 1024`:

```solidity
_append(js.commitments[0], js.encryptedNotes[0]);
_pushRoot(_append(js.commitments[1], js.encryptedNotes[1]));   // JoinSplit: one root
// settleBatch: root = _append(...) for each output; _pushRoot(root) once
```

* An eviction now needs 1,024 transactions, about 1.6·10⁹ gas. That cannot fit in a proof's lifetime.
* Gas **falls**: `transferNote` goes from 1,625,492 to 1,581,175, and `settleBatch` (4 intents, real proof) from
  3,056,697 to 2,956,871.
* Proofs are always generated against post-transaction roots, so no fixture changed.

**Verified.** The root survives 64 junk transfers.

---

### H-3 · Tree exhaustion freezes every shielded note forever (High)

**Root cause.** `DEPTH = 20` with no rollover. `withdrawNote` itself inserts 2 leaves (change and dummy). Once
`nextLeafIndex = 2²⁰`, every `withdrawNote`, `transferNote` and `settleBatch` reverts `TreeFull`, and that includes
refunds. Notes have no other exit.

**Cost to fill.** 2¹⁹ junk `transferNote`s × 1,625,492 gas = **8.52·10¹¹ gas**. That is about 8.5 ETH at 0.01 gwei, or
about 0.85 ETH at 0.001 gwei. Organic growth reaches the same wall.

**Profit motive.** In OptionsMarket, shielded holders must `unshield` before they can `exercise`. A **writer** who fills
the tree (or keeps evicting roots, H-2) before expiry makes those calls worthless and then `claimWriter`s the
collateral. The same NoteTree is shared by Berrycoin and OptionsMarket.

**Evidence.** `test_H3_fullTreeFreezesAllShieldedFunds` (state reached with `vm.store`): `withdrawNote` and
`transferNote` revert `TreeFull`, leaving 10 token0 of user funds unreachable.

**Patch P-5.** Rollover with **no circuit change**:

```solidity
if (index == uint32(1) << DEPTH) { _rollover(); index = 0; }      // in _append
function _rollover() private {
    isFinalRoot[_tip] = true;                // the completed tree's root stays valid FOREVER
    emit TreeRolled(treeId, _tip); treeId += 1;
    for (uint256 i; i < DEPTH; ++i) _filled[i] = _zeros[i];
    nextLeafIndex = 0;
}
function _knownRoot(bytes32 r) internal view returns (bool) { return isKnownRoot[r] || isFinalRoot[r]; }
```

* `_tip` is written only for the last slot of a tree, so this adds **zero** gas in normal operation.
* Nullifiers bind `(commitment, leafIndex, key)`. Two notes in different trees can collide only if their commitments are
  identical *and* sit at the same index, and only the owner can create such a pair.
* Wallets (`frontend/src/zk/engine.ts`) and the Go `notetree` must segment their scans on `TreeRolled`.

**Verified.** In the "full" state, the withdrawal now succeeds. The note in the old tree stays spendable against its
final root.

**Further options.**

* `DEPTH = 32`. This needs new circuits and keys, and costs +12 Poseidon per insert and about +60 % Merkle constraints.
* Subtree insertion of the 2 or `n` leaves of a transaction, which removes about 50–75 % of the hashes. The real cost
  today is 2.75 M of `settleBatch`'s 3.05 M gas, nearly all Poseidon.

---

### H-4 · The intent-opening holder sandwiches users; "at worst censor" is false (High)

**Root cause.**

* The Automator receives each intent's terms (`value`, direction, `minOut`, `deadline`).
* Batches are permissionless and fee-free.
* The circuit only enforces `o ≥ minOut`.

**Exploit.** Three one-intent batches:

1. the operator's own front-run;
2. the victim's intent;
3. the operator's back-run.

They can share a block or a transaction, because X, Y and the roots are predictable. The operator also holds a free
timing option until the deadline (and see M-5).

**Evidence.** A 10 token0 sell into a 2000-priced pool with 1 % slippage (`test_H4_…`, `sandwich.py`). The victim is
quoted 19,819.82 and filled at **19,623.98**, which is 98.8 bps of the 100 bps budget. The attacker makes **+376.02**
and the Treasury gets 0.

Sweep over the slippage tolerance (`sandwich2.py`):

| slippage | unpatched: attacker / victim extra loss | P-1: attacker / victim extra loss |
|---|---|---|
| 20 bps | 219.6 / 19 bps | 42.2 / 19 bps |
| 100 bps | 376.0 / 98 bps | 44.8 / 45 bps |
| 300 bps | 772.8 / 299 bps | 44.8 / 45 bps |

**What P-1 does here.** The back-run moves the pool *toward* the oracle, so P-1 rejects it. The attacker must unwind
through `flashRebalance` and pay 80 %. The attacker's **marginal** gain over plain back-running falls from 195.84 to
**8.80** (−96 %), and most of the victim's extra loss now goes to the Treasury.

The victim still loses about 45 bps more than without the attack. **P-1 is necessary but not sufficient.**

**Patch P-9.** In-circuit, oracle-relative price protection:

* Add `P` as a public input of `batch_swap`, fed by the contract from `_freshPrice()`.
* Have each intent commit to `maxSlipBps` in place of an absolute `minOut`.
* Enforce, per swap slot:
  * `o·WAD·10⁴ ≥ value·P·(10⁴ − maxSlipBps)` for token0 sellers;
  * `o·P·10⁴ ≥ value·WAD·(10⁴ − maxSlipBps)` for token1 sellers.

A front-run can then never push a user below the oracle-relative bound. The user sets `maxSlipBps` to the expected
impact plus a small buffer.

Interim fix, frontend only (M-4): compute `minOut` from the **oracle** rather than `getAmountOut`, and make 50 bps
the maximum rather than the default.

**Structural fix:** clear batches at the oracle price (an RFQ or frequent-batch-auction design), so ordering carries no
value.

---

### H-5 · A batch proof is bound to the exact pool state: a 74 k-gas grief voids it, refunds included (High)

**Root cause.** `_batchInputs` feeds the **current** `X = r0 + v0` and `Y = r1 + v1`, but a batch proof takes about
40 s. Any state change in between voids the proof:

* a 1-share `removeLiquidity`;
* any `flashRebalance`;
* any batch.

That includes refund-only batches, so expired intents cannot be returned either. Intent notes have no other exit.

**Evidence.** `test_H5_real_oneWeiReserveChangeBlocksSwapsAndRefunds` (real proofs): `removeLiquidity(1e6 shares)` costs
**74,444 gas** and the real batch, which includes a refund slot, reverts `InvalidProof`.

**Patch P-8.** Accept a proof against a **recent** state. Then enforce `K` and P-1 against the **current** state:

```solidity
struct Batch { … uint256 X; uint256 Y; }                  // the state the proof was generated against
bytes32 h = keccak256(abi.encode(b.X, b.Y));
if (h != currentState) {
    uint256 ended = stateEndedAt[h];                      // 0 = never a state of this pool
    if (ended == 0 || block.number > ended + STATE_WINDOW_BLOCKS) revert StaleState();
}
// _markState() runs after every reserve/share change: stateEndedAt[old] = block.number; currentState = new
```

**Why the pool and users stay safe.**

* **Pool.** The flows are checked against the *current* `K` and value. If the state moved in the users' favour, the
  `K` check rejects the batch.
* **Users.** Their prices stay within `minOut`, and P-9 makes that oracle-relative. The submitter can choose among the
  states of the last 64 blocks, which is bounded and equivalent to choosing *when* to submit.

**Cost.** Two `SSTORE`s per state change, about 25–30 k gas. Net gas still falls, thanks to P-6.

**Interface change.** The `Batch` struct gains `X` and `Y`. The Go encoder in `internal/chain/abi.go` must follow.

**Verified.** The proof is accepted within the window. The E2E test now also checks the `StaleState` expiry.

---

### H-6 · `TimeDeposits.settle` credits other people's money (High)

**Root cause.** In ROUTED mode, `paid = balanceAfter − balanceBefore`, measured around
`d.adapter.recall(d.handle)`. The adapter is chosen **by the depositor** and runs arbitrary code. Any token that reaches
TimeDeposits during that window is credited to the depositor.

**Exploit.** Take a plausible honest adapter whose `recall` returns funds to the depositor recorded at `deploy`, and so
does not bother checking the caller (`OpenRecallAdapter`). Mallory then:

1. opens a 1-wei deposit with `ThiefAdapter`;
2. after maturity, calls `settle`.

`ThiefAdapter.recall` triggers Alice's honest `recall` inside Mallory's measuring window.

**Evidence.** `test_H6_…`: Mallory receives Alice's **1,000,000**. Alice's own `settle` reverts `RecallFailed`, and
after `RECALL_GRACE` she can only write the deposit off.

**Patch P-H6.** Pull-measured settlement, plus an adapter specification:

```solidity
interface IYieldAdapter { function recall(bytes32 handle) external returns (uint256 amount); } // approve, never push
try d.adapter.recall(d.handle) returns (uint256 amount) {
    uint256 before = token.balanceOf(address(this));
    if (amount > 0) token.safeTransferFrom(address(d.adapter), address(this), amount);
    if (token.balanceOf(address(this)) - before != amount) revert FeeOnTransferUnsupported();
    paid = amount;
} catch { … }
```

**Verified.** Mallory receives 0.

**Complete fix.** Give each ROUTED deposit its own escrow (a minimal-proxy vault as the adapter's depositor). Third-party
pushes then land in the right account by construction. The specification must also say that `recall` is callable only
by the depositor.

Related, **L-5**: a deposit written off after `RECALL_GRACE` cannot collect funds that arrive later. They are stranded,
because there is no rescue path by design. The per-deposit escrow fixes this too.

---

### M-1 · A 1 bp loss freezes 100 % of demand deposits (Medium)

`DemandDeposits.withdraw` ends with a **global** check, `balanceOf(this) ≥ totalDeposits`. Any shortfall caused by the
token freezes every account for every amount: a negative rebase, stETH slashing, a bridge haircut or a sender-side fee.
The shortfall persists, because each withdrawal reduces both sides equally.

**Evidence.** `test_M1_…`: after a −0.01 % rebase, withdrawing even 1 wei reverts. 99.99 % of the money is present and
0 % is withdrawable.

**Patch P-M1.** A pro-rata haircut instead of a freeze, plus an exact outflow check:

```solidity
uint256 held = token.balanceOf(address(this));
uint256 pay  = held >= total ? amount : amount * held / total;   // held/total is invariant under exits
token.safeTransfer(to, pay);
if (held - token.balanceOf(address(this)) != pay) revert FeeOnTransferUnsupported();
```

Why the order of exits stops mattering:

```
(held − amount·held/total) / (total − amount)  =  held/total
```

**Verified.** Withdrawals proceed, each one bearing the 1 bp.

---

### M-2 · OptionsMarket: one shortfall freezes every series of that token (Medium)

`_requireSolvent(t)` runs after every operation, against the **global** `liabilities[t]`. A shortfall in the underlying
of one call series blocks every series that uses that token:

* `exercise`;
* `claimWriter`;
* `closePosition`;
* new writes.

**Evidence.** `test_M2_…`: Bob's untouched series cannot be closed, and Alice cannot claim after expiry.

**Patch P-M2.** `_payOut(t, to, owed)` pays `owed·balance/(liabilities + owed)` when short, with an exact outflow check.
`_requireSolvent` stays only on the **inflow** path (minting), so no new liabilities are taken on against a token that
is already short.

**Verified.** Both calls succeed with the haircut.

---

### M-3 · `addLiquidity` / `removeLiquidity` have no slippage bounds (Medium)

`minted = min(a0·S/r0, a1·S/r1)`, and the unbalanced excess is **donated** to the pool. Front-running a deposit with a
batch that shifts `r1/r0` converts part of the victim's deposit into a donation to the LPs, the attacker included. The
same holds in reverse for withdrawals. P-3 removes the price effect of proportional deposits, but not the ratio attack.

**Patch:**

```solidity
addLiquidity(a0, a1, to, minShares, deadline)
removeLiquidity(shares, to, min0, min1, deadline)
```

Revert when the result is below the minimum or `block.timestamp > deadline`. This is the Uniswap-v2 router pattern.

### M-4 · `minOut` comes from a manipulable quote (Medium, frontend)

`usePrivateSwap.ts` computes `minOut = getAmountOut(amount)·(1 − slippage)`. If the pool is already skewed when the user
quotes, the "protection" bakes the skew in.

**Patch:**

```
minOut = min(curveQuote, value·P_oracle·(1 − impact(value)))·(1 − buffer)
```

Default `buffer` to 10–20 bps above the modelled impact. P-9 is the in-circuit version.

### L-1 · Zero entries inside `n` (Low)

The circuit treats `(nullifier, commitment) = (0, 0)` as a padding slot. `_consumeBatch` nonetheless spends nullifier 0
and inserts leaf 0 for every listed entry (`test_L1_…`).

Impact: one junk leaf, and burning nullifier 0 forever. The Go Automator only lists active slots, so it is unaffected.

**Patch P-7:** `if (nullifier == 0 || commitment == 0) revert BadBatch();`

### Low and informational (DarkPool, NoteTree, SovereignBank, OptionsMarket)

* **L-2 · Silent `uint128` casts.** These appear in `custody0 += uint128(amount)`, `custody0 -= uint128(amount)` and
  `reserve0 = uint128(n0)`. They are the custody back-stop of last resort, the one meant to survive forged proofs, and
  they truncate silently. Today they are guarded by the 128-bit range in-circuit and by token supply, so the risk is
  low. Use `SafeCast.toUint128` so the back-stop does not depend on the circuit.
* **L-3 · bps flooring in `_deviation`.** `|y/x − P|` is floored to whole bps. A pool within 1 bp of the oracle cannot
  be rebalanced at all, and some legitimate rebalances revert. Compare exactly instead:

  ```
  |y'·WAD − P·x'|·x  <  |y·WAD − P·x|·x'
  ```

  `x`, `y` < 2¹⁰⁵, so every term is below 2²⁵⁰.
* **L-4 · No domain separation in `extDataHash`.** It binds the recipient and ciphertexts, but not `block.chainid`,
  `address(this)` or the function. Today, cross-deployment replay needs identical tree roots and the empty root only
  admits dummy inputs, so it is not exploitable. Make it `keccak(chainid, this, selector, …)`.
* **L-6 · Self-loss.** `exercise(…, to = address(this))` loses the delivered collateral into surplus. Reject `to == this`
  as `safeTransferFrom` already does.
* **I-1 · Read-only reentrancy.** During the `flashRebalance` callback, `balanceOf(pool)` is reduced while the reserves
  are stale. Integrators must read `reserve0/1` and never balances.
* **I-2 · Untested modules.** **SovereignBank, OptionsMarket, StrategyVault and SearcherExecutor have no test suite at
  all.** M-1, M-2 and H-6 would have been caught by the most basic adversarial-token tests.
* **I-3 · Committed dev keys.** `frontend/public/zk/` commits the dev `joinsplit.zkey` (11 MB with the wasm). That is
  consistent with the bundle pinning its hash, but these are single-party keys (C-0).

### What held: attacks tried and refuted

* **Unbacked options (OptionsMarket).** The only mint path pulls `qty·U` (or `qty·Q`) and checks the *received* delta
  exactly. Sizes are integer contracts, so there is no rounding. `claimWriter` pays pro-rata, and the last claimant gets
  the exact remainder. A shielded supply cannot exceed what was shielded, by the ceiling. **No unbacked mint exists.**
* **Custody under forged proofs (DarkPool).** Even with forged proofs, `withdrawNote` can take at most `custody` (checked
  subtraction), and batches cannot extract reserves past `K`. LP capital is protected by `K`, user custody by the
  circuit.
* **Rounding.** The existing fuzzers already show round-trip and many-round-trip extraction is impossible. I re-derived
  the bound for the batch price: `pLo ≤ q ≤ pHi` keeps `K' ≥ K` for every batch.
* **Donations.** Absorbing surplus lowers the Treasury's due by exactly 80 % of the donated value, which is a net loss
  for the donor (existing test).
* **Reentrancy.** Every state-changing entrypoint is `nonReentrant`. The callback window cannot reach another mutating
  path of the same pool. Cross-pool reentrancy matters only through the Treasury balance, which is H-1.
* **DemandDeposits.** No path moves funds except the owner's own withdrawal. Tokens are isolated from each other.

---

## 3. Mission 2 — Cryptographic audit (joinsplit, batch_swap, zkdeco_oracle)

### 3.1 Exact under-constraint scan (`circuits/audit/freewire.mjs`)

For each circuit, I computed a valid witness `w` and then examined every prover-controlled wire `i` exactly. Write
`a, b, c` for the coefficients of wire `i` in constraint `k`. Changing wire `i` by `δ` gives:

```
g_k(δ) = (A_k·w + a δ)(B_k·w + b δ) − (C_k·w + c δ) = δ·(L_k + δ·Q_k)
L_k = (A_k·w)·b + (B_k·w)·a − c        Q_k = a·b
```

So the set of `δ ≠ 0` that keeps constraint `k` satisfied is one of three things:

* every `δ` (`L = Q = 0`);
* no `δ` (`Q = 0`, `L ≠ 0`);
* exactly `{−L/Q}`.

A wire admits a second value, with every other wire and every public input held fixed, if and only if the intersection
over its constraints is non-empty. For one-wire changes this is an exhaustive test, not sampling.

| circuit | constraints | wires | unconstrained wires | one-wire alternatives | non-benign |
|---|---|---|---|---|---|
| joinsplit (2 real inputs / shield) | 12,596 | 12,645 | 0 | 2 / 2 | **0** |
| batch_swap N=4 (2 swaps, refund, padding) | 28,736 | 28,779 | 0 | 4 | **0** |
| zkdeco_oracle | 21,637 | 21,390 | 0 | 68 | **0** |

Every alternative is an `IsZero.inv` hint at `in = 0`: `member.isz`, `isDummy`, `vz`, `fSel`, `isDot`, `isTerm` and the
`SliceAt` equality bits. Those hints feed no other constraint. **No signal is under-constrained.** The real bug in
zk-DECO (C-3) is *semantic*: every constraint is satisfied, but the statement proven is the wrong one.

### 3.2 Integer soundness of every equation (no wrap mod p)

`p ≈ 2^253.6`. For each equation, both sides are bounded below `p`, so the equation holds over the integers:

| equation | bound |
|---|---|
| JoinSplit: `in0 + in1 + publicIn = out0 + out1 + publicOut` | every term < 2¹²⁸, so each side < 2¹³⁰ |
| batch: `value·num = outValue·den + rem` | value < 2¹¹², num ≤ pLo < 2¹²⁸, so < 2²⁴⁰; rem < 2¹²⁸ |
| batch: `pLo·D + pRem = WAD·Nn` | pLo < 2¹²⁸, D < 2¹²⁰, so < 2²⁴⁸; WAD·Nn < 2¹⁸¹ |
| batch: `netIn − netOut = ΣA − ΣO` | each side in (−2¹²⁰, 2¹²⁰) |
| `LessThan(128)` with `den = pHi = 2¹²⁸` | correct: `n2b` input = `rem` < 2¹²⁸ |

### 3.3 Nullifiers, double spends and Merkle topology

* **Nullifier binding (JoinSplit).** `nullifier = Poseidon(cm, Bits2Num(pathIndices), privKey)`. The leaf index is the
  path itself, so one note has exactly one nullifier.
  * Proving the same leaf under different path bits requires a Poseidon collision.
  * Duplicate leaves at `2j` and `2j+1` hash symmetrically, but they are two real notes and are spent once each.
* **Leaf / internal-node confusion.** The depth is fixed at 20, so an internal node can never be presented as a leaf.
  Leaves are `Poseidon(4)` (t = 5) and nodes are `Poseidon(2)` (t = 3), whose different widths act as domain
  separation. The zero leaf `keccak("…zero") mod p` has no known preimage.
* **Path direction.** `bit = 0` means `H(node, sibling)` in both `_insert` and `MerkleInclusion`. The real-proof E2E
  tests confirm the two agree.
* **Dummy inputs.** A value-0 input skips membership but still produces a nullifier from its own random key. It cannot
  forge another note's nullifier, because that requires the victim's key, or `value = 0` for an intent, which every
  active slot forbids.
* **Intent notes cannot be spent by JoinSplit.** That would require `Poseidon(k) = BATCH_PUBKEY`, a keccak-derived
  nothing-up-my-sleeve constant.
* **Unused public inputs.** `extDataHash²` and `providerId²` keep them in the QAP. Without that, their IC point would be
  ∞ and the inputs malleable. **Present and correct.**
* **Groth16 proof malleability.** `(A, B, C)` can be re-randomised. That is harmless here, because uniqueness comes from
  nullifiers and never from proof hashes.

### C-3 · zk-DECO: `price` and `ts` are not bound to the object that carries `symbol` (Critical for SyntheticRWA)

**Root cause.** The three slices (`"symbol":"…"`, `"price":…` and `"ts":…`) are located at three **independent**
prover-chosen offsets. Nothing ties them to the same JSON object, nothing requires the first occurrence, and nothing
fixes their order. Any notarised body containing more than one price or timestamp lets the prover choose.

**Evidence.** `circuits/audit/zkdeco_crossfield.mjs` produces **real Groth16 proofs that verify** against the deployed
verification key:

| body (≤ 96 B, notarisable) | claimed for hidden query XAUUSD |
|---|---|
| `{"symbol":"XAUUSD","price":2345.60,"ts":…,"x":{"symbol":"XAGUSD","price":29.10}}` | **29.10** (silver price for gold) |
| `{"symbol":"XAUUSD","bid":{"price":2340.10},"ask":{"price":2351.30},"ts":…}` | bid **or** ask, prover's choice |
| `{"symbol":"XAUUSD","price":2345.60,"ts":1767225600,"prev":{"price":2290.00,"ts":1767225000}}` | **2290.00 at the earlier ts** |

The last row is especially damaging. `SyntheticRWA.settle` replaces the canonical price with any attestation that has
an *earlier* `observedAt`, so the prover can override a settled series.

**Patch ZD-1.** Pin the canonical single-object layout
`{"symbol":"<SYM>","price":<I>.<F>,"ts":<10 digits>}`:

```circom
body[0] === 123;                                   // '{'
offSymKey === 1;
body[1 + 10 + SYMLEN + 1] === 44;                  // ',' right after the symbol's closing quote
offPriceKey === 1 + 10 + SYMLEN + 2;
termProd[k] <== (c - 44) * 1;                      // price terminator must be ',' (ts follows)
offTsKey === offPriceKey + 8 + intLen + fracLen + 2;
tSlice.out[15] === 125;                            // '}' closes THE object
```

**Verified.** Using witnesses from the recompiled circuit:

* 3 canonical bodies are accepted.
* All 4 forgeries are rejected, as is the honest first price inside a non-canonical body. The patch fails closed.

As a side effect, constant offsets let circom fold the symbol and price slices: the circuit drops from **21,637 to 7,164
constraints (−67 %)**.

A provider with a different layout needs its own pinned template. Never use a general "find the key" parser.

### H-7 · Intent nullifiers use a public key: every batch slot links to its creator (High, privacy axiom)

**Root cause.** In `batch_swap`, `nullifier = Poseidon(cm, leafIndex, BATCH_NK)`, where `BATCH_NK` is a published
constant. The circuit header notes the key is "safe to make derivable by anyone", and it is, *for double spending*. For
privacy it is fatal:

1. An observer computes `Poseidon(cm_i, i, BATCH_NK)` for every leaf from `NoteInserted` events and matches the results
   against batch nullifiers.
2. Slot `j` carries `nullifier[j]` *and* `commitment[j]`, so the observer also links the intent to its output note.
3. The frontend sends `transferNote` from the user's own wallet (`usePrivateSwap.ts`), so the observer learns *which
   address traded in which batch, and which output note it received*.

**Evidence.** `circuits/audit/intent_linkability.mjs` uses only the public data of the real-proof E2E batch. It links
**4 of 4** slots to their creating transaction.

**Patch ZK-P1.** Use a secret already in the opening as the nullifier key:

```circom
nf.inputs[2] <== destBlinding;     // was BATCH_NK
```

`destBlinding` is bound inside `cm` through `intentBlinding`, so the nullifier stays unique per note and there is no
double-spend path. Only the user and the Automator know it, and the Automator already knows the intent. The wallet
re-learns it from its own intent ciphertext, so recovery is unchanged.

**Verified.** The recompiled `batch_swap` (28,748 constraints, +12) passes **38 of 38** of its adversarial tests with the
patched reference library.

Also relay `transferNote` instead of sending it from the user's address. Otherwise the intent's creation is public
regardless.

### M-5 · Deadlines stretch by 300 s, and the prover's choice of `tNow` is a free option (Medium)

The circuit allows a swap when `deadline ≥ tNow`. The contract accepts `tNow ≥ block.timestamp − 300`. A swap therefore
executes up to **300 s after its deadline**. Near the deadline, the prover's choice of `tNow` decides between swap and
refund, which is a free option for the Automator.

**Patch.** Swap only if `deadline ≥ tNow + W` (in-circuit: a 65-bit `LessThan` on `(deadline, tNow + W)`). Refund only
if `deadline < tNow`. Reduce `W` to a few blocks (for example 36 s).

### M-6 · The anonymity floor is only an Automator setting (Medium)

`MinSwaps` lives in the Go batcher. On-chain, a one-swap batch is valid, and its net flow **is** the user's size. A
malicious Automator, or anyone holding an opening, can isolate a user.

**Patch.** Add the constraint `Σ isSwap ≥ MIN_SWAPS` in-circuit. Allow `Σ isSwap = 0` only for refund-only batches.
Alternatively, expose `Σ isSwap` as a public output that the contract checks.

### M-7 · The zk-DECO query salt is a dilemma (Medium)

`queryCommitment = Poseidon(symPacked, salt)`.

* **Public salt:** a 6-byte ticker is brute-forced from any ticker list, so there is no privacy.
* **Secret salt:** only the creator can settle, which gives the creator an option to withhold an unfavourable
  settlement.

**Fix.** Escrow the salt with a threshold of keepers, or encrypt it to the settlement window with time-lock encryption.
Alternatively, drop the claim of query privacy.

### C-0 · Trusted setup (Critical, acknowledged)

The keys come from `circuits/scripts/setup-dev.mjs`: one contributor, random entropy passed on the command line, and a
2¹⁵ dev ptau. Whoever ran it can forge any JoinSplit or batch proof. The impact is bounded by the back-stops:

* forged `withdrawNote`s drain at most **all user custody**;
* batch extraction past `K` is impossible.

This is a **deployment blocker**: production needs a public phase-1 (for example Perpetual Powers of Tau) plus a
multi-party phase-2.

---

## 4. Mission 3 — HFT engine and `SearcherExecutor`

### Measurements (`bots/inventory-cpp/audit/bench.cpp`, Xeon 2.1 GHz, g++ 13)

| hot path | current | patched | speed-up | correctness |
|---|---|---|---|---|
| book update + `micro()` + `imbalance(5)` (2 M messages, uncrossed L2 feed) | 106–119 ns/msg (`std::map<double>`) | **29–31 ns** (flat tick ladder and occupancy bitmaps) | 3.6–3.9× | identical checksum |
| planner `plan()` | 4.5–9.8 µs (80 bisection + 200×2 `evalAt` = ~800 `sqrt`) | **26–27 ns** (closed form) | 165–373× | go/no-go agreement 20,000/20,000; **closed form is better in 5 cases, by up to 6.19 token1** (Q-3) |
| `encodeRebalance` | 210–250 ns (5 `std::string` allocations and concatenations) | **32–60 ns** (fixed 330 B buffer, byte lookup table) | 3.8–6.6× | byte-identical |
| variance estimator, 100 Hz feed | **+312 % bias** (tick EWMA of dp²/dt) | +14 % (1 s grid) | — | true σ² = 0.16/s |

### Q-1 · Strategic: until C-1 is fixed, the engine is in a race it cannot win (Critical for the business case)

* Competitors arbitrage through `settleBatch` and keep 100 %. `flashRebalance` keeps 20 %. No amount of nanoseconds
  closes a 5× gap.
* After P-1, all searchers compete for the same 20 %, which is decided by **inclusion**, not by CPU:
  * FCFS sequencers reward network distance to the sequencer;
  * priority-ordered L2s reward the priority fee.
* Spend the effort on (a) co-locating with the sequencer's RPC and feed, and (b) gas: every 10 k gas saved is
  priority-fee budget.

### Q-2 · `tick()` blocks on four synchronous RPC round trips (High)

`readOracle()`, `readInventory()`, `readPool()` and `preflight()` (an `eth_call`) run serially on every decision. That
is 4 × RTT: roughly 1–5 ms to a co-located node and 20–100 ms remote (estimated; there is no network in this
environment). That is **10⁴–10⁶ times** the whole compute path once the patches above are applied (≈ 100 ns).

**Patch (architecture):**

1. Subscribe over WebSocket to `newHeads`, the pool's `Sync`/`Rebalanced`/`BatchSettled` logs and oracle updates.
   Maintain `PoolState`, the oracle price and inventory **locally**; they only change through those events.
2. Replace `preflight` with an exact local simulation of the three rules in 256-bit integers (`chainRules` with U256 in
   place of doubles). Then sign and send immediately; an optional `eth_call` runs in parallel and only cancels.
3. Pin threads to cores: feed thread → SPSC ring → strategy thread → signer thread, no locks. Build with
   `-O3 -march=native -flto`.

### Q-3 · Bug: the planner's "bisection" overshoots and leaves profit behind (Medium, real money)

`plan()` starts by shrinking `[lo, hi]` until the probe at the near end is feasible:

```cpp
if (sell) lo = lo + (hi - lo) * 0.5;   // halves toward hi; stops at the FIRST feasible probe
```

This is not a bisection. It never moves back toward the true feasibility boundary, so it can land up to **half the
interval** inside the feasible region. The ternary search that follows is then confined to the wrong sub-interval.

In the benchmark's random states, whenever the real-reserve constraint (`out1 < r1`) binds near the oracle, the current
planner trades 4.5 token0 for 21.28 net, where the optimum is **8.52 token0 for 27.47 net (+29 %)**. So it leaves up to
23 % of the profit on the table, exactly when the opportunity is largest.

**Patch: the closed form.** The objective `plan()` maximises is concave. Let `φ = 0.8·(1 + margin)`, `a` the ask, `b`
the bid and `K = xy`.

Sell case (spot > P):

```
net(x_t) = (1−φ)(y − K/x_t) − (x_t − x)(a − φP) − gas
⇒ p* = (a − φP)/(1 − φ)
```

Buy case (spot < P):

```
net(y_t) = (x − K/y_t)(b − φP) − (y_t − y)(1 − φ) − gas
⇒ p* = (b − φP)/(1 − φ)
```

Clamp `p*` to the feasible interval, which is also closed-form:

* sell: `p ≥ K/(x+inv0)²` and `p > (y−r1)²/K`;
* buy: `p < K/(x−r0)²`, and `√p ≤ u⁺`, where `u⁺` is the positive root of
  `(1−φ)√K·u² − ((1−φ)y − φPx + inv1)·u − φP√K = 0`.

Because the objective is concave in `x_t` (or `y_t`), the clamped stationary point is the exact optimum. The
implementation is `planClosed()` in `audit/bench.cpp`: 26 ns, and never worse than the search (`net` differs by at most
3·10⁻¹⁰ in the search's favour, which is float noise).

With P-4, set `margin = 0`: the pool pulls the exact cut.

### Q-4 · Order book: `std::map<double,double>` (Medium)

The book uses a node allocation per new level and pointer chasing in `imbalance(n)`. It also keys on `double` prices,
where `2000.1` vs `2000.0999999` silently creates a second level. Use integer ticks in a contiguous ladder:

```cpp
std::array<double, W> bid_, ask_;  std::array<uint64_t, W/64> occB_, occA_;   // W = 16384 ticks, re-centre on exit
// best bid after deletion: prevSet() = word scan + 63 - countl_zero(word); next level for imbalance(): same
```

The result is 29–31 ns per message including `micro()` and `imbalance(5)`, with no heap allocation after start-up.

### Q-5 · `encodeRebalance` allocates five strings per order (Low)

Use a fixed `char[330]`, `memcpy` the selector and a 256-entry two-character hex table over the low 128 bits of each
word. The output is byte-identical; see `encodeRebalanceInto`. Better still, sign the raw ABI bytes and skip hex
entirely until the RPC layer.

### Q-6 · `VarianceEstimator` measures microstructure noise, not volatility (Medium)

The estimator takes `EWMA(dp²/dt)` per tick. With mid-quote bounce of variance `η²`:

```
E[dp²/dt] = σ² + 2η²/dt
```

So the bias grows with the feed rate: +27 % at 10 Hz and **+312 % at 100 Hz** in the benchmark. The Avellaneda-Stoikov
skew `q·γ·σ²·τ` is then 4× too strong, and the bot dumps inventory at the wrong prices.

**Patch:** sample on a fixed grid (1 s) as in the benchmark (+14 %), or use a two-scale or realised-kernel estimator
(Zhang, Mykland and Aït-Sahalia).

### Q-7 · The Avellaneda-Stoikov spread is applied to a taker (Medium, model risk)

* The `(2/γ)·ln(1 + γ/κ)` half-spread is the optimum for a **maker** posting limit orders that are filled at intensity
  `A·e^(−κδ)`. The bot here *takes*: it trades the pool against its hedge venue. The correct reference is the hedge
  venue's executable price for the size, including fees and expected slippage, for example
  `ask_eff = VWAP_ask(size)·(1 + takerFee)`. That feeds straight into `p*`.
* `KappaEstimator` uses `|trade − mid|`, which for touch trades is about half the spread. So `κ̂ ≈ 2/spread` describes
  the venue's tick size, not a fill-intensity decay.
* Keep the inventory skew `r = s − qγσ²τ`, which is the useful part of the model, and drop the spread term.

### Q-8 · `SearcherExecutor`: a cold key on a hot path, and no on-chain exactness (Medium)

* **Roles.** `operator` both trades and `sweep`s the whole inventory, so it has to be the MPC/TSS key. TSS signing
  costs 10–100+ ms per transaction. Split the roles:
  * `trader`: a hot key in the bot's memory that can only call `rebalance` (libsecp256k1 signs in about 30 µs);
  * `owner`: the cold MPC key that holds `sweep` and the right to rotate `trader`.
* **Exact repayment computed on-chain.** In the callback, compute the minimum repayment from `amountOut` and the pool's
  reserves, with `pay0 = ⌈K/(y − out1)⌉ − x`. This removes the off-chain `1e-9` and 5 bps margins and the risk of
  reverting on a stale state.
* **Pull model (P-4).** `approve(pool, treasuryPay)`, then call, then `approve(pool, 0)`. The Treasury cut is exact and
  capped. Implemented in the patch.
* **Gas.** `_inFlight` and the reentrancy guard are four `SSTORE`s per rebalance. On Cancun chains, use transient storage
  (EIP-1153), at 100 gas each. That saves about 20 k gas per rebalance, which becomes priority-fee budget. Note that
  `foundry.toml` pins `evm_version = paris`.

### Q-9 · Minor (Low)

`std::exp` and `std::tanh` run per update in `OfiTracker` and `VarianceEstimator`; precompute the decay factors for the
fixed grid. The `Searcher` holds the book behind a reference, with no snapshot discipline between the feed and decision
threads. Make the hand-off an SPSC queue of immutable top-of-book structs.

---

## 5. The patch set (`docs/AUDIT_PHASE8.patch`)

| patch | files | fixes | interface change | consumers to update |
|---|---|---|---|---|
| P-1 | DarkPool | C-1, H-4 (partly) | `error ValueLeak` | Automator: value-moving batches need a fresh oracle |
| P-3 | DarkPool | C-2 | `virtual0/1()` become views; `refShares` added | none (they are read per refresh) |
| P-4 | DarkPool, SearcherExecutor | H-1, Q-8 | the Treasury cut is **pulled** from `msg.sender` | searchers approve the pool; C++ `treasuryPay` becomes a cap |
| P-5 | NoteTree | H-3 | `treeId`, `isFinalRoot`, `event TreeRolled` | wallet (`engine.ts`) and Go `notetree` segment scans by tree |
| P-6 | NoteTree, DarkPool | H-2 | `ROOT_HISTORY = 1024`, one root per transaction | none |
| P-7 | DarkPool | L-1 | zero entries → `BadBatch` | none |
| P-8 | DarkPool | H-5 | `Batch.X`, `Batch.Y`; `StaleState` | Go `internal/chain/abi.go` encodes X, Y |
| P-H6 | SovereignBank | H-6 | `IYieldAdapter.recall` returns `uint256` (pull) | adapters approve and never push |
| P-M1 | SovereignBank | M-1 | pro-rata exit; `Withdrawn` reports the amount paid | none |
| P-M2 | OptionsMarket | M-2 | pro-rata pay-out; solvency check on inflows only | none |
| ZD-1 | zkdeco_oracle.circom | C-3 | canonical layout (new keys) | providers need a pinned template |
| ZK-P1 | batch_swap.circom, batch.lib.mjs | H-7 | nullifier key = `destBlinding` (new keys) | Go `intent` and `batcher` nullifier formula |

The patch also adapts the existing tests to the new semantics, without weakening any of them:

* `_batch()` supplies `X, Y` and a fresh oracle;
* the mock searcher approves the pool;
* two `flashRebalance` tests now assert the exact pulled cut;
* the K-monotonicity fuzzer also asserts `ΔV_P ≥ 0`, and its domain is restricted to prices the oracle can represent
  (`≤ MAX_PRICE`);
* the E2E proof-binding test now asserts validity inside the window and `StaleState` after it;
* the exploit tests are re-targeted at the patched interfaces, so that each one fails *on its guard*.

**Verification on a fresh copy:**

* `forge test` passes **92 of 92**.
* `FOUNDRY_TEST=audit forge test` makes the **12 exploits fail** on their guards: `ValueLeak`, `StaleOracle`, `BadBatch`,
  the pull, rollover, the window, the haircut and the pull-recall. `P3Check` and the gas test pass.
* zk-DECO witness checks: 8 of 8 as expected. `batch_swap`: 38 of 38.

**Gas impact.** `settleBatch` (4 intents, real proof) goes from 3,056,697 to **2,956,871 (−3.3 %)**. `transferNote` goes
from 1,625,492 to **1,581,175 (−2.7 %)**.

**Order of deployment:**

1. P-1 + P-4 + P-3: these stop the bleeding.
2. P-6 + P-5 + P-7: liveness and freeze.
3. P-8 + the Go ABI.
4. The SovereignBank and OptionsMarket patches.
5. New circuits (ZD-1, ZK-P1, M-5, M-6, P-9) **with** a real multi-party ceremony (C-0).

---

## 6. Limits of this audit

* **Out of scope:** StrategyVault, Berrycoin specifics beyond the shared NoteTree, SyntheticRWA beyond its use of the
  oracle, Go Automator code beyond batching and ABI, frontend and web security. I-2 applies to them all: most have no
  tests.
* **Single-wire scan only.** The under-constraint scan is exact for single-wire changes. Multi-wire semantic attacks
  (such as C-3) were searched by hand, guided by the statement each circuit claims.
* **Estimates.** RPC latency figures are estimates. All CPU figures were measured on a shared VM, and run-to-run
  variation is shown as ranges.
* **Gas costs.** Gas prices used for attack costs are illustrative. The gas *amounts* were measured with real Poseidon
  and a real verifier.
