"""agents/roe.py against the predicates it replaces (docs/SCENARIO-SETTINGS.md section 3), on v0's
`[watch]`, a table of `get_tracks` entries: the officer's (agents/watch/picture.py `resolved`, its
clear list `CLEAR`, and the doubt `Poller.approach` judges, picture.py:296; stakes.py
`Watch.entry`) and the scoring's (eval/headless/v3/labels.py `resolved_at` on the picture, the
identification being right; `in_doubt`; `approaches`).

Every row states roe's answer, and each predicate of today that answers otherwise is named with
the design's reason; one that is not named must agree. The reasons:

- `CLEAR`: the officer's clear list names `friendly`, which the bridge never sends, and leaves out
  `assumed_friend`, which it does (section 1's "clear affiliations").
- `BOTH`: a contact leaves doubt only identified and shown clear (section 3); the officer let one
  go on either: shown neutral below the bar (the tracker's 0.3, section 1), or identified but shown
  unknown, suspect or hostile. The scoring's `in_doubt` already held both.
- `SENSOR`: doubt reads the identification's level and sensor; the scoring's `in_doubt` read its
  confidence and conflict alone (a fishing boat named by sonar at 0.7, shown neutral).
- `SUPPORT`: an identity that does not say what supports it supports nothing (the scoring's
  reading; the officer let it through).
- `STILL`: a shown speed under `still_mps` is no velocity (the scoring's rule); the officer took
  an aircraft straight in only when its velocity was null.
"""

from __future__ import annotations

import math
import tomllib
from typing import Any

import pytest

from eval import REPO
from eval.headless.v2.labels import Params
from eval.headless.v3 import labels as L
from playbook.config import make
from roe import CLEAR as ROE_CLEAR
from roe import Roe
from watch import picture as P
from watch.stakes import watch_of

WATCH = tomllib.loads((REPO / "sim" / "policy-v0-watch.toml").read_text())["watch"]
R = Roe(WATCH)
BRIDGE = (-122.4783, 37.8199)
AREA = {"name": "the Golden Gate Bridge", "center": list(BRIDGE), "radius_m": 3000.0}

CLEAR, BOTH, SENSOR, SUPPORT, STILL = "CLEAR", "BOTH", "SENSOR", "SUPPORT", "STILL"
MISSING = object()

# (domain, kind, level, confidence, supported_by, conflict, affiliation, true kind or None for the
# shown one), roe's identified and in doubt, and what of today differs and why
ROWS: list[tuple[tuple[Any, ...], bool, bool, dict[str, str]]] = [
    (("surface", "small_fast_boat", "identify", 0.8, ["eo_ir"], None, "suspect", None), True, True,
     {"picture.doubt": BOTH}),
    (("surface", "small_surface", "detect", 0.93, ["radar"], None, "unknown", "small_fast_boat"), False, True, {}),
    (("surface", "small_craft", "recognise", 0.9, ["eo_ir"], None, "unknown", "fishing_vessel"), False, True, {}),
    (("surface", "fishing_vessel", "identify", 0.3, ["eo_ir"], None, "neutral", None), False, True,
     {"picture.doubt": BOTH}),
    (("surface", "fishing_vessel", "identify", 0.75, ["eo_ir"], None, "neutral", None), True, False, {}),
    (("surface", "sailboat", "identify", 0.65, ["eo_ir", "ais"], None, "neutral", None), True, False, {}),
    (("surface", "cargo_ship", "identify", 0.85, ["ais"], None, "neutral", None), True, False, {}),
    (("surface", "patrol_boat", "identify", 0.85, ["ais"], None, "assumed_friend", None), True, False,
     {"picture.clear": CLEAR}),
    # never sent by the bridge: the officer's list would clear it
    (("surface", "patrol_boat", "identify", 0.85, ["ais"], None, "friendly", None), True, True,
     {"picture.clear": CLEAR, "picture.doubt": CLEAR, "labels.in_doubt": CLEAR}),
    (("surface", "fishing_vessel", "identify", 0.7, ["sonar"], None, "neutral", None), False, True,
     {"picture.doubt": BOTH, "labels.in_doubt": SENSOR}),
    (("surface", "small_fast_boat", "identify", 0.8, ["eo_ir"], "AIS says cargo (MV X) but eo_ir classifies it as "
      "small_fast_boat", "suspect", None), False, True, {}),
    (("surface", "small_fast_boat", "identify", 0.8, MISSING, None, "suspect", None), False, True,
     {"picture.resolved": SUPPORT, "picture.doubt": SUPPORT}),
    (("surface", "small_fast_boat", "identify", 0.9, ["eo_ir"], None, "hostile", None), True, True,
     {"picture.doubt": BOTH}),
    (("surface", None, None, 0.0, [], None, "unknown", "small_fast_boat"), False, True, {}),
    (("air", "small_drone", "identify", 0.7, ["eo_ir"], None, "suspect", None), True, True, {"picture.doubt": BOTH}),
    (("air", "small_air", "detect", 0.99, ["radar"], None, "unknown", "small_drone"), False, True, {}),
    (("air", "light_aircraft", "identify", 0.45, ["eo_ir"], None, "neutral", None), False, True,
     {"picture.doubt": BOTH}),
    (("air", "helicopter", "identify", 0.8, ["eo_ir"], None, "neutral", None), True, False, {}),
    (("air", "small_drone", "identify", 0.9, ["ais"], None, "suspect", None), False, True, {}),
    (("ground", "vehicle", "identify", 0.6, ["eo_ir"], None, "unknown", None), True, True, {"picture.doubt": BOTH}),
    (("ground", "truck", "identify", 0.59, ["eo_ir"], None, "unknown", None), False, True, {}),
    (("subsurface", "submerged", "recognise", 0.7, ["sonar"], None, "suspect", "uuv"), True, True,
     {"picture.doubt": BOTH}),
    (("subsurface", "uuv", "identify", 0.8, ["sonar"], None, "suspect", None), True, True, {"picture.doubt": BOTH}),
    (("subsurface", "uuv", "identify", 0.8, ["eo_ir"], None, "suspect", None), False, True, {}),
]


def entry(row: tuple[Any, ...]) -> tuple[dict[str, Any], dict[str, Any]]:
    """The `get_tracks` entry and the contact's truth (the scoring's `meta`)."""
    domain, kind, level, conf, sensors, conflict, aff, true_kind = row
    ident: dict[str, Any] = {"kind": kind, "level": level, "confidence": conf, "conflict": conflict}
    if sensors is not MISSING:
        ident["supported_by"] = sensors
    track = {"id": "T-1", "domain": domain, "affiliation": aff, "identity": ident}
    meta = {"kind": true_kind or kind, "domain": domain,
            "ais": sensors is not MISSING and "ais" in sensors}  # AIS shown is its own, truly broadcast
    return track, meta


class OnePicture:
    """The one track `resolved_at` reads, as the record's picture gives it."""

    def __init__(self, track: dict[str, Any]):
        self.t = track

    def track_of(self, cid: str, k: int | None) -> str:
        return "T-1"

    def identity(self, tid: str, k: int | None) -> dict[str, Any]:
        return self.t["identity"]


def today(track: dict[str, Any], meta: dict[str, Any]) -> dict[str, bool]:
    """Each predicate of today on the row, keyed as the rows name them, beside what roe answers."""
    return {
        "picture.resolved": P.resolved(track),
        "labels.resolved": L.resolved_at(OnePicture(track), 0, "C-1", meta),  # type: ignore[arg-type]
        "labels.in_doubt": L.in_doubt(track),
        # Poller.approach (picture.py:296)
        "picture.doubt": track.get("affiliation") not in P.CLEAR and not P.resolved(track),
        "picture.clear": track.get("affiliation") in P.CLEAR,
    }


def roe_says(track: dict[str, Any]) -> dict[str, bool]:
    ident, doubt = R.identified(track), R.in_doubt(track)
    return {"picture.resolved": ident, "labels.resolved": ident, "labels.in_doubt": doubt, "picture.doubt": doubt,
            "picture.clear": track.get("affiliation") in ROE_CLEAR}


@pytest.mark.parametrize("row,identified,in_doubt,differs", ROWS, ids=[f"{r[0][0]}-{r[0][1]}-{r[0][6]}-{i}"
                                                                     for i, r in enumerate(ROWS)])
def test_roe_agrees_with_today_but_where_the_design_says(row, identified, in_doubt, differs):
    track, meta = entry(row)
    assert (R.identified(track), R.in_doubt(track)) == (identified, in_doubt)
    ours, theirs = roe_says(track), today(track, meta)
    for name in ours:
        if name in differs:
            assert theirs[name] != ours[name], f"{name} no longer differs ({differs[name]}): drop it from the row"
        else:
            assert theirs[name] == ours[name], f"{name} disagrees with roe and the row gives no reason"


def test_every_reason_is_used():
    used = {why for _, _, _, d in ROWS for why in d.values()}
    assert used == {CLEAR, BOTH, SENSOR, SUPPORT}


def east(m: float, north: float = 0.0) -> tuple[float, float]:
    return (BRIDGE[0] + m / (111_320.0 * math.cos(math.radians(BRIDGE[1]))), BRIDGE[1] + north / 111_320.0)


# (domain, metres east, metres north, velocity, horizon), whether it approaches, what differs
APPROACHES = [
    (("surface", 4000, 0, (-10.0, 0.0), 300), True, {}),
    (("surface", 4000, 0, (-10.0, 0.0), 90), False, {}),
    (("surface", 6500, 0, (-10.0, 0.0), 300), False, {}),
    (("surface", 4000, 0, (0.0, 10.0), 300), False, {}),
    (("surface", 4000, 0, None, 300), False, {}),
    (("surface", 2000, 0, None, 300), True, {}),
    (("air", 5000, 0, None, 180), True, {}),
    (("air", 5000, 0, None, 60), False, {}),
    (("air", 11000, 0, None, 300), False, {}),
    (("air", 5000, 0, (20.0, 0.0), 300), False, {}),
    (("air", 5000, 0, (0.2, 0.1), 180), True, {"stakes": STILL}),
    (("ground", 3500, 3500, (-5.0, -5.0), 300), True, {}),
    (("subsurface", 0, -3500, (0.0, 1.0), 300), False, {}),
    (("subsurface", 0, -3500, (0.0, 1.0), 600), True, {}),
]


@pytest.mark.parametrize("case,approaches,differs", APPROACHES)
def test_approaching_agrees_with_the_officer_and_the_scoring(case, approaches, differs):
    domain, e, n, vel, h = case
    where = east(e, n)
    track = {"domain": domain, "position": list(where), "velocity_mps": list(vel) if vel else None,
             "speed_mps": math.hypot(*vel) if vel else 0.0, "course_deg": None, "last_seen_t": 0.0}
    if vel:
        track["course_deg"] = math.degrees(math.atan2(vel[0], vel[1])) % 360
    assert R.approaches(track, AREA, h) is approaches
    officer = watch_of(make({"watch_area": [AREA]}, "test"))
    assert officer is not None
    theirs = {"stakes": officer.entry(where, vel, domain, horizon_s=h) is not None,
              "labels": L.approaches({"position": where, "velocity": vel, "last_seen_s": 0.0}, domain, 0.0,
                                     Params(justify_horizon_s=h), BRIDGE, AREA["radius_m"])[0]}
    for name, got in theirs.items():
        assert (got != approaches) == (name in differs), (name, got)
