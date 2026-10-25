"""Why a need was lost (eval/SCORING-V3.md 10.2; WORLD-V3-PLAN 7, lesson 3): one cause per eligible
need that was not served by its deadline, specific enough that one quirk (one link shadow, one
vehicle, one sensor gap) shows as one key. The dev check reports, per family and agent, the share of
lost needs that the most frequent key explains; over 50 % is redesigned before validation.

The first that applies, judged on the picture up to the deadline:

1. `not_tracked`: no track was associated with the contact by the deadline.
1b. `spoof_believed@<light>`: a contact broadcasting a spoofed AIS identity (F8's spoofer) whose
   track still shows another kind than its own at the deadline (the AIS's, or a look's below
   identification).
2. `misidentified:<platform>`: the track showed a conflict, or another kind at the identification
   level, at the deadline (the platform: the observer whose wrong-call draw is set and that looked,
   else `unknown`). Another family is not counted here: ESM's detection names a domain's family
   (`aircraft`, `submerged`) whatever the contact is, so it shows under 5.
3. For an agent that dispatched for the need (its own orders' vehicles; the oracle's log names the
   need): `lost:<platform>` when every vehicle it sent for it left the task (a failsafe's return, a
   failed or ended order) before the deadline without raising its hits.
4. `no_look@<light>`: no vehicle's domain sensor raised its hits on the track from 60 s before the
   need opened to the deadline (light: day, night, twilight or fog, from the drawn conditions).
5. `looked:<platform>@<light>`: the domain's sensors looked (the platform with the most new hits)
   and the track reached `recognised` (its family) or `detected` (its size class) but not the
   identification by the deadline; `late` is added when it was identified within the episode.
"""

from __future__ import annotations

from collections import Counter
from typing import Any

from ..labels import Truth
from ..v2.record import Picture
from .labels import FAMILY
from .mission import rule

LOOK_BEFORE_S = 60.0


def light(rec: dict[str, Any]) -> str:
    """day, night, twilight or fog, from the record's drawn conditions (the sun's elevation from
    the clock and place, as the sensing model has it, is approximated by the local hour)."""
    c = (rec.get("world") or {}).get("conditions") or {}
    if c.get("visibility_km") is not None and float(c["visibility_km"]) < 2.0:
        return "fog"
    lt = str(c.get("local_time") or "12:00")
    h = int(lt[:2]) + int(lt[3:5]) / 60.0
    if 6.5 <= h < 19.5:
        return "day"
    if 5.5 <= h < 6.5 or 19.5 <= h < 20.5:
        return "twilight"
    return "night"


def platforms(pic: Picture) -> dict[str, str]:
    return {str(v["uid"]): str(v.get("platform")) for v in pic.fleet}


def new_hits(pic: Picture, cid: str, sensors: tuple[str, ...], t1: float, t2: float) -> Counter:
    """Per vehicle, how many samples its domain sensors raised their hits on the contact's track."""
    out: Counter = Counter()
    k1 = pic.index(t1)
    k2 = pic.index(t2)
    if k2 is None:
        return out
    for k in range((0 if k1 is None else k1) + 1, k2 + 1):
        tid = pic.track_of(cid, k)
        if tid is None:
            continue
        T = pic.tracks.get(tid) or {}
        for key in T.get("src") or {}:
            uid, s = key.split(":", 1)
            if s in sensors and pic.hits(tid, key, k) > pic.hits(tid, key, k - 1):
                out[uid] += 1
    return out


def level_shown(kind: str | None, true_kind: str) -> str:
    if kind is None:
        return "none"
    if kind == true_kind:
        return "identified"
    if kind == FAMILY.get(true_kind):
        return "recognised"
    if kind in set(FAMILY.values()):
        return "wrong_family"
    if kind in FAMILY:
        return "wrong_kind"
    return "detected"


def agent_vehicles(rec: dict[str, Any], need: str) -> list[dict[str, Any]]:
    """The vehicles the agent ordered for this need, as its log names it (the oracle's proposals
    carry `need`), with when each order began and ended (the audit's op states)."""
    by_intent = {int(i["id"]): i for i in ((rec.get("v3") or {}).get("audit") or {}).get("intents") or []}
    out = []
    for ln in rec.get("agent_log") or []:
        if ln.get("kind") != "proposal" or ln.get("need") != need or ln.get("intent") is None:
            continue
        i = by_intent.get(int(ln["intent"]))
        if i is None:
            continue
        for uid, rows in (i.get("op") or {}).items():
            end = next((r for r in rows if r[1] in ("failed", "superseded", "cancelled", "requeued", "unconfirmed")), None)
            out.append({"uid": uid, "intent": int(ln["intent"]), "start": rows[0][0] if rows else None,
                        "end": end[0] if end else None, "why": (end[2] if end else None)})
    return out


def cause_of(rec: dict[str, Any], pic: Picture, truth: Truth, n: dict[str, Any], wrong: list[int]) -> str:
    cid = n["contact"]
    D = float(n["deadline"])
    t0 = float(n["t_need"])
    plat = platforms(pic)
    lt = light(rec)
    kD = pic.index(D)
    tracked = any(pic.track_of(cid, k) is not None for k in range(0, (kD or 0) + 1))
    if not tracked:
        return "not_tracked"
    tid = pic.track_of(cid, kD)
    idn = pic.identity(tid, kD) if tid else None
    true_kind = str(n.get("kind"))
    shown = level_shown(idn["kind"] if idn else None, true_kind)
    if n.get("ais_spoofed") and idn and idn["kind"] != true_kind:
        # the picture still believes the spoofed AIS (or a family of it): no camera identified it
        return f"spoof_believed@{lt}"
    if idn and (idn.get("conflict") or shown == "wrong_kind"):
        looked = new_hits(pic, cid, rule(n["domain"]).sensors, t0 - LOOK_BEFORE_S, D)
        who = [str(u) for u in wrong if str(u) in looked]
        return f"misidentified:{plat.get(who[0], 'unknown') if who else 'unknown'}"
    sent = agent_vehicles(rec, cid)
    sensors = tuple(rule(n["domain"]).sensors)
    if sent:
        hit = new_hits(pic, cid, sensors, t0 - LOOK_BEFORE_S, D)
        lost = [s for s in sent if s["end"] is not None and s["end"] < D and hit.get(s["uid"], 0) == 0]
        if len(lost) == len(sent):
            return f"lost:{plat.get(lost[0]['uid'], lost[0]['uid'])}"
    looked = new_hits(pic, cid, sensors, t0 - LOOK_BEFORE_S, D)
    if not looked:
        return f"no_look@{lt}"
    top = max(sorted(looked), key=lambda u: looked[u])
    tail = ",late" if n.get("t_served") is not None else ""
    return f"looked:{plat.get(top, top)}@{lt}({shown}){tail}"


def single_cause(causes: list[str]) -> dict[str, Any]:
    """The most frequent cause's key and share of the lost needs (keys compared whole, and by
    their part before `@` or `(`: a platform or a gap regardless of light)."""
    if not causes:
        return {"lost": 0, "top": None, "share": None, "top_coarse": None, "share_coarse": None, "counts": {}}
    c = Counter(causes)
    key, k = c.most_common(1)[0]
    coarse = Counter(x.split("@")[0].split("(")[0] for x in causes)
    ck, cn = coarse.most_common(1)[0]
    return {"lost": len(causes), "top": key, "share": round(k / len(causes), 3), "top_coarse": ck,
            "share_coarse": round(cn / len(causes), 3), "counts": dict(c.most_common())}
