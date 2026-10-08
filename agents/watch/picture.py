"""What the driver reads from the bridge, and how it tells the model.

The driver reads as the same agent the model acts as, over MCP (the agent's key in a
credentials run): `get_changes` every couple of seconds for what changed, and, while a track
is declared hostile, `get_nomination_options` now and then to see whether a nomination has
become possible (a `nomination.possible` change), for the armed platforms that could pass
only and asking only whether each passes. Nothing it reads is logged by the bridge.
"""

from __future__ import annotations

import datetime as dt
import math
import time
from typing import Any, Callable

TERMINAL = ("rejected", "expired", "voided", "done", "failed", "effected", "aborted")
OPEN = ("proposed", "checked", "pending_approval", "approved", "queued", "dispatched", "auto_approved")


class NoChangesTool(RuntimeError):
    pass


# Which armed platforms the driver asks about for a hostile track. The checks refuse an
# intercept longer than the policy's `max_transit_s` (`get_policy`: policy.rules.engage, 10 to
# 7200 s; read once a session, TRANSIT_S when the policy has no engagement section), so a
# platform farther than its cruise speed plus the track's speed times that is out of reach. A
# platform with no cruise figure in the catalog is asked about within FALLBACK_REACH_M.
TRANSIT_S = 900.0
FALLBACK_REACH_M = 30_000.0


def max_transit_s(policy: dict) -> float:
    """The policy's longest intercept (`get_policy`), else TRANSIT_S."""
    rules = ((policy.get("policy") or {}).get("rules") or {}) if isinstance(policy, dict) else {}
    v = (rules.get("engage") or {}).get("max_transit_s")
    return float(v) if isinstance(v, (int, float)) and v > 0 else TRANSIT_S


def distance_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    p1, p2 = math.radians(lat1), math.radians(lat2)
    a = (math.sin((p2 - p1) / 2) ** 2
         + math.cos(p1) * math.cos(p2) * math.sin(math.radians(lon2 - lon1) / 2) ** 2)
    return 2 * 6371000.0 * math.asin(math.sqrt(a))


def candidates(vehicles: list[dict], track: dict, cruise: dict, transit_s: float = TRANSIT_S) -> list[int]:
    """The armed platforms that could pass on `track`, nearest first: armed, link up,
    navigation ready, a position, and within reach (`get_fleet` compact entries; `cruise` maps
    a platform id to its catalog cruise speed; `transit_s` is the policy's longest intercept)."""
    pos = track.get("position")
    if not (isinstance(pos, (list, tuple)) and len(pos) == 2):
        return []
    speed = track.get("speed_mps") if isinstance(track.get("speed_mps"), (int, float)) else 0.0
    out = []
    for v in vehicles:
        if not (v.get("armed") is True and v.get("link_lost") is False and v.get("nav_ready") is True):
            continue
        if not all(isinstance(v.get(k), (int, float)) for k in ("lon", "lat")):
            continue
        c = cruise.get(v.get("platform"))
        reach = (c + speed) * transit_s if isinstance(c, (int, float)) and c > 0 else FALLBACK_REACH_M
        d = distance_m(v["lon"], v["lat"], pos[0], pos[1])
        if d <= reach:
            out.append((d, v["uid"]))
    return [u for _, u in sorted(out)]


class Poller:
    """The change feed for one agent session, reconnecting when the bridge goes away."""

    def __init__(self, connect: Callable[[], Any], nomination_every_s: float = 15.0):
        self.connect = connect  # () -> an AgentSession (not yet entered)
        self.session = None
        self.cursor: str | int | None = None  # the feed's cursor, "<feed>:<n>"
        self.run: str | None = None
        self.hostile: set[str] = set()
        self.passing: dict[str, list] = {}
        self.checked: dict[str, float] = {}
        self.nomination_every_s = nomination_every_s
        self.cruise: dict | None = None  # platform id -> catalog cruise speed (read once a session)
        self.transit_s: float | None = None  # the policy's max_transit_s (read once a session)

    async def open(self) -> dict:
        if self.session is not None:
            await self.close()
        s = self.connect()
        await s.__aenter__()
        self.session = s
        self.cruise = None
        self.transit_s = None
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
        check, `nomination.closed` when it loses the last one. A read-only dry run, which the
        bridge allows an agent once a second (the model shares that allowance): one track per
        call, the one read longest ago, each track at most every `nomination_every_s`, so a
        poll never waits on it. It asks about the platforms that could pass (`candidates`)
        and only whether each passes (`passed_only`): every armed platform with the full
        answer was 213 557 characters at 100 stand-ins."""
        now = time.time() if now is None else now
        due = [t for t in self.hostile if now - self.checked.get(t, 0.0) >= self.nomination_every_s]
        if not due:
            return []
        track = min(due, key=lambda t: (self.checked.get(t, 0.0), t))
        self.checked[track] = now
        s = self.session
        try:
            if self.cruise is None:
                catalog = await s.call("get_platforms")
                self.cruise = {p.get("id"): p.get("cruise_mps") for p in catalog.get("platforms") or []}
            if self.transit_s is None:
                self.transit_s = max_transit_s(await s.call("get_policy"))
            fleet = await s.call("get_fleet", compact=True)
            hostile = await s.tracks(affiliation="hostile")
        except Exception:  # noqa: BLE001 (the next poll tries again)
            return []
        t = next((x for x in hostile if x.get("id") == track), None)
        if t is None:
            return []  # gone, or no longer estimated hostile: the change feed says which
        uids = candidates(fleet.get("vehicles") or [], t, self.cruise, self.transit_s)
        passing: list[dict] = []
        if uids:
            result = await s.session.call_tool("get_nomination_options",
                                               {"track_id": track, "uids": uids, "passed_only": True})
            body = _content(result)
            if getattr(result, "is_error", None) or getattr(result, "isError", None):
                return []  # rate limited (the model read one this second), or the track is gone
            passing = [{"uid": o.get("uid"), "callsign": o.get("callsign")}
                       for o in body.get("options") or [] if o.get("passed")]
        had = self.passing.get(track) or []
        self.passing[track] = passing
        if passing and not had:
            return [{"kind": "nomination.possible", "t": now, "track": track, "platforms": passing}]
        if had and not passing:
            return [{"kind": "nomination.closed", "t": now, "track": track}]
        return []

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
#
# Everything in an update is data. Free text that comes from outside the driver (a contact's
# AIS broadcast, an autopilot's status text, an alert's detail, the engine's evidence, a
# reason or an operator's note) is shown quoted, on one line, cut to QUOTE_MAX characters and
# labelled with where it comes from, so it reads as a quotation, never as an instruction
# (the prompt says so too: agents/watch/prompt.md).

QUOTE_MAX = 160


def quote(text: Any, n: int = QUOTE_MAX) -> str:
    """Free text as a quotation: one line, double quotes inside made single, cut to `n`."""
    s = " ".join(str(text).split()).replace('"', "'")
    if len(s) > n:
        s = s[:n].rstrip() + "..."
    return f'"{s}"'


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


def _ais(c: dict) -> str:
    a = c.get("ais")
    if not isinstance(a, dict):
        return ""
    return (f"; it broadcasts AIS (its own claim, unverified) name {quote(a.get('name', ''), 40)}, "
            f"type {quote(a.get('ship_type', ''), 40)}")


def _why(c: dict) -> str:
    """An intent's last reason and, for a rejection, the operator's note, each labelled."""
    out = ""
    if c.get("reason"):
        out += f"; recorded reason {quote(c['reason'])}"
    note = c.get("operator_note")
    if isinstance(note, dict) and note.get("note"):
        out += f"; operator {note.get('by')}'s note on the decision {quote(note['note'])}"
    return out


def render(c: dict) -> str:
    """One line for one change."""
    k = c.get("kind", "?")
    t = hms(c.get("t"))
    tr = c.get("track")
    if k == "intent.new":
        by = "you" if c.get("mine") else (c.get("source") or {}).get("id", "?")
        return f"{t} intent #{c.get('intent')} {c.get('task')} on {c.get('uids')} by {by}: {c.get('state')}{_why(c)}"
    if k == "intent.state":
        whose = "yours" if c.get("mine") else (c.get("source") or {}).get("id", "?")
        return (f"{t} intent #{c.get('intent')} {c.get('task')} on {c.get('uids')} ({whose}): "
                f"{c.get('from')} -> {c.get('to')}{_why(c)}")
    if k == "track.new":
        src = ", ".join(f"uid {s.get('uid')} {s.get('sensor')}" for s in c.get("sources") or [])
        conf = c.get("affiliation_confidence")
        conf_s = f" {conf:.2f}" if isinstance(conf, (int, float)) else ""
        return (f"{t} new track {tr}: {c.get('domain')} {c.get('identity') or 'unclassified'}, "
                f"estimated {c.get('affiliation')}{conf_s}, at {_pos(c.get('position'))}, "
                f"{c.get('speed_mps')} m/s course {c.get('course_deg')}, seen by {src or '?'}{_ais(c)}")
    if k == "track.affiliation":
        return f"{t} track {tr} affiliation estimate {c.get('from')} -> {c.get('to')}"
    if k == "track.declared":
        was = f" (was declared {c['from']})" if c.get("from") else ""
        return f"{t} track {tr} declared {c.get('declared')} by {c.get('by')}{was}"
    if k == "track.declaration_confirmed":
        return f"{t} track {tr}: the declaration was confirmed by {c.get('confirmed_by')}"
    if k == "track.declaration_revoked":
        return f"{t} track {tr}: its declaration ({c.get('was')}) was revoked"
    if k == "track.identity_conflict":
        return (f"{t} track {tr} identity conflict, the tracker's note (it quotes what the contact "
                f"broadcasts, unverified) {quote(c.get('conflict', ''))}")
    if k == "track.identity_cleared":
        return f"{t} track {tr}: its identity conflict cleared"
    if k in ("track.stale", "track.active"):
        return f"{t} track {tr} is {k.split('.')[1]}"
    if k == "track.dropped":
        return f"{t} track {tr} dropped by the tracker{' (still counted as a lost contact)' if c.get('lost') else ''}"
    if k == "review.flag":
        return (f"{t} review flag {c.get('flag')} ({c.get('flag_kind')}) on {c.get('tracks')}, the engine's "
                f"evidence {quote(c.get('evidence', ''))}")
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
        return f"{t} {_who(c)} {c.get('event_kind')}, its autopilot's text {quote(c.get('text', ''))}"
    if k == "vehicle.events_truncated":
        return f"{t} more vehicle events than shown (get_recent_events has them)"
    if k in ("alert.raised", "alert.changed"):
        was = ""
        if k == "alert.changed":
            f = c.get("from") or {}
            was = f" (was {f.get('alert_kind')} {f.get('severity')})"
        return (f"{t} alert {c.get('alert_kind')} ({c.get('severity')}){was} {_who(c)}, the bridge's detail "
                f"{quote(c.get('detail', ''))}")
    if k == "alert.cleared":
        return f"{t} alert cleared: {c.get('alert_kind')} {_who(c)}"
    if k == "condition.flapping":
        about = f"track {tr}" if tr else (f"alert {c.get('alert')}" if c.get("alert") else _who(c))
        return (f"{t} {about}: {c.get('condition')} came and went {c.get('cycles')} times within "
                f"{c.get('window_s', 0):.0f} s")
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
            reason = f", recorded reason {quote(hist[-1].get('reason', ''))}" if hist and state in TERMINAL else ""
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
    dropped = getattr(wake, "notes_dropped", 0) or 0
    if dropped:
        parts.append(f"({dropped} older notes were not kept: too many between updates.)")
    held = getattr(wake, "held", None) or []
    if held:
        parts.append("Held back as repeats of something you were told in the last few minutes "
                     "(each wakes you when its interval ends): " + "; ".join(held) + ".")
    for note in consult_notes:
        parts.append(note)
    parts.append("Your proposals: " + ("; ".join(mine) if mine else "none open."))
    parts.append(budget_line)
    parts.append("Read what you need with the tools, then act or say no action. End with your "
                 "note for the operator.")
    return "\n\n".join(parts)
