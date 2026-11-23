#!/usr/bin/env bash
# The soak's collector, on the soak host: keeps the engine's --json output (and the relayer's, the vaults' and the
# proxy's journals) under $SOAK_DIR for the whole soak, then exports and judges it.
#
#   ops/soak/collect.sh start    follow the journals into $SOAK_DIR (in the background; idempotent)
#   ops/soak/collect.sh stop     stop following
#   ops/soak/collect.sh status   what is being collected, and how much
#   ops/soak/collect.sh export   export_lab.py on what was collected: bars, live path, service levels (--strict)
#
# The engine must run with --json --metrics-every 10 (a snapshot a second) and, for the reorg runbook, --log-txs.
# Nothing collected holds a secret: the services print no key, no RPC URL and no salt.
set -euo pipefail

SOAK_DIR=${SOAK_DIR:-/var/lib/strawberryswap/soak}
ENGINE_UNIT=${ENGINE_UNIT:-strawberryswap-engine.service}
OTHER_UNITS=${OTHER_UNITS:-strawberryswap-relayer.service strawberryswap-notary.service strawberryswap-vault@operator.service strawberryswap-vault@relayer.service strawberryswap-vault@automator.service strawberryswap-rpc-failover.service}
CALIBRATION=${CALIBRATION:-/etc/strawberryswap/calibration.json}
JOURNALCTL=${JOURNALCTL:-journalctl}
here=$(cd "$(dirname "$0")" && pwd)
pids="$SOAK_DIR/collector.pids"

start() {
  mkdir -p "$SOAK_DIR"
  if [ -s "$pids" ] && xargs -r kill -0 <"$pids" 2>/dev/null; then
    echo "collect: already running ($(wc -l <"$pids") followers)"
    return
  fi
  : >"$pids"
  # -o cat: the message only, so each engine line is its JSON object; --since now: the soak starts here.
  nohup "$JOURNALCTL" -u "$ENGINE_UNIT" -o cat -f --since now >>"$SOAK_DIR/engine.jsonl" 2>/dev/null &
  echo $! >>"$pids"
  for unit in $OTHER_UNITS; do
    nohup "$JOURNALCTL" -u "$unit" -o short-iso-precise -f --since now >>"$SOAK_DIR/${unit%.service}.log" 2>/dev/null &
    echo $! >>"$pids"
  done
  date -u +%FT%TZ >"$SOAK_DIR/started"
  echo "collect: following $ENGINE_UNIT and $(echo "$OTHER_UNITS" | wc -w) more into $SOAK_DIR"
}

stop() {
  if [ -s "$pids" ]; then
    xargs -r kill <"$pids" 2>/dev/null || true
    : >"$pids"
  fi
  date -u +%FT%TZ >"$SOAK_DIR/stopped"
  echo "collect: stopped"
}

status() {
  echo "collect: started $(cat "$SOAK_DIR/started" 2>/dev/null || echo never)"
  ls -la "$SOAK_DIR"
  grep -c '"event":"metrics"' "$SOAK_DIR/engine.jsonl" 2>/dev/null | sed 's/^/engine snapshots: /' || true
}

export_() {
  python3 "$here/export_lab.py" --engine "$SOAK_DIR/engine.jsonl" --calibration "$CALIBRATION" \
    --drills "$SOAK_DIR/drills.jsonl" --out "$SOAK_DIR/export" --strict
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  export) export_ ;;
  *)
    echo "usage: $0 start|stop|status|export" >&2
    exit 2
    ;;
esac
