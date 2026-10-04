"""What code a run ran: the git commit, whether the tree had uncommitted changes, and the
SHA-256 of every binary the stack started. Recorded in each run's `run.json` and in the
batch, and shown in the report, so a number can always be traced to the code behind it.
"""

from __future__ import annotations

import hashlib
import subprocess
from pathlib import Path
from typing import Any

from . import REPO

BINARIES = ("murmur-bridge", "murmur-standins", "murmur-linkemu", "murmur-contacts", "murmur-replay", "murmur-scenarios")


def _git(*args: str) -> str | None:
    try:
        p = subprocess.run(["git", *args], cwd=REPO, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return p.stdout.strip() if p.returncode == 0 else None


def sha256(path: Path) -> str | None:
    if not path.exists():
        return None
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def collect() -> dict[str, Any]:
    """The commit, the dirty flag (tracked files changed; results and other ignored files
    don't count) with the changed paths, and the binaries' hashes."""
    status = _git("status", "--porcelain", "--untracked-files=no")
    changed = [line[3:] for line in (status or "").splitlines() if line.strip()]
    return {
        "commit": _git("rev-parse", "HEAD"),
        "short": _git("rev-parse", "--short", "HEAD"),
        "dirty": bool(changed),
        "changed": changed[:50],
        "binaries": {name: sha256(REPO / "target" / "release" / name) for name in BINARIES},
    }


def code_id(p: dict[str, Any] | None) -> str:
    """A short label for the code that ran: commit, marked when the tree was dirty."""
    if not p:
        return "unknown"
    return f"{p.get('short') or '?'}{' (dirty)' if p.get('dirty') else ''}"
