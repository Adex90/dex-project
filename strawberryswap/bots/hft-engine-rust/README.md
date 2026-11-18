# hft-engine-rust

StrawberrySwap's market-making execution engine, **iteration 2: the live chain**. On a 100 ms tokio loop, the engine:

1. **Reads** the chain in one batched RPC request: the zk oracle, the pool, the executor's inventory, the block, the
   operator's nonces and gas balance, and the receipt of the transaction in flight.
2. **Quotes** with the Avellaneda–Stoikov closed forms of the Python lab (`hft-engine/python-sim`).
3. **Sizes** a DarkPool rebalance with the C++ engine's curve planner, ported bit for bit, and builds the unsigned
   EIP-1559 `SearcherExecutor.rebalance` transaction.
4. **Simulates** it (`eth_call` from the vault's address): a rebalance a competing searcher already made unprofitable
   is dropped before any signature is asked for.
5. **Has it signed** by the **threshold vault** (`strawberryswap/vaults/threshold-signer`) over its Unix socket.
6. **Checks** the signed transaction, then broadcasts it: one transaction in flight at a time.

**The engine holds no key and links no signer crate.** A signature that does not come back within its deadline, or
that does not check out, means the rebalance is withdrawn: nothing is broadcast for it.

With `--rpc-url` the chain is live. Without it, the chain is a **mock**: the C++ engine's SimChain, behind a real
transaction boundary. It accepts only signed EIP-1559 transactions, recovers the sender and enforces the operator.

```bash
# the operator vault: the operator key in AWS KMS, rebalance only (see its README)
cargo run --release --manifest-path ../../vaults/threshold-signer/Cargo.toml -- --role operator --chain-id 421614 \
  --rpc-url <RPC> --kms-key-id <operator key ARN> --executor 0x<SearcherExecutor> \
  --max-fee-eth-per-window 0.5 --max-volume0-per-window 50e18 --max-volume1-per-window 175000e18
# the engine, live (the RPC URL is never printed: it may hold an API key)
cargo run --release -- --rpc-url <RPC> --chain-id 421614 --executor 0x<SearcherExecutor> --feed 0x<ZkPriceFeed> --json
# or on the mock chain
cargo run --release -- --ticks 300 --log-txs

cargo test                    # 57 unit tests and 4 end-to-end tests with the vault binary
cargo test --test live_e2e -- --ignored   # the live end to end: anvil, contracts, oracle pipeline, two vaults (CI runs it)
```

| flag | meaning |
|---|---|
| `--rpc-url URL` | the chain's JSON-RPC: live mode. Never printed, and redacted from every error |
| `--feed ADDRESS` | live: the ZkPriceFeed the pool must read, checked at start |
| `--vault-socket PATH` | the operator vault's socket. Default: the vault's own default, `$XDG_RUNTIME_DIR/strawberryswap-vault/operator.sock` |
| `--vault-timeout-ms MS` | deadline of each signature, from connecting to the checked reply. Default 80, the tick's budget: a KMS signature (10 to 20 ms in-region) and, for a new nonce, the vault's chain read. Longer slows the loop when the vault is slow |
| `--chain-id ID` | chain id of the transactions. Default 31337 |
| `--executor ADDRESS` | the SearcherExecutor. Default `0x5FbDB2315678afecb367f032d93F642f64180aa3` |
| `--target-token0 TOKENS` | the executor's token0 target; the model's inventory q is the balance minus it. Default 20000 |
| `--calibration PATH` | the `engine` block of the lab's `calibration.json`. Default: mock parameters |
| `--gas-limit GAS` | a rebalance's gas limit. Default 300000 |
| `--max-fee-wei WEI`, `--priority-fee-wei WEI` | the fee caps. Defaults 30 gwei and 1 gwei. With the gas limit, they must be within the vault's policy |
| `--gas-cost TOKENS1`, `--min-net TOKENS1` | the planner's cost of a rebalance's gas, and the least net profit worth one. Defaults 0.01 |
| `--ticks N` | stop after N ticks. Default: run until Ctrl-C or SIGTERM |
| `--json` | events, alerts and metrics as JSON lines |
| `--metrics-every TICKS` | a metrics snapshot every TICKS ticks. Default 100 (10 s); 0 for none |
| `--mid PRICE` | mock: starting price. Default 2 |
| `--seed N` | mock: seed. Default 7; a run is reproducible for a seed |
| `--log-txs` | print every broadcast transaction in full (EIP-2718 hex) |

`--lot` is gone. The planner sizes each rebalance from the pool, the quotes and the balances. γ is still per token0 of
inventory, so the calibration stays the one made with 1-token lots (γ = 0.0041 in the lab's last run). It is loaded
with the σ², κ and τ of the same run, never on its own.

## One tick

```text
read (one batch) ──► in flight? ──► fresh oracle? ──► A-S quote ──► planner ──► unsigned EIP-1559 tx
   80 ms            wait for its     45 s / 5 s       r ∓ δ/2       C++ port     next nonce, capped fees
                    receipt                                                          │
                                                                                     ▼
broadcast ◄── checked: same tx, signed by the vault ◄── vault (Unix socket) ◄── simulate (eth_call from the vault)
(one in flight)                                         deadline; else WITHDRAWN    reverts? BEATEN, never signed
```

1. **Read** the chain in one batched request, within 80 ms. A late or failed read skips the tick.
2. **The transaction in flight.** While one waits for its receipt, nothing else is planned. It is settled by:
   * its receipt (`MINED`);
   * another transaction spending its nonce (`nonce_spent`);
   * the node not knowing it 3 s after the broadcast (`tx_dropped`: the nonce is reused).

   The next nonce never goes below what the engine knows it spent, even when the node lags. A pending transaction from
   the operator that the engine did not send holds the engine.
3. **Fees:** a base fee above `--max-fee-wei` holds the engine; its transaction would wait unmined.
4. **Freshness:** reject an oracle reading older than 45 s or stamped more than 5 s ahead. `DarkPool.MAX_ORACLE_AGE`
   is 60 s.
5. **Quote:** r = s − q·γ·σ²·τ and δ = γ·σ²·τ + (2/γ)·ln(1 + γ/κ). The planner gets bid = r − δ/2 and ask = r + δ/2,
   as bots/inventory-cpp hands them to its own: no tick grid and no clamp to the mid. They are prices for trading
   against the pool, not resting orders.
6. **Plan** (`planner.rs`): the closed-form flash rebalance that moves the pool toward the oracle, if one pays its gas
   and the minimum net. Its feasibility bounds use the executor's balances, shrunk by a few parts per billion
   (`tx::spendable`) so that the rounded transaction still fits.
7. **Build** (`tx.rs`): amounts in wei, rounded in the pool's favour. The pool's payouts round down; the executor's
   repayments round up, with a 1e-9 relative margin; the Treasury allowance rounds up. Then the `rebalance` calldata
   (selector `0xc0661737`) and the EIP-1559 signing payload `0x02 ‖ rlp(fields)`.
8. **Simulate** the transaction as an `eth_call` from the vault's address on the latest block. If it would revert —
   a competing searcher moved the pool first, or the plan went stale — it is dropped as `BEATEN`, with DarkPool's or
   the executor's error decoded. The vault is never asked, and no gas is spent.
9. **Sign** (`vault.rs`): one NDJSON request on the vault's socket, within `--vault-timeout-ms`. The reply must decode
   to exactly the requested transaction, and its signature must recover to the vault's address. A late, refused or
   mismatched reply **withdraws** the rebalance. Nothing is retried blind: the next tick plans afresh on new reads.
10. **Broadcast** the checked bytes, within 80 ms. It is now the transaction in flight. A broadcast that gets no answer
    in time is tracked all the same: its receipt, its nonce, or the node not knowing it settles it.

Missed ticks are skipped, not bunched. Repeated identical notes (the same skip or withdrawal reason) are printed once.
On Ctrl-C or SIGTERM the engine prints `summary ticks=… sent=… mined=… …` and exits 0.

**The vault client** keeps one connection and checks each reply's id. The connection is taken out for the exchange and
put back only when the stream is in step, so a call that fails, times out or is cancelled half-way leaves no unread
reply behind. A kept connection that the vault closed in the meantime (it closes connections idle for 30 s) is
replaced within the same call. Without a vault at start, the engine runs anyway and withdraws every rebalance until
the vault answers.

## The live chain (`live.rs`)

**One batched request per tick.** The engine sends:

* EngineLens's creation code as an `eth_call` without a `to` (`contracts/src/periphery/EngineLens.sol`, never
  deployed). Its constructor reads, all from **one execution at one block**:
  * the oracle, through the pool as DarkPool reads it;
  * the pool's reserves and virtual reserves;
  * the executor's balances;
  * the block's number, timestamp and base fee.

  It returns them as the would-be code. A batch of separate `eth_call`s could straddle two blocks; Arbitrum makes one
  every 250 ms.
* the operator's nonce, at `latest` and at `pending`;
* the operator's ether balance;
* while a transaction is in flight, its receipt and `eth_getTransactionByHash`.

The lens's creation code is pinned in `src/lens.hex`, and CI checks the pin against the forge build.

**At start** the engine refuses to run unless all of these hold:

* the vault answers;
* the vault's `policy` is an **operator** vault's (a relayer's or an automator's vault holds another key and signs
  another function), and covers the engine's chain, executor, gas limit and max fee;
* the RPC's chain id is `--chain-id`;
* there is a contract at the executor, and its `operator()` is the vault's address (otherwise every rebalance would
  revert `NotOperator`);
* the executor and its pool trade the same tokens;
* with `--feed`, the pool reads that feed.

## Metrics and alerts (`metrics.rs`)

* **Events.** `SENT`, `MINED`, `BEATEN`, `WITHDRAWN`, `REJECTED`, `SKIPPED`, `ALERT` and `RESOLVED`, printed as text
  or, with `--json`, as JSON lines.
* **Metrics.** Every `--metrics-every` ticks, a snapshot of:
  * the counters;
  * latency histograms (mean, p50, p99, max) of the read, the simulation, the signature, the broadcast, the whole
    tick, and from broadcast to receipt;
  * gauges: block, base fee, oracle age, the pool's deviation from the oracle in bps, mid, inventory, balances,
    gas balance, nonces, the transaction in flight;
  * the active alerts.
* **Alerts.** Each is raised once and resolved once.

| alert | raised when |
|---|---|
| `rpc_down` | reads fail or miss their budget for 1 s (10 ticks) |
| `oracle_stale` | the oracle reading is too old to trade on for 1 s: the relayer is not pushing |
| `vault_down` | the vault fails to answer 10 times in a row |
| `fees_above_cap` | the base fee is above the max fee for 1 s |
| `tx_stuck` | a transaction has waited 30 s for its receipt |
| `foreign_transaction` | a transaction from the operator that the engine did not send is pending |
| `low_gas_balance` | the operator holds less than 10 rebalances' gas at the max fee |
| `tx_reverted`, `nonce_moved`, `nonce_spent`, `tx_dropped` | one-offs: what the names say |

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

**Broadcast.** `send_raw_transaction` mines at once, and the engine reads the receipt on its next tick, as it does
live. It checks before inclusion:

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
| `live_e2e::the_engine_rebalances_the_live_pool_through_the_vault` (CI, `--ignored`) | **The live path end to end on anvil, with two vaults (Phase 4).** The contracts are real: DarkPool, SearcherExecutor whose operator is the operator key, ZkPriceFeed, ZkDecoOracle and the dev verifier. Every key is in the stand-in KMS. The relayer pushes proven prices every 2 s through the relayer vault, and the engine signs through the operator vault. Each vault refuses the other's function, live. When the market moves 1.25 %, the engine rebalances the pool: the rebalance is mined from the operator key's address, the pool moves toward the oracle, the Treasury is paid, nothing reverts, and every broadcast was simulated first. The operator vault's window counts it and its nonce moves on. While the engine trades, the operator key is found in the KMS stand-in's memory (the positive control), and in neither the vault's nor the engine's. |

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
| Live adapter (alloy's mock transport, every request recorded) | A tick is exactly one batch: the lens call with its creation code and arguments, both nonces and the gas balance, plus the receipt and status of the transaction in flight. Each value is parsed where the engine expects it, and a short lens answer or a failed batch is an error, not a reading. The simulation's revert is decoded (`DeviationNotReduced`, `NotOperator`, a panic) and told apart from a node failure. The broadcast's errors never show the RPC URL's key. The start-up checks refuse a wrong chain, no contract, an operator other than the vault, and a pool reading another feed. |
| Loop, on a misbehaving node | A rebalance that would revert is never signed (no vault call, no transaction). One transaction in flight: consecutive nonces, each sent once, nothing new while one waits. A dropped transaction is forgotten after 3 s and its nonce reused. A foreign pending transaction holds the engine and raises `foreign_transaction`. A node a block behind on the nonce does not make the engine reuse one. Fees above the cap hold it and raise `fees_above_cap`, resolved when they fall. Late reads raise `rpc_down`. Events and metrics are structured. |
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
  * the HTTP provider over rustls, JSON-RPC batches and `sol!` bindings, for the live chain;
  * `json-rpc`: the revert data of a failed simulation.

  **No signer feature** (`signer-local`, keystores, mnemonics).
* **Dev-only:**
  * `k256`, already in the graph: the loop tests' in-process stand-in for the vault;
  * `memchr`, already in the graph: the memory scan;
  * `tower-service`, already in the graph: recording the requests alloy's mock transport answers.

  A dev-dependency is never linked into the binary.
* Release builds keep integer-overflow checks.

## Next

1. **Heads over WebSocket** instead of a read every 100 ms, where the RPC offers it.
2. **Stuck transactions.** Fee bumping (a replacement at the same nonce, re-signed by the vault). Today a stuck
   transaction raises `tx_stuck` and the engine waits.
3. **The vault:** the threshold (MPC/TSS) backend behind the same socket protocol. The engine does not change.
4. **Costs from the chain.** The planner's gas cost from the live base fee and the measured gas of a rebalance.
5. **Estimation and guards:**
   * estimate σ² and κ online (the C++ `VarianceEstimator` and `KappaEstimator`);
   * add the oracle-divergence guard once a second price source exists.
