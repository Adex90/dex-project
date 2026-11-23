"""The RPC failover proxy against two fake providers: failover on errors and timeouts, recovery, lag, drills, and no
provider URL (they carry keys) in anything it says."""

from __future__ import annotations

import contextlib
import http.client
import io
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import rpc_failover  # noqa: E402


class FakeProvider:
    """A JSON-RPC node: eth_blockNumber answers `block`; any other method answers its own label. `mode` is ok, 500,
    hang (slower than the proxy's timeout) or rpc_error."""

    def __init__(self, label: str, block: int):
        self.label, self.block, self.mode, self.calls = label, block, "ok", 0
        fake = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if body["method"] != "eth_blockNumber":
                    fake.calls += 1
                if fake.mode == "hang":
                    time.sleep(1.0)
                if fake.mode == "500":
                    payload, status = b"upstream error", 500
                elif fake.mode == "rpc_error":
                    payload, status = json.dumps({"jsonrpc": "2.0", "id": body["id"], "error": {"code": -32000, "message": "execution reverted"}}).encode(), 200
                else:
                    result = hex(fake.block) if body["method"] == "eth_blockNumber" else fake.label
                    payload, status = json.dumps({"jsonrpc": "2.0", "id": body["id"], "result": result}).encode(), 200
                self.send_response(status)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        class Server(ThreadingHTTPServer):
            def handle_error(self, request, client_address):  # a hung reply to a proxy that gave up: expected
                pass

        self.server = Server(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/v2/SECRET-KEY-{label}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class FailoverTest(unittest.TestCase):
    def setUp(self):
        self.out = io.StringIO()
        self.redirect = contextlib.redirect_stdout(self.out)
        self.redirect.__enter__()
        self.primary, self.backup = FakeProvider("primary", 1000), FakeProvider("backup", 1000)
        self.dir = tempfile.TemporaryDirectory()
        path = Path(self.dir.name) / "upstreams"
        path.write_text(f"# label url\nprimary {self.primary.url}\nbackup {self.backup.url}\n")
        os.chmod(path, 0o600)
        self.proxy = rpc_failover.Proxy(rpc_failover.read_upstreams(str(path)), timeout=0.3, cooldown=0.2, max_lag=20)
        # No background health loop: each test runs the health checks it needs, so none races the test.
        self.server = rpc_failover.serve(self.proxy, "127.0.0.1:0", drills=True, health_every=3600, health=False)
        self.proxy.health_check()
        self.port = self.server.server_address[1]
        self.client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)  # one keep-alive connection

    def tearDown(self):
        self.client.close()
        self.server.shutdown()
        self.primary.close()
        self.backup.close()
        self.dir.cleanup()
        self.redirect.__exit__(None, None, None)
        said = self.out.getvalue()
        self.assertNotIn("SECRET-KEY", said, "a provider URL (with its key) was logged")

    def call(self, method="eth_call"):
        self.client.request("POST", "/", body=json.dumps({"jsonrpc": "2.0", "id": 7, "method": method, "params": []}),
                            headers={"Content-Type": "application/json"})
        resp = self.client.getresponse()
        return resp.status, json.loads(resp.read())

    def status(self):
        self.client.request("GET", "/status")
        body = self.client.getresponse().read()
        self.assertNotIn(b"SECRET-KEY", body)
        return json.loads(body)

    def drill(self, verb, label):
        self.client.request("POST", f"/drill/{verb}/{label}", body=b"")
        resp = self.client.getresponse()
        resp.read()
        return resp.status

    def test_the_primary_serves_while_it_is_up(self):
        for _ in range(3):
            self.assertEqual(self.call(), (200, {"jsonrpc": "2.0", "id": 7, "result": "primary"}))
        s = self.status()
        self.assertEqual((s["active"], s["failovers"]), ("primary", 0))
        self.assertEqual(self.backup.calls, 0)

    def test_errors_and_timeouts_fail_over_within_the_request(self):
        self.call()
        self.primary.mode = "500"
        self.assertEqual(self.call()[1]["result"], "backup")
        self.assertEqual(self.status()["failovers"], 1)
        self.assertIn('"event": "failover"', self.out.getvalue())

        # Back on the primary after its cooldown and a health check, then a hang: one request, still answered.
        self.primary.mode = "ok"
        time.sleep(0.25)
        self.proxy.health_check()
        self.assertEqual(self.call()[1]["result"], "primary")
        self.primary.mode = "hang"
        started = time.monotonic()
        self.assertEqual(self.call()[1]["result"], "backup")
        self.assertLess(time.monotonic() - started, 0.9, "a hung provider costs its timeout, not its hang")
        self.assertEqual(self.status()["failovers"], 3)

    def test_a_json_rpc_error_is_an_answer(self):
        self.primary.mode = "rpc_error"
        status, body = self.call()
        self.assertEqual((status, body["error"]["message"]), (200, "execution reverted"))
        self.assertEqual(self.backup.calls, 0)

    def test_a_drill_holds_a_provider_down_until_it_is_released(self):
        self.call()
        self.assertEqual(self.drill("down", "primary"), 200)
        self.assertEqual(self.call()[1]["result"], "backup")
        self.proxy.health_check()  # a forced-down provider stays down whatever its health
        self.assertEqual(self.call()[1]["result"], "backup")
        self.assertTrue(self.status()["providers"][0]["forced_down"])
        self.assertEqual(self.drill("up", "primary"), 200)
        self.assertEqual(self.call()[1]["result"], "primary")
        self.assertEqual(self.drill("down", "nobody"), 404)

    def test_a_lagging_provider_is_skipped(self):
        self.primary.block, self.backup.block = 1000, 1100
        self.proxy.health_check()
        self.assertEqual(self.call()[1]["result"], "backup")
        self.primary.block = 1095
        self.proxy.health_check()
        self.assertEqual(self.call()[1]["result"], "primary")

    def test_when_every_provider_fails_the_engine_gets_a_json_rpc_error(self):
        self.primary.mode = self.backup.mode = "500"
        status, body = self.call()
        self.assertEqual(status, 502)
        self.assertEqual(body["error"]["code"], -32603)
        self.assertIn("all_providers_down", self.out.getvalue())


class ConfigTest(unittest.TestCase):
    def test_the_providers_file_is_private_and_well_formed(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "upstreams"
            path.write_text("primary https://rpc.example/KEY\n")
            os.chmod(path, 0o644)
            with self.assertRaises(SystemExit) as e:
                rpc_failover.read_upstreams(str(path))
            self.assertIn("chmod 600", str(e.exception))
            self.assertNotIn("KEY", str(e.exception))
            os.chmod(path, 0o600)
            path.write_text("a https://one.example/K1\na https://two.example/K2\n")
            with self.assertRaises(SystemExit):
                rpc_failover.read_upstreams(str(path))
            path.write_text("a ftp://one.example/K1\n")
            with self.assertRaises(ValueError) as e:
                rpc_failover.read_upstreams(str(path))
            self.assertNotIn("K1", str(e.exception))

    def test_it_listens_on_loopback_only(self):
        proxy = rpc_failover.Proxy([rpc_failover.Upstream.parse("a", "http://127.0.0.1:1/")], 0.1, 1, 1)
        with self.assertRaises(SystemExit):
            rpc_failover.serve(proxy, "0.0.0.0:0", drills=False, health_every=3600)


if __name__ == "__main__":
    unittest.main()
