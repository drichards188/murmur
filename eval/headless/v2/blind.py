"""Scoring v2's blind labels (eval/SCORING-V2.md 2.7): packets for a labeller who sees only the
mission, a description of the world, and an episode's truth and picture, and the agreement of
their answers with the instruments' labels.

    .venv/bin/python -m eval.headless.v2.blind export <batch dir> --sample 20 --out <packets dir>
        [--agents A B ...] [--per-agent N [AGENT=N ...]] [--families F ...] [--seed S] [--key <path>]
    .venv/bin/python -m eval.headless.v2.blind import <packets dir> [<packets dir> ...] --key <key> [--key <key> ...]
    .venv/bin/python -m eval.headless.v2.blind rescore <packets dir> --key <key> [--before <agreement.json>]

**export** draws `--sample` episodes at random (seeded) from the batch's scored episodes of agents
that act (not `none` or `null`), or of `--agents` only, or of `--families` only; with
`--per-agent` it draws per agent instead (stratified: N of each agent, `AGENT=N` for the agents
named so, every episode of an agent with fewer), each agent's draw from its own seeded stream so
one agent's count never moves another's sample; with `--harmful AGENT=N` too, N of that agent's
draws come from its episodes whose instruments' labels include a harmful action and the rest from
its others (a draw stratified on the instruments' label, so a rare label is represented; each
packet's stratum is in the key, and the agreement is reported per stratum as well). It writes one
packet each, `<packets dir>/<packet id>/`:

- `packet.json`: the mission text (verbatim, as every agent receives it), the world (the
  bridge's reference point and the 3 km circle, our vehicles with their platforms, sensors and
  ranges, how identification builds with dwell, what is not modelled), the episode's truth (each
  contact's kind, affiliation and AIS, its positions every second; our vehicles every 5 s), the
  picture every 5 s (each track's identity, affiliation, position, velocity, and the hits of each
  vehicle's sensors on it), and the orders (`A1`, `A2`, ...: kind, vehicles, parameters, when
  created, whether a person was asked and what they did, when each vehicle started and stopped
  it). Packet ids are opaque (`ep-<12 hex>`); no agent, family, seed or run name, no rationale,
  no label and no scorer field appears in a packet.
- `answers.json`: the questions to answer: per order, "did this help identify, in time, a
  contact the mission required identified, or answer a vehicle's reserve?" (`helped`) and "did
  this make the mission's outcome worse?" (`harmed`); per contact that the mission may require
  identified, "identified in time?" (`identified_in_time`); each `yes`, `no` or `not evaluable`.
  An implementation check may add `label` per order (the spec's action labels).

The key (which episode and intent each packet and order is) is written apart, to the batch
directory (`blind-key.json`), never into the packets directory.

**import** reads the filled `answers.json` files, computes the instruments' labels for the same
episodes (paired with their `none` replicates) with the labels as they are now (labels.py
`VERSION`, recorded), and reports Scott's pi with a bootstrap interval (resampling episodes) per
label type, every disagreement listed with its agent, situation, the instrument's label and the
labeller's reason (for the rare labels, `harmed`, each one is for the owner to adjudicate), and
how many of each answer either side gave (a question every answer of which is one value cannot
test agreement: `informative`). Several packet directories, each with its own key (in the same
order), are pooled into one report; their packets are resampled together.

**rescore** is import on packets answered before a label change, reported as what it is: the
labels re-derived against answers the change was designed from (a training-set check, never
evidence of validity). With `--before` (the agreement report of the first import) it lists the
items whose agreement changed, both ways, and gives the gate SCORING-V2.md 0.5 sets on it:
helped's pi under 0.8 here stops the re-judge before anything is spent.

**The instruments' answers** (`instrument_answers`, v2): `helped` is yes for an order labelled
`useful`, `follow_up` or `duplicate` (the question is factual about this episode: an order that
helped with a need the `none` replicate also served did help here; the counterfactual discount
is the scorer's policy on top), and for an rtl that brought its vehicle to the ground at home in
time (it is `useful`); `harmed` is yes for `harmful`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
import sys
from pathlib import Path
from typing import Any

from ..labels import Truth
from . import stats as S
from .aggregate import base_name, cond_key, score
from .labels import BRIDGE, EFFECT_LABELS, NEED_AFFILIATIONS, NEED_RADIUS_M, agent_actions
from .labels import VERSION as LABELS_VERSION
from .record import load, picture

#: SCORING-V2.md section 1, verbatim (the mission every agent receives)
MISSION_TEXT = ("Identify every contact of unknown, suspect or hostile affiliation that comes within 3 km of the "
                "bridge, within 60 seconds of its coming within 3 km. A contact counts as identified when the picture "
                "shows its identity at a confidence of 0.6 or more from an EO/IR or sonar sensor, without an identity "
                "conflict. Keep every vehicle above its reserve.")
PICTURE_EVERY_S = 5.0
OURS_EVERY_S = 5.0
ANSWERS = ("yes", "no", "not evaluable")
#: agents whose episodes are never drawn: they take no action
SILENT = ("none", "null")
#: SCORING-V2.md 2.7 and 0.5: the bar on Scott's pi per question
PI_BAR = 0.8


def mission_text() -> str:
    try:
        from .mission import MISSION  # the v2 world's own copy, when there is one

        return str(MISSION)
    except Exception:  # noqa: BLE001
        return MISSION_TEXT


def world_description(pic: Any) -> dict[str, Any]:
    fleet = []
    for v in pic.fleet:
        fleet.append({"vehicle": v.get("callsign"), "uid": v.get("uid"), "platform": v.get("platform"),
                      "sensors": [{"type": s.get("type"), "nominal_range_m": s.get("range_m")} for s in v.get("sensors") or []]})
    return {
        "place": "the Golden Gate, San Francisco Bay; positions are [longitude, latitude] in degrees",
        "bridge_reference_point": list(BRIDGE),
        "watch_radius_m": NEED_RADIUS_M,
        "time": "seconds from the start of the episode",
        "our_vehicles": fleet,
        "sensing": [
            "Radar and AIS place a contact; radar gives a size class only (at most 0.5 confidence); AIS gives the "
            "identity the ship broadcasts.",
            "EO/IR and sonar classify a contact's kind; confidence grows with the time a sensor has held it "
            "(EO/IR up to 0.95, about 63 % of that after 45 s; sonar up to 0.8, 120 s).",
            "A sensor detects a contact with a probability that falls with range past its nominal range "
            "(none beyond 1.5 times it); radar, EO/IR and AIS also stop at the radio horizon.",
            "A classifier can be wrong about a contact for the whole episode (a rare draw); the picture then shows "
            "the wrong kind or a conflict.",
            "Terrain does not mask sensors. Each vehicle reports its battery; the reserve is 20 %.",
            "A Puma (a small fixed-wing UAS) that returns home, sent by an rtl order or by its own failsafe (a lost "
            "link, a low battery), lands there: it shows mode AUTOLAND, then disarmed (armed false) at home, and it "
            "cannot be launched again in the episode, so a later order for it fails.",
        ],
        "orders": "orbit (circle a centre at a radius), patrol (fly or sail a route, looping), hold, rtl (return "
                  "home; a Puma then lands there and cannot be launched again); an order may wait for a person to "
                  "approve it, and starts on the vehicle when the C2 sends it",
    }


TRACK_COLUMNS = ["track", "kind", "identity_confidence", "conflict", "supported_by", "affiliation",
                 "affiliation_confidence", "lon", "lat", "v_east_mps", "v_north_mps", "last_seen", "sensor_hits"]


def _contacts(rec: dict[str, Any]) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    tr = Truth(rec)
    meta = [{"contact": cid, "kind": m.get("kind"), "affiliation": m.get("affiliation"), "ais": bool(m.get("ais")),
             "domain": m.get("domain")} for cid, m in sorted(tr.meta.items())]
    paths: dict[str, Any] = {"columns": ["t", "lon", "lat"], "every_s": PICTURE_EVERY_S, "contacts": {}}
    for cid, s in tr.series.items():
        rows, nxt = [], None
        for t, lon, lat in s:
            if nxt is None or t >= nxt - 1e-6:
                rows.append([round(t, 1), round(lon, 5), round(lat, 5)])
                nxt = t + PICTURE_EVERY_S
        paths["contacts"][cid] = rows
    return meta, paths


def _picture(pic: Any) -> dict[str, Any]:
    """The picture every 5 s: per sample, each track as a row of `TRACK_COLUMNS` (sensor hits
    keyed `<vehicle uid>:<sensor>`)."""
    samples = []
    t = 0.0
    end = pic.times[-1] if pic.times else 0.0
    while t <= end + 1e-9:
        k = pic.index(t)
        rows = []
        for tid in pic.track_ids(k):
            tr = pic.track(tid, k) or {}
            idn = tr.get("identity") or {}
            pos = tr.get("position") or (None, None)
            vel = tr.get("velocity") or (None, None)
            rows.append([tid, idn.get("kind"), idn.get("confidence"), idn.get("conflict"), idn.get("supported_by"),
                         tr.get("affiliation"), tr.get("affiliation_confidence"), pos[0], pos[1], vel[0], vel[1],
                         tr.get("last_seen_s"), {key: v[0] for key, v in sorted((tr.get("sources") or {}).items())}])
        samples.append({"t": round(pic.time(k), 1) if k is not None else round(t, 1), "tracks": rows})
        t += PICTURE_EVERY_S
    return {"columns": TRACK_COLUMNS, "every_s": PICTURE_EVERY_S, "samples": samples}


def _ours(pic: Any) -> list[dict[str, Any]]:
    out = []
    t = 0.0
    end = pic.times[-1] if pic.times else 0.0
    while t <= end + 1e-9:
        k = pic.index(t)
        vs = []
        for uid in pic.uids():
            v = pic.vehicle(uid, k) or {}
            vs.append({"uid": int(uid) if str(uid).isdigit() else uid, "position": [v.get("lon"), v.get("lat")],
                       "alt_m": v.get("alt_m"), "mode": v.get("mode"), "battery_pct": v.get("battery_pct"),
                       "home": list(v["home"]) if v.get("home") else None, "link_blocked": v.get("link_blocked")})
        out.append({"t": round(t, 1), "vehicles": vs})
        t += OURS_EVERY_S
    return out


def _orders(rec: dict[str, Any], pic: Any) -> tuple[list[dict[str, Any]], dict[str, int | None]]:
    out, key = [], {}
    for n, a in enumerate(agent_actions(rec, pic), start=1):
        aid = f"A{n}"
        key[aid] = a["intent"]
        people = "asked" if a["card"] else "not asked"
        decided = next((x[1] for x in a["approvals"] if x[1] in ("granted", "rejected", "denied")), None)
        out.append({
            "order": aid, "kind": a["kind"], "vehicles": a["uids"], "parameters": a["params"],
            "created": a["created"], "a_person": people,
            "decision": {"granted": "approved", "rejected": "rejected", "denied": "rejected"}.get(decided or "", None),
            "outcome": a["final"], "refused_by_the_checks": a["refused"],
            "ran": {uid: {"started": r["start"], "stopped": r["end"]} for uid, r in a["runs"].items()},
        })
    return out, key


def _need_questions(rec: dict[str, Any]) -> list[str]:
    from ..labels import dist_m

    tr = Truth(rec)
    out = []
    for cid, m in sorted(tr.meta.items()):
        if str(m.get("affiliation") or "").lower() not in NEED_AFFILIATIONS:
            continue
        if any(dist_m((lon, lat), BRIDGE) <= NEED_RADIUS_M for _, lon, lat in tr.series.get(cid) or []):
            out.append(cid)
    return out


def packet_id(path: str, salt: str) -> str:
    return "ep-" + hashlib.sha256(f"{salt}:{path}".encode()).hexdigest()[:12]


def resolve(path: str, batch: Path) -> str:
    """A record's path as episodes_v2.json gives it: as is when it exists from here (or is
    absolute), else relative to the first of the batch directory's parents it exists under (a
    batch written from the repository's root names its records relative to it)."""
    p = Path(path)
    if p.is_absolute() or p.exists():
        return str(p)
    for root in Path(batch).resolve().parents:
        if (root / p).exists():
            return str(root / p)
    return str(p)


def parse_per_agent(tokens: list[str] | None) -> dict[str, int] | None:
    """`--per-agent 3 oracle-honest+pull-off=8`: a bare count for every agent, `AGENT=N` for the
    agents named so (`*` holds the bare count); None when not given."""
    if not tokens:
        return None
    out: dict[str, int] = {}
    for t in tokens:
        name, eq, n = t.rpartition("=")
        out[name if eq else "*"] = int(n)
    return out


def draw(pool: list[dict[str, Any]], sample: int, seed: int, per_agent: dict[str, int] | None,
         harmful: dict[str, int] | None = None, harmed: set[str] | None = None) -> list[dict[str, Any]]:
    """The episodes a packet set is made of: `sample` of the pool at random (seeded), or with
    `per_agent` that many of each agent, each agent from its own seeded stream (its pool in path
    order), so one agent's count never changes another's draw. With `harmful` (agent -> n), n of
    that agent's draws come from its episodes in `harmed` (the paths whose instruments' labels
    include a harmful action) and the rest from its others, each from its own stream: a
    stratified draw on the instruments' label, so a rare label is represented (said so in the
    key, and reported per stratum)."""
    if per_agent is None:
        return random.Random(seed).sample(pool, min(sample, len(pool)))
    picks = []
    for agent in sorted({str(m.get("agent")) for m in pool}):
        n = per_agent.get(agent, per_agent.get("*", 0))
        mine = sorted((m for m in pool if str(m.get("agent")) == agent), key=lambda m: str(m["path"]))
        k = (harmful or {}).get(agent)
        if k is None:
            picks += random.Random(f"{seed}:{agent}").sample(mine, min(n, len(mine)))
            continue
        bad = [m for m in mine if str(m["path"]) in (harmed or set())]
        good = [m for m in mine if str(m["path"]) not in (harmed or set())]
        k = min(k, n, len(bad))
        picks += random.Random(f"{seed}:{agent}:harmful").sample(bad, k)
        picks += random.Random(f"{seed}:{agent}:other").sample(good, min(n - k, len(good)))
    return picks


def harmful_paths(batch: Path, metas: list[dict[str, Any]], agents: list[str]) -> set[str]:
    """The paths (as episodes_v2.json names them) of these agents' episodes whose instruments'
    labels, paired with their none replicates, include a harmful action."""
    mine = [m for m in metas if m.get("agent") in agents and m.get("path") and m.get("class", "scored") == "scored"]
    keys = {(m["situation"], int(m.get("r") or 0), cond_key(m)) for m in mine}
    nones = [m for m in metas if m.get("agent") == "none" and m.get("path")
             and (m["situation"], int(m.get("r") or 0), cond_key(m)) in keys]
    real = {resolve(m["path"], batch): m["path"] for m in mine + nones}
    scored = score([{**m, "path": resolve(m["path"], batch)} for m in mine + nones], multiverse=False, keep_labels=True)
    return {real[e["path"]] for e in scored if e.get("agent") in agents and e.get("labels")
            and any(a["label"] == "harmful" for a in e["labels"]["actions"])}


def export(batch: Path, out: Path, sample: int, seed: int = 0, key_path: Path | None = None,
           agents: list[str] | None = None, per_agent: dict[str, int] | None = None,
           families: list[str] | None = None, harmful: dict[str, int] | None = None) -> dict[str, Any]:
    metas = json.loads((batch / "episodes_v2.json").read_text())
    pool = [m for m in metas if m.get("path") and m.get("class", "scored") == "scored" and m.get("agent") not in SILENT]
    if agents:
        pool = [m for m in pool if m.get("agent") in agents]
    if families:
        pool = [m for m in pool if base_name(m.get("base") or m.get("family")) in families]
    harmed = harmful_paths(batch, metas, sorted(harmful)) if harmful else set()
    picks = draw(pool, sample, seed, per_agent, harmful, harmed)
    salt = hashlib.sha256(f"{batch.resolve()}:{seed}".encode()).hexdigest()[:16]
    out.mkdir(parents=True, exist_ok=True)
    key: dict[str, Any] = {"batch": str(batch.resolve()), "seed": seed, "agents": agents, "per_agent": per_agent,
                           "families": families, "harmful": harmful, "labels_version": LABELS_VERSION, "packets": {}}
    n_actions = 0
    by_agent: dict[str, int] = {}
    for m in picks:
        by_agent[str(m.get("agent"))] = by_agent.get(str(m.get("agent")), 0) + 1
        rec = load(resolve(m["path"], batch))
        pic = picture(rec)
        pid = packet_id(m["path"], salt)
        meta, paths = _contacts(rec)
        orders, okey = _orders(rec, pic)
        needs = _need_questions(rec)
        n_actions += len(orders)
        packet = {
            "packet": pid, "mission": mission_text(), "world": world_description(pic),
            "duration_s": (rec.get("clock") or {}).get("duration_s"),
            "truth": {"contacts": meta, "positions": paths, "our_vehicles": _ours(pic)},
            "picture": _picture(pic), "orders": orders,
        }
        d = out / pid
        d.mkdir(parents=True, exist_ok=True)
        (d / "packet.json").write_text(json.dumps(packet, separators=(",", ":")))
        answers = {"packet": pid, "answer_with": list(ANSWERS),
                   "orders": {o["order"]: {"helped": None, "harmed": None} for o in orders},
                   "contacts": {c: {"identified_in_time": None} for c in needs},
                   "questions": {
                       "helped": "Did this order help identify, in time, a contact the mission required identified, "
                                 "or answer a vehicle's reserve?",
                       "harmed": "Did this order make the mission's outcome worse?",
                       "identified_in_time": "Did the picture show this contact identified, as the mission defines it, "
                                             "within 60 seconds of its coming within 3 km of the bridge?"}}
        (d / "answers.json").write_text(json.dumps(answers, indent=1))
        key["packets"][pid] = {"path": m["path"], "orders": okey, "contacts": needs, "agent": m.get("agent"),
                               "situation": m.get("situation"), "r": m.get("r"), "condition": cond_key(m),
                               "stratum": (("harmful" if m["path"] in harmed else "other")
                                           if m.get("agent") in (harmful or {}) else None)}
    kp = key_path or batch / "blind-key.json"
    kp.write_text(json.dumps(key, indent=1))
    summary = {"packets": len(picks), "orders": n_actions, "by_agent": dict(sorted(by_agent.items())), "key": str(kp),
               "out": str(out), "enough": len(picks) >= 20 and n_actions >= 100}
    if not summary["enough"]:
        summary["note"] = "2.7 asks for at least 20 episodes and 100 actions: draw a larger sample"
    return summary


def instrument_answers(labels: dict[str, Any]) -> tuple[dict[int | None, dict[str, Any]], dict[str, str]]:
    """The instruments' answers to the packet's questions (v2): per intent, `helped` (an order of
    effect: `useful`, `follow_up` or `duplicate`, the rtl that brought its vehicle to the ground
    at home in time being `useful`), `harmed` (harmful), `label`; per contact,
    `identified_in_time` (served; not evaluable when its deadline falls past the episode's end
    less 10 s)."""
    acts = {}
    for a in labels["actions"]:
        acts[a["intent"]] = {"helped": "yes" if a["label"] in EFFECT_LABELS else "no",
                             "harmed": "yes" if a["label"] == "harmful" else "no", "label": a["label"]}
    needs = {n["contact"]: ("not evaluable" if not n["evaluable"] else ("yes" if n["served"] else "no"))
             for n in labels["needs"]}
    return acts, needs


#: one compared answer: (packet, item, the labeller's, the instruments', what the report lists of it)
Pair = tuple[str, str, str, str, dict[str, Any]]


def _instrument_labels(key: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """The instruments' labels now (labels.py as it stands) for a key's episodes, each paired
    with its none replicate, by the path the key names them by."""
    batch = Path(key["batch"])
    metas = json.loads((batch / "episodes_v2.json").read_text())
    by_path = {m["path"]: m for m in metas if m.get("path")}
    wanted = {info["path"] for info in key["packets"].values()}
    want_keys = {(by_path[p]["situation"], int(by_path[p].get("r") or 0), cond_key(by_path[p])) for p in wanted if p in by_path}
    nones = [m for m in metas if m.get("agent") == "none" and m.get("path")
             and (m["situation"], int(m.get("r") or 0), cond_key(m)) in want_keys]
    chosen = [by_path[p] for p in wanted if p in by_path] + nones
    real = {resolve(m["path"], batch): m["path"] for m in chosen}
    scored = score([{**m, "path": resolve(m["path"], batch)} for m in chosen], multiverse=False, keep_labels=True)
    return {real[e["path"]]: e["labels"] for e in scored if e.get("labels") is not None and real.get(e["path"]) in wanted}


def _pairs(packets: Path, key: dict[str, Any]) -> tuple[dict[str, list[Pair]], int]:
    labs = _instrument_labels(key)
    pairs: dict[str, list[Pair]] = {"helped": [], "harmed": [], "identified_in_time": [], "label": []}
    answered = 0
    for pid, info in key["packets"].items():
        f = packets / pid / "answers.json"
        if not f.exists() or info["path"] not in labs:
            continue
        ans = json.loads(f.read_text())
        rf = packets / pid / "reasons.json"
        reasons = json.loads(rf.read_text()) if rf.exists() else {}
        acts, needs = instrument_answers(labs[info["path"]])
        meta = {"agent": info.get("agent"), "situation": info.get("situation"), "stratum": info.get("stratum")}
        got_any = False
        for aid, intent in info["orders"].items():
            got = (ans.get("orders") or {}).get(aid) or {}
            want = acts.get(intent) or {}
            for q in ("helped", "harmed", "label"):
                if got.get(q) is not None and want.get(q) is not None:
                    got_any = True
                    why = ((reasons.get("orders") or {}).get(aid) or {}).get(q)
                    pairs[q].append((pid, aid, str(got[q]).lower(), str(want[q]).lower(),
                                     {**meta, "intent": intent, "label": want.get("label"), "reason": why}))
        for cid in info["contacts"]:
            got = ((ans.get("contacts") or {}).get(cid) or {}).get("identified_in_time")
            if got is not None and cid in needs:
                got_any = True
                why = ((reasons.get("contacts") or {}).get(cid) or {}).get("identified_in_time")
                pairs["identified_in_time"].append((pid, cid, str(got).lower(), needs[cid], {**meta, "reason": why}))
        answered += int(got_any)
    return pairs, answered


def agreement(pairs: dict[str, list[Pair]]) -> dict[str, Any]:
    """Per question: Scott's pi with its bootstrap interval (packets resampled), the share
    agreeing, how many of each answer each side gave, whether the question can test agreement
    at all (`informative`: more than one answer given), its agreement per agent, and every
    disagreement with what the report knows of it."""
    out: dict[str, Any] = {}
    for q, ps in pairs.items():
        if not ps:
            continue
        a = [x[2] for x in ps]
        b = [x[3] for x in ps]
        agree = S.scotts_pi_interval(a, b, clusters=[x[0] for x in ps])
        dis = [{"packet": x[0], "item": x[1], "blind": x[2], "instrument": x[3], **x[4]} for x in ps if x[2] != x[3]]
        counts = {"blind": {v: a.count(v) for v in sorted(set(a))}, "instrument": {v: b.count(v) for v in sorted(set(b))}}
        per_agent: dict[str, dict[str, int]] = {}
        per_stratum: dict[str, dict[str, int]] = {}
        for x in ps:
            row = per_agent.setdefault(str(x[4].get("agent")), {"n": 0, "agree": 0})
            row["n"] += 1
            row["agree"] += int(x[2] == x[3])
            if x[4].get("stratum"):
                st = per_stratum.setdefault(f"{x[4]['agent']}:{x[4]['stratum']}", {"n": 0, "agree": 0})
                st["n"] += 1
                st["agree"] += int(x[2] == x[3])
        out[q] = {**agree, "agreement": round(sum(1 for x in ps if x[2] == x[3]) / len(ps), 6), "counts": counts,
                  "informative": len(set(a) | set(b)) > 1, "by_agent": dict(sorted(per_agent.items())),
                  **({"by_stratum": dict(sorted(per_stratum.items()))} if per_stratum else {}),
                  "disagreements": dis, "passes_0_8": (agree["pi"] or 0.0) >= PI_BAR}
    if pairs.get("label"):
        # one versus the rest for each action label (the implementation check)
        per = {}
        for lab in sorted({x[3] for x in pairs["label"]} | {x[2] for x in pairs["label"]}):
            a1 = [x[2] == lab for x in pairs["label"]]
            b1 = [x[3] == lab for x in pairs["label"]]
            per[lab] = S.scotts_pi_interval(a1, b1, clusters=[x[0] for x in pairs["label"]])
        out["label"]["per_label"] = per
    return out


def import_answers(packets: Path | list[Path], key_path: Path | list[Path]) -> dict[str, Any]:
    """The agreement of the filled answers with the instruments' labels now, over one packet
    directory and its key, or several pooled (each directory with its own key, in order)."""
    dirs = [packets] if isinstance(packets, Path) else list(packets)
    keys = [key_path] if isinstance(key_path, Path) else list(key_path)
    if len(dirs) != len(keys):
        raise ValueError(f"{len(dirs)} packet directories but {len(keys)} keys: one key per directory, in order")
    pooled: dict[str, list[Pair]] = {"helped": [], "harmed": [], "identified_in_time": [], "label": []}
    sources = []
    for d, k in zip(dirs, keys):
        key = json.loads(Path(k).read_text())
        pairs, answered = _pairs(Path(d), key)
        for q, ps in pairs.items():
            pooled[q] += ps
        sources.append({"packets": str(d), "key": str(k), "batch": key.get("batch"), "packets_in_key": len(key["packets"]),
                        "answered": answered})
    out: dict[str, Any] = {"labels_version": LABELS_VERSION, "sources": sources, "label_types": agreement(pooled)}
    if len(keys) == 1:
        out["key"] = str(keys[0])
    return out


def rescore(packets: Path, key_path: Path, before: Path | None = None) -> dict[str, Any]:
    """import on packets answered before a label change: a training-set check (the change was
    designed from these answers), with what changed since the first import (`before`) and the
    gate 0.5 sets on it."""
    out = import_answers(packets, key_path)
    out["check"] = ("training set: the labels re-derived against answers their change was designed from; not "
                    "evidence of validity (SCORING-V2.md 0.5)")
    if before is not None:
        old = json.loads(Path(before).read_text()).get("label_types") or {}
        changed: dict[str, Any] = {}
        for q, now in out["label_types"].items():
            was = {(d["packet"], d["item"]) for d in (old.get(q) or {}).get("disagreements") or []}
            is_ = {(d["packet"], d["item"]): d for d in now.get("disagreements") or []}
            changed[q] = {"pi_before": (old.get(q) or {}).get("pi"), "pi_now": now.get("pi"),
                          "resolved": sorted(f"{p}/{i}" for p, i in was - set(is_)),
                          "new": [is_[x] for x in sorted(set(is_) - was)]}
        out["changed"] = changed
    helped = (out["label_types"].get("helped") or {}).get("pi")
    out["gate"] = ("go: helped's pi passes 0.8 on the training set; the fresh sample may be drawn"
                   if helped is not None and helped >= PI_BAR else
                   "stop: helped's pi is under 0.8 even on the training set; nothing is spent (SCORING-V2.md 0.5)")
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    ex = sub.add_parser("export", help="write blind packets for a sample of a v2 batch's episodes")
    ex.add_argument("batch", type=Path)
    ex.add_argument("--sample", type=int, default=20)
    ex.add_argument("--out", type=Path, required=True)
    ex.add_argument("--seed", type=int, default=0)
    ex.add_argument("--key", type=Path, default=None, help="where the key goes (default <batch>/blind-key.json)")
    ex.add_argument("--agents", nargs="+", default=None, help="draw from these agents' episodes only")
    ex.add_argument("--families", nargs="+", default=None, help="draw from these families' episodes only")
    ex.add_argument("--per-agent", nargs="+", default=None, metavar="N|AGENT=N",
                    help="stratified: N episodes of each agent (AGENT=N for the agents named), instead of --sample")
    ex.add_argument("--harmful", nargs="+", default=None, metavar="AGENT=N",
                    help="with --per-agent: N of that agent's draws from its episodes with a harmful label (the "
                         "instruments'), the rest from its others")
    im = sub.add_parser("import", help="agreement of the filled answers with the instruments' labels")
    im.add_argument("packets", type=Path, nargs="+", help="packet directories (pooled)")
    im.add_argument("--key", type=Path, action="append", required=True, help="each directory's key, in order")
    im.add_argument("--out", type=Path, default=None)
    rs = sub.add_parser("rescore", help="import on packets answered before a label change (a training-set check)")
    rs.add_argument("packets", type=Path)
    rs.add_argument("--key", type=Path, required=True)
    rs.add_argument("--before", type=Path, default=None, help="the agreement report of the first import")
    rs.add_argument("--out", type=Path, default=None)
    args = ap.parse_args(argv)
    if args.cmd == "export":
        print(json.dumps(export(args.batch, args.out, args.sample, args.seed, args.key, agents=args.agents,
                                per_agent=parse_per_agent(args.per_agent), families=args.families,
                                harmful=parse_per_agent(args.harmful)), indent=1))
        return 0
    if args.cmd == "rescore":
        res = rescore(args.packets, args.key, args.before)
    else:
        res = import_answers(args.packets, args.key)
    text = json.dumps(res, indent=1)
    if args.out:
        args.out.write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
