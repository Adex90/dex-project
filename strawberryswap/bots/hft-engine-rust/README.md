# hft-engine-rust

StrawberrySwap's market-making execution engine, **iteration 0**: the Avellaneda–Stoikov closed forms of the Python lab
(`hft-engine/python-sim`), ported to Rust, on a 100 ms tokio loop that reads **mock** data sources. Nothing is read from
or sent to a chain yet. This iteration exists to verify the arithmetic and the loop before they are wired to the live
contracts.

```bash
cargo test                                  # 19 tests, offline
cargo run --release -- --ticks 50           # 5 s of quoting against the mocks
cargo run --release -- --calibration ../../hft-engine/python-sim/results/<run>/calibration.json --mid 100 --lot 1
```

| flag | meaning |
|---|---|
| `--calibration PATH` | the `engine` block of a `calibration.json` written by the lab's `run_sim.py`; default: mock parameters |
| `--ticks N` | stop after N ticks (default: run until Ctrl-C) |
| `--mid PRICE` | starting mid of the mock oracle (default 2) |
| `--lot TOKENS` | quote size in token0, the cap is ten lots (default 100) |
| `--seed N` | seed of the mocks (default 7); a run is reproducible for a seed |

## Model

```text
reservation price      r = s − q·γ·σ²·τ
optimal total spread   δ = γ·σ²·τ + (2/γ)·ln(1 + γ/κ)
quotes                 bid = min(r − δ/2, s),  ask = max(r + δ/2, s),  rounded outward to the tick
```

| symbol | unit | source |
|---|---|---|
| s | token1 per token0 | the zk-attested oracle, `IZKOracle.latestVerifiedPrice()`, 18-decimal fixed point |
| q | token0, signed | the executor's token0 balance minus its target |
| σ² | price² per second | `sigma2PerSec` |
| τ | seconds | `horizonSec`: a rolling horizon, re-armed every tick (as in `bots/inventory-cpp`) |
| γ | 1 / (price · token0) | `gamma` |
| κ | 1 / price | `kappa` |

γ is per token0 of inventory, as the lab's simulator applies it. Run the engine with the lot the calibration was made
with (the lab's `--lot`, 1 by default). A 100-token lot with a γ calibrated on 1-token lots skews r a hundred times
harder per fill.

## One tick

1. Read the oracle and the executor's inventory concurrently, within an 80 ms budget.
2. Reject an oracle reading older than 45 s or stamped more than 5 s ahead. `DarkPool.MAX_ORACLE_AGE` is 60 s, so this
   is the C++ engine's margin.
3. Convert from 18-decimal fixed point to `f64` with one correct rounding (the exact decimal expansion is parsed).
4. Compute r and δ, then bid and ask, which never cross the mid.
5. Round outward to the tick (bid down, ask up, with the lab's 1e-9-tick tolerance). On chain, a quote is an exact
   multiple of the tick (`U256`).
6. Apply the engine's guards:
   * No quote on the side whose fill would breach the inventory cap (the lab's `--max-inventory`).
   * No bid below one tick.
   * A non-finite result is an error, never a quote.
   * A bid and an ask rounded onto the same tick are moved one tick apart, on the side the inventory wants to trade
     less.
7. Hand the quote to the venue. A late or failed read, a stale oracle or a model error stands the quote down instead of
   leaving a stale one.

Missed ticks are skipped, not bunched. Ctrl-C or `--ticks` stands the quote down and exits.

**Mocks.** `fetch_oracle_price()` returns `(price, updatedAt)` like the contract call. Its price follows dS = σ dW with
the calibrated σ², after a 2 ms simulated round trip. `fetch_onchain_inventory()` is the executor's token0 balance. The
quote handed over rests, and each side fills one lot with the lab's probability 1 − exp(−A·e^(−κδ)·dt), with
A = 0.5 per second.

## Verification

| check | result |
|---|---|
| Golden vectors from the lab's own `model.py` (numpy 2.5.4) | `fill_probability`: 48 cases, all bit-identical. r, bid, ask and δ: 62 cases, all bit-identical but one δ, 1 ulp apart. There numpy's AVX-512 `log1p` is 0.546 ulp from the exact value; glibc's, which Rust calls, is correctly rounded |
| Precision | `ln_1p` keeps the spread exact when γ/κ < 2⁻⁵³, where ln(1 + x) returns 0. The tick tolerance absorbs 0.3 / 0.0001 = 2999.9999999999995. Fixed point ↔ f64 is correctly rounded. serde_json runs with `float_roundtrip` (its default parser read the lab's σ² one ulp off) |
| Loop, on paused virtual time | ticks exactly every 100 ms. Late (> 80 ms) or failed reads and stale oracles stand the quote down, and quoting resumes. Shutdown stands it down. A 20,000-tick run checks every quote and inventory (tick grid, mid never crossed, bid < ask, cap) |
| Memory | `#![forbid(unsafe_code)]`. valgrind memcheck on the binary and on the test suite: 0 errors, 0 bytes definitely or indirectly lost |
| Lint | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` |

The slowest tick of a release run is about 3.3 ms, of which 2 ms is the mocks' simulated round trip.

To regenerate the golden vectors, run `python3 scripts/gen_lab_golden.py --lab ../../hft-engine/python-sim` with the
lab's requirements installed.

## Dependencies

* **tokio 1.54.1:** a current-thread runtime, so the loop is one task and its state needs no locks.
* **alloy 2.5.0, without default features:** the HTTP provider over rustls, `sol!` contract bindings and the RPC types,
  for the next iteration. It has no local signer: signing will go through the threshold vault (`mpc/tss-vault`), never
  a key in this process.
* Release builds keep integer-overflow checks.

## Next: wiring to the chain

1. **`Oracle`:** `IZKOracle(darkPool.oracle()).latestVerifiedPrice()` through an alloy provider. Subscribe to new heads
   over WebSocket instead of polling.
2. **Inventory:** `fetch_onchain_inventory` reads `IERC20(token0).balanceOf(executor)`.
3. **`submit_quotes`:** in StrawberrySwap the quotes are not resting orders. As in `bots/inventory-cpp`:
   * the bid and the ask feed the curve planner, a closed-form flash rebalance of the DarkPool toward the oracle;
   * then a preflight `eth_call`, then `SearcherExecutor.rebalance`, signed through the threshold vault.
4. **Estimation and guards:**
   * Estimate σ² and κ online (the C++ `VarianceEstimator` and `KappaEstimator`), seeded from the calibration.
   * Add the oracle-divergence guard once a second price source exists.
