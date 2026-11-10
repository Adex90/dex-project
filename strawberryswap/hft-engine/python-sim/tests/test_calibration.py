import unittest

import numpy as np
import pandas as pd

from as_sim import calibration, data


def flat_mid_bars(up, down, mid=100.0):
    """One episode of consecutive minutes whose closes stay at `mid` while highs/lows reach the given excursions."""
    n = len(up)
    index = pd.date_range("2026-01-05 00:00", periods=n, freq="1min", tz="UTC")
    bars = pd.DataFrame({"open": mid, "high": mid + up, "low": mid - down, "close": mid, "volume": 1.0}, index=index)
    return data.prepare(bars, "UTC")


def sample_excursions(rng, n, A, k):
    """Excursions with P(U ≥ δ) = 1 − exp(−A·e^(−kδ)): exactly the touch law fit_intensity assumes."""
    v = rng.random(n)
    return np.maximum(-np.log(-np.log(v) / A) / k, 0.0)


class SigmaTest(unittest.TestCase):
    def test_recovers_the_volatility_of_a_random_walk(self):
        df = data.prepare(data.synthetic_bars(days=10, sigma_per_minute=0.2, seed=3), "America/New_York")
        self.assertAlmostEqual(calibration.sigma_per_minute(df), 0.2, delta=0.2 * 0.04)

    def test_range_proxy_understates_volatility(self):
        bars = data.synthetic_bars(days=10, sigma_per_minute=0.2, seed=3)
        sig_range = calibration.sigma_per_minute(data.prepare(bars, "America/New_York", mid_source="range"))
        self.assertLess(sig_range, 0.9 * 0.2)

    def test_ewma_uses_only_the_past(self):
        df = data.prepare(data.synthetic_bars(days=2, seed=5), "America/New_York")
        before = calibration.ewma_sigma(df, halflife_minutes=20)
        changed = df.copy()
        changed.iloc[500:, changed.columns.get_loc("mid")] += 5.0  # a jump at bar 500
        after = calibration.ewma_sigma(changed, halflife_minutes=20)
        np.testing.assert_array_equal(before.iloc[:500].to_numpy(), after.iloc[:500].to_numpy())
        self.assertGreater(after.iloc[500], before.iloc[500])

    def test_ewma_of_constant_moves_is_that_move(self):
        n = 200
        index = pd.date_range("2026-01-05", periods=n, freq="1min", tz="UTC")
        mid = 100 + 0.3 * (np.arange(n) % 2)  # ±0.3 every minute
        bars = pd.DataFrame({"open": mid, "high": mid, "low": mid, "close": mid, "volume": 1.0}, index=index)
        np.testing.assert_allclose(calibration.ewma_sigma(data.prepare(bars, "UTC")).to_numpy(), 0.3)


class IntensityTest(unittest.TestCase):
    def test_recovers_A_and_k_from_touches(self):
        rng = np.random.default_rng(11)
        A, k, n = 2.0, 10.0, 40_000
        df = flat_mid_bars(sample_excursions(rng, n, A, k), sample_excursions(rng, n, A, k))
        fit = calibration.fit_intensity(df)
        self.assertAlmostEqual(fit.A, A, delta=0.1 * A)
        self.assertAlmostEqual(fit.k, k, delta=0.1 * k)
        self.assertTrue(fit.table["used"].sum() >= 3)

    def test_needs_enough_bars(self):
        with self.assertRaises(ValueError):
            calibration.fit_intensity(flat_mid_bars(np.full(10, 0.1), np.full(10, 0.1)))

    def test_kappa_mle_matches_the_cpp_estimator(self):
        depths = np.random.default_rng(2).exponential(scale=1 / 4.0, size=200_000)
        self.assertAlmostEqual(calibration.kappa_mle(depths), 4.0, delta=0.08)


if __name__ == "__main__":
    unittest.main()
