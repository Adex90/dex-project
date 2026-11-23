#!/usr/bin/env bash
# Drill: the relayer stops for DOWN_S seconds (default 150): ZkPriceFeed's price goes stale; then the relayer returns.
#
# Expected, judged by ops/soak/drill_check.py:
# * the engine stops acting on the price once it is 45 s old (its rule, inside DarkPool's 60 s) and raises
#   oracle_stale 10 ticks later: within 75 s of the stop (the last push may be up to 15 s old when it stops);
# * nothing is sent on a stale price: no rejected transaction and none reverted (DarkPool would revert StaleOracle);
# * the relayer's first push after its restart (every ~15 s) resolves oracle_stale within 90 s;
# * the engine's view never goes dark (its snapshots keep coming).
# oracle_stale is checked on every tick from the chain read, so this drill is conclusive whatever the market does.
set -euo pipefail
# shellcheck source=SCRIPTDIR/lib.sh
. "$(dirname "$0")/lib.sh"
DOWN_S=${DOWN_S:-150}
RECOVER_S=${RECOVER_S:-90}

drill_begin stale-feed
trap 'unit_start "$RELAYER_UNIT"' EXIT
unit_stop "$RELAYER_UNIT"
say "$RELAYER_UNIT stopped for $DOWN_S s"
if wait_event alert:oracle_stale "$DOWN_S" >/dev/null; then
  say "oracle_stale raised"
fi
now=$(now_ms)
left=$(((DRILL_START_MS + DOWN_S * 1000 - now) / 1000))
[ "$left" -le 0 ] || sleep "$left"
unit_start "$RELAYER_UNIT"
trap - EXIT
drill_end

wait_event resolved:oracle_stale "$RECOVER_S" "$DRILL_END_MS" >/dev/null || say "oracle_stale not resolved yet"
judge --expect-in-fault alert:oracle_stale --within 75 --expect-after-fault resolved:oracle_stale --after "$RECOVER_S" \
  --forbid rejected --forbid mined:reverted --forbid alert:foreign_transaction --max-snapshot-gap 10
