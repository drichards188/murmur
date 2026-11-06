"""SITL vehicles act out a strike's damage (sim/sitl_damage.py, bd murmur-y85j.8): the mapping
from a damage record to what the vehicle does (propulsion outputs failed through SITL's
SIM_ENGINE_FAIL, the stand-in's DAMAGE report at once and every 30 s, a destroyed vehicle's
process stopped), and the watcher that follows the damage file.

    .venv/bin/python -m pytest eval/tests/test_sitl_damage.py
"""

from __future__ import annotations

import importlib.util
import json
import os
import sys
from pathlib import Path

import pytest

from eval.stack import _swarm

REPO = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("murmur_sitl_damage", REPO / "sim" / "sitl_damage.py")
sd = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = sd  # its dataclasses look their module up
_spec.loader.exec_module(sd)
swarm = _swarm()

MOB = "DAMAGE mobility: propulsion lost"
FIRE = "DAMAGE firepower: weapons out of action"


def engine(mask):
    return (("SIM_ENGINE_MUL", 0.0), ("SIM_ENGINE_FAIL", float(mask)))


@pytest.mark.parametrize(
    "kind, mask",
    [("plane", 0x04), ("vtol", 0xF4), ("copter", 0x0F), ("blimp", 0x0F), ("boat", 0x05), ("ugv", 0x05),
     ("sub", 0x3F)],
)
def test_mobility_fails_the_kinds_propulsion_outputs_and_reports(kind, mask):
    p = sd.plan(kind, ["mobility"])
    assert p == sd.Plan(params=engine(mask), reports=(MOB,), silent=False)


@pytest.mark.parametrize("kind", ["sailboat", "tracker"])
def test_a_kind_sitl_cannot_stop_only_reports(kind):
    assert sd.plan(kind, ["mobility"]) == sd.Plan(reports=(MOB,))


def test_firepower_only_reports():
    assert sd.plan("copter", ["firepower"]) == sd.Plan(reports=(FIRE,))


def test_both_report_in_the_order_taken_and_destroyed_silences_everything():
    assert sd.plan("plane", ["firepower", "mobility"]) == sd.Plan(params=engine(0x04), reports=(FIRE, MOB))
    assert sd.plan("plane", ["mobility", "firepower", "mobility"]).reports == (MOB, FIRE)
    for states in (["destroyed"], ["mobility", "destroyed"], ["firepower", "destroyed"]):
        assert sd.plan("plane", states) == sd.Plan(silent=True)


def test_every_sitl_kind_is_considered():
    # a new SITL kind in swarm.py needs a propulsion mask, or a reason it has none
    assert set(sd.PROPULSION) | {"sailboat", "tracker"} == set(swarm.VEHICLES)


def test_the_reports_are_the_stand_ins_words():
    text = (REPO / "crates" / "murmur-standins" / "src" / "damage.rs").read_text()
    for words in sd.REPORTS.values():
        assert f'"{words}"' in text


def test_records_whole_lines_only_and_skips_what_changes_nothing():
    data = (b'{"uid":3,"state":"mobility","t":1.0,"cause":"x"}\nnot json\n{"uid":4,"state":"undamaged"}\n'
            b'{"uid":5,"state":"destroyed"}\n{"uid":6,')
    recs, at = sd.read_records(data, 0)
    assert recs == [(3, "mobility"), (5, "destroyed")]
    assert data[at:] == b'{"uid":6,'
    recs, at2 = sd.read_records(data + b'"state":"firepower"}\n', at)
    assert recs == [(6, "firepower")] and at2 == len(data) + len(b'"state":"firepower"}\n')


def test_the_file_is_the_stand_ins():
    assert sd.damage_path({"MURMUR_DAMAGE_FILE": "/x/d.jsonl"}) == Path("/x/d.jsonl")
    assert sd.damage_path({"MURMUR_ROOT": "/r", "MURMUR_HTTP_PORT": "9265"}) == Path("/r/sim/run/damage-9265.jsonl")
    assert sd.damage_path({"MURMUR_ROOT": "/r"}) == Path("/r/sim/run/damage-8765.jsonl")
    assert sd.damage_path({}) == REPO / "sim" / "run" / "damage-8765.jsonl"
    assert sd.control_port(0) == 5763 and sd.control_port(160) == 7363


# ---- the watcher ------------------------------------------------------------------------------


class FakeLink:
    """A vehicle's SERIAL2: echoes a parameter `echo_after` pumps after it was set."""

    def __init__(self, echo_after=1):
        self.values = {}
        self.pending = []
        self.sets = []
        self.texts = []
        self.closed = False
        self.echo_after = echo_after

    def pump(self):
        for item in list(self.pending):
            item[2] -= 1
            if item[2] <= 0:
                self.values[item[0]] = item[1]
                self.pending.remove(item)

    def set_param(self, name, value):
        self.sets.append((name, value))
        self.pending.append([name, value, self.echo_after])

    def statustext(self, text):
        self.texts.append(text)

    def close(self):
        self.closed = True


class Rig:
    VEHICLES = [
        {"uid": 2, "sysid": 2, "instance": 0, "kind": "plane", "callsign": "PUMA 1"},
        {"uid": 3, "sysid": 3, "instance": 1, "kind": "copter", "callsign": "X10 1"},
        {"uid": 4, "sysid": 4, "instance": 2, "kind": "sailboat", "callsign": "SAILDRONE 1"},
    ]

    def __init__(self, path, started_ns, fail_first=0):
        self.t = 100.0
        self.frozen = []
        self.links = {}
        self.fail_left = fail_first
        self.logs = []
        self.w = sd.Watcher(path, self.VEHICLES, self.frozen.append, started_ns, control=self.open,
                            clock=lambda: self.t, log=self.logs.append)

    def open(self, v):
        if self.fail_left:
            self.fail_left -= 1
            raise ConnectionRefusedError("refused")
        self.links[v["uid"]] = FakeLink()
        return self.links[v["uid"]]

    def tick(self, dt=1.0):
        self.w.poll()
        self.w.step()
        self.t += dt


def append(path, *records):
    with open(path, "a") as f:
        for uid, state in records:
            f.write(json.dumps({"uid": uid, "state": state, "t": 1.0, "cause": "test"}) + "\n")


def test_a_previous_runs_file_is_skipped_until_emptied(tmp_path):
    path = tmp_path / "damage.jsonl"
    append(path, (2, "destroyed"))
    os.utime(path, ns=(1_000, 1_000))
    rig = Rig(path, started_ns=2_000)
    rig.tick()
    assert rig.frozen == [] and rig.w.hurt == {}
    append(path, (3, "firepower"))  # written since the fleet started, after the stale bytes
    rig.tick()
    assert rig.frozen == [] and list(rig.w.hurt) == [3]
    path.write_text("")  # the contacts process starting
    rig.tick()
    append(path, (2, "destroyed"))
    rig.tick()
    assert rig.frozen == [2]


def test_mobility_holds_the_parameters_and_reports_at_once_then_every_30_s(tmp_path):
    path = tmp_path / "damage.jsonl"
    rig = Rig(path, started_ns=0)
    rig.tick()
    append(path, (3, "mobility"), (99, "destroyed"))  # 99: a stand-in, its engine's
    rig.tick()
    link = rig.links[3]
    assert link.sets == list(engine(0x0F)) and link.texts == [MOB]
    rig.tick()  # echoed now: nothing set again, nothing said again
    assert link.sets == list(engine(0x0F)) and link.texts == [MOB]
    assert any("propulsion lost (SIM_ENGINE_MUL 0, SIM_ENGINE_FAIL 15)" in s for s in rig.logs)
    for _ in range(27):
        rig.tick()
    assert link.texts == [MOB]
    rig.tick(dt=2.0)
    rig.tick()
    assert link.texts == [MOB, MOB]
    # a new state is reported at once, with the one before
    append(path, (3, "firepower"))
    rig.tick()
    assert link.texts == [MOB, MOB, MOB, FIRE]
    assert rig.frozen == [] and 99 not in rig.w.hurt


def test_a_parameter_not_echoed_is_set_again():
    rig = Rig(Path("/nonexistent/damage.jsonl"), started_ns=0)
    rig.w.hurt[2] = sd.Hurt(vehicle=Rig.VEHICLES[0], states=["mobility"])
    rig.tick()
    rig.links[2].echo_after = 99
    rig.links[2].pending.clear()
    rig.tick()
    rig.tick()
    assert rig.links[2].sets == list(engine(0x04)) * 3


def test_destroyed_goes_silent_once_and_closes_its_link(tmp_path):
    path = tmp_path / "damage.jsonl"
    rig = Rig(path, started_ns=0)
    append(path, (2, "mobility"))
    rig.tick()
    link = rig.links[2]
    append(path, (2, "destroyed"))
    rig.tick()
    rig.tick()
    assert rig.frozen == [2] and link.closed
    assert link.texts == [MOB]


def test_a_sailboat_only_reports_and_says_so(tmp_path):
    path = tmp_path / "damage.jsonl"
    rig = Rig(path, started_ns=0)
    append(path, (4, "mobility"))
    rig.tick()
    assert rig.links[4].sets == [] and rig.links[4].texts == [MOB]
    assert any("cannot take a sailboat's propulsion" in s for s in rig.logs)


def test_a_link_that_will_not_open_is_tried_again_later(tmp_path):
    path = tmp_path / "damage.jsonl"
    rig = Rig(path, started_ns=0, fail_first=1)
    append(path, (3, "mobility"))
    rig.tick()
    assert 3 not in rig.links
    for _ in range(int(sd.Watcher.RETRY_S) - 1):
        rig.tick()
    assert 3 not in rig.links
    rig.tick()
    assert rig.links[3].texts == [MOB]
