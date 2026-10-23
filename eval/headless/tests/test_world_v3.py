"""World v3's headless pieces (docs/WORLD-V3-PLAN.md section 4; docs/HEADLESS.md, worlds,
faults and timelines), on the real `murmur-headless` binary (built here if stale):

- H5: the Golden Gate's world file holds exactly the constants it replaced; conditions draw
  inside their ranges, the same for the same generator; a world with another ground station gets
  a links file with it; Harbor watch and every family stay as before world v3 (no world key, no
  grid, still water: the golden test holds the episode itself);
- H1: the world's land/water grid answers the bridge's domain rule (a boat's route over land
  fails, over water passes; a UGV's the other way round), and Harbor watch's pin keeps "not
  evaluated";
- H4: each fault from a situation's step, as the stand-ins or ArduPilot behave: a GNSS jammer
  takes the fix (the C2 sees no fix, no position source) and the UGV's EKF failsafe holds it,
  until the fix comes back; a spoofer walks the believed position off the truth with the flags
  good; a battery's step and a new draw; a reboot (silent, its time since boot from 0, the C2
  logs `vehicle.rebooted`, a boat comes back disarmed, an aircraft armed in its mode); the
  world's currents and a current step move the drifter and the drifting contact;
- the timeline: the fault drill's every step runs (the smoke episode), the live runner refuses
  it, the step format is checked.
"""

from __future__ import annotations

import json
import math
import random
import shutil
import subprocess
import tomllib
from pathlib import Path
from typing import Any

import pytest

from eval.agent import _rest_body
from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.sim import BINARY, Sim
from eval.headless.situations import (

    GATE,
    TERRAIN_GRID,
    Calibration,
    EpisodeSpec,
    from_situation,
    links_for,
    process_spec,
)
from eval.situation import SituationError, load_situation, resolve_modes
from eval.world import World, WorldError, load_world

GG = load_world("golden-gate")
UGV, HUNTER, PUMA, BUOY = 4, 1, 2, 3
LAND_ROUTE = [[-122.4500, 37.8058], [-122.4440, 37.8062]]   # Crissy Field
WATER_ROUTE = [[-122.4720, 37.8135], [-122.4720, 37.8250]]  # the channel east of the bridge


@pytest.fixture(scope="module", autouse=True)
def binary() -> Path:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)
    return BINARY


def drill(tmp: Path, seed: int = 1, **over: Any) -> tuple[Sim, EpisodeSpec]:
    """The fault drill's world and fleet, its timeline left out (each test makes its own fault)."""
    e = from_situation("fault-drill")
    e.script = []
    for k, v in over.items():
        setattr(e, k, v)
    spec, env = process_spec(e, seed, tmp, Calibration.load())
    return Sim(spec, env), e


def order(sim: Sim, task: str, uid: int, **kw: Any) -> str:
    """op-1's order (its own submission is its approval in a human tier): the intent's state."""
    status, out = sim.api("POST", "/api/intents", _rest_body({"task": task, "uids": [uid], **kw}, "test order"), "op-1")
    assert status // 100 == 2, out
    return str((out.get("intent") or {}).get("state"))


def vehicle(sim: Sim, uid: int) -> dict[str, Any]:
    return sim.mcp("agent-watch", ("test", "0"), "get_vehicle", {"uid": uid})["structured"]["vehicle"]


def ours(sim: Sim, uid: int) -> dict[str, Any]:
    return next(o for o in sim.truth(association=False)["ours"] if int(o["uid"]) == uid)


def dist_m(a: tuple[float, float], b: tuple[float, float]) -> float:
    lat = math.radians((a[1] + b[1]) / 2)
    return math.hypot((a[0] - b[0]) * 111_320.0 * math.cos(lat), (a[1] - b[1]) * 111_320.0)


def audit(sim: Sim) -> list[dict[str, Any]]:
    end = sim.end()
    return [json.loads(ln) for ln in Path(end["audit"]).read_text().splitlines() if ln.strip()]


# ---- H5: the world -----------------------------------------------------------------------------


def test_the_golden_gate_world_holds_the_constants_it_replaced():
    """Every value moved into sim/worlds/golden-gate.toml is the constant the code held."""
    assert GG.point("bridge") == GATE == (-122.4783, 37.8199)
    assert GG.point("mouth") == (-122.5050, 37.8100) and GG.point("bay_axis") == (-122.4500, 37.8250)
    assert (GG.mission.center, GG.mission.radius_m, GG.mission.grace_s) == ((-122.4783, 37.8199), 3000.0, 60.0)
    assert TERRAIN_GRID == REPO / "sim" / "headless" / "terrain" / "golden-gate.grid.gz"
    assert GG.areas.resolve() == (REPO / "sim" / "areas").resolve()
    assert GG.operating_box == {"lat_min": 37.74, "lat_max": 37.90, "lon_min": -122.57, "lon_max": -122.35}
    links = tomllib.loads((REPO / "sim" / "links.toml").read_text())["ground_station"]
    (gs,) = GG.ground_stations
    assert (gs.lat, gs.lon, gs.alt_m) == (links["lat"], links["lon"], links["alt_m"])
    # its links file is sim/links.toml itself (the space cache's name and the provenance hold)
    assert links_for(GG, Path("/nonexistent")) == REPO / "sim" / "links.toml"
    from eval.headless import families, labels
    from eval.headless.v2 import labels as v2_labels

    assert families.GATE == GATE and labels.PROTECTED == GATE and v2_labels.BRIDGE == GATE
    assert labels.LOOK_RADIUS_M == v2_labels.NEED_RADIUS_M == 3000.0 and labels.LOOK_GRACE_S == 60.0


def test_conditions_draw_inside_their_ranges_and_repeat():
    c = GG.conditions
    a = [GG.draw_conditions(random.Random(f"x:{k}")) for k in range(200)]
    assert a == [GG.draw_conditions(random.Random(f"x:{k}")) for k in range(200)]
    for d in a:
        assert c["visibility_km"][0] <= d["visibility_km"] <= c["visibility_km"][1]
        assert c["wind_mps"][0] <= d["wind_mps"] <= c["wind_mps"][1]
        assert c["sea_state"][0] <= d["sea_state"] <= c["sea_state"][1] and 0 <= d["wind_from_deg"] < 360
        assert c["at"][0] <= d["at"] <= c["at"][1] and d["at"].endswith(":00Z")
    assert len({d["at"] for d in a}) > 150, "the start is drawn across the window"


def test_a_world_with_another_ground_station_gets_its_own_links_file(tmp_path: Path):
    text = (REPO / "sim" / "worlds" / "golden-gate.toml").read_text().replace(
        'name = "golden-gate"', 'name = "elsewhere"').replace("lat = 37.8068", "lat = 37.8100")
    (tmp_path / "elsewhere.toml").write_text(text)
    w = load_world(tmp_path / "elsewhere.toml")
    p = links_for(w, tmp_path)
    assert p.parent == tmp_path
    got = tomllib.loads(p.read_text())
    want = tomllib.loads((REPO / "sim" / "links.toml").read_text())
    assert got["ground_station"] == {**want["ground_station"], "lat": 37.81}
    assert {k: v for k, v in got.items() if k != "ground_station"} == {k: v for k, v in want.items() if k != "ground_station"}
    # two stations: the emulator has one
    two = text + '\n[[ground_station]]\nname = "b"\nlat = 37.8\nlon = -122.4\nalt_m = 1.0\n'
    (tmp_path / "two.toml").write_text(two.replace('name = "elsewhere"', 'name = "two"'))
    with pytest.raises(ValueError, match="one"):
        links_for(load_world(tmp_path / "two.toml"), tmp_path)
    (tmp_path / "bad.toml").write_text(text.replace('name = "elsewhere"', 'name = "bad"').replace("[mission]", "[mission]\nwrong = 1"))
    with pytest.raises(WorldError):
        load_world(tmp_path / "bad.toml")


def test_harbor_watch_and_the_families_are_as_before_world_v3():
    """Harbor watch pins the headless simulator as before world v3, and every family is built on
    it: no world key in the record, no land/water grid for the bridge, no water in the spec, an
    empty script (its two notes are the panel's)."""
    from eval.headless.families import generate

    hw = from_situation("harbor-watch")
    assert (hw.map, hw.domain_rule, hw.currents, hw.script) == ("golden-gate", "not_evaluated", "none", [])
    assert hw.as_before_world_v3() and hw.world_record() is None
    spec, env = process_spec(hw, 1, Path("/tmp/unused"), Calibration())
    assert "water" not in spec and "MURMUR_LAND_WATER_GRID" not in env and "MURMUR_AREAS" not in env
    assert spec["terrain_grid"] == str(TERRAIN_GRID) and spec["links"] == str(REPO / "sim" / "links.toml")
    for fam, world in (("fast-unknown", "v1"), ("stuck-vehicle", "v2"), ("approval-flood", "v1")):
        e, _ = generate(fam, 3, Path("/tmp"), world=world)
        assert e.as_before_world_v3(), (fam, world)


# ---- H1: the land/water grid ------------------------------------------------------------------


def test_the_land_water_grid_answers_the_domain_rule(tmp_path: Path):
    def domain(sim: Sim, uid: int, route: list[list[float]]) -> dict[str, Any]:
        status, out = sim.api("POST", "/api/intents/preview",
                              _rest_body({"task": "patrol", "uids": [uid], "route": route}, "test"), "op-1")
        assert status == 200, out
        return next(c for c in out["checks"] if c["rule"] == "domain")

    sim, _ = drill(tmp_path / "grid")
    try:
        sim.step(10)
        assert domain(sim, HUNTER, LAND_ROUTE)["outcome"] == "fail"
        assert "is not water" in domain(sim, HUNTER, LAND_ROUTE)["detail"]
        assert domain(sim, HUNTER, WATER_ROUTE)["outcome"] == "pass"
        assert "is not land" in domain(sim, UGV, WATER_ROUTE)["detail"]
        assert domain(sim, UGV, LAND_ROUTE)["outcome"] == "pass"
    finally:
        sim.kill()
    # Harbor watch's pin: no terrain source, "not evaluated" as before world v3
    sim, _ = drill(tmp_path / "none", domain_rule="not_evaluated")
    try:
        sim.step(10)
        c = domain(sim, HUNTER, LAND_ROUTE)
        assert c["outcome"] == "not_evaluated" and "MURMUR_TIDEWATER_URL" in c["detail"], c
    finally:
        sim.kill()


# ---- H4: the faults ---------------------------------------------------------------------------


def test_a_gnss_jammer_takes_the_fix_and_the_ugv_failsafe_holds_it(tmp_path: Path):
    """ArduRover's EKF failsafe (failsafe.rs: in a mode that needs a position, HOLD) once the
    jammer leaves fewer than four satellites; the C2 sees no fix and no position source, and the
    fix again after; a vehicle outside the circle keeps its own."""
    sim, _ = drill(tmp_path)
    try:
        sim.step(10)
        assert order(sim, "patrol", UGV, route=LAND_ROUTE) == "dispatched"
        sim.step(40)
        assert ours(sim, UGV)["mode"] == "AUTO" and vehicle(sim, UGV)["nav"]["gps_fix"] == 3
        out = sim.fault("gnss", effect="jam", circle={"lat": 37.8058, "lon": -122.4470, "radius_m": 800}, duration_s=60)
        assert out["effect"] == "jam"
        sim.step(60)
        nav = vehicle(sim, UGV)["nav"]
        assert (nav["gps_fix"], nav["satellites"], nav["ready"], nav["source"]) == (1, 0, False, "none"), nav
        assert ours(sim, UGV)["mode"] == "HOLD"
        assert vehicle(sim, PUMA)["nav"]["gps_fix"] == 3, "outside the circle"
        sim.step(115)
        nav = vehicle(sim, UGV)["nav"]
        assert (nav["gps_fix"], nav["ready"], nav["source"]) == (3, True, "gnss"), nav
        assert ours(sim, UGV)["mode"] == "HOLD", "ArduRover does not leave the failsafe's HOLD by itself"
    finally:
        sim.kill()


def test_a_spoofer_walks_the_position_the_vehicle_believes(tmp_path: Path):
    """The receiver captured: the position HUNTER 1 reports (and believes) walks north of the
    truth at 3 m/s up to its cap, the fix and the flags still good."""
    sim, _ = drill(tmp_path)
    try:
        sim.step(10)
        out = sim.fault("gnss", effect="spoof", uids=[HUNTER], walk_mps=3.0, bearing_deg=0.0, max_offset_m=400.0,
                        duration_s=200)
        assert out["effect"] == "spoof"
        sim.step(70)

        def offset() -> tuple[float, dict[str, Any]]:
            v, t = vehicle(sim, HUNTER), ours(sim, HUNTER)
            return dist_m((v["position"]["lon"], v["position"]["lat"]), (t["lon"], t["lat"])), v

        d, v = offset()
        assert 120.0 <= d <= 220.0, d
        assert (v["nav"]["gps_fix"], v["nav"]["ready"], v["nav"]["source"]) == (3, True, "gnss")
        sim.step(220)
        d, _ = offset()
        assert d <= 430.0, f"capped at 400 m: {d}"
        assert vehicle(sim, BUOY)["nav"]["gps_fix"] == 3
    finally:
        sim.kill()


def test_a_battery_loses_a_share_at_once_and_drains_faster_at_a_new_rate(tmp_path: Path):
    def pct(sim: Sim, uid: int) -> float:
        o = ours(sim, uid)
        return 100.0 * (1.0 - o["battery_consumed_mah"] / o["battery_capacity_mah"])

    sim, _ = drill(tmp_path)
    try:
        sim.step(10)
        p0 = pct(sim, PUMA)
        sim.fault("battery", uid=PUMA, drop_pct=30.0)
        sim.step(12)
        p1 = pct(sim, PUMA)
        assert 29.5 <= p0 - p1 <= 30.5, (p0, p1)
        assert abs(vehicle(sim, PUMA)["battery"]["pct"] - p1) <= 1.5, "the C2 reads the drop"
        sim.step(102)
        d_normal = p1 - pct(sim, PUMA)
        p2 = pct(sim, PUMA)
        sim.fault("battery", uid=PUMA, rate=3.0, duration_s=90.0)
        sim.step(192)
        d_fast = p2 - pct(sim, PUMA)
        assert 2.6 <= d_fast / d_normal <= 3.4, (d_normal, d_fast)
        p3 = pct(sim, PUMA)
        sim.step(282)
        assert 0.8 <= (p3 - pct(sim, PUMA)) / d_normal <= 1.2, "the rate ends with its duration"
    finally:
        sim.kill()


def test_a_reboot_is_silent_then_heard_again_from_zero(tmp_path: Path):
    """The boat's autopilot restarts: nothing heard for its boot, then disarmed in its boot mode
    (ArduRover's MANUAL) with its parameters; the C2 sees its time since boot go back and logs
    `vehicle.rebooted`. The Puma, in the air, comes back armed in its mode (ArduPilot's watchdog
    restore)."""
    sim, _ = drill(tmp_path)
    try:
        sim.step(10)
        assert order(sim, "patrol", HUNTER, route=WATER_ROUTE) == "dispatched"
        sim.step(60)
        assert ours(sim, HUNTER)["armed"] and ours(sim, HUNTER)["mode"] == "AUTO"
        puma_mode = ours(sim, PUMA)["mode"]
        out = sim.fault("reboot", uid=HUNTER, down_s=8.0)
        assert (out["airborne"], out["armed"], out["mode"]) == (False, False, "MANUAL"), out
        heard = vehicle(sim, HUNTER)["link"]["last_rx_t"]
        sim.step(66)
        assert vehicle(sim, HUNTER)["link"]["last_rx_t"] == heard, "silent while it boots"
        sim.step(90)
        v = vehicle(sim, HUNTER)
        assert v["link"]["last_rx_t"] > heard
        assert (v["autopilot"]["mode"], v["autopilot"]["mav_armed"]) == ("MANUAL", False)
        out = sim.fault("reboot", uid=PUMA)
        assert (out["airborne"], out["armed"], out["mode"], out["down_s"]) == (True, True, puma_mode, 3.0), out
        sim.step(140)  # the C2 logs it once a message with the time since boot comes (its plan re-sent)
        assert ours(sim, PUMA)["armed"] and ours(sim, PUMA)["mode"] == puma_mode
        log = audit(sim)
    except BaseException:
        sim.kill()
        raise
    rebooted = [e for e in log if e.get("type") == "vehicle.rebooted"]
    assert sorted(e["uid"] for e in rebooted) == [str(HUNTER), str(PUMA)], rebooted


def test_the_world_currents_move_the_drifter_and_the_drifting_contact(tmp_path: Path):
    """With the world's water the buoy and the boat drifting off Mile Rock move with tidewater's
    current; in still water they stay put (but for the stand-in's and the contact's own jitter);
    a current step sets every drifting contact east."""
    def run(name: str, **over: Any) -> tuple[float, float]:
        sim, _ = drill(tmp_path / name, **over)
        try:
            sim.step(5)
            b0, c0 = ours(sim, BUOY), drifting(sim)
            if name == "step":
                sim.fault("current", east_mps=1.0, north_mps=0.0, duration_s=100.0)
            sim.step(105)
            b1, c1 = ours(sim, BUOY), drifting(sim)
            return dist_m((b0["lon"], b0["lat"]), (b1["lon"], b1["lat"])), (c1[0] - c0[0]) * 111_320.0 * math.cos(math.radians(c0[1]))
        finally:
            sim.kill()

    def drifting(sim: Sim) -> tuple[float, float]:
        c = next(x for x in sim.truth(association=False)["contacts"]["contacts"] if x["id"] == "fishing-milerock")
        return (c["lon"], c["lat"])

    buoy_world, east_world = run("world")
    buoy_still, east_still = run("still", currents="none")
    _, east_step = run("step", currents="none")
    assert buoy_world > 50.0 and buoy_still < 5.0, (buoy_world, buoy_still)
    assert abs(east_world - east_still) > 20.0, (east_world, east_still)
    assert 85.0 <= east_step - east_still <= 115.0, (east_step, east_still)


# ---- the timeline -----------------------------------------------------------------------------


def test_the_fault_drill_runs_every_step(tmp_path: Path):
    """The smoke episode: the fault drill's timeline, every step run without an error, and the
    record says which world and what the simulator took from it."""
    e = from_situation("fault-drill")
    rec = Episode(e, 1, tmp_path / "w", Options(agent="none", calibration=Calibration.load(),
                                              operator={"mode": "approve-all"})).run()
    steps = {s["step"]: s for s in rec["script"] if s.get("kind") == "step"}
    assert set(steps) == {"set-east", "jammer", "cell-lost", "heavy-draw", "spoofer", "reboot", "cut", "reposition",
                          "ugv-stops"}
    assert not [s for s in rec["script"] if s.get("error")], rec["script"]
    assert steps["reboot"]["fault"][0]["mode"] == "MANUAL"
    assert steps["reposition"]["intent"] is not None and steps["reposition"]["refused"] is None
    assert rec["world"] == {"name": "golden-gate", "domain_rule": "grid", "currents": "world", "conditions": None}
    assert '"type":"vehicle.rebooted"' in rec["audit_text"]


def test_a_timeline_engage_step_nominates_and_the_signers_sign(tmp_path: Path):
    """The engagement situation's fallback step (`unless_nominated`, `wait_s`): with no AI, it
    waits for BLADE 1 to pass the nomination form's checks, agent-scripted nominates it, the
    episode's scripted signers (section 16) sign it and it goes out; with a nomination standing
    it is skipped, saying whose."""
    e = from_situation("hostile-act-engagement")
    opts = dict(calibration=Calibration.load(), signers={"preset": "scripted"}, review={"preset": "confirm-declarations"})
    rec = Episode(e, 1, tmp_path / "none", Options(agent="none", **opts)).run()
    (step,) = [s for s in rec["script"] if s.get("step") == "fallback-nomination"]
    assert step["engage"] and step["intent"] is not None and not step.get("skipped") and not step.get("error"), step
    assert step["request"]["extra"]["fallback"] == "return" and step["request"]["extra"]["effect_radius_m"] == 75.0
    assert "situation timeline (scripted, not a person)" in rec["audit_text"]
    signed = [d for d in rec["signers"]["decisions"] if d.get("intent_id", d.get("intent")) == step["intent"]]
    assert signed and signed[-1].get("result") == "dispatched", rec["signers"]["decisions"]
    rec = Episode(e, 1, tmp_path / "scripted", Options(agent="scripted", **opts)).run()
    (step,) = [s for s in rec["script"] if s.get("step") == "fallback-nomination"]
    assert step["skipped"].startswith("a nomination on ") and step.get("intent") is None, step


def test_the_live_runner_refuses_the_simulators_faults():
    sit = load_situation("fault-drill")
    with pytest.raises(SituationError, match="headless simulator"):
        resolve_modes(sit, operator="autoplay", ai="none")
    # Harbor watch and the engagement situation have none
    for name in ("harbor-watch", "hostile-act-engagement"):
        resolve_modes(load_situation(name), operator="autoplay", ai="none")


@pytest.mark.parametrize("step,match", [
    ({"kind": "gnss", "effect": "jam"}, "needs a circle"),
    ({"kind": "gnss", "effect": "fog", "circle": {"lat": 1, "lon": 2, "radius_m": 3}}, "jam or spoof"),
    ({"kind": "gnss", "effect": "jam", "circle": {"lat": 1, "lon": 2, "radius_m": 3}, "walk_mps": 1}, "not jam fields"),
    ({"kind": "battery", "vehicles": {"callsign": "PUMA 1"}}, "drop_pct"),
    ({"kind": "battery", "vehicles": {"callsign": "PUMA 1"}, "drop_pct": 120}, "above"),
    ({"kind": "reboot"}, "needs vehicles"),
    ({"kind": "current", "east_mps": 1}, "east_mps and north_mps"),
    ({"kind": "stop", "vehicles": {"callsign": "PUMA 1"}, "duration_s": 0}, "duration_s"),
])
def test_a_fault_step_is_checked(tmp_path: Path, step: dict[str, Any], match: str):
    text = (REPO / "sim" / "situations" / "fault-drill.toml").read_text()
    body = "\n".join(f"{k} = {json.dumps(v) if not isinstance(v, dict) else _inline(v)}" for k, v in step.items())
    p = tmp_path / "bad.toml"
    p.write_text(text.replace("[[watch]]", f"[[step]]\nid = \"bad\"\nat_s = 10\n{body}\n\n[[watch]]", 1))
    with pytest.raises(SituationError, match=match):
        load_situation(p)


def _inline(d: dict[str, Any]) -> str:
    return "{ " + ", ".join(f"{k} = {json.dumps(v)}" for k, v in d.items()) + " }"


