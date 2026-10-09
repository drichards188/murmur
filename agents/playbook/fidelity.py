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
import sys
from pathlib import Path

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
