"""A stand-in for `claude` that speaks the stream-json events the watch officer reads. No model.

    fake_claude.py --version | auth status | -p ... (stream-json on stdin and stdout)

Behaviour by environment:
    FAKE_STATE          directory: every invocation appends {argv, env flags} to calls.jsonl
    FAKE_SCENARIO       ok (default) | limit | limit_once | crash_once | slow | max_turns |
                        error_once | error_prose
    FAKE_RESET_IN       a limit's resetsAt, seconds from now (default 90)
    FAKE_LIMIT_AT       limit_once: the message (of a process) that hits the limit (default 1)
    FAKE_ERROR_MODEL    error_once: only a process run with this --model errors
    FAKE_AUTH_METHOD    authMethod in `auth status` (default claude.ai)
    FAKE_LOGGED_OUT     set: `auth status` says not logged in
    FAKE_VERSION        what --version prints (default 2.1.294 (Claude Code))
"""

import json
import os
import sys
import time
import uuid
from pathlib import Path

state = Path(os.environ.get("FAKE_STATE", "."))
scenario = os.environ.get("FAKE_SCENARIO", "ok")
argv = sys.argv[1:]


def record() -> None:
    state.mkdir(parents=True, exist_ok=True)
    with open(state / "calls.jsonl", "a") as f:
        f.write(json.dumps({
            "argv": argv,
            "anthropic_api_key": "ANTHROPIC_API_KEY" in os.environ,
            "anthropic_auth_token": "ANTHROPIC_AUTH_TOKEN" in os.environ,
            "agent_key": os.environ.get("MURMUR_AGENT_KEY"),
            "tool_search": os.environ.get("ENABLE_TOOL_SEARCH"),
            "max_retries": os.environ.get("CLAUDE_CODE_MAX_RETRIES"),
            "cwd": os.getcwd(),
        }) + "\n")


def emit(obj) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def opt(name: str):
    return argv[argv.index(name) + 1] if name in argv else None


if argv[:1] == ["--version"]:
    print(os.environ.get("FAKE_VERSION", "2.1.294 (Claude Code)"))
    sys.exit(0)
if argv[:2] == ["auth", "status"]:
    record()
    if os.environ.get("FAKE_LOGGED_OUT"):
        print(json.dumps({"loggedIn": False}))
        sys.exit(1)
    print(json.dumps({"loggedIn": True, "authMethod": os.environ.get("FAKE_AUTH_METHOD", "claude.ai"),
                      "subscriptionType": "max"}))
    sys.exit(0)

record()
resume = opt("--resume")
session = resume or str(uuid.uuid4())
model = {"sonnet": "claude-sonnet-5-5", "opus": "claude-opus-5-5"}.get(opt("--model") or "", opt("--model"))
crashed_marker = state / "crashed"
cost = 0.0
n = 0
inited = False


def result(subtype="success", is_error=False, text=None, turns=3):
    emit({"type": "result", "subtype": subtype, "is_error": is_error, "result": text, "num_turns": turns,
          "duration_ms": 12, "total_cost_usd": cost, "session_id": session,
          "usage": {"input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 100}})


for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "control_request":
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": msg.get("request_id")}})
        result("error_during_execution", True, None)
        continue
    if msg.get("type") != "user":
        continue
    n += 1
    if not inited:
        inited = True
        emit({"type": "system", "subtype": "init", "session_id": session, "model": model,
              "tools": ["mcp__murmur__get_fleet", "mcp__murmur__propose_intent"],
              "mcp_servers": [{"name": "murmur", "status": "connected"}], "permissionMode": "dontAsk",
              "apiKeySource": "none"})
    if "--replay-user-messages" in argv:
        emit(msg)
    text = msg["message"]["content"]
    if scenario == "crash_once" and not resume and not crashed_marker.exists():
        crashed_marker.write_text("1")
        sys.stderr.write("fake: crashed\n")
        sys.exit(3)
    limited = state / "limited"
    limit_at = int(os.environ.get("FAKE_LIMIT_AT", "1"))  # limit_once: on this message
    if scenario == "limit" or (scenario == "limit_once" and not limited.exists() and n >= limit_at):
        limited.write_text("1")
        # the CLI's own message for an API error comes as a "<synthetic>" assistant message
        emit({"type": "assistant", "message": {"model": "<synthetic>", "content": [
            {"type": "text", "text": "You've hit your session limit · resets 3:45pm"}]}})
        reset_in = float(os.environ.get("FAKE_RESET_IN", "90"))
        emit({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "resetsAt": time.time() + reset_in,
                                                              "rateLimitType": "five_hour"}})
        result("success", True, "You've hit your session limit · resets 3:45pm", 1)
        sys.exit(1)
    errored = state / "errored"
    error_model = os.environ.get("FAKE_ERROR_MODEL")  # error_once: only on this --model
    if scenario == "error_once" and not errored.exists() and error_model in (None, opt("--model")):
        # an API error after the CLI's own retries (overloaded): the turn ends, the process lives
        errored.write_text("1")
        emit({"type": "assistant", "message": {"model": "<synthetic>", "content": [
            {"type": "text", "text": "API Error: 529 Overloaded"}]}})
        result("success", True, "API Error: 529 Overloaded", 1)
        continue
    if scenario == "error_prose":
        # the model's own text mentions a limit; the turn then fails for another reason
        emit({"type": "assistant", "message": {"model": model, "content": [
            {"type": "text", "text": "Tool call limit reached for this update; the battery reserve limit reached 20%."}]}})
        result("error_during_execution", True, None, 2)
        continue
    emit({"type": "assistant", "message": {"model": model, "content": [
        {"type": "tool_use", "id": f"tu_{n}_1", "name": "mcp__murmur__get_fleet", "input": {}}]}})
    if scenario == "slow":
        continue  # no result until an interrupt arrives
    emit({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": f"tu_{n}_1", "content": [{"type": "text", "text": '{"count": 2}'}]}]}})
    if scenario == "max_turns":
        cost += 0.01
        result("error_max_turns", True, None, 13)
        continue
    emit({"type": "assistant", "message": {"model": model, "content": [
        {"type": "tool_use", "id": f"tu_{n}_2", "name": "mcp__murmur__propose_intent",
         "input": {"task": "hold", "uids": [1], "rationale": "fake"}}]}})
    emit({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": f"tu_{n}_2",
         "content": [{"type": "text", "text": json.dumps({"id": 40 + n, "state": "pending_approval"})}]}]}})
    emit({"type": "assistant", "message": {"model": model, "content": [
        {"type": "text", "text": f"Proposed #{40 + n}. Update was {len(text)} characters."}]}})
    emit({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "resetsAt": int(time.time()) + 3600}})
    cost += 0.01
    result("success", False, f"Proposed #{40 + n}.")
