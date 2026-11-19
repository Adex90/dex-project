#!/usr/bin/env bash
# Drill: the operator's vault goes down for DOWN_S seconds (default 120), then comes back.
#
# Expected, judged by ops/soak/drill_check.py:
# * nothing is broadcast while it is down: each rebalance the engine wants is withdrawn, unsigned;
# * vault_down is raised once 10 attempts in a row have failed, and resolved by the first send after the vault is back
#   (the vault re-reads the operator's nonce from the chain at start: no foreign nonce, no gap);
# * no transaction is rejected or reverted.
# The alert follows the engine's attempts, so a quiet pool proves nothing: with fewer than 10 attempts during the fault
# the drill is INCONCLUSIVE (exit 3). Run it when the engine has been rebalancing (ops/soak/RUNBOOK.md).
set -euo pipefail
# shellcheck source=SCRIPTDIR/lib.sh
. "$(dirname "$0")/lib.sh"
DOWN_S=${DOWN_S:-120}
RECOVER_S=${RECOVER_S:-600}

drill_begin vault-down
trap 'unit_start "$VAULT_UNIT"' EXIT # whatever happens, the vault comes back
unit_stop "$VAULT_UNIT"
say "$VAULT_UNIT stopped for $DOWN_S s"
sleep "$DOWN_S"
unit_start "$VAULT_UNIT"
trap - EXIT
drill_end

say "waiting up to $RECOVER_S s for the engine's next send (it resolves vault_down)"
wait_event sent "$RECOVER_S" "$DRILL_END_MS" >/dev/null || say "no send yet: the pool may be quiet"
judge --require-activity withdrawn:vault --min-activity 10 --expect-in-fault alert:vault_down --within 5 \
  --forbid-in-fault sent --recovery-activity sent --expect-after-fault resolved:vault_down --after "$RECOVER_S" \
  --forbid rejected --forbid mined:reverted --forbid alert:foreign_transaction --forbid alert:nonce_moved
