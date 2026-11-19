#!/usr/bin/env python3
"""Compares the soak's live engine with the calibration lab at the testnet's risk aversion (γ = 0.0041).

    ops/soak/compare.py --export soak/export [--gamma 0.0041] [--lab hft-engine/python-sim] [--paths 500]

It reruns the lab (hft-engine/python-sim/run_sim.py) on the soak's own price path (export_lab.py's bars.csv) with the
engine's κ and rolling horizon, then sets what the engine did beside what the model expects, per UTC day (the lab's
episodes for crypto):

* σ: the engine's configured σ against the σ the lab measures on the live mids;
* P&L: each day's live mark-to-market P&L against the lab's distribution (z-score);
* inventory: the live excursion from each day's start against the lab's terminal and maximum inventory;
* skew: the live |reservation − mid| against the lab's path;
* activity: live rebalances against the lab's fills.

The live engine trades against the pool when it is mispriced; the lab's fills arrive at random. So these are review
triggers for the calibration (flagged when outside their bands), not pass/fail: the soak's pass/fail is its service
levels (export_lab.py). Writes comparison.json and comparison.md next to the export; needs the lab's requirements.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
LAB = HERE.parent.parent / "hft-engine" / "python-sim"

# Review bands.
SIGMA_BAND = (2 / 3, 3 / 2)
INVENTORY_BAND = (0.5, 2.0)
PNL_Z_MAX = 2.0


def read_csv(path: Path) -> list[dict]:
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def fnum(v) -> float | None:
    try:
        x = float(v)
    except (TypeError, ValueError):
        return None
    return x if math.isfinite(x) else None


def run_lab(export: Path, gamma: float, params: dict, lab: Path, paths: int, python: str) -> Path:
    out = export / "lab"
    cmd = [python, str(lab / "run_sim.py"), "--csv", str(export / "bars.csv"), "--tz", "UTC", "--label", "soak",
           "--gamma", repr(gamma), "--horizon", repr(params["horizonSec"] / 60.0), "--k", repr(params["kappa"]),
           "--paths", str(paths), "--out", str(out)]
    print("lab:", " ".join(cmd[1:]), file=sys.stderr)
    subprocess.run(cmd, check=True, cwd=lab, stdout=sys.stderr)
    runs = sorted(out.glob("soak_*"), key=lambda p: p.stat().st_mtime)
    if not runs:
        raise SystemExit(f"the lab wrote nothing under {out}")
    return runs[-1]


def per_day(path_rows: list[dict]) -> dict[str, dict]:
    days: dict[str, list[dict]] = {}
    for row in path_rows:
        days.setdefault(row["episode"], []).append(row)
    out = {}
    for day, rows in sorted(days.items()):
        q0 = fnum(rows[0]["inventory"]) or 0.0
        q = [fnum(r["inventory"]) or 0.0 for r in rows]
        pnl = [fnum(r["pnl_mtm"]) or 0.0 for r in rows]
        skew = [abs((fnum(r["reservation"]) or 0.0) - (fnum(r["mid"]) or 0.0)) for r in rows]
        out[day] = {"minutes": len(rows), "pnl": pnl[-1] - pnl[0], "final_dq": q[-1] - q0,
                    "max_abs_dq": max(abs(x - q0) for x in q),
                    "rebalance_minutes": sum(1 for r in rows if r["bought"] == "True" or r["sold"] == "True"),
                    "mean_abs_skew": statistics.fmean(skew) if skew else None}
    return out


def band(value: float | None, lo: float, hi: float) -> str:
    if value is None:
        return "n/a"
    return "within" if lo <= value <= hi else "REVIEW"


def compare(export: Path, lab_dir: Path, gamma: float) -> dict:
    summary = json.loads((export / "soak_summary.json").read_text())
    params = summary["engine_params"]
    live = per_day(read_csv(export / "live_path.csv"))
    lab_rows = [r for r in read_csv(lab_dir / "summary.csv") if math.isclose(float(r["gamma"]), gamma, rel_tol=1e-9)]
    lab = {r["strategy"]: {k: fnum(v) for k, v in r.items() if k not in ("strategy",)} for r in lab_rows}
    if "inventory" not in lab:
        raise SystemExit(f"{lab_dir}/summary.csv has no inventory-strategy row for γ = {gamma}")
    inv = lab["inventory"]
    calibration = json.loads((lab_dir / "calibration.json").read_text())
    lab_path = read_csv(lab_dir / "path_example.csv")

    engine_sigma = math.sqrt(params["sigma2PerSec"] * 60.0)
    live_sigma = calibration["sigma"]["in_sample_per_minute"]
    sigma_ratio = engine_sigma / live_sigma if live_sigma else None

    pnl_std = inv.get("pnl_std") or 0.0
    z = {d: ((v["pnl"] - (inv.get("pnl_mean") or 0.0)) / pnl_std if pnl_std > 0 else None) for d, v in live.items()}
    zs = [x for x in z.values() if x is not None]
    mean_z = statistics.fmean(zs) if zs else None

    days = list(live.values())
    live_final = statistics.fmean(abs(v["final_dq"]) for v in days) if days else None
    live_max = statistics.fmean(v["max_abs_dq"] for v in days) if days else None
    inv_final_ratio = live_final / inv["final_q_std"] if live_final is not None and inv.get("final_q_std") else None
    inv_max_ratio = live_max / inv["max_abs_q_mean"] if live_max is not None and inv.get("max_abs_q_mean") else None

    live_skew = statistics.fmean(v["mean_abs_skew"] for v in days if v["mean_abs_skew"] is not None) if days else None
    lab_skew = statistics.fmean(abs(float(r["reservation"]) - float(r["mid"])) for r in lab_path) if lab_path else None
    mined_per_day = summary["counters"]["mined"] / max(len(days), 1)

    checks = [
        {"name": "sigma", "live": live_sigma, "engine": engine_sigma, "ratio": sigma_ratio,
         "verdict": band(sigma_ratio, *SIGMA_BAND),
         "meaning": "the engine's σ against the σ of the live mids (per √min); outside the band, recalibrate σ²"},
        {"name": "pnl", "live_per_day": {d: v["pnl"] for d, v in live.items()}, "lab_mean": inv.get("pnl_mean"),
         "lab_std": inv.get("pnl_std"), "z_per_day": z, "mean_z": mean_z,
         "verdict": "n/a" if mean_z is None else ("within" if abs(mean_z) <= PNL_Z_MAX else "REVIEW"),
         "meaning": "each day's live P&L (token1) against the lab's episodes at the same γ"},
        {"name": "inventory_final", "live": live_final, "lab_final_q_std": inv.get("final_q_std"),
         "ratio": inv_final_ratio, "verdict": band(inv_final_ratio, *INVENTORY_BAND),
         "meaning": "mean |q change| over a day (token0) against the lab's terminal-inventory std"},
        {"name": "inventory_max", "live": live_max, "lab_max_abs_q_mean": inv.get("max_abs_q_mean"),
         "ratio": inv_max_ratio, "verdict": band(inv_max_ratio, *INVENTORY_BAND),
         "meaning": "mean largest |q excursion| in a day against the lab's"},
        {"name": "skew", "live": live_skew, "lab": lab_skew,
         "ratio": live_skew / lab_skew if live_skew is not None and lab_skew else None, "verdict": "report",
         "meaning": "mean |reservation − mid|: how hard inventory bends the engine's prices"},
        {"name": "activity", "live_mined_per_day": mined_per_day, "lab_fills_mean": inv.get("fills_mean"),
         "verdict": "report", "meaning": "rebalances mined per day against the lab's fills per episode"},
    ]
    return {
        "gamma": gamma, "engine_params": params, "lab_dir": str(lab_dir), "days": live,
        "lab": {"inventory": inv, "symmetric": lab.get("symmetric"), "sigma": calibration["sigma"],
                "arrivals": calibration["arrivals"]},
        "checks": checks, "review": [c["name"] for c in checks if c["verdict"] == "REVIEW"],
        "service_levels_pass": summary["pass"],
    }


def fmt(v, digits: int = 4) -> str:
    if v is None:
        return "—"
    if isinstance(v, float):
        return f"{v:.{digits}g}"
    return str(v)


def markdown(result: dict, summary: dict) -> str:
    lines = [f"# Soak vs. the calibration lab (γ = {result['gamma']:g})", "",
             f"Soak {summary['start']} → {summary['end']} ({summary['hours']:.1f} h). "
             f"Service levels: **{'PASS' if summary['pass'] else 'FAIL'}** (soak_summary.json).", "",
             "| check | live | lab / engine | ratio or z | verdict |", "|---|---|---|---|---|"]
    for c in result["checks"]:
        if c["name"] == "sigma":
            row = (fmt(c["live"]), f"engine {fmt(c['engine'])}", fmt(c["ratio"]))
        elif c["name"] == "pnl":
            row = (", ".join(fmt(v) for v in c["live_per_day"].values()), f"{fmt(c['lab_mean'])} ± {fmt(c['lab_std'])}", f"mean z {fmt(c['mean_z'])}")
        elif c["name"] == "inventory_final":
            row = (fmt(c["live"]), fmt(c["lab_final_q_std"]), fmt(c["ratio"]))
        elif c["name"] == "inventory_max":
            row = (fmt(c["live"]), fmt(c["lab_max_abs_q_mean"]), fmt(c["ratio"]))
        elif c["name"] == "skew":
            row = (fmt(c["live"]), fmt(c["lab"]), fmt(c["ratio"]))
        else:
            row = (fmt(c["live_mined_per_day"]), fmt(c["lab_fills_mean"]), "")
        lines.append(f"| {c['name']} | {row[0]} | {row[1]} | {row[2]} | {c['verdict']} |")
    lines += ["", "What each line means:", ""] + [f"* **{c['name']}**: {c['meaning']}." for c in result["checks"]]
    lines += ["", "The live engine rebalances a pool when it is mispriced; the lab's fills arrive at random, so a REVIEW "
              "is a reason to recalibrate (rerun the lab on the soak's bars, update calibration.json), not a failure."]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--export", type=Path, required=True, help="export_lab.py's --out")
    p.add_argument("--gamma", type=float, default=0.0041, help="the risk aversion to compare at [0.0041]")
    p.add_argument("--lab", type=Path, default=LAB, help=f"the lab [{LAB}]")
    p.add_argument("--lab-run", type=Path, help="an existing lab output directory (skips rerunning the lab)")
    p.add_argument("--paths", type=int, default=500, help="the lab's Monte Carlo fill paths per episode [500]")
    p.add_argument("--python", default=sys.executable, help="the Python with the lab's requirements")
    args = p.parse_args(argv)
    args.export = args.export.resolve()  # the lab runs from its own directory
    if args.lab_run:
        args.lab_run = args.lab_run.resolve()

    summary = json.loads((args.export / "soak_summary.json").read_text())
    engine_gamma = summary["engine_params"]["gamma"]
    if not math.isclose(engine_gamma, args.gamma, rel_tol=1e-9):
        print(f"warning: the engine ran with γ = {engine_gamma:g}, the comparison is at γ = {args.gamma:g}", file=sys.stderr)
    lab_dir = args.lab_run or run_lab(args.export, args.gamma, summary["engine_params"], args.lab.resolve(), args.paths, args.python)
    result = compare(args.export, lab_dir, args.gamma)
    (args.export / "comparison.json").write_text(json.dumps(result, indent=2) + "\n")
    (args.export / "comparison.md").write_text(markdown(result, summary))
    print(markdown(result, summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
