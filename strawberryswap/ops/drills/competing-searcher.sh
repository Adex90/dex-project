#!/usr/bin/env bash
# Drill (anvil rig): a competing searcher races the engine for the same pool for DURATION_S seconds (default 600).
#
# A second SearcherExecutor, operated by one of anvil's unlocked accounts (no key), is deployed on the rig and funded
# with the faucet tokens' minter (the rig's deployer); ops/drills/competitor.py then rebalances the pool whenever it
# drifts more than COMPETITOR_BPS from the oracle, half the way back, paying the Treasury its cut.
#
# Expected, judged by ops/soak/drill_check.py: the engine loses some races and sees it before signing (beaten: its
# simulation reverts, nothing is sent); what it does send rarely reverts (at most REVERT_BUDGET: a race lost between
# its simulation and the next block); no foreign nonce (the competitor has its own operator); and it keeps winning
# rebalances (mined) during and after the race. If the competitor never acts, the drill is INCONCLUSIVE (exit 3).
set -euo pipefail
# shellcheck source=SCRIPTDIR/lib.sh
. "$(dirname "$0")/lib.sh"
RIG_RPC=${RIG_RPC:-http://127.0.0.1:8545}
MANIFEST=${MANIFEST:-$OPS/../contracts/deployments/31337.json}
# anvil's accounts 0 (the rig's deployer and the tokens' minter) and 7 (the competitor): public addresses, unlocked.
RIG_DEPLOYER=${RIG_DEPLOYER:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}
COMPETITOR=${COMPETITOR:-0x14dC79964da2C08b23698B3D3cc7Ca32193d9955}
DURATION_S=${DURATION_S:-600}
COMPETITOR_BPS=${COMPETITOR_BPS:-15}
REVERT_BUDGET=${REVERT_BUDGET:-3}
RECOVER_S=${RECOVER_S:-300}

[ "$(cast chain-id --rpc-url "$RIG_RPC")" = 31337 ] || die "$RIG_RPC is not the anvil rig (chain 31337)"
pool=$(jq -r .contracts.pool "$MANIFEST")
tweth=$(jq -r .contracts.tWETH "$MANIFEST")
tusd=$(jq -r .contracts.tUSD "$MANIFEST")

say "deploying the competitor's SearcherExecutor (operator $COMPETITOR)"
executor=$(cd "$OPS/../contracts" && forge create --rpc-url "$RIG_RPC" --unlocked --from "$RIG_DEPLOYER" --broadcast \
  src/periphery/SearcherExecutor.sol:SearcherExecutor --constructor-args "$COMPETITOR" "$pool" --json | jq -r .deployedTo)
[[ "$executor" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "the competitor's executor was not deployed"
cast send --rpc-url "$RIG_RPC" --unlocked --from "$RIG_DEPLOYER" "$tweth" "mint(address,uint256)" "$executor" 50ether >/dev/null
cast send --rpc-url "$RIG_RPC" --unlocked --from "$RIG_DEPLOYER" "$tusd" "mint(address,uint256)" "$executor" 200000ether >/dev/null
say "competitor executor $executor funded"

drill_begin competing-searcher
python3 "$OPS/drills/competitor.py" --rpc "$RIG_RPC" --pool "$pool" --executor "$executor" --operator "$COMPETITOR" \
  --bps "$COMPETITOR_BPS" --duration "$DURATION_S" | tee "$SOAK_DIR/competitor.jsonl"
drill_end
won=$(grep -c '"result": "mined"' "$SOAK_DIR/competitor.jsonl" || true)
lost=$(grep -c '"result": "reverted"' "$SOAK_DIR/competitor.jsonl" || true)
say "the competitor won $won rebalance(s) and lost $lost"
[ "$won" -gt 0 ] || {
  say "INCONCLUSIVE: the competitor never rebalanced (no drift above $COMPETITOR_BPS bps on the rig)"
  exit 3
}
wait_event mined:success "$RECOVER_S" "$DRILL_END_MS" >/dev/null || say "no rebalance mined yet after the race"
judge --forbid rejected --forbid alert:foreign_transaction --allow "mined:reverted=$REVERT_BUDGET" \
  --recovery-activity sent --expect-after-fault mined:success --after "$RECOVER_S"
