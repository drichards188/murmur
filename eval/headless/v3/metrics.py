"""Scoring v3's metrics (eval/SCORING-V3.md section 3): v2's, per episode, from v3's labels
(eval/headless/v2/metrics.py `episode_metrics`, imported unchanged), with what changes in v3:

- time to serve and its RMST per domain, each censored at its own grace (v2's at 60 s);
- **need served per domain** (3.1b): served over eligible needs of each domain, beside the pooled
  share;
- nominations and review decisions per situation-hour by their labels (3.11, 3.12);
- `unjudged` actions (a collect or cover a record cannot judge, 2.10) counted apart, never in a rate of
  quality.
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any

from ..v2 import metrics as V2M
from ..v2 import stats as S
from .labels import NOMINATION_LABELS
from .mission import DOMAINS

HOUR_S = 3600.0


def episode_metrics(rec: dict[str, Any], labels: dict[str, Any]) -> dict[str, Any]:
    out = V2M.episode_metrics(rec, labels)
    dur = float(labels["duration_s"])
    eligible = [n for n in labels["needs"] if n["eligible"]]
    # time to serve: each need censored at its own domain's grace (v2's tau was 60 s for all)
    tts = [n["time_to_serve"] if n["served"] else n["grace_s"] for n in eligible]
    out["time_to_serve"] = tts
    by: dict[str, dict[str, Any]] = {}
    for dom in DOMAINS:
        ns = [n for n in eligible if n["domain"] == dom]
        if not ns:
            continue
        t = [n["time_to_serve"] if n["served"] else n["grace_s"] for n in ns]
        km = S.rmst(t, [bool(n["served"]) for n in ns], tau=DOMAINS[dom].grace_s)
        by[dom] = {"eligible": len(ns), "served": sum(1 for n in ns if n["served"]),
                   "served_share": round(sum(1 for n in ns if n["served"]) / len(ns), 6),
                   "pre_served": sum(1 for n in ns if n["pre_served"]), "rmst": km["rmst"],
                   "time_to_serve_served": [n["time_to_serve"] for n in ns if n["served"]], "grace_s": DOMAINS[dom].grace_s}
    out["by_domain"] = by
    out["rmst"] = None if len(by) != 1 else next(iter(by.values()))["rmst"]
    noms: dict[str, int] = defaultdict(int)
    unjudged = 0
    for a in labels["actions"]:
        if a.get("kind") == "engage" and a["label"] in NOMINATION_LABELS:
            noms[a["label"]] += 1
        if a["label"] == "unjudged":
            unjudged += 1
    out["nominations"] = dict(noms)
    out["nominations_per_hour"] = {k: round(v / (dur / HOUR_S), 6) for k, v in noms.items()}
    rev: dict[str, int] = defaultdict(int)
    for r in labels.get("review") or []:
        rev[r["label"]] += 1
    out["review"] = dict(rev)
    out["unjudged_actions"] = unjudged
    return out
