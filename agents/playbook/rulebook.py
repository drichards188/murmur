"""The evaluation's frozen rule book (`eval/evidence.py`, v2) as the engine's veto.

The engine never proposes what Murmur's own rule book objects to: after a passing preview, the
rule book reads the preview's checks and, for an armed task aimed at a point, the tracks as this
tick read them, and any reason it gives drops the candidate (with a back-off, like a failing
preview). The function is imported, not copied, so the engine and the evaluation's proxy and
metrics apply one rule book; it is pure (math and typing only), and importing it reads no file
of the evaluation's.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[2]
if str(REPO) not in sys.path:
    sys.path.append(str(REPO))

from eval.evidence import C_MIN, RULEBOOK_VERSION, evidence  # noqa: E402

__all__ = ["C_MIN", "RULEBOOK_VERSION", "veto"]


def veto(armed: bool, preview: dict[str, Any] | None, request: dict[str, Any], tracks: list[dict[str, Any]]) -> list[str]:
    """Every reason the rule book gives against the task; empty when nothing does. `preview`
    None (shadow mode: nothing previewed) reads no checks."""
    checks = list((preview or {}).get("checks") or [])
    return evidence(armed, checks, request.get("center"), request.get("radius_m"), tracks)
