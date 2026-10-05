"""Rules 9 in the evaluation: rule-based identification against manual declaration, on the
same scenarios and seeds. MODEL RESULTS FROM SCRIPTED PROXIES, NEVER HUMAN DATA.

**What runs.** The identification scenarios of `sim/scenarios/engage/` (every file with an
`act` step: a scripted hostile act, which the engine attributes to the tracks in the picture
and then declares by rule or flags for review) are played by `murmur-scenarios --json`
against the real decision engine, once per arm and seed, with seeded noise on where each act's
author was observed (`--act-noise`, the act's own sigma) so the attribution varies across
seeds as it would across sensor draws. Arms:

| arm | identification | who declares |
| --- | --- | --- |
| `rule` | `rule_declaration = true` (the shipped policy) | the engine's rule, when the evidence is clear; flags otherwise |
| `rule-nominate` | as `rule`, plus `nominate_unconfirmed = true` (`eval/policy/minimal.toml`) | as `rule`; a rule declaration may be nominated before anyone confirms it |
| `manual` | `rule_declaration = false` (rules 7 behaviour) | a scripted operator proxy, by the rule's own evidence standard, `--manual-delay` seconds (median) after the act |
| `manual-best` | `rule_declaration = false` | the proxy declares the most likely track of every observed act (a best guess): a sensitivity case |

The manual proxy's delay defaults to 16 s, lognormal sigma 0.4: half the proxy operator's
look interval plus its time to read a card (`eval/proxy.py` defaults, 20 s and 6 s). It is a
model of latency, not of judgment: with the rule's standard it declares exactly what the rule
would have, later; `manual-best` shows what a looser human standard would cost.

**Labels (truth, never shown to a decider).** Each run's `ident` records carry the truth the
scenario file states: whether a declared track's contact is hostile (`truth`), and which track
committed each act (or that its author was untracked). A declaration is *correct* when it names
the act's real author and that author is hostile; a *false hostile declaration* names a track
whose contact is not hostile; a *wrong-author declaration* names a hostile that did not commit
the act. A *missed hostile act* is an observed act by a tracked hostile whose track nobody
declared hostile by the end of the run. Every wrong declaration is sorted by what the C2 showed
when it was made: *the C2 showed it* (another track shared the attribution at 0.1 or more, the
declared track's own estimate was neutral or friendly, or a review flag was on it) or *picture
wrong* (the picture pointed cleanly at the wrong track: the author untracked, or the noise put
the act on a neighbour). The two are never pooled. Every flag is sorted the same way by what a
declaration of its best candidate would have been: a flag that *prevented* a wrong declaration,
or one that *held back* a right one (a person's look the rule could have saved).

**Caveats.** The scenario timelines are scripted: nominations, confirmations, revocations and
flag resolutions happen at fixed seconds whatever the arm, so engagement outcomes across arms
mostly measure whether a declaration existed at the nomination's second (latency), and the
"human resolutions" are the scenario's scripted operator acts. The tracks are the scenario
runner's (truth plus seeded noise), not the bridge's tracker; the live bridge's act sensing is
exercised by the live runs (eval README), not here.

    python -m eval.run ident --seeds 1 2 3 4 5 6 7 8 9 10
"""

from __future__ import annotations

import json
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import REPO
from .metrics import load_jsonl

SCENARIOS = REPO / "sim" / "scenarios" / "engage"
BINARY = REPO / "target" / "release" / "murmur-scenarios"
#: below this attribution a candidate is not worth naming (the engine's FLAG_FLOOR)
FLAG_FLOOR = 0.1
#: the manual proxy's median delay, s: half the proxy's look interval plus its read time
MANUAL_DELAY_S = 16.0

ARMS: dict[str, list[str]] = {
    "rule": ["--ident", "on"],
    "rule-nominate": ["--ident", "on", "--nominate-unconfirmed"],
    "manual": ["--ident", "off", "--manual-delay", "{delay}"],
    "manual-best": ["--ident", "off", "--manual-delay", "{delay}", "--manual-standard", "best"],
}
DEFAULT_ARMS = ("rule", "rule-nominate", "manual", "manual-best")
NOT_HUMAN = (
    "Model results from scripted proxies on scripted scenarios: the manual arm's declarer is a "
    "latency model with a fixed evidence standard, the scenario timelines are scripted, and the "
    "labels are the scenario files' truth. Not human data and not evidence about people."
)


def ident_files(scenarios: Path = SCENARIOS) -> list[Path]:
    """The scenario files with a scripted hostile act."""
    return sorted(p for p in scenarios.glob("*.toml") if 'action = "act"' in p.read_text())


def arm_args(arm: str, delay: float, threshold: float | None) -> list[str]:
    args = [a.replace("{delay}", f"{delay:g}") for a in ARMS[arm]]
    if threshold is not None:
        args += ["--threshold", f"{threshold:g}"]
    return args


def run_arm(
    arm: str,
    seed: int,
    out: Path,
    files: list[Path],
    delay: float = MANUAL_DELAY_S,
    threshold: float | None = None,
    binary: Path = BINARY,
) -> dict[str, Any]:
    """One arm, one seed: every identification scenario, act noise on, logs kept under `out`."""
    out.mkdir(parents=True, exist_ok=True)
    if not binary.exists():
        return {"arm": arm, "seed": seed, "error": f"missing {binary}: run `make build`", "results": []}
    cmd = [str(binary), "--json", "--seed", str(seed), "--act-noise", *arm_args(arm, delay, threshold)]
    cmd += ["--root", str(REPO), "--log-dir", str(out / "logs"), *map(str, files)]
    t0 = time.monotonic()
    p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True, timeout=600)
    (out / "results.jsonl").write_text(p.stdout)
    if p.returncode == 2:
        return {"arm": arm, "seed": seed, "error": p.stderr.strip()[-1000:] or "load error", "results": []}
    results = []
    for line in p.stdout.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        stem = Path(r.get("file", "")).stem
        events = load_jsonl(out / "logs" / f"{stem}.jsonl")
        results.append(analyze(r, events, arm, seed))
    return {"arm": arm, "seed": seed, "wall_s": round(time.monotonic() - t0, 2), "results": results, "stderr": p.stderr.strip()[-500:]}


# ---- one run ---------------------------------------------------------------------------------


def _acts_in_log(events: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Each act's candidates and the picture it was attributed in, from the audit log."""
    out = {}
    for e in events:
        if e.get("type") == "ident.act":
            act = e.get("act") or {}
            out[act.get("act")] = {"candidates": e.get("candidates") or [], "tracks": (e.get("inputs") or {}).get("tracks") or {}}
    return out


def _engagements(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Every engagement nominated: its target, how the target was declared when it was
    nominated (operator, rule confirmed, rule unconfirmed, none), and how it ended."""
    decl: dict[str, dict[str, Any]] = {}
    out: dict[int, dict[str, Any]] = {}
    for e in events:
        t = e.get("type")
        if t == "ident.declared":
            d = e.get("declaration") or {}
            by = d.get("by") or {}
            decl[d.get("track")] = {"basis": "rule" if by.get("kind") == "rule" else "operator", "confirmed": False}
        elif t == "ident.confirmed":
            if e.get("track") in decl:
                decl[e["track"]]["confirmed"] = True
        elif t == "ident.revoked":
            decl.pop(e.get("track"), None)
        elif t == "intent.created":
            i = e.get("intent") or {}
            if i.get("kind") == "engage":
                track = (i.get("params") or {}).get("track")
                d = decl.get(track)
                basis = "none" if d is None else d["basis"] if d["basis"] == "operator" else ("rule (confirmed)" if d["confirmed"] else "rule (unconfirmed)")
                out[i.get("id")] = {"intent": i.get("id"), "track": track, "basis": basis, "final": "proposed", "effect": False}
        elif t == "intent.state" and e.get("intent") in out:
            out[e["intent"]]["final"] = e.get("to")
        elif t == "engage.effect" and e.get("intent") in out:
            out[e["intent"]]["effect"] = True
    return list(out.values())


def analyze(result: dict[str, Any], events: list[dict[str, Any]], arm: str, seed: int) -> dict[str, Any]:
    """One scenario run: its identification events labelled with truth, and the counts."""
    recs = result.get("ident") or []
    log_acts = _acts_in_log(events)
    acts = [r for r in recs if r["kind"] == "act"]
    act_by_id = {r.get("act"): r for r in acts}
    # a flag is logged once per track it names, its best candidate first
    flagged_tracks = {r.get("track") for r in recs if r["kind"] == "flag" and r.get("track")}
    flags: list[dict[str, Any]] = []
    for r in recs:
        if r["kind"] == "flag" and all(f["detail"] != r["detail"] for f in flags):
            flags.append(r)
    declarations = []
    for r in recs:
        if r["kind"] not in ("rule_declaration", "operator_declaration") or r.get("detail") != "hostile":
            continue
        act = r.get("act")
        if r["kind"] == "operator_declaration" and not act:
            # a scripted operator declaration the timeline makes, not from an act
            declarations.append({**r, "source": "scripted", "verdict": "scripted"})
            continue
        source = "rule" if r["kind"] == "rule_declaration" else "manual proxy"
        a = act_by_id.get(act) or {}
        if r.get("truth_actor") and r.get("truth_hostile"):
            verdict = "correct"
        elif r.get("truth_hostile") is False:
            verdict = "false_hostile"
        else:
            verdict = "wrong_author"
        shown = None
        if verdict != "correct":
            la = log_acts.get(act) or {}
            others = [c for c in la.get("candidates", []) if c.get("track") != r.get("track") and c.get("confidence", 0) >= FLAG_FLOOR]
            est = ((la.get("tracks") or {}).get(r.get("track")) or {}).get("affiliation") or {}
            est = est.get("value") if isinstance(est, dict) else est
            shown = bool(others) or est in ("neutral", "assumed_friend") or r.get("track") in flagged_tracks
        declarations.append(
            {
                **r,
                "source": source,
                "verdict": verdict,
                "c2_showed_it": shown,
                "delay_s": None if a.get("act_t_s") is None else round(r["t_s"] - a["act_t_s"], 1),
            }
        )
    # each act by a tracked hostile: was its author declared, and how soon
    per_act = []
    for a in acts:
        actor = a.get("actor")
        hostile_author = actor not in (None, "", "untracked") and a.get("truth_hostile") is not False
        right = [d for d in declarations if d.get("track") == actor and d["verdict"] in ("correct", "scripted") and d["t_s"] >= a["t_s"] - 0.5]
        earlier = [d for d in declarations if d.get("track") == actor and d["t_s"] < a["t_s"] - 0.5]
        # the declaration this act led to (the rule's, or the manual proxy's reading it)
        from_act = [d for d in right if d.get("act") == a.get("act")]
        first = min(from_act, key=lambda d: d["t_s"]) if from_act else None
        per_act.append(
            {
                "act": a.get("act"),
                "actor": actor,
                "hostile_author": hostile_author,
                "attribution_best": a.get("attribution"),
                "best_is_author": a.get("truth_actor"),
                "verdict": a.get("detail"),
                "declared_by": None if first is None else first["source"],
                "time_to_declaration_s": None if first is None else round(first["t_s"] - a["t_s"], 1),
                "already_declared": bool(earlier),
                # nobody (the rule, the proxy, or a scripted operator) declared its author
                "missed": hostile_author and not right and not earlier,
            }
        )
    # each flag: would declaring its best candidate have been right?
    flag_rows = []
    for f in flags:
        a = act_by_id.get(f.get("act")) or {}
        if f.get("act"):
            would_be_right = bool(a.get("truth_actor")) and a.get("truth_hostile") is not False and a.get("actor") not in (None, "untracked")
            kind = "held back a right one" if would_be_right else "prevented a wrong one"
        else:
            kind = "proximity (no act)"
        flag_rows.append({**f, "effect": kind})
    resolved = [r for r in recs if r["kind"] == "flag_resolved"]
    return {
        "scenario": Path(result.get("file", "")).stem,
        "arm": arm,
        "seed": seed,
        "ok": result.get("ok"),
        "replay_ok": result.get("replay_ok"),
        "tamper_detected": result.get("tamper_detected"),
        "acts": per_act,
        "declarations": declarations,
        "flags": flag_rows,
        "flag_resolutions": [(r.get("detail") or "? ?").split()[-1] for r in resolved],
        "confirmations": sum(1 for r in recs if r["kind"] == "confirmed"),
        "revocations": sum(1 for r in recs if r["kind"] == "revoked"),
        "engagements": _engagements(events),
        "events": len(events),
    }


# ---- over runs --------------------------------------------------------------------------------


def _median(xs: list[float]) -> float | None:
    return round(statistics.median(xs), 1) if xs else None


def summarize(passes: list[dict[str, Any]]) -> dict[str, Any]:
    """Per arm, over scenarios and seeds: the counts the comparison reads, and per scenario."""
    by_arm: dict[str, dict[str, Any]] = {}
    by_cell: dict[tuple[str, str], dict[str, Any]] = {}

    def blank() -> dict[str, Any]:
        return {
            "runs": 0,
            "replay_ok": 0,
            "acts": 0,
            "hostile_acts": 0,
            "missed_hostile_acts": 0,
            "declarations": defaultdict(int),
            "correct": 0,
            "false_hostile": 0,
            "wrong_author": 0,
            "wrong_c2_showed_it": 0,
            "wrong_picture_wrong": 0,
            "flags": 0,
            "flags_prevented_wrong": 0,
            "flags_held_back_right": 0,
            "flags_proximity": 0,
            "flag_resolutions": defaultdict(int),
            "flags_unresolved": 0,
            "confirmations": 0,
            "revocations": 0,
            "time_to_declaration_s": [],
            "review_items": 0,
            "person_declared": 0,
            "engagements": defaultdict(lambda: defaultdict(int)),
        }

    for ps in passes:
        for r in ps["results"]:
            for row in (by_arm.setdefault(r["arm"], blank()), by_cell.setdefault((r["scenario"], r["arm"]), blank())):
                row["runs"] += 1
                row["replay_ok"] += int(bool(r.get("replay_ok")))
                for a in r["acts"]:
                    row["acts"] += 1
                    row["hostile_acts"] += int(a["hostile_author"])
                    row["missed_hostile_acts"] += int(a["missed"])
                    if a["time_to_declaration_s"] is not None:
                        row["time_to_declaration_s"].append(a["time_to_declaration_s"])
                for d in r["declarations"]:
                    row["declarations"][d["source"]] += 1
                    if d["source"] == "scripted":
                        continue
                    row[d["verdict"]] += 1
                    if d["verdict"] != "correct":
                        row["wrong_c2_showed_it" if d.get("c2_showed_it") else "wrong_picture_wrong"] += 1
                    if d["source"] == "manual proxy":
                        row["person_declared"] += 1
                    else:
                        row["review_items"] += 1
                for f in r["flags"]:
                    row["flags"] += 1
                    key = {"prevented a wrong one": "flags_prevented_wrong", "held back a right one": "flags_held_back_right"}.get(f["effect"], "flags_proximity")
                    row[key] += 1
                for res in r["flag_resolutions"]:
                    row["flag_resolutions"][res or "?"] += 1
                row["flags_unresolved"] += max(0, len(r["flags"]) - len(r["flag_resolutions"]))
                row["confirmations"] += r["confirmations"]
                row["revocations"] += r["revocations"]
                # what asks a person for a look: every rule declaration (a non-blocking review
                # item) and every flag; the manual arms' declarations are a person's by definition
                row["review_items"] += len(r["flags"])
                for e in r["engagements"]:
                    row["engagements"][e["basis"]]["nominated"] += 1
                    row["engagements"][e["basis"]][e["final"] or "?"] += 1
                    row["engagements"][e["basis"]]["effect"] += int(e["effect"])

    def finish(row: dict[str, Any]) -> dict[str, Any]:
        times = row.pop("time_to_declaration_s")
        row["time_to_declaration_median_s"] = _median(times)
        row["time_to_declaration_n"] = len(times)
        row["declarations"] = dict(row["declarations"])
        row["flag_resolutions"] = dict(row["flag_resolutions"])
        row["engagements"] = {k: dict(v) for k, v in row["engagements"].items()}
        return row

    return {
        "arms": {a: finish(v) for a, v in by_arm.items()},
        "cells": [{"scenario": s, "arm": a, **finish(v)} for (s, a), v in sorted(by_cell.items())],
        "errors": [{"arm": p["arm"], "seed": p["seed"], "error": p["error"]} for p in passes if p.get("error")],
    }


def run_comparison(
    out: Path,
    seeds: list[int],
    arms: tuple[str, ...] | list[str] = DEFAULT_ARMS,
    delay: float = MANUAL_DELAY_S,
    threshold: float | None = None,
    scenarios: Path = SCENARIOS,
) -> dict[str, Any]:
    """Every arm over `seeds` into `out/<arm>/seed-<s>/`, and `comparison.json`."""
    out.mkdir(parents=True, exist_ok=True)
    files = ident_files(scenarios)
    passes = [run_arm(arm, seed, out / arm / f"seed-{seed}", files, delay, threshold) for arm in arms for seed in seeds]
    comp = {
        "label": NOT_HUMAN,
        "scenarios": [str(f.relative_to(REPO)) for f in files],
        "seeds": seeds,
        "arms": list(arms),
        "manual_delay_s": delay,
        "threshold": threshold,
        "act_noise": True,
        "passes": passes,
        "summary": summarize(passes),
    }
    (out / "comparison.json").write_text(json.dumps(comp, indent=1, default=str))
    return comp


def load_comparison(batch: Path) -> dict[str, Any] | None:
    p = Path(batch) / "ident" / "comparison.json"
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text())
    except ValueError:
        return None


def table(comp: dict[str, Any]) -> str:
    """The comparison as plain text, one line per arm."""
    lines = [comp["label"], ""]
    head = f"{'arm':<14}{'runs':>5}{'acts':>6}{'missed':>8}{'correct':>9}{'false':>7}{'wrong-auth':>11}{'C2 showed':>10}{'pict.wrong':>11}{'flags':>7}{'prevent':>9}{'held back':>10}{'review':>8}{'by person':>10}{'t decl (s)':>11}"
    lines.append(head)
    for arm in comp["arms"]:
        r = comp["summary"]["arms"].get(arm)
        if not r:
            continue
        lines.append(
            f"{arm:<14}{r['runs']:>5}{r['acts']:>6}{r['missed_hostile_acts']:>8}{r['correct']:>9}{r['false_hostile']:>7}{r['wrong_author']:>11}"
            f"{r['wrong_c2_showed_it']:>10}{r['wrong_picture_wrong']:>11}{r['flags']:>7}{r['flags_prevented_wrong']:>9}{r['flags_held_back_right']:>10}"
            f"{r['review_items']:>8}{r['person_declared']:>10}{str(r['time_to_declaration_median_s']):>11}"
        )
    lines.append("")
    for arm in comp["arms"]:
        r = comp["summary"]["arms"].get(arm) or {}
        eng = ", ".join(f"{basis}: {v.get('nominated', 0)} nominated, {v.get('effect', 0)} released" for basis, v in sorted((r.get("engagements") or {}).items()))
        lines.append(f"engagements, {arm}: {eng or 'none'}")
    return "\n".join(lines)
