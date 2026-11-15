"""A track with no velocity yet is shown as such (bd murmur-jhgn): the feed's speed is 0.0 then."""

from __future__ import annotations

import math

BRIDGE = (-122.4783, 37.8199)


def at(east_m: float, north_m: float) -> list[float]:
    return [BRIDGE[0] + east_m / (111_320.0 * math.cos(math.radians(BRIDGE[1]))), BRIDGE[1] + north_m / 111_320.0]


def test_the_track_new_line_says_no_velocity_yet():
    from watch.picture import render

    new = {"kind": "track.new", "t": 0.0, "track": "T-9", "domain": "air", "identity": "drone", "affiliation": "unknown",
           "position": at(-4500, 0), "speed_mps": 0.0, "course_deg": None, "sources": [{"uid": 3, "sensor": "radar"}]}
    assert "no velocity yet, seen by" in render(new) and "0.0 m/s" not in render(new)
    new.update(speed_mps=7.0, course_deg=270.0)
    assert "7.0 m/s course 270, seen by" in render(new)
