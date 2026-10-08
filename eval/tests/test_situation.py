"""Situations (sim/situations/, eval/situation.py, eval/situation_run.py): the format, the
status document and what a blinded run hides, the AI's launch contract, its own directory and
preflight, the port block and check, the audit tail, the watch policy against the shipped one,
who acts as whom (scripted ids, the opening orders' dry run), and the scripted engagement
signers against the bridge's shapes. No stack starts and no model is called.

    .venv/bin/python -m pytest eval/tests/test_situation.py
"""

from __future__ import annotations

import asyncio
import json
import os
import socket
import subprocess
import sys
import time
import tomllib
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eval import REPO
from eval import situation as S
from eval.agent import _mcp_args
from eval.proxy import EngagementSigners, ProxyOperator, ProxyParams

# ---- the format ----------------------------------------------------------------------------------


def test_every_situation_loads():
    paths = sorted(S.SITUATIONS.glob("*.toml"))
    names = {p.stem for p in paths}
    assert {"harbor-watch", "comms-cut-gnss-spoof", "hostile-act-engagement", "approval-flood"} <= names
    for p in paths:
        sit = S.load_situation(p)
        assert sit.title and sit.brief, p
        if p.stem != "harbor-watch":
            assert sit.todo, f"{p.stem} is a stub until the AI work lands"
    hw = S.load_situation("harbor-watch")
    assert not hw.todo
    assert hw.fleet and hw.fleet.exists() and hw.contacts and hw.contacts.exists()
    assert hw.policy == (S.SITUATIONS / "policies" / "watch.toml").resolve()
    assert (hw.operator, hw.ai, hw.cosigner, hw.reveal) == ("live", "claude", "you", False)
    assert 600 <= hw.duration_s <= 900 and hw.rate == 1
    assert [o.task for o in hw.orders] == ["patrol", "orbit", "orbit"]
    assert hw.watch and all(w.text for w in hw.watch)
    # blinded, the watch items give no answers: the one that does is the director's
    shown = [w.text for w in hw.watch if not w.director]
    assert not any("Nothing to do" in t or "western approach" in t for t in shown)
    assert [w.at_s for w in hw.watch if w.director] == [120.0]
    # the brief is accurate: an AI's hold or rtl goes out at once (safety-commands), and the
    # opening orders that need a person wait for one
    assert "hold or a return home (rtl)" in hw.brief and "wait for you in the Inbox" in hw.brief
    assert all(s.params.get("director") for s in hw.steps), "the truth notes are the director's"
    assert hw.playbook["platform"] == "puma" and len(hw.playbook["near"]) == 2
    # the brief is what the AI is told: never the director's notes
    assert "15 m/s" in hw.director and "15 m/s" not in S.brief_text(hw)


def test_harbor_watch_contacts_and_fleet_fit_the_story():
    """The fast boat is the only contact without AIS near the Gate, it comes after the fleet is
    up, and the reserve Puma is the first Puma (the playbook's sensor)."""
    hw = S.load_situation("harbor-watch")
    contacts = tomllib.loads(hw.contacts.read_text())["contact"]
    boat = next(c for c in contacts if c["kind"] == "small_fast_boat")
    assert "ais" not in boat and boat["start_s"] >= 60 and boat["motion"] == "transit"
    dark = [c["id"] for c in contacts if "ais" not in c]
    assert set(dark) == {boat["id"], "fishing-milerock"}
    fleet = tomllib.loads(hw.fleet.read_text())["unit"]
    assert [u["platform"] for u in fleet] == ["corsair", "fvr90", "puma", "puma"]
    assert all(u.get("sim") == "standin" for u in fleet)


def _write(tmp_path: Path, name: str, body: str) -> Path:
    p = tmp_path / f"{name}.toml"
    p.write_text(body)
    return p


BASE = 'title = "T"\nbrief = "B"\nfleet = "sim/fleets/harbor-watch.toml"\n'


@pytest.mark.parametrize(
    "body",
    [
        'title = "T"\nfleet = "sim/fleets/harbor-watch.toml"\n',  # no brief
        BASE + 'colour = "blue"\n',  # unknown key
        BASE + 'operator = "remote"\n',
        BASE + 'ai = "gpt"\n',
        BASE + 'cosigner = "nobody"\n',
        BASE + "reveal = 1\n",
        BASE + "duration_s = 0\n",
        BASE + "ai_start_s = 9999\n",  # the AI would never come on watch
        'title = "T"\nbrief = "B"\nfleet = "sim/fleets/nope.toml"\n',
        BASE + '[[order]]\ntask = "orbit"\nvehicles = {callsign = "PUMA 1"}\n',  # orbit, no center
        BASE + '[[order]]\ntask = "patrol"\nvehicles = {callsign = "PUMA 1"}\nroute = [[-122.4, 37.8]]\n',
        BASE + '[[order]]\ntask = "strike"\nvehicles = {callsign = "PUMA 1"}\n',
        BASE + '[[order]]\ntask = "hold"\n',  # no vehicles
        BASE + '[[order]]\nid = "a"\ntask = "hold"\nvehicles = {uids = [1]}\n[[order]]\nid = "a"\ntask = "rtl"\nvehicles = {uids = [1]}\n',
        BASE + '[[step]]\nkind = "teleport"\n',
        BASE + '[[step]]\nkind = "propose"\ntask = "orbit"\nvehicles = {callsign = "PUMA 1"}\ncenter = {contact = "fastboat-west"}\n',
        BASE + '[[watch]]\nat_s = 10\n',  # no text
        BASE + '[playbook]\nnear = [1]\n',
        BASE + '[storyline]\nproposer = "nobody"\n',
        # each step kind takes its own keys only, and a declaration never names its operator
        BASE + '[[step]]\nkind = "note"\ntask = "orbit"\ntext = "x"\n',
        BASE + '[[step]]\nkind = "declare"\naffiliation = "hostile"\ntrack = {select = "most_suspicious"}\noperator = "op-1"\n',
        BASE + '[[step]]\nkind = "note"\ntext = "x"\ndirector = "yes"\n',
        BASE + '[[watch]]\nat_s = 10\ntext = "x"\ndirector = 1\n',
        BASE + '[[watch]]\nat_s = 10\ntext = "x"\nwhen = "later"\n',
        # nothing at or after the end
        BASE + 'duration_s = 100\n[[step]]\nkind = "note"\nat_s = 100\ntext = "x"\n',
        BASE + 'duration_s = 100\n[[watch]]\nat_s = 100\ntext = "x"\n',
        # the autoplay operator's parameters are checked at load
        BASE + '[autoplay]\npreset = "reckless"\n',
        BASE + '[autoplay]\ncourage = 1.0\n',
        # a wrong type is a SituationError naming the key, never a crash
        BASE + 'duration_s = "ten minutes"\n',
        BASE + 'seed = "abc"\n',
        BASE + '[[step]]\nkind = "note"\nat_s = "soon"\ntext = "x"\n',
        BASE + 'playbook = 3\n',
        BASE + 'autoplay = "fast"\n',
        BASE + 'order = 5\n',
        BASE + '[[order]]\ntask = "hold"\nvehicles = {uids = ["one"]}\n',
    ],
)
def test_bad_situations_are_refused(tmp_path: Path, body: str):
    with pytest.raises(S.SituationError):
        S.load_situation(_write(tmp_path, "bad", body))


def test_an_opening_order_for_an_armed_platform_is_refused(tmp_path: Path):
    """An opening order is a script's: one for an armed platform would be a human decision it
    is not (and an engagement is never an order kind)."""
    fleet = 'title = "T"\nbrief = "B"\nfleet = "sim/fleets/engage-keepout-demo.toml"\n'
    with pytest.raises(S.SituationError) as e:
        S.load_situation(_write(tmp_path, "armed", fleet + '[[order]]\ntask = "hold"\nvehicles = {platform = "switchblade"}\n'))
    assert "armed platform" in str(e.value)
    with pytest.raises(S.SituationError):
        S.load_situation(_write(tmp_path, "armed2", fleet + '[[order]]\ntask = "rtl"\nvehicles = {platform = "magura"}\n'))
    ok = S.load_situation(_write(tmp_path, "unarmed", fleet + '[[order]]\ntask = "hold"\nvehicles = {platform = "corsair"}\n'))
    assert [o.task for o in ok.orders] == ["hold"]


def test_a_situation_step_needs_no_truth_label(tmp_path: Path):
    p = _write(
        tmp_path,
        "steps",
        BASE
        + '[[step]]\nid = "p"\nat_s = 30\nkind = "propose"\ntask = "orbit"\nvehicles = {callsign = "PUMA 1"}\n'
        + 'center = {track = {select = "most_suspicious"}}\nradius_m = 600\ntext = "a second station asks for an orbit"\n'
        + '[[step]]\nid = "n"\nat_s = 10\nkind = "note"\ntext = "hello"\n',
    )
    sit = S.load_situation(p)
    assert [s.id for s in sit.steps] == ["n", "p"]
    assert "reason" not in sit.steps[1].params


def test_resolve_modes_and_who_is_scripted(tmp_path: Path):
    hw = S.load_situation("harbor-watch")
    m = S.resolve_modes(hw)
    assert (m["operator"], m["ai"], m["ai_label"], m["agent"]) == ("live", "claude", "Claude", "agent-watch")
    # live: the opening orders are the setup script's; op-1 is the person and never a script
    assert m["scripted_operators"] == ["op-setup"] and m["console_operator"] == "op-1"
    assert m["autoplay_operator"] is None and m["evidence_note"] is None
    m = S.resolve_modes(hw, operator="autoplay", ai="playbook")
    assert m["scripted_operators"] == ["op-setup", "op-auto", "op-2", "op-3"] and m["agent"] == "agent-planner"
    assert m["autoplay_operator"] == "op-auto" and m["ai_label"] == "Playbook (deterministic)"
    m = S.resolve_modes(hw, ai="codex", cosigner="scripted")
    assert m["ai_label"] == "ChatGPT (Codex)" and m["agent"] == "agent-watch"
    assert m["scripted_operators"] == ["op-setup", "op-2", "op-3"], "live: the scripted signers only cosign"
    assert S.resolve_modes(hw, ai="none")["ai_label"] == "None"
    for kind in ("live", "autoplay"):
        assert "op-1" not in S.resolve_modes(hw, operator=kind)["scripted_operators"]
    # a model AI watching a revealed run (truth served on the bridge) is no evidence about it
    assert S.resolve_modes(hw, reveal=True)["reveal"] is True
    for ai in ("claude", "codex"):
        assert "not evidence about the AI" in S.resolve_modes(hw, ai=ai, reveal=True)["evidence_note"]
    assert S.resolve_modes(hw, ai="playbook", reveal=True)["evidence_note"] is None
    with pytest.raises(S.SituationError):
        S.resolve_modes(hw, ai="gemini")
    # the timeline's second station is a script too, when the timeline uses it
    p = _write(tmp_path, "planner", BASE + '[[step]]\nkind = "declare"\naffiliation = "suspect"\ntrack = {select = "most_suspicious"}\n')
    assert S.resolve_modes(S.load_situation(p))["scripted_operators"] == ["op-planner"]
    p = _write(tmp_path, "operator", BASE + '[storyline]\nproposer = "operator"\n[[step]]\nkind = "propose"\ntask = "orbit"\n'
               'vehicles = {callsign = "PUMA 1"}\ncenter = {track = {select = "most_suspicious"}}\nradius_m = 600\n')
    assert S.resolve_modes(S.load_situation(p))["scripted_operators"] == ["op-planner"]
    # the roster a situation's dev run gets: every scripted id its own, op-1 and the signers
    # with engagement authority, never a scripted id that decides ordinary orders
    assert S.OPERATORS == ["op-1", "op-2", "op-3", "op-auto", "op-setup", "op-planner"]
    assert S.ENGAGE_OPERATORS == ["op-1", "op-2", "op-3"]
    assert {"agent-watch", "agent-planner", "agent-scripted", "agent-setup"} == set(S.AGENTS)


# ---- the status document -------------------------------------------------------------------------


def _doc(reveal: bool, elapsed: float = 200.0) -> dict[str, Any]:
    hw = S.load_situation("harbor-watch")
    modes = S.resolve_modes(hw, reveal=reveal)
    entries = [
        {"t_s": 25.0, "kind": "phase", "text": "fleet ready", "director": False},
        {"t_s": 150.0, "kind": "step", "text": "A fast boat without AIS enters (truth)", "director": True},
    ]
    return S.status_doc(
        hw,
        modes,
        phase="running",
        elapsed_s=elapsed,
        now=1_000.0,
        entries=entries,
        done_steps={"fast-boat-enters"},
        orders=[],
        agent={"kind": "claude", "status": "running"},
        autoplay=None,
        run={"id": "r"},
    )


def test_a_blinded_status_carries_nothing_still_to_come_nor_the_directors_notes():
    d = _doc(reveal=False)
    text = json.dumps(d)
    # nor a watch item the director keeps (it would give an answer)
    assert all(not w["director"] for w in d["watch"]) and "Nothing to do" not in text
    assert "director" not in d and "upcoming" not in d["timeline"] and "watch_upcoming" not in d
    assert [e["text"] for e in d["timeline"]["done"]] == ["fleet ready"]
    assert "15 m/s" not in text and "(truth)" not in text
    # watch items only as they come due
    assert d["watch"] and all(w["at_s"] <= 200 for w in d["watch"])
    assert d["clock"]["remaining_s"] == pytest.approx(520.0)
    assert d["modes"]["ai_label"] == "Claude" and "agent" not in d["modes"]


def test_a_revealed_status_shows_the_director_and_what_comes():
    d = _doc(reveal=True)
    assert [w["at_s"] for w in d["watch"] if w["director"]] == [120.0]
    assert "15 m/s" in d["director"]
    assert [u["id"] for u in d["timeline"]["upcoming"]] == ["fast-boat-holds"]
    assert len(d["timeline"]["done"]) == 2
    assert all(w["at_s"] > 200 for w in d["watch_upcoming"])


def test_step_text():
    step = S.Step(id="cut", kind="link", at_s=10, params={"text": "the jammer comes on"})
    rec = {"kind": "link", "step": "cut", "callsigns": ["KESTREL 1"], "effect": {"cut": True}}
    assert S.step_text(rec, step) == "the jammer comes on (link cut on KESTREL 1)"
    rec = {"kind": "propose", "step": "p", "request": {"task": "orbit", "uids": [3]}, "vehicle": {"callsign": "PUMA 1"}, "source": {"id": "agent-scripted"}, "intent_id": 9, "state": "pending_approval"}
    assert S.step_text(rec, None) == "orbit for PUMA 1 proposed by agent-scripted: intent 9 pending_approval"
    rec = {"kind": "propose", "step": "p", "request": {"task": "orbit", "uids": [3]}, "error": "no track"}
    assert "error: no track" in S.step_text(rec, None)
    assert S.step_text({"kind": "note", "text": "hi"}, None) == "hi"
    # a note's text is its headline too: said once
    note = S.Step(id="n", kind="note", at_s=1, params={"text": "a boat enters (truth)"})
    assert S.step_text({"kind": "note", "text": "a boat enters (truth)"}, note) == "a boat enters (truth)"


# ---- the AI: the launch contract and the preflight -----------------------------------------------


def test_the_ai_launch_contract():
    hw = S.load_situation("harbor-watch")
    kw = dict(python="py", mcp_url="http://127.0.0.1:9/mcp", brief_file=Path("/b.md"), agent_log=Path("/a.jsonl"), until=1234.4, seconds=300)
    for backend in ("claude", "codex"):
        cmd = S.ai_command(backend, hw, **kw)
        assert cmd[1].endswith("agents/watch_officer.py")
        args = dict(zip(cmd[2::2], cmd[3::2]))
        assert args == {
            "--backend": backend,
            "--url": "http://127.0.0.1:9/mcp",
            "--agent": "agent-watch",
            "--brief": "/b.md",
            "--situation-log": "/a.jsonl",
            "--until": "1234",
        }
    pb = S.ai_command("playbook", hw, **kw)
    assert pb[1].endswith("agents/policy_agent.py")
    assert "--near=-122.4783,37.8199" in pb, "a negative longitude goes in one token"
    assert pb[pb.index("--agent") + 1] == "agent-planner" and pb[pb.index("--watch") + 1] == "300"
    assert pb[pb.index("--platform") + 1] == "puma"
    assert S.ai_command("none", hw, **kw) is None


def test_the_watch_officer_placeholder_keeps_the_contract(tmp_path: Path):
    log = tmp_path / "agent.jsonl"
    for backend in ("claude", "codex"):
        r = subprocess.run(
            [sys.executable, str(REPO / "agents" / "watch_officer.py"), "--backend", backend, "--url", "http://127.0.0.1:9/mcp",
             "--agent", "agent-watch", "--brief", str(tmp_path / "b.md"), "--situation-log", str(log), "--until", "123"],
            capture_output=True, text=True, timeout=30,
        )
        assert r.returncode == 0, r.stderr
        assert "not built yet" in r.stdout
    notes = [json.loads(line) for line in log.read_text().splitlines()]
    assert len(notes) == 2 and all(n["kind"] == "note" for n in notes)


class FakeRun:
    def __init__(self, code: int, out: str, err: str = ""):
        self.calls: list[list[str]] = []
        self.result = SimpleNamespace(returncode=code, stdout=out, stderr=err)

    def __call__(self, cmd, **kw):
        self.calls.append(cmd)
        return self.result


def test_ai_preflight():
    which = {"claude": "/bin/claude", "codex": "/bin/codex"}.get
    assert S.ai_preflight("playbook") == {"ai": "playbook", "checked": False}
    assert S.ai_preflight("none")["checked"] is False
    # not installed: says how to install and that the playbook needs no account
    for ai in ("claude", "codex"):
        with pytest.raises(S.SituationError) as e:
            S.ai_preflight(ai, which=lambda _: None)
        assert "not installed" in str(e.value) and "AI=playbook" in str(e.value)
    # claude: `claude auth status` (JSON); only yes or no and the kind of login are kept
    status = {"loggedIn": True, "authMethod": "claude.ai", "email": "someone@example.com", "orgId": "x"}
    run = FakeRun(0, json.dumps(status))
    out = S.ai_preflight("claude", run=run, which=which)
    assert run.calls == [["/bin/claude", "auth", "status"]]
    assert out == {"ai": "claude", "checked": True, "cli": "/bin/claude", "logged_in": True, "auth_method": "claude.ai"}
    assert "someone@example.com" not in json.dumps(out)
    with pytest.raises(S.SituationError) as e:
        S.ai_preflight("claude", run=FakeRun(1, json.dumps({"loggedIn": False, "authMethod": "none"})), which=which)
    assert "/login" in str(e.value) and "AI=playbook" in str(e.value)
    # codex: `codex login status`
    run = FakeRun(0, "Logged in using ChatGPT\n")
    out = S.ai_preflight("codex", run=run, which=which)
    assert run.calls == [["/bin/codex", "login", "status"]] and out["auth_method"] == "chatgpt"
    with pytest.raises(S.SituationError) as e:
        S.ai_preflight("codex", run=FakeRun(1, "Not logged in\n"), which=which)
    assert "codex login" in str(e.value)
    old = S.ai_preflight("codex", run=FakeRun(2, "", "error: unrecognized subcommand 'status'"), which=which)
    assert old["checked"] is False and "not checked" in old["note"]


def test_an_old_claude_is_told_to_update():
    """A Claude Code older than `claude auth status` answers it with no JSON: the refusal says
    it is too old and which version it needs, not that it is logged out."""
    which = {"claude": "/bin/claude"}.get

    def run(version):
        def go(cmd, **kw):
            if cmd[1:] == ["--version"]:
                return SimpleNamespace(returncode=0, stdout=f"{version} (Claude Code)\n", stderr="")
            return SimpleNamespace(returncode=1, stdout="", stderr="error: unknown command 'auth'")

        return go

    with pytest.raises(S.SituationError) as e:
        S.ai_preflight("claude", run=run("2.0.31"), which=which)
    assert "too old" in str(e.value) and "2.1.294" in str(e.value) and "AI=playbook" in str(e.value)
    # new enough, no JSON all the same: not logged in, how to fix it
    with pytest.raises(S.SituationError) as e:
        S.ai_preflight("claude", run=run("2.1.300"), which=which)
    assert "not logged in" in str(e.value) and "too old" not in str(e.value)


# ---- the ports -----------------------------------------------------------------------------------


def _free_tcp() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_a_situation_has_its_own_port_block():
    """By default a situation runs on its own block, clear of the default stack, a second stack
    beside it, the evaluation's fixed blocks and the host's ephemeral range; each `MURMUR_*`
    port variable overrides its port (and the bridge base follows the vehicle base, as in
    up.sh)."""
    from eval.ports import ephemeral_range

    p = S.stack_ports({})
    assert p == {"vehicle_base": 33000, "bridge_base": 34000, "http": 9265, "web": 5673, "linkemu": 9266, "tidewater": 9270, "sitl_instance_base": 160}
    udp = [range(p["vehicle_base"], p["vehicle_base"] + 1000), range(p["bridge_base"], p["bridge_base"] + 1000)]
    tcp = [p["http"], p["web"], p["linkemu"], p["tidewater"]]
    taken_udp = [range(14560, 16560), range(24560, 26560), range(35000, 37000)]
    taken_tcp = {8765, 8766, 5173, 8770, 8865, 8866, 5273, 8870, *range(10600, 10700)}
    assert not any(set(a) & set(b) for a in udp for b in taken_udp)
    assert not set(tcp) & taken_tcp
    eph = ephemeral_range()
    if eph:
        assert all(r.stop - 1 < eph[0] for r in udp), f"below the ephemeral range {eph}"
    p = S.stack_ports({"MURMUR_BASE_PORT": "31000", "MURMUR_HTTP_PORT": "9065", "MURMUR_BRIDGE_BASE_PORT": ""})
    assert p["bridge_base"] == 34000 and p["http"] == 9065 and p["web"] == 5673
    env = S.situation_env({"MURMUR_WEB_PORT": "5473", "PATH": "/bin"})
    assert env["MURMUR_WEB_PORT"] == "5473" and env["MURMUR_HTTP_PORT"] == "9265" and env["PATH"] == "/bin"


def test_a_busy_port_or_a_running_stack_is_reported(tmp_path: Path):
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("0.0.0.0", 0))
    try:
        ports = {"vehicle_base": udp.getsockname()[1], "bridge_base": _free_tcp(), "http": listener.getsockname()[1],
                 "web": _free_tcp(), "linkemu": _free_tcp(), "tidewater": _free_tcp(), "sitl_instance_base": 0}
        problems = S.port_problems(ports, 1, 0, root=tmp_path)
        assert any("MURMUR_HTTP_PORT" in p for p in problems), problems
        assert any("MURMUR_BASE_PORT" in p for p in problems), problems
        (tmp_path / "logs").mkdir()
        (tmp_path / "logs" / "up.pid").write_text(str(os.getpid()))
        assert any("logs/up.pid" in p for p in S.port_problems(ports, 1, 0, root=tmp_path))
    finally:
        listener.close()
        udp.close()
    (tmp_path / "logs" / "up.pid").write_text("999999999")
    free = {k: _free_tcp() for k in ("bridge_base", "http", "web", "linkemu", "tidewater")}
    free.update(vehicle_base=_free_tcp(), sitl_instance_base=0)
    assert S.port_problems(free, 1, 0, root=tmp_path) == []


# ---- the audit tail and the events ---------------------------------------------------------------


def test_the_audit_tail_reads_complete_lines_only(tmp_path: Path):
    p = tmp_path / "audit.jsonl"
    tail = S.AuditTail(p)
    assert tail.read() == []
    a = {"seq": 1, "t": 1_000_000, "type": "agent.action", "agent": "agent-planner", "action": "intent.preview", "detail": "orbit on [3]"}
    b = {"seq": 2, "t": 1_002_000, "type": "intent.created", "intent": {"id": 1}}
    c = {"seq": 3, "t": 1_003_000, "type": "agent.action", "agent": "agent-watch", "action": "intent.propose", "detail": "orbit"}
    with p.open("w") as f:
        f.write(json.dumps(a) + "\n" + json.dumps(b)[:10])
    assert tail.read() == [a]
    with p.open("a") as f:
        f.write(json.dumps(b)[10:] + "\n" + json.dumps(c) + "\n")
    got = tail.read()
    assert got == [b, c]
    assert S.agent_activity([a, b, c], "agent-watch") == [{"t": 1003.0, "seq": 3, "action": "intent.propose", "detail": "orbit"}]
    assert len(S.agent_activity([a, b, c], None)) == 2


def test_events_files_merge_into_one(tmp_path: Path):
    files = [REPO / "sim" / "link-events" / "golden-gate.toml", REPO / "sim" / "events" / "golden-gate-gnss.toml"]
    merged = tomllib.loads(S.merge_events(files))["event"]
    want = [e for f in files for e in tomllib.loads(f.read_text()).get("event", [])]
    assert merged == want
    assert tomllib.loads(S.merge_events([])) == {}


# ---- the watch policy ----------------------------------------------------------------------------


def test_the_watch_policy_is_the_shipped_one_plus_agent_tasking():
    """sim/situations/policies/watch.toml is sim/policy.toml with one tier added before the
    default: an agent's orbit, patrol or track goes to a human. Everything else, the
    engagement tier and identification included, is the shipped policy."""
    sim = tomllib.loads((REPO / "sim" / "policy.toml").read_text())
    pol = tomllib.loads((S.SITUATIONS / "policies" / "watch.toml").read_text())
    added = [t for t in pol["tier"] if t["name"] == "agent-tasking"]
    assert added == [
        {
            "name": "agent-tasking",
            "when": {"source": "agent", "kind": ["orbit", "patrol", "track"]},
            "tier": "human",
            "reason": "an AI's proposal to move one of our vehicles waits for the operator on watch",
        }
    ]
    assert [t for t in pol["tier"] if t["name"] != "agent-tasking"] == sim["tier"]
    names = [t["name"] for t in pol["tier"]]
    assert names.index("agent-tasking") == names.index("default") - 1
    assert {k: v for k, v in pol.items() if k != "tier"} == {k: v for k, v in sim.items() if k != "tier"}


# ---- the scripted operators ----------------------------------------------------------------------


def test_mcp_engagements_name_their_track_as_the_tool_does():
    args = _mcp_args({"task": "engage", "uids": [2], "extra": {"track": "T-0003", "effect_radius_m": 50}})
    assert args["track_id"] == "T-0003" and "track" not in args
    assert "track_id" not in _mcp_args({"task": "orbit", "uids": [2], "center": [0, 0]})


def test_who_signs_next():
    signers = EngagementSigners(None, ["op-2", "op-3"])  # type: ignore[arg-type]
    view = {"approvers_needed": 2, "signed": [], "proposer": None, "eligible": ["op-1", "op-2", "op-3"]}
    assert signers.next_signer(view, {"kind": "agent", "id": "agent-scripted"}) == ("op-2", "")
    assert signers.next_signer({**view, "signed": ["op-2"]}, {})[0] == "op-3"
    # never the proposer, never twice
    assert signers.next_signer({**view, "proposer": "op-2"}, {"kind": "human", "id": "op-2"})[0] == "op-3"
    assert signers.next_signer({**view, "signed": ["op-2", "op-3"]}, {})[0] is None
    # never one without engagement authority
    assert signers.next_signer({**view, "eligible": ["op-1"]}, {})[0] is None
    co = EngagementSigners(None, ["op-2", "op-3"], mode="cosign")  # type: ignore[arg-type]
    who, why = co.next_signer(view, {})
    assert who is None and "person to sign first" in why
    assert co.next_signer({**view, "signed": ["op-2"]}, {})[0] is None, "a scripted first signature is no person's"
    assert co.next_signer({**view, "signed": ["op-1"]}, {})[0] is None, "no plan a person froze"
    assert co.next_signer({**view, "signed": ["op-1"], "plan": {"by": "op-1"}}, {})[0] == "op-2"
    assert co.next_signer({**view, "signed": ["op-1"], "plan": {"by": "op-2"}}, {})[0] is None
    assert co.next_signer({**view, "approvers_needed": 3, "signed": ["op-1", "op-2"], "plan": {"by": "op-1"}}, {})[0] is None, "once"
    with pytest.raises(ValueError):
        EngagementSigners(None, ["op-2"])  # type: ignore[arg-type]
    with pytest.raises(ValueError):
        EngagementSigners(None, ["op-2", "op-2"], mode="cosign")  # type: ignore[arg-type]


class FakeBridge:
    """The bridge's engagement API as the signers use it, in the shapes it serves: the inbox's
    pending items carry the engagement's `engage` view (who signed, the plan the first signature
    froze, the live picture); `GET /api/intents/{id}` and an approval's answer carry the intent
    without it. A picture token per look (`frozen` once a plan is), an approval that quotes it."""

    def __init__(self, passed: bool = True, signed: list[str] | None = None, plan_by: str | None = None, needed: int = 2):
        self.calls: list[tuple[str, str, dict, str | None]] = []
        self.signed: list[str] = list(signed or [])
        self.plan_by = plan_by
        self.needed = needed
        self.tokens = 0
        self.passed = passed
        # a signature reset (approval.reset) between the inbox and the look: the token is
        # then issued on no frozen plan
        self.reset_before_look = False

    def intent_json(self) -> dict[str, Any]:
        state = "dispatched" if len(self.signed) >= self.needed else "pending_approval"
        return {"id": 5, "kind": "engage", "state": state, "source": {"kind": "agent", "id": "agent-scripted"}}

    def inbox_item(self) -> dict[str, Any]:
        return {
            **self.intent_json(),
            "engage": {
                "approvers_needed": self.needed,
                "signed": list(self.signed),
                "proposer": None,
                "eligible": ["op-1", "op-2", "op-3"],
                "plan": {"by": self.plan_by, "effect_point": [-122.5, 37.8]} if self.plan_by else None,
                "live": {"passed": self.passed, "checks": [{"rule": "keep_out_tracks", "outcome": "pass" if self.passed else "fail"}]},
            },
        }

    def intent(self, id: int) -> dict[str, Any]:
        return {"intent": self.intent_json()}

    def post(self, path: str, body: Any = None, operator: str | None = None, ok: tuple[int, ...] = (200,)) -> Any:
        self.calls.append(("POST", path, dict(body or {}), operator))
        if path.endswith("/picture"):
            self.tokens += 1
            if self.reset_before_look:
                self.signed, self.plan_by = [], None
            return {"id": 100 + self.tokens, "intent": 5, "by": operator, "frozen": self.plan_by is not None}
        if path.endswith("/approve"):
            assert body["token"] == 100 + self.tokens, "the signature quotes the picture its signer was shown"
            self.signed.append(operator or "?")
            self.plan_by = self.plan_by or operator
            return {"intent": self.intent_json(), "dispatched": len(self.signed) >= self.needed}
        raise AssertionError(path)

    def try_get(self, path: str) -> Any:
        assert path == "/api/inbox", path
        return {"pending": [self.inbox_item()] if len(self.signed) < self.needed else []}


def _acts(b: FakeBridge) -> list[tuple[str, str | None]]:
    return [(c[1].rsplit("/", 1)[1], c[3]) for c in b.calls]


def test_two_scripted_operators_sign_through_picture_tokens():
    b = FakeBridge()
    log: list[dict[str, Any]] = []
    signers = EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, label="scripted (test)", log=log)  # type: ignore[arg-type]
    # the run loop hands it the inbox's item; each next signature reads the inbox again
    made = asyncio.run(signers.consider(b.inbox_item()))
    assert _acts(b) == [("picture", "op-2"), ("approve", "op-2"), ("picture", "op-3"), ("approve", "op-3")]
    assert [m["by"] for m in made] == ["op-2", "op-3"] and made[-1]["dispatched"]
    notes = [c[2]["note"] for c in b.calls if c[1].endswith("/approve")]
    assert all("not a person" in n and "scripted (test)" in n for n in notes)
    # handed an intent as GET /api/intents/{id} serves it (no engage view), it reads the inbox
    b = FakeBridge()
    made = asyncio.run(EngagementSigners(b, ["op-2", "op-3"], read_s=0.0).consider(b.intent_json()))  # type: ignore[arg-type]
    assert [m["by"] for m in made] == ["op-2", "op-3"]


def test_scripted_signers_wait_while_the_picture_fails():
    b = FakeBridge(passed=False)
    log: list[dict[str, Any]] = []
    signers = EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, log=log)  # type: ignore[arg-type]
    assert asyncio.run(signers.consider(b.inbox_item())) == []
    assert asyncio.run(signers.consider(b.inbox_item())) == []
    assert b.calls == [], "no look and no signature while the engine's picture fails"
    assert len(log) == 1 and log[0]["action"] == "wait" and "keep_out_tracks" in log[0]["reason"]


def test_a_scripted_cosigner_signs_once_and_only_a_plan_a_person_froze():
    # a person (op-1) signed and froze the plan: the cosigner looks, then signs once
    b = FakeBridge(signed=["op-1"], plan_by="op-1")
    co = EngagementSigners(b, ["op-2", "op-3"], mode="cosign", read_s=0.0)  # type: ignore[arg-type]
    made = asyncio.run(co.consider(b.inbox_item()))
    assert _acts(b) == [("picture", "op-2"), ("approve", "op-2")] and [m["by"] for m in made] == ["op-2"]
    # at most one signature per nomination, even where the tier wants a third
    b = FakeBridge(signed=["op-1"], plan_by="op-1", needed=3)
    co = EngagementSigners(b, ["op-2", "op-3"], mode="cosign", read_s=0.0)  # type: ignore[arg-type]
    assert len(asyncio.run(co.consider(b.inbox_item()))) == 1
    assert len(asyncio.run(co.consider(b.inbox_item()))) == 0 and b.signed == ["op-1", "op-2"]
    # the signatures were reset between the inbox and the look: its token is on no frozen plan,
    # so its signature would be the first, which is never a script's
    b = FakeBridge(signed=["op-1"], plan_by="op-1")
    b.reset_before_look = True
    log: list[dict[str, Any]] = []
    co = EngagementSigners(b, ["op-2", "op-3"], mode="cosign", read_s=0.0, log=log)  # type: ignore[arg-type]
    assert asyncio.run(co.consider(b.inbox_item())) == []
    assert _acts(b) == [("picture", "op-2")], "looked, never signed"
    assert log[-1]["action"] == "wait" and "no frozen plan" in log[-1]["reason"]
    # a plan a scripted id froze is no person's
    b = FakeBridge(signed=["op-1"], plan_by="op-3")
    co = EngagementSigners(b, ["op-2", "op-3"], mode="cosign", read_s=0.0)  # type: ignore[arg-type]
    assert asyncio.run(co.consider(b.inbox_item())) == [] and b.calls == []


def test_a_signer_rereads_the_nomination_before_it_signs():
    """Between the look and the signature the picture may change (here: it stops passing); the
    signer reads the inbox again and does not sign."""
    b = FakeBridge()
    orig = b.post

    def post(path, body=None, operator=None, ok=(200,)):
        out = orig(path, body, operator, ok)
        if path.endswith("/picture"):
            b.passed = False
        return out

    b.post = post  # type: ignore[method-assign]
    log: list[dict[str, Any]] = []
    signers = EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, log=log)  # type: ignore[arg-type]
    assert asyncio.run(signers.consider(b.inbox_item())) == []
    assert _acts(b) == [("picture", "op-2")] and "changed while op-2 looked" in log[-1]["reason"]


def test_a_signer_error_is_recorded_and_the_loop_goes_on():
    class Broken(FakeBridge):
        def post(self, path, body=None, operator=None, ok=(200,)):
            raise KeyError("surprise")

    b = Broken()
    log: list[dict[str, Any]] = []
    signers = EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, period_s=0.01, log=log)  # type: ignore[arg-type]

    async def go():
        stop = asyncio.Event()
        task = asyncio.create_task(signers.run(stop))
        await asyncio.sleep(0.05)
        stop.set()
        await task

    asyncio.run(go())
    assert log and all(d["action"] == "error" and "surprise" in d["reason"] for d in log)


def test_the_proxy_leaves_engagements_to_its_signers():
    class Inbox:
        def try_get(self, path):
            return {"pending": [{"id": 1, "kind": "engage", "expires_t": 1}, {"id": 2, "kind": "orbit", "expires_t": 2}]}

    decided: list[int] = []
    signers = EngagementSigners(Inbox(), ["op-2", "op-3"])  # type: ignore[arg-type]
    proxy = ProxyOperator(Inbox(), ProxyParams(attend_s=0.01, attend_sigma=0.0), seed=1, operator="op-auto", signers=signers, note_prefix="scripted")  # type: ignore[arg-type]

    async def decide(i, queue):
        decided.append(i["id"])

    proxy._decide = decide  # type: ignore[method-assign]

    async def go():
        stop = asyncio.Event()
        task = asyncio.create_task(proxy._looks(stop))
        await asyncio.sleep(0.2)
        stop.set()
        await task

    asyncio.run(go())
    assert decided == [2]
    assert proxy.operator == "op-auto" and proxy.describe()["engagement_signers"]["operators"] == ["op-2", "op-3"]


# ---- the runner's pieces -------------------------------------------------------------------------


def test_the_runner_makes_a_fresh_station_and_a_dev_roster(tmp_path: Path, monkeypatch):
    from eval import situation_run as R

    monkeypatch.setattr(R, "REPO", tmp_path)
    audit = tmp_path / "logs" / "audit"
    audit.mkdir(parents=True)
    (audit / ".last-run-9065").write_text("/somewhere/old.jsonl")
    (audit / ".resume-9065.json").write_text("{}")
    (audit / ".last-run-8765").write_text("another stack's")
    hw = S.load_situation("harbor-watch")
    env = {"MURMUR_HTTP_PORT": "9065", "MURMUR_EVAL_TRUTH": "1", "MURMUR_SIM_TOKEN": "theirs", "MURMUR_CONTACTS": "x", "PATH": "/bin",
           "MURMUR_TIDEWATER_URL": "http://127.0.0.1:8870"}

    async def make():
        return R.Runner(hw, S.resolve_modes(hw, operator="autoplay", ai="playbook"), env, {"ai": "playbook", "checked": False})

    runner = asyncio.run(make())
    removed = runner.fresh_station()
    assert len(removed) == 2 and not (audit / ".last-run-9065").exists() and not (audit / ".resume-9065.json").exists()
    assert (audit / ".last-run-8765").exists(), "only this port's pointers"
    e = runner.stack_env()
    assert e["MURMUR_AUTH"] == "dev" and e["MURMUR_RESUME"] == "0" and e["MURMUR_RUN_ID"] == runner.run_id
    assert e["MURMUR_OPERATORS"] == "op-1,op-2,op-3,op-auto,op-setup,op-planner" and e["MURMUR_ENGAGE_OPERATORS"] == "op-1,op-2,op-3"
    assert e["MURMUR_AGENTS"] == "agent-watch,agent-planner,agent-scripted,agent-setup"
    # its own block's ports where none is set, and its own tidewater (not another stack's)
    assert e["MURMUR_HTTP_PORT"] == "9065" and e["MURMUR_WEB_PORT"] == "5673" and e["MURMUR_TIDEWATER_PORT"] == "9270"
    assert "MURMUR_TIDEWATER_URL" not in e
    assert e["MURMUR_SIM_TOKEN"] == runner.token != "theirs"
    assert "MURMUR_EVAL_TRUTH" not in e, "no truth endpoint in a situation"
    assert e["MURMUR_CONTACTS"] == str(hw.contacts) and e["MURMUR_POLICY"] == str(hw.policy)
    # the storyline's records: a step that never ran (the run ended first) is not done
    log = R.StepLog(runner)
    log.write({"kind": "skipped", "step": "fast-boat-holds", "reason": "run ended"})
    log.write({"kind": "note", "step": "fast-boat-enters", "t_rel": 150.0, "text": "A fast boat without AIS enters from the west (truth)"})
    assert runner.done_steps == {"fast-boat-enters"}
    doc = runner.doc()
    assert doc["timeline"]["done"] == [], "a director's step stays hidden on a blinded run"
    assert doc["autoplay"]["scripted"] and doc["agent"]["label"] == "Playbook (deterministic)"
    recs = [json.loads(line) for line in runner.log_path.read_text().splitlines()]
    assert [r["event"] for r in recs] == ["fresh_station", "step", "step"]
    # the summary, with no stack to ask (nothing answers on the port)
    runner.bridge = R.Bridge(f"http://127.0.0.1:{_free_tcp()}", timeout=1.0)
    runner.orders = [{"id": "gate-patrol", "task": "patrol", "vehicles": ["MARLIN 1"], "by": "agent-setup", "intent_id": 1, "state": "pending_approval"}]
    runner.autoplay_log.append({"t": 1.0, "intent_id": 4, "action": "approve", "reason": "nothing speaks against it", "result": "dispatched"})
    # failed attempts are no decisions
    runner.autoplay_log.append({"t": 2.0, "intent_id": 5, "action": "sign_failed", "by": "op-2", "result": "error 409: reset"})
    runner.autoplay_log.append({"t": 3.0, "intent_id": 6, "action": "reject", "reason": "x", "result": "error 409: expired"})
    runner.refresh()
    assert len(runner.problems) == 2 and runner.doc()["problems"] == runner.problems
    runner.summary()
    text = (runner.out / "summary.txt").read_text()
    assert "opening order gate-patrol" in text and "by agent-setup" in text and "murmur-replay" in text
    assert "op-auto approve intent 4: dispatched" in text, "the proxy's decisions are op-auto's, never op-1's"
    assert "scripted decisions: 1 (and 2 failed attempts)" in text


class OrderBridge:
    """The operator API as the opening orders use it: the operator's dry run says whether the
    submission would be its own approval (here: for MARLIN 1, endurance unknown)."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, dict, str | None]] = []

    def post(self, path: str, body: Any = None, operator: str | None = None, ok: tuple[int, ...] = (200,)) -> Any:
        self.calls.append((path, dict(body or {}), operator))
        assert path == "/api/intents/preview", path
        human = body["uids"] == [1]
        return {
            "would_reach": "approved" if human else "auto_approved",
            "submission_authorizes": human,
            "policy": {"tier": "human" if human else "auto", "rule": "endurance-unknown" if human else "default", "reason": "r"},
        }

    def submit(self, body: dict, operator: str) -> dict:
        self.calls.append(("/api/intents", dict(body), operator))
        return {"intent": {"id": 7, "state": "dispatched"}, "dispatched": True}


class SetupAgent:
    def __init__(self) -> None:
        self.proposed: list[tuple[dict, str]] = []

    async def propose(self, req: dict[str, Any], rationale: str) -> dict[str, Any]:
        self.proposed.append((req, rationale))
        return {"id": 8, "state": "pending_approval", "dispatched": False}


def _runner(tmp_path: Path, **modes: Any) -> Any:
    from eval import situation_run as R

    hw = S.load_situation("harbor-watch")

    async def make():
        return R.Runner(hw, S.resolve_modes(hw, **modes), {}, {"checked": False}, out_dir=tmp_path / "run")

    return asyncio.run(make())


def test_opening_orders_never_authorize_a_human_tier(tmp_path: Path):
    """Each opening order is an operator's dry run as op-setup first. Where op-setup's
    submission would be its own approval (a human tier), agent-setup proposes it instead and it
    waits for a decision; otherwise op-setup submits it, under the scripted label. op-1 is
    never used."""
    runner = _runner(tmp_path, operator="live", ai="none")
    runner.bridge = OrderBridge()
    agent = SetupAgent()
    vehicles = [{"uid": 1, "identity": {"callsign": "MARLIN 1"}}, {"uid": 4, "identity": {"callsign": "PUMA 2"}}]
    orders = {o.id: o for o in runner.sit.orders}
    rec = asyncio.run(runner.issue_order(orders["gate-patrol"], vehicles, agent))
    assert rec["by"] == "agent-setup" and rec["state"] == "pending_approval" and "the person at the console" in rec["routed"]
    assert rec["dry_run"]["submission_authorizes"] and rec["dry_run"]["rule"] == "endurance-unknown"
    assert [c[0] for c in runner.bridge.calls] == ["/api/intents/preview"], "never submitted under a scripted operator id"
    assert runner.bridge.calls[0][2] == "op-setup" and agent.proposed[0][0]["uids"] == [1]
    rec = asyncio.run(runner.issue_order(orders["north-station"], vehicles, agent))
    assert rec["by"] == "op-setup" and rec["state"] == "dispatched"
    path, body, op = runner.bridge.calls[-1]
    assert path == "/api/intents" and op == "op-setup" and body["rationale"].startswith(S.SCRIPTED_SETUP)
    assert all(c[2] != "op-1" for c in runner.bridge.calls)
    # autoplay: the autoplay operator decides it
    runner = _runner(tmp_path, operator="autoplay", ai="none")
    runner.bridge = OrderBridge()
    rec = asyncio.run(runner.issue_order(orders["gate-patrol"], vehicles, SetupAgent()))
    assert rec["by"] == "agent-setup" and "op-auto" in rec["routed"]
    # a dry run that fails sends nothing
    class Down(OrderBridge):
        def post(self, path, body=None, operator=None, ok=(200,)):
            from eval.bridge import BridgeError

            raise BridgeError(503, {"detail": "not now"})

    runner.bridge = Down()
    rec = asyncio.run(runner.issue_order(orders["north-station"], vehicles, agent))
    assert rec.get("error") == "not now" and "intent_id" in rec and rec["intent_id"] is None


def test_the_timelines_operator_proposer_never_authorizes_a_human_tier_live():
    from eval import situation_run as R
    from eval.agent import ProposeError

    class B(OrderBridge):
        def post(self, path, body=None, operator=None, ok=(200,)):
            if path != "/api/intents/preview":
                self.calls.append((path, dict(body or {}), operator))
                return {}
            return super().post(path, body, operator, ok)

    req = {"task": "orbit", "uids": [1], "center": [-122.5, 37.8], "radius_m": 500}
    b = B()
    live = R.ScriptedOperatorProposer(b, "op-planner", S.SCRIPTED_TIMELINE, live=True)  # type: ignore[arg-type]
    with pytest.raises(ProposeError) as e:
        asyncio.run(live.propose(req, "an orbit"))
    assert "not sent in live mode" in str(e.value) and not any(c[0] == "/api/intents" for c in b.calls)
    # an auto tier goes, labelled
    out = asyncio.run(live.propose({**req, "uids": [4]}, "an orbit"))
    assert out["state"] == "dispatched" and b.calls[-1][1]["rationale"] == f"{S.SCRIPTED_TIMELINE}: an orbit"
    # autoplay: every id is scripted and the run is never evidence; the proposal goes, labelled
    b = B()
    auto = R.ScriptedOperatorProposer(b, "op-planner", S.SCRIPTED_TIMELINE, live=False)  # type: ignore[arg-type]
    asyncio.run(auto.propose(req, "an orbit"))
    assert [c[0] for c in b.calls] == ["/api/intents"]


def test_a_timeline_declaration_says_it_is_a_scripts():
    from eval import situation_run as R

    calls: list[tuple[str, dict, str | None]] = []

    class T(R.TimelineBridge):
        def post(self, path, body=None, operator=None, ok=(200,)):
            calls.append((path, dict(body or {}), operator))
            return {"declared": {"affiliation": body["affiliation"]}}

    T("http://x").declare("T-0001", "suspect", "fast and dark", "op-planner")
    T("http://x").declare("T-0002", "suspect", None, "op-planner")
    assert [c[1]["note"] for c in calls] == [f"{S.SCRIPTED_TIMELINE}: fast and dark", S.SCRIPTED_TIMELINE]


def test_storyline_proposals_due_together_each_have_their_own_session(monkeypatch):
    """Two steps due at once: each proposal opens, uses and closes a session of its own (a shared
    one was closed under the other)."""
    import policy_agent

    from eval import situation_run as R

    opened: list[str] = []
    closed: list[str] = []

    class FakeSession:
        n = 0

        def __init__(self, url, agent, key):
            FakeSession.n += 1
            self.name = f"S{FakeSession.n}"
            self.closed = False

        async def __aenter__(self):
            await asyncio.sleep(0.01)
            opened.append(self.name)
            return self

        async def __aexit__(self, *a):
            self.closed = True
            closed.append(self.name)

        async def propose(self, rationale, **kw):
            await asyncio.sleep(0.02)
            if self.closed:
                raise RuntimeError(f"{self.name} closed")
            return {"id": rationale, "state": "pending_approval"}

    monkeypatch.setattr(policy_agent, "AgentSession", FakeSession)
    p = R.FreshAgentProposer("http://x/mcp", agent="agent-scripted", label=S.SCRIPTED_TIMELINE)
    req = {"task": "orbit", "uids": [3], "center": [0, 0], "radius_m": 300}

    async def both():
        return await asyncio.gather(p.propose(req, "A"), p.propose(req, "B"))

    got = asyncio.run(both())
    assert [g["id"] for g in got] == [f"{S.SCRIPTED_TIMELINE}: A", f"{S.SCRIPTED_TIMELINE}: B"]
    assert sorted(opened) == sorted(closed) == ["S1", "S2"]


def test_the_ai_starts_in_an_empty_directory_of_its_own(tmp_path: Path, monkeypatch):
    """The AI's working directory is outside the repository and the run directory and holds only
    the brief (no director's notes); its notes go there; its environment has no MURMUR_*."""
    from eval import situation_run as R

    seen: dict[str, Any] = {}

    class FakePopen:
        pid = 999_999_999

        def __init__(self, cmd, **kw):
            seen["cmd"] = cmd
            seen.update(kw)
            seen["files"] = sorted(os.listdir(kw["cwd"]))

        def poll(self):
            return 0

    monkeypatch.setattr(R.subprocess, "Popen", FakePopen)
    monkeypatch.setenv("MURMUR_SIM_TOKEN", "secret")
    monkeypatch.setenv("MURMUR_AGENT_KEY", "k")
    runner = _runner(tmp_path, ai="claude")
    runner.zero = time.time()
    runner.start_ai()
    cwd = Path(seen["cwd"]).resolve()
    assert not cwd.is_relative_to(REPO.resolve()) and not cwd.is_relative_to(runner.out.resolve())
    assert seen["files"] == ["brief.md"]
    brief = (cwd / "brief.md").read_text()
    assert "Harbor watch" in brief and "15 m/s" not in brief
    assert not [k for k in seen["env"] if k.startswith("MURMUR_")] and seen["env"]["PATH"]
    args = dict(zip(seen["cmd"][2::2], seen["cmd"][3::2]))
    assert Path(args["--brief"]).parent == cwd and Path(args["--situation-log"]).parent == cwd
    assert seen["start_new_session"] and runner.ai_pgid == FakePopen.pid
    # at the end: its notes kept with the run, its directory removed
    (cwd / "agent.jsonl").write_text(json.dumps({"t": 1.0, "kind": "note", "text": "on watch"}) + "\n")
    runner.refresh()
    assert runner.ai_notes[-1]["text"] == "on watch"
    runner.ai_pgid = None  # (no group to stop: the process was a fake)
    runner.finish_ai_dir()
    assert not cwd.exists() and (runner.out / "agent.jsonl").exists()


def test_stopping_the_ai_stops_its_whole_group_even_after_its_leader_exits(tmp_path: Path):
    """A watch officer that exits and leaves a CLI it started behind: the runner still stops
    the CLI (its own session's process group)."""
    runner = _runner(tmp_path, ai="playbook")
    code = "import subprocess, sys; p = subprocess.Popen(['sleep', '60']); print(p.pid, flush=True)"
    leader = subprocess.Popen([sys.executable, "-c", code], stdout=subprocess.PIPE, text=True, start_new_session=True)
    child = int(leader.stdout.readline())
    leader.wait(timeout=10)

    def alive(pid: int) -> bool:
        try:
            state = Path(f"/proc/{pid}/stat").read_text().split(") ")[1][0]
        except (OSError, IndexError):
            return False
        return state != "Z"

    assert alive(child)
    runner.ai_proc, runner.ai_pgid = leader, leader.pid
    runner.ai_status = {"status": "exited 0"}
    runner.stop_ai()
    deadline = time.monotonic() + 5
    while alive(child) and time.monotonic() < deadline:
        time.sleep(0.05)
    assert not alive(child)
    assert runner.ai_status["status"] == "exited 0", "an AI that had exited is not said to be stopped"


def test_the_runner_refuses_a_stub_and_a_credentials_run(monkeypatch, capsys):
    from eval import situation_run as R

    assert R.main(["approval-flood"]) == 2
    assert "stub" in capsys.readouterr().err
    monkeypatch.setenv("MURMUR_AUTH", "credentials")
    assert R.main(["harbor-watch", "--ai", "playbook"]) == 2
    assert "dev runs" in capsys.readouterr().err
