"""PHASE 8 AUDIT — H-4: sandwich of an intent by the holder of its opening, before/after patch P-1. Run: python3 sandwich.py"""
from model import *
import copy
E = 10**18
P = 2000 * E

def fresh():
    p = Pool(1_000 * E, 2_000_000 * E)
    p.add('lp', 100 * E, 200_000 * E)          # spot == oracle == 2000
    return p

def dV(p_before, p_after):                     # pool value change at oracle price (token1), = -(trader gain vs P)
    return (p_after.r0 - p_before.r0) * P // WAD + (p_after.r1 - p_before.r1)

def flash_harvest_to_P(p):
    """Searcher moves the pool back to P through flashRebalance: returns harvest (pool value loss at P)."""
    q = copy.deepcopy(p); before = copy.deepcopy(q)
    arb_to(q, P, q.batch_sell0, q.batch_sell1)  # same curve maths; flashRebalance differs only in WHO gets paid
    return -dV(before, q), q

def band_ok(p, band_bps):
    s = p.spot(); return abs(s - P) * BPS <= band_bps * P

v, slip = 10 * E, 100                          # victim sells 10 token0, 1% slippage tolerance (100 bps)
quote = fresh().batch_sell0(v)                 # what the victim expects (quote at submission)
minOut = quote * (BPS - slip) // BPS
print(f"victim: sells {v/E} token0, quote {quote/E:,.4f} token1, minOut {minOut/E:,.4f}")

# baseline: no attacker; searchers then arb back via flashRebalance (Treasury gets 80% of the harvest)
p = fresh(); got0 = p.batch_sell0(v); H0, _ = flash_harvest_to_P(p)
print(f"baseline: victim gets {got0/E:,.4f}; harvest after victim {H0/E:,.4f} -> Treasury {0.8*H0/E:,.4f}")

def sandwich(A0, backrun):
    p = fresh(); s0 = copy.deepcopy(p)
    o_front = p.batch_sell0(A0)                # attacker front-run (own intent, own batch)
    got = p.batch_sell0(v)                     # victim's batch
    if got < minOut: return None
    pre_back = copy.deepcopy(p)
    if backrun == 'batch':                     # UNPATCHED: back-run through settleBatch, zero fee
        d0, d1 = arb_to(p, P, p.batch_sell0, p.batch_sell1)
        atk = (-A0 + d0) * P // WAD + (o_front + d1)
        treas = 0
    else:                                      # PATCHED P-1: back-run batch is rejected (ΔV<0) -> must flashRebalance
        H, p = flash_harvest_to_P(p)
        atk = (-A0) * P // WAD + o_front + H - (H * 8000 // BPS)   # front-run cost + 20% of harvest
        treas = H * 8000 // BPS
    return atk, got, treas

for mode in ['batch', 'flash']:
    best = (0, 0, None)
    lo, hi = 0, 200 * E
    for A0 in [i * E // 4 for i in range(0, 800)]:
        try: r = sandwich(A0, mode)
        except AssertionError: break
        if r and r[0] > best[0]: best = (r[0], A0, r)
    atk, A0, r = best
    if r is None:
        print(f"[{mode}] no profitable front-run exists"); continue
    print(f"[{'UNPATCHED free batch back-run' if mode=='batch' else 'PATCH P-1 back-run via flashRebalance'}] "
          f"best front-run {A0/E:.2f} token0: attacker +{atk/E:,.4f}, victim gets {r[1]/E:,.4f} "
          f"(loses {(got0-r[1])/E:,.4f} = {(got0-r[1])*BPS/got0:.1f} bps), Treasury {r[2]/E:,.4f}")

# PATCH P-1 + band: post-batch spot within 50 bps of the oracle
band = 50
p = fresh(); ok_victim_alone = (p.batch_sell0(v) and band_ok(p, band))
print(f"\nP-1 + 50 bps band: victim alone passes band check: {ok_victim_alone}")
maxA = 0
for A0 in [i * E // 20 for i in range(0, 4000)]:
    q = fresh()
    try: q.batch_sell0(A0)
    except AssertionError: break
    if not band_ok(q, band): break
    q.batch_sell0(v)
    if band_ok(q, band): maxA = A0
print(f"largest front-run that still lets the victim's batch pass the band: {maxA/E:.2f} token0")
