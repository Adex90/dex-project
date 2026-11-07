"""Exact-integer model of DarkPool (mirrors contracts/src/dex/DarkPool.sol and circuits/batch_swap pricing).
All amounts in wei (1e18 = one token). Python ints are unbounded, so every floor() below is the Solidity floor."""
from math import isqrt
WAD = 10**18
BPS = 10_000

class Pool:
    def __init__(s, v0, v1, scale_virtual=False):
        s.V0, s.V1 = v0, v1           # immutables (unpatched) / per-share basis (patched)
        s.r0 = s.r1 = 0
        s.ts = 0
        s.shares = {}
        s.scale = scale_virtual       # PATCH P-3: virtual reserves proportional to totalShares
        s.ts0 = None
    # ---- virtual reserves
    def v(s):
        if not s.scale or s.ts0 is None: return s.V0, s.V1
        return s.V0 * s.ts // s.ts0, s.V1 * s.ts // s.ts0
    def xy(s):
        v0, v1 = s.v(); return s.r0 + v0, s.r1 + v1
    def spot(s):
        x, y = s.xy(); return y * WAD // x
    def K(s):
        x, y = s.xy(); return x * y
    # ---- liquidity (DarkPool.addLiquidity / removeLiquidity)
    def add(s, who, a0, a1):
        if s.ts == 0:
            m = isqrt(a0 * a1); assert m > 1000; m -= 1000
            s.ts = 1000; s.shares[None] = 1000; s.ts0 = None
        else:
            m = min(a0 * s.ts // s.r0, a1 * s.ts // s.r1); assert m > 0
        s.ts += m; s.shares[who] = s.shares.get(who, 0) + m
        s.r0 += a0; s.r1 += a1
        if s.ts0 is None: s.ts0 = s.ts
        return m
    def remove(s, who, sh):
        a0 = s.r0 * sh // s.ts; a1 = s.r1 * sh // s.ts
        s.shares[who] -= sh; s.ts -= sh; s.r0 -= a0; s.r1 -= a1
        return a0, a1
    # ---- one-intent batch, exactly as batch_swap prices it (zero fee, NO oracle)
    def batch_sell0(s, A0):
        X, Y = s.xy(); pLo = WAD * Y // (X + A0); o1 = A0 * pLo // WAD
        assert o1 < s.r1 + 0, "pool must hold the real token1"
        kb = X * Y; s.r0 += A0; s.r1 -= o1; assert s.K() >= kb
        return o1
    def batch_sell1(s, A1):
        X, Y = s.xy(); pLo = WAD * (Y + A1) // X; pHi = pLo + 1; o0 = A1 * WAD // pHi
        assert o0 < s.r0
        kb = X * Y; s.r1 += A1; s.r0 -= o0; assert s.K() >= kb
        return o0
    # ---- value at oracle price P (token1 units), DarkPool._value on EFFECTIVE reserves
    def value_real(s, P):
        return s.r0 * P // WAD + s.r1

def arb_to(pool, P, sell_fn0, sell_fn1, max_frac=0.999999):
    """Trade the pool to oracle price P with zero-fee one-intent batches. Returns (d0, d1) = attacker token deltas."""
    x, y = pool.xy(); K = x * y
    if y * WAD // x > P:      # pool overprices token0: sell token0 in
        xt = isqrt(K * WAD // P)
        A0 = xt - x
        o1 = sell_fn0(A0); return (-A0, o1)
    else:                     # pool underprices token0: sell token1 in, take token0 (capped by real r0)
        v0, v1 = pool.v()
        xt = max(isqrt(K * WAD // P), v0 + pool.r0 // 10**6)  # cannot take more than real token0
        yt = K // xt + 1
        A1 = yt - y
        o0 = sell_fn1(A1); return (o0, -A1)
