# hft-engine-rust

StrawberrySwap's market-making execution engine, **iteration 1**. On a 100 ms tokio loop, the engine does five things:

1. It quotes with the Avellaneda–Stoikov closed forms of the Python lab (`hft-engine/python-sim`).
2. It sizes a DarkPool rebalance with the C++ engine's curve planner, ported bit for bit.
3. It builds the unsigned EIP-1559 `SearcherExecutor.rebalance` transaction.
4. It has the transaction signed by the **threshold vault** (`strawberryswap/vaults/threshold-signer`) over its Unix
   socket.
5. It checks the signed transaction, then broadcasts it.

**The engine holds no key and links no signer crate.** A signature that does not come back within its deadline, or
that does not check out, means the rebalance is withdrawn: nothing is broadcast for it.

The chain is still a **mock**: the C++ engine's SimChain, behind a real transaction boundary. It accepts only signed
EIP-1559 transactions, recovers the sender and enforces the operator. The vault is the real daemon.

```bash
# the vault (a mock in-memory key; see its README)
cargo run --manifest-path ../../vaults/threshold-signer/Cargo.toml -- --chain-id 31337 \
    --executor 0x5FbDB2315678afecb367f032d93F642f64180aa3
# the engine, on the vault's default socket
cargo run --release -- --ticks 300 --log-txs

cargo test                                  # 39 unit tests, plus 4 end-to-end tests that run both binaries
```

| flag | meaning |
|---|---|
| `--vault-socket PATH` | the vault's socket. Default: the vault's own default, `$XDG_RUNTIME_DIR/strawberryswap-vault/vault.sock` |
| `--vault-timeout-ms MS` | deadline of each signature, from connecting to the checked reply. Default 15 |
| `--chain-id ID` | chain id of the transactions. Default 31337 |
| `--executor ADDRESS` | the SearcherExecutor. Default `0x5FbDB2315678afecb367f032d93F642f64180aa3` |
| `--target-token0 TOKENS` | the executor's token0 target; the model's inventory q is the balance minus it. Default 20000 |
| `--calibration PATH` | the `engine` block of the lab's `calibration.json`. Default: mock parameters |
| `--ticks N` | stop after N ticks. Default: run until Ctrl-C or SIGTERM |
| `--mid PRICE` | starting price of the mock chain. Default 2 |
| `--seed N` | seed of the mock chain. Default 7; a run is reproducible for a seed |
| `--log-txs` | print every broadcast transaction in full (EIP-2718 hex) |

`--lot` is gone. The planner sizes each rebalance from the pool, the quotes and the balances. γ is still per token0 of
inventory, so the calibration stays the one made with 1-token lots (γ = 0.0041 in the lab's last run). It is loaded
with the σ², κ and τ of the same run, never on its own.

## One tick

```text
chain.read() ──► fresh oracle? ──► A-S quote r ∓ δ/2 ──► planner ──► Rebalance (wei) ──► unsigned EIP-1559 tx
   80 ms           45 s / 5 s       q = balance − target    C++ port     rounded for the pool        nonce from chain
                                                                                                         │
       broadcast ◄── checked: same tx, signed by the vault's address ◄── vault, over its Unix socket ◄────┘
                                                                          (deadline; else WITHDRAWN)
```

1. **Read** the oracle, the executor's balances, the pool's reserves and the operator's nonce, within 80 ms. A late or
   failed read skips the tick.
2. **Freshness:** reject an oracle reading older than 45 s or stamped more than 5 s ahead. `DarkPool.MAX_ORACLE_AGE`
   is 60 s.
3. **Quote:** r = s − q·γ·σ²·τ and δ = γ·σ²·τ + (2/γ)·ln(1 + γ/κ). The planner gets bid = r − δ/2 and ask = r + δ/2,
   as bots/inventory-cpp hands them to its own: no tick grid and no clamp to the mid. They are prices for trading
   against the pool, not resting orders.
4. **Plan** (`planner.rs`): the closed-form flash rebalance that moves the pool toward the oracle, if one pays its gas
   and the minimum net. Its feasibility bounds use the executor's balances, shrunk by a few parts per billion
   (`tx::spendable`) so that the rounded transaction still fits.
5. **Build** (`tx.rs`): amounts in wei, rounded in the pool's favour. The pool's payouts round down; the executor's
   repayments round up, with a 1e-9 relative margin; the Treasury allowance rounds up. Then the `rebalance` calldata
   (selector `0xc0661737`) and the EIP-1559 signing payload `0x02 ‖ rlp(fields)`.
6. **Sign** (`vault.rs`): one NDJSON request on the vault's socket, within `--vault-timeout-ms`. The reply must decode
   to exactly the requested transaction, and its signature must recover to the vault's address. A late, refused or
   mismatched reply **withdraws** the rebalance. Nothing is retried blind: the next tick plans afresh on new reads.
7. **Broadcast** the checked bytes, within 80 ms.

Missed ticks are skipped, not bunched. Repeated identical notes (the same skip or withdrawal reason) are printed once.
On Ctrl-C or SIGTERM the engine prints `summary ticks=… sent=… withdrawn=…` and exits 0.

**The vault client** keeps one connection and checks each reply's id. The connection is taken out for the exchange and
put back only when the stream is in step, so a call that fails, times out or is cancelled half-way leaves no unread
reply behind. A kept connection that the vault closed in the meantime (it closes connections idle for 30 s) is
replaced within the same call. Without a vault at start, the engine runs anyway and withdraws every rebalance until
the vault answers.

## The planner

`planner.rs` ports `bots/inventory-cpp/include/strawberry/curve.hpp` operation for operation: the same formula order,
and `std::max`, `std::min` and `std::clamp` with their exact NaN behaviour. It covers:

* the target price p* = (ask − φP)/(1 − φ) when selling token0, or (bid − φP)/(1 − φ) when buying it, with φ the 80 %
  extraction and a 5 bps Treasury margin;
* the feasibility bounds;
* `chainRules`, DarkPool's acceptance rules:
  * I1: K does not decrease;
  * I3a: the pool moves toward the oracle;
  * I3b: the Treasury allowance covers 80 % of the harvest.

`scripts/gen_planner_golden.cpp` compiles the C++ header with `-ffp-contract=off`, so there is no FMA and every
operation rounds once, as in Rust. It writes `src/planner_golden.rs`: 300 random cases and 11 edge cases, of which 172
are plans (90 sells, 82 buys). Every double is printed in its shortest round-trip form.

## The mock chain

`chain::MockChain` is the C++ SimChain (`sim.hpp`) behind a transaction boundary.

**Broadcast.** `send_raw_transaction` checks before inclusion:

* the bytes decode as an EIP-1559 transaction;
* on this chain id;
* the signer recovers;
* at the account's nonce.

Anything else is rejected before inclusion. An included transaction consumes its nonce. It then reverts if it is not
`rebalance` on the executor, carries value, comes from someone other than the executor's operator (`NotOperator`), or
fails the executor's balance check or DarkPool's three rules. The operator is the vault's address, fetched at start.

**The world, each tick:**

* the oracle steps by σ√(0.1 s);
* with a 30 % chance, a user batch hits the curve: noise of 0.1 % of the pool's token0, plus informed flow that
  trades 5 % of the way back to the oracle.

Without the informed part, the pool's deviation is a random walk that no finite inventory can absorb. With it, the pool
strays about 0.5 % from the oracle, the engine sends a rebalance every few seconds, and its inventory stays within ±30 %
of target.

## How the tests show that the engine never holds a key

`tests/vault_e2e.rs` runs the engine binary and the real vault binary as child processes, on a temporary socket. The
vault is built with its debug-only `insecure-test-key` feature. That gives it a key the test knows: keccak256 of a
public phrase, whose address viem computed independently. The key file is deleted as soon as the vault listens.

| test | shows |
|---|---|
| `the_engine_trades_through_the_vault_and_never_holds_its_key` | **Signatures:** five rebalances go out, each decoded and recovered by the test on its own, independently of the engine's checks. Each is signed by the vault's address, goes to the executor with nonces 0 to 4, and executes on the mock chain. Every transaction the engine broadcast is in the vault's audit log. **Memory:** while the engine trades, the test reads all of its readable memory through `/proc/<pid>/mem` (31 mappings, 6.5 MiB, 0 bytes unreadable). It looks for the key as big- and little-endian bytes and as lower- and upper-case hex, and finds **none**. The same scan finds the key in the vault, as k256's little-endian limbs, so the scan works. |
| `without_the_vault_nothing_is_broadcast` | With no vault, 200 ticks: every rebalance is withdrawn and nothing is sent. |
| `a_vault_that_refuses_the_transactions_gets_them_withdrawn` | A vault whose policy refuses the engine's fee: every rebalance is withdrawn, and the refusal is in the vault's audit log. |
| `no_crate_that_can_hold_a_wallet_key_is_in_the_engines_graph` | The engine's `Cargo.lock` has no local, hardware or cloud signer, keystore or mnemonic crate. The same parser finds `alloy-signer-local` in the vault's lock. |

**Mutation check.** I ran a build of the engine that keeps the key in memory. The memory test fails with `the key is in
the engine's memory as {"big-endian bytes"}`.

The test reads the two processes' memory as their parent, which Yama's default `ptrace_scope` of 1 allows. CI builds the
vault once and passes it in `THRESHOLD_SIGNER_BIN`; without it, the test builds the vault with cargo.

## Verification

| check | result |
|---|---|
| Planner vs the C++ engine | 311 golden rows, 8086 values: every field is bit-identical (0 ulps), and the rule checks match exactly. On 40,000 random pools, every plan has a positive net, fits the inventory, passes the three rules, and stops at or before the oracle. On the C++ self-test's pool, the closed form's net is at least the best of a 10,000-point grid. |
| A-S vs the Python lab | r, bid, ask and δ: 62 cases, all bit-identical but one δ, 1 ulp apart. There numpy's AVX-512 `log1p` is 0.546 ulp from the exact value; glibc's, which Rust calls, is correctly rounded. |
| Wei rounding | `to_wei` is exact: the double's mantissa times 10¹⁸, shifted in 256 bits. Checked on 100,000 balances from 1e-9 to 1e15 tokens: a plan on the spendable balance always fits it. |
| The vault's signature | `fixtures::SIGNED_REBALANCE` is the C++ end-to-end scenario's rebalance, signed by the real vault. viem recovers the vault's address from it. Signing in process with k256 produces the same bytes (RFC 6979). |
| Vault client | Accepts our transaction signed by the vault. Rejects a transaction we did not ask for, or one signed by another address. A missing, silent, closing or flaky vault fails within the deadline. A cancelled call does not desynchronise the next one, and a connection closed between calls is replaced within the call. |
| Loop, on paused virtual time | Ticks exactly every 100 ms. A 5,000-tick run: every rebalance is signed, included and accepted, with 0 reverts. The pool stays within 1 % of the oracle on average. The inventory stays in [10k, 30k]. A late, refusing or silent vault withdraws every rebalance, sends nothing and keeps the pace. Quoting resumes when the vault recovers. Runs are reproducible for a seed. |
| Memory | `#![forbid(unsafe_code)]`. valgrind memcheck shows 0 errors on the release binary trading against the vault (150 ticks) and on the unit tests. Nothing is definitely or indirectly lost. The only block reported is 48 bytes "possibly lost": std's handle for the main thread, allocated once for the life of the process. |
| Lint | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` |

To regenerate:

* **Planner goldens:** use the `g++` command at the top of `scripts/gen_planner_golden.cpp`.
* **Lab goldens:** `python3 scripts/gen_lab_golden.py --lab ../../hft-engine/python-sim`.
* **Signed fixture:** `cargo test print_the_fixture -- --ignored --nocapture`. Then have the vault, built with the
  test key, sign the printed payload.

## Dependencies

* **tokio 1.54.1:** a current-thread runtime, so the loop is one task and its state needs no locks. `net` and
  `io-util` serve the vault's socket.
* **alloy 2.5.0, without default features:**
  * `consensus` and `eips`: the transaction, its signing payload and the decoding of the vault's reply;
  * `k256`: signature recovery;
  * the HTTP provider over rustls and `sol!` bindings, for the live chain.

  **No signer feature** (`signer-local`, keystores, mnemonics).
* **Dev-only:**
  * `k256`, already in the graph: the loop tests' in-process stand-in for the vault;
  * `memchr`, already in the graph: the memory scan.

  A dev-dependency is never linked into the binary.
* Release builds keep integer-overflow checks.

## Next: the live chain

1. **`Chain::read`:** one batched `eth_call` through an alloy provider:
   * `IZKOracle.latestVerifiedPrice()`;
   * `balanceOf` for both tokens;
   * DarkPool's reserves;
   * the operator's pending nonce.

   Subscribe to new heads over WebSocket instead of polling.
2. **Before signing:** a preflight `eth_call` of the rebalance from the operator. Check once at start that the vault's
   address is `SearcherExecutor.operator()`.
3. **`send_raw_transaction`:** `eth_sendRawTransaction`, with nonce and replacement management for transactions that
   are pending.
4. **The vault:** the threshold (MPC/TSS) backend behind the same socket protocol. The engine does not change.
5. **Estimation and guards:**
   * estimate σ² and κ online (the C++ `VarianceEstimator` and `KappaEstimator`);
   * add the oracle-divergence guard once a second price source exists.
