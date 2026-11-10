"""Saturation in the headless simulator (docs/HEADLESS.md, "Saturation"; bd murmur-lleq).

- the situation runs in its own world (sim/worlds/golden-gate-saturation.toml) as a world v3
  situation: its fleet, contacts, link events, areas and opening orders as live, the sensing
  model its contacts name (not the Golden Gate world's legacy pin), the fleet's swarms declared in
  the bridge's manifest;
- the runner sends an order to `{swarm = ...}` to every member as the fleet plans them, and asks a
  swarm's dry run again while it fails only on stale facts, on the simulated clock, as the live
  runner does (eval/situation_run.py `issue_order`);
- a fleet's declared swarms reach the bridge (`/api/swarms`: names and roles as declared).

The whole 3,602-node episode is not run here (about half an hour at half real time):
docs/HEADLESS.md has its measurements and how to run it.
"""

from __future__ import annotations

import asyncio
import dataclasses
import tempfile
from pathlib import Path
from typing import Any

from eval.headless import REPO
from eval.headless.episode import Runner
from eval.headless.situations import Calibration, EpisodeSpec, from_situation, process_spec
from eval.headless.tests.test_engage_space import binary  # noqa: F401 (a fixture: builds the simulator)
from eval.situation import Order
from eval.situation_run import STALE_RETRIES, STALE_RETRY_S


def test_saturation_runs_in_its_own_world_as_live():
    e = from_situation("swarm-saturation")
    assert (e.world, e.map, e.space_snapshot) == ("v3", "golden-gate-saturation", "golden-gate")
    assert e.fleet == REPO / "sim" / "fleets" / "swarm-saturation.toml"
    assert e.contacts == REPO / "sim" / "scenarios" / "swarm-saturation.toml"
    assert len(e.orders) == 36 and all(set(o.vehicles) == {"swarm"} for o in e.orders)
    assert {o.vehicles["swarm"] for o in e.orders} == {f"PICKET-{i}" for i in range(1, 18)} | {
        f"SEARCH-{i}" for i in range(1, 11)} | {"RELAY-1", "RELAY-2", "RELAY-3"} | {
        f"SURFACE-{i}" for i in range(1, 6)} | {"UNDERSEA-1"}
    # every [[step]] is a panel note: nothing for the script to do
    assert e.script == []
    assert [ev["at_s"] for ev in e.events] == [240, 240, 240]
    with tempfile.TemporaryDirectory() as td:
        spec, env = process_spec(e, 1, Path(td), Calibration.load())
    assert len(spec["fleet"]) == 3602 and len(spec["standins"]) == 3602
    assert len(spec["swarms"]) == 37
    assert {s["role"] for s in spec["swarms"]} == {"picket", "search", "relay", "surface", "undersea", "reserve"}
    # the sensing model its contacts name (physical, as live): not pinned to the legacy one
    assert "MURMUR_SENSE_MODEL" not in env
    assert env["MURMUR_AREAS"] == str(REPO / "sim" / "situations" / "areas" / "swarm-saturation")
    assert env["MURMUR_LAND_WATER_GRID"].endswith("golden-gate.water.grid.gz")
    assert spec["water"]["currents"].endswith("golden-gate.currents.gz")
    assert spec["contacts"]["scenario"] == str(e.contacts)


def test_a_fleet_without_swarms_has_no_swarms_in_its_spec():
    with tempfile.TemporaryDirectory() as td:
        spec, _ = process_spec(from_situation("harbor-watch"), 1, Path(td), Calibration.load())
    assert "swarms" not in spec


# ---- the runner's swarm orders, on a fake simulator ------------------------------------------

STALE = {"would_reach": "rejected", "submission_authorizes": False,
         "checks": [{"rule": "vehicle_ready", "outcome": "fail",
                     "detail": "cannot evaluate: navigation status is 12 s old (limit 10 s)"}],
         "policy": {"tier": "auto", "rule": "default"}}
FINE = {"would_reach": "auto_approved", "submission_authorizes": False, "checks": [],
        "policy": {"tier": "auto", "rule": "default"}}


class FakeSim:
    """`/api/state` with every vehicle ready; the dry runs answer from a queue per first uid."""

    def __init__(self, uids: list[int], previews: dict[int, list[dict[str, Any]]]):
        self.uids = uids
        self.previews = previews
        self.calls: list[tuple[str, Any]] = []
        self.t = 0.0
        self.tools: list[str] = []

    def mcp(self, agent: str, client: tuple[str, str], tool: str, args: dict[str, Any]) -> dict[str, Any]:
        # agent-setup's proposal of an order op-setup may not submit itself
        self.calls.append((f"mcp:{tool}", args))
        return {"structured": {"id": 99, "state": "pending_approval"}}

    def api(self, method: str, path: str, body: Any = None, operator: str | None = None) -> tuple[int, Any]:
        if path == "/api/state":
            return 200, {"vehicles": [{"uid": u, "nav_ready": True, "position": [0, 0], "home": [0, 0],
                                       "identity": {"callsign": f"V{u}"}} for u in self.uids]}
        self.calls.append((path, body))
        first = body["uids"][0]
        if path == "/api/intents/preview":
            q = self.previews[first]
            return 200, q.pop(0) if len(q) > 1 else q[0]
        return 200, {"intent": {"id": len(self.calls), "state": "dispatched"}, "dispatched": True}


@dataclasses.dataclass
class FakeEp:
    sim: FakeSim
    e: EpisodeSpec
    planned: list[dict[str, Any]]
    want: set[int]
    operator_at: list[float] = dataclasses.field(default_factory=list)

    def run_async(self, coro: Any) -> Any:
        return asyncio.new_event_loop().run_until_complete(coro)

    def start_operator(self, now: float) -> None:
        self.operator_at.append(now)


def fake_episode(previews: dict[int, list[dict[str, Any]]]) -> FakeEp:
    planned = [{"uid": u, "callsign": f"V{u}", **({"swarm": "S-A"} if u <= 3 else {"swarm": "S-B"} if u <= 6 else {})}
               for u in range(1, 8)]
    orders = [Order("a", "orbit", {"swarm": "S-A"}, {"center": [-122.47, 37.82], "radius_m": 300.0}),
              Order("b", "orbit", {"swarm": "S-B"}, {"center": [-122.46, 37.82], "radius_m": 300.0}),
              Order("c", "hold", {"callsign": "V7"}, {})]
    e = dataclasses.replace(from_situation("harbor-watch"), name="fake", orders=orders)
    # V2 not heard yet: a swarm's order goes to the members the bridge has
    sim = FakeSim([1, 3, 4, 5, 6, 7], previews)
    return FakeEp(sim, e, planned, {1, 3, 4, 5, 6, 7})


def test_a_swarm_order_waits_out_stale_facts_on_the_simulated_clock():
    ep = fake_episode({1: [FINE], 4: [STALE, STALE, FINE], 7: [FINE]})
    r = Runner(ep)  # type: ignore[arg-type]
    assert r.wake(10.0) == 10.0 + STALE_RETRY_S  # order a out, b refused for a stale fact: asked again
    assert [o["id"] for o in r.orders] == ["a"] and not ep.operator_at
    assert r.wake(15.0) == 15.0 + STALE_RETRY_S
    assert r.wake(20.0) is None  # b out, then c at once, then the operator
    assert ep.operator_at == [20.0]
    a, b, c = r.orders
    assert (a["swarm"], a["vehicles"], a["t"], a["t_issued"]) == ("S-A", ["V1", "V3"], 10.0, 10.0)
    assert "stale_retries" not in a
    assert (b["swarm"], b["vehicles"], b["t"], b["t_issued"], b["stale_retries"]) == ("S-B", ["V4", "V5", "V6"], 10.0, 20.0, 2)
    assert b["dry_run"]["would_reach"] == "auto_approved" and b["dispatched"]
    assert "swarm" not in c and "t_issued" not in c and c["t"] == 20.0
    sent = [body["uids"] for path, body in ep.sim.calls if path == "/api/intents"]
    assert sent == [[1, 3], [4, 5, 6], [7]]


def test_a_swarm_order_still_stale_after_the_retries_goes_on_refused():
    ep = fake_episode({1: [STALE], 4: [FINE], 7: [FINE]})
    r = Runner(ep)  # type: ignore[arg-type]
    t = 0.0
    for _ in range(STALE_RETRIES):
        nxt = r.wake(t)
        assert nxt == t + STALE_RETRY_S
        t = nxt
    assert r.wake(t) is None
    a = r.orders[0]
    assert a["stale_retries"] == STALE_RETRIES and a["dry_run"]["would_reach"] == "rejected"
    # refused by its dry run: agent-setup proposes it (as live), it is not submitted
    assert (a["by"], a["state"]) == ("agent-setup", "pending_approval")
    assert len(r.orders) == 3


# ---- the swarms reach the bridge -----------------------------------------------------------

SWARM_FLEET = """
title = "Three Saildrones as one swarm (tests/test_saturation.py)"

[[swarm]]
id = "SURFACE-T"
name = "Saildrones off Point Bonita"
role = "surface"
color = "#4fd1c5"
platform = "saildrone"
count = 3
sim = "standin"
layout = "block"
center = { lat = 37.8150, lon = -122.5300 }
columns = 3
spacing_m = 30
"""


def test_the_fleet_swarms_reach_the_bridge_as_declared(binary: Path, tmp_path: Path):  # noqa: F811
    from eval.headless.sim import Sim

    fleet = tmp_path / "fleet.toml"
    fleet.write_text(SWARM_FLEET)
    e = dataclasses.replace(from_situation("harbor-watch"), fleet=fleet, contacts=None, orders=[])
    spec, env = process_spec(e, 1, tmp_path, Calibration.load())
    sim = Sim(spec, env, log=tmp_path / "sim.log")
    try:
        sim.step(20.0)
        status, body = sim.api("GET", "/api/swarms")
    finally:
        sim.end()
    assert status == 200
    swarms = body["swarms"] if isinstance(body, dict) else body
    (s,) = swarms
    assert (s["id"], s["name"], s["role"]) == ("SURFACE-T", "Saildrones off Point Bonita", "surface")
