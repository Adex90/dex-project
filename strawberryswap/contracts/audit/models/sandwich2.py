"""PHASE 8 AUDIT — H-4 sweep over slippage tolerance x back-run path x band. Run: python3 sandwich2.py"""
from sandwich import *   # re-runs the base report first
print("\n--- sweep: slippage tolerance x back-run path x band (post-batch |spot-P| <= band) ---")
def best_sandwich(slip_bps, mode, band_bps=None):
    q = fresh().batch_sell0(v); mo = q * (BPS - slip_bps) // BPS
    best = (0, 0, 0)
    for A0 in [i * E // 10 for i in range(0, 1500)]:
        p = fresh()
        try:
            of = p.batch_sell0(A0)
            if band_bps is not None and not band_ok(p, band_bps): break
            got = p.batch_sell0(v)
        except AssertionError: break
        if got < mo: break
        if band_bps is not None and not band_ok(p, band_bps): break
        if mode == 'batch':
            d0, d1 = arb_to(p, P, p.batch_sell0, p.batch_sell1)
            atk = (-A0 + d0) * P // WAD + (of + d1)
        else:
            H, _ = flash_harvest_to_P(p)
            atk = (-A0) * P // WAD + of + H - H * 8000 // BPS
        if atk > best[0]: best = (atk, A0, got)
    return best
for slip in [20, 50, 100, 300]:
    for mode, band in [('batch', None), ('flash', None), ('flash', 300)]:
        atk, A0, got = best_sandwich(slip, mode, band)
        tag = {'batch': 'unpatched', 'flash': 'P-1'}[mode] + (f'+band{band}' if band else '')
        extra = (got0 - got) * BPS // got0 if got else 0
        print(f"slippage {slip:3d} bps  {tag:14s}: attacker {atk/E:9.2f}  victim extra loss {extra:3d} bps  (front-run {A0/E:.1f})")
