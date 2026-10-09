# The playbook engine: what "useful" means, frozen before its first live run

Written 2026-10-09 for murmur-t8m (step 1 of the playbook engine), before any live run of the
engine, and frozen: a later change to a target or a threshold is a new dated section below,
never an edit of this one. The criteria are the research report's (C.1, "What useful should
mean here"); each is testable from records the harness already keeps (the audit log, the
situation log, the storyline and truth records) or from the engine's own fixture tests.

The engine has three jobs: the **free option** that runs a situation for someone with no model
subscription, the **baseline** that says what a model adds, and the **shadow** beside the
model watch officer. The criteria serve all three.

## The criteria and their targets

| criterion | definition (how measured) | target |
|---|---|---|
| **Coverage** | For each situation type, the share of runs in which the engine recognizes the situation from observable facts (never the storyline or the director's notes) and makes the right response; plus, for every play, a negative test in which it must stay silent (a neutral ferry, an assumed friend alongside, a stale track, a suspect below 0.5 for armed plays). | Harbor watch (the one runnable situation): a sensor orbit proposed on the unknown fast boat. Silent in every negative test. The other three situations are stubs: counted when they exist. |
| **Self-consistency** | Proposals that Murmur's own rule book (`eval/evidence.py`, v2) or `preview_intent` objects to at proposal time: from the audit log (each proposal's checks) and the evidence record riding in its rationale (`rulebook_v2`). | **Zero.** By construction: a failing preview is dropped (with a back-off) and the rule book vetoes before `propose_intent`. |
| **Precision** | Truth-labelled `legitimate` share of its proposals, split "the C2 showed it" and "picture wrong" (METRICS.md labels). Needs the evaluation's truth labels: measured in step 2, when the engine is the evaluation's baseline arm. | At least 90 % on the catchable category; picture-wrong reported, not targeted. Step 1 reports each live proposal against the situation's director's notes (truth for that run), by hand. |
| **Timeliness** | Seconds from the trigger becoming observable (the first read in which the play's trigger holds: a track inside a watch area or closing within the CPA alert, existence confidence at least 0.5, unclassified, fast or in conflict; a `lost_link` alert's `since_t` plus the gap; an alert's `since_t` plus 30 s) to the proposal in the audit log. | At most one tick plus a preview with polling: about 3 s at the 2 s tick. Reported beside the model watch officer's. |
| **Rationale fidelity** | Every fact cited in the rationale matches what the engine read at proposal (value, age within the tick), the play and its version are named, and the withdraw condition is stated. Checked by script against the evidence record that rides after the delimiter line in every rationale (`rationale.parse_record`). | 100 %, by construction (the text is rendered from the record); the fixture test guards regressions. |
| **Behaviour under refusal** | Re-proposals of the same (play, target, vehicle) after a rejection without a material change (defined per play, named in the rationale when it applies); own cards left pending after their premise vanished. From the audit log and the engine's ledger. | Zero re-proposals without a stated material change. Own stale cards withdrawn within one tick of the premise vanishing. |
| **Inbox load** | Human-tier cards per operator-minute, own pending at once, the share of own cards that expired unread, override rate and goodput (METRICS.md). | At most 2 own pending human-tier cards at any moment; at most one new human-tier card per 30 s after a burst of 2; none but P0 and P1 while the inbox holds 6 or more. Expired-unread share reported against the model's. |
| **Reproducibility** | The same recorded reads give byte-identical proposals and rationales (fixture test). Live runs: proposal-by-proposal comparison across repeated runs, with timing jitter reported (PLAN.md found 5 of 180 runs reordered by tick phase). | Exact on fixtures. Live mismatches listed and explained. |
| **Cost and availability** | Runs with no model, no network beyond the bridge, no credentials; compute per tick. | Zero marginal cost; under 50 ms of engine compute per tick at 100 vehicles (from a recorded or synthetic read of 100 vehicles). |

Precision matters more than recall for human-tier plays (false-alarm-prone aids teach the
operator to discount the next card; research report Part B.6). Auto-tier plays may lean to
recall, since they cost no decision and the engine's checks still stop what is unsafe.

## Where the thresholds come from

Every threshold a play uses is in `agents/playbook/defaults.toml` with its reason, and in the
evidence record of every proposal that relies on it (`thresholds`). They come from doctrine,
the shipped policy and the research, never from Harbor watch: the situation configures only
where to watch (watch areas, protected points), which plays are on, and a preferred sensor
platform.

| threshold | value | used by | reason |
|---|---|---|---|
| existence confidence now | at least 0.5 | `isr.investigate`, `isr.armed_overwatch` | more likely than not that the contact is there: the same 0.5 as the rule book's `C_MIN` |
| affiliation confidence | at least 0.5 | `isr.armed_overwatch`; withdraw on neutral or assumed friend | the rule book's `C_MIN` ("more likely than not"); below it the picture does not support an armed task |
| classification settled | identity confidence 0.6 | `isr.investigate` | the engagement rules' `min_identity_confidence`: the identity confidence the engine itself treats as settled |
| fast | above 8 m/s (15.5 kn) | `isr.investigate` | faster than a displacement hull under 10 m can go (hull speed about 1.34 x sqrt(waterline ft) kn, 7.3 kn at 30 ft): a small contact this fast is planing, the fast-craft profile of force protection |
| CPA alert | 457 m (500 yd) | `isr.investigate`, `isr.armed_overwatch` | the 500-yard naval vessel protection zone of 33 CFR 165 subpart G, the distance inside which an approach to a protected US naval vessel is regulated |
| TCPA horizon | 600 s | `isr.investigate`, `isr.armed_overwatch` | inside the policy's assumed task length (`task_duration_s` 900): a look proposed now is still on station at the closest approach |
| unseen | 60 s | withdraw, release | the tracker's existence-confidence decay constant (`confidence_decay_s` 60): the confidence has fallen to 1/e |
| orbit radius | max(1.1 x min turn, a quarter of the imaging sensor's range) | `isr.*` | the rules' `orbit_radius_margin`; the sensing model detects with probability 0.5 at a sensor's nominal range and better inside (PLAN.md C2), so a quarter keeps the whole orbit well inside it |
| recentre | target more than half the radius from the centre; at most once per 60 s per vehicle | `isr.recentre` | the research design's hysteresis against flip-flopping |
| link gap | lost for 2 x the link's `lost_after_s` | `link.cover_gap` | the research design: a flap shorter than that is not a gap |
| low reserve | battery under the policy's `reserve_pct` + 5 points | `safety.low_reserve` | the research design; the margin lets the return start before the rules' reserve refuses tasks |
| free battery | at least the low-reserve line + 10 points | the free-vehicle definition | a task can start without tripping `safety.low_reserve` within the policy's 15-minute task on an airframe of 2.5 h or more |
| stuck | a `not_moving` or `task_failed` alert for 30 s | `safety.stuck` | the research design |
| nav suspect clears | 60 s of good navigation | `nav.suspect_exclude` | the research design ("flags clear for 60 s") |
| budget | 1 human-tier card per 30 s, burst 2; at most 2 own pending; only P0 and P1 at 6 or more pending in the inbox; at most 3 previews a tick | the arbiter | research report C.2.5, provisional until the console's measured card times (`operator.view`) give the interaction time per card |
| cooldowns | rejected 10 min; expired unread 2 min and the score halved; voided 30 s | the ledger | research report C.2.5 |
| preview back-off | 10 s, doubling to 160 s | the engine | a failing preview is retried, never dropped for good (the old agent's "never again" lost the boat once the geofence refused one orbit) |
| tick | 2 s | the engine | the research design; the bridge refreshes its change digest at most once a second |

## How step 1 checks them

- Fixture tests (`agents/tests/test_playbook_*.py`, in `make check`): every play fires on its
  trigger and stays silent in its negative cases; the arbiter's order and claims; the budget;
  cooldowns and material change; withdraws; `nav.suspect_exclude`; shadow mode makes no writer
  call; byte-for-byte determinism on recorded reads; compute per tick at 100 vehicles; the
  evidence record round-trips.
- Live runs on stand-ins (results in the commit that adds them and the report to the issue):
  Harbor watch with autoplay; a rejection with a note and the cooldown holding; a link loss
  exercising `link.cover_gap` and `link.withdraw_pending`. Every audit log replayed with its
  anchors.

## Additions, 2026-10-09, before the first live run

Found while writing the engine against recorded reads of a live Harbor watch run (no engine
running in it; `agents/tests/fixtures/playbook/harbor-watch-reads.json`), and frozen with the
rest before the engine's first live run. None changes a target above.

| addition | value | reason |
|---|---|---|
| leaving the watch | beyond 1.1 x a watch area's radius, and no closest approach inside 2 x the CPA alert within 1.5 x the TCPA horizon; a track whose velocity is not known never counts as gone | hysteresis: in the recorded reads the tracker's velocity for the young fast boat alternated between a value and none on every read (165 to 191 s), so a look proposed on one tick was withdrawn on the next. An unknown motion is not a motion away |
| holding | under 0.5 m/s a contact has no closest approach ahead: its distance now counts | a velocity that small is within the tracker's noise |
| withdrawn by the engine | the key cools 30 s, like a void | a premise that comes back at once is not proposed again in the same breath |
| choices of vehicle | a look offers its first three free vehicles, each 0.001 lower in score | one claim per vehicle then falls to the next free vehicle, instead of holding a second look back; the rationale says which vehicle went first and to what |
| identity conflict | a reason to look, beside an unsettled classification and speed | the tracker's own anomaly flag (an AIS identity that does not fit what was seen): what a close look settles |
| the 0.5 gate on a look | the track's existence confidence now | the look exists to classify: gating it on affiliation confidence would keep it off every unknown (0.0 until a sensor classifies it). Affiliation confidence 0.5 gates the armed play |
| navigation suspect | excludes a vehicle from every tasking play, not only armed ones (hold and rtl still go) | a sensor whose own position is in doubt places what it sees in the wrong place; a filler flies the wrong route |

## Changes after the first live runs, 2026-10-09 (the targets above unchanged)

The first live runs (Harbor watch on stand-ins; agents/README.md, "The playbook engine") found
behaviour the fixtures had not. Each was fixed and a fixture test added; the targets above
stand as frozen, and the results are judged against them.

| change | before | after | found |
|---|---|---|---|
| material change on the closest approach | the CPA halved | the CPA halved and by at least half the CPA alert (229 m) | a rejection at 192 m, then a re-proposal 4 s later citing 79 m, the boat on the same heading: estimate noise, not news |
| `link.withdraw_pending` | any card of ours waiting | a card waiting for the operator (pending approval) only | it withdrew an order the operator had approved and the bridge's queue held for the link (rules 5) |
| an order not flying | an order that failed | failed, superseded, or back in the queue | a requeued order still claimed its vehicle |
| an order of ours gone out | held its track only once its vehicle flew it | holds its track until it ends | a look read as not holding in the second between dispatch and the vehicle's report |
| a safety command | a P0 command took a vehicle from any card of ours | never from another safety command; once per alert or battery while its own command is live | a stuck vehicle got two holds a second apart (the first queued for the link) |
