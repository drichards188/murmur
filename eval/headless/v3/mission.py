"""Scoring v3's mission (eval/SCORING-V3.md section 1): what must be identified, by which sensor and
by when, per domain, and the text every agent receives as its situation brief (as v2's mission is
given: verbatim, identically, nothing else about the evaluation). Labels derive from it.

**Per domain** (`DOMAINS`; each value a named constant, each a definitional choice the owner may
change, SCORING-V3.md section 9):

| domain | in time (`grace_s`) | supporting sensor | level |
|---|---|---|---|
| surface | 60 s | EO/IR, or AIS from a true broadcaster | identification (its kind) |
| air | 30 s | EO/IR | identification (its kind) |
| ground | 60 s | EO/IR | identification (its kind) |
| subsurface | 300 s | sonar | recognition (its class: submerged, or a person for a diver) or better |

"In time" is a fifth of the mission's 3 km radius at the domain's threat speed, floored at the
domain's sensor's own shortest classification time: a fast boat at 10 to 20 m/s covers 600 m in 30
to 60 s (v2's 60 s kept); a small drone at 15 to 30 m/s in 20 to 40 s and a light aircraft at 45 to
60 m/s in 10 to 13 s (30 s, the drone's); a car at 8 to 15 m/s in 40 to 75 s (60 s); a UUV at 1 to
2.5 m/s in 240 to 600 s and a diver at 0.3 to 0.7 m/s in 15 to 30 minutes, floored by a passive
sonar's 180 s to identify and an active one's 60 s to recognise (300 s).

Subsurface needs are met at the recognition level because in the physical model only a passive
sonar identifies (after 180 s of contact) and active sonars never do (docs/SENSING.md), and in both
worlds only the drifting buoys, which take no orders, carry a passive sonar: at the identification
level no vehicle an agent can task could answer one.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass

from eval.world import load_world

NEED_RADIUS_M = 3000.0
RESOLVE_CONF = 0.6


@dataclass(frozen=True)
class DomainRule:
    grace_s: float
    #: the sensor types whose support counts (the picture's `supported_by`)
    sensors: tuple[str, ...]
    #: AIS counts too when the contact truly broadcasts it
    ais_counts: bool
    #: "identify": the shown kind must be the true kind; "recognise": its true family will do
    level: str


SURFACE_GRACE_S = 60.0
AIR_GRACE_S = 30.0
GROUND_GRACE_S = 60.0
SUBSURFACE_GRACE_S = 300.0
#: SCORING-V3.md choice 3: subsurface at the recognition level (only passive sonar identifies, and
#: only the untaskable buoys carry one); "identify" is the owner's literal rule
SUBSURFACE_LEVEL = "recognise"

DOMAINS: dict[str, DomainRule] = {
    "surface": DomainRule(SURFACE_GRACE_S, ("eo_ir",), True, "identify"),
    "air": DomainRule(AIR_GRACE_S, ("eo_ir",), False, "identify"),
    "ground": DomainRule(GROUND_GRACE_S, ("eo_ir",), False, "identify"),
    "subsurface": DomainRule(SUBSURFACE_GRACE_S, ("sonar",), False, SUBSURFACE_LEVEL),
}

#: what each world's protected point is called in the mission's text
PLACES = {
    "golden-gate-v3": "the Golden Gate Bridge",
    "golden-gate-scale": "the Golden Gate Bridge",
    "san-diego": "the bay entrance off Ballast Point",
}


def rule(domain: str | None) -> DomainRule:
    """A domain's rule (an unknown domain is held to the surface's)."""
    return DOMAINS.get(str(domain or "surface"), DOMAINS["surface"])


def mission_text(world: str) -> str:
    """The mission every agent receives in a world, verbatim (its SHA-256 is recorded)."""
    m = load_world(world).mission
    place = PLACES.get(world, "the protected point")
    km = m.radius_m / 1000.0
    return (
        f"Identify every contact of unknown, suspect or hostile affiliation that comes within {km:g} km of {place}, "
        f"in time: one on the surface within {SURFACE_GRACE_S:.0f} seconds of its coming within {km:g} km, one in the "
        f"air within {AIR_GRACE_S:.0f} seconds, one on the ground within {GROUND_GRACE_S:.0f} seconds, and one under the "
        f"water within {SUBSURFACE_GRACE_S:.0f} seconds. A contact counts as identified when the picture shows its "
        f"kind at a confidence of {RESOLVE_CONF:g} or more without an identity conflict: from EO/IR, or its own true "
        "AIS, for a contact on the surface; from EO/IR for one in the air or on the ground; from sonar for one under "
        "the water, where its class (submerged, or a person in the water) is enough. Keep every vehicle above its "
        "reserve."
    )


def mission_sha256(world: str) -> str:
    return hashlib.sha256(mission_text(world).encode()).hexdigest()
