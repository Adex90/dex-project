"""The live drills' scripts (ops/drills/*.sh), run for real against a stand-in engine: systemctl is a no-op, a thread
writes the engine's telemetry as the services would make it behave, and the RPC drill drives a real failover proxy.
They check the scripts' control flow, windows and verdicts; the services' own behaviour is what the drills test live."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

OPS = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(OPS / "soak"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import rpc_failover  # noqa: E402
from test_rpc_failover import FakeProvider  # noqa: E402


class Engine(threading.Thread):
    """Writes the engine's --json lines: a metrics snapshot every 0.2 s, plus what `script(elapsed)` returns."""

    def __init__(self, log: Path, script):
        super().__init__(daemon=True)
        self.log, self.script, self.stop = log, script, threading.Event()
        self.start_time = time.time()

    def write(self, f, **fields):
        f.write(json.dumps({"ts_ms": int(time.time() * 1000), **fields}) + "\n")
        f.flush()

    def run(self):
        with open(self.log, "a") as f:
            while not self.stop.is_set():
                self.write(f, event="metrics", counters={"ticks": 1}, latency={}, gauges={"mid": 3450.0}, alerts=[])
                for event in self.script(time.time() - self.start_time):
                    self.write(f, **event)
                time.sleep(0.2)


@unittest.skipUnless(shutil.which("bash") and shutil.which("jq") and shutil.which("curl"), "needs bash, jq and curl")
class DrillScriptsTest(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.env = {**os.environ, "SOAK_DIR": str(self.dir), "SYSTEMCTL": "true"}

    def tearDown(self):
        shutil.rmtree(self.dir)

    def run_drill(self, name: str, script, **env) -> tuple[int, str]:
        engine = Engine(self.dir / "engine.jsonl", script)
        engine.start()
        time.sleep(0.5)  # the collector has something before the drill starts
        try:
            p = subprocess.run(["bash", str(OPS / "drills" / f"{name}.sh")], env={**self.env, **env},
                               capture_output=True, text=True, timeout=120)
        finally:
            engine.stop.set()
            engine.join()
        out = p.stdout + p.stderr
        results = (self.dir / "drill-results.jsonl").read_text().splitlines() if (self.dir / "drill-results.jsonl").exists() else []
        self.assertTrue(results, out)
        return p.returncode, out

    def test_stale_feed(self):
        def script(t):
            if 1.0 <= t < 1.2:
                yield {"event": "skipped", "reason": "oracle reading stamped … outside [now − 45 s, now + 5 s]"}
                yield {"event": "alert", "alert": "oracle_stale", "detail": "stale"}
            if 4.5 <= t < 4.7:  # the relayer's first push after the restart (the drill ends at ~3.5 s)
                yield {"event": "resolved", "alert": "oracle_stale"}

        rc, out = self.run_drill("stale-feed", script, DOWN_S="3", RECOVER_S="4")
        self.assertEqual(rc, 0, out)
        self.assertIn("drill stale-feed: PASS", out)

    def test_stale_feed_that_never_recovers_fails(self):
        def script(t):
            if 1.0 <= t < 1.2:
                yield {"event": "alert", "alert": "oracle_stale", "detail": "stale"}

        rc, out = self.run_drill("stale-feed", script, DOWN_S="2", RECOVER_S="2")
        self.assertEqual(rc, 1, out)
        self.assertIn("FAIL  expect resolved:oracle_stale after the fault", out)

    def test_vault_down(self):
        def script(t):
            if 1.0 <= t < 3.0:
                yield {"event": "withdrawn", "nonce": 7, "reason": "vault unavailable: connect: no such file"}
            if 2.8 <= t < 3.0:
                yield {"event": "alert", "alert": "vault_down", "detail": "vault unavailable"}
            if 5.0 <= t < 5.2:  # the vault is back (the drill ends at ~4.5 s): the next rebalance is sent
                yield {"event": "sent", "nonce": 7, "tx": "0x07"}
                yield {"event": "resolved", "alert": "vault_down"}

        rc, out = self.run_drill("vault-down", script, DOWN_S="4", RECOVER_S="5")
        self.assertEqual(rc, 0, out)
        self.assertIn("drill vault-down: PASS", out)

    def test_vault_down_in_a_quiet_pool_is_inconclusive(self):
        rc, out = self.run_drill("vault-down", lambda t: iter(()), DOWN_S="2", RECOVER_S="1")
        self.assertEqual(rc, 3, out)
        self.assertIn("INCONCLUSIVE", out)

    def test_rpc_failover(self):
        primary, backup = FakeProvider("primary", 1000), FakeProvider("backup", 1000)
        upstreams = self.dir / "upstreams"
        upstreams.write_text(f"primary {primary.url}\nbackup {backup.url}\n")
        upstreams.chmod(0o600)
        proxy = rpc_failover.Proxy(rpc_failover.read_upstreams(str(upstreams)), 0.3, 0.2, 20)
        server = rpc_failover.serve(proxy, "127.0.0.1:0", drills=True, health_every=0.5)
        port = server.server_address[1]

        def engine_reads():  # the engine's reads, through the proxy, as its 100 ms tick would make them
            import http.client
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            while not done.is_set():
                conn.request("POST", "/", body=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_call", "params": []}))
                conn.getresponse().read()
                time.sleep(0.1)

        done = threading.Event()
        reader = threading.Thread(target=engine_reads, daemon=True)
        quiet = contextlib.redirect_stdout(io.StringIO())  # the in-process proxy's own JSON log
        quiet.__enter__()
        try:
            proxy.health_check()
            reader.start()
            time.sleep(0.3)
            rc, out = self.run_drill("rpc-failover", lambda t: iter(()), PROXY_URL=f"http://127.0.0.1:{port}", DOWN_S="2")
        finally:
            done.set()
            reader.join()
            server.shutdown()
            primary.close()
            backup.close()
            quiet.__exit__(None, None, None)
        self.assertEqual(rc, 0, out)
        self.assertIn("the proxy serves from backup", out)
        self.assertIn("drill rpc-failover: PASS", out)
        self.assertNotIn("SECRET-KEY", out)


if __name__ == "__main__":
    unittest.main()
