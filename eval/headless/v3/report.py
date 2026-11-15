"""The v3 dev check's report (eval/SCORING-V3.md 10): every episode of a v3 batch labelled against its
`none` replicate, and per family and world: the coverage, need served per agent with time to serve,
the room test (10.1, room.py: the ceiling measurably above `none`; it replaced v2's fixed world
conditions on 2026-10-25), the vehicle types the sensing model says could answer each need and those
that did, and the share of the needs `none` and the oracle lost that one cause explains (causes.py).
The oracle of these columns is the ceiling, `oracle-v3-shown` since 2026-10-26 (the owner's
decision, SCORING-V3.md 9); the foreknowledge ceiling `oracle-v3` is reported beside it, never
judged. A batch with the model watch officer (run.py's model arm) gets its own section
(`model_arm`): need served beside `none`, the cost column, and the needs paired by situation against
the playbook and `none` with the exact McNemar test (`paired`; an episode not scored serves nothing
there). The restraint table shows the playbook's and the model's columns beside the free agents',
and the orders naming more than one vehicle (group orders, bd murmur-3kqi). The episodes are
summarized in name order, so a report is the same whatever its workers. MODEL RESULTS ONLY.
"""

from __future__ import annotations

import json
import math
import multiprocessing as mp
import statistics
import tomllib
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from .. import REPO
from ..labels import Truth
from ..v2.record import load
from .able import able
from .causes import cause_of, light, new_hits, single_cause
from .faults import lost_vehicles
from .families import WORLDS, family_domain, parse
from ..v2.labels import Params
from .labels import episode_labels, mark_spoofed, picture, pointed_at, sensing_errors
from .metrics import episode_metrics
from .mission import rule
from .room import CEILING, FORESIGHT, room

#: the ceiling (room.py): the oracle the room, lesson 3 and the capable types read
ORACLE = CEILING
#: the planners whose reaches and feasibility a record carries (planner.py), and the basic oracle's
PLANNERS = (CEILING, FORESIGHT)
#: a vehicle type can answer a need when its model range (a pointed sensor, Pd 0.5, under the
#: situation's conditions, open geometry) times the oracle's share reaches at least this far
CAPABLE_M = {"eo_ir": 150.0, "sonar": 35.0}


def _score(args: tuple[dict[str, Any], dict[str, Any] | None]) -> dict[str, Any]:
    meta, none_meta = args
    rec = load(meta["path"])
    none_labels = None
    if meta["agent"] != "none" and none_meta is not None and none_meta.get("path"):
        none_labels = episode_labels(load(none_meta["path"]))
    lab = episode_labels(rec, none_labels=none_labels)
    met = episode_metrics(rec, lab)
    pic = picture(rec)
    truth = mark_spoofed(rec, Truth(rec))
    wrong = sensing_errors(pic, truth)
    plat = {str(v["uid"]): str(v.get("platform")) for v in pic.fleet}
    needs = []
    for n in lab["needs"]:
        row = {k: n[k] for k in ("contact", "domain", "kind", "t_need", "deadline", "eligible", "served", "pre_served",
                                 "t_served", "time_to_serve", "sensing_error")}
        if n["eligible"] and not n["served"]:
            row["cause"] = cause_of(rec, pic, truth, n, wrong[n["contact"]]["wrong_observers"])
        if n["served"] and n["t_identified"] is not None:
            hit = new_hits(pic, n["contact"], rule(n["domain"]).sensors, n["t_need"] - 300.0, n["t_identified"] + 1.0)
            row["identified_with"] = sorted({plat.get(u, u) for u in hit})
        needs.append(row)
    if meta["agent"] == "none":
        # the types the sensing model says could answer each need: once a situation, on its `none`
        for row, types in zip(needs, (able(rec, lab["needs"]) or {}).values()):
            row["able"] = types
    detail = rec.get("agent_detail") or {}
    reach = (detail.get("reach_m") or {}) if meta["agent"] in PLANNERS + ("oracle-v3-basic",) else {}
    # the ceilings' plan verdict per need (planner.py `feasibility`: the ceiling's when the picture first
    # showed the contact, the foreknowledge ceiling's at its first tick)
    feas = {k: {x: (v.get("feasibility") or {}).get(x) for x in ("verdict", "light", "capable", "timely", "shown_t")}
            for k, v in (detail.get("needs") or {}).items() if v.get("feasibility")} if meta["agent"] in PLANNERS else {}
    fleet_types = sorted({str(v.get("platform")) for v in pic.fleet})
    sensors = sorted({str(x.get("type")) for v in pic.fleet for x in v.get("sensors") or []})
    domains_ours = Counter(str((v.get("platform"))) for v in pic.fleet)
    cond = (rec.get("world") or {}).get("conditions") or {}
    decoys = {d["contact"] for d in ((rec.get("truth") or {}).get("stated") or {}).get("decoys") or []}
    contact_domains = sorted({str(m.get("domain")) for m in truth.meta.values()})
    actions = Counter(a["label"] for a in lab["actions"])
    return {"name": meta["name"], "family": meta["family"], "agent": meta["agent"], "seed": meta["situation_seed"],
            "needs": needs, "metrics": {k: met[k] for k in ("served_share", "cards", "cards_per_10min", "useless_share",
                                                             "by_domain", "per_hour", "actions", "nominations", "review",
                                                             "unjudged_actions", "bad_card_share", "repeats",
                                                             "fault_met_share", "rmst", "harmful_kinds")},
            "feasibility": feas,
            "actions": dict(actions), "reach": reach, "fleet_types": fleet_types, "fleet_count": domains_ours,
            "sensors": sensors, "light": light(rec), "conditions": cond, "contact_domains": contact_domains,
            "contacts": len(truth.meta), "kinds_task": sorted({a.get("kind") for a in lab["actions"] if a.get("kind")}),
            "fault_needs": [{"uid": f["uid"], "platform": plat.get(str(f["uid"])), "domain": f.get("domain"),
                             "status": f["status"]} for f in lab["fault_needs"]],
            "lost": lost_vehicles(rec, pic),
            # F8's restraint: the agent's orders that send a vehicle at a look-alike the situation added
            # and at no need (v2's "pointed at": truth positions over the next 300 s within 1 km of
            # the order's geometry; an identify's, the contact its track names: labels.py `pointed_at`)
            "decoy_looks": sum(1 for a in lab["actions"] if decoys and a.get("label") != "refused"
                               and a.get("kind") in ("orbit", "patrol", "identify")
                               and decoy_only(pointed_at(a, a.get("aim") or {}, pic, truth, Params()), decoys,
                                              {n["contact"] for n in lab["needs"]})),
            "decoys": len(decoys),
            # one order to several vehicles (a group order: its known faults are bd murmur-3kqi's)
            "group_orders": sum(1 for a in lab["actions"] if len(a.get("uids") or []) > 1),
            "per_hour": {k: met["per_hour"].get(k) for k in ("useless", "useless_justified", "useless_unjustified",
                                                              "harmful", "fault_response", "fault_actions")}}


def decoy_only(pointed: set[str], decoys: set[str], needs: set[str]) -> bool:
    """An order pointed at a look-alike and at none of the situation's needs."""
    return bool(pointed & decoys) and not (pointed & needs)


def scored_meta(m: dict[str, Any]) -> bool:
    """An index entry the metrics read: one with a record, and for the model only a `scored` one (a
    model episode the cost guard stopped, whose call the API refused as such or whose input hit the
    leakage scan is not the model's run to judge: `model_arm` lists it apart)."""
    return bool(m.get("path")) and (m.get("agent") != "model" or m.get("class") == "scored")


def score_batch(d: Path, workers: int = 8) -> list[dict[str, Any]]:
    """Every scored episode of the batch, in name order (the index's), whatever order its workers
    finish in: a tie between causes then names the same one in every report."""
    metas = [m for m in json.loads((d / "episodes_v3.json").read_text()) if scored_meta(m)]
    nones = {(m["family"], m["situation_seed"], m["r"]): m for m in metas if m["agent"] == "none"}
    jobs = [(m, nones.get((m["family"], m["situation_seed"], m["r"]))) for m in metas]
    if workers <= 1:
        out = [_score(j) for j in jobs]
    else:
        with mp.get_context("spawn").Pool(workers) as pool:
            out = list(pool.imap_unordered(_score, jobs, chunksize=2))
    return sorted(out, key=lambda s: s["name"])


def situation(m: dict[str, Any]) -> tuple[str, int, int]:
    """An index entry's situation: its family, situation seed and replicate."""
    return (str(m["family"]), int(m["situation_seed"]), int(m["r"]))


def mcnemar_exact(b: int, c: int) -> float:
    """The exact two-sided McNemar test on `b` and `c` discordant pairs: twice the binomial tail at
    one half of the smaller count, at most 1 (1 without a discordant pair)."""
    n = b + c
    if n == 0:
        return 1.0
    tail = sum(math.comb(n, i) for i in range(min(b, c) + 1)) / 2 ** n
    return round(min(1.0, 2.0 * tail), 6)


def paired(model: dict[Any, dict[str, bool] | None], other: dict[Any, dict[str, bool]]) -> dict[str, Any]:
    """Needs paired by situation (the pilot's registration, SCORING-V3.md 17.9): `other` maps each
    situation the other agent ran to its needs (need: served), `model` each situation the model ran
    to its own, or to None for an episode not scored (the cost guard stopped it, the API refused its
    call as such, its input hit the leakage scan), which serves none of the other's needs: no
    exclusion. A pair is a need of both episodes, served by both, the model only, the other only or
    neither; the exact two-sided McNemar p reads the discordant pairs."""
    n = {"both": 0, "model_only": 0, "other_only": 0, "neither": 0}
    sits = 0
    for key, theirs in sorted(other.items()):
        if key not in model:
            continue
        sits += 1
        mine = model[key]
        for need, served in sorted(theirs.items()):
            if mine is not None and need not in mine:
                continue  # not a need of the model's episode
            m = bool(mine and mine[need])
            n["both" if m and served else "model_only" if m else "other_only" if served else "neither"] += 1
    return {"situations": sits, "pairs": sum(n.values()), **n, "mcnemar_p": mcnemar_exact(n["model_only"], n["other_only"])}


#: the agents the model is judged against (the registration's comparator, and the floor)
AGAINST = ("playbook", "none")


def paired_arm(metas: list[dict[str, Any]], scored: list[dict[str, Any]],
               units: Any) -> dict[str, Any]:
    """`paired` for the model against each of `AGAINST`, per family and pooled over the batch's:
    `units(row)` gives a scored episode's needs (need: served). `metas` is the whole index, so a
    model episode not scored is counted, as serving nothing, and named."""
    index = {m["name"]: m for m in metas}
    rows = {s["name"]: s for s in scored}
    mine = {situation(m): (units(rows[m["name"]]) if scored_meta(m) and m["name"] in rows else None)
            for m in metas if m.get("agent") == "model"}
    out: dict[str, Any] = {"against": {}, "unscored": sorted(m["name"] for m in metas if m.get("agent") == "model"
                                                            and not (scored_meta(m) and m["name"] in rows))}
    for agent in AGAINST:
        theirs = {situation(index[s["name"]]): units(s) for s in scored if s["agent"] == agent and s["name"] in index}
        fams = {}
        for f in sorted({k[0] for k in mine}):
            fams[f] = paired({k: v for k, v in mine.items() if k[0] == f}, {k: v for k, v in theirs.items() if k[0] == f})
        out["against"][agent] = {"families": fams, "pooled": paired(mine, theirs)}
    return out


def contact_units(s: dict[str, Any]) -> dict[str, bool]:
    """A scored episode's eligible contact needs: served or not."""
    return {n["contact"]: bool(n["served"]) for n in s["needs"] if n["eligible"]}


def platform_domains() -> dict[str, str]:
    cat = tomllib.loads((REPO / "sim" / "platforms.toml").read_text())
    return {k: str(v.get("domain")) for k, v in cat.items() if isinstance(v, dict)}


def capable(reach: dict[str, float], domain: str, movers: set[str], need: str | None = None) -> list[str]:
    """The platform types whose reach for a need (`need`: its contact; every need of the episode
    when None, as with one need an episode) is at least `CAPABLE_M` of the domain's sensor."""
    s = rule(domain).sensors[0]
    return sorted({k.split(":")[0] for k, v in reach.items() if float(v) >= CAPABLE_M[s] and k.split(":")[0] in movers
                   and (need is None or k.split(":", 1)[1] == need)})


def flags(causes: dict[str, dict[str, Any]], share: float = 0.5, least: int = 3) -> list[str]:
    """Lesson 3 (SCORING-V3.md 10.2): the causes that explain over half of an agent's losses (of at
    least `least`), by the whole key or by its platform or gap alone; `none`'s `no_look` is the room
    a family leaves an agent, counted apart (choice 13)."""
    out = []
    for agent, sc in causes.items():
        if not sc or (sc.get("lost") or 0) < least:
            continue
        for key, sh in ((sc.get("top"), sc.get("share")), (sc.get("top_coarse"), sc.get("share_coarse"))):
            if key is None or (sh or 0) <= share:
                continue
            if agent == "none" and str(key).startswith("no_look"):
                continue
            out.append(f"{agent}: {key} {round(100 * sh)} %")
    return sorted(set(out))


def summarize(scored: list[dict[str, Any]]) -> dict[str, Any]:
    pdom = platform_domains()
    out: dict[str, Any] = {"families": {}, "coverage": {}}
    fams = sorted({s["family"] for s in scored})
    # coverage: what the batch exercised
    worlds = sorted({parse(f)[1] for f in fams})
    by_world: dict[str, Any] = {}
    for w in worlds:
        ex = next(s for s in scored if parse(s["family"])[1] == w)
        counts: dict[str, int] = defaultdict(int)
        for p, n in ex["fleet_count"].items():
            counts[pdom.get(p, "?")] += n
        by_world[w] = {"vehicles": sum(ex["fleet_count"].values()), "by_domain": dict(sorted(counts.items())),
                       "types": ex["fleet_types"], "sensors": ex["sensors"], "contacts": ex["contacts"],
                       "contact_domains": ex["contact_domains"]}
    lights = Counter((s["family"], s["seed"], s["light"]) for s in scored if s["agent"] == "none")
    out["coverage"] = {"worlds": by_world, "light": dict(Counter(k[2] for k in lights)),
                       "agents": sorted({s["agent"] for s in scored}),
                       "tasks_ordered": sorted({k for s in scored for k in s["kinds_task"]}),
                       "situations": len({(s["family"], s["seed"]) for s in scored}),
                       "episodes": len(scored)}
    for f in fams:
        fam, world = parse(f)
        rows = [s for s in scored if s["family"] == f]
        block: dict[str, Any] = {"family": fam, "world": world, "domain": family_domain(fam), "agents": {}}
        movers = set()
        for s in rows:
            if s["agent"] in PLANNERS:
                movers |= {k.split(":")[0] for k in s["reach"]}
        for agent in sorted({s["agent"] for s in rows}):
            ag = [s for s in rows if s["agent"] == agent]
            needs = [n for s in ag for n in s["needs"] if n["eligible"]]
            served = [n for n in needs if n["served"]]
            tts = [n["time_to_serve"] for n in served if n["time_to_serve"] is not None]
            causes = [n["cause"] for n in needs if not n["served"] and n.get("cause")]
            ident = Counter(p for n in served for p in n.get("identified_with") or [])
            block["agents"][agent] = {
                "situations": len(ag), "eligible": len(needs), "served": len(served),
                "share": round(len(served) / len(needs), 3) if needs else None,
                "pre_served": sum(1 for n in needs if n["pre_served"]),
                "time_to_serve_mean_s": round(statistics.fmean(tts), 1) if tts else None,
                "time_to_serve_median_s": round(statistics.median(tts), 1) if tts else None,
                "sensing_errors": sum(1 for s in ag for n in s["needs"] if n["sensing_error"]),
                "single_cause": single_cause(causes),
                "identified_with": dict(ident.most_common()),
                "actions": dict(sum((Counter(s["actions"]) for s in ag), Counter())),
                "by_light": {lt: {"eligible": sum(1 for s in ag if s["light"] == lt for n in s["needs"] if n["eligible"]),
                                  "served": sum(1 for s in ag if s["light"] == lt for n in s["needs"] if n["eligible"] and n["served"])}
                             for lt in sorted({s["light"] for s in ag})},
                "by_domain": {dom: {"eligible": sum(1 for n in needs if n["domain"] == dom),
                                    "served": sum(1 for n in served if n["domain"] == dom)}
                              for dom in sorted({n["domain"] for n in needs})},
                **fault_summary(ag),
            }
        caps = [len(n["able"]) for s in rows if s["agent"] == "none" for n in s["needs"] if "able" in n]
        if not caps:  # a batch without `none`: the oracle's own reaches (the first dev check's)
            for s in rows:
                if s["agent"] not in PLANNERS:
                    continue
                for n in s["needs"]:
                    caps.append(len(capable(s["reach"], n["domain"], movers, n["contact"])))
        block["capable_types"] = {"min": min(caps) if caps else None, "median": statistics.median(caps) if caps else None,
                                  "needs_with_fewer_than_2": sum(1 for c in caps if c < 2), "of": len(caps)}
        # 10.1, the room test (replacing v2's fixed conditions, the owner's decision of 2026-10-25)
        block["room"] = room(scored, f) if {ORACLE, "none"} <= set(block["agents"]) else None
        # reported, never judged: the foreknowledge ceiling over the floor
        block["foreknowledge"] = (room(scored, f, ceiling=FORESIGHT) if {FORESIGHT, "none"} <= set(block["agents"])
                                  else None)
        block["flags"] = flags({a: block["agents"].get(a, {}).get("single_cause", {}) for a in ("none", ORACLE)})
        block["redesign"] = bool(block["flags"])
        out["families"][f] = block
    # pooled over both worlds, per family
    pooled: dict[str, Any] = {}
    for fam in sorted({parse(f)[0] for f in fams}):
        pooled[fam] = {}
        for agent in sorted({s["agent"] for s in scored}):
            needs = [n for s in scored if parse(s["family"])[0] == fam and s["agent"] == agent for n in s["needs"] if n["eligible"]]
            served = [n for n in needs if n["served"]]
            ag = [s for s in scored if parse(s["family"])[0] == fam and s["agent"] == agent]
            tts = [n["time_to_serve"] for n in served if n["time_to_serve"] is not None]
            pooled[fam][agent] = {"eligible": len(needs), "served": len(served),
                                  "share": round(len(served) / len(needs), 3) if needs else None,
                                  "time_to_serve_median_s": round(statistics.median(tts), 1) if tts else None,
                                  "single_cause": single_cause([n["cause"] for n in needs if not n["served"] and n.get("cause")]),
                                  **fault_summary(ag)}
    out["pooled"] = pooled
    return out


def fault_summary(ag: list[dict[str, Any]]) -> dict[str, Any]:
    """An agent's episodes' fault needs (met, unmet, censored; met over judged), the vehicles they
    lost (faults.py `lost_vehicles`, a diagnostic) and its actions' rates of restraint per hour."""
    fns = [f for s in ag for f in s.get("fault_needs") or []]
    st = Counter(f["status"] for f in fns)
    judged = st["met"] + st["unmet"]
    lost = Counter(f"{x['platform']} {x['why']}" for s in ag for x in s.get("lost") or [])

    def mean(k: str) -> float | None:
        xs = [float((s.get("per_hour") or {}).get(k) or 0.0) for s in ag]
        return round(statistics.fmean(xs), 2) if xs else None

    return {"fault_needs": {"met": st["met"], "unmet": st["unmet"], "censored": st["censored"],
                            "met_share": round(st["met"] / judged, 3) if judged else None},
            "decoy_looks": sum(int(s.get("decoy_looks") or 0) for s in ag),
            "decoy_situations_looked": sum(1 for s in ag if s.get("decoy_looks")),
            "group_orders": sum(int(s.get("group_orders") or 0) for s in ag),
            "lost_vehicles": sum(lost.values()), "lost_by": dict(lost.most_common()),
            "per_hour": {k: mean(k) for k in ("useless", "useless_justified", "harmful", "fault_response")}}


def pct(x: float | None) -> str:
    return "n/a" if x is None else f"{100 * x:.0f} %"


def _served(rows: list[dict[str, Any]]) -> tuple[int, int]:
    needs = [n for s in rows for n in s["needs"] if n["eligible"]]
    return sum(1 for n in needs if n["served"]), len(needs)


def model_arm(d: Path, scored: list[dict[str, Any]]) -> dict[str, Any] | None:
    """The model watch officer's episodes (run.py's model arm), per family and world: how many were
    scored and why the rest were not (the cost guard, a call the API refused as such, the leakage
    scan), need served beside `none` on the same situations, and what they cost (US dollars at list
    prices, from the officer's totals in the index), wakes, calls, errors and refusals by their
    `stop_details` category; and the needs paired by situation against the playbook and `none`
    (`paired_arm`). None for a batch without the model."""
    index = json.loads((d / "episodes_v3.json").read_text())
    metas = [m for m in index if m.get("agent") == "model"]
    if not metas:
        return None

    def mean(xs: list[float]) -> float | None:
        return round(statistics.fmean(xs), 4) if xs else None

    def costs(ms: list[dict[str, Any]]) -> dict[str, Any]:
        tot = [m.get("model") or {} for m in ms]
        usd = [float(t.get("usd") or 0.0) for t in tot]
        cats: Counter = Counter()
        for t in tot:
            cats.update(t.get("refusal_categories") or {})
        return {"episodes": len(ms), "usd_total": round(sum(usd), 4), "usd_per_episode": mean(usd),
                "usd_max": round(max(usd), 4) if usd else None,
                "wakes_per_episode": mean([float(t.get("wakes") or 0) for t in tot]),
                "api_calls_per_episode": mean([float(t.get("api_calls") or 0) for t in tot]),
                "tokens": {k: sum(int((t.get("tokens") or {}).get(k) or 0) for t in tot)
                           for k in ("input", "cache_write", "cache_read", "output")},
                "errors": sum(int(t.get("errors") or 0) for t in tot),
                "refusals": sum(int(t.get("refusals") or 0) for t in tot), "refusal_categories": dict(sorted(cats.items())),
                "scored": sum(1 for m in ms if m.get("class") == "scored"),
                "budget_stopped": sorted(m["name"] for m in ms if m.get("class") == "budget_stop"),
                "model_failed": sorted(m["name"] for m in ms if m.get("class") == "model_failed"),
                "harness_error": sorted(m["name"] for m in ms if m.get("class") == "harness_error")}

    out: dict[str, Any] = {"all": costs(metas), "families": {}}
    for f in sorted({m["family"] for m in metas}):
        ms = [m for m in metas if m["family"] == f]
        mine = [s for s in scored if s["family"] == f and s["agent"] == "model"]
        seeds = {s["seed"] for s in mine}
        floor = [s for s in scored if s["family"] == f and s["agent"] == "none" and s["seed"] in seeds]
        fam, world = parse(f)
        (ms_, me), (ns_, ne) = _served(mine), _served(floor)
        out["families"][f] = {"family": fam, "world": world, **costs(ms),
                              "served": {"model": [ms_, me], "none": [ns_, ne]}}
    out["paired"] = paired_arm(index, scored, contact_units)
    first = next((m for m in metas if m.get("path")), None)
    if first is not None:
        prov = (load(first["path"]).get("provenance") or {}).get("model") or {}
        out["config"] = {k: prov.get(k) for k in ("routine", "consult", "max_tokens", "fallbacks")}
    ledger = d / "model-ledger.json"
    if ledger.exists() and ledger.read_text().strip():
        out["ledger"] = json.loads(ledger.read_text())
    return out


PAIRED_NOTE = ("Each {what} of a situation (its family, seed and replicate) that the model and the other agent "
               "both ran, served by both, by the model only, by the other only or by neither; McNemar p is the exact "
               "two-sided test on the discordant pairs. A model episode the cost guard stopped, whose call the API "
               "refused as such or whose input hit the leakage scan serves none of its situation's needs here (no "
               "exclusion).")
PAIRED_HEAD = ["family", "world", "against", "situations", "needs", "both", "model only", "other only", "neither",
               "McNemar p"]


def paired_rows(arm: dict[str, Any], lead: list[str] | None = None) -> list[str]:
    """Markdown table rows of `paired_arm`'s counts: for each agent the model is judged against, its
    families and worlds, then pooled over the batch's families (`lead`: cells before them)."""
    out = []
    for agent, x in arm["against"].items():
        for f, r in list(x["families"].items()) + [("pooled", x["pooled"])]:
            fam, world = ("pooled", "all") if f == "pooled" else parse(f)
            p = f"{r['mcnemar_p']:.4g}" if r["pairs"] else "n/a"
            cells = (lead or []) + [fam, world, agent] + [str(r[k]) for k in ("situations", "pairs", "both", "model_only",
                                                                              "other_only", "neither")] + [p]
            out.append("| " + " | ".join(cells) + " |")
    return out


def model_markdown(arm: dict[str, Any]) -> list[str]:
    a = arm["all"]
    cfg = arm.get("config") or {}

    def lane(k: str) -> str:
        x = cfg.get(k) or {}
        return f"{x.get('model')} at {x.get('effort')}" if x else "?"

    def frac(p: list[int]) -> str:
        return f"{p[0]}/{p[1]} ({pct(p[0] / p[1])})" if p[1] else "no needs"

    cats = ", ".join(f"{k} {v}" for k, v in a["refusal_categories"].items())
    lines = ["", "## The model watch officer", "",
             f"The watch officer's design through the API (eval/headless/officer.py): routine {lane('routine')}, "
             f"consulted {lane('consult')}; no refusal fallback. These results describe the model as an agent in "
             "simulated situations, never people. An episode the cost guard stopped, whose call the API refused as "
             "such, or whose input hit the leakage scan is listed here and left out of every metric above.", "",
             "| family | world | scored / episodes | need served: model / none (same situations) | cost (US$) | "
             "per episode | wakes per episode | API errors | refusals | not scored |",
             "|---|---|---|---|---|---|---|---|---|---|"]
    for f, b in arm["families"].items():
        cut = [f"{k.replace('_', ' ')} {len(b[k])}" for k in ("budget_stopped", "model_failed", "harness_error") if b[k]]
        lines.append(f"| {b['family']} | {b['world']} | {b['scored']}/{b['episodes']} | {frac(b['served']['model'])} / "
                     f"{frac(b['served']['none'])} | {b['usd_total']:.2f} | {b['usd_per_episode']:.3f} | "
                     f"{b['wakes_per_episode']} | {b['errors']} | {b['refusals']} | {', '.join(cut) or 'none'} |")
    lines += ["", f"In all: {a['episodes']} model episodes, US${a['usd_total']:.2f} at list prices (US${a['usd_per_episode']:.3f} "
              f"an episode, at most US${a['usd_max']:.3f}); tokens: {a['tokens']['input']:,} fresh input, "
              f"{a['tokens']['cache_read']:,} cache reads, {a['tokens']['cache_write']:,} cache writes, "
              f"{a['tokens']['output']:,} output; {a['errors']} API errors; {a['refusals']} refusals"
              + (f" ({cats})" if cats else "") + "."]
    if arm.get("ledger"):
        lg = arm["ledger"]
        lines.append(f"The batch's ledger: US${float(lg.get('usd') or 0):.2f} spent of its US${float(lg.get('max_usd') or 0):.2f} cap.")
    pa = arm["paired"]
    lines += ["", "### Needs served, paired by situation", "", PAIRED_NOTE.format(what="eligible contact need"), "",
              "| " + " | ".join(PAIRED_HEAD) + " |", "|" + "---|" * len(PAIRED_HEAD), *paired_rows(pa), "",
              f"Model episodes not scored, counted as serving nothing: {', '.join(pa['unscored']) or 'none'}."]
    return lines


def markdown(summary: dict[str, Any], d: Path) -> str:
    cov = summary["coverage"]
    who = ("free deterministic agents and the model watch officer" if summary.get("model")
           else "free deterministic agents")
    lines = ["# Scoring v3 dev check", "", f"Batch `{d.name}`. MODEL RESULTS ONLY: {who} on generated "
             "situations, the approve-all operator, the paused clock; never human data.", "", "## Coverage", ""]
    for w, x in cov["worlds"].items():
        lines.append(f"- **{w}**: {x['vehicles']} vehicles ({', '.join(f'{k} {v}' for k, v in x['by_domain'].items())}); "
                     f"types {', '.join(x['types'])}; sensors {', '.join(x['sensors'])}; {x['contacts']} contacts in "
                     f"the domains {', '.join(x['contact_domains'])}.")
    lines += [f"- Conditions drawn per situation: {', '.join(f'{k} {v}' for k, v in sorted(cov['light'].items()))} "
              f"(situations by light).", f"- Agents: {', '.join(cov['agents'])}; tasks ordered: "
              f"{', '.join(cov['tasks_ordered']) or 'none'}.", f"- {cov['situations']} situations, {cov['episodes']} episodes.", ""]
    lines += ["## Per family and world", "",
              f"The oracle is the ceiling (`{ORACLE}`); the foreknowledge ceiling (`{FORESIGHT}`) is reported, never judged.",
              "",
              "| family | world | none | random-valid | oracle | foreknowledge | time to serve (oracle, median s) | room (10.1) | "
              "capable types (min) | none's top cause | oracle's top cause | lesson 3 |",
              "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for f, b in summary["families"].items():
        a = b["agents"]

        def cell(agent: str) -> str:
            x = a.get(agent) or {}
            return f"{x.get('served', 0)}/{x.get('eligible', 0)} ({pct(x.get('share'))})" if x.get("eligible") else "no needs"

        def cause(agent: str) -> str:
            sc = (a.get(agent) or {}).get("single_cause") or {}
            return f"{sc['top']} {pct(sc['share'])} of {sc['lost']}" if sc.get("lost") else "none lost"

        rm = b.get("room") or {}
        ok = "n/a" if not rm or rm.get("mean_difference") is None else (
            f"{rm['verdict']}: {100 * rm['mean_difference']:+.0f} [{100 * (rm.get('lo') or 0):+.0f}, "
            f"{100 * (rm.get('hi') or 0):+.0f}]")
        lines.append(f"| {b['family']} | {b['world']} | {cell('none')} | {cell('random-valid')} | {cell(ORACLE)} | "
                     f"{cell(FORESIGHT)} | "
                     f"{(a.get(ORACLE) or {}).get('time_to_serve_median_s')} | {ok} | {b['capable_types']['min']} | "
                     f"{cause('none')} | {cause(ORACLE)} | {'; '.join(b['flags']) or 'no flag'} |")
    lines += ["", "## Pooled over both worlds", "", "| family | none | random-valid | oracle | foreknowledge |",
              "|---|---|---|---|---|"]
    for fam, x in summary["pooled"].items():
        def c2(agent: str) -> str:
            y = x.get(agent) or {}
            return f"{y.get('served', 0)}/{y.get('eligible', 0)} ({pct(y.get('share'))})" if y.get("eligible") else "no needs"
        lines.append(f"| {fam} | {c2('none')} | {c2('random-valid')} | {c2(ORACLE)} | {c2(FORESIGHT)} |")
    lines += ["", "## Faults and restraint, per family and world", "",
              "Fault needs met (low reserve, met over judged; censored apart), vehicles lost by the end (a diagnostic), "
              "useless orders per situation-hour (justified on the picture among them), and orders naming more than one "
              "vehicle (group orders: their known faults are bd murmur-3kqi's). The model's columns are its scored "
              "episodes'.", "",
              "| family | world | fault needs met: none / random-valid / oracle | vehicles lost: none / random-valid / oracle | "
              "useless per hour (justified): random-valid / oracle / playbook / model | harmful per hour: random-valid / "
              "oracle / playbook / model | orders at a look-alike: random-valid / oracle / playbook / model | "
              "orders naming more than one vehicle: random-valid / oracle / playbook / model |",
              "|---|---|---|---|---|---|---|---|"]
    shown = ("random-valid", ORACLE, "playbook", "model")
    for f, b in summary["families"].items():
        a = b["agents"]

        def fn(agent: str) -> str:
            x = (a.get(agent) or {}).get("fault_needs") or {}
            j = (x.get("met") or 0) + (x.get("unmet") or 0)
            return f"{x.get('met', 0)}/{j}" + (f" (+{x['censored']} c.)" if x.get("censored") else "") if x else "n/a"

        def lost(agent: str) -> str:
            return str((a.get(agent) or {}).get("lost_vehicles", "n/a"))

        def ph(agent: str, k: str) -> str:
            return str(((a.get(agent) or {}).get("per_hour") or {}).get(k))

        def each(cell: Any) -> str:
            return " / ".join(cell(x) for x in shown)

        def useless(agent: str) -> str:
            return f"{ph(agent, 'useless')} ({ph(agent, 'useless_justified')})"

        def harmful(agent: str) -> str:
            return ph(agent, "harmful")

        def looks(agent: str) -> str:
            return str((a.get(agent) or {}).get("decoy_looks", "n/a"))

        def groups(agent: str) -> str:
            return str((a.get(agent) or {}).get("group_orders", "n/a"))

        lines.append(f"| {b['family']} | {b['world']} | {fn('none')} / {fn('random-valid')} / {fn(ORACLE)} | "
                     f"{lost('none')} / {lost('random-valid')} / {lost(ORACLE)} | "
                     f"{each(useless)} | {each(harmful)} | {each(looks)} | {each(groups)} |")
    if summary.get("model"):
        lines += model_markdown(summary["model"])
    return "\n".join(lines) + "\n"


def write(d: Path, workers: int = 8) -> tuple[Path, Path]:
    scored = score_batch(d, workers)
    summary = summarize(scored)
    arm = model_arm(d, scored)
    if arm is not None:  # a batch with the model only: a free batch's files are as they were
        summary["model"] = arm
    j = d / "dev_v3.json"
    j.write_text(json.dumps({"summary": summary, "episodes": scored}, indent=1, sort_keys=True, default=str))
    md = d / "dev_v3.md"
    md.write_text(markdown(summary, d))
    return j, md


__all__ = ["write", "summarize", "score_batch", "WORLDS"]
