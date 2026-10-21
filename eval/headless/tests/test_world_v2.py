"""Scoring v2's world (eval/SCORING-V2.md, sections 0, 1, 4.4, 4.5, 9.1): the platform variants and
the v2 fleet, the mission as every agent's brief, each v2 family's element (deterministic for a
situation seed), the v1 families unchanged, opaque run ids, the look trigger for any agent under
test, and the misclassification draw's port against the simulator itself.
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import tempfile
import tomllib
from dataclasses import fields
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless.families import (
    AIS_PLAUSIBLE_MPS,
    BASE_CONTACTS,
    CONFUSION,
    FAMILIES,
    FAMILIES_V2,
    GATE,
    OFF_APPROACHES_M,
    SITUATIONS_V2,
    V2_AREAS,
    V2_CONFIG_NAME,
    V2_FLEET,
    classifier_uids,
    contacts_toml,
    dist_m,
    generate,
    misclassified,
    need_deadline_s,
    offset,
    ring,
    v2_vehicles,
    water,
)
from eval.headless.script import Script, first_look
from eval.headless.situations import Calibration, fleet_plan, from_situation, opaque_run_id, process_spec, run_id
from eval.headless.v2.mission import MISSION, MISSION_SHA256, mission_sha256

SEEDS = range(1000, 1030)
PUMAS = [3, 4]


def gen(family: str, seed: int, tmp: Path, world: str = "v2") -> tuple[Any, Path, dict[str, Any]]:
    d = tmp / f"{world}-{family}-{seed}"
    d.mkdir(parents=True, exist_ok=True)
    e, p = generate(family, seed, d, world=world)
    return e, p, tomllib.loads(p.read_text())


def added(raw: dict[str, Any]) -> list[dict[str, Any]]:
    base = {c["id"] for c in tomllib.loads(BASE_CONTACTS.read_text())["contact"]}
    return [c for c in raw["contact"] if c["id"] not in base]


def comes_within(c: dict[str, Any], m: float) -> bool:
    if c.get("route"):
        pts = [tuple(p) for p in c["route"]]
        return any(dist_m(tuple(q), GATE) <= m for a, b in zip(pts, pts[1:])
                   for q in [(a[0] + k / 50 * (b[0] - a[0]), a[1] + k / 50 * (b[1] - a[1])) for k in range(51)])
    at = tuple(c.get("at") or c.get("center"))
    return dist_m(at, GATE) + float(c.get("radius_m", 0.0)) <= m


# ---- the mission, the platforms, the fleet -----------------------------------------------------

def test_the_mission_is_the_contracts_text_and_its_hash():
    assert mission_sha256() == MISSION_SHA256 == hashlib.sha256(MISSION.encode()).hexdigest()
    assert MISSION.startswith("Identify every contact of unknown, suspect or hostile affiliation that comes within 3 km")
    assert "a confidence of 0.6 or more from an EO/IR or sonar sensor, without an identity conflict." in MISSION
    assert MISSION.endswith("Keep every vehicle above its reserve.")


def test_the_variants_are_their_platforms_without_eo_ir_or_sonar():
    cat = tomllib.loads((REPO / "sim" / "platforms.toml").read_text())
    for base, var in (("mq9b", "mq9b_radar"), ("seahunter", "seahunter_radar")):
        a, b = dict(cat[base]), dict(cat[var])
        sa, sb = a.pop("sensors"), b.pop("sensors")
        assert a == b, f"{var} differs from {base} beyond its sensors"
        assert sb == [s for s in sa if s["type"] not in ("eo_ir", "sonar")]
        assert {s["type"] for s in sb} == {"radar", "ais"}


def test_the_v2_fleet_is_harbor_watch_with_the_variants():
    v1, _ = fleet_plan(REPO / "sim" / "fleets" / "harbor-watch.toml")
    v2, _ = fleet_plan(V2_FLEET)
    assert [v["callsign"] for v in v2] == ["HUNTER 1", "REAPER 1", "PUMA 1", "PUMA 2"]
    swap = {"seahunter": "seahunter_radar", "mq9b": "mq9b_radar", "puma": "puma"}
    for a, b in zip(v1, v2):
        assert b["platform"] == swap[a["platform"]]
        assert {k: v for k, v in a.items() if k != "platform"} == {k: v for k, v in b.items() if k != "platform"}
    assert classifier_uids() == PUMAS
    assert v2_vehicles()["REAPER 1"]["sensors"] == ["ais", "radar"]
    assert v2_vehicles()["HUNTER 1"]["sensors"] == ["ais", "radar"]


# ---- v1 is unchanged ----------------------------------------------------------------------------

#: the v1 generator's output before scoring v2 (2026-10-21, commit e8dcedb): `v1_digest` over the
#: fields `EpisodeSpec` had then, and the contacts file
V1_DIGESTS = {
    "approval-flood:1": "8b8123ea1fc527ed3a86a1b38ac7ffb5642d75606e00f94a2790c24ab3687d63",
    "approval-flood:1000": "eab5e4787dd8f47d1880bb7299fcc2f54f239cb829028cc85886aa767c86825e",
    "approval-flood:17": "8c79a370c8f2944c31da7ee8b4e9f862533480c89708bd521413d385952e10d7",
    "decoy:1": "8781bd8644172d982e60e1695b4a4f5173ec3db20e50ff344f93e9f2f3a6d840",
    "decoy:1000": "eb243cfc7d545b7a9dcbd7290ec3bf651b74d5f778641ce24aa1bc6e796ea330",
    "decoy:17": "0a7cdac8ab136d192581ccf7c1cabaccfc9237edafe6e7a860ed402ad658a3e9",
    "fast-unknown:1": "2c2945c12945911d2861d2a27c235b491c3f0ad00446c08d1712a58f8fd1366b",
    "fast-unknown:1000": "c7e545c79eb2ddb127dd4d206667d89556446e8b19a6a40a1d17ca4d14dc6fc0",
    "fast-unknown:17": "1143474054c87084897986f5d8cf479e1390738cee02733306afa2d4be10b82e",
    "link-cut-mid-look:1": "6e8a01a38b2e8a69be4b026e4ad01f645551f0dff04c6e727b73242dc2ca8cf2",
    "link-cut-mid-look:1000": "8231d748ceb567debd5a4349747e82428a420f55cd7df4c50239b2f2a656f4e4",
    "link-cut-mid-look:17": "cb69eb22da60d3a164de2f06a88d95c4daabe0f72d1ea13b9685419ec22fe104",
    "low-battery:1": "47a22f6f046b08e269bfd3378f20a6a41d950b8f6b9fd0166ead9eae3f66155f",
    "low-battery:1000": "ae32b2045eb7f2bfbb2ae3476638cc1aab36196ce4d2cd25ea0f490b7a67001b",
    "low-battery:17": "dcce620410f416287e14376f9716a16f6c019a39537f3d1f4c14cf42658d945e",
    "stuck-vehicle:1": "1f4c05568636bbc0ce67047ed199f34e9bcd3ee490533c4db564bd65f0862765",
    "stuck-vehicle:1000": "06a1925bb6b892aec7a01c5425f61e36fdab6ebc83628343d03db928eca3ad4e",
    "stuck-vehicle:17": "6cdbce147dc3bb5f653c01faaf83e4cac9f48f2365e3d6364d2f1c7039c2e601",
    "two-at-once:1": "7b8639ffbde916455ff40a9a7937e69d7974af705389584488c5a4856a810c6d",
    "two-at-once:1000": "5a3701dd1f154fdd08c6b8a246d1b80462ff9686f0829676f1fa2a0ccade0209",
    "two-at-once:17": "14aabdd6af3ed02d5c6449869479fae148861103730f89963903f02dcd41d5c1",
}
V1_FIELDS = ("name", "fleet", "contacts", "policy", "at", "duration_s", "ai_start_s", "ready_timeout_s", "orders",
             "playbook", "autoplay", "seed", "events", "contacts_seed", "family", "split", "situation_seed",
             "battery_pct", "script", "truth", "path", "config_name", "brief")


def v1_digest(e: Any, path: Path) -> str:
    d: dict[str, Any] = {}
    for f in fields(e):
        if f.name not in V1_FIELDS:
            continue
        v = getattr(e, f.name)
        if f.name == "contacts":
            v = Path(v).name if v else None
        elif isinstance(v, Path):
            v = str(v)
        elif f.name == "orders":
            v = [repr(o) for o in v]
        d[f.name] = v
    d["contacts_text"] = Path(path).read_text()
    return hashlib.sha256(json.dumps(d, sort_keys=True, default=str).encode()).hexdigest()


def test_the_v1_families_are_unchanged(tmp_path: Path):
    assert set(FAMILIES) == {k.split(":")[0] for k in V1_DIGESTS}
    for key, want in V1_DIGESTS.items():
        family, seed = key.split(":")
        e, p, _ = gen(family, int(seed), tmp_path, world="v1")
        assert v1_digest(e, p) == want, f"v1 {key} changed"
        assert e.world == "v1" and e.replicate is None
        # the default world is v1
        assert generate(family, int(seed), p.parent)[0].world == "v1"


def test_v1_run_ids_and_operator_notes_are_unchanged(tmp_path: Path):
    hw = from_situation("harbor-watch")
    _, env = process_spec(hw, 1, tmp_path, Calibration())
    assert env["MURMUR_RUN_ID"] == "headless-harbor-watch-s1"  # the golden episode's audit log holds it
    assert hw.operator_note("op-auto") == "scripted (situation harbor-watch, autoplay): op-auto (not a person)"
    e, _, _ = gen("fast-unknown", 3, tmp_path, world="v1")
    assert run_id(e, 3) == "headless-fast-unknown-3-s3"


# ---- every v2 situation -------------------------------------------------------------------------

@pytest.mark.parametrize("family", SITUATIONS_V2)
def test_a_v2_situation_is_the_v2_base_with_the_mission_as_its_brief(family: str, tmp_path: Path):
    e, p, raw = gen(family, 1000, tmp_path)
    assert e.world == "v2" and e.split == "v2" and e.family == family and e.situation_seed == 1000
    assert e.fleet == V2_FLEET
    assert e.brief == MISSION, "the mission verbatim, nothing else (no title, no situation name)"
    assert e.truth["mission_sha256"] == MISSION_SHA256 and e.truth["area"] == V2_AREAS[family]
    # the opening orders, each with its own note (episode.py's fallback note names the situation)
    hw = from_situation("harbor-watch")
    assert [o.id for o in e.orders] == [o.id for o in hw.orders] and all(o.note for o in e.orders)
    # the playbook's config: Harbor watch's table (same hash), a label that names no situation
    assert e.playbook == hw.playbook and e.config_name == V2_CONFIG_NAME
    assert e.policy == hw.policy and e.duration_s == hw.duration_s and e.ai_start_s == hw.ai_start_s
    assert "situation" not in e.operator_note("op-auto") and family not in e.operator_note("op-auto")
    # the stated misclassification draws are the sensing model's for this file's seed
    rate = raw["sensing"]["misclassification_rate"]
    assert e.truth["sensing"]["seed"] == raw["seed"]
    for c in raw["contact"]:
        want = [s for s in ("eo_ir", "sonar") if misclassified(raw["seed"], c["id"], s, rate)]
        assert e.truth["sensing"]["misclassified"].get(c["id"], []) == want
    # the v2 prefix names the same situation
    e2, _ = generate(f"v2-{family}", 1000, p.parent if p.parent.is_relative_to(tmp_path) else tmp_path)
    assert e2.truth == e.truth and e2.world == "v2"


@pytest.mark.parametrize("family", SITUATIONS_V2)
def test_a_v2_situation_seed_is_one_situation_and_seeds_vary_it(family: str, tmp_path: Path):
    e1, p1, r1 = gen(family, 1003, tmp_path / "a")
    e2, p2, r2 = gen(family, 1003, tmp_path / "b")
    assert p1.read_text() == p2.read_text()
    assert (e1.truth, e1.script, e1.battery_pct) == (e2.truth, e2.script, e2.battery_pct)
    texts = {gen(family, s, tmp_path / "c")[1].read_text() for s in range(1000, 1005)}
    if family == "harbor-watch":
        assert texts == {BASE_CONTACTS.read_text()}, "Harbor watch is one situation: its own file"
    else:
        assert len(texts) == 5, "contacts (their seed, at least) vary with the situation seed"


def test_s1_and_s2_draw_the_v1_contacts(tmp_path: Path):
    """Unchanged draws: the same contacts in both worlds (title aside), so a v1-world run shows
    what the world change did."""
    for family in ("fast-unknown", "two-at-once"):
        for s in (1000, 1001, 1002):
            r1 = gen(family, s, tmp_path, world="v1")[2]
            r2 = gen(family, s, tmp_path, world="v2")[2]
            assert {k: v for k, v in r1.items() if k != "title"} == {k: v for k, v in r2.items() if k != "title"}


# ---- each family's element ------------------------------------------------------------------------

def test_s1_fast_unknown_adds_one_closing_unknown(tmp_path: Path):
    for s in SEEDS:
        e, _, raw = gen("fast-unknown", s, tmp_path)
        (c,) = added(raw)
        assert (c["affiliation"], c["kind"], c.get("ais")) == ("unknown", "small_fast_boat", None)
        assert comes_within(c, 3000.0) and water().path_wet([tuple(p) for p in c["route"]])
        assert not e.script and not e.battery_pct


def test_s2_two_at_once_adds_two_unknowns_a_minute_apart(tmp_path: Path):
    for s in SEEDS:
        _, _, raw = gen("two-at-once", s, tmp_path)
        a, b = added(raw)
        assert a["affiliation"] == b["affiliation"] == "unknown"
        assert 0.0 <= b["start_s"] - a["start_s"] <= 60.0
        assert comes_within(a, 3000.0) and comes_within(b, 3000.0)


def test_s3_decoy_adds_a_neutral_craft_inside_the_circle_and_one_need(tmp_path: Path):
    kinds, motions = set(), set()
    for s in SEEDS:
        e, _, raw = gen("decoy", s, tmp_path)
        new = {c["id"]: c for c in added(raw)}
        decoy, need = new["craft-1"], new["fast-1"]
        assert decoy["affiliation"] == "neutral" and "ais" not in decoy
        assert comes_within(decoy, 3000.0), "it looks suspicious where the mission looks: inside 3 km"
        assert need["affiliation"] == "unknown" and comes_within(need, 3000.0)
        kinds.add(decoy["kind"])
        motions.add(decoy["motion"])
    assert kinds == {"fishing_vessel", "sailboat", "small_fast_boat"} and motions == {"transit", "loiter", "drift"}


def test_s4_quiet_has_nothing_that_needs_a_look(tmp_path: Path):
    base = tomllib.loads(BASE_CONTACTS.read_text())["contact"]
    for s in SEEDS:
        e, _, raw = gen("quiet", s, tmp_path)
        assert [c["id"] for c in raw["contact"]] == [c["id"] for c in base if c["id"] != "fastboat-west"]
        assert not [c for c in raw["contact"] if c["affiliation"] in ("unknown", "suspect", "hostile")]
        assert not e.script and not e.battery_pct


def test_s5_low_battery_draws_a_pumas_twenty_percent_crossing_inside_the_episode(tmp_path: Path):
    uids = set()
    for s in SEEDS:
        e, _, _ = gen("low-battery", s, tmp_path)
        ((uid, pct),) = e.battery_pct.items()
        assert uid in PUMAS
        uids.add(uid)
        assert pct <= 35.0, "the low-reserve need opens at the start"
        # a Puma at cruise drains its pack in its catalog endurance (2.5 h): 1 % in 90 s
        crossing = (pct - 20.0) / 100.0 * 2.5 * 3600.0
        assert 200.0 - 1.0 <= crossing <= 600.0 + 1.0
        assert e.truth["low_battery"]["drawn_crossing_s"] == pytest.approx(crossing, abs=1.0)
    assert uids == set(PUMAS)


def test_s6_stuck_vehicle_stops_the_first_looking_puma_or_puma_2(tmp_path: Path):
    for s in SEEDS:
        e, _, raw = gen("stuck-vehicle", s, tmp_path)
        (item,) = e.script
        assert item["kind"] == "stop" and item["look_uids"] == PUMAS and item["fallback_uid"] == 4
        assert set(item["look_tasks"]) == {"orbit", "patrol"} and 5.0 <= item["delay_s"] <= 60.0
        (need,) = added(raw)
        assert item["deadline_s"] == pytest.approx(need_deadline_s(need), abs=0.1)
        assert need["start_s"] + 60.0 < item["deadline_s"] < e.duration_s


def test_s7_link_cut_waits_for_the_first_puma_look(tmp_path: Path):
    for s in SEEDS:
        e, _, _ = gen("link-cut-mid-look", s, tmp_path)
        (item,) = e.script
        assert item["kind"] == "link_cut" and item["look_uids"] == PUMAS
        assert set(item["look_tasks"]) == {"orbit", "patrol"}
        assert 5.0 <= item["delay_s"] <= 90.0 and 8.0 <= item["duration_s"] <= 120.0 and item["deadline_s"] == 600.0


def test_s8_conflict_adds_an_honest_vessel_in_conflict_and_a_misclassified_need(tmp_path: Path):
    types = set()
    for s in SEEDS:
        e, _, raw = gen("conflict", s, tmp_path)
        new = {c["id"]: c for c in added(raw)}
        v, need = new["vessel-1"], new["fast-1"]
        # honest: neutral, its true AIS, not spoofed; faster than the tracker believes of its type
        assert v["affiliation"] == "neutral" and not v["ais"].get("spoofed")
        assert v["speed_mps"] >= AIS_PLAUSIBLE_MPS[v["ais"]["ship_type"]] + 2.0 and comes_within(v, 3000.0)
        types.add(v["ais"]["ship_type"])
        # the need's EO/IR draw is set under the contacts seed, at the base traffic's rate
        assert need["affiliation"] == "unknown" and comes_within(need, 3000.0)
        rate = raw["sensing"]["misclassification_rate"]
        assert rate == 0.02 and misclassified(raw["seed"], "fast-1", "eo_ir", rate)
        assert "eo_ir" in e.truth["sensing"]["misclassified"]["fast-1"]  # sonar's may be set too: none in v2
        assert e.truth["misclassified_need"]["shown_as"] == CONFUSION["small_fast_boat"] == "fishing_vessel"
    assert types == {"fishing", "pleasure"}


def test_s9_flood_orders_stay_off_the_approaches(tmp_path: Path):
    for s in SEEDS:
        e, _, _ = gen("approval-flood", s, tmp_path)
        (flood,) = e.script
        assert 6 <= len(flood["proposals"]) <= 12
        for p in flood["proposals"]:
            assert "flood" not in p["note"]
            # neither vehicle can identify anything in v2
            assert not set(p["uids"]) & set(PUMAS)
            if p["task"] == "patrol":
                pts = [tuple(x) for x in p["route"]]
                pts = [(a[0] + k / 40 * (b[0] - a[0]), a[1] + k / 40 * (b[1] - a[1]))
                       for a, b in zip(pts, pts[1:]) for k in range(41)]
                assert water().path_wet([tuple(x) for x in p["route"]])
            else:
                pts = ring(tuple(p["center"]), p["radius_m"], 72)
                # the bridge's platform_limits check refuses an orbit under 1.1 times the turn radius
                assert p["radius_m"] >= 1.1 * v2_vehicles()["REAPER 1"]["min_turn_radius_m"]
            assert min(dist_m(q, GATE) for q in pts) >= OFF_APPROACHES_M - 5.0, (s, p)


# ---- opaque run ids -------------------------------------------------------------------------------

def test_v2_run_ids_are_opaque_and_distinct(tmp_path: Path):
    seen: dict[str, tuple[str, int, Any]] = {}
    for family in SITUATIONS_V2:
        for s in range(1000, 1010):
            e, _, _ = gen(family, s, tmp_path)
            for rep in (None, 0, 1):
                e.replicate = rep
                rid = run_id(e, s)
                assert re.fullmatch(r"ep-[0-9a-f]{12}", rid), rid
                assert family not in rid and str(s) not in rid and "harbor" not in rid
                assert rid not in seen, (rid, seen.get(rid), (family, s, rep))
                seen[rid] = (family, s, rep)
            e.replicate = 1
            _, env = process_spec(e, s, tmp_path, Calibration())
            assert env["MURMUR_RUN_ID"] == opaque_run_id(family, s, 1) == run_id(e, s)
    # the same (family, situation seed, replicate) is the same run id, whichever agent runs it
    assert opaque_run_id("decoy", 1000, 0) == opaque_run_id("decoy", 1000, 0) != opaque_run_id("decoy", 1000, 1)


# ---- the look trigger -------------------------------------------------------------------------------

def intent(iid: int, source: str, kind: str, uids: list[int], dispatched_s: float | None) -> dict[str, Any]:
    hist = [{"state": "pending_approval", "t": 1_000.0}]
    if dispatched_s is not None:
        hist.append({"state": "dispatched", "t": (EPOCH + dispatched_s) * 1000.0})
    return {"id": iid, "source": {"kind": "agent", "id": source}, "kind": kind, "uids": uids, "history": hist}


EPOCH = 1_781_431_200.0
V2_CUT = {"kind": "link_cut", "after_first_look": [5, 90], "delay_s": 12.5, "duration_s": 30.0, "deadline_s": 600.0,
          "look_tasks": ["orbit", "patrol"], "look_uids": PUMAS}
V2_STOP = {"kind": "stop", "after_first_look": [5, 60], "delay_s": 7.0, "deadline_s": 400.0, "fallback_uid": 4,
           "look_tasks": ["orbit", "patrol"], "look_uids": PUMAS}
#: an order stream: the opening orders (agent-setup), a scripted proposal, a model officer's
#: orbit with REAPER 1 (no classifying sensor), its first look with a Puma, a later one
STREAM = [
    intent(1, "agent-setup", "orbit", [4], 3.0),
    intent(2, "agent-scripted", "patrol", [1], 90.0),
    intent(3, "agent-watch", "orbit", [2], 150.0),
    intent(4, "agent-watch", "patrol", [3], 200.0),
    intent(5, "agent-watch", "orbit", [4], 260.0),
    intent(6, "agent-watch", "orbit", [4], None),
]


def test_first_look_counts_any_agent_under_test_never_the_scripted_ones():
    # v2: the model officer's first look flown by a Puma, a patrol counted as a look
    assert first_look(STREAM, V2_CUT) == (EPOCH + 200.0, 4, 3)
    # v1's items (orbits, any vehicle): the model officer's orbit counts too (it once did not)
    v1 = {"kind": "link_cut", "after_first_look": [5, 90], "delay_s": 10.0, "duration_s": 30.0, "deadline_s": 600.0}
    assert first_look(STREAM, v1) == (EPOCH + 150.0, 3, 2)
    # scripted orders alone are no look
    assert first_look(STREAM[:2], V2_CUT) is None and first_look(STREAM[:2], v1) is None
    # the playbook as before
    pb = [dict(i, source={"kind": "agent", "id": "agent-playbook"}) for i in STREAM[2:]]
    assert first_look(pb, v1) == (EPOCH + 150.0, 3, 2)


class FakeSim:
    def __init__(self, intents: list[dict[str, Any]]):
        self.intents = intents
        self.faults: list[dict[str, Any]] = []

    def api(self, method: str, path: str, body: Any = None, operator: str | None = None) -> tuple[int, Any]:
        assert (method, path) == ("GET", "/api/intents")
        return 200, {"intents": self.intents}

    def truth(self, association: bool = True) -> dict[str, Any]:
        return {"ours": [{"uid": u, "mode": "AUTO"} for u in (1, 2, 3, 4)]}

    def fault(self, kind: str, **f: Any) -> dict[str, Any]:
        self.faults.append({"kind": kind, **f})
        return {"ok": True}


class FakeEpisode:
    sim_epoch = EPOCH
    ours_samples: list[dict[str, Any]] = []

    def __init__(self, sim: FakeSim):
        self.sim = sim


def drive(script: Script, until: float) -> None:
    t = script.first()
    while t is not None and t <= until:
        t = script.wake(t)


def test_the_cut_and_the_stop_fire_on_a_non_playbook_agents_first_look():
    sim = FakeSim([])
    sc = Script(FakeEpisode(sim), [V2_CUT])
    drive(sc, 199.0)
    assert not sc.log, "nothing dispatched yet: nothing cut"
    sim.intents = STREAM
    drive(sc, 400.0)
    (cut,) = [r for r in sc.log if r["kind"] == "link_cut"]
    assert cut["uid"] == 3 and cut["t"] == pytest.approx(212.5)
    assert cut["trigger"] == {"intent": 4, "dispatched_s": 200.0, "delay_s": 12.5}
    assert sim.faults[0]["kind"] == "link_event" and sim.faults[0]["event"]["uid"] == 3
    # the deadline went with the trigger: nothing more happens
    drive(sc, 700.0)
    assert len(sc.log) == 1

    sim = FakeSim(STREAM)
    st = Script(FakeEpisode(sim), [V2_STOP])
    drive(st, 700.0)
    (stop,) = st.log
    assert (stop["kind"], stop["uid"], stop["t"]) == ("stop", 3, pytest.approx(207.0))
    assert stop["trigger"]["intent"] == 4 and sim.faults == [{"kind": "stop", "uid": 3}]


def test_no_look_by_the_deadline_cuts_nothing_and_stops_the_fallback():
    sim = FakeSim(STREAM[:3])  # setup, scripted, and an orbit by a vehicle that cannot look
    sc = Script(FakeEpisode(sim), [V2_CUT])
    drive(sc, 700.0)
    assert [r.get("skipped") for r in sc.log] == ["no look to cut: none dispatched by 600 s"] and not sim.faults
    st = Script(FakeEpisode(sim), [V2_STOP])
    drive(st, 700.0)
    (stop,) = st.log
    assert stop["uid"] == 4 and stop["t"] == 400.0 and "fallback" in stop and sim.faults == [{"kind": "stop", "uid": 4}]


# ---- against the simulator --------------------------------------------------------------------------

@pytest.fixture(scope="module")
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


def test_the_misclassification_port_is_the_simulators_draw(binary: None):
    """Drifting craft around PUMA 2's station at a misclassification rate of 0.5: every one
    EO/IR has classified shows the kind the Python draw predicts (its confusion when set)."""
    from eval.headless.episode import Episode, Options

    seed, rate, station = 4242, 0.5, (-122.4680, 37.8240)
    w = water()
    pts = [offset(offset(station, 0, 250 * i), 90, 250 * j) for i in range(-5, 6) for j in range(-5, 6)]
    pts = [p for p in pts if dist_m(p, station) <= 1300 and w.wet(p, 80.0)]
    probes = [{"id": f"probe-{k:02d}", "name": "probe", "affiliation": "neutral", "domain": "surface",
               "kind": "fishing_vessel" if k % 2 == 0 else "sailboat", "motion": "drift", "at": list(p)}
              for k, p in enumerate(pts)]
    with tempfile.TemporaryDirectory() as td:
        e, _ = generate("quiet", 1, Path(td), world="v2")
        path = Path(td) / "probe.toml"
        path.write_text(contacts_toml("probe", seed, {"misclassification_rate": rate}, probes))
        e.duration_s = 240.0
        ep = Episode(e, 1, Path(td), Options(agent="none", calibration=Calibration.load()), contacts_path=path)
        got: dict[str, Any] = {}
        sample = ep.sample

        def sampled(now: float) -> float | None:
            nxt = sample(now)
            if now >= 230.0 and not got:
                got["tracks"] = ep.sim.mcp("agent-playbook", ("agent-playbook", "test"), "get_tracks", {})
                got["truth"] = ep.sim.truth(association=True)
            return nxt

        ep.sample = sampled  # type: ignore[method-assign]
        ep.run()
    body = got["tracks"].get("structured", got["tracks"])
    tracks = body["tracks"] if isinstance(body, dict) else body
    assoc = {a["track_id"]: a["contact_id"] for a in got["truth"]["association"]}
    kinds = {c["id"]: c["kind"] for c in probes}
    compared, wrong, mismatches = 0, 0, []
    for t in tracks:
        cid = assoc.get(t["id"])
        if cid not in kinds or not [s for s in t["sources"] if s["sensor"] == "eo_ir"]:
            continue
        set_ = misclassified(seed, cid, "eo_ir", rate)
        compared += 1
        wrong += set_
        if t["identity"]["kind"] != (CONFUSION[kinds[cid]] if set_ else kinds[cid]):
            mismatches.append((cid, kinds[cid], set_, t["identity"]["kind"]))
    assert compared >= 30 and 10 <= wrong <= compared - 10, (compared, wrong)
    assert not mismatches


def test_a_v2_episode_stops_the_playbooks_first_look_and_names_no_situation(binary: None):
    """One whole v2 episode (S6, the playbook): the stop hits the Puma flying its first look; the
    run id the agent reads is opaque; neither the audit log nor the agent's reads name the
    family, the seed or Harbor watch."""
    from eval.headless.episode import Episode, Options

    with tempfile.TemporaryDirectory() as td:
        e, p = generate("stuck-vehicle", 1000, Path(td), world="v2")
        rec = Episode(e, 1000, Path(td), Options(calibration=Calibration.load()), contacts_path=p).run()
    (stop,) = rec["script"]
    looks = [ln for ln in rec["agent_log"] if ln["kind"] == "proposal" and ln.get("uid") in PUMAS]
    if "trigger" in stop:
        assert stop["uid"] in PUMAS and stop["t"] == pytest.approx(stop["trigger"]["dispatched_s"] + 8.3, abs=0.01)
    else:
        assert stop["uid"] == 4 and "fallback" in stop
    assert looks, "the playbook looked"
    connected = next(ln for ln in rec["agent_log"] if ln["kind"] == "bridge.connected")
    assert re.fullmatch(r"ep-[0-9a-f]{12}", connected["bridge_run"])
    assert connected["client_version"].split(" config ")[1].startswith(f"{V2_CONFIG_NAME}#")
    for text in (rec["audit_text"], json.dumps(rec["agent_log"])):
        for token in ("stuck-vehicle", "harbor-watch", "Harbor watch", "-s1000", "seed 1000", "v2-stuck"):
            assert token not in text, token
