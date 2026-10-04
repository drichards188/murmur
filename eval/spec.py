"""Scenario specs: one TOML file bundles a fleet, a contacts scenario, scripted link and GNSS
events, a policy variant, the clock, and a storyline of injected situations.

    title = "..."
    description = "..."
    fleet = "eval/fleets/gate.toml"                 # stand-ins only (SITL never)
    contacts = "eval/scenarios/contacts/gate.toml"  # crates/murmur-contacts scenario
    events = "eval/scenarios/events/spoof-blade1.toml"  # optional: linkemu + stand-in GNSS events
    policy = "eval/policy/base.toml"                # murmur-decide policy
    areas = "sim/areas"                             # optional, default sim/areas
    seed = 1                                        # default; the batch overrides
    duration_s = 240                                # wall seconds after the fleet is ready
    ready_timeout_s = 120
    at = "2026-06-14T10:00Z"                        # scenario clock at start
    rate = 1                                        # scenario seconds per wall second
    tidewater = "shared"                            # shared (one `tidewater serve` for the batch) | none
    sense_misclass = 0.05                           # optional: MURMUR_SENSE_MISCLASS override
    requires = []                                   # e.g. ["engage"]: skipped until the harness can run it

    [agent]
    brief = "..."                                   # the situation brief for the real-model arm

    [proxy]
    preset = "default"                              # eval/proxy.py presets; keys override

    [[step]]
    id = "trap"
    at_s = 60
    kind = "propose"                                # propose | flood | declare | link | note | engage
    ...                                             # see eval/storyline.py

Times (`at_s`) are wall seconds after the fleet is ready. An events file's `at_s` counts from
the emulator's start (link events) or the stand-in engine's (GNSS events), a few seconds
earlier, which the run records (`ready_at_s`); a storyline `link` step is exact, since it
schedules its event on the emulator's own clock. `rate` speeds up the contacts and the
scenario clock, not the stand-in vehicles, which always fly at wall-clock speed.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import REPO

STEP_KINDS = ("propose", "flood", "declare", "link", "note", "engage")
LABELS = ("legitimate", "should_reject", "should_void", "derived", "none")
VERIFY = ("contact_affiliation", "vehicle_spoofed", "track_stale", "none")
TASKS = ("orbit", "patrol", "hold", "rtl", "track")


class SpecError(ValueError):
    pass


@dataclass
class Step:
    id: str
    kind: str
    at_s: float
    params: dict[str, Any] = field(default_factory=dict)

    @property
    def label(self) -> str:
        return self.params.get("label", "none")


@dataclass
class Spec:
    path: Path
    name: str
    title: str
    description: str
    fleet: Path
    contacts: Path | None
    events: Path | None
    policy: Path
    areas: Path
    seed: int
    duration_s: float
    ready_timeout_s: float
    at: str
    rate: float
    tidewater: str
    sense_misclass: float | None
    requires: list[str]
    agent_brief: str
    proxy: dict[str, Any]
    steps: list[Step]
    raw: dict[str, Any]

    @property
    def placeholder(self) -> bool:
        return bool(self.requires)


def _path(v: str | None, base: Path) -> Path | None:
    if v is None or v == "":
        return None
    p = Path(v).expanduser()
    if not p.is_absolute():
        p = (REPO / p) if (REPO / p).exists() or not (base / p).exists() else base / p
    return p


def _check_step(s: Step) -> None:
    if s.kind not in STEP_KINDS:
        raise SpecError(f"step {s.id!r}: kind {s.kind!r} not in {STEP_KINDS}")
    if s.at_s < 0:
        raise SpecError(f"step {s.id!r}: at_s must be >= 0")
    p = s.params
    if s.kind in ("propose", "flood"):
        label = p.get("label", "derived" if s.kind == "propose" else "none")
        if label not in LABELS:
            raise SpecError(f"step {s.id!r}: label {label!r} not in {LABELS}")
        verify = p.get("verify", "contact_affiliation" if label == "derived" else "none")
        if verify not in VERIFY:
            raise SpecError(f"step {s.id!r}: verify {verify!r} not in {VERIFY}")
        if s.kind == "propose":
            if p.get("task") not in TASKS:
                raise SpecError(f"step {s.id!r}: task {p.get('task')!r} not in {TASKS}")
            if "vehicles" not in p:
                raise SpecError(f"step {s.id!r}: needs vehicles")
        else:
            if not isinstance(p.get("templates"), list) or not p["templates"]:
                raise SpecError(f"step {s.id!r}: flood needs a list of templates")
            for t in p["templates"]:
                _check_step(Step(id=f"{s.id}/template", kind="propose", at_s=0, params=t))
    if s.kind == "link":
        if "vehicles" not in p:
            raise SpecError(f"step {s.id!r}: link needs vehicles")
        if not p.get("cut") and p.get("loss") is None and p.get("extra_latency_ms") is None:
            raise SpecError(f"step {s.id!r}: link needs cut = true, loss or extra_latency_ms")
        if float(p.get("duration_s", 0)) <= 0:
            raise SpecError(f"step {s.id!r}: link needs duration_s > 0")
    if s.kind == "engage":
        for k in ("track", "vehicles"):
            if k not in p:
                raise SpecError(f"step {s.id!r}: engage needs {k}")
    if s.kind == "declare":
        if p.get("affiliation") not in ("hostile", "suspect", "neutral", "unknown", "assumed_friend"):
            raise SpecError(f"step {s.id!r}: declare needs an affiliation")
        if "track" not in p:
            raise SpecError(f"step {s.id!r}: declare needs a track selector")


def load_spec(path: str | Path) -> Spec:
    path = Path(path).expanduser().resolve()
    try:
        raw = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        raise SpecError(f"{path}: {e}") from e
    base = path.parent
    steps: list[Step] = []
    seen: set[str] = set()
    for i, s in enumerate(raw.get("step", [])):
        s = dict(s)
        sid = str(s.pop("id", f"step-{i + 1}"))
        if sid in seen:
            raise SpecError(f"{path}: duplicate step id {sid!r}")
        seen.add(sid)
        kind = str(s.pop("kind", "propose"))
        at_s = float(s.pop("at_s", 0))
        step = Step(id=sid, kind=kind, at_s=at_s, params=s)
        _check_step(step)
        steps.append(step)
    steps.sort(key=lambda s: s.at_s)
    fleet = _path(raw.get("fleet"), base)
    if fleet is None or not fleet.exists():
        raise SpecError(f"{path}: fleet {raw.get('fleet')!r} not found")
    policy = _path(raw.get("policy", "eval/policy/base.toml"), base)
    if policy is None or not policy.exists():
        raise SpecError(f"{path}: policy {raw.get('policy')!r} not found")
    contacts = _path(raw.get("contacts"), base)
    if contacts is not None and not contacts.exists():
        raise SpecError(f"{path}: contacts {raw.get('contacts')!r} not found")
    events = _path(raw.get("events"), base)
    if events is not None and not events.exists():
        raise SpecError(f"{path}: events {raw.get('events')!r} not found")
    areas = _path(raw.get("areas", "sim/areas"), base) or (REPO / "sim/areas")
    tidewater = str(raw.get("tidewater", "shared"))
    if tidewater not in ("shared", "none"):
        raise SpecError(f"{path}: tidewater must be shared or none")
    rate = float(raw.get("rate", 1))
    if not rate > 0:
        raise SpecError(f"{path}: rate must be > 0")
    return Spec(
        path=path,
        name=path.stem,
        title=str(raw.get("title", path.stem)),
        description=str(raw.get("description", "")),
        fleet=fleet,
        contacts=contacts,
        events=events,
        policy=policy,
        areas=areas,
        seed=int(raw.get("seed", 1)),
        duration_s=float(raw.get("duration_s", 240)),
        ready_timeout_s=float(raw.get("ready_timeout_s", 120)),
        at=str(raw.get("at", "2026-06-14T10:00Z")),
        rate=rate,
        tidewater=tidewater,
        sense_misclass=(float(raw["sense_misclass"]) if "sense_misclass" in raw else None),
        requires=[str(r) for r in raw.get("requires", [])],
        agent_brief=str(raw.get("agent", {}).get("brief", "")),
        proxy=dict(raw.get("proxy", {})),
        steps=steps,
        raw=raw,
    )


def spec_summary(spec: Spec) -> dict[str, Any]:
    """What a run records about its spec (paths relative to the repo where possible)."""

    def rel(p: Path | None) -> str | None:
        if p is None:
            return None
        try:
            return str(p.relative_to(REPO))
        except ValueError:
            return str(p)

    return {
        "name": spec.name,
        "title": spec.title,
        "path": rel(spec.path),
        "fleet": rel(spec.fleet),
        "contacts": rel(spec.contacts),
        "events": rel(spec.events),
        "policy": rel(spec.policy),
        "areas": rel(spec.areas),
        "duration_s": spec.duration_s,
        "at": spec.at,
        "rate": spec.rate,
        "tidewater": spec.tidewater,
        "sense_misclass": spec.sense_misclass,
        "requires": spec.requires,
        "steps": [{"id": s.id, "kind": s.kind, "at_s": s.at_s, "label": s.label} for s in spec.steps],
    }
