"""docs/AUTONOMOUS-NODES.md stage 5 (bd murmur-y85j.5, murmur-mmxk): a contact's orders, its fire
control and its weapons headless, as the contacts process runs them live (both run
`murmur_contacts::step`; crates/murmur-headless tests/pieces.rs checks the simulator's loop
against it second by second).

Smoke episodes on the real `murmur-headless` (built here if stale): the
`hostile-act-loitering-munition` fleet and opening orders with a contacts file of its own, an armed
fast boat (sim/contacts.toml `armed_fast_boat`) that runs in, holds mid-strait and is ordered at
200 s to attack HUNTER 1 with its missiles from where it is. The node takes the order, looks, sees
HUNTER 1 in the in-process bridge's answer to its look, fires its catalog missile at its reload,
and each strike on HUNTER 1 is drawn by the damage model; the audit log replays with its anchors.
The same order as a timeline's `red` step (the contacts' intake in process, as the live runner
posts it to their API) makes the node do exactly the same. The episodes run on the physical
sensing model, as the situation runs live (its headless default is the v1 world's legacy pin,
eval/headless/situations.py `process_spec`).
"""

from __future__ import annotations

import dataclasses
import tempfile
from pathlib import Path
from typing import Any

from eval.headless.tests.test_engage_space import binary, events, replays  # noqa: F401 (a fixture)

#: the raider of sim/scenarios/hostile-act-engagement.toml, of an armed kind, holding mid-strait
BOAT = """
title = "Armed boat ordered to attack (docs/AUTONOMOUS-NODES.md stage 5 test)"
seed = 17

[sensing]
misclassification_rate = 0.0

[[contact]]
id = "fastboat-raider"
name = "Armed fast boat without AIS, in from the sea, holding mid-strait west of the bridge"
affiliation = "hostile"
domain = "surface"
kind = "armed_fast_boat"
speed_mps = 15
motion = "transit"
route = [[-122.5250, 37.8080], [-122.4950, 37.8120]]
start_s = 5
"""

#: its order: at 200 s, attack HUNTER 1 (uid 1) with its missiles from where it is
ORDER = {"node": "fastboat-raider", "do": "attack", "target": {"vehicle": 1}, "weapon": "ATGM-class missile",
         "stay": True, "label": "attacks the sensor ship (HUNTER 1)"}

#: the order in the file, at 200 s on the contacts' clock
ATTACK = BOAT + """
[[order]]
at_s = 200
node = "fastboat-raider"
do = "attack"
target = { vehicle = 1 }
weapon = "ATGM-class missile"
stay = true
label = "attacks the sensor ship (HUNTER 1)"
"""


def run(tmp_path: Path, contacts_text: str = ATTACK, script: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    from eval.headless.episode import Episode, Options
    from eval.headless.situations import Calibration, from_situation

    tmp_path.mkdir(parents=True, exist_ok=True)
    contacts = tmp_path / "contacts.toml"
    contacts.write_text(contacts_text)
    e = from_situation("hostile-act-loitering-munition")
    e = dataclasses.replace(e, world="v3", duration_s=330.0, script=e.script + list(script or []))
    with tempfile.TemporaryDirectory(prefix="nodes-") as td:
        return Episode(e, 1, Path(td), Options(calibration=Calibration.load(), keep=tmp_path / "a", agent="none"),
                       contacts_path=contacts).run()


def test_an_ordered_node_fires_by_its_catalog_and_the_episode_replays(binary: Path, tmp_path: Path):  # noqa: F811
    rec = run(tmp_path / "file")
    opp = rec["truth"]["opponent"]
    ev = opp["node_events"]
    taken = next(e for e in ev if e["what"] == "order")
    assert taken["node"] == "fastboat-raider" and 199.0 <= taken["s"] <= 201.0, taken
    # it fired only once it perceived HUNTER 1, which only the bridge's answer to its look says
    fired = [e for e in ev if e["what"] == "fired"]
    assert fired, ev
    assert all(e["weapon"] == "ATGM-class missile" for e in fired), fired
    # the catalog's missile (four rounds, a 30 s reload), from where it held
    flights = opp["weapons"]
    assert 1 <= len(flights) <= 4 and {f["name"] for f in flights} == {"ATGM-class missile"}, flights
    assert all(f["by"] == "fastboat-raider" and f["against_vehicle"] == 1 and f["kind"] == "atgm" for f in flights)
    launches = sorted(f["launch_t"] for f in flights)
    assert all(b - a >= 30.0 - 1e-6 for a, b in zip(launches, launches[1:])), launches
    assert {a["id"] for a in opp["acts"]} == {f["act"] for f in flights}
    # each strike on HUNTER 1 drawn by the damage model (the Sea Hunter reads as a fishing vessel)
    assert opp["impacts"], opp
    for i in opp["impacts"]:
        assert i["against_vehicle"] == 1 and i["damage"]["figures"] == "fishing_vessel", i
    # the bridge sensed the launches as live does (a camera of ours on the launch, rules 9's
    # ident.act), and its audit log replays, every decision and its anchors
    seen = [e for e in events(rec) if e["type"] == "ident.act"]
    assert seen and all(e["act"]["against"] == {"kind": "vehicle", "uid": "1"} for e in seen), seen
    replays(tmp_path / "file" / "a")

    # the same order as a timeline's red step at 200 s: posted to the contacts' intake in process
    # (no at_s: now), taken at their next tick, the second the file's is taken at; the node does
    # exactly the same, its order numbered the same
    step = {"kind": "step", "at_s": 200.0, "id": "raid", "step": "red", "params": dict(ORDER), "proposer": None}
    red = run(tmp_path / "red", BOAT, [step])
    (posted,) = [r for r in red["script"] if r.get("step") == "raid"]
    assert posted["status"] == 200 and posted["answer"] == {"id": 1, "accepted": True}, posted
    assert red["truth"]["opponent"] == opp
