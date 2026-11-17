"""The settings a record was graded under (eval/headless/v3/settings.py, docs/SCENARIO-SETTINGS.md
section 3): the frozen v0 file is pinned by its sha256 and holds exactly what the code held before
decision rules 33; a record whose run logged a `[watch]` is read by it; every existing record
logged none and is v0's, its mission text's sha256 one of the two the frozen file records."""

from __future__ import annotations

import gzip
import hashlib
import json
import os
from pathlib import Path

import pytest

from eval import REPO
from eval.headless.v2 import labels as V2
from eval.headless.v3 import mission as M
from eval.headless.v3 import settings as S
from eval.world import load_world

#: the frozen file's sha256: editing sim/policy-v0-watch.toml fails here (and in `S.frozen`)
V0_SHA256 = "b09c6492674271dd2d8f30686c05cd9ff2b645453068dffae8456196d87a0791"
#: the result records (gitignored, only read here; a test whose record is absent skips): a
#: worktree has none, so point `MURMUR_RECORDS` at the main checkout's `eval/results/headless`
RESULTS = Path(os.environ.get("MURMUR_RECORDS") or REPO / "eval" / "results" / "headless")


@pytest.fixture(autouse=True)
def _fresh():
    S.frozen.cache_clear()
    S.v0.cache_clear()
    yield
    S.frozen.cache_clear()
    S.v0.cache_clear()


def test_the_frozen_file_is_pinned():
    assert hashlib.sha256(S.V0_PATH.read_bytes()).hexdigest() == V0_SHA256
    assert S.V0_SHA256 == V0_SHA256


def test_an_edited_frozen_file_is_refused(tmp_path, monkeypatch):
    edited = tmp_path / "policy-v0-watch.toml"
    edited.write_text(S.V0_PATH.read_text().replace("bar = 0.6", "bar = 0.65"))
    monkeypatch.setattr(S, "V0_PATH", edited)
    with pytest.raises(ValueError, match="frozen"):
        S.v0("golden-gate-v3")


#: the place each v3 world's v0 text names (what mission.py's `PLACES` held before stage S6 moved
#: it into the frozen file)
PLACES = {"golden-gate-v3": "the Golden Gate Bridge", "golden-gate-scale": "the Golden Gate Bridge",
          "san-diego": "the bay entrance off Ballast Point"}


def test_v0_holds_what_the_code_held():
    """Each frozen value against what the code held before stage S6 (mission.py's constants, which
    S6 removed: the values here are theirs; v2/labels.py's, still there; the worlds' `[mission]`
    tables, which S6 removed: each world's named point and 3 km), and the current mission text's
    sha256 per world."""
    centre = {"golden-gate-v3": "bridge", "golden-gate-scale": "bridge", "san-diego": "bay_entrance"}
    for world in ("golden-gate-v3", "san-diego", "golden-gate-scale"):
        s = S.v0(world)
        assert s.source == "v0" and s.is_v0 and s.loosened == ()
        assert s.bar == V2.RESOLVE_CONF == 0.6 and s.still_mps == 0.5
        assert dict(s.no_velocity_mps) == {"air": 25.0}
        assert s.approach_horizon_s == V2.JUSTIFY_HORIZON_S and s.look_m == V2.AIM_FALLBACK_M
        assert (s.identify_on_station_s, s.stale_s, s.neutral_after_bar) == (300.0, 10.0, False)
        held = {"surface": ("identify", 60.0, ("eo_ir", "ais")), "air": ("identify", 30.0, ("eo_ir",)),
                "ground": ("identify", 60.0, ("eo_ir",)), "subsurface": ("recognise", 300.0, ("sonar",))}
        for d, (level, grace, sensors) in held.items():
            c = s.domain(d)
            assert c is not None and (c.level, c.grace_s, c.sensors) == (level, grace, sensors)
            r = s.rule(d)
            assert r.sensors == tuple(x for x in sensors if x != "ais") and r.ais_counts == ("ais" in sensors)
        assert s.area == S.Area(PLACES[world], load_world(world).point(centre[world]), 3000.0)
        assert s.v0 is not None
        assert (s.v0.low_reserve_open_pct, s.v0.reserve_now_pct) == (V2.LOW_RESERVE_OPEN_PCT, V2.RESERVE_PCT)
        assert s.reserve_now() == V2.RESERVE_PCT
        assert s.v0.mission_sha256 == M.mission_sha256(world)
        # the worlds no longer hold the circle: the frozen file and a scenario's policy do
        assert load_world(world).mission is None
    assert S.v0("san-diego").area.center == (-117.229, 32.686)
    with pytest.raises(ValueError, match="no v0 circle"):
        S.v0("golden-gate")


def _watch(**kw) -> dict:
    w = S.v0("golden-gate-v3").watch()
    w.update(kw)
    return w


def test_a_logged_watch_is_read_from_run_started(tmp_path):
    w = _watch(bar=0.77, approach_horizon_s=233)
    w["domain"]["air"]["grace_s"] = 45
    w["area"] = {"name": "the drill's pier", "center": [-122.40, 37.80], "radius_m": 2000.0}
    started = {"seq": 1, "type": "run.started", "rules_version": 33,
               "policy": {"watch": w, "loosened": {"rules.engage.min_identity_confidence": "a drill"}}}
    text = json.dumps(started) + "\n" + json.dumps({"seq": 2, "type": "intent.created"}) + "\n"
    rec = {"world": {"name": "golden-gate-v3"}, "audit_text": text}
    s = S.of_record(rec)
    assert s.source == "logged" and not s.is_v0
    assert (s.bar, s.approach_horizon_s, s.domain("air").grace_s) == (0.77, 233.0, 45.0)
    assert s.area == S.Area("the drill's pier", (-122.40, 37.80), 2000.0)
    assert s.loosened == (("rules.engage.min_identity_confidence", "a drill"),)
    # the same from the audit log kept beside a saved record (which has no audit_text)
    kept = tmp_path / "x.audit.jsonl"
    kept.write_text(text)
    assert S.of_record({"world": {"name": "golden-gate-v3"}}, audit=kept) == s
    # a run that logged its policy without a [watch] is v0's
    started["policy"] = {"rules": {}}
    assert S.of_record({"world": {"name": "san-diego"}, "audit_text": json.dumps(started)}) == S.v0("san-diego")
    # an unknown key is refused, not passed over
    with pytest.raises(ValueError, match="unknown"):
        S.from_policy({"watch": _watch(stale=12)})


def test_a_saved_record_keeps_the_settings_its_run_logged():
    """A saved record keeps no run.started (batch.py drops `audit_text`): the v3 recorder copies
    what the run logged into `rec["settings"]` (`record_settings`, rules 33 on), and `of_record`
    reads it first. Under older rules nothing is added (the record digests stay)."""
    w = _watch(bar=0.75, stale_s=12)
    w["area"] = {"name": "the drill's pier", "center": [-122.40, 37.80], "radius_m": 2000.0}
    layers = [{"path": "sim/situations/policies/drill.toml", "sha256": "a" * 64},
              {"path": "watch.toml", "sha256": "b" * 64}]
    started = {"seq": 1, "type": "run.started", "rules_version": 33, "policy_layers": layers,
               "policy": {"watch": w, "loosened": {"identification.nominate_unconfirmed": "the drill"},
                          "rules": {"reserve_pct": 25.0, "task_duration_s": 600,
                                    "engage": {"min_identity_confidence": 0.8, "min_identity_level": "identify"}}}}
    kept = S.record_settings(started)
    assert kept == {"rules_version": 33, "watch": w, "reserve_pct": 25.0, "task_duration_s": 600,
                    "engage": {"min_identity_confidence": 0.8, "min_identity_level": "identify"},
                    "loosened": {"identification.nominate_unconfirmed": "the drill"}, "policy_layers": layers}
    # read first, with no run.started anywhere (a saved record)
    s = S.of_record({"world": {"name": "golden-gate-v3"}, "settings": kept})
    assert s.source == "logged" and (s.bar, s.stale_s) == (0.75, 12.0)
    assert s.area == S.Area("the drill's pier", (-122.40, 37.80), 2000.0)
    assert s.loosened == (("identification.nominate_unconfirmed", "the drill"),)
    # the same values as reading the run.started itself
    assert s == S.of_record({"world": {"name": "golden-gate-v3"}, "audit_text": json.dumps(started)})
    # a run that logged no [watch] (rules below 33): nothing kept, v0's
    assert S.record_settings({"type": "run.started", "rules_version": 32, "policy": {"rules": {}}}) is None
    assert S.record_settings(None) is None


def test_the_recorder_keeps_settings_only_when_the_run_logged_a_watch():
    """episode.py's v3 part: `settings` from the audit log's run.started when it holds a [watch],
    no key at all otherwise; and the picture's identity levels with them (v3/record.py), never
    without."""
    from types import SimpleNamespace

    from eval.headless.episode import Episode

    def part(policy: dict) -> dict:
        ep = Episode.__new__(Episode)
        ep.opts = SimpleNamespace(agent="none", replicate=None)
        ep.operator = None
        ep.e = SimpleNamespace(policy="sim/situations/policies/golden-gate-v3-watch.toml")
        ep.sim_epoch = 0.0
        ep.picture_enc = SimpleNamespace(finish=lambda levels=False: {"times": [], "levels_asked": levels})
        text = json.dumps({"seq": 1, "t": 0, "type": "run.started", "rules_version": 33, "policy": policy}) + "\n"
        return ep.v2_part(text, "v3")

    old = part({"rules": {}})
    assert "settings" not in old and not old["v3"]["picture"]["levels_asked"]
    got = part({"watch": _watch(), "rules": {"reserve_pct": 20.0, "task_duration_s": 900}})
    assert got["settings"]["watch"] == _watch() and got["settings"]["rules_version"] == 33
    assert got["v3"]["picture"]["levels_asked"]


def test_policy_file_show_returns_the_effective_watch():
    """`murmur-policy show` (eval/policy_file.py): under rules 32 the legacy watch with the scenario's
    circle; under rules 33, the default, the merged [watch], which the logged policy carries too."""
    from eval.policy_file import show

    shown = show("sim/situations/policies/san-diego-watch.toml", rules=32)
    w = shown["watch"]
    v0 = S.v0("san-diego")
    assert S.watch_of(w, "logged").watch() == v0.watch()
    assert "watch" not in shown["policy"]
    assert [layer["path"] for layer in shown["layers"]][1:] == ["watch.toml", "../../policy.toml"]
    new = show("sim/situations/policies/san-diego-watch.toml")
    assert new["watch"] == new["policy"]["watch"] and new["watch"]["neutral_after_bar"] is True
    assert S.from_policy(new["policy"]).area == v0.area
    # the standing policy names no circle
    assert "area" not in show("sim/policy.toml")["watch"]


#: one record per (result directory, world): the mission text each was briefed with
RECORDS = [
    ("v3-baseline-2026-11-14/v3-f1-fast-boat-gg-none-s5000-r0.json.gz", "golden-gate-v3", "before_isxg"),
    ("v3-model-pilot-2026-11-14", "san-diego", "before_isxg"),
    ("v3-effective-2026-11-16", "golden-gate-v3", "current"),
    ("v3-model-drone-check-2026-11-15/v3-f2-drone-sd-model-s6605-r0.json.gz", "san-diego", "current"),
    ("v3-model-night-2026-11-16/v3-f12-gnss-gg-model-s5000-r0.json.gz", "golden-gate-v3", "current"),
]


def _record(where: str, world: str) -> Path | None:
    p = RESULTS / where
    if p.is_file():
        return p
    tag = "-sd-" if world == "san-diego" else "-gg-"
    return next((f for f in sorted(p.glob("*.json.gz")) if tag in f.name), None) if p.is_dir() else None


@pytest.mark.parametrize("where,world,which", RECORDS)
def test_existing_records_are_graded_under_v0(where, world, which):
    path = _record(where, world)
    if path is None:
        pytest.skip(f"no record at {RESULTS / where}")
    rec = json.loads(gzip.open(path).read())
    s = S.of_record(rec)
    assert s == S.v0(world) and s.area is not None and s.area.name == PLACES[world]
    assert S.logged_policy(rec) is None  # a saved record keeps no run.started
    assert s.v0 is not None
    want = s.v0.mission_sha256 if which == "current" else s.v0.mission_sha256_before_isxg
    assert rec["truth"]["stated"]["mission_sha256"] == want
