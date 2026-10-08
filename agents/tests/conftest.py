"""The watch officer's tests: stand-in CLIs (fake_claude.py, fake_codex.py), no model, no bridge.

    .venv/bin/python -m pytest -q agents/tests
"""

from __future__ import annotations

import os
import stat
import sys
from pathlib import Path

import pytest

TESTS = Path(__file__).resolve().parent
AGENTS = TESTS.parent
sys.path.insert(0, str(AGENTS))


def wrapper(tmp_path: Path, name: str, script: str) -> Path:
    """An executable that runs the stand-in with this interpreter."""
    exe = tmp_path / "bin" / name
    exe.parent.mkdir(parents=True, exist_ok=True)
    exe.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{TESTS / script}" "$@"\n')
    exe.chmod(exe.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return exe


@pytest.fixture
def fake_claude(tmp_path, monkeypatch):
    state = tmp_path / "state"
    monkeypatch.setenv("FAKE_STATE", str(state))
    return wrapper(tmp_path, "claude", "fake_claude.py")


@pytest.fixture
def fake_codex(tmp_path, monkeypatch):
    state = tmp_path / "state"
    monkeypatch.setenv("FAKE_STATE", str(state))
    return wrapper(tmp_path, "codex", "fake_codex.py")


@pytest.fixture
def run_dir(tmp_path):
    d = tmp_path / "run"
    d.mkdir()
    return d


def calls(tmp_path: Path) -> list[dict]:
    import json

    f = tmp_path / "state" / "calls.jsonl"
    return [json.loads(line) for line in f.read_text().splitlines()] if f.exists() else []


@pytest.fixture(autouse=True)
def _no_real_keys(monkeypatch):
    """Tests set the API-key variables themselves to check they are removed."""
    for k in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CODEX_API_KEY", "OPENAI_API_KEY", "MURMUR_AGENT_KEY"):
        monkeypatch.delenv(k, raising=False)
    os.environ.pop("FAKE_SCENARIO", None)
