#!/usr/bin/env python3
"""The evaluation runner (Phase G).

    python -m eval.run run eval/scenarios/plausible-wrong.toml --arm agent+human-proxy --seed 1
    python -m eval.run batch --scenarios plausible-wrong stale-track --arms rules-only human-proxy \
        --seeds 1 2 3 --parallel 3
    python -m eval.run report eval/results/<batch>
    python -m eval.run study eval/scenarios/plausible-wrong.toml --participant p01
    python -m eval.run measure
    python -m eval.run kill <run-dir>

One run = one isolated stack (eval/stack.py) on a port block (eval/ports.py), one arm
(eval/arms.py), one seed: the storyline is injected, the supervisor decides, truth is
sampled, and at the end the audit log, truth logs, link stats, tracks, intents and a
metrics.json land in the run directory. A batch runs seeds x arms x scenarios as separate
processes, a few at a time, then builds the HTML report.
"""

from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import json
import os
import platform
import subprocess
import sys
import time
import traceback
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any

from . import PYTHON, REPO
from .agent import make_proposer
from .arms import ARMS, Arm, arm as get_arm
from .bridge import Bridge
from .engage import EngageAdapter
from .metrics import compute, load_jsonl, run_replay
from .ports import Allocator, PortBlock, TIDEWATER_PORT
from .proxy import ProxyParams, make_supervisor, proxy_params
from .spec import Spec, SpecError, load_spec, spec_summary
from .stack import OPERATORS, Stack, StackError, kill_from_pids
from .storyline import Log, Storyline, TruthSampler, World

SCENARIOS = REPO / "eval" / "scenarios"
RESULTS = REPO / "eval" / "results"
TIDEWATER_SCENARIO = Path(os.environ.get("TIDEWATER_HOME", Path.home() / "tidewater")) / "scenarios" / "golden-gate-2026-06.toml"
TIDEWATER_BIN = Path(os.environ.get("TIDEWATER_HOME", Path.home() / "tidewater")) / ".venv" / "bin" / "tidewater"


def now_id() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def find_scenario(name: str) -> Path:
    p = Path(name).expanduser()
    if p.exists():
        return p.resolve()
    for cand in (SCENARIOS / f"{name}.toml", SCENARIOS / "engagement" / f"{name}.toml"):
        if cand.exists():
            return cand
    raise SystemExit(f"no scenario {name!r}; see {SCENARIOS}")


# ---- tidewater ---------------------------------------------------------------------------------


def _answers(url: str) -> bool:
    return Bridge(url).try_get("/v1/info") is not None


def ensure_tidewater(mode: str, out_dir: Path | None) -> tuple[str | None, subprocess.Popen | None, str]:
    """A `tidewater serve` to share: the one in MURMUR_TIDEWATER_URL, the default stack's
    at :8770, or one started here for the batch (stopped by the caller). Returns (url,
    process or None, note)."""
    if mode == "none":
        return None, None, "tidewater: off (spec)"
    env_url = os.environ.get("MURMUR_TIDEWATER_URL", "").strip()
    if env_url and _answers(env_url):
        return env_url, None, f"tidewater: using {env_url} (MURMUR_TIDEWATER_URL)"
    if _answers("http://127.0.0.1:8770"):
        return "http://127.0.0.1:8770", None, "tidewater: using the default stack's serve at :8770"
    url = f"http://127.0.0.1:{TIDEWATER_PORT}"
    if _answers(url):
        return url, None, f"tidewater: using the one already at {url}"
    if not TIDEWATER_BIN.exists() or not TIDEWATER_SCENARIO.exists():
        return None, None, f"tidewater: not available ({TIDEWATER_BIN} or {TIDEWATER_SCENARIO} missing); running without conditions"
    log = (out_dir or Path.cwd()) / "tidewater-serve.log"
    f = open(log, "w")
    p = subprocess.Popen(
        [str(TIDEWATER_BIN), "serve", str(TIDEWATER_SCENARIO), "--port", str(TIDEWATER_PORT)],
        stdout=f,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    f.close()
    deadline = time.monotonic() + 150
    while time.monotonic() < deadline:
        if _answers(url):
            return url, p, f"tidewater: started serve on :{TIDEWATER_PORT} (pid {p.pid})"
        if p.poll() is not None:
            return None, None, f"tidewater serve exited ({p.returncode}); see {log}; running without conditions"
        time.sleep(1)
    stop_process(p)
    return None, None, "tidewater serve did not answer in 150 s; running without conditions"


def stop_process(p: subprocess.Popen | None) -> None:
    if p is None or p.poll() is not None:
        return
    try:
        os.killpg(os.getpgid(p.pid), 15)
        p.wait(timeout=10)
    except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
        try:
            os.killpg(os.getpgid(p.pid), 9)
        except (ProcessLookupError, PermissionError):
            pass


# ---- one run -----------------------------------------------------------------------------------


class Run:
    def __init__(
        self,
        spec: Spec,
        arm: Arm,
        seed: int,
        out: Path,
        block: PortBlock,
        tidewater_url: str | None,
        proxy: ProxyParams,
        model: str | None = None,
        participant: str | None = None,
        duration_s: float | None = None,
    ):
        self.spec = spec
        self.arm = arm
        self.seed = seed
        self.out = out
        self.block = block
        self.tidewater_url = tidewater_url
        self.proxy = proxy
        self.model = model
        self.participant = participant
        self.duration_s = duration_s if duration_s is not None else spec.duration_s
        self.run_id = f"eval-{spec.name}-{arm.name}-s{seed}-{now_id()}".replace("+", "-")
        self.info: dict[str, Any] = {
            "run_id": self.run_id,
            "scenario": spec.name,
            "arm": arm.name,
            "seed": seed,
            "proposer": arm.proposer,
            "supervisor": arm.supervisor,
            "operators": OPERATORS,
            "ports": block.as_dict(),
            "spec": spec_summary(spec),
            "started": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "status": "starting",
            "warnings": [],
            "notes": [],
        }
        if arm.supervisor == "proxy":
            self.info["proxy"] = {**proxy.as_dict(), "note": "a scripted proxy, not a human; see eval/proxy.py"}
        if participant:
            self.info["participant"] = participant

    def save(self) -> None:
        (self.out / "run.json").write_text(json.dumps(self.info, indent=1))

    async def execute(self) -> dict[str, Any]:
        self.out.mkdir(parents=True, exist_ok=True)
        stack = Stack(self.spec, self.seed, self.block, self.out, self.run_id, self.tidewater_url, web=self.arm.web)
        self.info["tidewater_url"] = self.tidewater_url
        self.save()
        proposer = None
        try:
            stack.start()
            self.info["status"] = "waiting for the fleet"
            self.save()
            ready = stack.wait_ready(self.spec.ready_timeout_s)
            if not ready.get("ready"):
                self.info["warnings"].append(f"fleet not fully ready after {self.spec.ready_timeout_s:.0f} s: {ready}")
            ready["contacts_truth_after_s"] = stack.start_contacts()
            if self.spec.contacts and ready["contacts_truth_after_s"] is None:
                self.info["warnings"].append("no contact truth reached the bridge within 30 s of starting the contacts")
            self.info["ready"] = ready
            t_ready = time.time()
            self.info["ready_at_s"] = round(t_ready - (stack.started_at or t_ready), 1)
            bridge = stack.bridge
            mcp_url = f"http://127.0.0.1:{self.block.http}/mcp"
            if self.arm.web:
                self.info["console"] = f"http://127.0.0.1:{self.block.web}"
                print(f"console for the participant: http://127.0.0.1:{self.block.web}", flush=True)
            proposer = make_proposer(self.arm.proposer, bridge, mcp_url, self.model, self.spec.agent_brief)
            await proposer.start()
            supervisor = make_supervisor(self.arm.supervisor, bridge, self.proxy, self.seed)
            self.info["supervisor_config"] = supervisor.describe()
            log = Log(self.out / "storyline.jsonl")
            log.write({"kind": "phase", "name": "ready", "ready_after_s": ready.get("ready_after_s"), "vehicles": ready.get("vehicles")})
            story = Storyline(
                self.spec,
                bridge,
                proposer,
                World(bridge, self.out),
                log,
                t_ready,
                events_file=stack.events_file,
                emu_url=stack.emu_url,
                engage=EngageAdapter(bridge),
            )
            sampler = TruthSampler(bridge, self.out / "truth.jsonl")
            stop = asyncio.Event()
            self.info["status"] = "running"
            self.save()
            tasks = [
                asyncio.create_task(story.run(stop)),
                asyncio.create_task(supervisor.run(stop)),
                asyncio.create_task(sampler.run(stop)),
            ]
            end = t_ready + self.duration_s
            while time.time() < end:
                dead = [p.name for p in stack.procs if not p.alive()]
                if dead:
                    self.info["warnings"].append(f"{', '.join(dead)} exited during the run")
                    break
                await asyncio.sleep(1.0)
            stop.set()
            await asyncio.wait(tasks, timeout=30)
            for t in tasks:
                if not t.done():
                    t.cancel()
                elif t.exception():
                    self.info["warnings"].append(f"task failed: {t.exception()!r}")
            log.write({"kind": "phase", "name": "end"})
            self.info["decisions"] = getattr(supervisor, "decisions", [])
            if hasattr(proposer, "describe"):
                self.info["proposer_config"] = proposer.describe()
            if getattr(proposer, "transcript", None):
                (self.out / "model-transcript.json").write_text(json.dumps(proposer.transcript, indent=1))
            # final reads
            for name, path in (("intents", "/api/intents"), ("inbox", "/api/inbox"), ("links", "/api/links"), ("tracks", "/api/tracks"), ("stats", "/api/stats")):
                data = bridge.try_get(path)
                if data is not None:
                    (self.out / f"{name}.json").write_text(json.dumps(data, indent=1))
            self.info["status"] = "collected"
        except asyncio.CancelledError:
            self.info["status"] = "interrupted"
            raise
        except StackError as e:
            self.info["status"] = "failed"
            self.info["error"] = str(e)
        except Exception as e:  # noqa: BLE001 (recorded, never swallowed)
            self.info["status"] = "failed"
            self.info["error"] = f"{e!r}\n{traceback.format_exc()[-2000:]}"
        finally:
            if proposer is not None:
                try:
                    await proposer.stop()
                except Exception as e:  # noqa: BLE001
                    self.info["warnings"].append(f"proposer stop: {e!r}")
            self.info["exits"] = stack.stop()
            self.info["ended"] = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")
            self.save()
        # replay and metrics (also for failed runs: whatever log exists is scored)
        self.info["replay"] = run_replay(self.out / "audit.jsonl")
        if self.info["replay"].get("ran") and not self.info["replay"].get("ok"):
            self.info["warnings"].append("replay reported mismatches")
        try:
            m = compute(load_jsonl(self.out / "audit.jsonl"), load_jsonl(self.out / "storyline.jsonl"), set(OPERATORS), self.info["replay"], self.arm.name)
            (self.out / "metrics.json").write_text(json.dumps(m, indent=1))
            self.info["summary"] = {
                "intents": m["counts"]["intents"],
                "should_reject": m["should_reject"]["n"],
                "caught": m["should_reject"]["k"],
                "should_void": m["should_void"]["n"],
                "voided": m["should_void"]["k"],
                "human_decisions": m["counts"]["human_decisions"],
                "violations": m["invariants"]["violations"],
                "storyline_errors": m["counts"]["storyline_errors"],
            }
        except Exception as e:  # noqa: BLE001
            self.info["warnings"].append(f"metrics failed: {e!r}")
        if self.info["status"] == "collected":
            self.info["status"] = "ok"
        self.save()
        return self.info


def do_run(args: argparse.Namespace) -> int:
    spec = load_spec(find_scenario(args.scenario))
    a = get_arm(args.arm)
    if a.external_model and not os.environ.get("MURMUR_EVAL_MODEL"):
        raise SystemExit("the agent+model arm calls an external API: set MURMUR_EVAL_MODEL=1 to allow it (never in tests)")
    if spec.placeholder and not args.include_placeholders:
        raise SystemExit(f"{spec.name} requires {spec.requires}: a placeholder until the bridge has it (--include-placeholders to run anyway)")
    out = Path(args.out) if args.out else RESULTS / "single" / f"{spec.name}-{args.arm}-s{args.seed}-{now_id()}"
    out.mkdir(parents=True, exist_ok=True)
    alloc = Allocator()
    block = alloc.allocate(args.block)
    tw_proc = None
    tidewater_url = args.tidewater_url
    note = ""
    if tidewater_url is None:
        tidewater_url, tw_proc, note = ensure_tidewater(args.tidewater or spec.tidewater, out)
    try:
        run = Run(spec, a, args.seed, out, block, tidewater_url, proxy_params(spec.proxy, args.proxy), args.model, args.participant, args.duration)
        if note:
            run.info["notes"].append(note)
        try:
            info = asyncio.run(run.execute())
        except KeyboardInterrupt:
            print(f"interrupted: the stack is stopped; partial run in {out}", file=sys.stderr)
            return 130
    finally:
        stop_process(tw_proc)
        alloc.release(block)
    print(json.dumps({k: info.get(k) for k in ("run_id", "status", "error", "warnings", "summary")}, indent=1))
    print(f"run directory: {out}")
    return 0 if info.get("status") == "ok" else 1


# ---- batch -------------------------------------------------------------------------------------


def machine() -> dict[str, Any]:
    cpus = os.cpu_count() or 1
    mem_gb = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                mem_gb = round(int(line.split()[1]) / 1e6, 1)
    except OSError:
        pass
    load = os.getloadavg()[0] if hasattr(os, "getloadavg") else None
    free_blocks = Allocator().free_count()
    # one stack is five light processes (bridge ~3% of a core, stand-ins and the emulator
    # under 1% each at this fleet size); the proxy and the sampler are idle most of the
    # time. Three stacks per 12 cores is conservative; memory is not the limit.
    recommended = max(1, min(free_blocks, cpus // 4, 6))
    return {"cpus": cpus, "mem_gb": mem_gb, "load_1m": load, "free_port_blocks": free_blocks, "recommended_parallel": recommended, "platform": platform.platform()}


def do_measure(args: argparse.Namespace) -> int:
    print(json.dumps(machine(), indent=1))
    return 0


def do_batch(args: argparse.Namespace) -> int:
    scenarios = [load_spec(find_scenario(s)) for s in args.scenarios]
    arms = [get_arm(a) for a in args.arms]
    for a in arms:
        if a.external_model and not os.environ.get("MURMUR_EVAL_MODEL"):
            raise SystemExit("the agent+model arm calls an external API: set MURMUR_EVAL_MODEL=1 to allow it")
        if a.web:
            raise SystemExit("study mode is interactive: use `python -m eval.run study`")
    out = Path(args.out) if args.out else RESULTS / now_id()
    out.mkdir(parents=True, exist_ok=True)
    m = machine()
    parallel = args.parallel or m["recommended_parallel"]
    parallel = max(1, min(parallel, m["free_port_blocks"]))
    jobs = []
    skipped = []
    for s in scenarios:
        if s.placeholder and not args.include_placeholders:
            skipped.append({"scenario": s.name, "reason": f"requires {s.requires} (placeholder)"})
            continue
        for a in arms:
            for seed in args.seeds:
                jobs.append((s, a, seed))
    tidewater_url, tw_proc, note = (None, None, "tidewater: off (--tidewater none)") if args.tidewater == "none" else ensure_tidewater("shared", out)
    batch: dict[str, Any] = {
        "id": out.name,
        "started": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "machine": m,
        "parallel": parallel,
        "scenarios": [spec_summary(s) for s in scenarios],
        "arms": [a.name for a in arms],
        "seeds": list(args.seeds),
        "proxy_preset": args.proxy,
        "tidewater": note,
        "skipped": skipped,
        "runs": [],
    }
    (out / "batch.json").write_text(json.dumps(batch, indent=1))
    print(f"batch {out.name}: {len(jobs)} runs, {parallel} in parallel; {note}", flush=True)
    alloc = Allocator()
    results: list[dict[str, Any]] = []

    def one(job: tuple[Spec, Arm, int]) -> dict[str, Any]:
        s, a, seed = job
        run_dir = out / s.name / a.name / f"seed-{seed}"
        run_dir.mkdir(parents=True, exist_ok=True)
        block = alloc.allocate()
        cmd = [
            str(PYTHON), "-m", "eval.run", "run", str(s.path),
            "--arm", a.name, "--seed", str(seed), "--out", str(run_dir),
            "--block", str(block.index), "--tidewater-url", tidewater_url or "none",
        ]
        if args.proxy:
            cmd += ["--proxy", args.proxy]
        if args.duration:
            cmd += ["--duration", str(args.duration)]
        if args.include_placeholders:
            cmd.append("--include-placeholders")
        t0 = time.time()
        with open(run_dir / "runner.log", "w") as f:
            p = subprocess.run(cmd, cwd=str(REPO), stdout=f, stderr=subprocess.STDOUT, env={**os.environ, "PYTHONUNBUFFERED": "1"})
        alloc.release(block)
        info = {}
        if (run_dir / "run.json").exists():
            try:
                info = json.loads((run_dir / "run.json").read_text())
            except ValueError:
                info = {}
        rec = {
            "scenario": s.name,
            "arm": a.name,
            "seed": seed,
            "dir": str(run_dir.relative_to(out)),
            "exit": p.returncode,
            "status": info.get("status", "no run.json"),
            "error": info.get("error"),
            "warnings": info.get("warnings", []),
            "summary": info.get("summary"),
            "wall_s": round(time.time() - t0, 1),
        }
        print(f"  {s.name} / {a.name} / seed {seed}: {rec['status']} in {rec['wall_s']} s", flush=True)
        return rec

    try:
        with ThreadPoolExecutor(max_workers=parallel) as pool:
            futures = [pool.submit(one, j) for j in jobs]
            for fut in as_completed(futures):
                results.append(fut.result())
    finally:
        stop_process(tw_proc)
    results.sort(key=lambda r: (r["scenario"], r["arm"], r["seed"]))
    batch["runs"] = results
    batch["ended"] = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")
    (out / "batch.json").write_text(json.dumps(batch, indent=1))
    # in a fresh interpreter: a report that fails never costs the batch, and it is built by
    # the code on disk now, not the code this process loaded an hour ago
    rep = subprocess.run([str(PYTHON), "-m", "eval.report", str(out)], cwd=str(REPO), capture_output=True, text=True)
    report = rep.stdout.strip() if rep.returncode == 0 else f"failed ({rep.stderr.strip()[-500:]}); rebuild with python -m eval.run report {out}"
    ok = sum(1 for r in results if r["status"] == "ok")
    print(f"{ok}/{len(results)} runs ok; report: {report}")
    return 0 if ok == len(results) else 1


def do_report(args: argparse.Namespace) -> int:
    from .report import build

    print(build(Path(args.dir)))
    return 0


def do_kill(args: argparse.Namespace) -> int:
    stopped = kill_from_pids(Path(args.dir))
    print("stopped: " + (", ".join(stopped) if stopped else "nothing (no live pids)"))
    return 0


def do_study(args: argparse.Namespace) -> int:
    """Study mode: a real participant on the console; the runner only records. The
    participant id is an anonymous label chosen by the study (see eval/README.md)."""
    args.arm = "study"
    args.out = args.out or str(RESULTS / "study" / f"{args.participant}-{Path(args.scenario).stem}-{now_id()}")
    args.include_placeholders = False
    args.model = None
    print(
        "Study session. Before starting: consent obtained, the participant knows the run is "
        "recorded (decisions and what the console showed, under an anonymous id, nothing else), "
        "and can stop at any time.",
        flush=True,
    )
    return do_run(args)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python -m eval.run", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    def common(p: argparse.ArgumentParser) -> None:
        p.add_argument("--seed", type=int, default=1)
        p.add_argument("--out", help="run directory (default eval/results/single/...)")
        p.add_argument("--block", type=int, help="port block index (default: first free)")
        p.add_argument("--proxy", help=f"proxy preset: {', '.join(['default', 'diligent', 'rubber-stamp', 'fatigued', 'trusting'])}")
        p.add_argument("--tidewater", choices=["shared", "none"], help="override the spec's tidewater mode")
        p.add_argument("--tidewater-url", help="an existing tidewater serve ('none' for none)")
        p.add_argument("--duration", type=float, help="override the spec's duration_s")
        p.add_argument("--include-placeholders", action="store_true", help="run scenarios whose requirements the bridge lacks (steps are skipped)")

    r = sub.add_parser("run", help="one scenario, one arm, one seed")
    r.add_argument("scenario")
    r.add_argument("--arm", default="agent+human-proxy", choices=list(ARMS))
    r.add_argument("--model", help="agent+model: the model id (default claude-opus-5-5)")
    r.add_argument("--participant", help=argparse.SUPPRESS)
    common(r)

    b = sub.add_parser("batch", help="scenarios x arms x seeds, in parallel")
    b.add_argument("--scenarios", nargs="+", required=True)
    b.add_argument("--arms", nargs="+", default=["rules-only", "human-proxy", "agent+human-proxy"], choices=list(ARMS))
    b.add_argument("--seeds", nargs="+", type=int, default=[1, 2, 3])
    b.add_argument("--parallel", type=int, help="stacks at once (default: measured, conservative)")
    b.add_argument("--out")
    b.add_argument("--proxy")
    b.add_argument("--tidewater", choices=["shared", "none"], default="shared")
    b.add_argument("--duration", type=float)
    b.add_argument("--include-placeholders", action="store_true")

    s = sub.add_parser("study", help="a real participant on the console; the runner records")
    s.add_argument("scenario")
    s.add_argument("--participant", required=True, help="an anonymous id, e.g. p01")
    common(s)

    rp = sub.add_parser("report", help="build report.html for a batch directory")
    rp.add_argument("dir")
    k = sub.add_parser("kill", help="stop the processes a run directory's pids.json names")
    k.add_argument("dir")
    sub.add_parser("measure", help="CPUs, memory, free port blocks, recommended parallelism")

    args = ap.parse_args(argv)
    if getattr(args, "tidewater_url", None) == "none":
        args.tidewater_url = None
        args.tidewater = "none"
    try:
        return {"run": do_run, "batch": do_batch, "report": do_report, "kill": do_kill, "measure": do_measure, "study": do_study}[args.cmd](args)
    except SpecError as e:
        print(f"spec error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
