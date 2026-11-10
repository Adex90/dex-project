import unittest
from dataclasses import replace

import numpy as np
import pandas as pd

from as_sim import data, simulator
from as_sim.model import optimal_spread


def setup_df(days=3, sigma=0.15, seed=4):
    df = data.prepare(data.synthetic_bars(days=days, sigma_per_minute=sigma, seed=seed), "America/New_York")
    return df, pd.Series(sigma, index=df.index)


class SimulatorTest(unittest.TestCase):
    def test_no_arrivals_no_trades(self):
        df, sig = setup_df(days=1)
        res = simulator.run(df, sig, simulator.SimConfig(gamma=0.01, k=10.0, A=0.0, n_paths=20), [0.01])
        self.assertTrue((res.summary["fills_mean"] == 0).all())
        self.assertTrue((res.summary["pnl_mean"] == 0).all())

    def test_one_round_trip_earns_the_quoted_spread(self):
        cfg = simulator.SimConfig(gamma=0.01, k=10.0, A=5.0, n_paths=3, tick=0.0)
        uniforms = np.zeros((2, 1, 3))  # both quotes fill
        m, _ = simulator.simulate_episode([100.0, 100.0], [0.0, 1.0], [0.1, 0.1], cfg, uniforms, "inventory")
        spread = optimal_spread(0.01, 0.1, 1.0, 10.0)
        np.testing.assert_allclose(m["final_q"], 0.0)
        np.testing.assert_allclose(m["pnl"], spread)  # bought at the bid, sold at the ask

    def test_inventory_skew_controls_inventory(self):
        # The A-S result: the skewed strategy holds far less inventory than the symmetric one.
        df, sig = setup_df(days=3)
        res = simulator.run(df, sig, simulator.SimConfig(gamma=0.02, k=12.0, A=2.0, n_paths=300), [0.02])
        s = res.summary.set_index("strategy")
        self.assertLess(s.loc["inventory", "final_q_std"], 0.5 * s.loc["symmetric", "final_q_std"])
        self.assertLess(s.loc["inventory", "pnl_std"], s.loc["symmetric", "pnl_std"])

    def test_same_seed_same_results(self):
        df, sig = setup_df(days=2)
        cfg = simulator.SimConfig(gamma=0.02, k=12.0, A=2.0, n_paths=50, seed=9)
        a = simulator.run(df, sig, cfg, [0.01, 0.05]).summary
        b = simulator.run(df, sig, cfg, [0.01, 0.05]).summary
        pd.testing.assert_frame_equal(a, b)

    def test_inventory_cap(self):
        df, sig = setup_df(days=1)
        cfg = simulator.SimConfig(gamma=0.001, k=12.0, A=3.0, n_paths=100, max_inventory=3.0)
        res = simulator.run(df, sig, cfg, [0.001])
        self.assertTrue((res.summary["max_abs_q_mean"] <= 3.0).all())
        self.assertLessEqual(res.path["inventory"].abs().max(), 3.0)

    def test_rolling_horizon_and_path_record(self):
        df, sig = setup_df(days=1)
        cfg = replace(simulator.SimConfig(gamma=0.02, k=12.0, A=2.0, n_paths=10), horizon=60.0)
        res = simulator.run(df, sig, cfg, [0.02])
        self.assertTrue((res.path["tau_min"] == 60.0).all())
        self.assertEqual(set(res.path["strategy"]), {"inventory", "symmetric"})
        self.assertTrue((res.path["bid"] <= res.path["mid"]).all() and (res.path["ask"] >= res.path["mid"]).all())


if __name__ == "__main__":
    unittest.main()
