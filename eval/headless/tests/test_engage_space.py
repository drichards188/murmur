"""Section 16 of docs/HEADLESS.md: the engagement chain, the review work and the space segment
headless.

Unit tests (no binary): the scripted signers' and reviewer's policies, the scripted agent's
choices and the pinned element sets. Smoke episodes (the real `murmur-headless`, built here if
stale): the engagement chain end to end on `hostile-act-engagement` (a hostile act, the rule
declaration, its review, the nomination, two signatures, the effect, the assessment, what is
owed back), its abort (the fallback and the restore), its miss (a munition that survives and an
operator who un-spends it), a lost contact that blocks until its AIS claim is confirmed, a
collect and a cover on the pinned snapshot, each audit log replayed with its anchors; and two
runs of one seed the same record.
"""

from __future__ import annotations

import gzip
import json
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless import review as rv
from eval.headless import scripted as sc
from eval.headless import signers as sg
from eval.headless import space

REPLAY = REPO / "target" / "release" / "murmur-replay"


# ---- a fake bridge for the operator processes -------------------------------------------------

class FakeSim:
    """The operator API as the signers and the reviewer use it: `api(method, path, body,
    operator)`, answered by `handler`, every request kept."""

    def __init__(self, handler: Any):
        self.handler = handler
        self.calls: list[tuple[str, str, Any, Any]] = []

    def api(self, method: str, path: str, body: Any = None, operator: str | None = None) -> tuple[int, Any]:
        self.calls.append((method, path, body, operator))
        return self.handler(method, path, body, operator)


def engage_card(iid: int = 7, signed: list[str] | None = None, passed: bool = True, proposer: str | None = None,
                **view: Any) -> dict[str, Any]:
    signed = list(signed or [])
    ops = ["op-1", "op-2", "op-3"]
    v = {"approvers_needed": 2, "signed": signed, "proposer": proposer,
         "eligible": [o for o in ops if o not in signed and o != proposer], "engage_operators": ops,
         "plan": {"by": signed[0]} if signed else None,
         "live": {"passed": passed, "checks": [] if passed else [{"rule": "keep_out_tracks", "outcome": "fail"}]}}
    v.update(view)
    return {"id": iid, "kind": "engage", "state": "pending_approval",
            "source": {"kind": "agent", "id": "agent-scripted"} if proposer is None else {"kind": "human", "id": proposer},
            "engage": v}


class Engine:
    """One nomination's signatures as the engine keeps them, enough for the signers' loop."""

    def __init__(self, card: dict[str, Any] | None):
        self.card = card
        self.tokens = 0
        self.state = "pending_approval"

    def __call__(self, method: str, path: str, body: Any, op: Any) -> tuple[int, Any]:
        c = self.card
        if (method, path) == ("GET", "/api/inbox"):
            return 200, {"pending": [c] if c is not None and self.state == "pending_approval" else []}
        if path.endswith("/picture"):
            self.tokens += 1
            return 200, {"id": self.tokens, "frozen": bool(c["engage"]["signed"])}
        if path.endswith("/approve"):
            signed = c["engage"]["signed"] + [op]
            self.card = engage_card(c["id"], signed=signed, proposer=c["engage"].get("proposer"))
            if len(signed) >= 2:
                self.state = "dispatched"
            return 200, {"intent": {"state": "pending_approval" if len(signed) < 2 else "dispatched"}, "dispatched": len(signed) >= 2}
        if path.endswith("/reject"):
            self.state = "rejected"
            return 200, {"intent": {"state": "rejected"}}
        if path.endswith("/abort"):
            return 200, {"intent": {"state": "aborted"}}
        if (method, path) == ("GET", "/api/intents"):
            return 200, {"intents": []}
        return 404, {"detail": f"unexpected {method} {path}"}


def drive(proc: Any, start: float, until: float) -> list[float]:
    """Run one process alone: its wakes until `until`."""
    t = proc.start(start)
    wakes = []
    while t is not None and t <= until:
        wakes.append(t)
        t = proc.wake(t)
    return wakes


# ---- the signers ------------------------------------------------------------------------------

def test_signer_presets_and_their_refusals():
    assert sg.signer_policy(None).as_dict()["operators"] == ["op-2", "op-3"]
    assert sg.signer_policy({"preset": "deliberate"}).read_s == 20.0
    assert sg.signer_policy({"preset": "cautious"}).decline_if == ("accepted_lost", "accepted_unknown", "changed")
    assert sg.signer_policy({"preset": "abort"}).abort_after_s == 20.0
    for bad in ({"preset": "nope"}, {"mode": "maybe"}, {"operators": ["op-2"]}, {"operators": ["op-2", "op-2"]},
                {"decline_if": ["moon"]}, {"mode": "rates", "p_sign": 0.8, "p_decline": 0.4}, {"read_s": 0},
                {"colour": "red"}):
        with pytest.raises(ValueError):
            sg.signer_policy(bad)


def test_two_signers_look_read_and_sign_in_turn():
    eng = Engine(engage_card())
    s = sg.Signers(FakeSim(eng), sg.signer_policy({}), seed=1)
    drive(s, 10.0, 30.0)
    signs = [d for d in s.decisions if d["action"] == "sign"]
    assert [(d["by"], d["t"], d["looked_t"]) for d in signs] == [("op-2", 13.0, 10.0), ("op-3", 16.0, 13.0)]
    assert signs[-1]["result"] == "dispatched"
    # each signature quoted the token of its own look, asked as its own operator
    looks = [(c[1], c[3]) for c in s.sim.calls if c[1].endswith("/picture")]
    approves = [(c[2]["token"], c[3]) for c in s.sim.calls if c[1].endswith("/approve")]
    assert looks == [("/api/intents/7/picture", "op-2"), ("/api/intents/7/picture", "op-3")]
    assert approves == [(1, "op-2"), (2, "op-3")]
    assert all("not a person" in c[2]["note"] for c in s.sim.calls if c[1].endswith("/approve"))


def test_the_proposer_never_signs_and_a_failing_picture_waits():
    eng = Engine(engage_card(proposer="op-2"))
    s = sg.Signers(FakeSim(eng), sg.signer_policy({}), seed=1)
    drive(s, 0.0, 3.0)
    assert [d["by"] for d in s.decisions if d["action"] == "sign"] == ["op-3"]
    eng2 = Engine(engage_card(passed=False))
    s2 = sg.Signers(FakeSim(eng2), sg.signer_policy({}), seed=1)
    drive(s2, 0.0, 30.0)
    assert not [c for c in s2.sim.calls if c[1].endswith("/picture")]
    waits = [d for d in s2.decisions if d["action"] == "wait"]
    assert len(waits) == 1 and "keep_out_tracks" in waits[0]["reason"], "said once per reason"


def test_decline_never_and_cautious():
    eng = Engine(engage_card())
    s = sg.Signers(FakeSim(eng), sg.signer_policy({"preset": "decline"}), seed=1)
    drive(s, 0.0, 10.0)
    assert [(d["action"], d["by"], d["result"]) for d in s.decisions] == [("decline", "op-2", "rejected")]
    s = sg.Signers(FakeSim(Engine(engage_card())), sg.signer_policy({"preset": "absent"}), seed=1)
    assert s.start(0.0) is None and s.wake(0.0) is None and not s.sim.calls
    card = engage_card(accepted_lost=[{"track": "T-0003", "since": 1, "by": "op-2"}])
    p = sg.signer_policy({"preset": "cautious"})
    assert sg.objections(p, card) and "T-0003" in sg.objections(p, card)[0]
    assert sg.objections(sg.signer_policy({}), card) == []
    changed = engage_card()
    changed["engage"]["live"]["changed"] = [{"rule": "target_recency", "worse": True}]
    assert sg.objections(p, changed) == ["checks worse than at nomination: target_recency"]
    s = sg.Signers(FakeSim(Engine(card)), p, seed=1)
    drive(s, 0.0, 10.0)
    assert [(d["action"], d["by"]) for d in s.decisions] == [("decline", "op-2")]
    assert "T-0003" in s.decisions[0]["reason"]


def test_rates_draw_each_nomination_once_from_the_seed():
    def fates(seed: int) -> list[str]:
        p = sg.signer_policy({"mode": "rates", "p_sign": 0.5, "p_decline": 0.25, "latency_median_s": 1.0})
        s = sg.Signers(FakeSim(Engine(None)), p, seed=seed)
        return [s._fate_of(i, 0.0)[0] for i in range(40)]

    a, b = fates(3), fates(3)
    assert a == b and fates(4) != a
    assert {"sign", "decline", "ignore"} <= set(a)
    p = sg.signer_policy({"mode": "rates", "p_sign": 0.0, "p_decline": 0.0})
    s = sg.Signers(FakeSim(Engine(engage_card())), p, seed=1)
    drive(s, 0.0, 30.0)
    assert [d["action"] for d in s.decisions] == ["wait"]


def test_abort_after_dispatch():
    epoch = 1_000_000.0

    def handler(method: str, path: str, body: Any, op: Any) -> tuple[int, Any]:
        if path == "/api/intents":
            return 200, {"intents": [{"id": 9, "kind": "engage", "state": "dispatched",
                                      "history": [{"state": "dispatched", "t": (epoch + 100.0) * 1000}]}]}
        if path == "/api/inbox":
            return 200, {"pending": []}
        if path.endswith("/abort"):
            return 200, {"intent": {"state": "aborted"}}
        return 404, {}

    s = sg.Signers(FakeSim(handler), sg.signer_policy({"preset": "abort"}), seed=1, epoch=epoch)
    drive(s, 100.0, 130.0)
    aborts = [d for d in s.decisions if d["action"] == "abort"]
    assert len(aborts) == 1 and aborts[0]["t"] == 121.0 and aborts[0]["by"] == "op-2" and aborts[0]["result"] == "aborted"


# ---- the reviewer -----------------------------------------------------------------------------

def test_review_presets_and_their_refusals():
    assert rv.review_policy(None).declarations == "confirm"
    a = rv.review_policy({"preset": "accept-all"})
    assert (a.flags, a.lost_contacts, a.ais_claims) == ("confirm", "accept", "confirm")
    assert rv.review_policy({"preset": "skeptic"}).declarations == "revoke"
    for bad in ({"preset": "x"}, {"flags": "ignore"}, {"declarations": "approve"}, {"attend_s": 0}, {"hue": 1}):
        with pytest.raises(ValueError):
            rv.review_policy(bad)


def test_review_items_become_the_console_requests():
    p = rv.review_policy({"preset": "accept-all", "unspend": "unspend"})
    decl = {"id": "D-1", "track": "T-0008", "actions": ["confirm", "revoke"], "basis": "rule hostile_act"}
    assert rv.declaration_act(p, decl)["path"] == "/api/tracks/T-0008/confirm"
    assert rv.declaration_act(rv.review_policy({"preset": "skeptic"}), decl)["path"] == "/api/tracks/T-0008/revoke"
    assert rv.declaration_act(rv.review_policy({"preset": "absent"}), decl) is None
    assert rv.declaration_act(p, {**decl, "actions": []}) is None
    flag = {"id": "F-2", "flag": 2, "flag_kind": "weak_attribution", "tracks": ["T-0004"], "actions": ["confirm", "dismiss", "declare"]}
    assert rv.flag_act(p, flag)["body"]["resolution"] == "confirmed"
    assert rv.flag_act(rv.review_policy({"preset": "skeptic"}), flag)["body"]["resolution"] == "dismissed"
    d = rv.flag_act(rv.review_policy({"flags": "declare"}), flag)
    assert d["path"] == "/api/tracks/T-0004/declare" and d["body"]["affiliation"] == "hostile" and d["body"]["flag"] == 2
    assert rv.flag_act(rv.review_policy({"flags": "declare"}), {**flag, "tracks": ["T-1", "T-2"]}) is None
    claim = {"id": "T-0002~T-0009", "lost": "T-0002", "track": "T-0009", "actions": ["confirm"], "blockers": []}
    c = rv.claim_act(p, claim)
    assert c["path"] == "/api/review/lost/T-0002/confirm" and c["body"] == {"by_track": "T-0009"}
    assert rv.claim_act(p, {**claim, "actions": [], "blockers": ["the claimant is suspect"]}) is None
    card = engage_card(lost_open=[{"track": "T-0002", "since": 1781431300000}])
    acc = rv.lost_acts(p, card)
    assert [(a["path"], a["body"], a["by"]) for a in acc] == [
        ("/api/intents/7/accept_unknown", {"track": "T-0002", "since": 1781431300000}, "op-2")]
    reset = engage_card(lost_open=[{"track": "T-0002", "since": 1781431300000}], reset={"t": 5})
    assert rv.lost_acts(p, reset)[0]["key"] != acc[0]["key"], "a reset clears the acceptances: given again"
    assert rv.lost_acts(rv.review_policy({}), card) == []
    un = rv.unspend_acts(p, [{"id": "spent:3", "kind": "spent", "uid": 3}, {"id": "x", "kind": "lost_link", "uid": 2}])
    assert [(a["path"], a["body"]["uid"]) for a in un] == [("/api/restores/unspend", 3)]


def test_the_reviewer_reads_then_acts_once_per_item():
    review = {"declarations": [{"id": "D-1", "track": "T-0008", "actions": ["confirm", "revoke"]}],
              "open": [{"id": "F-1", "flag": 1, "tracks": ["T-0004"], "actions": ["confirm", "dismiss", "declare"]}],
              "lost_claims": []}

    def handler(method: str, path: str, body: Any, op: Any) -> tuple[int, Any]:
        if path == "/api/review":
            return 200, review
        if path == "/api/inbox":
            return 200, {"pending": [], "alerts": []}
        return 200, {}

    r = rv.Reviewer(FakeSim(handler), rv.review_policy({"flags": "dismiss", "read_s": 2.0, "attend_s": 5.0}), seed=1)
    wakes = drive(r, 10.0, 40.0)
    assert [(d["kind"], d["action"], d["t"], d["read_from_t"], d["by"]) for d in r.decisions] == [
        ("declaration", "confirm", 12.0, 10.0, "op-auto"), ("flag", "dismiss", 14.0, 12.0, "op-auto")]
    posts = [c for c in r.sim.calls if c[0] == "POST"]
    assert [c[1] for c in posts] == ["/api/tracks/T-0008/confirm", "/api/review/1/resolve"]
    assert wakes[:3] == [10.0, 12.0, 14.0] and all(b - a == 5.0 for a, b in zip(wakes[3:], wakes[4:]))
    idle = rv.Reviewer(FakeSim(handler), rv.review_policy({"preset": "absent"}), seed=1)
    assert idle.start(0.0) is None


# ---- the scripted agent's choices -------------------------------------------------------------

def test_the_scripted_agents_choices():
    tracks = [{"id": "T-0003", "affiliation": "hostile"}, {"id": "T-0001", "affiliation": "hostile"},
              {"id": "T-0002", "affiliation": "suspect"}]
    assert sc.hostile_tracks(tracks) == ["T-0001", "T-0003"]
    noms = [{"id": 3, "track": "T-0001", "state": "expired"}, {"id": 5, "params": {"track": "T-0001"}, "state": "pending_approval"}]
    assert sc.standing_on(noms, "T-0001")["id"] == 5
    assert sc.standing_on(noms[:1], "T-0001") is None
    assert sc.first_passing([{"uid": 2, "passed": False}, {"uid": 3, "passed": True}])["uid"] == 3
    assert sc.failing_rules([{"uid": 3, "callsign": "BLADE 1", "checks": [
        {"rule": "keep_out_tracks", "outcome": "fail"}, {"rule": "reach", "outcome": "pass"},
        {"rule": "target_declared", "outcome": "unverified"}]}]) == {"BLADE 1": ["keep_out_tracks", "target_declared"]}
    now = 1_781_431_200_000
    opts = [{"norad_id": 1, "collect_time": "2026-06-14T10:05:00Z"}, {"norad_id": 2, "collect_time": "2026-06-14T11:00:00Z"}]
    assert sc.choose_option(opts, now, 600.0)["norad_id"] == 2
    assert sc.choose_option(opts, now, 0.0)["norad_id"] == 1
    plan = sc.ScriptedPlan.from_spec({"engage": None, "cover": {"window_s": 300}})
    assert plan.engage is None and plan.as_dict()["cover"]["window_s"] == 300 and plan.collect is None
    with pytest.raises(ValueError):
        sc.ScriptedPlan.from_spec({"collect": {"when": 3}})
    assert sc._area_text({"kind": "circle", "center": [-122.0, 37.0], "radius_km": 1.0}).count(",") == 3


# ---- the pinned element sets ------------------------------------------------------------------

def test_the_golden_gate_snapshot_holds_its_provenance():
    assert space.verify("golden-gate") == []
    prov = space.provenance("golden-gate")
    assert prov["world"] == "golden-gate" and len(prov["id"]) == 64
    # every element-set file the catalog's queries read is pinned, each with where it came from
    queries = {f["query"] for f in prov["files"] if "query" in f}
    assert {"GROUP=starlink", "GROUP=gps-ops", "NAME=CAPELLA", "NAME=ICEYE"} <= queries
    assert all(m.get("fetched_at") for m in prov["element_sets"])


def test_a_changed_snapshot_file_is_caught(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    shutil.copytree(space.snapshot_dir("golden-gate"), tmp_path / "golden-gate")
    monkeypatch.setattr(space, "SNAPSHOTS", tmp_path)
    f = tmp_path / "golden-gate" / "cache" / "name-wgs.json.gz"
    data = gzip.decompress(f.read_bytes()).replace(b"WGS", b"XGS", 1)
    f.write_bytes(gzip.compress(data, mtime=0))
    problems = space.verify("golden-gate")
    assert any("name-wgs.json" in p for p in problems) and any(p.startswith("id ") for p in problems)
    with pytest.raises(ValueError):
        space.unpack("golden-gate", into=tmp_path / "unpacked")


def test_the_snapshot_unpacks_as_the_loaders_tree(tmp_path: Path):
    root = space.unpack("golden-gate", into=tmp_path)
    assert root == space.unpack("golden-gate", into=tmp_path), "written once per id"
    s = root / "sim" / "space"
    assert (s / "constellations.toml").is_file() and (s / "areas.toml").is_file()
    assert (s / "cache" / "group-starlink.json").is_file() and (s / "cache" / "group-starlink.meta.json").is_file()
    assert (root / "sim" / "platforms.toml").resolve() == (REPO / "sim" / "platforms.toml").resolve()


# ---- smoke episodes on the real binary --------------------------------------------------------

@pytest.fixture(scope="module")
def binary() -> Path:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-decide", "--bin", "murmur-replay"], cwd=REPO, check=True)
    from eval.headless.sim import BINARY

    return BINARY


def episode(situation: str, keep: Path, duration_s: float | None = None, seed: int = 1, **opts: Any) -> dict[str, Any]:
    from eval.headless.episode import Episode, Options
    from eval.headless.situations import Calibration, from_situation

    e = from_situation(situation)
    if duration_s is not None:
        e.duration_s = duration_s
    with tempfile.TemporaryDirectory(prefix="engage-space-") as td:
        return Episode(e, seed, Path(td), Options(calibration=Calibration.load(), keep=keep, **opts)).run()


def events(rec: dict[str, Any]) -> list[dict[str, Any]]:
    return [json.loads(ln) for ln in rec["audit_text"].splitlines() if ln.strip()]


def first(evs: list[dict[str, Any]], typ: str, **match: Any) -> dict[str, Any]:
    found = [e for e in evs if e["type"] == typ and all(e.get(k) == v for k, v in match.items())]
    assert found, f"no {typ} {match or ''} in the audit log"
    return found[0]


def fixes_by(o: Any, uid: str) -> list[int]:
    """Every fix time (`source_t`, ms) of vehicle `uid`'s sensors anywhere in an audit event."""
    if isinstance(o, dict):
        own = [t for s, t in (o.get("source_t") or {}).items() if s.startswith(uid + ":")] if isinstance(o.get("source_t"), dict) else []
        return own + [t for k, v in o.items() if k != "source_t" for t in fixes_by(v, uid)]
    if isinstance(o, list):
        return [t for v in o for t in fixes_by(v, uid)]
    return []


def same_episode(a: dict[str, Any], b: dict[str, Any]) -> None:
    """Two runs of one seed: the whole record (provenance and the audit log included; only
    `perf`, the computer's own timings, left out) is the same, the change feed's per-process id
    masked where an agent read the feed (the scripted agent never does: then there is none)."""
    from eval.headless.tests.test_episode import FEED_ID, only_the_feed_id_differs

    ta = json.dumps({k: v for k, v in a.items() if k != "perf"}, sort_keys=True)
    if FEED_ID.search(ta):
        only_the_feed_id_differs(a, b)
        return
    tb = json.dumps({k: v for k, v in b.items() if k != "perf"}, sort_keys=True)
    if ta != tb:
        i = next((i for i, (x, y) in enumerate(zip(ta, tb)) if x != y), min(len(ta), len(tb)))
        pytest.fail(f"two runs of one seed differ at {i}: {ta[max(0, i - 200):i + 200]!r} vs {tb[max(0, i - 200):i + 200]!r}")


def replays(keep: Path, *extra: str) -> str:
    log = next(keep.glob("headless-*.jsonl"))
    out = subprocess.run([str(REPLAY), str(log), "--anchors", str(log.with_suffix(".anchors")), *extra],
                         capture_output=True, text=True)
    text = out.stdout + out.stderr
    assert out.returncode == 0, text
    assert "every decision reproduced exactly" in text and "ANCHORS OK" in text, text
    return text


ENGAGE = {"agent": "scripted", "signers": {"preset": "scripted"}, "review": {"preset": "confirm-declarations"},
          "effects": {"p_hit": 1.0}}


def test_the_engagement_chain_runs_end_to_end(binary: Path, tmp_path: Path):
    """A hostile act, the rule declaration, an operator's confirmation, the scripted agent's
    nomination through the MCP tools, two signatures on one frozen plan, the dispatch and the
    platform's lost-link preload, the effect, the assessment by our sensors, and what was owed
    back closed (the munition is spent by its own effect); the log replays, every decision."""
    rec = episode("hostile-act-engagement", tmp_path / "a", **ENGAGE)
    evs = events(rec)
    act = first(evs, "ident.act")
    decl = first(evs, "ident.declared")
    assert decl["declaration"]["by"]["kind"] == "rule" and decl["declaration"]["by"]["evidence"]["act"] == act["act"]["act"]
    conf = first(evs, "ident.confirmed")
    assert conf["by"] == "op-auto" and conf["track"] == decl["declaration"]["track"]
    nom = next(e for e in evs if e["type"] == "intent.created" and e["intent"]["kind"] == "engage")
    iid = nom["intent"]["id"]
    assert nom["intent"]["source"] == {"kind": "agent", "id": "agent-scripted"}
    assert nom["intent"]["rationale"].startswith(sc.LABEL)
    granted = first(evs, "approval.granted", intent=iid)
    concurred = [e for e in evs if e["type"] == "approval.concurred" and e["intent"] == iid]
    assert {granted["by"], concurred[-1]["by"]} == {"op-2", "op-3"}
    first(evs, "intent.state", intent=iid, to="dispatched")
    assert {e["phase"] for e in evs if e["type"] == "engage.preload" and e["intent"] == iid} >= {"intent", "set", "verify"}
    effect = first(evs, "engage.effect", intent=iid)
    closed = first(evs, "restore.closed")
    assert closed["closed_by"] == {"kind": "expended", "intent": iid}
    assessed = first(evs, "engage.assessed", intent=iid)
    assert assessed["result"] == "ceased"
    order = [evs.index(x) for x in (act, decl, conf, nom, granted, effect, closed, assessed)]
    assert order == sorted(order)
    # the effect on the truth side: the drawn hit took the boat out, the munition left the world
    (fx,) = rec["effects"]["applied"]
    assert fx["hit"] and fx["platform_removed"] and fx["applied"]["out_of_play"] == ["fastboat-raider"]
    # a munition that left the world senses nothing from the next truth message on (not when
    # its link goes stale): no track holds a fix of it from more than a second after the effect
    later = [t for x in evs if x["t"] > effect["t"] for t in fixes_by(x, str(effect["uid"]))]
    assert later and max(later) <= effect["t"] + 1000, max(later)
    # the scripted operators' own records, and the two-person rule read from the log alone
    assert [d["by"] for d in rec["signers"]["decisions"] if d["action"] == "sign"][-2:] in (["op-2", "op-3"], ["op-3", "op-2"])
    assert [(d["kind"], d["result"]) for d in rec["review"]["decisions"]] == [("declaration", "done")]
    from eval.metrics import invariants, trails

    inv = invariants(evs, trails(evs), {"op-1", "op-2", "op-3", "op-auto", "op-setup", "op-planner"}, None)
    assert inv["ok"], inv["counts"]
    assert inv["engage_intents"] >= 1
    text = replays(tmp_path / "a")
    assert "1 effect(s)" in text and "1 assessment(s)" in text
    # the same seed again: the same episode
    same_episode(rec, episode("hostile-act-engagement", None, **ENGAGE))


def test_an_aborted_engagement_takes_its_fallback_and_restores(binary: Path, tmp_path: Path):
    rec = episode("hostile-act-engagement", tmp_path / "a",
                  **{**ENGAGE, "signers": {"preset": "abort"}, "scripted": {"engage": {"max_nominations": 1}}})
    evs = events(rec)
    stopped = first(evs, "engage.stopped")
    assert stopped["cause"]["kind"] == "operator" and stopped["fallback"] == "return"
    restore = first(evs, "engage.preload", intent=stopped["intent"], phase="restore")
    assert restore["confirmed"]
    assert not [e for e in evs if e["type"] == "engage.effect"] and rec["effects"]["applied"] == []
    replays(tmp_path / "a")


def test_a_miss_leaves_the_target_and_a_surviving_munition_is_unspent(binary: Path, tmp_path: Path):
    rec = episode("hostile-act-engagement", tmp_path / "a",
                  **{**ENGAGE, "effects": {"p_hit": 0.0, "survives_miss": True},
                     "review": {"preset": "confirm-declarations", "unspend": "unspend"},
                     "scripted": {"engage": {"max_nominations": 1}}})
    evs = events(rec)
    (fx,) = rec["effects"]["applied"]
    assert not fx["hit"] and not fx["platform_removed"] and fx["applied"]["out_of_play"] == []
    assert first(evs, "engage.assessed")["result"] == "persists"
    unspent = first(evs, "platform.unspent")
    assert unspent["operator"] == "op-2"
    assert first(evs, "platform.restore", uid=unspent["uid"])["confirmed"]
    assert ("unspend", "done") in [(d["kind"], d["result"]) for d in rec["review"]["decisions"]]
    replays(tmp_path / "a")


#: a trawler that goes dark near the holding area, and the same AIS seen again later: its track is
#: dropped (a lost contact every engagement's keep-out counts) and the new one claims it
LOST_TRAWLER = """

[[contact]]
id = "trawler-dark"
name = "Trawler that goes dark east of the holding area (AIS)"
affiliation = "neutral"
domain = "surface"
kind = "fishing_vessel"
motion = "station"
at = [-122.4820, 37.8160]
start_s = 0
end_s = 5
ais = { mmsi = "366999401", name = "F/V SEA WOLF", ship_type = "fishing" }

[[contact]]
id = "trawler-back"
name = "The same trawler seen again (its AIS)"
affiliation = "neutral"
domain = "surface"
kind = "fishing_vessel"
motion = "station"
at = [-122.4815, 37.8162]
start_s = 240
ais = { mmsi = "366999401", name = "F/V SEA WOLF", ship_type = "fishing" }
"""


def test_a_lost_contact_blocks_until_its_ais_claim_is_confirmed(binary: Path, tmp_path: Path):
    """Rules 16 headless: a trawler goes dark near the holding area; its track is dropped with its
    AIS and every keep-out counts it, so no platform passes the form; the same AIS seen again
    claims it, the reviewer (`accept-all`) confirms the claim through the review list's path,
    and only then does the agent nominate; the chain runs on to the effect, and the log replays."""
    from eval.headless.episode import Episode, Options
    from eval.headless.situations import Calibration, from_situation

    contacts = tmp_path / "contacts.toml"
    contacts.write_text((REPO / "sim" / "scenarios" / "hostile-act-engagement.toml").read_text() + LOST_TRAWLER)
    opts = Options(calibration=Calibration.load(), keep=tmp_path / "a",
                   **{**ENGAGE, "review": {"preset": "accept-all"}})
    with tempfile.TemporaryDirectory(prefix="engage-space-") as td:
        rec = Episode(from_situation("hostile-act-engagement"), 1, Path(td), opts, contacts_path=contacts).run()
    evs = events(rec)
    dropped = first(evs, "track.dropped")
    assert dropped["ais"]["mmsi"] == "366999401"
    claimed = first(evs, "lost.claimed", track=dropped["track"])
    back = first(evs, "track.reacquired", track=dropped["track"])
    assert back["by"] == "op-auto" and back["by_track"] == claimed["by_track"]
    assert ("ais_claim", "done") in [(d["kind"], d["result"]) for d in rec["review"]["decisions"]]
    t0 = evs[0]["t"]  # the bridge's clock at the situation's zero, ms
    confirmed_s = (first(evs, "ident.confirmed")["t"] - t0) / 1000.0
    back_s = (back["t"] - t0) / 1000.0
    # with the declaration confirmed and until the claim was, the lost contact blocked BLADE 1's
    # keep-out, as the form's explanation names it
    lost = f"lost_contact:{dropped['track']}"
    blocked = [ln for ln in rec["agent_log"] if ln["kind"] == "engage.none_passes" and confirmed_s < ln["t"] < back_s]
    assert blocked and all("keep_out_tracks" in ln["failing"]["BLADE 1"] and lost in ln["blocking"]["BLADE 1"]
                           for ln in blocked), rec["agent_log"]
    after = [ln for ln in rec["agent_log"] if ln["kind"] == "engage.none_passes" and ln["t"] > back_s]
    assert all(lost not in ln["blocking"]["BLADE 1"] for ln in after), "retired once the claim was confirmed"
    nominated = next(ln for ln in rec["agent_log"] if ln["kind"] == "engage.nominated")
    assert nominated["t"] > back_s
    first(evs, "engage.effect", intent=nominated["intent"])
    replays(tmp_path / "a")


def test_a_collect_is_tasked_collected_and_delivered(binary: Path, tmp_path: Path):
    """The pinned snapshot's segment loads on the simulated clock; the scripted agent reads the
    passes and the tasking options, proposes a collect with the first option, the operator
    approves it, and the bridge's simulated executor runs it through its pass and product; the
    log replays, and with the snapshot every space query is recomputed identically."""
    rec = episode("harbor-watch", tmp_path / "a", duration_s=9000.0, agent="scripted", space_snapshot="golden-gate",
                  scripted={"engage": None, "collect": {"at_s": 60}})
    evs = events(rec)
    loaded = first(evs, "space.loaded")
    assert loaded["t"] == evs[0]["t"], "loaded at the episode's start, on the simulated clock"
    log = {ln["kind"]: ln for ln in rec["agent_log"]}
    assert log["collect.passes"]["windows"] > 0
    iid = log["collect.proposed"]["intent"]
    assert log["collect.proposed"]["state"] == "pending_approval"
    first(evs, "intent.state", intent=iid, to="dispatched")
    states = [e["state"] for e in evs if e["type"] == "op.state" and e["intent"] == iid]
    assert states == ["tasked", "collected", "product_ready"]
    first(evs, "intent.state", intent=iid, to="done")
    assert rec["space"]["snapshot"]["id"] == space.provenance("golden-gate")["id"]
    root = space.unpack("golden-gate")
    text = replays(tmp_path / "a", "--space", str(root / "sim" / "space" / "constellations.toml"),
                   str(root / "sim" / "space" / "cache"))
    assert "recomputed" in text and "cache mismatch" not in text.lower()


def test_a_cover_watches_its_window(binary: Path, tmp_path: Path):
    rec = episode("harbor-watch", tmp_path / "a", agent="scripted", space_snapshot="golden-gate",
                  scripted={"engage": None, "cover": {"at_s": 60, "window_s": 600, "service": {"type": "gnss"}}})
    evs = events(rec)
    iid = next(ln for ln in rec["agent_log"] if ln["kind"] == "cover.proposed")["intent"]
    first(evs, "intent.state", intent=iid, to="auto_approved")
    states = [e["state"] for e in evs if e["type"] == "op.state" and e["intent"] == iid]
    assert states[0] == "watching" and states[-1] == "ended"
    first(evs, "intent.state", intent=iid, to="done")
    replays(tmp_path / "a")
    # the same seed again: the same episode
    same_episode(rec, episode("harbor-watch", None, agent="scripted", space_snapshot="golden-gate",
                              scripted={"engage": None, "cover": {"at_s": 60, "window_s": 600,
                                                                  "service": {"type": "gnss"}}}))


def test_without_the_options_an_episode_has_none_of_it(binary: Path):
    """Section 16 is opt-in: a v1 episode runs no signer, reviewer or space segment and its
    record has none of their keys (the golden holds the rest byte for byte)."""
    rec = episode("harbor-watch", None, duration_s=120.0)
    assert not {"signers", "review", "effects", "space"} & set(rec)
    assert not [e for e in events(rec) if e["type"] == "space.loaded"]
