"""Scoring v3's families F7-F13 (eval/headless/v3/families_f7_f13.py; eval/SCORING-V3.md 6.1) on both
worlds: their draws keep every element to its medium on the world's own baked grids, bring every
need within the mission's circle in time to be judged, make the look-alikes truth neutral, change the
conditions only where the family says so and record it, and script their faults and link events as
stated; and one short episode per family and world on the real `murmur-headless` binary (built here
if stale) with the basic v3 oracle, labelled with labels v3, its faults and link events taking
effect."""

from __future__ import annotations

import math
import shutil
import subprocess
import tomllib
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.situations import Calibration, at_unix, run_id
from eval.headless.v3 import families as F
from eval.headless.v3 import families_f7_f13 as X
from eval.headless.v3.labels import episode_labels
from eval.headless.v3.mission import DOMAINS, mission_text
from eval.world import load_world

SEEDS = range(5300, 5316)
NAMES = [n for n in F.all_names() if F.parse(n)[0] in X.GENERATORS]
GRACE = {k: v.grace_s for k, v in DOMAINS.items()}


def contacts(path: Path) -> dict[str, dict[str, Any]]:
    return {c["id"]: c for c in tomllib.loads(path.read_text())["contact"]}


def in_medium(world: str, c: dict[str, Any]) -> bool:
    route = [tuple(p) for p in c["route"]]
    if c["domain"] == "surface":
        return F.wet(world, route, 2.0)
    if c["domain"] == "subsurface":
        return F.wet(world, route, -float(c["alt_m"]) + 1.0)
    if c["domain"] == "ground":
        return F.dry(world, route)
    return float(c["alt_m"]) >= F.top_surface(world, route, 100.0) + 50.0


def test_the_new_families_register_beside_the_others():
    """F1-F18 are all named: F7-F13 on both maps through the extension hook, beside F1-F6, F18 and
    F14-F17 (F17 on the scale world only)."""
    every = F.all_names()
    assert len(NAMES) == 14 and set(NAMES) < set(every) and len(every) == len(set(every))
    numbers = sorted({int(F.parse(n)[0].split("-")[0][1:]) for n in every})
    assert numbers == list(range(1, 19)), numbers
    for n in NAMES:
        fam, world = F.parse(n)
        assert F.family_name(fam, world) == n and F.family_domain(fam)
    with pytest.raises(ValueError):
        F.parse("v3-f9-night-gg")


@pytest.mark.parametrize("name", NAMES)
def test_every_family_draws_its_elements_in_their_media_and_its_needs_in_time(name: str, tmp_path: Path):
    fam, world = F.parse(name)
    w = load_world(world)
    for seed in SEEDS:
        e, path = F.generate(name, seed, tmp_path)
        assert e.brief == mission_text(world) and e.opaque and e.map == world and e.world == "v3"
        assert run_id(e, 123).startswith("ep-") and e.family == name and e.situation_seed == seed
        assert e.truth["contacts_seed"] == tomllib.loads(path.read_text())["seed"]
        cs = contacts(path)
        assert e.truth["added"] and all(cid in cs for cid in e.truth["added"])
        for cid in e.truth["added"]:
            assert in_medium(world, cs[cid]), (name, seed, cid)
        needs = e.truth["elements"]
        assert needs and len({n["contact"] for n in needs}) == len(needs)
        for n in needs:
            c = cs[n["contact"]]
            assert c["affiliation"] in ("unknown", "suspect")
            k = F.first_within([tuple(p) for p in c["route"]], w.mission.center, w.mission.radius_m)
            assert k is not None, "every need comes within the circle"
            entry = float(c["start_s"]) + F.time_to(k, float(c["speed_mps"]))
            assert entry == pytest.approx(n["entry_s"], abs=0.2)
            assert entry + GRACE[c["domain"]] <= e.duration_s - 10.0, "judged inside the episode"
            if c["domain"] == "surface" and c.get("speed_mps", 0) and n.get("lead_s") is not None:
                assert F.time_to(k, float(c["speed_mps"])) + GRACE["surface"] < 180.0, "no buoy identifies it in time"
        for dcy in e.truth.get("decoys") or []:
            assert cs[dcy["contact"]]["affiliation"] == "neutral"


@pytest.mark.parametrize("world", ["gg", "sd"])
def test_f7_brings_three_domains_in_together(world: str, tmp_path: Path):
    for seed in SEEDS:
        e, _ = F.generate(f"v3-f7-three-at-once-{world}", seed, tmp_path)
        els = e.truth["elements"]
        assert sorted(x["domain"] for x in els) == ["air", "subsurface", "surface"]
        t = [x["entry_s"] for x in els]
        assert max(t) - min(t) <= 240.0 + 1.0 and e.duration_s == 1200.0


@pytest.mark.parametrize("world", ["gg", "sd"])
def test_f8_has_a_neutral_look_alike_in_every_domain_and_a_spoofer_on_the_surface(world: str, tmp_path: Path):
    spoofers = 0
    for seed in range(5300, 5340):
        e, path = F.generate(f"v3-f8-decoys-{world}", seed, tmp_path)
        cs = contacts(path)
        decoys = {d["contact"]: cs[d["contact"]] for d in e.truth["decoys"]}
        assert sorted(c["domain"] for c in decoys.values()) == ["air", "ground", "subsurface", "surface", "surface"]
        assert "ais" in decoys["decoy-ais"] and "ais" not in decoys["decoy-surface"]
        if e.truth["spoofer"]:
            spoofers += 1
            boat = cs["boat-1"]
            assert boat["ais"]["spoofed"] is True and boat["ais"]["ship_type"] == "fishing"
            assert boat["affiliation"] == "suspect" and float(boat["speed_mps"]) <= X.F8_SPOOFER_SPEED[1]
    assert 3 <= spoofers <= 20  # a quarter of 40, give or take


@pytest.mark.parametrize("world", ["gg", "sd"])
def test_f9_draws_night_twilight_and_fog_and_records_them(world: str, tmp_path: Path):
    w = load_world(f"{'golden-gate-v3' if world == 'gg' else 'san-diego'}")
    lights = []
    for seed in range(5300, 5340):
        e, _ = F.generate(f"v3-f9-night-fog-{world}", seed, tmp_path)
        c = e.conditions
        light = c["light_by_family"]
        lights.append(light)
        if light == "day":
            assert c["visibility_by_family"] and c["visibility_km"] <= X.F9_FOG_KM_TOP
        if light == "twilight":
            el = X.sun_elevation_deg(at_unix(e.at) + e.truth["elements"][0]["entry_s"], w.mission.center[1],
                                     w.mission.center[0])
            assert X.F9_TWILIGHT_SUN_DEG[0] - 0.5 <= el <= X.F9_TWILIGHT_SUN_DEG[1] + 0.5
        if light == "night":
            h = int(c["local_time"][:2])
            assert h >= 21 or h < 5
    assert {"night", "twilight", "day"} <= set(lights)


def test_the_sun_is_up_at_noon_and_down_at_midnight_in_june():
    lon, lat = load_world("golden-gate-v3").mission.center
    assert X.sun_elevation_deg(at_unix("2026-06-14T20:00:00Z"), lat, lon) > 60.0  # 13:00 PDT
    assert X.sun_elevation_deg(at_unix("2026-06-14T08:00:00Z"), lat, lon) < -15.0  # 01:00 PDT


@pytest.mark.parametrize("world", ["gg", "sd"])
def test_f10_draws_the_sea_toward_its_top_and_slows_the_boats(world: str, tmp_path: Path):
    wn = "golden-gate-v3" if world == "gg" else "san-diego"
    lo, hi = load_world(wn).conditions["sea_state"]
    for seed in SEEDS:
        e, path = F.generate(f"v3-f10-sea-state-{world}", seed, tmp_path)
        sea = e.conditions["sea_state"]
        assert hi - 2 <= sea <= hi and e.conditions["sea_by_family"]
        for n in e.truth["elements"]:
            assert float(contacts(path)[n["contact"]]["speed_mps"]) <= X.F10_TOP_SPEED[sea] + 1e-6


@pytest.mark.parametrize("world", ["gg", "sd"])
def test_f11_and_f12_script_their_trouble_over_the_need(world: str, tmp_path: Path):
    for seed in SEEDS:
        e, path = F.generate(f"v3-f11-link-trouble-{world}", seed, tmp_path)
        tr = e.truth["trouble"]
        assert e.events and all(ev["circle"] == tr["circle"] for ev in e.events)
        if tr["kind"] == "rf_jammer":
            assert {ev.get("class") or ev.get("profile") for ev in e.events} == {"los", "mesh", "satcom", "satcom_leo"}
            assert next(ev for ev in e.events if ev.get("profile") == "satcom_leo")["loss"] == 0.0, "latency, not a jam"
            assert tr["from_s"] < tr["need_in_s"] < tr["from_s"] + tr["duration_s"]
        else:
            assert e.events[0]["class"] == "acoustic" and e.events[0]["cut"] and e.truth["elements"][0]["domain"] == "subsurface"
        e, _ = F.generate(f"v3-f12-gnss-{world}", seed, tmp_path)
        g = e.truth["gnss"]
        st = [s for s in e.script if s.get("kind") == "step"]
        assert len(st) == 1 and st[0]["step"] == "gnss" and st[0]["params"]["effect"] == g["kind"]
        if g["kind"] == "jam":
            c = g["circle"]
            centre = (c["lon"], c["lat"])
            assert F.dist_m(centre, load_world(e.map).mission.center) <= 3000.0 + 1.0
        else:
            assert st[0]["params"]["vehicles"] == {"callsign": g["callsign"]}


@pytest.mark.parametrize("world", ["gg", "sd"])
def test_f13_spreads_its_faults_across_domains_and_vehicles(world: str, tmp_path: Path):
    wn = "golden-gate-v3" if world == "gg" else "san-diego"
    cat = X.by_callsign(wn)
    for seed in SEEDS:
        e, _ = F.generate(f"v3-f13-vehicle-faults-{world}", seed, tmp_path)
        faults = e.truth["faults"]
        kinds = [f["kind"] for f in faults]
        assert kinds[:2] == ["breakdown", "battery"]
        stop = next(s for s in e.script if s.get("kind") == "stop")
        assert stop["after_first_look"] and stop["fallback_uid"] == cat[faults[0]["fallback"]]["uid"]
        who = {f.get("fallback") or f.get("callsign") for f in faults}
        assert len(who) == len(faults), "no vehicle takes two faults"
        bat = faults[1]
        assert cat[bat["callsign"]]["spec"]["domain"] in ("air", "ground")
        assert 20.0 < bat["planned_left_pct"] <= 35.0 and bat["planned_crossing_s"] < e.duration_s
        # version 2: a prompt rtl can bring it home before the reserve
        assert bat["planned_crossing_s"] - bat["at_s"] >= X.F13_SLACK[0] * bat["home_s"] - 0.2
        if "reboot" in kinds:
            assert cat[faults[2]["callsign"]]["spec"]["domain"] == "surface"


def test_the_families_are_seeded_and_share_the_world_s_conditions(tmp_path: Path):
    a, _ = F.generate("v3-f13-vehicle-faults-sd", 5303, tmp_path / "a")
    b, _ = F.generate("v3-f13-vehicle-faults-sd", 5303, tmp_path / "b")
    c, _ = F.generate("v3-f7-three-at-once-sd", 5303, tmp_path / "c")
    first, _ = F.generate("v3-f1-fast-boat-sd", 5303, tmp_path / "d")
    assert a.truth == b.truth and a.script == b.script
    assert c.conditions == first.conditions == a.conditions, "unchanged where the family changes nothing"


# ---- one short episode per family and world -------------------------------------------------------

@pytest.fixture(scope="module")
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


def test_the_types_able_to_answer_a_need_come_from_the_sensing_model(binary: None):
    """eval/headless/v3/able.py: a UUV is a sonar's (REMUS, Sea Hunter's hull sonar), never an
    aircraft's; a boat by day is answered by many types; a drone at night by the thermal cameras."""
    from eval.headless.v3.able import able

    def rec(at: str) -> dict[str, Any]:
        return {"world": {"name": "golden-gate-v3", "conditions": {"at": at, "visibility_km": 20.0, "sea_state": 2}},
                "truth": {"stated": {"elements": [{"contact": "uuv-1", "depth_m": 8.0, "speed_mps": 1.5},
                                                  {"contact": "boat-1", "speed_mps": 14.0},
                                                  {"contact": "drone-1", "alt_m": 120.0, "speed_mps": 18.0}]}}}

    needs = [{"contact": "uuv-1", "domain": "subsurface", "kind": "uuv", "t_need": 300.0},
             {"contact": "boat-1", "domain": "surface", "kind": "small_fast_boat", "t_need": 300.0},
             {"contact": "drone-1", "domain": "air", "kind": "small_drone", "t_need": 300.0}]
    day = able(rec("2026-06-14T19:00:00Z"), needs)  # local noon
    assert {"remus", "seahunter"} <= set(day["uuv-1"]) and not {"puma", "x10", "lta"} & set(day["uuv-1"])
    assert len(day["boat-1"]) >= 4 and "puma" in day["boat-1"]
    night = able(rec("2026-06-14T08:00:00Z"), needs)  # 01:00 local
    assert set(night["drone-1"]) <= {"puma", "fvr90", "lta", "x10", "corsair", "seahunter", "ugv_recon"}
    assert "saildrone" not in night["boat-1"], "a day camera ring sees nothing at night"


@pytest.mark.parametrize("name", NAMES)
def test_a_short_episode_per_family_and_world(name: str, tmp_path: Path, binary: None):
    fam, world = F.parse(name)
    e, path = F.generate(name, 5300, tmp_path)
    # long enough for the first need to be judged
    first = min(n["entry_s"] + GRACE[n["domain"]] for n in e.truth["elements"])
    e.duration_s = min(e.duration_s, max(math.ceil(first) + 20.0, 240.0))
    opts = Options(agent="oracle-v3", operator={"mode": "approve-all"}, calibration=Calibration.load(), scoring="v3",
                   replicate={"r": 0, "seed": 5300, "situation_seed": 5300})
    rec = Episode(e, 5300, tmp_path / "ep", opts, contacts_path=path).run()
    assert rec["scoring"] == "v3" and rec["episode_class"] == "scored" and rec["world"]["name"] == world
    detail = rec["agent_detail"]
    assert detail["agent"] == "oracle-v3" and detail["errors"] == 0, rec["agent_log"][-3:]
    lab = episode_labels(rec)
    judged = {n["contact"] for n in lab["needs"] if n["evaluable"]}
    assert judged and judged <= set(e.truth["needs"]), "only the family's needs, and at least one judged"
    due = {n["contact"] for n in e.truth["elements"] if n["entry_s"] < e.duration_s - 2.0}
    assert due <= set(detail["needs"]), "the oracle knows every need that comes in the (shortened) episode"
    log = rec.get("script") or []
    for s in log:
        assert not s.get("error"), s
        if s.get("kind") == "step":
            f = s["fault"]
            assert all(x.get("ok") for x in (f if isinstance(f, list) else [f])), s
    due = [s for s in e.script if s.get("kind") == "step" and float(s["at_s"]) < e.duration_s - 1.0]
    assert len([s for s in log if s.get("kind") == "step"]) == len(due), "every fault due ran"
    trouble = e.truth.get("trouble") or {}
    if trouble.get("kind") == "acoustic_outage" and trouble["from_s"] + 2.0 < e.duration_s:
        uids = [str(v["uid"]) for v in rec["v3"]["picture"]["fleet"] if v["platform"] == "remus"]
        rows = rec["v3"]["picture"]["ours"][uids[0]]
        assert any(r[8] == "cut" for r in rows), "the acoustic channel closed on the AUV"
