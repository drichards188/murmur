"""Scoring v3's model arm (eval/headless/v3/run.py, officer.py; eval/SCORING-V3.md 17.8) with a fake
Anthropic client (fake_anthropic.py): no network, no key, nothing billed.

- one short v3 situation through the runner and the report: the model episode is scored, its brief
  is the world's mission verbatim, it reads two tools and places one order, its index entry and
  record carry the officer's totals, the leakage scan of its every input finds nothing, the report
  has its section with the cost, and the free `none` beside it is as a free batch's;
- the gate: no model run without MURMUR_EVAL_MODEL=1, a batch cap and the model's seeds;
- the cap on the command line is every episode's guard's, through the batch's ledger.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless import officer as O
from eval.headless.v2.record import load
from eval.headless.v3 import families as F
from eval.headless.v3 import run as R
from eval.headless.v3.mission import mission_text
from eval.model_agent import CostGuardExceeded, Ledger

from .fake_anthropic import FakeAnthropic, last_results, text, tool_use

FAMILY = "v3-f1-fast-boat-gg"
SEED = 5000  # a dev seed (SCORING-V3.md 0.1)
DURATION_S = 300.0


@pytest.fixture(scope="module", autouse=True)
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


class OneOrder:
    """At each update: the fleet (compact) and the tracks; the first time, one orbit of the first
    idle unarmed aircraft on the first track shown unknown or suspect (else on the protected point);
    then no action. Keeps the system prompt it was sent."""

    def __init__(self, center: tuple[float, float]):
        self.center = center
        self.system = ""
        self.keys: set[str] = set()
        self.order: dict[str, Any] | None = None
        self.answer: Any = None
        self.n = 0

    def id(self) -> str:
        self.n += 1
        return f"toolu_{self.n:04d}"

    def __call__(self, req: dict[str, Any]) -> tuple[list[Any], str]:
        self.system = "".join(b["text"] for b in req["system"])
        self.keys |= set(req)
        last = req["messages"][-1]
        if isinstance(last.get("content"), str):  # an update
            return [tool_use("get_fleet", {"compact": True}, self.id()), tool_use("get_tracks", {}, self.id())], "tool_use"
        res = last_results(req)
        if "propose_intent" in res:
            self.answer = res["propose_intent"]
            return [text(f"Proposed #{self.answer.get('id')}: a look.")], "end_turn"
        if self.order is not None or "get_fleet" not in res:
            return [text("No action.")], "end_turn"
        fleet = res["get_fleet"].get("vehicles") or []
        tracks = (res.get("get_tracks") or {}).get("tracks") or []
        air = next((v for v in fleet if v.get("domain") == "air" and not v.get("armed") and not v.get("task")), None)
        if air is None:
            return [text("No action: no idle aircraft.")], "end_turn"
        t = next((t for t in tracks if t.get("affiliation") in ("unknown", "suspect") and t.get("position")), None)
        center = (t.get("predicted") or t["position"]) if t else list(self.center)
        self.order = {"task": "orbit", "uids": [air["uid"]], "center": center, "radius_m": 800.0,
                      "rationale": f"Orbit {air['callsign']} for a look\n- {t['id'] if t else 'the protected point'}"}
        return [text("A look."), tool_use("propose_intent", self.order, self.id())], "tool_use"


def test_a_v3_model_episode_through_the_runner_and_the_report(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    from eval.world import load_world

    world = F.parse(FAMILY)[1]
    script = OneOrder(load_world(world).mission.center)
    client = FakeAnthropic(script)
    monkeypatch.setenv("MURMUR_EVAL_MODEL", "1")
    monkeypatch.setattr(O, "make_client", lambda: client)  # in place of the Anthropic client
    generate = F.generate

    def short(name: str, seed: int, workdir: Path):
        e, path = generate(name, seed, workdir)
        e.duration_s = DURATION_S
        return e, path

    monkeypatch.setattr(F, "generate", short)
    out = tmp_path / "batch"
    assert R.main(["batch", "--families", FAMILY, "--seeds", str(SEED), "--agent", "model", "--workers", "1",
                   "--no-build", "--out", str(out), "--max-usd", "5"]) == 0
    idx = {m["agent"]: m for m in json.loads((out / "episodes_v3.json").read_text())}
    assert set(idx) == {"model", "none"}
    m = idx["model"]
    assert m["class"] == "scored" and m["attempts"] == 1 and m["reruns"] == [] and m["leakage_hits"] == 0
    assert m["budget_stopped"] is False and m["model_failed"] is False
    t = m["model"]
    assert t["wakes"] >= 2 and t["api_calls"] == len(client.requests) and t["proposals"] == 1
    assert t["usd"] > 0 and t["tokens"]["cache_read"] > 0 and t["errors"] == 0 and t["refusals"] == 0
    # a free entry is as a free batch writes it
    assert not {"model", "budget_stopped", "model_failed", "leakage_hits"} & set(idx["none"])
    rec = load(m["path"])
    assert rec["scoring"] == "v3" and rec["episode_class"] == "scored" and rec["model"]["totals"] == t
    # same tasking: the watch officer's prompt with the world's mission as the brief, verbatim
    head = (REPO / "agents" / "watch" / "prompt.md").read_text().rstrip() + "\n\n## Situation brief\n\n"
    assert script.system == head + mission_text(world) + "\n"
    # the leakage scan read every input (the prompt, every tool definition, update and tool result)
    lk = rec["v3"]["leakage"]
    assert lk["n_hits"] == 0 and lk["version"] == "2"
    assert lk["by_source"]["system_prompt"] == 1 and lk["by_source"]["update"] == t["wakes"]
    assert lk["by_source"]["tool_result"] == t["tool_calls"] and lk["by_source"]["tool_definition"] > 10
    assert lk["context"]["family"] == FAMILY and lk["context"]["situation_seed"] == SEED
    # one order: the proposal line and its intent
    (p,) = [ln for ln in rec["agent_log"] if ln.get("kind") == "proposal"]
    assert p["task"] == "orbit" and p["intent"] == script.answer["id"] and p["refused"] is None
    pm = rec["provenance"]["model"]
    assert pm["routine"] == {"model": "claude-sonnet-5-5", "effort": "medium"}
    assert pm["consult"] == {"model": "claude-opus-5-5", "effort": "high"} and pm["fallbacks"] is False
    # every request: the effort explicit; no thinking, tool_choice, sampling or fallback parameter
    assert script.keys == {"model", "max_tokens", "system", "tools", "messages", "output_config", "cache_control"}
    assert all(r["effort"] in ("medium", "high") for r in client.requests)
    # the batch's ledger holds what the episode billed, under the command line's cap
    ledger = json.loads((out / R.LEDGER).read_text())
    assert ledger["max_usd"] == 5.0 and ledger["reserved"] == 0 and ledger["usd"] == pytest.approx(t["usd"], abs=1e-5)
    # the report: the model's own section, its cost; the metrics read the scored episode
    assert R.main(["report", str(out), "--workers", "1"]) == 0
    md = (out / "dev_v3.md").read_text()
    assert "## The model watch officer" in md and "never people" in md and "free deterministic agents and the model" in md
    arm = json.loads((out / "dev_v3.json").read_text())["summary"]["model"]
    assert arm["all"]["episodes"] == 1 and arm["all"]["scored"] == 1
    assert arm["all"]["usd_total"] == pytest.approx(t["usd"], abs=1e-4) and arm["ledger"]["max_usd"] == 5.0
    assert set(arm["families"][FAMILY]["served"]) == {"model", "none"}


def _args(**kw: Any) -> SimpleNamespace:
    return SimpleNamespace(**{"max_usd": 5.0, "model_config": None, **kw})


def test_the_gate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    base = ["batch", "--families", FAMILY, "--agent", "model", "--no-build", "--out", str(tmp_path / "b")]
    monkeypatch.delenv("MURMUR_EVAL_MODEL", raising=False)
    with pytest.raises(SystemExit, match="MURMUR_EVAL_MODEL=1"):
        R.main(base + ["--seeds", "6600", "--max-usd", "5"])
    monkeypatch.setenv("MURMUR_EVAL_MODEL", "1")
    with pytest.raises(SystemExit, match="--max-usd"):
        R.main(base + ["--seeds", "6600"])
    for held_out in ("5500", "7000", "1000"):  # dev-validate, test, v2's
        with pytest.raises(SystemExit, match="never dev-validate or test"):
            R.main(base + ["--seeds", "6600-6601", held_out, "--max-usd", "5"])
    with pytest.raises(SystemExit, match="answered by another"):
        R.model_config(_args(model_config='{"fallbacks": true}'), [6600], tmp_path)
    with pytest.raises(SystemExit, match="may not set ledger"):
        R.model_config(_args(model_config='{"ledger": "/tmp/x.json"}'), [6600], tmp_path)
    assert not (tmp_path / "b").exists()  # nothing built, written or run


def test_the_cap_on_the_command_line_is_every_episodes_guard(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """Two episodes' guards (two workers) from the runner's configuration share the batch's ledger:
    what one settles counts against the other, and a call past the cap is never sent."""
    monkeypatch.setenv("MURMUR_EVAL_MODEL", "1")
    cfg = O.ModelConfig.resolve(R.model_config(_args(max_usd=0.05), [5000, 6600], tmp_path))
    assert cfg.batch_max_usd == 0.05 and cfg.ledger == str(tmp_path / R.LEDGER) and cfg.fallbacks is False

    def guard() -> O.WatchGuard:
        return O.WatchGuard(cfg.model, cfg.max_usd, Ledger(Path(cfg.ledger), cfg.batch_max_usd))

    a, b = guard(), guard()
    a.check(0.03)
    a.add({"input_tokens": 10_000, "output_tokens": 2_000})  # $0.04 at Sonnet 5.5's list prices
    with pytest.raises(CostGuardExceeded, match="batch's \\$0.05"):
        b.check(a.estimate_for(cfg.model, 20_000, 100))  # $0.0210: past the cap with a's $0.04
    assert b.tripped and b.calls == 0


def test_paired_needs_count_a_model_episode_not_scored_as_serving_nothing():
    """The report's paired comparison (report.py `paired`): a pair is a need of both episodes of a
    situation, a model episode not scored (None) serves none of the other's needs, a situation the
    model did not run is no pair, and the McNemar p is the exact two-sided one."""
    from eval.headless.v3.report import mcnemar_exact, paired

    other = {("f", 1, 0): {"a": True, "b": False}, ("f", 2, 0): {"a": True}, ("f", 3, 0): {"a": True}}
    model = {("f", 1, 0): {"a": True, "b": True}, ("f", 2, 0): None}
    r = paired(model, other)
    assert [r[k] for k in ("situations", "pairs", "both", "model_only", "other_only", "neither")] == [2, 3, 1, 1, 1, 0]
    assert mcnemar_exact(0, 6) == 0.03125 and mcnemar_exact(1, 9) == round(22 / 1024, 6) and mcnemar_exact(3, 3) == 1.0


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-q"]))
