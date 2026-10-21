"""The v2 batch's clock conditions (`--clocks`, SCORING-V2.md 4.1 and 4.2) and its re-run of an
episode whose model input hit the leakage scan (section 7.1), with the episode faked: no binary,
no network."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from eval.headless import batch
from eval.headless.situations import Calibration


def _args(**kw: Any) -> Any:
    base = dict(families=["v2-decoy"], situation=None, seeds=["1000"], replicates=2, operators=["approve-all"],
                operator=None, policy=None, model_config=None, keep_audit=False, no_anchors=False, order_seed=0,
                reserve=None, clocks=None)
    return batch.argparse.Namespace(**{**base, **kw})


def test_each_clock_is_a_condition_and_a_paused_job_keeps_its_name(tmp_path: Path, monkeypatch):
    from eval.headless.episode import Episode as Ep

    calib = Calibration.load().as_dict()
    plain = batch.v2_jobs(_args(), ["playbook"], calib, tmp_path)
    jobs = batch.v2_jobs(_args(clocks=["paused", "charged"]), ["playbook"], calib, tmp_path)
    assert len(jobs) == 2 * len(plain)
    assert {j["clock"] for j in plain} == {"paused"}
    paused = sorted(batch.job_name(j) for j in jobs if j["clock"] == "paused")
    charged = sorted(batch.job_name(j) for j in jobs if j["clock"] == "charged")
    assert paused == sorted(batch.job_name(j) for j in plain)
    assert all(n.endswith("-charged") for n in charged) and len(set(charged)) == len(charged)

    seen: list[str] = []
    monkeypatch.setattr(Ep, "run", lambda self: seen.append(self.opts.clock) or {})
    for j in jobs:
        wd = tmp_path / batch.job_name(j)
        wd.mkdir()
        batch._v2_episode(j, wd)
    assert seen == [j["clock"] for j in jobs]


def _rec(cls: str, hits: int) -> dict[str, Any]:
    leakage = {"hits": [{"token": "decoy", "source": "tool_result"}] * hits}
    return {"episode_class": cls, "audit_text": "", "truth": {}, "v2": {"picture": {}, "leakage": leakage}}


@pytest.mark.parametrize("classes", [["harness_error", "scored"], ["harness_error", "harness_error"]])
def test_a_leakage_hit_is_run_again_once(tmp_path: Path, monkeypatch, classes: list[str]):
    import eval.headless.v2.aggregate as agg
    import eval.headless.v2.record as record

    calls: list[dict[str, Any]] = []

    def fake_episode(job: dict[str, Any], wd: Path) -> dict[str, Any]:
        calls.append(job)
        cls = classes[len(calls) - 1]
        return _rec(cls, 1 if cls == "harness_error" else 0)

    monkeypatch.setattr(batch, "_v2_episode", fake_episode)
    monkeypatch.setattr(record, "dump", lambda rec, path: 1)
    monkeypatch.setattr(agg, "meta_of", lambda rec, path: {"class": rec["episode_class"], "path": str(path)})
    job = batch.v2_jobs(_args(replicates=1), ["model"], Calibration.load().as_dict(), tmp_path)[0]
    meta = batch._job_v2({**job, "tmp": str(tmp_path)})
    assert len(calls) == 2 and calls[0]["seed"] == calls[1]["seed"]
    assert meta["attempts"] == 2 and meta["class"] == classes[1]
    assert len(meta["reruns"]) == 1 and meta["reruns"][0].startswith("leakage: 1 hits")
