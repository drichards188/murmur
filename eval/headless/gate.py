"""The gates of eval/headless/CRITERIA.md: G1 (fidelity on Harbor watch against the full stack)
and G1b (speed). The definitions are CRITERIA.md's; this module only computes them.

    .venv/bin/python -m eval.headless.gate g1 --full logs/headless-g1/gate --seeds 1-10 --out logs/headless-g1/g1.json
    .venv/bin/python -m eval.headless.gate g1b --seeds 1-200 --workers 14
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import tempfile
from pathlib import Path
from typing import Any

from eval.stats import t_interval, wilson

from . import REPO
from .batch import parse_seeds
from .metrics import episode_metrics

WINDOW_S = 10.0
FLOOR = {"time_to_look": 2.0, "false_looks": 0.5}


def match(h: list[dict[str, Any]], f: list[dict[str, Any]], window: float = WINDOW_S) -> list[tuple[int, int]]:
    """CRITERIA.md's pairing: in time order, each headless proposal with the earliest unpaired
    full-stack one of the same play, truth target (not ambiguous) and vehicle within `window` s."""
    pairs = []
    used: set[int] = set()
    for i, p in sorted(enumerate(h), key=lambda x: (x[1]["t"] is None, x[1]["t"] or 0)):
        if p["truth_target"] in (None, "ambiguous") or p["t"] is None:
            continue
        cands = [(q["t"], j) for j, q in enumerate(f) if j not in used and q["t"] is not None
                 and q["play"] == p["play"] and q["truth_target"] == p["truth_target"] and q["uid"] == p["uid"]
                 and abs(q["t"] - p["t"]) <= window]
        if cands:
            j = min(cands)[1]
            used.add(j)
            pairs.append((i, j))
    return pairs


def interval(kind: str, name: str, values: list[Any]) -> dict[str, Any]:
    """A side's mean and its 95 % interval, CRITERIA.md's way."""
    if kind == "proportion":
        k, n = sum(v[0] for v in values), sum(v[1] for v in values)
        if n == 0:
            return {"mean": None, "lo": None, "hi": None, "n": 0}
        p, lo, hi = wilson(k, n)
        return {"mean": k / n, "lo": lo, "hi": hi, "n": n, "k": k}
    xs = [float(v) for v in values if v is not None]
    if not xs:
        return {"mean": None, "lo": None, "hi": None, "n": 0}
    m = statistics.fmean(xs)
    if len(xs) >= 2:
        _, lo, hi = t_interval(xs)
    else:
        lo = hi = m
    half = max((hi - lo) / 2.0, FLOOR[name])
    return {"mean": m, "lo": m - half, "hi": m + half, "n": len(xs), "t_lo": lo, "t_hi": hi}


def inside(a: dict[str, Any], b: dict[str, Any]) -> bool | None:
    """Side `a`'s mean inside side `b`'s interval (both empty: passes; one empty: fails)."""
    if a["mean"] is None and b["mean"] is None:
        return True
    if a["mean"] is None or b["mean"] is None:
        return False
    return b["lo"] - 1e-9 <= a["mean"] <= b["hi"] + 1e-9


def g1(headless: dict[int, dict[str, Any]], full: dict[int, dict[str, Any]]) -> dict[str, Any]:
    seeds = sorted(set(headless) & set(full))
    per_seed = []
    M = H = F = 0
    sides: dict[str, dict[str, list[Any]]] = {"headless": {}, "full": {}}
    for s in seeds:
        mh, mf = headless[s], full[s]
        ph, pf = mh["proposals"], mf["proposals"]
        pairs = match(ph, pf)
        M += len(pairs)
        H += len(ph)
        F += len(pf)
        per_seed.append({"seed": s, "headless": [(p["t"], p["play"], p["truth_target"], p["uid"]) for p in ph],
                         "full": [(p["t"], p["play"], p["truth_target"], p["uid"]) for p in pf],
                         "pairs": pairs})
        for side, m in (("headless", mh), ("full", mf)):
            d = sides[side]
            d.setdefault("look_covered", []).append((int(bool(m["look_covered"])), 1) if m["look_covered"] is not None else (0, 0))
            d.setdefault("time_to_look", []).append(m["time_to_look_first"])
            d.setdefault("false_looks", []).append(m["false_looks"])
            d.setdefault("precision", []).append((m["precision"]["legit"], m["precision"]["n"]))
    agreement = M / (H + F - M) if (H + F - M) else None
    kinds = {"look_covered": "proportion", "precision": "proportion", "time_to_look": "time", "false_looks": "count"}
    metrics = {}
    for name, kind in kinds.items():
        ih = interval(kind, name, sides["headless"][name]) if seeds else None
        iff = interval(kind, name, sides["full"][name]) if seeds else None
        ok = bool(seeds) and inside(ih, iff) and inside(iff, ih)  # type: ignore[arg-type]
        metrics[name] = {"headless": ih, "full": iff, "pass": ok}
    passed = (agreement is not None and agreement >= 0.80 and all(v["pass"] for v in metrics.values())
              and len(seeds) >= 8)
    return {"seeds": seeds, "agreement": agreement, "pairs": M, "headless_proposals": H, "full_proposals": F,
            "metrics": metrics, "per_seed": per_seed, "pass": passed}


def headless_records(seeds: list[int], sits: Path, keep: Path | None) -> dict[int, dict[str, Any]]:
    from .episode import Episode, Options
    from .situations import Calibration, from_situation

    out = {}
    calib = Calibration.load()
    for s in seeds:
        e = from_situation(sits / f"harbor-watch-seed{s}.toml")
        with tempfile.TemporaryDirectory(prefix="g1-") as td:
            rec = Episode(e, s, Path(td), Options(calibration=calib, keep=(keep / f"seed-{s}") if keep else None)).run()
        if keep:
            r = dict(rec)
            (keep / f"seed-{s}").mkdir(parents=True, exist_ok=True)
            (keep / f"seed-{s}" / "record.json").write_text(json.dumps({k: v for k, v in r.items() if k != "audit_text"}, sort_keys=True))
        out[s] = episode_metrics(rec)
    return out


def full_records(root: Path, seeds: list[int]) -> dict[int, dict[str, Any]]:
    from .fullstack import record

    out = {}
    for s in seeds:
        d = root / f"seed-{s}"
        if d.exists():
            out[s] = episode_metrics(record(d))
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("g1")
    a.add_argument("--full", type=Path, required=True, help="the full-stack runs (fullstack.py run --out)")
    a.add_argument("--seeds", nargs="+", default=["1-10"])
    a.add_argument("--out", type=Path, default=None)
    a.add_argument("--keep", type=Path, default=None, help="keep each headless episode's record and audit log here")
    args = ap.parse_args(argv)
    seeds = parse_seeds(args.seeds)
    full_root = args.full if args.full.is_absolute() else REPO / args.full
    full = full_records(full_root, seeds)
    head = headless_records(sorted(full), full_root / "situations", args.keep)
    res = g1(head, full)
    res["calibration"] = __import__("eval.headless.situations", fromlist=["Calibration"]).Calibration.load().as_dict()
    text = json.dumps(res, indent=1, default=str)
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text)
    print(json.dumps({k: res[k] for k in ("seeds", "agreement", "pairs", "headless_proposals", "full_proposals", "pass")}))
    for k, v in res["metrics"].items():
        print(k, json.dumps(v, default=str))
    for ps in res["per_seed"]:
        print("seed", ps["seed"])
        print("  headless:", ps["headless"])
        print("  full:    ", ps["full"])
    return 0 if res["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
