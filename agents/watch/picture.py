"""What the driver reads from the bridge, and how it tells the model.

The driver reads as the same agent the model acts as, over MCP (the agent's key in a
credentials run): `get_changes` every couple of seconds for what changed, and, while a track
is declared hostile, `get_nomination_options` now and then to see whether a nomination has
become possible (a `nomination.possible` change). Nothing it reads is logged by the bridge.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import time
from typing import Any, Callable

TERMINAL = ("rejected", "expired", "voided", "done", "failed", "effected", "aborted")
OPEN = ("proposed", "checked", "pending_approval", "approved", "queued", "dispatched", "auto_approved")


class NoChangesTool(RuntimeError):
    pass


class Poller:
    """The change feed for one agent session, reconnecting when the bridge goes away."""

    def __init__(self, connect: Callable[[], Any], nomination_every_s: float = 15.0):
        self.connect = connect  # () -> an AgentSession (not yet entered)
        self.session = None
        self.cursor: int | None = None
        self.run: str | None = None
        self.hostile: set[str] = set()
        self.passing: dict[str, list] = {}
        self.checked: dict[str, float] = {}
        self.nomination_every_s = nomination_every_s
        self.nomination_gap_s = 1.05  # the bridge allows one dry run a second per agent

    async def open(self) -> dict:
        if self.session is not None:
            await self.close()
        s = self.connect()
        await s.__aenter__()
        self.session = s
        if "get_changes" not in s.tools:
            await self.close()
            raise NoChangesTool(
                "this bridge has no get_changes tool: rebuild it from this tree (make build) "
                "and restart the stack")
        first = await s.call("get_changes")
        reset = self.run is not None and first.get("run") != self.run
        if reset or self.cursor is None:
            self.cursor = first.get("cursor")
        # else: the same bridge after a dropped session: its feed still has what we missed
        self.run = first.get("run")
        tracks = await s.tracks(affiliation="hostile")
        self.hostile = {t["id"] for t in tracks if t.get("declared")}
        return {"cursor": self.cursor, "run": self.run, "tools": list(s.tools), "reset": reset}

    async def close(self) -> None:
        if self.session is not None:
            try:
                await self.session.__aexit__(None, None, None)
            finally:
                self.session = None

    async def poll(self) -> list[dict]:
        """Every change since the last poll, oldest first."""
        out: list[dict] = []
        while True:
            r = await self.session.call("get_changes", since=self.cursor, limit=500)
            if r.get("reset"):
                out.append({"kind": "feed.reset", "t": r.get("t"), "run": r.get("run"), "was": self.run})
                self.run = r.get("run")
            if r.get("truncated"):
                out.append({"kind": "feed.truncated", "t": r.get("t")})
            out.extend(r.get("changes") or [])
            self.cursor = r.get("cursor", self.cursor)
            if not r.get("more"):
                break
        for c in out:
            k = c.get("kind")
            if k == "track.declared" and c.get("declared") == "hostile":
                self.hostile.add(c["track"])
            elif k in ("track.declaration_revoked", "track.dropped") or (
                    k == "track.declared" and c.get("declared") != "hostile"):
                self.hostile.discard(c.get("track"))
                self.passing.pop(c.get("track"), None)
        return out

    async def nominations(self, now: float | None = None) -> list[dict]:
        """`nomination.possible` when a hostile track gets an armed platform that passes every
        check, `nomination.closed` when it loses the last one (read-only dry run, at most
        one call a second per agent on the bridge; we read each track every
        `nomination_every_s`)."""
        now = time.time() if now is None else now
        out = []
        for track in sorted(self.hostile):
            if now - self.checked.get(track, 0.0) < self.nomination_every_s:
                continue
            self.checked[track] = now
            result = await self.session.session.call_tool("get_nomination_options", {"track_id": track})
            body = _content(result)
            if getattr(result, "is_error", None) or getattr(result, "isError", None):
                continue  # rate limited, or the track is gone: try again later
            passing = [{"uid": o.get("uid"), "callsign": o.get("callsign")}
                       for o in body.get("options") or [] if o.get("passed")]
            had = self.passing.get(track) or []
            if passing and not had:
                out.append({"kind": "nomination.possible", "t": now, "track": track, "platforms": passing})
            elif had and not passing:
                out.append({"kind": "nomination.closed", "t": now, "track": track})
            self.passing[track] = passing
            await asyncio.sleep(self.nomination_gap_s)
        return out

    async def mine(self) -> list[dict]:
        return await self.session.intents(mine=True)


def _content(result) -> dict:
    import json

    structured = getattr(result, "structured_content", None) or getattr(result, "structuredContent", None)
    if isinstance(structured, dict):
        return structured
    for block in getattr(result, "content", []) or []:
        if getattr(block, "type", "") == "text":
            try:
                return json.loads(block.text)
            except ValueError:
                return {}
    return {}


# ---- telling the model ---------------------------------------------------------------------

def hms(t: Any) -> str:
    try:
        return dt.datetime.fromtimestamp(float(t), dt.timezone.utc).strftime("%H:%M:%SZ")
    except (TypeError, ValueError):
        return "--:--:--"


def _who(c: dict) -> str:
    s = f"uid {c.get('uid')}"
    if c.get("callsign"):
        s += f" {c['callsign']}"
    bits = [b for b in (c.get("platform"), "ARMED" if c.get("armed") else None) if b]
    return s + (f" ({', '.join(bits)})" if bits else "")


def _pos(p: Any) -> str:
    if isinstance(p, (list, tuple)) and len(p) == 2 and all(isinstance(x, (int, float)) for x in p):
        return f"{p[0]:.4f},{p[1]:.4f}"
    return "?"


def render(c: dict) -> str:
    """One line for one change."""
    k = c.get("kind", "?")
    t = hms(c.get("t"))
    tr = c.get("track")
    if k == "intent.new":
        by = "you" if c.get("mine") else (c.get("source") or {}).get("id", "?")
        return f"{t} intent #{c.get('intent')} {c.get('task')} on {c.get('uids')} by {by}: {c.get('state')}"
    if k == "intent.state":
        whose = "yours" if c.get("mine") else (c.get("source") or {}).get("id", "?")
        why = f" ({c['reason']})" if c.get("reason") else ""
        return f"{t} intent #{c.get('intent')} {c.get('task')} on {c.get('uids')} ({whose}): {c.get('from')} -> {c.get('to')}{why}"
    if k == "track.new":
        src = ", ".join(f"uid {s.get('uid')} {s.get('sensor')}" for s in c.get("sources") or [])
        conf = c.get("affiliation_confidence")
        conf_s = f" {conf:.2f}" if isinstance(conf, (int, float)) else ""
        return (f"{t} new track {tr}: {c.get('domain')} {c.get('identity') or 'unclassified'}, "
                f"estimated {c.get('affiliation')}{conf_s}, at {_pos(c.get('position'))}, "
                f"{c.get('speed_mps')} m/s course {c.get('course_deg')}, seen by {src or '?'}")
    if k == "track.affiliation":
        return f"{t} track {tr} affiliation estimate {c.get('from')} -> {c.get('to')}"
    if k == "track.declared":
        return f"{t} track {tr} declared {c.get('declared')} by {c.get('by')}"
    if k == "track.declaration_confirmed":
        return f"{t} track {tr}: the declaration was confirmed by {c.get('confirmed_by')}"
    if k == "track.declaration_revoked":
        return f"{t} track {tr}: its declaration was revoked"
    if k == "track.identity_conflict":
        return f"{t} track {tr} identity conflict: {c.get('conflict')}"
    if k in ("track.stale", "track.active"):
        return f"{t} track {tr} is {k.split('.')[1]}"
    if k == "track.dropped":
        return f"{t} track {tr} dropped by the tracker{' (still counted as a lost contact)' if c.get('lost') else ''}"
    if k == "review.flag":
        return f"{t} review flag {c.get('flag')} ({c.get('flag_kind')}) on {c.get('tracks')}: {c.get('evidence')}"
    if k == "review.flag_status":
        return f"{t} review flag {c.get('flag')}: {c.get('from')} -> {c.get('to')}"
    if k == "vehicle.joined":
        return f"{t} {_who(c)} joined"
    if k == "vehicle.link_lost":
        return f"{t} {_who(c)} lost its link"
    if k == "vehicle.link_restored":
        return f"{t} {_who(c)} link restored"
    if k == "vehicle.nav":
        f, to = c.get("from") or {}, c.get("to") or {}
        return f"{t} {_who(c)} navigation ready {f.get('ready')} {f.get('source')} -> ready {to.get('ready')} {to.get('source')}"
    if k == "vehicle.gps_glitch":
        return f"{t} {_who(c)} GPS glitch {'FLAGGED by its navigation filter' if c.get('glitch') else 'cleared'}"
    if k == "vehicle.event":
        return f"{t} {_who(c)} {c.get('event_kind')}: {c.get('text')}"
    if k == "alert.raised":
        return f"{t} alert {c.get('alert_kind')} ({c.get('severity')}) {_who(c)}: {c.get('detail')}"
    if k == "alert.cleared":
        return f"{t} alert cleared: {c.get('alert_kind')} {_who(c)}"
    if k == "nomination.possible":
        ps = ", ".join(f"uid {p.get('uid')} {p.get('callsign') or ''}".strip() for p in c.get("platforms") or [])
        return f"{t} track {tr}: armed platforms that pass every nomination check now: {ps}"
    if k == "nomination.closed":
        return f"{t} track {tr}: no armed platform passes every nomination check any more"
    if k == "feed.reset":
        return f"{t} the bridge restarted (run {c.get('run')}): read the picture again"
    if k == "feed.truncated":
        return f"{t} some older changes were dropped from the feed: read the picture again"
    return f"{t} {k}"


def render_mine(intents: list[dict], now: float) -> list[str]:
    """The model's own proposals worth a line: open ones, and those ended in the last ten
    minutes."""
    lines = []
    for i in intents:
        state = i.get("state")
        hist = i.get("history") or []
        last_t = (hist[-1].get("t") / 1000.0) if hist and isinstance(hist[-1].get("t"), (int, float)) else None
        recent = last_t is not None and now - last_t < 600
        if state in OPEN or (state in TERMINAL and recent):
            exp = f", expires in {i['expires_in_s']:.0f} s" if isinstance(i.get("expires_in_s"), (int, float)) and state == "pending_approval" else ""
            reason = f" ({hist[-1].get('reason')})" if hist and state in TERMINAL else ""
            lines.append(f"#{i.get('id')} {i.get('kind')} on {i.get('uids')}: {state}{exp}{reason}")
    return lines


MAX_LINES = 60


def compose(wake, mine: list[str], budget_line: str, consult_notes: list[str], now: float,
            first: bool = False) -> str:
    """The update the model is woken with."""
    head = f"Watch update {wake.n} at {hms(now)} ({wake.reason})."
    parts = [head]
    if first:
        parts.append("Your watch starts now. Read the picture (fleet, tracks, inbox, your own "
                     "intents) and give the operator a short summary. Propose only if something "
                     "clearly needs it.")
    if wake.why:
        parts.append("Why you are woken: " + "; ".join(wake.why) + ".")
    elif wake.reason == "heartbeat":
        parts.append("Why you are woken: routine check. Look for slow developments (tracks closing "
                     "on our vehicles or areas, proposals waiting long) and stale proposals of yours.")
    if wake.changes:
        lines = [render(c) for c in wake.changes]
        shown = lines[-MAX_LINES:]
        parts.append(f"Changes since your last update ({len(lines)}, oldest first):\n" + "\n".join(f"- {s}" for s in shown))
        if len(lines) > len(shown):
            parts.append(f"({len(lines) - len(shown)} earlier changes not listed: get_changes has them.)")
    else:
        parts.append("No changes since your last update.")
    for note in consult_notes:
        parts.append(note)
    parts.append("Your proposals: " + ("; ".join(mine) if mine else "none open."))
    parts.append(budget_line)
    parts.append("Read what you need with the tools, then act or say no action. End with your "
                 "note for the operator.")
    return "\n\n".join(parts)
