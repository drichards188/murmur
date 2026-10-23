"""Scoring v2's leakage scan (eval/SCORING-V2.md, sections 4.4 and 7): every **input** a model
agent receives in an episode (the system prompt, the tool definitions, every update it is woken
with, every tool result) is scanned for distinctive evaluation tokens before it is sent; a hit is
recorded and the episode is a `harness_error` (the officer stops before the input reaches the
model, so a harness error costs nothing more). The model's own text (its text and thinking
blocks, its tool calls' arguments) is scanned as a diagnostic only, never a class, as soon as
each answer arrives; an input hit whose matched text the model itself wrote earlier is its own
words coming back (the bridge echoes a proposal's rationale) and is kept apart as an `echo`.

**The tokens** (`patterns`: the fixed ones, and per episode the `Context`'s name and seeds):

- **family names**: every v1 family (split.toml) and every v2 situation kind
  (families.V2_GENERATORS: the suite and Harbor watch), hyphenated or with underscores
  (`fast-unknown`, `two_at_once`); Harbor watch's title (`harbor watch`); every `v2-` form
  (`v2-decoy`, `v2_fast_unknown`); and any family name followed by a number (`decoy-1000`, a
  situation's name). The one-word families (`decoy`, `quiet`, `conflict`) are ordinary words:
  alone they are not tokens (a tool result may well say "conflict").
- **run names** other than the opaque `ep-<12 hex>` id: a v1 run id (`headless-<situation>-s<seed>`),
  a batch job's name (`<family>-<agent>-s<seed>-r<r>`), any lowercase hyphenated name ending in
  `-s<digits>` (`hw-model-s1`), and the episode's own situation name (`v2-fast-unknown-1000`).
- **seed numbers as part of a name**: the episode's situation seed and episode seed as `-s<seed>`,
  `_s<seed>` or `seed<seed>` (`seed 1000`, `seed=1000`, `seed-1000`). A bare number is a value
  (an altitude, a count), not a name, and a seed the bridge serves as a JSON value
  (`"seed":1000`) is a world value, exempt.
- **scorer field names** (`FIELDS`): the v2 labels', metrics' and record's names and v1's labels'
  and metrics', as identifiers (case as written, `_`-joined compounds included: `pre_served`
  finds `pre_served_share`): `t_need`, `pre_served`, `look_by`, `episode_class` and the rest of
  `FIELDS`. Two of them are ordinary words (`WORD_FIELDS`: `eligible`, `contributed`):
  those are tokens only in a field name's form (a JSON key, `name=`, a `_`-joined compound),
  never as prose.

**Exempt**: world and policy values the bridge serves. Numbers are never tokens; a JSON tool
result is scanned by its structure, every string value and every key, and a key the bridge's
own views use where it puts it (`BRIDGE_KEYS`: an engagement's `engage.eligible`, the operators
who may sign) is exempt. Every other string the bridge serves is scanned: a contact's AIS name,
an order's rationale or an operator's note that named a family would be a leak through the
bridge, which is exactly what the scan is for.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from typing import Any, Iterator

VERSION = "1"
EXCERPT = 40
MAX_KEPT = 50

#: scorer field names (v2: labels.py, metrics.py, record.py, aggregate.py; v1: labels.py,
#: metrics.py): identifiers no bridge view, tool definition or watch prompt uses (checked by
#: the tests against crates/ and agents/watch)
FIELDS = (
    # v2 needs and their outcome (SCORING-V2.md 2.1 to 2.4)
    "t_need", "t_served", "pre_served", "served_share", "served_late", "served_after_stop",
    "contact_need", "fault_need", "fault_needs", "sensing_error", "identity_resolved",
    "identified_in_time", "need_outcomes", "aimed_need", "fault_met", "fault_censored", "met_by",
    "t_met", "t_open", "t_closed", "t_identified", "t_landed",
    # v2 action labels (2.5) that are not ordinary words, and their columns
    "fault_response", "follow_up", "also_fault", "redundant_because", "justified_on_picture",
    "useless_justified", "useless_unjustified", "harmful_dispatched", "useless_dispatched",
    "duplicate_of_none", "fault_actions", "met_fault",
    # v2 metrics and the multiverse (3, 8.5)
    "time_to_serve", "time_margin", "bad_card_share", "flood_share", "n_eff",
    "justify_horizon_s", "aim_fallback_m", "low_reserve_open_pct",
    # v2 record and episode classes (7)
    "episode_class", "harness_error", "budget_stop", "budget_stopped", "model_failed",
    # v1 labels and metrics (eval/headless/labels.py, metrics.py)
    "look_by", "must_look", "needs_look", "look_covered", "time_to_look", "false_looks",
    "first_within_s", "truth_target", "tracker_target", "look_in_progress_cut", "no_look_to_cut",
    "human_cards", "reproposals_after_rejection",
)
#: scorer field names that are ordinary words: tokens only as a field name is written
WORD_FIELDS = ("eligible", "contributed")
#: (parent key, key) of the bridge's own views that coincide with a field name: exempt there
BRIDGE_KEYS = frozenset({("engage", "eligible")})


def families() -> tuple[str, ...]:
    """Every family name, v1 (split.toml) and v2 (the suite and Harbor watch)."""
    from ..families import FAMILIES, SITUATIONS_V2

    return tuple(sorted(set(FAMILIES) | set(SITUATIONS_V2)))


@dataclass(frozen=True)
class Context:
    """What names this episode: its family, situation seed, episode seed and situation name. The
    opaque run id (`ep-...`) is what the bridge serves and is allowed."""

    family: str | None = None
    situation_seed: int | None = None
    seed: int | None = None
    name: str | None = None


def context_of(ep: Any) -> Context:
    e = ep.e
    return Context(family=getattr(e, "family", None), situation_seed=getattr(e, "situation_seed", None),
                   seed=getattr(ep, "seed", None), name=getattr(e, "name", None))


def _joined(name: str) -> str:
    """A hyphenated name, hyphens or underscores between its words."""
    return "[-_]".join(re.escape(w) for w in name.split("-"))


_W = r"(?<![A-Za-z0-9])"  # no letter or digit before (a `_` or `-` may be)
_E = r"(?![A-Za-z0-9])"


def patterns(ctx: Context | None = None) -> list[tuple[str, str, re.Pattern[str]]]:
    """(kind, token, regex) for every token of an episode: the fixed ones and the context's."""
    ctx = ctx or Context()
    fams = families()
    out: list[tuple[str, str, re.Pattern[str]]] = []
    for f in fams:
        if "-" in f:
            out.append(("family", f, re.compile(_W + _joined(f) + _E, re.I)))
    out.append(("family", "harbor watch", re.compile(_W + r"harbor\s+watch" + _E, re.I)))
    alts = "|".join(_joined(f) for f in sorted(fams, key=len, reverse=True))
    out.append(("family", "v2-<family>", re.compile(_W + r"v2[-_](?:" + alts + ")" + _E, re.I)))
    out.append(("family", "<family>-<number>", re.compile(_W + r"(?:" + alts + r")[-_:]\d+" + _E, re.I)))
    out.append(("run", "headless-<situation>-s<seed>", re.compile(_W + r"headless-[a-z0-9-]+-s\d+" + _E)))
    out.append(("run", "<name>-s<seed>[-r<r>]",
                re.compile(r"(?<![A-Za-z0-9_-])[a-z][a-z0-9]*(?:-[a-z0-9]+)*-s\d+(?:-r\d+)?" + _E)))
    if ctx.name and re.search(r"[-_]", ctx.name):
        out.append(("run", ctx.name, re.compile(_W + re.escape(ctx.name) + _E, re.I)))
    for seed in sorted({s for s in (ctx.situation_seed, ctx.seed) if s is not None}):
        out.append(("seed", f"s{seed}", re.compile(r"[-_]s" + str(int(seed)) + r"(?![0-9])", re.I)))
        out.append(("seed", f"seed{seed}", re.compile(_W + r"seed[-_ :=#]?" + str(int(seed)) + r"(?![0-9])", re.I)))
    for f in FIELDS:
        out.append(("field", f, re.compile(_W + re.escape(f) + _E)))
    for w in WORD_FIELDS:
        out.append(("field", w, re.compile(
            r'"' + w + r'"\s*:|' + _W + w + r"\s*=(?!=)|[A-Za-z0-9]_" + w + _E + "|" + _W + w + r"_[A-Za-z0-9]")))
    return out


def tokens_sha256() -> str:
    """The fixed token set's hash (the context's names and seeds come on top), for the record."""
    return hashlib.sha256(json.dumps([[k, t, p.pattern] for k, t, p in patterns()]).encode()).hexdigest()


def _strings(obj: Any, path: str = "", parent: str | None = None) -> Iterator[tuple[str, str, bool]]:
    """(path, text, is_key) of every key and string value of a JSON value; a key the bridge
    serves where it puts it (`BRIDGE_KEYS`) is left out."""
    if isinstance(obj, dict):
        for k, v in obj.items():
            k = str(k)
            p = f"{path}.{k}" if path else k
            if (parent, k) not in BRIDGE_KEYS:
                yield p, k, True
            yield from _strings(v, p, k)
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            yield from _strings(v, f"{path}[{i}]", parent)
    elif isinstance(obj, str):
        yield path, obj, False


class Scanner:
    """One episode's scan: `inputs` (what the model receives: hits make a harness error) and
    `diagnose` (the model's own text: diagnostics only)."""

    def __init__(self, ctx: Context | None = None):
        self.ctx = ctx or Context()
        self.patterns = patterns(self.ctx)
        self.n_hits = 0
        self.hits: list[dict[str, Any]] = []
        self.n_diagnostic = 0
        self.diagnostic: list[dict[str, Any]] = []
        self.n_echoes = 0
        self.echoes: list[dict[str, Any]] = []
        self._said: list[str] = []  # everything the model wrote, lowercased
        self.inputs = 0
        self.chars = 0
        self.by_source: dict[str, int] = {}

    def _find(self, text: str, is_key: bool = False) -> list[tuple[str, str, int, int]]:
        # (kind, token, start, end) of the first match of each pattern
        found = []
        for kind, token, rx in self.patterns:
            if is_key and token in WORD_FIELDS:
                if text == token or rx.search(text):  # a key is a field name's own form
                    found.append((kind, token, 0, len(text)))
                continue
            m = rx.search(text)
            if m:
                found.append((kind, token, m.start(), m.end()))
        return found

    def _matches(self, text: str, structured: bool) -> list[dict[str, Any]]:
        if structured:
            try:
                obj = json.loads(text)
            except ValueError:
                obj = None
            if isinstance(obj, (dict, list)):
                out = []
                for path, s, is_key in _strings(obj):
                    for kind, token, a, b in self._find(s, is_key):
                        out.append({"kind": kind, "token": token, "match": s[a:b], "at": path, "key": is_key,
                                    "excerpt": s[max(0, a - EXCERPT):b + EXCERPT]})
                return out
        return [{"kind": kind, "token": token, "match": text[a:b], "at": a,
                 "excerpt": text[max(0, a - EXCERPT):b + EXCERPT]}
                for kind, token, a, b in self._find(text)]

    def _echo(self, match: str) -> bool:
        m = match.lower()
        return any(m in s for s in self._said)

    def scan(self, text: Any, source: str, structured: bool = False, **where: Any) -> list[dict[str, Any]]:
        """An input the model receives: every hit recorded (the first `MAX_KEPT` kept whole). A
        hit whose matched text the model itself wrote earlier in the episode (`said`) is the
        model's own words coming back (the bridge echoes a rationale, the update lists the
        model's own proposals): kept apart as an echo, never a harness error. Such a token
        reached the model's output either from an earlier input, which this scan already
        flagged, or from the model itself."""
        text = text if isinstance(text, str) else json.dumps(text, ensure_ascii=False)
        self.inputs += 1
        self.chars += len(text)
        kind = source.split(":")[0]
        self.by_source[kind] = self.by_source.get(kind, 0) + 1
        found = [{"source": source, **where, **m} for m in self._matches(text, structured)]
        echoes = [h for h in found if self._echo(h["match"])]
        hits = [h for h in found if not self._echo(h["match"])]
        self.n_echoes += len(echoes)
        self.echoes += echoes[:max(0, MAX_KEPT - len(self.echoes))]
        self.n_hits += len(hits)
        self.hits += hits[:max(0, MAX_KEPT - len(self.hits))]
        return hits

    def said(self, text: Any, source: str, **where: Any) -> list[dict[str, Any]]:
        """The model's own words (a text or thinking block, a tool call's arguments), as soon as
        its answer arrives: kept to recognize their echoes, and scanned as a diagnostic only."""
        if not text:
            return []
        text = text if isinstance(text, str) else json.dumps(text, ensure_ascii=False)
        self._said.append(text.lower())
        hits = [{"source": source, **where, **m} for m in self._matches(text, False)]
        self.n_diagnostic += len(hits)
        self.diagnostic += hits[:max(0, MAX_KEPT - len(self.diagnostic))]
        return hits

    @property
    def leaked(self) -> bool:
        return self.n_hits > 0

    def record(self) -> dict[str, Any]:
        return {
            "version": VERSION, "tokens_sha256": tokens_sha256(),
            "context": {"family": self.ctx.family, "situation_seed": self.ctx.situation_seed, "seed": self.ctx.seed,
                        "name": self.ctx.name},
            "inputs": self.inputs, "chars": self.chars, "by_source": dict(sorted(self.by_source.items())),
            "n_hits": self.n_hits, "hits": self.hits, "n_echoes": self.n_echoes, "echoes": self.echoes,
            "n_diagnostic": self.n_diagnostic, "diagnostic": self.diagnostic,
        }


def scan_texts(texts: dict[str, str], ctx: Context | None = None) -> list[dict[str, Any]]:
    """Scan named inputs at once (the tests, a check of a prompt or tool list): every hit."""
    s = Scanner(ctx)
    for source, text in texts.items():
        s.scan(text, source, structured=source.startswith(("tool_definition", "tool_result")))
    return s.hits
