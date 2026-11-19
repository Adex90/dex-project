#!/usr/bin/env bash
# Shared by the chaos drills (sourced, not run): configuration, the drills' log, and the engine's telemetry.
#
# The drills run on the soak host, next to the services they disturb (ops/soak/RUNBOOK.md). Every name below can be
# overridden from the environment. They need the soak's collector running (ops/soak/collect.sh start), which keeps the
# engine's --json output in $ENGINE_LOG; each drill appends its start and end to $DRILLS_LOG, so the soak's service
# levels exclude its window and ops/soak/drill_check.py can judge it.

SOAK_DIR=${SOAK_DIR:-/var/lib/strawberryswap/soak}
ENGINE_LOG=${ENGINE_LOG:-$SOAK_DIR/engine.jsonl}
DRILLS_LOG=${DRILLS_LOG:-$SOAK_DIR/drills.jsonl}
ENGINE_UNIT=${ENGINE_UNIT:-strawberryswap-engine.service}
VAULT_UNIT=${VAULT_UNIT:-strawberryswap-vault@operator.service}
RELAYER_UNIT=${RELAYER_UNIT:-strawberryswap-relayer.service}
PROXY_URL=${PROXY_URL:-http://127.0.0.1:8547}
SYSTEMCTL=${SYSTEMCTL:-sudo systemctl}
OPS=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DRILL=${DRILL:-unnamed}
RESULTS=${RESULTS:-$SOAK_DIR/drill-results.jsonl}

now_ms() { date +%s%3N; }
say() { printf '%s %s: %s\n' "$(date -u +%FT%TZ)" "$DRILL" "$*"; }
die() {
  say "ERROR: $*" >&2
  exit 2
}

mark() { printf '{"drill":"%s","phase":"%s","ts_ms":%s}\n' "$DRILL" "$1" "$(now_ms)" >>"$DRILLS_LOG"; }

# drill_begin NAME: checks the collector, records the start.
drill_begin() {
  DRILL=$1
  [ -s "$ENGINE_LOG" ] || die "no engine telemetry in $ENGINE_LOG: start the collector (ops/soak/collect.sh start)"
  mkdir -p "$(dirname "$DRILLS_LOG")"
  DRILL_START_MS=$(now_ms)
  mark start
  say "fault injected"
}

drill_end() {
  mark end
  DRILL_END_MS=$(now_ms)
  say "fault removed after $(((DRILL_END_MS - DRILL_START_MS) / 1000)) s"
}

# unit_stop / unit_start UNIT
unit_stop() { $SYSTEMCTL stop "$1"; }
unit_start() { $SYSTEMCTL start "$1"; }

# wait_event SPEC TIMEOUT_S [SINCE_MS]: waits for the engine to log an event matching SPEC (drill_check.py syntax).
wait_event() {
  python3 "$OPS/soak/wait_event.py" --engine "$ENGINE_LOG" --since-ms "${3:-$DRILL_START_MS}" --timeout "$2" "$1"
}

# judge ARGS…: drill_check.py on this drill; appends its JSON verdict to $RESULTS and prints the table. Exit 0 PASS,
# 1 FAIL, 3 INCONCLUSIVE.
judge() {
  local rc=0
  python3 "$OPS/soak/drill_check.py" --engine "$ENGINE_LOG" --drills "$DRILLS_LOG" --drill "$DRILL" "$@" || rc=$?
  python3 "$OPS/soak/drill_check.py" --engine "$ENGINE_LOG" --drills "$DRILLS_LOG" --drill "$DRILL" --json "$@" >>"$RESULTS" || true
  return "$rc"
}
