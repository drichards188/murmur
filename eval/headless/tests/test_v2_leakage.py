"""Scoring v2's same tasking and leakage scan (eval/SCORING-V2.md 4.3, 4.4 and 7;
eval/headless/v2/leakage.py), on the real `murmur-headless` binary with a fake Anthropic client
(fake_anthropic.py): no network, nothing billed.

- the token list: every family name and its `v2-` form, run names, seeds in names, the scorer's
  field names; ordinary words, numbers, the opaque run id and the bridge's own keys are not hits;
  no field name is used by the bridge, its tool definitions or the watch prompt;
- same tasking: every v2 situation's brief is the mission verbatim, the system prompt is the
  watch prompt with that brief and nothing else, the same prompt and tools for every situation;
- the real v2 brief, the real system prompt and the real tool definitions pass the scan;
- a model that reads every tool on every v2 situation receives nothing the scan flags;
- a family name in a tool result (an opening order's note naming it) is a harness error: the
  officer stops before the result is sent; a clean episode is scored.
"""

from __future__ import annotations

import dataclasses
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.families import FAMILIES, SITUATIONS_V2, generate
from eval.headless.situations import Calibration
from eval.headless.v2 import leakage as LK
from eval.headless.v2.mission import MISSION

from .fake_anthropic import FakeAnthropic, LookScript, ReadAll

sys.path.insert(0, str(REPO / "agents"))
import watch_officer as WO  # noqa: E402

#: a fake client's spending limits: nothing is billed, the cost guard must not stop a reader
UNCAPPED = {"max_usd": 1000.0, "batch_max_usd": 1000.0}


@pytest.fixture(scope="module", autouse=True)
def binary() -> None:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)


class Capture:
    """A fake model's script that keeps what it was sent: the system prompt and the tools."""

    def __init__(self, script: Any):
        self.script = script
        self.system: list[str] = []
        self.tools: list[Any] = []

    def __call__(self, req: dict[str, Any]) -> Any:
        self.system.append("".join(b["text"] for b in req["system"]))
        self.tools.append(req["tools"])
        return self.script(req)


def run_v2(family: str, seed: int, script: Any, edit: Any = None,
           clock: str = "paused") -> tuple[dict[str, Any], FakeAnthropic, Any]:
    with tempfile.TemporaryDirectory(prefix="leak-") as td:
        e, contacts = generate(family, seed, Path(td), world="v2")
        if edit is not None:
            edit(e)
        client = FakeAnthropic(script)
        opts = Options(agent="model", model_client=client, model=UNCAPPED, calibration=Calibration.load(), scoring="v2",
                       clock=clock)
        return Episode(e, seed, Path(td), opts, contacts_path=contacts).run(), client, e


# ---- the tokens ----------------------------------------------------------------------------------

CTX = LK.Context(family="decoy", situation_seed=1000, seed=123456789, name="v2-decoy-1000")


def hits(text: str, ctx: LK.Context = CTX, structured: bool = False) -> list[tuple[str, str]]:
    s = LK.Scanner(ctx)
    s.scan(text, "test", structured=structured)
    return sorted({(h["kind"], h["token"]) for h in s.hits})


def test_every_family_name_is_a_token_and_the_list_is_the_suites():
    fams = LK.families()
    assert set(FAMILIES) <= set(fams) and set(SITUATIONS_V2) <= set(fams)
    assert set(fams) == {"fast-unknown", "decoy", "two-at-once", "link-cut-mid-look", "stuck-vehicle", "low-battery",
                         "approval-flood", "quiet", "conflict", "harbor-watch"}
    for f in fams:
        if "-" in f:
            assert hits(f"a contact named {f.upper()} closing")[0][0] == "family", f
            assert hits(f"x {f.replace('-', '_')} y"), f
        assert ("family", "v2-<family>") in hits(f"situation v2-{f}"), f
        assert ("family", "<family>-<number>") in hits(f"see {f}-1042 now"), f
    assert hits("Harbor  Watch tonight") == [("family", "harbor watch")]


def test_run_names_and_seeds_in_names_are_tokens():
    assert ("run", "headless-<situation>-s<seed>") in hits("run headless-harbor-watch-s7")
    assert ("run", "<name>-s<seed>[-r<r>]") in hits("job decoy-model-s1000-r0 done")
    assert ("run", "<name>-s<seed>[-r<r>]") in hits("hw-model-s1")
    assert ("run", "v2-decoy-1000") in hits("from v2-decoy-1000")
    assert ("seed", "s1000") in hits("episode_x-s1000")
    assert ("seed", "seed1000") in hits("drawn with seed 1000") and ("seed", "seed1000") in hits("seed=1000")
    assert ("seed", "seed123456789") in hits("SEED:123456789")
    # the opaque run id, a bare number, a seed the bridge serves as a value: not names
    assert hits("bridge run ep-0123456789ab at 1000 m, 123456789 ms") == []
    assert hits(json.dumps({"seed": 1000, "run": "ep-0123456789ab", "alt_m": 1000}), structured=True) == []
    # callsigns, track ids, contact ids are not run names
    assert hits("PUMA 1 on T-0008; fast-1 and craft-1; UUV-S1") == []


def test_field_names_are_tokens_and_ordinary_words_are_not():
    for f in LK.FIELDS:
        assert ("field", f) in hits(f"x {f} y"), f
        assert ("field", f) in hits(json.dumps({f: 1}), structured=True), f
    assert ("field", "pre_served") in hits("pre_served_share 0.4")
    assert ("field", "look_by") in hits('{"look_by_s": 120}')
    # `eligible` and `contributed` are words: tokens only as a field name is written
    for w in LK.WORD_FIELDS:
        assert ("field", w) in hits(f'{{"{w}": true}}'), w
        assert ("field", w) in hits(json.dumps({w: True}), structured=True), w
        assert ("field", w) in hits(f"n_{w}=3") and ("field", w) in hits(f"{w}_needs") and ("field", w) in hits(f"{w}=1")
        assert hits(f"the {w} operators; it {w} nothing; {w.upper()}") == [], w
    # the bridge's own key where it serves it: the operators who may sign an engagement
    assert hits(json.dumps({"engage": {"eligible": ["op-2", "op-3"], "signers": []}}), structured=True) == []
    assert ("field", "eligible") in hits(json.dumps({"labels": {"eligible": True}}), structured=True)
    # the one-word family names and the label words are ordinary words
    prose = ("A decoy in quiet water shows an identity conflict: useful, useless, redundant, harmful, refused, "
             "served, a follow-up, a fault response, low battery, a fast unknown, two at once, stuck vehicle.")
    assert hits(prose) == []


def test_no_field_name_is_used_by_the_bridge_its_tools_or_the_watch_prompt():
    """The scan cannot fail by construction: none of the scorer's identifiers occurs in the
    bridge's code, the headless binary's, the watch officer's prompt or its code (crates/,
    agents/watch, agents/watch_officer.py); `eligible` only as the engagement's key
    (`BRIDGE_KEYS`)."""
    files = [p for d in ("crates", "agents/watch") for p in (REPO / d).rglob("*")
             if p.is_file() and p.suffix in (".rs", ".py", ".md", ".toml")] + [REPO / "agents" / "watch_officer.py"]
    text = "\n".join(p.read_text(errors="replace") for p in files)
    for f in LK.FIELDS:
        assert not re.search(r"(?<![A-Za-z0-9_])" + re.escape(f) + r"(?![A-Za-z0-9_])", text), f
    assert not re.search(r"(?<![A-Za-z0-9_])contributed(?![A-Za-z0-9_])", text)
    uses = re.findall(r'"eligible"', "\n".join(p.read_text() for p in (REPO / "crates" / "murmur-bridge" / "src").rglob("*.rs")))
    assert uses, "the bridge's engagement view serves `eligible` (BRIDGE_KEYS)"


# ---- same tasking (4.3) and the real inputs --------------------------------------------------------


def test_the_brief_is_the_mission_and_the_prompt_nothing_else(tmp_path: Path):
    """Every v2 situation's brief is the mission verbatim; the system prompt is the watch prompt
    (agents/watch/prompt.md) with that brief and nothing more: the same for every situation."""
    head = (REPO / "agents" / "watch" / "prompt.md").read_text().rstrip() + "\n\n## Situation brief\n\n"
    for family in SITUATIONS_V2:
        for seed in (1000, 1001):
            e, _ = generate(family, seed, tmp_path, world="v2")
            assert e.brief == MISSION, family
            assert WO.build_prompt(e.brief) == head + MISSION + "\n"


def test_the_real_brief_prompt_and_tool_definitions_pass_the_scan():
    from eval.headless.officer import tool_defs
    from eval.headless.sim import Sim
    from eval.headless.situations import process_spec
    from types import SimpleNamespace

    assert LK.scan_texts({"brief": MISSION, "system_prompt": WO.build_prompt(MISSION)}, CTX) == []
    with tempfile.TemporaryDirectory() as td:
        e, contacts = generate("two-at-once", 1000, Path(td), world="v2")
        spec, env = process_spec(e, 1000, Path(td), Calibration.load(), contacts_path=contacts)
        sim = Sim(spec, env, log=Path(td) / "sim.log")
        try:
            defs = sim.tool_defs()
        finally:
            sim.end()
    tools = tool_defs([SimpleNamespace(name=d["name"], description=d.get("description"),
                                       input_schema=d.get("inputSchema")) for d in defs])
    assert len(tools) >= 18
    ctx = LK.Context(family="two-at-once", situation_seed=1000, seed=1000, name=e.name)
    assert LK.scan_texts({f"tool_definition:{t['name']}": json.dumps(t) for t in tools}, ctx) == []


@pytest.fixture(scope="module")
def readers() -> dict[str, tuple[dict[str, Any], Capture]]:
    out = {}
    for family in SITUATIONS_V2:
        cap = Capture(ReadAll())
        rec, _, _ = run_v2(family, 1000, cap)
        out[family] = (rec, cap)
    return out


def test_a_model_reading_every_tool_on_every_v2_situation_receives_no_token(readers):
    for family, (rec, cap) in readers.items():
        lk = rec["v2"]["leakage"]
        assert rec["episode_class"] == "scored", (family, lk["hits"][:3])
        assert lk["n_hits"] == 0 and lk["hits"] == []
        assert lk["context"] == {"family": family, "situation_seed": 1000, "seed": 1000, "name": f"v2-{family}-1000"}
        assert set(lk["by_source"]) == {"system_prompt", "tool_definition", "update", "tool_result"}
        assert lk["by_source"]["tool_definition"] == len(cap.tools[0])
        # every tool result the model got was scanned: one per tool call
        calls = rec["model"]["totals"]["tool_calls"]
        assert lk["by_source"]["tool_result"] == calls and calls >= 11 * rec["model"]["totals"]["wakes"]
        assert lk["inputs"] == sum(lk["by_source"].values()) and lk["chars"] > 100_000
        assert rec["model"]["totals"]["stopped"] is None


def test_every_situation_gets_the_same_prompt_and_tools(readers):
    """Same tasking, as sent: every request of every situation carries the one system prompt
    (the watch prompt and the mission) and the one list of tool definitions."""
    systems = {s for _, cap in readers.values() for s in cap.system}
    tools = {json.dumps(t, sort_keys=True) for _, cap in readers.values() for t in cap.tools}
    assert systems == {WO.build_prompt(MISSION)}
    assert len(tools) == 1


@pytest.mark.parametrize("clock", ["paused", "charged"])
def test_a_family_name_in_a_tool_result_is_a_harness_error(clock: str):
    """An opening order whose note names a family (a leak through the bridge: the model reads
    the rationale with get_intents): the scan flags the result, the officer stops before it is
    sent, and the driver classes the episode `harness_error`; on either clock."""

    def name_it(e: Any) -> None:
        e.orders = [dataclasses.replace(o, note=o.note + " (two-at-once drill)") if i == 0 else o
                    for i, o in enumerate(e.orders)]

    rec, client, _ = run_v2("decoy", 1000, ReadAll(), edit=name_it, clock=clock)
    assert rec["episode_class"] == "harness_error"
    lk = rec["v2"]["leakage"]
    (hit,) = lk["hits"]
    assert hit["source"] == "tool_result:get_intents" and hit["token"] == "two-at-once" and hit["kind"] == "family"
    assert hit["at"].endswith(".rationale") and hit["wake"] == 1
    # one request (the update) went out; the one that would carry the result never did
    assert len(client.requests) == 1
    (w,) = rec["model"]["wakes"]
    assert w["stop"] == "leakage" and len(w["api_calls"]) == 1 and w["error"] == "not sent: leakage"
    assert rec["model"]["totals"]["stopped"] == "leakage"
    assert any(ln["kind"] == "leakage" and ln["first"]["token"] == "two-at-once" for ln in rec["agent_log"])
    assert rec["budget_stopped"] is False and rec["model_failed"] is False


def test_a_leak_in_an_update_sends_nothing(monkeypatch):
    """An update that names the situation is scanned before the wake's first call: that call is
    never made."""
    import eval.headless.officer as OF

    real = OF.compose
    monkeypatch.setattr(OF, "compose", lambda *a, **k: real(*a, **k) + "\n\n(situation stuck-vehicle)")
    rec, client, _ = run_v2("quiet", 1000, LookScript())
    assert rec["episode_class"] == "harness_error" and client.requests == []
    (w,) = rec["model"]["wakes"]
    assert w["stop"] == "leakage" and w["api_calls"] == [] and w["tool_calls"] == []
    (hit,) = rec["v2"]["leakage"]["hits"]
    assert hit["source"] == "update" and hit["token"] == "stuck-vehicle" and hit["wake"] == 1


def test_a_leak_in_the_system_prompt_sends_nothing():
    """A brief that names its situation (the v1 brief, `# Harbor watch`) is caught before the
    first request: nothing is sent, the episode is a harness error."""

    def v1_brief(e: Any) -> None:
        e.brief = "# Harbor watch\n\n" + MISSION

    rec, client, _ = run_v2("quiet", 1000, LookScript(), edit=v1_brief)
    assert rec["episode_class"] == "harness_error" and client.requests == []
    assert rec["v2"]["leakage"]["hits"][0]["source"] == "system_prompt"
    assert rec["model"]["wakes"] == [] and rec["model"]["totals"]["stopped"] == "leakage"


def test_a_clean_episode_is_scored_and_the_models_own_words_are_diagnostics_only():
    """The model writes a family name and a field name in its own text and its proposal's
    rationale: recorded as diagnostics; the rationale coming back from the bridge (the
    proposal's answer, the intents) is the model's own words, an echo; the episode stays
    scored."""
    look = LookScript()

    def chatty(req: dict[str, Any]) -> Any:
        content, stop = look(req)
        for b in content:
            if getattr(b, "type", None) == "text":
                b.text += " (this looks like a fast-unknown drill; t_need soon)"
            if getattr(b, "type", None) == "tool_use" and b.name == "propose_intent":
                b.input = {**b.input, "rationale": b.input["rationale"] + " v2-decoy?"}
        return content, stop

    rec, _, _ = run_v2("fast-unknown", 1000, chatty)
    lk = rec["v2"]["leakage"]
    assert rec["episode_class"] == "scored" and lk["n_hits"] == 0
    tokens = {(d["source"].split(":")[0], d["token"]) for d in lk["diagnostic"]}
    assert ("model_text", "fast-unknown") in tokens and ("model_text", "t_need") in tokens
    assert ("model_tool_input", "v2-<family>") in tokens
    echoed = {(e["source"], e["match"]) for e in lk["echoes"]}
    assert ("tool_result:propose_intent", "v2-decoy") in echoed and lk["n_echoes"] >= 1
    assert rec["model"]["totals"]["proposals"] == 1 and rec["model"]["totals"]["stopped"] is None


def test_an_echo_is_only_what_the_model_wrote_first():
    s = LK.Scanner(CTX)
    s.scan("nothing yet", "update")
    s.said("I think this is the v2-Quiet one", "model_text")
    assert s.scan('{"rationale": "I think this is the v2-quiet one"}', "tool_result:x", structured=True) == []
    assert s.n_echoes == 1 and s.echoes[0]["match"] == "v2-quiet" and not s.leaked
    # another token, never written by the model: a hit
    assert s.scan("from v2-decoy", "update") and s.leaked and s.n_hits == 1


def test_the_driver_classes_a_leak_before_a_budget_stop():
    """Section 7: a leakage hit is a harness error, whatever else stopped the officer."""
    from types import SimpleNamespace

    from eval.headless.v2.record import PictureEncoder

    def part(leaked: bool, stopped: bool) -> str:
        ep = Episode.__new__(Episode)
        ep.opts = Options(agent="model", scoring="v2")
        scan = LK.Scanner(CTX)
        if leaked:
            scan.scan("v2-quiet", "update")
        ep.agent = SimpleNamespace(budget_stopped=stopped, model_failed=False, scan=scan)
        ep.operator = None
        ep.e = SimpleNamespace(policy=None)
        ep.picture_enc = PictureEncoder(0.0)
        ep.sim_epoch = 0.0
        out = ep.v2_part("")
        assert out["v2"]["leakage"]["n_hits"] == int(leaked)
        return out["episode_class"]

    assert part(True, True) == "harness_error" and part(True, False) == "harness_error"
    assert part(False, True) == "budget_stop" and part(False, False) == "scored"
