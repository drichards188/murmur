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
  two steps of the clock. Or none.
- **the operator** (operator.py).
- **the script**: the situation's faults and scripted proposers (families.py).
- **the truth sampler**: our vehicles every `sample_s` and the tracker's association every 10 s;
  the contacts' truth is their own per-step log, read at the end (never given to the AI).
- **the picture sampler** (scoring v2 only, `Options.scoring = "v2"`; eval/SCORING-V2.md 2.6):
  every 1 s of situation time, the picture as the bridge serves it, the tracker's association,
  the inbox's alerts and our vehicles' truth, one request that never moves the clock, kept as
  change points (v2/record.py). A v2 record also carries `episode_class` (section 7: the driver
  sets it), the replicate, the audit log's digest and the invariants. A v1 episode runs none of
  this, so its record is unchanged.

Simultaneous events run in a fixed order (runner, script, operator, AI, sampler, picture) and
each process's actions happen at one simulated instant, so an episode is a function of
(situation, seed, calibration, operator model).
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
PRIORITY = {"runner": 0, "script": 1, "operator": 2, "agent": 3, "sampler": 4, "picture": 5}
AGENTS = ("playbook", "model", "none")
SCORINGS = ("v1", "v2")
#: SCORING-V2.md section 7: the classes the driver sets (the batch adds `harness_error` for an
#: episode that failed in the simulator or the driver)
EPISODE_CLASSES = ("scored", "harness_error", "budget_stop", "model_failed")


@dataclass
class Options:
    agent: str = "playbook"  # playbook | model | none
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


class Loop:
    """The event loop: processes wake at simulated times; the simulator steps between."""

    def __init__(self, sim: Sim):
        self.sim = sim
        self.q: list[tuple[float, int, int, str]] = []
        self.n = 0
        self.handlers: dict[str, Any] = {}

    def at(self, t: float | None, name: str) -> None:
        if t is None:
            return
        self.n += 1
        heapq.heappush(self.q, (round(float(t), 6), PRIORITY[name], self.n, name))

    def run(self, end: float) -> None:
        while self.q:
            t, _, _, name = heapq.heappop(self.q)
            if t > end:
                break
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
            ready = {v["uid"] for v in vehicles if v.get("nav_ready") and v.get("position")}
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
        try:
            rep = self.ep.run_async(self.engine.tick(self.session))
            self.ticks += 1
            self.misses = 0
            if rep.compute_ms > 50.0:
                self.slow_ticks += 1
        except Exception as e:  # noqa: BLE001 (three in a row: a new session, as the agent does)
            self.misses += 1
            self.errors += 1
            self.write("error", error=f"{type(e).__name__}: {e}", misses=self.misses)
            if self.misses >= 3:
                self.started = False
        self.next_at = now + float(self.cfg["tick_s"])

    def wake(self, now: float) -> float | None:
        if not self.started:
            if not self._start(now):
                delay, self.retry_s = self.retry_s, min(self.retry_s * 2.0, 30.0)
                return now + delay
            self._tick(now)
            if not self.started:  # three misses in a row: a new session at once, as live
                return now
            return min(now + POLL_S, self.next_at or now)
        assert self.next_at is not None
        try:
            new = self.ep.run_async(self.engine.poll_changes(self.session))
        except Exception:  # noqa: BLE001 (the tick reads again)
            new = []
            self._tick(now)
            if not self.started:
                return now
            return min(now + POLL_S, self.next_at)
        if waking(new) or now >= self.next_at - 1e-9:
            self._tick(now)
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
        self.picture_enc: Any = None  # v2/record.py PictureEncoder (scoring v2 only)

    def run_async(self, coro: Any) -> Any:
        return self.aloop.run_until_complete(coro)

    def start_operator(self, now: float) -> None:
        if self.operator is not None:
            self.loop.at(self.operator.start(now), "operator")

    def run(self) -> dict[str, Any]:
        wall0 = time.perf_counter()
        workdir = self.workdir
        workdir.mkdir(parents=True, exist_ok=True)
        spec, env = process_spec(self.e, self.seed, workdir, self.opts.calibration, terrain=self.opts.terrain,
                                 space=self.opts.space, anchors=self.opts.anchors, contacts_path=self.contacts_path)
        self.want = {int(v["uid"]) for v in spec["fleet"]}
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
            cfg = make_config(json.loads(playbook_json(self.e)), self.e.config_name or self.e.name)  # type: ignore[arg-type]
            if self.opts.agent not in AGENTS:
                raise ValueError(f"unknown agent {self.opts.agent!r}: {', '.join(AGENTS)}")
            if self.opts.agent == "model":
                from .officer import ModelConfig, WatchOfficer

                self.agent = WatchOfficer(self, ModelConfig.resolve(self.opts.model), client=self.opts.model_client)
            elif self.opts.agent == "playbook":
                self.agent = PlaybookAgent(self, cfg)
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
            if self.opts.scoring == "v2":
                from .v2.record import PictureEncoder

                self.picture_enc = PictureEncoder(self.sim_epoch)
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
            "provenance": provenance(self.opts.binary),
            "playbook": {"engine": f"playbook-{ENGINE_VERSION}", "config_label": cfg.label,
                         "client_version": cfg.client_version} if pb else None,
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
        if self.opts.scoring == "v2":
            rec.update(self.v2_part(audit_text))
        return rec

    def v2_part(self, audit_text: str) -> dict[str, Any]:
        """Scoring v2's keys (SCORING-V2.md 2.6, 7): the episode class as the driver sets it,
        the replicate, the condition (operator, policy), the picture samples, the audit log's
        digest and the invariants. The batch sets `harness_error` for an episode that failed."""
        from .v2.record import digest_audit, invariant_counts

        if self.opts.agent == "model" and self.agent.budget_stopped:
            cls = "budget_stop"
        elif self.opts.agent == "model" and self.agent.model_failed:
            cls = "model_failed"
        else:
            cls = "scored"
        pic = self.picture_enc.finish()
        return {
            "scoring": "v2",
            "episode_class": cls,
            "replicate": dict(self.opts.replicate) if self.opts.replicate else None,
            "v2": {
                "agent": self.opts.agent,
                "condition": {"operator": (self.operator.m.mode if self.operator else None),
                              "policy": Path(self.e.policy).name if self.e.policy else None},
                "picture": pic,
                "picture_samples": len(pic["times"]),
                "audit": digest_audit(audit_text, self.sim_epoch),
                "invariants": invariant_counts(audit_text),
            },
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


_PROV: dict[str, Any] | None = None


def provenance(binary: Path | None) -> dict[str, Any]:
    """The commit, whether the tree differed from it, the simulator's binary and its hash, and
    the untracked inputs it read (the link profiles, the satellite element sets)."""
    global _PROV
    if _PROV is None:
        from .provenance import git_state, inputs, sha256_file
        from .sim import BINARY
        from .situations import links_file

        b = Path(binary or BINARY)
        _PROV = {**git_state(), "binary": str(b), "binary_sha256": sha256_file(b) if b.exists() else None,
                 **inputs(links_file())}
    return _PROV
