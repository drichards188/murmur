"""agents/roe.py against the predicates it replaces (docs/SCENARIO-SETTINGS.md section 3), on v0's
`[watch]`, a table of `get_tracks` entries: the scoring's (eval/headless/v3/labels.py `resolved_at`
on the picture, the identification being right; `in_doubt`; `approaches`) and the officer's
approach geometry (agents/watch/stakes.py `Watch.entry`).

Every row states roe's answer, and each predicate of today that answers otherwise is named with
the design's reason; one that is not named must agree. The reasons:

- `CLEAR`: a clear list that names `friendly`, which the bridge never sends, and leaves out
  `assumed_friend`, which it does (section 1's "clear affiliations"); here the scoring's
  `in_doubt`, which clears any affiliation outside its need list.
- `SENSOR`: doubt reads the identification's level and sensor; the scoring's `in_doubt` read its
  confidence and conflict alone (a fishing boat named by sonar at 0.7, shown neutral).

Until stage S5 the officer's own identity and doubt (picture.py `resolved`, its clear list
`CLEAR`, the doubt `Poller.approach` judged) were rows here too, differing by `CLEAR`; by `BOTH`
(it let a contact go shown neutral below the bar, or identified but shown unknown, suspect or
hostile); by `SUPPORT` (an identity that does not say what supports it supported it); and its
`Watch.entry` by `STILL` (it took an aircraft straight in only when its velocity was null, not
when slower than `still_mps`). S5 moved the officer onto roe itself (`Watch.roe`: its doubt, its
horizon, its still and no-velocity speeds), so those differences are gone.
"""

from __future__ import annotations

import math
import tomllib
from typing import Any

import pytest

from eval import REPO
from eval.headless.v2.labels import Params
from eval.headless.v3 import labels as L
from eval.headless.v3.settings import v0
from playbook.config import make
from roe import Roe
from watch.stakes import watch_of

WATCH = tomllib.loads((REPO / "sim" / "policy-v0-watch.toml").read_text())["watch"]
R = Roe(WATCH)
#: the scoring's copy of the same table (eval/headless/v3/settings.py)
S0 = v0("golden-gate-v3")
BRIDGE = (-122.4783, 37.8199)
AREA = {"name": "the Golden Gate Bridge", "center": list(BRIDGE), "radius_m": 3000.0}

CLEAR, SENSOR = "CLEAR", "SENSOR"
MISSING = object()

# (domain, kind, level, confidence, supported_by, conflict, affiliation, true kind or None for the
# shown one), roe's identified and in doubt, and what of today differs and why
ROWS: list[tuple[tuple[Any, ...], bool, bool, dict[str, str]]] = [
    (("surface", "small_fast_boat", "identify", 0.8, ["eo_ir"], None, "suspect", None), True, True, {}),
    (("surface", "small_surface", "detect", 0.93, ["radar"], None, "unknown", "small_fast_boat"), False, True, {}),
    (("surface", "small_craft", "recognise", 0.9, ["eo_ir"], None, "unknown", "fishing_vessel"), False, True, {}),
    (("surface", "fishing_vessel", "identify", 0.3, ["eo_ir"], None, "neutral", None), False, True, {}),
    (("surface", "fishing_vessel", "identify", 0.75, ["eo_ir"], None, "neutral", None), True, False, {}),
    (("surface", "sailboat", "identify", 0.65, ["eo_ir", "ais"], None, "neutral", None), True, False, {}),
    (("surface", "cargo_ship", "identify", 0.85, ["ais"], None, "neutral", None), True, False, {}),
    (("surface", "patrol_boat", "identify", 0.85, ["ais"], None, "assumed_friend", None), True, False, {}),
    # never sent by the bridge: the scoring's need list would clear it
    (("surface", "patrol_boat", "identify", 0.85, ["ais"], None, "friendly", None), True, True,
     {"labels.in_doubt": CLEAR}),
    (("surface", "fishing_vessel", "identify", 0.7, ["sonar"], None, "neutral", None), False, True,
     {"labels.in_doubt": SENSOR}),
    (("surface", "small_fast_boat", "identify", 0.8, ["eo_ir"], "AIS says cargo (MV X) but eo_ir classifies it as "
      "small_fast_boat", "suspect", None), False, True, {}),
    (("surface", "small_fast_boat", "identify", 0.8, MISSING, None, "suspect", None), False, True, {}),
    (("surface", "small_fast_boat", "identify", 0.9, ["eo_ir"], None, "hostile", None), True, True, {}),
    (("surface", None, None, 0.0, [], None, "unknown", "small_fast_boat"), False, True, {}),
    (("air", "small_drone", "identify", 0.7, ["eo_ir"], None, "suspect", None), True, True, {}),
    (("air", "small_air", "detect", 0.99, ["radar"], None, "unknown", "small_drone"), False, True, {}),
    (("air", "light_aircraft", "identify", 0.45, ["eo_ir"], None, "neutral", None), False, True, {}),
    (("air", "helicopter", "identify", 0.8, ["eo_ir"], None, "neutral", None), True, False, {}),
    (("air", "small_drone", "identify", 0.9, ["ais"], None, "suspect", None), False, True, {}),
    (("ground", "vehicle", "identify", 0.6, ["eo_ir"], None, "unknown", None), True, True, {}),
    (("ground", "truck", "identify", 0.59, ["eo_ir"], None, "unknown", None), False, True, {}),
    (("subsurface", "submerged", "recognise", 0.7, ["sonar"], None, "suspect", "uuv"), True, True, {}),
    (("subsurface", "uuv", "identify", 0.8, ["sonar"], None, "suspect", None), True, True, {}),
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
        "labels.resolved": L.resolved_at(OnePicture(track), 0, "C-1", meta, S0),  # type: ignore[arg-type]
        "labels.in_doubt": L.in_doubt(track, S0),
    }


def roe_says(track: dict[str, Any]) -> dict[str, bool]:
    return {"labels.resolved": R.identified(track), "labels.in_doubt": R.in_doubt(track)}


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
    assert used == {CLEAR, SENSOR}


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
    (("air", 5000, 0, (0.2, 0.1), 180), True, {}),
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
    officer = watch_of(make({"watch_area": [AREA]}, "test"), roe=R)
    assert officer is not None and officer.roe is R
    theirs = {"stakes": officer.entry(where, vel, domain, horizon_s=h) is not None,
              "labels": L.approaches({"position": where, "velocity": vel, "last_seen_s": 0.0}, domain, 0.0,
                                     Params(justify_horizon_s=h), BRIDGE, AREA["radius_m"], S0)[0]}
    for name, got in theirs.items():
        assert (got != approaches) == (name in differs), (name, got)
