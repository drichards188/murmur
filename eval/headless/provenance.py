"""What an episode or a full-stack run was made from, beyond the code at a commit: the inputs
git does not track (the satellite element sets in `sim/space/cache`, a symlink into the main
checkout here; the link profiles, `MURMUR_LINKS` or `sim/links.toml`) and the binaries' hashes.
Each digest is computed once per process."""

from __future__ import annotations

import functools
import hashlib
import subprocess
from pathlib import Path
from typing import Any

from . import REPO

#: the full stack's own binaries (scripts/up.sh builds them), and the simulator's
STACK_BINARIES = ("murmur-bridge", "murmur-standins", "murmur-linkemu", "murmur-contacts", "murmur-replay")


def sha256_file(p: Path) -> str | None:
    try:
        h = hashlib.sha256()
        with open(p, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return None


@functools.cache
def git_state() -> dict[str, Any]:
    try:
        commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True).stdout.strip()
        dirty = bool(subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=REPO,
                                    capture_output=True, text=True).stdout.strip())
    except OSError:
        return {"commit": None, "dirty": None}
    return {"commit": commit or None, "dirty": dirty}


@functools.cache
def space_cache_digest(cache: Path | None = None) -> dict[str, Any]:
    """The element sets the emulator's SATCOM reads: the directory (resolved), its file count
    and one SHA-256 over every file's name and contents, in name order."""
    d = (cache or REPO / "sim" / "space" / "cache")
    if not d.is_dir():
        return {"path": str(d), "files": 0, "sha256": None}
    h = hashlib.sha256()
    files = sorted(f for f in d.iterdir() if f.is_file())
    for f in files:
        h.update(f.name.encode() + b"\0")
        h.update((sha256_file(f) or "").encode() + b"\n")
    return {"path": str(d.resolve()), "files": len(files), "sha256": h.hexdigest()}


def links_digest(path: Path) -> dict[str, Any]:
    return {"path": str(path), "sha256": sha256_file(path)}


@functools.cache
def stack_binaries() -> dict[str, str | None]:
    rel = REPO / "target" / "release"
    return {b: sha256_file(rel / b) for b in STACK_BINARIES}


def inputs(links: Path) -> dict[str, Any]:
    """The untracked inputs both sides read."""
    return {"links": links_digest(links), "space_cache": space_cache_digest()}
