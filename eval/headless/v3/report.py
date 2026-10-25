"""The v3 dev check's report (eval/SCORING-V3.md 10.2): every episode of a v3 batch labelled against its
`none` replicate, and per family and world: the coverage, need served per agent with time to serve,
v2's 10.1 world conditions (`none` at most 50 %, the oracle at least 80 % of eligible needs), the
vehicle types the sensing model says could answer each need and those that did, and the share of the
needs `none` and the oracle lost that one cause explains (causes.py). MODEL RESULTS ONLY.
"""

from __future__ import annotations

import json
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
from .labels import episode_labels, picture, sensing_errors
from .metrics import episode_metrics
from .mission import rule

NONE_MAX = 0.5
ORACLE_MIN = 0.8
ORACLE = "oracle-v3"
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
    truth = Truth(rec)
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
    reach = ((rec.get("agent_detail") or {}).get("reach_m") or {}) if meta["agent"] == ORACLE else {}
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
                                                             "unjudged_actions")},
            "actions": dict(actions), "reach": reach, "fleet_types": fleet_types, "fleet_count": domains_ours,
            "sensors": sensors, "light": light(rec), "conditions": cond, "contact_domains": contact_domains,
            "contacts": len(truth.meta), "kinds_task": sorted({a.get("kind") for a in lab["actions"] if a.get("kind")}),
            "fault_needs": [{"uid": f["uid"], "platform": plat.get(str(f["uid"])), "domain": f.get("domain"),
                             "status": f["status"]} for f in lab["fault_needs"]],
            "lost": lost_vehicles(rec, pic),
            # F8's restraint: the agent's orders aimed at a look-alike the situation added
            "decoy_looks": sum(1 for a in lab["actions"] if (a.get("aim") or {}).get("primary") in decoys),
            "decoys": len(decoys),
            "per_hour": {k: met["per_hour"].get(k) for k in ("useless", "useless_justified", "useless_unjustified",
                                                              "harmful", "fault_response", "fault_actions")}}


def score_batch(d: Path, workers: int = 8) -> list[dict[str, Any]]:
    metas = [m for m in json.loads((d / "episodes_v3.json").read_text()) if m.get("path")]
    nones = {(m["family"], m["situation_seed"], m["r"]): m for m in metas if m["agent"] == "none"}
    jobs = [(m, nones.get((m["family"], m["situation_seed"], m["r"]))) for m in metas]
    if workers <= 1:
        return [_score(j) for j in jobs]
    with mp.get_context("spawn").Pool(workers) as pool:
        return list(pool.imap_unordered(_score, jobs, chunksize=2))


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
            if s["agent"] == ORACLE:
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
                if s["agent"] != ORACLE:
                    continue
                for n in s["needs"]:
                    caps.append(len(capable(s["reach"], n["domain"], movers, n["contact"])))
        block["capable_types"] = {"min": min(caps) if caps else None, "median": statistics.median(caps) if caps else None,
                                  "needs_with_fewer_than_2": sum(1 for c in caps if c < 2), "of": len(caps)}
        nn = block["agents"].get("none", {}).get("share")
        oo = block["agents"].get(ORACLE, {}).get("share")
        block["conditions_10_1"] = {"none_at_most_50": None if nn is None else nn <= NONE_MAX,
                                    "oracle_at_least_80": None if oo is None else oo >= ORACLE_MIN}
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
            "lost_vehicles": sum(lost.values()), "lost_by": dict(lost.most_common()),
            "per_hour": {k: mean(k) for k in ("useless", "useless_justified", "harmful", "fault_response")}}


def pct(x: float | None) -> str:
    return "n/a" if x is None else f"{100 * x:.0f} %"


def markdown(summary: dict[str, Any], d: Path) -> str:
    cov = summary["coverage"]
    lines = ["# Scoring v3 dev check", "", f"Batch `{d.name}`. MODEL RESULTS ONLY: free deterministic agents on generated "
             "situations, the approve-all operator, the paused clock; never human data.", "", "## Coverage", ""]
    for w, x in cov["worlds"].items():
        lines.append(f"- **{w}**: {x['vehicles']} vehicles ({', '.join(f'{k} {v}' for k, v in x['by_domain'].items())}); "
                     f"types {', '.join(x['types'])}; sensors {', '.join(x['sensors'])}; {x['contacts']} contacts in "
                     f"the domains {', '.join(x['contact_domains'])}.")
    lines += [f"- Conditions drawn per situation: {', '.join(f'{k} {v}' for k, v in sorted(cov['light'].items()))} "
              f"(situations by light).", f"- Agents: {', '.join(cov['agents'])}; tasks ordered: "
              f"{', '.join(cov['tasks_ordered']) or 'none'}.", f"- {cov['situations']} situations, {cov['episodes']} episodes.", ""]
    lines += ["## Per family and world", "",
              "| family | world | none | random-valid | oracle | time to serve (oracle, median s) | 10.1 | capable types (min) | "
              "none's top cause | oracle's top cause | lesson 3 |", "|---|---|---|---|---|---|---|---|---|---|---|"]
    for f, b in summary["families"].items():
        a = b["agents"]

        def cell(agent: str) -> str:
            x = a.get(agent) or {}
            return f"{x.get('served', 0)}/{x.get('eligible', 0)} ({pct(x.get('share'))})" if x.get("eligible") else "no needs"

        def cause(agent: str) -> str:
            sc = (a.get(agent) or {}).get("single_cause") or {}
            return f"{sc['top']} {pct(sc['share'])} of {sc['lost']}" if sc.get("lost") else "none lost"

        c = b["conditions_10_1"]
        ok = "n/a" if c["none_at_most_50"] is None else (
            ("pass" if c["none_at_most_50"] and c["oracle_at_least_80"] else
             "fail: " + ", ".join(k for k, v in (("none over 50", not c["none_at_most_50"]),
                                                 ("oracle under 80", c["oracle_at_least_80"] is False)) if v)))
        lines.append(f"| {b['family']} | {b['world']} | {cell('none')} | {cell('random-valid')} | {cell(ORACLE)} | "
                     f"{(a.get(ORACLE) or {}).get('time_to_serve_median_s')} | {ok} | {b['capable_types']['min']} | "
                     f"{cause('none')} | {cause(ORACLE)} | {'; '.join(b['flags']) or 'no flag'} |")
    lines += ["", "## Pooled over both worlds", "", "| family | none | random-valid | oracle |", "|---|---|---|---|"]
    for fam, x in summary["pooled"].items():
        def c2(agent: str) -> str:
            y = x.get(agent) or {}
            return f"{y.get('served', 0)}/{y.get('eligible', 0)} ({pct(y.get('share'))})" if y.get("eligible") else "no needs"
        lines.append(f"| {fam} | {c2('none')} | {c2('random-valid')} | {c2(ORACLE)} |")
    lines += ["", "## Faults and restraint, per family and world", "",
              "Fault needs met (low reserve, met over judged; censored apart), vehicles lost by the end (a diagnostic), "
              "useless orders per situation-hour (justified on the picture among them).", "",
              "| family | world | fault needs met: none / random-valid / oracle | vehicles lost: none / random-valid / oracle | "
              "useless per hour (justified): random-valid / oracle | harmful per hour: random-valid / oracle | "
              "orders at a look-alike: random-valid / oracle |",
              "|---|---|---|---|---|---|---|"]
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

        lines.append(f"| {b['family']} | {b['world']} | {fn('none')} / {fn('random-valid')} / {fn(ORACLE)} | "
                     f"{lost('none')} / {lost('random-valid')} / {lost(ORACLE)} | "
                     f"{ph('random-valid', 'useless')} ({ph('random-valid', 'useless_justified')}) / "
                     f"{ph(ORACLE, 'useless')} ({ph(ORACLE, 'useless_justified')}) | "
                     f"{ph('random-valid', 'harmful')} / {ph(ORACLE, 'harmful')} | "
                     f"{(a.get('random-valid') or {}).get('decoy_looks', 'n/a')} / {(a.get(ORACLE) or {}).get('decoy_looks', 'n/a')} |")
    return "\n".join(lines) + "\n"


def write(d: Path, workers: int = 8) -> tuple[Path, Path]:
    scored = score_batch(d, workers)
    summary = summarize(scored)
    j = d / "dev_v3.json"
    j.write_text(json.dumps({"summary": summary, "episodes": scored}, indent=1, sort_keys=True, default=str))
    md = d / "dev_v3.md"
    md.write_text(markdown(summary, d))
    return j, md


__all__ = ["write", "summarize", "score_batch", "WORLDS"]
