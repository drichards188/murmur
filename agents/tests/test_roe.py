"""The scenario's watch rules as the agents read them (agents/roe.py, docs/SCENARIO-SETTINGS.md
section 3): identified by the domain's level, sensor, the bar and no conflict; in doubt unless
identified and shown neutral, assumed friend or friend; approaching the circle within the horizon,
one shown without velocity straight in at its domain's speed. On v0's `[watch]`
(sim/policy-v0-watch.toml) and a tailored one; tracks as MCP `get_tracks` gives them."""

from __future__ import annotations

import math
import tomllib
from pathlib import Path

from roe import CLEAR, Roe

V0_WATCH = tomllib.loads((Path(__file__).resolve().parents[2] / "sim" / "policy-v0-watch.toml").read_text())["watch"]
BRIDGE = (-122.4783, 37.8199)
AREA = {"name": "the Golden Gate Bridge", "center": list(BRIDGE), "radius_m": 3000.0}


def roe(**kw) -> Roe:
    return Roe({**V0_WATCH, **kw})


def at(east_m: float, north_m: float = 0.0) -> list[float]:
    return [BRIDGE[0] + east_m / (111_320.0 * math.cos(math.radians(BRIDGE[1]))), BRIDGE[1] + north_m / 111_320.0]


def track(domain="surface", kind="small_fast_boat", level="identify", conf=0.8, sensors=("eo_ir",), conflict=None,
          affiliation="suspect", east_m=0.0, north_m=0.0, vel=None, **kw) -> dict:
    t = {"id": "T-1", "domain": domain, "affiliation": affiliation, "position": at(east_m, north_m),
         "velocity_mps": list(vel) if vel is not None else None,
         "speed_mps": round(math.hypot(*vel), 1) if vel is not None else 0.0,
         "course_deg": round(math.degrees(math.atan2(vel[0], vel[1])) % 360) if vel is not None else None,
         "identity": {"kind": kind, "level": level, "confidence": conf, "conflict": conflict,
                      "supported_by": list(sensors)}, "last_seen_t": 100.0}
    t.update(kw)
    return t


def test_criteria_are_the_watch_s():
    r = roe()
    assert r.criteria("surface").sensors == ("eo_ir", "ais") and r.criteria("air").grace_s == 30.0
    assert r.criteria("subsurface").level == "recognise"
    assert r.criteria("space") is None and r.criteria(None) is None


def test_identified_needs_the_level_the_sensor_the_bar_and_no_conflict():
    r = roe()
    assert r.identified(track())
    assert r.identified(track(conf=0.6))  # the bar itself counts
    assert not r.identified(track(conf=0.59))
    # a confident size class is a detection (bd murmur-p9lt's fast boat)
    assert not r.identified(track(kind="small_surface", level="detect", conf=0.93, sensors=("radar",)))
    assert not r.identified(track(kind="small_craft", level="recognise", conf=0.9))
    assert not r.identified(track(conflict="eo_ir calls it sailboat while the estimate is small_fast_boat"))
    # AIS counts on the surface only; a camera never under the water
    assert r.identified(track(kind="cargo_ship", sensors=("ais",), conf=0.85, affiliation="neutral"))
    assert not r.identified(track(domain="air", kind="small_drone", sensors=("ais",)))
    assert not r.identified(track(kind="fishing_vessel", sensors=("sonar",)))
    assert r.identified(track(domain="subsurface", kind="submerged", level="recognise", sensors=("sonar",)))
    assert r.identified(track(domain="subsurface", kind="uuv", level="identify", sensors=("sonar",)))
    assert not r.identified(track(domain="subsurface", kind="uuv", sensors=("eo_ir",)))
    # what the entry does not say supports nothing; a domain the watch does not name, nothing
    t = track()
    del t["identity"]["supported_by"]
    assert not r.identified(t)
    assert not r.identified(track(domain="space"))
    assert not r.identified(track(conf=None)) and not r.identified({"domain": "surface"})
    # the scenario's bar
    assert not roe(bar=0.77).identified(track(conf=0.76)) and roe(bar=0.77).identified(track(conf=0.77))


def test_a_contact_leaves_doubt_only_identified_and_shown_clear():
    r = roe()
    assert CLEAR == ("neutral", "assumed_friend", "friend")
    assert not r.in_doubt(track(kind="patrol_boat", sensors=("ais",), conf=0.85, affiliation="assumed_friend"))
    assert not r.in_doubt(track(kind="fishing_vessel", conf=0.75, affiliation="neutral"))
    # the tracker's neutral at 0.3, below the bar (section 1)
    assert r.in_doubt(track(kind="fishing_vessel", conf=0.3, affiliation="neutral"))
    # identified, but shown suspect or hostile
    assert r.in_doubt(track(affiliation="suspect")) and r.in_doubt(track(affiliation="hostile"))
    # "friendly" is no affiliation the bridge sends
    assert r.in_doubt(track(kind="patrol_boat", sensors=("ais",), conf=0.85, affiliation="friendly"))


def test_approaching_the_circle_within_the_horizon():
    r = roe()
    assert r.entry(track(east_m=2000.0), AREA) == (0.0, "no velocity: judged where it is")
    assert not r.approaches(track(east_m=4000.0), AREA)  # a boat with no velocity is judged where it is
    # 4 km east, west at 10 m/s: in after about 100 s
    t, basis = r.entry(track(east_m=4000.0, vel=(-10.0, 0.0)), AREA)
    assert basis == "its velocity" and 95 < t < 105
    assert not r.approaches(track(east_m=4000.0, vel=(-10.0, 0.0)), AREA, horizon_s=90)
    assert not r.approaches(track(east_m=4000.0, vel=(0.0, 10.0)), AREA)  # passing 4 km off
    assert not r.approaches(track(east_m=4000.0, vel=(10.0, 0.0)), AREA)  # opening
    # an aircraft without velocity comes straight in at 25 m/s: 2 km in 80 s; 8 km takes 320 s
    t, basis = r.entry(track(domain="air", kind="small_drone", east_m=5000.0), AREA)
    assert basis == "no velocity: at 25 m/s straight in" and abs(t - 80.0) < 1
    assert not r.approaches(track(domain="air", kind="small_drone", east_m=11000.0), AREA)
    assert r.approaches(track(domain="air", kind="small_drone", east_m=11000.0), AREA, horizon_s=330)
    # shown slower than still_mps is no velocity
    assert r.approaches(track(domain="air", east_m=5000.0, vel=(0.2, 0.1)), AREA)
    assert not roe(still_mps=0.1).approaches(track(domain="air", east_m=5000.0, vel=(0.2, 0.1)), AREA)
    # the scenario's horizon and speeds
    assert not roe(approach_horizon_s=60).approaches(track(domain="air", east_m=5000.0), AREA)
    assert roe(no_velocity_mps={"air": 25.0, "surface": 10.0}).approaches(track(east_m=4000.0), AREA)
    # carried from its last fix to `now` along its velocity: 4 km east at -10 m/s, 150 s later inside
    moving = track(east_m=4000.0, vel=(-10.0, 0.0))
    assert r.entry(moving, AREA, now=250.0) == (0.0, "its velocity")
    assert r.entry({**moving, "predicted": at(1000.0)}, AREA) == (0.0, "its velocity")
    assert r.entry({"domain": "surface"}, AREA) is None
