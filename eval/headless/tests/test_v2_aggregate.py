"""Scoring v2's aggregate (eval/SCORING-V2.md, sections 3.1, 7, 8) on synthetic scored episodes:
replicates averaged within a situation, the model's replicate 0 alone in the primary, section 7's
exclusions and sensitivity, the incomplete mark, Holm, and the scorecard's cells."""

from __future__ import annotations

import copy
from typing import Any

import pytest

from eval.headless.tests.test_v2_labels import Synth, inbound, labels, orbit, resolved_from, rising
from eval.headless.v2 import aggregate as A
from eval.headless.v2 import metrics as M
from eval.headless.v2.scorecard import UNDERPOWERED, markdown, scorecard


def base_metrics() -> dict[str, Any]:
    s = Synth(300)
    s.contact("boat", path=inbound())
    s.track("T-0001", "boat", ident=resolved_from(130.0), hits=rising("3:eo_ir", 105.0, 135.0))
    s.vehicle(3, lambda t: {})
    orbit(s, 10, 3, created=95.0, start=100.0)
    return M.episode_metrics(s.record(), labels(s, none_served={"boat": False}))


BASE = base_metrics()


def ep(agent: str, sit: int, r: int, served: float, cls: str = "scored", operator: str = "approve-all") -> dict[str, Any]:
    m = copy.deepcopy(BASE)
    m["served_share"] = served
    return {"path": None, "agent": agent, "base": "fast-unknown", "family": "fast-unknown",
            "situation": f"fast-unknown:{sit}", "situation_seed": sit, "r": r, "operator": operator,
            "policy": "watch.toml", "clock": "paused", "class": cls, "area": None,
            "metrics": m if cls == "scored" else None, "cells": {}}


def test_the_primary_reads_the_model_at_replicate_0_and_averages_the_playbook():
    scored = []
    for sit in range(1, 5):
        scored += [ep("model", sit, 0, 1.0), ep("model", sit, 1, 0.0),  # replicate 1 never enters
                   ep("playbook", sit, 0, 0.5), ep("playbook", sit, 1, 0.0), ep("none", sit, 0, 0.0)]
    p = A.primary_endpoint(scored, ("model", "playbook"), "approve-all", ("model",), 0)
    c = p["approve-all;watch.toml;paused"]
    assert c["n_situations"] == 4 and c["mean_difference"] == pytest.approx(0.75)
    assert c["method"] == "exact sign test" and c["positive"] == 4


def test_harness_errors_and_budget_stops_are_excluded_with_a_sensitivity_line():
    scored = []
    for sit in range(1, 41):
        scored += [ep("playbook", sit, 0, 0.5), ep("none", sit, 0, 0.0)]
        if sit == 1:
            scored.append(ep("model", sit, 0, 0.0, cls="harness_error"))
        elif sit in (2, 3):
            scored.append(ep("model", sit, 0, 0.0, cls="budget_stop"))
        else:
            scored.append(ep("model", sit, 0, 1.0))
        scored.append(ep("model", sit, 1, 0.0, cls="model_failed"))  # replicate 1: not the primary's
    c = A.comparison(scored, "model", "playbook", "approve-all;watch.toml;paused", "served_share", r0_only_a=True)
    assert c["n_situations"] == 37 and set(c["excluded"]) == {"fast-unknown:1", "fast-unknown:2", "fast-unknown:3"}
    assert c["excluded_share"] == pytest.approx(3 / 40) and c["incomplete"]
    assert c["method"] == "paired cluster bootstrap (BCa)" and c["lo"] == c["hi"] == pytest.approx(0.5)
    sens = c["sensitivity_every_need_unserved"]
    assert sens["n_situations"] == 39 and sens["mean_difference"] == pytest.approx((37 * 0.5 + 2 * -0.5) / 39)


def test_the_aggregate_and_the_scorecard():
    scored = []
    for sit in range(1, 6):
        scored += [ep("playbook", sit, 0, 1.0), ep("playbook", sit, 1, 0.5), ep("none", sit, 0, 0.0),
                   ep("none", sit, 1, 0.0)]
    agg = A.aggregate(scored)
    block = agg["families"]["fast-unknown"]["approve-all;watch.toml;paused"]
    arm = block["arms"]["playbook"]["metrics"]["served_share"]
    assert arm["estimate"] == pytest.approx(0.75) and arm["n_situations"] == 5 and arm["underpowered"]
    comp = block["comparisons"]["playbook - none"]["served_share"]
    assert comp["mean_difference"] == pytest.approx(0.75) and comp["p"] == pytest.approx(2 / 32)
    assert "approve-all;watch.toml;paused playbook - none served_share" in agg["holm"]
    card = scorecard(scored)
    row = card["conditions"]["approve-all;watch.toml;paused"]["rows"][0]
    assert row["area"] == "S1" and row["metrics"]["served_share"]["playbook"]["mark"] == UNDERPOWERED
    assert row["checks"]["playbook"]["invariant_violations"]["k"] == 0 and row["checks"]["playbook"]["needs"] == 10
    md = markdown(card)
    assert "| S1 identify (fast-unknown) | served_share | 75% [" in md and "none" in md


def test_the_driver_sets_the_episode_class():
    """Section 7: the episode sets scored, budget_stop or model_failed (the batch sets
    harness_error); no metric does. Checked on the driver's own method, no model run."""
    from types import SimpleNamespace

    from eval.headless.episode import Episode, Options
    from eval.headless.v2.record import PictureEncoder

    def part(agent: str, stopped: bool = False, failed: bool = False) -> dict[str, Any]:
        ep = Episode.__new__(Episode)
        ep.opts = Options(agent=agent, scoring="v2", replicate={"r": 1, "seed": 5, "situation_seed": 1000})
        ep.agent = SimpleNamespace(budget_stopped=stopped, model_failed=failed)
        ep.operator = None
        ep.e = SimpleNamespace(policy="sim/situations/policies/watch.toml")
        ep.sim_epoch = 0.0
        ep.picture_enc = PictureEncoder(0.0)
        return ep.v2_part("")

    assert part("playbook")["episode_class"] == "scored"
    assert part("model", stopped=True)["episode_class"] == "budget_stop"
    assert part("model", failed=True)["episode_class"] == "model_failed"
    p = part("none")
    assert p["replicate"] == {"r": 1, "seed": 5, "situation_seed": 1000}
    assert p["v2"]["condition"]["policy"] == "watch.toml"


def test_area_of_strips_a_version_mark():
    assert A.area_of({"family": "fast-unknown-v2"}) == "S1"
    assert A.area_of({"family": "v2-conflict"}) == "S8"
    assert A.area_of({"family": "anything", "area": "sealed"}) == "sealed"
    assert A.area_of({"base": "harbor-watch"}) == "HW"
    assert A.area_of({"family": "unknown-family"}) is None
