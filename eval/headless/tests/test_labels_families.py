"""Labels, the target association, the family generator and its frozen split; what the
production bridge build may not contain; the baked terrain grid's provenance."""

from __future__ import annotations

import hashlib
import json
import tomllib
from pathlib import Path

import pytest

from eval.headless import REPO
from eval.headless.families import FAMILIES, generate, split, water
from eval.headless.gate import interval, inside, match
from eval.headless.labels import AMBIGUOUS, LOOK_GRACE_S, PROTECTED, Truth, associate, contact_labels, dist_m


def _rec(series: dict[str, list[tuple[float, float, float]]], meta: dict[str, str]) -> dict:
    times = sorted({t for s in series.values() for t, _, _ in s})
    samples = []
    for t in times:
        samples.append({"t": t, "contacts": [[cid, lon, lat, 10.0, True] for cid, s in series.items()
                                             for tt, lon, lat in s if tt == t]})
    return {"truth": {"contacts": [{"id": k, "affiliation": v} for k, v in meta.items()], "samples": samples}}


def _toward(start_m: float, speed: float, t: float) -> tuple[float, float]:
    """A point west of the bridge, closing at `speed`."""
    d = start_m - speed * t
    return (PROTECTED[0] - d / (111_320.0 * 0.79), PROTECTED[1])


def test_a_closing_unknown_needs_a_look_by_its_crossing_plus_a_minute():
    s = {"boat": [(t, *_toward(5000.0, 10.0, t)) for t in range(0, 400, 2)],
         "ferry": [(t, *_toward(2500.0, 0.0, t)) for t in range(0, 400, 2)]}
    rec = _rec({k: [(float(t), lo, la) for t, lo, la in v] for k, v in s.items()}, {"boat": "unknown", "ferry": "neutral"})
    lb = contact_labels(rec)
    assert lb["boat"]["needs_look"] and lb["boat"]["first_within_s"] == pytest.approx(200.0, abs=1.0)
    assert lb["boat"]["look_by_s"] == pytest.approx(lb["boat"]["first_within_s"] + LOOK_GRACE_S)
    assert not lb["ferry"]["needs_look"]  # neutral, inside 3 km: no look


def test_association_names_the_nearest_contact_or_ambiguous():
    a, b = (PROTECTED[0] - 0.02, PROTECTED[1]), (PROTECTED[0] - 0.02, PROTECTED[1] + 0.0015)
    rec = _rec({"a": [(0.0, *a), (2.0, *a)], "b": [(0.0, *b), (2.0, *b)]}, {"a": "unknown", "b": "neutral"})
    tr = Truth(rec)
    near_a = [a[0], a[1] - 0.0003]
    assert associate(tr, near_a, 1.0) == "a"
    between = [a[0], (a[1] + b[1]) / 2]
    assert associate(tr, between, 1.0) == AMBIGUOUS
    far = [a[0] + 0.05, a[1]]
    assert associate(tr, far, 1.0) == AMBIGUOUS
    assert dist_m(a, b) > 100


def test_the_split_is_the_committed_one():
    assert split() == {"fast-unknown": "tuning", "decoy": "tuning", "two-at-once": "held-out",
                       "link-cut-mid-look": "tuning", "stuck-vehicle": "held-out", "low-battery": "tuning",
                       "approval-flood": "held-out"}
    raw = tomllib.loads((REPO / "eval" / "headless" / "split.toml").read_text())
    assert raw["frozen"] == "2026-10-09" and raw["version"] == 1


@pytest.mark.parametrize("family", FAMILIES)
def test_a_family_seed_is_one_situation_and_seeds_vary_it(family: str, tmp_path: Path):
    for d in ("a", "b", "c"):
        (tmp_path / d).mkdir()
    e1, p1 = generate(family, 3, tmp_path / "a")
    e2, p2 = generate(family, 3, tmp_path / "b")
    assert p1.read_text() == p2.read_text() and e1.truth == e2.truth and e1.script == e2.script
    e3, p3 = generate(family, 4, tmp_path / "c")
    assert p3.read_text() != p1.read_text(), "the contacts (their seed, at least) vary with the seed"
    seeds = {tomllib.loads(generate(family, s, tmp_path / "c")[1].read_text())["seed"] for s in range(1, 6)}
    assert len(seeds) == 5, "contact motion is drawn per situation"
    assert e1.split == split()[family]


def test_generated_boats_stay_on_water(tmp_path: Path):
    w = water()
    for fam in ("fast-unknown", "two-at-once"):
        for s in range(1, 21):
            e, p = generate(fam, s, tmp_path)
            for c in tomllib.loads(p.read_text())["contact"]:
                if c.get("kind") == "small_fast_boat" and c["id"].startswith("fast"):
                    assert w.path_wet([tuple(x) for x in c["route"]]), (fam, s, c["route"])


def test_the_production_bridge_build_never_sets_the_headless_cfg():
    bridge = REPO / "crates" / "murmur-bridge"
    assert not (bridge / "build.rs").exists(), "a build script in the bridge could set murmur_headless"
    assert "rustc-cfg" not in (bridge / "Cargo.toml").read_text()
    assert "build =" not in (bridge / "Cargo.toml").read_text()
    # the cfg is the headless crate's own (its build script), and the bridge's test says so
    assert 'cargo:rustc-cfg=murmur_headless' in (REPO / "crates" / "murmur-headless" / "build.rs").read_text()
    assert "assert!(!cfg!(murmur_headless))" in (bridge / "src" / "clock.rs").read_text()


def test_the_terrain_grid_is_the_one_its_provenance_names():
    d = REPO / "sim" / "headless" / "terrain"
    prov = json.loads((d / "golden-gate.provenance.json").read_text())
    assert hashlib.sha256((d / prov["file"]).read_bytes()).hexdigest() == prov["sha256"], \
        "the grid changed without its provenance: bake it again (eval/headless/bake_terrain.py)"
    for k in ("tidewater", "scenario", "spacing_m", "baked", "command", "sampled_at"):
        assert prov.get(k), k


def test_g1_pairs_within_ten_seconds_exactly():
    """CRITERIA.md: within 10 s pairs, beyond it does not (the default window, not a narrower one)."""
    def one(t: float) -> list[dict]:
        return [{"t": t, "play": "isr.recentre", "truth_target": "boat", "uid": 3}]

    assert match(one(200.0), one(209.9)) == [(0, 0)]
    assert match(one(209.9), one(200.0)) == [(0, 0)]
    assert match(one(200.0), one(210.1)) == []
    assert match(one(210.1), one(200.0)) == []
    assert match(one(200.0), one(206.0)) == [(0, 0)]


def test_g1_pairs_and_intervals():
    h = [{"t": 177.0, "play": "isr.investigate", "truth_target": "boat", "uid": 3},
         {"t": 213.0, "play": "isr.recentre", "truth_target": "boat", "uid": 3},
         {"t": 300.0, "play": "isr.recentre", "truth_target": AMBIGUOUS, "uid": 3}]
    f = [{"t": 179.0, "play": "isr.investigate", "truth_target": "boat", "uid": 3},
         {"t": 224.5, "play": "isr.recentre", "truth_target": "boat", "uid": 3},
         {"t": 300.0, "play": "isr.recentre", "truth_target": AMBIGUOUS, "uid": 3}]
    assert match(h, f) == [(0, 0)]
    a = interval("time", "time_to_look", [-115.0] * 10)
    assert a["lo"] == pytest.approx(-117.0) and a["hi"] == pytest.approx(-113.0)
    b = interval("time", "time_to_look", [-114.0, -116.0, -115.5, -114.5, -115.0])
    assert inside(a, b) and inside(b, a)
    p = interval("proportion", "look_covered", [(1, 1)] * 10)
    assert p["mean"] == 1.0 and p["lo"] < 1.0
