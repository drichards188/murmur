"""Murmur's evaluation harness (Phase G): headless scenario runs, arms, metrics, report.

    .venv/bin/python -m eval.run --help

See eval/README.md for how to run a batch and eval/METRICS.md for what is measured.
"""

import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
# the repo's venv when there is one (a git worktree may not have its own), else this one
PYTHON = REPO / ".venv" / "bin" / "python"
if not PYTHON.exists():
    PYTHON = Path(sys.executable)
