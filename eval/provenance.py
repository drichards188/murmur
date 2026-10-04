"""What code a run ran: the git commit, whether tracked code had uncommitted changes (the
changed paths are kept; documentation does not count), and the SHA-256 of every binary the
stack started. Recorded in each run's `run.json` and in the
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
    # keep leading spaces: `status --porcelain` starts a line with one ("XY path")
    return p.stdout.rstrip("\n") if p.returncode == 0 else None


def is_doc(path: str) -> bool:
    """A path that cannot change what a run does: documentation."""
    return path.startswith(("docs/", "ocs/")) or path.endswith(".md")


def code_dirty(p: dict[str, Any] | None) -> bool | None:
    """Whether code (anything but documentation) differed from the commit, from the recorded
    changed paths (so older records, whose flag counted documentation too, read the same)."""
    if not p:
        return None
    changed = p.get("changed")
    if changed is None:
        return p.get("dirty")
    return any(not is_doc(c) for c in changed)


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
    changed = [line[3:] for line in (status or "").splitlines() if len(line) > 3]
    return {
        "commit": (_git("rev-parse", "HEAD") or "").strip() or None,
        "short": (_git("rev-parse", "--short", "HEAD") or "").strip() or None,
        # code only: a documentation edit during a batch does not change what a run does
        "dirty": any(not is_doc(c) for c in changed),
        "changed": changed[:50],
        "binaries": {name: sha256(REPO / "target" / "release" / name) for name in BINARIES},
    }


def code_id(p: dict[str, Any] | None) -> str:
    """A short label for the code that ran: commit, marked when code differed from it."""
    if not p:
        return "unknown"
    return f"{p.get('short') or '?'}{' (dirty)' if code_dirty(p) else ''}"
