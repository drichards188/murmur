"""The situation log: one JSON object per line, appended (the situation framework may write
its own lines to the same file; ours carry `"source": "watch_officer"`).

Every line has `t` (Unix seconds), `iso` (UTC), `source`, `run` (this driver's run id),
`kind`, and `text`: one short line for a person (the situation framework's runner shows
`{t, kind, text}` in the console's Situation panel). The kinds the driver writes:

    run.start         backend, versions, models, budgets, run dir, the brief's and prompt's sha256
    preflight         what was checked (CLI, version, login method, API-key variables removed)
    preflight.failed  the problem and the fix; the run ends
    backend.init      session or thread id, MCP server status, tools the model sees
    wake              n, reason (start, changes, heartbeat), why, escalated, model, `update`
                      (exactly what the model was told), the changes behind it
    turn              n, model, its tool calls (name, input, result excerpt), its final
                      message (`text`: its note for the operator), usage, cost estimate,
                      duration, stop, intents it proposed
    shadow            what the deterministic playbook would have proposed (never proposed)
    budget.deferred   a wake held back by the hourly cap
    budget.exhausted  the run's wake cap is spent; the run ends
    limit.hit         the provider's message, the parsed reset time, wait or stop
    limit.resume      the wait ended; the backend resumes
    backend.crashed   exit code, stderr tail; backend.restart follows (resuming the session)
    poll.error        the bridge did not answer a read (retried)
    run.stop          why (until, signal, budget, limit, error), totals
"""

from __future__ import annotations

import datetime as dt
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

SOURCE = "watch_officer"


class SituationLog:
    def __init__(self, path: Path | None, run: str, echo: bool = True):
        self.path = path
        self.run = run
        self.echo = echo
        self.fd: int | None = None
        if path is not None:
            path.parent.mkdir(parents=True, exist_ok=True)
            self.fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)

    def write(self, kind: str, **fields: Any) -> dict:
        t = time.time()
        rec = {"t": round(t, 3), "iso": dt.datetime.fromtimestamp(t, dt.timezone.utc).isoformat(timespec="milliseconds"),
               "source": SOURCE, "run": self.run, "kind": kind}
        for k, v in fields.items():
            rec[f"field_{k}" if k in rec else k] = v  # a line's own keys are never overwritten
        if not isinstance(rec.get("text"), str) or not rec["text"].strip():
            rec["text"] = panel(rec)
        line = json.dumps(rec, ensure_ascii=False, default=str) + "\n"
        if self.fd is not None:
            os.write(self.fd, line.encode())  # one write per line: O_APPEND keeps lines whole
        if self.echo:
            print(f"[watch] {kind} {summary(rec)}", file=sys.stderr, flush=True)
        return rec

    def close(self) -> None:
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


def _hm(t: Any) -> str:
    try:
        return dt.datetime.fromtimestamp(float(t), dt.timezone.utc).strftime("%H:%M:%SZ")
    except (TypeError, ValueError):
        return "?"


def panel(rec: dict) -> str:
    """One line for a person reading the Situation panel."""
    k = rec.get("kind")
    if k == "wake":
        how = f"{rec.get('model')}{', escalated: ' + '; '.join(rec.get('escalate_why') or []) if rec.get('escalated') else ''}"
        why = "; ".join(rec.get("why") or []) or ("routine look" if rec.get("reason") == "heartbeat" else "watch start")
        return f"watch officer woken ({how}): {why}"
    if k == "run.start":
        b = rec.get("backend") or {}
        return f"watch officer started: {b.get('provider')} {b.get('model')}, until {_hm(rec.get('until'))}"
    if k == "run.stop":
        return (f"watch officer stopped ({rec.get('reason')}): {rec.get('wakes')} wakes, "
                f"proposed {rec.get('proposed') or 'nothing'}")
    if k == "shadow":
        req = rec.get("request") or {}
        what = f"{req.get('task')} on {req.get('uids')}" if req else "nothing (no sensor)"
        return f"the playbook would propose {what} for {rec.get('track')} (not proposed)"
    if k == "limit.hit":
        when = _hm(rec.get("resume_at") or rec.get("until") or rec.get("reset_at"))
        return f"usage limit ({rec.get('provider')} {rec.get('limit')}): {rec.get('action')}, until {when}"
    if k == "limit.resume":
        return "usage limit over: the watch resumes"
    if k in ("backend.crashed", "backend.restart"):
        return f"{k.replace('.', ' ')}: {rec.get('why') or rec.get('error') or ''}".strip()
    if k == "preflight.failed":
        return f"watch officer can't start: {rec.get('problem')}. Fix: {rec.get('fix')}"
    if k == "error":
        return f"watch officer error: {rec.get('error')}"
    if k in ("budget.deferred", "budget.exhausted"):
        return f"{k.replace('.', ' ')}: {rec.get('what')}"
    return summary(rec)


def summary(rec: dict) -> str:
    """One short line for the terminal."""
    k = rec.get("kind")
    if k == "wake":
        return f"#{rec.get('n')} {rec.get('reason')} ({rec.get('model')}{', escalated' if rec.get('escalated') else ''}): {'; '.join(rec.get('why') or [])}"
    if k == "turn":
        calls = ", ".join(c.get("name", "?").removeprefix("mcp__murmur__") for c in rec.get("tool_calls") or [])
        text = (rec.get("text") or "").replace("\n", " ")
        return f"#{rec.get('n')} {rec.get('stop')} {rec.get('duration_s')}s [{calls}] {text[:200]}"
    keep = {key: v for key, v in rec.items() if key not in ("t", "iso", "source", "run", "kind")}
    s = json.dumps(keep, ensure_ascii=False, default=str)
    return s if len(s) < 300 else s[:300] + "..."
