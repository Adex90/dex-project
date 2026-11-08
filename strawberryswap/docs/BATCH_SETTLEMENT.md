# Private batch settlement — design, security argument, costs

Module 2 closes its privacy loop with three parts: the `batch_swap` circuit (`circuits/batch_swap`), the settlement
entry point `DarkPool.settleBatch`, and the off-chain `bots/automator` service that builds and proves batches.

## 1. The flow

```
user:   note ──JoinSplit(own key)──► INTENT NOTE   (owner = BATCH_PUBKEY, blinding = H(destPub, refundPub, destBlind, minOut, deadline))
user  ► Automator (over Tor): the intent's opening          (the Automator never gets a spending key)
Automator: pick feasible intents → uniform price → payouts → NET flow → witness → batch_swap proof
chain:  settleBatch(proof, nullifiers[], commitments[], net flows)   → K check, custody/reserve update
user:   scan NoteInserted, open the sealed output, JoinSplit(own key) → withdrawNote
```

### Why an intent note
A swap executed by a third party needs the user's authorisation, but a spend authorisation in this note model is the
private key itself. So the user first moves funds into a note whose owner key is `BATCH_PUBKEY`, a constant with no
known Poseidon pre-image: **no JoinSplit can spend it** (that circuit needs `Poseidon(privKey) = pubKey`). The only
circuit that can is `batch_swap`, which (a) pays `destPubKey` at least `minOut` before `deadline`, or (b) after the
deadline returns the full amount to `refundPubKey`. The terms are fixed because they are hashed into the note's
blinding. Consequence: **the Automator learns an intent but cannot spend, redirect, delay-and-keep, or alter it.**
It *can* choose when to settle it and with which other intents, including its own: within the user's `minOut` it can
sandwich the user (Phase-8 audit H-4). The on-chain value guard (§4, I5) rejects any batch that moves the pool toward
the oracle, so the unwind leg must go through `flashRebalance` and pay the Treasury's 80 %; that removes ~96 % of the
incentive, not all of it. Keep `minOut` tight and computed against the oracle; an oracle-relative bound in-circuit is the
complete fix (open, see §6).

Nullifier of an intent note is `Poseidon(commitment, leafIndex, destBlinding)`. `destBlinding` is bound inside the
commitment, so each note has exactly one nullifier (no double-spend path), and it is known only to the user and the
holder of the opening, so an observer cannot link a spent intent to the transaction that created it. (A public key
here made every slot linkable: audit H-7.)

## 2. The clearing price (closed form)
With effective reserves `X, Y`, gross token0 sold `A0` and token1 sold `A1`:

    q = (Y + A1) / (X + A0)

If token0-sellers receive `s·q` token1 and token1-sellers `s/q` token0 then
`X' = Y/q`, `Y' = q·X` and therefore **`X'·Y' = X·Y` exactly** (zero fee). In integers the circuit uses
`pLo = ⌊WAD·(Y+A1)/(X+A0)⌋` for token0-sellers and `pHi = pLo+1` for token1-sellers; every payout is a floor, so all
roundings go against the user by ≤ 1e-18 relative and **K' ≥ K always** (proved below, and re-checked on-chain). The
division and the price are verified with remainders and `rem < divisor` bounds, so the prover has **no freedom** in
the price: it is a function of public data (`X, Y` and the net flow).

**Proof of K' ≥ K.** Let `p = pLo/WAD ≤ q < pHi/WAD`. Payouts: `O1 ≤ A0·p ≤ A0·q`, `O0 ≤ A1/(pHi/WAD) < A1/q`.
`Y' = Y + A1 − O1 ≥ Y + A1 − A0·q = q·X` and `X' = X + A0 − O0 ≥ X + A0 − A1/q = Y/q`
(using `Y + A1 = q(X + A0)`). Hence `X'·Y' ≥ XY`. ∎

## 3. What is public, what is not
| public on-chain | hidden |
|---|---|
| root, `tNow`, `X`, `Y` | every individual trade size, direction, owner, destination, `minOut`, deadline |
| **net** flow per asset (`netIn/netOut`) | **gross** volumes `A0, A1, O0, O1` (only their difference leaks) |
| one opaque nullifier + one opaque output commitment per intent | the clearing price is *derivable* from `(X, Y, net)`, nothing more |
| the *number* of intents in the batch | which intents / who |

**Leaks that remain by arithmetic:** the net imbalance; and a batch with a single real swap reveals that swap's size
(the Automator enforces `minSwaps`, default 2 — a policy, not a cryptographic guarantee). Deposits and withdrawals
(`depositNote`/`withdrawNote`) reveal their amounts and caller.

## 4. Threat model for `settleBatch`
| attacker | tries | stopped by |
|---|---|---|
| malicious aggregator | inflate/deflate the net flow | proof (net equation) — tested `test_aggregatorCannotInflateNetFlow` |
| " | redirect a user's payout note | proof (output commitment) — `test_aggregatorCannotRedirectAnOutputNote` |
| " | skim 1 wei from a user (even with consistent remainders) | `rem < den` bound — circuit tests + mutation M7 |
| " | understate the price | `pRem < D` bound — circuit tests + mutation M8 |
| " | drop an intent but keep the net flow | net equation — circuit + `test_aggregatorCannotDropAnIntentAndKeepTheRest` |
| " | settle against an old pool state | `(X, Y)` travel with the batch and are accepted only if they were the pool's state within `STATE_WINDOW_BLOCKS` (64); K and I5 are checked against the CURRENT state — `test_proofIsBoundToPoolState`, `test_H5_*` |
| " / arbitrageur | arbitrage the pool through batches without paying the Treasury | I5: `(in0 − out0)·P + (in1 − out1) ≥ 0` at the fresh oracle price; refund-only batches are exempt — `test_C1_*` |
| " | settle while the oracle is stale | value-moving batches need a fresh oracle (`StaleOracle`); refunds never do |
| chain observer | link a batch slot to the wallet that created the intent | secret nullifier key — `circuits/test/batch.test.mjs` (H-7) |
| " | replay | nullifier set — `test_batchCannotBeReplayed` |
| " | execute after a deadline / refund early | in-circuit clock + contract window — circuit tests, `test_clockWindow` |
| broken verifier / bad circuit | create value | contract re-checks `K' ≥ K` and `balance ≥ reserve + custody` (I1, I2) |
| anyone | spend an intent note with JoinSplit | needs a pre-image of `BATCH_PUBKEY` (infeasible) |

## 5. Costs (measured)
Circuit constraints: N=2 **14,988** · N=4 **28,748** · N=8 **56,268** · N=16 **111,308** (≈6.9k per slot, of which
≈5k is the 20-level Poseidon Merkle path). Design choices that kept it down: closed-form price (computed once, shared by
all slots), one muxed division gadget per slot, no hashing of the public arrays (they are public inputs instead:
≈2N extra verifier inputs ≈ 7k gas each, versus ≈300k constraints for SHA-256 over `abi.encode(arrays)`).

Gas, N=4 real batch: SNARK verification **297k** · `settleBatch` total **2.96M** (3.05M before one root per
transaction). **The proof is not the bottleneck:** ≈2.66M is four on-chain Poseidon Merkle insertions (≈650k each;
`NoteTree` hashes 20 levels per leaf with the circomlib contract). Obvious next optimisation (not done): insert the batch's commitments as one aligned subtree
(compute the subtree root in the circuit, one insertion of `log2(N)`-shorter path), cutting this by roughly `N×`.

## 6. Not done / open
* No ceremony: keys in `test/fixtures` are single-party DEV keys.
* In-batch duplicate use of one intent note is rejected by the contract's nullifier set, not in-circuit.
* A batch carries at most one output per intent (no partial fills / change notes).
* `minSwaps` is an Automator policy, not an on-chain rule (audit M-6), and it can be gamed by an Automator padding
  batches with its own swaps; cover traffic is not implemented.
* Deadlines stretch by `BATCH_CLOCK_WINDOW` (300 s) and the prover's choice of `tNow` decides swap vs refund near the
  deadline (audit M-5).
* Price protection is the user's absolute `minOut`; an oracle-relative bound in-circuit (audit P-9) is not implemented.
* The transaction that creates an intent note is public; the wallet sends it from the user's own address. Relaying it
  is required for unlinkability of the deposit edge.
* An adverse state change while a proof is generated (a large withdrawal, a rebalance against the batch) still fails
  the current-state K check by design: an LP can still delay batches at the cost of leaving the pool (audit H-5).
