"""The ROE card (eval/roe_card.py, docs/SCENARIO-SETTINGS.md section 3): before decision rules 33,
or on v0's settings, it is the v0 mission text byte for byte (its sha256 the frozen one per
world); from rules 33 it states each of the scenario's values, here all changed from the standing
ones so that none can come from a constant."""

from __future__ import annotations

import pytest

from eval import roe_card as R
from eval.headless.v3 import settings as S
from eval.headless.v3.mission import mission_text

#: the v0 mission text's sha256 per world (sim/policy-v0-watch.toml `[mission_sha256.<world>]`)
V0_CARDS = {
    "golden-gate-v3": "a2a13f2a19aa8ef7a96fb12b0554d6b725f067f86a6687789028ed71d347dbba",
    "golden-gate-scale": "a2a13f2a19aa8ef7a96fb12b0554d6b725f067f86a6687789028ed71d347dbba",
    "san-diego": "375d89d482db5dfed67669578590427917382e21314c0bb7f5ba247f85be6538",
}


@pytest.mark.parametrize("world", sorted(V0_CARDS))
def test_before_rules_33_the_card_is_the_v0_mission_text(world):
    s = S.v0(world)
    for rules in (21, 32, 33, 40):  # v0's settings give v0's text under any rules
        assert R.render(s, rules, world) == mission_text(world)
        assert R.card_sha256(s, rules, world) == V0_CARDS[world]


def tailored() -> tuple[S.Settings, dict]:
    """A scenario changing every value the card states: bar 0.77, the air's grace 45 s, horizon
    233 s, 420 s on station, reserve 27 % after 1,200 s, the engagement bar 0.81, three signers,
    10 m/s for a surface contact without velocity, still under 0.7 m/s, one loosened key."""
    w = S.v0("san-diego").watch()
    w.update(bar=0.77, approach_horizon_s=233, identify_on_station_s=420, still_mps=0.7, neutral_after_bar=True,
             no_velocity_mps={"air": 25.0, "surface": 10.0})
    w["domain"]["air"]["grace_s"] = 45
    w["area"] = {"name": "Pier 7", "center": [-122.39, 37.80], "radius_m": 2500.0}
    policy = {"watch": w, "loosened": {"nominate_unconfirmed": "the drill rehearses the review"},
              "rules": {"reserve_pct": 27, "task_duration_s": 1200,
                        "engage": {"min_identity_confidence": 0.81, "min_identity_level": "identify"}},
              "tier": [{"name": "engagement", "when": {"kind": ["engage"]}, "tier": "human", "approvers": 3,
                        "proposer_may_approve": False}]}
    return S.from_policy(policy), policy


def test_under_rules_33_the_card_states_the_scenario_s_values():
    s, policy = tailored()
    card = R.render(s, 33, "san-diego", policy)
    for said in (
        "The circle: 2.5 km around Pier 7, its centre at [-122.3900, 37.8000] (longitude, latitude).",
        "- on the surface, within 60 seconds of its coming inside: its kind (the identification level), from EO/IR "
        "or its own true AIS.",
        "- in the air, within 45 seconds of its coming inside: its kind (the identification level), from EO/IR.",
        "- on the ground, within 60 seconds",
        "- under the water, within 300 seconds of its coming inside: its class (the recognition level",
        "at an identity confidence of 0.77 or more",
        "A size class (small_surface, small_air) is a detection, not an identification",
        "by its own look at 0.77 or more, or until its 420 seconds on station are over",
        "No contact is shown neutral or assumed friend by its classification before it is identified at 0.77",
        "brings it inside within 233 seconds",
        "(none, or under 0.7 m/s) is taken to come straight in at 10 m/s on the surface or at 25 m/s in the air",
        "27 % of its endurance left after a task of 1200 seconds",
        "identified at its kind (the identification level) at 0.81 or more, and three people with engagement "
        "authority to sign it, neither of them its proposer",
        "Loosened by this scenario: nominate_unconfirmed, because the drill rehearses the review.",
    ):
        assert said in card, said
    # nothing from the standing values or another world's circle
    for standing in ("0.6", "180 seconds", "300 seconds on station", " 20 %", "900 seconds", "3 km", "Ballast"):
        assert standing not in card, standing
    # the reserve given as arguments in place of the policy's
    assert "30 % of its endurance left after a task of 600 seconds" in R.render(
        s, 33, "san-diego", policy, reserve_pct=30, task_duration_s=600)
    # nothing loosened, nothing said of it
    plain = R.render(S.from_policy({**policy, "loosened": {}}), 33, "san-diego", policy)
    assert "Loosened" not in plain and plain == card.rsplit("\n\n", 1)[0]


def test_each_value_moves_the_card():
    s, policy = tailored()
    base = R.render(s, 33, "san-diego", policy)
    w = s.watch()
    for key, value in (("bar", 0.78), ("approach_horizon_s", 234), ("identify_on_station_s", 421), ("still_mps", 0.8)):
        assert R.render(S.watch_of({**w, key: value}, "logged"), 33, "san-diego", policy) != base, key
    w2 = s.watch()
    w2["domain"]["subsurface"]["grace_s"] = 301
    assert R.render(S.watch_of(w2, "logged"), 33, "san-diego", policy) != base
    assert R.render(s, 33, "san-diego", {**policy, "rules": {**policy["rules"], "reserve_pct": 28}}) != base


def test_the_card_needs_its_circle_and_the_policy_s_values():
    s, policy = tailored()
    with pytest.raises(ValueError, match="rules.reserve_pct"):
        R.render(s, 33, "san-diego", {"rules": {"task_duration_s": 900, "engage": {"min_identity_confidence": 0.8}}})
    with pytest.raises(ValueError, match="circle"):
        w = s.watch()
        del w["area"]
        R.render(S.watch_of(w, "logged"), 33, "san-diego", policy)
    # the floor's level and two signers when the policy names neither
    bare = {"rules": {"reserve_pct": 20, "task_duration_s": 900, "engage": {"min_identity_confidence": 0.8}}}
    assert "at its kind (the identification level) at 0.8 or more, and two people" in R.render(s, 33, "san-diego", bare)
