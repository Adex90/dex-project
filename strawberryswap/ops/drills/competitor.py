#!/usr/bin/env python3
"""A competing searcher for the competing-searcher drill (anvil rig only): it races the engine to rebalance DarkPool.

    ops/drills/competitor.py --rpc http://127.0.0.1:8545 --pool POOL --executor EXECUTOR --operator ADDRESS \\
        [--bps 15] [--fraction 0.5] [--every 1.0] [--duration 600]

Every --every seconds it reads the pool's effective reserves and the oracle price; when the pool's spot is more than
--bps away from the oracle, it moves the pool --fraction of the way toward it along the invariant, through its own
SearcherExecutor (operated by one of anvil's unlocked accounts: `cast send --unlocked`, no key), paying the Treasury
exactly what the pool pulls (DarkPool.quoteMinPayment). One JSON line per attempt. It refuses any chain but anvil's.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
import time

WAD = 10**18


def isqrt_ceil(n: int) -> int:
    r = math.isqrt(n)
    return r if r * r == n else r + 1


def deviation_bps(x: int, y: int, price: int) -> int:
    """DarkPool._deviation, in integers as the contract computes it: |spot − P|·10⁴ / P with spot = y·WAD / x."""
    return abs(y * WAD // x - price) * 10_000 // price


def plan(x: int, y: int, r0: int, r1: int, price: int, fraction: float) -> dict | None:
    """One rebalance from effective reserves (x, y) = real (r0, r1) + virtual toward `price` (token1 per token0, WAD):
    the amounts out and in, rounded so that x·y never falls. None when DarkPool would refuse it: the pool already at the
    price, a move too small to lower the contract's integer deviation (it requires a strict decrease), or a reserve
    it would empty."""
    k = x * y
    spot = y * WAD // x
    if spot == price:
        return None
    if spot < price:  # token0 is cheap in the pool: take token0 out, pay token1 in
        target = math.isqrt(k * WAD // price)
        out0 = int((x - target) * fraction)
        if out0 <= 0 or out0 >= r0:
            return None
        x2 = x - out0
        y2 = -(-k // x2) + 1  # ceil(k / x2) + 1 wei: the invariant never falls
        step = {"amount0Out": out0, "amount1Out": 0, "pay0": 0, "pay1": y2 - y, "n0": r0 - out0, "n1": r1 + (y2 - y)}
    else:
        target = isqrt_ceil(k * price // WAD)
        out1 = int((y - target) * fraction)
        if out1 <= 0 or out1 >= r1:
            return None
        y2 = y - out1
        x2 = -(-k // y2) + 1
        step = {"amount0Out": 0, "amount1Out": out1, "pay0": x2 - x, "pay1": 0, "n0": r0 + (x2 - x), "n1": r1 - out1}
    if deviation_bps(x2, y2, price) >= deviation_bps(x, y, price):
        return None
    return step


def cast(*args: str) -> str:
    return subprocess.run(["cast", *args], check=True, capture_output=True, text=True).stdout.strip()


def call(rpc: str, to: str, sig: str, *args: str) -> list[str]:
    return [str(v) for v in json.loads(cast("call", "--rpc-url", rpc, "--json", to, sig, *args))]


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--rpc", required=True)
    p.add_argument("--pool", required=True)
    p.add_argument("--executor", required=True)
    p.add_argument("--operator", required=True, help="an unlocked anvil account: the executor's operator")
    p.add_argument("--bps", type=float, default=15.0)
    p.add_argument("--fraction", type=float, default=0.5)
    p.add_argument("--every", type=float, default=1.0)
    p.add_argument("--duration", type=float, default=600.0)
    args = p.parse_args(argv)
    if cast("chain-id", "--rpc-url", args.rpc) != "31337":
        print("competitor: anvil (chain 31337) only", file=sys.stderr)
        return 2
    feed = call(args.rpc, args.pool, "oracle()(address)")[0]
    deadline = time.monotonic() + args.duration
    while time.monotonic() < deadline:
        x, y = (int(v) for v in call(args.rpc, args.pool, "effectiveReserves()(uint256,uint256)"))
        r0 = int(call(args.rpc, args.pool, "reserve0()(uint128)")[0])
        r1 = int(call(args.rpc, args.pool, "reserve1()(uint128)")[0])
        price = int(call(args.rpc, feed, "latestVerifiedPrice()(uint256,uint64)")[0])
        spot = y * WAD // x
        gap_bps = abs(spot - price) * 10_000 / price
        step = plan(x, y, r0, r1, price, args.fraction) if gap_bps > args.bps else None
        if step:
            fee = int(call(args.rpc, args.pool, "quoteMinPayment(uint256,uint256)(uint256)", str(step["n0"]), str(step["n1"]))[0])
            line = {"event": "competitor", "ts_ms": int(time.time() * 1000), "gap_bps": round(gap_bps, 2), "treasury": fee}
            try:
                cast("send", "--rpc-url", args.rpc, "--unlocked", "--from", args.operator, args.executor,
                     "rebalance(uint256,uint256,uint256,uint256,uint256)", str(step["amount0Out"]), str(step["amount1Out"]),
                     str(step["pay0"]), str(step["pay1"]), str(fee))
                line["result"] = "mined"
            except subprocess.CalledProcessError as e:
                line["result"] = "reverted"  # the engine got there first
                line["error"] = (e.stderr or "").strip().splitlines()[-1:] or ""
            print(json.dumps(line), flush=True)
        time.sleep(args.every)
    return 0


if __name__ == "__main__":
    sys.exit(main())
