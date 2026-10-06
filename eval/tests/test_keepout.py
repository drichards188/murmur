"""Keep-outs against engagements (eval/keepout.py): the generator, the truth labels, the
tracker-report model, the summaries, and (with a release build of eval/keepout_probe) the
engine's verdicts on generated and hand-built situations."""

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


def _needs_probe():
    if not K.PROBE.exists():
        pytest.skip("no release build of eval/keepout_probe (python -m eval.run keepout builds it)")


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
    # the target is declared hostile by an operator; nobody else is declared
    target = s["probe"]["tracks"]["T-0000"]
    assert target["declared_by"] == "op-1" and target["affiliation"]["value"] == "hostile"
    assert all("declared_by" not in t and t["affiliation"]["value"] in ("neutral", "unknown") for k, t in s["probe"]["tracks"].items() if k != "T-0000")
    # an evaluation for each nominal time to effect, and each platform at each distance
    keys = [e["key"] for e in s["probe"]["evals"]]
    assert keys[: len(K.TTE_S)] == [f"tte-{t}" for t in K.TTE_S]
    assert len(keys) == len(K.TTE_S) + len(K.DIST_M) * len(K.PLATFORMS)


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


def test_the_mixed_traffic_follows_its_shares():
    p, cruise = _params(), K.cruise_speeds()
    kinds: dict[str, int] = {}
    for i in range(30):
        for k, v in K.situation(2, 0.3, i, "mixed", p, cruise)["kinds"].items():
            kinds[k] = kinds.get(k, 0) + v
    n = sum(kinds.values())
    for k, share in K.MIX:
        assert abs(kinds.get(k, 0) / n - share) < 0.04, (k, kinds)


# ---- what the tracker reports ---------------------------------------------------------------------


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
    # a tour boat circling 300 m at 5 m/s: manoeuvring, the fastest it went 5 m/s plus two sigmas
    tour = f(K.Circle((0.0, 0.0), 300.0, 5.0, 1, 0.0), 600.0)
    assert tour["manoeuvre"]["speed_mps"] == pytest.approx(6.0) and tour["manoeuvre"]["change_mps"] > 2.63
    assert tour["manoeuvre"]["t"] == K.T0_MS  # still turning now
    # a slow wide circle turns 1 radian in the memory: 1.9 m/s of change, below the threshold
    assert K.manoeuvre_seen(K.Circle((0.0, 0.0), 600.0, 2.0, 1, 0.0), 600.0, 0.5) is None
    # a zigzag is manoeuvring
    m, _ = K.motion("manoeuvring", (0.0, 0.0), random.Random(5))
    assert K.manoeuvre_seen(m, 600.0, 0.5) is not None
    # a manoeuvre seen only within the track's own history: two legs 4 minutes apart, the
    # track begun after the turn, sees none
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
    """Who is truly inside the effect circle (and the keep-out) at the effect time, from the
    motion: a boat holding 50 m off is in danger whenever; one 200 m off only inside the
    keep-out; one crossing the point is in danger only when it is there; a circling one when its
    circle brings it there; one 5 km off never; one of ours on its way, when it arrives."""
    w = 5.0 / 300.0
    sit = {
        "origin": ORIGIN,
        "truth": {
            "near-hold": {"motion": K.Hold((50.0, 0.0)), "behaviour": "stationary"},
            "buffer-hold": {"motion": K.Hold((0.0, 200.0)), "behaviour": "stationary"},
            "crossing": {"motion": K.Line((-600.0, 0.0), (10.0, 0.0)), "behaviour": "transiting"},
            # round (300, 0) at 5 m/s, at the effect point 60 s from now
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
    assert K.at_risk(sit, effect, 100.0, 75.0, 275.0)["ours_danger"] == ["usv-1"]
    # an effect point elsewhere: the crossing boat at 30 s is 300 m short of the point
    off = K.at_risk(sit, K.to_lonlat(ORIGIN, -300.0, 0.0), 30.0, 75.0, 275.0)
    assert "crossing" in off["danger"]
    # the flat frame round-trips
    e, n = K.to_enu(ORIGIN, K.to_lonlat(ORIGIN, 1234.0, -567.0))
    assert (e, n) == pytest.approx((1234.0, -567.0), abs=1e-6)


# ---- the summaries --------------------------------------------------------------------------------


def test_block_classes_components_and_thresholds():
    assert K.classify({"danger": 1, "near": 1}) == "needed"
    assert K.classify({"danger": 0, "near": 2}) == "buffer"
    assert K.classify({"danger": 0, "near": 0}) == "clear"
    comp = {"current": 120.0, "mean": 80.0, "manoeuvre": -300.0, "young": -50.0}
    assert not K.would_pass_without(comp, "manoeuvre")  # the young region is still inside
    assert K.would_pass_without(comp, ("manoeuvre", "young"))
    assert K.would_pass_without({"current": -1.0}, "current")
    curve = [(30, 1.0), (60, 0.8), (120, 0.4), (300, 0.1)]
    assert K.threshold(curve, 0.5) == pytest.approx(105.0)
    assert K.threshold(curve, 0.9) == pytest.approx(45.0)
    assert K.threshold([(30, 0.4), (60, 0.2)], 0.5) is None
    assert K.threshold([(30, 1.0), (300, 0.95)], 0.9) == math.inf
    # a pooled share whose cases come in clumps (a situation's evaluations) is less certain
    flat = K.cluster_rate([(4, 8)] * 50)
    clumped = K.cluster_rate([(8, 8)] * 25 + [(0, 8)] * 25)
    assert flat["rate"] == clumped["rate"] == 0.5
    assert clumped["deff"] > 4 and (clumped["hi"] - clumped["lo"]) > 2 * (flat["hi"] - flat["lo"])
    assert K.cluster_rate([(0, 3)] * 10)["rate"] == 0.0 and K.cluster_rate([])["rate"] is None


def _rec(sid, b, nominal, kot, *, danger=0, near=0, comp=None, dom=None, kof=0, kof_lost=0, lost=False, seed=1, density=0.3):
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
        "ours_danger": 0,
        "rules": {"12": x},
    }


def test_the_summary_counts_blocks_classes_components_and_fields():
    """Block rates per cell; needed, buffer and clear blocks; the component a conservative
    block is down to; passes that missed someone; the keep-out on our vehicles once per field
    whatever the contacts do."""
    recs = [
        # circling: blocked by the manoeuvre region alone, nobody near; then a needed block
        _rec(0, "circling", 60, 1, comp={"current": 50.0, "mean": 40.0, "manoeuvre": -200.0}, dom="manoeuvre", kof=1, kof_lost=1, lost=True),
        _rec(1, "circling", 60, 1, danger=1, near=1, comp={"current": -100.0}, dom="current"),
        _rec(2, "circling", 60, 0, comp={"current": 30.0}),
        _rec(3, "circling", 60, 0, danger=1, near=1, comp={"current": 5.0}),  # missed
        # young and current both inside: the block is neither one's alone
        _rec(4, "circling", 60, 1, near=1, comp={"current": -10.0, "young": -900.0}, dom="young"),
        # the same fields under another behaviour (field 0's lost friendly blocks under both)
        _rec(0, "stationary", 60, 0, kof=1, kof_lost=1, lost=True),
        _rec(1, "stationary", 60, 0),
        _rec(2, "stationary", 60, 0),
        _rec(3, "stationary", 60, 0),
        _rec(4, "stationary", 60, 0),
    ]
    s = K.summarize(recs, draws=50)
    cell = next(r for r in s["tte"] if r["behaviour"] == "circling")
    assert (cell["tracks"]["k"], cell["tracks"]["n"], cell["missed"]) == (3, 5, 1)
    blk = next(r for r in s["blocks"] if r["behaviour"] == "circling")
    assert (blk["blocks"], blk["needed"]["k"], blk["buffer"], blk["clear"], blk["passes"], blk["missed"]["k"]) == (3, 1, 1, 1, 2, 1)
    cbc = blk["conservative_by_component"]
    assert cbc["manoeuvre"]["pass_without"]["k"] == 1 and cbc["manoeuvre"]["dominant"]["k"] == 1
    assert cbc["young"]["dominant"]["k"] == 1 and cbc["young"]["pass_without"]["k"] == 0
    assert cbc["current"]["inside"]["k"] == 1
    assert blk["conservative_pass_without_rules_9_12"]["k"] == 1
    # the keep-out on our vehicles: five fields, each once (the circling copies are the same
    # fields), one blocked by the lost vehicle
    fr = next(r for r in s["friendly"] if r["tte_s"] == 60)
    assert (fr["fields"], fr["blocked"]["k"], fr["lost"], fr["lost_fields"]) == (5, 1, 1, 1)
    assert s["situations"] == 10 and s["fields"] == 5


# ---- through the engine (the probe) -----------------------------------------------------------------


def test_no_protected_contacts_never_blocks_on_tracks():
    """With nobody to protect, the keep-out on tracks passes under every rules version at every
    time to effect and distance, whatever the platform."""
    _needs_probe()
    p, cruise = _params(), K.cruise_speeds()
    sits = [K.situation(seed, 0.0, i, b, p, cruise) for seed in (1, 2) for i in range(3) for b in ("mixed", "new")]
    assert all(s["n_contacts"] == 0 for s in sits)
    res = K.run_probe([s["probe"] for s in sits])
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


def test_the_engine_decides_hand_built_contacts_by_version():
    """Through the engine: a boat holding 150 m from the target blocks under every rules
    version (along its velocity, needed or not); a young one 2 km off blocks from rules 12 only
    (its young region); a zigzagging one 1 km off from rules 9 (its manoeuvre region)."""
    _needs_probe()
    rng = random.Random(7)
    s0 = _one_contact({})
    origin = s0["origin"]
    tgt = K.to_enu(origin, s0["probe"]["tracks"]["T-0000"]["position"]["value"])

    def at(e, n):
        return (tgt[0] + e, tgt[1] + n)

    def verdicts(track):
        s = _one_contact(track)
        (r,) = K.run_probe([s["probe"]])
        assert all(not x.get("mismatch") for x in r["rules"].values()), r
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


def test_a_small_run_is_deterministic_and_versions_only_add_regions(tmp_path: Path):
    """A run end to end (no cargo here: skipped without the release build): the same records
    twice; no evaluation where the regions disagree with the engine's verdict; rules 9 count
    every region rules 6 does and rules 12 every one rules 9 does, so a block never goes away
    with a newer version; the report renders the section."""
    _needs_probe()
    params = {"densities": [0.3], "behaviours": ["circling", "new"], "tte_s": [30, 120], "dist_m": [1000], "platforms": ["magura"]}
    a = K.run(tmp_path / "a" / "keepout", [1], 3, params=params, workers=1, build=False)
    b = K.run(tmp_path / "b" / "keepout", [1], 3, params=params, workers=2, build=False)

    def recs(p):
        with gzip.open(p / "keepout" / "records.jsonl.gz", "rt") as f:
            return [json.loads(line) for line in f]

    ra, rb = recs(tmp_path / "a"), recs(tmp_path / "b")
    assert ra == rb and len(ra) == 2 * 3 * (2 + 1)
    assert a["counts"]["mismatches"] == 0 and a["counts"]["errors"] == 0 and a["counts"]["no_preview"] == 0
    for r in ra:
        k = {v: x["kot"] for v, x in r["rules"].items()}
        assert k["6"] <= k["9"] <= k["12"], r
    assert a["regions"] and {q["regions"] for q in a["regions"]} == {name for name, *_ in K.REFERENCE}
    assert "Not human data" in K.table(a)
    (tmp_path / "a" / "batch.json").write_text(json.dumps({"id": "a", "started": "now", "seeds": [], "runs": []}))
    from eval.report import build

    html = build(tmp_path / "a").read_text()
    assert "Keep-outs against engagements" in html and "Model results from simulated geometry" in html
