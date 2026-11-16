"""Scoring v3's v0 mission text (eval/SCORING-V3.md section 1): the brief every agent received, verbatim,
in every run before decision rules 33, its SHA-256 in each situation's stated truth. Since stage S6
(docs/SCENARIO-SETTINGS.md) the values it states are no longer this module's: the scoring reads each
record's own settings (settings.py `of_record`), a run under rules 33 or later is briefed with its
scenario's ROE card (eval/roe_card.py `render`), and this module keeps only the v0 renderer, which
`render` returns under older rules.

`mission_text(world)` renders the v0 text from the frozen v0 settings (sim/policy-v0-watch.toml:
its `[watch]` and the world's `[area.<world>]`, read through settings.py `v0`), so its SHA-256 is
the one the frozen file records for the world (`[mission_sha256.<world>] current`; eval/roe_card.py
checks it). What the text says, per domain (SCORING-V3.md 2.1 says why each value):

| domain | in time | supporting sensor | level |
|---|---|---|---|
| surface | 60 s | EO/IR, or AIS from a true broadcaster | identification (its kind) |
| air | 30 s | EO/IR | identification (its kind) |
| ground | 60 s | EO/IR | identification (its kind) |
| subsurface | 300 s | sonar | recognition (its class: submerged, or a person for a diver) or better |
"""

from __future__ import annotations

import hashlib

from .settings import v0


def mission_text(world: str) -> str:
    """The v0 mission every agent received in a v3 world before rules 33, verbatim."""
    s = v0(world)
    a = s.area
    assert a is not None
    km = a.radius_m / 1000.0
    grace = {d: s.rule(d).grace_s for d in ("surface", "air", "ground", "subsurface")}
    # the circle's centre as a point (the owner's decision of 2026-11-15, bd murmur-isxg): the words
    # alone left an agent guessing where "the bay entrance" is by a kilometre or two
    lon, lat = a.center
    return (
        f"Identify every contact of unknown, suspect or hostile affiliation that comes within {km:g} km of {a.name} "
        f"(the circle's centre is [{lon:.4f}, {lat:.4f}], longitude then latitude), in time: one on the surface within {grace['surface']:.0f} seconds of its coming within {km:g} km, one in the "
        f"air within {grace['air']:.0f} seconds, one on the ground within {grace['ground']:.0f} seconds, and one under the "
        f"water within {grace['subsurface']:.0f} seconds. A contact counts as identified when the picture shows its "
        f"kind at a confidence of {s.bar:g} or more without an identity conflict: from EO/IR, or its own true "
        "AIS, for a contact on the surface; from EO/IR for one in the air or on the ground; from sonar for one under "
        "the water, where its class (submerged, or a person in the water) is enough. Keep every vehicle above its "
        "reserve."
    )


def mission_sha256(world: str) -> str:
    return hashlib.sha256(mission_text(world).encode()).hexdigest()
