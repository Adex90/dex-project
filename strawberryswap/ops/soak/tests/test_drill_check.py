"""drill_check: the windows and the expectations a drill is judged on."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import drill_check  # noqa: E402

T0 = 1_800_000_000.0


def ev(offset: float, **fields) -> str:
    return json.dumps({"ts_ms": int((T0 + offset) * 1000), **fields})


class DrillCheckTest(unittest.TestCase):
    def run_check(self, engine_lines: list[str], *flags: str) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "engine.jsonl").write_text("\n".join(engine_lines) + "\n")
            (d / "drills.jsonl").write_text("\n".join([
                json.dumps({"drill": "vault-down", "phase": "start", "ts_ms": int((T0 + 100) * 1000)}),
                json.dumps({"drill": "vault-down", "phase": "end", "ts_ms": int((T0 + 160) * 1000)}),
            ]) + "\n")
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                rc = drill_check.main(["--engine", str(d / "engine.jsonl"), "--drills", str(d / "drills.jsonl"),
                                       "--drill", "vault-down", *flags])
            return rc, out.getvalue()

    FLAGS = ("--expect-in-fault", "alert:vault_down", "--within", "5", "--forbid-in-fault", "sent",
             "--expect-after-fault", "resolved:vault_down", "--after", "60",
             "--forbid", "rejected", "--forbid", "mined:reverted", "--forbid", "alert:foreign_transaction")

    def test_a_drill_that_behaves(self):
        lines = [
            ev(90, event="sent", nonce=1), ev(91, event="mined", nonce=1, status="success"),  # before: not judged
            ev(101.2, event="withdrawn", nonce=2, reason="the vault did not answer in time"),
            ev(102, event="alert", alert="vault_down", detail="x"),
            ev(165, event="resolved", alert="vault_down"), ev(170, event="sent", nonce=2),
        ]
        rc, out = self.run_check(lines, *self.FLAGS)
        self.assertEqual(rc, 0, out)
        self.assertIn("drill vault-down: PASS", out)

    def test_each_misbehaviour_fails_its_check(self):
        cases = {
            "the alert came late": [ev(110, event="alert", alert="vault_down"), ev(165, event="resolved", alert="vault_down")],
            "something was sent while the vault was down": [ev(102, event="alert", alert="vault_down"), ev(130, event="sent", nonce=2),
                                                             ev(165, event="resolved", alert="vault_down")],
            "it never recovered": [ev(102, event="alert", alert="vault_down")],
            "a revert on chain": [ev(102, event="alert", alert="vault_down"), ev(165, event="resolved", alert="vault_down"),
                                  ev(180, event="mined", nonce=2, status="reverted")],
            "a foreign nonce": [ev(102, event="alert", alert="vault_down"), ev(165, event="resolved", alert="vault_down"),
                                ev(170, event="alert", alert="foreign_transaction")],
        }
        for name, lines in cases.items():
            with self.subTest(name):
                rc, out = self.run_check(lines, *self.FLAGS)
                self.assertEqual(rc, 1, out)
                self.assertEqual(out.count("FAIL  "), 1, out)

    def test_without_an_attempt_the_drill_is_inconclusive(self):
        flags = ("--require-activity", "withdrawn:vault", "--expect-in-fault", "alert:vault_down", "--within", "2")
        quiet = [ev(130, event="metrics", counters={}), ev(165, event="sent", nonce=2)]
        rc, out = self.run_check(quiet, *flags)
        self.assertEqual(rc, 3, out)
        self.assertIn("INCONCLUSIVE", out)
        # With an attempt, --within counts from it, not from the fault's start.
        busy = [ev(140, event="withdrawn", nonce=2, reason="the vault did not answer in time"),
                ev(141, event="alert", alert="vault_down")]
        rc, out = self.run_check(busy, *flags)
        self.assertEqual(rc, 0, out)
        rc, out = self.run_check(busy[:1] + [ev(145, event="alert", alert="vault_down")], *flags)
        self.assertEqual(rc, 1, out)

    def test_activity_thresholds_and_snapshot_gaps(self):
        attempts = [ev(100 + i, event="withdrawn", nonce=2, reason="vault unavailable: connect") for i in range(1, 11)]
        alert = [ev(111, event="alert", alert="vault_down")]
        flags = ("--require-activity", "withdrawn:vault", "--min-activity", "10", "--expect-in-fault", "alert:vault_down",
                 "--within", "2", "--recovery-activity", "sent", "--expect-after-fault", "resolved:vault_down", "--after", "60")
        rc, out = self.run_check(attempts[:9] + alert, *flags)
        self.assertEqual(rc, 3, out)  # nine attempts cannot raise it
        rc, out = self.run_check(attempts + alert, *flags)
        self.assertEqual(rc, 3, out)  # no send after the fault: its resolution is not due
        recovered = attempts + alert + [ev(170, event="sent", nonce=2), ev(170, event="resolved", alert="vault_down")]
        rc, out = self.run_check(recovered, *flags)
        self.assertEqual(rc, 0, out)

        snaps = [ev(t, event="metrics", counters={}, gauges={}) for t in range(95, 225, 5)]
        rc, out = self.run_check(snaps, "--max-snapshot-gap", "6", "--after", "60")
        self.assertEqual(rc, 0, out)
        rc, out = self.run_check([s for s in snaps if not 130 <= json.loads(s)["ts_ms"] / 1000 - T0 <= 150],
                                 "--max-snapshot-gap", "6", "--after", "60")
        self.assertEqual(rc, 1, out)
        self.assertIn("longest silence 30.0 s", out)  # 125 s → 155 s

    def test_a_budget_allows_a_few(self):
        reverts = [ev(110 + i, event="mined", nonce=i, status="reverted") for i in range(3)]
        self.assertEqual(self.run_check(reverts, "--allow", "mined:reverted=3")[0], 0)
        rc, out = self.run_check(reverts, "--allow", "mined:reverted=2")
        self.assertEqual(rc, 1, out)
        self.assertIn("3 seen", out)

    def test_specs_match_on_their_field(self):
        e = drill_check.telemetry.Event(0, "skipped", {"reason": "the oracle reading is 50 s old"})
        self.assertTrue(drill_check.matches(e, "skipped"))
        self.assertTrue(drill_check.matches(e, "skipped:oracle"))
        self.assertFalse(drill_check.matches(e, "skipped:vault"))
        self.assertFalse(drill_check.matches(e, "beaten"))
        a = drill_check.telemetry.Event(0, "alert", {"alert": "oracle_stale"})
        self.assertTrue(drill_check.matches(a, "alert:oracle_stale"))
        self.assertFalse(drill_check.matches(a, "alert:oracle"))


if __name__ == "__main__":
    unittest.main()
