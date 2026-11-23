#!/usr/bin/env bash
# Drill: the primary RPC provider fails for DOWN_S seconds (default 300): the failover proxy in front of the engine
# (ops/soak/rpc_failover.py, run with --drills) holds it down, then releases it.
#
# Expected:
# * the proxy fails over at once: its /status shows another provider active, and one more failover;
# * the engine never notices: no rpc_down, its snapshots keep coming (no gap over 5 s), nothing rejected or reverted;
# * after the release, the health check brings the primary back and traffic returns to it.
# The proxy's /status names providers by label only; their URLs (with their keys) never leave its configuration.
set -euo pipefail
# shellcheck source=SCRIPTDIR/lib.sh
. "$(dirname "$0")/lib.sh"
DOWN_S=${DOWN_S:-300}
PRIMARY=${PRIMARY:-primary}

status() { curl -fsS --max-time 5 "$PROXY_URL/status"; }
before=$(status | jq -r .failovers) || die "the failover proxy does not answer at $PROXY_URL"
[ "$(status | jq -r .active)" = "$PRIMARY" ] || die "the proxy is not on $PRIMARY: nothing to fail over from"

drill_begin rpc-failover
trap 'curl -fsS -X POST "$PROXY_URL/drill/up/$PRIMARY" >/dev/null' EXIT
curl -fsS -X POST "$PROXY_URL/drill/down/$PRIMARY" >/dev/null || die "the proxy refused the drill (is it run with --drills?)"
sleep 5
active=$(status | jq -r .active)
[ "$active" != "$PRIMARY" ] || die "the proxy still serves from $PRIMARY 5 s into the drill"
say "the proxy serves from $active"
sleep "$DOWN_S"
curl -fsS -X POST "$PROXY_URL/drill/up/$PRIMARY" >/dev/null
trap - EXIT
drill_end

for _ in $(seq 30); do
  [ "$(status | jq -r .active)" = "$PRIMARY" ] && break
  sleep 2
done
after=$(status | jq -r .failovers)
say "failovers: $before before, $after after; active: $(status | jq -r .active)"
[ "$after" -ge $((before + 2)) ] || say "WARNING: expected a failover away and one back"
judge --forbid alert:rpc_down --forbid rejected --forbid mined:reverted --forbid alert:foreign_transaction \
  --max-snapshot-gap 5 --after 60
