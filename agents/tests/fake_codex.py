"""A stand-in for `codex` that prints the JSONL events `codex exec --json` documents. No model.

    fake_codex.py --version | login status | exec [resume <thread>] ... -

Behaviour by environment:
    FAKE_STATE            directory: every invocation appends {argv, env flags} to calls.jsonl
    FAKE_SCENARIO         ok (default) | limit | limit_later | crash
    FAKE_VERSION          what --version prints (default codex-cli 0.161.0)
    FAKE_LOGGED_OUT       set: `login status` says not logged in
"""

import json
import os
import sys
import uuid
from pathlib import Path

state = Path(os.environ.get("FAKE_STATE", "."))
scenario = os.environ.get("FAKE_SCENARIO", "ok")
argv = sys.argv[1:]


def record(prompt=None) -> None:
    state.mkdir(parents=True, exist_ok=True)
    with open(state / "calls.jsonl", "a") as f:
        f.write(json.dumps({
            "argv": argv, "prompt": prompt,
            "codex_api_key": "CODEX_API_KEY" in os.environ,
            "openai_api_key": "OPENAI_API_KEY" in os.environ,
            "codex_home": os.environ.get("CODEX_HOME"),
            "agent_key": os.environ.get("MURMUR_AGENT_KEY"),
            "cwd": os.getcwd(),
        }) + "\n")


def emit(obj) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


if argv[:1] == ["--version"]:
    print(os.environ.get("FAKE_VERSION", "codex-cli 0.161.0"))
    sys.exit(0)
if argv[:2] == ["login", "status"]:
    record()
    if os.environ.get("FAKE_LOGGED_OUT"):
        print("Not logged in")
        sys.exit(1)
    print("Logged in using ChatGPT")
    sys.exit(0)
if argv[:1] != ["exec"]:
    sys.exit(2)

prompt = sys.stdin.read()
record(prompt)
thread = argv[argv.index("resume") + 1] if "resume" in argv else str(uuid.uuid4())
model = argv[argv.index("-m") + 1] if "-m" in argv else "?"
emit({"type": "thread.started", "thread_id": thread})
emit({"type": "turn.started"})
if scenario == "crash":
    sys.stderr.write("fake: crashed\n")
    sys.exit(3)
if scenario in ("limit", "limit_later"):
    emit({"type": "error", "message": "stream disconnected, retrying"})
    when = "later" if scenario == "limit_later" else "at 3:42 PM"
    emit({"type": "turn.failed", "error": {"message": f"You’ve hit your usage limit. Try again {when}."}})
    sys.exit(1)
emit({"type": "item.started", "item": {"id": "item_0", "type": "mcp_tool_call", "server": "murmur",
                                       "tool": "get_fleet", "arguments": {}, "status": "in_progress"}})
emit({"type": "item.completed", "item": {"id": "item_0", "type": "mcp_tool_call", "server": "murmur",
                                         "tool": "get_fleet", "arguments": {},
                                         "result": {"content": [{"type": "text", "text": '{"count": 2}'}]},
                                         "error": None, "status": "completed"}})
emit({"type": "item.completed", "item": {"id": "item_1", "type": "mcp_tool_call", "server": "murmur",
                                         "tool": "propose_intent", "arguments": {"task": "hold", "uids": [1]},
                                         "result": {"content": [{"type": "text", "text": '{"id": 51, "state": "pending_approval"}'}]},
                                         "error": None, "status": "completed"}})
emit({"type": "item.completed", "item": {"id": "item_2", "type": "agent_message", "text": "Looking."}})
emit({"type": "item.completed", "item": {"id": "item_3", "type": "agent_message", "text": f"Proposed #51 ({model})."}})
emit({"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 80, "output_tokens": 20}})
