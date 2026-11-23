# The 72-hour testnet soak and its chaos drills

The soak runs the whole testnet stack on Arbitrum Sepolia for 72 hours:

* the contracts of `contracts/script/DeployTestnet.s.sol`;
* the price endpoint, the notary and the relayer pushing ZkPriceFeed every ~15 s;
* the three vaults signing with KMS;
* the HFT engine rebalancing DarkPool;
* the Go automator settling batches.

During the soak, five chaos drills disturb it on purpose. The soak passes on its **service levels** and on the
**drills**. Its telemetry is then compared with the **calibration lab** at the testnet's risk aversion, **γ = 0.0041**.

Everything here is testnet-only. The disclosure stands on chain (`TestnetDisclosure`) and on every page of the
frontend: a trusted bootstrap, dev-key verifiers, tokens without value.

| tool | what it does |
| --- | --- |
| `ops/soak/collect.sh` | follows the services' journals into `$SOAK_DIR` (default `/var/lib/strawberryswap/soak`) for the whole soak |
| `ops/soak/export_lab.py` | turns the engine's telemetry into the lab's inputs and judges the service levels |
| `ops/soak/compare.py` | reruns the lab on the soak's own price path at γ = 0.0041, and sets the live engine beside it |
| `ops/soak/rpc_failover.py` | the JSON-RPC failover proxy in front of the engine, and the RPC drill's switch |
| `ops/soak/drill_check.py` | judges a drill from the engine's events inside and after its window |
| `ops/drills/*.sh` | the five drills; each logs its window to `drills.jsonl`, so the service levels exclude it |

## 1. Before the soak (T − 1 h)

The deployment is `docs/TESTNET.md`. Before starting the clock, check each of these:

* [ ] `MANIFEST=deployments/421614.json forge script script/DeployTestnet.s.sol:CheckTestnet --rpc-url arbitrum_sepolia`
  passes: every wiring, the Poseidon vector, the disclosure.
* [ ] Every contract is source-verified on the explorer (`contracts/script/verify-testnet.sh`).
* [ ] The frontend's release is on IPFS (`ops/release/publish-ipfs.sh`), and its banner shows on every page.
* [ ] Each vault has started and named its role, its KMS key and its address. Each address matches the manifest:
  the operator is `SearcherExecutor.operator()`, and the notary is `ZkDecoOracle.notaryOf(providerId)`.
* [ ] The engine runs with these settings:
  * `--json --metrics-every 10 --log-txs`: a snapshot every second, and the raw bytes of each transaction, which the
    reorg recovery needs;
  * `--calibration /etc/strawberryswap/calibration.json`, whose `engine.gamma` is **0.0041** (§ 6);
  * `--rpc-url http://127.0.0.1:8547`, the failover proxy.
* [ ] The proxy runs as `rpc_failover.py --upstreams /etc/strawberryswap/rpc-upstreams --drills`:
  * the upstreams file is mode 0600 and lists at least two providers, in priority order, as `label URL` lines;
  * `curl -s 127.0.0.1:8547/status` shows every provider up.
* [ ] The operator, relayer and automator each hold gas for 72 h: at least 0.05 testnet ETH, and no
  `low_gas_balance` alert.
* [ ] No alert is active: `jq .alerts` of the engine's last snapshot is `[]`.
* [ ] `ops/soak/collect.sh start` is running, and `ops/soak/collect.sh status` counts snapshots.

## 2. The clock

| when | what | where |
| --- | --- | --- |
| T + 0 | `collect.sh start`; note the start in the soak log | soak host |
| every 6 h | `collect.sh export`: the service levels so far, and any FAIL is investigated | soak host |
| T + 6 h | `ops/drills/vault-down.sh` | soak host |
| T + 12 h | `ops/drills/stale-feed.sh` | soak host |
| T + 24 h | `ops/drills/rpc-failover.sh` | soak host |
| T + 30 h | `vault-down.sh` again if T + 6 h was INCONCLUSIVE | soak host |
| T + 36 h | `ops/drills/reorg.sh` | anvil rig (§ 5) |
| T + 48 h | `ops/drills/competing-searcher.sh` | anvil rig |
| T + 60 h | `stale-feed.sh` again, with the market in a different state | soak host |
| T + 72 h | `collect.sh stop`, then `export` and `compare.py` (§ 6); sign-off (§ 7) | soak host |

The service levels exclude each drill's window plus 5 minutes (`--drill-margin`). A drill disturbs the stack, so only
its own verdict judges that period.

## 3. Service levels (outside the drills)

`export_lab.py` judges these (`ops/soak/telemetry.py`, `SLO`):

| level | target | why |
| --- | --- | --- |
| coverage | ≥ 99.5 % of the soak with engine snapshots | the engine is up and its view is live |
| tick p99 | ≤ 100 ms | the engine's tick |
| chain read p99 | ≤ 80 ms | its read budget (one batched `eth_call`) |
| vault signature p99 | ≤ 80 ms | the vault deadline, KMS round trip included |
| oracle age p99 / max | ≤ 30 s / ≤ 45 s | the relayer's ~15 s cadence; the engine stops at 45 s, DarkPool at 60 s |
| reverted on chain | 0 | every send is simulated first |
| rejected by the node | 0 | fees within caps, nonce in sequence |
| `foreign_transaction`, `nonce_moved`, `nonce_spent`, `tx_stuck`, `low_gas_balance` | none | each means a person must look |

Also reported, without a target:

* counters (sent, mined, beaten, withdrawn);
* latencies per stage;
* the pool's deviation from the oracle (p50 and p99);
* gas spent;
* every alert episode with its duration.

**Abort** (stop the engine and the automator, keep collecting, investigate) on any of these:

* `foreign_transaction` or a vault `halted` line: a nonce was spent by something no vault signed;
* `nonce_spent` or `tx_dropped` more than once;
* a KMS `AccessDenied`;
* gas below one day's spending.

## 4. The drills on the soak host

Each drill script works the same way:

* it logs its window to `drills.jsonl`;
* it injects the fault, and always removes it, even on an error;
* it waits for the recovery, then judges with `drill_check.py`. The verdict is printed and appended to
  `drill-results.jsonl`.

Exit codes are 0 for PASS, 1 for FAIL, 2 for a setup error and 3 for INCONCLUSIVE.

### Vault down: `ops/drills/vault-down.sh` (DOWN_S = 120)

The script stops `strawberryswap-vault@operator` for two minutes.

| expected | signal |
| --- | --- |
| nothing is broadcast while the vault is down | no `sent` in the window; each attempt is `withdrawn` ("vault unavailable") |
| the alert, once 10 attempts have failed | `alert:vault_down` within 5 s of the 10th |
| recovery | the first `sent` after the restart, then `resolved:vault_down`; the vault re-reads its nonce, so no gap and no foreign nonce |
| no damage | no `rejected`, `mined:reverted`, `foreign_transaction` or `nonce_moved` |

`vault_down` follows the engine's attempts. In a quiet pool, with fewer than 10 attempts, the drill is
**INCONCLUSIVE**. Rerun it when the pool moves: a volatile hour, or after a large faucet trade.

### Stale price feed: `ops/drills/stale-feed.sh` (DOWN_S = 150)

The script stops `strawberryswap-relayer` for 150 s.

| expected | signal |
| --- | --- |
| the engine stops acting once the price is 45 s old | `skipped` (oracle reading outside its window), then `alert:oracle_stale` within 75 s of the stop |
| nothing is sent on a stale price | no `rejected` and no `mined:reverted` (DarkPool would revert `StaleOracle` at 60 s) |
| recovery | the relayer's first push resolves the alert: `resolved:oracle_stale` within 90 s of its restart |
| the engine stays live | its snapshots never pause for more than 10 s |

The engine checks the oracle on every tick, so this drill is conclusive whatever the market does.

### RPC failover: `ops/drills/rpc-failover.sh` (DOWN_S = 300)

The script holds the proxy's primary provider down for 5 minutes (`POST /drill/down/primary`).

| expected | signal |
| --- | --- |
| the proxy fails over within the first request | `/status`: another provider active, one more failover |
| the engine does not notice | no `rpc_down`; snapshots never pause for more than 5 s; nothing rejected or reverted |
| recovery | the primary is released; the health check brings it back and `/status` returns to it |

The proxy names providers by label. Their URLs, with their keys, stay in its 0600 file and are never logged.

## 5. The drills on the anvil rig

Arbitrum's sequencer orders the testnet: a reorg cannot be caused there, and a competitor would need a second funded
key. These two drills run on the **rig**, the testnet stack on a local anvil (chain 31337). CI already rehearses it:

* The `testnet-release` job deploys it: `DeployTestnet` with `--unlocked`.
* The `system-e2e` job runs the services against it, with the vault's debug-only test key and the oracle's KMS
  stand-ins. For the rig, deploy with `OPERATOR_ADDRESS` set to the vault's test-key address (vault README), and run
  the engine with `--json --log-txs` into `$SOAK_DIR/engine.jsonl`.

Never point these scripts at a real chain: both refuse any chain id but 31337.

### Chain reorg: `ops/drills/reorg.sh` (OPERATOR = the rig's operator address)

1. The script waits for the engine's next mined rebalance, at nonce N.
2. `anvil_reorg` removes its block.

What follows:

* The vault's view of the chain only moves forward (it takes a read behind its last one for a lagging node), so it
  never signs N again and no conflicting transaction is possible.
* The engine reports `nonce_moved`. Its next rebalance, at N + 1, is signed and then waits in the node's queue behind
  the gap.

The recovery is § 8's: re-broadcast the dropped transaction. N is mined again, N + 1 follows, and the engine resumes.

| expected | signal |
| --- | --- |
| detection | `alert:nonce_moved` |
| no damage | no `rejected`, `mined:reverted` or `foreign_transaction` (the re-broadcast is the vault's own signature) |
| recovery | a `mined:success` after the drill (INCONCLUSIVE if the rig's pool is idle) |

### Competing searcher: `ops/drills/competing-searcher.sh` (DURATION_S = 600)

Setup: a second `SearcherExecutor`, operated by anvil's unlocked account 7 and funded by the faucet tokens' minter.
`ops/drills/competitor.py` then moves the pool half way back to the oracle whenever it drifts more than 15 bps.

Its plan follows DarkPool's rules:

* the invariant never falls;
* the contract's integer deviation strictly falls;
* the Treasury is paid exactly `quoteMinPayment`.

| expected | signal |
| --- | --- |
| lost races are seen before signing | `beaten` (the simulation reverts; nothing is sent) |
| few lost races on chain | at most `REVERT_BUDGET` (3) `mined:reverted`: a race lost between the simulation and the next block |
| no confusion of keys | no `foreign_transaction` (the competitor has its own operator) |
| the engine still wins | `mined:success` during and after the race |

## 6. The lab comparison (γ = 0.0041)

**Calibration.** The engine runs with the lab's `calibration.json`, whose `engine` block is
`{gamma, horizonSec, sigma2PerSec, kappa}`. For the testnet:

```bash
cd hft-engine/python-sim
python run_sim.py --symbol ETH --days 20 --gamma 0.0041 --horizon 1 --label eth-testnet
```

`--horizon` is in minutes: 1 is the engine's 60 s rolling horizon. Install
`results/eth-testnet_*/calibration.json` as `/etc/strawberryswap/calibration.json`. Without access to Yahoo, use
`--csv` with your own ETH bars.

**After the soak:**

```bash
ops/soak/collect.sh export          # bars.csv, live_path.csv, soak_summary.json (service levels, --strict)
python3 ops/soak/compare.py --export $SOAK_DIR/export --gamma 0.0041
```

`compare.py` reruns the lab on the soak's own price path, with the engine's κ and horizon, and writes
`comparison.md`. Per UTC day (the lab's episodes for crypto) it reports:

| check | live | lab | review when |
| --- | --- | --- | --- |
| σ | the σ the lab measures on the live mids | the engine's configured σ² | ratio outside [2/3, 3/2]: recalibrate σ² |
| P&L | each day's mark-to-market P&L (Δtoken1 + Δtoken0·mid, net of the Treasury's cut) | the lab's P&L distribution | mean \|z\| > 2 |
| inventory | mean daily \|Δq\| and largest excursion | terminal-inventory std, mean max \|q\| | ratio outside [0.5, 2] |
| skew | mean \|r − mid\| | the lab path's | (reported) |
| activity | rebalances mined per day | fills per episode | (reported) |

The live engine trades against a mispriced pool, while the lab's fills arrive at random. A REVIEW is therefore a
reason to recalibrate, not a failure of the soak. If σ is flagged, rerun the lab on the soak's `bars.csv`, install the
new calibration and watch the next day. Gas, in ether, is outside the P&L, and `soak_summary.json` reports it.

## 7. Sign-off

* [ ] The service levels PASS (`soak_summary.json`: `"pass": true`), with any exception explained in writing.
* [ ] Every drill has PASSed. INCONCLUSIVE drills were rerun until conclusive.
* [ ] `comparison.md` has been reviewed. The calibration was updated if σ was flagged, and the updated calibration ran
  at least 24 h.
* [ ] No abort condition occurred, or each one has a written incident with its fix.
* [ ] These are archived to the metadata bucket next to the release record: `soak_summary.json`, `comparison.md`,
  `drill-results.jsonl` and the commit and manifest the soak ran on.

Mainnet is out of scope here. It needs the multi-party ceremony's keys, threshold signing (MPC) instead of single
KMS keys, and the external audit.

## 8. When a transaction is dropped by a reorg (also for real incidents)

Symptoms: `nonce_moved` (the operator's nonce went *back*), then `tx_stuck` for the next rebalance.

1. Find the dropped nonce N: the engine's last `mined` before `nonce_moved`, or `cast nonce <operator>`.
2. Take its raw transaction from the engine's log. These are the same bytes the vault signed, so no new signature is
   needed:
   `jq -r 'select(.event=="sent" and .nonce==N) | .raw' $SOAK_DIR/engine.jsonl | tail -1`
3. Broadcast it: `cast publish --rpc-url <rpc> <raw>`. The queued N + 1 follows, and the engine and the vault are in
   step again.

**Do not** restart the operator's vault to get past N. It would re-read the chain's next nonce and could sign a
*different* transaction at N, racing the dropped one.
