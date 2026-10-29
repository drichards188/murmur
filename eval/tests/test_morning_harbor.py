"""The real-data watch: source integrity, timing, modes and separation from fictional acts."""

import csv
import datetime as dt
import json
import math
import tomllib
from collections import Counter

from eval import REPO
from eval.situation import _planned_fleet, brief_text, load_situation, resolve_modes
from scripts.import_morning_harbor import DATA, OUTPUT, START, WEATHER, render, render_weather


def test_source_rows_survive_the_import_with_their_timestamps():
    assert OUTPUT.read_text() == render()
    meta = json.loads((DATA / "provenance.json").read_text())
    with (DATA / "ais.csv").open(newline="") as f:
        rows = list(csv.DictReader(f))
    scenario = tomllib.loads(OUTPUT.read_text())
    contacts = scenario["contact"]
    assert len(contacts) == meta["identities"] == 109
    assert sum(len(c["samples"]) for c in contacts) == len(rows) == meta["records"] == 2126
    expected = Counter((r["mmsi"], (dt.datetime.fromisoformat(r["base_date_time"]).replace(tzinfo=dt.timezone.utc) - START).total_seconds(), float(r["lon"]), float(r["lat"])) for r in rows)
    actual = Counter((c["ais"]["mmsi"], p["at_s"], *p["position"]) for c in contacts for p in c["samples"])
    assert actual == expected
    assert not scenario.get("act")
    assert all(c["affiliation"] == "neutral" and c["motion"] == "recorded" for c in contacts)
    # The cruise ship's AIS type is passenger, not a claim that it is a local ferry.
    cruise = next(c for c in contacts if c["name"] == "CARNIVAL MIRACLE")
    assert all(p.get("speed_mps") == 0 for p in cruise["samples"])
    assert any("course_deg" not in p for c in contacts for p in c["samples"])
    assert all(0 <= p["at_s"] < 3600 for c in contacts for p in c["samples"])


def test_morning_watch_is_playable_without_an_account_and_has_eight_unarmed_observers():
    sit = load_situation("morning-harbor-watch")
    assert not sit.todo and sit.at == "2024-06-14T15:00:00Z"
    assert sit.duration_s == 3600 and sit.rate == 1
    modes = resolve_modes(sit)
    assert modes["ai"] == "playbook" and modes["operator"] == "live"
    assert sit.tidewater is None and sit.currents == sit.conditions == "none"
    fleet = _planned_fleet(sit.fleet)
    assert len(fleet) == 8
    platforms = tomllib.loads((REPO / "sim/platforms.toml").read_text())
    assert all(platforms[v["platform"]]["endurance_h"] > 1 for v in fleet)
    assert all(not platforms[v["platform"]].get("armed", False) for v in fleet)
    callsigns = {v["callsign"] for v in fleet}
    assigned = {o.vehicles["callsign"] for o in sit.orders}
    assert assigned <= callsigns and len(assigned) == 6
    assert callsigns - assigned == {"PUMA 1", "PUMA 4"}
    assert not sit.steps
    assert all(w.at_s < sit.duration_s for w in sit.watch)
    assert "BOW KISO" in sit.director and "BOW KISO" not in brief_text(sit)
    assert "not 109 vessels present at once" in sit.brief


def test_wind_uses_the_recorded_hour_and_keeps_missing_weather_missing():
    assert WEATHER.read_text() == render_weather()
    weather = json.loads(WEATHER.read_text())
    rows = weather["points"][0]["rows"]
    assert len(rows) == 11
    assert rows[0]["t"] == START.timestamp() and rows[-1]["t"] == START.timestamp() + 3600
    first = rows[0]["values"]
    assert math.isclose(math.hypot(first["wind_east_mps"], first["wind_north_mps"]), 2.7)
    assert first["wind_east_mps"] > 0 and first["wind_north_mps"] > 0  # FROM 212 degrees
    assert all(set(r["values"]) == {"wind_east_mps", "wind_north_mps"} for r in rows)
