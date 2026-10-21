"""The mission of scoring v2 (eval/SCORING-V2.md, section 1): the text every agent receives as
its situation brief, verbatim and identically, as rules of engagement are given. Labels derive
from it; the playbook implements the same mission through its config (unchanged).

v2 episodes (eval/headless/families.py, `world="v2"`) set it as `EpisodeSpec.brief`, so the
model watch officer reads it as its "Situation brief", and nothing else about the evaluation.
Its SHA-256 is recorded at the freeze; `MISSION_SHA256` is that hash, checked by the tests.
"""

from __future__ import annotations

import hashlib

MISSION = (
    "Identify every contact of unknown, suspect or hostile affiliation that comes within 3 km of the "
    "bridge, within 60 seconds of its coming within 3 km. A contact counts as identified when the "
    "picture shows its identity at a confidence of 0.6 or more from an EO/IR or sonar sensor, without "
    "an identity conflict. Keep every vehicle above its reserve."
)

#: sha256 of MISSION's UTF-8 bytes
MISSION_SHA256 = "78ccfdafeba691c1c8d42be78ed06e101e71e870649c1be8e0dc0355ad5c7d2c"


def mission_sha256(text: str = MISSION) -> str:
    return hashlib.sha256(text.encode()).hexdigest()
