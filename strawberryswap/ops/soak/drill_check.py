#!/usr/bin/env python3
"""Judges one chaos drill from the engine's --json output and the drills' log.

    ops/soak/drill_check.py --engine soak/engine.jsonl --drills soak/drills.jsonl --drill vault-down \\
        --expect-in-fault alert:vault_down --within 5 --forbid-in-fault sent \\
        --expect-after-fault resolved:vault_down --after 120 --forbid rejected --forbid mined:reverted

The fault window is the drill's last start..end in the drills' log (ops/drills/lib.sh writes them); the recovery window
is the --after seconds that follow. An event spec is KIND or KIND:VALUE, KIND being the engine's "event" field:

* alert:NAME, resolved:NAME   the alert's name (vault_down, oracle_stale, rpc_down, foreign_transaction, …)
* mined:STATUS                success or reverted
* skipped|beaten|withdrawn|rejected:TEXT   TEXT within the reason
* sent, waiting, …            any such event

--expect-in-fault must happen inside the fault window (within --within seconds of its start, when given);
--forbid-in-fault must not; --expect-after-fault must happen in the recovery window; --forbid must happen nowhere in
fault + recovery. Prints one line per check; exits 1 when any fails.

--require-activity SPEC [--min-activity N]: some alerts follow the engine's own attempts (vault_down is raised once
10 rebalances in a row were withdrawn because the vault could not sign them, and resolved by the next send), so a
quiet market proves nothing. With fewer than N (default 1) events matching SPEC during the fault, the drill is
INCONCLUSIVE (exit 3: rerun it while the pool moves); otherwise --within counts from the N-th of them.
--recovery-activity SPEC: likewise after the fault (no send, no resolution to expect).
--max-snapshot-gap S: the engine's metrics snapshots never stop for more than S seconds in fault and recovery.
--allow SPEC=N: at most N events matching SPEC in fault and recovery (a budget, where --forbid is zero).
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import telemetry  # noqa: E402


def matches(event: telemetry.Event, spec: str) -> bool:
    kind, _, value = spec.partition(":")
    if event.kind != kind:
        return False
    if not value:
        return True
    d = event.data
    if kind in ("alert", "resolved"):
        return d.get("alert") == value
    if kind == "mined":
        return d.get("status") == value
    return value in str(d.get("reason", ""))


def window(drills: list[dict], name: str) -> tuple[float, float]:
    runs = [d for d in drills if d["drill"] == name]
    if not runs:
        raise SystemExit(f"drill_check: no {name} drill in the drills' log")
    last = runs[-1]
    if not math.isfinite(last["end"]):
        raise SystemExit(f"drill_check: the {name} drill has not ended")
    return last["start"], last["end"]


def check(events: list[telemetry.Event], start: float, end: float, after: float, within: float | None,
          expect_in: list[str], forbid_in: list[str], expect_after: list[str], forbid: list[str],
          origin: float | None = None) -> list[tuple[str, bool, str]]:
    """`origin`: where --within counts from (the fault's start by default)."""
    fault = [e for e in events if start <= e.ts <= end]
    recovery = [e for e in events if end < e.ts <= end + after]
    origin = start if origin is None else origin
    out = []
    for spec in expect_in:
        hits = [e for e in fault if matches(e, spec)]
        first = hits[0].ts - origin if hits else None
        ok = bool(hits) and (within is None or first <= within)
        out.append((f"expect {spec} during the fault", ok, f"first after {first:.1f} s" if hits else "never"))
    for spec in forbid_in:
        n = sum(1 for e in fault if matches(e, spec))
        out.append((f"no {spec} during the fault", n == 0, f"{n} seen"))
    for spec in expect_after:
        hits = [e for e in recovery if matches(e, spec)]
        out.append((f"expect {spec} after the fault", bool(hits),
                    f"{hits[0].ts - end:.1f} s after it ended" if hits else f"not within {after:g} s"))
    for spec in forbid:
        n = sum(1 for e in fault + recovery if matches(e, spec))
        out.append((f"no {spec} at all", n == 0, f"{n} seen"))
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--engine", type=Path, action="append", required=True)
    p.add_argument("--drills", type=Path, required=True)
    p.add_argument("--drill", required=True)
    p.add_argument("--within", type=float, help="seconds after the fault's start for --expect-in-fault")
    p.add_argument("--after", type=float, default=120.0, help="the recovery window, seconds [120]")
    p.add_argument("--expect-in-fault", action="append", default=[])
    p.add_argument("--forbid-in-fault", action="append", default=[])
    p.add_argument("--expect-after-fault", action="append", default=[])
    p.add_argument("--forbid", action="append", default=[])
    p.add_argument("--require-activity", metavar="SPEC", help="without such an event during the fault: INCONCLUSIVE")
    p.add_argument("--min-activity", type=int, default=1, help="how many --require-activity events are needed [1]")
    p.add_argument("--recovery-activity", metavar="SPEC", help="without such an event after the fault: INCONCLUSIVE")
    p.add_argument("--max-snapshot-gap", type=float, help="the longest silence of the engine's snapshots, seconds")
    p.add_argument("--allow", action="append", default=[], metavar="SPEC=N", help="at most N such events")
    p.add_argument("--json", action="store_true", help="print the result as JSON")
    args = p.parse_args(argv)

    t = telemetry.read_files(args.engine)
    start, end = window(telemetry.load_drills(args.drills), args.drill)
    origin = None

    def inconclusive(note: str) -> int:
        print(json.dumps({"drill": args.drill, "verdict": "INCONCLUSIVE", "note": note}) if args.json
              else f"drill {args.drill}: INCONCLUSIVE: {note}")
        return 3

    if args.require_activity:
        attempts = [e for e in t.events if start <= e.ts <= end and matches(e, args.require_activity)]
        if len(attempts) < args.min_activity:
            return inconclusive(f"{len(attempts)} {args.require_activity} during the fault (needs {args.min_activity}): "
                                "the engine attempted too little; rerun while the pool moves")
        origin = attempts[args.min_activity - 1].ts
    if args.recovery_activity and not any(end < e.ts <= end + args.after and matches(e, args.recovery_activity) for e in t.events):
        return inconclusive(f"no {args.recovery_activity} within {args.after:g} s after the fault")
    results = check(t.events, start, end, args.after, args.within, args.expect_in_fault, args.forbid_in_fault,
                    args.expect_after_fault, args.forbid, origin)
    for budget in args.allow:
        spec, _, limit = budget.rpartition("=")
        if not spec or not limit.isdigit():
            raise SystemExit(f"drill_check: --allow takes SPEC=N, not {budget!r}")
        n = sum(1 for e in t.events if start <= e.ts <= end + args.after and matches(e, spec))
        results.append((f"at most {limit} {spec}", n <= int(limit), f"{n} seen"))
    if args.max_snapshot_gap is not None:
        stamps = [x.ts for x in t.snapshots if start - args.max_snapshot_gap <= x.ts <= end + args.after]
        gap = max((b - a for a, b in zip(stamps, stamps[1:])), default=math.inf if len(stamps) < 2 else 0.0)
        results.append((f"snapshots never silent > {args.max_snapshot_gap:g} s", gap <= args.max_snapshot_gap,
                        f"longest silence {gap:.1f} s"))
    passed = all(ok for _, ok, _ in results)
    if args.json:
        print(json.dumps({"drill": args.drill, "start": telemetry.iso(start), "end": telemetry.iso(end),
                          "verdict": "PASS" if passed else "FAIL", "pass": passed,
                          "checks": [{"check": c, "pass": ok, "detail": d} for c, ok, d in results]}))
    else:
        print(f"drill {args.drill}: fault {telemetry.iso(start)} → {telemetry.iso(end)} ({end - start:.0f} s)")
        for c, ok, d in results:
            print(f"  {'PASS' if ok else 'FAIL'}  {c:<48} {d}")
        print(f"drill {args.drill}: {'PASS' if passed else 'FAIL'}")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
