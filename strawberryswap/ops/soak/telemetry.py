"""The soak's telemetry: the HFT engine's `--json` lines, read back into what the 72-hour soak is judged on.

The engine (bots/hft-engine-rust, run with --json) prints one JSON object per line:

* a `metrics` snapshot every --metrics-every ticks: cumulative counters and latency histograms (since the process
  started) and the gauges of the last tick: block, oracle age, pool/oracle deviation, the oracle mid, the inventory q
  (executor token0 minus its target), the executor's token0 and token1, the operator's gas;
* events: sent, mined, beaten, withdrawn, rejected, skipped, waiting, and alert / resolved.

Every line carries ts_ms. Lines that are not JSON objects (start-up banners, journald prefixes) are skipped, or read
from their first "{" on. Standard library only: it runs on the bot host as well as next to the lab.
"""

from __future__ import annotations

import csv
import json
import math
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

# Alerts that are raised and later resolved (bots/hft-engine-rust/src/metrics.rs, Alert); the others are one-off
# notices (tx_reverted, nonce_spent, tx_dropped, nonce_moved).
RESOLVABLE = {"rpc_down", "oracle_stale", "vault_down", "fees_above_cap", "tx_stuck", "foreign_transaction", "low_gas_balance"}
COUNTERS = ("ticks", "idle", "skipped", "waiting", "beaten", "withdrawn", "sent", "mined", "reverted", "rejected", "dropped")
LATENCIES = ("read", "simulate", "sign", "broadcast", "tick", "confirm")


@dataclass
class Snapshot:
    ts: float  # seconds since the epoch
    counters: dict
    latency: dict
    gauges: dict
    alerts: list


@dataclass
class Event:
    ts: float
    kind: str
    data: dict


@dataclass
class Telemetry:
    snapshots: list[Snapshot] = field(default_factory=list)
    events: list[Event] = field(default_factory=list)
    skipped_lines: int = 0


def parse_lines(lines: Iterable[str]) -> Telemetry:
    t = Telemetry()
    for raw in lines:
        start = raw.find("{")
        if start < 0:
            if raw.strip():
                t.skipped_lines += 1
            continue
        try:
            obj = json.loads(raw[start:])
        except ValueError:
            t.skipped_lines += 1
            continue
        ts, kind = obj.get("ts_ms") if isinstance(obj, dict) else None, obj.get("event") if isinstance(obj, dict) else None
        if not isinstance(ts, (int, float)) or not isinstance(kind, str):
            t.skipped_lines += 1
            continue
        if kind == "metrics":
            t.snapshots.append(Snapshot(ts / 1000.0, obj.get("counters") or {}, obj.get("latency") or {}, obj.get("gauges") or {}, obj.get("alerts") or []))
        else:
            t.events.append(Event(ts / 1000.0, kind, obj))
    t.snapshots.sort(key=lambda s: s.ts)
    t.events.sort(key=lambda e: e.ts)
    return t


def read_files(paths: Iterable[Path]) -> Telemetry:
    lines: list[str] = []
    for path in paths:
        with open(path, encoding="utf-8", errors="replace") as f:
            lines.extend(f)
    return parse_lines(lines)


def number(v) -> float | None:
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) else None


def iso(ts: float) -> str:
    return datetime.fromtimestamp(ts, tz=timezone.utc).isoformat()


# ---------------------------------------------------------------------------------------------------------- the model
@dataclass(frozen=True)
class EngineParams:
    """The `engine` block of the lab's calibration.json, which the engine runs with (--calibration)."""

    gamma: float
    horizon_sec: float
    sigma2_per_sec: float
    kappa: float

    @classmethod
    def from_calibration(cls, path: Path) -> "EngineParams":
        e = json.loads(Path(path).read_text())["engine"]
        p = cls(float(e["gamma"]), float(e["horizonSec"]), float(e["sigma2PerSec"]), float(e["kappa"]))
        if not (p.gamma > 0 and p.kappa > 0 and p.horizon_sec >= 0 and p.sigma2_per_sec >= 0):
            raise ValueError(f"{path}: the engine block must have γ, κ > 0 and τ, σ² ≥ 0")
        return p

    def quote(self, mid: float, q: float) -> tuple[float, float]:
        """The engine's reservation price and half spread (bots/hft-engine-rust/src/model.rs, the lab's closed forms,
        without the tick grid: the engine trades against the pool, it does not rest orders)."""
        r = mid - q * self.gamma * self.sigma2_per_sec * self.horizon_sec
        half = 0.5 * (self.gamma * self.sigma2_per_sec * self.horizon_sec + (2.0 / self.gamma) * math.log1p(self.gamma / self.kappa))
        return r, half


# ---------------------------------------------------------------------------------------------- lab-format exports
def minute_bars(snapshots: list[Snapshot]) -> list[dict]:
    """1-minute OHLC bars of the oracle mid: the lab's --csv input (run_sim.py --csv bars.csv --tz UTC). `volume` is
    the number of snapshots in the minute (the lab does not use it)."""
    bars: dict[int, dict] = {}
    for s in snapshots:
        mid = number(s.gauges.get("mid"))
        if mid is None or mid <= 0:
            continue
        minute = int(s.ts // 60) * 60
        b = bars.get(minute)
        if b is None:
            bars[minute] = {"t": minute, "open": mid, "high": mid, "low": mid, "close": mid, "volume": 1}
        else:
            b["high"], b["low"], b["close"] = max(b["high"], mid), min(b["low"], mid), mid
            b["volume"] += 1
    return [bars[k] for k in sorted(bars)]


def write_bars(bars: list[dict], path: Path) -> None:
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["timestamp", "open", "high", "low", "close", "volume"])
        for b in bars:
            w.writerow([iso(b["t"]), repr(b["open"]), repr(b["high"]), repr(b["low"]), repr(b["close"]), b["volume"]])


PATH_COLUMNS = ["timestamp", "episode", "t_min", "mid", "sigma", "tau_min", "reservation", "bid", "ask", "spread",
                "bought", "sold", "inventory", "token0", "token1", "cash", "pnl_mtm", "deviation_bps", "oracle_age_sec"]


def live_path(snapshots: list[Snapshot], params: EngineParams) -> list[dict]:
    """The live engine minute by minute, in the columns of the lab's path_example.csv (plus the balances and the
    pool's deviation), from the last snapshot of each minute:

    * sigma, tau_min, reservation, bid, ask, spread: the engine's own quote, from its parameters, the mid and q;
    * bought / sold: whether the executor's token0 rose / fell since the previous minute (a mined rebalance);
    * cash, pnl_mtm: the trading P&L against holding the starting balances, Δtoken1 + Δtoken0·mid, in token1. It
      counts the Treasury's 80 % cut (paid in token1) and not the gas (in ether, reported separately).
    """
    last: dict[int, Snapshot] = {}
    for s in snapshots:
        g = s.gauges
        if all(number(g.get(k)) is not None for k in ("mid", "inventory", "token0", "token1")):
            last[int(s.ts // 60) * 60] = s
    rows: list[dict] = []
    sigma = math.sqrt(params.sigma2_per_sec * 60.0)
    first = prev = None
    for minute in sorted(last):
        g = last[minute].gauges
        mid, q, t0, t1 = (float(g[k]) for k in ("mid", "inventory", "token0", "token1"))
        if first is None:
            first = (minute, t0, t1)
        r, half = params.quote(mid, q)
        d0, d1 = t0 - first[1], t1 - first[2]
        rows.append({
            "timestamp": iso(minute), "episode": iso(minute)[:10], "t_min": (minute - first[0]) / 60.0, "mid": mid,
            "sigma": sigma, "tau_min": params.horizon_sec / 60.0, "reservation": r, "bid": r - half, "ask": r + half,
            "spread": 2.0 * half, "bought": prev is not None and t0 > prev + 1e-12,
            "sold": prev is not None and t0 < prev - 1e-12, "inventory": q, "token0": t0, "token1": t1, "cash": d1,
            "pnl_mtm": d1 + d0 * mid, "deviation_bps": number(g.get("deviation_bps")),
            "oracle_age_sec": number(g.get("oracle_age_sec")),
        })
        prev = t0
    return rows


def write_rows(rows: list[dict], columns: list[str], path: Path) -> None:
    with open(path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=columns, extrasaction="ignore")
        w.writeheader()
        for row in rows:
            w.writerow({k: ("" if row.get(k) is None else row[k]) for k in columns})


# ------------------------------------------------------------------------------------------------------------ SLOs
def quantile(values: list[float], q: float) -> float | None:
    if not values:
        return None
    v = sorted(values)
    return v[min(len(v) - 1, max(0, math.ceil(q * len(v)) - 1))]


def load_drills(path: Path | None) -> list[dict]:
    """Drill windows from the drills' log (ops/drills/lib.sh appends {"drill", "phase": start|end, "ts_ms"})."""
    if path is None or not Path(path).exists():
        return []
    open_: dict[str, float] = {}
    windows: list[dict] = []
    for line in Path(path).read_text().splitlines():
        try:
            d = json.loads(line)
        except ValueError:
            continue
        name, phase, ts = d.get("drill"), d.get("phase"), d.get("ts_ms")
        if not isinstance(name, str) or not isinstance(ts, (int, float)):
            continue
        if phase == "start":
            open_[name] = ts / 1000.0
        elif phase == "end" and name in open_:
            windows.append({"drill": name, "start": open_.pop(name), "end": ts / 1000.0})
    windows.extend({"drill": n, "start": s, "end": math.inf} for n, s in open_.items())
    return windows


def in_drill(ts: float, drills: list[dict], margin: float) -> bool:
    return any(d["start"] <= ts <= d["end"] + margin for d in drills)


def segments(snapshots: list[Snapshot]) -> list[list[Snapshot]]:
    """Runs of one engine process: its cumulative tick counter restarts at a restart."""
    out: list[list[Snapshot]] = []
    for s in snapshots:
        ticks = number(s.counters.get("ticks")) or 0.0
        if not out or ticks < (number(out[-1][-1].counters.get("ticks")) or 0.0):
            out.append([s])
        else:
            out[-1].append(s)
    return out


# The soak's service levels (ops/soak/RUNBOOK.md), outside the drill windows.
SLO = {
    "coverage_min": 0.995,  # share of the soak with engine snapshots no more than gap_factor intervals apart
    "gap_factor": 5.0,
    "tick_p99_us_max": 100_000,  # the engine's 100 ms tick
    "read_p99_us_max": 80_000,  # its chain-read budget
    "sign_p99_us_max": 80_000,  # the vault's deadline
    "oracle_age_p99_s_max": 30.0,  # the relayer pushes every ~15 s
    "oracle_age_max_s_max": 45.0,  # the engine stops acting at 45 s; DarkPool refuses at 60 s
    "reverted_max": 0,  # every send is simulated first
    "rejected_max": 0,
    "forbidden_alerts": ["foreign_transaction", "nonce_moved", "nonce_spent", "low_gas_balance", "tx_stuck"],
}


def summarize(t: Telemetry, drills: list[dict] | None = None, drill_margin: float = 300.0, slo: dict | None = None) -> dict:
    slo = {**SLO, **(slo or {})}
    drills = drills or []
    snaps = t.snapshots
    if len(snaps) < 2:
        raise ValueError("fewer than two metrics snapshots: run the engine with --json and --metrics-every")
    start, end = snaps[0].ts, snaps[-1].ts
    duration = end - start
    gaps = [b.ts - a.ts for a, b in zip(snaps, snaps[1:])]
    interval = quantile(gaps, 0.5) or 1.0
    downtime = sum(g for g, a in zip(gaps, snaps) if g > slo["gap_factor"] * interval and not in_drill(a.ts, drills, drill_margin))
    drill_time = sum(max(0.0, min(d["end"] + drill_margin, end) - max(d["start"], start)) for d in drills)
    judged = max(duration - drill_time, 1e-9)

    runs = segments(snaps)
    counters = {k: sum(int(number(r[-1].counters.get(k)) or 0) for r in runs) for k in COUNTERS}
    latency = {}
    for name in LATENCIES:
        finals = [r[-1].latency.get(name) or {} for r in runs]
        p99s = [number(f.get("p99_us")) for f in finals if number(f.get("p99_us")) is not None]
        latency[name] = {"count": sum(int(number(f.get("count")) or 0) for f in finals),
                         "p99_us_worst_run": max(p99s) if p99s else None,
                         "max_us": max((number(f.get("max_us")) or 0) for f in finals) if finals else None}

    outside = [s for s in snaps if not in_drill(s.ts, drills, drill_margin)]
    ages = [a for s in outside if (a := number(s.gauges.get("oracle_age_sec"))) is not None]
    devs = [d for s in outside if (d := number(s.gauges.get("deviation_bps"))) is not None]
    gas = [g for s in snaps if (g := number(s.gauges.get("gas_balance"))) is not None]

    alerts: dict[str, dict] = {}
    raised: dict[str, float] = {}
    for e in t.events:
        if e.kind not in ("alert", "resolved"):
            continue
        name = str(e.data.get("alert"))
        a = alerts.setdefault(name, {"raised": 0, "in_drills": 0, "seconds": 0.0, "longest_s": 0.0})
        if e.kind == "alert":
            a["raised"] += 1
            a["in_drills"] += in_drill(e.ts, drills, drill_margin)
            if name in RESOLVABLE:
                raised[name] = e.ts
        elif name in raised:
            held = e.ts - raised.pop(name)
            a["seconds"] += held
            a["longest_s"] = max(a["longest_s"], held)
    for name, since in raised.items():
        alerts[name]["open_since"] = iso(since)

    events_outside = [e for e in t.events if not in_drill(e.ts, drills, drill_margin)]
    reverted = sum(1 for e in events_outside if e.kind == "mined" and e.data.get("status") == "reverted")
    rejected = sum(1 for e in events_outside if e.kind == "rejected")

    coverage = 1.0 - downtime / judged
    checks = [
        ("coverage", coverage >= slo["coverage_min"], f"{coverage:.4%} of the soak outside drills (≥ {slo['coverage_min']:.1%})"),
        ("tick_p99", (latency["tick"]["p99_us_worst_run"] or 0) <= slo["tick_p99_us_max"], f"{latency['tick']['p99_us_worst_run']} µs (≤ {slo['tick_p99_us_max']})"),
        ("read_p99", (latency["read"]["p99_us_worst_run"] or 0) <= slo["read_p99_us_max"], f"{latency['read']['p99_us_worst_run']} µs (≤ {slo['read_p99_us_max']})"),
        ("sign_p99", (latency["sign"]["p99_us_worst_run"] or 0) <= slo["sign_p99_us_max"], f"{latency['sign']['p99_us_worst_run']} µs (≤ {slo['sign_p99_us_max']})"),
        ("oracle_age_p99", (quantile(ages, 0.99) or 0) <= slo["oracle_age_p99_s_max"], f"{quantile(ages, 0.99)} s (≤ {slo['oracle_age_p99_s_max']})"),
        ("oracle_age_max", (max(ages) if ages else 0) <= slo["oracle_age_max_s_max"], f"{max(ages) if ages else None} s (≤ {slo['oracle_age_max_s_max']})"),
        ("reverted", reverted <= slo["reverted_max"], f"{reverted} reverted on chain outside drills (≤ {slo['reverted_max']})"),
        ("rejected", rejected <= slo["rejected_max"], f"{rejected} rejected by the node outside drills (≤ {slo['rejected_max']})"),
    ]
    for name in slo["forbidden_alerts"]:
        n = alerts.get(name, {}).get("raised", 0) - alerts.get(name, {}).get("in_drills", 0)
        checks.append((f"no_{name}", n == 0, f"{n} outside drills"))

    return {
        "start": iso(start), "end": iso(end), "hours": duration / 3600.0, "snapshots": len(snaps),
        "snapshot_interval_s": interval, "engine_runs": len(runs), "skipped_lines": t.skipped_lines,
        "drills": [{**d, "start": iso(d["start"]), "end": iso(d["end"]) if math.isfinite(d["end"]) else None} for d in drills],
        "counters": counters, "latency": latency,
        "oracle_age_s": {"p50": quantile(ages, 0.5), "p99": quantile(ages, 0.99), "max": max(ages) if ages else None},
        "deviation_bps": {"p50": quantile(devs, 0.5), "p99": quantile(devs, 0.99), "max": max(devs) if devs else None},
        "gas_ether": {"first": gas[0] if gas else None, "last": gas[-1] if gas else None, "min": min(gas) if gas else None},
        "alerts": alerts,
        "slo": [{"name": n, "pass": bool(ok), "detail": d} for n, ok, d in checks],
        "pass": all(ok for _, ok, _ in checks),
    }
