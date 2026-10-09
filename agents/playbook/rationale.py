"""Rationales from facts with ages (research report C.2.4).

Every candidate carries an **evidence record** built only from values the tick read: the play
and its version, the config label, the facts it relied on (each with its reference, value,
confidence, reason and age at the tick), what it derived (CPA, TCPA, ETA, the radius), the
thresholds and why each exists, the alternatives it considered, the rule book's verdict, the
preview's tier, its withdraw condition and, inside a cooldown, the material change that let it
propose. The rationale's text is rendered from that record by a fixed template per play, and
the record itself rides after a delimiter line, so a script can check the text against it from
the audit log (the bridge logs the rationale verbatim) and the console folds it behind a
toggle.

Determinism: floats are rounded at fixed precision when the record is built (`r1`, `r2`, `r3`,
`ll`), the record is serialized with sorted keys, and the templates format numbers at fixed
precision too, so the same reads give the same bytes.
"""

from __future__ import annotations

import json
from typing import Any

DELIMITER = "--- evidence record (playbook, JSON) ---"
SEP = "\n\n" + DELIMITER + "\n"


def _clean(x: float) -> float:
    return 0.0 if x == 0 else x  # no -0.0


def r1(x: float | None) -> float | None:
    return None if x is None else _clean(round(float(x), 1))


def r2(x: float | None) -> float | None:
    return None if x is None else _clean(round(float(x), 2))


def r3(x: float | None) -> float | None:
    return None if x is None else _clean(round(float(x), 3))


def r0(x: float | None) -> float | None:
    return None if x is None else _clean(float(round(float(x))))


def ll(p: Any) -> list[float] | None:
    """A [lon, lat] at 6 decimals (about 0.1 m)."""
    if p is None:
        return None
    return [_clean(round(float(p[0]), 6)), _clean(round(float(p[1]), 6))]


def dumps(record: dict[str, Any]) -> str:
    return json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def compose(text: str, record: dict[str, Any]) -> str:
    """The rationale: the rendered text, the delimiter line, the record."""
    return text.rstrip() + SEP + dumps(record)


def split(rationale: str) -> tuple[str, str | None]:
    """(the text, the record's JSON or None) of a rationale."""
    i = rationale.find(SEP)
    if i < 0:
        return rationale, None
    return rationale[:i], rationale[i + len(SEP):]


def parse_record(rationale: str) -> dict[str, Any] | None:
    """The evidence record riding in a rationale, or None (another proposer's, or garbled)."""
    _, body = split(rationale)
    if body is None:
        return None
    try:
        rec = json.loads(body)
    except ValueError:
        return None
    return rec if isinstance(rec, dict) else None


# ---- pieces every template uses ---------------------------------------------------------------


def mins(s: float | None) -> str:
    if s is None:
        return "?"
    return f"{s / 60.0:.1f} min" if s >= 90 else f"{s:.0f} s"


def quote(text: Any, n: int = 80) -> str:
    """Free text from outside the engine (an AIS name, a reason, a note): one line, quoted."""
    s = " ".join(str(text).split()).replace('"', "'")
    if len(s) > n:
        s = s[:n].rstrip() + "..."
    return f'"{s}"'


def head(record: dict[str, Any]) -> str:
    play, _, version = str(record["play"]).partition("@")
    return f"[{play} v{version}]"


def tail(record: dict[str, Any]) -> str:
    """The withdraw condition, the material change (inside a cooldown), the rule book, the tier."""
    parts = []
    w = record.get("withdraw_if") or []
    if w:
        parts.append("Will withdraw if " + "; or ".join(w) + ".")
    mc = record.get("material_change")
    if mc:
        parts.append(f"Proposed again inside a cooldown ({mc['cooldown']}): material change: {mc['change']}.")
    rb = record.get("rulebook_v2")
    if rb is not None:
        parts.append("Rule book v2: " + ("; ".join(rb) if rb else "nothing against") + ".")
    pv = record.get("preview")
    if pv:
        if pv.get("shadow"):
            parts.append("Shadow: not previewed, not proposed.")
        else:
            parts.append(f"Preview: {pv.get('checks')}; tier {pv.get('tier')} ({pv.get('rule')}).")
    return " ".join(parts)
