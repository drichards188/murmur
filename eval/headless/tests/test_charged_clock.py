"""Scoring v2's latency-charged clock (eval/SCORING-V2.md 4.2; eval/headless/latency.py,
officer.py `OpenTurn`, docs/HEADLESS.md 14.1.1) on the real `murmur-headless` binary with a fake
Anthropic client: no network, nothing billed.

- the constants and the charge formula; the loop runs events in time order and refuses one in
  the past;
- a charged model turn: each call's answer is applied at the call's time plus its charge, the
  next call is made then, the turn ends when its last answer is due, the simulator's clock moves
  with it;
- the operator's decisions, the script's proposals and the samplers fire at their own times
  inside a turn, in order;
- a proposal at the end of a turn takes effect at its charged time (the audit log's creation),
  not at the wake's start as on the paused clock;
- a charged episode repeats exactly; an answer due after the end never reaches the world; a
  failed call is charged its fixed part; the playbook acts its tick charge after it read;
- the paused clock's records are byte-identical to before the charged clock existed (digests of
  three records taken on the code before it, fixed seeds).
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eval.headless import REPO, episode as EP
from eval.headless.episode import PRIORITY, Episode, Loop, Options
from eval.headless.families import generate
from eval.headless.latency import CLOCKS, LATENCY_FILE, Latency
from eval.headless.officer import ms_up
from eval.headless.situations import Calibration, from_situation

from .fake_anthropic import FakeAnthropic, LookScript

#: long enough that the approval flood and the operator's answers fall inside the model's turns
LONG = Latency(a_s=25.0)


@pytest.fixture(scope="module", autouse=True)
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


class LoggedLoop(Loop):
    """The episode's loop, keeping (time, process) of every event it runs."""

    def __init__(self, sim: Any):
        super().__init__(sim)
        self.events: list[tuple[float, str]] = []
        LoggedLoop.last = self

    def run(self, end: float) -> None:
        for name, h in list(self.handlers.items()):
            self.handlers[name] = self._logged(name, h)
        super().run(end)

    def _logged(self, name: str, h: Any) -> Any:
        def handler(t: float) -> Any:
            self.events.append((t, name))
            return h(t)

        return handler


def run(family: str, seed: int = 1000, agent: str = "model", script: Any = None, clock: str = "charged",
        latency: Latency | None = None, operator: dict[str, Any] | None = None) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="clock-") as td:
        e, contacts = generate(family, seed, Path(td), world="v2")
        client = FakeAnthropic(script if script is not None else LookScript()) if agent == "model" else None
        opts = Options(agent=agent, model_client=client, calibration=Calibration.load(), scoring="v2", clock=clock,
                       latency=latency, operator=operator)
        return Episode(e, seed, Path(td), opts, contacts_path=contacts).run()


@pytest.fixture(scope="module")
def flood() -> tuple[dict[str, Any], list[tuple[float, str]]]:
    """S9 (approval-flood) with the approve-all operator, the model on the charged clock with a
    long fixed charge: its turns span the flood and the operator's answers."""
    mp = pytest.MonkeyPatch()
    mp.setattr(EP, "Loop", LoggedLoop)
    try:
        rec = run("approval-flood", latency=LONG, operator={"mode": "approve-all"})
        return rec, LoggedLoop.last.events
    finally:
        mp.undo()


# ---- the constants and the loop ------------------------------------------------------------------


def test_the_constants_and_the_charge(tmp_path: Path):
    lat = Latency.load()
    assert lat == Latency.load(LATENCY_FILE) and lat.source.startswith("placeholders")
    assert CLOCKS == ("paused", "charged")
    tokens = {"input": 30, "cache_write": 970, "cache_read": 9000, "output": 500}
    assert lat.call_s(tokens, 3) == pytest.approx(lat.a_s + lat.b_in_s * 10_000 + lat.b_out_s * 500 + lat.c_s * 3)
    assert Latency(a_s=1, b_in_s=0, b_out_s=0, c_s=0).call_s({}, 0) == 1
    bad = tmp_path / "latency.toml"
    bad.write_text("a_s = -1.0\n")
    with pytest.raises(ValueError, match="a_s"):
        Latency.load(bad)
    assert ms_up(56.29338) == 56.294 and ms_up(56.294) == 56.294 and ms_up(1.0000001) == 1.001
    with pytest.raises(ValueError, match="unknown clock"):
        Episode(from_situation("harbor-watch"), 1, tmp_path, Options(clock="frozen"))


def test_the_loop_runs_in_time_order_and_refuses_the_past():
    sim = SimpleNamespace(t=0.0)
    sim.step = lambda t: setattr(sim, "t", t)
    loop = Loop(sim)  # type: ignore[arg-type]
    seen: list[tuple[float, str]] = []
    plan = {"agent": [5.0, 5.0, None], "operator": [5.0, None], "script": [None]}

    def handler(name: str) -> Any:
        def h(t: float) -> float | None:
            seen.append((t, name))
            return plan[name].pop(0)

        return h

    for name in plan:
        loop.handlers[name] = handler(name)
    loop.at(2.0, "agent")
    loop.at(5.0, "script")
    loop.at(1.0, "operator")
    loop.run(10.0)
    # time first, then the process order (script before operator before agent), then first come
    assert seen == [(1.0, "operator"), (2.0, "agent"), (5.0, "script"), (5.0, "operator"), (5.0, "agent"),
                    (5.0, "agent")]
    assert PRIORITY["script"] < PRIORITY["operator"] < PRIORITY["agent"] and sim.t == 10.0
    loop.handlers["agent"] = lambda t: t - 1.0  # asks for an instant already passed
    loop.at(11.0, "agent")
    with pytest.raises(RuntimeError, match="before the loop's current"):
        loop.run(20.0)


# ---- the charged turn ------------------------------------------------------------------------------


def test_a_turn_advances_the_clock_by_each_calls_charge(flood):
    """Each model call is made at its time on what the conversation holds; its answer (and the
    tool calls in it) is applied at that time plus its charge, on the simulator's millisecond
    grid; the next call is made then; the turn ends when its last answer is due; the simulator's
    clock is there."""
    rec, events = flood
    wakes = rec["model"]["wakes"]
    assert len(wakes) >= 5 and all(w["stop"] == "success" for w in wakes)
    agent_at = {t for t, name in events if name == "agent"}
    for w in wakes:
        calls = w["api_calls"]
        assert calls and calls[0]["t"] == pytest.approx(w["t"], abs=1e-3)
        for i, c in enumerate(calls):
            ran = [tc for tc in w["tool_calls"] if tc["t"] == c["t_answer"]]
            assert c["charge_s"] == pytest.approx(LONG.call_s(c["tokens"], len(ran)), abs=1e-6)
            assert c["t_answer"] == ms_up(c["t"] + c["charge_s"]) and c["charge_s"] >= LONG.a_s
            assert c["t_answer"] in agent_at  # the loop ran the officer then
            if i + 1 < len(calls):
                assert calls[i + 1]["t"] == pytest.approx(c["t_answer"], abs=1e-9)
        assert sum(len([tc for tc in w["tool_calls"] if tc["t"] == c["t_answer"]]) for c in calls) == len(w["tool_calls"])
        assert w["t_end"] == calls[-1]["t_answer"] and w["clock"] == [pytest.approx(w["t"], abs=1e-3), w["t_end"]]
        assert w["charge_s"] == pytest.approx(sum(c["charge_s"] for c in calls), abs=1e-5)
    assert rec["model"]["totals"]["charge_s"] == pytest.approx(sum(w["charge_s"] for w in wakes), abs=1e-4)
    # one turn at a time: a wake starts no earlier than the last one ended
    for a, b in zip(wakes, wakes[1:]):
        assert b["t"] >= a["t_end"] - 1e-9


def test_the_operator_script_and_samplers_run_inside_a_turn_in_order(flood):
    rec, events = flood
    assert [t for t, _ in events] == sorted(t for t, _ in events), "the loop ran an event out of time order"
    turns = [(w["t"], w["t_end"]) for w in rec["model"]["wakes"]]

    def inside(t: float) -> bool:
        return any(a < t < b for a, b in turns)

    # the scripted proposer's flood: each at its own time (at_s + k * window / n), some inside a turn
    item = rec["truth"]["stated"]["flood"]
    props = [s for s in rec["script"] if s["kind"] == "propose"]
    step = item["window_s"] / item["n"]
    assert [s["t"] for s in props] == [pytest.approx(item["at_s"] + k * step, abs=1e-3) for k in range(item["n"])]
    assert any(inside(s["t"]) for s in props) and all(s["state"] == "pending_approval" for s in props)
    # the operator (approve-all, a look every second) answered them meanwhile, in order
    dec = rec["operator_decisions"]
    assert [d["t"] for d in dec] == sorted(d["t"] for d in dec)
    assert sum(inside(d["t"]) for d in dec) >= 3 and all(d["action"] == "approve" for d in dec)
    # every process kept its own clock inside the turns: the operator every second, the truth
    # sampler every 2 s, the picture every 1 s
    for name, every in (("operator", 1.0), ("sampler", 2.0), ("picture", 1.0)):
        n = 0
        for a, b in turns:
            ts = [t for t, nm in events if nm == name and a < t < b]
            n += len(ts)
            assert all(round(y - x, 3) == every for x, y in zip(ts, ts[1:])), (name, a, b, ts[:6])
        assert n > 20, name
    # the officer's own poller too (every 2 s), with the turn's answers between
    polls = [t for t, n in events if n == "agent" and inside(t)]
    assert len(polls) > 10


def test_an_action_takes_effect_at_its_charged_time(flood):
    """The look the model proposes at the end of its turn reaches the bridge at the charged time
    of the answer that asked for it: its audit-log creation, the proposal line, the picture the
    driver read for it; on the paused clock the same model proposes at the wake's start."""
    rec, _ = flood
    (p,) = [ln for ln in rec["agent_log"] if ln["kind"] == "proposal"]
    w = next(w for w in rec["model"]["wakes"] if w["n"] == p["wake"])
    (call,) = [c for c in w["api_calls"] if any(tc["name"] == "propose_intent" and tc["t"] == c["t_answer"]
                                                for tc in w["tool_calls"])]
    assert p["t"] == call["t_answer"] > w["t"] + LONG.a_s
    created = {i["id"]: i for i in rec["v2"]["audit"]["intents"]}[p["intent"]]
    assert created["created"] == pytest.approx(p["t"], abs=1e-3) and created["source"]["id"] == "agent-watch"
    # the operator approved it after it existed, perhaps while the turn was still running
    (d,) = [d for d in rec["operator_decisions"] if d["intent_id"] == p["intent"]]
    assert d["t"] >= p["t"]
    paused = run("approval-flood", clock="paused", operator={"mode": "approve-all"})
    (q,) = [ln for ln in paused["agent_log"] if ln["kind"] == "proposal"]
    wq = next(w for w in paused["model"]["wakes"] if w["n"] == q["wake"])
    assert q["t"] == wq["t"] and "t_end" not in wq and "t" not in wq["tool_calls"][0]


def test_the_record_names_the_condition_and_its_constants(flood):
    rec, _ = flood
    assert rec["clock"]["condition"] == "charged"
    assert rec["provenance"]["clock"] == {"condition": "charged", "latency": LONG.as_dict()}
    assert rec["v2"]["condition"]["clock"] == "charged" and rec["episode_class"] == "scored"
    from eval.headless.v2.aggregate import cond_key, meta_of

    assert cond_key(meta_of(rec)) == "approve-all;watch.toml;charged"


FEED_ID = re.compile(r'"cursor": ?"([0-9a-f]+):')
BUILD = re.compile(r'"build":\{[^}]*\}')
CHAIN = re.compile(r'"(prev|hash)":"[0-9a-f]{64}"')
#: the areas digest the bridge logs covers the areas source's absolute path (`<checkout>/sim/areas
#: [...]`), so it differs between checkouts of the same commit
AREAS = re.compile(r'(\\?"areas_sha256\\?": ?\\?"|\\?"areas\\?": ?\{\\?"sha256\\?": ?\\?")[0-9a-f]{64}')


def _strip(o: Any) -> Any:
    if isinstance(o, dict):
        return {k: _strip(v) for k, v in o.items() if k != "wall_s"}
    if isinstance(o, list):
        return [_strip(v) for v in o]
    return o


def normalized(rec: dict[str, Any]) -> str:
    """The record without what a run cannot repeat or the checkout names: computer time
    (`perf`, every `wall_s`), the provenance (commit, binary), the commit in the audit log and
    the hashes that carry it, the change feed's per-process id, the checkout's path; and the
    leakage scan's own record (`v2.leakage`, added with the scan)."""
    r = {k: v for k, v in rec.items() if k not in ("perf", "provenance")}
    if isinstance(r.get("v2"), dict):
        r["v2"] = {k: v for k, v in r["v2"].items() if k != "leakage"}
    if "audit_text" in r:
        r["audit_text"] = CHAIN.sub("<chain>", BUILD.sub("<build>", r["audit_text"]))
    if isinstance(r.get("audit"), dict):
        r["audit"] = {k: v for k, v in r["audit"].items() if k not in ("head", "sha256")}
    text = json.dumps(_strip(r), sort_keys=True).replace(str(REPO), "<repo>")
    for f in set(FEED_ID.findall(text)):
        text = text.replace(f, "<feed>")
    return AREAS.sub(r"\1<areas>", text)


def test_a_charged_episode_repeats_exactly(flood):
    rec, _ = flood
    again = run("approval-flood", latency=LONG, operator={"mode": "approve-all"})
    assert normalized(again) == normalized(rec)


def test_an_answer_due_after_the_end_never_reaches_the_world():
    """A first call charged past the episode's end: its tool calls never run, nothing is
    proposed, the wake is recorded `episode_end` with the call billed."""
    rec = run("fast-unknown", latency=Latency(a_s=700.0))
    (w,) = rec["model"]["wakes"]
    assert w["stop"] == "episode_end" and w["tool_calls"] == [] and len(w["api_calls"]) == 1
    assert w["due"] > rec["clock"]["duration_s"] and w["t_end"] == rec["clock"]["duration_s"]
    t = rec["model"]["totals"]
    assert t["proposals"] == 0 and t["usd"] > 0 and t["charge_s"] >= 700.0


class Overloaded(Exception):
    status_code = 529


def test_a_failed_call_is_charged_its_fixed_part():
    calls = {"n": 0}
    look = LookScript()

    def script(req: dict[str, Any]) -> Any:
        calls["n"] += 1
        if calls["n"] == 1:
            raise Overloaded("overloaded")
        return look(req)

    lat = Latency.load()
    rec = run("fast-unknown", script=script)
    w0 = rec["model"]["wakes"][0]
    assert w0["stop"] == "error" and w0["api_calls"] == [] and w0["error_charge_s"] == lat.a_s
    assert w0["t_end"] == ms_up(w0["t"] + lat.a_s) and w0["charge_s"] == lat.a_s
    assert rec["model"]["totals"]["errors"] == 1 and rec["model"]["totals"]["proposals"] == 1


def test_the_playbook_acts_its_tick_charge_after_it_read():
    """Each playbook tick decides on the picture read when it began (its plan's time) and its
    proposals reach the bridge `playbook_tick_s` later."""
    lat = Latency.load()
    rec = run("fast-unknown", agent="playbook")
    epoch = rec["clock"]["epoch"]
    plans = {round(p["t"] - epoch, 3) for p in rec["plans"]}
    props = [ln for ln in rec["agent_log"] if ln["kind"] == "proposal"]
    assert props
    created = {i["id"]: i["created"] for i in rec["v2"]["audit"]["intents"]}
    for p in props:
        assert round(p["t"] - lat.playbook_tick_s, 3) in plans, p["t"]
        assert created[p["intent"]] == pytest.approx(p["t"], abs=1e-3)
    assert rec["provenance"]["clock"]["latency"]["playbook_tick_s"] == lat.playbook_tick_s
    assert rec["v2"]["condition"]["clock"] == "charged"


# ---- the paused clock is unchanged -----------------------------------------------------------------

#: sha256 of `normalized(record)` for three paused episodes, the model with the fake look script
#: on Harbor watch seed 1 (v1 scoring), the playbook and the model on v2 fast-unknown, situation
#: seed 1000 (v2 scoring). First taken on the code before the charged clock (39233b2) in the
#: worktree that built it; re-taken in the main checkout once the areas digest, which covers the
#: checkout's path, was masked (`AREAS`): the playbook's matches 39233b2 run in the main checkout,
#: masked. The two v2 digests re-taken when PUMA 1's reserve point moved to Crissy Field (e72d0e2):
#: with the fleet before it, main after the anchors merge still gives the earlier digests. All three
#: re-taken when a Puma's RTL came to end in a landing (murmur-a5v): the code before it gave the
#: earlier digests in that worktree, and each record was byte-identical to it up to its first RTL
#: to AUTOLAND hand-over (a Puma's lost-link failsafe in all three). Re-taken again when a plane
#: that starts in the air got its home on the surface below it and a landed, disarmed aircraft's
#: link a terrain check at rest (murmur-a5v): against the records before it, the engine's logged
#: check inputs (altitudes above the new homes) and REAPER 1's battery (a hundredth of a percent,
#: late) differ, and in v2 the landed PUMA 1's link events after its landing; every proposal,
#: operator decision, order and plan is the same. A change to the simulator, the bridge or the v2
#: world changes them too: re-take them on the code before a change to the driver, never after it.
#: Re-taken for decision rules 17 (murmur-k8i item 2): the code before it gave the earlier digests,
#: and each record is the same but for its audit log's `run.started` (`rules_version` 17 for 16).
#: The two model digests re-taken for the identity's level (SCORING-V3.md 15 (a)): the code before
#: it gave the earlier ones, and each record is the same but for the model's `get_tracks` results
#: (the longer note, each track's `identity.level`) and its transcript's sizes and estimated costs;
#: the playbook's, which keeps no reads, did not move.
#: Re-taken for decision rules 18 and the air velocity gate (murmur-vni): each record is the
#: earlier one but for its audit log's `run.started` (`rules_version` 18 for 17); the gate moved
#: nothing in them.
#: All three re-taken for predicted link coverage (bd murmur-hei), on top of rules 18: the code
#: before it gave rules 18's digests, and each record is the same but for the model's tool list (`get_link_coverage`
#: added: its transcript's sizes and estimated costs) and the playbook's two proposals (play version
#: 2, the predicted-link sentence in the rationale and its audit line); every proposal, decision,
#: order and vehicle state is the same.
#: Re-taken for decision rules 19 (bd murmur-j0x, a live track accepted as behaving now): each record
#: is the earlier one but for its audit log's `run.started` (`rules_version` 19 for 18): with that one
#: number put back, each gives the digest before it.
#: Re-taken for archived weather (murmur-sgok). A separate af45c04 build already differed
#: from the rules-19 fixture: before-weather digests were 4c08b140...e66d42f (model harbor),
#: 3a73c175...4e63d7c (playbook v2), c3e155ea...19f4a2d (model v2). Comparing those complete
#: normalized records with this change, only get_weather in tool lists and the model's
#: prompt sizes/token estimates differ; every motion, observation, order and audit event
#: is identical. The prior fixture drift is tracked as murmur-yi9n.
PAUSED = {
    "model harbor-watch s1": "51b890f1d75e620b35832a803bb43f338ca3059f401f12b0768d15b08ea3e906",
    "playbook v2 fast-unknown 1000": "58352e1f543edb430c2d57f22bc8925ad206030cf14e14f2c148d7aceddf7db1",
    "model v2 fast-unknown 1000": "bc24ac516eadfa324a15dda2db326274067eff76c23f96a79005a30b53d4bc15",
}


def test_paused_records_are_byte_identical_to_before():
    with tempfile.TemporaryDirectory(prefix="paused-") as td:
        opts = Options(agent="model", model_client=FakeAnthropic(LookScript()), calibration=Calibration.load())
        hw = Episode(from_situation("harbor-watch"), 1, Path(td), opts).run()
    got = {
        "model harbor-watch s1": hw,
        "playbook v2 fast-unknown 1000": run("fast-unknown", agent="playbook", clock="paused"),
        "model v2 fast-unknown 1000": run("fast-unknown", clock="paused"),
    }
    for rec in got.values():
        assert "condition" not in rec["clock"] and "clock" not in rec["provenance"]
        assert "clock" not in (rec.get("v2") or {}).get("condition", {})
    digests = {k: hashlib.sha256(normalized(r).encode()).hexdigest() for k, r in got.items()}
    assert digests == PAUSED
