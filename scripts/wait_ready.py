#!/usr/bin/env python3
"""Wait until a Murmur stack started in the background is ready to watch, saying how far it got.

    .venv/bin/python scripts/wait_ready.py --port 9265     # a situation (make situation ...)
    .venv/bin/python scripts/wait_ready.py --port 8765     # the open world (make goldengate)

For an agent (or a script) that started a stack without the menu and needs to know when to tell
the person to look (AGENTS.md, "Helping someone use Murmur"). It reads the bridge's REST API
only, as the launcher does (eval/launcher.py): a situation (port 9265, or `--situation` on a
port of its own) is ready when its runner says it runs
(`GET /api/situation`, phase `running`; its opening orders are out by then); the open world when
every vehicle in `GET /api/state` is navigation-ready with a position, or 180 s after the bridge
answered with at least one ready (the launcher's rule: the rest may follow). Prints a line each
time the progress changes and a last line saying what it found.

Exit 0 ready (or a situation already over, its stack still up), 1 failed or timed out (the
runner's own output, e.g. logs/situation.out, says why), 2 a bad argument.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request

OPEN_WORLD_GRACE_S = 180.0  # eval/launcher.py READY_TIMEOUT_S


def get(port: int, path: str) -> dict | None:
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=3) as r:
            return json.loads(r.read())
    except (OSError, ValueError, urllib.error.URLError):
        return None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=8765, help="the bridge's HTTP port: 8765 the open world, 9265 a situation")
    ap.add_argument("--timeout", type=float, default=600.0, help="give up after this many seconds (default 600)")
    ap.add_argument("--situation", action="store_true",
                    help="wait for a situation runner's status, not just vehicles (the default on 9265, the situations' port)")
    args = ap.parse_args(argv)
    situation = args.situation or args.port == 9265
    start = time.monotonic()
    bridge_t: float | None = None
    said = ""

    def say(line: str) -> None:
        nonlocal said
        if line != said:
            print(f"[{time.monotonic() - start:4.0f} s] {line}", flush=True)
            said = line

    while time.monotonic() - start < args.timeout:
        state = get(args.port, "/api/state")
        if state is None:
            say(f"waiting for the bridge on :{args.port}")
            time.sleep(3)
            continue
        if bridge_t is None:
            bridge_t = time.monotonic()
        vehicles = state.get("vehicles") or []
        ready = sum(1 for v in vehicles if v.get("nav_ready") and v.get("position"))
        counted = f"{ready} of {len(vehicles)} vehicles ready"
        sit = (get(args.port, "/api/situation") or {}).get("situation")
        if isinstance(sit, dict):
            phase = sit.get("phase") or "starting"
            title = sit.get("title") or "the situation"
            if phase == "running":
                print(f"ready: {title} is running; {counted}", flush=True)
                return 0
            if phase == "ended":
                print(f"ready: {title} has ended (its time is up); the stack is still up for inspection; {counted}", flush=True)
                return 0
            if phase in ("failed", "interrupted"):
                print(f"failed: {title}: {phase}: {sit.get('phase_detail') or ''}", flush=True)
                return 1
            say(f"{title}: {phase}: {sit.get('phase_detail') or ''}; {counted}")
        elif situation:
            say(f"bridge up, waiting for the situation runner's status; {counted}")
        else:
            if vehicles and ready >= len(vehicles):
                print(f"ready: {counted}", flush=True)
                return 0
            if ready and time.monotonic() - bridge_t > OPEN_WORLD_GRACE_S:
                print(f"ready: {counted} after {OPEN_WORLD_GRACE_S:.0f} s; the rest may follow", flush=True)
                return 0
            say(f"bridge up; {counted}")
        time.sleep(3)
    print(f"timed out after {args.timeout:.0f} s: {said or 'nothing answered'}", flush=True)
    return 1


if __name__ == "__main__":
    sys.exit(main())
