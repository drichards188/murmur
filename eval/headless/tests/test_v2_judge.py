"""The blind labeller (eval/headless/v2/judge.py) with a fake client, no network: a packet made by
blind.py's export from a synthetic record answered end to end into answers.json and reasons.json
(a paused turn resumed on its container, the upload deleted, the cost settled in the ledger, the
answers read back by blind.py's import); a malformed answer put right in one more turn, or given
up on; the cost guard stopping before the cap; a second run skipping what is answered; the
leakage scan over the brief and every message the driver writes; the dry run calling nothing."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable

import pytest

from eval.headless.officer import _plain
from eval.headless.v2 import blind
from eval.headless.v2 import judge as J
from eval.headless.v2.record import dump

from .test_v2_labels import look_world, orbit


@pytest.fixture
def packets(tmp_path: Path) -> Path:
    """Two packets from blind.py's export: each an unknown boat inbound to 1 km of the bridge and
    two orbits by one vehicle (orders A1 and A2, contact `boat`)."""
    s = look_world()
    orbit(s, 10, 3, created=95.0, start=100.0, end_state=(140.0, "superseded"))
    orbit(s, 11, 3, created=110.0, start=140.0)
    rec = s.record("playbook")
    batch = tmp_path / "batch"
    batch.mkdir()
    metas = []
    for k in range(2):
        p = batch / f"ep{k}.json"
        dump(rec, p)
        metas.append({"path": str(p), "agent": "playbook", "situation": f"x:{k}", "r": 0, "class": "scored",
                      "operator": "approve-all", "policy": "watch.toml"})
    (batch / "episodes_v2.json").write_text(json.dumps(metas))
    out = tmp_path / "packets"
    res = blind.export(batch, out, sample=2, seed=0, key_path=tmp_path / "blind-key.json")
    assert res["packets"] == 2
    return out


def ids(packets: Path) -> list[str]:
    return sorted(d.name for d in packets.iterdir() if d.is_dir())


# ---- the fake client ---------------------------------------------------------------------------


def text(s: str) -> SimpleNamespace:
    return SimpleNamespace(type="text", text=s)


def code(n: int, stdout: str = "fast boat: 1000 m at t=200") -> list[SimpleNamespace]:
    return [SimpleNamespace(type="server_tool_use", id=f"srvtoolu_{n}", name="bash_code_execution",
                            input={"command": "python3 -c 'import json; json.load(open(\"packet.json\"))'"}),
            SimpleNamespace(type="bash_code_execution_tool_result", tool_use_id=f"srvtoolu_{n}",
                            content=SimpleNamespace(type="bash_code_execution_result", stdout=stdout, stderr="",
                                                    return_code=0))]


def answer(helped: Any = "yes", harmed: Any = "no", iit: Any = "yes", reasons: bool = True,
           orders: tuple[str, ...] = ("A1", "A2")) -> str:
    o = {}
    for a in orders:
        o[a] = {"helped": helped, "harmed": harmed}
        if reasons:
            o[a].update({"reason_helped": f"{a}'s vehicle held the boat at 105 s.", "reason_harmed": "Nothing worse."})
    c = {"boat": {"identified_in_time": iit, **({"reason": "Shown at 0.9 by EO/IR at 130 s."} if reasons else {})}}
    return "Here is what I found.\n```json\n" + json.dumps({"orders": o, "contacts": c}) + "\n```"


class FakeStream:
    def __init__(self, resp: Any):
        self.resp = resp

    def __enter__(self) -> "FakeStream":
        return self

    def __exit__(self, *a: Any) -> bool:
        return False

    def get_final_message(self) -> Any:
        return self.resp


class FakeClient:
    """`script(request, n)` gives (content blocks, stop reason) for the n-th request (from 1).
    Usage: a quarter of the request's characters as input, `out_tokens` output."""

    def __init__(self, script: Callable[[dict[str, Any], int], tuple[list[Any], str]], out_tokens: int = 500):
        self.script = script
        self.out_tokens = out_tokens
        self.requests: list[dict[str, Any]] = []
        self.uploads: list[dict[str, Any]] = []
        self.deleted: list[str] = []
        self.files = SimpleNamespace(upload=self._upload, delete=self._delete)
        self.messages = SimpleNamespace(stream=self._stream)
        self.beta = SimpleNamespace(messages=SimpleNamespace(stream=self._beta_stream))

    def _upload(self, file: Any) -> SimpleNamespace:
        name, data, ctype = file
        self.uploads.append({"name": name, "bytes": len(data), "type": ctype, "data": data})
        return SimpleNamespace(id=f"file_{len(self.uploads)}")

    def _delete(self, file_id: str) -> None:
        self.deleted.append(file_id)

    def _stream(self, **kw: Any) -> FakeStream:
        req = json.loads(json.dumps(kw, default=_plain))  # a snapshot: the driver appends to its lists later
        self.requests.append(req)
        blocks, stop = self.script(req, len(self.requests))
        size = len(json.dumps(req))
        usage = SimpleNamespace(input_tokens=size // 4, output_tokens=self.out_tokens, cache_creation_input_tokens=0,
                                cache_read_input_tokens=0)
        return FakeStream(SimpleNamespace(content=blocks, stop_reason=stop, model=kw["model"], usage=usage,
                                          container=SimpleNamespace(id="container_1"), stop_details=None))

    def _beta_stream(self, betas: list[str], fallbacks: Any, **kw: Any) -> FakeStream:
        kw["_betas"] = betas
        return self._stream(**kw)


def user_texts(req: dict[str, Any]) -> list[str]:
    out = []
    for m in req["messages"]:
        if m["role"] != "user":
            continue
        if isinstance(m["content"], str):
            out.append(m["content"])
        else:
            out += [b["text"] for b in m["content"] if b.get("type") == "text"]
    return out


def run(packets: Path, client: Any, *args: str) -> int:
    return J.main([str(packets), "--max-usd", "20", *args], client=client)


# ---- end to end ----------------------------------------------------------------------------------


def test_a_packet_is_answered_end_to_end_and_import_reads_it(packets: Path, tmp_path: Path):
    def script(req: dict[str, Any], n: int) -> tuple[list[Any], str]:
        if n == 1:
            return code(1), "pause_turn"  # the server's loop paused mid-work
        return [*code(2), text(answer())], "end_turn"

    fake = FakeClient(script)
    first = J.plan(packets, 0)[0][0].name
    assert run(packets, fake, "--limit", "1") == 0
    answered = [p for p in ids(packets) if (packets / p / "reasons.json").exists()]
    assert answered == [first], "the first of the seeded order"
    pid = answered[0]
    d = packets / pid
    ans = json.loads((d / "answers.json").read_text())
    assert ans["orders"] == {"A1": {"helped": "yes", "harmed": "no"}, "A2": {"helped": "yes", "harmed": "no"}}
    assert ans["contacts"] == {"boat": {"identified_in_time": "yes"}}
    assert ans["questions"]["helped"].startswith("Did this order help") and ans["answered_by"]["model"] == J.DEFAULT_MODEL
    reasons = json.loads((d / "reasons.json").read_text())
    assert reasons["orders"]["A1"]["helped"].startswith("A1's vehicle") and reasons["contacts"]["boat"]["identified_in_time"]
    assert json.loads((d / J.MARK).read_text())["status"] == "answered"
    assert (d / "judge-transcript.json").exists()

    # the requests: the file attached for the container, the code tool, streamed with adaptive
    # thinking at the effort given; the paused turn sent back as it stands on the same container
    r1, r2 = fake.requests
    assert fake.uploads[0]["name"] == "packet.json" and fake.uploads[0]["data"] == (d / "packet.json").read_bytes()
    assert r1["tools"] == [{"type": "code_execution_20260521", "name": "code_execution"}]
    assert r1["thinking"] == {"type": "adaptive"} and r1["output_config"] == {"effort": "high"}
    assert r1["model"] == "claude-opus-5-5" and "container" not in r1
    up = [b for b in r1["messages"][0]["content"] if b["type"] == "container_upload"]
    assert up == [{"type": "container_upload", "file_id": "file_1"}]
    assert r1["system"][0]["text"] == J.brief(blind.mission_text())
    assert r2["container"] == "container_1" and [m["role"] for m in r2["messages"]] == ["user", "assistant"]
    assert [b["type"] for b in r2["messages"][1]["content"]] == ["server_tool_use", "bash_code_execution_tool_result"]
    assert fake.deleted == ["file_1"]

    # the cost: settled in the ledger, nothing left reserved; the run log has the packet's calls
    led = json.loads((packets / J.LEDGER).read_text())
    assert led["reserved"] == 0.0 and 0 < led["usd"] < 1 and led["max_usd"] == 20.0
    rows = [json.loads(x) for x in (packets / J.LOG).read_text().splitlines()]
    assert len(rows) == 1 and rows[0]["status"] == "answered" and rows[0]["calls"] == 2
    assert rows[0]["stop_reasons"] == ["pause_turn", "end_turn"] and rows[0]["file_deleted"] is True
    assert rows[0]["tokens"]["output"] == 1000 and rows[0]["usd"] == pytest.approx(led["usd"], abs=1e-5)
    assert all(c["usd"] <= c["reserved_usd"] for c in rows[0]["per_call"])

    # blind.py's import reads what was written (a packet answered, the other not yet)
    got = blind.import_answers(packets, tmp_path / "blind-key.json")
    assert set(got["label_types"]) >= {"helped", "harmed", "identified_in_time"}
    assert got["label_types"]["identified_in_time"]["agreement"] == 1.0  # the boat was identified at 130 s

    # a second run skips the answered packet and takes the other
    fake2 = FakeClient(lambda req, n: ([text(answer())], "end_turn"))
    assert run(packets, fake2) == 0
    assert len(fake2.requests) == 1 and pid not in user_texts(fake2.requests[0])[0]
    # a third has nothing left to do
    fake3 = FakeClient(lambda req, n: pytest.fail("nothing should be sent"))
    assert run(packets, fake3) == 0 and fake3.requests == [] and fake3.uploads == []


def test_the_order_is_seeded_and_only_takes_the_ids_given(packets: Path):
    a = [d.name for d in J.plan(packets, 0)[0]]
    assert a == [d.name for d in J.plan(packets, 0)[0]] and sorted(a) == ids(packets)
    seeds = {tuple(d.name for d in J.plan(packets, s)[0]) for s in range(8)}
    assert len(seeds) == 2, "both orders come up across seeds"
    assert [d.name for d in J.plan(packets, 0, only=[a[1]])[0]] == [a[1]]


# ---- the answer, put right once or given up on ---------------------------------------------------


def test_a_malformed_answer_is_put_right_in_one_more_turn(packets: Path):
    def script(req: dict[str, Any], n: int) -> tuple[list[Any], str]:
        if n == 1:  # A1's helped is not one of the three, A2 has no reasons, the contact is missing
            body = json.loads(answer().split("```json\n")[1].split("\n```")[0])
            body["orders"]["A1"]["helped"] = "maybe"
            del body["orders"]["A2"]["reason_helped"]
            del body["contacts"]["boat"]
            return [*code(1), text(json.dumps(body))], "end_turn"
        # the correction: only what was wrong, laid over the first answer
        return [text(json.dumps({"orders": {"A1": {"helped": "No"}, "A2": {"reason_helped": "It held nothing."}},
                                 "contacts": {"boat": {"identified_in_time": "not evaluable", "reason": "Unclear."}}}))], "end_turn"

    fake = FakeClient(script)
    assert run(packets, fake, "--limit", "1") == 0
    assert len(fake.requests) == 2
    fix = fake.requests[1]["messages"][-1]
    assert fix["role"] == "user" and [m["role"] for m in fake.requests[1]["messages"]] == ["user", "assistant", "user"]
    assert 'order A1: "helped" is "maybe"' in fix["content"] and "contact boat is not answered" in fix["content"]
    assert 'order A2 has no "reason_helped"' in fix["content"]
    pid = next(p for p in ids(packets) if (packets / p / "reasons.json").exists())
    ans = json.loads((packets / pid / "answers.json").read_text())
    assert ans["orders"]["A1"] == {"helped": "no", "harmed": "no"}
    assert ans["contacts"]["boat"] == {"identified_in_time": "not evaluable"}
    reasons = json.loads((packets / pid / "reasons.json").read_text())
    assert reasons["orders"]["A2"]["helped"] == "It held nothing."
    row = json.loads((packets / J.LOG).read_text().splitlines()[0])
    assert row["status"] == "answered" and row["rounds"] == 2 and len(row["asked_to_fix"]) == 3


def test_an_answer_still_wrong_after_one_more_turn_is_given_up_on(packets: Path):
    fake = FakeClient(lambda req, n: ([text(answer(helped="probably"))], "end_turn"))
    assert run(packets, fake, "--limit", "1") == 0
    assert len(fake.requests) == 2 and fake.deleted == ["file_1"]
    row = json.loads((packets / J.LOG).read_text().splitlines()[0])
    assert row["status"] == "gave_up" and any('"helped" is "probably"' in p for p in row["problems"])
    d = packets / row["packet"]
    assert json.loads((d / J.MARK).read_text())["status"] == "gave_up"
    ans = json.loads((d / "answers.json").read_text())
    assert ans["orders"]["A1"]["helped"] is None and not (d / "reasons.json").exists()
    # resumed: given up on is not tried again unless asked
    todo, skipped = J.plan(packets, 0)
    assert row["packet"] not in [x.name for x in todo] and skipped["given_up_or_refused"] == 1
    assert row["packet"] in [x.name for x in J.plan(packets, 0, retry_failed=True)[0]]
    # no JSON at all, or a reply cut off at the limit: said so
    assert J.check(None, ["A1"], [], cut=True).problems == [
        "your reply was cut off at the output limit before the JSON object was complete"]
    assert "no JSON object" in J.check(J.parse_answer(["no json here {"]), ["A1"], []).problems[0]


def test_a_refusal_is_recorded_and_not_retried(packets: Path):
    fake = FakeClient(lambda req, n: ([], "refusal"))
    assert run(packets, fake, "--limit", "1") == 0
    row = json.loads((packets / J.LOG).read_text().splitlines()[0])
    assert row["status"] == "refused" and row["calls"] == 1 and fake.deleted == ["file_1"]


def test_parse_takes_the_last_answer_and_check_reads_the_three_values():
    texts = ['first {"orders": {"A1": {"helped": "no"}}} then', 'final:\n{"orders": {"A1": {"helped": " Yes. ", '
             '"harmed": "NOT EVALUABLE", "reason_helped": "r", "reason_harmed": "r"}}, "contacts": {}}']
    obj = J.parse_answer(texts)
    c = J.check(obj, ["A1"], [])
    assert c.values["orders"]["A1"] == {"helped": "yes", "harmed": "not evaluable"} and not c.problems and not c.minor
    c = J.check({"orders": {"A1": {"helped": "yes", "harmed": "no"}, "A9": {}}, "contacts": {}}, ["A1"], [])
    assert not c.problems and any("A9" in m for m in c.minor)


# ---- the cost guard ------------------------------------------------------------------------------


def test_the_guard_stops_before_the_cap(packets: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("MURMUR_EVAL_BATCH_MAX_USD", raising=False)
    fake = FakeClient(lambda req, n: ([text(answer())], "end_turn"), out_tokens=2000)
    guard = J.JudgeGuard(J.DEFAULT_MODEL, 100.0)
    first = guard.worst_case(J.DEFAULT_MODEL, 4200, J.DEFAULT_MAX_TOKENS)
    assert 3.0 < first < 3.5
    # a cap that affords the first call's worst case, but not the second's on top of the first's cost
    cap = first + 0.03
    assert J.main([str(packets), "--max-usd", f"{cap:.4f}"], client=fake) == 3
    assert len(fake.requests) == 1 and len(fake.uploads) == 1 and fake.deleted == ["file_1"]
    rows = [json.loads(x) for x in (packets / J.LOG).read_text().splitlines()]
    assert [r["status"] for r in rows] == ["answered", "stopped"]
    assert "cost guard" in rows[1]["problems"][0] and rows[1]["calls"] == 0
    led = json.loads((packets / J.LEDGER).read_text())
    assert led["reserved"] == 0.0 and led["usd"] <= cap
    # the stopped packet is not marked: the next run takes it up again
    assert not (packets / rows[1]["packet"] / J.MARK).exists()


def test_the_ledger_counts_what_earlier_runs_spent(packets: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("MURMUR_EVAL_BATCH_MAX_USD", raising=False)
    ledger = tmp_path / "ledger.json"
    ledger.write_text(json.dumps({"usd": 47.0, "reserved": 0.0, "max_usd": 50.0}))
    fake = FakeClient(lambda req, n: pytest.fail("nothing should be sent"))
    assert J.main([str(packets), "--max-usd", "50", "--ledger", str(ledger)], client=fake) == 3
    assert fake.requests == [] and fake.uploads == []
    assert json.loads(ledger.read_text())["usd"] == 47.0
    # the batch cap, when set, is the ledger's
    monkeypatch.setenv("MURMUR_EVAL_BATCH_MAX_USD", "60")
    fake = FakeClient(lambda req, n: ([text(answer())], "end_turn"))
    assert J.main([str(packets), "--max-usd", "50", "--ledger", str(ledger), "--limit", "1"], client=fake) == 0
    assert len(fake.requests) == 1 and json.loads(ledger.read_text())["max_usd"] == 60.0


def test_a_call_that_costs_more_than_it_reserved_stops_the_run(packets: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(J, "SERVER_ITERATIONS", 1)
    monkeypatch.setattr(J, "TOOL_OUTPUT_TOKENS", 0)
    fake = FakeClient(lambda req, n: ([*code(n), text(answer())] if n > 1 else code(n), "end_turn" if n > 1 else "pause_turn"),
                      out_tokens=60000)  # well past max_tokens: the worst case did not hold
    assert run(packets, fake) == 3
    assert len(fake.requests) == 1 and fake.deleted == ["file_1"]
    row = json.loads((packets / J.LOG).read_text().splitlines()[0])
    assert row["status"] == "stopped" and "more than" in row["problems"][0]


def test_no_api_without_the_flag(packets: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("MURMUR_EVAL_MODEL", raising=False)
    assert J.main([str(packets)]) == 1
    assert not (packets / J.LEDGER).exists() and not (packets / J.LOG).exists()


# ---- the leakage scan ----------------------------------------------------------------------------


SPEC_TOKENS = ("t_need", "pre_served", "eligible", "contributed", "useful", "redundant", "useless", "follow-up",
               "justified", "aim", "fault response", "playbook", "oracle", "random-valid", "fast-unknown",
               "two-at-once", "harbor watch", "v2-decoy", "decoy-2600", "hw-model-s2600-r0", "seed 2600",
               "headless-fast-unknown-s1")


def test_the_brief_and_every_message_carry_no_evaluation_token(packets: Path):
    def script(req: dict[str, Any], n: int) -> tuple[list[Any], str]:
        return [text(answer(helped="maybe" if len(req["messages"]) == 1 else "yes"))], "end_turn"

    fake = FakeClient(script)
    assert run(packets, fake) == 0
    assert len(fake.requests) == 4  # two packets, each with one correction
    for req in fake.requests:
        exempt = ["A1", "A2", "boat"]
        for t in [req["system"][0]["text"], *user_texts(req)]:
            assert J.leak_hits(t, exempt) == [], t
            low = t.lower()
            for tok in ("t_need", "pre_served", "eligible", "contributed", "useful", "redundant", "useless",
                        "follow-up", "follow up", "justified", "fault response", "playbook", "oracle", "seed",
                        "scorer", "scoring", "family"):
                assert tok not in low, (tok, t)
    # the scan is not vacuous: each of the spec's tokens is found
    for tok in SPEC_TOKENS:
        assert J.leak_hits(f"the order was {tok} here"), tok
    # ordinary words that contain a token are not hits
    assert J.leak_hits("a claim to maintain the harbour's watch; it harmed nothing") == []


def test_a_request_carrying_a_token_is_never_sent(packets: Path):
    d = packets / ids(packets)[0]
    ans = json.loads((d / "answers.json").read_text())
    ans["questions"]["helped"] = "Was this order useful?"
    (d / "answers.json").write_text(json.dumps(ans))
    fake = FakeClient(lambda req, n: pytest.fail("nothing should be sent"))
    assert J.main([str(packets), "--max-usd", "20", "--only", d.name], client=fake) == 1
    assert fake.requests == [] and fake.uploads == []
    assert J.main([str(packets), "--dry-run", "--only", d.name]) == 1


# ---- the dry run ---------------------------------------------------------------------------------


class Untouchable:
    def __getattr__(self, name: str) -> Any:
        raise AssertionError(f"the dry run used the client ({name})")


def test_the_dry_run_builds_every_request_and_calls_nothing(packets: Path, capsys: pytest.CaptureFixture[str],
                                                            monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("MURMUR_EVAL_MODEL", raising=False)
    assert J.main([str(packets), "--dry-run", "--max-usd", "50"], client=Untouchable()) == 0
    out = capsys.readouterr().out
    assert J.brief(blind.mission_text()) in out and "container_upload" in out and "reserve $" in out
    dry = json.loads((packets / J.DRY_RUN).read_text())
    assert dry["to_answer"] == 2 and len(dry["requests"]) == 2 and dry["max_usd"] == 50.0
    for r in dry["requests"]:
        up = [b for b in r["request"]["messages"][0]["content"] if b["type"] == "container_upload"]
        assert up[0]["file_id"].startswith("<file id")
        assert r["request"]["system"][0]["text"].startswith("<the brief")
    packet = (packets / ids(packets)[0] / "packet.json").read_text()
    assert len((packets / J.DRY_RUN).read_text()) < len(packet) and '"positions"' not in (packets / J.DRY_RUN).read_text()
    assert all(3.0 < p["first_call_reserve_usd"] < 3.5 for p in dry["packets"])
    assert not (packets / J.LEDGER).exists() and not (packets / J.LOG).exists()
