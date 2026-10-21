"""Scoring v2's blind labels (eval/SCORING-V2.md 2.7): packets for a labeller who sees only the
mission, a description of the world, and an episode's truth and picture, and the agreement of
their answers with the instruments' labels.

    .venv/bin/python -m eval.headless.v2.blind export <batch dir> --sample 20 --out <packets dir>
    .venv/bin/python -m eval.headless.v2.blind import <packets dir> [--key <batch dir>/blind-key.json]

**export** draws `--sample` episodes at random (seeded) from the batch's scored episodes of agents
that act (not `none`), and writes one packet each, `<packets dir>/<packet id>/`:

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
episodes (paired with their `none` replicates), and reports Scott's pi with a bootstrap interval
(resampling episodes) per label type, every disagreement listed (for the rare labels, `harmed`,
each one is for the owner to adjudicate).
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
from .aggregate import cond_key, score
from .labels import BRIDGE, NEED_AFFILIATIONS, NEED_RADIUS_M, agent_actions
from .record import load, picture

#: SCORING-V2.md section 1, verbatim (the mission every agent receives)
MISSION_TEXT = ("Identify every contact of unknown, suspect or hostile affiliation that comes within 3 km of the "
                "bridge, within 60 seconds of its coming within 3 km. A contact counts as identified when the picture "
                "shows its identity at a confidence of 0.6 or more from an EO/IR or sonar sensor, without an identity "
                "conflict. Keep every vehicle above its reserve.")
PICTURE_EVERY_S = 5.0
OURS_EVERY_S = 5.0
ANSWERS = ("yes", "no", "not evaluable")


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
        ],
        "orders": "orbit (circle a centre at a radius), patrol (fly or sail a route, looping), hold, rtl (return "
                  "home); an order may wait for a person to approve it, and starts on the vehicle when the C2 sends it",
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


def export(batch: Path, out: Path, sample: int, seed: int = 0, key_path: Path | None = None) -> dict[str, Any]:
    metas = json.loads((batch / "episodes_v2.json").read_text())
    pool = [m for m in metas if m.get("path") and m.get("class", "scored") == "scored" and m.get("agent") != "none"]
    rng = random.Random(seed)
    picks = rng.sample(pool, min(sample, len(pool)))
    salt = hashlib.sha256(f"{batch.resolve()}:{seed}".encode()).hexdigest()[:16]
    out.mkdir(parents=True, exist_ok=True)
    key: dict[str, Any] = {"batch": str(batch.resolve()), "seed": seed, "packets": {}}
    n_actions = 0
    for m in picks:
        rec = load(m["path"])
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
                               "situation": m.get("situation"), "r": m.get("r"), "condition": cond_key(m)}
    kp = key_path or batch / "blind-key.json"
    kp.write_text(json.dumps(key, indent=1))
    summary = {"packets": len(picks), "orders": n_actions, "key": str(kp), "out": str(out),
               "enough": len(picks) >= 20 and n_actions >= 100}
    if not summary["enough"]:
        summary["note"] = "2.7 asks for at least 20 episodes and 100 actions: draw a larger sample"
    return summary


def instrument_answers(labels: dict[str, Any]) -> tuple[dict[int | None, dict[str, Any]], dict[str, str]]:
    """The instruments' answers to the packet's questions: per intent, `helped` (useful or a
    follow-up, or a fault response or useful order that met a low-reserve need), `harmed`
    (harmful), `label`; per contact, `identified_in_time` (served; not evaluable when its
    deadline falls past the episode's end less 10 s)."""
    acts = {}
    for a in labels["actions"]:
        met_reserve = bool(a.get("met_fault")) or (a["label"] == "fault_response" and any(
            f.startswith("low_reserve:") for f in a.get("faults_open") or []))
        acts[a["intent"]] = {"helped": "yes" if a["label"] in ("useful", "follow_up") or met_reserve else "no",
                             "harmed": "yes" if a["label"] == "harmful" else "no", "label": a["label"]}
    needs = {n["contact"]: ("not evaluable" if not n["evaluable"] else ("yes" if n["served"] else "no"))
             for n in labels["needs"]}
    return acts, needs


def import_answers(packets: Path, key_path: Path) -> dict[str, Any]:
    key = json.loads(key_path.read_text())
    batch = Path(key["batch"])
    metas = json.loads((batch / "episodes_v2.json").read_text())
    by_path = {m["path"]: m for m in metas if m.get("path")}
    wanted = {info["path"] for info in key["packets"].values()}
    # the instruments' labels for the sampled episodes, each paired with its none replicate
    want_keys = {(by_path[p]["situation"], int(by_path[p].get("r") or 0), cond_key(by_path[p])) for p in wanted if p in by_path}
    nones = [m for m in metas if m.get("agent") == "none" and m.get("path")
             and (m["situation"], int(m.get("r") or 0), cond_key(m)) in want_keys]
    scored = score([by_path[p] for p in wanted if p in by_path] + nones, multiverse=False, keep_labels=True)
    labs = {e["path"]: e["labels"] for e in scored if e.get("labels") is not None and e["path"] in wanted}
    pairs: dict[str, list[tuple[str, str, str, str]]] = {"helped": [], "harmed": [], "identified_in_time": [], "label": []}
    for pid, info in key["packets"].items():
        f = packets / pid / "answers.json"
        if not f.exists() or info["path"] not in labs:
            continue
        ans = json.loads(f.read_text())
        acts, needs = instrument_answers(labs[info["path"]])
        for aid, intent in info["orders"].items():
            got = (ans.get("orders") or {}).get(aid) or {}
            want = acts.get(intent) or {}
            for q in ("helped", "harmed", "label"):
                if got.get(q) is not None and want.get(q) is not None:
                    pairs[q].append((pid, aid, str(got[q]).lower(), str(want[q]).lower()))
        for cid in info["contacts"]:
            got = ((ans.get("contacts") or {}).get(cid) or {}).get("identified_in_time")
            if got is not None and cid in needs:
                pairs["identified_in_time"].append((pid, cid, str(got).lower(), needs[cid]))
    out: dict[str, Any] = {"key": str(key_path), "label_types": {}}
    for q, ps in pairs.items():
        if not ps:
            continue
        a = [x[2] for x in ps]
        b = [x[3] for x in ps]
        agree = S.scotts_pi_interval(a, b, clusters=[x[0] for x in ps])
        dis = [{"packet": x[0], "item": x[1], "blind": x[2], "instrument": x[3]} for x in ps if x[2] != x[3]]
        out["label_types"][q] = {**agree, "agreement": round(sum(1 for x in ps if x[2] == x[3]) / len(ps), 6),
                                 "disagreements": dis, "passes_0_8": (agree["pi"] or 0.0) >= 0.8}
    if "label" in pairs and pairs["label"]:
        # one versus the rest for each action label (the implementation check)
        per = {}
        for lab in sorted({x[3] for x in pairs["label"]} | {x[2] for x in pairs["label"]}):
            a = [x[2] == lab for x in pairs["label"]]
            b = [x[3] == lab for x in pairs["label"]]
            per[lab] = S.scotts_pi_interval(a, b, clusters=[x[0] for x in pairs["label"]])
        out["label_types"]["label"]["per_label"] = per
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
    im = sub.add_parser("import", help="agreement of the filled answers with the instruments' labels")
    im.add_argument("packets", type=Path)
    im.add_argument("--key", type=Path, required=True)
    im.add_argument("--out", type=Path, default=None)
    args = ap.parse_args(argv)
    if args.cmd == "export":
        print(json.dumps(export(args.batch, args.out, args.sample, args.seed, args.key), indent=1))
        return 0
    res = import_answers(args.packets, args.key)
    text = json.dumps(res, indent=1)
    if args.out:
        args.out.write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
