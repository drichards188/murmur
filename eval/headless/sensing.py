"""The physical sensing model's predictions in Python (WORLD-V3-PLAN 2.8 item 5; evaluation only).

`SenseModel` runs `murmur-headless --sense` and asks it, one request per line, what one of our
vehicles' sensors makes of one contact: the line of sight (horizon, hull-down, terrain) and the
chance that the sensor, pointed at it, detects, recognises and identifies it under the stated
conditions, with the D/R/I ranges along the line of sight on request. The answers are the
bridge's own functions (crates/murmur-bridge/src/tracks/physical/predict.rs), never a Python
copy: the legacy model's ports (v2/labels.py, families.py, agents_v2.py) stay as they are for
the v2 records.

    with SenseModel() as m:
        p = m.predict({"platform": "puma", "lat": 37.82, "lon": -122.48, "alt_m": 120},
                      {"kind": "small_fast_boat", "domain": "surface", "lat": 37.82, "lon": -122.46},
                      sensor="eo_ir", conditions={"sun_elevation_deg": 45})
        p[0]["p_identify"]

What it leaves out is the running model's state: where a camera points, how long it has
looked, which scan or report a draw falls on, misclassification (docs/SENSING.md).
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from typing import Any

from . import REPO
from .sim import BINARY


class SenseError(RuntimeError):
    pass


class SenseModel:
    """A `murmur-headless --sense` process; `predict` asks it, `close` ends it."""

    def __init__(self, binary: Path | None = None, root: Path | None = None):
        binary = Path(binary or os.environ.get("MURMUR_HEADLESS_BIN") or BINARY)
        if not binary.exists():
            raise SenseError(f"no {binary}: build it with `cargo build --release -p murmur-headless`")
        self.proc = subprocess.Popen([str(binary), "--sense", "--root", str(root or REPO)], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=str(REPO))

    def ask(self, req: dict[str, Any]) -> dict[str, Any]:
        assert self.proc.stdin is not None and self.proc.stdout is not None
        self.proc.stdin.write(json.dumps(req, separators=(",", ":")).encode() + b"\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise SenseError(f"murmur-headless --sense closed (exit {self.proc.poll()})")
        out = json.loads(line)
        if not out.get("ok"):
            raise SenseError(str(out.get("error")))
        return out

    def predict(self, observer: dict[str, Any], target: dict[str, Any], sensor: str | None = None,
                conditions: dict[str, Any] | None = None, t: float | None = None, terrain: str | None = None,
                bathymetry: str | None = None, ranges: bool = False, ais_load_per_min: float = 0.0) -> list[dict[str, Any]]:
        """One prediction per sensor of the observer's platform (or of type `sensor`). `observer`:
        `platform`, `lat`, `lon`, `alt_m` (`heading_deg`, `mast_m`); `target`: `kind`, `domain`,
        `lat`, `lon` (`alt_m`, `speed_mps`, `emits`, `ais`, `signature`); `conditions`: a scenario's
        `[conditions]`; `t`: unix seconds of the scenario clock (the sun), unless the conditions
        state `sun_elevation_deg`; `terrain`, `bathymetry`: grid paths relative to the repository."""
        req: dict[str, Any] = {"observer": observer, "target": target, "ranges": ranges,
                               "ais_load_per_min": ais_load_per_min}
        for k, v in (("sensor", sensor), ("conditions", conditions), ("t", t), ("terrain", terrain),
                     ("bathymetry", bathymetry)):
            if v is not None:
                req[k] = v
        return list(self.ask(req)["predictions"])

    def close(self) -> None:
        if self.proc.poll() is None:
            assert self.proc.stdin is not None
            self.proc.stdin.close()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        if self.proc.stdout is not None:
            self.proc.stdout.close()

    def __enter__(self) -> "SenseModel":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()
