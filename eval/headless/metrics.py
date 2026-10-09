"""The metrics of one episode record (docs/HEADLESS.md, section 9; CRITERIA.md), the same for a
headless record and a full-stack one (fullstack.py builds the same schema). Truth-labelled
metrics come from labels.py; outcomes from the audit log; MODEL RESULTS (a scripted operator,
generated or scripted situations), never human data.
"""

from __future__ import annotations

import json
import statistics
import sys
from typing import Any

from . import REPO
from .labels import Truth, contact_labels, proposals

sys.path.insert(0, str(REPO / "agents"))
from playbook import fidelity  # noqa: E402
from playbook.rationale import parse_record  # noqa: E402

LOOK_PLAYS = ("isr.investigate", "isr.armed_overwatch")
FINAL = ("rejected", "expired", "voided", "done", "failed", "dispatched", "approved", "queued", "pending_approval")


def clock(rec: dict[str, Any]) -> Any:
    """Bridge seconds to situation seconds for this record."""
    c = rec.get("clock") or {}
    if "zero_unix" in c:  # full stack: the bridge's clock to UTC (anchors), minus the zero
        off, zero = float(c.get("bridge_to_utc_s") or 0.0), float(c["zero_unix"])
        return lambda b: b + off - zero
    epoch = float(c["epoch"])
    return lambda b: b - epoch


def audit_intents(rec: dict[str, Any], to_s: Any) -> dict[int, dict[str, Any]]:
    """Each intent's source, kind and the situation time it first reached each state."""
    out: dict[int, dict[str, Any]] = {}
    for line in (rec.get("audit_text") or "").splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        typ = e.get("type")
        t = to_s(e["t"] / 1000.0) if isinstance(e.get("t"), (int, float)) else None
        if typ == "intent.created":
            i = e.get("intent") or {}
            out[int(i["id"])] = {"source": i.get("source"), "kind": i.get("kind"), "uids": i.get("uids"),
                                 "created": t, "states": {}, "by": None, "note": None}
        elif typ == "intent.state" and int(e.get("intent", -1)) in out:
            out[int(e["intent"])]["states"].setdefault(str(e.get("to")), t)
            out[int(e["intent"])]["last"] = str(e.get("to"))
        elif typ in ("approval.granted", "approval.rejected", "approval.denied") and int(e.get("intent", -1)) in out:
            o = out[int(e["intent"])]
            o["by"], o["note"], o["decided"] = e.get("by"), e.get("note"), t
            o["decision"] = "approved" if typ == "approval.granted" else "rejected"
    return out


def mean(xs: list[float]) -> float | None:
    return round(statistics.fmean(xs), 3) if xs else None


def episode_metrics(rec: dict[str, Any]) -> dict[str, Any]:
    to_s = clock(rec)
    truth = Truth(rec)
    labels = contact_labels(rec, truth)
    props = proposals(rec, to_s)
    intents = audit_intents(rec, to_s)
    must = {cid: lb for cid, lb in labels.items() if lb["needs_look"]}
    looks = [p for p in props if p["play"] in LOOK_PLAYS]
    covered, times = {}, {}
    for cid, lb in must.items():
        mine = [p for p in looks if p["truth_target"] == cid and p["t"] is not None]
        first = min((p["t"] for p in mine), default=None)
        covered[cid] = first is not None and first <= lb["look_by_s"]
        times[cid] = round(first - lb["first_within_s"], 3) if first is not None else None
    false_looks = [p for p in props if str(p["play"]).startswith("isr.")
                   and p["truth_target"] in labels and not labels[p["truth_target"]]["needs_look"]]
    human = [p for p in props if p["tier"] == "human" or p["state"] == "pending_approval"]
    legit = []
    for p in human:
        if str(p["play"]).startswith("isr."):
            legit.append(p["truth_target"] in must)
    outcomes: dict[str, int] = {}
    latencies = []
    for p in human:
        o = intents.get(int(p["intent"])) if p["intent"] is not None else None
        if not o:
            continue
        end = o.get("decision") or o.get("last") or "?"
        if "expired" in o["states"] and not o.get("decision"):
            end = "expired"
        outcomes[end] = outcomes.get(end, 0) + 1
        if o.get("decided") is not None and o["states"].get("pending_approval") is not None:
            latencies.append(o["decided"] - o["states"]["pending_approval"])
    # re-proposals of a (play, truth target) after a rejection, with no material change named
    rejected = [(p, intents[int(p["intent"])]["decided"]) for p in human
                if p["intent"] is not None and intents.get(int(p["intent"]), {}).get("decision") == "rejected"]
    reprop = 0
    for p, td in rejected:
        for q in props:
            if q["t"] is not None and td is not None and q["t"] > td and q["play"] == p["play"] \
                    and q["truth_target"] == p["truth_target"]:
                ev = parse_record(str(q["rationale"] or "")) or {}
                if not ev.get("material_change"):
                    reprop += 1
    unfaithful = sum(1 for p in props if fidelity.check(str(p["rationale"] or "")))
    objected = 0
    for p in props:
        ev = parse_record(str(p["rationale"] or "")) or {}
        if ev.get("rulebook_v2") or "fail" in str((ev.get("preview") or {}).get("checks")):
            objected += 1
    # CRITERIA.md's timeliness: from the first tick whose plan held the play's candidate
    timely = []
    plans = rec.get("plans") or []
    for p in props:
        first = next((pl["t"] for pl in plans if [p["play"], p["target"]] in pl["candidates"]), None)
        if first is not None and p["t"] is not None:
            timely.append(round(p["t"] - to_s(first), 3))
    ai_start = float((rec.get("clock") or {}).get("ai_start_s") or 45.0)
    dur = float((rec.get("clock") or {}).get("duration_s") or 720.0)
    agree = [p["truth_target"] == p["tracker_target"] for p in props if p["tracker_target"] is not None]
    return {
        "labels": labels,
        "proposals": [{k: v for k, v in p.items() if k != "rationale"} for p in props],
        "must_look": sorted(must),
        "look_covered": all(covered.values()) if must else None,
        "covered": covered,
        "time_to_look": times,
        "time_to_look_first": next(iter(times.values()), None) if len(times) == 1 else None,
        "false_looks": len(false_looks),
        "precision": {"legit": sum(legit), "n": len(legit)},
        "human_cards": len(human),
        "human_cards_per_operator_min": round(len(human) / max(1e-9, (dur - ai_start) / 60.0), 4),
        "outcomes": outcomes,
        "approval_latency_s": [round(x, 3) for x in latencies],
        "reproposals_after_rejection": reprop,
        "unfaithful": unfaithful,
        "objected": objected,
        "timeliness_s": timely,
        "association_agreement": {"agree": sum(agree), "n": len(agree)},
        "n_proposals": len(props),
    }
