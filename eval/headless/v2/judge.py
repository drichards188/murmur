"""Scoring v2's blind labeller (eval/SCORING-V2.md 2.7, the construct check): a model, briefed only
with the mission and an episode's blind packet (blind.py), fills that packet's `answers.json`.

    .venv/bin/python -m eval.headless.v2.judge <packets dir> [--limit N] [--seed S] [--only <packet ids>]
        [--model claude-opus-5-5] [--effort high] [--max-usd X] [--ledger <path>] [--dry-run]

**It calls the Anthropic API and costs money.** It refuses to call without `MURMUR_EVAL_MODEL=1`
(the tests hand it a fake client), and every call passes a cost guard first (below).

A packet is about 300K characters of numeric JSON, too much to read whole and too much to do
arithmetic over by eye, so the labeller works on it with Anthropic's server-side code execution
tool. For each packet whose answers are not filled yet, in a seeded random order (so `--limit`
takes an unbiased sample):

- `packet.json` is uploaded with the Files API and attached to the code container
  (`container_upload`); the request (streamed; adaptive thinking at `--effort`) carries the brief
  as its system prompt, and the questions of the packet's `answers.json` (its own wording, its
  order and contact ids) as the user's message. A turn the server pauses (`pause_turn`, its
  sampling loop's limit) is resumed by sending it back as it stands, on the same container, at
  most `--max-continuations` times. The uploaded file is deleted afterwards, whatever happened.
- The answer is one JSON object at the end of the reply: per order `helped`, `harmed`,
  `reason_helped`, `reason_harmed`; per contact `identified_in_time`, `reason`; each answer
  `yes`, `no` or `not evaluable`. It is checked against `answers.json` (every order and contact
  answered, only those three values, a reason for each); if anything is wrong or missing, one
  more turn says exactly what, and what comes back is laid over the first answer. An answer still
  incomplete then is given up on, and why is recorded. Missing reasons alone do not fail a packet.
- The values go into `answers.json` (the shape `blind.py import` reads, with `answered_by`), the
  reasons into `reasons.json` beside it, the conversation (code, outputs, text) into
  `judge-transcript.json`, and the packet's outcome into `judge.json`.

**The brief** (`brief()`) is the labeller's only instructions: an independent labeller for a
maritime surveillance exercise, the mission (verbatim from the packet) as the standard, the
packet's parts as all it has, the three answers and a reason each, the code tool for every
computation, "not evaluable" rather than a guess. It names nothing of the spec beyond what the
packet carries: before any request is sent, the brief and every message this driver writes are
scanned (`leak_hits`) for the scorer's terms and label names, family, agent, seed and run names
(eval/headless/v2/leakage.py's tokens, less `identified_in_time`, which is the packet's own
question key, and the label words of 2.5); a hit stops the run before anything is sent. The
packet's own ids are exempt there (they are packet content), and recorded if they would hit.

**Resumable**: a packet whose answers are filled is skipped, and so is one given up on or
refused (`judge.json`) unless `--retry-failed`. `--limit N` attempts at most N packets.
`--dry-run` builds every request and writes them (`judge-dry-run.json`, no file contents), prints
the brief, one request's shape and the cost guard's reservations, and calls nothing.

**The cost guard** (eval/model_agent.py's guard and ledger, priced as eval/headless/officer.py's
WatchGuard prices cached calls). Before each call it reserves the call's worst case, refused if it
would take this run past `--max-usd` or the ledger past its cap (`MURMUR_EVAL_BATCH_MAX_USD` when
set, else `--max-usd`); a refusal stops the run cleanly, the packet in hand left for the next run.
The worst case of one call, at the model's list price:

- input: the server's sampling loop runs up to `SERVER_ITERATIONS` (10) iterations in one call,
  each reading the whole context again: the request (at 2 characters a token), plus everything
  generated so far (at most `max_tokens`), plus the code's printed output (an allowance of
  `TOOL_OUTPUT_TOKENS` per iteration, which the brief keeps it well under), all priced as cache
  writes (1.25 times input), the dearest way input is billed;
- output: `max_tokens` (the cap on the whole reply);
- the container: `CALL_RESERVE_S` of code execution at `CONTAINER_USD_PER_HOUR`.

With `--fallbacks` (off by default, as for the officer), at the dearest list price and twice. After
the call the reservation is settled at what `usage` says was billed (fresh input, cache writes,
cache reads, output, per iteration when `usage.iterations` itemizes them) plus the container at
its wall time (at least `CONTAINER_MIN_S`, free hours not counted). A call that costs more than it
reserved stops the run at once (the guard's assumptions failed: look before going on). A call
that fails keeps its reservation as spent. Per packet, its calls, tokens and cost go into the run
log `<packets dir>/judge-log.jsonl`; the ledger defaults to `<packets dir>/judge-ledger.json`
(`--ledger`, or `MURMUR_EVAL_LEDGER`).

Configuration (environment):

    MURMUR_EVAL_MODEL=1          required: allows the external API at all
    MURMUR_EVAL_MAX_USD          the cap when --max-usd is not given (default 2.00)
    MURMUR_EVAL_BATCH_MAX_USD    the ledger's cap (default: the run's cap)
    MURMUR_EVAL_LEDGER           the ledger when --ledger is not given
    ANTHROPIC_API_KEY (or an `ant auth login` profile): the SDK's usual credentials
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import random
import re
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from eval.headless.officer import (API_MODELS, CACHE_WRITE_FACTOR, FALLBACK_BETA, ModelArmError, WatchGuard, _plain,
                                   _type, chars, fatal, usage_parts)
from eval.model_agent import PRICES, WORST_PRICE, CostGuardExceeded, Ledger, price_of

from . import leakage
from .blind import ANSWERS, mission_text

VERSION = "1"
DEFAULT_MODEL = "claude-opus-5-5"
DEFAULT_EFFORT = "high"
EFFORTS = ("low", "medium", "high", "xhigh", "max")
DEFAULT_MAX_TOKENS = 32000
DEFAULT_MAX_USD = "2.00"
MAX_CONTINUATIONS = 5
#: the server-side code execution tool (the claude-api skill: SKILL.md "Server Tools", the current
#: version; plain `messages`, no beta header, in Python)
CODE_TOOL = {"type": "code_execution_20260521", "name": "code_execution"}
#: the server's sampling loop pauses (`pause_turn`) after this many iterations
#: (shared/tool-use-concepts.md, "Stop reasons for server-side tools")
SERVER_ITERATIONS = 10
#: the code's printed output allowed for per iteration in a call's worst case
TOOL_OUTPUT_TOKENS = 4000
#: code execution: $0.05 an hour past the organization's free hours (shared/tool-use-concepts.md,
#: "Server-Side Tools: Code Execution"); reserved for this long per call, settled at the wall time
CONTAINER_USD_PER_HOUR = 0.05
CALL_RESERVE_S = 1200.0
CONTAINER_MIN_S = 300.0
UPLOAD_NAME = "packet.json"
LOG = "judge-log.jsonl"
LEDGER = "judge-ledger.json"
DRY_RUN = "judge-dry-run.json"
#: a packet's own outcome; given up on and refused are not retried unless asked
MARK = "judge.json"
FINAL = ("gave_up", "refused")
#: consecutive transient failures that stop the run (the API is down: stop spending reservations)
MAX_ERRORS_IN_ROW = 2


class LeakError(RuntimeError):
    pass


class Stop(RuntimeError):
    """Stop the run before the next call (a call cost more than it reserved)."""


# ---- the brief, the questions, the leakage scan ------------------------------------------------


def brief(mission: str) -> str:
    """The labeller's only instructions (the system prompt)."""
    return f"""You are an independent labeller for a maritime surveillance exercise. A C2 system supervised a \
small fleet of uncrewed vehicles at the Golden Gate for one episode. From the record of that episode you judge \
what its orders did and whether its contacts were identified in time.

The standard is the mission the C2 was given, verbatim:

"{mission}"

The episode's packet is everything you have. It is in your code container as {UPLOAD_NAME} (in the container's \
input directory; find it by name if it is not where you expect). Its parts:
- mission: the text above.
- world: the place, the bridge reference point ([longitude, latitude]) and the watch radius in metres, our \
vehicles with their platforms and sensors, how sensing and identification work, and the kinds of order.
- duration_s: the episode's length in seconds.
- truth: what really happened. truth.contacts gives each contact's kind, affiliation and whether it broadcasts \
AIS; truth.positions gives each contact's position as [t, lon, lat] rows; truth.our_vehicles gives our vehicles' \
positions, altitudes, modes, batteries, homes and links.
- picture: what the C2 showed: per sample, one row per track, with the columns named in picture.columns \
(identity kind, confidence, conflict, the sensors supporting it, affiliation, position, velocity, last seen, and \
each vehicle sensor's hits on the track, keyed "<vehicle uid>:<sensor>").
- orders: each order's id, kind, vehicles, parameters, when it was created, whether a person was asked and what \
they decided, whether the C2's checks refused it, its outcome, and when each vehicle started and stopped it.
Times are seconds from the start of the episode. Nothing outside the packet is available; do not assume anything \
it does not show.

Work with the code tool: load {UPLOAD_NAME} in Python and compute what each answer needs (distances in metres \
from the bridge reference point, the times things happened, the confidences, sensors and hits the picture showed, \
where each vehicle was and what it was doing). Never print the file or large parts of it: print compact results, \
at most about 2,000 characters at a time.

Answer each question exactly "yes", "no" or "not evaluable", each with a reason of one or two sentences that \
cites what you found (times, distances, confidences). Say "not evaluable" when the packet does not let you \
decide; do not guess.

When you have finished, end your reply with one JSON object, in this shape:
{{"orders": {{"A1": {{"helped": "yes", "harmed": "no", "reason_helped": "...", "reason_harmed": "..."}}}}, \
"contacts": {{"<contact id>": {{"identified_in_time": "no", "reason": "..."}}}}}}
It has one entry for every order and every contact you are asked about, and no others."""


def asked(answers: dict[str, Any]) -> tuple[list[str], list[str]]:
    """The order ids and contact ids a packet's answers.json asks about, in its order."""
    return list((answers.get("orders") or {}).keys()), list((answers.get("contacts") or {}).keys())


def answered(answers: dict[str, Any]) -> bool:
    """Every question of the packet has an answer (vacuously so for a packet that asks none)."""
    orders = answers.get("orders") or {}
    contacts = answers.get("contacts") or {}
    return (all((o or {}).get("helped") is not None and (o or {}).get("harmed") is not None for o in orders.values())
            and all((c or {}).get("identified_in_time") is not None for c in contacts.values()))


def question_text(pid: str, answers: dict[str, Any]) -> str:
    """The user's message: the packet's own questions and ids."""
    orders, contacts = asked(answers)
    q = answers.get("questions") or {}
    lines = [f"This episode's packet is attached to your code container as {UPLOAD_NAME} (packet {pid}).", "",
             "The questions, as asked for this episode:"]
    for key in ("helped", "harmed", "identified_in_time"):
        if q.get(key):
            lines.append(f'- {key}: "{q[key]}"')
    lines.append("")
    if orders:
        lines.append(f"Answer helped and harmed for each of these orders ({UPLOAD_NAME} orders): {', '.join(orders)}.")
    else:
        lines.append('There are no orders to answer for this episode: give "orders": {}.')
    if contacts:
        lines.append(f"Answer identified_in_time for each of these contacts ({UPLOAD_NAME} truth.contacts): "
                     f"{', '.join(contacts)}.")
    else:
        lines.append('No contact is asked about in this episode: give "contacts": {}.')
    lines += ["", 'Each answer is exactly "yes", "no" or "not evaluable", with a reason of one or two sentences. '
              "When you have finished, end your reply with the JSON object."]
    return "\n".join(lines)


def fix_text(problems: list[str]) -> str:
    """The one turn that asks for exactly what was wrong to be put right."""
    return ("Your reply cannot be used as it stands:\n" + "\n".join(f"- {p}" for p in problems) + "\n\n"
            "Reply with the whole JSON object again, corrected: every order and contact listed above, each answer "
            'exactly "yes", "no" or "not evaluable", each with its reason.')


#: the spec's words for its labels and terms that the packet does not carry (SCORING-V2.md 2.1 to
#: 2.5), the arms' names, and the evaluation's own vocabulary: none may reach the labeller
_SPEC_WORDS = (
    ("label", "t_need", r"t[-_ ]need"), ("label", "pre_served", r"pre[-_ ]served"),
    ("label", "eligible", r"eligib\w*"), ("label", "contributed", r"contribut\w*"),
    ("label", "useful", r"useful\w*"), ("label", "redundant", r"redundan\w*"), ("label", "useless", r"useless\w*"),
    ("label", "follow-up", r"follow[-_ ]?ups?"), ("label", "justified", r"justif\w*"),
    ("label", "aim", r"aim(?:s|ed|ing)?"), ("label", "fault response", r"fault[-_ ]respons\w*"),
    ("label", "harmful", r"harmful\w*"), ("label", "credited", r"credit\w*"),
    ("agent", "playbook", r"playbook\w*"), ("agent", "oracle", r"oracle\w*"), ("agent", "random-valid", r"random[-_ ]valid"),
    ("agent", "agent id", r"agent[-_](?:model|watch|playbook|setup|scripted)"), ("agent", "watch officer", r"watch[-_ ]officer"),
    ("spec", "scorer", r"scor(?:e|er|es|ing)\b\w*"), ("spec", "instrument", r"instrument\w*"),
    ("spec", "family", r"famil(?:y|ies)"), ("spec", "seed", r"seed\w*"), ("spec", "replicate", r"replicat\w*"),
    ("spec", "pilot", r"pilot\w*"), ("spec", "ladder", r"ladder\w*"), ("spec", "scott", r"scott\w*"),
)
#: the packet's own question keys: leakage.py counts them as scorer fields, but the packet carries them
PACKET_KEYS = frozenset({"identified_in_time"})


def leak_patterns() -> list[tuple[str, str, re.Pattern[str]]]:
    out = [(k, t, rx) for k, t, rx in leakage.patterns() if t not in PACKET_KEYS]
    out += [(k, t, re.compile(r"(?<![A-Za-z0-9])" + p + r"(?![A-Za-z0-9])", re.I)) for k, t, p in _SPEC_WORDS]
    return out


_PATTERNS = leak_patterns()


def leak_hits(text: str, exempt: list[str] | tuple[str, ...] = ()) -> list[dict[str, Any]]:
    """Every token the text carries, the exempt strings (the packet's own ids) taken out first."""
    for e in sorted((e for e in exempt if e), key=len, reverse=True):
        text = text.replace(e, " ")
    out = []
    for kind, token, rx in _PATTERNS:
        m = rx.search(text)
        if m:
            out.append({"kind": kind, "token": token, "match": m.group(0),
                        "excerpt": text[max(0, m.start() - 40):m.end() + 40]})
    return out


# ---- the answer ------------------------------------------------------------------------------


def parse_answer(texts: list[str]) -> dict[str, Any] | None:
    """The last JSON object in the reply's text that has `orders` or `contacts`."""
    text = "\n".join(t for t in texts if t)
    dec = json.JSONDecoder()
    found = None
    i = text.find("{")
    while i != -1:
        try:
            obj, end = dec.raw_decode(text, i)
        except ValueError:
            i = text.find("{", i + 1)
            continue
        if isinstance(obj, dict) and ("orders" in obj or "contacts" in obj):
            found = obj
            i = text.find("{", end)
        else:
            i = text.find("{", i + 1)
    return found


def merge(first: dict[str, Any] | None, second: dict[str, Any] | None) -> dict[str, Any] | None:
    """The second answer laid over the first, entry by entry (a reply may correct only what was wrong)."""
    if not isinstance(first, dict):
        return second
    if not isinstance(second, dict):
        return first
    out: dict[str, Any] = {}
    for part in ("orders", "contacts"):
        a = first.get(part) if isinstance(first.get(part), dict) else {}
        b = second.get(part) if isinstance(second.get(part), dict) else {}
        merged = {k: dict(v) if isinstance(v, dict) else v for k, v in a.items()}
        for k, v in b.items():
            if isinstance(v, dict) and isinstance(merged.get(k), dict):
                merged[k] = {**merged[k], **v}
            else:
                merged[k] = v
        out[part] = merged
    return out


def _value(v: Any) -> str | None:
    if not isinstance(v, str):
        return None
    s = v.strip().lower().rstrip(".")
    return s if s in ANSWERS else None


def _reason(v: Any) -> str | None:
    return v.strip() if isinstance(v, str) and v.strip() else None


@dataclass
class Checked:
    values: dict[str, Any]
    reasons: dict[str, Any]
    problems: list[str]  # answers missing or not one of the three values: the packet fails on these
    minor: list[str]  # reasons missing, entries not asked about: asked about once, never fail a packet


def check(obj: Any, orders: list[str], contacts: list[str], cut: bool = False) -> Checked:
    """The answer against the questions: every order and contact answered with one of the three
    values, a reason each."""
    values: dict[str, Any] = {"orders": {}, "contacts": {}}
    reasons: dict[str, Any] = {"orders": {}, "contacts": {}}
    problems: list[str] = []
    minor: list[str] = []
    if not isinstance(obj, dict):
        why = ("your reply was cut off at the output limit before the JSON object was complete" if cut else
               'no JSON object with "orders" and "contacts" was found at the end of your reply')
        return Checked(values, reasons, [why], [])
    allowed = ", ".join(f'"{a}"' for a in ANSWERS)
    got_o = obj.get("orders") if isinstance(obj.get("orders"), dict) else {}
    got_c = obj.get("contacts") if isinstance(obj.get("contacts"), dict) else {}
    for aid in orders:
        e = got_o.get(aid)
        if not isinstance(e, dict):
            problems.append(f"order {aid} is not answered")
            continue
        for q in ("helped", "harmed"):
            v = _value(e.get(q))
            if v is None:
                problems.append(f'order {aid}: "{q}" is {json.dumps(e.get(q))}, not one of {allowed}')
            else:
                values["orders"].setdefault(aid, {})[q] = v
            r = _reason(e.get(f"reason_{q}"))
            if r is None:
                minor.append(f'order {aid} has no "reason_{q}"')
            else:
                reasons["orders"].setdefault(aid, {})[q] = r
    for cid in contacts:
        e = got_c.get(cid)
        if not isinstance(e, dict):
            problems.append(f"contact {cid} is not answered")
            continue
        v = _value(e.get("identified_in_time"))
        if v is None:
            problems.append(f'contact {cid}: "identified_in_time" is {json.dumps(e.get("identified_in_time"))}, '
                            f"not one of {allowed}")
        else:
            values["contacts"][cid] = {"identified_in_time": v}
        r = _reason(e.get("reason"))
        if r is None:
            minor.append(f'contact {cid} has no "reason"')
        else:
            reasons["contacts"][cid] = {"identified_in_time": r}
    extra = [f"order {k}" for k in got_o if k not in orders] + [f"contact {k}" for k in got_c if k not in contacts]
    if extra:
        minor.append(f"{', '.join(extra)} {'is' if len(extra) == 1 else 'are'} not asked about: leave "
                     f"{'it' if len(extra) == 1 else 'them'} out")
    return Checked(values, reasons, problems, minor)


# ---- the cost guard --------------------------------------------------------------------------


@dataclass
class JudgeGuard(WatchGuard):
    """The officer's guard with this driver's worst case (a server-side code execution loop in one
    call) and the code container's time on top of the tokens."""

    container_usd: float = 0.0

    def worst_case(self, model: str, request_chars: int, max_tokens: int) -> float:
        pin, pout = WORST_PRICE if self.fallbacks else price_of(model)
        attempts = 2 if self.fallbacks else 1
        n = SERVER_ITERATIONS
        tokens_in = n * (request_chars / 2.0 + max_tokens) + n * (n - 1) / 2.0 * TOOL_OUTPUT_TOKENS
        container = CALL_RESERVE_S / 3600.0 * CONTAINER_USD_PER_HOUR
        return attempts * (tokens_in * pin * CACHE_WRITE_FACTOR + max_tokens * pout) / 1e6 + container

    def release(self) -> None:
        """Nothing was sent (the upload failed): the reservation goes back."""
        if self.reserved_usd and self.ledger is not None:
            self.ledger.settle(self.reserved_usd, 0.0)
        self.reserved_usd = 0.0

    def container(self, wall_s: float) -> float:
        usd = max(wall_s, CONTAINER_MIN_S) / 3600.0 * CONTAINER_USD_PER_HOUR
        self.container_usd += usd
        self.spent_usd += usd
        if self.ledger is not None:
            self.ledger.add(usd)
        return usd

    def summary(self) -> dict[str, Any]:
        return {**super().summary(), "container_usd": round(self.container_usd, 6),
                "pricing": "list prices; cache reads and writes at their own rates; code execution at its hourly "
                           "rate, free hours not counted"}


# ---- the driver ------------------------------------------------------------------------------


def make_client() -> Any:
    """The Anthropic client (synchronous), only with MURMUR_EVAL_MODEL=1 (the SDK's own credentials)."""
    if not os.environ.get("MURMUR_EVAL_MODEL"):
        raise ModelArmError("the labeller calls the Anthropic API: set MURMUR_EVAL_MODEL=1 to allow it (never in tests)")
    try:
        import anthropic
    except ImportError as e:
        raise ModelArmError("the labeller needs the Anthropic SDK: pip install anthropic") from e
    return anthropic.Anthropic()


def _now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def _write(path: Path, obj: Any, indent: int | None = 1) -> None:
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(obj, indent=indent, default=_plain, ensure_ascii=False))
    tmp.replace(path)


def _texts(blocks: list[Any]) -> list[str]:
    out = []
    for b in blocks:
        if _type(b) == "text":
            t = b.get("text") if isinstance(b, dict) else getattr(b, "text", None)
            if t:
                out.append(str(t))
    return out


@dataclass
class PacketRun:
    pid: str
    data: bytes
    system: list[dict[str, Any]]
    messages: list[dict[str, Any]]
    upload: dict[str, Any]
    exempt: list[str]
    file_id: str | None = None
    container: str | None = None
    calls: list[dict[str, Any]] = field(default_factory=list)


def plan(packets: Path, seed: int = 0, only: list[str] | None = None, retry_failed: bool = False) -> tuple[list[Path], dict[str, int]]:
    """The packets to answer, in a seeded random order; and how many were skipped, and why."""
    dirs = sorted(d for d in packets.iterdir() if d.is_dir() and (d / "packet.json").exists()
                  and (d / "answers.json").exists())
    random.Random(seed).shuffle(dirs)
    if only:
        want = set(only)
        dirs = [d for d in dirs if d.name in want]
    todo, skipped = [], {"answered": 0, "nothing_to_ask": 0, "given_up_or_refused": 0}
    for d in dirs:
        ans = json.loads((d / "answers.json").read_text())
        o, c = asked(ans)
        if not o and not c:
            skipped["nothing_to_ask"] += 1
            continue
        if answered(ans):
            skipped["answered"] += 1
            continue
        mark = d / MARK
        if not retry_failed and mark.exists():
            try:
                status = json.loads(mark.read_text()).get("status")
            except ValueError:
                status = None
            if status in FINAL:
                skipped["given_up_or_refused"] += 1
                continue
        todo.append(d)
    return todo, skipped


class Judge:
    def __init__(self, packets: Path, client: Any, guard: JudgeGuard, model: str = DEFAULT_MODEL,
                 effort: str = DEFAULT_EFFORT, max_tokens: int = DEFAULT_MAX_TOKENS,
                 max_continuations: int = MAX_CONTINUATIONS, fallbacks: bool = False):
        self.packets = packets
        self.client = client
        self.guard = guard
        self.model = model
        self.effort = effort
        self.max_tokens = max_tokens
        self.max_continuations = max_continuations
        self.fallbacks = fallbacks
        self.log = packets / LOG
        self.halt: str | None = None  # a reason not to send another call
        self.errors_in_row = 0

    # -- requests

    def start(self, d: Path) -> tuple[PacketRun, dict[str, Any]]:
        """A packet's first request, built and scanned (nothing sent, nothing uploaded)."""
        answers = json.loads((d / "answers.json").read_text())
        data = (d / "packet.json").read_bytes()
        mission = json.loads(data).get("mission") or mission_text()
        orders, contacts = asked(answers)
        system_text = brief(mission)
        user_text = question_text(d.name, answers)
        exempt = [d.name, *orders, *contacts]
        hits = [{"in": "brief", **h} for h in leak_hits(system_text, exempt)]
        hits += [{"in": "question", **h} for h in leak_hits(user_text, exempt)]
        upload: dict[str, Any] = {"type": "container_upload", "file_id": None}
        run = PacketRun(pid=d.name, data=data, system=[{"type": "text", "text": system_text, "cache_control": {"type": "ephemeral"}}],
                        messages=[{"role": "user", "content": [{"type": "text", "text": user_text}, upload]}],
                        upload=upload, exempt=exempt)
        ids_hit = {i: [h["token"] for h in leak_hits(i)] for i in exempt if leak_hits(i)}
        return run, {"orders": orders, "contacts": contacts, "answers": answers, "ids_matching_tokens": ids_hit,
                     "leaks": hits}

    def kwargs(self, run: PacketRun) -> dict[str, Any]:
        kw: dict[str, Any] = {
            "model": self.model, "max_tokens": self.max_tokens, "system": run.system, "tools": [CODE_TOOL],
            "messages": run.messages, "thinking": {"type": "adaptive"}, "output_config": {"effort": self.effort},
            "cache_control": {"type": "ephemeral"},
        }
        if run.container:
            kw["container"] = run.container
        return kw

    def request_chars(self, run: PacketRun) -> int:
        return chars(run.system) + chars([CODE_TOOL]) + chars(run.messages)

    def _call(self, run: PacketRun) -> Any:
        if self.halt:
            raise Stop(self.halt)
        size = self.request_chars(run)
        est = self.guard.worst_case(self.model, size, self.max_tokens)
        self.guard.check(est)  # raises CostGuardExceeded: nothing is sent
        if run.file_id is None:
            try:
                up = self.client.files.upload(file=(UPLOAD_NAME, run.data, "application/json"))
            except BaseException:
                self.guard.release()
                raise
            run.file_id = str(up.id if not isinstance(up, dict) else up["id"])
            run.upload["file_id"] = run.file_id
        kw = self.kwargs(run)
        t0 = time.monotonic()
        try:
            if self.fallbacks:
                cm = self.client.beta.messages.stream(betas=[FALLBACK_BETA], fallbacks="default", **kw)
            else:
                cm = self.client.messages.stream(**kw)
            with cm as stream:
                resp = stream.get_final_message()
        except BaseException:
            self.guard.failed()  # what it billed is unknown: its worst case counts as spent
            raise
        wall = time.monotonic() - t0
        served = getattr(resp, "model", None)
        served = served if isinstance(served, str) else self.model
        usd = self.guard.add(resp.usage, served)
        cusd = self.guard.container(wall)
        parts = usage_parts(resp.usage, served)
        c = getattr(resp, "container", None)
        cid = (c.get("id") if isinstance(c, dict) else getattr(c, "id", None)) if c is not None else None
        if cid:
            run.container = str(cid)
        stop = getattr(resp, "stop_reason", None)
        info = {
            "call": len(run.calls) + 1, "served": sorted({p[0] for p in parts}), "request_chars": size,
            "reserved_usd": round(est, 6), "usd": round(usd + cusd, 6), "container_usd": round(cusd, 6),
            "tokens": {"input": sum(p[1] for p in parts), "cache_write": sum(p[2] for p in parts),
                       "cache_read": sum(p[3] for p in parts), "output": sum(p[4] for p in parts)},
            "stop_reason": stop, "wall_s": round(wall, 3), "container": run.container,
        }
        if stop == "refusal":
            sd = getattr(resp, "stop_details", None)
            info["stop_details"] = _plain(sd) if sd is not None else None
        run.calls.append(info)
        if usd + cusd > est + 1e-9:
            self.halt = (f"packet {run.pid} call {info['call']} cost ${usd + cusd:.4f}, more than the ${est:.4f} it "
                         "reserved: the guard's worst case does not hold, so the run stops here")
        return resp

    def _round(self, run: PacketRun) -> tuple[str, list[str]]:
        """One turn of the labeller: a call, resumed while the server pauses it. (stop reason, texts)"""
        texts: list[str] = []
        for _ in range(1 + self.max_continuations):
            resp = self._call(run)
            blocks = list(getattr(resp, "content", None) or [])
            if run.messages[-1]["role"] == "assistant":
                run.messages[-1]["content"].extend(blocks)  # a resumed turn: the same assistant turn, continued
            else:
                run.messages.append({"role": "assistant", "content": blocks})
            texts += _texts(blocks)
            stop = str(getattr(resp, "stop_reason", None))
            if stop != "pause_turn":
                return stop, texts
        return "pause_limit", texts

    # -- a packet

    def packet(self, d: Path) -> dict[str, Any]:
        run, q = self.start(d)
        if q["leaks"]:
            raise LeakError(f"packet {d.name}: the request carries evaluation tokens: {q['leaks'][:5]}")
        orders, contacts, answers = q["orders"], q["contacts"], q["answers"]
        row: dict[str, Any] = {"packet": run.pid, "at": _now(), "version": VERSION, "model": self.model,
                               "effort": self.effort, "orders": len(orders), "contacts": len(contacts)}
        if q["ids_matching_tokens"]:
            row["ids_matching_tokens"] = q["ids_matching_tokens"]
        t0 = time.monotonic()
        status: str | None = None
        problems: list[str] = []
        minor: list[str] = []
        still = f"still working after {self.max_continuations} continuations"
        try:
            stop, texts = self._round(run)
            first = parse_answer(texts)
            if stop == "refusal":
                status = "refused"
            elif stop == "pause_limit":
                status, problems = "gave_up", [still]
            else:
                c = check(first, orders, contacts, cut=stop == "max_tokens")
                if c.problems or c.minor:
                    text = fix_text(c.problems + c.minor)
                    hits = leak_hits(text, run.exempt)
                    if hits:
                        raise LeakError(f"packet {run.pid}: the correction carries evaluation tokens: {hits[:5]}")
                    run.messages.append({"role": "user", "content": text})
                    row["asked_to_fix"] = c.problems + c.minor
                    stop, texts = self._round(run)
                    if stop == "refusal":
                        status = "refused"
                    elif stop == "pause_limit":
                        status, problems = "gave_up", [still]
                    else:
                        c = check(merge(first, parse_answer(texts)), orders, contacts, cut=stop == "max_tokens")
                if status is None:
                    if c.problems:
                        status, problems = "gave_up", c.problems
                    else:
                        status, minor = "answered", c.minor
                        self.write(d, run.pid, answers, c)
            self.errors_in_row = 0
        except (CostGuardExceeded, Stop) as e:
            status, problems = "stopped", [str(e)]
            self.halt = self.halt or str(e)
        except LeakError:
            raise
        except Exception as e:  # noqa: BLE001
            status, problems = "error", [f"{type(e).__name__}: {e}"]
            self.errors_in_row += 1
            if fatal(e):
                self.halt = f"the API refused the request as such ({type(e).__name__}: {e})"
            elif self.errors_in_row >= MAX_ERRORS_IN_ROW:
                self.halt = f"{self.errors_in_row} packets failed in a row on transient errors; last: {type(e).__name__}: {e}"
        finally:
            if run.file_id:
                try:
                    self.client.files.delete(run.file_id)
                    row["file_deleted"] = True
                except Exception as e:  # noqa: BLE001
                    row["file_deleted"] = False
                    row["delete_error"] = f"{type(e).__name__}: {e} (file {run.file_id})"
        row.update({
            "status": status or "error", "problems": problems, "minor": minor, "calls": len(run.calls),
            "rounds": 1 + ("asked_to_fix" in row), "stop_reasons": [c["stop_reason"] for c in run.calls],
            "tokens": {k: sum(c["tokens"][k] for c in run.calls) for k in ("input", "cache_write", "cache_read", "output")},
            "usd": round(sum(c["usd"] for c in run.calls), 6),
            "container_usd": round(sum(c["container_usd"] for c in run.calls), 6),
            "reserved_max_usd": round(max((c["reserved_usd"] for c in run.calls), default=0.0), 6),
            "wall_s": round(time.monotonic() - t0, 3), "per_call": run.calls,
        })
        if run.calls:
            _write(d / "judge-transcript.json", {"packet": run.pid, "model": self.model, "effort": self.effort,
                                                  "messages": run.messages})
        if status in ("answered", *FINAL):
            _write(d / MARK, {k: row[k] for k in ("packet", "at", "status", "problems", "minor", "model", "effort",
                                                   "calls", "usd")})
        with open(self.log, "a") as f:
            f.write(json.dumps(row, default=_plain) + "\n")
        return row

    def write(self, d: Path, pid: str, answers: dict[str, Any], c: Checked) -> None:
        out = json.loads(json.dumps(answers))
        for aid, v in c.values["orders"].items():
            out["orders"][aid] = {**(out["orders"].get(aid) or {}), **v}
        for cid, v in c.values["contacts"].items():
            out["contacts"][cid] = {**(out["contacts"].get(cid) or {}), **v}
        out["answered_by"] = {"by": "eval/headless/v2/judge.py", "version": VERSION, "model": self.model,
                              "effort": self.effort}
        _write(d / "reasons.json", {"packet": pid, "model": self.model, "effort": self.effort, **c.reasons})
        _write(d / "answers.json", out)

    def run(self, todo: list[Path], limit: int | None = None) -> dict[str, Any]:
        rows = []
        for d in todo:
            if limit is not None and len(rows) >= limit:
                break
            if self.halt:
                break
            rows.append(self.packet(d))
        by: dict[str, int] = {}
        for r in rows:
            by[r["status"]] = by.get(r["status"], 0) + 1
        out = {"attempted": len(rows), "by_status": by, "stopped": self.halt, "log": str(self.log),
               "guard": self.guard.summary()}
        if self.guard.ledger is not None:
            out["ledger_usd"] = round(self.guard.ledger.spent(), 6)
        return out


# ---- the dry run -----------------------------------------------------------------------------


def _shape(kw: dict[str, Any], brief_chars: int) -> dict[str, Any]:
    """A request as sent, the brief and the file stood in for (no file contents)."""
    s = json.loads(json.dumps(kw, default=_plain))
    s["system"] = [{**b, "text": f"<the brief, {brief_chars} characters>"} for b in s["system"]]
    for m in s["messages"]:
        for b in m["content"] if isinstance(m["content"], list) else []:
            if b.get("type") == "container_upload":
                b["file_id"] = f"<file id of the uploaded {UPLOAD_NAME}>"
    return s


def dry_run(packets: Path, todo: list[Path], limit: int | None, judge: Judge, skipped: dict[str, int],
            ledger: Path, ledger_cap: float, max_usd: float) -> dict[str, Any]:
    picks = todo[:limit] if limit is not None else todo
    rows, shapes = [], []
    first_brief = None
    for d in picks:
        run, q = judge.start(d)
        kw = judge.kwargs(run)
        text = run.system[0]["text"]
        first_brief = first_brief or text
        size = judge.request_chars(run)
        est = judge.guard.worst_case(judge.model, size, judge.max_tokens)
        rows.append({"packet": run.pid, "orders": len(q["orders"]), "contacts": len(q["contacts"]),
                     "packet_bytes": len(run.data), "request_chars": size, "first_call_reserve_usd": round(est, 4),
                     "ids_matching_tokens": q["ids_matching_tokens"] or None, "leaks": q["leaks"] or None})
        shapes.append({"packet": run.pid, "request": _shape(kw, len(text))})
    max_calls = 2 * (1 + judge.max_continuations)
    spent = 0.0
    if ledger.exists():
        try:
            st = json.loads(ledger.read_text() or "{}")
            spent = float(st.get("usd", 0.0)) + float(st.get("reserved", 0.0))
        except ValueError:
            spent = float("nan")
    res = {
        "packets_dir": str(packets), "to_answer": len(picks), "skipped": skipped, "model": judge.model,
        "effort": judge.effort, "max_tokens": judge.max_tokens, "fallbacks": judge.fallbacks,
        "price_per_mtok": {"input": price_of(judge.model)[0], "output": price_of(judge.model)[1]},
        "max_usd": max_usd, "ledger": str(ledger), "ledger_cap_usd": ledger_cap, "ledger_spent_usd": round(spent, 6),
        "reserve": {
            "per_call": "SERVER_ITERATIONS x (request + max_tokens) + 45 x TOOL_OUTPUT_TOKENS input tokens at the cache "
                        "write price, max_tokens output, CALL_RESERVE_S of container time",
            "server_iterations": SERVER_ITERATIONS, "tool_output_tokens": TOOL_OUTPUT_TOKENS,
            "container_usd_per_hour": CONTAINER_USD_PER_HOUR, "call_reserve_s": CALL_RESERVE_S,
            "max_calls_per_packet": max_calls,
            "first_call_usd": {"min": min((r["first_call_reserve_usd"] for r in rows), default=None),
                               "max": max((r["first_call_reserve_usd"] for r in rows), default=None)},
        },
        "packets": rows,
        "brief": first_brief,
        "requests": shapes,
    }
    _write(packets / DRY_RUN, res)
    return res


# ---- the command -----------------------------------------------------------------------------


def _env(name: str, default: Any) -> Any:
    v = os.environ.get(name)
    return v if v not in (None, "") else default


def main(argv: list[str] | None = None, client: Any = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("packets", type=Path, help="the blind packets directory (blind.py export --out)")
    ap.add_argument("--limit", type=int, default=None, help="attempt at most N packets this run")
    ap.add_argument("--seed", type=int, default=0, help="the packets' random order")
    ap.add_argument("--only", nargs="+", default=None, metavar="PACKET", help="these packet ids only")
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--effort", default=DEFAULT_EFFORT, choices=EFFORTS)
    ap.add_argument("--max-usd", type=float, default=None, help="this run's cap (default MURMUR_EVAL_MAX_USD or 2.00)")
    ap.add_argument("--ledger", type=Path, default=None, help=f"the spend ledger (default MURMUR_EVAL_LEDGER or <packets>/{LEDGER})")
    ap.add_argument("--max-tokens", type=int, default=DEFAULT_MAX_TOKENS)
    ap.add_argument("--max-continuations", type=int, default=MAX_CONTINUATIONS)
    ap.add_argument("--fallbacks", action="store_true", help="the server-side refusal fallback (priced at the dearest model, twice)")
    ap.add_argument("--retry-failed", action="store_true", help="try again the packets given up on or refused")
    ap.add_argument("--dry-run", action="store_true", help="build every request, call nothing")
    args = ap.parse_args(argv)
    model = API_MODELS.get(args.model, args.model)
    if model not in PRICES:
        print(f"no list price for {model}: add its row to eval/model_agent.py PRICES first", file=sys.stderr)
        return 2
    max_usd = float(args.max_usd if args.max_usd is not None else _env("MURMUR_EVAL_MAX_USD", DEFAULT_MAX_USD))
    ledger_path = Path(args.ledger or _env("MURMUR_EVAL_LEDGER", args.packets / LEDGER))
    ledger_cap = float(_env("MURMUR_EVAL_BATCH_MAX_USD", max_usd))
    todo, skipped = plan(args.packets, args.seed, args.only, args.retry_failed)
    if args.dry_run:
        guard = JudgeGuard(model, max_usd, None, fallbacks=args.fallbacks)
        judge = Judge(args.packets, None, guard, model, args.effort, args.max_tokens, args.max_continuations, args.fallbacks)
        res = dry_run(args.packets, todo, args.limit, judge, skipped, ledger_path, ledger_cap, max_usd)
        print("== the brief (system prompt) ==")
        print(res["brief"] or "(no packet to answer)")
        if res["requests"]:
            print("\n== the first request's shape (no file contents) ==")
            print(json.dumps(res["requests"][0]["request"], indent=1))
        print("\n== per packet: the first call's reservation (the guard's worst case) ==")
        for r in res["packets"]:
            print(f"{r['packet']}  orders {r['orders']}  contacts {r['contacts']}  request {r['request_chars']} chars  "
                  f"reserve ${r['first_call_reserve_usd']:.2f}"
                  + (f"  ids matching tokens {r['ids_matching_tokens']}" if r["ids_matching_tokens"] else "")
                  + (f"  LEAKS {[h['token'] for h in r['leaks']]}" if r["leaks"] else ""))
        summary = {k: res[k] for k in ("to_answer", "skipped", "model", "effort", "max_tokens", "fallbacks",
                                       "max_usd", "ledger", "ledger_cap_usd", "ledger_spent_usd", "reserve")}
        summary["packets_with_leaks"] = sum(1 for r in res["packets"] if r["leaks"])
        print("\n" + json.dumps(summary, indent=1))
        print(f"\nevery request: {args.packets / DRY_RUN}")
        return 1 if summary["packets_with_leaks"] else 0
    try:
        client = client if client is not None else make_client()
    except ModelArmError as e:
        print(str(e), file=sys.stderr)
        return 1
    ledger = Ledger(ledger_path, ledger_cap)
    guard = JudgeGuard(model, max_usd, ledger, fallbacks=args.fallbacks)
    judge = Judge(args.packets, client, guard, model, args.effort, args.max_tokens, args.max_continuations, args.fallbacks)
    try:
        res = judge.run(todo, args.limit)
    except LeakError as e:
        print(f"stopped before sending: {e}", file=sys.stderr)
        return 1
    res["skipped"] = skipped
    print(json.dumps(res, indent=1, default=_plain))
    return 3 if res["stopped"] else 0


if __name__ == "__main__":
    sys.exit(main())
