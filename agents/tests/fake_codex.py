"""A stand-in for `codex` that prints the JSONL events `codex exec --json` documents. No model.

    fake_codex.py --version | login status | exec [resume <thread>] ... -

Behaviour by environment:
    FAKE_STATE            directory: every invocation appends {argv, env flags} to calls.jsonl
    FAKE_SCENARIO         ok (default) | limit | limit_later | crash | foreign | unknown
                          (unknown: an item of a type the backend does not know, then ok)
    FAKE_VERSION          what --version prints (default codex-cli 0.161.0)
    FAKE_LOGGED_OUT       set: `login status` says not logged in
    FAKE_FEATURE_STUCK, FAKE_FEATURE_GONE, FAKE_FEATURES_FAIL   `features list` (see features_list)
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


# what `codex features list` shows on 0.116 (a subset), name -> on by default
FEATURES = {"apply_patch_freeform": False, "apps": False, "code_mode": False, "fast_mode": True,
            "js_repl": False, "multi_agent": True, "personality": True, "shell_snapshot": True,
            "shell_tool": True, "skill_mcp_dependency_install": True, "unified_exec": True,
            "web_search_request": False}


def features_list() -> int:
    """`[--disable X]... [-c k=v]... features list`. FAKE_FEATURE_STUCK: one that stays on;
    FAKE_FEATURE_GONE: one not listed; FAKE_FEATURES_FAIL: the command fails."""
    if os.environ.get("FAKE_FEATURES_FAIL"):
        print("error: unrecognized subcommand 'features'", file=sys.stderr)
        return 2
    on = dict(FEATURES)
    for i, a in enumerate(argv):
        if a == "--disable" and i + 1 < len(argv):
            if argv[i + 1] not in on:
                print(f"Error: Unknown feature flag: {argv[i + 1]}", file=sys.stderr)
                return 1
            on[argv[i + 1]] = False
    stuck = os.environ.get("FAKE_FEATURE_STUCK")
    if stuck:
        on[stuck] = True
    on.pop(os.environ.get("FAKE_FEATURE_GONE", ""), None)
    for name, v in sorted(on.items()):
        print(f"{name:<33}stable             {'true' if v else 'false'}")
    return 0


if argv[-2:] == ["features", "list"]:
    record()
    sys.exit(features_list())
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
if scenario == "foreign":
    # a tool outside the lockdown: a shell command (and a call to another MCP server)
    emit({"type": "item.started", "item": {"id": "item_0", "type": "command_execution",
                                           "command": "cat ~/.codex/auth.json", "status": "in_progress"}})
    emit({"type": "item.completed", "item": {"id": "item_1", "type": "mcp_tool_call", "server": "other",
                                             "tool": "read", "arguments": {}, "status": "completed"}})
    emit({"type": "item.completed", "item": {"id": "item_2", "type": "agent_message", "text": "Done."}})
    emit({"type": "turn.completed", "usage": {"input_tokens": 1, "output_tokens": 1}})
    sys.exit(0)
if scenario == "unknown":
    # a later Codex's new item type: recorded, the turn goes on
    emit({"type": "item.completed", "item": {"id": "item_9", "type": "collab_note", "text": "thinking aloud"}})
    # a call whose server field is missing, naming a Murmur tool
    emit({"type": "item.completed", "item": {"id": "item_8", "type": "mcp_tool_call", "tool": "get_tracks",
                                             "arguments": {}, "status": "completed"}})
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
