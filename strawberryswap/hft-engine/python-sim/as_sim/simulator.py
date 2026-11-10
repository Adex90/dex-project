"""Monte Carlo of A-S quoting along a historical mid path, the experiment of Avellaneda & Stoikov (2008) §3.

At the close of each bar the market maker posts a 1-lot bid and ask from the mid and σ known then. During the next
interval each quote is hit with probability 1 − exp(−A·e^(−kδ)·dt), δ being its distance from that mid. Inventory
and cash update, and P&L is marked to the next mid. The price path is history; only the fills are random.

Two strategies are compared, as in the paper:
    inventory   quotes around the reservation price r = s − qγσ²τ (inventory skew)
    symmetric   the same total spread centred on the mid (no skew): the benchmark

Every γ and both strategies see the same random draws (common random numbers), so differences between rows are the
strategy's doing, not luck. Each episode (a trading day) starts flat, at q = 0.
"""

from __future__ import annotations

from dataclasses import dataclass, replace

import numpy as np
import pandas as pd

from .model import fill_probability, quotes

STRATEGIES = ("inventory", "symmetric")
MIN_BARS = 60  # shorter episodes (half days with gaps, partial downloads) are skipped


@dataclass(frozen=True)
class SimConfig:
    gamma: float  # risk aversion, 1 / (price · lot)
    k: float  # arrival decay, 1 / price
    A: float  # fills per minute at the mid
    horizon: str | float = "session"  # "session": τ = minutes to the episode's end (A-S finite horizon);
    #                                   a number: rolling τ in minutes, re-armed every bar (as in the C++ engine)
    lot: float = 1.0
    tick: float = 0.01
    fee_bps: float = 0.0  # on fill notional; negative for a maker rebate
    max_inventory: float | None = None  # |q| cap: the side that would breach it is not quoted
    n_paths: int = 500
    seed: int = 7


@dataclass
class SimResult:
    episodes: pd.DataFrame  # one row per (γ, strategy, episode)
    summary: pd.DataFrame  # one row per (γ, strategy), pooled over episodes and paths
    path: pd.DataFrame  # path 0 of the first episode, minute by minute, for both strategies at one γ


def simulate_episode(mid, t_min, sigma, cfg: SimConfig, uniforms: np.ndarray, strategy: str, record_path: bool = False):
    """Simulate `cfg.n_paths` fill paths along one episode. `uniforms` has shape (2, len(mid) − 1, n_paths):
    the draws for ask and bid fills. Returns per-path arrays and, if asked, path 0 minute by minute."""
    if strategy not in STRATEGIES:
        raise ValueError(f"strategy must be one of {STRATEGIES}")
    mid, t_min, sigma = (np.asarray(a, dtype=float) for a in (mid, t_min, sigma))
    n, paths = len(mid), cfg.n_paths
    fee = cfg.fee_bps / 1e4
    q = np.zeros(paths)
    cash = np.zeros(paths)
    max_abs_q = np.zeros(paths)
    buys = np.zeros(paths)
    sells = np.zeros(paths)
    spread_sum = np.zeros(paths)
    rows = []
    t_end = t_min[-1]
    for i in range(n - 1):
        s, dt = mid[i], t_min[i + 1] - t_min[i]
        tau = (t_end - t_min[i]) if cfg.horizon == "session" else float(cfg.horizon)
        skew_q = q if strategy == "inventory" else 0.0
        r, bid, ask = quotes(s, skew_q, cfg.gamma, sigma[i], tau, cfg.k, cfg.tick)
        bid = np.broadcast_to(bid, (paths,))
        ask = np.broadcast_to(ask, (paths,))
        sell = uniforms[0, i] < fill_probability(ask - s, cfg.A, cfg.k, dt)
        buy = uniforms[1, i] < fill_probability(s - bid, cfg.A, cfg.k, dt)
        if cfg.max_inventory is not None:
            sell &= q - cfg.lot >= -cfg.max_inventory - 1e-12
            buy &= q + cfg.lot <= cfg.max_inventory + 1e-12
        q = q + cfg.lot * (buy.astype(float) - sell.astype(float))
        cash = cash + cfg.lot * (np.where(sell, ask * (1.0 - fee), 0.0) - np.where(buy, bid * (1.0 + fee), 0.0))
        np.maximum(max_abs_q, np.abs(q), out=max_abs_q)
        buys += buy
        sells += sell
        spread_sum += ask - bid
        if record_path:
            rows.append(
                {
                    "t_min": t_min[i],
                    "mid": s,
                    "sigma": sigma[i],
                    "tau_min": tau,
                    "reservation": float(np.broadcast_to(r, (paths,))[0]),
                    "bid": float(bid[0]),
                    "ask": float(ask[0]),
                    "spread": float(ask[0] - bid[0]),
                    "bought": bool(buy[0]),
                    "sold": bool(sell[0]),
                    "inventory": float(q[0]),
                    "cash": float(cash[0]),
                    "pnl_mtm": float(cash[0] + q[0] * mid[i + 1]),
                }
            )
    metrics = {
        "pnl": cash + q * mid[-1],
        "final_q": q,
        "max_abs_q": max_abs_q,
        "fills": buys + sells,
        "spread": spread_sum / max(n - 1, 1),
    }
    return metrics, (pd.DataFrame(rows) if record_path else None)


def run(df: pd.DataFrame, sigma: pd.Series, base: SimConfig, gammas, strategies=STRATEGIES) -> SimResult:
    """Run every γ in `gammas` and every strategy over every episode of `df` (from data.prepare). The example path
    is recorded for the first γ."""
    gammas = [float(g) for g in gammas]
    rng = np.random.default_rng(base.seed)
    pooled = {(g, s): [] for g in gammas for s in strategies}
    episode_rows, path_frames = [], []
    first_episode = True
    for episode, ep in df.groupby("episode", sort=True):
        if len(ep) < MIN_BARS:
            continue
        uniforms = rng.random((2, len(ep) - 1, base.n_paths))  # common random numbers
        sig = sigma.loc[ep.index].to_numpy()
        for g in gammas:
            cfg = replace(base, gamma=g)
            for strategy in strategies:
                record = first_episode and g == gammas[0]
                m, path = simulate_episode(ep["mid"], ep["t_min"], sig, cfg, uniforms, strategy, record_path=record)
                pooled[(g, strategy)].append(m)
                episode_rows.append(
                    {
                        "gamma": g,
                        "strategy": strategy,
                        "episode": episode,
                        "bars": len(ep),
                        "pnl_mean": m["pnl"].mean(),
                        "pnl_std": m["pnl"].std(),
                        "final_q_std": m["final_q"].std(),
                        "max_abs_q_mean": m["max_abs_q"].mean(),
                        "fills_mean": m["fills"].mean(),
                        "spread_mean": m["spread"].mean(),
                    }
                )
                if record:
                    path.insert(0, "timestamp", ep.index[: len(path)])
                    path.insert(1, "strategy", strategy)
                    path_frames.append(path)
        first_episode = False
    if not episode_rows:
        raise ValueError(f"no episode has at least {MIN_BARS} bars")

    summary_rows = []
    for (g, strategy), ms in pooled.items():
        pnl = np.concatenate([m["pnl"] for m in ms])
        fills = np.concatenate([m["fills"] for m in ms])
        summary_rows.append(
            {
                "gamma": g,
                "strategy": strategy,
                "episodes": len(ms),
                "pnl_mean": pnl.mean(),
                "pnl_std": pnl.std(),
                "pnl_sharpe": pnl.mean() / pnl.std() if pnl.std() > 0 else np.nan,
                "final_q_std": np.concatenate([m["final_q"] for m in ms]).std(),
                "max_abs_q_mean": np.concatenate([m["max_abs_q"] for m in ms]).mean(),
                "fills_mean": fills.mean(),
                "pnl_per_fill": pnl.mean() / fills.mean() if fills.mean() > 0 else np.nan,
                "spread_mean": np.concatenate([m["spread"] for m in ms]).mean(),
            }
        )
    return SimResult(
        episodes=pd.DataFrame(episode_rows),
        summary=pd.DataFrame(summary_rows).sort_values(["gamma", "strategy"], ignore_index=True),
        path=pd.concat(path_frames, ignore_index=True) if path_frames else pd.DataFrame(),
    )
