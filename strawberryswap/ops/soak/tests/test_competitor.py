"""The competing searcher's rebalance plan against DarkPool's own rules (flashRebalance): the invariant never falls, the
deviation from the oracle shrinks, and the pool keeps reserves."""

from __future__ import annotations

import random
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "drills"))
import competitor  # noqa: E402

WAD = competitor.WAD


def deviation_bps(x: int, y: int, price: int) -> int:
    """DarkPool._deviation, independently: |spot − P|·10⁴ / P, spot = y·WAD / x."""
    return abs(y * WAD // x - price) * 10_000 // price


class PlanTest(unittest.TestCase):
    def check(self, r0: int, r1: int, v0: int, v1: int, price: int, fraction: float):
        x, y = r0 + v0, r1 + v1
        step = competitor.plan(x, y, r0, r1, price, fraction)
        if step is None:
            return None
        n0, n1 = step["n0"], step["n1"]
        self.assertEqual(n0, r0 - step["amount0Out"] + step["pay0"])
        self.assertEqual(n1, r1 - step["amount1Out"] + step["pay1"])
        self.assertGreater(n0, 0)
        self.assertGreater(n1, 0)
        self.assertGreaterEqual((n0 + v0) * (n1 + v1), x * y, "I1: the invariant never falls")
        self.assertLess(deviation_bps(n0 + v0, n1 + v1, price), deviation_bps(x, y, price), "I3a: the deviation shrinks")
        return step

    def test_both_directions(self):
        r0, r1 = 100 * WAD, 345_678 * WAD
        cheap = self.check(r0, r1, r0, r1, 3_500 * WAD, 0.5)  # spot 3456.78 < 3500: buy token0
        self.assertGreater(cheap["amount0Out"], 0)
        self.assertGreater(cheap["pay1"], 0)
        dear = self.check(r0, r1, r0, r1, 3_400 * WAD, 0.5)  # spot > 3400: sell token0
        self.assertGreater(dear["amount1Out"], 0)
        self.assertGreater(dear["pay0"], 0)
        self.assertIsNone(competitor.plan(2 * WAD, 2 * 3_500 * WAD, WAD, 3_500 * WAD, 3_500 * WAD, 0.5))

    def test_a_move_the_contract_would_refuse_is_not_planned(self):
        # A hair away (< 1 bp): the contract's integer deviation is 0 before and after, and it requires a decrease.
        r0 = 100 * WAD
        x, y, price = 2 * r0, 2 * r0 * 3_500, 3_500 * WAD + 10**14
        self.assertEqual(deviation_bps(x, y, price), 0)
        self.assertIsNone(competitor.plan(x, y, r0, r0 * 3_500, price, 0.5))

    def test_random_pools(self):
        rng = random.Random(11)
        planned = 0
        for _ in range(500):
            r0 = rng.randrange(WAD, 10_000 * WAD)
            spot = rng.randrange(1_000, 5_000)
            r1 = r0 * spot
            price = int(spot * WAD * rng.uniform(0.95, 1.05))
            planned += self.check(r0, r1, r0, r1, price, rng.uniform(0.1, 0.9)) is not None
        self.assertGreater(planned, 400, "nearly every pool off by up to 5 % gets a plan")


if __name__ == "__main__":
    unittest.main()
