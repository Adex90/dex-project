# Avellaneda–Stoikov simulation lab

Python lab for calibrating the market-making engine's inventory control before it is written in Rust. It downloads
1-minute bars (SPY, ETH, …) with yfinance, takes the mid-price, estimates **σ** and the order-arrival curve
**λ(δ) = A·e^(−kδ)**, and simulates the optimal quotes from a flat start (q₀ = 0) over a sweep of risk aversions
**γ**. It writes the chosen inputs to `calibration.json`, in the units of the existing C++ implementation
(`bots/inventory-cpp/include/strawberry/avellaneda_stoikov.hpp`), which uses the same closed forms. A test pins
the two together.

## Quick start

```bash
cd strawberryswap/hft-engine/python-sim
python3 -m venv .venv && . .venv/bin/activate     # Python 3.11+ (tested on 3.13)
pip install -r requirements.txt

python run_sim.py --symbol SPY --days 7           # last 7 days of SPY, regular hours, one episode per session
python run_sim.py --symbol ETH --days 20          # ETH-USD, 24/7, one episode per UTC day (fetched in 7-day windows)
python run_sim.py --symbol ETH --horizon 60       # rolling 60-minute horizon instead of the session's end
python run_sim.py --csv bars.csv --tz UTC         # your own bars (bid/ask columns give the true mid)
python run_sim.py --synthetic                     # offline: simulated random walk, no market data

python -m unittest discover -s tests -t .         # 27 tests, offline
```

`python run_sim.py --help` lists every option (γ grid, k/A overrides, σ mode, lot, tick, fees, inventory cap).

## Model

| | formula |
|---|---|
| reservation price | r = s − q·γ·σ²·τ |
| optimal total spread | δa + δb = γ·σ²·τ + (2/γ)·ln(1 + γ/k) |
| quotes | bid = r − spread/2, ask = r + spread/2 (never crossing the mid; rounded outward to the tick) |
| fills | a quote at distance δ from the mid fills within dt with probability 1 − exp(−A·e^(−kδ)·dt) |

Units: prices in the quote currency; q in lots (signed, long > 0); σ in price per √minute; τ in minutes;
γ in 1/(price·lot); k in 1/price; A in fills per minute. `calibration.json` converts to the C++ engine's seconds.

**Horizon τ.** `session` (default) is the paper's finite horizon: τ is the time left to the episode's last bar,
so the inventory term fades into the close. A number gives a rolling horizon re-armed every bar, which is what the
C++ engine does (`ASParams::horizonSec`). It is the natural choice for 24/7 crypto. γ's effect scales with τ, so
calibrate γ with the τ mode the engine will run.

## Data

Yahoo serves **1-minute OHLCV bars, not ticks or quotes**. It keeps them for 30 days and returns at most 7 days per
request, so the lab fetches 7-day windows. With no bid/ask, the mid is a proxy:

* **close** (default): the last trade of the minute, the price at the moment the simulator posts quotes. It is
  off the true mid by at most half the bid/ask spread.
* **range** (`--mid range`): (high + low) / 2. It averages over the minute rather than marking a point in time.
  On a random walk its changes understate σ by about 19% (0.122 against a true 0.150 on the synthetic path), so it
  is offered for comparison only.

A CSV with `bid` and `ask` columns gets the true mid, (bid + ask)/2. That is the upgrade path once the engine
records quotes. Downloads are cached to `data/` as CSV.

## Calibration

* **σ**: by default an EWMA of Δmid²/dt with a 30-minute half-life, known at each bar from past data only. It is
  the C++ `VarianceEstimator` recursion in minutes. `--sigma-mode constant` uses one in-sample σ instead.
* **A, k** (`touch_curve.csv`): for each distance δ, the share of minutes in which the price traded at least δ above
  (or below) the previous close. With Poisson arrivals that share is 1 − exp(−λ(δ)), so ln λ is a straight line in
  δ. **A touch is not a fill**: there is no queue position, size or competition in 1-minute bars. Treat A and k as an
  optimistic upper bound. Refine k from real executions with `calibration.kappa_mle` (κ̂ = 1/mean depth, the
  C++ `KappaEstimator`).
* **γ** is a risk preference, not a statistic. Without `--gamma`, the lab sweeps a grid around
  **γ\* = 1/(k·σ²·τ̄)**: the γ at which one lot held over the average horizon moves r by the risk-neutral half-spread
  1/k. That keeps the sweep in a useful range whatever the price scale.

## Simulation

The price path is history; only the fills are random. At each bar's close the market maker posts one lot each side
from the mid and σ known then. Fills during the next interval are drawn from the arrival curve, and P&L is marked
to the next mid. Each episode (a session, or a UTC day for crypto) starts flat. Two strategies run side by side, as
in Avellaneda & Stoikov (2008) §3:

* **inventory**: quotes around r (the A-S skew);
* **symmetric**: the same total spread centred on the mid (no skew), the benchmark.

All γ values and both strategies share the same random draws (common random numbers), so differences between rows
come from the strategy, not from luck.

Reference output on the **synthetic** random walk (`--synthetic`, 5 sessions, 500 paths; not market data):

```
  gamma  strategy  pnl_mean  pnl_std  pnl_sharpe  final_q_std  max_abs_q_mean  fills_mean  spread_mean
 0.0047 inventory      32.9    4.491       7.326        3.584           4.994       400.7       0.1766
 0.0047 symmetric     34.68    22.45       1.545        13.98           16.95       396.3       0.1765
  0.016 inventory     29.18    3.082       9.467        2.635           3.583       333.5       0.2288
  0.016 symmetric     34.91    21.15       1.651        13.61           16.48       310.4       0.2283
```

The skew gives up a little mean P&L for far less risk: at γ = 0.016 the P&L standard deviation is 7× lower and
terminal inventory 5× tighter. That is the paper's result.

## Outputs

`results/<label>_<first day>_<last day>/`:

| file | contents |
|---|---|
| `summary.csv` | per (γ, strategy), pooled over episodes and paths: P&L mean/std/ratio, inventory dispersion, fills, quoted spread |
| `episodes.csv` | the same, per episode |
| `path_example.csv` | one fill path of the first episode, minute by minute: mid, σ, τ, r, bid, ask, fills, inventory, P&L |
| `touch_curve.csv` | the arrival-curve fit: δ, touch share, λ, points used |
| `calibration.json` | σ, A, k, horizon, the γ sweep and the highest-P&L/risk γ, plus `engine`: `{gamma, horizonSec, sigma2PerSec, kappa}` for the C++/Rust quotes() |

## Limits

1-minute bars calibrate **risk** (σ, and γ against it) well. They cannot calibrate **microstructure**: queue
position, fill sizes, latency or adverse selection beyond the next minute's move. The touch-based A and k are
optimistic, and quoted spreads come out far wider than SPY's one-cent book, because a minute's price excursion is
the finest scale the data has. For the production engine, fit A and k on tick or L2 data from the venues it will
quote on, or on its own fills. Keep σ and γ from here. Fees are off by default (`--fee-bps`).

**Network.** The lab needs `query1.finance.yahoo.com`, `query2.finance.yahoo.com`, `fc.yahoo.com` and
`finance.yahoo.com` to download bars. Where those are blocked, use `--csv` or `--synthetic`.
