"""Rules 16, the review of murmur-533 (item 7d): what the lost contacts ask of an operator over a
run. A lost contact stays in force until an operator accepts it for one engagement or confirms
a track carrying its AIS as it; with no cap on its reach, the contacts lost over a run pile up,
and every nomination may need each of them accepted. How many does an operator face two hours
in, and how often does a nomination need an acceptance (and of how many)?

The model (stated; a model result, never evidence about people or the sea):

- The keep-out measurement's field (eval/keepout.py: protected traffic Poisson at `density` per
  km^2 within 8 km of a holding target, declared hostile).
- Each contact goes out of view at `rate` per contact-hour, at a uniform time over the run; the
  tracker drops it 180 s later and it is a lost contact from then on, never confirmed (radar or
  EO/IR: no AIS to claim it by; an AIS contact seen again stays lost until an operator confirms
  the claim, which this leaves out: the burden's upper case) and never accepted for good (an
  acceptance covers one engagement). Its last fix: uniform in the field, holding (30%) or on a
  straight course at 3 to 12 m/s, 8 to 25 m 1-sigma, tracked long enough for an average
  velocity: the oov variants' traffic. The rate the oov variants assume (30% of contacts out of
  view within 15 minutes) is 1.2 per contact-hour; 0.1 and 0.3 are calmer water.
- At `at_min` minutes into the run, a nomination with the effect 60 or 120 s on (a magura or a
  switchblade placed for it, as the keep-out measurement does); the engine's own keep-out check
  (eval/keepout_probe, rules 16) says which lost contacts block it. No live track but the target
  (what the live traffic adds is in the keep-out measurement), none of ours.

    python -m eval.lost_burden --seeds 1 2 3 4 5 6 7 8 9 10 --situations 50
"""

from __future__ import annotations

import argparse
import json
import math
import random
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import keepout as K

#: losses per contact-hour (1.2: the oov variants' assumption)
RATES = (0.1, 0.3, 1.2)
DENSITIES = (0.03, 0.1, 0.3)
#: minutes into the run
AT_MIN = (15, 30, 60, 90, 120)
TTE_S = (60, 120)
RULES = (K.OOV_LOST,)


def _poisson(rng: random.Random, lam: float) -> int:
    # Knuth for small means, a rounded normal for large ones
    if lam > 50.0:
        return max(0, int(round(rng.gauss(lam, math.sqrt(lam)))))
    k, p, limit = 0, 1.0, math.exp(-lam)
    while True:
        p *= rng.random()
        if p <= limit:
            return k
        k += 1


def situation(seed: int, density: float, rate: float, at_min: int, index: int, cruise: dict[str, float]) -> dict[str, Any]:
    """One nomination `at_min` into a run: the probe's input, with the lost contacts in force."""
    rng = random.Random(f"lost burden {seed} {density:g} {rate:g} {at_min} {index}")
    origin = (rng.uniform(*K.CENTRE_BOX[0]), rng.uniform(*K.CENTRE_BOX[1]))
    run_s = at_min * 60.0
    target = K.facts(K.Hold((0.0, 0.0)), 900.0, rng.uniform(*K.SIGMA_P), K.VEL_SIGMA, rng, origin, "hostile", 0.9)
    target.update({"declared_by": "op-1", "identity": "small_fast_boat", "identity_confidence": 0.8, "confidence": 0.95})
    n = K._poisson(rng, density * math.pi * (K.FIELD_M / 1000.0) ** 2)
    losses = _poisson(rng, n * rate * run_s / 3600.0)
    lost: dict[str, Any] = {}
    for i in range(losses):
        seen = rng.uniform(0.0, run_s)  # its last fix, s into the run
        dropped = seen + K.DROP_S
        if dropped > run_s:
            continue  # still a live track out of view at the nomination, not lost yet
        p = K._disc(rng, K.FIELD_M)
        m: Any = K.Hold(p) if rng.random() < 0.3 else K.Line(p, K._vec(rng.uniform(3.0, 12.0), K._course(rng)))
        f = K.facts(m, rng.uniform(300.0, 900.0), rng.uniform(*K.SIGMA_P), K.VEL_SIGMA, rng, origin, "neutral" if rng.random() < 0.6 else "unknown", rng.uniform(0.3, 0.9))
        # its facts as at its last fix, `run_s - seen` before the nomination
        back = int(round((run_s - seen) * 1000.0))
        for k in ("last_seen_t", "first_seen_t"):
            f[k] -= back
        f["position"]["t"] -= back
        f["affiliation"]["t"] -= back
        if "manoeuvre" in f:
            f["manoeuvre"]["t"] -= back
        lost[f"L-{i + 1:04d}"] = {"dropped_t": K.T0_MS - int(round((run_s - dropped) * 1000.0)), "facts": f}
    evals = []
    for tte in TTE_S:
        plat = rng.choice(K.PLATFORMS)
        d = max(0.0, (float(tte) - K.DISPATCH_S) * cruise[plat])
        b = K._course(rng)
        evals.append({"key": f"tte-{tte}", "platform": plat, "at": K.to_lonlat(origin, d * math.sin(b), d * math.cos(b))})
    return {
        "id": f"{seed}-{density:g}-{rate:g}-{at_min}-{index}",
        "t_ms": K.T0_MS,
        "target": "T-0000",
        "radius_m": K.RADIUS_M,
        "fallback": "return",
        "tracks": {"T-0000": target},
        "vehicles": [],
        "evals": evals,
        "lost": lost,
        "_meta": {"seed": seed, "density": density, "rate": rate, "at_min": at_min, "contacts": n, "losses": losses},
    }


def run(seeds: list[int], situations: int, probe: Path = K.PROBE) -> dict[str, Any]:
    """Every cell's nominations through the probe; per (rate, density, minutes into the run):
    the lost contacts in force, the share of nominations the keep-out on tracks blocks (with no
    live track but the target, every block needs an acceptance), and how many lost contacts each
    blocked nomination needs accepted."""
    cruise = K.cruise_speeds()
    sits = [
        situation(seed, d, r, m, i, cruise)
        for seed in seeds
        for d in DENSITIES
        for r in RATES
        for m in AT_MIN
        for i in range(situations)
    ]
    meta = {s["id"]: s.pop("_meta") for s in sits}
    results = K.run_probe(sits, RULES, probe)
    cells: dict[tuple, dict[str, list]] = defaultdict(lambda: defaultdict(list))
    errors = 0
    for res in results:
        if "eval" not in res:
            continue
        if res.get("error") or not res.get("preview"):
            errors += 1
            continue
        m = meta[res["id"]]
        r = res["rules"][str(K.OOV_LOST)]
        c = cells[(m["rate"], m["density"], m["at_min"])]
        n_lost = r.get("n_lost") or 0
        blocked = r.get("kot") != "pass"
        c["lost"].append(n_lost)
        c["blocked"].append(int(blocked))
        c["to_accept"].append(r.get("n_lost_intr") or 0)
        c["accepted_passes"].append(int((r.get("lost_variants") or {}).get("accepted", r.get("kot")) == "pass"))
        if r.get("mismatch"):
            c["mismatch"].append(1)
    rows = []
    for (rate, density, at_min), c in sorted(cells.items()):
        n = len(c["blocked"])
        blocked = [a for a, b in zip(c["to_accept"], c["blocked"]) if b]
        rows.append(
            {
                "rate": rate,
                "density": density,
                "at_min": at_min,
                "n": n,
                "lost_mean": round(statistics.fmean(c["lost"]), 1),
                "lost_p90": sorted(c["lost"])[int(0.9 * (len(c["lost"]) - 1))],
                "blocked": K._rate(sum(c["blocked"]), n),
                "to_accept_mean": round(statistics.fmean(blocked), 1) if blocked else 0.0,
                "to_accept_max": max(blocked) if blocked else 0,
                "accepted_passes": K._rate(sum(c["accepted_passes"]), n),
                "mismatches": len(c["mismatch"]),
            }
        )
    return {"seeds": seeds, "situations": situations, "rates": RATES, "densities": DENSITIES, "at_min": AT_MIN, "tte_s": TTE_S, "errors": errors, "rows": rows}


def table(s: dict[str, Any]) -> str:
    """The burden as plain text."""
    lines = [
        "Lost contacts over a run (rules 16, model): per losses/contact-hour, density and minutes in: lost contacts in force (mean, p90),",
        "nominations blocked on tracks % (each needs an acceptance), lost contacts to accept per blocked nomination (mean, max), passes with all accepted %",
    ]
    for r in s["rows"]:
        lines.append(
            f"  {r['rate']:>4g}/h {r['density']:>5g}/km2 {r['at_min']:>4} min  lost {r['lost_mean']:>6} (p90 {r['lost_p90']:>4})  "
            f"blocked {K._pct(r['blocked'], False):>4}  accept {r['to_accept_mean']:>6} (max {r['to_accept_max']:>4})  all accepted pass {K._pct(r['accepted_passes'], False):>4}"
            + (f"  MISMATCH {r['mismatches']}" if r["mismatches"] else "")
        )
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(prog="python -m eval.lost_burden", description=__doc__.split("\n\n")[0])
    p.add_argument("--seeds", nargs="+", type=int, default=list(range(1, 11)))
    p.add_argument("--situations", type=int, default=50, help="nominations per seed and cell (default 50)")
    p.add_argument("--out", help="write the summary here (JSON)")
    a = p.parse_args()
    s = run(list(a.seeds), a.situations)
    if a.out:
        Path(a.out).parent.mkdir(parents=True, exist_ok=True)
        Path(a.out).write_text(json.dumps(s, indent=1))
    print(table(s))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
