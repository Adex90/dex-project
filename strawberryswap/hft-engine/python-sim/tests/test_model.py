import math
import unittest

import numpy as np

from as_sim.model import fill_probability, optimal_spread, quotes, reservation_price


class ModelTest(unittest.TestCase):
    def test_matches_the_cpp_engine(self):
        # The same numbers as bots/inventory-cpp/tests/selftest.cpp: γ = 0.1, τ = 60, σ² = 0.05, κ = 1.5.
        gamma, tau, sig2, k = 0.1, 60.0, 0.05, 1.5
        half = 0.5 * optimal_spread(gamma, math.sqrt(sig2), tau, k)
        self.assertAlmostEqual(half, 0.5 * (0.1 * sig2 * 60 + (2 / 0.1) * math.log(1 + 0.1 / k)), places=12)
        self.assertEqual(reservation_price(2000.0, 0, gamma, math.sqrt(sig2), tau), 2000.0)

    def test_inventory_skews_the_reservation_price(self):
        s, gamma, sigma, tau = 100.0, 0.02, 0.15, 120.0
        long_r = reservation_price(s, +5, gamma, sigma, tau)
        short_r = reservation_price(s, -5, gamma, sigma, tau)
        self.assertLess(long_r, s)  # long: keener to sell
        self.assertGreater(short_r, s)
        self.assertAlmostEqual(s - long_r, 5 * gamma * sigma**2 * tau, places=12)

    def test_risk_neutral_limit_is_two_over_k(self):
        self.assertAlmostEqual(optimal_spread(1e-9, 0.15, 100.0, 8.0), 2 / 8.0, places=6)

    def test_spread_narrows_towards_the_horizon(self):
        self.assertGreater(optimal_spread(0.05, 0.15, 300.0, 10.0), optimal_spread(0.05, 0.15, 1.0, 10.0))

    def test_rejects_non_positive_parameters(self):
        with self.assertRaises(ValueError):
            optimal_spread(0.0, 0.15, 10.0, 10.0)
        with self.assertRaises(ValueError):
            optimal_spread(0.1, 0.15, 10.0, 0.0)

    def test_quotes_never_cross_the_mid_and_round_outward(self):
        mid = 100.004
        r, bid, ask = quotes(mid, np.array([0.0, 500.0, -500.0]), 0.05, 0.15, 300.0, 10.0, tick=0.01)
        self.assertTrue(np.all(bid <= mid) and np.all(ask >= mid))
        self.assertEqual(ask[1], 100.01)  # huge long inventory: ask pinned at the mid, rounded up to the tick
        self.assertEqual(bid[2], 100.0)  # huge short inventory: bid pinned at the mid, rounded down
        np.testing.assert_allclose(np.round(bid / 0.01), bid / 0.01, atol=1e-9)
        np.testing.assert_allclose(np.round(ask / 0.01), ask / 0.01, atol=1e-9)

    def test_fill_probability(self):
        self.assertAlmostEqual(fill_probability(0.0, 2.0, 5.0, 1.0), 1 - math.exp(-2.0), places=12)
        self.assertGreater(fill_probability(0.1, 2.0, 5.0, 1.0), fill_probability(0.2, 2.0, 5.0, 1.0))
        self.assertEqual(fill_probability(-1.0, 2.0, 5.0, 1.0), fill_probability(0.0, 2.0, 5.0, 1.0))
        self.assertEqual(fill_probability(0.0, 0.0, 5.0, 1.0), 0.0)


if __name__ == "__main__":
    unittest.main()
