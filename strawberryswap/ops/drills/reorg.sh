#!/usr/bin/env bash
# Drill (anvil rig): a chain reorganization drops the engine's last mined rebalance.
#
# Arbitrum's sequencer orders the testnet, so its reorgs are rare and cannot be caused at will: this drill runs on the
# anvil rig (ops/soak/RUNBOOK.md: the testnet deployment and the services on a local anvil, chain 31337, the engine
# with --log-txs), where anvil_reorg rewinds the chain.
#
# What happens: the engine and the vault both saw nonce N mined; after the reorg the chain's next nonce is N again.
# The vault's view of the chain only moves forward (a read behind its last is taken for a lagging node), so it never
# signs anything at N again: no conflicting transaction is possible. The engine reports nonce_moved and keeps its
# floor: its next rebalance, at N + 1, is signed but waits in the node's queue behind the gap (tx_stuck after a
# while). The recovery is the runbook's: re-broadcast the dropped transaction, the very bytes the vault signed (the
# engine logs them with --log-txs). It is mined again at N, the queued N + 1 follows, and all three agree again.
#
# Expected, judged by ops/soak/drill_check.py: nonce_moved within 30 s of the reorg; no transaction rejected or
# reverted, no foreign nonce (the re-broadcast is the vault's own); after the recovery the engine sends and mines
# again (INCONCLUSIVE, exit 3, if the rig's pool gives it nothing to do within RECOVER_S).
set -euo pipefail
# shellcheck source=SCRIPTDIR/lib.sh
. "$(dirname "$0")/lib.sh"
RIG_RPC=${RIG_RPC:-http://127.0.0.1:8545}
OPERATOR=${OPERATOR:?the operator address on the rig (the executor operator)}
RECOVER_S=${RECOVER_S:-600}

[ "$(cast chain-id --rpc-url "$RIG_RPC")" = 31337 ] || die "$RIG_RPC is not the anvil rig (chain 31337): never reorg a real chain"

drill_begin reorg
say "waiting for the engine's next mined rebalance"
mined=$(wait_event mined:success 900) || die "the engine mined nothing in 15 minutes"
nonce=$(jq -r .nonce <<<"$mined")
tx=$(jq -r .tx <<<"$mined")
block=$(jq -r .block <<<"$mined")
raw=$(python3 - "$ENGINE_LOG" "$nonce" <<'PY'
import json, sys
raw = ""
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    i = line.find("{")
    if i < 0:
        continue
    try:
        e = json.loads(line[i:])
    except ValueError:
        continue
    if e.get("event") == "sent" and str(e.get("nonce")) == sys.argv[2] and e.get("raw"):
        raw = e["raw"]
print(raw)
PY
)
[ -n "$raw" ] || die "the engine's log has no raw bytes for nonce $nonce: run the engine with --log-txs on the rig"

head=$(cast block-number --rpc-url "$RIG_RPC")
depth=$((head - block + 1))
say "nonce $nonce mined in block $block (head $head): reorganizing $depth block(s) away"
cast rpc --rpc-url "$RIG_RPC" anvil_reorg "$depth" '[]' >/dev/null
if cast receipt --rpc-url "$RIG_RPC" "$tx" >/dev/null 2>&1; then
  die "tx $tx is still mined after the reorg"
fi
say "tx $tx is gone; the operator's next nonce is $(cast nonce --rpc-url "$RIG_RPC" "$OPERATOR")"
wait_event alert:nonce_moved 30 >/dev/null || say "WARNING: no nonce_moved within 30 s"

say "recovery: re-broadcasting nonce $nonce from the engine's log"
cast publish --rpc-url "$RIG_RPC" "$raw" >/dev/null
for _ in $(seq 30); do
  cast receipt --rpc-url "$RIG_RPC" "$tx" >/dev/null 2>&1 && break
  sleep 1
done
cast receipt --rpc-url "$RIG_RPC" "$tx" >/dev/null 2>&1 || die "the re-broadcast transaction was not mined"
say "nonce $nonce mined again; the operator's next nonce is $(cast nonce --rpc-url "$RIG_RPC" "$OPERATOR")"
drill_end

wait_event mined:success "$RECOVER_S" "$DRILL_END_MS" >/dev/null || say "no rebalance mined yet after the recovery"
judge --expect-in-fault alert:nonce_moved --forbid rejected --forbid mined:reverted --forbid alert:foreign_transaction \
  --recovery-activity sent --expect-after-fault mined:success --after "$RECOVER_S"
