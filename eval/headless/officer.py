"""The model watch officer on the headless simulator (docs/HEADLESS.md, section 14): the agent
`Options.agent = "model"` runs.

**It calls the Anthropic API and costs money.** Nothing in `make check`, the tests or a default
batch calls it: an episode refuses to start the model agent without `MURMUR_EVAL_MODEL=1`
unless it is handed a client (the tests hand it a fake one), and every call passes
eval/model_agent.py's cost guard first (a cap per episode, and the batch's ledger under its file
lock). A wake the guard refuses is recorded as `cost_guard`, never as the model choosing to do
nothing, the officer stops, and the episode is marked `budget_stopped`. A request the API
refuses as such (bad parameters, credentials) stops it too, `model_failed`; a transient error
fails only its wake, as the CLI's error turn does. The batch reports budget-stopped and failed
episodes apart and leaves them out of every metric.

It is agents/watch_officer.py's design on the episode's clock, not a new agent; every piece
that can be imported is:

- **when to wake**: agents/watch/wake.py's `Waker`, with the `Budgets` the live driver builds
  from its own argument defaults (`watch_officer.parse_args([])`: 60 wakes an hour, 12 tool
  turns a wake, 6 escalations, a 60 s heartbeat, 3 s coalescing, 10 s between wakes);
- **what it reads**: agents/watch/picture.py's `Poller` (the change feed on its cursor every
  `poll_s`, 2 s, and the nomination check while a track is declared hostile) over the session
  adapter, as agent `agent-watch`, the live officer's id;
- **what it is told**: `compose` (the update), `render_mine` (its own proposals), the
  officer's own budget line (`Officer.budget_line`), `CONSULT_HEAD` for a consult and the note
  the routine model gets about it afterwards, under `build_prompt` (agents/watch/prompt.md
  and the situation's brief); a failed turn backs its lane off by `Officer.failed`;
- **which model**: the `claude` row of `watch_officer.DEFAULTS` (routine `sonnet` at
  `medium`, consult `opus` at `high`) as API ids (`API_MODELS`), overridable by the episode's
  options and the environment (`ModelConfig.resolve`); a wake goes to the stronger model when
  the `Waker` escalates it and that lane is not backing off, as `Officer.do_wake` decides;
- **the model's turn**: a Messages API tool-use loop (`ApiBackend`) in place of the CLI
  backends: the bridge's MCP tools as the simulator lists them (`tools/list`, the definitions
  an MCP client hands its model), at most `max_turns_per_wake` model calls a wake. The routine
  model's conversation carries over from wake to wake, as the CLI's long-lived session does
  (append-only, prompt-cached); a consult is a fresh conversation (the system prompt and its
  update), as the CLI's consult process is.

**Simulated time.** The officer is the episode's `agent` process: it polls every `poll_s` of
simulated time and a wake runs between two simulator steps. Every tool call is a request at
the current instant (the simulator refuses one that moves its clock, and each wake reads the
clock before and after and fails the episode if it moved). The world waits for the model: the
wall time a wake took is recorded, never simulated.

**The record.** Each wake (`wakes`): its simulated time, reason, lane, model and effort, the
update it was given, every tool call (name, arguments, an excerpt of the answer), every model
call (request size, tokens, cost, wall time), the final text, the stop. Each proposal the
model made goes into `agent_log` as a `proposal` line with the play category it maps to
(`map_proposal`) and what the picture showed of its target at that instant (read by the driver
before the proposal went in), for metrics.py.
"""

from __future__ import annotations

import json
import os
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from eval.model_agent import PRICES, WORST_PRICE, CostGuard, CostGuardExceeded, Ledger, _n, price_of, tool_defs

from . import REPO
from .labels import AMBIGUOUS, AMBIGUOUS_RATIO, ASSOC_M, dist_m
from .metrics import LOW_RESERVE_PCT
from .sim import HeadlessSession, Sim

sys.path.insert(0, str(REPO / "agents"))
import watch_officer as WO  # noqa: E402  (agents/watch_officer.py)
from playbook.beliefs import NOT_RUNNING, RECOVERING_MODES  # noqa: E402
from playbook.plays.safety import STUCK_KINDS  # noqa: E402
from watch.backend import ToolCall, Turn, excerpt  # noqa: E402
from watch.picture import NoChangesTool, Poller, compose, render_mine  # noqa: E402
from watch.wake import Budgets, Wake, Waker  # noqa: E402

#: the live officer's agent id (eval/situation.py WATCH_AGENT)
AGENT = "agent-watch"
CLIENT_VERSION = "headless-model"
#: `watch_officer.DEFAULTS["claude"]` names the CLI's aliases; the API takes model ids (the
#: current generation, which the aliases resolve to)
API_MODELS = {"sonnet": "claude-sonnet-5-5", "opus": "claude-opus-5-5"}
#: a cache read's list price, US dollars per million tokens; a cache write (5-minute TTL) is
#: 1.25 times the input price
CACHE_READ_PRICE = {"claude-opus-5-5": 0.20, "claude-sonnet-5-5": 0.20}
CACHE_WRITE_FACTOR = 1.25
DEFAULT_MAX_TOKENS = 16000
DEFAULT_MAX_USD = "3.00"
FALLBACK_BETA = "server-side-fallback-2026-07-01"
#: the bridge's `anthropic/maxResultSizeChars` (mcp::tools::RESULT_MAX_CHARS): the CLI hands
#: an answer up to this long to its model whole
RESULT_MAX_CHARS = 150_000
TEXT_MAX = 2000
EXCERPT = 400

#: the play categories a model proposal can map to (docs/HEADLESS.md section 14; metrics.py)
LOOK = "isr.investigate"
RECENTRE = "isr.recentre"
RELEASE = "isr.release"
LOW_RESERVE = "safety.low_reserve"
STUCK = "safety.stuck"


class ModelArmError(RuntimeError):
    pass


def api_model(name: str) -> str:
    return API_MODELS.get(str(name), str(name))


def _env(name: str, default: Any) -> Any:
    v = os.environ.get(name)
    return v if v not in (None, "") else default


def _flag(v: Any) -> bool:
    return str(v).strip().lower() not in ("0", "false", "no", "off", "")


@dataclass
class ModelConfig:
    """The models, efforts and spending limits of one model episode. Each value comes from the
    episode's options (`Options.model`, a dict of these names), else the environment, else the
    live officer's `claude` defaults:

        model, effort                MURMUR_EVAL_WATCH_MODEL, MURMUR_EVAL_WATCH_EFFORT
                                     (routine; default sonnet = claude-sonnet-5-5, medium)
        strong_model, strong_effort  MURMUR_EVAL_WATCH_STRONG_MODEL, ..._STRONG_EFFORT
                                     (consult; default opus = claude-opus-5-5, high)
        max_tokens                   MURMUR_EVAL_MAX_TOKENS (per model call; default 16000)
        max_usd                      MURMUR_EVAL_MAX_USD (per episode; default 3.00)
        batch_max_usd, ledger        MURMUR_EVAL_BATCH_MAX_USD (default 20.00), MURMUR_EVAL_LEDGER
                                     (the batch sets it: one ledger for all its workers)
        fallbacks                    MURMUR_EVAL_WATCH_FALLBACKS (default off: a refusal is
                                     recorded as one; on, the guard prices every call at the
                                     dearest model, twice)
        escalation                   options only (default on: the consult lane is used)
    """

    model: str
    effort: str
    strong_model: str
    strong_effort: str
    max_tokens: int
    max_usd: float
    batch_max_usd: float
    ledger: str | None
    fallbacks: bool
    escalation: bool

    @classmethod
    def resolve(cls, given: dict[str, Any] | None = None) -> "ModelConfig":
        g = dict(given or {})
        d = WO.DEFAULTS["claude"]

        def pick(key: str, env: str, default: Any) -> Any:
            return g[key] if g.get(key) is not None else _env(env, default)

        cfg = cls(
            model=api_model(pick("model", "MURMUR_EVAL_WATCH_MODEL", d["model"])),
            effort=str(pick("effort", "MURMUR_EVAL_WATCH_EFFORT", d["effort"])),
            strong_model=api_model(pick("strong_model", "MURMUR_EVAL_WATCH_STRONG_MODEL", d["strong_model"])),
            strong_effort=str(pick("strong_effort", "MURMUR_EVAL_WATCH_STRONG_EFFORT", d["strong_effort"])),
            max_tokens=int(pick("max_tokens", "MURMUR_EVAL_MAX_TOKENS", DEFAULT_MAX_TOKENS)),
            max_usd=float(pick("max_usd", "MURMUR_EVAL_MAX_USD", DEFAULT_MAX_USD)),
            batch_max_usd=float(pick("batch_max_usd", "MURMUR_EVAL_BATCH_MAX_USD", "20.00")),
            ledger=pick("ledger", "MURMUR_EVAL_LEDGER", None),
            fallbacks=_flag(pick("fallbacks", "MURMUR_EVAL_WATCH_FALLBACKS", "0")),
            escalation=_flag(g.get("escalation", True)),
        )
        for m in (cfg.model, cfg.strong_model):
            if m not in PRICES:
                raise ModelArmError(f"no list price for {m}: add its row to eval/model_agent.py PRICES first "
                                    "(the cost guard must price every model it lets through)")
        return cfg

    def as_dict(self) -> dict[str, Any]:
        return {"routine": {"model": self.model, "effort": self.effort},
                "consult": {"model": self.strong_model, "effort": self.strong_effort},
                "max_tokens": self.max_tokens, "max_usd": self.max_usd, "batch_max_usd": self.batch_max_usd,
                "ledger": self.ledger, "fallbacks": self.fallbacks, "escalation": self.escalation}


# ---- the cost guard ------------------------------------------------------------------------


def cache_read_price(model: str) -> float:
    for name, p in CACHE_READ_PRICE.items():
        if model == name or model.startswith(name + "-"):
            return p
    return price_of(model)[0]  # unknown: as fresh input, as eval/model_agent.py prices every read


def usage_parts(usage: Any, model: str) -> list[tuple[str, int, int, int, int]]:
    """(model, fresh input, cache write, cache read, output) per billed iteration:
    `usage.iterations` when the response has them (each at the model that served it), else one
    entry at `model`."""
    its = usage.get("iterations") if isinstance(usage, dict) else getattr(usage, "iterations", None)
    src = list(its) if isinstance(its, (list, tuple)) and its else [usage]
    out = []
    for it in src:
        m = (it.get("model") if isinstance(it, dict) else getattr(it, "model", None)) or model
        out.append((str(m), _n(it, "input_tokens"), _n(it, "cache_creation_input_tokens"),
                    _n(it, "cache_read_input_tokens"), _n(it, "output_tokens")))
    return out


@dataclass
class WatchGuard(CostGuard):
    """eval/model_agent.py's guard for an officer with two models: before a call, the worst
    case of that call at the model it goes to (its input at 2 characters a token, its whole
    `max_tokens`, at the dearest list price twice with fallbacks on), refused if it would take
    the episode past its cap or the batch past its ledger's; after it, what the call billed at
    list prices, settled in the ledger. Unlike the storyline arm, which prices every cache read
    as fresh input, this arm's conversation is cached on purpose, so cache reads and writes are
    settled at their own list rates (`CACHE_READ_PRICE`, `CACHE_WRITE_FACTOR`); the worst-case
    check before each call still counts the whole request as fresh input."""

    fresh_input_tokens: int = 0
    cache_write_tokens: int = 0
    cache_read_tokens: int = 0

    def estimate_for(self, model: str, request_chars: int, max_tokens: int) -> float:
        pin, pout = WORST_PRICE if self.fallbacks else price_of(model)
        attempts = 2 if self.fallbacks else 1
        return attempts * ((request_chars / 2.0) * pin + max_tokens * pout) / 1e6

    def add(self, usage: Any, served_by: str | None = None) -> float:
        cost = 0.0
        models = []
        for m, fresh, write, read, out in usage_parts(usage, served_by or self.model):
            pin, pout = price_of(m)
            cost += (fresh * pin + write * pin * CACHE_WRITE_FACTOR + read * cache_read_price(m) + out * pout) / 1e6
            self.input_tokens += fresh + write + read
            self.fresh_input_tokens += fresh
            self.cache_write_tokens += write
            self.cache_read_tokens += read
            self.output_tokens += out
            models.append(m)
        self.calls += 1
        self.spent_usd += cost
        if self.ledger is not None:
            self.ledger.settle(self.reserved_usd, cost)
            self.reserved_usd = 0.0
        self.log.append({"call": self.calls, "billed": models, "usd": round(cost, 6), "run_usd": round(self.spent_usd, 6)})
        return cost

    def summary(self) -> dict[str, Any]:
        return {**super().summary(), "fresh_input_tokens": self.fresh_input_tokens,
                "cache_write_tokens": self.cache_write_tokens, "cache_read_tokens": self.cache_read_tokens,
                "pricing": "list prices; cache reads and writes at their own rates"}


def make_client() -> Any:
    """The Anthropic client, only with MURMUR_EVAL_MODEL=1 (the SDK's own credentials)."""
    if not os.environ.get("MURMUR_EVAL_MODEL"):
        raise ModelArmError("the model agent calls the Anthropic API: set MURMUR_EVAL_MODEL=1 to allow it "
                            "(never in tests)")
    try:
        import anthropic
    except ImportError as e:
        raise ModelArmError("the model agent needs the Anthropic SDK: pip install anthropic") from e
    return anthropic.AsyncAnthropic()


# ---- the model's turn ------------------------------------------------------------------------


def _plain(o: Any) -> Any:
    if hasattr(o, "model_dump"):
        return o.model_dump(exclude_none=True)
    if hasattr(o, "__dict__"):
        return vars(o)
    return str(o)


def chars(obj: Any) -> int:
    return len(json.dumps(obj, default=_plain, ensure_ascii=False))


class Conversation:
    """Messages and their size in characters, append-only (what each request sends again)."""

    def __init__(self) -> None:
        self.messages: list[dict[str, Any]] = []
        self.chars = 0

    def append(self, msg: dict[str, Any]) -> None:
        self.messages.append(msg)
        self.chars += chars(msg) + 1


def _blocks(r: Any) -> list[Any]:
    return list(getattr(r, "content", None) or [])


def _type(b: Any) -> str:
    return str(b.get("type") if isinstance(b, dict) else getattr(b, "type", ""))


def _get(b: Any, k: str) -> Any:
    return b.get(k) if isinstance(b, dict) else getattr(b, k, None)


def fatal(e: BaseException) -> bool:
    """An error no wait will cure: a request the API refuses as such (a 4xx other than a
    timeout, a conflict or a rate limit: bad parameters, credentials, permissions, an unknown
    model) or a failure before any request (the SDK's arguments). A connection error, a
    timeout, an overload or a server error is transient."""
    status = getattr(e, "status_code", None)
    if isinstance(status, int):
        return 400 <= status < 500 and status not in (408, 409, 429)
    name = type(e).__name__
    return not any(k in name for k in ("Connection", "Timeout", "Overloaded", "InternalServer", "RateLimit"))


def _not_run(b: Any, why: str) -> dict[str, Any]:
    return {"type": "tool_result", "tool_use_id": _get(b, "id"), "content": f"not run: {why}", "is_error": True}


class ApiBackend:
    """The CLI backends' place (agents/watch/claude.py): one wake's turn, a tool-use loop of at
    most `max_turns` model calls. `turn(..., consult=False)` continues the routine
    conversation; `consult=True` starts a fresh one (the system prompt and this update only).
    Every call is cached at its last block (the top-level `cache_control`), so a call reads what
    the last one wrote, and at the end of the system prompt (an explicit breakpoint: the tools
    and the prompt, the same for every call of a model, stay readable whatever the conversation
    holds); thinking blocks go back unchanged (append-only). `execute(name, input)` runs a tool
    and answers (text, is_error)."""

    def __init__(self, cfg: ModelConfig, client: Any, system: str, tools: list[dict[str, Any]],
                 guard: WatchGuard, execute: Any, max_turns: int):
        self.cfg = cfg
        self.client = client
        self.system = system
        self.system_blocks = [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}]
        self.tools = tools
        self.tools_chars = chars(tools)
        self.guard = guard
        self.execute = execute
        self.max_turns = max_turns
        self.routine = Conversation()
        self.last_fatal = False  # the last turn's error is one no wait will cure

    async def _create(self, model: str, effort: str, conv: Conversation) -> tuple[Any, dict[str, Any]]:
        size = len(self.system) + self.tools_chars + conv.chars
        est = self.guard.estimate_for(model, size, self.cfg.max_tokens)
        self.guard.check(est)  # raises CostGuardExceeded: nothing is sent
        kwargs: dict[str, Any] = {
            "model": model, "max_tokens": self.cfg.max_tokens, "system": self.system_blocks, "tools": self.tools,
            "messages": conv.messages, "output_config": {"effort": effort}, "cache_control": {"type": "ephemeral"},
        }
        t0 = time.perf_counter()
        try:
            if self.cfg.fallbacks:
                r = await self.client.beta.messages.create(betas=[FALLBACK_BETA], fallbacks="default", **kwargs)
            else:
                r = await self.client.messages.create(**kwargs)
        except Exception:
            self.guard.failed()  # what it billed is unknown: its worst case counts as spent
            raise
        served = getattr(r, "model", None)
        served = served if isinstance(served, str) else model
        usd = self.guard.add(r.usage, served)
        parts = usage_parts(r.usage, served)
        return r, {
            "model": model, "served": sorted({p[0] for p in parts}), "request_chars": size,
            "estimate_usd": round(est, 6), "usd": round(usd, 6),
            "tokens": {"input": sum(p[1] for p in parts), "cache_write": sum(p[2] for p in parts),
                       "cache_read": sum(p[3] for p in parts), "output": sum(p[4] for p in parts)},
            "stop_reason": getattr(r, "stop_reason", None), "wall_s": round(time.perf_counter() - t0, 3),
        }

    async def turn(self, text: str, model: str, effort: str, consult: bool) -> tuple[Turn, list[dict[str, Any]]]:
        conv = Conversation() if consult else self.routine
        conv.append({"role": "user", "content": text})
        self.last_fatal = False
        turn = Turn(model=model)
        calls: list[dict[str, Any]] = []
        t0 = time.perf_counter()
        stop = "success"
        while True:
            if len(calls) >= self.max_turns:
                stop = "max_turns"  # the CLI's error_max_turns: the turn ends, the session goes on
                break
            try:
                r, meta = await self._create(model, effort, conv)
            except CostGuardExceeded as e:
                stop, turn.error = "cost_guard", str(e)
                break
            except Exception as e:  # noqa: BLE001 (an API error after the SDK's retries: the turn failed)
                stop, turn.error = "error", f"{type(e).__name__}: {e}"[:2000]
                self.last_fatal = fatal(e)
                break
            calls.append(meta)
            content = _blocks(r)
            conv.append({"role": "assistant", "content": content})
            texts = [str(_get(b, "text")) for b in content if _type(b) == "text" and _get(b, "text")]
            if texts:
                turn.text = "\n".join(texts).strip()
            uses = [b for b in content if _type(b) == "tool_use"]
            reason = meta["stop_reason"]
            if reason == "refusal" or (reason == "max_tokens" and uses):
                if uses:  # the history stays valid: every tool_use answered
                    conv.append({"role": "user", "content": [_not_run(b, f"the response ended ({reason})") for b in uses]})
                stop = "refusal" if reason == "refusal" else "max_tokens"
                details = getattr(r, "stop_details", None)
                if reason == "refusal":
                    turn.error = f"refusal: {_plain(details) if details is not None else 'no details'}"
                break
            if not uses:
                stop = "success" if reason in (None, "end_turn", "stop_sequence") else str(reason)
                break
            results = []
            for b in uses:
                name, args = str(_get(b, "name")), _get(b, "input")
                answer, is_error = self.execute(name, args)
                turn.tool_calls.append(ToolCall(name, args, _get(b, "id"), excerpt(answer, EXCERPT), is_error))
                results.append({"type": "tool_result", "tool_use_id": _get(b, "id"), "content": answer,
                                "is_error": is_error})
            conv.append({"role": "user", "content": results})
        turn.stop = stop
        turn.num_turns = len(calls)
        turn.duration_s = time.perf_counter() - t0
        turn.cost_usd = round(sum(c["usd"] for c in calls), 6)
        turn.usage = {k: sum(c["tokens"][k] for c in calls) for k in ("input", "cache_write", "cache_read", "output")}
        turn.context_tokens = max((sum(c["tokens"][k] for k in ("input", "cache_write", "cache_read")) for c in calls),
                                  default=None)
        return turn, calls


# ---- what a proposal maps to -----------------------------------------------------------------


def track_where(t: dict[str, Any]) -> list[float] | None:
    """A track's position carried to now (`predicted`), else its last fix: what the playbook
    centres a look on (beliefs.Track.where)."""
    for k in ("predicted", "position"):
        p = t.get(k)
        if isinstance(p, list) and len(p) == 2 and all(isinstance(x, (int, float)) for x in p):
            return [float(p[0]), float(p[1])]
    return None


def picture_of_track(t: dict[str, Any]) -> dict[str, Any]:
    """What the picture showed of a track (labels.py `picture_of`'s shape): the affiliation the C2
    shows (a declaration's when there is one) and its confidence, the identity and its."""
    decl = t.get("declared")
    declared = decl.get("affiliation") if isinstance(decl, dict) else (decl if isinstance(decl, str) else None)
    ident = t.get("identity") if isinstance(t.get("identity"), dict) else {}

    def r2(v: Any) -> float | None:
        return round(float(v), 2) if isinstance(v, (int, float)) and not isinstance(v, bool) else None

    return {"affiliation": declared or t.get("affiliation"), "affiliation_confidence": r2(t.get("affiliation_confidence")),
            "declared": declared, "identity": ident.get("kind"), "identity_confidence": r2(ident.get("confidence"))}


def _uid(v: Any) -> int | None:
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def running_look(v: dict[str, Any] | None, looks: dict[int, dict[str, Any]]) -> dict[str, Any] | None:
    """The model's own look (an earlier proposal mapped to a look or a recentre) that the
    vehicle flies now (isr.py `runs`: its current order, not failed, superseded or requeued,
    and not recovering on its own)."""
    if not v:
        return None
    task = v.get("task") if isinstance(v.get("task"), dict) else {}
    iid = _uid(task.get("intent_id"))
    if iid is None or iid not in looks:
        return None
    if task.get("state") in NOT_RUNNING or str(v.get("mode") or "").upper() in RECOVERING_MODES:
        return None
    return {**looks[iid], "intent": iid}


def map_proposal(req: dict[str, Any], picture: dict[str, Any], looks: dict[int, dict[str, Any]]) -> dict[str, Any]:
    """The play category a model proposal is equivalent to, from what it does (docs/HEADLESS.md
    section 14, metrics.py): its task, vehicle, aim point and the track it aims at as the
    picture showed it when it was proposed (`picture`: `tracks` from get_tracks, `vehicles` from
    get_fleet compact, `alerts` from get_inbox, read at that instant). `looks`: the model's
    earlier looks, intent id -> {track, uid}. Anything that does not map is `unjudged:<task>`.

    - an orbit whose centre is within max(its radius, 500 m) of a track shown: a look on the
      nearest such track, `isr.recentre` when the vehicle already flies the model's own look on
      that track, else `isr.investigate`; a second track within 1.5 times that distance makes
      the aim `ambiguous` (judged as the playbook's ambiguous targets are);
    - a hold or an rtl of a vehicle the picture showed with a `not_moving` or `task_failed`
      alert: `safety.stuck` (an rtl of one also under the low-reserve line: `safety.low_reserve`);
      under the line (battery under 25 %, the shipped watch policy's reserve plus 5):
      `safety.low_reserve`; flying the model's own look and neither: `isr.release` on that track.
    """
    task = str(req.get("task") or "?")
    uids = [u for u in (_uid(x) for x in req.get("uids") or []) if u is not None]
    uid = uids[0] if uids else None
    tracks = [t for t in picture.get("tracks") or [] if isinstance(t, dict) and t.get("id") is not None]
    fleet = {u: v for v in picture.get("vehicles") or [] if isinstance(v, dict) and (u := _uid(v.get("uid"))) is not None}
    v = fleet.get(uid) if uid is not None else None
    run = running_look(v, looks)
    out: dict[str, Any] = {"play": f"unjudged:{task}", "target": None,
                           "rule": f"a {task} that is not a look, a recentre, a release or a P0 command"}
    if task == "orbit":
        c = req.get("center")
        if not (isinstance(c, (list, tuple)) and len(c) == 2 and all(isinstance(x, (int, float)) for x in c)):
            out["rule"] = "an orbit without a centre"
            return out
        radius = float(req.get("radius_m") or 200.0)
        reach = max(radius, ASSOC_M)
        near = sorted((dist_m((float(c[0]), float(c[1])), (w[0], w[1])), str(t["id"]))
                      for t in tracks if (w := track_where(t)) is not None)
        if not near or near[0][0] > reach:
            out["rule"] = f"an orbit whose centre is not within {reach:.0f} m (its radius or 500 m) of a track shown"
            return out
        d0, tid = near[0]
        t = next(x for x in tracks if str(x["id"]) == tid)
        ambiguous = len(near) > 1 and near[1][0] <= AMBIGUOUS_RATIO * max(d0, 1e-6)
        play = RECENTRE if run is not None and run.get("track") == tid else LOOK
        return {"play": play, "target": tid, "aim": AMBIGUOUS if ambiguous else tid, "aim_m": round(d0, 1),
                "predicted": track_where(t), "picture": picture_of_track(t),
                "rule": (f"an orbit centred {d0:.0f} m from {tid}" + (f", the vehicle on the model's own look i{run['intent']} on it"
                                                                      if play == RECENTRE else ""))}
    if task in ("hold", "rtl") and v is not None:
        stuck = [a for a in picture.get("alerts") or []
                 if isinstance(a, dict) and _uid(a.get("uid")) == uid and a.get("kind") in STUCK_KINDS]
        bat = v.get("battery_pct")
        low = isinstance(bat, (int, float)) and not isinstance(bat, bool) and bat < LOW_RESERVE_PCT
        if stuck and (task == "hold" or not low):
            return {"play": STUCK, "target": f"alert:{stuck[0].get('id')}",
                    "rule": f"a {task} of a vehicle with a {stuck[0].get('kind')} alert"}
        if low:
            return {"play": LOW_RESERVE, "target": f"battery:{uid}",
                    "rule": f"a {task} of a vehicle shown at {bat:.0f} % battery, under {LOW_RESERVE_PCT:.0f} %"}
        if run is not None:
            tid = run.get("track")
            t = next((x for x in tracks if str(x["id"]) == tid), None)
            return {"play": RELEASE, "target": tid, "aim": tid,
                    "predicted": track_where(t) if t else None, "picture": picture_of_track(t) if t else None,
                    "rule": f"a {task} of a vehicle flying the model's own look i{run['intent']} on {tid}"}
    return out


# ---- the session and the officer -------------------------------------------------------------


class WatchSession(HeadlessSession):
    """The session adapter with what agents/watch/picture.py's `Poller` reads through
    (`AgentSession.tracks` and `intents`, and the raw `session.call_tool` of its nomination
    check)."""

    def __init__(self, sim: Sim, agent: str = AGENT, client_version: str = CLIENT_VERSION):
        super().__init__(sim, agent, client_version)
        self.session = self

    async def tracks(self, **filters: Any) -> list[dict[str, Any]]:
        return list((await self.call("get_tracks", **filters)).get("tracks", []))

    async def intents(self, mine: bool = True) -> list[dict[str, Any]]:
        return list((await self.call("get_intents", mine=mine)).get("intents", []))

    async def call_tool(self, name: str, args: dict[str, Any]) -> Any:
        is_error, body = self._call(name, args)
        return SimpleNamespace(structured_content=body, content=[], is_error=is_error)


class _Log:
    """The officer's situation-log lines (agents/watch/situation.py's kinds), on the simulated
    clock, into the episode's `agent_log`."""

    def __init__(self, officer: "WatchOfficer"):
        self.o = officer

    def write(self, kind: str, **fields: Any) -> dict[str, Any]:
        rec = {"t": round(self.o.sim.t, 3), "source": "model", "kind": kind}
        for k, v in fields.items():
            rec[f"field_{k}" if k in rec else k] = v
        self.o.lines.append(rec)
        return rec


class WatchOfficer:
    """agents/watch_officer.py's `Officer` on the episode's clock (`wake` is the episode's
    `agent` process)."""

    def __init__(self, ep: Any, cfg: ModelConfig, client: Any = None):
        self.ep = ep
        self.sim: Sim = ep.sim
        self.cfg = cfg
        self.epoch = float(ep.sim_epoch)
        self.args = WO.parse_args([])  # the live driver's defaults: budgets, poll, back-off
        a = self.args
        self.budgets = Budgets(max_wakes_per_hour=a.max_wakes_per_hour, max_turns_per_wake=a.max_turns_per_wake,
                               max_escalations=a.max_escalations, max_wakes=a.max_wakes,
                               heartbeat_s=a.heartbeat_s, coalesce_s=a.coalesce_s, min_gap_s=a.min_gap_s)
        self.waker = Waker(self.budgets, escalation=cfg.escalation and not a.no_escalation)
        self.poll_s = float(a.poll_s)
        self.lines: list[dict[str, Any]] = []
        self.log = _Log(self)
        self.session = WatchSession(self.sim)
        self.poller = Poller(lambda: WatchSession(self.sim))
        # Officer.failed's state: each model's back-off, the crashes it counts
        self.lanes = {"routine": WO.Lane(), "consult": WO.Lane()}
        self.crashes: list[float] = []
        self.consult_notes: list[str] = []
        self.connected = False
        self.connect_delay = 2.0
        self.next_poll: float | None = None
        self.poll_errors = 0
        self.stopped: str | None = None
        self.stopped_t: float | None = None
        self.budget_stopped = False
        self.model_failed = False
        self.wakes: list[dict[str, Any]] = []
        self.looks: dict[int, dict[str, Any]] = {}
        self.proposed: list[int] = []
        self.wall_s = 0.0
        self._wake_ctx: dict[str, Any] = {}  # the wake a tool call belongs to
        self._wake_proposed: list[int] = []  # intents the wake in progress created
        self.system = WO.build_prompt(getattr(ep.e, "brief", "") or None)
        defs = self.sim.tool_defs()
        self.tools = tool_defs([SimpleNamespace(name=d["name"], description=d.get("description"),
                                                input_schema=d.get("inputSchema")) for d in defs])
        self.tool_names = {t["name"] for t in self.tools}
        ledger = Ledger(Path(cfg.ledger), cfg.batch_max_usd) if cfg.ledger else None
        self.guard = WatchGuard(cfg.model, cfg.max_usd, ledger, fallbacks=cfg.fallbacks)
        self.client = client if client is not None else make_client()
        self.backend = ApiBackend(cfg, self.client, self.system, self.tools, self.guard, self.execute,
                                  self.budgets.max_turns_per_wake)
        self.log.write("run.start", models=cfg.as_dict(), budgets=dict(vars(self.budgets)), agent=AGENT,
                       prompt_chars=len(self.system), tools=sorted(self.tool_names),
                       tools_chars=self.backend.tools_chars)

    # ---- the event loop's process ----------------------------------------------------------

    def run(self, coro: Any) -> Any:
        return self.ep.run_async(coro)

    def bnow(self, now_s: float) -> float:
        """The bridge's clock (Unix seconds) at situation second `now_s`."""
        return self.epoch + float(now_s)

    def wake(self, now_s: float) -> float | None:
        if self.stopped:
            return None
        bnow = self.bnow(now_s)
        if not self.connected:
            # Officer.connect: the change feed's cursor and the hostile tracks, retried 2 s
            # doubling to 30 s while the bridge does not answer
            try:
                info = self.run(self.poller.open())
            except NoChangesTool as e:
                self.log.write("error", error=str(e))
                return self.stop(now_s, "error")
            except Exception as e:  # noqa: BLE001
                self.log.write("poll.error", error=f"{type(e).__name__}: {e}", retry_in_s=self.connect_delay)
                delay, self.connect_delay = self.connect_delay, min(self.connect_delay * 2, 30.0)
                return now_s + delay
            self.connected = True
            self.log.write("bridge.connected", cursor=info["cursor"], bridge_run=info["run"], tools=info["tools"])
            self.next_poll = now_s
        assert self.next_poll is not None
        if now_s >= self.next_poll - 1e-9:
            self.poll(bnow)
            self.next_poll = now_s + self.poll_s
        for _ in range(3):  # Officer.run's loop at one instant: a wake, then (min_gap_s) none
            if self.waker.exhausted():
                self.log.write("budget.exhausted", what="wakes per run", max_wakes=self.budgets.max_wakes)
                return self.stop(now_s, "budget")
            routine = self.lanes["routine"]
            if bnow < routine.until:
                wake = None
                if bnow >= self.lanes["consult"].until:
                    wake = self.waker.due(bnow, escalating_only=True)
            else:
                wake = self.waker.due(bnow)
            self.log_dropped()
            if wake is None:
                break
            outcome = self.do_wake(wake, now_s)
            if outcome:
                return self.stop(now_s, outcome)
        nxt = min(self.waker.next_check(bnow), bnow + self.poll_s)
        if bnow < self.lanes["routine"].until:
            nxt = min(nxt, self.lanes["routine"].until)
        return min(self.next_poll, now_s + max(0.05, nxt - bnow))

    def stop(self, now_s: float, reason: str) -> None:
        self.stopped, self.stopped_t = reason, round(now_s, 3)
        self.log.write("run.stop", reason=reason, wakes=self.waker.wakes, escalations=self.waker.escalations,
                       proposed=self.proposed, usd=round(self.guard.spent_usd, 6))
        return None

    def poll(self, bnow: float) -> None:
        """Officer.poll_loop's body: the changes since the cursor and the nomination check."""
        try:
            changes = self.run(self.poller.poll())
            changes += self.run(self.poller.nominations(now=bnow))
            self.poll_errors = 0
            if changes:
                self.waker.add(changes, bnow)
                self.log_dropped()
        except Exception as e:  # noqa: BLE001 (keep polling; reconnect after a few misses)
            self.poll_errors += 1
            if self.poll_errors in (1, 5) or self.poll_errors % 30 == 0:
                self.log.write("poll.error", error=f"{type(e).__name__}: {e}", misses=self.poll_errors)
            if self.poll_errors >= 3:
                try:
                    info = self.run(self.poller.open())
                    self.log.write("bridge.reconnected", cursor=info["cursor"], bridge_run=info["run"], reset=info["reset"])
                except Exception:  # noqa: BLE001
                    pass

    def log_dropped(self) -> None:
        for change, error in self.waker.take_dropped():
            self.log.write("change.dropped", error=error, change=str(change)[:500])

    def budget_line(self, wake: Wake) -> str:
        return WO.Officer.budget_line(self, wake)  # type: ignore[arg-type]

    def failed(self, now: float, crashed: bool, lane: str) -> str | None:
        return WO.Officer.failed(self, now, crashed, lane)  # type: ignore[arg-type]

    # ---- one wake (Officer.do_wake) --------------------------------------------------------

    def do_wake(self, wake: Wake, now_s: float) -> str | None:
        bnow = self.bnow(now_s)
        escalate = wake.escalate and bnow >= self.lanes["consult"].until
        if wake.escalate and not escalate:
            self.waker.unescalate(wake)  # the stronger model waits after failed consults
        try:
            mine = render_mine(self.run(self.poller.mine()), bnow)
        except Exception:  # noqa: BLE001 (the update goes out without the list)
            mine = ["(could not read your intents: use get_intents with mine)"]
        notes = [] if escalate else list(self.consult_notes)
        text = compose(wake, mine, self.budget_line(wake), notes, bnow, first=wake.reason == "start")
        if escalate:
            text = WO.CONSULT_HEAD.format(why="; ".join(wake.escalate_why), agent=AGENT) + text
        model = self.cfg.strong_model if escalate else self.cfg.model
        effort = self.cfg.strong_effort if escalate else self.cfg.effort
        lane = "consult" if escalate else "routine"
        before = self.sim.now()
        self._wake_ctx = {"n": wake.n, "lane": lane}
        turn, calls = self.run(self.backend.turn(text, model, effort, consult=escalate))
        after = self.sim.now()
        if after != before:
            raise RuntimeError(f"the simulated clock moved during wake {wake.n}: {before} -> {after}")
        self.wall_s += turn.duration_s
        ids, self._wake_proposed = self._wake_proposed, []
        served = sorted({m for c in calls for m in c["served"]})
        rec = {
            "n": wake.n, "t": round(now_s, 3), "clock": [before, after], "reason": wake.reason, "why": wake.why,
            "lane": lane, "escalate_why": wake.escalate_why if escalate else [], "model": model, "effort": effort,
            "served": served, "changes": len(wake.changes), "update": text, "stop": turn.stop, "error": turn.error,
            "text": (turn.text or "")[:TEXT_MAX],
            "tool_calls": [{"name": c.name, "input": c.input, "is_error": c.is_error, "result": c.result}
                           for c in turn.tool_calls],
            "proposed": ids, "api_calls": calls, "tokens": turn.usage, "usd": turn.cost_usd,
            "wall_s": round(turn.duration_s, 3), "context_tokens": turn.context_tokens,
        }
        self.wakes.append(rec)
        if turn.stop == "cost_guard":
            # the guard refused a call: not the model's choice; no further call can pass it
            self.budget_stopped = True
            self.log.write("budget.exhausted", what="cost guard", error=turn.error, n=wake.n)
            return "cost_guard"
        if turn.stop == "error" and self.backend.last_fatal:
            # a request the API refuses as such (or one the SDK could not make): every later
            # one would fail alike, so the officer stops and the episode is not the model's
            self.model_failed = True
            rec["fatal"] = True
            self.log.write("error", error=f"the model call failed for good: {turn.error}", n=wake.n)
            return "model_failed"
        if turn.stop == "error":
            # Officer.do_wake: what it was told comes back for the next wake, an escalation it
            # spent is returned, and the model's lane backs off
            self.waker.give_back(wake, bnow)
            stop = self.failed(bnow, crashed=False, lane=lane)
            if stop:
                self.log.write("error", error=stop)
                return stop.split(":")[0]
            return None
        self.lanes[lane].reset()
        if escalate:
            names = ", ".join(c.name for c in turn.tool_calls) or "none"
            said = ", ".join(f"#{i}" for i in ids) or "none"
            self.consult_notes.append(
                f"While you were not woken, the senior watch officer ({', '.join(served) or model}) handled update "
                f"{wake.n} ({'; '.join(wake.escalate_why)}). Its tool calls: {names}. It proposed: {said}. "
                f"It said: {(turn.text or '').strip()[:800]}")
        else:
            self.consult_notes = []
        return None

    # ---- the model's tools -----------------------------------------------------------------

    def picture(self) -> dict[str, Any]:
        """What the picture shows now, read by the driver as the officer's agent (reads are not
        logged by the bridge): the tracks, the fleet (compact) and the inbox's alerts."""
        out: dict[str, Any] = {"tracks": [], "vehicles": [], "alerts": []}
        for tool, args, key, field_ in (("get_tracks", {}, "tracks", "tracks"),
                                         ("get_fleet", {"compact": True}, "vehicles", "vehicles"),
                                         ("get_inbox", {}, "alerts", "alerts")):
            try:
                is_error, body = self.session._call(tool, args)
            except RuntimeError:
                continue
            if not is_error and isinstance(body, dict):
                out[key] = list(body.get(field_) or [])
        return out

    def execute(self, name: str, args: Any) -> tuple[str, bool]:
        """One tool call of the model's, at the current instant: the bridge's answer as compact
        JSON (what the MCP server's text content holds), up to RESULT_MAX_CHARS."""
        if name not in self.tool_names:
            # what the locked-down CLI answers for a tool outside the Murmur server
            return f"<tool_use_error>Error: No such tool available: {name}</tool_use_error>", True
        if not isinstance(args, dict):
            return json.dumps({"error": "the arguments must be an object"}), True
        picture = self.picture() if name == "propose_intent" else None
        try:
            is_error, body = self.session._call(name, dict(args))
        except RuntimeError as e:  # a protocol error (bad parameters): what an MCP client raises
            return f"MCP error: {e}", True
        text = json.dumps(body, separators=(",", ":"), ensure_ascii=False)
        if len(text) > RESULT_MAX_CHARS:
            text = text[:RESULT_MAX_CHARS] + f"... [{len(text) - RESULT_MAX_CHARS} more characters not shown]"
        if name == "propose_intent":
            self.record_proposal(dict(args), is_error, body, picture or {})
        return text, is_error

    def record_proposal(self, args: dict[str, Any], is_error: bool, body: Any, picture: dict[str, Any]) -> None:
        """A proposal that created an intent (refused ones too, as the playbook's are), as a
        `proposal` line with its play category (`map_proposal`) and what the picture showed."""
        body = body if isinstance(body, dict) else {}
        intent = body.get("intent") if isinstance(body.get("intent"), dict) else {}
        iid = _uid(body.get("id")) if not is_error else _uid(intent.get("id"))
        if iid is None:
            return
        req = {k: v for k, v in args.items() if k != "rationale"}
        m = map_proposal(req, picture, self.looks)
        uids = [u for u in (_uid(x) for x in req.get("uids") or []) if u is not None]
        uid = uids[0] if uids else None
        ctx = self._wake_ctx or {}
        self.lines.append({
            "t": round(self.sim.t, 3), "source": "model", "kind": "proposal", "intent": iid,
            "play": m["play"], "target": m.get("target"), "aim": m.get("aim"), "aim_m": m.get("aim_m"),
            "predicted": m.get("predicted"), "picture": m.get("picture"), "mapping": m.get("rule"),
            "uid": uid, "request": req, "task": req.get("task"),
            "state": body.get("state") if not is_error else intent.get("state"),
            "refused": body.get("error") if is_error else None,
            "tier": ((intent.get("policy") or {}) if isinstance(intent.get("policy"), dict) else {}).get("tier"),
            "rationale": args.get("rationale"), "wake": ctx.get("n"), "lane": ctx.get("lane"),
        })
        if m["play"] in (LOOK, RECENTRE) and uid is not None:
            self.looks[iid] = {"track": m.get("target"), "uid": uid}
        self.proposed.append(iid)
        self._wake_proposed.append(iid)

    # ---- the end ---------------------------------------------------------------------------

    def close(self) -> None:
        close = getattr(self.client, "close", None)
        if close is not None:
            try:
                self.run(close())
            except Exception:  # noqa: BLE001 (closing is best effort)
                pass

    def totals(self) -> dict[str, Any]:
        tok = {k: sum((w.get("tokens") or {}).get(k, 0) for w in self.wakes)
               for k in ("input", "cache_write", "cache_read", "output")}
        stops: dict[str, int] = {}
        for w in self.wakes:
            stops[w["stop"]] = stops.get(w["stop"], 0) + 1
        return {
            "wakes": len(self.wakes), "consults": sum(1 for w in self.wakes if w["lane"] == "consult"),
            "api_calls": sum(len(w["api_calls"]) for w in self.wakes),
            "tool_calls": sum(len(w["tool_calls"]) for w in self.wakes),
            "proposals": len(self.proposed), "tokens": tok,
            "usd": round(self.guard.spent_usd, 6), "wall_s": round(self.wall_s, 3), "stops": stops,
            "errors": stops.get("error", 0), "refusals": stops.get("refusal", 0),
            "budget_stopped": self.budget_stopped, "model_failed": self.model_failed,
            "stopped": self.stopped, "stopped_t": self.stopped_t,
        }

    def describe(self) -> dict[str, Any]:
        return {
            "agent": AGENT, "config": self.cfg.as_dict(),
            "budgets": {k: getattr(self.budgets, k) for k in vars(self.budgets)},
            "poll_s": self.poll_s, "prompt_chars": len(self.system), "tools_chars": self.backend.tools_chars,
            "tools": [t["name"] for t in self.tools], "guard": self.guard.summary(),
            "totals": self.totals(), "wakes": self.wakes,
        }
