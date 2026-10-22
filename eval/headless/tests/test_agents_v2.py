"""Scoring v2's anchors and perturbations (eval/headless/agents_v2.py; SCORING-V2.md sections 4.5,
5), the ruler check's ladder of handicapped oracles and its judging (0.3, ladder.py), the
replicate wiring, admissibility in the primary (resolution 21), the world pilot's gate (10.1) and
the world's reserve calibration (0.1), on the real `murmur-headless` binary (built here if stale)
and on synthetic scored episodes.

Each anchor and perturbation is checked on a small episode for what its name says, against the
plain playbook (or `none`) on the same situation and replicate, and for repeating exactly on the
same seeds; `null` against `none` on a few dev-build seeds for 4.5's fidelity (the same truth,
picture samples, labels, and audit log bar `agent.action`)."""

from __future__ import annotations

import copy
import json
import math
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from eval.headless import REPO
from eval.headless.batch import fidelity_digest, normalized_audit, replicate_seed
from eval.headless.episode import AGENTS, ANCHORS, LADDER, PERTURBATIONS, Episode, Options
from eval.headless.families import (
    GATE,
    V2_FLEET,
    dist_m,
    fleet_with_reserve,
    generate,
    reserve_candidates,
    v2_reserve_spawn,
)
from eval.headless.sim import BINARY
from eval.headless.situations import Calibration, opaque_run_id
from eval.headless.v2 import labels as L

APPROVE_ALL = {"mode": "approve-all"}


@pytest.fixture(scope="module", autouse=True)
def binary() -> Path:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)
    return BINARY


def episode(family: str, seed: int, agent: str, r: int = 0, operator: dict[str, Any] | None = APPROVE_ALL,
            reserve: str | None = None) -> dict[str, Any]:
    """One v2 episode as the batch runs replicate r of (family, seed) (batch.py `_v2_episode`)."""
    from eval.headless.batch import _v2_episode

    job = {"family": f"v2-{family}", "situation_seed": seed, "r": r, "seed": replicate_seed(f"v2-{family}", seed, r),
           "agent": agent, "operator": operator, "calibration": Calibration.load().as_dict(), "reserve": reserve}
    with tempfile.TemporaryDirectory(prefix="agents-v2-") as td:
        return _v2_episode(job, Path(td))


_CACHE: dict[tuple, dict[str, Any]] = {}


def cached(*key: Any, **kw: Any) -> dict[str, Any]:
    k = (key, tuple(sorted(kw.items())))
    if k not in _CACHE:
        _CACHE[k] = episode(*key, **kw)
    return _CACHE[k]


def actions(rec: dict[str, Any]) -> list[dict[str, Any]]:
    return L.episode_labels(rec)["actions"]


def proposals(rec: dict[str, Any]) -> list[dict[str, Any]]:
    return [ln for ln in rec["agent_log"] if ln.get("kind") == "proposal"]


# ---- the registry ---------------------------------------------------------------------------------

def test_every_anchor_and_perturbation_is_an_agent():
    from eval.headless import agents_v2

    assert set(ANCHORS) == {"null", "random-valid", "oracle"} and set(PERTURBATIONS) == set(agents_v2.PERTURBATIONS)
    assert LADDER == agents_v2.LADDER == ("oracle-linkblind", "oracle-honest", "oracle-late")
    assert agents_v2.NAMES == ANCHORS + LADDER + PERTURBATIONS and set(agents_v2.NAMES) <= set(AGENTS)
    assert {"playbook", "model", "none"} <= set(AGENTS)


# ---- null (4.5) ---------------------------------------------------------------------------------------

@pytest.mark.parametrize("family,seed", [("fast-unknown", 1000), ("approval-flood", 1001), ("stuck-vehicle", 1002),
                                         ("link-cut-mid-look", 1003)])
def test_null_reproduces_none(family: str, seed: int):
    """4.5: `null` connects, reads and never acts, and its episode is `none`'s: the same truth,
    picture samples and labels, and the same audit log bar the `agent.action` events of its
    session (in order, without the counter and hash chain those take a place in). The scripted
    operator model (proxy) decides the flood's cards in both."""
    none = episode(family, seed, "none", operator=None)
    null = episode(family, seed, "null", operator=None)
    assert null["v2"]["agent"] == "null" and null["agent_detail"]["proposals"] == 0
    assert null["agent_detail"]["calls"]["get_tracks"] > 300 and "propose_intent" not in null["agent_detail"]["calls"]
    assert null["truth"]["samples"] == none["truth"]["samples"] and null["truth"]["ours"] == none["truth"]["ours"]
    assert null["truth"]["association"] == none["truth"]["association"]
    assert json.dumps(null["v2"]["picture"], sort_keys=True) == json.dumps(none["v2"]["picture"], sort_keys=True)
    assert normalized_audit(null["audit_text"]) == normalized_audit(none["audit_text"])
    assert any(json.loads(x).get("type") == "agent.action" for x in null["audit_text"].splitlines())
    assert fidelity_digest(null) == fidelity_digest(none)
    a, b = L.episode_labels(null), L.episode_labels(none)
    assert a["actions"] == [] and a["needs"] == b["needs"] and a["fault_needs"] == b["fault_needs"]
    assert a["scripted_cards"] == b["scripted_cards"]


def test_the_fidelity_digest_sees_a_difference_but_not_the_build_stamp():
    rec = cached("fast-unknown", 1000, "none")
    other = copy.deepcopy(rec)
    other["audit_text"] = "\n".join(rec["audit_text"].splitlines()[:-1])
    assert fidelity_digest(other)["audit"] != fidelity_digest(rec)["audit"]
    other = copy.deepcopy(rec)
    other["truth"]["samples"] = other["truth"]["samples"][:-1]
    assert fidelity_digest(other)["truth"] != fidelity_digest(rec)["truth"]
    # a rebuild between two episodes restamps run.started's build (the commit, dirty): not a
    # difference in the episode (the 2026-10-21 world pilot met it: a test rebuilt the binary mid-batch)
    lines = rec["audit_text"].splitlines()
    first = json.loads(lines[0])
    assert first["type"] == "run.started" and "build" in first
    first["build"] = {"commit": "0" * 40, "dirty": True}
    other = copy.deepcopy(rec)
    other["audit_text"] = "\n".join([json.dumps(first)] + lines[1:])
    assert fidelity_digest(other) == fidelity_digest(rec)


# ---- random-valid ------------------------------------------------------------------------------

def test_random_valid_acts_every_30_s_on_idle_vehicles_and_repeats():
    """Every 30 s from its first tick, one orbit drawn among the ones whose preview passed, by an
    idle vehicle (on the v2 base PUMA 1 alone at first: the others fly opening orders); the same
    seeds, the same draws and episode."""
    rec = cached("fast-unknown", 1000, "random-valid")
    again = episode("fast-unknown", 1000, "random-valid")
    d = rec["agent_detail"]
    assert d["draws"] == again["agent_detail"]["draws"] and d["draws"]
    assert normalized_audit(rec["audit_text"]) == normalized_audit(again["audit_text"])
    # its turns: from the first tick, 30 s apart
    turns = sorted({round(ln["t"], 3) for ln in rec["agent_log"]
                    if ln["kind"] in ("proposal", "none.valid", "error", "read.error")})
    first = rec["clock"]["ai_start_s"] + rec["clock"]["ai_first_tick_s"]
    assert turns[0] == pytest.approx(first, abs=0.01)
    assert all(math.isclose((t - first) % 30.0, 0.0, abs_tol=0.01) or math.isclose((t - first) % 30.0, 30.0, abs_tol=0.01)
               for t in turns)
    for dr in d["draws"]:
        assert 0 <= dr["drawn"] < dr["of"] <= dr["actions"]
    acts = actions(rec)
    assert acts and all(a["kind"] == "orbit" for a in acts)
    # the first draw: PUMA 1 (uid 3), the only idle vehicle at the start
    assert acts[0]["uids"] == ["3"]
    # a vehicle it tasked is not idle while its orbit runs: no second order to it while it does
    by_uid: dict[str, list[dict[str, Any]]] = {}
    for a in acts:
        by_uid.setdefault(a["uids"][0], []).append(a)
    for uid, xs in by_uid.items():
        for a, b in zip(xs, xs[1:]):
            run = (a["runs"] or {}).get(uid) or {}
            assert run.get("end", 0) <= (b["created"] or 0) + 1e-6, (uid, a["intent"], b["intent"])


def test_random_valid_draws_differ_with_the_replicate():
    a = cached("fast-unknown", 1000, "random-valid")["agent_detail"]["seed"]
    b = episode("fast-unknown", 1000, "random-valid", r=1)["agent_detail"]["seed"]
    assert a != b and a.startswith("random-valid:")


# ---- oracle -------------------------------------------------------------------------------------

def test_the_oracle_serves_what_none_cannot_and_says_it_read_truth():
    """With PUMA 1 waiting far from the approaches (Fort Mason), `none` misses the boat; the oracle
    sends a Puma from truth (one card), in time, and records its truth reads as privileged."""
    none = cached("fast-unknown", 1201, "none", reserve="fort-mason")
    orc = cached("fast-unknown", 1201, "oracle", reserve="fort-mason")
    assert orc["truth"]["stated"]["reserve"]["name"] == "fort-mason"
    nn = [n for n in L.episode_labels(none)["needs"] if n["eligible"]]
    no = [n for n in L.episode_labels(orc)["needs"] if n["eligible"]]
    assert nn and not any(n["served"] for n in nn)
    assert all(n["served"] for n in no)
    d = orc["agent_detail"]
    assert d["privileged"]["truth_reads"] > 300 and d["privileged"]["contacts_file"]
    acts = actions(orc)
    assert acts and all(a["kind"] == "orbit" and a["uids"][0] in ("3", "4") for a in acts)
    assert len([a for a in acts if a["card"]]) <= 2
    assert d["needs"]["fast-1"]["resolved_t"] is not None


def test_the_oracle_repeats():
    a = cached("fast-unknown", 1201, "oracle", reserve="fort-mason")
    b = episode("fast-unknown", 1201, "oracle", reserve="fort-mason")
    assert proposals(a) == proposals(b) and normalized_audit(a["audit_text"]) == normalized_audit(b["audit_text"])


def test_the_oracle_sends_a_low_puma_home_before_the_reserve():
    rec = cached("low-battery", 1200, "oracle")
    fn = L.episode_labels(rec)["fault_needs"]
    uid = rec["truth"]["stated"]["low_battery"]["uid"]
    assert [f["status"] for f in fn if str(f["uid"]) == str(uid)] == ["met"]
    rtl = [a for a in actions(rec) if a["kind"] == "rtl"]
    assert len(rtl) == 1 and rtl[0]["uids"] == [str(uid)]


def test_the_oracle_skips_a_need_no_look_can_resolve():
    """S8's misclassified boat: its EO/IR draw is set (the stated sensing draws, truth), so the
    oracle sends nothing for it."""
    rec = cached("conflict", 1000, "oracle")
    assert any(ln["kind"] == "need.unresolvable" for ln in rec["agent_log"])
    assert not proposals(rec)


# ---- the ladder (SCORING-V2.md 0.3) ---------------------------------------------------------------

def test_the_link_blind_oracle_plans_without_the_link_map(monkeypatch: pytest.MonkeyPatch):
    """oracle-linkblind: the oracle without where the link holds. It never consults the link map
    (the emulator's terrain check), reads no truth of a blocked link (every order it saw end says
    none), and reads the rest of truth as the oracle does (privileged). On the same situation its
    first look goes where the oracle's, kept to the link, does not."""
    from eval.headless import agents_v2

    def no_map(alt_m: float) -> Any:
        raise AssertionError("the link-blind oracle consulted the link map")

    monkeypatch.setattr(agents_v2, "link_map", no_map)
    rec = episode("fast-unknown", 1000, "oracle-linkblind")
    d = rec["agent_detail"]
    assert d["agent"] == "oracle-linkblind" and d["errors"] == 0 and d["proposals"] >= 1
    assert not any(ln["kind"].startswith("error") for ln in rec["agent_log"])
    assert d["privileged"]["truth_reads"] > 300 and "never where the link holds" in d["privileged"]["note"]
    ended = [ln for ln in rec["agent_log"] if ln["kind"] == "order.ended"]
    assert ended and all(ln["link_blocked"] is False for ln in ended)
    monkeypatch.undo()
    orc = cached("fast-unknown", 1000, "oracle")
    assert proposals(orc)[0]["request"]["center"] != proposals(rec)[0]["request"]["center"]


#: a situation where the late oracle looks at all (most of the time, by its need's opening no
#: Puma can reach a contact in its 60 s, so it sends nothing): the second boat, from the bay
LATE_CASE = ("two-at-once", 1000)


def test_the_late_oracle_looks_at_no_contact_before_its_need_opens():
    """oracle-late: the oracle's plan, but no look at a contact before its true position is read
    inside 3 km (it reads each second, so within a second of the need's opening by the labels).
    The oracle on the same situation looks before then."""
    rec = cached(*LATE_CASE, "oracle-late")
    orc = cached(*LATE_CASE, "oracle")
    needs = {n["contact"]: n for n in L.episode_labels(rec)["needs"]}
    looks_ = [p for p in proposals(rec) if p["request"]["task"] == "orbit"]
    assert looks_, "the late oracle looks once the need opens"
    for p in looks_:
        assert p["t"] >= needs[p["need"]]["t_need"] - 1.0, p
    first = [p for p in proposals(orc) if p["request"]["task"] == "orbit"][0]
    assert first["t"] < needs[first["need"]]["t_need"] - 30.0
    assert rec["agent_detail"]["privileged"]["truth_reads"] > 300


def _inside(cls: type) -> bool:
    """Whether a frame of an instance of `cls` is on the stack."""
    f = sys._getframe(1)
    while f is not None:
        if isinstance(f.f_locals.get("self"), cls):
            return True
        f = f.f_back
    return False


def _held(obj: Any, depth: int = 0, seen: set[int] | None = None) -> list[Any]:
    """Everything `obj` holds, through attributes, containers and bound methods (a few levels),
    stopping at a session: what is inside one is the MCP tools' business (sim.py)."""
    from eval.headless.sim import HeadlessSession

    seen = set() if seen is None else seen
    if id(obj) in seen or depth > 6:
        return []
    seen.add(id(obj))
    out = [obj]
    if isinstance(obj, HeadlessSession):
        return out
    if isinstance(obj, dict):
        kids = list(obj.keys()) + list(obj.values())
    elif isinstance(obj, (list, tuple, set, frozenset)):
        kids = list(obj)
    elif hasattr(obj, "__self__") and not isinstance(obj, type):
        kids = [obj.__self__]
    elif hasattr(obj, "__dict__") and not isinstance(obj, type):
        kids = list(vars(obj).values())
    else:
        kids = []
    for k in kids:
        out += _held(k, depth + 1, seen)
    return out


def test_the_honest_oracle_reads_no_truth(monkeypatch: pytest.MonkeyPatch):
    """oracle-honest is handed what the playbook and the model get, a session and the loop to
    answer it, never the episode: it holds no episode, simulator, contacts file or link map (its
    session holds the simulator behind the MCP tools, as theirs do), and while the episode runs
    no truth read, no contacts file read and no link map is made from inside it. It still serves
    a plain S1 need from the picture alone, and its record claims no privilege."""
    from eval.headless import agents_v2
    from eval.headless.episode import Episode as Ep
    from eval.headless.sim import HeadlessSession, Sim

    made: list[Any] = []
    real_make = agents_v2.make

    def make(ep_: Any, name: str, cfg: Any) -> Any:
        a = real_make(ep_, name, cfg)
        made.append(a)
        return a

    monkeypatch.setattr(agents_v2, "make", make)
    trapped: list[str] = []
    real_truth = Sim.truth

    def truth(self: Any, *a: Any, **k: Any) -> Any:
        if _inside(agents_v2.OracleHonest):
            trapped.append("Sim.truth")
        return real_truth(self, *a, **k)

    monkeypatch.setattr(Sim, "truth", truth)
    for fn in ("read_contacts", "need_routes", "link_map", "fleet_plan"):
        def trap(*a: Any, _fn: str = fn, _orig: Any = getattr(agents_v2, fn), **k: Any) -> Any:
            if _inside(agents_v2.OracleHonest):
                trapped.append(_fn)
            return _orig(*a, **k)

        monkeypatch.setattr(agents_v2, fn, trap)
    rec = episode("fast-unknown", 1000, "oracle-honest")
    assert trapped == []
    (agent,) = made
    assert isinstance(agent, agents_v2.OracleHonest) and not hasattr(agent, "ep")
    held = _held(agent)
    assert not [x for x in held if isinstance(x, (Ep, Sim, agents_v2.LinkMap, agents_v2.Route))]
    assert not [x for x in held if isinstance(x, (str, Path)) and "contacts" in str(x)]
    assert [x for x in held if isinstance(x, HeadlessSession)] == [agent.session]
    d = rec["agent_detail"]
    assert "privileged" not in d and d["truth"].startswith("none") and d["errors"] == 0
    assert d["calls"]["get_tracks"] > 300 and d["proposals"] >= 1
    needs = [n for n in L.episode_labels(rec)["needs"] if n["eligible"]]
    assert needs and all(n["served"] for n in needs)
    # it found the need on the picture (a track id), not from the contacts file (a contact id)
    assert set(d["needs"]) and all(k.startswith("T-") for k in d["needs"])


def test_straight_carries_a_track_along_its_shown_velocity():
    from eval.headless.agents_v2 import Straight

    s = Straight("T-0001")
    s.update((-4000.0, 0.0), (10.0, 0.0), 100.0)
    assert s.at(np.array([100.0, 150.0])).tolist() == [[-4000.0, 0.0], [-3500.0, 0.0]]
    assert s.first_within(3000.0, 100.0, 400.0) == 200.0
    assert s.first_within(3000.0, 100.0, 150.0) is None
    s.update((-4000.0, 0.0), (0.0, 0.0), 100.0)  # no velocity shown: where it is
    assert s.first_within(3000.0, 100.0, 400.0) is None


@pytest.mark.parametrize("agent", LADDER)
def test_each_rung_repeats_on_its_seeds(agent: str):
    a = cached("two-at-once", 1001, agent)
    b = episode("two-at-once", 1001, agent)
    assert proposals(a) == proposals(b) and a["agent_log"] == b["agent_log"]
    assert normalized_audit(a["audit_text"]) == normalized_audit(b["audit_text"])
    assert fidelity_digest(a) == fidelity_digest(b)


def test_the_ruler_check_judges_each_prediction_by_its_paired_interval():
    """0.3's rule (ladder.py): a difference whose interval lies above zero holds, one containing
    zero is a tie, one wholly below zero fails; the check passes when nothing fails in a family
    with needs; a family without needs is not judged; the pooled row is reported, never judged."""
    from eval.headless import ladder

    assert ladder.verdict({"lo": 0.01, "hi": 0.2}) == "holds"
    assert ladder.verdict({"lo": -0.1, "hi": 0.1}) == "tie"
    assert ladder.verdict({"lo": -0.3, "hi": -0.01}) == "fails"
    assert ladder.verdict({"lo": None, "hi": None}) == "no data"
    scored = []
    for sit in range(1, 26):
        scored += [ep("none", "fast-unknown", sit, 0), ep("oracle", "fast-unknown", sit, 1),
                   ep("oracle-late", "fast-unknown", sit, int(sit % 5 == 0)),
                   ep("oracle-linkblind", "fast-unknown", sit, sit % 2), ep("oracle-honest", "fast-unknown", sit, sit % 2),
                   ep("none", "quiet", sit, 0, eligible=0), ep("oracle", "quiet", sit, 0, eligible=0)]
    res = ladder.check(scored)[COND]
    assert res["passed"] and res["families_with_needs"] == ["fast-unknown"]
    fam = res["families"]["fast-unknown"]
    v = {(p["a"], p["b"]): p["verdict"] for p in fam["predictions"]}
    assert v[("oracle", "oracle-late")] == "holds" and v[("oracle-linkblind", "oracle-honest")] == "tie"
    assert v[("oracle-late", "none")] == "holds" and v[("oracle-honest", "none")] == "holds"
    assert "predictions" not in res["families"]["quiet"]
    assert fam["arms"]["oracle-linkblind"]["share"] == pytest.approx(13 / 25)
    assert fam["steps"] == {"link": 48.0, "truth": 0.0, "timing": 80.0}
    assert res["families"]["*"]["predictions"], "the pooled row is reported"
    # the link-blind rung serving nothing, below the honest one wholly: a reversal, the check fails
    worse = [dict(e, metrics={**e["metrics"], "served": 0, "served_share": 0.0})
             if e["agent"] == "oracle-linkblind" else e for e in scored]
    full = ladder.check(worse)
    res = full[COND]
    assert not res["passed"]
    assert [(f["family"], f["a"], f["b"]) for f in res["failures"]] == [("fast-unknown", "oracle-linkblind", "oracle-honest")]
    v = {(p["a"], p["b"]): p["verdict"] for p in res["families"]["fast-unknown"]["predictions"]}
    assert v[("oracle-linkblind", "none")] == "tie", "a tie with none is reported, not a failure"
    assert "Passed: **no**" in ladder.markdown(full)


def test_the_validation_judges_anchor_orders_and_perturbation_moves():
    """0.4's reading of 10.2 (validate.py): an anchor order holds when no link is reversed and its
    ends' interval excludes zero, and fails on a reversal; a perturbation's move is judged against
    the plain playbook, and a predicted fall where the playbook is not shown above none is not
    testable unless it is shown anyway."""
    from eval.headless import validate as V

    def with_(e: dict[str, Any], **kw: Any) -> dict[str, Any]:
        e["metrics"].update(kw)
        return e

    scored = []
    for sit in range(1, 26):
        for fam, pb in (("fast-unknown", 1), ("decoy", 0)):
            scored += [with_(ep("none", fam, sit, 0), cards_per_10min=0.0),
                       with_(ep("random-valid", fam, sit, 0), cards_per_10min=0.8 + 0.01 * sit),
                       with_(ep("oracle", fam, sit, 1), cards_per_10min=1.4 + 0.01 * (sit % 3)),
                       with_(ep("playbook", fam, sit, pb), cards_per_10min=2.0),
                       with_(ep("playbook+skip-first", fam, sit, 0), cards_per_10min=2.0),
                       with_(ep("playbook+spam", fam, sit, pb), cards_per_10min=2.0)]
    res = V.check(scored)[COND]
    assert res["admissible"] == ["decoy", "fast-unknown"]
    fam = res["families"]["fast-unknown"]
    a = {x["metric"]: x for x in fam["anchors"]}
    assert a["served_share"]["verdict"] == "holds"
    assert [x["verdict"] for x in a["served_share"]["links"]] == ["tie", "holds"], "none <= random-valid: a tie passes"
    assert a["cards_per_10min"]["verdict"] == "fails", "the oracle above random-valid on cards is a reversal"
    assert a["fault_met_share"]["verdict"] == "not testable", "no fault need in these situations"
    mv = {(p, m["metric"]): m["verdict"] for p, x in fam["perturbations"].items() for m in x["moves"]}
    assert mv[("playbook+skip-first", "served_share")] == "moved"
    assert mv[("playbook+spam", "served_share")] == "unchanged"
    assert mv[("playbook+spam", "cards_per_10min")] == "did not move"
    skip = res["families"]["decoy"]["perturbations"]["playbook+skip-first"]["moves"][0]
    assert skip["verdict"] == "not testable" and "not shown above none" in skip["why"]
    assert {"family": "fast-unknown", "metric": "cards_per_10min", "verdict": "fails"} in res["anchor_failures"]
    assert "playbook+skip-first" in V.markdown({COND: res})


# ---- the perturbations --------------------------------------------------------------------------

def looks(rec: dict[str, Any]) -> list[dict[str, Any]]:
    return [a for a in actions(rec) if a["kind"] == "orbit"]


def test_spam_proposes_each_held_look_again_every_20_s():
    pb = cached("fast-unknown", 1200, "playbook")
    sp = cached("fast-unknown", 1200, "playbook+spam")
    assert len(looks(sp)) > len(looks(pb)) >= 1
    notes = [ln for ln in sp["agent_log"] if ln["kind"] == "perturbation" and ln["what"] == "re-proposed"]
    assert notes and all(n["request"]["task"] == "orbit" for n in notes)
    # every copy repeats a look on its target 20 s or more after the last proposal on it (the
    # playbook's own, or a copy)
    looked: dict[str, list[float]] = {}
    for ln in sp["agent_log"]:
        if ln["kind"] == "proposal" and ln.get("play") in ("isr.investigate", "isr.recentre"):
            looked.setdefault(ln["target"], []).append(ln["t"])
    for n in notes:
        before = [t for t in looked.get(n["target"], []) + [m["t"] for m in notes if m["target"] == n["target"]]
                  if t < n["t"] - 1e-6]
        assert before and n["t"] - max(before) >= 20.0 - 1e-6, n
    # the copies are the playbook's own cards: its ledger holds them under the look's play
    assert sp["playbook"]["engine"] == pb["playbook"]["engine"]
    assert sp["playbook"]["config_label"] == pb["playbook"]["config_label"]
    assert sp["agent_detail"]["changes"] == len(notes)


def test_behind_moves_each_look_1_5_eo_ir_ranges_behind_the_contact():
    pb = cached("fast-unknown", 1200, "playbook")
    bh = cached("fast-unknown", 1200, "playbook+behind")
    notes = [ln for ln in bh["agent_log"] if ln["kind"] == "perturbation"]
    assert notes and len(looks(pb)) >= 1
    for n in notes:
        d = dist_m(tuple(n["old"]), tuple(n["new"]))
        assert d == pytest.approx(1.5 * 4000.0, rel=0.01)
    centres = {tuple(a["params"]["center"]) for a in looks(bh)}
    assert centres and centres <= {tuple(n["new"]) for n in notes}


def test_skip_first_never_looks_at_the_earliest_need():
    pb = cached("fast-unknown", 1200, "playbook")
    sk = cached("fast-unknown", 1200, "playbook+skip-first")
    first = sk["agent_detail"]["privileged"]["earliest_need"]
    assert first == "fast-1"
    assert any(first in (a["aim"].get("contacts") or []) for a in looks(pb)), "the playbook looks at it"
    assert not any(first == a["aim"].get("primary") for a in actions(sk))
    hidden = {ln["track"] for ln in sk["agent_log"] if ln["kind"] == "perturbation"}
    assert hidden and all(sk["truth"]["association"].get(t) == first for t in hidden)


def test_reserve_blind_never_sends_an_rtl():
    pb = cached("low-battery", 1200, "playbook")
    rb = cached("low-battery", 1200, "playbook+reserve-blind")
    uid = str(rb["truth"]["stated"]["low_battery"]["uid"])
    assert any(a["kind"] == "rtl" for a in actions(pb))
    assert not any(a["kind"] == "rtl" for a in actions(rb))
    met = {str(f["uid"]): f["status"] for f in L.episode_labels(pb)["fault_needs"]}
    blind = {str(f["uid"]): f["status"] for f in L.episode_labels(rb)["fault_needs"]}
    assert met[uid] == "met" and blind[uid] != "met"
    assert any(ln["kind"] == "perturbation" and ln["what"] == "dropped an rtl" for ln in rb["agent_log"])


def test_ferries_adds_one_look_at_each_ais_neutral_and_lets_it_go():
    pb = cached("fast-unknown", 1200, "playbook")
    fe = cached("fast-unknown", 1200, "playbook+ferries")
    contacts = {c["id"]: c for c in fe["truth"]["contacts"]}
    extra = [ln for ln in fe["agent_log"] if ln["kind"] == "perturbation"]
    assert extra and len(looks(fe)) > len(looks(pb))
    targets = [ln["target"] for ln in extra]
    assert len(targets) == len(set(targets)), "one look per track"
    for ln in extra:
        c = fe["truth"]["association"].get(ln["target"])
        assert c and contacts[c]["affiliation"] in ("neutral",) and contacts[c]["ais"]
    holds = [a for a in actions(fe) if a["kind"] == "hold"]
    assert holds, "a look at a ferry is let go (a hold), so the Puma is free again"
    assert fe["playbook"]["config_label"] != pb["playbook"]["config_label"]


@pytest.mark.parametrize("agent,family", [("playbook+spam", "fast-unknown"), ("playbook+behind", "fast-unknown"),
                                          ("playbook+skip-first", "fast-unknown"),
                                          ("playbook+reserve-blind", "low-battery"), ("playbook+ferries", "fast-unknown"),
                                          ("null", "fast-unknown")])
def test_each_perturbation_repeats_on_its_seeds(agent: str, family: str):
    seed = 1200 if family != "fast-unknown" or agent != "null" else 1000
    a = cached(family, seed, agent)
    b = episode(family, seed, agent)
    strip = lambda r: [{k: v for k, v in ln.items() if k != "rationale"} for ln in r["agent_log"]  # noqa: E731
                       if ln.get("kind") in ("proposal", "perturbation", "withdrawal", "decision")]
    assert strip(a) == strip(b) and normalized_audit(a["audit_text"]) == normalized_audit(b["audit_text"])
    assert fidelity_digest(a) == fidelity_digest(b)


# ---- the replicate wiring ------------------------------------------------------------------------

def test_the_batch_sets_the_replicate_and_its_run_id(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    """Section 1, 4.4: every replicate's episode carries `spec.replicate = r`, so the opaque run id
    (the audit log's name, served to the agent at connect) is per (family, situation seed, r), the
    same for every agent; the replicate seed drives the episode."""
    from eval.headless import batch
    from eval.headless.episode import Episode as Ep
    from eval.headless.situations import run_id

    seen: list[dict[str, Any]] = []

    def fake_run(self: Ep) -> dict[str, Any]:
        seen.append({"replicate": self.e.replicate, "seed": self.seed, "run_id": run_id(self.e, self.seed),
                     "agent": self.opts.agent, "opts_replicate": self.opts.replicate, "spec_seed": self.e.seed})
        return {}

    monkeypatch.setattr(Ep, "run", fake_run)
    args = batch.argparse.Namespace(families=["v2-decoy"], situation=None, seeds=["1000-1001"], replicates=3,
                                    operators=["approve-all"], operator=None, policy=None, model_config=None,
                                    keep_audit=False, no_anchors=False, order_seed=0, reserve=None)
    jobs = batch.v2_jobs(args, ["none", "oracle"], Calibration.load().as_dict(), tmp_path)
    assert len(jobs) == 2 * 3 * 2
    for j in jobs:
        wd = tmp_path / f"{j['agent']}-{j['situation_seed']}-{j['r']}"
        wd.mkdir()
        batch._v2_episode(j, wd)
    for s, j in zip(seen, jobs):
        assert s["replicate"] == j["r"] and s["seed"] == j["seed"] == s["spec_seed"]
        assert s["seed"] == replicate_seed("v2-decoy", j["situation_seed"], j["r"])
        assert s["run_id"] == opaque_run_id("decoy", j["situation_seed"], j["r"])
        assert s["opts_replicate"] == {"r": j["r"], "seed": j["seed"], "situation_seed": j["situation_seed"]}
    ids = {(s["run_id"], s["agent"]) for s in seen}
    assert len({i for i, _ in ids}) == 2 * 3, "one run id per situation and replicate, shared by the agents"


# ---- admissibility in the primary (resolution 21) and the world pilot (10.1) ------------------------

def ep(agent: str, fam: str, sit: int, served: int, eligible: int = 1, r: int = 0, fidelity: str = "x",
       actions_n: int = 0) -> dict[str, Any]:
    from eval.headless.tests.test_v2_aggregate import BASE

    m = copy.deepcopy(BASE)
    m.update(served=served, eligible=eligible, needs=eligible, served_share=(served / eligible) if eligible else None,
             actions=actions_n, fault_met=0)
    return {"path": None, "agent": agent, "base": fam, "family": fam, "situation": f"{fam}:{sit}", "situation_seed": sit,
            "r": r, "operator": "approve-all", "policy": "watch.toml", "clock": "paused", "class": "scored", "area": None,
            "metrics": m, "cells": {}, "fidelity": {"truth": fidelity, "picture": fidelity, "audit": fidelity}}


COND = "approve-all;watch.toml;paused"


def synthetic() -> list[dict[str, Any]]:
    """Three families: `fast-unknown` admissible (none 0 %, oracle 100 %), `decoy` not (none
    100 %), `harbor-watch` reported alone; the model beats the playbook by 1 in fast-unknown and
    loses by 1 in decoy."""
    out = []
    for sit in range(1, 6):
        out += [ep("none", "fast-unknown", sit, 0), ep("oracle", "fast-unknown", sit, 1),
                ep("model", "fast-unknown", sit, 1), ep("playbook", "fast-unknown", sit, 0),
                ep("none", "decoy", sit, 1), ep("oracle", "decoy", sit, 1),
                ep("model", "decoy", sit, 0), ep("playbook", "decoy", sit, 1),
                ep("none", "harbor-watch", sit, 0), ep("oracle", "harbor-watch", sit, 1),
                ep("model", "harbor-watch", sit, 0), ep("playbook", "harbor-watch", sit, 1)]
    return out


def test_the_primary_pools_only_admissible_families():
    from eval.headless.v2 import aggregate as A

    scored = synthetic()
    agg = A.aggregate(scored)
    fams = agg["primary_families"][COND]
    assert fams["admitted"] == ["fast-unknown"] and set(fams["excluded"]) == {"decoy", "harbor-watch"}
    assert "this batch" in fams["source"] and fams["excluded"]["harbor-watch"].startswith("reported alone")
    p = agg["primary"][COND]
    assert p["families"] == ["fast-unknown"] and p["n_situations"] == 5 and p["mean_difference"] == pytest.approx(1.0)
    # pooled over every family the difference would be (5 - 5 - 5) / 15: admissibility is applied
    every = A.primary_endpoint(scored, ("model", "playbook"), "approve-all", ("model",), 0)[COND]
    assert every["n_situations"] == 15 and every["mean_difference"] == pytest.approx(-1 / 3)


def test_given_verdicts_admit_and_none_means_no_primary():
    from eval.headless.v2 import aggregate as A

    scored = [e for e in synthetic() if e["agent"] not in ("none", "oracle")]
    agg = A.aggregate(scored)
    assert agg["primary"][COND] == {"skipped": "no admitted family (resolution 21): see primary_families"}
    assert agg["primary_families"][COND]["admitted"] == []
    given = {"source": "admissibility-batch/aggregate_v2.json",
             "verdicts": {COND: {"decoy": {"admissible": True}, "fast-unknown": {"admissible": False,
                                                                                 "none": {"share": 0.9},
                                                                                 "oracle": {"share": 1.0}}}}}
    agg = A.aggregate(scored, admissible=given)
    fams = agg["primary_families"][COND]
    assert fams["admitted"] == ["decoy"] and fams["source"].startswith("given:")
    assert fams["excluded"]["fast-unknown"].startswith("not admissible")
    assert agg["primary"][COND]["mean_difference"] == pytest.approx(-1.0)


def test_the_world_pilot_gate():
    from eval.headless.v2 import aggregate as A

    scored = []
    for sit in range(1, 6):
        scored += [ep("none", "fast-unknown", sit, 0), ep("oracle", "fast-unknown", sit, 1),
                   ep("playbook", "fast-unknown", sit, 1 if sit > 1 else 0), ep("random-valid", "fast-unknown", sit, 0),
                   ep("null", "fast-unknown", sit, 0),
                   ep("none", "quiet", sit, 0, eligible=0), ep("oracle", "quiet", sit, 0, eligible=0),
                   ep("null", "quiet", sit, 0, eligible=0)]
    g = A.world_pilot(scored, COND)
    assert g["pass"] and g["families"]["fast-unknown"]["pass"]
    assert g["families"]["fast-unknown"]["playbook"]["share"] == pytest.approx(0.8)
    assert all(v is None for k, v in g["families"]["quiet"]["checks"].items() if k != "null_fidelity")
    # a null that differs from its none fails the gate, and says where
    bad = [dict(e, fidelity={"truth": "x", "picture": "y", "audit": "x"}) if e["agent"] == "null" and e["situation_seed"] == 3
           else e for e in scored]
    g = A.world_pilot(bad, COND)
    assert not g["pass"] and g["pooled"]["null_fidelity"]["differences"][0]["differs"] == ["picture"]
    # none over 50 % fails
    worse = [dict(e, metrics={**e["metrics"], "served": 1}) if e["agent"] == "none" and e["base"] == "fast-unknown"
             else e for e in scored]
    assert not A.world_pilot(worse, COND)["families"]["fast-unknown"]["checks"]["none_at_most_50"]


# ---- the world's reserve point (0.1) ------------------------------------------------------------------

def test_the_candidates_were_committed_with_coordinates_and_reasons():
    cands = reserve_candidates()
    assert list(cands)[0] == "spawn" and len(cands) == 6
    for c in cands.values():
        assert -122.53 <= c["lon"] <= -122.39 and 37.77 <= c["lat"] <= 37.87 and c["why"].strip() and c["place"]
    spawn = cands["spawn"]
    assert (spawn["lat"], spawn["lon"]) == (37.8210, -122.4930), "PUMA 1's spawn before the calibration"


def test_a_reserve_candidate_moves_puma_1_only(tmp_path: Path):
    base = tomllib.loads(V2_FLEET.read_text())
    path, what = fleet_with_reserve("fort-mason", tmp_path)
    moved = tomllib.loads(path.read_text())
    assert what["callsign"] == "PUMA 1" and len(moved["unit"]) == len(base["unit"])
    for i, (a, b) in enumerate(zip(base["unit"], moved["unit"])):
        if i == 2:
            assert b["spawn"][:3] == [37.8110, -122.4315, 120.0] and b["spawn"][3] == a["spawn"][3]
            assert {k: v for k, v in a.items() if k != "spawn"} == {k: v for k, v in b.items() if k != "spawn"}
        else:
            assert a == b
    e, _ = generate("v2-decoy", 1000, tmp_path, reserve="fort-mason")
    (tmp_path / "plain").mkdir()
    plain, _ = generate("v2-decoy", 1000, tmp_path / "plain")
    assert e.truth["reserve"]["name"] == "fort-mason" and "reserve" not in plain.truth
    assert e.fleet != plain.fleet == V2_FLEET
    assert v2_reserve_spawn()[:2] == [base["unit"][2]["spawn"][0], base["unit"][2]["spawn"][1]]
    with pytest.raises(ValueError):
        generate("decoy", 1000, tmp_path, reserve="fort-mason")


def test_the_calibration_rule(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    """0.1: a candidate qualifies when in every family with eligible needs the oracle serves at
    least 80 % and none at most 50 %; the lowest pooled none share wins; a tie goes to the one
    nearest the current spawn; no qualifier, no choice."""
    from eval.headless import reserve

    def fam(none: int, oracle: int, n: int = 10) -> dict[str, Any]:
        return {"none": {"served": none, "eligible": n, "share": none / n if n else None, "n_situations": n},
                "oracle": {"served": oracle, "eligible": n, "share": oracle / n if n else None, "n_situations": n}}

    table = {
        "spawn": {"fast-unknown": fam(9, 10), "decoy": fam(9, 10), "quiet": fam(0, 0, 0)},  # none too high
        "crissy-field": {"fast-unknown": fam(1, 10), "decoy": fam(0, 9), "quiet": fam(0, 0, 0)},
        "fort-baker": {"fast-unknown": fam(1, 10), "decoy": fam(0, 8), "quiet": fam(0, 0, 0)},  # ties crissy, nearer
        "kirby-cove": {"fast-unknown": fam(5, 10), "decoy": fam(0, 10)},
        "sausalito": {"fast-unknown": fam(0, 10), "decoy": fam(0, 7)},  # oracle under 80 % in decoy
        "fort-mason": {"fast-unknown": fam(2, 10), "decoy": fam(0, 10)},
    }
    for name in table:
        (tmp_path / name).mkdir()
        (tmp_path / name / "episodes_v2.json").write_text("[]")
    monkeypatch.setattr(reserve, "shares", lambda d: {"families": table[Path(d).name], "not_scored_or_other": []})
    res = reserve.choose(tmp_path)
    assert res["qualifying"] == ["crissy-field", "fort-baker", "kirby-cove", "fort-mason"]
    assert res["candidates"]["sausalito"]["fails"] == {"decoy": {"none": 0.0, "oracle": 0.7}}
    assert res["candidates"]["spawn"]["judged"] == ["decoy", "fast-unknown"], "quiet has no eligible needs"
    near = reserve_candidates()
    assert dist_m((near["fort-baker"]["lon"], near["fort-baker"]["lat"]), (near["spawn"]["lon"], near["spawn"]["lat"])) < \
        dist_m((near["crissy-field"]["lon"], near["crissy-field"]["lat"]), (near["spawn"]["lon"], near["spawn"]["lat"]))
    assert res["chosen"] == "fort-baker"
    assert (tmp_path / "calibration.md").read_text().count("| fort-baker |") == 1
    for name in ("crissy-field", "fort-baker", "kirby-cove", "fort-mason"):
        table[name]["decoy"] = fam(0, 5)
    assert reserve.choose(tmp_path)["chosen"] is None


def test_the_bridge_reference_point_is_the_gate():
    assert GATE == (-122.4783, 37.8199)


def test_a_v2_family_s_stated_area_places_it_in_the_scorecard():
    """The v2 generators state the area as `S1 identify` (families.py V2_AREAS); the scorecard's
    rows are by its code (resolution 27)."""
    from eval.headless.families import V2_AREAS
    from eval.headless.v2.aggregate import area_of

    assert area_of({"area": "S1 identify"}) == "S1" and area_of({"area": "Harbor watch"}) == "HW"
    for fam, area in V2_AREAS.items():
        assert area_of({"area": area, "family": fam}) == area_of({"family": fam}), fam
