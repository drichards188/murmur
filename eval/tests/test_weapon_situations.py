"""What the situation runner does for the weapon situations: a scripted lay order (rules 27)
proposed by the timeline and signed by the scripted signers like an engagement, a scripted
nomination naming its weapon, and the chain the audit log tells of a lay, its mines and a gun's
bursts; and the weapon situations themselves (sim/situations: shore-battery, road-mines,
hammerhead-line)."""

from __future__ import annotations

import asyncio
import math
from typing import Any

from eval import situation as S
from eval.engage import EngageAdapter
from eval.proxy import EngagementSigners, ProxyOperator, ProxyParams, lay_view


def _m(a: list[float], b: list[float]) -> float:
    dx = (b[0] - a[0]) * 111_320 * math.cos(math.radians(a[1]))
    dy = (b[1] - a[1]) * 111_320
    return math.hypot(dx, dy)


def test_the_weapon_situations_load_with_their_scripted_steps():
    shore = S.load_situation("shore-battery")
    engage = [s for s in shore.steps if s.kind == "engage"]
    assert [s.params["weapon"] for s in engage] == ["FGM-148 Javelin", "M2 .50 HMG"]
    assert all(s.params["vehicles"] == {"callsign": "BULL 1"} and s.params["unless_nominated"] for s in engage)
    for name, mine, count in (("road-mines", "TM-62M mine", 10), ("hammerhead-line", "Hammerhead (encapsulated Mk 54)", 2)):
        sit = S.load_situation(name)
        (lay,) = [s for s in sit.steps if s.params.get("task") == "lay"]
        assert lay.kind == "propose" and lay.params["weapon"] == mine and lay.params["count"] == count
        assert sit.operator == "autoplay" and sit.ai == "none" and sit.cosigner == "scripted"
    # a Hammerhead arms 30 minutes after it is laid: the line's watch runs at the real clock
    hl = S.load_situation("hammerhead-line")
    assert hl.rate == 1 and hl.duration_s > 1800 + 15 * 60


def test_a_lay_step_needs_its_line_count_and_live_period(tmp_path):
    base = 'title = "T"\nbrief = "B"\nfleet = "sim/fleets/hostile-act-engagement.toml"\n[[step]]\nkind = "propose"\ntask = "lay"\nvehicles = {callsign = "MOLE 1"}\n'
    ok = base + 'route = [[-122.464, 37.8], [-122.462, 37.8]]\ncount = 3\nlive_h = 1\nweapon = "TM-62M mine"\n'
    p = tmp_path / "ok.toml"
    p.write_text(ok)
    assert S.load_situation(p).steps[0].params["count"] == 3
    for missing in ("count = 3\n", "live_h = 1\n"):
        p = tmp_path / "bad.toml"
        p.write_text(ok.replace(missing, ""))
        try:
            S.load_situation(p)
        except S.SituationError as e:
            assert "lay order needs" in str(e)
        else:
            raise AssertionError(f"accepted a lay step without {missing.strip()}")


def test_the_storyline_carries_a_lay_orders_keys():
    from eval.storyline import Storyline

    class W:
        async def vehicles(self):
            return [{"uid": 1, "identity": {"callsign": "MOLE 1", "platform": "ugv_mines"}}]

    story = Storyline.__new__(Storyline)
    story.world = W()  # type: ignore[attr-defined]
    p = {"task": "lay", "vehicles": {"callsign": "MOLE 1"}, "route": [[-122.464, 37.8], [-122.462, 37.8]], "count": 10, "live_h": 1, "weapon": "TM-62M mine", "rationale": "x"}
    req, _, _, _ = asyncio.run(story.prepare(p))
    assert req["task"] == "lay" and req["uids"] == [1] and len(req["route"]) == 2
    assert req["extra"] == {"count": 10, "live_h": 1, "weapon": "TM-62M mine"}


def test_a_nomination_step_names_its_weapon_in_the_request_and_the_dry_run():
    track = {"track_id": "T-0007"}
    req = EngageAdapter.request({"weapon": "M2 .50 HMG", "effect_radius_m": 50}, [2], track)
    assert req["extra"]["weapon"] == "M2 .50 HMG" and req["extra"]["track"] == "T-0007"
    assert "weapon" not in EngageAdapter.request({}, [2], track)["extra"], "none named: the engine picks"
    posted: list[dict[str, Any]] = []

    class B:
        def post(self, path, body=None, operator=None):
            posted.append(body)
            return {"options": [{"uid": 2, "passed": True, "checks": []}]}

    class Story:
        bridge = B()

    assert asyncio.run(EngageAdapter(B()).option(Story(), req))["uid"] == 2  # type: ignore[arg-type]
    assert posted[0]["weapon"] == "M2 .50 HMG"


class LayBridge:
    """A lay order as the inbox serves it (the intent: `concurrences`, `approval`, `policy`), signed
    by plain approvals without a picture token."""

    def __init__(self, signed: list[str] | None = None, source: dict[str, Any] | None = None):
        self.signed = list(signed or [])
        self.source = source or {"kind": "agent", "id": "agent-scripted"}
        self.calls: list[tuple[str, dict, str | None]] = []

    def item(self) -> dict[str, Any]:
        state = "dispatched" if len(self.signed) >= 2 else "pending_approval"
        return {
            "id": 7, "kind": "lay", "state": state, "source": self.source,
            "params": {"count": 10, "live_h": 1.0, "weapon": "TM-62M mine"},
            "policy": {"approvers": 2}, "concurrences": [{"by": s} for s in self.signed[:1]],
            **({"approval": {"by": self.signed[1]}} if len(self.signed) > 1 else {}),
        }

    def try_get(self, path: str) -> Any:
        return {"pending": [self.item()] if len(self.signed) < 2 else []}

    def post(self, path: str, body: Any = None, operator: str | None = None) -> Any:
        self.calls.append((path, dict(body or {}), operator))
        assert path == "/api/intents/7/approve" and "token" not in body, "a lay order has no picture token"
        self.signed.append(operator or "?")
        return {"intent": self.item(), "dispatched": len(self.signed) >= 2}


def test_scripted_signers_sign_a_lay_order_as_an_engagement():
    assert lay_view(LayBridge(["op-1"]).item())["signed"] == ["op-1"]
    b = LayBridge()
    made = asyncio.run(EngagementSigners(b, ["op-2", "op-3"], read_s=0.0, label="scripted (test)").consider_lay(b.item()))  # type: ignore[arg-type]
    assert [m["by"] for m in made] == ["op-2", "op-3"] and made[-1]["dispatched"] and all(m["lay"] for m in made)
    assert all("not a person" in c[1]["note"] and "TM-62M mine" in c[1]["note"] for c in b.calls)
    # never the proposer
    b = LayBridge(source={"kind": "human", "id": "op-2"})
    made = asyncio.run(EngagementSigners(b, ["op-2", "op-3"], read_s=0.0).consider_lay(b.item()))  # type: ignore[arg-type]
    assert [m["by"] for m in made] == ["op-3"] and b.signed == ["op-3"]
    # the cosigner (live) never makes the first signature, and signs once after a person's
    b = LayBridge()
    log: list[dict[str, Any]] = []
    co = EngagementSigners(b, ["op-2", "op-3"], mode="cosign", read_s=0.0, log=log)  # type: ignore[arg-type]
    assert asyncio.run(co.consider_lay(b.item())) == [] and b.calls == []
    assert "person to sign first" in log[-1]["reason"]
    b = LayBridge(["op-1"])
    co = EngagementSigners(b, ["op-2", "op-3"], mode="cosign", read_s=0.0)  # type: ignore[arg-type]
    assert [m["by"] for m in asyncio.run(co.consider_lay(b.item()))] == ["op-2"] and b.signed == ["op-1", "op-2"]


def test_the_proxy_leaves_lay_orders_to_its_signers():
    class Inbox:
        def try_get(self, path):
            return {"pending": [{"id": 1, "kind": "lay", "expires_t": 1}, {"id": 2, "kind": "patrol", "expires_t": 2}]}

    decided: list[int] = []
    signers = EngagementSigners(Inbox(), ["op-2", "op-3"])  # type: ignore[arg-type]
    proxy = ProxyOperator(Inbox(), ProxyParams(attend_s=0.01, attend_sigma=0.0), seed=1, operator="op-auto", signers=signers)  # type: ignore[arg-type]

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


def test_the_chain_tells_of_a_lay_its_mines_and_a_guns_bursts():
    chain = S.EngagementChain({"1": "MOLE 1", "2": "BULL 1"})
    lay = {"t": 1000, "type": "intent.created", "intent": {"id": 2, "kind": "lay", "uids": ["1"], "source": {"kind": "agent", "id": "agent-scripted"}, "params": {"count": 2, "live_h": 1, "weapon": "TM-62M mine"}}}
    signed = [{"t": 2000, "type": "approval.concurred", "intent": 2, "by": "op-2", "n": 1, "of": 2}, {"t": 3000, "type": "approval.granted", "intent": 2, "by": "op-3"}]
    mine = {"t": 4000, "type": "mine.laid", "intent": 2, "n": 1, "of": 2, "at": [-122.464, 37.8], "weapon": {"name": "TM-62M mine"}, "arms_t": 64000}
    burst = {"t": 5000, "type": "engage.burst", "intent": 3, "burst": 2, "weapon": {"name": "M2 .50 HMG", "rounds": 6, "left": 488}}
    ended = {"t": 6000, "type": "engage.fire_ended", "intent": 3, "why": "ceased", "detail": "the assessment from the last burst"}
    dispatched = {"t": 3500, "type": "intent.dispatched", "intent": 2, "uids": ["1"]}
    gun = [
        {"t": 4500, "type": "intent.created", "intent": {"id": 3, "kind": "engage", "uids": ["2"], "source": {"kind": "agent", "id": "agent-scripted"}, "params": {"track": "T-0007"}}},
        {"t": 4600, "type": "intent.dispatched", "intent": 3, "engage": {"uid": "2", "fire_from": [-122.4765, 37.8065]}},
    ]
    out = chain.feed([lay, *signed, dispatched, mine, *gun, burst, ended])
    texts = [o["text"] for o in out]
    assert "intent 2 dispatched: MOLE 1 (uid 1) to lay along its line" in texts
    assert "intent 3 dispatched: BULL 1 (uid 2) to its firing position" in texts
    assert texts[0].startswith("lay order proposed by agent agent-scripted: intent 2, MOLE 1 (uid 1) to lay 2 TM-62M mine")
    assert any("mine 1 of 2 laid by intent 2" in t and "arms in 60 s" in t for t in texts)
    assert any("burst 2 of intent 3: M2 .50 HMG, 6 rounds, 488 left" in t for t in texts)
    assert any("fire ended (ceased)" in t for t in texts)
    summary = "\n".join(chain.summary_lines())
    assert "lay order intent 2" in summary and "signed by: op-2, op-3" in summary and "mines laid: 1" in summary


def test_the_shore_battery_fires_from_where_bull_stands():
    """BULL 1's spawn is inside both weapons' bands of the boats' holding points (rules 28: a
    missile's firing position is where it stands; rules 26: a gun's), so it never moves."""
    import tomllib

    from eval import REPO

    fleet = tomllib.loads((REPO / "sim/fleets/shore-battery.toml").read_text())
    bull = next(u for u in fleet["unit"] if u["platform"] == "ugv_armed")
    lat, lon, alt, _ = bull["spawn"]
    assert alt > 50, "spawned at the ground's height, so the link emulator sees it above the hill"
    contacts = tomllib.loads((REPO / "sim/scenarios/shore-battery.toml").read_text())
    holds = {c["id"]: c["route"][-1] for c in contacts["contact"] if c["affiliation"] == "hostile"}
    first, second = _m([lon, lat], holds["fastboat-first"]), _m([lon, lat], holds["fastboat-second"])
    assert 65 + 50 < first < 4750 - 50, "the Javelin's band"
    assert 65 + 50 < second < 1800 - 50, "the Javelin's band and the M2's reach less the effect radius"
