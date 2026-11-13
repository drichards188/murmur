"""Rules 30's headless check (docs/NODE-LOSS.md section 8, stage 2; bd murmur-zb1d.2): "a silent
vehicle's true position stays inside its region".

The 22-type fleet at the Golden Gate (eval/headless/node_loss.toml: one of each platform type, on
the world's land/water grid and currents, each on an order of a kind it can run, or none), every
link cut both ways for ten minutes from `--cut-s`. Every second of the cut the simulator answers
each silent vehicle's region as the engine counts it now and where its stand-in really is (the
`silent` request, crates/murmur-headless/src/sim/silent.rs): its margin, how far inside its region
the truth is (negative: outside). A silent vehicle's facts and the orders that may be running on
it stay as they were at its last report, so its region for an effect at t is the same whatever
time before t a decision computes it, and the check of the region now against the truth now
covers every prediction a decision makes during the silence.

Three times a second: with each vehicle's own lost-link failsafe as the bridge read it (`bridge`,
docs/NODE-LOSS.md stage 4), as this check maps the stand-in's parameters itself (`standins`, a
check on that read), and not known (`none`: every action its autopilot has counts).

`--dir both` (the default) cuts both ways: each vehicle loses us too, and its own GCS failsafe
acts as its firmware runs it (crates/murmur-standins/src/failsafe.rs; most of the fleet's
return home). `--dir up` cuts only what the vehicle sends: we lose it, it still hears us, so no
failsafe fires and it runs its order on (the hypothesis that it runs its order, alone).

    MURMUR_RULES=30 .venv/bin/python -m eval.headless.node_loss --out logs/node-loss/both.json
    MURMUR_RULES=30 .venv/bin/python -m eval.headless.node_loss --dir up --out logs/node-loss/up.json

The decision rules being built (30) are asked for by `MURMUR_RULES`, which eval/headless/sim.py
passes on (the driver sets it when unset).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any

from .episode import Episode, Options
from .situations import from_situation

SITUATION = Path(__file__).with_name("node_loss.toml")
#: when the links are cut, and for how long, s
CUT_S = 300.0
CUT_FOR_S = 600.0
PLANS = ("bridge", "standins", "none")


class Check(Episode):
    """An episode whose sampler cuts every link at `cut_s` and reads the regions every second."""

    def __init__(self, *args: Any, cut_s: float = CUT_S, cut_for_s: float = CUT_FOR_S, cut_dir: str = "both",
                 **kw: Any):
        super().__init__(*args, **kw)
        self.cut_dir = cut_dir
        self.cut_s = cut_s
        self.cut_for_s = cut_for_s
        self.cut_sent = False
        self.samples: dict[str, list[dict[str, Any]]] = {p: [] for p in PLANS}
        self.intents_at_cut: Any = None

    def sample(self, now: float) -> float | None:
        nxt = super().sample(now)
        if not self.cut_sent and now >= self.cut_s:
            uids = [int(v["uid"]) for v in self.planned]
            ev = {"name": "node-loss check: every link cut", "at_s": round(now, 3), "duration_s": self.cut_for_s,
                  "uids": uids, "cut": True, "dir": self.cut_dir}
            self.sim.fault("link_event", event=ev)
            self.cut_sent = True
            status, body = self.sim.api("GET", "/api/intents")
            self.intents_at_cut = body if status == 200 else {"status": status}
        if self.cut_sent and now <= self.cut_s + self.cut_for_s:
            for p in PLANS:
                out = self.sim.rpc({"op": "silent", "plans": p})
                for v in out.get("silent", []):
                    self.samples[p].append({"t": round(now, 3), **v})
        return nxt


#: the silences the margins are summed up over, s
SPANS = ((0, 60), (60, 300), (300, 601))


def _median(xs: list[float]) -> float | None:
    if not xs:
        return None
    xs = sorted(xs)
    n = len(xs)
    return round(xs[n // 2] if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) / 2.0, 1)


def summary(samples: list[dict[str, Any]]) -> dict[str, Any]:
    """Per vehicle: how many seconds silent it was checked, its worst margin and what set it, and
    its median margin over each stretch of silence (how far inside its region the truth was: a
    loose region shows as a large one); the samples outside their region."""
    by: dict[str, dict[str, Any]] = {}
    spans: dict[str, dict[str, list[float]]] = {}
    outside: list[dict[str, Any]] = []
    for s in samples:
        v = by.setdefault(s["callsign"], {"uid": s["uid"], "n": 0, "worst_m": None, "at": None})
        v["n"] += 1
        if v["worst_m"] is None or s["margin_m"] < v["worst_m"]:
            v["worst_m"] = round(s["margin_m"], 1)
            v["at"] = {k: s.get(k) for k in ("t", "span_s", "hypothesis", "kind", "what", "reach_m", "mode", "truth", "p0")}
        for lo, hi in SPANS:
            if lo <= s["span_s"] < hi:
                spans.setdefault(s["callsign"], {}).setdefault(f"{lo}-{hi - 1}", []).append(s["margin_m"])
        if s["margin_m"] < 0.0:
            outside.append(s)
        # the bridge's read of its failsafe against the stand-in's own parameters
        if s.get("read_agrees") is False:
            v["read_differs"] = v.get("read_differs", 0) + 1
    for cs, v in by.items():
        v["median_m"] = {k: _median(xs) for k, xs in spans.get(cs, {}).items()}
    worst = min((v["worst_m"] for v in by.values()), default=None)
    return {
        "vehicles": by,
        "samples": len(samples),
        "outside": len(outside),
        "outside_first": outside[:40],
        "worst_m": worst,
        "read_differs": {cs: v["read_differs"] for cs, v in by.items() if v.get("read_differs")},
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cut-s", type=float, default=CUT_S)
    ap.add_argument("--cut-for-s", type=float, default=CUT_FOR_S)
    ap.add_argument("--dir", choices=("both", "up"), default="both",
                    help="both: the vehicles lose us too (their failsafes act); up: only we lose them")
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--keep", type=Path, default=None, help="copy the audit log and the process log here")
    a = ap.parse_args(argv)
    os.environ.setdefault("MURMUR_RULES", "30")
    spec = from_situation(SITUATION)
    spec.duration_s = a.cut_s + a.cut_for_s + 30.0
    with tempfile.TemporaryDirectory(prefix="node-loss-") as d:
        ep = Check(spec, a.seed, Path(d), Options(agent="none", sample_s=1.0, keep=a.keep),
                   cut_s=a.cut_s, cut_for_s=a.cut_for_s, cut_dir=a.dir)
        record = ep.run()
    out = {
        "seed": a.seed,
        "rules": os.environ["MURMUR_RULES"],
        "cut": {"from_s": a.cut_s, "for_s": a.cut_for_s, "dir": a.dir},
        "orders": record.get("orders"),
        "intents_at_cut": ep.intents_at_cut,
        **{p: summary(ep.samples[p]) for p in PLANS},
    }
    text = json.dumps(out, indent=1, default=str)
    if a.out:
        a.out.parent.mkdir(parents=True, exist_ok=True)
        a.out.write_text(text)
    for p in PLANS:
        s = out[p]
        print(f"plans {p}: {s['samples']} samples, {s['outside']} outside their region, worst margin {s['worst_m']} m; "
              f"the bridge's read differs from the stand-in's parameters for {s['read_differs'] or 'none'}")
        for cs, v in sorted(s["vehicles"].items(), key=lambda kv: kv[1]["worst_m"]):
            at = v["at"] or {}
            med = "  ".join(f"{k} s {m:7.0f}" for k, m in v["median_m"].items() if m is not None)
            print(f"  {cs:14} {v['n']:4} s  worst {v['worst_m']:9.1f} m  at {at.get('span_s', 0):6.0f} s silent  "
                  f"{at.get('kind')}: {(at.get('what') or '')[:70]}")
            print(f"  {'':14} median margin  {med}")
    return 0 if all(out[p]["outside"] == 0 for p in PLANS) else 1


if __name__ == "__main__":
    sys.exit(main())
