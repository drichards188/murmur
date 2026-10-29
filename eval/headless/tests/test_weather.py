"""Paired fog demonstration: identical boat motion, real sensing, unchanged radio links.
These are regression checks of a scripted demonstration, not human evaluation findings.
"""
import json
import re
import shutil
import subprocess
from pathlib import Path

import pytest

from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.sim import BINARY
from eval.headless.situations import from_situation
from eval.headless.v2.record import Picture
from tidewater.weather import Weather


@pytest.fixture(scope='module', autouse=True)
def binary():
    subprocess.run([shutil.which('cargo') or str(Path.home()/'.cargo/bin/cargo'), 'build',
                    '--release', '-q', '-p', 'murmur-headless'], cwd=REPO, check=True)


class WeatherEpisode(Episode):
    def sample(self, now):
        if now == 260:
            code, w = self.sim.api('GET', '/api/weather?lat=37.821&lon=-122.460')
            assert code == 200 and w['available'] and not w['stale']
            self.weather_sample = w
            assert not any(secret in json.dumps(w) for secret in ('looping-boat', 'maneuver', 'timeline', 'route'))
            expected = Weather.load(REPO/'sim/headless/weather/fog-watch.json').at(37.821, -122.46, w['t'])
            for key in ('wind_east_mps', 'wind_north_mps', 'gust_mps', 'precipitation_mm'):
                assert w['weather']['values'][key] == pytest.approx(expected[key])
            assert len(w['weather']['archive_sha256']) == 64
            code, grid = self.sim.api('GET', '/api/weather/grid?bbox=-122.47,37.81,-122.44,37.83')
            assert code == 200 and grid['available'] and len(grid['cells']) == 32*24
            assert grid['t'] == w['t'] and grid['archive_sha256'] == w['weather']['archive_sha256']
            if w['weather']['synthetic']:
                assert all(c['visibility_m'] == 50 for c in grid['cells'])
            assert not any(secret in json.dumps(grid) for secret in ('looping-boat', 'timeline', 'route'))
            assert self.sim.api('GET', '/api/weather/grid?bbox=0,0,0,1')[0] == 400
            assert 'get_weather' in [t['name'] for t in self.sim.tool_defs()]
            assert self.sim.api('GET', '/api/weather?lat=91&lon=0')[0] == 400
        return super().sample(now)


def test_fog_hides_the_maneuver_without_cutting_the_radio(tmp_path):
    spec = from_situation('fog-watch')
    raw = spec.contacts.read_text()
    clear = tmp_path/'clear.toml'
    clear.write_text(re.sub(r'\[\[sensing.weather.timeline\]\].*?(?=\[\[contact\]\])', '', raw, flags=re.S))
    records, samples = [], []
    for label, path in [('fog', None), ('clear', clear)]:
        ep = WeatherEpisode(spec, 611, tmp_path/label,
                            Options(agent='none', scoring='v3', binary=BINARY), contacts_path=path)
        records.append(ep.run())
        samples.append(ep.weather_sample)
    fog, clear_record = records
    p, q = [Picture(r['v3']['picture']) for r in records]
    tid = next(t for t in p.tracks if p.present(t, p.index(120)))
    last = p.kinematics(tid, p.index(200))['last_seen_s']
    assert last < 190
    assert p.kinematics(tid, p.index(260))['last_seen_s'] == last
    assert q.kinematics(tid, q.index(260))['last_seen_s'] > 225
    assert not p.present(tid, p.index(410)) and q.present(tid, q.index(410))
    assert any(p.present(t, p.index(480)) for t in p.tracks if t != tid), 'camera reacquires after clearing'
    assert samples[0]['weather']['synthetic'] and samples[0]['sensing']['visibility_m'] == 50
    assert not samples[1]['weather']['synthetic'] and samples[1]['sensing']['visibility_m'] > 10000
    assert fog['truth']['samples'] == clear_record['truth']['samples'], 'only weather differs'
    for r in records:
        assert not any(o[4] for row in r['truth']['ours'] for o in row['ours']), 'no radio outage in this demo'
        assert r['orders'][0]['dispatched']
