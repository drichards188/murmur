"""Same fixture and boundary semantics as the Rust offline weather reader."""
import copy
import json
from pathlib import Path

import pytest

from tidewater.weather import Weather, fetch_weather

FIXTURE = Path(__file__).resolve().parents[2] / 'crates/murmur-weather/tests/data/weather.json'


def test_shared_weather_fixture():
    w = Weather.load(FIXTURE)
    v = w.at(37.5, -122.5, 1800)
    assert v['visibility_m'] == 5500
    assert w.vector(37.5, -122.5, 1800) == (1, 1)
    assert v['precipitation_mm'] == 4
    assert v['humidity_pct'] is None
    assert w.at(37.5, -122.5, 0)['precipitation_mm'] == 1
    for lat, t in [(36, 1800), (37.5, -1), (37.5, 7201)]:
        assert w.at(lat, -122.5, t) == {}
    assert w.vector(37.5, -122.5, 7201) == (None, None)
    d = json.loads(FIXTURE.read_text())
    del d['points'][0]['rows'][1]
    assert Weather(d).at(37.5, -122.5, 1800) == {}


def test_fetch_checks_units_preserves_null_and_reuses_cache(tmp_path, monkeypatch):
    from types import SimpleNamespace
    from tidewater.scenario import Area, parse_time
    from tidewater import fetch
    sc = SimpleNamespace(cache_dir=tmp_path, area=Area(-123, 37, -122, 38, 100),
                         wind={'grid': [1, 1], 'model': 'gfs_hrrr'},
                         start=parse_time('2026-06-14'), end=parse_time('2026-06-15'))
    raw = {'hourly': {'time': ['2026-06-14T00:00', '2026-06-14T01:00'],
                     'visibility': [1000, None], 'wind_speed_10m': [2, 2], 'wind_direction_10m': [270, 0]},
           'hourly_units': {'visibility': 'm', 'wind_speed_10m': 'm/s', 'wind_direction_10m': '°'}}
    calls = []
    def get(url, params):
        calls.append((url, params))
        return json.dumps(raw).encode()
    monkeypatch.setattr(fetch, '_get', get)
    out = fetch_weather(sc, lambda _: None)
    d = json.loads(out.read_text())
    assert d['points'][0]['rows'][1]['values']['visibility_m'] is None
    assert d['points'][0]['rows'][0]['values']['wind_east_mps'] == pytest.approx(2)
    assert len(d['raw_sha256']) == 64 and (tmp_path/'weather-source.json').exists()
    assert fetch_weather(sc) == out and len(calls) == 1
    out.unlink()
    raw['hourly_units']['visibility'] = 'km'
    with pytest.raises(ValueError, match='weather unit'):
        fetch_weather(sc, lambda _: None)


def test_bad_timestamps_and_invalid_percentages_are_refused():
    d = json.loads(FIXTURE.read_text())
    bad = copy.deepcopy(d)
    bad['points'][0]['rows'][1]['t'] = 0
    with pytest.raises(ValueError, match='timestamps'):
        Weather(bad)
    d['points'][0]['rows'][0]['values']['humidity_pct'] = 101
    with pytest.raises(ValueError, match='humidity'):
        Weather(d)


def test_batch_missing_weather_does_not_break_other_locations():
    import datetime as dt
    from tidewater.conditions import Conditions
    from tidewater.layers import Terrain, Tide, TimeSeries
    import numpy as np
    w = Weather.load(FIXTURE)
    terrain = Terrain(np.full((2, 2), 1.0), [-123, 37, -122, 38])
    c = Conditions(terrain, Tide(TimeSeries([0, 7200], [0, 0])), None, w)
    c.weather = w
    sample = c.at(37.5, -122.5, dt.datetime.fromtimestamp(1800, dt.timezone.utc))
    assert sample.wind_east == 1.0 and sample.weather['visibility_m'] == 5500
    rows = c.sample_many([37.5, 36], [-122.5, -122.5], 1800).to_dicts()
    assert rows[0]['wind_east'] == 1.0 and rows[1]['wind_east'] is None
    assert rows[1]['weather'] == {}
    json.dumps(rows, allow_nan=False)
