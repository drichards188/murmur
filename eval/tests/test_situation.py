"""Situations (sim/situations/, eval/situation.py, eval/situation_run.py): the format, the
status document and what a blinded run hides, the AI's launch contract and preflight, the port
check, the audit tail, the watch policy against the shipped one, and the scripted engagement
signers. No stack starts and no model is called.

    .venv/bin/python -m pytest eval/tests/test_situation.py
"""

from __future__ import annotations

import asyncio
import json
import os
import socket
import subprocess
import sys
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
    ],
)
def test_bad_situations_are_refused(tmp_path: Path, body: str):
    with pytest.raises(S.SituationError):
        S.load_situation(_write(tmp_path, "bad", body))


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


def test_resolve_modes_and_who_is_scripted():
    hw = S.load_situation("harbor-watch")
    m = S.resolve_modes(hw)
    assert (m["operator"], m["ai"], m["ai_label"], m["agent"]) == ("live", "claude", "Claude", "agent-watch")
    assert m["scripted_operators"] == []
    m = S.resolve_modes(hw, operator="autoplay", ai="playbook")
    assert m["scripted_operators"] == ["op-1", "op-2", "op-3"] and m["agent"] == "agent-planner"
    assert m["ai_label"] == "Playbook (deterministic)"
    m = S.resolve_modes(hw, ai="codex", cosigner="scripted")
    assert m["ai_label"] == "ChatGPT (Codex)" and m["agent"] == "agent-watch"
    assert m["scripted_operators"] == ["op-2", "op-3"], "live: the scripted signers only cosign"
    assert S.resolve_modes(hw, ai="none")["ai_label"] == "None"
    assert S.resolve_modes(hw, reveal=True)["reveal"] is True
    with pytest.raises(S.SituationError):
        S.resolve_modes(hw, ai="gemini")
    # the roster a situation's dev run gets
    assert set(S.ENGAGE_OPERATORS) <= set(S.OPERATORS) and len(set(S.SCRIPTED_SIGNERS)) == 2
    assert {"agent-watch", "agent-planner", "agent-scripted"} == set(S.AGENTS)


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
    assert "director" not in d and "upcoming" not in d["timeline"] and "watch_upcoming" not in d
    assert [e["text"] for e in d["timeline"]["done"]] == ["fleet ready"]
    assert "15 m/s" not in text and "(truth)" not in text
    # watch items only as they come due
    assert d["watch"] and all(w["at_s"] <= 200 for w in d["watch"])
    assert d["clock"]["remaining_s"] == pytest.approx(520.0)
    assert d["modes"]["ai_label"] == "Claude" and "agent" not in d["modes"]


def test_a_revealed_status_shows_the_director_and_what_comes():
    d = _doc(reveal=True)
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


# ---- the ports -----------------------------------------------------------------------------------


def _free_tcp() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_stack_ports_follow_up_sh():
    p = S.stack_ports({})
    assert p == {"vehicle_base": 14560, "bridge_base": 15560, "http": 8765, "web": 5173, "linkemu": 8766, "tidewater": 8770, "sitl_instance_base": 0}
    p = S.stack_ports({"MURMUR_BASE_PORT": "31000", "MURMUR_HTTP_PORT": "9065"})
    assert p["bridge_base"] == 32000 and p["http"] == 9065


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
    assert co.next_signer({**view, "signed": ["op-1"]}, {})[0] == "op-2"
    with pytest.raises(ValueError):
        EngagementSigners(None, ["op-2"])  # type: ignore[arg-type]
    with pytest.raises(ValueError):
        EngagementSigners(None, ["op-2", "op-2"], mode="cosign")  # type: ignore[arg-type]


class FakeBridge:
    """The bridge's engagement API as the signers use it: a picture token per look, an approve
    that quotes it, the engage view updated after each signature."""

    def __init__(self, passed: bool = True):
        self.calls: list[tuple[str, str, dict, str | None]] = []
        self.signed: list[str] = []
        self.tokens = 0
        self.passed = passed

    def intent_json(self) -> dict[str, Any]:
        state = "dispatched" if len(self.signed) >= 2 else "pending_approval"
        return {
            "id": 5,
            "kind": "engage",
            "state": state,
            "source": {"kind": "agent", "id": "agent-scripted"},
            "engage": {"approvers_needed": 2, "signed": list(self.signed), "proposer": None, "eligible": ["op-1", "op-2", "op-3"],
                       "live": {"passed": self.passed, "checks": [{"rule": "keep_out_tracks", "outcome": "pass" if self.passed else "fail"}]}},
        }

    def intent(self, id: int) -> dict[str, Any]:
        return {"intent": self.intent_json()}

    def post(self, path: str, body: Any = None, operator: str | None = None, ok: tuple[int, ...] = (200,)) -> Any:
        self.calls.append(("POST", path, dict(body or {}), operator))
        if path.endswith("/picture"):
            self.tokens += 1
            return {"id": 100 + self.tokens, "intent": 5, "by": operator}
        if path.endswith("/approve"):
            assert body["token"] == 100 + self.tokens, "the signature quotes the picture its signer was shown"
            self.signed.append(operator or "?")
            return {"intent": self.intent_json(), "dispatched": len(self.signed) >= 2}
        raise AssertionError(path)

    def try_get(self, path: str) -> Any:
        return {"pending": [self.intent_json()] if len(self.signed) < 2 else []}


def test_two_scripted_operators_sign_through_picture_tokens():
    b = FakeBridge()
    log: list[dict[str, Any]] = []
    signers = EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, label="scripted (test)", log=log)  # type: ignore[arg-type]
    made = asyncio.run(signers.consider(b.intent_json()))
    assert [(c[1].rsplit("/", 1)[1], c[3]) for c in b.calls] == [("picture", "op-2"), ("approve", "op-2"), ("picture", "op-3"), ("approve", "op-3")]
    assert [m["by"] for m in made] == ["op-2", "op-3"] and made[-1]["dispatched"]
    notes = [c[2]["note"] for c in b.calls if c[1].endswith("/approve")]
    assert all("not a person" in n and "scripted (test)" in n for n in notes)


def test_scripted_signers_wait_while_the_picture_fails():
    b = FakeBridge(passed=False)
    log: list[dict[str, Any]] = []
    signers = EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, log=log)  # type: ignore[arg-type]
    assert asyncio.run(signers.consider(b.intent_json())) == []
    assert asyncio.run(signers.consider(b.intent_json())) == []
    assert b.calls == [], "no look and no signature while the engine's picture fails"
    assert len(log) == 1 and log[0]["action"] == "wait" and "keep_out_tracks" in log[0]["reason"]


def test_the_proxy_leaves_engagements_to_its_signers():
    class Inbox:
        def try_get(self, path):
            return {"pending": [{"id": 1, "kind": "engage", "expires_t": 1}, {"id": 2, "kind": "orbit", "expires_t": 2}]}

    decided: list[int] = []
    signers = EngagementSigners(Inbox(), ["op-2", "op-3"])  # type: ignore[arg-type]
    proxy = ProxyOperator(Inbox(), ProxyParams(attend_s=0.01, attend_sigma=0.0), seed=1, operator="op-1", signers=signers, note_prefix="scripted")  # type: ignore[arg-type]

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
    assert proxy.operator == "op-1" and proxy.describe()["engagement_signers"]["operators"] == ["op-2", "op-3"]


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
    env = {"MURMUR_HTTP_PORT": "9065", "MURMUR_EVAL_TRUTH": "1", "MURMUR_SIM_TOKEN": "theirs", "MURMUR_CONTACTS": "x", "PATH": "/bin"}

    async def make():
        return R.Runner(hw, S.resolve_modes(hw, operator="autoplay", ai="playbook"), env, {"ai": "playbook", "checked": False})

    runner = asyncio.run(make())
    removed = runner.fresh_station()
    assert len(removed) == 2 and not (audit / ".last-run-9065").exists() and not (audit / ".resume-9065.json").exists()
    assert (audit / ".last-run-8765").exists(), "only this port's pointers"
    e = runner.stack_env()
    assert e["MURMUR_AUTH"] == "dev" and e["MURMUR_RESUME"] == "0" and e["MURMUR_RUN_ID"] == runner.run_id
    assert e["MURMUR_OPERATORS"] == "op-1,op-2,op-3,op-planner" and e["MURMUR_ENGAGE_OPERATORS"] == "op-1,op-2,op-3"
    assert e["MURMUR_AGENTS"] == "agent-watch,agent-planner,agent-scripted"
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
    runner.orders = [{"id": "gate-patrol", "task": "patrol", "vehicles": ["MARLIN 1"], "intent_id": 1, "state": "dispatched"}]
    runner.autoplay_log.append({"t": 1.0, "intent_id": 4, "action": "approve", "reason": "nothing speaks against it", "result": "dispatched"})
    runner.summary()
    text = (runner.out / "summary.txt").read_text()
    assert "opening order gate-patrol" in text and "murmur-replay" in text and "op-1 approve intent 4: dispatched" in text
