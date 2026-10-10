"""Whole episodes on the real `murmur-headless` binary (built here if stale: target/release).

- the golden: Harbor watch, seed 1, byte-identical to the committed digest (the record without
  its `perf` timings, the change feed's per-process nonce masked; the audit log itself, but for
  the commit the binary was built from and the hashes that carry it), and a
  second run identical to the first in everything but the feed id (each run's one feed id put in
  place of the other's, the whole record compared); seed 2 differs;
- the audit log replays exactly with its anchors (`murmur-replay --anchors`);
- the view identity: on one state, every tool the playbook calls answers the same bytes through
  the bridge's own `/mcp` endpoint (rmcp over HTTP, the same `Mcp`) as through the direct call
  the session adapter uses; the writers on two identical episodes, one per path;
- the reads' schema: every key path of the recorded live reads
  (agents/tests/fixtures/playbook/harbor-watch-reads.json) is in the headless reads, with the same
  JSON types, and the headless reads have no key the live ones lack.
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import pytest

from eval.headless import REPO
from eval.headless.episode import Episode, Options
from eval.headless.sim import BINARY, HeadlessSession, Sim
from eval.headless.situations import Calibration, from_situation, process_spec

GOLDEN = Path(__file__).with_name("golden-harbor-watch-s1.json")
FIXTURE = REPO / "agents" / "tests" / "fixtures" / "playbook" / "harbor-watch-reads.json"


@pytest.fixture(scope="module", autouse=True)
def binary() -> Path:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-decide", "--bin", "murmur-replay"], cwd=REPO, check=True)
    return BINARY


FEED = re.compile(r'"cursor": "[0-9a-f]+:')
# the commit the binary was built from (`run.started`'s `build`, murmur-decide/build.rs) and the
# hash chain that carries it into every later line's `prev`: the golden holds the episode, not
# the build; the chain itself is checked by the replay below
BUILD = re.compile(r'"build":\{[^}]*\}')
PREV = re.compile(r'"prev":"[0-9a-f]{64}"')


def audit_without_build(text: str) -> str:
    return PREV.sub('"prev":"<chain>"', BUILD.sub('"build":<build>', text))


def normalized(rec: dict[str, Any]) -> str:
    """The record as the golden holds it: no `perf` (computer time), the change feed's nonce (a
    fresh value per bridge process by design, `mcp/changes.rs`; it is only ever compared for
    equality) masked, the audit log without the commit it was built from. Everything else must be
    byte-identical."""
    r = {k: v for k, v in rec.items() if k not in ("perf", "provenance")}
    if "audit_text" in r:
        r["audit_text"] = audit_without_build(r["audit_text"])
    if isinstance(r.get("audit"), dict):
        # the log's head and hash are of the log with its build; the digest hashes it without
        r["audit"] = {k: v for k, v in r["audit"].items() if k not in ("head", "sha256")}
    return FEED.sub('"cursor": "<feed>:', json.dumps(r, sort_keys=True))


FEED_ID = re.compile(r'"cursor": "([0-9a-f]+):')


def only_the_feed_id_differs(a: dict[str, Any], b: dict[str, Any]) -> None:
    """Two runs of one seed: the whole record (provenance and the audit log included; only `perf`,
    the computer's own timings, left out) is the same once the one feed id of each is put in
    place of the other, everywhere it occurs, not only in cursors. Each run has exactly one feed
    id (one bridge process), and the two differ: the test fails if the mask hid nothing."""
    ta = json.dumps({k: v for k, v in a.items() if k != "perf"}, sort_keys=True)
    tb = json.dumps({k: v for k, v in b.items() if k != "perf"}, sort_keys=True)
    ids_a, ids_b = set(FEED_ID.findall(ta)), set(FEED_ID.findall(tb))
    assert len(ids_a) == 1 and len(ids_b) == 1, (ids_a, ids_b)
    (fa,), (fb,) = ids_a, ids_b
    assert fa != fb, "both runs had the same feed id: the check would show nothing"
    tb = tb.replace(fb, fa)
    if ta != tb:
        i = next(i for i, (x, y) in enumerate(zip(ta, tb)) if x != y) if len(ta) == len(tb) else min(len(ta), len(tb))
        pytest.fail(f"two runs of one seed differ beyond the feed id at {i}: {ta[max(0, i - 200):i + 200]!r} vs "
                    f"{tb[max(0, i - 200):i + 200]!r}")


def digest(rec: dict[str, Any]) -> dict[str, Any]:
    from eval.headless.metrics import episode_metrics

    m = episode_metrics(rec)
    return {
        "record_sha256": hashlib.sha256(normalized(rec).encode()).hexdigest(),
        "audit_sha256": hashlib.sha256(audit_without_build(rec["audit_text"]).encode()).hexdigest(),
        "audit_events": rec["audit"]["events"],
        "calibration": rec["clock"]["calibration"],
        "proposals": [[p["t"], p["play"], p["target"], p["truth_target"], p["uid"], p["state"]] for p in m["proposals"]],
        "operator": [[d["t"], d["intent_id"], d["action"], d["result"]] for d in rec["operator_decisions"]],
    }


def run(seed: int, keep: Path | None = None) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="golden-") as td:
        return Episode(from_situation("harbor-watch"), seed, Path(td), Options(calibration=Calibration.load(), keep=keep)).run()


def test_the_golden_episode_is_byte_identical(tmp_path: Path):
    a = run(1, keep=tmp_path / "a")
    b = run(1)
    assert normalized(a) == normalized(b), "the same seed gave two different episodes"
    only_the_feed_id_differs(a, b)
    got = digest(a)
    if not GOLDEN.exists():
        GOLDEN.write_text(json.dumps(got, indent=1) + "\n")
        pytest.fail(f"no golden yet: wrote {GOLDEN}; commit it")
    want = json.loads(GOLDEN.read_text())
    if got != want:
        out = tmp_path / "got.json"
        out.write_text(json.dumps(got, indent=1))
        pytest.fail(f"the golden episode changed: {out} (proposals {got['proposals']} vs {want['proposals']})")
    c = run(2)
    assert normalized(c) != normalized(a), "another seed gave the same episode"
    # the audit log replays exactly, with its anchors
    log = next((tmp_path / "a").glob("headless-*.jsonl"))
    anchors = log.with_suffix(".anchors")
    rep = subprocess.run([str(REPO / "target" / "release" / "murmur-replay"), str(log), "--anchors", str(anchors)],
                         capture_output=True, text=True)
    assert rep.returncode == 0, rep.stdout + rep.stderr
    assert "every decision reproduced exactly" in rep.stdout + rep.stderr


def run_family(family: str, seed: int) -> dict[str, Any]:
    from eval.headless.families import generate

    with tempfile.TemporaryDirectory(prefix="family-") as td:
        spec, contacts = generate(family, seed, Path(td))
        return Episode(spec, seed, Path(td), Options(calibration=Calibration.load()), contacts_path=contacts).run()


@pytest.mark.parametrize("family", ["link-cut-mid-look", "approval-flood"])
def test_a_family_episode_differs_only_in_the_feed_id(family: str):
    """The golden's check on the paths Harbor watch does not take: a link cut timed from a
    dispatch, a scripted proposer's flood."""
    a, b = run_family(family, 1), run_family(family, 1)
    assert a["script"], "the family's fault or proposer is in the episode"
    only_the_feed_id_differs(a, b)


def _sim(td: Path) -> Sim:
    e = from_situation("harbor-watch")
    spec, env = process_spec(e, 1, td, Calibration.load())
    return Sim(spec, env, log=td / "sim.log")


def _same(a: dict, b: dict) -> bool:
    return json.dumps(a, sort_keys=True) == json.dumps(b, sort_keys=True)


def test_the_views_are_the_bridge_mcp_endpoints():
    """Read tools on one state, both paths; the opening orders give intents to read."""
    with tempfile.TemporaryDirectory() as td:
        sim = _sim(Path(td))
        try:
            sim.step(200.0)
            client = ("agent-playbook", "test")
            reads = [("get_fleet", {"compact": True}), ("get_fleet", {"uids": [3]}), ("get_fleet", {}),
                     ("get_tracks", {}), ("get_inbox", {}), ("get_intents", {"mine": True, "limit": 50}),
                     ("get_intents", {}), ("get_policy", {}), ("get_platforms", {}), ("get_areas", {}),
                     ("get_nominations", {}), ("explain_intent", {"id": 1}), ("explain_intent", {"id": 999}),
                     ("get_vehicle", {"uid": 2}), ("get_recent_events", {"limit": 20}),
                     ("preview_intent", {"task": "orbit", "uids": [3], "center": [-122.5, 37.81], "radius_m": 1000.0}),
                     ("preview_intent", {"task": "hold", "uids": [3]}), ("no_such_tool", {})]
            differ = []
            for tool, args in reads:
                d = sim.mcp(client[0], client, tool, args)
                h = sim.rpc({"op": "mcp_http", "agent": client[0], "client": list(client), "tool": tool, "args": args})
                h.pop("ok", None)
                d.pop("ok", None)
                if not _same(d, h):
                    differ.append((tool, args, json.dumps(d)[:300], json.dumps(h)[:300]))
            # the change feed: each path reads its own cursor; the same answer for the same cursor
            first = sim.mcp(client[0], client, "get_changes", {})["structured"]
            cur = first["cursor"]
            d = sim.mcp(client[0], client, "get_changes", {"since": cur})
            h = sim.rpc({"op": "mcp_http", "agent": client[0], "client": list(client), "tool": "get_changes",
                         "args": {"since": cur}})
            for x in (d, h):
                x.pop("ok", None)
            if not _same(d, h):
                differ.append(("get_changes", cur, json.dumps(d)[:300], json.dumps(h)[:300]))
            assert not differ, differ
        finally:
            sim.end()


def test_two_processes_serve_the_same_views():
    """The determinism audit's empirical half: a map iterated into a view (std's hasher is seeded
    per process) would make two processes on the same seed answer differently."""
    tools = [("get_fleet", {}), ("get_fleet", {"compact": True}), ("get_tracks", {}), ("get_inbox", {}),
             ("get_intents", {}), ("get_policy", {}), ("get_platforms", {}), ("get_areas", {}),
             ("get_recent_events", {"limit": 100}), ("get_nominations", {})]
    answers = []
    for _ in range(2):
        with tempfile.TemporaryDirectory() as td:
            sim = _sim(Path(td))
            got = []
            try:
                for t in (60.0, 190.0, 330.0):
                    sim.step(t)
                    for tool, args in tools:
                        text = json.dumps(sim.mcp("agent-playbook", ("a", "b"), tool, args), sort_keys=False)
                        # the episode's own directory (its audit log's path) differs by design
                        got.append(FEED.sub('"cursor": "<feed>:', text.replace(td, "<workdir>")))
                    _, links = sim.api("GET", "/api/links")
                    got.append(json.dumps({k: v for k, v in links.items() if k not in ("summary", "age_s")}))
            finally:
                sim.end()
            answers.append(got)
    diffs = [(i, a[:200], b[:200]) for i, (a, b) in enumerate(zip(*answers)) if a != b]
    assert not diffs, diffs


def test_the_writers_answer_the_same_on_both_paths():
    def episode(path: str) -> list[dict]:
        with tempfile.TemporaryDirectory() as td:
            sim = _sim(Path(td))
            out = []
            try:
                sim.step(120.0)
                client = ("agent-playbook", "test")

                def call(tool: str, args: dict) -> dict:
                    if path == "direct":
                        r = sim.mcp(client[0], client, tool, args)
                    else:
                        r = sim.rpc({"op": "mcp_http", "agent": client[0], "client": list(client), "tool": tool, "args": args})
                    r.pop("ok", None)
                    return r

                orbit = {"task": "orbit", "uids": [3], "center": [-122.5, 37.81], "radius_m": 1000.0, "rationale": "test"}
                out.append(call("propose_intent", orbit))
                iid = out[-1]["structured"]["id"]
                out.append(call("withdraw_intent", {"id": iid, "reason": "test", "if_state": "approved"}))
                out.append(call("withdraw_intent", {"id": iid, "reason": "test", "if_state": "pending_approval"}))
                out.append(call("propose_intent", {**orbit, "uids": [99]}))
                out.append(call("propose_intent", {"task": "orbit", "uids": [3]}))
            finally:
                sim.end()
            return out

    a, b = episode("direct"), episode("http")
    assert [json.dumps(x, sort_keys=True) for x in a] == [json.dumps(x, sort_keys=True) for x in b]


def _paths(x: Any, p: str = "") -> dict[str, set[str]]:
    out: dict[str, set[str]] = {}

    def add(path: str, typ: str) -> None:
        out.setdefault(path, set()).add(typ)

    def walk(v: Any, path: str) -> None:
        if isinstance(v, dict):
            add(path, "object")
            for k, w in v.items():
                walk(w, f"{path}.{k}")
        elif isinstance(v, list):
            add(path, "array")
            for w in v:
                walk(w, f"{path}[]")
        elif v is None:
            add(path, "null")
        elif isinstance(v, bool):
            add(path, "bool")
        elif isinstance(v, (int, float)):
            add(path, "number")
        else:
            add(path, "string")

    walk(x, p)
    return out


def _merge(a: dict[str, set[str]], b: dict[str, set[str]]) -> None:
    for k, v in b.items():
        a.setdefault(k, set()).update(v)


def test_the_reads_have_the_live_reads_schema(monkeypatch):
    """Every key path (and its JSON type) of the recorded live reads is in what the playbook
    reads headless over a whole Harbor watch episode. The other way is reported, not asserted:
    the fixture predates fields the bridge has since gained, and its recorder kept no notes."""
    live = json.loads(FIXTURE.read_text())
    want: dict[str, dict[str, set[str]]] = {}
    for tick in live["ticks"]:
        for tool in ("get_fleet", "get_tracks", "get_inbox"):
            _merge(want.setdefault(tool, {}), _paths(tick[tool]))
    for tool in ("get_platforms", "get_policy"):
        want[tool] = _paths(live[tool])
    got: dict[str, dict[str, set[str]]] = {}
    real = HeadlessSession.call

    async def recording(self, name, **args):
        out = await real(self, name, **args)
        if name in want and (name != "get_fleet" or args.get("compact")):
            _merge(got.setdefault(name, {}), _paths(out))
        return out

    monkeypatch.setattr(HeadlessSession, "call", recording)
    run(1)
    missing, extra = [], []
    for tool, w in want.items():
        g = got.get(tool, {})
        for path in sorted(w):
            if path not in g:
                missing.append(f"{tool}{path}")
            else:
                tw, tg = w[path] - {"null"}, g[path] - {"null"}
                if tw and tg and not tw <= tg:
                    missing.append(f"{tool}{path}: {sorted(tw)} live, {sorted(tg)} headless")
        extra += [f"{tool}{p}" for p in sorted(set(g) - set(w)) if not p.endswith(".note")]
    print("in the headless reads, not the fixture's (newer fields):", extra)
    assert not missing, missing
