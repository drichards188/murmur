"""A policy file as the decision layer reads it (docs/SCENARIO-SETTINGS.md section 3).

A policy file may extend another (`extends = "<file>"`, relative to it) and set only what it
changes; crates/murmur-decide merges the layers and refuses a copy, a cycle, an unknown or
repeated tier and a chain of more than four files. Python never merges them itself: `show` asks
`murmur-policy show` for the policy exactly as a run would log it (`run.started`'s `policy`),
the files it was built from with each one's sha256 (the file given first, then each file it
extends, in turn), and the watch settings a run given the file decides under (`watch`: from rules
33 its `[watch]`; under older rules what they meant, sim/policy-v0-watch.toml's, with the file's
`[watch.area]`). Under rules below 33 a file whose settings differ from the standing values (the
area aside) is refused, as the bridge refuses it.

    from eval.policy_file import show
    show("sim/situations/policies/watch.toml")["policy"]["tier"]
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any

from . import REPO

BINARY = REPO / "target" / "release" / "murmur-policy"

_built: set[Path] = set()


class PolicyError(RuntimeError):
    """A policy file the decision layer refuses (murmur-policy's message), or no binary."""


def cargo() -> str:
    return os.environ.get("CARGO") or shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")


def binary() -> Path:
    """`target/release/murmur-policy`, built once a process (a no-op when it is current), or the
    binary `MURMUR_POLICY_BIN` names as it is."""
    named = os.environ.get("MURMUR_POLICY_BIN")
    if named:
        return Path(named)
    if BINARY not in _built:
        cmd = [cargo(), "build", "--release", "-q", "-p", "murmur-decide", "--bin", "murmur-policy"]
        p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True)
        if p.returncode != 0:
            raise PolicyError(f"building murmur-policy failed: {p.stderr.strip()[-2000:]}")
        _built.add(BINARY)
    return BINARY


def show(path: str | Path, rules: int | None = None) -> dict[str, Any]:
    """`murmur-policy show <path> [--rules N]`, parsed: `{"policy": ..., "layers": [{"path",
    "sha256"}], "watch": {...}}` (`watch` in the design's schema, its `area` only when the file
    sets one). A relative path is the repository's. Raises `PolicyError` with the binary's message
    when the file is refused."""
    cmd = [str(binary()), "show", str(path)]
    if rules is not None:
        cmd += ["--rules", str(rules)]
    p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True)
    if p.returncode != 0:
        raise PolicyError(p.stderr.strip() or f"murmur-policy exited {p.returncode}")
    return json.loads(p.stdout)
