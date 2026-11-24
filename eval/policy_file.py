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

`rules_in_force()` is the decision rules a run started now decides under: `MURMUR_RULES` when set
(the bridge's and the headless simulator's, eval/headless/sim.py passes it on), else the build's
default, `RULES_VERSION` as crates/murmur-decide/src/rules.rs defines it (read from the source, never
copied here).

    from eval.policy_file import show
    show("sim/situations/policies/watch.toml")["policy"]["tier"]
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any

from . import REPO

BINARY = REPO / "target" / "release" / "murmur-policy"
RULES_RS = REPO / "crates" / "murmur-decide" / "src" / "rules.rs"

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


def _rules_const(name: str) -> int:
    """The numeric value of a rules.rs constant, followed through the named constants it is defined
    by (`pub const RULES_VERSION: u32 = REACH_NOW_SINCE;`, `pub const REACH_NOW_SINCE: u32 = 32;`)."""
    text = RULES_RS.read_text()
    consts = dict(re.findall(r"(?m)^pub const ([A-Z0-9_]+): u32 = ([A-Z0-9_]+);", text))
    for _ in range(8):
        v = consts.get(name)
        if v is None:
            break
        if v.isdigit():
            return int(v)
        name = v
    raise PolicyError(f"{RULES_RS}: no numeric constant (stopped at {name})")


def default_rules() -> int:
    """The build's default decision rules: `RULES_VERSION` in crates/murmur-decide/src/rules.rs."""
    return _rules_const("RULES_VERSION")


def newest_rules() -> int:
    """The newest decision rules this build knows: `RULES_NEWEST`, one ahead of `default_rules`
    while a rules version is being built in stages (what `MURMUR_RULES` may ask for)."""
    return _rules_const("RULES_NEWEST")


def rules_in_force() -> int:
    """The decision rules a run started now decides under: `MURMUR_RULES`, else `default_rules`."""
    asked = (os.environ.get("MURMUR_RULES") or "").strip()
    return int(asked) if asked else default_rules()
