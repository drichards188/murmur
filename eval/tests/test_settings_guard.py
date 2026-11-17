"""The guard on the scenario's settings (docs/SCENARIO-SETTINGS.md sections 3 and 5; stage S6): one
source per scenario, its policy file's `[watch]` (and the reserve and the engagement's identity in its
`[rules]`), read through the single readers, eval/headless/v3/settings.py for the scoring and
agents/roe.py for the agents. It fails on:

- **a copy in code**: an assignment, anywhere under agents/ or eval/, of a name the inventory gave a
  copied value (`NAMES`, `PATTERNS`: the bar, the graces, the speeds, the horizons, the reserve, the
  circle's radius, the levels and sensors, the evidence bar, the look distance) outside the files
  allowed to hold one (`ALLOWED`: the two readers, and the frozen modules, each pinned by its sha256
  in `PINNED`);
- **a copy in a file**: a settings key in a TOML file under agents/, eval/ or sim/ outside the
  standing policy (sim/policy.toml), the frozen v0 file and the four whole policy fixtures that write
  the standing values out (`WHOLE_POLICIES`, pinned) or a supplement (`extends`: the decision layer
  refuses a key it sets equal to what it inherits); the playbook's own copies (`PLAYBOOK_KEYS`, read
  from the watch since stage S5 and S7) nowhere, its defaults.toml included (stage S7 took them out);
- **a situation repeating its policy's circle**: a `[playbook]` watch area or protected point at the
  centre its policy's `[watch.area]` already gives (the playbook takes the area from the policy);
- **an inventory site that stops reading the settings** (`SITES`): each reader that held a copy before
  S6 names, in its own body, what it reads now.

And a **perturbation check**: every Python reader run on settings with every value changed (the bar
0.77, the graces 1.5 times, the horizon 233 s, stale 13 s, the reserve 27 %, the look 700 m) follows.
"""

from __future__ import annotations

import ast
import hashlib
import math
import re
import sys
import tomllib
from dataclasses import replace
from pathlib import Path
from typing import Any

import pytest

from eval import REPO

if str(REPO / "agents") not in sys.path:
    sys.path.insert(0, str(REPO / "agents"))

#: the names an assignment of which is a copy (the inventory, section 1 and stage S6's list)
NAMES = {"RESOLVE_CONF", "P60", "THREAT_MPS", "STILL_MPS", "APPROACH_HORIZON_S", "JUSTIFY_HORIZON_S", "HORIZON_S",
         "RESERVE_PCT", "LOW_RESERVE_OPEN_PCT", "NEED_RADIUS_M", "RESOLVE_LEVEL", "RESOLVE_SENSORS", "EVIDENCE_CONF",
         "LOOK_M"}
PATTERNS = (re.compile(r".*_GRACE_S$"),)
#: the files allowed to assign them: the single readers, and the frozen modules (pinned below)
READERS = ("agents/roe.py", "eval/headless/v3/settings.py")
#: frozen, each by its sha256: scoring v2's modules (SCORING-V3.md: v2 is frozen) and scoring v1's
#: labels (they read the v1 world's own `[mission]`, which golden-gate.toml keeps, and for a v3
#: world, which has none since stage S6, the frozen v0 circle with v1's grace: `mission_of`)
PINNED = {
    "eval/headless/v2/__init__.py": "ffbb7b7348a971a585515d26a0f8d9d7e5654ca26b74d7bcc622a21e9599eb11",
    "eval/headless/v2/aggregate.py": "c930d3dba82330c86c25eba612915d50fbed59cff36f8e041ebc1cddc15a72a9",
    "eval/headless/v2/blind.py": "c3320ebfe065a89d36af2d30a4b1a6a841b75a6f41e83ab521804a0278ba06d7",
    "eval/headless/v2/judge.py": "5563d02e467a97a96a1fa0ece7ee8f2c946e1f94d0a2a2116cb00aa07170d651",
    "eval/headless/v2/labels.py": "782a0688c637e044291e20229a7bb8088a3f3b11145b3227c00bb94e923048fe",
    "eval/headless/v2/leakage.py": "1bff6f4a3c93b4916d907a9ac12c40d9423a9c1aabf835186cc43b365067d9d8",
    "eval/headless/v2/metrics.py": "9860eac3bc6c37de6b46b6fe7cc6ed50ae2fe58f7b6e4ee4b967f781d0d17e6b",
    "eval/headless/v2/mission.py": "0cb761872072871c10f49e9ee3c00bca3d5da175b96e44028c3c9a3cc328cd14",
    "eval/headless/v2/record.py": "dd6412535d85790a5428f23c408aabd799f3e3cd3e8d0f527c5885fff3f8e5a0",
    "eval/headless/v2/scorecard.py": "a0066311aa4ebf6b04ca83fadfc8189d6a621d112e3e16a0354d165070e998d6",
    "eval/headless/v2/stats.py": "60ad48a34346a993a32d1a640098ef8980844428a3c4062c2e849019e4026045",
    "eval/headless/labels.py": "45f5606a078c8c6a4fcc9f0e8ecd0923bf26f5d74aa8600505d78a7c0a8f8745",
}
ALLOWED = set(READERS) | set(PINNED)
#: the policy files that write the standing values out whole (section 7: no supplement merges to
#: them), pinned; the frozen v0 file is pinned by settings.py itself
WHOLE_POLICIES = {
    "sim/scenarios/engage/policies/no-engagement-tier.toml": "6cd9682129977f2f607b52f3500284ffb75188fc0dce15b470893f511620b0cb",
    "sim/scenarios/engage/policies/orbit-never-queued.toml": "127587e49e7ca9e871d91a0699f2df08a9e7c820c060ecda459999e20b31aff3",
    "sim/scenarios/engage/policies/nominate-unconfirmed.toml": "eb0a410348f7776246bd9fe8bedf2922dbc23eef87072df7137cf23b34a8dc9b",
    "eval/policy/base.toml": "fc2d1ecb6b7c2618ee06dab681ad85831d89408a04104b7d0f05b0338346a326",
}
STANDING = ("sim/policy.toml", "sim/policy-v0-watch.toml")
#: a policy's settings keys outside `[watch]` (dotted paths)
RULES_KEYS = ("rules.reserve_pct", "rules.task_duration_s", "rules.engage.min_identity_confidence",
              "rules.engage.min_identity_level", "rules.engage.default_effect_radius_m", "rules.silent.drift_mps")
#: the playbook's copies of the watch's values (agents/playbook/config.py `FROM_WATCH` and `RETIRED`:
#: its confidence-only "settled" until stage S7): in no file, agents/playbook/defaults.toml included
PLAYBOOK_KEYS = ("classifier_fresh_s", "tcpa_max_s", "classification_settled")

#: what each reader that held a copy before stage S6 reads now: (file, function or class.method, the
#: text its body must hold)
SITES = [
    ("eval/headless/v3/labels.py", "resolved_at", ("s.bar", "shown_ok(", "supported(", "level_reached(")),
    ("eval/headless/v3/labels.py", "level_reached", ("level_reaches(",)),
    ("eval/headless/v3/labels.py", "shown_ok", ("s.rule(",)),
    ("eval/headless/v3/labels.py", "supported", ("s.rule(",)),
    ("eval/headless/v3/labels.py", "in_doubt", ("s.bar",)),
    ("eval/headless/v3/labels.py", "approaches", ("s.no_velocity(", "s.still_mps", "p.justify_horizon_s")),
    ("eval/headless/v3/labels.py", "contact_needs", ("circle(s)", "s.rule(")),
    ("eval/headless/v3/labels.py", "sensing_errors", ("settings.rule(",)),
    ("eval/headless/v3/labels.py", "need_sensors", ("s.rule(",)),
    ("eval/headless/v3/labels.py", "harmful", ("s.reserve_now()",)),
    ("eval/headless/v3/labels.py", "label_actions", ("circle(s)",)),
    ("eval/headless/v3/labels.py", "effective", ("s.approach_horizon_s", "s.look_m")),
    ("eval/headless/v3/labels.py", "episode_labels", ("of_record(rec)", "effective(")),
    ("eval/headless/v3/faults.py", "fault_needs", ("s.reserve_now()", "s.task_duration_s", "s.v0.low_reserve_open_pct")),
    ("eval/headless/v3/metrics.py", "episode_metrics", ("of_record(rec)", "st.rule(dom).grace_s")),
    ("eval/headless/v3/report.py", "_score", ("of_record(rec)",)),
    ("eval/headless/v3/report.py", "capable", ("st.rule(",)),
    ("eval/headless/v3/causes.py", "cause_of", ("of_record(rec)",)),
    ("eval/headless/v3/able.py", "able", ("of_record(rec)", "st.rule(")),
    ("eval/headless/v3/mission.py", "mission_text", ("v0(world)",)),
    ("eval/headless/v3/families.py", "briefing", ("for_run(", "render(")),
    ("eval/headless/v3/families.py", "Draft.center", ("self.settings.area",)),
    ("eval/headless/v3/families.py", "Draft.radius", ("self.settings.area",)),
    ("eval/headless/v3/families_f7_f13.py", "_f13", ("d.settings.rule(",)),
    ("eval/headless/v3/oracle.py", "Need.__init__", ("st.rule(",)),
    ("eval/headless/v3/oracle.py", "OracleV3.read", ("self.settings.bar", "self.settings.shown_level_ok(")),
    ("eval/headless/v3/oracle.py", "OracleV3.settings", ("of_answer(",)),
    ("eval/headless/v3/oracle_ops.py", "OracleOps.radii", ("policy_defaults(",)),
    ("eval/headless/v3/oracle_ops.py", "OracleOps.engage", ("keep_out_sigma", "late_s")),
    ("eval/headless/v3/planner.py", "Ceiling.st", ("of_answer(",)),
    ("eval/headless/v3/planner.py", "Ceiling.reserve_pct", ("reserve_now()",)),
    ("eval/headless/v3/planner.py", "Ceiling.read_needs", ("self.st.bar", "self.st.shown_level_ok(")),
    ("eval/headless/v3/planner.py", "Quality.reach", ("self.bar",)),
    ("eval/headless/v3/planner.py", "eo_look", ("q.bar",)),
    ("eval/headless/v3/planner.py", "p_resolved", ("bar",)),
    ("eval/headless/v3/planner.py", "domain_sensor", ("st.rule(",)),
    ("eval/headless/v3/planner.py", "at_level", ("st.rule(", "st.shown_level_ok(")),
    ("eval/headless/v3/planner.py", "HonestCeiling.horizon_s", ("approach_horizon_s",)),
    ("eval/headless/v3/planner.py", "HonestCeiling.motion", ("self.st.no_velocity(", "self.st.still_mps")),
    ("eval/headless/v3/planner.py", "HonestCeiling.picture", ("self.st.bar", "self.st.domain(")),
    ("eval/headless/v3/perturb.py", "Ferries.after_plan", ("self.st.bar", "self.domain_sensors(")),
    ("eval/headless/v3/perturb.py", "Ferries.domain_sensors", ("self.st.domain(",)),
    ("eval/headless/review.py", "eo_kind", ("< bar",)),
    ("eval/headless/review.py", "Reviewer.bar", ("/api/policy",)),
    ("eval/headless/agents_v2.py", "Oracle", ("V2L.RESERVE_PCT",)),
    ("eval/headless/agents_v2.py", "OracleHonest", ("V2L.RESERVE_PCT", "V2L.JUSTIFY_HORIZON_S")),
    ("eval/situation.py", "situation_card", ("for_run(", "render(")),
    ("eval/situation_run.py", "policy_guard", ("show(", "situation_card(")),
    # stage S7: the playbook's "settled", its classification at 0.6 at any level until then
    ("agents/playbook/plays/isr.py", "unsettled", ("roe.shortfall(",)),
    ("agents/playbook/plays/isr.py", "custody_held", ("roe.identified(",)),
    ("agents/playbook/plays/isr.py", "look_thresholds", ("roe.bar",)),
    ("agents/playbook/ledger.py", "Ledger.see_classification", ("roe.shortfall(",)),
    ("eval/headless/agents_v2.py", "LookAtFerries.evaluate", ("V2L.RESOLVE_CONF",)),
]


def _rel(p: Path) -> str:
    return p.relative_to(REPO).as_posix()


def _py_files() -> list[Path]:
    out = []
    for top in ("agents", "eval"):
        for p in sorted((REPO / top).rglob("*.py")):
            rel = _rel(p)
            if "/results/" in rel or "__pycache__" in rel:
                continue
            out.append(p)
    return out


def _target_names(node: ast.AST) -> list[str]:
    targets: list[ast.AST] = []
    if isinstance(node, ast.Assign):
        targets = list(node.targets)
    elif isinstance(node, (ast.AnnAssign, ast.AugAssign)):
        targets = [node.target]
    out = []
    while targets:
        t = targets.pop()
        if isinstance(t, ast.Name):
            out.append(t.id)
        elif isinstance(t, ast.Attribute):
            out.append(t.attr)
        elif isinstance(t, (ast.Tuple, ast.List)):
            targets += list(t.elts)
    return out


def inventory_name(name: str) -> bool:
    return name in NAMES or any(p.match(name) for p in PATTERNS)


def copies_in(path: Path) -> list[str]:
    """Each assignment of an inventory name in a Python file: `file:line name`."""
    tree = ast.parse(path.read_text(), str(path))
    return [f"{_rel(path)}:{n.lineno} {name}" for n in ast.walk(tree)
            for name in _target_names(n) if inventory_name(name)]


def test_no_copy_of_a_setting_is_assigned_outside_the_readers_and_the_frozen_modules():
    found = [c for p in _py_files() if _rel(p) not in ALLOWED for c in copies_in(p)]
    assert not found, ("a copy of a scenario setting (docs/SCENARIO-SETTINGS.md): read it from the record's settings "
                       f"(eval/headless/v3/settings.py) or the run's (agents/roe.py), never assign it: {found}")


def test_the_guard_finds_a_copy():
    """The scanner itself, on a module that copies (so a broken scanner cannot pass the guard)."""
    src = "RESOLVE_CONF = 0.6\nclass X:\n    AIR_GRACE_S = 30.0\n    def f(self):\n        self.HORIZON_S = 300\n"
    tree = ast.parse(src)
    assert sorted(name for n in ast.walk(tree) for name in _target_names(n) if inventory_name(name)) == \
        ["AIR_GRACE_S", "HORIZON_S", "RESOLVE_CONF"]


def _sha(path: str) -> str:
    return hashlib.sha256((REPO / path).read_bytes()).hexdigest()


def test_the_frozen_modules_and_whole_policies_are_pinned():
    """The files allowed to hold a copy are frozen: an edit to one fails here (its new sha256 is a
    decision, made in this file with the reason)."""
    from eval.headless.v3 import settings as S

    have = {p: _sha(p) for p in list(PINNED) + list(WHOLE_POLICIES)}
    want = {**PINNED, **WHOLE_POLICIES}
    assert have == want
    assert _sha("sim/policy-v0-watch.toml") == S.V0_SHA256
    assert sorted(_rel(p) for p in (REPO / "eval" / "headless" / "v2").glob("*.py")) == \
        sorted(p for p in PINNED if p.startswith("eval/headless/v2/"))


def _flat(t: Any, prefix: str = "") -> dict[str, Any]:
    out: dict[str, Any] = {}
    if isinstance(t, dict):
        for k, v in t.items():
            key = f"{prefix}.{k}" if prefix else str(k)
            if isinstance(v, dict):
                out.update(_flat(v, key))
            out[key] = v
    return out


def _tomls() -> list[Path]:
    out = []
    for top in ("agents", "eval", "sim"):
        for p in sorted((REPO / top).rglob("*.toml")):
            rel = _rel(p)
            if "/results/" in rel or "/cache/" in rel:
                continue
            out.append(p)
    return out


def setting_keys(raw: dict[str, Any]) -> list[str]:
    """The settings keys a TOML document sets: `[watch]` beside its circle, the reserve and the
    engagement's identity in `[rules]`, the playbook's copies in a `[playbook]` table (or at the top
    of the playbook's own file)."""
    flat = _flat(raw)
    out = [k for k in flat if k.startswith("watch.") and not k.startswith("watch.area")
           and not isinstance(flat[k], dict)]
    out += [k for k in RULES_KEYS if k in flat]
    for sec in ("", "playbook."):
        for k in PLAYBOOK_KEYS:
            for key in (f"{sec}thresholds.{k}", f"{sec}{k}"):
                if key in flat:
                    out.append(key)
    return sorted(set(out))


def test_no_file_copies_a_setting_outside_the_standing_policy_the_frozen_files_and_supplements():
    found = []
    for p in _tomls():
        rel = _rel(p)
        if rel in STANDING or rel in WHOLE_POLICIES:
            continue
        try:
            raw = tomllib.loads(p.read_text())
        except tomllib.TOMLDecodeError:
            continue
        keys = setting_keys(raw)
        if "extends" in raw:
            # a supplement: the decision layer refuses a key it sets equal to what it inherits, so
            # what it sets is the scenario's own choice; the playbook's copies are never its
            keys = [k for k in keys if k.split(".")[-1] in PLAYBOOK_KEYS]
        if keys:
            found.append(f"{rel}: {', '.join(keys)}")
    assert not found, f"settings copied into files (docs/SCENARIO-SETTINGS.md section 3): {found}"


def test_no_situation_repeats_its_policy_s_circle():
    from eval.policy_file import show
    from eval.situation import load_situation

    found = []
    for p in sorted((REPO / "sim" / "situations").glob("*.toml")):
        raw = tomllib.loads(p.read_text())
        pb = raw.get("playbook") or {}
        points = [tuple(a["center"]) for a in pb.get("watch_area") or [] if isinstance(a, dict) and a.get("center")]
        points += [tuple(a["point"]) for a in pb.get("protected") or [] if isinstance(a, dict) and a.get("point")]
        if not points:
            continue
        policy = load_situation(p).policy
        if policy is None:
            continue
        area = show(policy)["watch"].get("area")
        if area is None:
            continue
        c = tuple(area["center"])
        rep = [q for q in points if math.isclose(q[0], c[0], abs_tol=1e-6) and math.isclose(q[1], c[1], abs_tol=1e-6)]
        if rep:
            found.append(f"{_rel(p)}: {rep} is its policy's [watch.area] centre")
    assert not found, f"the playbook takes the circle from the policy (agents/playbook/config.py `under`): {found}"


def _body(path: str, qual: str) -> str | None:
    src = (REPO / path).read_text()
    tree = ast.parse(src)
    parts = qual.split(".")
    nodes: list[ast.AST] = list(tree.body)
    found: ast.AST | None = None
    for i, name in enumerate(parts):
        found = next((n for n in nodes if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                      and n.name == name), None)
        if found is None:
            return None
        nodes = list(getattr(found, "body", []))
    assert found is not None
    return ast.get_source_segment(src, found)


@pytest.mark.parametrize("path, qual, needles", SITES, ids=[f"{p}:{q}" for p, q, _ in SITES])
def test_each_inventory_site_reads_the_settings(path: str, qual: str, needles: tuple[str, ...]):
    body = _body(path, qual)
    assert body is not None, f"{path}: no {qual} (renamed? update SITES with what reads the settings now)"
    missing = [n for n in needles if n not in body]
    assert not missing, f"{path} {qual} no longer reads {missing}: a setting must come from the settings"


# ---- the perturbation check -----------------------------------------------------------------------

def perturbed() -> tuple[Any, dict[str, Any]]:
    """Settings with every value changed: the bar 0.77, each grace 1.5 times, the horizon 233 s,
    stale 13 s, the look 700 m, the reserve 27 % (after the policy's 900 s task); and what a record
    of such a run keeps (`rec["settings"]`)."""
    from eval.headless.v3 import settings as S

    w = S.v0("golden-gate-v3").watch()
    w.update(bar=0.77, approach_horizon_s=233.0, stale_s=13.0, look_m=700.0, neutral_after_bar=True)
    for d in w["domain"].values():
        d["grace_s"] = 1.5 * float(d["grace_s"])
    kept = {"rules_version": 33, "watch": w, "reserve_pct": 27.0, "task_duration_s": 900,
            "engage": {"min_identity_confidence": 0.77, "min_identity_level": "identify"}, "loosened": {},
            "policy_layers": []}
    return S.of_record({"world": {"name": "golden-gate-v3"}, "settings": kept}), kept


def test_the_settings_reader_carries_every_changed_value():
    from eval.headless.v3 import settings as S

    st, _ = perturbed()
    v0 = S.v0("golden-gate-v3")
    assert st.source == "logged" and not st.is_v0
    assert (st.bar, st.approach_horizon_s, st.stale_s, st.look_m, st.reserve_now()) == (0.77, 233.0, 13.0, 700.0, 27.0)
    for d in S.DOMAINS:
        assert st.rule(d).grace_s == 1.5 * v0.rule(d).grace_s
    assert st.sha256() != v0.sha256() and S.from_dict(st.as_dict()) == st


def _synth_record(kept: dict[str, Any] | None, ident: tuple[Any, ...], battery: Any = None) -> dict[str, Any]:
    from eval.headless.tests.test_v2_labels import Synth, inbound
    from eval.headless.tests.test_v3_labels import UNRESOLVED, v3_record

    s = Synth(600)
    s.contact("boat", path=inbound())
    s.track("T-0001", "boat", ident=lambda t: ident if t >= 120.0 else UNRESOLVED)
    if battery is not None:
        s.vehicle(4, battery)
    rec = v3_record(s)
    if kept is not None:
        rec["settings"] = kept
    return rec


def test_the_labels_and_metrics_follow_the_record_s_settings():
    from eval.headless.tests.test_v2_labels import battery
    from eval.headless.v3 import labels as L
    from eval.headless.v3 import metrics as M

    st, kept = perturbed()
    # the bar: a 0.7 identification by EO/IR resolves the need under v0's 0.6, not under 0.77
    ident = ("small_fast_boat", 0.7, None, ["eo_ir"])
    v0_lab = L.episode_labels(_synth_record(None, ident, battery(50.0, 0.05)))
    p_lab = L.episode_labels(_synth_record(kept, ident, battery(50.0, 0.05)))
    (n0,), (n1,) = v0_lab["needs"], p_lab["needs"]
    assert n0["served"] and not n1["served"]
    # the grace: the deadline 1.5 times as far after the need opens
    assert n1["deadline"] - n1["t_need"] == pytest.approx(1.5 * (n0["deadline"] - n0["t_need"]))
    # the horizon and the look distance reach the multiverse's parameters
    assert p_lab["params"]["justify_horizon_s"] == 233.0 and p_lab["params"]["aim_fallback_m"] == 700.0
    assert v0_lab["params"]["justify_horizon_s"] == 300.0 and v0_lab["params"]["aim_fallback_m"] == 1000.0
    # the reserve: version 2's need (27 % plus a 900 s task at the Puma's 2.5 h, 37 %) against v0's 35 %
    (f0,), (f1,) = v0_lab["fault_needs"], p_lab["fault_needs"]
    assert f0["open_pct"] == 35.0 and f1["open_pct"] == pytest.approx(37.0)
    assert (f0["t_open"], f1["t_open"]) == (300.0, 261.0), "at or under 35 %; under 37 %"
    assert (f0["t_reserve"], f1["t_reserve"]) == (600.0, 460.0), "the reserve: 20 % now; 27 %"
    assert f1["instrument"]["name"] == "v3.low_reserve" and f1["reserve_pct"] == 27.0
    # the metrics censor each domain at its own grace
    m = M.episode_metrics(_synth_record(kept, ident), p_lab)
    assert m["by_domain"]["surface"]["grace_s"] == 1.5 * 60.0


def test_the_label_predicates_follow_the_settings():
    from eval.headless.v2.labels import Params
    from eval.headless.v3 import labels as L
    from eval.headless.v3 import settings as S

    st, _ = perturbed()
    v0 = S.v0("golden-gate-v3")
    tr = {"affiliation": "neutral", "identity": {"kind": "fishing_vessel", "confidence": 0.7, "conflict": None}}
    assert not L.in_doubt(tr, v0) and L.in_doubt(tr, st)
    # an aircraft with no velocity 6.2 km outside the circle: 25 m/s brings it in within 300 s, not 233 s
    centre = v0.area.center
    far = (centre[0] - (3000.0 + 6200.0) / (111_320.0 * math.cos(math.radians(centre[1]))), centre[1])
    kin = {"position": far, "velocity": None, "last_seen_s": 0.0}
    for s, want in ((v0, True), (st, False)):
        p = L.effective(Params(), s)
        assert L.approaches(kin, "air", 0.0, p, centre, 3000.0, s)[0] is want


def test_the_planners_the_oracle_and_the_reviewer_follow_the_settings():
    import numpy as np

    from eval.headless import review as rv
    from eval.headless.v3 import oracle as O
    from eval.headless.v3 import planner as P

    st, _ = perturbed()
    from eval.headless.v3 import settings as S

    v0 = S.v0("golden-gate-v3")
    arr = np.clip(1.0 - P.DISTANCES / 3000.0, 0.0, 1.0)
    q0 = P.Quality("eo_ir", arr, 4000.0, True, bar=v0.bar)
    q1 = P.Quality("eo_ir", arr, 4000.0, True, bar=st.bar)
    assert q1.reach() < q0.reach()
    lo = [P.Look(1, "eo", low=1.0, low_c=0.4), P.Look(2, "eo", low=1.0, low_c=0.4)]  # combine to 0.64
    assert P.p_resolved(lo, v0.bar) == pytest.approx(1.0) and P.p_resolved(lo, st.bar) == 0.0
    for cls in (P.Ceiling, P.HonestCeiling):
        c = cls.__new__(cls)
        c._st = st
        assert c.reserve_pct == 27.0 + cls.SHOWN_BATTERY_PCT
    h = P.HonestCeiling.__new__(P.HonestCeiling)
    h._st = st
    assert h.horizon_s == 233.0 and h.radius == 3000.0
    path = O.Path2({"id": "c", "route": [[-122.5, 37.82], [-122.48, 37.82]], "speed_mps": 10.0}, v0.area.center, 0.0)
    assert O.Need({"id": "c", "domain": "air"}, path, 100.0, st).deadline == 100.0 + 45.0
    assert P.NeedV3("c", "small_drone", "air", path, 100.0, 50.0, 20.0, st=st).deadline == 100.0 + 45.0
    track = {"identity": {"kind": "small_fast_boat", "confidence": 0.7, "supported_by": ["eo_ir"], "conflict": None}}
    assert rv.eo_kind(track, v0.bar) == "small_fast_boat" and rv.eo_kind(track, st.bar) is None


def test_the_card_and_the_agents_follow_the_settings():
    from eval.roe_card import render
    from playbook.config import FROM_WATCH, TCPA_HORIZON_FACTOR, make, under
    from roe import Roe

    st, kept = perturbed()
    policy = {"rules": {"reserve_pct": 27, "task_duration_s": 900,
                        "engage": {"min_identity_confidence": 0.77, "min_identity_level": "identify"}}}
    card = render(st, 33, "golden-gate-v3", policy)
    for want in ("0.77", "233 seconds", "27 %", "within 45 seconds", "within 90 seconds", "within 450 seconds"):
        assert want in card, want
    roe = Roe(st.watch())
    assert (roe.bar, roe.horizon_s, roe.stale_s, roe.look_m) == (0.77, 233.0, 13.0, 700.0)
    t = under(make({}, "test"), roe).data["thresholds"]
    assert set(FROM_WATCH) == {"classifier_fresh_s", "tcpa_max_s"}
    assert t["classifier_fresh_s"] == 13.0 and t["tcpa_max_s"] == TCPA_HORIZON_FACTOR * 233.0


def test_the_families_draw_about_the_settings_circle():
    from eval.headless.v3 import families as F
    from eval.headless.v3 import settings as S

    st, _ = perturbed()
    moved = replace(st, area=S.Area("Pier 7", (-122.39, 37.80), 2500.0))
    d = F.Draft(family="f1-fast-boat", world="golden-gate-v3", seed=1, rng=__import__("random").Random(1), stated={},
                settings=moved)
    assert d.center == (-122.39, 37.80) and d.radius == 2500.0


def test_the_report_states_the_settings_and_never_pools_two_sets():
    """report.py: a scored row keeps a logged run's settings (a v0 row has its world's); the
    coverage names each set per world by its sha256; rows of two sets of values are summarized
    apart, while v0's two worlds (only their circles differ) pool."""
    from eval.headless.v3 import report as RP
    from eval.headless.v3 import settings as S

    st, _ = perturbed()
    v0_row = {"name": "a", "family": "v3-f1-fast-boat-gg", "agent": "none"}
    logged = {"name": "b", "family": "v3-f1-fast-boat-gg", "agent": "none", "settings": st.as_dict()}
    sd_row = {"name": "c", "family": "v3-f1-fast-boat-sd", "agent": "none"}
    assert RP.row_settings(v0_row) == S.v0("golden-gate-v3") and RP.row_settings(logged) == st
    cov = RP.settings_coverage([v0_row, logged, sd_row])
    assert sorted(cov["golden-gate-v3"]) == sorted([S.v0("golden-gate-v3").sha256(), st.sha256()])
    assert [x["source"] for x in cov["san-diego"].values()] == ["v0"]
    groups = RP.by_values([v0_row, logged, sd_row])
    assert sorted(len(g) for g in groups.values()) == [1, 2]
    assert any("bar 0.77" in line for line in RP.settings_markdown(cov))
