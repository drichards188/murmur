"""The driver end to end on a stand-in CLI and a scripted picture (no bridge, no model): wakes,
escalation, the consult note, limits, a stop signal, the shadow, and the situation log."""

from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path

import pytest

import watch_officer as W
from watch.situation import SituationLog
from watch.wake import Budgets, Waker

FLEET = {"vehicles": [{
    "uid": 4, "callsign": "HAWK 4", "nav": {"ready": True, "stale": False}, "link": {"lost": False},
    "platform": {"id": "skywatch", "armed": False, "domain": "air", "tasks": ["orbit", "hold"],
                 "min_turn_radius_m": 80.0},
    "position": {"lon": -122.47, "lat": 37.81, "age_s": 1.0, "stale": False}}]}
TRACKS = [{"id": "T-0009", "affiliation": "suspect", "affiliation_confidence": 0.7, "affiliation_reason": "dark",
           "position": [-122.479, 37.817], "identity": {"kind": "small_fast_boat"}, "age_s": 2.0, "stale": False}]


class FakeSession:
    async def fleet(self):
        return FLEET

    async def tracks(self, **_):
        return TRACKS


class ScriptedPoller:
    """Changes at given seconds after the start, as `get_changes` would give them."""

    def __init__(self, script: list[tuple[float, dict]]):
        self.script = sorted(script, key=lambda s: s[0])
        self.t0 = 0.0
        self.cursor = 0
        self.session = FakeSession()
        self.closed = False

    async def open(self):
        self.t0 = time.time()
        return {"cursor": 0, "run": "test-run", "tools": ["get_changes"], "reset": False}

    async def poll(self):
        now = time.time() - self.t0
        due = [c for at, c in self.script if at <= now]
        self.script = [(at, c) for at, c in self.script if at > now]
        self.cursor += len(due)
        return [{**c, "t": time.time()} for c in due]

    async def nominations(self):
        return []

    async def mine(self):
        return [{"id": 41, "kind": "hold", "uids": [1], "state": "pending_approval", "expires_in_s": 90.0,
                 "history": [{"t": time.time() * 1000, "reason": "waits"}]}]

    async def close(self):
        self.closed = True


def officer(tmp_path: Path, fake: Path, script, *extra: str, seconds: float = 6.0):
    args = W.parse_args(["--claude-bin", str(fake), "--poll-s", "0.1", "--coalesce-s", "0.2", "--min-gap-s", "0.3",
                         "--heartbeat-s", "0", "--turn-timeout-s", "20", "--limit-margin-s", "0.2", *extra])
    run_dir = tmp_path / "run"
    run_dir.mkdir(exist_ok=True)
    prompt = run_dir / "system-prompt.md"
    prompt.write_text(W.build_prompt("A quiet morning at the Gate."))
    backend = W.make_backend(args, run_dir, prompt, prompt.read_text())
    log = SituationLog(tmp_path / "situation.jsonl", "test", echo=False)
    budgets = Budgets(max_turns_per_wake=args.max_turns_per_wake, max_escalations=args.max_escalations,
                      heartbeat_s=args.heartbeat_s, coalesce_s=args.coalesce_s, min_gap_s=args.min_gap_s)
    shadow = None
    if args.shadow_playbook:
        from watch.shadow import PolicyShadow

        shadow = PolicyShadow()
    return W.Officer(args, backend, log, Waker(budgets, escalation=not args.no_escalation), ScriptedPoller(script),
                     shadow, time.time() + seconds)


def lines(tmp_path: Path) -> list[dict]:
    return [json.loads(x) for x in (tmp_path / "situation.jsonl").read_text().splitlines()]


def test_a_watch_wakes_escalates_and_tells_the_routine_model(fake_claude, tmp_path):
    script = [
        (0.8, {"kind": "track.declared", "track": "T-0009", "declared": "hostile", "by": "rule:hostile_act"}),
        (0.8, {"kind": "vehicle.joined", "uid": 9}),
        (2.5, {"kind": "intent.state", "intent": 41, "task": "hold", "uids": [1], "mine": True,
               "from": "pending_approval", "to": "rejected", "reason": "operator: not now"}),
    ]
    o = officer(tmp_path, fake_claude, script, "--shadow-playbook")
    reason = asyncio.run(o.run())
    assert reason == "until"
    log = lines(tmp_path)
    kinds = [x["kind"] for x in log]
    assert kinds[:2] == ["bridge.connected", "backend.start"]
    wakes = [x for x in log if x["kind"] == "wake"]
    turns = [x for x in log if x["kind"] == "turn"]
    assert [w["reason"] for w in wakes] == ["start", "changes", "changes"]
    assert [w["escalated"] for w in wakes] == [False, True, False]
    assert [w["model"] for w in wakes] == ["sonnet", "opus", "sonnet"]
    assert wakes[1]["escalate_why"] == ["a track was declared"]
    assert "consulted as the senior watch officer" in wakes[1]["text"]
    assert "track T-0009 declared hostile by rule:hostile_act" in wakes[1]["text"]
    assert "uid 9 joined" in wakes[1]["text"], "a note rides along"
    assert "the senior watch officer (claude-opus-5-5) handled update 2" in wakes[2]["text"]
    assert "pending_approval -> rejected (operator: not now)" in wakes[2]["text"]
    assert "#41 hold on [1]: pending_approval" in wakes[0]["text"]
    assert [t["n"] for t in turns] == [1, 2, 3] and all(t["stop"] == "success" for t in turns)
    assert turns[0]["tool_calls"][1]["name"] == "mcp__murmur__propose_intent" and turns[0]["proposed"] == [41]
    assert turns[1]["escalated"] and turns[1]["model"] == "claude-opus-5-5"
    init = next(x for x in log if x["kind"] == "backend.init")
    assert init["mcp_servers"] == [{"name": "murmur", "status": "connected"}]
    shadow = [x for x in log if x["kind"] == "shadow"]
    assert len(shadow) == 1 and shadow[0]["track"] == "T-0009" and shadow[0]["request"]["task"] == "orbit"
    assert shadow[0]["request"]["uids"] == [4], "what it would have proposed, logged only"
    assert all(x["source"] == "watch_officer" and x["run"] == "test" for x in log)
    assert o.poller.closed and o.backend.p is None


def test_a_limit_with_an_early_reset_waits_and_resumes(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "limit_once")
    monkeypatch.setenv("FAKE_RESET_IN", "1")
    o = officer(tmp_path, fake_claude, [(2.0, {"kind": "review.flag", "flag": "F-1"})], seconds=7.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    hit = next(x for x in log if x["kind"] == "limit.hit")
    assert hit["action"] == "wait" and hit["kind"] == "limit.hit" and hit["provider"] == "claude"
    assert next(x for x in log if x["kind"] == "limit.resume")["resume"], "the session resumes"
    turns = [x for x in log if x["kind"] == "turn"]
    assert turns[0]["stop"] == "limit" and turns[-1]["stop"] == "success"
    later = [x for x in log if x["kind"] == "wake"][-1]
    assert "review flag F-1" in later["text"] or any("review flag F-1" in w["text"] for w in log if w["kind"] == "wake")


def test_a_limit_past_until_stops_the_run(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "limit")
    o = officer(tmp_path, fake_claude, [])
    assert asyncio.run(o.run()) == "limit"
    hit = next(x for x in lines(tmp_path) if x["kind"] == "limit.hit")
    assert hit["action"] == "stop" and hit["why"] == "the reset comes after --until"
    assert hit["reset_at"] > time.time() + 60


def test_a_stop_signal_ends_a_turn_in_flight(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "slow")
    o = officer(tmp_path, fake_claude, [], seconds=30.0)

    async def go():
        task = asyncio.ensure_future(o.run())
        await asyncio.sleep(1.5)
        o.signal("SIGTERM")
        return await asyncio.wait_for(task, 15)

    t0 = time.time()
    assert asyncio.run(go()) == "signal SIGTERM"
    assert time.time() - t0 < 12
    assert o.backend.p is None


def test_print_command_preflight_and_its_failure(fake_claude, tmp_path, monkeypatch, capsys):
    base = ["--claude-bin", str(fake_claude), "--situation-log", str(tmp_path / "s.jsonl"), "--quiet",
            "--run-dir", str(tmp_path / "rd")]
    assert asyncio.run(W.main_async(W.parse_args(base + ["--print-command"]))) == 0
    described = json.loads(capsys.readouterr().out)
    assert described["provider"] == "claude" and "--strict-mcp-config" in described["argv"]
    assert asyncio.run(W.main_async(W.parse_args(base + ["--preflight"]))) == 0
    monkeypatch.setenv("FAKE_LOGGED_OUT", "1")
    assert asyncio.run(W.main_async(W.parse_args(base + ["--preflight"]))) == 2
    assert "fix: run `claude` and /login" in capsys.readouterr().err
    log = [json.loads(x) for x in (tmp_path / "s.jsonl").read_text().splitlines()]
    assert [x["kind"] for x in log] == ["preflight", "preflight.failed"]
    prompt = (tmp_path / "rd" / "system-prompt.md").read_text()
    assert prompt.startswith("# Watch officer") and "## Situation brief" in prompt
    assert "\u2014" not in prompt


def test_the_codex_backend_runs_the_same_watch(fake_codex, tmp_path):
    args = W.parse_args(["--backend", "codex", "--codex-bin", str(fake_codex)])
    assert W.DEFAULTS["codex"]["model"] == "gpt-6-luna"
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    b = W.make_backend(args, run_dir, run_dir / "p.md", "prompt")
    assert b.cfg.model == "gpt-6-luna" and b.cfg.effort == "high" and b.cfg.strong_model == "gpt-6.1-sol"
    with pytest.raises(SystemExit):
        W.parse_args(["--backend", "gemini"])
