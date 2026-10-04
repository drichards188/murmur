"""Phase E in the evaluation: the engagement scenarios, and the plug-in point for live
engagement steps.

**The scenario suite (wired).** `sim/scenarios/engage/` holds one scenario per invariant of
the engagement decision chain (format: its README). `murmur-scenarios` (crates/murmur-decide)
plays each against the real decision engine, deterministically for a file, a seed and a
track-noise level, replays its audit log, and checks that a forged copy of the log is caught.
`run_suite` runs it over seeds and noise levels, keeps every result and every audit log, and
runs this harness's own invariants (eval/metrics.py) on each log, including the two-person
rule read from the log alone: an engagement dispatched, or its effect released, without
signatures from two distinct operators with engagement authority, neither the proposer, on
one frozen plan, is a violation. The batch runs the suite beside the live scenarios and the
report has a section for it.

    python -m eval.run engage --seeds 1 2 3 --noise 0 3 10 --out eval/results/engage-check

`--noise` adds seeded Gaussian noise (metres, 1-sigma) to every track estimate each second.
A scenario's designed end can stop holding at high noise (the plan check reacts to the noisy
velocity estimate: a velocity-noise follow-up is in progress in murmur-decide); the suite
reports that as a scenario that did not end as designed, separately from any invariant
violation.

**Live engagement steps (a placeholder).** A storyline step `kind = "engage"` would nominate
an armed platform against a track on the live stack through the arm's proposer (the bridge
takes `task = "engage"` with `track`, `effect_radius_m`, `window_s`, `fallback` on
`POST /api/intents` and the MCP `propose_intent`). What is missing is the decider: an
engagement needs two operators with engagement authority (`MURMUR_ENGAGE_OPERATORS`), each
asking for a picture token (`POST /api/intents/{id}/picture`) and approving with it
(`{"token": id}`). The proxy operator approves with a plain approve, which the engine
refuses for an engagement, so live engage steps stay skipped (`available()` is false) and
the four specs in `eval/scenarios/engagement/` stay placeholders (`requires = ["engage"]`).
To wire them: a second `ProxyOperator` as `op-2`, both listed in `MURMUR_ENGAGE_OPERATORS`
(eval/stack.py), signing through the picture token; `request()` below already has the
bridge's request shape.
"""

from __future__ import annotations

import json
import subprocess
import time
import tomllib
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import REPO
from .agent import ProposeError
from .bridge import Bridge
from .metrics import invariants, load_jsonl, trails
from .spec import Step
from .storyline import select_vehicles

SCENARIOS = REPO / "sim" / "scenarios" / "engage"
BINARY = REPO / "target" / "release" / "murmur-scenarios"
# fields of a result kept in the suite's summary (the replay and tamper mismatch lists can be
# long: they stay in results.jsonl)
SUMMARY_FIELDS = (
    "file", "title", "invariant", "attack", "ok", "failures", "expect", "final_state",
    "final_reason", "failed_rules", "assessment", "refusals", "fallback", "replay_ok",
    "replay_events", "tamper", "tamper_detected", "tamper_not_applicable",
)


# ---- the scenario suite ----------------------------------------------------------------------


def scenario_people(path: Path) -> tuple[set[str], set[str]]:
    """The operator ids and agent ids a scenario file declares."""
    raw = tomllib.loads(path.read_text())
    ops = {str(o["id"]) for o in raw.get("operator", []) if "id" in o}
    agents = {str(a["id"]) for a in raw.get("agent", []) if "id" in a}
    return ops, agents


def run_once(
    seed: int, noise: float, out: Path, scenarios: Path = SCENARIOS, binary: Path = BINARY, keep_logs: bool = False
) -> dict[str, Any]:
    """One pass over the suite: `murmur-scenarios --json --seed --noise --log-dir`, then this
    harness's invariants on every audit log. Writes `results.jsonl` and `logs/` under `out`;
    the log of a run that ended as designed with no violation is deleted unless `keep_logs`
    (the run is deterministic: the same seed and noise write it again)."""
    out.mkdir(parents=True, exist_ok=True)
    if not binary.exists():
        return {"seed": seed, "noise": noise, "error": f"missing {binary}: run `make build`", "results": []}
    cmd = [str(binary), "--json", "--seed", str(seed), "--noise", str(noise), "--root", str(REPO), "--log-dir", str(out / "logs"), str(scenarios)]
    t0 = time.monotonic()
    p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True, timeout=600)
    wall = round(time.monotonic() - t0, 2)
    (out / "results.jsonl").write_text(p.stdout)
    if p.returncode == 2:
        return {"seed": seed, "noise": noise, "error": p.stderr.strip()[-1000:] or "load error", "results": [], "wall_s": wall}
    results = []
    for line in p.stdout.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        stem = Path(r.get("file", "")).stem
        ops, _ = scenario_people(REPO / r["file"]) if r.get("file") and (REPO / r["file"]).exists() else (set(), set())
        events = load_jsonl(out / "logs" / f"{stem}.jsonl")
        replay = {"ran": True, "ok": bool(r.get("replay_ok")), "mismatches": len(r.get("replay_mismatches") or [])}
        inv = invariants(events, trails(events), ops, replay)
        rec = {k: r.get(k) for k in SUMMARY_FIELDS}
        rec.update(
            {
                "scenario": stem,
                "seed": seed,
                "noise": noise,
                "replay_mismatches": len(r.get("replay_mismatches") or []),
                "tamper_mismatches": len(r.get("tamper_mismatches") or []),
                "harness": {
                    "violations": inv["violations"],
                    "counts": {k: v for k, v in inv["counts"].items() if v},
                    "engage": {k: v for k, v in inv["engage"].items() if k != "bad"},
                    "engage_bad": inv["engage"]["bad"],
                    "agent_approve_attempts_refused": inv["agent_approve_attempts_refused"],
                    "events": len(events),
                },
            }
        )
        results.append(rec)
        if not keep_logs and r.get("ok") and inv["violations"] == 0:
            (out / "logs" / f"{stem}.jsonl").unlink(missing_ok=True)
    return {"seed": seed, "noise": noise, "exit": p.returncode, "wall_s": wall, "results": results, "stderr": p.stderr.strip()[-500:]}


def summarize(passes: list[dict[str, Any]]) -> dict[str, Any]:
    """Per scenario and noise level, over seeds: designed ends, replays, tampers caught, and
    this harness's invariant violations; plus totals."""
    rows: dict[tuple[str, float], dict[str, Any]] = {}
    for ps in passes:
        for r in ps["results"]:
            key = (r["scenario"], r["noise"])
            row = rows.setdefault(
                key,
                {
                    "scenario": r["scenario"], "noise": r["noise"], "title": r.get("title"), "invariant": r.get("invariant"),
                    "attack": r.get("attack"), "expect": r.get("expect"), "runs": 0, "ok": 0, "replay_ok": 0,
                    "tamper": r.get("tamper"), "tamper_runs": 0, "tamper_detected": 0, "violations": 0,
                    "released": 0, "dispatched": 0, "final_states": defaultdict(int), "failures": [],
                },
            )
            row["runs"] += 1
            row["ok"] += int(bool(r.get("ok")))
            row["replay_ok"] += int(bool(r.get("replay_ok")))
            if r.get("tamper") and not r.get("tamper_not_applicable"):
                row["tamper_runs"] += 1
                row["tamper_detected"] += int(bool(r.get("tamper_detected")))
            row["violations"] += r["harness"]["violations"]
            row["released"] += r["harness"]["engage"].get("released", 0)
            row["dispatched"] += r["harness"]["engage"].get("dispatched", 0)
            row["final_states"][r.get("final_state")] += 1
            if not r.get("ok"):
                row["failures"].append({"seed": r["seed"], "final_state": r.get("final_state"), "failure": (r.get("failures") or [""])[0]})
    table = []
    for key in sorted(rows, key=lambda k: (k[0], k[1])):
        row = rows[key]
        row["final_states"] = dict(row["final_states"])
        table.append(row)
    totals = {
        "runs": sum(r["runs"] for r in table),
        "ok": sum(r["ok"] for r in table),
        "replay_ok": sum(r["replay_ok"] for r in table),
        "tamper_runs": sum(r["tamper_runs"] for r in table),
        "tamper_detected": sum(r["tamper_detected"] for r in table),
        "violations": sum(r["violations"] for r in table),
        "released": sum(r["released"] for r in table),
        "errors": [{"seed": p["seed"], "noise": p["noise"], "error": p["error"]} for p in passes if p.get("error")],
    }
    return {"table": table, "totals": totals}


def run_suite(out: Path, seeds: list[int], noises: list[float], keep_logs: bool = False) -> dict[str, Any]:
    """The suite over seeds x noise levels into `out` (`noise-<n>/seed-<s>/`), with
    `suite.json` (every result and the summary)."""
    out.mkdir(parents=True, exist_ok=True)
    passes = []
    for noise in noises:
        for seed in seeds:
            passes.append(run_once(seed, noise, out / f"noise-{noise:g}" / f"seed-{seed}", keep_logs=keep_logs))
    suite = {"scenarios": str(SCENARIOS.relative_to(REPO)), "seeds": seeds, "noise_m": noises, "passes": passes, "summary": summarize(passes)}
    (out / "suite.json").write_text(json.dumps(suite, indent=1, default=str))
    return suite


def load_suite(batch: Path) -> dict[str, Any] | None:
    p = batch / "engage" / "suite.json"
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text())
    except ValueError:
        return None


# ---- live engagement steps (placeholder) -----------------------------------------------------


class EngageAdapter:
    """Live `engage` storyline steps. Not wired: see the module docstring."""

    def __init__(self, bridge: Bridge):
        self.bridge = bridge

    async def available(self) -> bool:
        """The bridge takes engagements (Phase E), but no supervisor here can sign one yet
        (two engagement-authority operators quoting picture tokens), so live steps are
        skipped and recorded as such."""
        return False

    @staticmethod
    def request(p: dict[str, Any], uids: list[int], track: dict[str, Any]) -> dict[str, Any]:
        """The nomination as the bridge takes it (`POST /api/intents`, MCP `propose_intent`)."""
        return {
            "task": "engage",
            "uids": uids,
            "extra": {
                "track": track.get("track_id"),
                "effect_radius_m": float(p.get("effect_radius_m", 50)),
                "window_s": int(p.get("window_s", 600)),
                "fallback": p.get("fallback", "abort"),
            },
        }

    async def run_step(self, story: Any, step: Step) -> dict[str, Any]:
        p = dict(step.params)
        sel = {"task": "engage", "vehicles": p["vehicles"], "center": {"track": p["track"]}, "rationale": p.get("rationale", "")}
        _, rationale, target, lead = await story.prepare_with_retry(step.id, {**p, **sel})
        vehicles = await story.world.vehicles()
        uids = [int(v["uid"]) for v in select_vehicles(p["vehicles"], vehicles)]
        req = self.request(p, uids, target)
        label = p.get("label", "derived")
        rec: dict[str, Any] = {
            "kind": "propose",
            "engage": True,
            "step": step.id,
            "t_rel": story.t_rel(),
            "source": {"kind": story.proposer.kind, "id": story.proposer.source_id},
            "request": req,
            "rationale": rationale,
            "label": label,
            "verify": p.get("verify", "contact_affiliation" if label == "derived" else "none"),
            "reason": p.get("reason", ""),
            "target": target,
            "vehicle": {"uid": lead.get("uid"), "callsign": (lead.get("identity") or {}).get("callsign")},
            "vehicle_truth": None,
        }
        try:
            out = await story.proposer.propose(req, rationale)
            rec.update({"intent_id": out.get("id"), "state": out.get("state"), "dispatched": out.get("dispatched")})
        except ProposeError as e:
            rec["error"] = str(e)
        return rec
