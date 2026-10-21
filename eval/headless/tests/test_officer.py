"""The model agent (eval/headless/officer.py) on the real `murmur-headless` binary, with a fake
Anthropic client (fake_anthropic.py): no network, no key, nothing billed.

- a whole Harbor watch episode with `agent="model"`: the record, the wakes, the routine
  conversation carried and the consult fresh, the models and efforts per lane, the look mapped
  to `isr.investigate` and judged by metrics.py as a playbook look is;
- the simulated clock does not move across a wake (and a slow wake's wall time is recorded);
- a model that does nothing; a wake the cost guard refuses (`cost_guard`, `budget_stopped`,
  nothing sent), a spend past the cap mid-wake, the batch ledger shared, the aggregate keeping
  budget-stopped episodes out of the metrics;
- the mapping of intents to play categories; the configuration and the permission it needs.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless.batch import aggregate, paired
from eval.headless.episode import Episode, Options
from eval.headless.metrics import episode_metrics
from eval.headless.officer import (API_MODELS, ModelArmError, ModelConfig, WatchGuard, make_client, map_proposal)
from eval.headless.situations import Calibration, from_situation
from eval.model_agent import CostGuardExceeded, Ledger

from .fake_anthropic import FakeAnthropic, LookScript, nothing, text, tool_use


@pytest.fixture(scope="module", autouse=True)
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


def episode(client: Any, seed: int = 1, model: dict[str, Any] | None = None, agent: str = "model") -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="officer-") as td:
        opts = Options(agent=agent, model_client=client, model=model, calibration=Calibration.load())
        return Episode(from_situation("harbor-watch"), seed, Path(td), opts).run()


@pytest.fixture(scope="module")
def look() -> tuple[dict[str, Any], FakeAnthropic]:
    client = FakeAnthropic(LookScript())
    return episode(client), client


def test_a_model_episode_runs_end_to_end(look):
    rec, client = look
    json.dumps(rec)  # the record is plain JSON
    m = rec["model"]
    assert rec["playbook"] is None and rec["budget_stopped"] is False
    assert m["agent"] == "agent-watch"
    t = m["totals"]
    assert t["wakes"] >= 3 and t["proposals"] == 1 and t["api_calls"] == len(client.requests)
    assert t["budget_stopped"] is False and set(t["stops"]) == {"success"}
    assert t["usd"] > 0 and t["tokens"]["cache_read"] > 0
    # the first wake is the opening update, at the AI's start plus the drawn first-tick offset
    w0 = m["wakes"][0]
    assert w0["reason"] == "start" and "Your watch starts now" in w0["update"]
    assert w0["t"] == pytest.approx(rec["clock"]["ai_start_s"] + rec["clock"]["ai_first_tick_s"], abs=1e-6)
    for w in m["wakes"]:
        assert w["lane"] in ("routine", "consult")
        assert w["model"] == (API_MODELS["opus"] if w["lane"] == "consult" else API_MODELS["sonnet"])
        assert w["effort"] == ("high" if w["lane"] == "consult" else "medium")
        assert "Budget: this is wake" in w["update"]
        for c in w["api_calls"]:
            assert c["request_chars"] > 30_000 and c["model"] == w["model"]
    # every request: cached at its last block, the effort of its lane, the bridge's tools
    for r in client.requests:
        assert r["cache_control"] == {"type": "ephemeral"} and r["effort"] in ("medium", "high")
        assert r["system_breakpoint"] == [{"type": "ephemeral"}]
        assert r["chars"]["tools"] > 20_000 and r["chars"]["system"] > 3_000
    # the routine conversation carries from wake to wake; a consult starts fresh
    routine = [r for r in client.requests if r["model"] == API_MODELS["sonnet"]]
    assert [r["n_messages"] for r in routine] == sorted(r["n_messages"] for r in routine)
    assert routine[-1]["n_messages"] > 10
    for r in client.requests:
        if r["model"] == API_MODELS["opus"]:
            assert r["n_messages"] in (1, 3)
    # provenance names the models and efforts
    pm = rec["provenance"]["model"]
    assert pm["routine"] == {"model": "claude-sonnet-5-5", "effort": "medium"}
    assert pm["consult"] == {"model": "claude-opus-5-5", "effort": "high"}
    assert client.closed


def test_the_look_is_mapped_to_investigate_and_judged_like_a_playbook_look(look):
    rec, _ = look
    props = [ln for ln in rec["agent_log"] if ln.get("kind") == "proposal"]
    assert len(props) == 1
    p = props[0]
    assert p["source"] == "model" and p["play"] == "isr.investigate" and p["task"] == "orbit"
    assert p["target"].startswith("T-") and p["aim"] == p["target"] and p["aim_m"] < 1.0
    assert p["picture"]["affiliation"] in ("unknown", "suspect") and p["predicted"]
    m = episode_metrics(rec)
    (mp,) = m["proposals"]
    assert mp["play"] == "isr.investigate" and mp["truth_target"] == "fastboat-west"
    assert mp["tracker_target"] == "fastboat-west" and mp["tier"] == "human"
    assert m["must_look"] == ["fastboat-west"]
    assert m["look_covered"] is True and m["time_to_look"]["fastboat-west"] < 60
    assert m["false_looks"] == 0
    assert m["precision"] == {"legit": 1, "n": 1, "unjudged": []}
    assert m["human_cards"] == 1 and m["by_play"] == {"isr.investigate": 1}
    assert m["unfaithful"] == 0 and m["objected"] == 0  # the playbook's record checks: not the model's prose


def test_the_clock_does_not_move_across_a_wake():
    """Every wake reads the simulator's clock before and after (the driver fails the episode
    if it moved, and the server refuses any request that moves it); a wake that takes wall
    time takes none of the simulation's, and its wall time is recorded."""
    slow = LookScript()

    def script(req: dict[str, Any]):
        time.sleep(0.05)
        return slow(req)

    rec = episode(FakeAnthropic(script))
    wakes = rec["model"]["wakes"]
    assert wakes
    for w in wakes:
        before, after = w["clock"]
        assert before == after == pytest.approx(w["t"], abs=1e-6)
        assert w["wall_s"] >= 0.05 * len(w["api_calls"])
    assert rec["perf"]["model_wall_s"] >= 0.05 * rec["model"]["totals"]["api_calls"]


def test_a_model_that_does_nothing():
    client = FakeAnthropic(nothing)
    rec = episode(client)
    t = rec["model"]["totals"]
    assert t["wakes"] >= 3 and t["proposals"] == 0 and t["tool_calls"] == 0 and not t["budget_stopped"]
    assert all(w["stop"] == "success" and w["text"] == "No action." for w in rec["model"]["wakes"])
    m = episode_metrics(rec)
    assert m["look_covered"] is False and m["human_cards"] == 0 and m["n_proposals"] == 0
    assert m["precision"] == {"legit": 0, "n": 0, "unjudged": []}


def test_a_none_episode_has_no_model_keys():
    rec = episode(None, agent="none")
    assert "model" not in rec and "budget_stopped" not in rec and "model" not in rec["provenance"]
    assert rec["agent_log"] == [] and rec["playbook"] is None


def test_a_wake_the_guard_refuses_is_cost_guard_and_the_episode_budget_stopped():
    client = FakeAnthropic(LookScript())
    rec = episode(client, model={"max_usd": 0.0001})
    assert client.requests == []  # nothing was sent
    (w,) = rec["model"]["wakes"]  # the officer stops: no later wake
    assert w["stop"] == "cost_guard" and "cost guard" in w["error"] and w["api_calls"] == []
    assert w["text"] == "" and w["tool_calls"] == []
    t = rec["model"]["totals"]
    assert t["budget_stopped"] is True and t["stopped"] == "cost_guard" and t["usd"] == 0
    assert rec["budget_stopped"] is True
    assert any(ln["kind"] == "budget.exhausted" and ln["what"] == "cost guard" for ln in rec["agent_log"])
    assert rec["model"]["guard"]["tripped"] is True and rec["model"]["guard"]["refused"] == 1


def test_a_spend_past_the_cap_stops_the_next_call():
    """The first call passes its worst-case check, then bills past the episode's cap: the next
    call of the same wake is refused."""
    client = FakeAnthropic(LookScript(), usage=lambda req, n: {"input_tokens": 1_000_000, "output_tokens": 100})
    rec = episode(client, model={"max_usd": 1.0})
    assert len(client.requests) == 1
    (w,) = rec["model"]["wakes"]
    assert w["stop"] == "cost_guard" and len(w["api_calls"]) == 1
    assert [c["name"] for c in w["tool_calls"]] == ["get_fleet", "get_tracks"]  # the reads ran, then the guard
    assert rec["model"]["totals"]["usd"] == pytest.approx(2.001)
    assert rec["budget_stopped"] is True


class Overloaded(Exception):
    status_code = 529


class BadRequest(Exception):
    status_code = 400


def test_a_transient_api_error_backs_the_lane_off_and_the_update_is_told_again():
    calls = {"n": 0}
    look = LookScript()

    def script(req: dict[str, Any]):
        calls["n"] += 1
        if calls["n"] == 1:
            raise Overloaded("overloaded")
        return look(req)

    rec = episode(FakeAnthropic(script))
    w0, *rest = rec["model"]["wakes"]
    assert w0["stop"] == "error" and "Overloaded" in w0["error"] and "fatal" not in w0
    # Officer.failed: the routine lane waits 30 s (an escalating wake meanwhile goes to the
    # consult, as live); the opening update is told again, as the opening
    assert all(w["lane"] == "consult" for w in rest if w["t"] < w0["t"] + 30)
    w1 = next(w for w in rest if w["lane"] == "routine")
    assert w1["reason"] == "start" and "Your watch starts now" in w1["update"] and w1["t"] >= w0["t"] + 30
    assert w1["stop"] == "success"
    t = rec["model"]["totals"]
    assert t["errors"] == 1 and not t["model_failed"] and not t["budget_stopped"] and t["proposals"] == 1
    assert rec["model_failed"] is False
    assert any(ln["kind"] == "backoff" and ln["wait_s"] == 30.0 for ln in rec["agent_log"])


def test_a_request_the_api_refuses_as_such_stops_the_officer():
    def script(req: dict[str, Any]):
        raise BadRequest("invalid_request_error: bad parameter")

    rec = episode(FakeAnthropic(script))
    (w,) = rec["model"]["wakes"]
    assert w["stop"] == "error" and w["fatal"] is True
    assert rec["model_failed"] is True and rec["budget_stopped"] is False
    assert rec["model"]["totals"]["stopped"] == "model_failed"
    g = aggregate([{**_result("hw-model-s1", 1, "model", False), "model_failed": True}])
    assert g["harbor-watch [model]"]["episodes"] == 0 and g["harbor-watch [model]"]["model_failed"]["names"] == ["hw-model-s1"]


def test_the_batch_ledger_caps_every_worker(tmp_path: Path):
    path = tmp_path / "ledger.json"
    a = WatchGuard("claude-sonnet-5-5", 5.0, Ledger(path, 0.5))
    b = WatchGuard("claude-sonnet-5-5", 5.0, Ledger(path, 0.5))
    a.check(0.3)
    with pytest.raises(CostGuardExceeded):  # a's reservation is still open
        b.check(0.3)
    # a settles at what it billed: 10,000 fresh, 100,000 cache read, 1,000 output tokens
    usd = a.add({"input_tokens": 10_000, "cache_read_input_tokens": 100_000, "output_tokens": 1_000})
    assert usd == pytest.approx((10_000 * 2.0 + 100_000 * 0.20 + 1_000 * 10.0) / 1e6)
    state = json.loads(path.read_text())
    assert state["usd"] == pytest.approx(usd) and state["reserved"] == 0
    c = WatchGuard("claude-opus-5-5", 5.0, Ledger(path, 0.5))
    c.check(0.3)  # the batch can afford it now
    assert c.estimate_for("claude-opus-5-5", 20_000, 1_000) == pytest.approx((10_000 * 4.0 + 1_000 * 20.0) / 1e6)


def _result(name: str, seed: int, agent: str, covered: bool, stopped: bool = False) -> dict[str, Any]:
    m = {"look_covered": covered, "time_to_look": {"boat": -10.0}, "false_looks": 0,
         "precision": {"legit": 1, "n": 1, "unjudged": ["unjudged:patrol"]}, "human_cards": 1,
         "human_cards_per_operator_min": 0.1, "unfaithful": 0, "objected": 0, "reproposals_after_rejection": 0,
         "faults": {}, "by_play": {"isr.investigate": 1}}
    r = {"name": name, "seed": seed, "situation": {"name": "harbor-watch"}, "metrics": m,
         "perf": {"start_s": 0.1, "sim_wait_s": 1.0, "python_s": 0.5, "calls": 10, "ticks": 0, "rust": {}}, "wall_s": 2.0}
    if agent != "playbook":
        r["agent"] = agent
    if agent == "model":
        r["model"] = {"usd": 0.4, "wakes": 10, "consults": 1, "api_calls": 20, "tool_calls": 15,
                      "tokens": {"input": 100, "cache_write": 1000, "cache_read": 9000, "output": 500},
                      "stops": {"cost_guard": 1} if stopped else {"success": 10}, "budget_stopped": stopped, "wall_s": 30.0}
        r["budget_stopped"] = stopped
    return r


def test_the_aggregate_keeps_budget_stopped_episodes_apart():
    rs = [_result("hw-s1", 1, "playbook", True), _result("hw-model-s1", 1, "model", True),
          _result("hw-model-s2", 2, "model", False, stopped=True), _result("hw-none-s1", 1, "none", False)]
    g = aggregate(rs)
    assert set(g) == {"harbor-watch", "harbor-watch [model]", "harbor-watch [none]"}
    gm = g["harbor-watch [model]"]
    assert gm["episodes"] == 1 and gm["look_covered"]["k"] == 1 and gm["look_covered"]["n"] == 1
    assert gm["budget_stopped"]["names"] == ["hw-model-s2"]
    assert gm["model_cost"]["episodes"] == 2 and gm["model_cost"]["usd_total"] == pytest.approx(0.8)
    assert gm["model_cost"]["budget_stopped"] == 1
    assert "budget_stopped" not in g["harbor-watch"] and "model_cost" not in g["harbor-watch"]
    assert g["harbor-watch"]["unjudged"] == ["unjudged:patrol"]
    p = paired(rs)["harbor-watch"]
    assert set(p["1"]) == {"playbook", "model", "none"} and p["2"] == {"model": {"budget_stopped": True, "usd": 0.4, "wakes": 10}}


# ---- the mapping ------------------------------------------------------------------------------

BOAT = {"id": "T-0008", "affiliation": "suspect", "affiliation_confidence": 0.8512, "declared": None,
        "identity": {"kind": "small_fast_boat", "confidence": 0.85}, "predicted": [-122.5198, 37.80595]}
FERRY = {"id": "T-0001", "affiliation": "neutral", "affiliation_confidence": 0.99, "declared": None,
         "identity": {"kind": "ferry", "confidence": 0.99}, "position": [-122.43, 37.8184]}


def fleet(task: dict[str, Any] | None = None, battery: float = 90.0, mode: str = "AUTO") -> list[dict[str, Any]]:
    return [{"uid": 3, "callsign": "PUMA 1", "battery_pct": battery, "mode": mode, "task": task}]


def test_the_mapping_of_intents_to_plays():
    pic = {"tracks": [BOAT, FERRY], "vehicles": fleet(), "alerts": []}
    near = [-122.5198 + 0.003, 37.80595]  # about 260 m east of the boat
    m = map_proposal({"task": "orbit", "uids": [3], "center": near, "radius_m": 300}, pic, {})
    assert m["play"] == "isr.investigate" and m["target"] == "T-0008" and m["aim"] == "T-0008"
    assert m["picture"] == {"affiliation": "suspect", "affiliation_confidence": 0.85, "declared": None,
                            "identity": "small_fast_boat", "identity_confidence": 0.85}
    # the same vehicle already flies the model's own look on that track: a recentre
    looks = {7: {"track": "T-0008", "uid": 3}}
    flying = {"tracks": [BOAT, FERRY], "vehicles": fleet({"intent_id": 7, "state": "done", "task": "orbit"}), "alerts": []}
    assert map_proposal({"task": "orbit", "uids": [3], "center": near}, flying, looks)["play"] == "isr.recentre"
    # ... unless its order failed or it is going home on its own
    failed = {**flying, "vehicles": fleet({"intent_id": 7, "state": "failed", "task": "orbit"})}
    assert map_proposal({"task": "orbit", "uids": [3], "center": near}, failed, looks)["play"] == "isr.investigate"
    home = {**flying, "vehicles": fleet({"intent_id": 7, "state": "done", "task": "orbit"}, mode="RTL")}
    assert map_proposal({"task": "orbit", "uids": [3], "center": near}, home, looks)["play"] == "isr.investigate"
    # an orbit near no track; a patrol; two tracks about equally near: ambiguous
    far = map_proposal({"task": "orbit", "uids": [3], "center": [-122.60, 37.70], "radius_m": 300}, pic, {})
    assert far["play"] == "unjudged:orbit" and far["target"] is None
    assert map_proposal({"task": "patrol", "uids": [3], "route": [[0, 0], [1, 1]]}, pic, {})["play"] == "unjudged:patrol"
    # an orbit placed ahead of a moving track, on its course: a look on it (where it will be)
    fast = {**BOAT, "speed_mps": 15.0, "course_deg": 90.0}
    ahead = [-122.5198 + 0.017, 37.80595]  # about 1.5 km east: 100 s at 15 m/s
    lead = map_proposal({"task": "orbit", "uids": [3], "center": ahead, "radius_m": 400}, {**pic, "tracks": [fast, FERRY]}, {})
    assert lead["play"] == "isr.investigate" and lead["target"] == "T-0008" and 90 <= lead["lead_s"] <= 110
    behind = [-122.5198 - 0.017, 37.80595]  # the same distance behind it: not a look
    assert map_proposal({"task": "orbit", "uids": [3], "center": behind, "radius_m": 400},
                        {**pic, "tracks": [fast, FERRY]}, {})["play"] == "unjudged:orbit"
    twin = {**BOAT, "id": "T-0009", "predicted": [-122.5198 + 0.006, 37.80595]}
    amb = map_proposal({"task": "orbit", "uids": [3], "center": near}, {**pic, "tracks": [BOAT, twin]}, {})
    assert amb["play"] == "isr.investigate" and amb["aim"] == "ambiguous"
    # hold and rtl: a stuck alert, a low battery, a release of the model's own look, else unjudged
    stuck = {**pic, "alerts": [{"id": 12, "uid": 3, "kind": "not_moving"}]}
    assert map_proposal({"task": "hold", "uids": [3]}, stuck, {}) == {
        "play": "safety.stuck", "target": "alert:12", "rule": "a hold of a vehicle with a not_moving alert"}
    low = {**pic, "vehicles": fleet(battery=21.0)}
    assert map_proposal({"task": "rtl", "uids": [3]}, low, {})["target"] == "battery:3"
    assert map_proposal({"task": "rtl", "uids": [3]}, {**stuck, "vehicles": fleet(battery=21.0)}, {})["play"] == "safety.low_reserve"
    assert map_proposal({"task": "hold", "uids": [3]}, flying, looks)["play"] == "isr.release"
    assert map_proposal({"task": "hold", "uids": [3]}, pic, {})["play"] == "unjudged:hold"
    assert map_proposal({"task": "engage", "track_id": "T-0008", "uids": [3]}, pic, {})["play"] == "unjudged:engage"


def test_a_model_proposal_is_judged_by_the_playbooks_rule():
    """labels.py matches a model proposal's target to truth from the track's position as the
    picture showed it: the nearest contact within 500 m, ambiguous when another is within 1.5x."""
    from eval.headless.labels import AMBIGUOUS, Truth, model_proposal

    rec = {"truth": {"contacts": [{"id": "boat", "affiliation": "unknown"}, {"id": "ferry", "affiliation": "neutral"}],
                     "samples": [{"t": 100.0, "contacts": [["boat", -122.5198, 37.80595, 0, True],
                                                           ["ferry", -122.43, 37.8184, 0, True]]}]}}
    truth = Truth(rec)
    ln = {"t": 100.2, "source": "model", "kind": "proposal", "play": "isr.investigate", "target": "T-0008",
          "aim": "T-0008", "predicted": [-122.5199, 37.8060], "request": {"task": "orbit"}, "tier": "human"}
    assert model_proposal(ln, truth, {})["truth_target"] == "boat"
    assert model_proposal({**ln, "aim": AMBIGUOUS}, truth, {})["truth_target"] == AMBIGUOUS
    assert model_proposal({**ln, "predicted": [-122.60, 37.70]}, truth, {})["truth_target"] == AMBIGUOUS
    assert model_proposal({**ln, "play": "safety.low_reserve", "target": "battery:3", "aim": None}, truth, {})["truth_target"] == "battery:3"


# ---- configuration and permission -------------------------------------------------------------


def test_the_models_are_the_watch_officers_and_can_be_overridden(monkeypatch):
    for k in ("MURMUR_EVAL_WATCH_MODEL", "MURMUR_EVAL_WATCH_EFFORT", "MURMUR_EVAL_WATCH_STRONG_MODEL",
              "MURMUR_EVAL_WATCH_STRONG_EFFORT", "MURMUR_EVAL_MAX_USD", "MURMUR_EVAL_MAX_TOKENS",
              "MURMUR_EVAL_WATCH_FALLBACKS", "MURMUR_EVAL_LEDGER"):
        monkeypatch.delenv(k, raising=False)
    c = ModelConfig.resolve()
    assert (c.model, c.effort, c.strong_model, c.strong_effort) == ("claude-sonnet-5-5", "medium", "claude-opus-5-5", "high")
    assert c.max_usd == 3.0 and c.fallbacks is False and c.ledger is None and c.escalation
    monkeypatch.setenv("MURMUR_EVAL_WATCH_EFFORT", "low")
    monkeypatch.setenv("MURMUR_EVAL_MAX_USD", "1.5")
    c = ModelConfig.resolve({"strong_model": "claude-sonnet-5-5"})
    assert c.effort == "low" and c.max_usd == 1.5 and c.strong_model == "claude-sonnet-5-5"
    assert ModelConfig.resolve({"effort": "high"}).effort == "high"  # the options beat the environment
    with pytest.raises(ModelArmError, match="no list price"):
        ModelConfig.resolve({"model": "claude-unknown-9"})


def test_the_model_agent_needs_permission_to_call_the_api(monkeypatch):
    monkeypatch.delenv("MURMUR_EVAL_MODEL", raising=False)
    with pytest.raises(ModelArmError, match="MURMUR_EVAL_MODEL=1"):
        make_client()
    with pytest.raises(ModelArmError, match="MURMUR_EVAL_MODEL=1"):
        episode(None)
    from eval.headless.batch import main

    with pytest.raises(SystemExit, match="MURMUR_EVAL_MODEL=1"):
        main(["--agent", "model", "--no-build", "--seeds", "1"])


def test_a_tool_outside_the_server_is_refused():
    """As the locked-down CLI answers it: no such tool, nothing runs."""
    calls = {"n": 0}

    def script(req: dict[str, Any]):
        calls["n"] += 1
        if calls["n"] == 1:
            return [tool_use("bash", {"command": "ls"}, "toolu_x")], "tool_use"
        return [text("No action.")], "end_turn"

    rec = episode(FakeAnthropic(script))
    w = rec["model"]["wakes"][0]
    assert w["tool_calls"][0]["name"] == "bash" and w["tool_calls"][0]["is_error"] is True
    assert "No such tool available" in w["tool_calls"][0]["result"]
