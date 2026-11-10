#!/usr/bin/env python3
"""StrawberrySwap A-S lab: 1-minute bars → calibrate σ and λ(δ) = A·e^(−kδ) → simulate the optimal quotes over a γ grid.

    python run_sim.py --symbol SPY --days 7
    python run_sim.py --symbol ETH --horizon 60 --gamma-grid 0.0001,0.001,0.01
    python run_sim.py --csv bars.csv --tz UTC --label binance-ethusdt
    python run_sim.py --synthetic           # offline smoke test on simulated bars (no market data)

Writes results/<label>_<first day>_<last day>/: summary.csv, episodes.csv, path_example.csv, touch_curve.csv and
calibration.json (the inputs for the C++/Rust engine, in its units).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd

from as_sim import calibration, data, simulator
from as_sim.model import optimal_spread

GRID_MULTIPLIERS = (0.03, 0.1, 0.3, 1.0, 3.0)


def parse_args(argv=None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    src = p.add_argument_group("data (one source)")
    src.add_argument("--symbol", default="SPY", help="Yahoo symbol: SPY, ETH (= ETH-USD), BTC (= BTC-USD), ... [SPY]")
    src.add_argument("--days", type=int, default=7, help="days of 1-minute bars to download, 1-29 [7]")
    src.add_argument("--csv", type=Path, help="load bars from a CSV instead of Yahoo")
    src.add_argument("--synthetic", action="store_true", help="simulated bars (arithmetic Brownian motion), no download")
    src.add_argument("--tz", help="episode timezone (default: New York for equities, UTC for crypto)")
    src.add_argument("--label", help="name for the output folder (default: the symbol)")
    src.add_argument("--cache", type=Path, default=Path("data"), help="where downloads are saved as CSV [data]")

    cal = p.add_argument_group("calibration")
    cal.add_argument("--mid", choices=data.MID_SOURCES, default="close",
                     help="mid-price proxy without quote data: close (point in time) or range ((high+low)/2) [close]")
    cal.add_argument("--sigma-mode", choices=("ewma", "constant"), default="ewma",
                     help="ewma: σ known at each bar, past data only; constant: one in-sample σ [ewma]")
    cal.add_argument("--halflife", type=float, default=30.0, help="EWMA half-life of σ, minutes [30]")
    cal.add_argument("--k", type=float, help="override the fitted arrival decay k (1/price)")
    cal.add_argument("--A", type=float, help="override the fitted arrival rate A (fills per minute at the mid)")

    sim = p.add_argument_group("simulation")
    sim.add_argument("--gamma", type=float, help="a single γ (1/(price·lot)); default: a grid around γ* = 1/(k·σ²·τ̄)")
    sim.add_argument("--gamma-grid", help="comma-separated γ values")
    sim.add_argument("--horizon", default="session", help='"session" (A-S finite horizon) or minutes (rolling) [session]')
    sim.add_argument("--paths", type=int, default=500, help="Monte Carlo fill paths per episode [500]")
    sim.add_argument("--seed", type=int, default=7)
    sim.add_argument("--lot", type=float, default=1.0, help="quote size in base units [1]")
    sim.add_argument("--tick", type=float, default=0.01, help="price tick [0.01]")
    sim.add_argument("--fee-bps", type=float, default=0.0, help="fee on fill notional, bps; negative = rebate [0]")
    sim.add_argument("--max-inventory", type=float, help="stop quoting the side that would exceed |q|")
    p.add_argument("--out", type=Path, default=Path("results"))
    args = p.parse_args(argv)
    if args.csv and args.synthetic:
        p.error("choose one data source: --csv or --synthetic (or neither, for Yahoo)")
    if args.horizon != "session":
        try:
            args.horizon = float(args.horizon)
        except ValueError:
            p.error('--horizon must be "session" or a number of minutes')
        if args.horizon <= 0:
            p.error("--horizon must be positive")
    return args


def load(args) -> tuple[pd.DataFrame, str, str]:
    """Bars, the episode timezone and a label for the outputs."""
    if args.synthetic:
        tz = args.tz or "America/New_York"
        return data.synthetic_bars(tz=tz, seed=args.seed), tz, args.label or "synthetic"
    if args.csv:
        return data.load_csv(args.csv), args.tz or data.session_timezone(args.symbol), args.label or args.csv.stem
    sym = data.resolve_symbol(args.symbol)
    bars = data.download_bars(sym, days=args.days)
    cached = data.save_csv(bars, args.cache / f"{sym}_1m_{bars.index[0]:%Y%m%d}_{bars.index[-1]:%Y%m%d}.csv")
    print(f"downloaded {len(bars)} bars of {sym}; saved to {cached}")
    return bars, args.tz or data.session_timezone(sym), args.label or sym


def gamma_grid(args, k: float, sigma: float, tau_ref: float) -> tuple[list[float], float]:
    """γ* = 1/(k·σ²·τ̄): the γ at which one lot held over the average horizon skews r by the risk-neutral
    half-spread 1/k. The default grid brackets it."""
    g_star = 1.0 / (k * sigma**2 * tau_ref)
    if args.gamma is not None:
        return [args.gamma], g_star
    if args.gamma_grid:
        return [float(x) for x in args.gamma_grid.split(",") if x.strip()], g_star
    return [float(f"{g_star * m:.2g}") for m in GRID_MULTIPLIERS], g_star


def main(argv=None) -> int:
    args = parse_args(argv)
    try:
        bars, tz, label = load(args)
    except (RuntimeError, ValueError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    df = data.prepare(bars, tz, mid_source=args.mid)
    has_quotes = {"bid", "ask"} <= set(df.columns)
    mid_desc = "(bid + ask) / 2 from quotes" if has_quotes else ("close of each 1-minute bar" if args.mid == "close" else "(high + low) / 2 of each 1-minute bar")
    episodes = df["episode"].nunique()

    # ---- calibration ------------------------------------------------------------------------------------------
    sigma_const = calibration.sigma_per_minute(df)
    alt = df.assign(mid=(df["high"] + df["low"]) / 2.0) if args.mid == "close" else df.assign(mid=df["close"])
    sigma_alt = calibration.sigma_per_minute(alt)
    sigma = (
        calibration.ewma_sigma(df, halflife_minutes=args.halflife)
        if args.sigma_mode == "ewma"
        else pd.Series(sigma_const, index=df.index, name="sigma")
    )
    fit = calibration.fit_intensity(df)
    k = args.k if args.k is not None else fit.k
    A = args.A if args.A is not None else fit.A
    session_len = df.groupby("episode")["t_min"].max().median()
    tau_ref = session_len / 2.0 if args.horizon == "session" else args.horizon
    gammas, g_star = gamma_grid(args, k, float(sigma.median()), tau_ref)

    print(f"\n{label}: {len(df)} bars, {episodes} episodes ({df['episode'].iloc[0]} to {df['episode'].iloc[-1]}, {tz})")
    print(f"  mid                  {mid_desc}")
    print(f"  σ                    {sigma_const:.6g} per √min, in sample (median EWMA {sigma.median():.6g};"
          f" with the {'range' if args.mid == 'close' else 'close'} proxy it would be {sigma_alt:.6g})")
    print(f"  arrivals λ(δ)=A·e^(−kδ)  A = {A:.4g} /min   k = {k:.4g} /price"
          f"{'   (override)' if args.k is not None or args.A is not None else '   (touch fit: an upper bound on fills)'}")
    print(f"  horizon              {'session (finite, A-S)' if args.horizon == 'session' else f'rolling {args.horizon:g} min'};"
          f" τ̄ = {tau_ref:.4g} min;  γ* = 1/(kσ²τ̄) = {g_star:.4g}")
    for g in gammas:
        half = 0.5 * optimal_spread(g, float(sigma.median()), tau_ref, k)
        skew = g * float(sigma.median()) ** 2 * tau_ref
        print(f"  γ = {g:<10.4g} half-spread at τ̄ {half:.4g}   reservation skew per lot {skew:.4g}")

    # ---- simulation ---------------------------------------------------------------------------------------------
    cfg = simulator.SimConfig(
        gamma=gammas[0], k=k, A=A, horizon=args.horizon, lot=args.lot, tick=args.tick,
        fee_bps=args.fee_bps, max_inventory=args.max_inventory, n_paths=args.paths, seed=args.seed,
    )
    res = simulator.run(df, sigma, cfg, gammas)
    cols = ["gamma", "strategy", "pnl_mean", "pnl_std", "pnl_sharpe", "final_q_std", "max_abs_q_mean", "fills_mean", "spread_mean"]
    print(f"\nper episode (q₀ = 0, {args.paths} fill paths × {res.summary['episodes'].iloc[0]} episodes, lot {args.lot:g}):")
    print(res.summary[cols].to_string(index=False, float_format=lambda v: f"{v:.4g}"))

    inv = res.summary[res.summary["strategy"] == "inventory"]
    best = inv.loc[inv["pnl_sharpe"].idxmax()]
    print(f"\nhighest P&L/risk (inventory strategy): γ = {best['gamma']:.4g}  "
          f"(mean {best['pnl_mean']:.4g}, std {best['pnl_std']:.4g}, terminal-inventory std {best['final_q_std']:.3g})")

    # ---- outputs ------------------------------------------------------------------------------------------------
    out = args.out / f"{label}_{df['episode'].iloc[0]}_{df['episode'].iloc[-1]}"
    out.mkdir(parents=True, exist_ok=True)
    res.summary.to_csv(out / "summary.csv", index=False)
    res.episodes.to_csv(out / "episodes.csv", index=False)
    res.path.to_csv(out / "path_example.csv", index=False)
    fit.table.to_csv(out / "touch_curve.csv", index=False)
    sig_eng = float(sigma.median())
    calib = {
        "label": label,
        "data": {"first_day": df["episode"].iloc[0], "last_day": df["episode"].iloc[-1], "bars": int(len(df)),
                 "episodes": int(episodes), "timezone": tz, "mid": mid_desc,
                 "synthetic": bool(args.synthetic)},
        "sigma": {"per_minute": sig_eng, "sigma2_per_minute": sig_eng**2, "sigma2_per_sec": sig_eng**2 / 60.0,
                  "mode": args.sigma_mode, "ewma_halflife_minutes": args.halflife,
                  "in_sample_per_minute": sigma_const},
        "arrivals": {"A_per_minute": A, "k_per_price": k, "method": "override" if args.k is not None or args.A is not None else "touch fit (upper bound)"},
        "horizon": {"mode": "session" if args.horizon == "session" else "rolling", "tau_ref_minutes": tau_ref},
        "gamma": {"tested": gammas, "gamma_star": g_star, "best_sharpe_inventory": float(best["gamma"])},
        # bots/inventory-cpp ASParams / quotes() inputs, in its units (seconds)
        "engine": {"gamma": float(best["gamma"]) if args.gamma is None else args.gamma,
                   "horizonSec": tau_ref * 60.0, "sigma2PerSec": sig_eng**2 / 60.0, "kappa": k},
    }
    (out / "calibration.json").write_text(json.dumps(calib, indent=2) + "\n")
    print(f"\nwrote {out}/ (summary.csv, episodes.csv, path_example.csv, touch_curve.csv, calibration.json)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
