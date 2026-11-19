#!/usr/bin/env python3
"""A JSON-RPC failover proxy: the engine's --rpc-url points here, and this forwards to the first healthy of several
RPC providers, so one provider's outage costs a request, not the engine's chain view.

    ops/soak/rpc_failover.py --upstreams /etc/strawberryswap/rpc-upstreams --listen 127.0.0.1:8547 [--drills]

* --upstreams: a file, mode 0600, one provider per line, `label URL` (e.g. `primary https://…`), in priority order.
  Provider URLs carry API keys: they are read from that file only, and never printed, logged or served. Everything
  this proxy says names providers by label.
* Each request goes to the highest-priority provider that is up. A transport failure, a timeout, HTTP 429 or 5xx marks
  that provider down for --cooldown-s and the same request goes to the next one at once (the response is the first
  provider's that answered). A JSON-RPC error is an answer, not a failure: it is passed through.
* A health check (eth_blockNumber) every --health-every-s brings providers back, and marks down one that lags the best
  by more than --max-lag-blocks (a stale node would feed the engine an old pool).
* GET /status: each provider's state, block, latency and failures, and the failovers so far (labels only).
* --drills: POST /drill/down/<label> and /drill/up/<label> force a provider down and back, for the RPC-failover drill
  (ops/drills/rpc-failover.sh). The listener is loopback only.

A transaction broadcast that times out on one provider is also sent to the next: the signed transaction is the same
bytes, so the worst case is the next provider answering "already known" for a transaction that is in fact pending,
which the engine tracks by its nonce. Each failover is one JSON line on stdout. Standard library only.
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
import stat
import sys
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

RETRY_STATUS = {429, 500, 502, 503, 504}


def log(event: str, **fields) -> None:
    print(json.dumps({"event": event, "ts_ms": int(time.time() * 1000), **fields}), flush=True)


@dataclass
class Upstream:
    label: str
    scheme: str
    host: str
    port: int
    path: str
    up: bool = True
    forced_down: bool = False
    down_until: float = 0.0
    block: int | None = None
    latency_ms: float | None = None
    failures: int = 0
    served: int = 0
    lagging: bool = False
    local: threading.local = field(default_factory=threading.local, repr=False)

    @classmethod
    def parse(cls, label: str, url: str) -> "Upstream":
        u = urlsplit(url)
        if u.scheme not in ("http", "https") or not u.hostname:
            raise ValueError(f"provider {label}: not an http(s) URL")  # the URL itself is never echoed
        port = u.port or (443 if u.scheme == "https" else 80)
        path = (u.path or "/") + (f"?{u.query}" if u.query else "")
        return cls(label, u.scheme, u.hostname, port, path)

    def available(self, now: float) -> bool:
        return not self.forced_down and not self.lagging and (self.up or now >= self.down_until)

    def connection(self, timeout: float) -> http.client.HTTPConnection:
        # One keep-alive connection per provider and per handler thread: no TLS handshake per request.
        conn = getattr(self.local, "conn", None)
        if conn is None:
            cls = http.client.HTTPSConnection if self.scheme == "https" else http.client.HTTPConnection
            conn = cls(self.host, self.port, timeout=timeout)
            self.local.conn = conn
        return conn

    def post(self, body: bytes, timeout: float) -> tuple[int, bytes]:
        for attempt in (0, 1):  # a keep-alive connection the provider closed is retried once on a fresh one
            conn = self.connection(timeout)
            try:
                conn.request("POST", self.path, body=body, headers={"Content-Type": "application/json"})
                resp = conn.getresponse()
                return resp.status, resp.read()
            except (http.client.RemoteDisconnected, BrokenPipeError, ConnectionResetError):
                conn.close()
                self.local.conn = None
                if attempt:
                    raise
            except Exception:
                conn.close()
                self.local.conn = None
                raise
        raise ConnectionError("unreachable")

    def view(self) -> dict:
        return {"label": self.label, "up": self.up, "forced_down": self.forced_down, "lagging": self.lagging,
                "block": self.block, "latency_ms": self.latency_ms, "failures": self.failures, "served": self.served}


class Proxy:
    def __init__(self, upstreams: list[Upstream], timeout: float, cooldown: float, max_lag: int):
        if not upstreams:
            raise ValueError("no provider")
        self.upstreams = upstreams
        self.timeout = timeout
        self.cooldown = cooldown
        self.max_lag = max_lag
        self.lock = threading.Lock()
        self.failovers = 0
        self.active: str | None = None

    def mark_down(self, u: Upstream, reason: str) -> None:
        with self.lock:
            u.failures += 1
            was_up, u.up, u.down_until = u.up, False, time.monotonic() + self.cooldown
        if was_up:
            log("provider_down", provider=u.label, reason=reason)

    def forward(self, body: bytes) -> tuple[int, bytes]:
        now = time.monotonic()
        tried = []
        # The providers that are up (or whose cooldown is over); when none is, every one not held down by a drill:
        # a provider marked down may well be back, and trying costs less than refusing the engine outright.
        candidates = [u for u in self.upstreams if u.available(now)] or [u for u in self.upstreams if not u.forced_down]
        for u in candidates:
            started = time.monotonic()
            try:
                status, payload = u.post(body, self.timeout)
            except Exception as e:  # noqa: BLE001: any transport failure fails over
                self.mark_down(u, type(e).__name__)
                tried.append(u.label)
                continue
            if status in RETRY_STATUS:
                self.mark_down(u, f"http {status}")
                tried.append(u.label)
                continue
            with self.lock:
                u.up, u.served = True, u.served + 1
                u.latency_ms = round((time.monotonic() - started) * 1000, 2)
                if self.active != u.label:
                    if self.active is not None:
                        self.failovers += 1
                        log("failover", **{"from": self.active, "to": u.label, "after": tried})
                    self.active = u.label
            return status, payload
        log("all_providers_down", tried=tried)
        return 502, json.dumps({"jsonrpc": "2.0", "id": None, "error": {"code": -32603, "message": "no RPC provider answered"}}).encode()

    def health_check(self) -> None:
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_blockNumber", "params": []}).encode()
        for u in self.upstreams:
            if u.forced_down:
                continue
            started = time.monotonic()
            try:
                status, payload = u.post(body, self.timeout)
                block = int(json.loads(payload)["result"], 16) if status == 200 else None
            except Exception:  # noqa: BLE001
                block = None
            if block is None:
                self.mark_down(u, "health check")
                continue
            with self.lock:
                came_back = not u.up
                u.up, u.block = True, block
                u.latency_ms = round((time.monotonic() - started) * 1000, 2)
            if came_back:
                log("provider_up", provider=u.label, block=block)
        best = max((u.block for u in self.upstreams if u.block is not None and u.up), default=None)
        for u in self.upstreams:
            lagging = best is not None and u.block is not None and best - u.block > self.max_lag
            if lagging != u.lagging:
                u.lagging = lagging
                log("provider_lagging" if lagging else "provider_caught_up", provider=u.label, block=u.block, best=best)

    def status(self) -> dict:
        with self.lock:
            return {"active": self.active, "failovers": self.failovers, "providers": [u.view() for u in self.upstreams]}

    def drill(self, label: str, down: bool) -> bool:
        for u in self.upstreams:
            if u.label == label:
                u.forced_down = down
                log("drill_provider_down" if down else "drill_provider_up", provider=label)
                return True
        return False


def read_upstreams(path: str) -> list[Upstream]:
    mode = os.stat(path).st_mode
    if mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise SystemExit(f"{path}: readable by group or others (mode {stat.S_IMODE(mode):o}); chmod 600 it: provider URLs carry keys")
    out = []
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) != 2:
                raise SystemExit(f"{path}:{n}: expected `label URL`")
            out.append(Upstream.parse(parts[0], parts[1]))
    if len({u.label for u in out}) != len(out):
        raise SystemExit(f"{path}: provider labels must be unique")
    return out


def make_handler(proxy: Proxy, drills: bool):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"  # keep-alive with the engine

        def log_message(self, *args) -> None:  # no access log: request bodies and paths stay out of the logs
            pass

        def reply(self, status: int, payload: bytes) -> None:
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self) -> None:
            if self.path == "/status":
                self.reply(200, json.dumps(proxy.status()).encode())
            else:
                self.reply(404, b'{"error":"not found"}')

        def do_POST(self) -> None:
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length else b""
            if self.path.startswith("/drill/"):
                parts = self.path.strip("/").split("/")
                if not drills or len(parts) != 3 or parts[1] not in ("down", "up"):
                    self.reply(404, b'{"error":"not found"}')
                elif proxy.drill(parts[2], parts[1] == "down"):
                    self.reply(200, json.dumps(proxy.status()).encode())
                else:
                    self.reply(404, b'{"error":"no such provider"}')
                return
            status, payload = proxy.forward(body)
            self.reply(status, payload)

    return Handler


def serve(proxy: Proxy, listen: str, drills: bool, health_every: float, health: bool = True) -> ThreadingHTTPServer:
    host, _, port = listen.rpartition(":")
    if host not in ("127.0.0.1", "localhost", "::1"):
        raise SystemExit("--listen must be a loopback address: the proxy has no authentication")
    server = ThreadingHTTPServer((host, int(port)), make_handler(proxy, drills))
    server.daemon_threads = True

    def health_loop() -> None:
        while True:
            proxy.health_check()
            time.sleep(health_every)

    if health:
        threading.Thread(target=health_loop, daemon=True).start()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--upstreams", required=True, help="file (0600) of `label URL` lines, in priority order")
    p.add_argument("--listen", default="127.0.0.1:8547")
    p.add_argument("--timeout-ms", type=int, default=1500, help="per request and provider [1500]")
    p.add_argument("--cooldown-s", type=float, default=10.0, help="how long a failed provider is skipped [10]")
    p.add_argument("--health-every-s", type=float, default=2.0, help="health-check period [2]")
    p.add_argument("--max-lag-blocks", type=int, default=20, help="behind the best provider by more is down [20]")
    p.add_argument("--drills", action="store_true", help="enable POST /drill/down|up/<label>")
    args = p.parse_args(argv)
    upstreams = read_upstreams(args.upstreams)
    proxy = Proxy(upstreams, args.timeout_ms / 1000.0, args.cooldown_s, args.max_lag_blocks)
    server = serve(proxy, args.listen, args.drills, args.health_every_s)
    log("listening", listen=f"{server.server_address[0]}:{server.server_address[1]}",
        providers=[u.label for u in upstreams], drills=args.drills)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
