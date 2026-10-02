"""Offline tests of the core math (no network, no cached data)."""

import math

import numpy as np
import pytest

from tidewater.conditions import Conditions
from tidewater.drivers.ardupilot import from_dir, parse_instances
from tidewater.layers import CurrentStation, Currents, Terrain, Tide, TimeSeries, Wind, heading_vector, idw


def test_heading_vector_points_toward():
    e, n = heading_vector(2.0, 90)
    assert e == pytest.approx(2.0) and n == pytest.approx(0.0, abs=1e-9)
    e, n = heading_vector(1.0, 180)
    assert n == pytest.approx(-1.0)


def test_ardupilot_direction_is_coming_from():
    # water flowing east comes FROM the west (270) in ArduPilot's SIM_TIDE_DIR convention
    speed, frm = from_dir(1.5, 0.0)
    assert speed == pytest.approx(1.5) and frm == pytest.approx(270)
    _, frm = from_dir(0.0, -1.0)  # flowing south, comes from north
    assert frm == pytest.approx(0)


def test_cosine_interpolation_between_tidal_events():
    # slack (0) at t=0, max flood (2) at t=100: half-cosine, not a straight line
    ts = TimeSeries([0, 100], [0.0, 2.0], cosine=True)
    assert ts.at(50) == pytest.approx(1.0)
    assert ts.at(25) == pytest.approx(2.0 * (1 - math.cos(math.pi * 0.25)) / 2)
    assert ts.at(25) < 0.5  # slower start than linear: the current builds gradually after slack
    assert TimeSeries([0, 100], [0.0, 2.0]).at(25) == pytest.approx(0.5)


def test_terrain_bilinear_and_bounds():
    # 2x2 grid, row 0 = north. bbox 0..2 deg lon, 0..2 deg lat; cell centres at 0.5 and 1.5
    t = Terrain(np.array([[10.0, 20.0], [30.0, 40.0]]), [0, 0, 2, 2])
    assert t.elevation(1.5, 0.5) == pytest.approx(10.0)  # NW centre
    assert t.elevation(0.5, 1.5) == pytest.approx(40.0)  # SE centre
    assert t.elevation(1.0, 1.0) == pytest.approx(25.0)  # middle
    assert t.elevation(3.0, 1.0) is None


def test_idw_exact_at_a_station_and_between():
    pts = [(37.80, -122.50), (37.80, -122.40)]
    vals = [(1.0, 0.0), (3.0, 0.0)]
    assert idw(pts, vals, 37.80, -122.50) == (1.0, 0.0)
    mid = idw(pts, vals, 37.80, -122.45)
    assert mid[0] == pytest.approx(2.0, abs=0.01)


def test_current_station_flood_and_ebb_axes():
    ts = TimeSeries([0, 10, 20], [1.0, 0.0, -2.0])
    st = CurrentStation("X", 37.8, -122.5, [(3.0, 70, 250, ts)])
    e, n = st.vector(0)
    assert math.degrees(math.atan2(e, n)) % 360 == pytest.approx(70)
    e, n = st.vector(20)  # negative = ebb direction
    assert math.hypot(e, n) == pytest.approx(2.0)
    assert math.degrees(math.atan2(e, n)) % 360 == pytest.approx(250)


def _conditions():
    # west half land (+5 m), east half seabed (-10 m); level 1 m; current 1 m/s east; wind from north 4 m/s
    terrain = Terrain(np.array([[5.0, -10.0], [5.0, -10.0]]), [-1, -1, 1, 1])
    tide = Tide(TimeSeries([0, 1e10], [1.0, 1.0]))
    flat = TimeSeries([0, 1e10], [1.0, 1.0])
    currents = Currents([CurrentStation("S", 0.0, 0.5, [(0.0, 90, 270, flat)])])
    wind = Wind([(0.0, 0.0, TimeSeries([0, 1e10], [0.0, 0.0]), TimeSeries([0, 1e10], [-4.0, -4.0]))])
    return Conditions(terrain, tide, currents, wind)


def test_conditions_land_water_and_draft():
    c = _conditions()
    land = c.at(0.5, -0.5, 100)
    sea = c.at(0.5, 0.5, 100)
    assert not land.wet and land.current_east == 0.0
    assert sea.wet and sea.water_depth_m == pytest.approx(11.0)
    assert sea.current_east == pytest.approx(1.0)
    assert sea.navigable(2.0) and not sea.navigable(12.0)
    assert sea.wind[1] == pytest.approx(180)  # blowing toward the south
    assert not c.at(5.0, 5.0, 100).in_area


def test_parse_instances():
    assert parse_instances("0-2,5") == [0, 1, 2, 5]
