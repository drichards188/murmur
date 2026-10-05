"""A run's stack setup and teardown, without starting a process: what one run leaves behind
for the next run on the same port block.

    .venv/bin/python -m pytest eval/tests/test_stack.py
"""

from __future__ import annotations

from pathlib import Path

import pytest

from eval import REPO
from eval import ports as P
from eval.spec import load_spec


def _stack(tmp_path: Path, run: str, block: P.PortBlock):
    """A stack whose bridge would write its audit log (and its restore pointer) under
    `tmp_path/logs/audit`, not the repository's."""
    from eval import stack as K

    spec = load_spec(REPO / "eval" / "scenarios" / "stale-track.toml")
    run_dir = tmp_path / run
    run_dir.mkdir(parents=True, exist_ok=True)
    s = K.Stack(spec, 1, block, run_dir, f"eval-{run}")
    audit = tmp_path / "logs" / "audit"
    audit.mkdir(parents=True, exist_ok=True)
    s.audit_src = audit / f"eval-{run}.jsonl"
    return s


def _pointer(tmp_path: Path, block: P.PortBlock) -> Path:
    """The bridge's rules-6 pointer for a port: `<root>/logs/audit/.last-run-<http port>`
    (`Settings::last_run`, crates/murmur-bridge/src/decision/mod.rs), naming the last audit
    log written on that port."""
    return tmp_path / "logs" / "audit" / f".last-run-{block.http}"


def _would_alert(pointer: Path) -> str | None:
    """What a bridge starting on this port does with the pointer (`carry_restores`,
    crates/murmur-bridge/src/decision/engage.rs): a pointer naming a log it can't read raises
    a High `restore:carry` inbox alert for the whole run."""
    if not pointer.exists():
        return None
    named = Path(pointer.read_text().strip())
    return None if named.is_file() else f"{pointer.name} names {named}, which can't be read"


def test_a_runs_teardown_leaves_the_next_run_on_its_port_no_stale_restore_pointer(tmp_path: Path):
    """murmur-kvb: the teardown moves the audit log into the run directory, so a pointer left
    naming it would make the next bridge on the port raise `restore:carry` (and, had the log
    stayed, carry lost-link values onto the next run's new vehicles)."""
    block = P.PortBlock(index=3)
    pointer = _pointer(tmp_path, block)
    first = _stack(tmp_path, "first", block)
    # what the first run's bridge leaves (rules 6): its log, the fleet identity beside it,
    # and the port's pointer naming the log
    first.audit_src.write_text('{"type":"run.started"}\n')
    first.audit_src.with_suffix(".fleet.json").write_text("{}")
    pointer.write_text(str(first.audit_src))

    first.stop()  # no process was started: the teardown alone

    assert (first.run_dir / "audit.jsonl").is_file() and not first.audit_src.exists()
    assert _would_alert(pointer) is None, _would_alert(pointer)
    assert not pointer.exists()
    # the log's fleet identity goes with it, under the name the bridge gives it there
    assert (first.run_dir / "audit.fleet.json").is_file()
    assert not first.audit_src.with_suffix(".fleet.json").exists()
    # and the run records what it removed
    assert first.restore_carry["at_stop"]["named"] == str(first.audit_src)
    assert first.restore_carry["at_stop"]["removed"] is True


class _Started(Exception):
    pass


class _Popen:
    pid = 0
    returncode = None

    def poll(self):
        return None


def test_a_bridge_starts_with_no_restore_pointer_for_its_port(tmp_path: Path, monkeypatch):
    """A run that never tore down (a crashed or killed runner) leaves its pointer and its log
    in place; the next run on the port must not let its bridge read them: its vehicles are
    new stand-ins, owed nothing."""
    from eval import stack as K

    block = P.PortBlock(index=4)
    pointer = _pointer(tmp_path, block)
    crashed = tmp_path / "logs" / "audit" / "eval-crashed.jsonl"
    crashed.parent.mkdir(parents=True, exist_ok=True)
    crashed.write_text('{"type":"run.started"}\n')
    pointer.write_text(str(crashed))
    s = _stack(tmp_path, "next", block)

    seen: dict[str, bool] = {}

    def spawn(self, name, cmd, env, cwd=None):
        if name == "linkemu":
            self.bridge_fleet_json.write_text("{}")
            return K.Proc(name, _Popen(), self.run_dir / f"{name}.log")
        # the moment the bridge would start: does a pointer for its port exist?
        seen[name] = pointer.exists()
        raise _Started

    monkeypatch.setattr(K.Stack, "_spawn", spawn)
    monkeypatch.setattr(K.Stack, "_wait_http", lambda self, url, proc, timeout: None)
    monkeypatch.setattr(K, "_binary", lambda name: Path("/bin/true"))
    monkeypatch.setattr(K.Bridge, "try_get", lambda self, path: {})
    with pytest.raises(_Started):
        s.start()

    assert seen == {"bridge": False}, f"the bridge would start with {pointer.name} naming {crashed.name}"
    rec = s.restore_carry["before_start"]
    assert rec["named"] == str(crashed) and rec["removed"] is True and rec["ours"] is False
    # the crashed run's log is left where it is (it is not this run's to move)
    assert crashed.is_file()
