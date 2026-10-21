"""Scoring v2 on the real `murmur-headless` binary (built here if stale: target/release): the
Python port of the misclassification draw against the bridge's own function; the picture
sampler changes nothing in the episode it samples, repeats exactly, and leaves a v1 record
alone; a v2 smoke batch with `none` and `playbook` on the v1 family `fast-unknown` gives labels,
an aggregate and a scorecard; the blind packets carry no agent, family, seed or scorer field and
come back in agreement when the answers are the instruments' own."""

from __future__ import annotations

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
from eval.headless.sim import BINARY, Sim
from eval.headless.situations import Calibration, from_situation, process_spec
from eval.headless.v2 import labels as L


@pytest.fixture(scope="module", autouse=True)
def binary() -> Path:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run([cargo, "build", "--release", "-q", "-p", "murmur-headless"], cwd=REPO, check=True)
    return BINARY


def test_the_misclassification_draw_is_the_bridges():
    """labels.py's port of sensing.rs `misclassify_draw` gives the same uniform, bit for bit,
    and the same draw, for several seeds, contacts and rates."""
    contacts = ["fast-1", "fast-2", "decoy", "ferry-larkspur", "cargo-inbound", "x", "", "a-much-longer-contact-id-ü"]
    seeds = [0, 1, 7, 11, 1567179054, 2**31 - 1, 2**63 + 5]
    with tempfile.TemporaryDirectory() as td:
        e = from_situation("harbor-watch")
        spec, env = process_spec(e, 1, Path(td), Calibration.load())
        sim = Sim(spec, env, log=Path(td) / "sim.log")
        try:
            n = 0
            for seed in seeds:
                for rate in (0.0, 0.02, 0.5, 1.0):
                    for d in sim.misclassify(seed, rate, contacts):
                        assert L.misclassify_u(seed, d["contact"], d["sensor"]) == d["u"], (seed, d)
                        assert L.misclassify_draw(seed, d["contact"], d["sensor"], rate) == d["wrong"], (seed, rate, d)
                        n += 1
            assert n == len(seeds) * 4 * len(contacts) * 5
        finally:
            sim.end()


FEED = re.compile(r'"cursor": ?"[0-9a-f]+:')
CHAIN = re.compile(r'"(prev|hash)":"[0-9a-f]{64}"')
BUILD = re.compile(r'"build":\{[^}]*\}')


def _audit(rec: dict[str, Any]) -> str:
    return BUILD.sub("", CHAIN.sub("", FEED.sub("", rec["audit_text"])))


def _episode(scoring: str) -> dict[str, Any]:
    from eval.headless.families import generate

    with tempfile.TemporaryDirectory(prefix="v2-") as td:
        spec, contacts = generate("fast-unknown", 1000, Path(td))
        return Episode(spec, 1000, Path(td), Options(calibration=Calibration.load(), scoring=scoring),
                       contacts_path=contacts).run()


def test_the_picture_sampler_changes_nothing_and_repeats():
    v1, v2, again = _episode("v1"), _episode("v2"), _episode("v2")
    assert _audit(v1) == _audit(v2), "the picture sampler changed the episode's audit log"
    strip = lambda r: FEED.sub("", json.dumps(r["agent_log"], sort_keys=True))  # noqa: E731
    assert strip(v1) == strip(v2)
    assert v1["operator_decisions"] == v2["operator_decisions"] and v1["truth"]["samples"] == v2["truth"]["samples"]
    for k in ("v2", "episode_class", "scoring", "replicate"):
        assert k not in v1, f"a v1 record gained {k}"
    assert v2["episode_class"] == "scored" and v2["scoring"] == "v2"
    pic = v2["v2"]["picture"]
    assert len(pic["times"]) == 721 and pic["times"][:3] == [0.0, 1.0, 2.0]
    assert pic["fleet"] and pic["sensing"].get("seed") is not None
    assert json.dumps(pic, sort_keys=True) == json.dumps(again["v2"]["picture"], sort_keys=True)
    assert v2["v2"]["invariants"]["total"] == 0
    lab = L.episode_labels(v2, none=None)
    assert [n["contact"] for n in lab["needs"]] == ["fast-1"]
    assert lab["actions"], "the playbook acted"


@pytest.fixture(scope="module")
def smoke(tmp_path_factory: pytest.TempPathFactory) -> Path:
    from eval.headless import batch

    out = tmp_path_factory.mktemp("v2-smoke")
    code = batch.main(["--scoring", "v2", "--families", "fast-unknown", "--seeds", "1000", "--replicates", "2",
                       "--agent", "playbook", "--operators", "approve-all", "--workers", "4", "--no-build",
                       "--out", str(out)])
    assert code == 0
    return out


def test_a_v2_smoke_batch_gives_labels_an_aggregate_and_a_scorecard(smoke: Path):
    from eval.headless.v2.aggregate import score
    from eval.headless.v2.record import load

    metas = json.loads((smoke / "episodes_v2.json").read_text())
    assert sorted((m["agent"], m["r"]) for m in metas) == [("none", 0), ("none", 1), ("playbook", 0), ("playbook", 1)]
    assert all(m["class"] == "scored" and Path(m["path"]).name.endswith(".json.gz") for m in metas)
    seeds = {m["r"]: m["seed"] for m in metas}
    from eval.headless.batch import replicate_seed

    assert seeds == {r: replicate_seed("fast-unknown", 1000, r) for r in (0, 1)}
    rec = load(next(m["path"] for m in metas if m["agent"] == "playbook"))
    assert rec["replicate"]["situation_seed"] == 1000 and rec["seed"] == seeds[rec["replicate"]["r"]]
    assert rec["operator"]["mode"] == "approve-all"
    scored = score(metas, multiverse=False, keep_labels=True)
    pb = [e for e in scored if e["agent"] == "playbook"]
    assert all(e["paired"] for e in pb) and all(e["labels"]["actions"] for e in pb)
    for e in pb:
        for a in e["labels"]["actions"]:
            assert a["label"] in L.ACTION_LABELS and a["instrument"]["name"] == "v2.action_label"
    agg = json.loads((smoke / "aggregate_v2.json").read_text())
    block = agg["families"]["fast-unknown"]["approve-all;watch.toml;paused"]
    assert set(block["arms"]) == {"none", "playbook"} and "playbook - none" in block["comparisons"]
    assert block["arms"]["playbook"]["metrics"]["served_share"]["n_situations"] == 1
    assert agg["multiverse"] and agg["none_added_for_pairing"]
    card = json.loads((smoke / "scorecard.json").read_text())
    row = card["conditions"]["approve-all;watch.toml;paused"]["rows"][0]
    assert row["area"] == "S1" and row["metrics"]["served_share"]["playbook"]["mark"] == "descriptive, underpowered"
    md = (smoke / "scorecard.md").read_text()
    assert "S1 identify" in md and "†" in md


def test_blind_packets_are_blind_and_import_agrees(smoke: Path, tmp_path: Path):
    from eval.headless.v2 import blind

    out = tmp_path / "packets"
    res = blind.export(smoke, out, sample=2, seed=1)
    assert res["packets"] == 2 and Path(res["key"]).parent == smoke
    for f in out.glob("*/*.json"):
        text = f.read_text()
        for word in ("playbook", "agent-", "fast-unknown", "t_need", "pre_served", "eligible", "contributed",
                     "rationale", "useful", "s1000", "harbor-watch"):
            assert word not in text, (f.name, word)
    # answered as the instruments answer: full agreement
    key = json.loads(Path(res["key"]).read_text())
    from eval.headless.v2.aggregate import score

    metas = json.loads((smoke / "episodes_v2.json").read_text())
    labs = {e["path"]: e["labels"] for e in score(metas, multiverse=False, keep_labels=True) if e.get("labels")}
    for pid, info in key["packets"].items():
        acts, needs = blind.instrument_answers(labs[info["path"]])
        f = out / pid / "answers.json"
        ans = json.loads(f.read_text())
        for aid, intent in info["orders"].items():
            ans["orders"][aid] = {"helped": acts[intent]["helped"], "harmed": acts[intent]["harmed"]}
        for c in ans["contacts"]:
            ans["contacts"][c]["identified_in_time"] = needs[c]
        f.write_text(json.dumps(ans))
    got = blind.import_answers(out, Path(res["key"]))
    for q in ("helped", "harmed", "identified_in_time"):
        assert got["label_types"][q]["agreement"] == 1.0 and got["label_types"][q]["disagreements"] == []
