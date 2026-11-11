"""1-minute bars for the lab: Yahoo Finance download (yfinance), CSV load/save, the mid-price and episodes.

Yahoo serves OHLCV bars, not ticks and not quotes, so there is no bid/ask to average. The mid is therefore a proxy:

    close   (default) the last trade of the minute: the price at the moment the simulator posts its quotes.
            Off the true mid by at most half the bid/ask spread (1 cent on SPY).
    range   (high + low) / 2: an average over the minute, not a price at one moment. Its changes understate σ
            (by about 19% on a Brownian path), so it is offered only for comparison.

A CSV with `bid` and `ask` columns (real quotes) gets the true mid, (bid + ask) / 2, whatever the choice.

Yahoo limits: 1-minute bars exist only for the last 30 days, and one request returns at most 7 days of them, so
download_bars fetches in 7-day windows. SPY is regular hours only (09:30-16:00 New York); ETH-USD trades 24/7.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pandas as pd

SYMBOL_ALIASES = {"ETH": "ETH-USD", "BTC": "BTC-USD"}
MAX_LOOKBACK_DAYS = 29  # Yahoo keeps 1-minute bars for 30 days
MAX_WINDOW_DAYS = 7  # and returns at most 7 days of them per request
COLUMNS = ["open", "high", "low", "close", "volume"]
QUOTE_COLUMNS = ["bid", "ask"]  # optional, kept when present
MID_SOURCES = ("close", "range")


def resolve_symbol(symbol: str) -> str:
    s = symbol.strip().upper()
    return SYMBOL_ALIASES.get(s, s)


def session_timezone(symbol: str) -> str:
    """Episodes are calendar days in this timezone: New York for US equities, UTC for 24/7 crypto pairs."""
    return "UTC" if resolve_symbol(symbol).endswith("-USD") else "America/New_York"


def download_bars(symbol: str, days: int = 7, end=None, regular_hours: bool = True, yf_module=None) -> pd.DataFrame:
    """The last `days` days of 1-minute bars for `symbol` (SPY, ETH, ETH-USD, ...), oldest first.

    Raises RuntimeError when Yahoo returns nothing: yfinance reports network and symbol errors as an empty frame.
    """
    if not 1 <= days <= MAX_LOOKBACK_DAYS:
        raise ValueError(f"days must be between 1 and {MAX_LOOKBACK_DAYS}: Yahoo keeps 1-minute bars for 30 days")
    if yf_module is None:
        import yfinance as yf_module  # imported here so the rest of the lab runs without it

    sym = resolve_symbol(symbol)
    end_ts = pd.Timestamp.now(tz="UTC") if end is None else pd.Timestamp(end)
    end_ts = (end_ts.tz_localize("UTC") if end_ts.tzinfo is None else end_ts.tz_convert("UTC")).floor("min")
    start_ts = end_ts - pd.Timedelta(days=days)

    ticker = yf_module.Ticker(sym)
    frames = []
    window_start = start_ts
    while window_start < end_ts:
        window_end = min(window_start + pd.Timedelta(days=MAX_WINDOW_DAYS), end_ts)
        part = ticker.history(
            start=window_start.to_pydatetime(),
            end=window_end.to_pydatetime(),
            interval="1m",
            prepost=not regular_hours,
            actions=False,
            auto_adjust=False,
        )
        if part is not None and len(part):
            frames.append(part)
        window_start = window_end
    if not frames:
        raise RuntimeError(
            f"Yahoo returned no 1-minute bars for {sym} between {start_ts:%Y-%m-%d %H:%M} and {end_ts:%Y-%m-%d %H:%M} UTC. "
            "Check network access to query1.finance.yahoo.com and query2.finance.yahoo.com, the symbol, "
            "and that the window lies within the last 30 days."
        )
    bars = pd.concat(frames)
    bars = bars[~bars.index.duplicated(keep="last")].sort_index()
    return _standardise(bars)


def _standardise(bars: pd.DataFrame) -> pd.DataFrame:
    out = bars.rename(columns=lambda c: str(c).strip().lower())
    missing = [c for c in COLUMNS if c not in out.columns]
    if missing:
        raise ValueError(f"bars are missing columns {missing}; expected {COLUMNS}")
    return out[COLUMNS + [c for c in QUOTE_COLUMNS if c in out.columns]].astype(float)


def save_csv(bars: pd.DataFrame, path: str | Path) -> Path:
    """Write bars with UTC ISO timestamps (reload with load_csv)."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    out = bars[[c for c in COLUMNS + QUOTE_COLUMNS if c in bars.columns]].copy()
    out.index = out.index.tz_convert("UTC") if out.index.tz is not None else out.index.tz_localize("UTC")
    out.to_csv(path, index_label="timestamp")
    return path


def load_csv(path: str | Path) -> pd.DataFrame:
    """Bars from a CSV with a time column (timestamp, datetime, date or time), open/high/low/close/volume and,
    optionally, bid/ask. Naive timestamps are read as UTC."""
    raw = pd.read_csv(path)
    time_col = next((c for c in raw.columns if c.strip().lower() in ("timestamp", "datetime", "date", "time")), None)
    if time_col is None:
        raise ValueError(f"{path}: no time column (timestamp, datetime, date or time)")
    index = pd.DatetimeIndex(pd.to_datetime(raw[time_col], utc=True), name="timestamp")
    return _standardise(raw.drop(columns=[time_col]).set_index(index)).sort_index()


def prepare(bars: pd.DataFrame, tz: str, mid_source: str = "close") -> pd.DataFrame:
    """Clean bars and add the columns the lab needs:

    mid      the mid-price at the bar's close: (bid + ask) / 2 when the bars carry quotes, else the proxy chosen
             by `mid_source` ("close" or "range", see the module docstring)
    episode  calendar day of the bar in `tz` (a trading session for SPY, a UTC day for crypto)
    t_min    minutes since the episode's first bar (gaps are kept as real time)
    """
    if mid_source not in MID_SOURCES:
        raise ValueError(f"mid_source must be one of {MID_SOURCES}")
    df = _standardise(bars)
    df.index = df.index.tz_convert(tz) if df.index.tz is not None else df.index.tz_localize("UTC").tz_convert(tz)
    df = df[~df.index.duplicated(keep="last")].sort_index()
    ok = df[["open", "high", "low", "close"]].notna().all(axis=1) & (df["low"] > 0) & (df["high"] >= df["low"])
    df = df.loc[ok].copy()
    if {"bid", "ask"} <= set(df.columns):
        df["mid"] = (df["bid"] + df["ask"]) / 2.0
    elif mid_source == "close":
        df["mid"] = df["close"]
    else:
        df["mid"] = (df["high"] + df["low"]) / 2.0
    df["episode"] = [ts.date().isoformat() for ts in df.index]
    first = pd.Series(df.index, index=df.index).groupby(df["episode"]).transform("min")
    df["t_min"] = (df.index - pd.DatetimeIndex(first)).total_seconds() / 60.0
    return df


def synthetic_bars(
    days: int = 5,
    s0: float = 500.0,
    sigma_per_minute: float = 0.15,
    minutes_per_day: int = 390,
    substeps: int = 30,
    seed: int = 7,
    tz: str = "America/New_York",
    start: str = "2026-01-05 09:30",
) -> pd.DataFrame:
    """Synthetic 1-minute OHLCV bars from an arithmetic Brownian motion sampled `substeps` times per minute, on
    consecutive weekdays. For offline runs and tests only: calibration needs real data."""
    rng = np.random.default_rng(seed)
    day = pd.Timestamp(start, tz=tz)
    frames, s = [], s0
    for _ in range(days):
        while day.weekday() >= 5:
            day += pd.Timedelta(days=1)
        steps = rng.normal(0.0, sigma_per_minute / np.sqrt(substeps), size=(minutes_per_day, substeps))
        path = s + np.cumsum(steps.ravel()).reshape(minutes_per_day, substeps)
        opens = np.concatenate([[s], path[:-1, -1]])
        highs = np.maximum(opens, path.max(axis=1))
        lows = np.minimum(opens, path.min(axis=1))
        index = pd.DatetimeIndex([day + pd.Timedelta(minutes=m) for m in range(minutes_per_day)], name="timestamp")
        frames.append(pd.DataFrame({"open": opens, "high": highs, "low": lows, "close": path[:, -1], "volume": 1000.0}, index=index))
        s = path[-1, -1]
        day += pd.Timedelta(days=1)
    return pd.concat(frames)
