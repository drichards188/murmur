"""The blind labels' tooling (eval/headless/v2/blind.py; SCORING-V2.md 2.7, 0.5) on synthetic
batches: the stratified draw per agent and its filters, the pooled import of several packet
directories with their keys, the training-set rescore against an earlier report, and a batch
whose records are named relative to the repository's root. No model is called: answers are
written here."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from eval.headless.v2 import blind
from eval.headless.v2.record import dump

from .test_v2_labels import look_world, orbit


def record(agent: str) -> dict[str, Any]:
    """An unknown boat inbound to 1 km of the bridge; two orbits by PUMA 1 (A1 identifies it at
    130 s, A2 starts after)."""
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=100.0, end_state=(140.0, "superseded"))
    orbit(s, 11, 3, created=110.0, start=140.0)
    return s.record(agent)


def batch(root: Path, name: str, agents: dict[str, int], with_none: bool = True) -> Path:
    """A batch directory with `n` episodes of each agent (situations x:0 ... x:n-1) and, when
    asked, each situation's `none` replicate."""
    b = root / name
    b.mkdir(parents=True)
    metas = []
    n_sit = max(agents.values())
    for agent, n in list(agents.items()) + ([("none", n_sit)] if with_none else []):
        for k in range(n):
            p = b / f"{agent}-{k}.json"
            dump(record(agent), p)
            metas.append({"path": str(p), "agent": agent, "situation": f"x:{k}", "r": 0, "class": "scored",
                          "operator": "approve-all", "policy": "watch.toml", "base": "fast-unknown"})
    (b / "episodes_v2.json").write_text(json.dumps(metas))
    return b


def fill(packets: Path, helped: str = "yes", harmed: str = "no", iit: str = "yes") -> None:
    """Every packet's answers, as the labeller would leave them, and a reason file."""
    for d in packets.iterdir():
        if not (d / "answers.json").exists():
            continue
        ans = json.loads((d / "answers.json").read_text())
        for o in ans["orders"].values():
            o.update(helped=helped, harmed=harmed)
        for c in ans["contacts"].values():
            c.update(identified_in_time=iit)
        (d / "answers.json").write_text(json.dumps(ans))
        (d / "reasons.json").write_text(json.dumps({"orders": {a: {"helped": f"{a} because"} for a in ans["orders"]},
                                                    "contacts": {}}))


def test_the_stratified_draw_takes_n_of_each_agent_and_never_a_silent_one(tmp_path: Path):
    b = batch(tmp_path, "b", {"a": 3, "b": 5})
    out = blind.export(b, tmp_path / "p", sample=0, seed=4, key_path=tmp_path / "key.json",
                       per_agent=blind.parse_per_agent(["2", "b=4"]))
    assert out["by_agent"] == {"a": 2, "b": 4} and out["packets"] == 6
    key = json.loads((tmp_path / "key.json").read_text())
    assert key["per_agent"] == {"*": 2, "b": 4} and key["labels_version"] == "2"
    assert all(info["agent"] != "none" for info in key["packets"].values())
    # each agent from its own stream: b's count does not move a's draw
    again = blind.export(b, tmp_path / "p2", sample=0, seed=4, key_path=tmp_path / "key2.json",
                         per_agent=blind.parse_per_agent(["2", "b=1"]))
    pick = lambda k: sorted(i["path"] for i in json.loads(k.read_text())["packets"].values() if i["agent"] == "a")  # noqa: E731
    assert again["by_agent"] == {"a": 2, "b": 1} and pick(tmp_path / "key.json") == pick(tmp_path / "key2.json")
    # the agent filter, and a random sample as before
    only = blind.export(b, tmp_path / "p3", sample=2, seed=0, key_path=tmp_path / "key3.json", agents=["a"])
    assert only["by_agent"] == {"a": 2}
    assert blind.parse_per_agent(None) is None


def test_the_draw_can_be_stratified_on_the_harmful_label(tmp_path: Path):
    """`--harmful AGENT=N`: N of the agent's draws from its episodes with a harmful action (by the
    instruments), the rest from its others; each packet's stratum in the key and the report."""
    from .test_v2_labels import UNRESOLVED, Synth, at, inbound, rising

    def harming() -> dict[str, Any]:  # a pull-off far from a boat whose hits it was raising
        s = Synth(300)
        s.contact("boat", path=inbound())
        s.track("T-0001", "boat", ident=lambda t: UNRESOLVED, hits=rising("3:eo_ir", 100.0, 140.0))
        s.vehicle(3, lambda t: {})
        orbit(s, 10, 3, created=118.0, start=120.0, center=at(4000.0, 4000.0), radius=500.0)
        return s.record("pull")

    b = tmp_path / "b"
    b.mkdir()
    metas = []
    for k in range(6):
        p = b / f"pull-{k}.json"
        dump(harming() if k < 3 else record("pull"), p)
        metas.append({"path": str(p), "agent": "pull", "situation": f"x:{k}", "r": 0, "class": "scored",
                      "operator": "approve-all", "policy": "watch.toml", "base": "fast-unknown"})
        q = b / f"none-{k}.json"
        dump(record("none"), q)
        metas.append({**metas[-1], "path": str(q), "agent": "none"})
    (b / "episodes_v2.json").write_text(json.dumps(metas))
    assert blind.harmful_paths(b, metas, ["pull"]) == {str(b / f"pull-{k}.json") for k in range(3)}
    out = blind.export(b, tmp_path / "p", sample=0, seed=1, key_path=tmp_path / "k.json",
                       per_agent={"*": 3}, harmful={"pull": 2})
    key = json.loads((tmp_path / "k.json").read_text())
    strata = sorted(i["stratum"] for i in key["packets"].values())
    assert out["packets"] == 3 and strata == ["harmful", "harmful", "other"] and key["harmful"] == {"pull": 2}
    assert all((i["stratum"] == "harmful") == (int(i["path"][-6]) < 3) for i in key["packets"].values())
    fill(tmp_path / "p", helped="no", harmed="yes")
    rep = blind.import_answers(tmp_path / "p", tmp_path / "k.json")["label_types"]["harmed"]
    assert rep["informative"] and set(rep["by_stratum"]) == {"pull:harmful", "pull:other"}
    assert rep["counts"]["instrument"]["yes"] == 2


def test_import_pools_packet_directories_each_with_its_key(tmp_path: Path):
    b1 = batch(tmp_path, "b1", {"model": 2})
    b2 = batch(tmp_path, "b2", {"oracle-honest+pull-off": 2})
    blind.export(b1, tmp_path / "p1", sample=2, seed=0, key_path=tmp_path / "k1.json")
    blind.export(b2, tmp_path / "p2", sample=2, seed=0, key_path=tmp_path / "k2.json")
    fill(tmp_path / "p1")
    fill(tmp_path / "p2", helped="no")
    one = blind.import_answers(tmp_path / "p1", tmp_path / "k1.json")
    both = blind.import_answers([tmp_path / "p1", tmp_path / "p2"], [tmp_path / "k1.json", tmp_path / "k2.json"])
    assert one["label_types"]["helped"]["n"] == 4 and both["label_types"]["helped"]["n"] == 8
    assert [s["answered"] for s in both["sources"]] == [2, 2] and both["labels_version"] == "2"
    h = both["label_types"]["helped"]
    # A1 identified the boat (useful), A2 came after it was identified (redundant): one yes, one no
    assert h["counts"]["instrument"] == {"no": 4, "yes": 4} and h["informative"]
    assert set(h["by_agent"]) == {"model", "oracle-honest+pull-off"}
    dis = h["disagreements"]
    assert len(dis) == 4 and all(d["agent"] and d["situation"] and d["label"] for d in dis)
    assert {d["reason"] for d in dis} == {"A1 because", "A2 because"}
    assert not both["label_types"]["harmed"]["informative"], "every answer no: cannot test agreement"
    try:
        blind.import_answers([tmp_path / "p1", tmp_path / "p2"], [tmp_path / "k1.json"])
        raise AssertionError("one key per directory")
    except ValueError:
        pass


def test_rescore_lists_what_changed_and_gates_the_spend(tmp_path: Path):
    b = batch(tmp_path, "b", {"playbook": 2})
    blind.export(b, tmp_path / "p", sample=2, seed=0, key_path=tmp_path / "k.json")
    fill(tmp_path / "p", helped="yes")
    first = blind.import_answers(tmp_path / "p", tmp_path / "k.json")
    # as if an earlier label set had disagreed on A1 (now resolved) and agreed on A2 (now not)
    a2 = [d for d in first["label_types"]["helped"]["disagreements"]]
    before = {"label_types": {"helped": {"pi": 0.5, "disagreements": [{"packet": d["packet"], "item": "A1"} for d in a2]}}}
    (tmp_path / "before.json").write_text(json.dumps(before))
    res = blind.rescore(tmp_path / "p", tmp_path / "k.json", tmp_path / "before.json")
    assert res["check"].startswith("training set")
    ch = res["changed"]["helped"]
    assert len(ch["resolved"]) == 2 and {d["item"] for d in ch["new"]} == {"A2"} and ch["pi_before"] == 0.5
    assert res["gate"].startswith("stop"), "helped's pi under 0.8: nothing is spent"
    fill(tmp_path / "p", helped="no")
    for d in (tmp_path / "p").iterdir():
        if (d / "answers.json").exists():
            ans = json.loads((d / "answers.json").read_text())
            ans["orders"]["A1"]["helped"] = "yes"
            (d / "answers.json").write_text(json.dumps(ans))
    assert blind.rescore(tmp_path / "p", tmp_path / "k.json")["gate"].startswith("go")


def test_records_named_from_the_repository_root_are_found(tmp_path: Path, monkeypatch: Any):
    """A batch written from the repository's root names its records relative to it; export and
    import find them from anywhere."""
    root = tmp_path / "repo"
    b = batch(root / "eval" / "results", "b", {"playbook": 2})
    metas = json.loads((b / "episodes_v2.json").read_text())
    for m in metas:
        m["path"] = str(Path(m["path"]).relative_to(root))
    (b / "episodes_v2.json").write_text(json.dumps(metas))
    monkeypatch.chdir(tmp_path)
    assert blind.resolve(metas[0]["path"], b) == str(root / metas[0]["path"])
    blind.export(b, tmp_path / "p", sample=2, seed=0, key_path=tmp_path / "k.json")
    fill(tmp_path / "p")
    got = blind.import_answers(tmp_path / "p", tmp_path / "k.json")
    assert got["sources"][0]["answered"] == 2 and got["label_types"]["identified_in_time"]["agreement"] == 1.0
