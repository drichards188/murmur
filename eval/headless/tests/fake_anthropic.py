"""A fake Anthropic client for the model agent's tests: scripted responses, no network.

`FakeAnthropic(script)` answers `messages.create(**request)` (and `beta.messages.create`) with
what `script(request)` returns: (content blocks, stop reason). Every request is kept
(`requests`: model, effort, sizes) and its usage is made up from its size (a quarter of its
characters as input tokens, the previous request's prefix as a cache read), so the cost path
runs on plausible numbers.
"""

from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any, Callable

SUSPICIOUS = ("unknown", "suspect", "hostile")


def text(s: str) -> SimpleNamespace:
    return SimpleNamespace(type="text", text=s)


def tool_use(name: str, args: dict[str, Any], id: str) -> SimpleNamespace:
    return SimpleNamespace(type="tool_use", name=name, input=args, id=id)


def _plain(o: Any) -> Any:
    return vars(o) if hasattr(o, "__dict__") else str(o)


class FakeMessages:
    def __init__(self, owner: "FakeAnthropic"):
        self.owner = owner

    async def create(self, **req: Any) -> SimpleNamespace:
        return self.owner.answer(req)


class FakeAnthropic:
    def __init__(self, script: Callable[[dict[str, Any]], tuple[list[Any], str]],
                 usage: Callable[[dict[str, Any], int], dict[str, int]] | None = None):
        self.script = script
        self.usage = usage
        self.requests: list[dict[str, Any]] = []
        self.messages = FakeMessages(self)
        self.beta = SimpleNamespace(messages=FakeMessages(self))
        self.closed = False
        self._prefix = 0

    def answer(self, req: dict[str, Any]) -> SimpleNamespace:
        size = {
            "system": len(json.dumps(req.get("system") or "")),
            "tools": len(json.dumps(req.get("tools") or [])),
            "messages": len(json.dumps(req.get("messages") or [], default=_plain)),
        }
        self.requests.append({"model": req.get("model"), "effort": (req.get("output_config") or {}).get("effort"),
                              "max_tokens": req.get("max_tokens"), "cache_control": req.get("cache_control"),
                              "system_breakpoint": [b.get("cache_control") for b in req.get("system") or []
                                                    if isinstance(b, dict)],
                              "n_messages": len(req.get("messages") or []), "chars": size})
        content, stop = self.script(req)
        total = sum(size.values()) // 4
        if self.usage is not None:
            u = self.usage(req, total)
        else:
            read = min(self._prefix, total)
            u = {"input_tokens": 30, "cache_creation_input_tokens": max(0, total - read - 30),
                 "cache_read_input_tokens": read, "output_tokens": 120}
            self._prefix = total
        return SimpleNamespace(content=content, stop_reason=stop, model=req.get("model"),
                               usage=SimpleNamespace(**u), stop_details=None)

    async def close(self) -> None:
        self.closed = True


def last_results(req: dict[str, Any]) -> dict[str, Any]:
    """The tool results of the request's last message, by the name of the tool that was called
    (found from the tool_use ids of the message before it)."""
    msgs = req["messages"]
    last = msgs[-1]
    if not isinstance(last.get("content"), list):
        return {}
    names = {}
    for b in msgs[-2]["content"]:
        if getattr(b, "type", None) == "tool_use":
            names[b.id] = b.name
    out = {}
    for r in last["content"]:
        if r.get("type") == "tool_result":
            try:
                out[names.get(r["tool_use_id"], "?")] = json.loads(r["content"])
            except ValueError:
                out[names.get(r["tool_use_id"], "?")] = {"text": r["content"]}
    return out


class LookScript:
    """Reads the fleet and the tracks; on the first fast contact without AIS that the picture
    shows unknown or suspect, proposes an orbit of PUMA 1 centred on it; otherwise, and after
    that, no action."""

    def __init__(self, radius_m: float = 600.0):
        self.radius_m = radius_m
        self.n = 0
        self.proposed: dict[str, Any] | None = None
        self.answer: dict[str, Any] | None = None

    def id(self) -> str:
        self.n += 1
        return f"toolu_{self.n:04d}"

    def __call__(self, req: dict[str, Any]) -> tuple[list[Any], str]:
        last = req["messages"][-1]
        if isinstance(last.get("content"), str):  # an update
            if self.proposed is not None:
                return [text("No action: my orbit on the fast boat stands.")], "end_turn"
            return [tool_use("get_fleet", {"compact": True}, self.id()), tool_use("get_tracks", {}, self.id())], "tool_use"
        res = last_results(req)
        if "propose_intent" in res:
            self.answer = res["propose_intent"]
            return [text(f"Proposed intent #{self.answer.get('id')}: an orbit of PUMA 1 on the fast boat.")], "end_turn"
        fleet = (res.get("get_fleet") or {}).get("vehicles") or []
        tracks = (res.get("get_tracks") or {}).get("tracks") or []
        puma = next((v for v in fleet if v.get("callsign") == "PUMA 1"), None)
        boat = next((t for t in tracks if t.get("affiliation") in SUSPICIOUS and not (t.get("identity") or {}).get("ais")
                     and (t.get("speed_mps") or 0) > 8 and t.get("predicted")), None)
        if puma is None or boat is None:
            return [text("No action: nothing unidentified closing.")], "end_turn"
        self.proposed = {"task": "orbit", "uids": [puma["uid"]], "center": boat["predicted"], "radius_m": self.radius_m,
                         "rationale": f"{boat['id']} {boat['affiliation']}, {boat['speed_mps']} m/s, no AIS, seen "
                                      f"{boat.get('age_s')} s ago: a sensor orbit to classify it."}
        return [text("A fast contact without AIS: I propose a look."),
                tool_use("propose_intent", self.proposed, self.id())], "tool_use"


def nothing(req: dict[str, Any]) -> tuple[list[Any], str]:
    """Never reads, never proposes."""
    return [text("No action.")], "end_turn"
