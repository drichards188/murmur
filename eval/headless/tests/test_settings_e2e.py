"""The scenario's settings end to end (docs/SCENARIO-SETTINGS.md section 5; stage S7): one episode
of the real `murmur-headless` binary under decision rules 33 with a tailored scenario policy, and
every reader held to it: the card the agents are briefed with, the engine's identify orders and
engagement check, the record, the scoring, the report and the officer's predicate.

**The scenario.** F2 (an unknown drone inbound) at the Golden Gate, situation seed 5000, under a
supplement of the v3 watch's policy (sim/situations/policies/golden-gate-v3-watch.toml, written to
the test's directory as `TAILORED`): the bar 0.75 (and the engagement's with it, the floor), the
air deadline 45 s, the approach horizon 240 s, 200 s on station for an identify order, the reserve
25 % and stale 12 s, and one named loosening. The loosening is
`identification.nominate_unconfirmed`: the other loosenable key, the engagement bar, cannot be
lowered below what this file extends (0.6) while it stays at or above this file's own bar (0.75,
the floor), and the four-file chain (this file, the v3 watch, watch.toml, sim/policy.toml) is
already the most `extends` allows, so no middle layer can raise it first.

**The driver** (`Driver`, in the scripted agent's place, as `agent-scripted` over the MCP tools; the
approve-all operator): privileged as a test's driver is (it finds the drone's track by the
tracker's association with truth), it

- nominates REAPER 1 on the first track the picture shows identified at a confidence of 0.6 or more
  and under 0.75 (by EO/IR, without a conflict): one the standing bar would pass;
- sends an identify order, with the first unarmed vehicle that passes (`get_identify_options`), on
  the drone's track, and on up to four tracks shown in that window;
- reads each identify order's progress (`get_intents`' `identify_progress`) every second.

**What the run shows** (the assertions that need a particular event name it; a change to the sensing
or the tracker that moves them needs another seed): REAPER 1's nomination on a helicopter shown at
0.60 refused at the identity check; five identify orders, three ending identified at 0.99 or 1.00
and two out of time, HAWK 1's with no look of its own, PUMA 2's on the drone after it named it
small_drone at 0.72, under the bar, and went on, so the drone's need is never resolved, as the
standing bar would have resolved it at that 0.72.

The episode runs once (about 20 s of wall time) and skips, saying why, without the release binaries
(`make check` builds them before eval/headless/tests). The repository marks no Python test slow;
this one is an episode test like eval/headless/tests/test_worlds_v3.py's, which runs in the suite.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
import tomllib
from collections import defaultdict
from types import SimpleNamespace
from typing import Any

import pytest

from eval import REPO

HEADLESS = REPO / "target" / "release" / "murmur-headless"
POLICY_BIN = REPO / "target" / "release" / "murmur-policy"
pytestmark = pytest.mark.skipif(
    not (HEADLESS.exists() and POLICY_BIN.exists()),
    reason="needs the release binaries murmur-headless and murmur-policy "
           "(cargo build --release -p murmur-headless -p murmur-decide)")

FAMILY = "v3-f2-drone-gg"
SEED = 5000
WORLD = "golden-gate-v3"
DRONE = "drone-1"
RULES = 33
BAR = 0.75
STANDING_BAR = 0.6
#: the window a nomination and the extra identify orders are aimed in: identified by the standing
#: bar, not by this scenario's
WINDOW = (STANDING_BAR, BAR)
MAX_IDENTIFY = 5
LOOSENED_KEY = "identification.nominate_unconfirmed"
LOOSENED_WHY = "the test lets a nomination follow a rule declaration nobody confirmed"
TAILORED = f'''# A test supplement of the v3 Golden Gate watch (eval/headless/tests/test_settings_e2e.py)
extends = "{REPO / "sim" / "situations" / "policies" / "golden-gate-v3-watch.toml"}"
loosened = {{ "{LOOSENED_KEY}" = "{LOOSENED_WHY}" }}

[watch]
bar = 0.75
approach_horizon_s = 240
identify_on_station_s = 200
stale_s = 12

[watch.domain.air]
grace_s = 45

[rules]
reserve_pct = 25

[rules.engage]
min_identity_confidence = 0.75

[identification]
nominate_unconfirmed = true
'''
#: what the tailored file sets, as the logged policy holds it
TAILORED_WATCH = {"bar": 0.75, "approach_horizon_s": 240.0, "identify_on_station_s": 200.0, "stale_s": 12.0}


def armed_platforms() -> set[str]:
    cat = tomllib.loads((REPO / "sim" / "platforms.toml").read_text())
    return {k for k, v in cat.items() if isinstance(v, dict) and v.get("armed")}


def in_window(t: dict[str, Any]) -> bool:
    """A track shown identified (its kind) by EO/IR without a conflict, at a confidence the standing
    bar passes and this scenario's does not."""
    idn = t.get("identity") or {}
    c = idn.get("confidence")
    return (idn.get("level") == "identify" and c is not None and WINDOW[0] <= c < WINDOW[1]
            and "eo_ir" in (idn.get("supported_by") or []) and not idn.get("conflict")
            and t.get("domain") in ("surface", "air"))


class Driver:
    """The test's driver as the episode's `agent` process (module doc)."""

    def __init__(self, ep: Any, plan: Any):
        from eval.situation import STORYLINE_AGENT

        from eval.headless.sim import HeadlessSession

        self.ep = ep
        self.session = HeadlessSession(ep.sim, STORYLINE_AGENT, "0.1")
        self.lines: list[dict[str, Any]] = []
        self.armed = armed_platforms()
        self.policy: dict[str, Any] | None = None
        self.nomination: dict[str, Any] | None = None
        self.sent: dict[str, dict[str, Any]] = {}
        self.tried: set[str] = set()
        self.busy: set[int] = set()
        self.progress: dict[int, list[dict[str, Any]]] = defaultdict(list)

    def describe(self) -> dict[str, Any]:
        return {"sent": self.sent, "nomination": self.nomination}

    def write(self, kind: str, **fields: Any) -> None:
        self.lines.append({"t": round(self.ep.sim.t, 3), "source": "scripted", "kind": kind, **fields})

    def call(self, tool: str, **args: Any) -> Any:
        return self.ep.run_async(self.session.call(tool, **args))

    def propose(self, rationale: str, **req: Any) -> dict[str, Any]:
        return self.ep.run_async(self.session.propose(f"test driver: {rationale}", **req))

    def nominate(self, t: dict[str, Any]) -> None:
        from eval.headless.scripted import policy_defaults

        fleet = self.call("get_fleet").get("vehicles") or []
        reaper = next(v for v in fleet if v.get("callsign") == "REAPER 1")
        # the effect radius and window the run's policy gives a nomination that names none
        out = self.propose("a nomination at an identity the standing bar passes", task="engage",
                           uids=[int(reaper["uid"])], track_id=t["id"], fallback="return",
                           **policy_defaults(self.policy or {}))
        self.nomination = {"track": t["id"], "t": round(self.ep.sim.t, 3), "identity": t.get("identity"),
                           "intent": out.get("id"), "state": out.get("state")}
        self.write("nominated", **self.nomination)

    def identify(self, tid: str) -> None:
        self.tried.add(tid)
        try:
            ans = self.call("get_identify_options", track_id=tid)
        except RuntimeError as e:
            self.write("identify.options_error", track=tid, error=str(e)[:300])
            return
        ok = [o for o in ans.get("vehicles") or [] if o.get("passed") and int(o["uid"]) not in self.busy
              and o.get("platform") not in self.armed]
        if not ok:
            self.write("identify.none_passes", track=tid)
            return
        pick = ok[0]
        out = self.propose(f"identify {tid}", task="identify", uids=[int(pick["uid"])], track_id=tid)
        self.sent[tid] = {"intent": out.get("id"), "uid": int(pick["uid"]), "callsign": pick.get("callsign"),
                          "t": round(self.ep.sim.t, 3)}
        self.busy.add(int(pick["uid"]))
        self.write("identify.proposed", track=tid, **self.sent[tid])

    def wake(self, now: float) -> float:
        if self.policy is None:
            self.policy = self.call("get_policy")
        tracks = {t["id"]: t for t in self.call("get_tracks").get("tracks") or []}
        if self.nomination is None:
            t = next((t for t in tracks.values() if in_window(t)), None)
            if t is not None:
                self.nominate(t)
                return now + 1.0
        # one dry run a wake (the tools allow one a second): the drone first, then the window's tracks
        drone = [tid for tid, cid in sorted(self.ep.association.items()) if cid == DRONE and tid in tracks]
        for tid in drone:
            if tid not in self.sent:
                self.identify(tid)
                return now + 1.0
        if len(self.sent) < MAX_IDENTIFY:
            t = next((t for t in tracks.values() if t["id"] not in self.tried and in_window(t)), None)
            if t is not None:
                self.identify(t["id"])
                return now + 1.0
        mine = {s["intent"] for s in self.sent.values()}
        for i in self.call("get_intents").get("intents") or []:
            p = i.get("identify_progress")
            if i.get("id") in mine and p and (not self.progress[i["id"]] or self.progress[i["id"]][-1]["p"] != p):
                self.progress[i["id"]].append({"t": round(now, 3), "state": i.get("state"), "p": p})
        return now + 1.0


@pytest.fixture(scope="module")
def run(tmp_path_factory: pytest.TempPathFactory) -> SimpleNamespace:
    from eval.headless import scripted
    from eval.headless.episode import Episode, Options
    from eval.headless.situations import Calibration
    from eval.headless.v3 import families as F
    from eval.headless.v3.record import PictureEncoderV3

    d = tmp_path_factory.mktemp("settings-e2e")
    policy = d / "tailored.toml"
    policy.write_text(TAILORED)
    #: each picture sample's identity level per track as the bridge served it, beside the record (which
    #: keeps it too, a run of rules 33: `test_the_record_keeps_each_sample_s_identity_level`)
    levels: dict[tuple[int, str], str | None] = {}
    add = PictureEncoderV3.add

    def add_keeping_levels(self: Any, p: dict[str, Any]) -> None:
        k = len(self.times)
        for tr in p.get("tracks") or []:
            levels[(k, str(tr["id"]))] = (tr.get("identity") or {}).get("level")
        add(self, p)

    from_situation = F.from_situation

    def tailored(name: str) -> Any:
        e = from_situation(name)
        e.policy = policy
        return e

    with pytest.MonkeyPatch.context() as mp:
        mp.setenv("MURMUR_RULES", str(RULES))
        mp.setattr(F, "from_situation", tailored)
        mp.setattr(scripted, "ScriptedAgent", Driver)
        mp.setattr(PictureEncoderV3, "add", add_keeping_levels)
        spec, contacts = F.generate(FAMILY, SEED, d / "situation")
        spec.seed = SEED
        opts = Options(agent="scripted", operator={"mode": "approve-all"}, calibration=Calibration.load(),
                       scoring="v3", replicate={"r": 0, "seed": SEED, "situation_seed": SEED}, scripted={"engage": False})
        ep = Episode(spec, SEED, d / "episode", opts, contacts_path=contacts)
        rec = ep.run()
    events = [json.loads(ln) for ln in rec["audit_text"].splitlines() if ln.strip()]
    started = [e for e in events if e.get("type") == "run.started"]
    assert len(started) == 1, "one run in the episode's log"
    return SimpleNamespace(rec=rec, spec=spec, policy=policy, driver=ep.agent, levels=levels, events=events,
                           started=started[0], logged=started[0]["policy"], dir=d)


# ---- the card -----------------------------------------------------------------------------------


def test_the_card_states_every_tailored_value_and_hashes_as_the_record_says(run: SimpleNamespace):
    from eval.policy_file import show
    from eval.roe_card import render
    from eval.headless.v3 import settings as S

    card = run.spec.brief
    for want in ("identity confidence of 0.75 or more", "its own look at 0.75 or more",
                 "in the air, within 45 seconds of its coming inside",
                 "within 240 seconds", "its 200 seconds on station are over",
                 "Keep every vehicle above its reserve: 25 % of its endurance left after a task of 900 seconds",
                 "at its kind (the identification level) at 0.75 or more",
                 f"Loosened by this scenario: {LOOSENED_KEY}, because {LOOSENED_WHY}."):
        assert want in card, want
    # nothing of the standing values the file changed is left on it
    assert not re.search(r"\b0\.6\b", card) and "within 30 seconds" not in card and "20 %" not in card
    stated = run.rec["truth"]["stated"]
    assert stated["rules_version"] == RULES
    assert stated["card_sha256"] == hashlib.sha256(card.encode()).hexdigest()
    # the settings the card was rendered from are the ones the run logged: the stated sha256 is the
    # logged policy's and the record's, and the card rendered from the logged policy is the brief
    logged = S.from_policy(run.logged)
    assert stated["settings_sha256"] == logged.sha256() == S.of_record(run.rec).sha256()
    assert render(logged, RULES, WORLD, run.logged) == card
    # and the decision layer's own merge of the file is what the bridge logged
    assert show(run.policy, RULES)["policy"] == run.logged


# ---- the record ---------------------------------------------------------------------------------


def test_the_record_keeps_the_logged_settings(run: SimpleNamespace):
    kept, logged = run.rec["settings"], run.logged
    assert run.started["rules_version"] == RULES
    assert kept == {
        "rules_version": RULES, "watch": logged["watch"], "reserve_pct": logged["rules"]["reserve_pct"],
        "task_duration_s": logged["rules"]["task_duration_s"],
        "engage": {k: logged["rules"]["engage"][k] for k in ("min_identity_confidence", "min_identity_level")},
        "loosened": logged["loosened"], "policy_layers": run.started["policy_layers"]}
    # and the logged values are the tailored ones
    w = kept["watch"]
    assert {k: w[k] for k in TAILORED_WATCH} == TAILORED_WATCH
    assert w["domain"]["air"]["grace_s"] == 45.0 and w["domain"]["surface"]["grace_s"] == 60.0
    assert (kept["reserve_pct"], kept["engage"]["min_identity_confidence"]) == (25, 0.75)
    assert kept["loosened"] == {LOOSENED_KEY: LOOSENED_WHY}
    assert kept["policy_layers"][0]["sha256"] == hashlib.sha256(run.policy.read_bytes()).hexdigest()
    assert len(kept["policy_layers"]) == 4


def test_the_record_keeps_each_sample_s_identity_level(run: SimpleNamespace):
    """A run that logged its settings keeps each picture sample's identity level as the bridge served
    it (eval/headless/v3/record.py; bd murmur-u1q4.8), every level the scoring may meet among them."""
    from eval.headless.v3 import labels as L
    from eval.headless.v3.record import shown_level

    pic = L.picture(run.rec)
    assert run.levels and all("level" in T for T in pic.tracks.values())
    for (k, tid), level in run.levels.items():
        assert shown_level(pic, tid, k) == (True, level), (k, tid)
    assert {"recognise", "identify"} <= set(run.levels.values())


# ---- the engine: identify orders and the engagement check ---------------------------------------


IDENTIFIED = re.compile(r"identified as (\S+) at (\d\.\d\d) by .+?'s (\S+) \(the bar is 0\.75, ")
NAMED_BAR = re.compile(r"named it (\S+) at (\d\.\d\d), not yet identified \(the bar is 0\.75\); it goes on$")
NAMED_OTHER = re.compile(r"named it (\S+) at (\d\.\d\d)(, an? \w+)?( by its \S+)?, not yet identified "
                         r"\((the criterion is .+|its identity is in conflict: .+)\); it goes on$")
BEST_LOOK = re.compile(r"best look named it (\S+) at (\d\.\d\d)( by its \S+)?(, .+?)? \(the bar is 0\.75, ")


def identify_events(r: SimpleNamespace) -> dict[int, dict[str, Any]]:
    """Each identify order the driver sent: its dispatch and its steps, by intent."""
    mine = {s["intent"] for s in r.driver.sent.values()}
    out: dict[int, dict[str, Any]] = {i: {"dispatched": None, "steps": []} for i in mine}
    for e in r.events:
        if not isinstance(e.get("intent"), int) or e["intent"] not in mine:
            continue
        if e.get("type") == "intent.dispatched":
            out[e["intent"]]["dispatched"] = e
        elif e.get("type") == "identify.step":
            out[e["intent"]]["steps"].append(e)
    return out


def test_identify_orders_run_to_the_bar(run: SimpleNamespace):
    orders = identify_events(run)
    drone = {tid for tid, cid in run.rec["truth"]["association"].items() if cid == DRONE}
    assert drone & set(run.driver.sent) and len(orders) >= 2, run.driver.lines
    ends: dict[str, int] = defaultdict(int)
    bar_named = 0
    for intent, o in sorted(orders.items()):
        disp = o["dispatched"]
        assert disp is not None, f"identify {intent} never dispatched"
        # its deadline: the transit, then the scenario's 200 s on station
        ident = disp["identify"]
        assert ident["deadline_t"] == disp["t"] + round((ident["transit_s"] + 200.0) * 1000.0)
        ended = [s for s in o["steps"] if s["step"] == "ended"]
        assert len(ended) == 1, f"identify {intent}: {len(ended)} ends"
        end = ended[0]
        before = [s for s in o["steps"] if s["seq"] < end["seq"]]
        named = [s for s in before if s["step"] == "named"]
        ends[end["outcome"]] += 1
        for s in named:
            m = NAMED_BAR.search(s["detail"])
            if m:
                bar_named += 1
                assert float(m.group(2)) < BAR, s["detail"]
            else:
                assert NAMED_OTHER.search(s["detail"]), s["detail"]
        if end["outcome"] == "identified":
            m = IDENTIFIED.search(end["detail"])
            assert m, end["detail"]
            assert float(m.group(2)) >= BAR, end["detail"]
            assert m.group(3) == "eo_ir" and end["level"] == "identify"
        elif end["outcome"] == "timed_out":
            assert "(the bar is 0.75, identification by EO/IR)" in end["detail"], end["detail"]
            assert end["t"] >= ident["deadline_t"]
            m = BEST_LOOK.search(end["detail"])
            if m and float(m.group(2)) >= BAR:
                assert m.group(4), f"a best look at the bar that did not end it says why: {end['detail']}"
        else:
            # a lost track: the order ends without an identification to state
            assert end["outcome"] == "lost" and "identified as" not in end["detail"], end["detail"]
        # whenever its progress showed a look of its own short of the criteria, a `named` step came
        # before the end
        short = [p for p in run.driver.progress.get(intent, []) if p["p"].get("look") and not p["p"]["identified"]]
        if short:
            assert named, f"identify {intent}: progress showed {short[0]['p']['words']!r} but no named step"
        for p in run.driver.progress.get(intent, []):
            assert p["p"]["bar"] == BAR and "the bar is 0.75" in p["p"]["words"], p
    assert ends["identified"] >= 1, dict(ends)
    assert bar_named >= 1, "no identify order was named under the bar and went on"


def test_a_nomination_under_the_bar_is_refused_at_the_identity_check(run: SimpleNamespace):
    nom = run.driver.nomination
    assert nom is not None, "no track was shown in the window"
    conf = nom["identity"]["confidence"]
    assert WINDOW[0] <= conf < WINDOW[1] and nom["identity"]["level"] == "identify"
    checks = next(e["results"] for e in run.events if e.get("type") == "check.result" and e.get("intent") == nom["intent"])
    ident = next(c for c in checks if c["rule"] == "target_identity")
    assert ident["outcome"] == "fail"
    m = re.fullmatch(rf"{nom['track']} identity \S+ at (\d\.\d\d) is below the bar, 0\.75: an engagement needs "
                     r"identification at 0\.75 or more", ident["detail"])
    assert m and float(m.group(1)) == pytest.approx(conf, abs=0.006), ident["detail"]
    states = [e for e in run.events if e.get("type") == "intent.state" and e.get("intent") == nom["intent"]]
    assert states[-1]["to"] == "rejected" and "target_identity: " in states[-1]["reason"]


# ---- the scoring --------------------------------------------------------------------------------


def test_the_needs_carry_the_air_deadline_and_resolve_only_at_the_bar(run: SimpleNamespace):
    from dataclasses import replace

    from eval.headless.labels import Truth
    from eval.headless.v3 import labels as L
    from eval.headless.v3 import settings as S

    rec = run.rec
    st = S.of_record(rec)
    assert st.source == "logged" and st.bar == BAR and st.rule("air").grace_s == 45.0
    lab = L.episode_labels(rec)
    (need,) = [n for n in lab["needs"] if n["contact"] == DRONE]
    assert need["grace_s"] == 45.0 and need["deadline"] - need["t_need"] == pytest.approx(45.0, abs=1e-3)
    assert need["resolved_by"] == {"name": "v3.identity_resolved", "version": "4"}, "the record keeps the levels"
    pic = L.picture(rec)
    truth = L.mark_spoofed(rec, Truth(rec))
    meta = truth.meta[DRONE]
    # every contact, every sample: resolved only at the bar; and the bar bites: some sample the
    # standing bar resolves, this one does not
    standing = replace(st, bar=STANDING_BAR)
    bites = 0
    for cid, m in truth.meta.items():
        for k in range(len(pic.times)):
            tid = pic.track_of(cid, k)
            if tid is None:
                continue
            at = L.resolved_at(pic, k, cid, m, st)
            if at:
                assert pic.identity(tid, k)["confidence"] >= BAR
            elif L.resolved_at(pic, k, cid, m, standing):
                bites += 1
    assert bites > 0
    # the drone itself: PUMA 2 named it small_drone at 0.72 and no camera did better, so its need is
    # never resolved under this scenario's bar; the same record relabelled under the standing bar
    # resolves it at that 0.72
    assert need["t_identified"] is None and not need["served"]
    rec6 = copy.deepcopy({k: v for k, v in rec.items() if k != "audit_text"})
    rec6["settings"]["watch"]["bar"] = STANDING_BAR
    (need6,) = [n for n in L.episode_labels(rec6)["needs"] if n["contact"] == DRONE]
    assert need6["t_identified"] is not None
    k = pic.index(need6["t_identified"])
    assert STANDING_BAR <= pic.identity(pic.track_of(DRONE, k), k)["confidence"] < BAR
    assert not L.resolved_at(pic, k, DRONE, meta, st)


def test_the_report_names_the_settings(run: SimpleNamespace):
    from eval.headless.v2.record import dump
    from eval.headless.v3 import settings as S
    from eval.headless.v3.report import write

    d = run.dir / "batch"
    d.mkdir()
    rec = {k: v for k, v in run.rec.items() if k != "audit_text"}
    name = f"{FAMILY}-scripted-s{SEED}-r0"
    path = d / f"{name}.json.gz"
    dump(rec, path)
    (d / "episodes_v3.json").write_text(json.dumps([{
        "name": name, "path": str(path), "family": FAMILY, "agent": "scripted", "situation_seed": SEED, "r": 0,
        "seed": SEED, "class": rec.get("episode_class")}]))
    j, md = write(d, workers=1)
    st = S.of_record(rec)
    line = next(ln for ln in md.read_text().splitlines() if ln.startswith(f"- Settings in **{WORLD}**"))
    for want in (f"sha256 {st.sha256()[:12]}", "logged by the run", "bar 0.75", "air identify by eo_ir in 45 s",
                 "surface identify by eo_ir/ais in 60 s", "horizon 240 s", "reserve 25 % left after a 900 s task",
                 f"loosened: {LOOSENED_KEY} ({LOOSENED_WHY})",
                 # the record keeps the levels shown: its needs resolved by the instrument's version 4
                 "(v3.identity_resolved version 4) in 1 of them"):
        assert want in line, (want, line)
    # the line names what the scoring reads; the stale age and the time on station (the bridge's and
    # the engine's, not the scoring's) are in the set its sha256 names
    (cov,) = json.loads(j.read_text())["summary"]["coverage"]["settings"][WORLD].values()
    assert cov["values"]["watch"]["stale_s"] == 12.0 and cov["values"]["watch"]["identify_on_station_s"] == 200.0
    assert cov["values"]["watch"]["bar"] == BAR and cov["values"]["reserve_pct"] == 25.0
    assert cov["levels_kept"] == 1


# ---- the officer and the scoring ----------------------------------------------------------------


def family_and_kind_recognised(level: str | None, kind: str | None, rule_level: str) -> bool:
    """A label that names a family as well as a kind (`person`: the family of a diver and of a person,
    crates/murmur-bridge tracks/sensing.rs `family`), shown at the recognition level, for a domain
    held to identification: the kind's identification it is not. The scoring read the level from the
    label until bd murmur-u1q4.8 and counted it (one walker ashore, one sample in this run); the record
    keeps each sample's level now and the scoring reads it, as the officer (agents/roe.py) and the
    engine's identify criteria do."""
    from eval.headless.v3 import labels as L

    return (level == "recognise" and rule_level == "identify" and kind is not None
            and kind in L.FAMILY and kind in L.FAMILY.values())


def test_the_officer_and_the_scoring_agree_on_every_shown_sample(run: SimpleNamespace):
    """agents/roe.py's `identified`, on each picture sample's track as the record shows it (its level
    from the same sample, as the record keeps it), with the truth side the officer cannot see (the
    shown kind is the true one), is the scoring's `resolved_at`, contact by contact and sample by
    sample, a family label that is a kind too, shown at recognition, included
    (`family_and_kind_recognised`: neither counts it)."""
    from roe import Roe

    from eval.headless.labels import Truth
    from eval.headless.v3 import labels as L
    from eval.headless.v3 import settings as S
    from eval.headless.v3.record import shown_level

    rec = run.rec
    st = S.of_record(rec)
    officer = Roe.of_policy(run.driver.policy)
    assert officer.watch == st.watch(), "the officer reads the settings the scoring grades by"
    assert (officer.bar, officer.horizon_s, officer.stale_s) == (BAR, 240.0, 12.0)
    pic = L.picture(rec)
    truth = L.mark_spoofed(rec, Truth(rec))
    agree = under_bar = family = 0
    differ = []
    for cid, m in sorted(truth.meta.items()):
        for k in range(len(pic.times)):
            tid = pic.track_of(cid, k)
            idn = pic.identity(tid, k) if tid is not None else None
            if idn is None:
                continue
            kept, level = shown_level(pic, tid, k)
            assert kept, (k, tid)
            track = {"id": tid, "domain": pic.tracks[tid]["domain"], "identity": {**idn, "level": level}}
            says = officer.identified(track) and L.shown_ok(idn["kind"], m, st)
            scored = L.resolved_at(pic, k, cid, m, st)
            if says != scored:
                differ.append((pic.time(k), cid, tid, track["domain"], m.get("domain"), level, idn, says, scored))
                continue
            agree += 1
            c = idn["confidence"]
            under_bar += int(not says and level == "identify" and L.shown_ok(idn["kind"], m, st)
                             and c is not None and WINDOW[0] <= c < WINDOW[1])
            family += int(family_and_kind_recognised(level, idn["kind"], st.rule(m.get("domain")).level)
                          and L.shown_ok(idn["kind"], m, st) and c is not None and c >= BAR
                          and L.supported(idn, m, st) and not idn["conflict"])
    assert not differ, differ[:10]
    # the agreement covers the bar: samples showing the true kind by the standing bar, not by 0.75,
    # that neither counts
    assert agree > 0 and under_bar > 0
    # and the walker: its true kind shown at the bar by its domain's sensor, at recognition, which
    # neither counts (the scoring did before the record kept the level)
    assert family >= 1
