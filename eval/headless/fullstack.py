"""Full-stack Harbor watch runs for the fidelity gate (CRITERIA.md, G1), and the same episode
record built from their files, so one metrics function scores both sides.

    .venv/bin/python -m eval.headless.fullstack run --seeds 101 102 103 --out logs/headless-g1/calibration
    .venv/bin/python -m eval.headless.fullstack record logs/headless-g1/gate/seed-1

`run`: for each seed, a copy of `sim/situations/harbor-watch.toml` with only `seed = k` changed
(the autoplay operator's draws), then `make situation NAME=<copy> AI=playbook OPERATOR=autoplay`
on the assigned port block (CRITERIA.md) with `MURMUR_LINKEMU_SEED` = k (the link emulator's
draws, as the simulator's for that copy: `stack_env`), one run at a time: the commit, the
binaries' hashes and the untracked inputs recorded (`provenance.json`), the ports checked free first; after
the run, while the stack is still up, the start time of each of its processes (from /proc) and
the contacts' truth log are kept with the run; then `make down`, and the audit log replayed with
its anchors (`murmur-replay --anchors`).

`record`: the run's playbook log (`agent.jsonl`), its runner's record (`situation.jsonl`), its
audit log, its contacts' truth log and its process start times, as an episode record of
eval/headless/episode.py's schema (times on the situation's clock).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from . import REPO

PORTS = {
    "MURMUR_BASE_PORT": "29000",
    "MURMUR_BRIDGE_BASE_PORT": "30000",
    "MURMUR_HTTP_PORT": "9565",
    "MURMUR_LINKEMU_PORT": "9566",
    "MURMUR_WEB_PORT": "5973",
    "MURMUR_TIDEWATER_PORT": "9570",
    "MURMUR_SITL_INSTANCE_BASE": "220",
}
SITUATION = REPO / "sim" / "situations" / "harbor-watch.toml"
CLK_TCK = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100


def say(msg: str) -> None:
    print(f"[fullstack {time.strftime('%H:%M:%S')}] {msg}", file=sys.stderr, flush=True)


# ---- running ---------------------------------------------------------------------------------

def listening(n_vehicles: int = 4) -> list[str]:
    """What answers on this block's ports now."""
    tcp = {int(PORTS[k]) for k in ("MURMUR_HTTP_PORT", "MURMUR_LINKEMU_PORT", "MURMUR_WEB_PORT", "MURMUR_TIDEWATER_PORT")}
    udp = {int(PORTS["MURMUR_BASE_PORT"]) + i for i in range(n_vehicles)} | \
          {int(PORTS["MURMUR_BRIDGE_BASE_PORT"]) + i for i in range(n_vehicles)}
    busy = []
    for proto, want in (("-ltn", tcp), ("-lun", udp)):
        out = subprocess.run(["ss", "-H", proto], capture_output=True, text=True).stdout
        for line in out.splitlines():
            m = re.search(r":(\d+)\s", line.split(None, 4)[3] + " ") if len(line.split()) > 3 else None
            if m and int(m.group(1)) in want:
                busy.append(f"{proto} {m.group(1)}")
    return busy


def proc_starts(http_port: str) -> dict[str, float]:
    """Unix start time of each process of this stack (by its environment or command line)."""
    btime = 0.0
    for line in Path("/proc/stat").read_text().splitlines():
        if line.startswith("btime "):
            btime = float(line.split()[1])
    out: dict[str, float] = {}
    names = {"murmur-linkemu": "linkemu", "murmur-bridge": "bridge", "murmur-contacts": "contacts",
             "murmur-standins": "standins", "playbook_agent.py": "playbook"}
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        try:
            cmd = (d / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
            env = (d / "environ").read_bytes()
            stat = (d / "stat").read_text()
        except OSError:
            continue
        mine = f"MURMUR_HTTP_PORT={http_port}".encode() in env or f"127.0.0.1:{http_port}/mcp" in cmd
        if not mine:
            continue
        for key, name in names.items():
            if key in cmd and name not in out:
                start_ticks = float(stat.rsplit(")", 1)[1].split()[19])
                out[name] = round(btime + start_ticks / CLK_TCK, 2)
    return out


def stack_env(copy: Path) -> dict[str, str]:
    """The environment `make situation` runs a copy with: this block's ports, and the link
    emulator's seed the simulator gives the same situation (situations.emulator_seed)."""
    from .situations import emulator_seed, from_situation

    env = {**os.environ, **PORTS}
    env.pop("MURMUR_TIDEWATER_URL", None)
    env["MURMUR_LINKEMU_SEED"] = str(emulator_seed(from_situation(copy)))
    return env


def stack_provenance(env: dict[str, str]) -> dict[str, Any]:
    """What a full-stack run was made from: the commit (and whether the tree differed), the
    binaries `make situation` runs (scripts/up.sh builds them first; hashed before it starts, so
    a rebuild would show), the emulator's seed, and the untracked inputs."""
    from .provenance import git_state, inputs, stack_binaries

    links = Path(env.get("MURMUR_LINKS") or REPO / "sim" / "links.toml")
    return {**git_state(), "binaries": stack_binaries(), "emulator_seed": int(env["MURMUR_LINKEMU_SEED"]),
            **inputs(links), "load_avg_at_start": [round(x, 2) for x in os.getloadavg()]}


def run_one(seed: int, out_root: Path) -> Path:
    out_root.mkdir(parents=True, exist_ok=True)
    busy = listening()
    if busy:
        raise SystemExit(f"ports of this block are in use ({', '.join(busy)}): not starting")
    sits = out_root / "situations"
    sits.mkdir(exist_ok=True)
    copy = sits / f"harbor-watch-seed{seed}.toml"
    text = SITUATION.read_text()
    new, n = re.subn(r"(?m)^seed = \d+", f"seed = {seed}", text)
    if n != 1:
        raise SystemExit("harbor-watch.toml: no single `seed = ` line")
    copy.write_text(new)
    env = stack_env(copy)
    prov = stack_provenance(env)
    say(f"seed {seed}: make situation (about 13 minutes; emulator seed {env['MURMUR_LINKEMU_SEED']})")
    t0 = time.time()
    with open(out_root / f"seed-{seed}.make.log", "w") as log:
        code = subprocess.run(["make", "situation", f"NAME={copy}", "AI=playbook", "OPERATOR=autoplay"],
                              cwd=REPO, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
    starts = proc_starts(PORTS["MURMUR_HTTP_PORT"])
    runs = sorted((REPO / "logs" / "situations").glob(f"situation-harbor-watch-seed{seed}-*"),
                  key=lambda p: p.stat().st_mtime)
    if not runs:
        subprocess.run(["make", "down"], cwd=REPO, env=env)
        raise SystemExit(f"seed {seed}: no situation run directory (make exited {code})")
    run_dir = runs[-1]
    dest = out_root / f"seed-{seed}"
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(run_dir, dest)
    shutil.copyfile(REPO / "logs" / "contacts-truth.jsonl", dest / "contacts-truth.jsonl")
    (dest / "proc-starts.json").write_text(json.dumps({"starts": starts, "make_exit": code,
                                                       "wall_s": round(time.time() - t0, 1)}, indent=1))
    (dest / "provenance.json").write_text(json.dumps(prov, indent=1))
    run_id = run_dir.name
    say(f"seed {seed}: {run_id}, make exit {code}; make down")
    subprocess.run(["make", "down"], cwd=REPO, env=env, stdout=subprocess.DEVNULL)
    for _ in range(60):
        if not listening():
            break
        time.sleep(1)
    audit = REPO / "logs" / "audit" / f"{run_id}.jsonl"
    anchors = REPO / "logs" / "anchors" / f"{run_id}.anchors"
    for f in (audit, anchors):
        if f.exists():
            shutil.copyfile(f, dest / f.name)
    rep = subprocess.run([str(REPO / "target" / "release" / "murmur-replay"), str(dest / audit.name), "--anchors",
                          str(dest / anchors.name)], capture_output=True, text=True)
    (dest / "replay.txt").write_text(rep.stdout + rep.stderr + f"\nexit {rep.returncode}\n")
    say(f"seed {seed}: replay exit {rep.returncode}")
    return dest


# ---- the record ------------------------------------------------------------------------------

def _jsonl(p: Path) -> list[dict[str, Any]]:
    out = []
    if p.exists():
        for line in p.read_text().splitlines():
            line = line.strip()
            if line:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    pass
    return out


def anchor_offset(anchors: Path) -> float:
    """UTC minus the bridge's clock, s (the median over the run's anchors)."""
    diffs = []
    for a in _jsonl(anchors):
        b, u = a.get("t"), a.get("utc")
        if isinstance(b, (int, float)) and isinstance(u, (int, float)):
            diffs.append((u - b) / 1000.0)
    if not diffs:
        return 0.0
    diffs.sort()
    return diffs[len(diffs) // 2]


def record(run: Path) -> dict[str, Any]:
    """A full-stack run as an episode record (eval/headless/episode.py's schema; the fields the
    metrics read)."""
    sit_lines = _jsonl(run / "situation.jsonl")
    zero = next((r["zero_unix"] for r in sit_lines if r.get("event") == "zero"), None)
    if zero is None:
        raise ValueError(f"{run}: no zero in situation.jsonl")
    starts = json.loads((run / "proc-starts.json").read_text()).get("starts", {}) if (run / "proc-starts.json").exists() else {}
    audit = next(iter(sorted(run.glob("situation-*.jsonl"))), None)
    anchors = next(iter(sorted(run.glob("situation-*.anchors"))), None)
    offset = anchor_offset(anchors) if anchors else 0.0
    # the agent's lines on the situation's clock: their own `t` (wall), and every bridge time
    # (an evidence record's tick_t) mapped to UTC by the anchors
    lines = []
    for r in _jsonl(run / "agent.jsonl"):
        r = dict(r)
        r["wall_t"] = r.get("t")
        r["t"] = round(float(r["t"]) - zero, 3) if isinstance(r.get("t"), (int, float)) else None
        lines.append(r)
    contacts_start = starts.get("contacts")
    truth_lines = _jsonl(run / "contacts-truth.jsonl")
    samples = []
    meta: dict[str, dict[str, Any]] = {}
    for m in truth_lines:
        s = m.get("s")
        if s is None or contacts_start is None:
            continue
        t = round(contacts_start - zero + float(s), 3)
        cs = []
        for c in m.get("contacts", []):
            cs.append([c["id"], c["lon"], c["lat"], c.get("speed_mps"), True])
            meta.setdefault(c["id"], {"id": c["id"], "name": c.get("name"), "affiliation": c.get("affiliation"),
                                      "domain": c.get("domain"), "kind": c.get("kind"), "ais": bool(c.get("ais"))})
        samples.append({"t": t, "contacts": cs, "ours": []})
    decisions = []
    for r in sit_lines:
        if r.get("event") == "autoplay":
            decisions.append(r)
    orders = [r for r in sit_lines if r.get("event") == "order"]
    ready = next((r for r in sit_lines if r.get("event") == "ready"), None)
    prov = run / "provenance.json"
    return {
        "schema": "murmur-headless-episode/1",
        "side": "full stack",
        "run": run.name,
        "provenance": json.loads(prov.read_text()) if prov.exists() else None,
        "situation": {"name": "harbor-watch", "family": None, "split": None},
        "seed": int(re.search(r"seed-(\d+)$", run.name).group(1)) if re.search(r"seed-(\d+)$", run.name) else None,
        "clock": {"zero_unix": zero, "bridge_to_utc_s": round(offset, 3), "process_starts":
                  {k: round(v - zero, 2) for k, v in starts.items()},
                  "ready_t": round(ready["t"] - zero, 3) if ready and isinstance(ready.get("t"), (int, float)) else None},
        "orders": orders,
        "agent_log": lines,
        "operator_decisions": decisions,
        "truth": {"contacts": list(meta.values()), "samples": samples, "association": {}, "stated": {}},
        "audit_text": audit.read_text() if audit else "",
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--seeds", type=int, nargs="+", required=True)
    r.add_argument("--out", type=Path, required=True)
    c = sub.add_parser("record")
    c.add_argument("run", type=Path)
    args = ap.parse_args(argv)
    if args.cmd == "run":
        failed = []
        for s in args.seeds:
            try:
                run_one(s, args.out if args.out.is_absolute() else REPO / args.out)
            except SystemExit as e:  # one seed's failure is listed; the next seed still runs
                say(f"seed {s}: FAILED: {e}")
                failed.append(s)
        say(f"done; failed seeds: {failed or 'none'}")
        return 1 if failed else 0
    rec = record(args.run)
    rec.pop("audit_text", None)
    print(json.dumps(rec, indent=1)[:4000])
    return 0


if __name__ == "__main__":
    sys.exit(main())
