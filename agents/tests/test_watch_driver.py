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


COMPACT = {"t": 0.0, "vehicles": [{
    "uid": 4, "callsign": "HAWK 4", "platform": "skywatch", "domain": "air", "armed": False, "mode": "LOITER",
    "lon": -122.47, "lat": 37.81, "position_age_s": 1.0, "nav_ready": True, "nav_source": "gnss", "gps_glitch": False,
    "battery_pct": 90.0, "link_lost": False, "link_age_s": 0.2, "task": None, "stale": []}]}
CATALOG = {"platforms": [{"id": "skywatch", "cruise_mps": 25, "min_turn_radius_m": 80.0, "tasks": ["hold", "orbit"],
                          "sensors": [{"type": "eo_ir", "range_m": 4000}]}]}
# the playbook's config for the shadow: a watch area around the Gate
SHADOW_CONFIG = {"watch_area": [{"name": "the Gate", "center": [-122.478, 37.818], "radius_m": 3000}]}
WRITERS = ("preview_intent", "propose_intent", "withdraw_intent")


class FakeSession:
    """The driver's MCP session as the shadow reads it (and every call it makes, recorded)."""

    def __init__(self):
        self.calls: list[str] = []

    async def fleet(self):
        return FLEET

    async def tracks(self, **_):
        return TRACKS

    async def call(self, name, **args):
        self.calls.append(name)
        assert name not in WRITERS, f"the shadow called {name}"
        now = time.time()
        if name == "get_fleet":
            return {**COMPACT, "t": now}
        if name == "get_tracks":
            return {"t": now, "tracks": [{**t, "confidence_now": 0.95, "velocity_mps": [8.0, 1.0], "speed_mps": 8.1,
                                          "predicted": t["position"], "sources": [{"uid": 4, "sensor": "eo_ir", "last_t": now - 2}]}
                                         for t in TRACKS]}
        if name == "get_inbox":
            return {"t": now, "pending": [], "alerts": []}
        if name == "get_changes":
            return {"t": now, "run": "test-run", "cursor": "f:1", "changes": [], "more": False}
        if name == "get_platforms":
            return CATALOG
        if name == "get_policy":
            return {"policy": {"rules": {"reserve_pct": 20}, "approval": {"expire_s": 120}}}
        if name == "get_areas":
            return {"t": now, "areas": []}
        raise AssertionError(f"unexpected read {name}")

    async def propose(self, *a, **k):
        raise AssertionError("the shadow proposed")


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


def officer(tmp_path: Path, fake: Path, script, *extra: str, seconds: float = 6.0, backend: str = "claude"):
    bin_flag = "--claude-bin" if backend == "claude" else "--codex-bin"
    args = W.parse_args(["--backend", backend, bin_flag, str(fake), "--poll-s", "0.1", "--coalesce-s", "0.2",
                         "--min-gap-s", "0.3", "--heartbeat-s", "0", "--turn-timeout-s", "20",
                         "--limit-margin-s", "0.2", "--limit-floor-s", "0.2", "--error-backoff-s", "0.2",
                         "--error-backoff-max-s", "0.8", *extra])
    run_dir = tmp_path / "run"
    run_dir.mkdir(exist_ok=True)
    prompt = run_dir / "system-prompt.md"
    prompt.write_text(W.build_prompt("A quiet morning at the Gate."))
    backend_ = W.make_backend(args, run_dir, prompt, prompt.read_text())
    log = SituationLog(tmp_path / "situation.jsonl", "test", echo=False)
    budgets = Budgets(max_turns_per_wake=args.max_turns_per_wake, max_escalations=args.max_escalations,
                      heartbeat_s=args.heartbeat_s, coalesce_s=args.coalesce_s, min_gap_s=args.min_gap_s)
    shadow = None
    if args.shadow_playbook:
        from playbook.config import make
        from watch.shadow import PlaybookShadow

        shadow = PlaybookShadow(make(SHADOW_CONFIG, "test"), log.write)
    return W.Officer(args, backend_, log, Waker(budgets, escalation=not args.no_escalation), ScriptedPoller(script),
                     shadow, time.time() + seconds)


def lines(tmp_path: Path) -> list[dict]:
    return [json.loads(x) for x in (tmp_path / "situation.jsonl").read_text().splitlines()]


def test_a_watch_wakes_escalates_and_tells_the_routine_model(fake_claude, tmp_path):
    script = [
        (0.8, {"kind": "track.declared", "track": "T-0009", "declared": "hostile", "by": "rule:hostile_act"}),
        (0.8, {"kind": "vehicle.joined", "uid": 9}),
        (2.5, {"kind": "intent.state", "intent": 41, "task": "hold", "uids": [1], "mine": True,
               "from": "pending_approval", "to": "rejected", "reason": "rejected by op-1",
               "operator_note": {"by": "op-1", "note": "not now"}}),
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
    assert "consulted as the senior watch officer" in wakes[1]["update"]
    assert "track T-0009 declared hostile by rule:hostile_act" in wakes[1]["update"]
    assert "uid 9 joined" in wakes[1]["update"], "a note rides along"
    assert "the senior watch officer (claude-opus-5-5) handled update 2" in wakes[2]["update"]
    assert "pending_approval -> rejected" in wakes[2]["update"]
    assert "operator op-1's note on the decision \"not now\"" in wakes[2]["update"]
    assert "#41 hold on [1]: pending_approval" in wakes[0]["update"]
    assert [t["n"] for t in turns] == [1, 2, 3] and all(t["stop"] == "success" for t in turns)
    assert turns[0]["tool_calls"][1]["name"] == "mcp__murmur__propose_intent" and turns[0]["proposed"] == [41]
    assert turns[1]["escalated"] and turns[1]["model"] == "claude-opus-5-5"
    init = next(x for x in log if x["kind"] == "backend.init")
    assert init["mcp_servers"] == [{"name": "murmur", "status": "connected"}]
    shadow = [x for x in log if x["kind"] == "shadow"]
    assert len(shadow) == 1 and shadow[0]["track"] == "T-0009" and shadow[0]["request"]["task"] == "orbit"
    assert shadow[0]["request"]["uids"] == [4], "what it would have proposed, logged only"
    assert shadow[0]["record"]["play"] == "isr.investigate@2" and shadow[0]["record"]["preview"]["shadow"] is True
    assert "Shadow: not previewed, not proposed." in shadow[0]["rationale"]
    calls = o.poller.session.calls
    assert calls and not set(calls) & set(WRITERS), "the shadow reads only"
    assert not any(x["kind"] == "play.status" for x in log), "the shadow's statuses are shadow.status (debounced)"
    assert all(x["source"] == "watch_officer" and x["run"] == "test" for x in log)
    assert o.poller.closed and o.backend.p is None
    # every line reads in the console's Situation panel ({t, kind, text}): a short line for a
    # person; the full update is under `update`, the model's note is a turn's text
    assert all(isinstance(x.get("text"), str) and x["text"].strip() for x in log), log
    assert wakes[1]["text"] == "watch officer woken (opus, escalated: a track was declared): a track was declared"
    assert turns[0]["text"] == "Proposed #41."
    assert all(len(x["text"]) <= 400 for x in log if x["kind"] != "turn")


def test_a_limit_with_an_early_reset_waits_resumes_and_tells_the_model_again(fake_claude, tmp_path, monkeypatch):
    """The limited wake's changes are told again after the resume (finding 18: the old test
    passed whether or not they were)."""
    monkeypatch.setenv("FAKE_SCENARIO", "limit_once")
    monkeypatch.setenv("FAKE_LIMIT_AT", "2")  # the start goes through, the next wake is limited
    monkeypatch.setenv("FAKE_RESET_IN", "1")
    o = officer(tmp_path, fake_claude, [(1.0, {"kind": "review.flag", "flag": "F-1"})], seconds=8.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    hit = next(x for x in log if x["kind"] == "limit.hit")
    assert hit["action"] == "wait" and hit["provider"] == "claude" and hit["limit"] == "session"
    assert next(x for x in log if x["kind"] == "limit.resume")["resume"], "the session resumes"
    wakes = [x for x in log if x["kind"] == "wake"]
    turns = [x for x in log if x["kind"] == "turn"]
    assert [t["stop"] for t in turns] == ["success", "limit", "success"]
    assert "review flag F-1" in wakes[1]["update"], "the limited wake"
    assert "review flag F-1" in wakes[2]["update"], "told again after the resume"


def test_an_error_turn_gives_its_changes_and_escalation_back(fake_claude, tmp_path, monkeypatch):
    """Finding 5: an error turn (overloaded after the CLI's retries) lost the wake's changes and
    spent the escalation."""
    monkeypatch.setenv("FAKE_SCENARIO", "error_once")
    monkeypatch.setenv("FAKE_ERROR_MODEL", "opus")  # the consult errors once
    script = [(0.8, {"kind": "track.declared", "track": "T-0009", "declared": "hostile", "by": "op-1"})]
    o = officer(tmp_path, fake_claude, script, "--max-escalations", "1", seconds=6.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    wakes = [x for x in log if x["kind"] == "wake"]
    turns = [x for x in log if x["kind"] == "turn"]
    assert [w["escalated"] for w in wakes] == [False, True, True], "the escalation came back"
    assert [t["stop"] for t in turns] == ["success", "error", "success"]
    assert all("track T-0009 declared hostile" in w["update"] for w in wakes[1:]), "told again"
    assert o.waker.escalations == 1


def test_a_bridge_restart_restarts_the_cli_and_checks_its_tools_again(fake_claude, tmp_path):
    """Finding 15: a long-lived claude -p kept a dead MCP session after a bridge restart."""
    o = officer(tmp_path, fake_claude, [(1.0, {"kind": "feed.reset", "run": "run-b"})], seconds=5.0)

    async def go():
        task = asyncio.ensure_future(o.run())
        # the poll loop marks the restart when it sees feed.reset
        while not o.bridge_restarted and not task.done():
            await asyncio.sleep(0.05)
        return await task

    assert asyncio.run(go()) == "until"
    log = lines(tmp_path)
    restart = [x for x in log if x["kind"] == "backend.restart"]
    assert len(restart) == 1 and restart[0]["why"] == "the bridge restarted" and restart[0]["resume"]
    assert len([x for x in log if x["kind"] == "backend.init"]) == 2, "the MCP status checked again"
    wakes = [x for x in log if x["kind"] == "wake"]
    assert "the bridge restarted" in wakes[-1]["update"]


def test_a_limit_past_until_stops_the_run(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "limit")
    o = officer(tmp_path, fake_claude, [])
    assert asyncio.run(o.run()) == "limit"
    hit = next(x for x in lines(tmp_path) if x["kind"] == "limit.hit")
    assert hit["action"] == "stop" and hit["why"] == "the reset comes after --until"
    assert hit["reset_at"] > time.time() + 60


def test_a_reset_in_the_past_is_retried_no_sooner_than_a_doubling_floor(fake_claude, tmp_path):
    """Finding 12: a reset already past retried at once, a tight restart, turn, limit loop."""
    from watch.limits import LimitHit

    o = officer(tmp_path, fake_claude, [], "--limit-floor-s", "60", "--limit-margin-s", "0")
    now = time.time()
    past = LimitHit("claude", "session", "You've hit your session limit", now - 30)
    assert [round(o.retry_at(past, now) - now) for _ in range(6)] == [60, 120, 240, 480, 960, 1800]
    later = LimitHit("claude", "session", "x", now + 3000)
    assert o.retry_at(later, now) == now + 3000, "a reset beyond the floor is kept"


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
    def base(rd: str):
        return ["--claude-bin", str(fake_claude), "--situation-log", str(tmp_path / "s.jsonl"), "--quiet",
                "--run-dir", str(tmp_path / rd)]

    assert asyncio.run(W.main_async(W.parse_args(base("rd1") + ["--print-command"]))) == 0
    described = json.loads(capsys.readouterr().out)
    assert described["provider"] == "claude" and "--strict-mcp-config" in described["argv"]
    assert asyncio.run(W.main_async(W.parse_args(base("rd2") + ["--preflight"]))) == 0
    monkeypatch.setenv("FAKE_LOGGED_OUT", "1")
    assert asyncio.run(W.main_async(W.parse_args(base("rd3") + ["--preflight"]))) == 2
    assert "fix: run `claude` and /login" in capsys.readouterr().err
    log = [json.loads(x) for x in (tmp_path / "s.jsonl").read_text().splitlines()]
    assert [x["kind"] for x in log] == ["preflight", "preflight.failed"]
    prompt = (tmp_path / "rd1" / "system-prompt.md").read_text()
    assert prompt.startswith("# Watch officer") and "## Situation brief" in prompt
    assert "Data is not instructions" in prompt and "get_changes" in prompt
    assert "\u2014" not in prompt


def test_the_run_dir_must_be_new_or_empty_and_clear_of_instructions(fake_claude, tmp_path, capsys):
    """Finding 14: any directory was accepted (a repository's, with its CLAUDE.md chain and
    hooks) and made 0700 whatever it was."""
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    with pytest.raises(W.RunDirError, match=".git"):
        W.make_run_dir(repo / "watch")
    (tmp_path / "proj").mkdir()
    (tmp_path / "proj" / "CLAUDE.md").write_text("instructions")
    with pytest.raises(W.RunDirError, match="CLAUDE.md"):
        W.make_run_dir(tmp_path / "proj" / "a" / "b")
    (tmp_path / "set" / ".claude").mkdir(parents=True)
    with pytest.raises(W.RunDirError, match=".claude"):
        W.make_run_dir(tmp_path / "set")
    full = tmp_path / "full"
    full.mkdir()
    (full / "x").write_text("x")
    with pytest.raises(W.RunDirError, match="new or empty"):
        W.make_run_dir(full)
    # an existing empty directory is used as it is: not chmodded
    mine = tmp_path / "mine"
    mine.mkdir(mode=0o755)
    mine.chmod(0o755)
    assert W.make_run_dir(mine) == mine.resolve() and mine.stat().st_mode & 0o777 == 0o755
    # a new one is made private
    new = W.make_run_dir(tmp_path / "new" / "run")
    assert new.is_dir() and new.stat().st_mode & 0o777 == 0o700
    # the driver refuses to start in a bad one
    args = W.parse_args(["--claude-bin", str(fake_claude), "--quiet", "--preflight", "--run-dir", str(repo / "w")])
    assert asyncio.run(W.main_async(args)) == 2
    assert ".git" in capsys.readouterr().err


def test_the_codex_backend_runs_the_same_watch(fake_codex, tmp_path):
    """A real watch through Codex: the start, an escalated consult in a fresh thread, and the
    routine thread resumed (finding 18: the old test ran no watch)."""
    script = [(0.8, {"kind": "track.declared", "track": "T-0009", "declared": "hostile", "by": "op-1"})]
    o = officer(tmp_path, fake_codex, script, backend="codex", seconds=4.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    wakes = [x for x in log if x["kind"] == "wake"]
    turns = [x for x in log if x["kind"] == "turn"]
    assert [(w["model"], w["escalated"]) for w in wakes] == [("gpt-6-luna", False), ("gpt-6.1-sol", True)]
    assert [t["stop"] for t in turns] == ["success", "success"]
    assert turns[0]["text"] == "Proposed #51 (gpt-6-luna)." and turns[1]["text"] == "Proposed #51 (gpt-6.1-sol)."
    assert turns[0]["proposed"] == [51]
    assert turns[1]["session"] != turns[0]["session"], "the consult in a fresh thread"
    assert next(x for x in log if x["kind"] == "backend.init")["thread_id"] == turns[0]["session"]
    with pytest.raises(SystemExit):
        W.parse_args(["--backend", "gemini"])


def test_failed_turns_back_off_doubling_and_the_stop_says_why(fake_claude, tmp_path, monkeypatch):
    """N3: one error stopped the watch's next wakes for nothing and three in ten minutes ended
    the run. Now each failure in a row doubles a wait (0.2, 0.4, 0.8 s here, as 30 s to 15
    min by default); the run stops only on failures that go on after a wait at the ceiling,
    and says so. A failed opening is told again as the opening."""
    monkeypatch.setenv("FAKE_SCENARIO", "error_always")
    o = officer(tmp_path, fake_claude, [], seconds=15.0)
    t0 = time.time()
    assert asyncio.run(o.run()) == "errors"
    assert time.time() - t0 >= 1.4, "it waited 0.2 + 0.4 + 0.8 s"
    log = lines(tmp_path)
    assert [x["wait_s"] for x in log if x["kind"] == "backoff"] == [0.2, 0.4, 0.8]
    assert [x["failures_in_a_row"] for x in log if x["kind"] == "backoff"] == [1, 2, 3]
    wakes = [x for x in log if x["kind"] == "wake"]
    assert [w["reason"] for w in wakes] == ["start"] * 4, "the opening, told again each time"
    assert all("Your watch starts now" in w["update"] for w in wakes)
    err = [x for x in log if x["kind"] == "error"][-1]["error"]
    assert err.startswith("errors: turns kept failing after a 1 s wait (4 in a row)"), err
    assert all(isinstance(x.get("text"), str) and x["text"].strip() for x in log)


def test_a_success_ends_the_back_off(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "error_once")
    o = officer(tmp_path, fake_claude, [(1.0, {"kind": "review.flag", "flag": "F-1"})], seconds=5.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    assert [x["wait_s"] for x in log if x["kind"] == "backoff"] == [0.2]
    assert [t["stop"] for t in log if t["kind"] == "turn"] == ["error", "success", "success"]
    assert o.lanes["routine"].streak == 0 and o.lanes["routine"].wait_s == 0.0


def test_repeated_crashes_stop_the_run_and_say_so(fake_claude, tmp_path, monkeypatch):
    """N3: the stop reason says crashes, not errors; a crashed turn's cost is unknown and said."""
    monkeypatch.setenv("FAKE_SCENARIO", "crash_always")
    o = officer(tmp_path, fake_claude, [], seconds=15.0)
    assert asyncio.run(o.run()) == "crashes"
    log = lines(tmp_path)
    crashed = [x for x in log if x["kind"] == "backend.crashed"]
    assert len(crashed) == 4 and all(c["cost"].startswith("unknown") for c in crashed)
    assert [x["wait_s"] for x in log if x["kind"] == "backoff"] == [0.2, 0.4, 0.8]
    err = [x for x in log if x["kind"] == "error"][-1]["error"]
    assert err == "crashes: the CLI died more than three times in ten minutes"


def test_a_tool_the_cli_refused_is_logged_and_the_watch_goes_on(fake_claude, tmp_path, monkeypatch):
    """N11: the live stream in which Claude Code refused the model a bash tool: the lockdown
    working, logged as lockdown.refused, not a breach."""
    monkeypatch.setenv("FAKE_SCENARIO", "refused")
    o = officer(tmp_path, fake_claude, [], seconds=3.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    refused = [x for x in log if x["kind"] == "lockdown.refused"]
    assert refused and refused[0]["tool"] == "bash" and "the CLI refused it" in refused[0]["text"]
    assert not [x for x in log if x["kind"] == "lockdown.breach"]
    turn = next(x for x in log if x["kind"] == "turn")
    assert turn["stop"] == "success" and turn["context_tokens"] == 9403


def test_a_tool_that_ran_outside_the_server_stops_the_run(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "breach")
    o = officer(tmp_path, fake_claude, [], seconds=10.0)
    assert asyncio.run(o.run()) == "lockdown"
    log = lines(tmp_path)
    turn = next(x for x in log if x["kind"] == "turn")
    assert turn["stop"] == "lockdown" and turn["tool_calls"][0]["name"] == "Bash"
    breach = next(x for x in log if x["kind"] == "lockdown.breach")
    assert "Bash" in breach["error"] and "secrets" in breach["error"]
    assert breach["text"].startswith("lockdown breach, the run stops")


def test_a_change_with_nested_values_wakes_and_a_malformed_one_never_stops_the_watch(fake_claude, tmp_path):
    """N1 end to end: a vehicle.nav change (its from and to are objects) raised TypeError in
    the waker and ended the run; a change that cannot be judged is set aside and logged."""
    nav = {"kind": "vehicle.nav", "uid": 4, "callsign": "HAWK 4", "domain": "air",
           "from": {"ready": True, "source": "gnss"}, "to": {"ready": True, "source": "dead_reckoning"}}
    o = officer(tmp_path, fake_claude, [(1.0, nav)], seconds=4.0)
    scripted = o.poller.poll

    async def poll():  # the feed also hands over something that is not a change at all
        out = await scripted()
        return out + ["not a change"] if out else out

    o.poller.poll = poll
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    wakes = [x for x in log if x["kind"] == "wake"]
    assert [w["reason"] for w in wakes] == ["start", "changes"]
    assert "HAWK 4" in wakes[1]["update"] and "dead_reckoning" in wakes[1]["update"]
    dropped = [x for x in log if x["kind"] == "change.dropped"]
    assert [d["change"] for d in dropped] == ["not a change"] and dropped[0]["error"]
    assert not [x for x in log if x["kind"] == "error"]


def test_the_run_dir_check_resolves_a_symlinked_home_and_temp(tmp_path, monkeypatch):
    """N10: a home reached by a symlink was not recognised, so its ~/.claude counted as a
    project's and every run directory under it was refused; mkdtemp's path is resolved."""
    real = tmp_path / "h" / "real"
    (real / ".claude").mkdir(parents=True)
    (real / "work").mkdir()
    link = tmp_path / "hl"
    link.symlink_to(real)
    monkeypatch.setenv("HOME", str(link))
    assert W.run_dir_problems(link / "work" / "w") == []
    assert W.run_dir_problems(real / "work" / "w") == []
    monkeypatch.setenv("TMPDIR", str(link / "work"))
    import tempfile

    monkeypatch.setattr(tempfile, "tempdir", None)  # read TMPDIR again
    d = W.make_run_dir(None)
    assert d == d.resolve() and d.parent == (real / "work") and d.stat().st_mode & 0o777 == 0o700
    # a project's .claude above the run directory is still refused
    (real / "work" / "proj" / ".claude").mkdir(parents=True)
    assert W.run_dir_problems(link / "work" / "proj" / "w") == [str(real / "work" / "proj" / ".claude")]


def test_a_failing_routine_model_does_not_hold_back_an_escalation(fake_claude, tmp_path, monkeypatch):
    """Third review, item 6: one back-off for both models, so with only the routine model
    failing a hostile declaration waited for its wait (up to 15 minutes by default)."""
    monkeypatch.setenv("FAKE_SCENARIO", "error_always")
    monkeypatch.setenv("FAKE_ERROR_MODEL", "sonnet")
    script = [(1.0, {"kind": "track.declared", "track": "T-0009", "declared": "hostile", "by": "op-1"})]
    o = officer(tmp_path, fake_claude, script, "--error-backoff-s", "6", "--error-backoff-max-s", "60",
                seconds=4.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    wakes = [x for x in log if x["kind"] == "wake"]
    turns = [x for x in log if x["kind"] == "turn"]
    assert [(w["reason"], w["escalated"]) for w in wakes] == [("start", False), ("changes", True)], wakes
    assert [t["stop"] for t in turns] == ["error", "success"]
    assert wakes[1]["t"] - wakes[0]["t"] < 3, "during the routine model's 6 s wait"
    assert [(b["model"], b["wait_s"]) for b in log if b["kind"] == "backoff"] == [("routine", 6.0)]
    assert o.lanes["consult"].streak == 0 and o.lanes["routine"].streak == 1


def test_a_failing_consult_does_not_hold_back_the_routine_model(fake_claude, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "error_always")
    monkeypatch.setenv("FAKE_ERROR_MODEL", "opus")
    script = [(0.8, {"kind": "track.declared", "track": "T-0009", "declared": "hostile", "by": "op-1"})]
    o = officer(tmp_path, fake_claude, script, "--error-backoff-s", "6", "--error-backoff-max-s", "60",
                seconds=4.0)
    assert asyncio.run(o.run()) == "until"
    log = lines(tmp_path)
    wakes = [x for x in log if x["kind"] == "wake"]
    turns = [x for x in log if x["kind"] == "turn"]
    assert [(w["model"], w["escalated"]) for w in wakes] == [("sonnet", False), ("opus", True), ("sonnet", False)]
    assert [t["stop"] for t in turns] == ["success", "error", "success"]
    assert wakes[2]["t"] - wakes[1]["t"] < 3, "the routine model takes it during the consult's wait"
    assert "track T-0009 declared hostile" in wakes[2]["update"]
    assert o.waker.escalations == 0, "the failed escalation was given back"


def test_a_limit_resume_starts_the_failure_streak_again(fake_claude, tmp_path):
    from watch.limits import LimitHit
    from watch.wake import Wake

    o = officer(tmp_path, fake_claude, [], "--limit-floor-s", "0.1", "--limit-margin-s", "0.1", seconds=30.0)
    o.lanes["routine"].streak, o.lanes["routine"].wait_s = 3, 0.8
    o.lanes["consult"].streak = 2

    async def go():
        await o.backend.start()
        hit = LimitHit("claude", "session", "You've hit your session limit", time.time() + 0.2)
        out = await o.on_limit(hit, False, Wake(1, "changes", [], [], False, [], time.time()))
        await o.backend.stop()
        return out

    assert asyncio.run(go()) is None
    assert o.lanes["routine"].streak == 0 and o.lanes["routine"].wait_s == 0.0
    assert o.lanes["consult"].streak == 2, "the consult's own streak is its own"
