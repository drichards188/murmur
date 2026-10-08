"""Usage limits of the model CLIs: recognise the message, and when the limit resets.

Neither CLI waits for a reset. Claude Code (`claude -p`) ends the turn with a result like
"You've hit your session limit · resets 3:45pm" (also weekly, Opus and Sonnet variants, a
time zone in brackets, or a date for a weekly limit) and exits non-zero; its stream also
carries `rate_limit_event`s whose `resetsAt` is exact (preferred when present). Codex
(`codex exec`) fails the turn with "You’ve hit your usage limit." (a curly apostrophe,
U+2019) and then "Try again at 3:42 PM.", a date form, or "Try again later." (no time).

`reset_at` is a Unix time, or None when the message gives none.
"""

from __future__ import annotations

import datetime as dt
import re
from dataclasses import dataclass
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError


@dataclass
class LimitHit:
    provider: str  # claude | codex
    kind: str  # session | weekly | opus | sonnet | five_hour | usage | ...
    text: str
    reset_at: float | None  # Unix time; None: the message gives no time

    def as_dict(self) -> dict:
        """For the situation log (`limit`: which limit; a line's `kind` is its own)."""
        return {"provider": self.provider, "limit": self.kind, "text": self.text[:500],
                "reset_at": self.reset_at,
                "reset_iso": _iso(self.reset_at) if self.reset_at else None}


def _iso(t: float) -> str:
    return dt.datetime.fromtimestamp(t, dt.timezone.utc).isoformat(timespec="seconds")


MONTHS = {m: i + 1 for i, m in enumerate(
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"])}
WEEKDAYS = {d: i for i, d in enumerate(["mon", "tue", "wed", "thu", "fri", "sat", "sun"])}

_TIME = r"(?P<h>\d{1,2})(?::(?P<m>\d{2}))?\s*(?P<ap>[ap]\.?m\.?)?"
_TIME_RE = re.compile(rf"^{_TIME}$", re.I)
# "Oct 9", "October 9th", "Oct 9th, 2026", then the time ("at" optional)
_DATE_RE = re.compile(
    rf"^(?P<mon>[a-z]{{3,9}})\.?\s+(?P<day>\d{{1,2}})(?:st|nd|rd|th)?(?:,?\s*(?P<year>\d{{4}}))?,?\s*(?:at\s+)?{_TIME}$",
    re.I)
_WEEKDAY_RE = re.compile(rf"^(?P<wd>[a-z]{{3,9}}),?\s*(?:at\s+)?{_TIME}$", re.I)
_IN_RE = re.compile(r"^in\s+(?:(?P<d>\d+)\s*d(?:ays?)?)?\s*(?:(?P<h>\d+)\s*h(?:ours?|rs?)?)?\s*"
                    r"(?:(?P<m>\d+)\s*m(?:in(?:ute)?s?)?)?$", re.I)


def _zone(name: str | None) -> dt.tzinfo | None:
    if not name:
        return None
    if name.upper() in ("UTC", "GMT", "Z"):
        return dt.timezone.utc
    try:
        return ZoneInfo(name)
    except (ZoneInfoNotFoundError, ValueError):
        return None


def _clock(h: str, m: str | None, ap: str | None) -> tuple[int, int] | None:
    hour, minute = int(h), int(m or 0)
    if ap:
        ap = ap.lower().replace(".", "")
        if not 1 <= hour <= 12:
            return None
        hour = hour % 12 + (12 if ap == "pm" else 0)
    if not (0 <= hour < 24 and 0 <= minute < 60):
        return None
    return hour, minute


def parse_when(when: str, now: dt.datetime) -> float | None:
    """A reset time as the CLIs print it, in `now`'s zone (aware), as the next such moment."""
    w = when.strip().rstrip(".").strip()
    m = _IN_RE.match(w)
    if m and any(m.group(k) for k in ("d", "h", "m")):
        delta = dt.timedelta(days=int(m.group("d") or 0), hours=int(m.group("h") or 0),
                             minutes=int(m.group("m") or 0))
        return (now + delta).timestamp()
    m = _TIME_RE.match(w)
    if m:
        hm = _clock(m.group("h"), m.group("m"), m.group("ap"))
        if hm is None:
            return None
        t = now.replace(hour=hm[0], minute=hm[1], second=0, microsecond=0)
        if t <= now:
            t += dt.timedelta(days=1)
        return t.timestamp()
    m = _DATE_RE.match(w)
    if m and m.group("mon")[:3].lower() in MONTHS:
        hm = _clock(m.group("h"), m.group("m"), m.group("ap"))
        if hm is None:
            return None
        year = int(m.group("year") or now.year)
        try:
            t = now.replace(year=year, month=MONTHS[m.group("mon")[:3].lower()], day=int(m.group("day")),
                            hour=hm[0], minute=hm[1], second=0, microsecond=0)
        except ValueError:
            return None
        if not m.group("year") and t < now - dt.timedelta(days=1):
            t = t.replace(year=year + 1)
        return t.timestamp()
    m = _WEEKDAY_RE.match(w)
    if m and m.group("wd")[:3].lower() in WEEKDAYS:
        hm = _clock(m.group("h"), m.group("m"), m.group("ap"))
        if hm is None:
            return None
        days = (WEEKDAYS[m.group("wd")[:3].lower()] - now.weekday()) % 7
        t = (now + dt.timedelta(days=days)).replace(hour=hm[0], minute=hm[1], second=0, microsecond=0)
        if t <= now:
            t += dt.timedelta(days=7)
        return t.timestamp()
    return None


def _now(now: dt.datetime | None, tz: dt.tzinfo | None) -> dt.datetime:
    if now is None:
        now = dt.datetime.now().astimezone()
    if now.tzinfo is None:
        now = now.astimezone()
    return now.astimezone(tz) if tz else now


# Only the CLI's own phrasings, at the start of a line: "You've hit your session limit ·
# resets 3:45pm" (any limit), the older "Claude AI usage limit reached|<epoch>", and
# "5-hour limit reached ∙ resets 4pm". Never prose that merely mentions a limit: the
# callers apply this to the CLI's result and stderr only, not to the model's text.
_CLAUDE_HIT = re.compile(
    r"^\s*(?:you['’]ve hit your\b[^\n]*\blimit\b"
    r"|claude(?: ai)? usage limit reached\s*\|\s*\d{9,11}"
    r"|[\w -]{0,24}\blimit reached\s*[·∙•|.-]\s*resets\b)",
    re.I | re.M)
_CLAUDE_KIND = re.compile(r"\b(session|weekly|opus|sonnet|fable|5-hour|five[- ]hour|daily|usage)\s+limit", re.I)
_RESETS = re.compile(
    r"\breset(?:s|ting)?\b(?:\s+at)?\s+(?P<when>[^()·∙|\n]+?)\s*(?:\((?P<tz>[A-Za-z_]+(?:/[A-Za-z0-9_+\-]+)*)\))?\s*\.?\s*$",
    re.I | re.M)
_EPOCH = re.compile(r"\|(\d{9,11})\b")


def _kind(text: str) -> str:
    m = _CLAUDE_KIND.search(text)
    if not m:
        return "usage"
    k = m.group(1).lower()
    return "five_hour" if k in ("5-hour", "five-hour", "five hour") else k


def parse_claude(text: str, now: dt.datetime | None = None) -> LimitHit | None:
    """Claude Code's limit message, or None when `text` is not one. For the CLI's own
    output (a result's text, stderr), never the model's prose."""
    if not text or "limit" not in text.lower() or not _CLAUDE_HIT.search(text):
        return None
    e = _EPOCH.search(text)
    if e:  # an older form: "Claude AI usage limit reached|1759999999"
        return LimitHit("claude", _kind(text), text, float(e.group(1)))
    reset = None
    m = _RESETS.search(text)
    if m:
        tz = _zone(m.group("tz"))
        reset = parse_when(m.group("when"), _now(now, tz))
    return LimitHit("claude", _kind(text), text, reset)


def from_rate_limit_event(info: dict | None) -> LimitHit | None:
    """A Claude `rate_limit_event`'s info, when it says the limit is hit (`rejected`)."""
    if not isinstance(info, dict) or info.get("status") != "rejected":
        return None
    reset = info.get("resetsAt")
    kind = str(info.get("rateLimitType") or "usage")
    return LimitHit("claude", kind, f"rate limit {kind} rejected",
                    float(reset) if isinstance(reset, (int, float)) else None)


_CODEX_HIT = re.compile(r"you['’]ve hit your usage limit", re.I)
_TRY_AGAIN = re.compile(r"try again (?P<rest>at\s+.+?|in\s+.+?|later)\s*\.?\s*$", re.I | re.M)


def parse_codex(text: str, now: dt.datetime | None = None) -> LimitHit | None:
    """Codex's limit message (`turn.failed`), or None when `text` is not one."""
    if not text or not _CODEX_HIT.search(text):
        return None
    reset = None
    m = _TRY_AGAIN.search(text)
    if m:
        rest = m.group("rest").strip()
        if rest.lower().startswith("at "):
            reset = parse_when(rest[3:], _now(now, None))
        elif rest.lower().startswith("in "):
            reset = parse_when(rest, _now(now, None))
    return LimitHit("codex", "usage", text, reset)
