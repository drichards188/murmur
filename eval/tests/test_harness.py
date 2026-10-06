"""The harness around the metrics: statistics, specs, the proxy's rule book, port blocks,
storyline resolution, the model adapter's cost guard (no API is called), the report.

    .venv/bin/python -m pytest eval/tests
"""

from __future__ import annotations

import json
import math
import threading
from pathlib import Path

import pytest

from eval import REPO
from eval import ports as P
from eval import proxy as X
from eval import stats as S
from eval import storyline as Y
from eval.arms import ARMS
from eval.spec import SpecError, load_spec

from .test_metrics import Log, neutral_target, propose

# ---- statistics --------------------------------------------------------------------------------


def test_wilson():
    assert S.wilson(0, 0) == (None, None, None)
    r, lo, hi = S.wilson(5, 10)
    assert r == 0.5 and lo == pytest.approx(0.2366, abs=1e-3) and hi == pytest.approx(0.7634, abs=1e-3)
    r, lo, hi = S.wilson(0, 6)
    assert r == 0.0 and lo == 0.0 and hi == pytest.approx(0.390, abs=1e-3)
    r, lo, hi = S.wilson(6, 6)
    assert hi == pytest.approx(1.0) and lo == pytest.approx(0.610, abs=1e-3)


def test_t_interval_and_summary():
    m, lo, hi = S.t_interval([0.5, 0.5, 0.5])
    assert (m, lo, hi) == (0.5, 0.5, 0.5)
    m, lo, hi = S.t_interval([0.0, 1.0])
    assert m == 0.5 and lo == pytest.approx(0.5 - 12.7062047 * (2**0.5 / 2) / 2**0.5, abs=1e-6)
    assert S.t_interval([1.0]) == (1.0, None, None)
    s = S.summary([3, 1, 2, None])
    assert s["n"] == 3 and s["median"] == 2 and s["min"] == 1 and s["max"] == 3


def test_spearman():
    assert S.spearman([1, 2, 3, 4], [10, 20, 30, 40]) == pytest.approx(1.0)
    assert S.spearman([1, 2, 3, 4], [4, 3, 2, 1]) == pytest.approx(-1.0)
    assert S.spearman([1, 1, 1], [1, 2, 3]) is None
    assert S.spearman([1, 2], [1, 2]) is None


# ---- specs -------------------------------------------------------------------------------------


def test_every_shipped_spec_loads():
    specs = sorted((REPO / "eval" / "scenarios").glob("*.toml")) + sorted((REPO / "eval" / "scenarios" / "engagement").glob("*.toml"))
    assert len(specs) >= 6
    for p in specs:
        s = load_spec(p)
        assert s.steps, p
        assert s.fleet.exists() and s.policy.exists()
        if p.parent.name == "engagement":
            assert "engage" in s.requires and s.placeholder


def test_bad_specs_are_refused(tmp_path: Path):
    base = 'fleet = "eval/fleets/gate.toml"\n'
    cases = {
        "kind": base + '[[step]]\nkind = "teleport"\n',
        "link": base + '[[step]]\nkind = "link"\nvehicles = {callsign = "BLADE 1"}\ncut = true\n',
        "task": base + '[[step]]\nkind = "propose"\ntask = "strike"\nvehicles = {callsign = "BLADE 1"}\n',
        "label": base + '[[step]]\nkind = "propose"\ntask = "orbit"\nvehicles = {callsign = "BLADE 1"}\nlabel = "maybe"\n',
        "fleet": 'fleet = "nope.toml"\n',
        "dup": base + '[[step]]\nid = "a"\nkind = "note"\n[[step]]\nid = "a"\nkind = "note"\n',
    }
    for name, text in cases.items():
        p = tmp_path / f"{name}.toml"
        p.write_text(text)
        with pytest.raises(SpecError):
            load_spec(p)


# ---- arms and the proxy ------------------------------------------------------------------------


def test_arms():
    assert ARMS["rules-only"].supervisor == "auto-approve"
    assert ARMS["rules-only-reject"].supervisor == "auto-reject"
    assert ARMS["human-proxy"].proposer == "human" and ARMS["human-proxy"].supervisor == "proxy"
    # the agent arm makes its own proposals from the picture (eval/agent.py AutonomousAgent)
    assert ARMS["agent+human-proxy"].proposer == "agent-autonomous" and ARMS["agent+human-proxy"].supervisor == "proxy"
    assert ARMS["agent+model"].external_model
    assert ARMS["study"].web and ARMS["study"].supervisor == "participant"


def test_proxy_presets():
    p = X.proxy_params({}, None)
    assert p.preset == "default" and p.diligence == 0.8
    p = X.proxy_params({"preset": "diligent", "attend_s": 5}, None)
    assert p.diligence == 1.0 and p.attend_s == 5.0
    with pytest.raises(ValueError):
        X.proxy_params({"nonsense": 1}, None)
    with pytest.raises(ValueError):
        X.proxy_params({}, "lazy")


def card(armed: bool = True, center=(-122.47, 37.81), radius_m: float = 300.0, unverified: bool = False, failed: bool = False) -> dict:
    """An intent as `/api/inbox` serves it (params carry `radius_m`)."""
    outcome = "fail" if failed else "unverified" if unverified else "pass"
    return {
        "id": 1,
        "rationale": "Track T-0001 is closing",
        "params": {"center": list(center), "radius_m": radius_m},
        "checks": [{"rule": "readiness", "uid": 3, "outcome": outcome}],
        "policy": {"facts": {"armed": ["3"] if armed else []}},
    }


def track(**kw) -> dict:
    t = {
        "id": "T-0001",
        "state": "active",
        "affiliation": "suspect",
        "affiliation_confidence": 0.8,
        "position": [-122.47, 37.81],
        "identity": {"ais": None},
    }
    t.update(kw)
    return t


def test_proxy_rule_book():
    """The generic rule book (eval/evidence.py) as the proxy applies it to a card and the
    tracks the C2 shows."""
    p = X.ProxyParams()
    assert X.judge(card(), [track()], p).action == "approve"
    # no track within the task's radius of the aim point
    assert X.judge(card(), [], p).action == "reject"
    assert X.judge(card(center=(-122.44, 37.82)), [track()], p).action == "reject"  # 2.8 km away
    # the nearest track shown stale, or with a low affiliation confidence
    assert X.judge(card(), [track(state="stale")], p).action == "reject"
    assert X.judge(card(), [track(affiliation="unknown", affiliation_confidence=0.29)], p).action == "reject"
    # ... unless an operator declared it
    declared = track(affiliation="hostile", affiliation_confidence=0.2, declared={"affiliation": "hostile"})
    assert X.judge(card(), [declared], p).action == "approve"
    # any track inside the radius shown neutral or assumed friend
    assert X.judge(card(), [track(affiliation="neutral", affiliation_confidence=0.95)], p).action == "reject"
    friend = track(id="T-0002", affiliation="assumed_friend", position=[-122.4720, 37.8100])  # about 180 m off
    assert X.judge(card(), [track(), friend], p).action == "reject"
    assert X.judge(card(radius_m=100.0), [track(), friend], p).action == "approve"
    # a failed or unverified check, armed or not
    assert X.judge(card(unverified=True), [track()], p).action == "reject"
    assert X.judge(card(armed=False, failed=True), [], p).action == "reject"
    # an unarmed task is judged on its checks only
    assert X.judge(card(armed=False), [track(affiliation="neutral")], p).action == "approve"
    # the tracks looked at are the ones within the radius, nearest first
    j = X.judge(card(), [friend, track()], p)
    assert j.tracks == ["T-0001", "T-0002"]


def test_rule_book_is_the_one_the_metrics_use():
    from eval import evidence as E

    near = [{"id": "T-1", "affiliation": "suspect", "affiliation_confidence": 0.9, "state": "active", "distance_m": 20.0}]
    assert E.evidence(True, [], [-122.47, 37.81], 300.0, near) == []
    assert E.evidence(True, [], [-122.47, 37.81], 300.0, []) == ["no track the C2 holds is within 300 m of the aim point"]
    low = [{**near[0], "affiliation_confidence": 0.3}]
    assert E.evidence(True, [], [-122.47, 37.81], 300.0, low)
    assert E.RULEBOOK_VERSION and E.C_MIN == 0.5


def test_shown_on_mirrors_the_console():
    i = {"checks": [{"rule": "readiness", "uid": "4"}, {"rule": "policy"}], "why": ["policy armed", "armed platform: vehicle 4"], "rationale": "x"}
    assert X.shown_on(i) == {"checks": ["readiness:4", "policy"], "warnings": ["armed platform: vehicle 4"], "rationale": True, "tracks": []}


# ---- ports -------------------------------------------------------------------------------------


def test_port_blocks_stay_in_their_ranges():
    for i in range(P.MAX_BLOCKS):
        b = P.PortBlock(i)
        assert 35000 <= b.vehicle_base and b.vehicle_base + P.BLOCK - 1 <= 35999
        assert 36000 <= b.bridge_base and b.bridge_base + P.BLOCK - 1 <= 36999
        for port in b.tcp_ports():
            assert 10600 <= port <= 10699
    assert all(10600 <= p <= 10699 for p in P.TIDEWATER_PORTS)
    assert not set(P.TIDEWATER_PORTS) & {p for i in range(P.MAX_BLOCKS) for p in P.PortBlock(i).tcp_ports()}


def test_allocator_never_hands_out_a_block_twice(monkeypatch, tmp_path: Path):
    monkeypatch.setattr(P, "LOCK_DIR", tmp_path)
    monkeypatch.setattr(P, "listening_ports", lambda: (set(), set()))
    monkeypatch.setattr(P, "_bindable", lambda port, kind: True)
    monkeypatch.setattr(P, "ephemeral_range", lambda: (60000, 60999))
    a = P.Allocator(max_blocks=8)
    got: list[P.BlockLease] = []

    def take():
        got.append(a.allocate())

    threads = [threading.Thread(target=take) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert sorted(bl.block.index for bl in got) == list(range(8))
    with pytest.raises(RuntimeError):
        a.allocate()
    a.release(next(bl for bl in got if bl.block.index == 3))
    assert a.allocate().block.index == 3


def test_a_second_runner_cannot_take_a_leased_block(monkeypatch, tmp_path: Path):
    """The lock file, not this process's memory, reserves a block: another allocator (as
    another batch would) is refused it while the lease is held."""
    monkeypatch.setattr(P, "LOCK_DIR", tmp_path)
    monkeypatch.setattr(P, "listening_ports", lambda: (set(), set()))
    monkeypatch.setattr(P, "_bindable", lambda port, kind: True)
    monkeypatch.setattr(P, "ephemeral_range", lambda: (60000, 60999))
    first = P.Allocator(max_blocks=2).allocate()
    other = P.Allocator(max_blocks=2)
    assert other.allocate().block.index != first.block.index
    with pytest.raises(RuntimeError):
        other.allocate()
    assert "reserved by another runner" in other.last_refusals.values()


def test_blocks_inside_the_ephemeral_range_are_refused():
    b = P.PortBlock(0)
    assert P.why_not_free(b, set(), set(), (35050, 35060)) is not None
    assert P.overlaps_ephemeral(range(35000, 35100), (32768, 60999))
    assert not P.overlaps_ephemeral(range(35000, 35100), (40000, 60999))


def test_track_selectors():
    tracks = [
        {"id": "T-0001", "affiliation": "neutral", "state": "active", "position": [-122.47, 37.81], "domain": "surface", "last_seen_t": 10},
        {"id": "T-0002", "affiliation": "suspect", "state": "active", "position": [-122.50, 37.80], "domain": "surface", "last_seen_t": 9},
        {"id": "T-0003", "affiliation": "unknown", "state": "stale", "position": [-122.48, 37.81], "domain": "surface", "last_seen_t": 1},
    ]
    assoc = {"fastboat": "T-0002"}
    assert Y.select_track({"contact": "fastboat"}, tracks, assoc, 20)["id"] == "T-0002"
    assert Y.select_track("T-0001", tracks, assoc, 20)["id"] == "T-0001"
    assert Y.select_track({"select": "most_suspicious"}, tracks, assoc, 20)["id"] == "T-0002"
    assert Y.select_track({"select": "stalest", "include_stale": True}, tracks, assoc, 20)["id"] == "T-0003"
    near = Y.select_track({"select": "nearest", "near": [-122.47, 37.81], "include_stale": True}, tracks, assoc, 20)
    assert near["id"] == "T-0001"
    assert Y.select_track({"select": "most_suspicious", "affiliation": ["hostile"]}, tracks, assoc, 20) is None


def test_vehicle_selectors():
    vs = [
        {"uid": 4, "identity": {"callsign": "BLADE 1", "platform": "switchblade"}},
        {"uid": 5, "identity": {"callsign": "BLADE 2", "platform": "switchblade"}},
        {"uid": 1, "identity": {"callsign": "PUMA 1", "platform": "puma"}},
    ]
    assert [v["uid"] for v in Y.select_vehicles({"callsign": "BLADE 2"}, vs)] == [5]
    assert [v["uid"] for v in Y.select_vehicles({"platform": "switchblade"}, vs)] == [4]
    assert [v["uid"] for v in Y.select_vehicles({"platform": "switchblade", "all": True}, vs)] == [4, 5]
    assert [v["uid"] for v in Y.select_vehicles({"uids": [1]}, vs)] == [1]
    with pytest.raises(Y.StepError):
        Y.select_vehicles({"callsign": "BLADE 9"}, vs)


def test_offset_and_association():
    p = Y.offset([-122.47, 37.81], 1000, 0)
    assert Y.haversine_m(-122.47, 37.81, p[0], p[1]) == pytest.approx(1000, rel=0.01)
    truth = {"association": [{"contact_id": "a", "track_id": "T-0001"}], "truth": {"message": {"contacts": [{"id": "a", "lon": 1, "lat": 2}]}}}
    assoc, contacts = Y.association_map(truth)
    assert assoc == {"a": "T-0001"} and contacts["a"]["lat"] == 2


# ---- the model adapter (no API call) -----------------------------------------------------------


def test_model_arm_needs_explicit_permission(monkeypatch):
    from eval.agent import ProposeError
    from eval.model_agent import ModelProposer

    monkeypatch.delenv("MURMUR_EVAL_MODEL", raising=False)
    with pytest.raises(ProposeError):
        ModelProposer("http://127.0.0.1:1/mcp")


def test_cost_guard(tmp_path: Path):
    from eval.model_agent import DEFAULT_MODEL, CostGuard, CostGuardExceeded

    assert DEFAULT_MODEL == "claude-opus-5-5"

    class Usage:
        def __init__(self, i, o, iterations=None):
            self.input_tokens, self.output_tokens = i, o
            self.cache_read_input_tokens = 0
            self.cache_creation_input_tokens = 0
            self.iterations = iterations

    g = CostGuard("claude-opus-5-5", max_usd=0.50)
    # 2 characters a token: 3,000 chars is 1,500 tokens x $4 plus 4,096 x $20 per million
    assert g.estimate(3_000, 4_096) == pytest.approx((1_500 * 4 + 4_096 * 20) / 1e6)
    g.check(g.estimate(3_000, 4_096))
    g.add(Usage(10_000, 2_000))  # 0.04 + 0.04
    assert g.usd == pytest.approx(0.08)
    # the next call's worst case would pass the cap: refused before it
    with pytest.raises(CostGuardExceeded):
        g.check(0.45)
    assert g.tripped and g.refused == 1
    with pytest.raises(CostGuardExceeded):
        g.check(0.0)  # stays tripped
    # with the server-side fallback on: the dearest list price, two attempts (a request
    # declined mid-stream bills the partial first attempt and the fallback's)
    fb = CostGuard("claude-opus-5-5", max_usd=10.0, fallbacks=True)
    assert fb.estimate(3_000, 4_096) == pytest.approx(2 * (1_500 * 10 + 4_096 * 50) / 1e6)
    unknown = CostGuard("some-future-model", max_usd=1.0)
    assert unknown.price == (10.0, 50.0)  # priced at the most expensive row
    # a fallback billed at the model that served it, per iteration
    f = CostGuard("claude-opus-5-5", max_usd=10.0)
    cost = f.add(Usage(0, 0, iterations=[{"model": "claude-opus-5-5", "input_tokens": 1000, "output_tokens": 0}, {"model": "claude-fable-5-1", "input_tokens": 1000, "output_tokens": 1000}]))
    assert cost == pytest.approx((1000 * 4 + 1000 * 10 + 1000 * 50) / 1e6)
    assert f.log[-1]["billed"] == ["claude-opus-5-5", "claude-fable-5-1"]
    # no iterations: the response's own model
    assert f.add(Usage(1000, 0), "claude-fable-5") == pytest.approx(1000 * 10 / 1e6)


def test_cost_guard_batch_ledger(tmp_path: Path):
    from eval.model_agent import CostGuard, CostGuardExceeded, Ledger

    class Usage:
        input_tokens, output_tokens = 100_000, 0

    ledger = Ledger(tmp_path / "ledger.json", max_usd=0.5)
    a = CostGuard("claude-opus-5-5", max_usd=1.0, ledger=ledger)
    b = CostGuard("claude-opus-5-5", max_usd=1.0, ledger=Ledger(tmp_path / "ledger.json", max_usd=0.5))
    # a's check reserves its worst case in the same locked step: b, another run of the
    # batch, can't pass a check the batch could only afford once
    a.check(0.3)
    with pytest.raises(CostGuardExceeded, match="batch"):
        b.check(0.3)
    a.add(Usage())  # billed 0.40: the reservation is settled at what was billed
    assert ledger.spent() == pytest.approx(0.4)
    c = CostGuard("claude-opus-5-5", max_usd=1.0, ledger=Ledger(tmp_path / "ledger.json", max_usd=0.5))
    c.check(0.05)
    c.failed()  # the call raised: its worst case counts as spent
    assert ledger.spent() == pytest.approx(0.45) and c.usd == pytest.approx(0.05)


def test_tool_defs():
    from eval.model_agent import tool_defs

    class T:
        name = "get_tracks"
        description = "tracks"
        inputSchema = {"$schema": "x", "type": "object", "properties": {"include_stale": {"type": "boolean"}}}

    d = tool_defs([T()])
    assert d == [{"name": "get_tracks", "description": "tracks", "input_schema": {"type": "object", "properties": {"include_stale": {"type": "boolean"}}}}]


# ---- the report --------------------------------------------------------------------------------


def test_report_from_a_synthetic_batch(tmp_path: Path):
    from eval.report import build

    hostile = {"id": "fastboat", "affiliation": "hostile", "distance_m": 2500.0}
    rec = propose(
        "wrong-centre", 1,
        target={"track_id": "T-0001", "contact_id": "fastboat", "truth_present": True, "truth_affiliation": "hostile", "truth_position": [-122.47, 37.81]},
        context={"radius_m": 300.0, "display_near": [], "truth_near": [], "aim_to_truth_m": 2500.0},
        request={"task": "orbit", "uids": [4], "center": [-122.44, 37.82], "radius_m": 300.0},
    )
    assert hostile
    for arm, approve in (("rules-only", True), ("human-proxy", False)):
        for seed in (1, 2):
            d = tmp_path / "plausible-wrong" / arm / f"seed-{seed}"
            d.mkdir(parents=True)
            log = Log()
            log.created(1, 0)
            log.checked(1, 0)
            log.pending(1, 0)
            if approve:
                log.approve(1, 1, by="op-auto")
            else:
                log.view(1, 10, "decide", "reject", 4000, tracks=["T-0001"])
                log.reject(1, 10)
            (d / "audit.jsonl").write_text("\n".join(json.dumps(e) for e in log.events) + "\n")
            (d / "storyline.jsonl").write_text(json.dumps(rec) + "\n")
            info = {"scenario": "plausible-wrong", "arm": arm, "seed": seed, "status": "ok", "operators": ["op-1", "op-auto"], "replay": {"ran": True, "ok": True, "mismatches": 0}, "warnings": [],
                    "provenance": {"short": "abc1234", "dirty": False, "binaries": {"murmur-bridge": "f" * 64}}}
            if not approve:
                info["decisions"] = [{"intent_id": 1, "action": "reject", "diligent": True, "if_read": "reject", "p_read": 0.8}]
            (d / "run.json").write_text(json.dumps(info))
    # a failed run and a job that never wrote run.json: listed, never scored
    bad = tmp_path / "plausible-wrong" / "human-proxy" / "seed-3"
    bad.mkdir(parents=True)
    (bad / "run.json").write_text(json.dumps({"scenario": "plausible-wrong", "arm": "human-proxy", "seed": 3, "status": "failed", "error": "the fleet was not ready by the origin"}))
    jobs = [{"scenario": "plausible-wrong", "arm": a, "seed": s, "dir": f"plausible-wrong/{a}/seed-{s}"} for a in ("rules-only", "human-proxy") for s in (1, 2, 3)]
    (tmp_path / "batch.json").write_text(json.dumps({"id": "test", "seeds": [1, 2, 3], "jobs": jobs, "skipped": [{"scenario": "engage-x", "reason": "requires ['engage'] (placeholder)"}]}))
    out = build(tmp_path)
    text = out.read_text()
    assert "Not human data" in text and "Monte Carlo variation of the model" in text
    assert "Should-reject dispatched, the C2 showed it" in text or "By construction, rules-only" in text
    assert "engage-x" in text
    assert "the fleet was not ready by the origin" in text and "plausible-wrong/rules-only/seed-3" in text
    assert "code abc1234" in text
    assert "<script" not in text and "http://" not in text and "https://" not in text  # self-contained
    summary = json.loads((tmp_path / "summary.json").read_text())
    assert summary["not_human_data"] is True and summary["runs_scored"] == 4 and len(summary["runs_excluded"]) == 2
    g = summary["groups"]["plausible-wrong|human-proxy"]
    assert g["sr_shown"]["k"] == 2 and g["sr_shown"]["seed_mean"] == 1.0
    assert g["read_shown"] == {"n": 2, "read": 2, "fired": 2, "caught": 2, "p_read_mean": 0.8}
    assert summary["groups"]["plausible-wrong|rules-only"]["sr_shown"]["k"] == 0


def test_neutral_target_helper_is_a_should_reject():
    from eval.metrics import Trail, resolve_label

    assert resolve_label(propose("x", 1, target=neutral_target()), Trail(id=1, armed=["4"]))[0] == "should_reject"


def test_parse_ss():
    udp = (
        "State  Recv-Q Send-Q  Local Address:Port  Peer Address:PortProcess\n"
        "UNCONN 0      0             0.0.0.0:36400      0.0.0.0:*\n"
        "UNCONN 0      0           127.0.0.1:35001      0.0.0.0:*\n"
        "UNCONN 0      0                [::]:5353          [::]:*\n"
    )
    tcp = "State  Recv-Q Send-Q  Local Address:Port  Peer Address:PortProcess\nLISTEN 0      5           127.0.0.1:10600       0.0.0.0:*\n"
    assert P.parse_ss(udp) == {36400, 35001, 5353}
    assert P.parse_ss(tcp) == {10600}
    assert P.parse_ss("") == set()
    blocked = P.PortBlock(0)
    assert not P.block_is_free(blocked, {35001}, set())
    assert not P.block_is_free(blocked, set(), {10600})


# ---- the model adapter's loop, against fakes (no API, no MCP server) ---------------------------


class _Block:
    def __init__(self, type: str, **kw):
        self.type = type
        for k, v in kw.items():
            setattr(self, k, v)


class _Resp:
    def __init__(self, content, stop_reason="end_turn"):
        self.content = content
        self.stop_reason = stop_reason
        self.usage = type("U", (), {"input_tokens": 1000, "output_tokens": 100, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0})()


class _FakeMessages:
    def __init__(self, script):
        self.script = list(script)
        self.calls = []

    async def create(self, **kw):
        self.calls.append(kw)
        return self.script.pop(0)


class _FakeClient:
    def __init__(self, script):
        self.messages = _FakeMessages(script)
        self.beta = type("B", (), {"messages": self.messages})()


class _ToolResult:
    def __init__(self, body, is_error=False):
        self.structured_content = body
        self.is_error = is_error
        self.content = []


class _FakeMcp:
    def __init__(self):
        self.called = []

    async def call_tool(self, name, args):
        self.called.append((name, args))
        if name == "propose_intent":
            return _ToolResult({"id": 7, "state": "pending_approval", "dispatched": False})
        return _ToolResult({"tracks": [{"id": "T-0001", "affiliation": "suspect"}]})


def _model(monkeypatch, script):
    import sys

    from eval.model_agent import ModelProposer

    sys.path.insert(0, str(REPO / "agents"))
    monkeypatch.setenv("MURMUR_EVAL_MODEL", "1")
    monkeypatch.setenv("MURMUR_EVAL_MAX_USD", "1.0")
    m = ModelProposer("http://127.0.0.1:1/mcp", brief="test", client=_FakeClient(script))
    m._session = type("S", (), {"session": _FakeMcp()})()
    return m


def test_model_proposes_through_the_relayed_tools(monkeypatch):
    import asyncio

    script = [
        _Resp([_Block("tool_use", id="u1", name="get_tracks", input={})], "tool_use"),
        _Resp([_Block("tool_use", id="u2", name="propose_intent", input={"task": "orbit", "uids": [4], "rationale": "T-0001 suspect"})], "tool_use"),
        _Resp([_Block("text", text="Proposed intent 7.")]),
    ]
    m = _model(monkeypatch, script)
    out = asyncio.run(m.propose({"task": "orbit", "uids": [4], "center": [-122.5, 37.8]}, "T-0001 is closing"))
    assert out["id"] == 7 and not out["dispatched"]
    assert [c[0] for c in m._session.session.called] == ["get_tracks", "propose_intent"]
    calls = m._client.messages.calls
    assert calls[0]["model"] == "claude-opus-5-5"
    assert calls[0]["betas"] == ["server-side-fallback-2026-07-01"] and calls[0]["fallbacks"] == "default"
    assert "thinking" not in calls[0] and "tool_choice" not in calls[0]
    # append-only: the second request starts with the first request's messages unchanged
    assert calls[1]["messages"][: len(calls[0]["messages"])] == calls[0]["messages"]
    assert m.guard.calls == 3


def test_model_declines_and_the_guard_stops_it(monkeypatch):
    import asyncio

    m = _model(monkeypatch, [_Resp([_Block("text", text="DECLINE: the track is neutral")])])
    out = asyncio.run(m.propose({"task": "orbit", "uids": [4]}, "x"))
    assert out["id"] is None and out["declined"].startswith("DECLINE")
    m.guard.max_usd = 0.0
    out = asyncio.run(m.propose({"task": "orbit", "uids": [4]}, "x"))
    assert out["id"] is None and "cost guard" in out["declined"]
    assert m.guard.calls == 1  # no call after the cap


# ---- the engagement scenario suite (Phase E) ---------------------------------------------------


def test_engagement_suite_runs_clean(tmp_path: Path):
    """The real suite through the real murmur-scenarios binary (skipped without a release
    build): every scenario ends as designed at seed 1 without noise, every log replays,
    every forged log is caught, and the harness's own invariants find nothing."""
    from eval import engage as E

    if not E.BINARY.exists():
        pytest.skip("no release build of murmur-scenarios")
    ps = E.run_once(1, 0.0, tmp_path / "s1")
    assert not ps.get("error"), ps
    rs = ps["results"]
    assert len(rs) >= 30
    assert all(r["ok"] for r in rs), [r["scenario"] for r in rs if not r["ok"]]
    assert all(r["replay_ok"] for r in rs)
    assert all(r["tamper_detected"] for r in rs if r["tamper"] and not r["tamper_not_applicable"])
    assert sum(r["harness"]["violations"] for r in rs) == 0
    released = sum(r["harness"]["engage"]["released"] for r in rs)
    assert released > 0  # the two-signer check had effects to check
    summary = E.summarize([ps])
    assert summary["totals"]["runs"] == len(rs) and summary["totals"]["violations"] == 0
    # logs of runs that went as designed are not kept
    assert not list((tmp_path / "s1" / "logs").glob("*.jsonl"))


# ---- identification: rule against manual (rules 9) ---------------------------------------------


def _ev(t, typ, **kw):
    return {"t": t, "type": typ, **kw}


def test_ident_generates_situations_with_their_truth(tmp_path: Path):
    """The generator writes seeded situations (the same for a seed, others for another), each
    naming no policy (every arm is the shipped policy changed only by its flags), with the
    truth of every track and who acted."""
    import tomllib

    from eval import ident as I

    a = I.generate(3, 8, tmp_path / "a")
    b = I.generate(3, 8, tmp_path / "b")
    c = I.generate(4, 8, tmp_path / "c")
    assert [m["truth"] for m in a] == [m["truth"] for m in b]
    assert (tmp_path / "a" / "sit-003-000.toml").read_text() == (tmp_path / "b" / "sit-003-000.toml").read_text()
    assert (tmp_path / "a" / "sit-003-000.toml").read_text() != (tmp_path / "c" / "sit-004-000.toml").read_text()
    for m in a:
        sc = tomllib.loads((tmp_path / "a" / f"{m['name']}.toml").read_text())
        assert "policy" not in sc
        acts = [st for st in sc["step"] if st["action"] == "act"]
        assert len(acts) == m["acts"]
        if m["author"] != "untracked":
            assert m["truth"][m["author"]] == "hostile" and acts[0]["by_track"] == m["author"]
        for t in sc.get("track", []):
            assert t["truth"] == m["truth"][t["id"]]
            assert len(t["velocity"]) == 2
    # contacts move as MOTION states: some hold, some are under way
    speeds = [
        math.hypot(*t["velocity"])
        for m in a
        for t in tomllib.loads((tmp_path / "a" / f"{m['name']}.toml").read_text()).get("track", [])
    ]
    assert any(v == 0 for v in speeds) and any(v > 0 for v in speeds)
    # the untracked-author share is a stated dimension
    many = I.generate(5, 60, tmp_path / "d", untracked_share=0.5)
    share = sum(m["author"] == "untracked" for m in many) / len(many)
    assert 0.3 < share < 0.7, share
    assert all(m["untracked_share"] == 0.5 for m in many)


def test_ident_counts_a_persons_decisions_alike_in_every_arm():
    """Workload from the proxy's own tally: one decision per item it got to the end of,
    whatever it decided (an act it could not attribute with the rule off counts as a decision,
    as a flag it dismissed does with the rule on); consequential ones are declarations,
    confirmations and revocations, in every arm."""
    from eval import ident as I

    base = {"replay_ok": True, "author_tracked": True, "acts": 2, "declarations": [], "author_declared": False, "missed": True,
            "first_right_after_s": None, "revocations": [], "confirmations": 0, "resolutions": 0, "person_declarations": 0,
            "rule_declarations": 0, "flags": 0, "effects_right": 0, "effects_wrong_target": 0, "effects_other_hostile": 0,
            "first_effect_after_s": None}
    manual = {**base, "arm": "manual", "proxy": {"items": 2, "looks": 3, "reads": 2, "decisions": 2, "declared": 1, "confirmed": 0,
                                                  "confirmed_unread": 0, "revoked": 0, "dismissed": 0, "nothing": 1}}
    rule = {**base, "arm": "rule", "proxy": {"items": 2, "looks": 2, "reads": 1, "decisions": 2, "declared": 0, "confirmed": 1,
                                              "confirmed_unread": 1, "revoked": 0, "dismissed": 1, "nothing": 0}}
    s = I.summarize([{"arm": "manual", "seed": 1, "results": [manual]}, {"arm": "rule", "seed": 1, "results": [rule]}])["arms"]
    assert (s["manual"]["items"], s["manual"]["decisions"], s["manual"]["consequential"], s["manual"]["decided_nothing"]) == (2, 2, 1, 1)
    assert (s["rule"]["items"], s["rule"]["decisions"], s["rule"]["consequential"], s["rule"]["dismissed"], s["rule"]["confirmed_unread"]) == (2, 2, 1, 1, 1)


def test_ident_labels_a_situation_from_its_log():
    """A situation's log, labelled: the author declared by rule at the act is correct; a
    neutral declared by the person is a false hostile declaration, shown by the C2 (another
    track shared the attribution); an effect released on the neutral is an effect on a wrong
    target; a revocation of the author's declaration is a wrong one."""
    from eval import ident as I

    t0 = 1_000_000
    meta = {"name": "s", "author": "T-0001", "truth": {"T-0001": "hostile", "T-0002": "neutral"}}
    events = [
        _ev(t0, "run.started"),
        _ev(t0 + 10_000, "ident.act", act={"act": "A-1"}, candidates=[{"track": "T-0001", "confidence": 0.95}, {"track": "T-0002", "confidence": 0.04}], inputs={"tracks": {}}, verdict={"verdict": "declare"}),
        _ev(t0 + 10_000, "ident.declared", declaration={"track": "T-0001", "affiliation": "hostile", "by": {"kind": "rule", "evidence": {"act": "A-1"}}}),
        _ev(t0 + 20_000, "ident.act", act={"act": "A-2"}, candidates=[{"track": "T-0002", "confidence": 0.6}, {"track": "T-0001", "confidence": 0.3}], inputs={"tracks": {}}, verdict={"verdict": "flag"}),
        _ev(t0 + 20_000, "ident.flagged", flag={"id": 1}),
        _ev(t0 + 30_000, "ident.declared", declaration={"track": "T-0002", "affiliation": "hostile", "by": {"kind": "operator", "note": "proxy: act A-2"}}),
        _ev(t0 + 31_000, "intent.created", intent={"id": 1, "kind": "engage", "params": {"track": "T-0002"}}),
        _ev(t0 + 90_000, "engage.effect", intent=1),
        _ev(t0 + 95_000, "ident.revoked", track="T-0001"),
    ]
    r = I.analyze({"replay_ok": True}, events, meta, "rule", 1)
    by = {d["track"]: d for d in r["declarations"]}
    assert by["T-0001"]["verdict"] == "correct" and by["T-0001"]["by"] == "rule" and by["T-0001"]["after_act_s"] == 0
    assert by["T-0002"]["verdict"] == "false_hostile" and by["T-0002"]["by"] == "person" and by["T-0002"]["c2_showed_it"] is True
    assert r["author_declared"] and not r["missed"]
    assert (r["effects_wrong_target"], r["effects_right"]) == (1, 0)
    assert r["revocations"] == [{"track": "T-0001", "right": False}]
    s = I.summarize([{"arm": "rule", "seed": 1, "results": [r]}])["arms"]["rule"]
    assert (s["false_hostile"], s["false_by_person"], s["wrong_c2_showed_it"], s["revocations_wrong"]) == (1, 1, 1, 1)
    assert s["looks"] == 1 + 1  # a rule declaration and a flag
    assert s["person_acts"] == 1 + 1  # the person's declaration and revocation
    # the manual arm looks at every act
    r = I.analyze({"replay_ok": True}, events, meta, "manual", 1)
    assert I.summarize([{"arm": "manual", "seed": 1, "results": [r]}])["arms"]["manual"]["looks"] == 2


def test_ident_comparison_runs(tmp_path: Path):
    """The real comparison through the real murmur-scenarios binary (skipped without a release
    build): every arm replays every generated situation; the rule declares at the act, the
    manual arm later, and the arms differ only by their flags."""
    from eval import ident as I

    if not I.BINARY.exists():
        pytest.skip("no release build of murmur-scenarios")
    comp = I.run_comparison(tmp_path, [1], arms=["manual", "rule"], situations=6, untracked_shares=[0.2])
    s = comp["summary"]
    assert not s["errors"], s["errors"]
    manual, rule = s["arms"]["manual"], s["arms"]["rule"]
    assert manual["situations"] == rule["situations"] == 6
    assert manual["replay_ok"] == 6 and rule["replay_ok"] == 6
    assert rule["act_to_declaration_median_s"] in (None, 0.0)
    assert manual["act_to_declaration_median_s"] is None or manual["act_to_declaration_median_s"] > 0
    # the proxy's tally reached the summary: every item put before it ends in one decision
    for r in (manual, rule):
        assert r["items"] > 0 and r["decisions"] <= r["items"]
    assert list(comp["by_share"]) == ["0.2"]
    assert "Not human data" in I.table(comp)
    assert (tmp_path / "comparison.json").exists()


# ---- policies ----------------------------------------------------------------------------------


def test_eval_policies_follow_the_shipped_policy():
    """eval/policy/*.toml are sim/policy.toml with the documented differences only: every
    human tier leaves `submitter_authorizes` false (base.toml's header), and strict.toml adds
    its `every-vehicle-task` tier before the default. Rules, the queue for unreachable vehicles,
    approval kinds (engagement included) and every other tier field are the shipped ones."""
    import tomllib

    sim = tomllib.loads((REPO / "sim" / "policy.toml").read_text())

    def tier_core(t: dict) -> dict:
        return {k: v for k, v in t.items() if k not in ("submitter_authorizes", "reason")}

    for name, extra in (("base", []), ("strict", ["every-vehicle-task"])):
        pol = tomllib.loads((REPO / "eval" / "policy" / f"{name}.toml").read_text())
        assert pol["rules"] == sim["rules"], name
        assert pol.get("queue") == sim.get("queue"), name
        assert pol["approval"]["kind"] == sim["approval"]["kind"], name
        assert pol["approval"]["expire_s"] == sim["approval"]["expire_s"]
        assert pol["approval"]["submitter_authorizes"] is False
        # rules 10: the step-up factor as shipped (a study run requires "passkey" itself)
        assert pol["approval"].get("step_up_factor") == sim["approval"].get("step_up_factor"), name
        names = [t["name"] for t in pol["tier"]]
        assert [n for n in names if n not in extra] == [t["name"] for t in sim["tier"]], name
        shipped = {t["name"]: t for t in sim["tier"]}
        for t in pol["tier"]:
            if t["name"] in extra:
                continue
            assert tier_core(t) == tier_core(shipped[t["name"]]), (name, t["name"])
            if t["tier"] == "human":
                assert not t.get("submitter_authorizes", False), (name, t["name"])
        # rules 9: identification as shipped
        assert pol.get("identification") == sim.get("identification"), name


def test_the_minimal_approval_profile_differs_only_as_documented():
    """eval/policy/minimal.toml (rules 9) is sim/policy.toml with a rule declaration nominable
    before an operator confirms it, and no step-up besides the engagement signatures; rule
    declaration on; tiers, rules, queue and approval kinds the shipped ones."""
    import tomllib

    sim = tomllib.loads((REPO / "sim" / "policy.toml").read_text())
    pol = tomllib.loads((REPO / "eval" / "policy" / "minimal.toml").read_text())
    for key in ("rules", "queue", "tier"):
        assert pol.get(key) == sim.get(key), key
    assert {k: v for k, v in pol["approval"].items() if k != "step_up_acts"} == sim["approval"]
    assert pol["approval"]["step_up_acts"] == []
    ident = dict(sim["identification"])
    ident["nominate_unconfirmed"] = True
    assert ident["rule_declaration"] is True
    assert pol["identification"] == ident


def test_provenance_counts_code_not_docs():
    from eval import provenance as V

    assert V.is_doc("docs/PLAN.md") and V.is_doc("eval/README.md") and not V.is_doc("eval/metrics.py")
    assert V.code_dirty({"dirty": True, "changed": ["docs/PLAN.md"]}) is False
    assert V.code_dirty({"dirty": True, "changed": ["docs/PLAN.md", "eval/report.py"]}) is True
    assert V.code_id({"short": "abc1234", "dirty": True, "changed": ["docs/PLAN.md"]}) == "abc1234"
    assert V.code_id({"short": "abc1234", "dirty": True, "changed": ["eval/run.py"]}) == "abc1234 (dirty)"
    p = V.collect()
    assert p["commit"] and len(p["commit"]) == 40 and set(p["binaries"]) == set(V.BINARIES)
    assert all(not c.startswith(" ") for c in p["changed"])


def test_provenance_counts_untracked_code(monkeypatch):
    from eval import provenance as V

    status = " M docs/PLAN.md\n?? eval/new_module.py\n"
    monkeypatch.setattr(V, "_git", lambda *a: status if a[0] == "status" else "abc1234")
    p = V.collect()
    assert p["changed"] == ["docs/PLAN.md", "eval/new_module.py"] and p["dirty"] is True


# ---- who the operators were (rules 8) ---------------------------------------------------------


def _run_started(auth: str | None, study: dict | None = None, rules: int = 8) -> dict:
    roster: dict = {"operators": ["op-1", "op-study"], "engage": []}
    if auth:
        roster["auth"] = auth
    if study is not None:
        roster["study"] = study
    return {"seq": 1, "t": 0, "type": "run.started", "rules_version": rules, "roster": roster}


# rules 10: a study's run.started names its participant
STUDY = {"participant": "op-study"}


def _step_up(op: str, auth: str, seq: int) -> dict:
    return {"seq": seq, "t": seq, "type": "operator.action", "operator": op, "session": 3, "action": "step_up", "detail": "", "auth": auth}


def _concurred(op: str, seq: int, step_seq: int) -> dict:
    return {"seq": seq, "t": seq, "type": "approval.concurred", "intent": 1, "by": op, "session": 3, "n": 1, "of": 2, "step_up": {"seq": step_seq}}


def _opened(op: str, auth: str, seq: int) -> dict:
    return {"seq": seq, "t": seq, "type": "operator.action", "operator": op, "session": seq, "action": "session.opened", "detail": "", "auth": auth}


def _signed(op: str, seq: int) -> dict:
    return {"seq": seq, "t": seq, "type": "approval.rejected", "intent": 1, "by": op, "session": 1}


def test_a_runs_auth_mode_and_whether_it_is_evidence_about_people():
    from eval.metrics import auth_of

    ok = {"ran": True, "ok": True, "anchors": "verified"}
    a = auth_of([_run_started("credentials")], ok)
    assert (a["mode"], a["human_control_evidence"], a["why_not"]) == ("credentials", True, [])
    # a log that did not replay is no evidence, whatever it claims
    assert auth_of([_run_started("credentials")], {**ok, "ok": False})["human_control_evidence"] is False
    # review fix 5: nor one whose anchors were not checked, or did not hold
    for anchors in ("not checked", "mismatch"):
        a = auth_of([_run_started("credentials")], {**ok, "anchors": anchors})
        assert a["human_control_evidence"] is False and a["why_not"] == [f"anchors {anchors}"], a
    assert auth_of([_run_started("credentials")], {"ran": True, "ok": True})["anchors"] == "not checked"
    a = auth_of([_run_started("dev")], ok)
    assert (a["mode"], a["human_control_evidence"]) == ("dev", False)
    # earlier rules recorded no mode: ids asserted
    a = auth_of([_run_started(None)], ok)
    assert (a["mode"], a["human_control_evidence"]) == ("none recorded", False)
    assert auth_of([], ok)["human_control_evidence"] is False


def test_only_the_participants_own_acts_are_evidence():
    """Review fix 2: in a study the scripted operators hold harness credentials (`auth:
    scripted`); their acts are never a person's, and the participant counts only signed in
    with their own password."""
    from eval.metrics import auth_of

    ok = {"ran": True, "ok": True, "anchors": "verified"}
    events = [_run_started("credentials", STUDY, 10), _opened("op-1", "scripted", 2), _opened("op-study", "password", 3),
              _signed("op-1", 4), _signed("op-1", 5), _signed("op-study", 6)]
    a = auth_of(events, ok, "op-study")
    assert a["human_control_evidence"] is True, a
    assert (a["people"], a["scripted"]) == (["op-study"], ["op-1"])
    assert (a["participant_acts"], a["acts_by_people"], a["acts_scripted"]) == (1, 1, 2)
    # the participant's id on a scripted credential: not a person, nothing counts
    events[2] = _opened("op-study", "scripted", 3)
    a = auth_of(events, ok, "op-study")
    assert a["human_control_evidence"] is False and a["participant_acts"] == 0
    assert a["why_not"] == ["op-study did not sign in as a person (own password or passkey)"]


def test_a_study_counts_only_with_the_participants_passkey_at_every_step_up():
    """Rules 10 (murmur-437): a study run is evidence only when its log names the study, no
    override allowed password step-ups, and every consequential act of the participant's
    quoted a passkey step-up; a log of earlier rules (no passkeys, no study) is a pilot."""
    from eval.metrics import auth_of

    ok = {"ran": True, "ok": True, "anchors": "verified"}
    base = [_opened("op-1", "scripted", 2), _opened("op-study", "passkey", 3)]
    # a signature by passkey: evidence (a passkey login is a person's)
    events = [_run_started("credentials", STUDY, 10)] + base + [_step_up("op-study", "passkey", 4), _concurred("op-study", 5, 4)]
    a = auth_of(events, ok, "op-study")
    assert a["human_control_evidence"] is True, a
    assert a["participant_steps"] == {"passkey": 1, "password": 0}
    assert a["people"] == ["op-study"]
    # the same signature quoting a password step-up: not evidence, and said why
    events = [_run_started("credentials", STUDY, 10)] + base + [_step_up("op-study", "password", 4), _concurred("op-study", 5, 4)]
    a = auth_of(events, ok, "op-study")
    assert a["human_control_evidence"] is False
    assert a["why_not"] == ["1 of op-study's consequential acts stepped up without a passkey"], a
    # the override allowed passwords: a pilot whatever the acts were
    over = {**STUDY, "password_step_up": "pilot: no authenticators yet"}
    events = [_run_started("credentials", over, 10)] + base + [_step_up("op-study", "passkey", 4), _concurred("op-study", 5, 4)]
    a = auth_of(events, ok, "op-study")
    assert a["human_control_evidence"] is False and "override" in a["why_not"][0], a
    # a log of rules 9 names no study: a pilot
    events = [_run_started("credentials", None, 9)] + base
    a = auth_of(events, ok, "op-study")
    assert a["human_control_evidence"] is False and "names no study" in a["why_not"][0], a
    # the scripted operators' password step-ups are theirs, never the participant's
    events = [_run_started("credentials", STUDY, 10)] + base + [_step_up("op-1", "scripted", 4), _concurred("op-1", 5, 4)]
    assert auth_of(events, ok, "op-study")["human_control_evidence"] is True


def test_a_study_run_counts_only_with_authenticated_operators(tmp_path: Path):
    from eval.report import build

    rec = propose(
        "wrong-centre", 1,
        target={"track_id": "T-0001", "contact_id": "fastboat", "truth_present": True, "truth_affiliation": "hostile", "truth_position": [-122.47, 37.81]},
        context={"radius_m": 300.0, "display_near": [], "truth_near": [], "aim_to_truth_m": 2500.0},
        request={"task": "orbit", "uids": [4], "center": [-122.44, 37.82], "radius_m": 300.0},
    )
    for seed, auth in ((1, "credentials"), (2, "dev")):
        d = tmp_path / "plausible-wrong" / "study" / f"seed-{seed}"
        d.mkdir(parents=True)
        log = Log()
        log.created(1, 0)
        log.checked(1, 0)
        log.pending(1, 0)
        log.reject(1, 10, by="op-study")
        signed_in = [_opened("op-1", "scripted", 2), _opened("op-study", "password", 3)] if auth == "credentials" else []
        started = _run_started(auth, STUDY, 10) if auth == "credentials" else _run_started(auth)
        events = [started] + signed_in + log.events
        (d / "audit.jsonl").write_text("\n".join(json.dumps(e) for e in events) + "\n")
        (d / "storyline.jsonl").write_text(json.dumps(rec) + "\n")
        info = {"scenario": "plausible-wrong", "arm": "study", "seed": seed, "status": "ok", "operators": ["op-1", "op-study"],
                "replay": {"ran": True, "ok": True, "mismatches": 0, "anchors": "verified"}, "warnings": [], "auth": auth,
                "provenance": {"short": "abc1234", "dirty": False, "binaries": {"murmur-bridge": "f" * 64}}}
        (d / "run.json").write_text(json.dumps(info))
    (tmp_path / "batch.json").write_text(json.dumps({"id": "study", "seeds": [1, 2], "jobs": []}))
    text = build(tmp_path).read_text()
    summary = json.loads((tmp_path / "summary.json").read_text())
    assert summary["runs_scored"] == 1
    assert any("not authenticated (auth dev)" in e.get("why", "") for e in summary["runs_excluded"]), summary["runs_excluded"]
    assert "1 run(s) credentials" in text


def test_the_client_signs_operators_in_and_sends_their_cookie():
    import http.server

    from eval.bridge import Bridge

    seen: list[dict] = []

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):  # quiet
            pass

        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            seen.append({"path": self.path, "cookie": self.headers.get("Cookie"), "op": self.headers.get("X-Murmur-Operator"), "body": body})
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            if self.path == "/api/auth/login":
                self.send_header("Set-Cookie", f"murmur_session_1=tok-{body['operator']}; Path=/; HttpOnly; SameSite=Strict")
            self.end_headers()
            self.wfile.write(b'{"ok": true}')

    srv = http.server.HTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        b = Bridge(f"http://127.0.0.1:{srv.server_port}")
        b.login("op-1", "a long enough passphrase")
        b.post("/api/intents", {"task": "hold"}, operator="op-1")
        b.post("/api/intents", {"task": "hold"}, operator="op-planner")
    finally:
        srv.shutdown()
    assert seen[1]["cookie"] == "murmur_session_1=tok-op-1" and seen[1]["op"] is None
    # an operator not signed in here names itself (a dev bridge)
    assert seen[2]["cookie"] is None and seen[2]["op"] == "op-planner"
    assert "passphrase" not in json.dumps(seen[1:])


def test_a_credentials_stack_writes_its_own_credentials_with_the_cli(tmp_path: Path):
    from eval import stack as K

    if not (REPO / "target" / "release" / "murmur-auth").exists():
        pytest.skip("murmur-auth not built (make build)")
    spec = load_spec(REPO / "eval" / "scenarios" / "plausible-wrong.toml")
    block = P.PortBlock(index=0)  # no port is opened: only the credential file is written
    s = K.Stack(spec, 1, block, tmp_path, "test-run", auth="credentials")
    s.write_credentials()
    stored = json.loads(s.credentials_file.read_text())
    assert sorted(stored["operators"]) == sorted(K.OPERATORS)
    assert sorted(stored["agents"]) == sorted(K.AGENTS)
    assert set(s.passwords) == set(K.OPERATORS) and all(len(p) >= 15 for p in s.passwords.values())
    assert all(k.startswith("mma_") for k in s.agent_keys.values())
    # review fix 2: every operator but the participant holds a harness credential (scripted)
    assert [op for op, e in stored["operators"].items() if not e.get("scripted")] == [K.PARTICIPANT_OPERATOR]
    text = s.credentials_file.read_text()
    assert not any(p in text for p in s.passwords.values()) and not any(k in text for k in s.agent_keys.values())
    # a bad mode is refused
    with pytest.raises(K.StackError):
        K.Stack(spec, 1, block, tmp_path, "x", auth="header")


def test_a_study_refuses_to_start_without_the_participants_passkey(tmp_path: Path):
    """Rules 10: a study needs its participant's own credential file with a passkey (from
    `enroll`), or the explicit override; the run-local file then carries the participant's
    credential (their password hash, their passkeys), copied with the CLI."""
    from eval import stack as K

    spec = load_spec(REPO / "eval" / "scenarios" / "plausible-wrong.toml")
    block = P.PortBlock(index=0)  # no port is opened
    # no file at all, no override: refused, saying how to enroll
    with pytest.raises(K.StackError, match="enroll"):
        K.Stack(spec, 1, block, tmp_path, "x", auth="credentials", participant_credentials=tmp_path / "nobody.json")
    with pytest.raises(K.StackError, match="passkey"):
        K.check_participant(None, None)
    entry = {"password": "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaA", "engage": False,
             "stamp": "0011223344556677", "updated_unix": 1}
    f = tmp_path / "p07.json"
    f.write_text(json.dumps({"murmur_credentials": 1, "operators": {"op-study": entry}}))
    with pytest.raises(K.StackError, match="has no passkey"):
        K.check_participant(f, None)
    # the override lets it through (a pilot), and so does a passkey
    K.check_participant(f, "pilot: no authenticators yet")
    K.check_participant(None, "pilot: no file")
    key = {"id": "Y3JlZC1pZA", "label": "laptop", "public_key": "BAAA", "counter": 3, "created_unix": 1, "webauthn": {}}
    f.write_text(json.dumps({"murmur_credentials": 1, "operators": {"op-study": {**entry, "passkeys": [key]}}}))
    K.check_participant(f, None)
    s = K.Stack(spec, 1, block, tmp_path, "test-run", auth="credentials", participant_credentials=f)
    assert s.study
    if not (REPO / "target" / "release" / "murmur-auth").exists():
        pytest.skip("murmur-auth not built (make build)")
    s.write_credentials()
    stored = json.loads(s.credentials_file.read_text())
    assert stored["operators"]["op-study"]["passkeys"] == [key], "the participant's own credential, passkeys and all"
    assert "op-study" not in s.passwords, "no fresh passphrase: the participant has their own"
    # a dev stack can't be a study
    with pytest.raises(K.StackError):
        K.Stack(spec, 1, block, tmp_path, "x", auth="dev", password_step_up="why")


def test_run_passphrases_meet_the_password_policy():
    from eval.stack import passphrase

    ps = {passphrase() for _ in range(50)}
    assert len(ps) == 50
    assert all(len(p) >= 15 and p.count("-") == 4 for p in ps)
