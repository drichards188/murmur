"""The backends against stand-in CLIs: the command lines, the environment, the event streams,
limits, crashes and resume. No model is called."""

from __future__ import annotations

import asyncio
import json
import time

import pytest
from conftest import calls

from watch.backend import FALLBACK_HINT, BackendCrashed, LimitReached, LockdownBreach, PreflightError
from watch.claude import ClaudeBackend, ClaudeConfig
from watch.codex import CodexBackend, CodexConfig


def run(coro):
    return asyncio.run(coro)


def claude(fake, run_dir, key=None, max_turns=12, **cfg) -> ClaudeBackend:
    prompt = run_dir / "system-prompt.md"
    prompt.write_text("You are the watch officer.\n")
    return ClaudeBackend(ClaudeConfig(bin=str(fake), **cfg), run_dir, "http://127.0.0.1:9/mcp", "agent-watch",
                         key, prompt, max_turns=max_turns)


def codex(fake, run_dir, key=None, **cfg) -> CodexBackend:
    return CodexBackend(CodexConfig(bin=str(fake), **cfg), run_dir, "http://127.0.0.1:9/mcp", "agent-watch",
                        key, "You are the watch officer.\nSay \"no action\" when nothing needs doing.")


# ---- Claude Code --------------------------------------------------------------------------

def test_claude_preflight_and_its_refusals(fake_claude, run_dir, monkeypatch, tmp_path):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-must-not-reach-the-cli")
    b = claude(fake_claude, run_dir)
    info = run(b.preflight())
    assert info["auth_method"] == "claude.ai" and info["api_key_env_removed"] == ["ANTHROPIC_API_KEY"]
    assert not any(c["anthropic_api_key"] for c in calls(tmp_path)), "the key never reaches the CLI"
    monkeypatch.setenv("FAKE_AUTH_METHOD", "api_key")
    with pytest.raises(PreflightError, match="not with a Claude subscription"):
        run(claude(fake_claude, run_dir).preflight())
    assert run(claude(fake_claude, run_dir, allow_other_auth=True).preflight())["auth_method"] == "api_key"
    monkeypatch.delenv("FAKE_AUTH_METHOD")
    monkeypatch.setenv("FAKE_LOGGED_OUT", "1")
    with pytest.raises(PreflightError) as e:
        run(claude(fake_claude, run_dir).preflight())
    assert "/login" in e.value.fix and e.value.fix.endswith(FALLBACK_HINT)
    assert "agents/policy_agent.py" in FALLBACK_HINT
    monkeypatch.delenv("FAKE_LOGGED_OUT")
    monkeypatch.setenv("FAKE_VERSION", "2.0.9 (Claude Code)")
    with pytest.raises(PreflightError, match="older than 2.1.294"):
        run(claude(fake_claude, run_dir).preflight())
    with pytest.raises(PreflightError, match="not installed"):
        run(claude(tmp_path / "no-such-claude", run_dir).preflight())


def test_claude_command_line_is_locked_down(fake_claude, run_dir):
    a = claude(fake_claude, run_dir, max_turns=7, max_budget_usd=1.5).argv("sonnet", "medium")
    joined = " ".join(a)
    for flag in ("-p", "--strict-mcp-config", "--replay-user-messages"):
        assert flag in a
    pairs = {a[i]: a[i + 1] for i in range(len(a) - 1) if a[i].startswith("--")}
    assert pairs["--tools"] == ""
    assert pairs["--allowedTools"] == "mcp__murmur__*"
    assert pairs["--disallowedTools"] == "Agent"
    assert pairs["--permission-mode"] == "dontAsk" and pairs["--permission-prompts"] == "none"
    assert pairs["--setting-sources"] == "project"
    assert pairs["--input-format"] == pairs["--output-format"] == "stream-json"
    assert pairs["--model"] == "sonnet" and pairs["--effort"] == "medium"
    assert pairs["--max-turns"] == "7" and pairs["--max-budget-usd"] == "1.50"
    assert "--bare" not in a and "bypassPermissions" not in joined and "--dangerously" not in joined
    assert "--safe-mode" not in a, "it drops the --mcp-config server"


def test_claude_turn_reads_tool_calls_usage_and_session(fake_claude, run_dir, tmp_path, monkeypatch):
    monkeypatch.setenv("ANTHROPIC_AUTH_TOKEN", "must-not-reach-the-cli")

    async def go():
        b = claude(fake_claude, run_dir, key="mma_secret_key")
        await b.start()
        t1 = await b.turn("Watch update 1.", time.time() + 30)
        t2 = await b.turn("Watch update 2.", time.time() + 30)
        await b.stop()
        return b, t1, t2

    b, t1, t2 = run(go())
    assert t1.stop == "success" and t1.text == "Proposed #41."
    assert [c.name for c in t1.tool_calls] == ["mcp__murmur__get_fleet", "mcp__murmur__propose_intent"]
    assert t1.tool_calls[0].result == '{"count": 2}' and t1.tool_calls[1].is_error is False
    assert t1.proposals() == [41] and t2.proposals() == [42]
    assert t1.model == "claude-sonnet-5-5" and t1.usage["input_tokens"] == 10
    assert t2.cost_usd == pytest.approx(0.02) and b.session_cost_usd == pytest.approx(0.02)
    assert b.init["mcp_servers"] == [{"name": "murmur", "status": "connected"}]
    assert json.loads((run_dir / "session.json").read_text())["session_id"] == t1.session == t2.session
    # one process for both turns, run in the run directory, without the API-key variables
    procs = [c for c in calls(tmp_path) if "-p" in c["argv"]]
    assert len(procs) == 1
    p = procs[0]
    assert p["cwd"] == str(run_dir) and not p["anthropic_api_key"] and not p["anthropic_auth_token"]
    assert p["tool_search"] == "false" and p["max_retries"] == "2"
    # the agent key reaches the CLI in its environment only, never the config file
    assert p["agent_key"] == "mma_secret_key"
    cfg = (run_dir / "mcp.json").read_text()
    assert "mma_secret_key" not in cfg and "${MURMUR_AGENT_KEY}" in cfg
    assert json.loads(cfg)["mcpServers"]["murmur"]["headers"]["X-Murmur-Agent"] == "agent-watch"


def one_claude_turn(fake, run_dir):
    async def go():
        b = claude(fake, run_dir)
        await b.start()
        try:
            return await b.turn("Watch update 1.", time.time() + 30)
        finally:
            await b.stop()

    return run(go())


def test_claude_a_tool_the_cli_refused_is_the_lockdown_working(fake_claude, run_dir, monkeypatch):
    """N11: the stream recorded live (2.1.294, fixtures/claude_bash_refused.jsonl) in which the
    model asked for bash and Claude Code refused it: recorded as refused, not a breach, and the
    turn goes on."""
    monkeypatch.setenv("FAKE_SCENARIO", "refused")
    t = one_claude_turn(fake_claude, run_dir)
    assert t.stop == "success" and t.proposals() == [41]
    assert [r["name"] for r in t.refused] == ["bash"]
    assert "grep -o" in t.refused[0]["input"]
    assert [c.name for c in t.tool_calls] == ["bash", "mcp__murmur__get_fleet", "mcp__murmur__propose_intent"]
    # the largest context one call read: input, cache read and cache write tokens
    assert t.context_tokens == 3 + 9000 + 400
    assert t.as_dict()["context_tokens"] == 9403 and t.as_dict()["refused"] == t.refused


def test_claude_a_tool_outside_the_server_that_ran_is_a_breach(fake_claude, run_dir, monkeypatch):
    """N11: a tool outside the Murmur server whose result is not a refusal ran: the turn is
    interrupted and LockdownBreach carries the turn and what the tool returned."""
    monkeypatch.setenv("FAKE_SCENARIO", "breach")
    with pytest.raises(LockdownBreach) as e:
        one_claude_turn(fake_claude, run_dir)
    assert "Bash" in str(e.value) and "secrets" in str(e.value)
    assert e.value.turn is not None and e.value.turn.stop == "lockdown"
    assert e.value.turn.refused == []


def test_claude_limit_is_reported_with_the_exact_reset(fake_claude, run_dir, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "limit")

    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        try:
            await b.turn("Watch update 1.", time.time() + 30)
        finally:
            await b.stop()

    with pytest.raises(LimitReached) as e:
        run(go())
    hit = e.value.hit
    assert hit.kind == "session" and "hit your session limit" in hit.text
    assert abs(hit.reset_at - (time.time() + 90)) < 10, "the stream's resetsAt, not the text's 3:45pm"
    assert e.value.turn.stop == "limit"


def test_claude_an_error_turn_whose_prose_mentions_a_limit_is_not_a_limit(fake_claude, run_dir, monkeypatch):
    """Finding 12: the model's own text was read for limit messages, so an error turn that
    said "limit reached" slept the watch."""
    monkeypatch.setenv("FAKE_SCENARIO", "error_prose")

    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        t = await b.turn("Watch update 1.", time.time() + 30)
        await b.stop()
        return t

    t = run(go())
    assert t.stop == "error" and "limit reached" in t.text


def test_claude_an_overloaded_turn_is_an_error_and_the_process_lives(fake_claude, run_dir, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "error_once")

    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        t1 = await b.turn("Watch update 1.", time.time() + 30)
        t2 = await b.turn("Watch update 1 again.", time.time() + 30)
        await b.stop()
        return t1, t2

    t1, t2 = run(go())
    assert (t1.stop, t1.error) == ("error", "API Error: 529 Overloaded") and t2.stop == "success"


def test_claude_crash_then_resume(fake_claude, run_dir, tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "crash_once")

    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        with pytest.raises(BackendCrashed) as e:
            await b.turn("Watch update 1.", time.time() + 30)
        assert e.value.code == 3 and "crashed" in e.value.stderr
        announced = b.session_id  # its system/init came before the crash
        info = await b.restart()
        t = await b.turn("Watch update 1 again.", time.time() + 30)
        await b.stop()
        return announced, info, t

    announced, info, t = run(go())
    assert announced and info["resume"] == announced
    assert t.stop == "success" and t.session == announced
    resumed = [c["argv"] for c in calls(tmp_path) if "-p" in c["argv"]][-1]
    assert resumed[resumed.index("--resume") + 1] == announced


def test_claude_spend_is_carried_across_a_resume(fake_claude, run_dir, tmp_path):
    """`total_cost_usd` counts per process (seen live after a resume): the backend adds the
    earlier processes' spend, and a resumed process gets only what is left of the budget."""
    async def go():
        b = claude(fake_claude, run_dir, max_budget_usd=1.0)
        await b.start()
        await b.turn("Watch update 1.", time.time() + 30)
        await b.restart()
        await b.turn("Watch update 2.", time.time() + 30)
        await b.stop()
        return b

    b = run(go())
    assert b.session_cost_usd == pytest.approx(0.02)
    argvs = [c["argv"] for c in calls(tmp_path) if "-p" in c["argv"]]
    assert [a[a.index("--max-budget-usd") + 1] for a in argvs] == ["1.00", "0.99"]


def test_claude_turn_past_its_deadline_is_interrupted_and_the_process_kept(fake_claude, run_dir, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "slow")

    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        t = await b.turn("Watch update 1.", time.time() + 1.0)
        alive = b.p.alive
        await b.stop()
        return t, alive

    t, alive = run(go())
    assert t.stop == "timeout" and [c.name for c in t.tool_calls] == ["mcp__murmur__get_fleet"]
    assert alive, "the interrupt ends the turn, not the process"


def test_claude_max_turns_ends_the_turn(fake_claude, run_dir, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "max_turns")

    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        t = await b.turn("Watch update 1.", time.time() + 30)
        await b.stop()
        return t

    assert run(go()).stop == "max_turns"


def test_claude_consult_runs_the_stronger_model_in_its_own_process(fake_claude, run_dir, tmp_path):
    async def go():
        b = claude(fake_claude, run_dir)
        await b.start()
        t = await b.escalate("You are consulted.", time.time() + 30)
        await b.stop()
        return b, t

    b, t = run(go())
    assert t.model == "claude-opus-5-5" and t.stop == "success"
    consult = [c["argv"] for c in calls(tmp_path) if "--no-session-persistence" in c["argv"]]
    assert len(consult) == 1
    a = consult[0]
    assert a[a.index("--model") + 1] == "opus" and a[a.index("--effort") + 1] == "high"
    assert "--resume" not in a and b.session_id is None, "the consult never touches the routine session"


# ---- Codex --------------------------------------------------------------------------------

def test_codex_preflight_refuses_an_old_cli_and_a_missing_login(fake_codex, run_dir, monkeypatch, tmp_path):
    monkeypatch.setenv("CODEX_API_KEY", "sk-must-not-reach-the-cli")
    monkeypatch.setenv("OPENAI_API_KEY", "sk-must-not-reach-the-cli")
    info = run(codex(fake_codex, run_dir, home=str(tmp_path / "codex-home")).preflight())
    assert info["login"] == "Logged in using ChatGPT"
    assert sorted(info["api_key_env_removed"]) == ["CODEX_API_KEY", "OPENAI_API_KEY"]
    c = calls(tmp_path)[-1]
    assert not c["codex_api_key"] and not c["openai_api_key"]
    assert c["codex_home"] == str(tmp_path / "codex-home")
    monkeypatch.setenv("FAKE_VERSION", "codex-cli 0.116.0")
    with pytest.raises(PreflightError) as e:
        run(codex(fake_codex, run_dir).preflight())
    assert "0.116.0 is older than 0.161.0" in e.value.problem and "npm install -g @openai/codex" in e.value.fix
    monkeypatch.delenv("FAKE_VERSION")
    monkeypatch.setenv("FAKE_LOGGED_OUT", "1")
    with pytest.raises(PreflightError, match="not logged in") as e:
        run(codex(fake_codex, run_dir, home="/tmp/h").preflight())
    assert "CODEX_HOME=/tmp/h codex login" in e.value.fix


def test_codex_command_line_is_locked_down(fake_codex, run_dir):
    b = codex(fake_codex, run_dir, key="mma_k")
    a = b.argv_new("gpt-6-luna", "high")
    cfg = [a[i + 1] for i in range(len(a)) if a[i] == "-c"]
    keys = {c.split("=", 1)[0]: c.split("=", 1)[1] for c in cfg}
    for flag in ("--json", "--skip-git-repo-check", "--ignore-user-config", "--ignore-rules", "--strict-config"):
        assert flag in a
    assert a[a.index("-s") + 1] == "read-only" and a[a.index("-C") + 1] == str(run_dir) and a[-1] == "-"
    assert keys["model_reasoning_effort"] == '"high"'
    assert keys["mcp_servers.murmur.url"] == '"http://127.0.0.1:9/mcp"'
    assert keys["mcp_servers.murmur.http_headers"] == '{"X-Murmur-Agent"="agent-watch"}'
    assert keys["mcp_servers.murmur.required"] == "true"
    assert keys["mcp_servers.murmur.default_tools_approval_mode"] == '"approve"'
    assert keys["features.code_mode.direct_only_tool_namespaces"] == '["mcp__murmur"]'
    assert keys["mcp_servers.murmur.bearer_token_env_var"] == '"MURMUR_AGENT_KEY"'
    for off in ("features.shell_tool", "features.unified_exec", "features.apps", "features.plugins",
                "agents.enabled", "features.multi_agent", "features.browser_use", "features.computer_use",
                "features.memories", "features.hooks", "skills.bundled.enabled"):
        assert keys[off] == "false", off
    assert keys["web_search"] == '"disabled"' and keys["project_doc_max_bytes"] == "0"
    # finding 13: nothing waits for an approval nobody answers; the image and patch tools off
    assert keys["approval_policy"] == '"never"'
    assert keys["tools.view_image"] == keys["include_apply_patch_tool"] == "false"
    assert json.loads(keys["developer_instructions"]).startswith("You are the watch officer.\nSay \"no action\"")
    assert "mma_k" not in " ".join(a), "the key stays in the environment"
    r = b.argv_resume("thread-1", "gpt-6-luna", "high")
    assert r[:4] == [str(fake_codex), "exec", "resume", "thread-1"] and "-C" not in r and "-s" not in r
    assert 'sandbox_mode="read-only"' in r
    with pytest.raises(ValueError, match="ultra"):
        codex(fake_codex, run_dir, strong_effort="ultra")


def test_codex_turns_start_a_thread_then_resume_it(fake_codex, run_dir, tmp_path):
    async def go():
        b = codex(fake_codex, run_dir, key="mma_k")
        await b.start()
        t1 = await b.turn("Watch update 1.", time.time() + 30)
        t2 = await b.turn("Watch update 2.", time.time() + 30)
        t3 = await b.escalate("You are consulted.", time.time() + 30)
        return b, t1, t2, t3

    b, t1, t2, t3 = run(go())
    assert t1.stop == "success" and t1.text == "Proposed #51 (gpt-6-luna)."
    assert [c.name for c in t1.tool_calls] == ["mcp__murmur__get_fleet", "mcp__murmur__propose_intent"]
    assert t1.proposals() == [51] and t1.usage["cached_input_tokens"] == 80
    assert t2.session == t1.session == b.thread_id
    assert json.loads((run_dir / "thread.json").read_text())["thread_id"] == b.thread_id
    assert t3.session != b.thread_id and t3.text == "Proposed #51 (gpt-6.1-sol)."
    runs = [c for c in calls(tmp_path) if c["argv"][:1] == ["exec"]]
    assert "resume" not in runs[0]["argv"] and runs[1]["argv"][1:3] == ["resume", t1.session]
    assert "resume" not in runs[2]["argv"], "the consult is a fresh thread"
    assert runs[0]["prompt"] == "Watch update 1." and runs[0]["cwd"] == str(run_dir)
    assert runs[0]["agent_key"] == "mma_k"


def test_codex_a_tool_outside_the_lockdown_stops_the_run(fake_codex, run_dir, monkeypatch):
    """Finding 13 and N9: the event stream lists no tools, so the backend checks what the model
    used: a command, or a call to another MCP server, is a breach, and the error carries the
    items as Codex sent them."""
    monkeypatch.setenv("FAKE_SCENARIO", "foreign")
    with pytest.raises(LockdownBreach) as e:
        run(codex(fake_codex, run_dir).turn("Watch update 1.", time.time() + 30))
    msg = str(e.value)
    assert '"type": "command_execution"' in msg and '"command": "cat ~/.codex/auth.json"' in msg
    assert '"type": "mcp_tool_call", "server": "other"' in msg
    assert e.value.turn is not None and e.value.turn.stop == "lockdown"


def test_codex_judges_each_item_fail_closed(fake_codex, run_dir):
    """N9: items that act or read outside Murmur are a breach, the documented harmless ones
    pass, a call with no server passes only when it names a Murmur tool, and anything else is
    unknown (recorded, not stopped on)."""
    b = codex(fake_codex, run_dir)
    j = b.judge_item
    for t in ("command_execution", "file_change", "web_search", "patch_apply", "image_view"):
        assert j({"type": t}) == "breach", t
    for t in ("agent_message", "reasoning", "todo_list", "error", "plan"):
        assert j({"type": t}) == "ok", t
    assert j({"type": "mcp_tool_call", "server": "murmur", "tool": "get_fleet"}) == "ok"
    assert j({"type": "mcp_tool_call", "server": "files", "tool": "get_fleet"}) == "breach"
    assert j({"type": "mcp_tool_call", "tool": "mcp__murmur__get_fleet"}) == "ok"
    b.murmur_tools = {"get_fleet", "get_tracks"}
    assert j({"type": "mcp_tool_call", "tool": "get_tracks"}) == "ok"
    assert j({"type": "mcp_tool_call", "tool": "read_file"}) == "breach", "not one of the bridge's tools"
    assert j({"type": "collab_note"}) == "unknown"


def test_codex_an_unknown_item_is_recorded_and_the_turn_goes_on(fake_codex, run_dir, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "unknown")
    b = codex(fake_codex, run_dir)
    b.murmur_tools = {"get_fleet", "get_tracks", "propose_intent"}
    t = run(b.turn("Watch update 1.", time.time() + 30))
    assert t.stop == "success" and t.proposals() == [51]
    assert [(u["id"], u["type"]) for u in t.unknown_items] == [("item_9", "collab_note")]
    assert "thinking aloud" in t.unknown_items[0]["item"], "the raw item"
    assert "mcp__murmur__get_tracks" in [c.name for c in t.tool_calls], "a call with no server, Murmur's"
    assert t.context_tokens == 100


def test_codex_limit_and_crash(fake_codex, run_dir, monkeypatch):
    monkeypatch.setenv("FAKE_SCENARIO", "limit")
    with pytest.raises(LimitReached) as e:
        run(codex(fake_codex, run_dir).turn("Watch update 1.", time.time() + 30))
    assert e.value.hit.provider == "codex" and e.value.hit.reset_at is not None
    assert e.value.turn.stop == "limit"
    monkeypatch.setenv("FAKE_SCENARIO", "limit_later")
    with pytest.raises(LimitReached) as e:
        run(codex(fake_codex, run_dir).turn("Watch update 1.", time.time() + 30))
    assert e.value.hit.reset_at is None
    monkeypatch.setenv("FAKE_SCENARIO", "crash")
    with pytest.raises(BackendCrashed) as e:
        run(codex(fake_codex, run_dir).turn("Watch update 1.", time.time() + 30))
    assert e.value.code == 3 and "crashed" in e.value.stderr
