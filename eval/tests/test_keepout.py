"""Keep-outs against engagements (eval/keepout.py): the generator, the truth labels, the
tracker-report model, the summaries, and through eval/keepout_probe the engine's verdicts and
the bridge's own tracker. The probe is built (cargo, a no-op when current) before the tests that
need it; a build that fails fails them, never skips them."""

from __future__ import annotations

import gzip
import json
import math
import random
from pathlib import Path

import pytest

from eval import keepout as K

ORIGIN = (-122.5, 37.81)


def _params(**kw):
    return {**K.default_params(), **kw}


@pytest.fixture(scope="session")
def probe() -> Path:
    """The probe, built now if missing or stale; a failed build fails the test loudly."""
    try:
        return K.build_probe()
    except (RuntimeError, OSError) as e:
        pytest.fail(f"eval/keepout_probe could not be built (cargo build --release -p {K.PROBE_PACKAGE}): {e}")


# ---- the generator ------------------------------------------------------------------------------


def test_situations_are_seeded_and_paired_across_behaviours():
    """The same seed, density and index give the same situation; another seed another. Every
    behaviour of one draw shares the target, the contacts' positions now, our vehicles and the
    platform's placements: only the motion (and what the tracker reports of it) differs."""
    p, cruise = _params(), K.cruise_speeds()
    a = K.situation(3, 0.3, 2, "mixed", p, cruise)
    b = K.situation(3, 0.3, 2, "mixed", p, cruise)
    assert json.dumps(a["probe"], sort_keys=True) == json.dumps(b["probe"], sort_keys=True)
    assert json.dumps(K.situation(4, 0.3, 2, "mixed", p, cruise)["probe"], sort_keys=True) != json.dumps(a["probe"], sort_keys=True)
    s, c = K.situation(3, 0.3, 2, "stationary", p, cruise), K.situation(3, 0.3, 2, "circling", p, cruise)
    assert s["origin"] == c["origin"] and s["n_contacts"] == c["n_contacts"] > 0
    assert s["probe"]["vehicles"] == c["probe"]["vehicles"] and s["probe"]["evals"] == c["probe"]["evals"]
    assert s["probe"]["tracks"]["T-0000"] == c["probe"]["tracks"]["T-0000"]
    for cid in s["truth"]:
        assert s["truth"][cid]["motion"].pos(0.0) == pytest.approx(c["truth"][cid]["motion"].pos(0.0))
    assert {t["behaviour"] for t in s["truth"].values()} == {"stationary"}
    assert {t["behaviour"] for t in c["truth"].values()} == {"circling"}
    target = s["probe"]["tracks"]["T-0000"]
    assert target["declared_by"] == "op-1" and target["affiliation"]["value"] == "hostile"
    assert all("declared_by" not in t and t["affiliation"]["value"] in ("neutral", "unknown") for k, t in s["probe"]["tracks"].items() if k != "T-0000")
    keys = [e["key"] for e in s["probe"]["evals"]]
    assert keys[: len(K.TTE_S)] == [f"tte-{t}" for t in K.TTE_S]
    assert len(keys) == len(K.TTE_S) + len(K.DIST_M) * len(K.PLATFORMS)
    # the real tracker's variants send the motions, not the model's facts; the same truth
    r = K.situation(3, 0.3, 2, "circling", K.variant_params("radar"), cruise)
    assert set(r["probe"]["tracks"]) == {"T-0000"} and set(r["probe"]["tracker"]["contacts"]) == set(c["truth"])
    assert r["probe"]["tracker"]["mode"] == "radar" and r["probe"]["tracker"]["observers"][0]["platform"] == "seahunter"


def test_density_sets_the_number_of_contacts():
    """A Poisson count at the density over the field's disc, every contact inside the disc."""
    p, cruise = _params(), K.cruise_speeds()
    want = 0.1 * math.pi * (K.FIELD_M / 1000.0) ** 2
    ns = []
    for i in range(40):
        s = K.situation(1, 0.1, i, "transiting", p, cruise)
        ns.append(s["n_contacts"])
        for tr in s["truth"].values():
            assert math.hypot(*tr["motion"].pos(0.0)) <= K.FIELD_M + 1e-6
    assert abs(sum(ns) / len(ns) - want) < 3 * math.sqrt(want / len(ns)) + 1
    assert K.situation(1, 0.0, 0, "mixed", p, cruise)["n_contacts"] == 0


def test_the_mixed_traffic_follows_its_shares_and_the_new_share():
    cruise = K.cruise_speeds()
    for share in (0.05, 0.25, 0.0):
        p = _params(new_share=share)
        kinds: dict[str, int] = {}
        for i in range(30):
            for k, v in K.situation(2, 0.3, i, "mixed", p, cruise)["kinds"].items():
                kinds[k] = kinds.get(k, 0) + v
        n = sum(kinds.values())
        for k, w in K.mix_for(share):
            assert abs(kinds.get(k, 0) / n - w) < 0.04, (share, k, kinds)
    assert sum(w for _, w in K.mix_for(0.15)) == pytest.approx(1.0)


def test_lanes_put_most_traffic_near_the_target_and_the_live_variant_a_circler():
    cruise = K.cruise_speeds()
    p = K.variant_params("lanes-model")
    lane, near_u, near_l = 0, 0, 0
    total = 0
    for i in range(20):
        s = K.situation(1, 0.3, i, "mixed", p, cruise)
        u = K.situation(1, 0.3, i, "mixed", _params(), cruise)
        lane += s["kinds"].get("lane", 0)
        total += s["n_contacts"]
        near_l += sum(1 for t in s["truth"].values() if math.hypot(*t["motion"].pos(0.0)) < 1500)
        near_u += sum(1 for t in u["truth"].values() if math.hypot(*t["motion"].pos(0.0)) < 1500)
    assert 0.6 < lane / total < 0.8 and near_l > 2 * near_u
    live = K.situation(1, 0.03, 0, "mixed", K.variant_params("live"), cruise)
    c = live["truth"]["C-9999"]
    assert c["behaviour"] == "circling (near)" and math.hypot(*c["motion"].pos(0.0)) <= 1000.0
    assert [e["key"] for e in live["probe"]["evals"]] == ["dist-1000-magura", "dist-1500-magura", "dist-2000-magura"]


# ---- what the tracker reports (the stated model) --------------------------------------------------


def test_tracker_reports_follow_the_bridge_trackers_rules():
    """No velocity in a track's first seconds, no average velocity before 45 s, an average over
    the last minute after, a manoeuvre only when the velocity really changed beyond the
    threshold within the memory (with the fastest speed plus two sigmas), and when the track
    began."""
    rng = random.Random(1)

    def f(m, tracked, vs=0.5):
        return K.facts(m, tracked, 15.0, vs, rng, ORIGIN, "neutral", 0.5)

    new = f(K.Hold((0.0, 0.0)), 3.0)
    assert "velocity_mps" not in new and "mean_velocity_mps" not in new and "manoeuvre" not in new
    assert new["first_seen_t"] == K.T0_MS - 3000
    young = f(K.Line((0.0, 0.0), (5.0, 0.0)), 30.0)
    assert "velocity_mps" in young and "mean_velocity_mps" not in young and "manoeuvre" not in young
    transit = f(K.Line((0.0, 0.0), (8.0, 0.0)), 600.0)
    assert transit["mean_velocity_mps"][0] == pytest.approx(8.0, abs=1.5) and "manoeuvre" not in transit
    hold = f(K.Hold((100.0, 100.0)), 600.0)
    assert math.hypot(*hold["mean_velocity_mps"]) < 1.5 and "manoeuvre" not in hold
    tour = f(K.Circle((0.0, 0.0), 300.0, 5.0, 1, 0.0), 600.0)
    assert tour["manoeuvre"]["speed_mps"] == pytest.approx(6.0) and tour["manoeuvre"]["change_mps"] > 2.63
    assert tour["manoeuvre"]["t"] == K.T0_MS
    assert K.manoeuvre_seen(K.Circle((0.0, 0.0), 600.0, 2.0, 1, 0.0), 600.0, 0.5) is None
    m, _ = K.motion("manoeuvring", (0.0, 0.0), random.Random(5))
    assert K.manoeuvre_seen(m, 600.0, 0.5) is not None
    legs = K.Legs((0.0, 0.0), [(-900.0, 6.0, 0.0), (-240.0, 0.0, 6.0)])
    assert K.manoeuvre_seen(legs, 600.0, 0.5) is not None
    assert K.manoeuvre_seen(legs, 200.0, 0.5) is None


def test_motions_are_continuous_and_keep_their_pattern():
    rng = random.Random(3)
    for kind in ("stationary", "transiting", "circling", "manoeuvring", "new"):
        for _ in range(20):
            m, tracked = K.motion(kind, (10.0, -20.0), rng)
            assert m.pos(0.0) == pytest.approx((10.0, -20.0))
            assert (tracked < 45.0) == (kind == "new")
            for t in (-500.0, -61.0, 0.0, 33.0, 299.0, 600.0):
                a, b = m.pos(t), m.pos(t + 1.0)
                assert math.hypot(b[0] - a[0], b[1] - a[1]) <= m.vmax + 1e-6
    c = K.Circle((0.0, 0.0), 200.0, 4.0, -1, 1.0)
    for t in (0.0, 100.0, 1000.0):
        assert math.hypot(*c.pos(t)) == pytest.approx(200.0)
        assert math.hypot(*c.vel(t)) == pytest.approx(4.0)


# ---- truth at the effect ----------------------------------------------------------------------------


def test_truth_labels_on_hand_built_cases():
    """Who is truly inside the effect circle and the keep-out at the effect time, and over the
    release window: a boat holding 50 m off is in the circle whenever; one 200 m off inside the
    keep-out only; one crossing the point is inside only when it is there, or later within the
    window; a circling one when its circle brings it there; one 5 km off never; one of ours on
    its way, when it arrives."""
    w = 5.0 / 300.0
    sit = {
        "origin": ORIGIN,
        "truth": {
            "near-hold": {"motion": K.Hold((50.0, 0.0)), "behaviour": "stationary"},
            "buffer-hold": {"motion": K.Hold((0.0, 200.0)), "behaviour": "stationary"},
            "crossing": {"motion": K.Line((-600.0, 0.0), (10.0, 0.0)), "behaviour": "transiting"},
            "circle": {"motion": K.Circle((300.0, 0.0), 300.0, 5.0, 1, 1.5 * math.pi - w * 60.0), "behaviour": "circling"},
            "far": {"motion": K.Hold((5000.0, 0.0)), "behaviour": "stationary"},
        },
        "ours": {"usv-1": K.Line((0.0, -1000.0), (0.0, 10.0))},
    }
    effect = K.to_lonlat(ORIGIN, 0.0, 0.0)
    at60 = K.at_risk(sit, effect, 60.0, 75.0, 275.0)
    assert sorted(at60["danger"]) == ["circle", "crossing", "near-hold"]
    assert sorted(at60["near"]) == ["buffer-hold", "circle", "crossing", "near-hold"]
    assert at60["ours_danger"] == []
    now = K.at_risk(sit, effect, 0.0, 75.0, 275.0)
    assert sorted(now["danger"]) == ["near-hold"] and sorted(now["near"]) == ["buffer-hold", "near-hold"]
    # over the release window from 0 s: the crossing boat arrives at 60 s, the circling one too
    late = K.at_risk(sit, effect, 0.0, 75.0, 275.0, late_s=60.0)
    assert sorted(late["danger_late"]) == ["circle", "crossing", "near-hold"]
    assert sorted(late["near_late"]) == ["buffer-hold", "circle", "crossing", "near-hold"]
    assert sorted(late["danger"]) == ["near-hold"]
    # a window that ends before it arrives
    early = K.at_risk(sit, effect, 0.0, 75.0, 275.0, late_s=20.0)
    assert "crossing" not in early["near_late"] and "crossing" not in early["danger_late"]
    assert K.at_risk(sit, effect, 100.0, 75.0, 275.0)["ours_danger"] == ["usv-1"]
    off = K.at_risk(sit, K.to_lonlat(ORIGIN, -300.0, 0.0), 30.0, 75.0, 275.0)
    assert "crossing" in off["danger"]
    e, n = K.to_enu(ORIGIN, K.to_lonlat(ORIGIN, 1234.0, -567.0))
    assert (e, n) == pytest.approx((1234.0, -567.0), abs=1e-6)


# ---- the summaries --------------------------------------------------------------------------------


def test_block_classes_components_thresholds_and_bounds():
    assert K.classify({"danger": 1, "near": 1}) == "severe"
    assert K.classify({"danger": 0, "near": 2}) == "needed"
    assert K.classify({"danger": 0, "near": 0}) == "conservative"
    comp = {"current": 120.0, "mean": 80.0, "manoeuvre": -300.0, "young": -50.0}
    assert not K.would_pass_without(comp, "manoeuvre")
    assert K.would_pass_without(comp, ("manoeuvre", "young"))
    assert K.would_pass_without({"current": -1.0}, "current")
    # a region 4 cm inside is inside (the probe's margins are unrounded)
    assert not K.would_pass_without({"current": -0.04, "mean": 10.0}, "mean")
    curve = [(30, 1.0), (60, 0.8), (120, 0.4), (300, 0.1)]
    assert K.threshold(curve, 0.5) == pytest.approx(105.0)
    assert K.threshold(curve, 0.9) == pytest.approx(45.0)
    assert K.threshold([(30, 0.4), (60, 0.2)], 0.5) is None
    assert K.threshold([(30, 1.0), (300, 0.95)], 0.9) == math.inf
    flat = K.cluster_rate([(4, 8)] * 50)
    clumped = K.cluster_rate([(8, 8)] * 25 + [(0, 8)] * 25)
    assert flat["rate"] == clumped["rate"] == 0.5
    assert clumped["deff"] > 4 and (clumped["hi"] - clumped["lo"]) > 2 * (flat["hi"] - flat["lo"])
    none = K.cluster_rate([(0, 3)] * 10)
    assert none["rate"] == 0.0 and none["upper95_tries"] == pytest.approx(1 - 0.05 ** (1 / 30))
    assert none["upper95_clusters"] == pytest.approx(1 - 0.05 ** (1 / 10))
    assert K.upper_bound_zero(15938) == pytest.approx(0.000188, rel=0.01)
    assert K.cluster_rate([])["rate"] is None


def _rec(sid, b, nominal, kot, *, danger=0, near=0, near_late=None, danger_late=None, comp=None, dom=None, kof=0, kof_lost=0, lost=False, seed=1, density=0.3):
    x = {"kot": kot, "kof": kof, "margin": min((comp or {"current": 10.0}).values()), "comp": comp or {"current": 10.0}, "dom": dom, "other": []}
    if kof:
        x["kof_lost"] = kof_lost
    return {
        "id": f"{seed}-{density:g}-{sid}-{b}",
        "seed": seed,
        "density": density,
        "behaviour": b,
        "kind": "tte",
        "nominal": nominal,
        "platform": "magura",
        "n_contacts": 3,
        "lost_friendly": lost,
        "preview": True,
        "tte_s": float(nominal),
        "danger": danger,
        "near": near,
        "danger_late": danger if danger_late is None else danger_late,
        "near_late": near if near_late is None else near_late,
        "ours_danger": 0,
        "rules": {"12": x},
    }


def test_the_summary_counts_blocks_classes_components_and_fields():
    """Block rates and perfect knowledge per cell; needed (inside the keep-out), severe (inside
    the circle) and conservative blocks; misses at both levels and over the release window; the
    component a conservative block is down to; the keep-out on our vehicles once per field."""
    recs = [
        # circling: blocked by the manoeuvre region alone, nobody near: conservative
        _rec(0, "circling", 60, 1, comp={"current": 50.0, "mean": 40.0, "manoeuvre": -200.0}, dom="manoeuvre", kof=1, kof_lost=1, lost=True),
        # a severe block (inside the circle)
        _rec(1, "circling", 60, 1, danger=1, near=1, comp={"current": -100.0}, dom="current"),
        # a pass, nobody near now, someone inside the keep-out later in the window
        _rec(2, "circling", 60, 0, comp={"current": 30.0}, near_late=1),
        # a pass with someone in the circle: missed, severe
        _rec(3, "circling", 60, 0, danger=1, near=1, comp={"current": 5.0}),
        # needed at the keep-out level (inside the buffer), young and current both inside
        _rec(4, "circling", 60, 1, near=1, comp={"current": -10.0, "young": -900.0}, dom="young"),
        _rec(0, "stationary", 60, 0, kof=1, kof_lost=1, lost=True),
        _rec(1, "stationary", 60, 0),
        _rec(2, "stationary", 60, 0),
        _rec(3, "stationary", 60, 0),
        _rec(4, "stationary", 60, 0),
    ]
    s = K.summarize(recs, draws=50)
    cell = next(r for r in s["tte"] if r["behaviour"] == "circling")
    assert (cell["tracks"]["k"], cell["tracks"]["n"]) == (3, 5)
    assert (cell["perfect"]["k"], cell["perfect_circle"]["k"], cell["perfect_late"]["k"]) == (3, 2, 4)
    assert cell["missed"] == {"near": 1, "danger": 1, "near_late": 2, "danger_late": 1}
    blk = next(r for r in s["blocks"] if r["behaviour"] == "circling")
    assert (blk["blocks"], blk["needed"]["k"], blk["severe"]["k"], blk["conservative"]["k"], blk["passes"]) == (3, 2, 1, 1, 2)
    assert (blk["missed"]["k"], blk["missed_severe"]["k"], blk["missed_late"]["k"]) == (1, 1, 2)
    cbc = blk["conservative_by_component"]
    assert cbc["manoeuvre"]["pass_without"]["k"] == 1 and cbc["manoeuvre"]["dominant"]["k"] == 1
    assert cbc["young"]["dominant"]["k"] == 0  # the young one's block was needed, not conservative
    assert blk["conservative_pass_without_rules_9_12"]["k"] == 1
    st = next(r for r in s["blocks"] if r["behaviour"] == "stationary")
    assert st["missed"]["k"] == 0 and st["missed"]["upper95_tries"] == pytest.approx(1 - 0.05 ** (1 / 5))
    fr = next(r for r in s["friendly"] if r["tte_s"] == 60)
    assert (fr["fields"], fr["blocked"]["k"], fr["lost"], fr["lost_fields"]) == (5, 1, 1, 1)
    assert s["situations"] == 10 and s["fields"] == 5
    sens = K.sensitivity({"x": s}, behaviour="circling")
    assert sens[0]["variant"] == "x" and sens[0]["missed"]["12"]["k"] == 1


# ---- through the engine (the probe) -----------------------------------------------------------------


def test_the_probe_moves_contacts_as_the_generator_does(probe):
    """The probe's copy of every motion (for the real tracker's history) puts each contact where
    the generator's does, before, at and after the nomination."""
    rng = random.Random(11)
    ms = [K.motion(k, (rng.uniform(-3000, 3000), rng.uniform(-3000, 3000)), rng)[0] for k in ("stationary", "transiting", "circling", "manoeuvring", "manoeuvring", "manoeuvring", "new")]
    times = [-950.0, -600.0, -123.4, -0.5, 0.0, 61.0, 299.9, 950.0]
    got = K.motion_check(ms, times, probe)
    for m, row in zip(ms, got):
        for t, p in zip(times, row):
            assert p == pytest.approx(list(m.pos(t)), abs=1e-6), (type(m).__name__, t)


def test_no_protected_contacts_never_blocks_on_tracks(probe):
    """With nobody to protect, the keep-out on tracks passes under every rules version at every
    time to effect and distance, whatever the platform or the tracker."""
    cruise = K.cruise_speeds()
    for params in (_params(), K.variant_params("radar")):
        sits = [K.situation(seed, 0.0, i, b, params, cruise) for seed in (1, 2) for i in range(3) for b in ("mixed", "new")]
        assert all(s["n_contacts"] == 0 for s in sits)
        res = [r for r in K.run_probe([s["probe"] for s in sits], probe=probe) if "eval" in r]
        assert len(res) == len(sits) * len(sits[0]["probe"]["evals"])
        for r in res:
            assert r.get("preview"), r
            for v, x in r["rules"].items():
                assert x["kot"] == "pass" and x["margin_m"] is None and not x.get("mismatch"), (v, x)


def _one_contact(track: dict, tte: int = 120) -> dict:
    p, cruise = _params(tte_s=[tte], dist_m=[]), K.cruise_speeds()
    s = K.situation(1, 0.0, 0, "stationary", p, cruise)
    s["probe"]["tracks"]["C-0001"] = track
    return s


def test_the_engine_decides_hand_built_contacts_by_version(probe):
    """Through the engine: a boat holding 150 m from the target blocks under every rules
    version; a young one 2 km off from rules 12 only; a zigzagging one 1 km off from rules 9.
    And a region 4 cm inside the keep-out blocks, its margin unrounded."""
    rng = random.Random(7)
    s0 = _one_contact({})
    origin = s0["origin"]
    tgt = K.to_enu(origin, s0["probe"]["tracks"]["T-0000"]["position"]["value"])

    def at(e, n):
        return (tgt[0] + e, tgt[1] + n)

    def run(track):
        s = _one_contact(track)
        (r,) = [x for x in K.run_probe([s["probe"]], probe=probe) if "eval" in x]
        assert all(not x.get("mismatch") for x in r["rules"].values()), r
        return r

    def verdicts(track):
        r = run(track)
        return {v: (x["kot"], (x.get("intr") or [[None, None]])[0][1]) for v, x in r["rules"].items()}

    near = K.facts(K.Hold(at(150.0, 0.0)), 600.0, 10.0, 0.0, rng, origin, "neutral", 0.6)
    assert verdicts(near) == {"6": ("fail", "current"), "9": ("fail", "current"), "12": ("fail", "current")}
    young = K.facts(K.Hold(at(0.0, 2000.0)), 20.0, 10.0, 0.0, rng, origin, "unknown", 0.3)
    v = verdicts(young)
    assert v["6"][0] == v["9"][0] == "pass" and v["12"] == ("fail", "young")
    legs = K.Legs(at(-1000.0, 0.0), [(-900.0 + 60.0 * k, *(K._vec(6.0, math.radians(45 if k % 2 else -45)))) for k in range(40)])
    zig = K.facts(legs, 600.0, 10.0, 0.0, rng, origin, "neutral", 0.6)
    assert "manoeuvre" in zig
    v = verdicts(zig)
    assert v["6"][0] == "pass" and v["9"] == ("fail", "manoeuvre") and v["12"][0] == "fail"
    # 4 cm inside: a holding boat moved toward the effect point by its margin plus 4 cm
    hold = K.facts(K.Hold(at(900.0, 0.0)), 600.0, 10.0, 0.0, random.Random(1), origin, "neutral", 0.6)
    hold["velocity_mps"], hold["mean_velocity_mps"] = [0.0, 0.0], [0.0, 0.0]
    r = run(hold)
    assert r["rules"]["6"]["margin_m"] > 0
    eff = K.to_enu(origin, r["preview"]["effect"])
    moved = hold
    for _ in range(4):
        # step toward the effect point by what is left (the flat frame and the engine's haversine
        # differ by about 0.1%, so a few steps)
        m = r["rules"]["6"]["comp"]["current"]
        pos = K.to_enu(origin, moved["position"]["value"])
        d = math.hypot(pos[0] - eff[0], pos[1] - eff[1])
        k = (d - (m + 0.04)) / d
        moved = dict(hold, position={"value": K.to_lonlat(origin, eff[0] + (pos[0] - eff[0]) * k, eff[1] + (pos[1] - eff[1]) * k), "t": K.T0_MS})
        r = run(moved)
    m = r["rules"]["6"]["comp"]["current"]
    assert -0.045 < m < -0.035 and r["rules"]["6"]["kot"] == "fail"
    assert not K.would_pass_without(r["rules"]["6"]["comp"], "mean")


def test_the_real_tracker_makes_the_tracks(probe):
    """The radar variant's tracks come from the bridge's tracker: every contact near our USV is
    tracked, reported with its own 1-sigma and velocity, and with AIS (every sensor, every
    contact broadcasting) the tracks are much better than radar alone."""
    cruise = K.cruise_speeds()
    out = {}
    for mode in ("radar", "ais"):
        sits = [K.situation(1, 0.3, i, "mixed", K.variant_params(mode, {"tte_s": [60], "dist_m": []}), cruise) for i in range(3)]
        res = K.run_probe([s["probe"] for s in sits], probe=probe)
        tr = [r["tracker"] for r in res if "tracker" in r]
        assert len(tr) == 3 and sum(t["tracked"] for t in tr) >= 0.95 * sum(t["contacts"] for t in tr)
        assert all(not x.get("mismatch") for r in res if "eval" in r for x in r["rules"].values())
        q = K.tracker_quality([p for t in tr for p in t.get("per_contact", [])])
        steady = next(x for x in q if x["behaviour"] == "transiting")
        out[mode] = steady
    assert out["ais"]["vel_err_median"] < 0.5 * out["radar"]["vel_err_median"]
    assert out["ais"]["sigma_median"] < out["radar"]["sigma_median"]


def test_a_small_run_is_deterministic_and_versions_only_add_regions(probe, tmp_path: Path):
    """Variants end to end: the same records twice (one process or several); no evaluation
    where the regions disagree with the engine's verdict; a block never goes away with a newer
    rules version; the combined summary carries the sensitivity rows; the report renders it."""
    over = {"densities": [0.3], "behaviours": ["circling", "new"], "tte_s": [30, 120], "dist_m": [1000], "platforms": ["magura"]}
    a = K.run_all(tmp_path / "a" / "keepout", [1], 3, variants=["radar", "model"], overrides=over, workers=1, build=False)
    b = K.run_all(tmp_path / "b" / "keepout", [1], 3, variants=["radar", "model"], overrides=over, workers=2, build=False)

    def recs(p, v):
        with gzip.open(p / "keepout" / v / "records.jsonl.gz", "rt") as f:
            return [json.loads(line) for line in f]

    for v in ("radar", "model"):
        ra, rb = recs(tmp_path / "a", v), recs(tmp_path / "b", v)
        assert ra == rb and len(ra) == 2 * 3 * (2 + 1)
        s = a["variants"][v]
        assert s["counts"]["mismatches"] == 0 and s["counts"]["errors"] == 0 and s["counts"]["no_preview"] == 0
        for r in ra:
            k = {x: y["kot"] for x, y in r["rules"].items()}
            assert k["6"] <= k["9"] <= k["12"], r
    assert a["primary"] == "radar" and a["variants"]["radar"]["tracker_quality"]
    assert {q["regions"] for q in a["variants"]["radar"]["regions"]} == {name for name, *_ in K.REFERENCE}
    assert "Not human data" in K.table(a)
    (tmp_path / "a" / "batch.json").write_text(json.dumps({"id": "a", "started": "now", "seeds": [], "runs": []}))
    from eval.report import build

    html = build(tmp_path / "a").read_text()
    assert "Keep-outs against engagements" in html and "Model results from simulated geometry" in html and "Perfect knowledge" in html
