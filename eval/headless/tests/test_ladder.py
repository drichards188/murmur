"""The ladder's rule 0.6 (eval/headless/ladder.py `NOT_JUDGED`; SCORING-V2.md 0.6) on synthetic
scored episodes: the truth step in stuck-vehicle (`oracle-linkblind` >= `oracle-honest`) is computed
and reported but not judged, so its reversal neither fails the check nor blocks the freeze; the
same order in any other family, and every other order in stuck-vehicle, is judged as 0.3's rule
judges it; rule 0.3, and validate.py's section 0.5, re-read a batch as 0.5 ran it."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from eval.headless import ladder
from eval.headless import validate as V
from eval.headless.tests.test_agents_v2 import COND, ep

TRUTH_STEP = ("oracle-linkblind", "oracle-honest")
#: per agent, the situations it serves: every order holds or ties, none fails
PLAIN: dict[str, Callable[[int], int]] = {
    "none": lambda sit: 0, "oracle": lambda sit: 1, "oracle-late": lambda sit: int(sit % 5 == 0),
    "oracle-linkblind": lambda sit: sit % 2, "oracle-honest": lambda sit: sit % 2,
}


def batch(**changed: dict[str, Callable[[int], int]]) -> list[dict[str, Any]]:
    """25 situations each of stuck-vehicle and decoy, every rung and `none`; `changed` replaces what
    some agents serve in one family (its name with `_` for `-`)."""
    out = []
    for fam in ("stuck-vehicle", "decoy"):
        served = {**PLAIN, **changed.get(fam.replace("-", "_"), {})}
        out += [ep(a, fam, sit, f(sit)) for sit in range(1, 26) for a, f in served.items()]
    return out


def preds(res: dict[str, Any], fam: str) -> dict[tuple[str, str], dict[str, Any]]:
    return {(p["a"], p["b"]): p for p in res["families"][fam]["predictions"]}


def reversals(res: dict[str, Any]) -> list[tuple[str, str, str]]:
    return [(x["family"], x["a"], x["b"]) for x in res["failures"]]


def test_the_truth_step_in_stuck_vehicle_is_reported_not_judged():
    """The link-blind rung serving nothing in stuck-vehicle, below the honest one wholly: under 0.6
    the reversal is computed and reported, never a failure, and the check passes; under 0.3 (as 0.5
    ran) it fails, and validate.py's section 0.5 blocks on it where 0.6 does not."""
    scored = batch(stuck_vehicle={"oracle-linkblind": lambda sit: 0})
    full = ladder.check(scored)
    res = full[COND]
    p = preds(res, "stuck-vehicle")[TRUTH_STEP]
    assert p["verdict"] == "fails" and p["hi"] < 0 and not p["judged"]
    assert res["rule"] == "0.6" and res["failures"] == [] and res["passed"]
    assert [(x["family"], x["a"], x["b"], x["verdict"]) for x in res["reported"]] == [("stuck-vehicle", *TRUTH_STEP, "fails")]
    assert preds(res, "decoy")[TRUTH_STEP]["judged"], "the truth step stays judged in every other family"
    assert all(q["judged"] for k, q in preds(res, "stuck-vehicle").items() if k != TRUTH_STEP)
    assert not any(q["judged"] for q in res["families"]["*"]["predictions"]), "the pooled row is reported"
    md = ladder.markdown(full)
    assert "Passed: **yes**" in md and "| fails (reported, not judged: 0.6) |" in md
    # 0.3's rule, as 0.3, 0.4 and 0.5 ran: every order judged, so the same batch fails
    old = ladder.check(scored, rule="0.3")[COND]
    assert reversals(old) == [("stuck-vehicle", *TRUTH_STEP)] and not old["passed"] and old["reported"] == []
    # validate.py: section 0.6 (the default) reports it and does not block; 0.5 re-reads it as it ran
    new = V.check(scored)[COND]
    assert new["section"] == "0.6" and new["ladder"]["passed"] and new["ladder"]["failures"] == []
    assert not any(b.startswith("ladder:") for b in new["blocks"])
    assert "reported, not judged (0.6): oracle-linkblind >= oracle-honest in stuck-vehicle fails" in V.markdown({COND: new})
    as_run = V.check(scored, section="0.5")[COND]
    assert "ladder: oracle-linkblind below oracle-honest in stuck-vehicle" in as_run["blocks"]


def test_the_truth_step_reversed_in_another_family_still_fails():
    scored = batch(decoy={"oracle-linkblind": lambda sit: 0})
    res = ladder.check(scored)[COND]
    assert reversals(res) == [("decoy", *TRUTH_STEP)] and not res["passed"]
    assert preds(res, "decoy")[TRUTH_STEP]["judged"]
    assert "ladder: oracle-linkblind below oracle-honest in decoy" in V.check(scored)[COND]["blocks"]


def test_every_other_order_in_stuck_vehicle_is_still_judged():
    """The oracle below the link-blind rung below the honest one, in stuck-vehicle: the truth step
    is reported, and the oracle's order over the link-blind rung fails the check and blocks."""
    scored = batch(stuck_vehicle={"oracle": lambda sit: 0, "oracle-late": lambda sit: 0,
                                  "oracle-honest": lambda sit: 1})
    res = ladder.check(scored)[COND]
    p = preds(res, "stuck-vehicle")
    assert p[TRUTH_STEP]["verdict"] == "fails" and not p[TRUTH_STEP]["judged"]
    assert reversals(res) == [("stuck-vehicle", "oracle", "oracle-linkblind")] and not res["passed"]
    blocks = V.check(scored)[COND]["blocks"]
    assert [b for b in blocks if b.startswith("ladder:")] == ["ladder: oracle below oracle-linkblind in stuck-vehicle"]
