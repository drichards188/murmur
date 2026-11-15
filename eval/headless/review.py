"""The review work on simulated time (docs/HEADLESS.md, section 16): a scripted operator who
works the identification review list and the review a nomination waits on, through the REST
paths a person's console uses, each act under the operator's own id and logged by the bridge as
the live stack logs it.

**A script with stated rules, not judgment, and never human data.** What it does with each kind
of item is its policy (`ReviewPolicy`, presets in `PRESETS`, a batch's `Options.review`):

| item | where it reads it | what it may do | REST path |
|---|---|---|---|
| a rule declaration awaiting an operator (rules 9) | `GET /api/review` `declarations` | confirm or revoke | `POST /api/tracks/{id}/confirm`, `.../revoke` |
| a review flag in force (rules 9) | `GET /api/review` `flags` | confirm, dismiss, or declare the flagged track hostile | `POST /api/review/{flag}/resolve`, `POST /api/tracks/{id}/declare` |
| a lost contact a waiting nomination's keep-out counts (rules 16) | `GET /api/inbox`, a nomination's `engage.lost_open` | accept it for that engagement (engagement authority) | `POST /api/intents/{id}/accept_unknown` `{track, since}` |
| a live track that blocks a waiting nomination's keep-out (rules 19) | `GET /api/inbox`, a nomination's `engage.behaving` (while `behaving_acceptable_now`) | accept it as behaving now for that engagement (engagement authority) when the explanation says that clears it (`acceptable` and `clears`); `live_blockers = "accept_aircraft"` only an aircraft, `"accept_clearing"` any | `POST /api/intents/{id}/accept_behaving` `{track}` |
| a live track carrying a lost contact's AIS (rules 16) | `GET /api/review` `lost_claims` | confirm it, only when nothing contradicts it | `POST /api/review/lost/{track}/confirm` `{by_track}` |
| a platform spent by its own effect but still heard (rules 15) | `GET /api/inbox` alerts of kind `spent` | un-spend it (engagement authority) | `POST /api/restores/unspend` `{uid, note}` |
| an agent's engagement nomination the engine refused (`adopt`, off by default) | `GET /api/intents` (rejected, from an agent, in the last `ADOPT_WITHIN_S`) | nominate it as the adopting operator's own (`ADOPTER`, engagement authority, never a signer), the same track, platform, radius, fallback and window, with every blocker accepted at the nomination (rules 16 and 19), only when the form (`POST /api/engage/options`) says the keep-outs alone fail and every blocker is a lost contact or a live track that accepting as behaving now clears (`"accept_aircraft"`: only aircraft among the live ones) | `POST /api/intents` task engage with `accept_lost`, `accept_behaving` |

**Adoption** (bd murmur-e3b): an agent's nomination can never carry an acceptance, and one the
keep-out blocks is refused when proposed, so without it a person's acceptance reaches only a
nomination that waits (a blocker that came after it). The console's person reads the refused
nomination's form and nominates it themselves with the blockers accepted; `adopt` is that act,
scripted. Its nominations are the adopter's (the bridge logs them under its session); each
decision names the agent's refused nomination (`item`) and the adopter's own (`intent`), and the
labels keep them apart from the agent's (v3/labels_ops.py `adopted_nominations`).

Each item is acted on once (the engine refuses what is no longer open, and that answer is
recorded). The operator looks every `attend_s` (lognormal, `attend_sigma`; 0 for a fixed
interval) and reads each item for `read_s` before acting, as eval/proxy.py's
`confirm_rule_declarations` reads a declaration first. Draws from
`random.Random(f"review:{seed}")`.

**The evidence reviewer** (`declarations`, `flags` or `ais_claims` = `"evidence"`; preset
`evidence`, scoring v3's F16, eval/SCORING-V3.md): still a script and truth-blind, it decides on
what the picture shows, and reads less as its load grows, as eval/proxy.py's operator reads
fewer cards in a long queue. Its load is the review items open plus the cards that reached an
operator in the last `load_window_s` (`GET /api/intents`); at an item's first read it is
diligent with probability `diligence * 2 ** -(load / fatigue_load)` (drawn once per item).
Not diligent, it says yes to what the engine asks (confirms the declaration, the flag, the
claim). Diligent, it reads the track's identity (`GET /api/tracks`; EO/IR evidence is an
identification at `EVIDENCE_CONF` or more that EO/IR supports) and the claim's own evidence:

| item | diligent: acts when | waits for evidence, then (after `max_wait_s` from its first read) |
|---|---|---|
| a rule declaration | revokes when EO/IR shows a civil kind (`CIVIL_KINDS`), confirms when it shows another | confirms on the act's evidence alone |
| a proximity flag | dismisses it once read (keeping company is weak evidence: it never declares) | |
| another flag | declares the one named track EO/IR shows as a non-civil kind (an attribution among several), dismisses when EO/IR shows every named track civil | dismisses (weak evidence never declares) |
| an AIS claim | confirms when EO/IR shows the claimant as the kind its AIS names (`AIS_KIND`), declines (`decline`, no request) when it shows another, when the claimant's identity conflicts or the engine names a blocker | leaves it (`leave`, no request) |

A decision with no request (`decline`, `leave`) is recorded with `result` "done (no request)".
Every flag decision records the flag's tracks (`tracks`), and an AIS claim's the claimant's
track (`by_track`), so each can be judged against truth (SCORING-V3.md 2.9, choice 9).
"""

from __future__ import annotations

import math
import random
from dataclasses import asdict, dataclass, field
from typing import Any

from eval.situation import AUTOPLAY_OPERATOR, SCRIPTED_SIGNERS

DECLARATIONS = ("confirm", "revoke", "leave", "evidence")
FLAGS = ("confirm", "dismiss", "declare", "leave", "evidence")
LOST = ("accept", "leave")
CLAIMS = ("confirm", "leave", "evidence")
UNSPEND = ("unspend", "leave")
#: rules 19: what the reviewer does with a live track blocking a waiting nomination's keep-out
LIVE = ("accept_aircraft", "accept_clearing", "leave")
#: adoption (module doc): which live blockers an adopted nomination may accept (lost contacts always)
ADOPT = ("accept_aircraft", "accept_clearing", "leave")
#: the adopting operator: a scripted operator with engagement authority, never one of the signers
#: (an engagement's proposer never signs it); in the bridge's rosters only when a policy adopts
ADOPTER = "op-4"
#: a refusal older than this (situation s) is not adopted: the picture it was refused on has moved
ADOPT_WITHIN_S = 15.0
#: rules 19: a live blocker still blocking this much later (by its estimates) is a new item
RETRY_S = 30.0
#: the evidence reviewer: what it calls a civil kind (a camera's identification of one contradicts
#: a hostile declaration), and the confidence an EO/IR identification must show to count
CIVIL_KINDS = ("ferry", "cargo_ship", "tanker", "sailboat", "fishing_vessel", "patrol_boat", "light_aircraft",
               "helicopter", "vehicle", "truck", "person")
EVIDENCE_CONF = 0.6
NO_REQUEST = "done (no request)"
#: the kind an AIS ship type names (crates/murmur-bridge tracks/model.rs `ais_kind`)
AIS_KIND = {"passenger": "ferry", "cargo": "cargo_ship", "tanker": "tanker", "fishing": "fishing_vessel",
            "pleasure": "sailboat", "sailing": "sailboat", "law_enforcement": "patrol_boat", "military": "patrol_boat",
            "sar": "patrol_boat", "tug": "tug"}


@dataclass
class ReviewPolicy:
    # any operator confirms, revokes, resolves and confirms claims; accepting a lost contact
    # and un-spending a platform take engagement authority
    operator: str = AUTOPLAY_OPERATOR
    engage_operator: str = SCRIPTED_SIGNERS[0]
    attend_s: float = 5.0
    attend_sigma: float = 0.0
    read_s: float = 3.0
    declarations: str = "confirm"
    flags: str = "leave"
    lost_contacts: str = "leave"
    ais_claims: str = "leave"
    unspend: str = "leave"
    # rules 19: accept a live blocker as behaving now when the explanation says that clears it
    live_blockers: str = "leave"
    # an agent's refused nomination nominated as the adopter's own, its blockers accepted (module doc)
    adopt: str = "leave"
    adopter: str = ADOPTER
    label: str = "scripted"
    preset: str = "confirm-declarations"
    # the evidence reviewer (module doc): read only when an item's policy is "evidence"
    diligence: float = 0.9
    fatigue_load: float = 8.0
    load_window_s: float = 120.0
    max_wait_s: float = 180.0

    def as_dict(self) -> dict[str, Any]:
        out = {**asdict(self), "scripted": True}
        if self.live_blockers == "leave":
            # a policy that accepts no live blocker records as it did before rules 19
            out.pop("live_blockers")
        if self.adopt == "leave":
            # nor one that adopts nothing as before adoption
            out.pop("adopt")
            out.pop("adopter")
        if not self.uses_evidence():
            # a policy that reads no evidence records as it did before the evidence reviewer
            for k in EVIDENCE_KEYS:
                out.pop(k)
        return out

    def uses_evidence(self) -> bool:
        return "evidence" in (self.declarations, self.flags, self.ais_claims)


EVIDENCE_KEYS = ("diligence", "fatigue_load", "load_window_s", "max_wait_s")


PRESETS: dict[str, dict[str, Any]] = {
    # what a situation's autoplay does with `confirm_rule_declarations` (eval/proxy.py): confirm
    # every rule declaration, leave the rest
    "confirm-declarations": {},
    # yes to everything the review list asks: every declaration and flag confirmed, every lost
    # contact a nomination waits on accepted, every uncontradicted AIS claim confirmed
    "accept-all": {"flags": "confirm", "lost_contacts": "accept", "ais_claims": "confirm"},
    # no to the engine's declarations and flags: revokes every rule declaration, dismisses
    # every flag, accepts nothing
    "skeptic": {"declarations": "revoke", "flags": "dismiss"},
    # nobody reviews
    "absent": {"declarations": "leave"},
    # decides on what the picture shows, reading less under load (module doc; scoring v3's F16)
    "evidence": {"declarations": "evidence", "flags": "evidence", "ais_claims": "evidence"},
    # rules 19 (bd murmur-j0x): accept-all's lost contacts, and a live aircraft that blocks a
    # waiting nomination accepted as behaving now when the explanation says that clears it
    "accept-behaving": {"flags": "confirm", "lost_contacts": "accept", "ais_claims": "confirm",
                        "live_blockers": "accept_aircraft"},
}


def review_policy(spec: dict[str, Any] | None = None) -> ReviewPolicy:
    """From a batch's `review` option: `{"preset": ..., ...overrides}`."""
    spec = dict(spec or {})
    name = str(spec.pop("preset", "confirm-declarations"))
    if name not in PRESETS:
        raise ValueError(f"unknown review preset {name!r}; presets: {', '.join(PRESETS)}")
    p = ReviewPolicy(preset=name)
    for k, v in {**PRESETS[name], **spec}.items():
        if not hasattr(p, k) or k == "preset":
            raise ValueError(f"unknown review parameter {k!r}")
        setattr(p, k, float(v) if isinstance(getattr(p, k), float) else v)
    for k, allowed in (("declarations", DECLARATIONS), ("flags", FLAGS), ("lost_contacts", LOST),
                       ("ais_claims", CLAIMS), ("unspend", UNSPEND), ("live_blockers", LIVE), ("adopt", ADOPT)):
        if getattr(p, k) not in allowed:
            raise ValueError(f"review {k}: {getattr(p, k)!r} is not one of {', '.join(allowed)}")
    if p.attend_s <= 0 or p.read_s < 0:
        raise ValueError("attend_s is positive and read_s not negative")
    if p.adopt != "leave" and p.adopter in SCRIPTED_SIGNERS:
        raise ValueError(f"the adopter {p.adopter!r} is a signer: an engagement's proposer never signs it")
    if not (0.0 <= p.diligence <= 1.0) or p.fatigue_load <= 0 or p.load_window_s <= 0 or p.max_wait_s < 0:
        raise ValueError("diligence is a probability; fatigue_load and load_window_s positive; max_wait_s not negative")
    return p


# ---- what the policy does with each item (pure, for tests) -------------------------------------

def declaration_act(p: ReviewPolicy, item: dict[str, Any]) -> dict[str, Any] | None:
    """A rule declaration awaiting an operator (`GET /api/review` `declarations`)."""
    if p.declarations == "leave" or not item.get("track"):
        return None
    track = str(item["track"])
    if p.declarations == "evidence":
        if not {"confirm", "revoke"} <= set(item.get("actions") or []):
            return None
        return {"key": f"decl:{item.get('id')}", "kind": "declaration", "action": "evidence", "item": item.get("id"),
                "track": track, "by": p.operator}
    if p.declarations not in (item.get("actions") or []):
        return None
    why = f"{p.label} reviewer (not a person): {p.declarations}s every rule declaration ({item.get('basis') or item.get('declared_by') or ''})"[:400]
    body = {"note": why} if p.declarations == "revoke" else {}
    return {"key": f"decl:{item.get('id')}", "kind": "declaration", "action": p.declarations, "item": item.get("id"),
            "track": track, "by": p.operator, "path": f"/api/tracks/{track}/{p.declarations}", "body": body}


def flag_act(p: ReviewPolicy, item: dict[str, Any]) -> dict[str, Any] | None:
    """A review flag in force (`GET /api/review` `flags`). The decision records the flag's tracks."""
    if p.flags == "leave":
        return None
    flag = item.get("flag")
    tracks = [str(t) for t in item.get("tracks") or []]
    if p.flags == "evidence":
        if "dismiss" not in (item.get("actions") or []):
            return None
        return {"key": f"flag:{flag}", "kind": "flag", "action": "evidence", "item": item.get("id"), "flag": flag,
                "flag_kind": item.get("flag_kind"), "tracks": tracks, "by": p.operator}
    if p.flags not in (item.get("actions") or []):
        return None
    note = f"{p.label} reviewer (not a person): {p.flags} every review flag ({item.get('flag_kind')})"
    if p.flags == "declare":
        if len(tracks) != 1:
            return None
        return {"key": f"flag:{flag}", "kind": "flag", "action": "declare", "item": item.get("id"), "track": tracks[0],
                "tracks": tracks, "by": p.operator, "path": f"/api/tracks/{tracks[0]}/declare",
                "body": {"affiliation": "hostile", "flag": flag, "note": note}}
    resolution = "confirmed" if p.flags == "confirm" else "dismissed"
    return {"key": f"flag:{flag}", "kind": "flag", "action": p.flags, "item": item.get("id"), "tracks": tracks,
            "by": p.operator, "path": f"/api/review/{flag}/resolve", "body": {"resolution": resolution, "note": note}}


def claim_act(p: ReviewPolicy, claim: dict[str, Any]) -> dict[str, Any] | None:
    """A live track carrying a lost contact's AIS (`GET /api/review` `lost_claims`): confirmed
    only when the engine says nothing contradicts it (`actions` holds confirm: no blockers). The
    decision records the claimant's track (`by_track`, SCORING-V3.md choice 9)."""
    if p.ais_claims == "evidence":
        return {"key": f"claim:{claim.get('id')}", "kind": "ais_claim", "action": "evidence", "item": claim.get("id"),
                "track": claim.get("lost"), "by_track": claim.get("track"), "by": p.operator}
    if p.ais_claims != "confirm" or "confirm" not in (claim.get("actions") or []):
        return None
    return {"key": f"claim:{claim.get('id')}", "kind": "ais_claim", "action": "confirm", "item": claim.get("id"),
            "track": claim.get("lost"), "by_track": claim.get("track"), "by": p.operator,
            "path": f"/api/review/lost/{claim.get('lost')}/confirm", "body": {"by_track": claim.get("track")}}


# ---- the evidence reviewer's judgment (pure, for tests) ------------------------------------------

def eo_kind(track: dict[str, Any] | None) -> str | None:
    """The kind EO/IR evidence shows for a track (`GET /api/tracks`): its identity's kind when EO/IR
    supports it at `EVIDENCE_CONF` or more without a conflict; else None."""
    idn = (track or {}).get("identity") or {}
    if idn.get("conflict") or "eo_ir" not in (idn.get("supported_by") or []):
        return None
    if float(idn.get("confidence") or 0.0) < EVIDENCE_CONF or not idn.get("kind"):
        return None
    return str(idn["kind"])


def judge(act: dict[str, Any], tracks: dict[str, dict[str, Any]], claim: dict[str, Any] | None,
          waited_s: float, max_wait_s: float) -> tuple[str, str]:
    """What a diligent evidence reviewer does with an item now: (action, why). The action is a
    request's (`confirm`, `revoke`, `declare:<track>`, `dismiss`), a decision without one
    (`decline`, `leave`), or `wait` (look again at the next look)."""
    late = waited_s >= max_wait_s
    if act["kind"] == "declaration":
        k = eo_kind(tracks.get(str(act["track"])))
        if k is not None:
            return ("revoke", f"EO/IR shows a {k}") if k in CIVIL_KINDS else ("confirm", f"EO/IR shows a {k}")
        return ("confirm", "no EO/IR evidence after the wait: the act's evidence stands") if late else ("wait", "")
    if act["kind"] == "flag":
        kinds = {t: eo_kind(tracks.get(t)) for t in act.get("tracks") or []}
        if act.get("flag_kind") == "proximity":
            # keeping company is weak evidence: read, it is dismissed whatever a look shows
            seen = ", ".join(f"{t} a {k}" for t, k in kinds.items() if k) or "no EO/IR identification"
            return "dismiss", f"proximity alone never declares ({seen})"
        armed = [t for t, k in kinds.items() if k is not None and k not in CIVIL_KINDS]
        if len(armed) == 1:
            return f"declare:{armed[0]}", f"EO/IR shows {armed[0]} a {kinds[armed[0]]}"
        if kinds and all(k is not None and k in CIVIL_KINDS for k in kinds.values()):
            return "dismiss", "EO/IR shows " + ", ".join(f"{t} a {k}" for t, k in kinds.items())
        return ("dismiss", "no telling EO/IR evidence after the wait: weak evidence never declares") if late else ("wait", "")
    if act["kind"] == "ais_claim":
        c = claim or {}
        tr = tracks.get(str(act.get("by_track"))) or {}
        idn = tr.get("identity") or {}
        want = AIS_KIND.get(str((idn.get("ais") or {}).get("ship_type") or ""))
        k = eo_kind(tr)
        if c.get("blockers") or idn.get("conflict"):
            why = "; ".join(str(b) for b in c.get("blockers") or []) or str(idn.get("conflict"))
            return "decline", f"what our sensors see contradicts the claim: {why}"[:240]
        if k is not None and want is not None and k != want:
            return "decline", f"EO/IR shows a {k}, its AIS says a {want}"
        if k is not None and k == want and "confirm" in (c.get("actions") or []):
            return "confirm", f"EO/IR shows a {k}, as its AIS says"
        return ("leave", "no EO/IR identification of the claimant after the wait") if late else ("wait", "")
    return "leave", "not an item the evidence reviewer judges"


def lost_acts(p: ReviewPolicy, card: dict[str, Any]) -> list[dict[str, Any]]:
    """The lost contacts a waiting nomination's keep-out counts that no one has accepted for it
    (`engage.lost_open`, shown while it waits with no signature), each accepted for it."""
    if p.lost_contacts != "accept" or card.get("kind") != "engage" or card.get("state") != "pending_approval":
        return []
    iid = int(card["id"])
    view = card.get("engage") or {}
    # a reset of the signatures clears the acceptances with the plan: given again after one
    reset_t = (view.get("reset") or {}).get("t")
    out = []
    for lost in view.get("lost_open") or []:
        out.append({"key": f"lost:{iid}:{lost.get('track')}:{lost.get('since')}:{reset_t}", "kind": "lost_contact",
                    "action": "accept", "item": iid, "track": lost.get("track"), "by": p.engage_operator,
                    "path": f"/api/intents/{iid}/accept_unknown",
                    "body": {"track": lost.get("track"), "since": lost.get("since")}})
    return out


def behaving_acts(p: ReviewPolicy, card: dict[str, Any]) -> list[dict[str, Any]]:
    """Rules 19: the live tracks blocking a waiting nomination's keep-out (`engage.behaving`, while
    it waits with no signature: `behaving_acceptable_now`) that the explanation says accepting as
    behaving now would clear (`acceptable` and `clears`), not accepted yet: each accepted for it
    (`accept_aircraft`: an aircraft only). A lapse ends an acceptance for good; the same track
    blocking again after one is a new item (its key counts the lapses), as after a reset, and so is
    one still blocking `RETRY_S` of its estimates later (an acceptance the engine refused, its
    velocity gone for a moment, is tried again: a person looks again)."""
    if p.live_blockers == "leave" or card.get("kind") != "engage" or card.get("state") != "pending_approval":
        return []
    view = card.get("engage") or {}
    if not view.get("behaving_acceptable_now"):
        return []
    iid = int(card["id"])
    reset_t = (view.get("reset") or {}).get("t")
    lapses = len(view.get("lapsed") or [])
    out = []
    for b in view.get("behaving") or []:
        if b.get("accepted_by") or not (b.get("acceptable") and b.get("clears")):
            continue
        if p.live_blockers == "accept_aircraft" and b.get("domain") != "air":
            continue
        track = b.get("track")
        window = int(float(b.get("estimate_t") or 0) // (RETRY_S * 1000.0))
        out.append({"key": f"behaving:{iid}:{track}:{reset_t}:{lapses}:{window}", "kind": "live_blocker", "action": "accept",
                    "item": iid, "track": track, "domain": b.get("domain"), "by": p.engage_operator,
                    "why": str(b.get("reason") or "")[:240],
                    "path": f"/api/intents/{iid}/accept_behaving", "body": {"track": track}})
    return out


def adopt_acts(p: ReviewPolicy, intents: list[dict[str, Any]], now_ms: float) -> list[dict[str, Any]]:
    """Adoption (module doc): each engagement nomination from an agent the engine refused in the
    last `ADOPT_WITHIN_S` (`GET /api/intents`), to be read and, when its form allows, nominated by
    the adopter with its blockers accepted."""
    if p.adopt == "leave":
        return []
    out = []
    for i in intents:
        src = i.get("source") or {}
        if i.get("kind") != "engage" or src.get("kind") != "agent" or i.get("state") != "rejected":
            continue
        if now_ms - float(i.get("created_t") or 0.0) > ADOPT_WITHIN_S * 1000.0:
            continue
        prm = i.get("params") or {}
        uids = [int(u) for u in i.get("uids") or [] if str(u).isdigit()]
        if not prm.get("track") or len(uids) != 1:
            continue
        until = (prm.get("window") or {}).get("until")
        out.append({"key": f"adopt:{i.get('id')}", "kind": "adopt", "action": "adopt", "item": int(i["id"]),
                    "track": str(prm["track"]), "uid": uids[0], "by": p.adopter, "agent": src.get("id"),
                    "radius_m": float(prm.get("effect_radius_m") or 75.0), "fallback": prm.get("fallback") or "return",
                    "until_ms": float(until) if isinstance(until, (int, float)) else None})
    return out


def adoption(p: ReviewPolicy, option: dict[str, Any] | None) -> tuple[dict[str, list[dict[str, Any]]] | None, str]:
    """What the adopter would accept to nominate a refused nomination on its platform's form
    (`POST /api/engage/options`, the option for that platform), or None and why not: only the
    keep-outs fail, every blocker is listed (the form lists 12 at most), none is one of ours, and
    each is a lost contact (rules 16, accepted since its last fix) or a live track that accepting
    as behaving now clears (rules 19; `accept_aircraft`: an aircraft only)."""
    if option is None:
        return None, "the form has no option for the platform"
    failing = {str(c.get("rule")) for c in option.get("checks") or [] if c.get("outcome") != "pass"}
    if option.get("passed"):
        return {"accept_lost": [], "accept_behaving": []}, "it passes now"
    if not failing or failing - {"keep_out_tracks", "keep_out_friendly"}:
        return None, "checks other than the keep-outs fail: " + ", ".join(sorted(failing - {"keep_out_tracks", "keep_out_friendly"}))
    ko = option.get("keep_outs") or {}
    items = [it for it in ko.get("items") or [] if it.get("blocks")]
    if isinstance(ko.get("blocking"), int) and ko["blocking"] > len(items):
        return None, f"{ko['blocking']} block, the form lists {len(items)}"
    lost: list[dict[str, Any]] = []
    behaving: list[dict[str, Any]] = []
    for it in items:
        if it.get("subject") == "vehicle":
            return None, f"one of ours blocks ({it.get('label') or it.get('id')})"
        if it.get("lost"):
            lost.append({"track": str(it["lost"].get("track") or it.get("id")), "since": int(it["lost"]["since"])})
            continue
        b = it.get("behaving") or {}
        if not (b.get("acceptable") and b.get("clears")):
            why = b.get("why_not") or ("accepting it would not clear it" if b else "it cannot be accepted")
            return None, f"{it.get('label') or it.get('id')}: {why}"[:240]
        if p.adopt == "accept_aircraft" and b.get("domain") != "air":
            return None, f"{it.get('label') or it.get('id')}: a live blocker not in the air"
        behaving.append({"track": str(b.get("track") or it.get("id"))})
    return {"accept_lost": lost, "accept_behaving": behaving}, f"accepting {len(lost)} lost and {len(behaving)} live"


def unspend_acts(p: ReviewPolicy, alerts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Platforms spent by their own effect but still heard (inbox alerts of kind `spent`)."""
    if p.unspend != "unspend":
        return []
    out = []
    for a in alerts:
        if a.get("kind") != "spent" or a.get("uid") is None:
            continue
        uid = int(a["uid"])
        out.append({"key": f"unspend:{uid}:{a.get('id')}", "kind": "unspend", "action": "unspend", "item": a.get("id"),
                    "uid": uid, "by": p.engage_operator, "path": "/api/restores/unspend",
                    "body": {"uid": uid, "note": f"{p.label} reviewer (not a person): un-spends every platform spent but still heard"}})
    return out


@dataclass
class Reviewer:
    """The reviewer as a process of the episode (`wake(now)` acts at `now` and says when next)."""

    sim: Any
    policy: ReviewPolicy
    seed: int
    decisions: list[dict[str, Any]] = field(default_factory=list)
    # the scenario clock's epoch (unix s at situation time 0): the evidence reviewer's load counts
    # the cards of the last `load_window_s` from the intents' history (None: no cards counted)
    epoch: float | None = None

    def __post_init__(self) -> None:
        self.rng = random.Random(f"review:{int(self.seed)}")
        self.done: set[str] = set()
        self._todo: list[dict[str, Any]] = []
        self._reading: dict[str, Any] | None = None
        # the evidence reviewer: each item's first read, its diligence draw, and the items it waits
        # on until its next look
        self._first: dict[str, float] = {}
        self._diligent: dict[str, dict[str, Any]] = {}
        self._defer: dict[str, float] = {}
        self._review: dict[str, Any] = {}

    def describe(self) -> dict[str, Any]:
        return self.policy.as_dict()

    def _idle(self) -> bool:
        p = self.policy
        return all(getattr(p, k) == "leave"
                   for k in ("declarations", "flags", "lost_contacts", "ais_claims", "unspend", "live_blockers", "adopt"))

    def start(self, now: float) -> float | None:
        return None if self._idle() else now

    def _interval(self) -> float:
        p = self.policy
        return p.attend_s * math.exp(self.rng.gauss(0.0, p.attend_sigma)) if p.attend_sigma > 0 else p.attend_s

    def items(self, now: float | None = None) -> list[dict[str, Any]]:
        """Everything the policy would act on now, not yet done, in a fixed order: declarations,
        flags, AIS claims (the review list), then lost contacts and spent platforms (the inbox).
        An item the evidence reviewer waits on is read again at its next look, not before."""
        acts: list[dict[str, Any] | None] = []
        status, review = self.sim.api("GET", "/api/review")
        if status == 200 and review:
            self._review = review
            acts += [declaration_act(self.policy, d) for d in review.get("declarations") or []]
            # the flags in force: the bridge lists them under `flags` (a fake may say `open`)
            acts += [flag_act(self.policy, f) for f in (review.get("flags") if "flags" in review else review.get("open")) or []]
            acts += [claim_act(self.policy, c) for c in review.get("lost_claims") or []]
        if now is not None:
            acts = [a for a in acts if a is None or self._defer.get(a["key"], -1.0) <= now + 1e-9]
        if self.policy.lost_contacts != "leave" or self.policy.unspend != "leave" or self.policy.live_blockers != "leave":
            status, inbox = self.sim.api("GET", "/api/inbox")
            if status == 200 and inbox:
                for card in sorted(inbox.get("pending") or [], key=lambda c: int(c.get("id", 0))):
                    acts += lost_acts(self.policy, card)
                    acts += behaving_acts(self.policy, card)
                acts += unspend_acts(self.policy, inbox.get("alerts") or [])
        if self.policy.adopt != "leave" and now is not None and self.epoch is not None:
            status, body = self.sim.api("GET", "/api/intents")
            if status == 200 and body:
                acts += adopt_acts(self.policy, body.get("intents") or [], (self.epoch + now) * 1000.0)
        seen: set[str] = set()
        out = []
        for a in acts:
            if a is None or a["key"] in self.done or a["key"] in seen:
                continue
            seen.add(a["key"])
            out.append(a)
        return out

    def wake(self, now: float) -> float | None:
        if self._reading is not None:
            act, self._reading = self._reading, None
            self._do(now, act)
        if not self._todo:
            self._todo = self.items(now)
            if not self._todo:
                return now + self._interval()
        act = self._todo.pop(0)
        self.done.add(act["key"])
        self._reading = {**act, "opened_t": now}
        return now + self.policy.read_s if self.policy.read_s > 0 else now

    def _do(self, now: float, act: dict[str, Any]) -> None:
        if act.get("action") == "evidence":
            act = self._judged(now, act)
            if act is None:
                return
        if act.get("kind") == "adopt":
            act = self._adopted(now, act)
        out: Any = None
        if act.get("path") is None:
            result = NO_REQUEST
        else:
            status, out = self.sim.api("POST", act["path"], act["body"], act["by"])
            if status // 100 == 2:
                result = "done"
            else:
                result = f"error {status}: {(out or {}).get('detail') if isinstance(out, dict) else out}"[:400]
        rec = {k: act[k] for k in ("kind", "action", "item", "by") if k in act}
        for k in ("track", "tracks", "by_track", "uid", "domain", "why", "diligent", "load", "agent", "accepted"):
            if k in act:
                rec[k] = act[k]
        if act.get("kind") == "adopt" and result == "done":
            # the adopter's own nomination, and the agent's refused one it adopts (`item`)
            made = out.get("intent") if isinstance(out, dict) else None
            rec["intent"] = made.get("id") if isinstance(made, dict) else None
        self.decisions.append({"t": round(now, 3), "read_from_t": round(act["opened_t"], 3), **rec, "result": result})

    # ---- adoption ----------------------------------------------------------------------------

    def _adopted(self, now: float, act: dict[str, Any]) -> dict[str, Any]:
        """An agent's refused nomination read: its platform's form asked now, and the adopter's
        nomination with every blocker accepted when `adoption` allows it, else a decision with no
        request saying why not."""
        p = self.policy
        window_s = 600
        if act.get("until_ms") is not None and self.epoch is not None:
            window_s = int(max(0.0, (act["until_ms"] - (self.epoch + now) * 1000.0) / 1000.0))
        form = {"track": act["track"], "effect_radius_m": act["radius_m"], "window_s": window_s,
                "fallback": act["fallback"], "uids": [act["uid"]]}
        status, body = self.sim.api("POST", "/api/engage/options", form, p.adopter)
        opts = (body or {}).get("options") or [] if status == 200 else []
        option = next((o for o in opts if int(o.get("uid") or -1) == int(act["uid"])), None)
        accepted, why = adoption(p, option) if window_s > 0 else (None, "its window has closed")
        if accepted is None:
            return {**act, "action": "leave", "why": why, "path": None, "body": None}
        note = (f"{p.label} operator (not a person): adopts {act.get('agent')}'s refused nomination i{act['item']} "
                f"on {act['track']}, {why}")[:400]
        req = {"task": "engage", "uids": [act["uid"]], "track": act["track"], "effect_radius_m": act["radius_m"],
               "window_s": window_s, "fallback": act["fallback"], "rationale": note, **accepted}
        return {**act, "why": why, "accepted": accepted, "path": "/api/intents", "body": req}

    # ---- the evidence reviewer ---------------------------------------------------------------

    def load(self, now: float) -> int:
        """The review items open now plus the cards that reached an operator in the last
        `load_window_s` (the intents whose history went to `pending_approval` then)."""
        c = (self._review.get("counts") or {}) if self._review else {}
        n = int(c.get("declarations") or 0) + int(c.get("flags") or 0) + int(c.get("lost_claims") or 0)
        if self.epoch is None:
            return n
        status, body = self.sim.api("GET", "/api/intents")
        lo_ms = (self.epoch + now - self.policy.load_window_s) * 1000.0
        for i in ((body or {}).get("intents") or []) if status == 200 else []:
            if any(h.get("state") == "pending_approval" and float(h.get("t") or 0.0) >= lo_ms for h in i.get("history") or []):
                n += 1
        return n

    def _judged(self, now: float, act: dict[str, Any]) -> dict[str, Any] | None:
        """An evidence item read: the act to carry out (a request, or a decision with none), or
        None when the reviewer waits on it (read again at its next look)."""
        p, key = self.policy, act["key"]
        first = self._first.setdefault(key, act["opened_t"])
        if key not in self._diligent:
            load = self.load(now)
            pr = p.diligence * 2.0 ** (-load / p.fatigue_load)
            self._diligent[key] = {"diligent": self.rng.random() < pr, "load": load, "p": round(pr, 3)}
        d = self._diligent[key]
        claim = None
        if act["kind"] == "ais_claim":
            claim = next((c for c in self._review.get("lost_claims") or [] if c.get("id") == act["item"]), None)
            if claim is None:
                status, review = self.sim.api("GET", "/api/review")
                claim = next((c for c in ((review or {}).get("lost_claims") or []) if c.get("id") == act["item"]), None)
        if not d["diligent"]:
            # a yes to what the engine asks, unread
            choice, why = ("confirm", "skimmed under load: yes to what the engine asks")
            if act["kind"] == "ais_claim" and "confirm" not in ((claim or {}).get("actions") or []):
                choice, why = ("leave", "skimmed under load: the claim cannot be confirmed")
        else:
            status, body = self.sim.api("GET", "/api/tracks")
            tracks = {str(t.get("id")): t for t in ((body or {}).get("tracks") or [])} if status == 200 else {}
            choice, why = judge(act, tracks, claim, now - first, p.max_wait_s)
        if choice == "wait":
            self.done.discard(key)
            self._defer[key] = now + self._interval()
            return None
        note = f"{p.label} reviewer (not a person), {'read' if d['diligent'] else 'skimmed'}: {why}"[:400]
        out = {**act, "why": why, "diligent": d["diligent"], "load": d["load"]}
        if act["kind"] == "declaration" and choice in ("confirm", "revoke"):
            out.update(action=choice, path=f"/api/tracks/{act['track']}/{choice}",
                       body={"note": note} if choice == "revoke" else {})
        elif act["kind"] == "flag" and choice.startswith("declare:"):
            t = choice.split(":", 1)[1]
            out.update(action="declare", track=t, path=f"/api/tracks/{t}/declare",
                       body={"affiliation": "hostile", "flag": act["flag"], "note": note})
        elif act["kind"] == "flag" and choice in ("confirm", "dismiss"):
            out.update(action=choice, path=f"/api/review/{act['flag']}/resolve",
                       body={"resolution": "confirmed" if choice == "confirm" else "dismissed", "note": note})
        elif act["kind"] == "ais_claim" and choice == "confirm":
            out.update(action="confirm", path=f"/api/review/lost/{act['track']}/confirm",
                       body={"by_track": act["by_track"]})
        else:
            out.update(action=choice, path=None, body=None)
        return out
