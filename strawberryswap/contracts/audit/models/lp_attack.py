"""PHASE 8 AUDIT — C-2: flash-liquidity attack on fixed virtual reserves (exact integers). Run: python3 lp_attack.py"""
from model import *
E = 10**18
def scenario(f_num, f_den, scale=False, P_launch=2000, P_mkt=2400, verbose=False):
    p = Pool(1_000 * E, 2_000_000 * E, scale_virtual=scale)
    p.add('honest', 100 * E, 200_000 * E)            # launch-ratio deposit
    P = P_mkt * E
    arb_to(p, P, p.batch_sell0, p.batch_sell1)       # market moved: someone arbs the pool to 2400
    V_honest0 = p.value_real(P) * p.shares['honest'] // p.ts
    r0, r1 = p.r0, p.r1
    # ---- attacker, one transaction, flash-loaned capital
    a0, a1 = r0 * f_num // f_den, r1 * f_num // f_den
    spot0 = p.spot()
    sh = p.add('atk', a0, a1)
    spot1 = p.spot()
    d0a, d1a = arb_to(p, P, p.batch_sell0, p.batch_sell1)
    spotArb = p.spot()
    b0, b1 = p.remove('atk', sh)
    spot2 = p.spot()
    d0b, d1b = arb_to(p, P, p.batch_sell0, p.batch_sell1)
    net0 = -a0 + d0a + b0 + d0b
    net1 = -a1 + d1a + b1 + d1b
    profit = net0 * P // WAD + net1
    V_honest1 = p.value_real(P) * p.shares['honest'] // p.ts
    if verbose:
        print(f"  pool before attack: r0={r0/E:,.4f} r1={r1/E:,.2f}  spot={spot0/E:,.2f}  (real ratio r1/r0={r1/r0:,.0f}, virtual ratio={p.V1/p.V0:,.0f})")
        print(f"  flash-borrowed: {a0/E:,.2f} token0 + {a1/E:,.0f} token1  (~{(a0*P//WAD+a1)/E:,.0f} token1 of value)")
        print(f"  spot after add={spot1/E:,.2f}   after arb={spotArb/E:,.2f}   after remove={spot2/E:,.2f}   final={p.spot()/E:,.2f}")
        print(f"  attacker net: token0 {net0/E:+,.6f}  token1 {net1/E:+,.2f}  => profit {profit/E:+,.2f} token1 @P")
        print(f"  honest LP value @P: {V_honest0/E:,.2f} -> {V_honest1/E:,.2f}  ({100*(V_honest1-V_honest0)/V_honest0:+.2f}%)")
    return profit, V_honest0, V_honest1

print("== UNPATCHED: fixed virtual reserves, arb through settleBatch (zero fee, no oracle)")
best = None
for f in [1, 2, 5, 10, 20, 50, 100, 200, 1000]:
    pr, v0, v1 = scenario(f, 1)
    print(f"f={f:5d}: attacker profit {pr/E:14,.2f}   honest LP loss {(v0-v1)/E:14,.2f} ({100*(v0-v1)/v0:5.2f}% of their position)")
print("\nDetail f=100:"); scenario(100, 1, verbose=True)
print("\n== PATCHED P-3 (virtual reserves ∝ totalShares): same attack")
for f in [1, 10, 100, 1000]:
    try:
        pr, v0, v1 = scenario(f, 1, scale=True)
        print(f"f={f:5d}: attacker profit {pr/E:14,.6f}   honest LP change {(v1-v0)/E:+,.6f}")
    except AssertionError as e:
        print(f"f={f}: assertion {e}")
