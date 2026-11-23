"""python3 -m unittest discover -s strawberryswap/ops/soak/tests -t strawberryswap/ops/soak"""

from __future__ import annotations

import csv
import importlib.util
import json
import math
import os
import random
import sys
import tempfile
import unittest
from pathlib import Path

SOAK = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SOAK))
import telemetry  # noqa: E402
import export_lab  # noqa: E402

DAY0 = 1_795_000_000  # a UTC instant; the synthetic soak starts at its day boundary
START = DAY0 - DAY0 % 86_400
PARAMS = {"gamma": 0.0041, "horizonSec": 900.0, "sigma2PerSec": 0.05, "kappa": 0.5}


def snapshot(ts: float, ticks: int, mid: float, q: float, t0: float, t1: float, age: int, **extra) -> dict:
    lat = {"count": ticks, "mean_us": 9000, "p50_us": 10_000, "p99_us": 25_000, "max_us": 60_000}
    return {"event": "metrics", "ts_ms": int(ts * 1000),
            "counters": {"ticks": ticks, "idle": ticks, "skipped": 0, "waiting": 0, "beaten": extra.get("beaten", 0),
                         "withdrawn": 0, "sent": extra.get("sent", 0), "mined": extra.get("mined", 0), "reverted": 0,
                         "rejected": 0, "dropped": 0},
            "latency": {k: dict(lat) for k in telemetry.LATENCIES},
            "gauges": {"block": ticks, "base_fee_wei": "10000000", "oracle_age_sec": age, "deviation_bps": 3.5,
                       "mid": mid, "inventory": q, "token0": t0, "token1": t1, "gas_balance": 0.5, "nonce": 0,
                       "pending_nonce": 0, "in_flight": None},
            "alerts": []}


def synthetic(days: float = 2.0, every: float = 10.0, seed: int = 3) -> tuple[list[str], dict]:
    """An engine's --json output: a random-walk mid, a rebalance now and then, one restart, one vault-down drill."""
    rng = random.Random(seed)
    lines: list[str] = ["engine: StrawberrySwap HFT engine (live)", ""]
    mid, t0, t1, target = 3450.0, 20.0, 100_000.0, 20.0
    ticks = sent = mined = 0
    restart_at = START + days * 86_400 / 2
    restarted = False
    n = int(days * 86_400 / every)
    drill = {"start": START + 3 * 3600, "end": START + 3 * 3600 + 120}
    for i in range(n):
        ts = START + i * every
        if not restarted and ts >= restart_at:
            restarted, ticks, sent, mined = True, 0, 0, 0
            lines.append("engine: restarting")
        mid += rng.gauss(0, 0.4 * math.sqrt(every / 60))
        ticks += int(every * 10)
        if rng.random() < 0.03:  # a rebalance toward the oracle, mined
            side = rng.choice((-1, 1))
            t0 += side * 0.5
            t1 -= side * 0.5 * mid - 0.02
            sent += 1
            mined += 1
            lines.append(json.dumps({"event": "sent", "ts_ms": int(ts * 1000), "nonce": mined, "tx": "0x01"}))
            lines.append(json.dumps({"event": "mined", "ts_ms": int(ts * 1000) + 300, "nonce": mined, "status": "success"}))
        lines.append(json.dumps(snapshot(ts, ticks, mid, t0 - target, t0, t1, int(i * every) % 15, sent=sent, mined=mined)))
    # The drill's alert, raised and resolved inside its window.
    lines.append(json.dumps({"event": "alert", "ts_ms": int((drill["start"] + 2) * 1000), "alert": "vault_down", "detail": "x"}))
    lines.append(json.dumps({"event": "resolved", "ts_ms": int((drill["end"] + 5) * 1000), "alert": "vault_down"}))
    return lines, drill


class TelemetryTest(unittest.TestCase):
    def test_lines_are_parsed_and_noise_is_skipped(self):
        t = telemetry.parse_lines([
            "engine: banner", "", "not json {",
            'Nov 19 00:00:01 host engine[42]: {"event":"sent","ts_ms":1000,"nonce":1}',
            json.dumps(snapshot(2.0, 20, 3450.0, 0.0, 20.0, 1e5, 3)), '{"event":"metrics"}',
        ])
        self.assertEqual(len(t.snapshots), 1)
        self.assertEqual([e.kind for e in t.events], ["sent"])
        self.assertEqual(t.skipped_lines, 3)  # the banner, the broken line, the line without ts_ms

    def test_minute_bars_are_ohlc_of_the_mid(self):
        snaps = [telemetry.Snapshot(START + s, {}, {}, {"mid": m}, []) for s, m in [(0, 10.0), (20, 12.0), (40, 9.0), (59, 11.0), (61, 11.5)]]
        bars = telemetry.minute_bars(snaps)
        self.assertEqual(bars[0], {"t": START, "open": 10.0, "high": 12.0, "low": 9.0, "close": 11.0, "volume": 4})
        self.assertEqual(bars[1]["open"], 11.5)

    def test_the_live_path_carries_the_engines_quotes_and_pnl(self):
        p = telemetry.EngineParams(0.0041, 900.0, 0.05, 0.5)
        snaps = [telemetry.Snapshot(START + 30, {}, {}, {"mid": 3400.0, "inventory": 0.0, "token0": 20.0, "token1": 1000.0}, []),
                 telemetry.Snapshot(START + 90, {}, {}, {"mid": 3410.0, "inventory": 1.0, "token0": 21.0, "token1": 0.0}, [])]
        rows = telemetry.live_path(snaps, p)
        self.assertEqual(len(rows), 2)
        r = rows[1]
        self.assertAlmostEqual(r["reservation"], 3410.0 - 1.0 * 0.0041 * 0.05 * 900.0)
        half = 0.5 * (0.0041 * 0.05 * 900.0 + (2 / 0.0041) * math.log1p(0.0041 / 0.5))
        self.assertAlmostEqual(r["ask"] - r["bid"], 2 * half)
        self.assertTrue(r["bought"]) and self.assertFalse(r["sold"])
        self.assertAlmostEqual(r["pnl_mtm"], -1000.0 + 1.0 * 3410.0)  # Δtoken1 + Δtoken0·mid
        self.assertAlmostEqual(r["sigma"], math.sqrt(0.05 * 60))
        self.assertEqual(r["t_min"], 1.0)

    def test_a_clean_soak_passes_and_drills_are_excluded(self):
        lines, drill = synthetic()
        t = telemetry.parse_lines(lines)
        drills = [drill]
        s = telemetry.summarize(t, drills)
        self.assertTrue(s["pass"], s["slo"])
        self.assertEqual(s["engine_runs"], 2, "the restart splits the runs")
        self.assertGreater(s["counters"]["mined"], 0)
        self.assertEqual(s["alerts"]["vault_down"]["in_drills"], 1)
        self.assertAlmostEqual(s["alerts"]["vault_down"]["longest_s"], 123.0)

        # The same alert outside any drill is a failure when it is forbidden; a long silence costs coverage.
        t.events.append(telemetry.Event(START + 20 * 3600, "alert", {"alert": "foreign_transaction"}))
        s = telemetry.summarize(t, drills)
        self.assertFalse(s["pass"])
        self.assertEqual([c["name"] for c in s["slo"] if not c["pass"]], ["no_foreign_transaction"])
        t.events.pop()
        t.snapshots = [x for x in t.snapshots if not (START + 30 * 3600 <= x.ts <= START + 31 * 3600)]
        s = telemetry.summarize(t, drills)
        self.assertEqual([c["name"] for c in s["slo"] if not c["pass"]], ["coverage"])

    def test_drill_windows_come_from_the_drills_log(self):
        with tempfile.TemporaryDirectory() as d:
            log = Path(d) / "drills.jsonl"
            log.write_text("\n".join([json.dumps({"drill": "vault-down", "phase": "start", "ts_ms": 1000}),
                                      json.dumps({"drill": "vault-down", "phase": "end", "ts_ms": 61000}),
                                      "garbage", json.dumps({"drill": "stale-feed", "phase": "start", "ts_ms": 90000})]))
            w = telemetry.load_drills(log)
        self.assertEqual(w[0], {"drill": "vault-down", "start": 1.0, "end": 61.0})
        self.assertEqual(w[1]["drill"], "stale-feed")
        self.assertTrue(math.isinf(w[1]["end"]), "a drill still running excludes everything after its start")


class ExportTest(unittest.TestCase):
    def test_export_writes_the_labs_inputs_and_the_summary(self):
        lines, drill = synthetic(days=1.0)
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "engine.jsonl").write_text("\n".join(lines) + "\n")
            (d / "calibration.json").write_text(json.dumps({"engine": PARAMS}))
            (d / "drills.jsonl").write_text(json.dumps({"drill": "vault-down", "phase": "start", "ts_ms": int(drill["start"] * 1000)}) + "\n"
                                           + json.dumps({"drill": "vault-down", "phase": "end", "ts_ms": int(drill["end"] * 1000)}) + "\n")
            rc = export_lab.main(["--engine", str(d / "engine.jsonl"), "--calibration", str(d / "calibration.json"),
                                  "--drills", str(d / "drills.jsonl"), "--out", str(d / "out"), "--strict"])
            self.assertEqual(rc, 0)
            with open(d / "out" / "bars.csv") as f:
                bars = list(csv.DictReader(f))
            self.assertEqual(len(bars), 1440)
            self.assertEqual(list(bars[0]), ["timestamp", "open", "high", "low", "close", "volume"])
            with open(d / "out" / "live_path.csv") as f:
                path = list(csv.DictReader(f))
            self.assertEqual(len(path), 1440)
            self.assertEqual(list(path[0]), telemetry.PATH_COLUMNS)
            summary = json.loads((d / "out" / "soak_summary.json").read_text())
            self.assertTrue(summary["pass"])
            self.assertEqual(summary["engine_params"]["gamma"], 0.0041)


@unittest.skipUnless(importlib.util.find_spec("pandas") and importlib.util.find_spec("numpy"), "the lab needs pandas and numpy")
class CompareTest(unittest.TestCase):
    def test_the_lab_reruns_on_the_soaks_path_at_the_testnets_gamma(self):
        import compare

        lines, _ = synthetic(days=2.0, every=20.0)
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "engine.jsonl").write_text("\n".join(lines) + "\n")
            (d / "calibration.json").write_text(json.dumps({"engine": PARAMS}))
            out = d / "export"
            self.assertEqual(export_lab.main(["--engine", str(d / "engine.jsonl"), "--calibration", str(d / "calibration.json"), "--out", str(out)]), 0)
            # A relative --export, as the runbook writes it: the lab runs from its own directory.
            cwd = os.getcwd()
            os.chdir(d)
            try:
                self.assertEqual(compare.main(["--export", "export", "--gamma", "0.0041", "--paths", "40"]), 0)
            finally:
                os.chdir(cwd)
            result = json.loads((out / "comparison.json").read_text())
            self.assertEqual(result["gamma"], 0.0041)
            names = [c["name"] for c in result["checks"]]
            self.assertEqual(names, ["sigma", "pnl", "inventory_final", "inventory_max", "skew", "activity"])
            sigma = result["checks"][0]
            # The synthetic mid moves 0.4 per √min; the engine was told σ² = 0.05/s, i.e. σ ≈ 1.73 per √min.
            self.assertAlmostEqual(sigma["live"], 0.4, delta=0.05)
            self.assertEqual(sigma["verdict"], "REVIEW", "a σ four times too high must be flagged")
            self.assertIn("sigma", result["review"])
            self.assertEqual(len(result["days"]), 2)
            self.assertIn("| sigma |", (out / "comparison.md").read_text())


if __name__ == "__main__":
    unittest.main()
