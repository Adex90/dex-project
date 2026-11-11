"""Calibrating the A-S inputs from 1-minute bars: σ, the volatility of the mid, and the order-arrival curve
λ(δ) = A·e^(−kδ). γ is not estimated from data; it is the risk preference the simulator's γ sweep helps choose.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import pandas as pd


def increments(df: pd.DataFrame, column: str = "mid") -> pd.DataFrame:
    """Changes of `column` between consecutive bars of the same episode, with their spacing in minutes.
    Gaps between episodes (overnight, weekends) are excluded."""
    g = df.groupby("episode", sort=False)
    out = pd.DataFrame({"d": g[column].diff(), "dt": g["t_min"].diff()}, index=df.index)
    return out[out["dt"] > 0]


def sigma_per_minute(df: pd.DataFrame, column: str = "mid") -> float:
    """Volatility of an arithmetic random walk, in price per √minute: the RMS of Δprice/√dt.
    RMS rather than standard deviation, as in the C++ VarianceEstimator: at 1-minute scale the drift is noise."""
    inc = increments(df, column)
    if inc.empty:
        raise ValueError("need at least two bars in one episode")
    return float(np.sqrt(np.mean(inc["d"].to_numpy() ** 2 / inc["dt"].to_numpy())))


def ewma_sigma(df: pd.DataFrame, halflife_minutes: float = 30.0, warmup_increments: int = 30) -> pd.Series:
    """σ known at the close of each bar, from that bar and earlier ones only (no look-ahead).

    Same recursion as the C++ VarianceEstimator, in minutes: var ← (1 − w)·var + w·Δmid²/dt with
    w = 1 − exp(−ln2·dt/halflife). It carries across episodes, skipping the overnight gaps. It starts from the RMS
    of the first `warmup_increments` changes: the only step that uses data before it would be known.
    """
    g = df.groupby("episode", sort=False)
    d = g["mid"].diff().to_numpy()
    dt = g["t_min"].diff().to_numpy()
    valid = np.isfinite(d) & (np.nan_to_num(dt) > 0)
    z2 = np.where(valid, d**2 / np.where(valid, dt, 1.0), np.nan)
    first = z2[valid][:warmup_increments]
    if first.size == 0:
        raise ValueError("need at least two bars in one episode")
    var = float(np.mean(first))
    lam = np.log(2.0) / halflife_minutes
    out = np.empty(len(df))
    for i in range(len(df)):
        if valid[i]:
            w = -np.expm1(-lam * dt[i])
            var = (1.0 - w) * var + w * z2[i]
        out[i] = np.sqrt(var)
    return pd.Series(out, index=df.index, name="sigma")


@dataclass(frozen=True)
class IntensityFit:
    A: float  # fills per minute for a quote at the mid (δ = 0)
    k: float  # 1 / price
    table: pd.DataFrame  # delta, p_touch, lambda_per_min, used


def touch_excursions(df: pd.DataFrame) -> tuple[np.ndarray, np.ndarray]:
    """How far each 1-minute bar traded above and below the previous bar's closing mid, where the simulator posts its
    quotes: the distance a quote would have been reached (high − mid₋₁ on the ask side, mid₋₁ − low on the bid
    side), clipped at zero."""
    g = df.groupby("episode", sort=False)
    prev_mid = g["mid"].shift(1)
    one_minute = g["t_min"].diff().round(6) == 1.0
    up = (df["high"] - prev_mid)[one_minute].clip(lower=0).to_numpy()
    down = (prev_mid - df["low"])[one_minute].clip(lower=0).to_numpy()
    return up, down


def fit_intensity(df: pd.DataFrame, n_grid: int = 30, p_min: float = 0.01, p_max: float = 0.95) -> IntensityFit:
    """Fit λ(δ) = A·e^(−kδ) from how often the price TOUCHED each distance δ from the prior mid within a minute.

    For each δ on a grid, p(δ) is the share of bar sides (bid and ask pooled; the model is symmetric) that reached δ.
    With Poisson arrivals, p = 1 − exp(−λ·1 min), so λ(δ) = −ln(1 − p), and ln λ = ln A − kδ is a straight line.

    A touch is not a fill: there is no queue position, size or competition in 1-minute bars. Treat A and k as an
    optimistic upper bound on the fill rate; refine k from real executions with kappa_mle when the engine has them.
    """
    up, down = touch_excursions(df)
    x = np.sort(np.concatenate([up, down]))
    if x.size < 100:
        raise ValueError("need at least 50 consecutive 1-minute bars to fit the arrival curve")
    grid = np.linspace(0.0, float(np.quantile(x, 0.99)), n_grid)
    p = 1.0 - np.searchsorted(x, grid, side="left") / x.size  # share with excursion ≥ δ
    with np.errstate(divide="ignore"):
        lam = -np.log1p(-np.minimum(p, 1.0 - 1e-12))
    used = (p >= p_min) & (p <= p_max)
    if used.sum() < 3:
        raise ValueError("too few distances with a usable touch frequency to fit the arrival curve")
    slope, intercept = np.polyfit(grid[used], np.log(lam[used]), 1)
    if slope >= 0:
        raise ValueError("touch frequency does not fall with distance; cannot fit λ(δ) = A·e^(−kδ)")
    table = pd.DataFrame({"delta": grid, "p_touch": p, "lambda_per_min": lam, "used": used})
    return IntensityFit(A=float(np.exp(intercept)), k=float(-slope), table=table)


def kappa_mle(depths) -> float:
    """κ̂ = 1 / mean(depth) for execution depths |trade price − mid|: the maximum-likelihood estimate when depths are
    Exponential(κ), as in the C++ KappaEstimator. Use it on real trade prints or the engine's own fills."""
    d = np.asarray(depths, dtype=float)
    d = d[np.isfinite(d)]
    if d.size == 0:
        raise ValueError("no depths")
    return float(1.0 / max(np.mean(np.abs(d)), 1e-12))
