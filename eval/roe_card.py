"""The scenario's ROE card (docs/SCENARIO-SETTINGS.md section 3, "The brief is the scenario's ROE
card"): the text an agent is briefed with, rendered from the run's settings
(eval/headless/v3/settings.py) and its policy. Since stage S6 the v3 families' brief is the card
(eval/headless/v3/families.py `briefing`) and a situation's brief carries it after its own text
(eval/situation.py `brief_text`), each from rules 33; a scenario whose policy sets no circle (a
situation outside the v3 watches) is carded without one, its brief saying where to watch.

- Under decision rules below 33, or with v0's settings, the card is the v0 mission text
  (eval/headless/v3/mission.py `mission_text`) exactly, and its sha256 must be the one the frozen
  v0 file records for the world (`sim/policy-v0-watch.toml`): old briefs stay what they were.
- From rules 33 it states the circle (its place, its centre as [lon, lat], its radius); each
  domain's deadline, level and sensors; the bar, and that a size class is not an identification;
  that an identify order runs until the bar is met or its time on station is over; that no
  contact is called neutral by its classification before it is identified (or, where the
  scenario lets the tracker, that such a call is not an identification); the approach horizon and
  the rule for a contact shown without velocity; the reserve as a percentage left after a task of
  the policy's length; the engagement bar, level and signers; and each loosened key with its
  reason.

The policy is the run's merged policy as `run.started` logs it or `murmur-policy show` prints it
(`rules.reserve_pct`, `rules.task_duration_s`, `rules.engage.min_identity_confidence` and, from
S2, `min_identity_level`, and the engagement tier's `approvers`); `reserve_pct` and
`task_duration_s` may be given instead. The engagement level and the signers default to the
floor's (section 3: the level `identify`, two approvers, never the proposer), which a policy may
only make stricter.
"""

from __future__ import annotations

import hashlib
from typing import Any

from eval.headless.v3.mission import mission_text
from eval.headless.v3.settings import Settings, frozen

#: the decision rules from which the card is rendered from the settings (before it, v0's text)
CARD_SINCE = 33
#: the floor's engagement identity level and number of signers (section 3), when the policy
#: names none
FLOOR_ENGAGE_LEVEL = "identify"
FLOOR_SIGNERS = 2

DOMAIN_WORDS = {"surface": "on the surface", "air": "in the air", "ground": "on the ground",
                "subsurface": "under the water"}
LEVEL_WORDS = {"identify": "its kind (the identification level)",
               "recognise": "its class (the recognition level: submerged, or a person in the water) or its kind",
               "detect": "its size class (the detection level)"}
SENSOR_WORDS = {"eo_ir": "EO/IR", "ais": "its own true AIS", "sonar": "sonar", "radar": "radar", "esm": "ESM"}
NUMBER_WORDS = {1: "one", 2: "two", 3: "three", 4: "four"}


def _v0_text(world: str) -> str:
    text = mission_text(world)
    want = (frozen()["mission_sha256"].get(world) or {}).get("current")
    got = hashlib.sha256(text.encode()).hexdigest()
    if want is None or got != want:
        raise ValueError(f"the v0 mission text for {world!r} hashes {got}, not the frozen {want}: v0's brief "
                         "changed (sim/policy-v0-watch.toml)")
    return text


def _or(items: list[str]) -> str:
    return items[0] if len(items) == 1 else ", ".join(items[:-1]) + " or " + items[-1]


def _seconds(s: float) -> str:
    return f"{s:g} seconds"


def _policy_value(policy: dict[str, Any] | None, path: tuple[str, ...]) -> Any:
    v: Any = policy or {}
    for k in path:
        if not isinstance(v, dict) or k not in v:
            return None
        v = v[k]
    return v


def _signers(policy: dict[str, Any] | None) -> tuple[int, bool]:
    """(approvers, proposer may approve) of the tier that routes an engagement, else the floor's."""
    for t in (policy or {}).get("tier") or []:
        if "engage" in ((t.get("when") or {}).get("kind") or []):
            return int(t.get("approvers") or FLOOR_SIGNERS), bool(t.get("proposer_may_approve", False))
    return FLOOR_SIGNERS, False


def render(settings: Settings, rules_version: int, world: str, policy: dict[str, Any] | None = None, *,
           reserve_pct: float | None = None, task_duration_s: float | None = None) -> str:
    """The card a run in `world` under `rules_version` is briefed with."""
    if rules_version < CARD_SINCE or settings.is_v0:
        return _v0_text(world)
    reserve = reserve_pct if reserve_pct is not None else _policy_value(policy, ("rules", "reserve_pct"))
    task_s = task_duration_s if task_duration_s is not None else _policy_value(policy, ("rules", "task_duration_s"))
    engage_bar = _policy_value(policy, ("rules", "engage", "min_identity_confidence"))
    missing = [k for k, v in (("rules.reserve_pct", reserve), ("rules.task_duration_s", task_s),
                              ("rules.engage.min_identity_confidence", engage_bar)) if v is None]
    if missing:
        raise ValueError(f"the card needs the policy's {', '.join(missing)}")
    engage_level = _policy_value(policy, ("rules", "engage", "min_identity_level")) or FLOOR_ENGAGE_LEVEL
    approvers, proposer_may = _signers(policy)
    a, s = settings.area, settings
    bar = f"{s.bar:g}"
    if a is not None:
        km = a.radius_m / 1000.0
        out = [f"The circle: {km:g} km around {a.name}, its centre at [{a.center[0]:.4f}, {a.center[1]:.4f}] "
               "(longitude, latitude)."]
        inside, into = "inside the circle", "inside"
    else:
        out = ["The circle: this scenario's policy sets none ([watch.area]); the brief above says where to watch, "
               "and the circle below is that watched area."]
        inside, into = "inside the watched area", "inside it"
    lines = []
    for d, c in s.domains:
        sensors = [SENSOR_WORDS.get(x, x) for x in c.sensors]
        lines.append(f"- {DOMAIN_WORDS.get(d, d)}, within {_seconds(c.grace_s)} of its coming inside: "
                     f"{LEVEL_WORDS.get(c.level, c.level)}, from {_or(sensors)}.")
    out.append(f"Identify every contact of unknown, suspect or hostile affiliation that comes {inside}, "
               "in time:\n" + "\n".join(lines))
    out.append(f"A contact counts as identified when the picture shows it at its domain's level at an identity "
               f"confidence of {bar} or more, supported by its domain's sensor, without an identity conflict. A size "
               f"class (small_surface, small_air) is a detection, not an identification, at any confidence.")
    out.append(f"An identify order runs until the vehicle sent identifies the contact by its own look at {bar} or "
               f"more, or until its {_seconds(s.identify_on_station_s)} on station are over. A kind named below "
               f"{bar} is not yet an identification: the order goes on.")
    if s.neutral_after_bar:
        out.append(f"No contact is shown neutral or assumed friend by its classification before it is identified "
                   f"at {bar} or more (a call from its own AIS identity may come sooner).")
    else:
        out.append(f"The tracker may show a contact neutral by its classification below {bar}: that is not an "
                   "identification.")
    nv = [f"at {v:g} m/s {DOMAIN_WORDS.get(d, d)}" for d, v in s.no_velocity_mps]
    still = f"A contact shown without velocity (none, or under {s.still_mps:g} m/s) " + (
        f"is taken to come straight in {_or(nv)}, and is judged where it is in any other domain." if nv
        else "is judged where it is.")
    out.append(f"A contact in doubt (not identified, or shown other than neutral, assumed friend or friend) is "
               f"approaching when it is {inside} or its shown course brings it {into} within "
               f"{_seconds(s.approach_horizon_s)}. " + still)
    out.append(f"Keep every vehicle above its reserve: {float(reserve):g} % of its endurance left after a task of "
               f"{_seconds(float(task_s))}.")
    who = NUMBER_WORDS.get(approvers, str(approvers))
    out.append(f"An engagement needs its track identified at {LEVEL_WORDS.get(engage_level, engage_level)} at "
               f"{float(engage_bar):g} or more, and {who} people with engagement authority to sign it"
               + ("" if proposer_may else ", neither of them its proposer") + "; it is never automatic.")
    if s.loosened:
        out.append("Loosened by this scenario: " + "; ".join(f"{k}, because {why}" for k, why in s.loosened) + ".")
    return "\n\n".join(out)


def card_sha256(settings: Settings, rules_version: int, world: str, policy: dict[str, Any] | None = None, *,
                reserve_pct: float | None = None, task_duration_s: float | None = None) -> str:
    """The sha256 of `render`'s card (a record states it, as it states the v0 mission text's)."""
    text = render(settings, rules_version, world, policy, reserve_pct=reserve_pct, task_duration_s=task_duration_s)
    return hashlib.sha256(text.encode()).hexdigest()
