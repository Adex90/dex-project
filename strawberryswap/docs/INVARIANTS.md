# Security invariants — statements, proofs, and how each is tested

Fuzzing and invariant campaigns give strong **evidence**; they are not proofs. For the two headline
invariants a short algebraic proof exists, so both are given here, and the Foundry suites then check that the
*code* matches the proof (and, via mutation testing, that the tests fail when it does not).

## I-SUPPLY  (Berrycoin)

> At every point in time: `Σ publicBalance[a] + shieldedSupply == MAX_SUPPLY`, and `MAX_SUPPLY` is constant.

**Proof (induction over state transitions).**
*Base:* the constructor sets `balanceOf[genesisHolder] = MAX_SUPPLY`, `shieldedSupply = 0`. There is no other
write to `balanceOf` that creates value: no `mint`, no `burn`, no owner (checked by
`test_noMintBurnOwnerOrUpgradeEntrypointsExist`).
*Step:* each external function either
1. `transfer/transferFrom` — moves `x` between two balances (`balance[from] -= x; balance[to] += x`): Σ unchanged;
2. `shield(x)` — `balance[caller] -= x; shieldedSupply += x`: Σ + shielded unchanged;
3. `unshield(x)` — requires `x <= shieldedSupply` **before and independently of** the proof check, then
   `shieldedSupply -= x; balance[recipient] += x`: Σ + shielded unchanged;
4. `transferZK` — touches only the Merkle tree and nullifier set, no balance: unchanged.
Checked arithmetic (Solidity 0.8) rules out overflow/underflow. ∎

**Consequence (A3).** The proof of step 3 never uses soundness of the SNARK. A verifier that accepts *every*
proof can steal what is shielded, but cannot create a single wei: `unshield` can release at most `shieldedSupply`.
Tested with an accept-all verifier (`BerrycoinInvariantTest`, 4 invariants × 256 runs × 64 calls, unshield
amounts drawn from `[1, 2·MAX_SUPPLY]`) and `testFuzz_forgedUnshieldCannotInflateSupply`.

## I-K  (DarkPool)

> For every accepted swap batch and every accepted `flashRebalance`: `K' = (R0'+v0)(R1'+v1) >= K = (R0+v0)(R1+v1)`.

**Proof for the quote.** Let `x, y` be effective reserves, `d` the amount in, and
`out = ⌊ y·d / (x+d) ⌋` (integer division = floor). Then `out <= y·d/(x+d)`, so
`y − out >= y − y·d/(x+d) = x·y/(x+d)`, hence `(x+d)(y−out) >= x·y`. ∎
Rounding is therefore always in the pool's favour; the trader's fractional remainder stays in the pool.

**For `settleBatch` / `flashRebalance`.** The contract does not trust the proposed amounts: it recomputes
`K'` from the new reserves and reverts if `K' < K` (`InvariantBroken`). So I-K holds for *any* aggregator or
searcher behaviour, with no assumption on the circuit. ∎

**Corollary (no slow extraction).** Any round trip `A→B→A` returns at most the starting amount, so rounding
cannot be farmed by repetition. Tested by `testFuzz_roundTripNeverProfits` and
`testFuzz_manyRoundTripsNeverExtractValue` (20 consecutive round trips per run).

**Scope.** `K` *does* legitimately change when liquidity is added or removed. For those operations the
invariant is *reserves-per-share never decreases* (`r'·ts >= r·ts'`, proved by the floor argument above and
checked as `invariant_noLiquidityDilution`).

## Other properties and where they are checked

| Property | Mechanism | Test |
|---|---|---|
| Treasury receives ≥ 80 % of harvest | `flashRebalance` rule I3b | `invariant_treasuryAlwaysPaid`, `test_flashRebalance_underpayingTreasuryReverts` |
| Rebalance must move toward the oracle | I3a | `test_flashRebalance_movingAwayFromOracleReverts` |
| Pool fully backed (balance ≥ reserve + custody) | `_checkBalances` | `invariant_poolIsFullyBacked` |
| A greedy batch (1 wei beyond the curve) is never accepted | I-K | `invariant_greedyBatchNeverAccepted` |
| Proofs bound to caller / recipient / amount / ciphertext | `extDataHash` | `JoinSplitIntegrationTest.test_*Bound*`, `test_ciphertextSwapRejected` |
| A forged proof cannot be replayed | nullifier set | `test_replayedProofReverts` |
| Malformed proof cannot burn unbounded gas | verifier gas cap | `testFuzz_malformedProofCannotBurnMoreThanGasCap` |

## Mutation testing (do the tests actually fail when the code is wrong?)

Each of these single-line mutations makes the suite fail (run and recorded during development):
M1 delete the `amount > shieldedSupply` check · M2 round swap quotes **up** · M3 delete the `K' < K` check ·
M4 lower `EXTRACTION_BPS` to 40 % · M5 remove the verifier gas cap.

## Findings made while testing (kept, not hidden)

1. **Unbounded gas burn on malformed proofs** — real, fixed: a failed EC precompile consumes all forwarded
   gas (≈1e9 in the fuzz run). Verifier calls now forward a fixed budget (`gasCap`).
2. **Donations lower the Treasury's share.** `harvest` is measured on post-call reserves, which absorb any
   donated surplus. Not exploitable (a donor gives up the full value to save 80 % of it —
   `test_donationLowersTreasury…`), but the Treasury's take is net of donations.
3. **Quotes vs real reserves.** `getAmountOut` prices against effective (real + virtual) reserves, so on a thin
   pool it can quote more than the *real* reserve; `settleBatch` then reverts. A liveness limit for
   aggregators, not a safety issue.
