"""Scoring v3's metrics (eval/SCORING-V3.md section 3): v2's, per episode, from v3's labels
(eval/headless/v2/metrics.py `episode_metrics`, imported unchanged), with what changes in v3:

- time to serve and its RMST per domain, each censored at its own grace (v2's at 60 s): the
  domains and graces of the record's own settings (settings.py `of_record`);
- **need served per domain** (3.1b): served over eligible needs of each domain, beside the pooled
  share;
- nominations and review decisions per situation-hour by their labels (3.11, 3.12);
- `unjudged` actions (a collect or cover a record cannot judge, 2.10) counted apart, never in a rate of
  quality;
- **an order that served no need** (2.5 item 6, choice 40, the owner's decision of 2026-11-15) is
  `justified` or `not_justified`, where v2 has `useless` (`useless_justified` and
  `useless_unjustified` by the picture alone): v2's useless figures are replaced by v3's
  (`per_hour` and `counts` of `justified` and `not_justified`, `not_justified_share` for
  `useless_share`, `not_justified_dispatched` for `useless_dispatched`; `bad_card_share` counts the
  cards whose order is not_justified or harmful), and v2's own functions and outputs are untouched.
  **The judgment measure** is not_justified per situation-hour; **the burden** is reported apart
  (3.4b): the agent's orders and its cards per situation-hour, whatever their labels
  (`per_hour.orders`, `per_hour.cards`; `cards_per_10min` stays v2's, over the watch). The two
  together (`served_no_need`, per hour and as `served_no_need_share`) are v2's useless figures value
  for value, kept for the ruler's moves registered on them (13.4), never a judgment of an agent.
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any

from ..v2 import metrics as V2M
from ..v2 import stats as S
from .labels import NO_NEED_LABELS, NOMINATION_LABELS
from .settings import DOMAINS, of_record

HOUR_S = 3600.0
#: v2's figures of its `useless` label, which v3's labels never give (replaced below)
V2_USELESS = ("useless", "useless_justified", "useless_unjustified")
#: per-hour counts v3 adds: its two labels of an order that served no need and the two together (v2's
#: useless, for the ruler's moves), and the burden
V3_RATES = NO_NEED_LABELS + ("served_no_need", "orders", "cards")


def episode_metrics(rec: dict[str, Any], labels: dict[str, Any]) -> dict[str, Any]:
    # v2's metrics read an order that served no need as `useless` (its fault actions among them): they
    # are handed v3's labels so, then every useless figure is replaced by v3's
    as_v2 = {**labels, "actions": [{**a, "label": "useless", "justified": {"justified": False}}
                                   if a["label"] in NO_NEED_LABELS else a for a in labels["actions"]]}
    out = V2M.episode_metrics(rec, as_v2)
    dur = float(labels["duration_s"])
    acts = labels["actions"]
    counts: dict[str, int] = defaultdict(int)
    for a in acts:
        if a["label"] in NO_NEED_LABELS:
            counts[a["label"]] += 1
            counts["served_no_need"] += 1
    counts["orders"] = len(acts)
    counts["cards"] = sum(1 for a in acts if a["card"])
    for k in V2_USELESS:
        out["counts"].pop(k, None)
        out["per_hour"].pop(k, None)
    out["counts"].update({k: int(counts.get(k, 0)) for k in V3_RATES})
    out["per_hour"].update({k: round(counts.get(k, 0) / (dur / HOUR_S), 6) for k in V3_RATES})
    acted = [a for a in acts if a["label"] != "refused"]
    out.pop("useless_share", None)
    out.pop("justified_useless_share", None)
    out.pop("useless_dispatched", None)
    out["not_justified_share"] = round(counts.get("not_justified", 0) / len(acted), 6) if acted else None
    out["served_no_need_share"] = round(counts.get("served_no_need", 0) / len(acted), 6) if acted else None
    out["not_justified_dispatched"] = sum(1 for a in acts if a["label"] == "not_justified" and a["dispatched"] is not None)
    cards = [a for a in acts if a["card"]]
    out["bad_card_share"] = (round(sum(1 for a in cards if a["label"] in ("not_justified", "harmful")) / len(cards), 6)
                             if cards else None)
    eligible = [n for n in labels["needs"] if n["eligible"]]
    # time to serve: each need censored at its own domain's grace (v2's tau was 60 s for all)
    tts = [n["time_to_serve"] if n["served"] else n["grace_s"] for n in eligible]
    out["time_to_serve"] = tts
    by: dict[str, dict[str, Any]] = {}
    st = of_record(rec)
    for dom in DOMAINS:
        ns = [n for n in eligible if n["domain"] == dom]
        if not ns:
            continue
        grace = st.rule(dom).grace_s
        t = [n["time_to_serve"] if n["served"] else n["grace_s"] for n in ns]
        km = S.rmst(t, [bool(n["served"]) for n in ns], tau=grace)
        by[dom] = {"eligible": len(ns), "served": sum(1 for n in ns if n["served"]),
                   "served_share": round(sum(1 for n in ns if n["served"]) / len(ns), 6),
                   "pre_served": sum(1 for n in ns if n["pre_served"]), "rmst": km["rmst"],
                   "time_to_serve_served": [n["time_to_serve"] for n in ns if n["served"]], "grace_s": grace}
    out["by_domain"] = by
    out["rmst"] = None if len(by) != 1 else next(iter(by.values()))["rmst"]
    noms: dict[str, int] = defaultdict(int)
    unjudged = 0
    for a in acts:
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
