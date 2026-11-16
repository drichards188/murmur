"""Old results stay honest (docs/SCENARIO-SETTINGS.md section 3; stage S6): rescoring a batch of v3
records whose runs logged no settings (every run before decision rules 33, graded under the frozen
v0 values) reproduces its report byte for byte. The batch here is small and synthetic (the labels'
own test records, eval/headless/tests/test_v2_labels.py `Synth`): a fast boat inbound, a vehicle
whose battery runs low, and three orders, for `none` and the ceiling, on both v3 maps; its
`dev_v3.json` was written by the code before S6 moved the scoring onto each record's settings
(eval/headless/tests/fixtures/rescore/dev_v3.json), and the report of today must equal it. The
same check on the real result directories (`python -m eval.headless.v3.run report` into a copy of
each, compared with the original) is SCORING-V3.md section 17.27's.

Regenerate the fixture only for a change that is meant to move v0's reports (and say so where the
result directories are rescored): `python -m eval.headless.tests.test_v3_rescore <out file>`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from eval.headless.tests.test_v2_labels import BR, Synth, at, battery, inbound, orbit, resolved_from, rising
from eval.headless.tests.test_v3_labels import v3_record
from eval.headless.v2.record import dump

FIXTURE = Path(__file__).resolve().parent / "fixtures" / "rescore" / "dev_v3.json"
#: the batch's episodes: (family, situation seed, agent, world)
EPISODES = [("v3-f1-fast-boat-gg", 5000, "none", "golden-gate-v3"),
            ("v3-f1-fast-boat-gg", 5000, "oracle-v3-shown", "golden-gate-v3"),
            ("v3-f1-fast-boat-sd", 5000, "none", "san-diego"),
            ("v3-f1-fast-boat-sd", 5000, "oracle-v3-shown", "san-diego")]


def situation(agent: str, world: str) -> dict[str, Any]:
    """One synthetic record: a boat inbound from 5 km to 1 km west of the Golden Gate Bridge at 20
    m/s (inside the Golden Gate's v0 circle at 100 s; never inside San Diego's, a world with no
    need), its track identified by PUMA 1's camera from 130 s; PUMA 2 draining from 50 % (the
    low-reserve need opens at 35 %, the reserve at 20 %); the ceiling's orders: an orbit on the
    boat's way, an orbit of PUMA 2 9 km off and an rtl of PUMA 2, about the world's centre."""
    from eval.headless.v3.settings import v0

    area = v0(world).area
    assert area is not None
    s = Synth(400)
    shift = (area.center[0] - BR[0], area.center[1] - BR[1])
    s.contact("boat", path=inbound())
    s.track("T-0001", "boat", ident=resolved_from(130.0), hits=rising("3:eo_ir", 105.0, 135.0))
    s.vehicle(3, lambda t: {"lon": area.center[0] - 0.01, "lat": area.center[1]})
    s.vehicle(4, battery(50.0, 0.12, rtl_at=200.0))
    if agent != "none":
        orbit(s, 11, 3, 60.0, 62.0, center=(at(-1500.0, 0.0)[0] + shift[0], at(-1500.0, 0.0)[1] + shift[1]))
        orbit(s, 12, 4, 90.0, 92.0, center=(at(0.0, 9000.0)[0] + shift[0], at(0.0, 9000.0)[1] + shift[1]))
        orbit(s, 13, 4, 200.0, 201.0, kind="rtl")
    rec = v3_record(s, agent=agent, world=world)
    rec["world"]["conditions"]["at"] = "2026-06-14T19:00:00Z"  # local noon: the sensing model's sun
    v0_extra = v0(world).v0
    assert v0_extra is not None
    rec["truth"]["stated"] = {"family": "f1-fast-boat", "world": world, "seed": 5000, "scoring": "v3",
                              "mission_sha256": v0_extra.mission_sha256}
    return rec


def write_batch(d: Path) -> None:
    """The batch's records and index in `d` (paths absolute: the report loads each by its path)."""
    d.mkdir(parents=True, exist_ok=True)
    index = []
    for fam, seed, agent, world in EPISODES:
        name = f"{fam}-{agent}-s{seed}-r0"
        path = d / f"{name}.json.gz"
        dump(situation(agent, world), path)
        index.append({"name": name, "path": str(path), "family": fam, "agent": agent, "situation_seed": seed,
                      "r": 0, "seed": seed, "class": "scored", "attempts": 1})
    (d / "episodes_v3.json").write_text(json.dumps(index, indent=1, sort_keys=True))


def report(d: Path) -> bytes:
    from eval.headless.v3.report import write

    write_batch(d)
    j, _ = write(d, workers=1)
    return j.read_bytes()


def test_rescoring_a_v0_batch_reproduces_its_report_byte_for_byte(tmp_path: Path):
    got = report(tmp_path / "batch")
    assert got == FIXTURE.read_bytes(), "a v0 record's report moved: rescoring must reproduce it (module doc)"


def test_the_batch_exercises_what_the_settings_decide():
    """The fixture is not empty: needs served and not, a low-reserve fault need, orders labelled."""
    doc = json.loads(FIXTURE.read_text())
    eps = {e["name"]: e for e in doc["episodes"]}
    assert len(eps) == len(EPISODES)
    shown = eps["v3-f1-fast-boat-gg-oracle-v3-shown-s5000-r0"]
    assert [n["served"] for n in shown["needs"]] == [True] and shown["fault_needs"]
    assert sum(shown["actions"].values()) == 3


if __name__ == "__main__":
    import tempfile

    with tempfile.TemporaryDirectory() as td:
        out = report(Path(td) / "batch")
    Path(sys.argv[1]).write_bytes(out)
