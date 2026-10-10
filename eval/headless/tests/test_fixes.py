"""Fix round 1 of the independent review (2026-10-10): one test per finding, each failing on the
code before its fix.

1. both sides of G1 run the link emulator on the same seed for an episode;
3. link-cut-mid-look cuts only a look that started, and records "no look to cut" otherwise;
4. no computer-time line in the record (the live agent's `tick.slow`);
5. the untracked inputs (links file, space cache) are in the provenance, and the bridge reads the
   links file the emulator reads;
6. the metrics as CRITERIA.md writes them: a look is `isr.investigate`; precision judges P0
   commands by truth; truth at the scenario second nearest, not interpolated;
7. timing: the contacts' truth per contacts step, the playbook's first tick drawn from the seed,
   the bridge up at its own offset;
8. a full-stack run records its binaries' hashes;
9. the agent's reconnect delay doubles to 30 s as live, and a new session follows three missed
   ticks at once.
(2: test_labels_families.py `test_g1_pairs_within_ten_seconds_exactly`; 9's tool-result shape:
crates/murmur-headless/tests/pieces.rs.)
"""

from __future__ import annotations

import asyncio
import json
import re
import tomllib
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless import metrics as M
from eval.headless.families import LINK_CUT_DEADLINE_S, generate
from eval.headless.labels import AMBIGUOUS, Truth, associate, contact_labels
from eval.headless.script import Script
from eval.headless.situations import Calibration, from_situation, links_file, process_spec

SITUATION = REPO / "sim" / "situations" / "harbor-watch.toml"


def copy_with_seed(tmp: Path, k: int) -> Path:
    text, n = re.subn(r"(?m)^seed = \d+", f"seed = {k}", SITUATION.read_text())
    assert n == 1
    p = tmp / f"harbor-watch-seed{k}.toml"
    p.write_text(text)
    return p


# ---- 1. the emulator's seed ------------------------------------------------------------------

def test_both_sides_run_the_emulator_on_the_situation_seed(tmp_path: Path):
    from eval.headless.fullstack import stack_env

    links_seed = tomllib.loads(links_file().read_text()).get("seed", 1)
    for k in (1, 7, 13):
        copy = copy_with_seed(tmp_path, k)
        env = stack_env(copy)
        full = int(env.get("MURMUR_LINKEMU_SEED") or links_seed)  # unset: the links file's seed
        spec, _ = process_spec(from_situation(copy), k, tmp_path / f"w{k}", Calibration())
        assert spec.get("linkemu_seed") == full == k
    # up.sh hands it to the emulator, and leaves the links file's seed alone without it
    up = (REPO / "scripts" / "up.sh").read_text()
    assert '[ -n "${MURMUR_LINKEMU_SEED:-}" ] && emu_args+=(--seed "$MURMUR_LINKEMU_SEED")' in up
    # a plain Harbor watch (seed 1) and a family episode: the situation's seed
    spec, _ = process_spec(from_situation("harbor-watch"), 5, tmp_path / "hw", Calibration())
    assert spec["linkemu_seed"] == 1
    e, contacts = generate("fast-unknown", 9, tmp_path)
    spec, _ = process_spec(e, 9, tmp_path / "fam", Calibration(), contacts_path=contacts)
    assert spec["linkemu_seed"] == 9


# ---- 3. the link cut waits for a look ---------------------------------------------------------

class FakeSim:
    def __init__(self, intents: list[dict[str, Any]]):
        self.intents = intents
        self.faults: list[dict[str, Any]] = []

    def api(self, method: str, path: str, body: Any = None, operator: str | None = None) -> tuple[int, Any]:
        assert (method, path) == ("GET", "/api/intents")
        return 200, {"intents": self.intents}

    def fault(self, kind: str, **fields: Any) -> dict[str, Any]:
        self.faults.append({"kind": kind, **fields})
        return {"ok": True}


class FakeEp:
    def __init__(self, sim: FakeSim):
        self.sim = sim
        self.sim_epoch = 1_000_000.0


def drive(script: Script, until: float) -> None:
    t = script.first()
    while t is not None and t <= until:
        t = script.wake(t)


def test_the_link_cut_waits_for_a_look_and_never_falls_back(tmp_path: Path):
    for seed in range(1, 31):
        e, _ = generate("link-cut-mid-look", seed, tmp_path)
        (item,) = e.script
        assert "fallback_at_s" not in item and "fallback_uid" not in item
        assert item["deadline_s"] == LINK_CUT_DEADLINE_S and e.truth["family_version"] == 2
    item = {"kind": "link_cut", "after_first_look": [5, 90], "delay_s": 30.0, "duration_s": 20.0,
            "deadline_s": LINK_CUT_DEADLINE_S}
    # no look dispatched: nothing cut, the log says so at the deadline
    s = Script(FakeEp(sim := FakeSim([])), [item])
    drive(s, 720.0)
    assert sim.faults == []
    assert len(s.log) == 1 and s.log[0]["skipped"].startswith("no look to cut")
    assert s.log[0]["t"] == pytest.approx(LINK_CUT_DEADLINE_S)
    # a look dispatched at 200 s: its vehicle cut 30 s later, once
    look = {"id": 4, "kind": "orbit", "uids": [3], "source": {"kind": "agent", "id": "agent-playbook"},
            "history": [{"state": "dispatched", "t": (1_000_000.0 + 200.0) * 1000.0}]}
    s = Script(FakeEp(sim := FakeSim([look])), [item])
    drive(s, 720.0)
    assert [f["event"]["uid"] for f in sim.faults] == [3]
    assert sim.faults[0]["event"]["at_s"] == pytest.approx(230.0, abs=1.0)
    assert not any("skipped" in r for r in s.log)


# ---- 4 and 9. the agent loop ------------------------------------------------------------------

class AgentSim:
    tools = ["get_fleet"]

    def __init__(self) -> None:
        self.t = 0.0


class AgentEp:
    def __init__(self) -> None:
        self.sim = AgentSim()
        self.aloop = asyncio.new_event_loop()

    def run_async(self, coro: Any) -> Any:
        return self.aloop.run_until_complete(coro)


def make_agent() -> Any:
    from eval.headless.episode import PlaybookAgent, make_config, playbook_json

    e = from_situation("harbor-watch")
    cfg = make_config(json.loads(playbook_json(e)), "harbor-watch")
    return PlaybookAgent(AgentEp(), cfg)  # type: ignore[arg-type]


def test_no_computer_time_line_goes_into_the_record():
    from playbook.engine import TickReport

    a = make_agent()

    async def slow_tick(session: Any) -> TickReport:
        return TickReport(t=0.0, compute_ms=120.0)

    a.engine.tick = slow_tick  # type: ignore[method-assign]
    a.started = True
    a._tick(50.0)
    assert not [ln for ln in a.lines if ln["kind"] == "tick.slow" or "compute_ms" in ln]
    assert a.slow_ticks == 1


def test_the_reconnect_delay_doubles_to_thirty_seconds_and_three_misses_reconnect_at_once():
    a = make_agent()

    async def refuse(session: Any) -> Any:
        raise ConnectionError("no bridge")

    a.engine.start = refuse  # type: ignore[method-assign]
    t, waits = 46.0, []
    for _ in range(7):
        nxt = a.wake(t)
        waits.append(round(nxt - t, 3))
        t = nxt
    assert waits == [2.0, 4.0, 8.0, 16.0, 30.0, 30.0, 30.0]

    async def start(session: Any) -> dict[str, Any]:
        return {"run": "r", "cursor": "c", "reset": True}

    async def fail(session: Any) -> Any:
        raise RuntimeError("tick failed")

    async def no_changes(session: Any) -> list[Any]:
        return []

    a.engine.start, a.engine.tick, a.engine.poll_changes = start, fail, no_changes  # type: ignore[method-assign]
    nxt = a.wake(t)  # connects (delay back to 2 s), first miss
    assert a.started and a.retry_s == 2.0
    for _ in range(40):
        if not a.started:
            break
        t, nxt = nxt, a.wake(nxt)
    assert not a.started and nxt == t  # the third miss: the next session at the same instant


# ---- 5. provenance ----------------------------------------------------------------------------

def test_the_untracked_inputs_are_in_the_provenance_and_the_bridge_reads_the_emulators_links(tmp_path: Path):
    from eval.headless.episode import provenance
    from eval.headless.provenance import sha256_file

    prov = provenance(None)
    assert prov["links"] == {"path": str(links_file()), "sha256": sha256_file(links_file())}
    cache = REPO / "sim" / "space" / "cache"
    assert prov["space_cache"]["sha256"] if cache.is_dir() else prov["space_cache"]["files"] == 0
    spec, env = process_spec(from_situation("harbor-watch"), 1, tmp_path, Calibration())
    assert env["MURMUR_LINKS"] == spec["links"] == str(links_file())


# ---- 6. metrics as CRITERIA.md writes them ----------------------------------------------------

def samples(*rows: tuple[float, list[tuple[str, float, float]]]) -> list[dict[str, Any]]:
    return [{"t": t, "contacts": [[cid, lon, lat, 0.0, True] for cid, lon, lat in cs], "ours": []} for t, cs in rows]


M_PER_DEG_LON = 111_320.0 * 0.7907  # at 37.8 deg


def east(m: float) -> float:
    return -122.60 + m / M_PER_DEG_LON


def test_truth_is_the_scenario_second_nearest_not_interpolated():
    # A runs east 1 km a second; B sits 700 m east of A's 100 s fix; a track at 400 m at 100.4 s
    rec = {"truth": {"contacts": [{"id": "A", "affiliation": "unknown"}, {"id": "B", "affiliation": "neutral"}],
                     "samples": samples((100.0, [("A", east(0), 37.8), ("B", east(700), 37.8)]),
                                        (101.0, [("A", east(1000), 37.8), ("B", east(700), 37.8)]))}}
    tr = Truth(rec)
    # interpolated, A would be exactly there; at the nearest second (100 s) B is 300 m off and A
    # 400 m, within 1.5 times: ambiguous
    assert associate(tr, [east(400), 37.8], 100.4) == AMBIGUOUS
    assert associate(tr, [east(950), 37.8], 100.6) == "A"
    # the crossing is the first sample within, not a point between samples
    rec2 = {"truth": {"contacts": [{"id": "A", "affiliation": "unknown"}],
                      "samples": samples((10.0, [("A", -122.4783 - 3200 / M_PER_DEG_LON, 37.8199)]),
                                         (11.0, [("A", -122.4783 - 2600 / M_PER_DEG_LON, 37.8199)]))}}
    assert contact_labels(rec2)["A"]["first_within_s"] == 11.0


def test_a_look_is_investigate_and_precision_judges_p0_by_truth(monkeypatch):
    rec = {"clock": {"epoch": 0.0, "ai_start_s": 45.0, "duration_s": 720.0},
           "truth": {"contacts": [{"id": "boat", "affiliation": "unknown"}],
                     "samples": samples((100.0, [("boat", -122.4783, 37.8199)])),
                     "ours": [{"t": 300.0, "ours": [[4, 0, 0, "AUTO", None, 22.0], [2, 0, 0, "AUTO", None, 80.0]]}]},
           "script": [{"t": 250.0, "kind": "stop", "uid": 1}], "audit_text": "", "agent_log": []}

    def prop(t: float, play: str, target: str | None, uid: int, tier: str = "human") -> dict[str, Any]:
        return {"t": t, "intent": None, "play": play, "target": target, "truth_target": target, "uid": uid,
                "tier": tier, "state": "pending_approval", "rationale": "", "tracker_target": None,
                "picture": None, "version": "1", "task": "orbit", "refused": None}

    props = [prop(110.0, "isr.armed_overwatch", "boat", 3),
             prop(300.0, "safety.low_reserve", "battery:4", 4), prop(300.0, "safety.low_reserve", "battery:2", 2),
             prop(260.0, "safety.stuck", "alert:1", 1), prop(200.0, "safety.stuck", "alert:1", 1),
             prop(120.0, "isr.recentre", "boat", 3), prop(130.0, "isr.release", "boat", 3)]
    monkeypatch.setattr(M, "proposals", lambda rec, to_s: props)
    m = M.episode_metrics(rec)
    assert m["look_covered"] is False  # armed overwatch is not a look (CRITERIA.md)
    # legit: low reserve on uid 4 (22 % < 25), stuck after the stop, the recentre on the boat;
    # not: low reserve on uid 2 (80 %), stuck before the stop; unjudged: overwatch, release
    assert m["precision"] == {"legit": 3, "n": 5, "unjudged": ["isr.armed_overwatch", "isr.release"]}


# ---- 7. timing --------------------------------------------------------------------------------

def _sim(tmp: Path, calib: Calibration) -> Any:
    from eval.headless.sim import Sim

    tmp.mkdir(parents=True, exist_ok=True)
    spec, env = process_spec(from_situation("harbor-watch"), 1, tmp, calib)
    return Sim(spec, env, log=tmp / "sim.log")


def test_the_contacts_truth_is_one_sample_per_step(tmp_path: Path):
    calib = Calibration(contacts_start_s=0.823, standins_start_s=1.001, bridge_start_s=1.026)
    sim = _sim(tmp_path, calib)
    try:
        sim.step(12.0)
        out = sim.rpc({"op": "truth", "association": False, "log_from": 0})
    finally:
        sim.end()
    assert out["contacts_start_s"] == 0.823
    assert [ln["s"] for ln in out["log"]] == [float(n) for n in range(12)]
    assert all(len(ln["contacts"]) == len(out["log"][0]["contacts"]) for ln in out["log"])


def test_the_first_tick_is_drawn_from_the_seed_within_the_calibrated_range():
    c = Calibration(ai_first_tick_lo_s=0.9, ai_first_tick_hi_s=1.6)
    draws = [c.ai_first_tick(k) for k in range(1, 41)]
    assert all(0.9 <= d <= 1.6 for d in draws)
    assert draws == [c.ai_first_tick(k) for k in range(1, 41)]
    assert len(set(draws)) > 30 and max(draws) - min(draws) > 0.4


def test_the_bridge_is_up_at_its_own_offset(tmp_path: Path):
    """Before the bridge is up, a vehicle's frames reach no one (stand-ins start at 1.0 s)."""
    heard = {}
    for b0 in (0.0, 1.6):
        calib = Calibration(contacts_start_s=0.823, standins_start_s=1.001, bridge_start_s=b0)
        sim = _sim(tmp_path / f"b{b0}", calib)
        try:
            sim.step(1.5)
            _, st = sim.api("GET", "/api/state")
            heard[b0] = sum(1 for v in st.get("vehicles", []) if (v.get("link") or {}).get("last_rx_t"))
            assert sim.truth(association=False)["bridge_start_s"] == b0
        finally:
            sim.end()
    assert heard[0.0] == 4 and heard[1.6] == 0


# ---- 8. the full stack's provenance -----------------------------------------------------------

def test_a_full_stack_run_records_its_binaries_and_inputs(tmp_path: Path):
    from eval.headless.fullstack import stack_env, stack_provenance
    from eval.headless.provenance import STACK_BINARIES

    prov = stack_provenance(stack_env(copy_with_seed(tmp_path, 11)))
    assert set(prov["binaries"]) == set(STACK_BINARIES)
    assert prov["binaries"]["murmur-bridge"]  # built (make check builds it)
    assert prov["emulator_seed"] == 11 and prov["commit"] and "dirty" in prov
    assert prov["links"]["sha256"] and "space_cache" in prov
