"""A scripted agent for the engagement chain and the space segment (docs/HEADLESS.md, section
16): `Options.agent = "scripted"`, agent id `agent-scripted` (the situations' scripted
proposer), through the MCP tools only, as a model or the playbook would use them.

**A script with stated rules, not an AI and not a judgment.** Every rationale it writes begins
"scripted agent (not a person, not a model)". It exists so the paths no agent under evaluation
takes yet can be run end to end headless, and so families can put them in front of one.

What it does (`ScriptedPlan`, a batch's `Options.scripted`), every `period_s` from the AI's
start:

- **engage** (on by default): for each track the picture shows declared hostile (`get_tracks`:
  affiliation hostile), unless a nomination on it stands (`get_nominations`, whoever made it),
  it reads the nomination form's dry run (`get_nomination_options` for that track, at most once
  a second as the tool allows) and nominates the first armed platform that passes every check
  (`propose_intent` task engage), at most `max_nominations` times per track (a nomination that
  expired, was voided or was declined may be made again). When none passes, the log names each
  platform's failing checks and what blocks its keep-outs, as the form's explanation does.
- **collect** (`collect = {...}`): at `at_s`, the imaging passes over the area
  (`get_satellite_passes`), then the ranked tasking options (`get_satellite_tasking_options`),
  and a collect proposed with the first option whose collect time is at least `min_lead_s`
  ahead (`propose_intent` task collect with `chosen`).
- **cover** (`cover = {...}`): at `at_s`, a cover proposed over the area and window
  (`propose_intent` task cover).

Everything it read and proposed goes to `lines` (the record's `agent_log`).
"""

from __future__ import annotations

import datetime as dt
from dataclasses import dataclass, field
from typing import Any

from eval.situation import STORYLINE_AGENT

from .sim import HeadlessSession

LABEL = "scripted agent (not a person, not a model)"
#: an engagement nomination that stands (eval/engage.py STANDING)
STANDING = ("proposed", "checked", "pending_approval", "approved", "queued", "dispatched", "effected", "done")
ENGAGE_DEFAULTS: dict[str, Any] = {"effect_radius_m": 75.0, "window_s": 600, "fallback": "return", "uids": None,
                                   "max_nominations": 3}
COLLECT_DEFAULTS: dict[str, Any] = {"at_s": 60.0, "area": {"kind": "circle", "center": [-122.4783, 37.8199], "radius_km": 2.0},
                                    "capabilities": ["optical", "sar"], "window_s": 4 * 3600, "rank": "earliest_product",
                                    "min_lead_s": 600.0}
COVER_DEFAULTS: dict[str, Any] = {"at_s": 60.0, "area": {"kind": "circle", "center": [-122.4783, 37.8199], "radius_km": 5.0},
                                  "service": {"type": "gnss"}, "window_s": 600, "min_level": None}


@dataclass
class ScriptedPlan:
    engage: dict[str, Any] | None = field(default_factory=dict)
    collect: dict[str, Any] | None = None
    cover: dict[str, Any] | None = None
    period_s: float = 5.0

    @classmethod
    def from_spec(cls, spec: dict[str, Any] | None) -> "ScriptedPlan":
        spec = dict(spec or {})
        known = {"engage", "collect", "cover", "period_s"}
        bad = sorted(set(spec) - known)
        if bad:
            raise ValueError(f"unknown scripted plan keys {bad}; known: {', '.join(sorted(known))}")
        p = cls()
        if "engage" in spec:
            p.engage = None if spec["engage"] is False or spec["engage"] is None else dict(spec["engage"])
        for k in ("collect", "cover"):
            if spec.get(k) is not None and spec[k] is not False:
                setattr(p, k, dict(spec[k]))
        if "period_s" in spec:
            p.period_s = float(spec["period_s"])
        for name, given, defaults in (("engage", p.engage, ENGAGE_DEFAULTS), ("collect", p.collect, COLLECT_DEFAULTS),
                                      ("cover", p.cover, COVER_DEFAULTS)):
            if given is not None:
                unknown = sorted(set(given) - set(defaults))
                if unknown:
                    raise ValueError(f"scripted {name}: unknown keys {unknown}; known: {', '.join(sorted(defaults))}")
        return p

    def as_dict(self) -> dict[str, Any]:
        return {"engage": None if self.engage is None else {**ENGAGE_DEFAULTS, **self.engage},
                "collect": None if self.collect is None else {**COLLECT_DEFAULTS, **self.collect},
                "cover": None if self.cover is None else {**COVER_DEFAULTS, **self.cover},
                "period_s": self.period_s}


def iso(unix_s: float) -> str:
    return dt.datetime.fromtimestamp(unix_s, dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def hostile_tracks(tracks: list[dict[str, Any]]) -> list[str]:
    """Track ids the picture shows declared hostile, in id order (pure, for tests)."""
    return sorted(str(t["id"]) for t in tracks if t.get("affiliation") == "hostile" and t.get("id"))


def standing_on(nominations: list[dict[str, Any]], track: str) -> dict[str, Any] | None:
    """The newest nomination on `track` that stands, whoever made it (pure, for tests)."""
    def tr(n: dict[str, Any]) -> Any:
        return (n.get("params") or {}).get("track") or n.get("track")
    found = [n for n in nominations if tr(n) == track and n.get("state") in STANDING]
    return max(found, key=lambda n: int(n.get("id") or 0)) if found else None


def first_passing(options: list[dict[str, Any]]) -> dict[str, Any] | None:
    """The first armed platform the dry run lists as passing every check (listed by uid)."""
    return next((o for o in options if o.get("passed")), None)


def failing_rules(options: list[dict[str, Any]]) -> dict[str, list[str]]:
    """What keeps each platform of the dry run from passing: its checks that do not pass, by
    rule (pure, for tests)."""
    return {str(o.get("callsign") or o.get("uid")): sorted({str(c.get("rule")) for c in o.get("checks") or []
                                                            if c.get("outcome") != "pass"})
            for o in options}


def keep_out_blockers(options: list[dict[str, Any]]) -> dict[str, list[str]]:
    """What blocks each platform's keep-outs, as the form's explanation names it
    (`keep_outs.items` that block: `track:T-0003`, `lost_contact:T-0002`, `vehicle:2`)."""
    return {str(o.get("callsign") or o.get("uid")): [f"{it.get('subject')}:{it.get('id')}"
                                                     for it in (o.get("keep_outs") or {}).get("items") or []
                                                     if it.get("blocks")]
            for o in options}


def choose_option(options: list[dict[str, Any]], now_ms: int, min_lead_s: float) -> dict[str, Any] | None:
    """The first ranked option whose collect time is at least `min_lead_s` after now (scenario
    milliseconds), as the answer ranks them (pure, for tests)."""
    for o in options:
        t = o.get("collect_time")
        ms = t if isinstance(t, (int, float)) else _parse_ms(t)
        if ms is not None and ms >= now_ms + min_lead_s * 1000:
            return o
    return None


def _parse_ms(t: Any) -> int | None:
    if not isinstance(t, str):
        return None
    try:
        return int(dt.datetime.fromisoformat(t.replace("Z", "+00:00")).timestamp() * 1000)
    except ValueError:
        return None


class ScriptedAgent:
    """The scripted agent as the episode's `agent` process."""

    def __init__(self, ep: Any, plan: ScriptedPlan):
        self.ep = ep
        self.plan = plan
        self.session = HeadlessSession(ep.sim, STORYLINE_AGENT, "0.1")
        self.lines: list[dict[str, Any]] = []
        self.nominations: dict[str, int] = {}  # track -> nominations made
        self._turn = 0
        self.collect_done = plan.collect is None
        self.cover_done = plan.cover is None
        self.detail: dict[str, Any] = {"plan": plan.as_dict()}

    def describe(self) -> dict[str, Any]:
        return self.detail

    def write(self, kind: str, **fields: Any) -> None:
        self.lines.append({"t": round(self.ep.sim.t, 3), "source": "scripted", "kind": kind, **fields})

    def call(self, tool: str, **args: Any) -> Any:
        return self.ep.run_async(self.session.call(tool, **args))

    def propose(self, rationale: str, **req: Any) -> dict[str, Any]:
        return self.ep.run_async(self.session.propose(f"{LABEL}: {rationale}", **req))

    def scenario_now(self) -> float:
        return float(self.ep.sim_epoch) + float(self.ep.sim.t)

    def wake(self, now: float) -> float | None:
        for what, due, done, act in (("collect", self.plan.collect, self.collect_done, self._collect),
                                     ("cover", self.plan.cover, self.cover_done, self._cover)):
            if not done and due is not None and now >= float({**(COLLECT_DEFAULTS if what == "collect" else COVER_DEFAULTS),
                                                              **due}["at_s"]) - 1e-9:
                try:
                    act(now)
                except Exception as e:  # noqa: BLE001 (recorded; the episode goes on)
                    self.write(f"{what}.error", error=f"{type(e).__name__}: {e}")
                setattr(self, f"{what}_done", True)
        if self.plan.engage is not None:
            try:
                self._engage(now)
            except Exception as e:  # noqa: BLE001
                self.write("engage.error", error=f"{type(e).__name__}: {e}")
        return now + self.plan.period_s

    # ---- engage ----------------------------------------------------------------------------

    def _engage(self, now: float) -> None:
        cfg = {**ENGAGE_DEFAULTS, **(self.plan.engage or {})}
        tracks = self.call("get_tracks").get("tracks") or []
        hostile = hostile_tracks(tracks)
        if not hostile:
            return
        noms = self.call("get_nominations").get("nominations") or []
        # one dry run a wake (the tool allows one a second), the tracks taken in turn
        k = self._turn % len(hostile)
        self._turn += 1
        for track in hostile[k:] + hostile[:k]:
            there = standing_on(noms, track)
            if there is not None or self.nominations.get(track, 0) >= int(cfg["max_nominations"]):
                continue
            # the whole dry run (each platform's checks), so the log says what blocks
            args: dict[str, Any] = {"track_id": track, "effect_radius_m": float(cfg["effect_radius_m"]),
                                    "window_s": int(cfg["window_s"]), "fallback": cfg["fallback"]}
            if cfg.get("uids"):
                args["uids"] = [int(u) for u in cfg["uids"]]
            try:
                form = self.call("get_nomination_options", **args)
            except RuntimeError as e:
                self.write("engage.options_error", track=track, error=str(e)[:300])
                return
            opts = form.get("options") or []
            pick = first_passing(opts)
            if pick is None:
                self.write("engage.none_passes", track=track, options=len(opts), failing=failing_rules(opts),
                           blocking=keep_out_blockers(opts))
                return
            uid = int(pick["uid"])
            rationale = (f"{track} is declared hostile and no nomination on it stands; "
                         f"{pick.get('callsign') or uid} passes every nomination check now (the form's dry run): "
                         f"effect radius {float(cfg['effect_radius_m']):g} m, window {int(cfg['window_s'])} s, "
                         f"fallback {cfg['fallback']}")
            out = self.propose(rationale, task="engage", uids=[uid], track_id=track,
                               effect_radius_m=float(cfg["effect_radius_m"]), window_s=int(cfg["window_s"]),
                               fallback=cfg["fallback"])
            self.nominations[track] = self.nominations.get(track, 0) + 1
            self.write("engage.nominated", track=track, uid=uid, callsign=pick.get("callsign"),
                       intent=out.get("id"), state=out.get("state"), refused=out.get("refused"))
            return  # one nomination a wake

    # ---- space -----------------------------------------------------------------------------

    def _collect(self, now: float) -> None:
        cfg = {**COLLECT_DEFAULTS, **(self.plan.collect or {})}
        t0 = self.scenario_now()
        frm, to = iso(t0), iso(t0 + float(cfg["window_s"]))
        area = cfg["area"]
        passes = self.call("get_satellite_passes", area=_area_text(area), **{"from": frm, "to": to})
        windows = passes.get("windows") or []
        self.write("collect.passes", windows=len(windows), time_shifted=passes.get("time_shifted"))
        task = self.call("get_satellite_tasking_options", area=area, capabilities=list(cfg["capabilities"]),
                         rank=cfg["rank"], **{"from": frm, "to": to})
        options = task.get("options") or []
        pick = choose_option(options, int(t0 * 1000), float(cfg["min_lead_s"]))
        self.detail["collect_options"] = [
            {k: o.get(k) for k in ("norad_id", "satellite", "class_id", "collect_time", "product_time", "resolution_m")}
            for o in options[:5]]
        if pick is None:
            self.write("collect.no_option", options=len(options), exclusions=task.get("exclusions_total"))
            return
        cap = "any" if len(cfg["capabilities"]) != 1 else cfg["capabilities"][0]
        chosen = {"norad_id": pick["norad_id"], "collect_time": pick["collect_time"]}
        out = self.propose(f"imagery of the area from the first ranked option at least {float(cfg['min_lead_s']):g} s ahead "
                           f"({pick.get('satellite') or pick['norad_id']})",
                           task="collect", area=area, window={"from": frm, "to": to}, capability=cap, chosen=chosen)
        self.write("collect.proposed", intent=out.get("id"), state=out.get("state"), refused=out.get("refused"),
                   chosen=chosen, satellite=pick.get("satellite"), options=len(options))

    def _cover(self, now: float) -> None:
        cfg = {**COVER_DEFAULTS, **(self.plan.cover or {})}
        t0 = self.scenario_now()
        req: dict[str, Any] = {"task": "cover", "area": cfg["area"],
                               "window": {"from": iso(t0), "to": iso(t0 + float(cfg["window_s"]))},
                               "service": cfg["service"]}
        if cfg.get("min_level") is not None:
            req["min_level"] = float(cfg["min_level"])
        out = self.propose(f"is {cfg['service'].get('type')} service good enough over the area for the next "
                           f"{float(cfg['window_s']):g} s", **req)
        self.write("cover.proposed", intent=out.get("id"), state=out.get("state"), refused=out.get("refused"),
                   service=cfg["service"])


def _area_text(area: Any) -> str | None:
    """`get_satellite_passes` takes a named area or a box "w,s,e,n": a circle as its box."""
    if isinstance(area, str):
        return area
    if isinstance(area, dict) and area.get("kind") == "circle":
        import math

        lon, lat = area["center"]
        r = float(area["radius_km"])
        dlat = r / 111.32
        dlon = r / (111.32 * max(0.2, math.cos(math.radians(lat))))
        return f"{lon - dlon:.5f},{lat - dlat:.5f},{lon + dlon:.5f},{lat + dlat:.5f}"
    if isinstance(area, dict) and "bbox" in area:
        return ",".join(f"{x:.5f}" for x in area["bbox"])
    if isinstance(area, dict) and area.get("kind") == "polygon":
        ring = area["ring"]
        return f"{min(p[0] for p in ring):.5f},{min(p[1] for p in ring):.5f},{max(p[0] for p in ring):.5f},{max(p[1] for p in ring):.5f}"
    return None
