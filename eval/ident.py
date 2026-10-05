"""Rules 9 in the evaluation: rule-based identification against manual declaration, on the
same generated situations and seeds. MODEL RESULTS FROM SCRIPTED PROXIES, NEVER HUMAN DATA.

**What runs.** For each seed and untracked-author share, `generate` writes `n` identification
situations with seeded random geometry (`sim/scenarios/engage` format): one hostile author,
tracked or not (`untracked_share` of them not, a stated dimension: 0.2 and 0.5 by default),
who commits an abstract hostile act against our patrol USV (sometimes a second one later);
none to three other contacts near it (a fishing boat, shown neutral at some confidence or not
classified at all, an AIS ferry, a friendly patrol boat, another hostile), each tracked with
its own uncertainty and moving as `MOTION` states (holding or under way at a drawn speed and
course); one or two of our USVs observing (each its own fix of the act), and an armed aircraft.
Truth (who is hostile, who did it) is in the file, never shown to a decider.
`murmur-scenarios --json` plays every situation against the real decision engine once per
arm, with seeded noise of each fix's sigma on where it places the author, a scripted operator
reviewing the inbox (`scenario::ReviewProxy`: its own procedure over what the console shows,
never the engine's rule) and a nominator that nominates every track declared hostile as soon
as it may and has two signers sign it (`--nominator`). Arms, each the shipped policy
(`sim/policy.toml`; the generated files name no policy) changed only by its flags:

| arm | identification | review |
| --- | --- | --- |
| `manual` | `rule_declaration = false` | the proxy reads each observed act and declares the author it picks |
| `rule` | the shipped policy: rule on, `nominate_unconfirmed = false`, `corroboration = "none"` | the proxy confirms or revokes each rule declaration (no nomination before its confirmation) and answers flags |
| `rule-nominate` | as `rule`, `nominate_unconfirmed = true` (eval/policy/minimal.toml's) | the same, but a rule declaration is nominated at once |
| `rule-corroborated` | as `rule`, `corroboration = "second_observation"` | the same |
| `rule-corroborated-nominate` | both | the same |

**Labels.** A declaration is *correct* when it names the act's author and the author is
hostile; *false hostile* when the track's contact is not hostile; *wrong author* when it names
another hostile. A *missed* author is a tracked hostile author nobody declared. A wrong
declaration is *C2 showed it* when, at the act, another track had 0.1 or more of the
attribution or the declared track was shown neutral or assumed friend at 0.5 or more; else
*picture wrong*. An *effect on a wrong target* is an effect released on a track whose contact
is not hostile (the worst outcome). Workload: the items a person had to look at (every act in
the manual arm; every rule declaration and flag in the rule arms), the person's acts
(declarations, confirmations, revocations, flag resolutions), and the confirmations an
engagement had to wait for. Workload is counted alike in every arm from the proxy's own
tally (`scenario::ProxyStats`): the items put before the person, their decisions (one per
item it got to the end of: declared, confirmed, revoked, dismissed, or read and decided
nothing), and of those the consequential ones (declared, confirmed, revoked).

**Caveats.** The proxy is a model with stated parameters (eval/README.md), and its procedure
is close to the rule's own: the best fix, two combined sigmas, the victim excluded, a track
shown neutral or friendly at 0.5 left alone. Their agreement is built in (`agreement`
measures it): the manual arm says nothing about how accurately a person identifies, and a case
the rule gets wrong the proxy mostly gets wrong too. The nominator and the signers are scripts
(the signers sign whatever is nominated); the situations are generated from stated
distributions, not sampled from the world; the tracks are the scenario runner's (truth plus
seeded noise), not the bridge's tracker. Scenarios 72-78 illustrate the rule (the engagement
suite runs them); they are not part of this measurement.

    python -m eval.run ident --seeds 1 2 3 4 5 6 7 8 9 10 --situations 20 --untracked-share 0.2 0.5
"""

from __future__ import annotations

import json
import math
import random
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import REPO
from .metrics import load_jsonl

BINARY = REPO / "target" / "release" / "murmur-scenarios"
#: below this attribution a candidate is not worth naming (the engine's FLAG_FLOOR)
FLAG_FLOOR = 0.1
#: the shipped policy's attribution threshold (sim/policy.toml), for the agreement measure
RULE_THRESHOLD = 0.9
#: what every arm runs with: act noise, the review proxy, the nominator
COMMON = ["--act-noise", "--proxy", "--nominator"]
ARMS: dict[str, list[str]] = {
    "manual": ["--ident", "off"],
    "rule": [],
    "rule-nominate": ["--nominate-unconfirmed"],
    "rule-corroborated": ["--corroboration", "second_observation"],
    "rule-corroborated-nominate": ["--corroboration", "second_observation", "--nominate-unconfirmed"],
}
DEFAULT_ARMS = tuple(ARMS)
NOT_HUMAN = (
    "Model results from scripted proxies on generated situations: the reviewing operator is a "
    "model with stated parameters (its own procedure over what the console shows), the "
    "nominator and signers are scripts, the situations come from stated distributions, and "
    "the labels are the generated truth. Not human data and not evidence about people."
)

# ---- the generator ---------------------------------------------------------------------------

#: where situations are placed: inside ez-gate-west, clear of the no-strike areas
CENTRE_BOX = ((-122.520, -122.497), (37.806, 37.819))
BLADE_AT = (-122.4700, 37.8160)


def _offset(p: tuple[float, float], east: float, north: float) -> tuple[float, float]:
    lon, lat = p
    return (lon + east / (111_320.0 * math.cos(math.radians(lat))), lat + north / 111_320.0)


def _polar(rng: random.Random, p: tuple[float, float], r: float) -> tuple[float, float]:
    a = rng.uniform(0, 2 * math.pi)
    return _offset(p, r * math.sin(a), r * math.cos(a))


def _dist(a: tuple[float, float], b: tuple[float, float]) -> float:
    de = (a[0] - b[0]) * 111_320.0 * math.cos(math.radians((a[1] + b[1]) / 2))
    dn = (a[1] - b[1]) * 111_320.0
    return math.hypot(de, dn)


def _pt(p: tuple[float, float]) -> str:
    return f"[{p[0]:.6f}, {p[1]:.6f}]"


#: how a contact moves (stated distributions): the chance it holds still, else a speed drawn
#: uniformly from the range (m/s) on a course drawn uniformly
MOTION: dict[str, tuple[float, tuple[float, float]]] = {
    "author": (0.4, (2.0, 8.0)),
    "fishing boat, classified": (0.5, (1.0, 4.0)),
    "fishing boat, not classified": (0.5, (1.0, 4.0)),
    "ferry with AIS": (0.0, (6.0, 10.0)),
    "friendly patrol boat": (0.3, (3.0, 8.0)),
    "another hostile": (0.4, (2.0, 8.0)),
}
#: the untracked-author shares a comparison runs by default
UNTRACKED_SHARES = (0.2, 0.5)


def _velocity(rng: random.Random, kind: str) -> tuple[float, float]:
    hold, (lo, hi) = MOTION[kind]
    if rng.random() < hold:
        return (0.0, 0.0)
    v, c = rng.uniform(lo, hi), rng.uniform(0, 2 * math.pi)
    return (v * math.sin(c), v * math.cos(c))


BYSTANDERS = [
    # (kind, weight, truth, shown affiliation, identity, affiliation confidence range or None)
    ("fishing boat, classified", 0.30, "neutral", "neutral", "fishing_vessel", (0.3, 0.9)),
    ("fishing boat, not classified", 0.20, "neutral", "unknown", None, None),
    ("ferry with AIS", 0.15, "neutral", "neutral", "ferry", (0.75, 0.9)),
    ("friendly patrol boat", 0.10, "assumed_friend", "assumed_friend", "patrol_boat", (0.8, 0.9)),
    ("another hostile", 0.25, "hostile", "suspect", "small_fast_boat", (0.4, 0.8)),
]


def generate(seed: int, n: int, out: Path, untracked_share: float = 0.2) -> list[dict[str, Any]]:
    """`n` situations for `seed` into `out`, each with its truth (`meta`); `untracked_share`
    of the authors are no track we hold."""
    out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(seed * 7919 + 17)
    metas = []
    for i in range(n):
        centre = (rng.uniform(*CENTRE_BOX[0]), rng.uniform(*CENTRE_BOX[1]))
        usv1 = _polar(rng, centre, rng.uniform(1_000, 2_200))
        usv2 = _polar(rng, centre, rng.uniform(1_200, 3_000)) if rng.random() < 0.6 else None
        tracked = rng.random() >= untracked_share
        tracks: list[dict[str, Any]] = []
        truth: dict[str, str] = {}
        n_id = 0

        def new_id() -> str:
            nonlocal n_id
            n_id += 1
            return f"T-{n_id:04d}"

        author = "untracked"
        if tracked:
            author = new_id()
            shown, conf, ident = ("suspect", rng.uniform(0.4, 0.8), "small_fast_boat") if rng.random() < 0.5 else ("unknown", 0.0, None)
            tracks.append({"id": author, "at": centre, "aff": shown, "conf": conf, "identity": ident, "sigma": rng.uniform(8, 25), "vel": _velocity(rng, "author")})
            truth[author] = "hostile"
        k = rng.choices([0, 1, 2, 3], weights=[0.35, 0.35, 0.2, 0.1])[0]
        desc = []
        for _ in range(k):
            kind, _, tr, shown, ident, crange = rng.choices(BYSTANDERS, weights=[b[1] for b in BYSTANDERS])[0]
            tid = new_id()
            at = _polar(rng, centre, rng.uniform(15, 400))
            tracks.append(
                {
                    "id": tid,
                    "at": at,
                    "aff": shown,
                    "conf": rng.uniform(*crange) if crange else 0.0,
                    "identity": ident,
                    "sigma": rng.uniform(8, 25),
                    "vel": _velocity(rng, kind),
                }
            )
            truth[tid] = tr
            desc.append(f"{kind} {_dist(at, centre):.0f} m off")
        # the act's fixes: each observing USV's radar, at its range
        observers = ["usv-1"] + (["usv-2"] if usv2 else [])
        ranges = [_dist(usv1, centre)] + ([_dist(usv2, centre)] if usv2 else [])
        sigma = 30.0 + 0.01 * min(ranges)
        second = rng.random() < 0.2
        lines = [
            f'title = "generated identification situation {seed}-{i}"',
            'invariant = "rule_declaration"',
            f'attack = "{"tracked" if tracked else "untracked"} author; {", ".join(desc) or "nobody near"}"',
            'expect = "done"',
            "end_s = 220",
            "",
            '[[operator]]\nid = "op-1"',
            '[[operator]]\nid = "op-2"\nengage = true',
            '[[operator]]\nid = "op-3"\nengage = true',
            "",
            f'[[vehicle]]\nuid = "blade-1"\nplatform = "switchblade"\nat = {_pt(BLADE_AT)}',
            f'[[vehicle]]\nuid = "usv-1"\nplatform = "seahunter"\nat = {_pt(usv1)}',
        ]
        if usv2:
            lines.append(f'[[vehicle]]\nuid = "usv-2"\nplatform = "seahunter"\nat = {_pt(usv2)}')
        for t in tracks:
            lines.append(
                "\n".join(
                    [
                        "[[track]]",
                        f'id = "{t["id"]}"',
                        f"at = {_pt(t['at'])}",
                        f"velocity = [{t['vel'][0]:.2f}, {t['vel'][1]:.2f}]",
                        f'affiliation = "{t["aff"]}"',
                        f"affiliation_confidence = {t['conf']:.2f}",
                        f'truth = "{truth[t["id"]]}"',
                        *( [f'identity = "{t["identity"]}"', "identity_confidence = 0.8"] if t["identity"] else ["identity_confidence = 0.2"] ),
                        f"sigma_m = {t['sigma']:.1f}",
                        'sources = ["usv-1:radar"]',
                    ]
                )
            )
        sensors = ", ".join(f'"{o}:radar"' for o in observers)
        by = f'by_track = "{author}"' if tracked else f"at = {_pt(centre)}"
        for at_s in [10] + ([40] if second else []):
            lines.append(f'[[step]]\nat_s = {at_s}\naction = "act"\n{by}\nagainst_vehicle = "usv-1"\nsensors = [{sensors}]\nsigma_m = {sigma:.1f}')
        text = "\n\n".join(lines) + "\n"
        assert "\npolicy" not in text, "a generated situation never names a policy: every arm is the shipped one"
        name = f"sit-{seed:03d}-{i:03d}"
        (out / f"{name}.toml").write_text(text)
        metas.append(
            {
                "name": name,
                "author": author,
                "truth": truth,
                "acts": 2 if second else 1,
                "observers": len(observers),
                "bystanders": desc,
                "untracked_share": untracked_share,
                "shown": {t["id"]: {"aff": t["aff"], "conf": round(t["conf"], 2), "from_author_m": round(_dist(t["at"], centre))} for t in tracks},
            }
        )
    (out / "situations.json").write_text(json.dumps(metas, indent=1))
    return metas


# ---- one arm, one seed -----------------------------------------------------------------------


def run_arm(arm: str, seed: int, situations: Path, metas: list[dict[str, Any]], out: Path, binary: Path = BINARY, extra: list[str] | None = None) -> dict[str, Any]:
    """Every situation of a seed under one arm; each analysed with its truth."""
    out.mkdir(parents=True, exist_ok=True)
    if not binary.exists():
        return {"arm": arm, "seed": seed, "error": f"missing {binary}: run `make build`", "results": []}
    files = [situations / f"{m['name']}.toml" for m in metas]
    cmd = [str(binary), "--json", "--seed", str(seed), *COMMON, *ARMS[arm], *(extra or [])]
    cmd += ["--root", str(REPO), "--log-dir", str(out / "logs"), *map(str, files)]
    t0 = time.monotonic()
    p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True, timeout=1800)
    (out / "results.jsonl").write_text(p.stdout)
    if p.returncode == 2:
        return {"arm": arm, "seed": seed, "error": p.stderr.strip()[-1000:] or "load error", "results": []}
    by_name = {m["name"]: m for m in metas}
    results = []
    for line in p.stdout.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        stem = Path(r.get("file", "")).stem
        events = load_jsonl(out / "logs" / f"{stem}.jsonl")
        results.append(analyze(r, events, by_name.get(stem, {}), arm, seed))
    return {"arm": arm, "seed": seed, "wall_s": round(time.monotonic() - t0, 2), "results": results, "stderr": p.stderr.strip()[-500:]}


def _secs(t: int, t0: int) -> float:
    return round((t - t0) / 1000.0, 1)


def analyze(result: dict[str, Any], events: list[dict[str, Any]], meta: dict[str, Any], arm: str, seed: int) -> dict[str, Any]:
    """One situation under one arm, labelled with its truth."""
    truth: dict[str, str] = meta.get("truth", {})
    author = meta.get("author", "untracked")
    t0 = events[0]["t"] if events else 0
    acts: dict[str, dict[str, Any]] = {}
    first_act_t = None
    for e in events:
        if e.get("type") == "ident.act":
            a = e.get("act") or {}
            acts[a.get("act")] = {"t": e["t"], "candidates": e.get("candidates") or [], "tracks": (e.get("inputs") or {}).get("tracks") or {}, "verdict": e.get("verdict") or {}}
            first_act_t = first_act_t if first_act_t is not None else e["t"]

    def shown(act: str | None, track: str) -> bool:
        a = acts.get(act or "") or (next(iter(acts.values())) if acts else {})
        others = [c for c in a.get("candidates", []) if c.get("track") != track and c.get("confidence", 0) >= FLAG_FLOOR]
        est = (a.get("tracks", {}).get(track) or {})
        aff = (est.get("affiliation") or {}).get("value") if isinstance(est.get("affiliation"), dict) else est.get("affiliation")
        return bool(others) or (aff in ("neutral", "assumed_friend") and (est.get("affiliation_confidence") or 0) >= 0.5)

    declarations = []
    revocations = []
    confirmations = resolutions = person_declarations = flags = rule_declarations = 0
    for e in events:
        t = e.get("type")
        if t == "ident.declared":
            d = e.get("declaration") or {}
            if d.get("affiliation") != "hostile":
                continue
            by = d.get("by") or {}
            rule = by.get("kind") == "rule"
            act = (by.get("evidence") or {}).get("act") if rule else (by.get("note") or "").removeprefix("proxy: act ") or None
            track = d.get("track")
            tr = truth.get(track)
            verdict = "correct" if track == author and tr == "hostile" else ("wrong_author" if tr == "hostile" else "false_hostile")
            declarations.append(
                {
                    "track": track,
                    "by": "rule" if rule else "person",
                    "t_s": _secs(e["t"], t0),
                    "after_act_s": None if first_act_t is None else _secs(e["t"], first_act_t),
                    "verdict": verdict,
                    "c2_showed_it": None if verdict == "correct" else shown(act, track),
                }
            )
            rule_declarations += int(rule)
            person_declarations += int(not rule)
        elif t == "ident.revoked":
            track = e.get("track")
            revocations.append({"track": track, "right": truth.get(track) != "hostile" or track != author})
        elif t == "ident.confirmed":
            confirmations += 1
        elif t == "ident.flag_resolved":
            resolutions += 1
        elif t == "ident.flagged":
            flags += 1
    engagements: dict[int, dict[str, Any]] = {}
    for e in events:
        t = e.get("type")
        if t == "intent.created":
            i = e.get("intent") or {}
            if i.get("kind") == "engage":
                track = (i.get("params") or {}).get("track")
                engagements[i.get("id")] = {"track": track, "truth": truth.get(track), "final": "proposed", "effect_after_act_s": None}
        elif t == "intent.state" and e.get("intent") in engagements:
            engagements[e["intent"]]["final"] = e.get("to")
        elif t == "engage.effect" and e.get("intent") in engagements and first_act_t is not None:
            engagements[e["intent"]]["effect_after_act_s"] = _secs(e["t"], first_act_t)
    author_tracked = author != "untracked"
    right = [d for d in declarations if d["verdict"] == "correct"]
    effects = [g for g in engagements.values() if g["effect_after_act_s"] is not None]
    # the proxy's agreement with the rule: what the rule would declare on each act (its best
    # candidate at the threshold, not shown neutral or friendly at 0.5), against the track the
    # person declared on it (person declarations name their act)
    def rule_would(act: str) -> str | None:
        a = acts.get(act) or {}
        c = (a.get("candidates") or [None])[0]
        if not c or c.get("confidence", 0) < RULE_THRESHOLD:
            return None
        est = a.get("tracks", {}).get(c.get("track")) or {}
        aff = (est.get("affiliation") or {}).get("value") if isinstance(est.get("affiliation"), dict) else est.get("affiliation")
        if aff in ("neutral", "assumed_friend") and (est.get("affiliation_confidence") or 0) >= 0.5:
            return None
        return c.get("track")

    person_on_act = {}
    for e in events:
        if e.get("type") == "ident.declared":
            d = e.get("declaration") or {}
            by = d.get("by") or {}
            note = by.get("note") or ""
            if by.get("kind") == "operator" and note.startswith("proxy: act "):
                person_on_act[note.removeprefix("proxy: act ")] = d.get("track")
    agreement = {
        "person_declarations": len(person_on_act),
        "same_as_rule": sum(1 for a, tr in person_on_act.items() if rule_would(a) == tr),
        "rule_would_declare": sum(1 for a in acts if rule_would(a)),
    }
    shown_tracks = meta.get("shown") or {}
    wrong_effects = [
        {
            "situation": meta.get("name"),
            "track": g["track"],
            "truth": g["truth"],
            "author": author,
            "shown": shown_tracks.get(g["track"]),
        }
        for g in effects
        if g["truth"] != "hostile"
    ]
    return {
        "situation": meta.get("name"),
        "arm": arm,
        "seed": seed,
        "replay_ok": result.get("replay_ok"),
        "author_tracked": author_tracked,
        "acts": len(acts),
        "declarations": declarations,
        "author_declared": bool(right),
        "missed": author_tracked and not right,
        "first_right_after_s": min((d["after_act_s"] for d in right if d["after_act_s"] is not None), default=None),
        "revocations": revocations,
        "confirmations": confirmations,
        "resolutions": resolutions,
        "person_declarations": person_declarations,
        "rule_declarations": rule_declarations,
        "flags": flags,
        "engagements": list(engagements.values()),
        "effects_right": sum(1 for g in effects if g["truth"] == "hostile" and g["track"] == author),
        "effects_wrong_target": sum(1 for g in effects if g["truth"] != "hostile"),
        "effects_other_hostile": sum(1 for g in effects if g["truth"] == "hostile" and g["track"] != author),
        "first_effect_after_s": min((g["effect_after_act_s"] for g in effects), default=None),
        "wrong_effects": wrong_effects,
        "proxy": result.get("proxy") or {},
        "agreement": agreement,
    }


# ---- over runs --------------------------------------------------------------------------------


def _median(xs: list[float]) -> float | None:
    return round(statistics.median(xs), 1) if xs else None


def summarize(passes: list[dict[str, Any]]) -> dict[str, Any]:
    """Per arm, over situations and seeds."""
    rows: dict[str, dict[str, Any]] = {}
    for ps in passes:
        for r in ps["results"]:
            row = rows.setdefault(
                r["arm"],
                {
                    "situations": 0, "replay_ok": 0, "tracked_authors": 0, "untracked_authors": 0,
                    "author_declared": 0, "missed": 0, "declared_by_rule": 0, "declared_by_person": 0,
                    "false_hostile": 0, "wrong_author": 0, "wrong_c2_showed_it": 0, "wrong_picture_wrong": 0,
                    "false_by_rule": 0, "false_by_person": 0, "revocations": 0, "revocations_wrong": 0,
                    "looks": 0, "person_acts": 0, "confirmations": 0, "flags": 0,
                    "items": 0, "proxy_looks": 0, "reads": 0, "decisions": 0, "consequential": 0, "dismissed": 0,
                    "decided_nothing": 0, "confirmed_unread": 0,
                    "agree_person_declarations": 0, "agree_same_as_rule": 0, "rule_would_declare": 0,
                    "wrong_effects": [],
                    "effects_right": 0, "effects_wrong_target": 0, "effects_other_hostile": 0,
                    "_t_decl": [], "_t_effect": [],
                },
            )
            row["situations"] += 1
            row["replay_ok"] += int(bool(r.get("replay_ok")))
            row["tracked_authors"] += int(r["author_tracked"])
            row["untracked_authors"] += int(not r["author_tracked"])
            row["author_declared"] += int(r["author_declared"])
            row["missed"] += int(r["missed"])
            for d in r["declarations"]:
                if d["verdict"] == "correct":
                    row["declared_by_rule" if d["by"] == "rule" else "declared_by_person"] += 1
                    continue
                row[d["verdict"]] += 1
                row["false_by_rule" if d["by"] == "rule" else "false_by_person"] += 1
                row["wrong_c2_showed_it" if d["c2_showed_it"] else "wrong_picture_wrong"] += 1
            row["revocations"] += len(r["revocations"])
            row["revocations_wrong"] += sum(1 for v in r["revocations"] if not v["right"])
            # what a person had to look at: every act without the rule; every rule
            # declaration and flag with it
            row["looks"] += r["acts"] if r["arm"] == "manual" else r["rule_declarations"] + r["flags"]
            row["person_acts"] += r["person_declarations"] + r["confirmations"] + len(r["revocations"]) + r["resolutions"]
            # the proxy's own tally, counted alike in every arm (scenario::ProxyStats)
            px = r.get("proxy") or {}
            row["items"] += px.get("items", 0)
            row["proxy_looks"] += px.get("looks", 0)
            row["reads"] += px.get("reads", 0)
            row["decisions"] += px.get("decisions", 0)
            row["consequential"] += px.get("declared", 0) + px.get("confirmed", 0) + px.get("revoked", 0)
            row["dismissed"] += px.get("dismissed", 0)
            row["decided_nothing"] += px.get("nothing", 0)
            row["confirmed_unread"] += px.get("confirmed_unread", 0)
            ag = r.get("agreement") or {}
            row["agree_person_declarations"] += ag.get("person_declarations", 0)
            row["agree_same_as_rule"] += ag.get("same_as_rule", 0)
            row["rule_would_declare"] += ag.get("rule_would_declare", 0)
            row["wrong_effects"] += r.get("wrong_effects") or []
            row["confirmations"] += r["confirmations"]
            row["flags"] += r["flags"]
            row["effects_right"] += r["effects_right"]
            row["effects_wrong_target"] += r["effects_wrong_target"]
            row["effects_other_hostile"] += r["effects_other_hostile"]
            if r["first_right_after_s"] is not None:
                row["_t_decl"].append(r["first_right_after_s"])
            if r["first_effect_after_s"] is not None:
                row["_t_effect"].append(r["first_effect_after_s"])
    for row in rows.values():
        td, te = row.pop("_t_decl"), row.pop("_t_effect")
        row["act_to_declaration_median_s"] = _median(td)
        row["act_to_effect_median_s"] = _median(te)
    return {"arms": rows, "errors": [{"arm": p["arm"], "seed": p["seed"], "error": p["error"]} for p in passes if p.get("error")]}


def run_comparison(
    out: Path,
    seeds: list[int],
    arms: tuple[str, ...] | list[str] = DEFAULT_ARMS,
    situations: int = 20,
    extra: list[str] | None = None,
    binary: Path | None = None,
    untracked_shares: tuple[float, ...] | list[float] = UNTRACKED_SHARES,
) -> dict[str, Any]:
    """Every arm over the same generated situations of each seed, for each untracked-author
    share, into `out`, and `comparison.json` (`summary` and `passes`: the first share's;
    `by_share`: every share's)."""
    out.mkdir(parents=True, exist_ok=True)
    by_share: dict[str, dict[str, Any]] = {}
    for share in untracked_shares:
        key = f"{share:g}"
        base = out if len(untracked_shares) == 1 else out / f"untracked-{key}"
        passes = []
        for seed in seeds:
            sdir = base / "situations" / f"seed-{seed}"
            metas = generate(seed, situations, sdir, untracked_share=share)
            for arm in arms:
                passes.append(run_arm(arm, seed, sdir, metas, base / arm / f"seed-{seed}", binary=binary or BINARY, extra=extra))
        by_share[key] = {"untracked_share": share, "passes": passes, "summary": summarize(passes)}
    first = by_share[f"{untracked_shares[0]:g}"]
    comp = {
        "label": NOT_HUMAN,
        "seeds": seeds,
        "situations_per_seed": situations,
        "arms": list(arms),
        "proxy": "scenario::ReviewProxy defaults: a look every 20 s, 6 s to read, reads 80%, a 5% slip" if not extra else " ".join(extra),
        "untracked_shares": list(untracked_shares),
        "passes": first["passes"],
        "summary": first["summary"],
        "by_share": {k: {"untracked_share": v["untracked_share"], "summary": v["summary"]} for k, v in by_share.items()},
    }
    (out / "comparison.json").write_text(json.dumps(comp, indent=1, default=str))
    return comp


def load_comparison(batch: Path) -> dict[str, Any] | None:
    p = Path(batch) / "ident" / "comparison.json"
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text())
    except ValueError:
        return None


def table(comp: dict[str, Any]) -> str:
    """The comparison as plain text, one block per untracked-author share, one line per arm."""
    lines = [comp["label"], ""]
    shares = comp.get("by_share") or {"": {"summary": comp["summary"], "untracked_share": None}}
    for block in shares.values():
        if block.get("untracked_share") is not None:
            lines.append(f"untracked authors: {block['untracked_share']:g}")
        lines.append(
            f"{'arm':<27}{'sit':>5}{'declared':>9}{'missed':>7}{'false':>6}{'C2':>4}{'pict':>5}{'revoked':>8}{'items':>6}{'decide':>7}{'conseq':>7}{'conf':>5}"
            f"{'eff ok':>7}{'eff WRONG':>10}{'t decl':>7}{'t eff':>7}"
        )
        for arm in comp["arms"]:
            r = block["summary"]["arms"].get(arm)
            if not r:
                continue
            lines.append(
                f"{arm:<27}{r['situations']:>5}{r['author_declared']:>9}{r['missed']:>7}{r['false_hostile']:>6}{r['wrong_c2_showed_it']:>4}"
                f"{r['wrong_picture_wrong']:>5}{r['revocations']:>8}{r['items']:>6}{r['decisions']:>7}{r['consequential']:>7}{r['confirmations']:>5}"
                f"{r['effects_right']:>7}{r['effects_wrong_target']:>10}{str(r['act_to_declaration_median_s']):>7}{str(r['act_to_effect_median_s']):>7}"
            )
        lines.append("")
    return "\n".join(lines)
