import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np
import pandas as pd

import run_sim
from as_sim import data


class FakeYahoo:
    """Stands in for the yfinance module: records each history() window and returns bars for it."""

    def __init__(self, empty=False):
        self.calls, self.empty = [], empty

    def Ticker(self, symbol):  # noqa: N802 (yfinance's name)
        fake = self

        class _T:
            def history(self, **kw):
                fake.calls.append((symbol, kw))
                if fake.empty:
                    return pd.DataFrame()
                idx = pd.date_range(kw["start"], kw["end"], freq="1D", tz="UTC", inclusive="left")
                return pd.DataFrame({"Open": 1.0, "High": 2.0, "Low": 0.5, "Close": 1.5, "Adj Close": 1.5, "Volume": 10}, index=idx)

        return _T()


class DataTest(unittest.TestCase):
    def test_symbols_and_session_timezones(self):
        self.assertEqual(data.resolve_symbol("eth"), "ETH-USD")
        self.assertEqual(data.session_timezone("ETH"), "UTC")
        self.assertEqual(data.session_timezone("SPY"), "America/New_York")

    def test_download_in_seven_day_windows(self):
        fake = FakeYahoo()
        bars = data.download_bars("ETH", days=20, end="2026-11-01T00:00:00Z", yf_module=fake)
        self.assertEqual(len(fake.calls), 3)
        for symbol, kw in fake.calls:
            self.assertEqual(symbol, "ETH-USD")
            self.assertEqual(kw["interval"], "1m")
            self.assertLessEqual(kw["end"] - kw["start"], pd.Timedelta(days=7))
        self.assertEqual(list(bars.columns), data.COLUMNS)
        self.assertTrue(bars.index.is_monotonic_increasing and bars.index.is_unique)

    def test_empty_download_explains_itself(self):
        with self.assertRaisesRegex(RuntimeError, "query1.finance.yahoo.com"):
            data.download_bars("SPY", days=2, yf_module=FakeYahoo(empty=True))
        with self.assertRaises(ValueError):
            data.download_bars("SPY", days=45, yf_module=FakeYahoo())

    def test_prepare_mid_sources_and_episodes(self):
        bars = data.synthetic_bars(days=3)
        close = data.prepare(bars, "America/New_York")
        rng = data.prepare(bars, "America/New_York", mid_source="range")
        np.testing.assert_allclose(close["mid"], bars["close"])
        np.testing.assert_allclose(rng["mid"], (bars["high"] + bars["low"]) / 2)
        self.assertEqual(close["episode"].nunique(), 3)
        self.assertTrue((close.groupby("episode")["t_min"].min() == 0).all())

    def test_quotes_give_the_true_mid(self):
        bars = data.synthetic_bars(days=1).assign(bid=lambda b: b["close"] - 0.01, ask=lambda b: b["close"] + 0.03)
        df = data.prepare(bars, "America/New_York")
        np.testing.assert_allclose(df["mid"], bars["close"] + 0.01)

    def test_csv_round_trip(self):
        bars = data.synthetic_bars(days=1)
        with tempfile.TemporaryDirectory() as tmp:
            back = data.load_csv(data.save_csv(bars, Path(tmp) / "bars.csv"))
        np.testing.assert_allclose(back.to_numpy(), bars[data.COLUMNS].to_numpy())
        self.assertTrue((back.index == bars.index.tz_convert("UTC")).all())

    def test_cli_end_to_end_offline(self):
        with tempfile.TemporaryDirectory() as tmp:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(run_sim.main(["--synthetic", "--paths", "40", "--out", tmp]), 0)
            out = next(Path(tmp).iterdir())
            for name in ("summary.csv", "episodes.csv", "path_example.csv", "touch_curve.csv", "calibration.json"):
                self.assertTrue((out / name).exists(), name)
            calib = json.loads((out / "calibration.json").read_text())
            self.assertEqual(set(calib["engine"]), {"gamma", "horizonSec", "sigma2PerSec", "kappa"})
            self.assertTrue(calib["data"]["synthetic"])


if __name__ == "__main__":
    unittest.main()
