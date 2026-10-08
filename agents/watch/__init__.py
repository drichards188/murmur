"""The watch officer's parts (agents/watch_officer.py is the driver):

    backend.py    the backend interface and what a turn reports
    claude.py     Claude Code (`claude -p`, stream-json) as a backend
    codex.py      Codex CLI (`codex exec`, JSONL) as a backend
    limits.py     usage-limit messages and their reset times
    wake.py       the trigger table, coalescing and budgets
    picture.py    reading the bridge (get_changes) and composing the update
    shadow.py     the deterministic agent logged beside the model
    situation.py  the situation log (JSONL)
    prompt.md     the system prompt (the run's brief is appended)
"""
