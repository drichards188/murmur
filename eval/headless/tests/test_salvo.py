"""Rules 35 headless (docs/SALVO.md, bd murmur-4nik.2): the HIV3 salvo situation's timeline on the
real `murmur-headless`, as its director's notes say it plays live. Plan 1 asks 6 of HIVE 1's 18
rounds on target A, signed once by op-2 and op-3, and releases all six 2 s apart; plan 2 waits for
plan 1's last round (`after_step`), nominates 6 on target B 5 s later, and the timeline's scripted
operator ceases it after its third (`cease_after_rounds`): rounds 4 to 6 never go, 9 are left.
The log replays with its anchors, and one seed twice is the same episode.

Rules 35 is built behind `RULES_NEWEST`, so the run asks for it (`MURMUR_RULES`, which
eval/headless/sim.py passes to the simulator) until the flip.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from eval.headless.tests.test_engage_space import binary, episode, events, first, replays, same_episode  # noqa: F401

SIGNERS = {"signers": {"preset": "scripted"}}
DURATION_S = 300.0  # both plans assessed by about 230 s


def _salvo(rec: dict[str, Any]) -> tuple[list[dict[str, Any]], dict[int, list[dict[str, Any]]]]:
    evs = events(rec)
    noms = [e for e in evs if e["type"] == "intent.created" and e["intent"]["kind"] == "engage"]
    rounds: dict[int, list[dict[str, Any]]] = {}
    for e in evs:
        if e["type"] in ("engage.effect", "engage.round"):
            rounds.setdefault(int(e["intent"]), []).append(e)
    return noms, rounds


def test_the_hiv3_salvo_plays_headless_as_its_notes_say(binary: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("MURMUR_RULES", "35")
    rec = episode("hiv3-salvo", tmp_path / "a", duration_s=DURATION_S, **SIGNERS)
    evs = events(rec)
    noms, rounds = _salvo(rec)
    assert [n["intent"]["params"]["track"] for n in noms] == ["T-0001", "T-0002"]
    a, b = (int(n["intent"]["id"]) for n in noms)
    for n in noms:
        assert n["intent"]["source"] == {"kind": "agent", "id": "agent-scripted"}
        signed = {e["by"] for e in evs if e["type"] in ("approval.concurred", "approval.granted") and e["intent"] == n["intent"]["id"]}
        assert signed == {"op-2", "op-3"}, "two signers, once each, neither the proposer"
    # plan 1: the release and rounds 2 to 6, 2 s apart, then the fire ends and the assessment
    ra = rounds[a]
    assert [e["type"] for e in ra] == ["engage.effect"] + ["engage.round"] * 5
    assert [e.get("round") for e in ra[1:]] == [2, 3, 4, 5, 6]
    assert {ra[k + 1]["t"] - ra[k]["t"] for k in range(5)} == {2000}
    first(evs, "engage.fire_ended", intent=a)
    assert first(evs, "engage.assessed", intent=a)["result"] == "ceased"
    # plan 2: nominated after plan 1's last round (`after_step`, `after_s` 5), ceased after its third
    assert noms[1]["t"] >= ra[-1]["t"] + 5000
    rb = rounds[b]
    assert [e["type"] for e in rb] == ["engage.effect", "engage.round", "engage.round"]
    stop = first(evs, "engage.stopped", intent=b)
    assert "op-planner" in stop["reason"] and "scripted, not a person" in stop["reason"]
    assert not [e for e in evs if e["type"] == "engage.round" and e["intent"] == b and e["t"] > stop["t"]]
    assert (rb[-1].get("weapon") or {}).get("left") == 9, "18 less six and three"
    # the timeline's own records: the wait for plan 1, and the Cease by the scripted operator
    script = {s.get("step"): s for s in rec["script"] if s.get("kind") == "step"}
    assert script["plan-2-salvo-on-b"]["after"] == "plan-1-salvo-on-a" and script["plan-2-salvo-on-b"]["intent"] == b
    (cease,) = [s for s in rec["script"] if s.get("kind") == "cease"]
    assert (cease["operator"], cease["rounds"], cease["status"]) == ("op-planner", 3, 200)
    replays(tmp_path / "a")
    same_episode(rec, episode("hiv3-salvo", None, duration_s=DURATION_S, **SIGNERS))
