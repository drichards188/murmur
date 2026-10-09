#!/usr/bin/env python3
"""Rationale fidelity, checked from an audit log (CRITERIA.md): every proposal of the playbook's
carries its evidence record after the delimiter, names its play and version and its withdraw
condition, and its text is exactly what the play's template renders from that record, so every
fact it cites is a value the engine read, at the age it read it.

    .venv/bin/python agents/playbook/fidelity.py logs/audit/<run>.jsonl [--agent agent-playbook]

Prints one line per proposal and exits 1 if any fails. Reads the log only.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from playbook import rationale as R  # noqa: E402
from playbook.plays import PLAYS  # noqa: E402


def check(rationale: str) -> list[str]:
    """What is wrong with one rationale; empty when it is faithful to its record."""
    text, body = R.split(rationale)
    if body is None:
        return ["no evidence record after the delimiter"]
    rec = R.parse_record(rationale)
    if rec is None:
        return ["the evidence record does not parse"]
    problems = []
    play, _, version = str(rec.get("play", "")).partition("@")
    if play not in PLAYS or not version:
        return [f"unknown play {rec.get('play')!r}"]
    if not text.startswith(f"[{play} v{version}]"):
        problems.append("the text does not name its play and version first")
    if rec.get("withdraw_if") and "Will withdraw if" not in text:
        problems.append("the withdraw condition is not stated")
    try:
        rendered = PLAYS[play].render(rec)
    except Exception as e:  # noqa: BLE001 (a record the template cannot read is a failure)
        return problems + [f"the template cannot render the record ({type(e).__name__}: {e})"]
    if rendered.rstrip() != text.rstrip():
        problems.append("the text is not what the template renders from the record")
    return problems + untraced(text, rec)


# ---- every number in the text traces to a non-null field of the record ------------------------------

NUM = re.compile(r"(?<![\w.:-])(course )?(-?\d+(?:\.\d+)?)(?:( ?)(m/s|%|min\b|m\b|s\b|x\b|points\b))?")
TIME = re.compile(r"\b\d{2}:\d{2}:\d{2}Z\b")
IDENT = re.compile(r"\b(?:i\d+|v\d+|uid \d+)\b")


def _leaves(x: Any, key: str = "") -> list[tuple[str, Any]]:
    """(key, value) of every leaf; a fact's `value` is keyed by its ref's last part
    (`vehicle:3.battery_pct` -> battery_pct)."""
    if isinstance(x, dict):
        out: list[tuple[str, Any]] = []
        ref = str(x.get("ref") or "")
        for k, v in x.items():
            kk = ref.rsplit(".", 1)[-1] if k == "value" and ref else k
            out += _leaves(v, kk)
        return out
    if isinstance(x, list):
        out = [(f"len:{key}", len(x))]
        for v in x:
            out += _leaves(v, key)
        return out
    return [(key, x)]


def _shows(v: float, token: str, scale: float = 1.0) -> bool:
    d = len(token.split(".")[1]) if "." in token else 0
    return f"{v / scale:.{d}f}" == token or (d == 0 and f"{v / scale:03.0f}" == token)


def untraced(text: str, rec: dict[str, Any]) -> list[str]:
    """The numbers in a rationale's text that no non-null field of its record shows at the text's
    precision and unit (m/s from a *_mps field, % and points from a *pct* field, m from *_m, s and
    min from *_s, a course from course_deg, a factor from *factor*; a bare number from any
    numeric field or a list's length). Quoted text and the record's own strings (a trigger, a
    withdraw condition, a callsign, the preview's summary) are taken as they are; identifiers
    (i101, v1, uid 3) are not numbers. A time (14:38:20Z) must be some timestamp of the record."""
    leaves = [(k, v) for k, v in _leaves(rec) if v is not None]
    nums = [(k, float(v)) for k, v in leaves if isinstance(v, (int, float)) and not isinstance(v, bool)]
    s = re.sub(r'"[^"]*"', " ", text)
    for k, v in sorted({(k, v) for k, v in leaves if isinstance(v, str)}, key=lambda kv: -len(kv[1])):
        if len(v) >= 3 and re.search(r"[A-Za-z]", v):
            s = s.replace(v, " ")
    problems = []
    for tok in TIME.findall(s):
        if not any(v > 1e9 and time.strftime("%H:%M:%SZ", time.gmtime(v)) == tok for _, v in nums):
            problems.append(f"the time {tok} is no timestamp of the record")
    s = TIME.sub(" ", s)
    s = IDENT.sub(" ", s)
    s = re.sub(r"\[[\w.]+ v\d+\]", " ", s)
    for m in NUM.finditer(s):
        course, tok, _, unit = m.group(1), m.group(2), m.group(3), m.group(4)
        if course:
            ok = any(k == "course_deg" and _shows(v, tok) for k, v in nums)
        elif unit == "m/s":
            ok = any(k.endswith("mps") and _shows(v, tok) for k, v in nums)
        elif unit in ("%", "points"):
            ok = any("pct" in k and _shows(v, tok) for k, v in nums) or (
                unit == "points" and any(k.startswith("len:") and _shows(v, tok) for k, v in nums))
        elif unit == "min":
            ok = any(k.endswith("_s") and _shows(v, tok, 60.0) for k, v in nums)
        elif unit == "m":
            ok = any(k.endswith("_m") and _shows(v, tok) for k, v in nums)
        elif unit == "s":
            ok = any(k.endswith("_s") and _shows(v, tok) for k, v in nums)
        elif unit == "x":
            ok = any("factor" in k and _shows(v, tok) for k, v in nums)
        else:
            ok = any(_shows(v, tok) for _, v in nums)
        if not ok:
            what = f"course {tok}" if course else f"{tok}{(m.group(3) or '') + unit if unit else ''}"
            problems.append(f"the number {what} in the text traces to no field of the record")
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("audit_log", type=Path)
    ap.add_argument("--agent", default="agent-playbook")
    args = ap.parse_args(argv)
    bad = n = 0
    for line in args.audit_log.read_text().splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        i = e.get("intent") if e.get("type") == "intent.created" else None
        if not isinstance(i, dict) or (i.get("source") or {}).get("id") != args.agent:
            continue
        n += 1
        problems = check(str(i.get("rationale") or ""))
        rec = R.parse_record(str(i.get("rationale") or "")) or {}
        bad += bool(problems)
        print(f"i{i.get('id')} {rec.get('play')} on {rec.get('target')}: {'; '.join(problems) or 'faithful'}")
    print(f"{n} proposals, {n - bad} faithful")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
