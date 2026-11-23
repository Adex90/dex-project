#!/usr/bin/env python3
"""Exports a soak's engine telemetry for the calibration lab, and judges its service levels.

    ops/soak/export_lab.py --engine soak/engine.jsonl --calibration /etc/strawberryswap/calibration.json \\
        --drills soak/drills.jsonl --out soak/export [--strict]

Writes to --out:

* bars.csv          1-minute bars of the live oracle mid, the lab's --csv input
                    (hft-engine/python-sim/run_sim.py --csv bars.csv --tz UTC --gamma 0.0041 ...; compare.py runs it)
* live_path.csv     the live engine minute by minute in the lab's path_example.csv columns: mid, σ, τ, its reservation
                    price and quotes, rebalances (bought/sold), inventory, balances and mark-to-market P&L
* soak_summary.json counters, latencies, oracle age, pool deviation, gas, alert episodes, and each service level
                    of ops/soak/RUNBOOK.md with its verdict, outside the drill windows

--strict exits 1 when a service level fails. Standard library only: it runs on the bot host.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import telemetry  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--engine", type=Path, action="append", required=True, help="the engine's --json output (repeatable)")
    p.add_argument("--calibration", type=Path, required=True, help="the calibration.json the engine runs with")
    p.add_argument("--drills", type=Path, help="the drills' log (ops/drills/lib.sh), whose windows the SLOs exclude")
    p.add_argument("--drill-margin", type=float, default=300.0, help="seconds after a drill still excluded [300]")
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--strict", action="store_true", help="exit 1 when a service level fails")
    args = p.parse_args(argv)

    t = telemetry.read_files(args.engine)
    params = telemetry.EngineParams.from_calibration(args.calibration)
    args.out.mkdir(parents=True, exist_ok=True)

    bars = telemetry.minute_bars(t.snapshots)
    telemetry.write_bars(bars, args.out / "bars.csv")
    path = telemetry.live_path(t.snapshots, params)
    telemetry.write_rows(path, telemetry.PATH_COLUMNS, args.out / "live_path.csv")
    summary = telemetry.summarize(t, telemetry.load_drills(args.drills), args.drill_margin)
    summary["engine_params"] = {"gamma": params.gamma, "horizonSec": params.horizon_sec,
                                "sigma2PerSec": params.sigma2_per_sec, "kappa": params.kappa}
    summary["exports"] = {"bars": len(bars), "path_minutes": len(path)}
    (args.out / "soak_summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    print(f"soak {summary['start']} → {summary['end']} ({summary['hours']:.1f} h, {summary['snapshots']} snapshots, "
          f"{summary['engine_runs']} engine run(s), {len(summary['drills'])} drill(s))")
    c = summary["counters"]
    print(f"  ticks {c['ticks']}  sent {c['sent']}  mined {c['mined']}  reverted {c['reverted']}  beaten {c['beaten']}"
          f"  withdrawn {c['withdrawn']}  rejected {c['rejected']}  dropped {c['dropped']}")
    for s in summary["slo"]:
        print(f"  {'PASS' if s['pass'] else 'FAIL'}  {s['name']:<26} {s['detail']}")
    print(f"wrote {args.out}/ (bars.csv: {len(bars)} bars, live_path.csv: {len(path)} minutes, soak_summary.json)")
    return 1 if args.strict and not summary["pass"] else 0


if __name__ == "__main__":
    sys.exit(main())
