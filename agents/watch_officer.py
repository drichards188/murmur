#!/usr/bin/env python3
"""The AI watch officer for situations (`sim/situations/`): a placeholder.

The real one drives the user's own Claude Code (`claude -p`) or Codex CLI (`codex exec`)
against the bridge's MCP server as `agent-watch`. Another piece of work builds it to the
launch contract in agents/README.md ("Situations: the launch contract"):

    .venv/bin/python agents/watch_officer.py --backend claude|codex --url <bridge>/mcp \\
        --agent agent-watch --brief <brief file> --situation-log <path> --until <unix time>

Until then this placeholder takes the same arguments, says it is not built yet, writes one
note to the situation log, and exits 0; the situation runner goes on without an AI. It reads
and stores no credential, and calls no model.
"""

from __future__ import annotations

import argparse
import json
import sys
import time


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--backend", choices=["claude", "codex"], default="claude", help="whose CLI drives the watch officer")
    ap.add_argument("--url", required=True, help="the bridge's MCP endpoint, e.g. http://127.0.0.1:8765/mcp")
    ap.add_argument("--agent", default="agent-watch", help="the agent id (in MURMUR_AGENTS)")
    ap.add_argument("--brief", required=True, help="a file with the situation's title and brief (what the watch officer is told)")
    ap.add_argument("--situation-log", required=True, help="a JSON-lines file the watch officer appends its own notes to")
    ap.add_argument("--until", type=float, required=True, help="unix time at which to stop")
    args = ap.parse_args(argv)
    text = f"the {args.backend} watch officer is not built yet (agents/watch_officer.py is a placeholder): no AI in this run"
    print(text, flush=True)
    try:
        with open(args.situation_log, "a") as f:
            f.write(json.dumps({"t": round(time.time(), 3), "kind": "note", "text": text}) + "\n")
    except OSError as e:
        print(f"could not write {args.situation_log}: {e}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
