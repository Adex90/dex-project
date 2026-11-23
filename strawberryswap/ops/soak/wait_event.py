#!/usr/bin/env python3
"""Waits until the engine logs an event matching SPEC (drill_check.py's syntax) at or after --since-ms.

    ops/soak/wait_event.py --engine soak/engine.jsonl --since-ms 1795000000000 --timeout 120 resolved:vault_down

Exits 0 when it appears (printing it), 1 on timeout. Reads the log as it grows. Used by ops/drills/*.sh.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import drill_check  # noqa: E402
import telemetry  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--engine", type=Path, required=True)
    p.add_argument("--since-ms", type=int, required=True)
    p.add_argument("--timeout", type=float, default=120.0)
    p.add_argument("spec")
    args = p.parse_args(argv)
    deadline = time.monotonic() + args.timeout
    offset = 0
    while True:
        if args.engine.exists():
            with open(args.engine, encoding="utf-8", errors="replace") as f:
                f.seek(offset)
                chunk = f.read()
                offset = f.tell()
            for e in telemetry.parse_lines(chunk.splitlines()).events:
                if e.ts * 1000 >= args.since_ms and drill_check.matches(e, args.spec):
                    print(json.dumps(e.data))
                    return 0
        if time.monotonic() >= deadline:
            print(f"wait_event: no {args.spec} within {args.timeout:g} s", file=sys.stderr)
            return 1
        time.sleep(0.5)


if __name__ == "__main__":
    sys.exit(main())
