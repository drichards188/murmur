"""One headless episode (docs/HEADLESS.md, section 4): the situation runner's steps, the
operator, the AI and any scripted faults, as processes of one event loop on the simulated
clock, over one `murmur-headless` process.

- **the runner** (eval/situation_run.py's `run`): a readiness check every second
  (`/api/state`: every vehicle navigation-ready with a position), then the opening orders
  (`issue_order`: `op-setup`'s dry run, then its submission or `agent-setup`'s proposal), then
  the operator; the AI at `ai_start_s`; the end at `duration_s`.
- **the AI** (agents/playbook_agent.py's `Agent.loop`): the unchanged engine
  (`agents/playbook/engine.py`) on a `HeadlessSession`: `start`, then a tick, the change feed
  every 0.5 s until 2 s after the tick began, ticking at once on a waking change. Or
  (`Options.agent = "model"`) the model watch officer (officer.py): agents/watch_officer.py's
  trigger table, picture and prompt, a real model through the Anthropic API, its wakes between
  two steps of the clock. Or none. Or (scoring v2) one of the anchors `null`, `random-valid`,
  `oracle`, a rung of the ruler check's ladder (`oracle-linkblind`, `oracle-honest`,
  `oracle-late`), or a perturbation of the playbook (`playbook+spam`, ...) or of the honest
  oracle (`oracle-honest+spam`, ..., `oracle-honest+pull-off`): eval/headless/agents_v2.py, each
  on a session and woken on the loop from the same first tick.
- **the operator** (operator.py).
- **the script**: the situation's faults and scripted proposers (families.py).
- **the signers and the reviewer** (signers.py, review.py; `Options.signers`, `Options.review`,
  docs/HEADLESS.md section 16): scripted operators who decide engagement nominations and work
  the review list, from when the runner starts the operator. Off by default.
- **the truth sampler**: our vehicles every `sample_s` and the tracker's association every 10 s;
  the contacts' truth is their own per-step log, read at the end (never given to the AI).
- **the picture sampler** (scoring v2 only, `Options.scoring = "v2"`; eval/SCORING-V2.md 2.6):
  every 1 s of situation time, the picture as the bridge serves it, the tracker's association,
  the inbox's alerts and our vehicles' truth, one request that never moves the clock, kept as
  change points (v2/record.py). A v2 record also carries `episode_class` (section 7: the driver
  sets it), the replicate, the audit log's digest and the invariants. A v1 episode runs none of
  this, so its record is unchanged. Scoring v3 (`Options.scoring = "v3"`, eval/SCORING-V3.md)
  runs the same sampler and writes the same part under `v3` (eval/headless/v3/); its own agent,
  the basic v3 oracle (`oracle-v3`), is eval/headless/v3/oracle.py.

Simultaneous events run in a fixed order (runner, script, operator, AI, sampler, picture) and
each process's actions happen at one simulated instant, so an episode is a function of
(situation, seed, calibration, operator model).

**The clock condition** (`Options.clock`, SCORING-V2.md 4.1 and 4.2; docs/HEADLESS.md 14.1.1):
`paused` (default) as above, the world waiting for the AI; `charged`, the AI's thinking costing
simulated time by latency.py's constants: a model wake is a turn the loop resumes at each
answer's charged time (officer.py), a playbook tick reads at its start and acts
`playbook_tick_s` later, the other processes running meanwhile. Only a charged record carries
the condition (`clock.condition`, `provenance.clock`, `v2.condition.clock`).
"""

from __future__ import annotations

import asyncio
import hashlib
import heapq
import json
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from eval.agent import _mcp_args, _rest_body
from eval.situation import AUTOPLAY_OPERATOR, SCRIPTED_SETUP, SETUP_AGENT, SETUP_OPERATOR, playbook_json
from eval.situation_run import dry_run_summary, labelled, submits_itself
from eval.storyline import select_vehicles

from . import REPO
from .latency import CLOCKS, Latency
from .operator import Operator, OperatorModel, operator_model
from .sim import HeadlessSession, Sim
from .situations import Calibration, EpisodeSpec, process_spec

sys.path.insert(0, str(REPO / "agents"))
from playbook import ENGINE_VERSION  # noqa: E402
from playbook.config import make as make_config  # noqa: E402
from playbook.engine import Engine, waking  # noqa: E402
from playbook.ledger import Ledger  # noqa: E402

POLL_S = 0.5  # agents/playbook_agent.py
PLAYBOOK_AGENT = "agent-playbook"
RECORD_SCHEMA = "murmur-headless-episode/1"
# the signers and the reviewer are operators: right after the operator at the same instant
PRIORITY = {"runner": 0, "script": 1, "operator": 2, "signers": 2.1, "review": 2.2, "agent": 3, "sampler": 4,
            "picture": 5}
#: scoring v2's anchors and perturbations (eval/headless/agents_v2.py; SCORING-V2.md section 5)
ANCHORS = ("null", "random-valid", "oracle")
#: the ruler check's ladder of handicapped oracles (agents_v2.py; SCORING-V2.md 0.3)
LADDER = ("oracle-linkblind", "oracle-honest", "oracle-late")
PERTURBATIONS = ("playbook+spam", "playbook+behind", "playbook+skip-first", "playbook+reserve-blind",
                 "playbook+ferries")
#: the perturbations of the realistic ceiling (agents_v2.py; SCORING-V2.md 0.5)
HONEST_PERTURBATIONS = ("oracle-honest+spam", "oracle-honest+behind", "oracle-honest+skip-first",
                        "oracle-honest+reserve-blind", "oracle-honest+ferries", "oracle-honest+pull-off")
V2_AGENTS = ANCHORS + LADDER + PERTURBATIONS + HONEST_PERTURBATIONS
#: scoring v3's own agents (eval/headless/v3/oracle.py): the basic v3 oracle; `none` and
#: `random-valid` carry over from v2
V3_AGENTS = ("oracle-v3",)
#: the scripted agent for the engagement chain and the space segment (scripted.py)
AGENTS = ("playbook", "model", "none") + V2_AGENTS + ("scripted",) + V3_AGENTS
SCORINGS = ("v1", "v2", "v3")
#: the scorings that run the picture sampler and write its part of the record (v2 and v3 alike)
PICTURE_SCORINGS = ("v2", "v3")
#: SCORING-V2.md section 7: the classes the driver sets (the batch adds `harness_error` for an
#: episode that failed in the simulator or the driver)
EPISODE_CLASSES = ("scored", "harness_error", "budget_stop", "model_failed")


@dataclass
class Options:
    agent: str = "playbook"  # playbook | model | none | an anchor or perturbation (AGENTS)
    operator: dict[str, Any] | None = None  # None: the situation's [autoplay] (op-auto)
    calibration: Calibration = field(default_factory=Calibration)
    terrain: bool = True
    space: bool = True
    anchors: bool = True
    keep: Path | None = None  # copy the audit log, anchors and the process log here
    binary: Path | None = None
    sample_s: float = 2.0
    # agent "model" only (officer.py): ModelConfig overrides, and a client to use instead of the
    # Anthropic API's (the tests' fake); without one the episode needs MURMUR_EVAL_MODEL=1
    model: dict[str, Any] | None = None
    model_client: Any = None
    # "v2": the picture sampler, `episode_class`, the audit log's digest (SCORING-V2.md); "v1"
    # (default) leaves the episode and its record exactly as before
    scoring: str = "v1"
    # v2: which replicate this is ({"r", "seed", "situation_seed"}), recorded as given
    replicate: dict[str, Any] | None = None
    # the clock condition (SCORING-V2.md 4.1, 4.2; latency.py): "paused" (default: the world
    # waits for the agent, the episode and its record exactly as before) or "charged" (the
    # agent's thinking costs simulated time); `latency` overrides latency.toml's constants
    clock: str = "paused"
    latency: Any = None
    # docs/HEADLESS.md section 16, each off (None) by default, the episode and its record then
    # exactly as before: the engagement signers (signers.py `signer_policy`: {"preset", ...}),
    # the reviewer (review.py `review_policy`), the scripted agent's plan (agent "scripted",
    # scripted.py `ScriptedPlan`), how an effect plays out on the truth side ({"p_hit",
    # "spend_expended", "survives_miss"}: the simulator's `effects`), and the pinned element
    # sets the bridge's space segment and the emulator's SATCOM read (space.py: a world name)
    signers: dict[str, Any] | None = None
    review: dict[str, Any] | None = None
    scripted: dict[str, Any] | None = None
    effects: dict[str, Any] | None = None
    space_snapshot: str | None = None


class Loop:
    """The event loop: processes wake at simulated times; the simulator steps between. Events
    run in (time, process priority, order scheduled); time only moves forward: a process may
    schedule itself now or later, never at an instant already passed (`at` refuses it), so no
    event is reordered and nothing runs at an instant the simulator has left. A charged turn
    (officer.py) is an ordinary event of the `agent` process at each answer's charged time."""

    def __init__(self, sim: Sim):
        self.sim = sim
        self.q: list[tuple[float, int, int, str]] = []
        self.n = 0
        self.handlers: dict[str, Any] = {}
        self.now = 0.0

    def at(self, t: float | None, name: str) -> None:
        if t is None:
            return
        t = round(float(t), 6)
        if t < self.now - 1e-6:
            raise RuntimeError(f"{name} asked to run at {t} s, before the loop's current {self.now} s")
        self.n += 1
        heapq.heappush(self.q, (t, PRIORITY[name], self.n, name))

    def run(self, end: float) -> None:
        while self.q:
            t, _, _, name = heapq.heappop(self.q)
            if t > end:
                break
            self.now = max(self.now, t)
            if t > self.sim.t:
                self.sim.step(t)
            self.at(self.handlers[name](t), name)
        if self.sim.t < end:
            self.sim.step(end)


class Runner:
    """eval/situation_run.py's steps, on the simulated clock."""

    def __init__(self, ep: "Episode"):
        self.ep = ep
        self.ready_t: float | None = None
        self.orders: list[dict[str, Any]] = []
        self.phase = "waiting"

    def wake(self, now: float) -> float | None:
        ep = self.ep
        if self.phase == "waiting":
            status, body = ep.sim.api("GET", "/api/state")
            vehicles = (body or {}).get("vehicles", []) if status == 200 else []
            # world v3's worlds: a home too (an order's checks need it; a slow link's vehicle, an
            # AUV on its acoustic modem, can be navigating before the bridge has its home), so the
            # opening orders are not refused for a fact a second away; v1 and v2 as before
            ready = {v["uid"] for v in vehicles
                     if v.get("nav_ready") and v.get("position") and (ep.e.world != "v3" or v.get("home"))}
            if ep.want <= ready or now >= ep.e.ready_timeout_s:
                self.ready_t = now
                self.issue_orders(vehicles, now)
                self.phase = "running"
                ep.start_operator(now)
                return None
            return now + 1.0
        return None

    def issue_orders(self, vehicles: list[dict[str, Any]], now: float) -> None:
        ep = self.ep
        setup = HeadlessSession(ep.sim, SETUP_AGENT, "0.1")
        for o in ep.e.orders:
            rec: dict[str, Any] = {"id": o.id, "task": o.task, "t": round(now, 3)}
            try:
                chosen = select_vehicles(o.vehicles, vehicles)
                rec["vehicles"] = [(v.get("identity") or {}).get("callsign") or str(v["uid"]) for v in chosen]
                req = {"task": o.task, "uids": [v["uid"] for v in chosen], **dict(o.params)}
                note = o.note or f"opening order of situation {ep.e.name}"
                body = _rest_body(req, labelled(SCRIPTED_SETUP, note))
                status, preview = ep.sim.api("POST", "/api/intents/preview", body, SETUP_OPERATOR)
                if status // 100 != 2:
                    raise RuntimeError(f"dry run: {status} {preview}")
                rec["dry_run"] = dry_run_summary(preview)
                if not submits_itself(preview):
                    out = ep.run_async(setup.propose(labelled(SCRIPTED_SETUP, note), **_mcp_args(req)))
                    rec.update(by=SETUP_AGENT, intent_id=out.get("id"), state=out.get("state"))
                else:
                    status, out = ep.sim.api("POST", "/api/intents", body, SETUP_OPERATOR)
                    intent = (out or {}).get("intent") or {}
                    rec.update(by=SETUP_OPERATOR, intent_id=intent.get("id"), state=intent.get("state"),
                               dispatched=bool((out or {}).get("dispatched")))
            except Exception as ex:  # noqa: BLE001 (recorded; the situation goes on)
                rec["error"] = str(ex)
            self.orders.append(rec)


class PlaybookAgent:
    """agents/playbook_agent.py's loop on simulated time, the engine unchanged."""

    def __init__(self, ep: "Episode", cfg: Any):
        self.ep = ep
        self.cfg = cfg
        self.lines: list[dict[str, Any]] = []
        self.engine = Engine(cfg, Ledger(), log=self.write)
        self.session = HeadlessSession(ep.sim, PLAYBOOK_AGENT, cfg.client_version)
        self.started = False
        self.next_at: float | None = None
        self.misses = 0
        self.ticks = 0
        self.errors = 0
        # playbook_agent.py's reconnect delay: 2 s, doubling to 30 s, back to 2 s on a connect
        self.retry_s = 2.0
        # ticks over 50 ms of compute (the live agent's `tick.slow`): computer time, so counted
        # in `perf`, never written into the record
        self.slow_ticks = 0
        self.plans: list[dict[str, Any]] = []
        # the charged clock (SCORING-V2.md 4.2): a tick reads the picture when it starts and its
        # previews, proposals and withdrawals reach the bridge `tick_charge_s` later (`pending`)
        lat = getattr(ep, "latency", None)
        self.tick_charge_s: float | None = float(lat.playbook_tick_s) if lat is not None else None
        self.pending: dict[str, Any] | None = None
        # what each tick's pure part decided (wrapped from outside, the engine unchanged): for
        # CRITERIA.md's timeliness, the first tick whose plan held a play's candidate
        plan = self.engine.plan

        def recorded(reads: Any) -> Any:
            p = plan(reads)
            cands = sorted({(c.play, c.target) for r in p.results.values() for c in r.candidates})
            if cands:
                self.plans.append({"t": round(p.t, 3), "candidates": [list(c) for c in cands]})
            return p

        self.engine.plan = recorded  # type: ignore[method-assign]

    def write(self, kind: str, **fields: Any) -> dict[str, Any]:
        rec = {"t": round(self.ep.sim.t, 3), "source": "playbook", "kind": kind}
        for k, v in fields.items():
            rec[f"field_{k}" if k in rec else k] = v
        self.lines.append(rec)
        return rec

    def _start(self, now: float) -> bool:
        try:
            info = self.ep.run_async(self.engine.start(self.session))
        except Exception as e:  # noqa: BLE001
            self.write("bridge.error", error=f"{type(e).__name__}: {e}", retry_in_s=self.retry_s)
            return False
        self.write("bridge.connected", bridge_run=info["run"], cursor=info["cursor"], reset=info["reset"],
                   client_version=self.cfg.client_version, tools=list(self.session.tools))
        self.started = True
        self.retry_s = 2.0
        return True

    def _tick(self, now: float) -> None:
        if self.tick_charge_s is not None:
            self._begin_tick(now)
            return
        try:
            rep = self.ep.run_async(self.engine.tick(self.session))
            self.ticks += 1
            self.misses = 0
            if rep.compute_ms > 50.0:
                self.slow_ticks += 1
        except Exception as e:  # noqa: BLE001 (three in a row: a new session, as the agent does)
            self._missed(e)
        self.next_at = now + float(self.cfg["tick_s"])

    def _missed(self, e: BaseException) -> None:
        self.misses += 1
        self.errors += 1
        self.write("error", error=f"{type(e).__name__}: {e}", misses=self.misses)
        if self.misses >= 3:
            self.started = False

    def _begin_tick(self, now: float) -> None:
        """Charged: the tick's reads now (the engine's own `read`, as its tick would make them);
        the rest of the tick when they are due (`_finish_tick`). An engine not yet verified
        reads nothing now: its whole tick (settle, read, act) runs when due."""
        reads = None
        if self.engine.verified:
            try:
                reads = self.ep.run_async(self.engine.read(self.session))
            except Exception as e:  # noqa: BLE001 (as a failed tick)
                self._missed(e)
                self.next_at = now + float(self.cfg["tick_s"])
                return
        assert self.tick_charge_s is not None
        self.pending = {"t": now, "due": now + self.tick_charge_s, "reads": reads}

    def _finish_tick(self, now: float) -> None:
        """Charged: the tick decides on the picture read when it began, and its previews,
        proposals and withdrawals go out now. The engine is unchanged: its `read` is answered
        once with what was read then (an attribute of this engine, removed after)."""
        p, self.pending = self.pending, None
        assert p is not None
        eng = self.engine
        if p["reads"] is not None:
            taken = p["reads"]

            async def read(session: Any) -> Any:
                return taken

            eng.read = read  # type: ignore[method-assign]
        try:
            rep = self.ep.run_async(eng.tick(self.session))
            self.ticks += 1
            self.misses = 0
            if rep.compute_ms > 50.0:
                self.slow_ticks += 1
        except Exception as e:  # noqa: BLE001
            self._missed(e)
        finally:
            eng.__dict__.pop("read", None)
        self.next_at = p["t"] + float(self.cfg["tick_s"])

    def wake(self, now: float) -> float | None:
        if self.pending is not None:
            # charged: a tick in progress (no polls meanwhile: the live agent's loop awaits it)
            if now < self.pending["due"] - 1e-9:
                return self.pending["due"]
            self._finish_tick(now)
            if not self.started:
                return now
            assert self.next_at is not None
            return now + max(0.0, min(POLL_S, self.next_at - now))
        if not self.started:
            if not self._start(now):
                delay, self.retry_s = self.retry_s, min(self.retry_s * 2.0, 30.0)
                return now + delay
            self._tick(now)
            if self.pending is not None:
                return self.pending["due"]
            if not self.started:  # three misses in a row: a new session at once, as live
                return now
            return min(now + POLL_S, self.next_at or now)
        assert self.next_at is not None
        try:
            new = self.ep.run_async(self.engine.poll_changes(self.session))
        except Exception:  # noqa: BLE001 (the tick reads again)
            new = []
            self._tick(now)
            if self.pending is not None:
                return self.pending["due"]
            if not self.started:
                return now
            return min(now + POLL_S, self.next_at)
        if waking(new) or now >= self.next_at - 1e-9:
            self._tick(now)
            if self.pending is not None:
                return self.pending["due"]
            if not self.started:  # three misses in a row: a new session at once, as live
                return now
        return now + min(POLL_S, self.next_at - now)


class Episode:
    def __init__(self, e: EpisodeSpec, seed: int, workdir: Path, opts: Options | None = None,
                 contacts_path: Path | None = None):
        self.e = e
        self.seed = int(seed)
        self.workdir = workdir
        self.opts = opts or Options()
        self.contacts_path = contacts_path
        self.aloop = asyncio.new_event_loop()
        self.want: set[int] = set()
        self.operator: Operator | None = None
        self.agent: Any = None  # PlaybookAgent, officer.WatchOfficer or None
        self.script: Any = None
        self.truth_samples: list[dict[str, Any]] = []
        self.ours_samples: list[dict[str, Any]] = []
        self.contacts_meta: list[dict[str, Any]] = []
        self.association: dict[str, str] = {}
        self.ai_first_tick_s: float | None = None
        self.contacts_start_s: float | None = None
        self.bridge_start_s: float | None = None
        if self.opts.scoring not in SCORINGS:
            raise ValueError(f"unknown scoring {self.opts.scoring!r}: {', '.join(SCORINGS)}")
        self.picture_enc: Any = None  # v2/record.py PictureEncoder (v3: v3/record.py's)
        self.signers: Any = None  # signers.Signers (Options.signers)
        self.reviewer: Any = None  # review.Reviewer (Options.review)
        self.effects: list[dict[str, Any]] = []
        # the latency charge (latency.py) on the charged clock; None on the paused one
        if self.opts.clock not in CLOCKS:
            raise ValueError(f"unknown clock {self.opts.clock!r}: {', '.join(CLOCKS)}")
        self.latency: Latency | None = None
        if self.opts.clock == "charged":
            self.latency = self.opts.latency if self.opts.latency is not None else Latency.load()

    def run_async(self, coro: Any) -> Any:
        return self.aloop.run_until_complete(coro)

    def start_operator(self, now: float) -> None:
        if self.operator is not None:
            self.loop.at(self.operator.start(now), "operator")
        if self.signers is not None:
            self.loop.at(self.signers.start(now), "signers")
        if self.reviewer is not None:
            self.loop.at(self.reviewer.start(now), "review")

    def run(self) -> dict[str, Any]:
        wall0 = time.perf_counter()
        workdir = self.workdir
        workdir.mkdir(parents=True, exist_ok=True)
        # world v3: the episode's conditions drawn from its world's ranges by its seed (a
        # situation with `[headless] conditions = "world"`); otherwise the spec as given
        self.e = self.e.drawn(self.seed)
        spec, env = process_spec(self.e, self.seed, workdir, self.opts.calibration, terrain=self.opts.terrain,
                                 space=self.opts.space, anchors=self.opts.anchors, contacts_path=self.contacts_path)
        self.section16(spec, env)
        self.want = {int(v["uid"]) for v in spec["fleet"]}
        # the fleet as planned (uid, callsign, platform): what a timeline's fault step selects in
        self.planned = list(spec["fleet"])
        self.sim_epoch = float(spec["epoch"])
        log = workdir / "sim.log"
        sim = Sim(spec, env, binary=self.opts.binary, log=log)
        self.sim = sim
        started_s = time.perf_counter() - wall0
        try:
            self.loop = Loop(sim)
            runner = Runner(self)
            self.loop.handlers["runner"] = runner.wake
            model: OperatorModel = operator_model(self.opts.operator if self.opts.operator is not None
                                                  else self.e.autoplay)
            self.operator = Operator(sim, model, self.e.seed, AUTOPLAY_OPERATOR,
                                     self.e.operator_note(AUTOPLAY_OPERATOR),
                                     spec["epoch"])
            self.loop.handlers["operator"] = self.operator.wake
            if self.opts.signers is not None:
                from .signers import Signers, signer_policy

                self.signers = Signers(sim, signer_policy(self.opts.signers), self.seed, float(spec["epoch"]))
                self.loop.handlers["signers"] = self.signers.wake
            if self.opts.review is not None:
                from .review import Reviewer, review_policy

                self.reviewer = Reviewer(sim, review_policy(self.opts.review), self.seed)
                self.loop.handlers["review"] = self.reviewer.wake
            cfg = make_config(json.loads(playbook_json(self.e)), self.e.config_name or self.e.name)  # type: ignore[arg-type]
            if self.opts.agent not in AGENTS:
                raise ValueError(f"unknown agent {self.opts.agent!r}: {', '.join(AGENTS)}")
            if self.opts.agent == "model":
                from .officer import ModelConfig, WatchOfficer

                self.agent = WatchOfficer(self, ModelConfig.resolve(self.opts.model), client=self.opts.model_client)
            elif self.opts.agent == "playbook":
                self.agent = PlaybookAgent(self, cfg)
            elif self.opts.agent in V2_AGENTS:
                from . import agents_v2

                self.agent = agents_v2.make(self, self.opts.agent, cfg)
            elif self.opts.agent == "scripted":
                from .scripted import ScriptedAgent, ScriptedPlan

                self.agent = ScriptedAgent(self, ScriptedPlan.from_spec(self.opts.scripted))
            elif self.opts.agent in V3_AGENTS:
                from .v3 import oracle as oracle_v3

                self.agent = oracle_v3.make(self, self.opts.agent)
            if self.agent is not None:
                self.loop.handlers["agent"] = self.agent.wake
                self.ai_first_tick_s = self.opts.calibration.ai_first_tick(self.seed)
                self.loop.at(self.e.ai_start_s + self.ai_first_tick_s, "agent")
            from .script import Script

            self.script = Script(self, self.e.script)
            self.loop.handlers["script"] = self.script.wake
            self.loop.at(self.script.first(), "script")
            self.loop.handlers["sampler"] = self.sample
            self.loop.at(0.0, "sampler")
            if self.opts.scoring in PICTURE_SCORINGS:
                from .v2.record import PictureEncoder
                from .v3.record import PictureEncoderV3

                # v3 also keeps each track's looks (the physical model's: which sensor reached
                # which level); a legacy picture has none, so the two encode alike
                self.picture_enc = (PictureEncoderV3 if self.opts.scoring == "v3" else PictureEncoder)(self.sim_epoch)
                self.loop.handlers["picture"] = self.picture_sample
                self.loop.at(0.0, "picture")
            self.loop.at(1.0, "runner")
            self.loop.run(self.e.duration_s)
            if self.opts.agent == "model":
                self.agent.close()
            final = sim.rpc({"op": "truth", "association": True, "log_from": 0})
            for a in final.get("association") or []:
                self.association.setdefault(a["track_id"], a["contact_id"])
            self.truth_log(final)
            self.effects = list(final.get("effects") or [])
            # where the simulator's computer time went (diagnostics; not part of the episode)
            rust_perf = sim.rpc({"op": "perf"}).get("tasks") or {}
            end = sim.end()
        except BaseException:
            sim.kill()
            raise
        sim_wall = time.perf_counter() - wall0
        audit_path = Path(end["audit"])
        audit_text = audit_path.read_text() if audit_path.exists() else ""
        anchors = audit_path.with_suffix(".anchors")
        record = self.record(spec, runner, audit_text, cfg)
        pb = self.agent if isinstance(self.agent, PlaybookAgent) else None
        record["perf"] = {
            "wall_s": round(time.perf_counter() - wall0, 3),
            "start_s": round(started_s, 3),
            "sim_wait_s": round(sim.wait_s, 3),
            "python_s": round(sim_wall - sim.wait_s - started_s, 3),
            "calls": sim.calls,
            "ticks": pb.ticks if pb else 0,
            "compute_ms_mean": (round(sum(pb.engine.compute) / len(pb.engine.compute), 3)
                                if pb and pb.engine.compute else None),
            "playbook_s": (round(sum(pb.engine.compute) / 1000.0, 3)
                           if pb and pb.engine.compute else 0.0),
            "slow_ticks": pb.slow_ticks if pb else 0,
            # per task or request kind: seconds and polls, timed in the simulator (perf.rs); the
            # tasks run inside `call.*` requests, so they are parts of those, not added to them
            "rust": {k: {"s": round(v["s"], 4), "polls": v["polls"]} for k, v in sorted(rust_perf.items())},
        }
        if self.opts.agent == "model":
            # the wall time the model's wakes took (inside python_s): recorded, never simulated
            record["perf"]["model_wall_s"] = round(self.agent.wall_s, 3)
        if self.opts.keep is not None:
            self.opts.keep.mkdir(parents=True, exist_ok=True)
            for f in (audit_path, anchors, log):
                if f.exists():
                    shutil.copyfile(f, self.opts.keep / f.name)
        return record

    def sample(self, now: float) -> float | None:
        # the tracker's association every 10 s (a track id names one contact for the run)
        assoc = int(round(now)) % 10 == 0
        tr = self.sim.truth(association=assoc)
        c = tr.get("contacts") or {}
        if c and not self.contacts_meta:
            self.contacts_meta = [{k: x.get(k) for k in ("id", "name", "affiliation", "domain", "kind", "ais", "start_s")}
                                  for x in c.get("contacts", [])]
        # our vehicles as the stand-ins really are (diagnostics, and the truth of a P0 command's
        # fault); the contacts' truth is their own per-step log (truth_log)
        self.ours_samples.append({
            "t": round(tr["t"], 3),
            "ours": [[o["uid"], round(o["lon"], 7), round(o["lat"], 7), o["mode"], o["link_blocked"],
                      round(100.0 * (1.0 - o["battery_consumed_mah"] / o["battery_capacity_mah"]), 2)
                      if o.get("battery_capacity_mah") else None]
                     for o in tr.get("ours", [])],
        })
        for a in tr.get("association") or []:
            self.association.setdefault(a["track_id"], a["contact_id"])
        return now + self.opts.sample_s

    def picture_sample(self, now: float) -> float | None:
        """Scoring v2's picture sampler (SCORING-V2.md 2.6): one read at this instant, every
        `PICTURE_DT` of situation time to the end; the first also carries the fleet's platforms
        and sensors and the sensing model's draws' seed and rate."""
        from .v2.record import PICTURE_DT

        first = not self.picture_enc.times
        self.picture_enc.add(self.sim.picture(homes=first))
        return now + PICTURE_DT

    def truth_log(self, final: dict[str, Any]) -> None:
        """The contacts' truth, one sample per contacts step on the situation's clock (the
        process's start plus its scenario second), as fullstack.py reads contacts-truth.jsonl."""
        self.contacts_start_s = final.get("contacts_start_s")
        self.bridge_start_s = final.get("bridge_start_s")
        c0 = float(self.contacts_start_s or 0.0)
        self.truth_samples = [
            {"t": round(c0 + float(ln["s"]), 3),
             "contacts": [[x[0], round(x[1], 7), round(x[2], 7), round(x[3], 2), x[4]] for x in ln["contacts"]],
             "ours": []}
            for ln in final.get("log") or []
        ]

    def record(self, spec: dict[str, Any], runner: Runner, audit_text: str, cfg: Any) -> dict[str, Any]:
        lines = self.agent.lines if self.agent else []
        pb = self.agent if isinstance(self.agent, PlaybookAgent) else None
        meta = self.contacts_meta
        rec = {
            "schema": RECORD_SCHEMA,
            "situation": {"name": self.e.name, "family": self.e.family, "split": self.e.split,
                          "situation_seed": self.e.situation_seed, "path": str(self.e.path) if self.e.path else None},
            "seed": self.seed,
            "provenance": provenance(self.opts.binary, Path(spec["links"])),
            "playbook": {"engine": f"playbook-{ENGINE_VERSION}", "config_label": pb.cfg.label,
                         "client_version": pb.cfg.client_version} if pb else None,
            "operator": self.operator.describe() if self.operator else None,
            "clock": {"epoch": spec["epoch"], "at": self.e.at, "duration_s": self.e.duration_s,
                      "ai_start_s": self.e.ai_start_s, "calibration": self.opts.calibration.as_dict(),
                      "ai_first_tick_s": self.ai_first_tick_s, "contacts_start_s": self.contacts_start_s,
                      "bridge_start_s": self.bridge_start_s, "emulator_seed": spec.get("linkemu_seed"),
                      "ready_t": runner.ready_t},
            "orders": runner.orders,
            "agent_log": lines,
            "plans": pb.plans if pb else [],
            "operator_decisions": self.operator.decisions if self.operator else [],
            "script": self.script.log if self.script else [],
            "truth": {"contacts": meta, "samples": self.truth_samples, "ours": self.ours_samples,
                      "association": self.association, "stated": self.e.truth},
            "audit": audit_summary(audit_text),
            "audit_text": audit_text,
        }
        world = self.e.world_record()
        if world is not None:
            # world v3 only (eval/world.py): the world and what the simulator took from it; a
            # record without it is the Golden Gate's as before world v3
            rec["world"] = world
        if self.opts.agent == "model":
            # the model agent only (a playbook or none record has none of these keys): its config, every
            # wake, its totals; the provenance names the models and efforts
            rec["model"] = self.agent.describe()
            rec["budget_stopped"] = self.agent.budget_stopped
            # a model call the API refused as such (bad parameters, credentials): not the model's run
            rec["model_failed"] = self.agent.model_failed
            cfg_m = rec["model"]["config"]
            rec["provenance"] = {**rec["provenance"], "model": {
                "agent": "model", "routine": cfg_m["routine"], "consult": cfg_m["consult"],
                "fallbacks": cfg_m["fallbacks"], "max_tokens": cfg_m["max_tokens"],
                "prompt_sha256": hashlib.sha256(self.agent.system.encode()).hexdigest(),
                "tools_sha256": hashlib.sha256(json.dumps(self.agent.tools, sort_keys=True).encode()).hexdigest()}}
        if self.latency is not None:
            # charged only (a paused record is unchanged): the condition and its constants
            rec["clock"]["condition"] = "charged"
            rec["provenance"] = {**rec["provenance"], "clock": {"condition": "charged", "latency": self.latency.as_dict()}}
        if self.opts.agent in V2_AGENTS or self.opts.agent == "scripted" or self.opts.agent in V3_AGENTS:
            # an anchor's or a perturbation's own account: what it read, what it changed, and what
            # it read of truth (privileged; agents_v2.py, v3/oracle.py); the scripted agent's plan
            # and options
            rec["agent_detail"] = self.agent.describe()
        rec.update(self.section16_record())
        if self.opts.scoring in PICTURE_SCORINGS:
            rec.update(self.v2_part(audit_text, self.opts.scoring))
        return rec

    def section16(self, spec: dict[str, Any], env: dict[str, str]) -> None:
        """The simulator's side of docs/HEADLESS.md section 16, only where an option asks: the
        effects (`spec["effects"]`), and the pinned element sets unpacked once and given to the
        bridge's space loader (`MURMUR_ROOT`) and the emulator's SATCOM (`space_root`, its own
        look cache)."""
        if self.opts.effects is not None:
            spec["effects"] = dict(self.opts.effects)
        snapshot = self.space_snapshot
        if snapshot is not None:
            from . import space

            root = space.unpack(snapshot)
            env["MURMUR_ROOT"] = str(root)
            spec["space_root"] = str(root)
            if spec.get("space"):
                # the episode's link profiles (its world's: section 17; sim/links.toml at the
                # Golden Gate, as before)
                spec["space_cache"] = str(space.look_cache_file(snapshot, Path(spec["links"])))

    @property
    def space_snapshot(self) -> str | None:
        """The pinned element sets the episode reads: its options', else its world's (world v3's
        worlds name theirs: sim/worlds/README.md), else none (as before)."""
        return self.opts.space_snapshot if self.opts.space_snapshot is not None else self.e.space_snapshot

    def section16_record(self) -> dict[str, Any]:
        """The record's keys for section 16, each only when its option is on (a record without
        them is exactly as before)."""
        out: dict[str, Any] = {}
        if self.signers is not None:
            out["signers"] = {"policy": self.signers.describe(), "decisions": self.signers.decisions}
        if self.reviewer is not None:
            out["review"] = {"policy": self.reviewer.describe(), "decisions": self.reviewer.decisions}
        if self.opts.effects is not None:
            out["effects"] = {"config": dict(self.opts.effects), "applied": self.effects}
        if self.space_snapshot is not None:
            from . import space

            out["space"] = {"snapshot": space.describe(self.space_snapshot)}
        return out

    def v2_part(self, audit_text: str, scoring: str = "v2") -> dict[str, Any]:
        """Scoring v2's keys (SCORING-V2.md 2.6, 7): the episode class as the driver sets it,
        the replicate, the condition (operator, policy; the clock when charged), the picture
        samples, the audit log's digest, the invariants and the model's leakage scan. The
        episode sets `harness_error` on a leakage hit; the batch sets it for an episode that
        failed. Scoring v3 writes the same part under its own key (`v3`)."""
        from .v2.record import digest_audit, invariant_counts

        # the model's leakage scan (v2/leakage.py): a hit on an input is a harness error (7.1)
        scan = getattr(self.agent, "scan", None) if self.opts.agent == "model" else None
        if scan is not None and scan.leaked:
            cls = "harness_error"
        elif self.opts.agent == "model" and self.agent.budget_stopped:
            cls = "budget_stop"
        elif self.opts.agent == "model" and self.agent.model_failed:
            cls = "model_failed"
        else:
            cls = "scored"
        pic = self.picture_enc.finish()
        condition = {"operator": (self.operator.m.mode if self.operator else None),
                     "policy": Path(self.e.policy).name if self.e.policy else None}
        if getattr(self, "latency", None) is not None:  # charged only; no clock reads as paused
            condition["clock"] = "charged"
        v2 = {
            "agent": self.opts.agent,
            "condition": condition,
            "picture": pic,
            "picture_samples": len(pic["times"]),
            "audit": digest_audit(audit_text, self.sim_epoch),
            "invariants": invariant_counts(audit_text),
        }
        if scan is not None:
            v2["leakage"] = scan.record()
        return {
            "scoring": scoring,
            "episode_class": cls,
            "replicate": dict(self.opts.replicate) if self.opts.replicate else None,
            scoring: v2,
        }


def audit_summary(text: str) -> dict[str, Any]:
    lines = [ln for ln in text.splitlines() if ln.strip()]
    head = None
    if lines:
        try:
            head = json.loads(lines[-1]).get("hash")
        except ValueError:
            head = None
    return {"events": len(lines), "head": head, "sha256": hashlib.sha256(text.encode()).hexdigest()}


_PROV: dict[tuple[str | None, str | None], dict[str, Any]] = {}


def provenance(binary: Path | None, links: Path | None = None) -> dict[str, Any]:
    """The commit, whether the tree differed from it, the simulator's binary and its hash, and
    the untracked inputs it read (the link profiles, the episode's: its world's, the satellite
    element sets)."""
    key = (str(binary) if binary else None, str(links) if links else None)
    if key not in _PROV:
        from .provenance import git_state, inputs, sha256_file
        from .sim import BINARY
        from .situations import links_file

        b = Path(binary or BINARY)
        _PROV[key] = {**git_state(), "binary": str(b), "binary_sha256": sha256_file(b) if b.exists() else None,
                      **inputs(links or links_file())}
    return _PROV[key]
