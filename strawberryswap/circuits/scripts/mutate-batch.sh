#!/bin/bash
# Mutation testing for the batch_swap circuit: each single-line mutation must make the adversarial suite FAIL.
# Usage: CIRCOM=/path/to/circom [ONLY=M7] scripts/mutate-batch.sh
set -u
CIRCOM=${CIRCOM:-circom}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
run() {
  local name="$1" from="$2" to="$3"
  [[ "$name" =~ ${ONLY:-.} ]] || return
  rm -rf "$WORK/m" && mkdir -p "$WORK/m/batch_swap" "$WORK/m/lib" "$WORK/out"
  cp "$ROOT/lib/merkle.circom" "$WORK/m/lib/"
  python3 - "$ROOT/batch_swap/batch_swap.circom" "$WORK/m/batch_swap/batch_swap.circom" "$from" "$to" <<'PYEOF'
import sys
src, dst, a, b = sys.argv[1:5]
s = open(src).read()
assert a in s, "mutation target not found: " + a
open(dst, "w").write(s.replace(a, b, 1))
PYEOF
  [ $? -eq 0 ] || { echo "MUTATION $name: target missing"; return; }
  cp "$ROOT/batch_swap/main_n4.circom" "$WORK/m/batch_swap/"
  (cd "$ROOT" && "$CIRCOM" "$WORK/m/batch_swap/main_n4.circom" --wasm -l node_modules -o "$WORK/out" >/dev/null 2>&1) || { echo "MUTATION $name: did not compile"; return; }
  local res
  res=$(cd "$ROOT" && BATCH_WASM="$WORK/out/main_n4_js/main_n4.wasm" node test/batch.test.mjs 2>&1 | grep -E "^  FAIL" | head -2 | cut -c1-110)
  echo "MUTATION $name"
  if [ -n "$res" ]; then echo "$res" | sed 's/^/   caught: /'; else echo "   !!! SURVIVED"; fi
}
run "M1 drop the minOut check"            "isSwap * (1 - ge.out) === 0;"                       ""
run "M2 token1 sellers paid at pLo"        "pHi <== pLo + 1;"                                    "pHi <== pLo;"
run "M3 allow swaps after the deadline"    "isSwap * expired.out === 0;"                         ""
run "M4 drop the net-flow equation (asset0)" "netIn0 - netOut0 === sA0[N] - sO0[N];"             ""
run "M5 drop the nullifier binding"        "nullifier === active * nf.out;"                      ""
run "M6 never check Merkle membership"     "member.enabled <== active;"                          "member.enabled <== 0;"
run "M7 drop the division remainder bound" "isSwap * (1 - remLt.out) === 0;"                     ""
run "M8 drop the price remainder bound"    "pRemLt.out === 1;"                                   ""
run "M9 refunds allowed before deadline"   "isRefund * (1 - expired.out) === 0;"                 ""
