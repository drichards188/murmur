"""The dev check's report for F14-F17 (eval/SCORING-V3.md section 12): every episode of a v3 batch of
those families labelled against its `none` replicate (labels.py, labels_ops.py), and per family and
world: the coverage, each family's needs served or decisions right per agent, harm and unwarranted
counts, the share of losses one cause explains, and for F15 what blocks a nomination (the basic
oracle's attempts) and for F17 the simulator's real-time factor. MODEL RESULTS ONLY.

    .venv/bin/python -m eval.headless.v3.report_ops <batch dir>
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from ..labels import Truth
from ..v2.record import load
from .causes import cause_of, light, single_cause
from .families import parse
from .labels import episode_labels, picture, sensing_errors

ORACLE = "oracle-v3"
STATIONARY_MPS = 0.2
YOUNG_S = 60.0


def _speeds(rec: dict[str, Any]) -> dict[str, list[tuple[float, float]]]:
    out: dict[str, list[tuple[float, float]]] = defaultdict(list)
    for s in (rec.get("truth") or {}).get("samples") or []:
        for c in s.get("contacts") or []:
            if len(c) > 4 and c[4]:
                out[str(c[0])].append((float(s["t"]), float(c[3] or 0.0)))
    return out


def _moved_before(series: list[tuple[float, float]], t: float, window: float = 60.0) -> float | None:
    """The contact's fastest truth speed over the `window` before `t` (None: not in the world)."""
    xs = [v for (ts, v) in series if t - window <= ts <= t]
    return max(xs) if xs else None


def blockers(rec: dict[str, Any], pic: Any) -> dict[str, Any]:
    """F15: the oracle's nomination attempts, what blocked each (by category), the no-velocity
    ground tracks' truth (stationary or moving; the track young or not), and the spans."""
    atts = [ln for ln in rec.get("agent_log") or [] if ln.get("kind") == "engage.attempt"]
    if not atts:
        return {}
    whole = (rec.get("truth") or {}).get("association") or {}
    speeds = _speeds(rec)
    first_seen: dict[str, float] = {}
    for tid, T in (pic.tracks or {}).items():
        span = (T.get("span") or [[None, None]])[0][0]
        if span is not None:
            first_seen[str(tid)] = pic.time(int(span))
    cats: Counter = Counter()
    only: Counter = Counter()
    noveloc: Counter = Counter()
    noveloc_tracks: dict[str, str] = {}
    spans_best: list[float] = []
    spans_pass: list[float] = []
    passed = keepout_blocked = other_blocked = 0
    for a in atts:
        opts = a.get("options") or []
        best = min((o for o in opts if o.get("span_s") is not None), key=lambda o: o["span_s"], default=None)
        if best is not None:
            spans_best.append(float(best["span_s"]))
        if a.get("passed"):
            passed += 1
            p = next((o for o in opts if o.get("passed")), None)
            if p and p.get("span_s") is not None:
                spans_pass.append(float(p["span_s"]))
            continue
        ref = best or (opts[0] if opts else {})
        failing = set(ref.get("failing") or [])
        if not failing & {"keep_out_tracks", "keep_out_friendly"}:
            other_blocked += 1
            cats["other_checks:" + ",".join(sorted(failing))[:60]] += 1
            continue
        keepout_blocked += 1
        here = set()
        for tid, subject, kind, domain, _short in ref.get("blocking") or []:
            if subject == "lost_contact" or kind in ("out_of_view", "lost"):
                c = "lost_or_out_of_view"
            elif subject == "vehicle":
                c = "ours"
            elif kind == "aircraft" or domain == "air":
                c = "aircraft"
            elif kind == "no_velocity" and domain == "ground":
                c = "ground_no_velocity"
                cid = whole.get(str(tid))
                mv = _moved_before(speeds.get(str(cid), []), float(a["t"])) if cid else None
                age = float(a["t"]) - first_seen.get(str(tid), float(a["t"]))
                truth = "unassociated" if cid is None else ("stationary" if (mv is not None and mv < STATIONARY_MPS) else "moving")
                noveloc[(truth, "young" if age < YOUNG_S else "older")] += 1
                noveloc_tracks[str(tid)] = truth
            elif kind == "young":
                c = "young"
            else:
                c = f"other:{kind}"
            here.add(c)
        for c in here:
            cats[c] += 1
        if len(here) == 1:
            only[next(iter(here))] += 1
    n = len(atts)
    return {"attempts": n, "passed": passed, "keepout_blocked": keepout_blocked, "other_blocked": other_blocked,
            "categories": dict(cats), "only_category": dict(only),
            "no_velocity_ground": {f"{k[0]}/{k[1]}": v for k, v in noveloc.items()},
            "no_velocity_tracks": dict(Counter(noveloc_tracks.values())),
            "span_best": spans_best, "span_pass": spans_pass}


def _score(args: tuple[dict[str, Any], dict[str, Any] | None]) -> dict[str, Any]:
    meta, none_meta = args
    rec = load(meta["path"])
    none_labels = None
    if meta["agent"] != "none" and none_meta is not None and none_meta.get("path"):
        none_labels = episode_labels(load(none_meta["path"]))
    lab = episode_labels(rec, none_labels=none_labels)
    pic = picture(rec)
    truth = Truth(rec)
    wrong = sensing_errors(pic, truth)
    needs = []
    for n in lab["needs"]:
        row = {k: n[k] for k in ("contact", "domain", "eligible", "served", "pre_served", "time_to_serve")}
        if n["eligible"] and not n["served"]:
            row["cause"] = cause_of(rec, pic, truth, n, wrong[n["contact"]]["wrong_observers"])
        needs.append(row)
    acts = [{k: a.get(k) for k in ("intent", "kind", "label", "created")} | {"target": (a.get("nomination") or {}).get("target"),
                                                                            "why": (a.get("space") or {}).get("why")}
            for a in lab["actions"]]
    reviews = [{k: r.get(k) for k in ("kind", "action", "label", "diligent", "load", "why", "contact")} for r in lab["review"]]
    perf = rec.get("perf") or {}
    dur = float((rec.get("clock") or {}).get("duration_s") or 0.0)
    st = (rec.get("truth") or {}).get("stated") or {}
    detail = rec.get("agent_detail") or {}
    out = {"name": meta["name"], "family": meta["family"], "agent": meta["agent"], "seed": meta["situation_seed"],
           "needs": needs, "actions": acts, "review": reviews, "ops": lab.get("ops") or {},
           "light": light(rec), "rtf": round(dur / float(perf["wall_s"]), 2) if perf.get("wall_s") else None,
           "fleet": len(pic.fleet), "contacts": len(truth.meta), "items": st.get("review_items"),
           "element": st.get("element"), "space_options": detail.get("space_options"),
           "effects": [{"hit": e.get("hit"), "removed": e.get("platform_removed")}
                       for e in ((rec.get("effects") or {}).get("applied") or [])]}
    if meta["agent"] == ORACLE:
        out["blockers"] = blockers(rec, pic)
    return out


def score_batch(d: Path, workers: int = 8) -> list[dict[str, Any]]:
    metas = [m for m in json.loads((d / "episodes_v3.json").read_text()) if m.get("path")]
    nones = {(m["family"], m["situation_seed"], m["r"]): m for m in metas if m["agent"] == "none"}
    jobs = [(m, nones.get((m["family"], m["situation_seed"], m["r"]))) for m in metas]
    if workers <= 1:
        return [_score(j) for j in jobs]
    with mp.get_context("spawn").Pool(workers) as pool:
        return list(pool.imap_unordered(_score, jobs, chunksize=1))


def pct(a: int, b: int) -> str:
    return "n/a" if not b else f"{a}/{b} ({100.0 * a / b:.0f} %)"


def med(xs: list[float]) -> float | None:
    return round(statistics.median(xs), 1) if xs else None


def summarize(scored: list[dict[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for f in sorted({s["family"] for s in scored}):
        fam, world = parse(f)
        rows = [s for s in scored if s["family"] == f]
        blk: dict[str, Any] = {"family": fam, "world": world, "situations": len({s["seed"] for s in rows}), "agents": {}}
        for agent in sorted({s["agent"] for s in rows}):
            ag = [s for s in rows if s["agent"] == agent]
            needs = [n for s in ag for n in s["needs"] if n["eligible"]]
            x: dict[str, Any] = {"episodes": len(ag), "contact_needs": len(needs),
                                 "contact_served": sum(1 for n in needs if n["served"]),
                                 "contact_causes": single_cause([n["cause"] for n in needs if not n["served"] and n.get("cause")]),
                                 "by_domain": {}, "actions": dict(Counter(a["label"] for s in ag for a in s["actions"]))}
            for dom in sorted({n["domain"] for n in needs}):
                dn = [n for n in needs if n["domain"] == dom]
                x["by_domain"][dom] = [sum(1 for n in dn if n["served"]), len(dn)]
            gaps = [g for s in ag for g in (s["ops"].get("gap") or [])]
            covs = [c for s in ag for c in (s["ops"].get("cover") or []) if c.get("evaluable")]
            engs = [e for s in ag for e in (s["ops"].get("engagement") or [])]
            noms = [a for s in ag for a in s["actions"] if a["kind"] == "engage"]
            revs = [r for s in ag for r in s["review"]]
            if gaps:
                x["gap"] = {"served": sum(1 for g in gaps if g["served"]), "of": len(gaps),
                            "by": dict(Counter(g["by"] for g in gaps if g["served"])),
                            "causes": single_cause([g["cause"] for g in gaps if not g["served"]])}
                x["collect_labels"] = dict(Counter(a["label"] for s in ag for a in s["actions"] if a["kind"] == "collect"))
            if covs:
                x["cover"] = {"served": sum(1 for c in covs if c["served"]), "of": len(covs),
                              "with_outage": sum(1 for c in covs if c["outage"]),
                              "served_with_outage": sum(1 for c in covs if c["outage"] and c["served"]),
                              "causes": single_cause([c["cause"] for c in covs if not c["served"]])}
                x["cover_labels"] = dict(Counter(a["label"] for s in ag for a in s["actions"] if a["kind"] == "cover"))
            if engs:
                x["engagement"] = {"right": sum(1 for e in engs if e["right"]), "effective": sum(1 for e in engs if e["effective"]),
                                   "restraint_ok": sum(1 for e in engs if e["restraint_ok"]), "of": len(engs),
                                   "causes": single_cause([e["cause"] for e in engs if not e["effective"] and e.get("cause")])}
            if noms or engs:
                x["nominations"] = dict(Counter(a["label"] for a in noms))
                x["harmful"] = sum(1 for a in noms if a["label"] == "harmful")
                x["unwarranted"] = sum(1 for a in noms if a["label"] == "unwarranted")
                x["effects"] = dict(Counter(("hit" if e["hit"] else "miss") for s in ag for e in s["effects"]))
            if revs or any(s.get("items") for s in ag):
                items = [i for s in ag for i in (s.get("items") or [])]
                x["review"] = {"labels": dict(Counter(r["label"] for r in revs)),
                               "by_kind": {k: dict(Counter(r["label"] for r in revs if r["kind"] == k))
                                           for k in sorted({r["kind"] for r in revs})},
                               "items_stated": dict(Counter(i["item"] for i in items)),
                               "diligent": sum(1 for r in revs if r.get("diligent")),
                               "skimmed": sum(1 for r in revs if r.get("diligent") is False),
                               "causes": single_cause([f"{r['kind']}:{r['action']}:"
                                                       f"{'read' if r.get('diligent') else ('skimmed' if r.get('diligent') is False else 'rule')}"
                                                       for r in revs if r["label"] == "wrong"])}
            rtf = [s["rtf"] for s in ag if s["rtf"]]
            x["rtf_median"] = med(rtf)
            if agent == ORACLE:
                bl = [s["blockers"] for s in ag if s.get("blockers")]
                if bl:
                    agg: dict[str, Any] = {"attempts": sum(b["attempts"] for b in bl), "passed": sum(b["passed"] for b in bl),
                                           "keepout_blocked": sum(b["keepout_blocked"] for b in bl),
                                           "other_blocked": sum(b["other_blocked"] for b in bl)}
                    for key in ("categories", "only_category", "no_velocity_ground", "no_velocity_tracks"):
                        c: Counter = Counter()
                        for b in bl:
                            c.update(b[key])
                        agg[key] = dict(c.most_common())
                    agg["span_best_median"] = med([v for b in bl for v in b["span_best"]])
                    agg["span_pass_median"] = med([v for b in bl for v in b["span_pass"]])
                    agg["situations_with_a_pass"] = sum(1 for b in bl if b["passed"])
                    agg["situations"] = len(bl)
                    x["blockers"] = agg
                so = [s["space_options"] for s in ag if s.get("space_options")]
                if so:
                    caps = [len((v or {}).get("capabilities_in_time") or []) for d in so for v in d.values()]
                    classes = [len((v or {}).get("classes_in_time") or {}) for d in so for v in d.values()]
                    x["space_options"] = {"needs": len(caps), "two_capabilities": sum(1 for c in caps if c >= 2),
                                          "two_classes": sum(1 for c in classes if c >= 2),
                                          "no_option": sum(1 for c in classes if c == 0)}
            blk["agents"][agent] = x
        out[f] = blk
    return out


def markdown(summary: dict[str, Any], d: Path) -> str:
    L = [f"# F14-F17 dev check (`{d.name}`)", "", "MODEL RESULTS ONLY: free deterministic agents, scripted operators, "
         "the paused clock; never human data.", ""]
    for f, b in summary.items():
        L += [f"## {b['family']} on {b['world']} ({b['situations']} situations)", ""]
        for agent, x in b["agents"].items():
            parts = [f"contact needs {pct(x['contact_served'], x['contact_needs'])}"]
            if "gap" in x:
                parts.append(f"gap {pct(x['gap']['served'], x['gap']['of'])}")
            if "cover" in x:
                parts.append(f"cover {pct(x['cover']['served'], x['cover']['of'])} (outage in {x['cover']['with_outage']})")
            if "engagement" in x:
                e = x["engagement"]
                parts.append(f"engagement right {pct(e['right'], e['of'])}, effective {e['effective']}, restraint ok {e['restraint_ok']}")
            if "nominations" in x:
                parts.append(f"nominations {x['nominations']} harmful {x['harmful']} unwarranted {x['unwarranted']}")
            if "review" in x:
                r = x["review"]
                parts.append(f"review {r['labels']} (read {r['diligent']}, skimmed {r['skimmed']})")
            parts.append(f"rtf {x.get('rtf_median')}")
            L.append(f"- **{agent}**: " + "; ".join(parts))
            for key in ("gap", "cover", "engagement"):
                if key in x and x[key]["causes"].get("lost"):
                    c = x[key]["causes"]
                    L.append(f"  - {key} losses: {c['lost']}, top cause {c['top']} {100 * (c['share'] or 0):.0f} %")
            if x["contact_causes"].get("lost"):
                c = x["contact_causes"]
                L.append(f"  - contact needs lost: {c['lost']}, top cause {c['top']} {100 * (c['share'] or 0):.0f} %"
                         f" (coarse {c['top_coarse']} {100 * (c['share_coarse'] or 0):.0f} %)")
            if "review" in x and x["review"]["causes"].get("lost"):
                c = x["review"]["causes"]
                L.append(f"  - wrong decisions: {c['lost']}, top {c['top']} {100 * (c['share'] or 0):.0f} %")
            if "blockers" in x:
                bl = x["blockers"]
                L.append(f"  - attempts {bl['attempts']}: passed {bl['passed']}, keep-out blocked {bl['keepout_blocked']}, "
                         f"other {bl['other_blocked']}; situations with a pass {bl['situations_with_a_pass']}/{bl['situations']}; "
                         f"span median {bl['span_best_median']} s (passing {bl['span_pass_median']} s)")
                L.append(f"  - blocker categories {bl['categories']}; only one {bl['only_category']}")
                L.append(f"  - no-velocity ground blockers {bl['no_velocity_ground']} (tracks {bl['no_velocity_tracks']})")
            if "space_options" in x:
                L.append(f"  - space options {x['space_options']}")
        L.append("")
    return "\n".join(L) + "\n"


def write(d: Path, workers: int = 8) -> tuple[Path, Path]:
    scored = score_batch(d, workers)
    summary = summarize(scored)
    j = d / "dev_ops.json"
    j.write_text(json.dumps({"summary": summary, "episodes": scored}, indent=1, sort_keys=True, default=str))
    md = d / "dev_ops.md"
    md.write_text(markdown(summary, d))
    return j, md


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("dir", type=Path)
    ap.add_argument("--workers", type=int, default=8)
    a = ap.parse_args(argv)
    _, md = write(a.dir, a.workers)
    print(md.read_text())
    return 0


if __name__ == "__main__":
    sys.exit(main())


__all__ = ["write", "summarize", "score_batch", "blockers"]
