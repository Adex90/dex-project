"""Avellaneda–Stoikov (2008) closed forms, written exactly as in bots/inventory-cpp (avellaneda_stoikov.hpp).

Model: the mid follows dS = σ dW (arithmetic Brownian motion), market orders reach a quote at distance δ from the mid
with Poisson intensity λ(δ) = A·exp(−kδ), and the market maker has CARA utility with risk aversion γ:

    reservation price      r(s, q, τ) = s − q·γ·σ²·τ
    optimal total spread   δa + δb    = γ·σ²·τ + (2/γ)·ln(1 + γ/k)
    quotes                 bid = r − spread/2,   ask = r + spread/2

Units in this lab (time in MINUTES; calibration.json converts σ² and τ to the engine's seconds):

    s, r, bid, ask   price, in the quote currency (USD)
    q                inventory, signed, in lots of the base asset (long > 0)
    σ                price per √minute (σ² in price² per minute)
    τ = T − t        minutes
    γ                1 / (price · lot)
    k                1 / price
    A                fills per minute for a quote at the mid (δ = 0)
"""

from __future__ import annotations

import numpy as np


def reservation_price(mid, inventory, gamma: float, sigma, tau):
    """r = s − q·γ·σ²·τ: a long inventory pulls r below the mid (keener to sell), a short one above it."""
    return mid - inventory * gamma * np.square(sigma) * tau


def optimal_spread(gamma: float, sigma, tau, k: float):
    """Total A-S spread δa + δb = γσ²τ + (2/γ)·ln(1 + γ/k). As γ → 0 it tends to 2/k (the risk-neutral spread)."""
    if gamma <= 0 or k <= 0:
        raise ValueError("gamma and k must be positive")
    return gamma * np.square(sigma) * tau + (2.0 / gamma) * np.log1p(gamma / k)


def quotes(mid, inventory, gamma: float, sigma, tau, k: float, tick: float = 0.0):
    """Reservation price and the bid/ask around it. Works on scalars or numpy arrays (e.g. one inventory per path).

    Two practical guards the closed form does not have: a quote never crosses the mid (a large inventory moves the
    near-side quote to the mid at most, where it fills at the maximal rate A), and quotes are rounded outward to the
    tick (bid down, ask up), so rounding never makes a quote more aggressive.
    """
    r = reservation_price(mid, inventory, gamma, sigma, tau)
    half = 0.5 * optimal_spread(gamma, sigma, tau, k)
    bid = np.minimum(r - half, mid)
    ask = np.maximum(r + half, mid)
    if tick > 0:
        bid = np.floor(bid / tick + 1e-9) * tick
        ask = np.ceil(ask / tick - 1e-9) * tick
    return r, bid, ask


def fill_probability(delta, A: float, k: float, dt_minutes):
    """P(at least one market order reaches a quote at distance δ ≥ 0 within dt) = 1 − exp(−A·e^(−kδ)·dt)."""
    lam = A * np.exp(-k * np.maximum(delta, 0.0))
    return -np.expm1(-lam * dt_minutes)
