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

## Fix round 1, 2026-10-09 (a review of step 1; the targets above unchanged)

A review ran the engine against scripted worlds and found it repeating itself without a cause
it could name, and overriding people. The principle the fixes follow: **a person's later
decision wins over the playbook, and nothing the playbook does repeats without a cause it can
name.** An order of ours that ended failed, was superseded by a human's, or was refused at
submission is an answer, not a blank. Each finding has a test in
`agents/tests/test_playbook_review.py` that failed before its fix.

**Corrections to the definitions above** (the definitions were wrong, not the targets):

- **Timeliness of `safety.stuck`.** The table measures it from "an alert's `since_t` plus
  30 s". That is wrong: a `not_moving` alert's `since_t` is the last state change of the order
  it is about (minutes before, for an order that ran a while), not when the vehicle stopped, and
  the bridge says how long it has not moved only in the alert's detail ("NOT MOVING for 12s").
  Measured from now on as: from the first read in which the playbook saw the alert (keyed by the
  alert's id and kind) plus 30 s, to the proposal. The play debounces the same way, and its
  rationale quotes the bridge's detail instead of computing a duration. The bridge's alert
  semantics are unchanged (murmur-9kn proposes a structured stall start on the alert).
- **What rationale fidelity proves.** The check (`agents/playbook/fidelity.py`) proves that a
  rationale's text is exactly what its play's template renders from the evidence record riding
  with it, and, since this round, that every number in the text is shown by a non-null field of
  that record at the text's precision and unit (m/s from a speed, % from a battery or a reserve,
  m from a distance, s and min from a duration, a course from `course_deg`, a time from a
  timestamp); quoted text and the record's own strings are taken as they are. It does not prove
  that the record holds what the bridge served (the record is the engine's own reading; nothing
  compares it with the audit log's state at that moment), that a template picked the right field
  for its word (a number shown by another field of the same unit passes), or that a quoted reason
  is true. It is internal consistency, text against record, and no number from nowhere: the
  reviewer's "0.0 m/s course 000" for a track with no velocity now fails it.

**Methodology.** Harbor watch is a **tuning set** for the hysteresis of leaving the watch
(`leave_area_factor` 1.1, `leave_cpa_factor` 2, `leave_tcpa_factor` 1.5, the additions above)
and for the materiality of a closest approach (half the CPA alert, the changes above): those
values were set from what Harbor watch's recordings and first runs showed. Harbor watch results
on those behaviours are in-sample, and a situation the values were not set on is needed to judge
them.

| change | before | after |
|---|---|---|
| a person re-tasks a vehicle we sent home | `safety.low_reserve` sent rtl again the next tick | not again while someone else's order newer than our rtl runs on it; once more if the battery falls under the policy's own reserve (not the reserve plus 5), saying so; then only when that order no longer stands and no later one runs |
| recovering on its own | RTL, QRTL, LAND, SMART_RTL, AUTO_RTL | every return, landing, docking and surfacing mode of the bridge's mode tables (QLAND, LOITERALTQLAND, AUTOLAND, AUTOROTATE, DOCK, SURFACE added); a test scans `mav.rs` |
| the ledger | kept across bridge runs | bound to its bridge run (`get_changes`' `run`): another run's ledger, or one with no run, is set aside renamed beside the new one, and a fresh one started |
| an order of ours that failed | proposed again at once | backs off its key and its vehicle (10 s doubling to 160 s); a retry within 10 minutes names the failure |
| refused at submission | neither cooled nor backed off | backs off like a failing preview, never cleared by the refusal, logged with the bridge's reason |
| a vehicle `safety.stuck` held on our task | given the same task the next tick | not that play and target until the alert has cleared and `stuck_retry_s` (300 s) more; free for other tasks |
| `isr.release` | resumed the vehicle's previous order, failed or not | resumes an order of ours only if it was running when ours superseded it and its premise still holds; else a hold |
| withdrawn by the engine | the key cooled 30 s (the addition above) | (play, target) cools 30 s on every vehicle, as a rejection does |
| withdrawals | any card of ours waiting (pending, approved, queued) | only a card still pending approval, and only if it still is at that moment (`withdraw_intent`'s `if_state`, checked under the engine's lock); an approved or queued card whose premise went is said in the panel and left |
| the inbox budget | applied before the preview, by task kind and a remembered tier | applied after it: the preview says the tier, only a human-tier result spends the budget, P0 exempt by class |
| a look for classification | whenever the classification was unsettled | only while no EO/IR or sonar of ours detected the track in the last 10 s (fast and closing still count); Harbor watch's look now fires on fast and closing alone |
| a running armed overwatch | never released | released like a look |
| an engagement in play | a hold or an rtl still went to the platform | no order of any kind (a dispatched one supersedes the engagement, rules 7); the panel says what the playbook would have done |
| a status line | logged on every change | logged once it has held 3 ticks |
| the stuck threshold (table above) | an alert for 30 s | an alert in our reads for 30 s |

## Fix round 2, 2026-10-09 (a second review; the targets above unchanged)

A second review confirmed every round-1 fix (reverting each one failed its test) and found side
effects of round 1. Each item has a test in `agents/tests/test_playbook_review2.py` that failed
before its fix.

**Correction to round 1's table** (the "an engagement in play" row): round 1 gave a platform with
an engagement in play no order of any kind, P0 included. That was wrong. The bridge already sends
any agent's order on such a platform to a person, who must acknowledge that it stops the
engagement, and it raises no battery alert of its own, so a withheld rtl reached nobody. A P0
command (`safety.low_reserve`, `safety.stuck`) now goes on and becomes a human card, its rationale
naming the engagement. ISR, link and hygiene orders stay withheld, with the `withheld` line.

| change | before (round 1) | after |
|---|---|---|
| a card the inbox budget holds | previewed again every tick; held cards filled the cap of three and an auto-tier filler starved (about 1,800 audit lines an hour per held card) | after a human-tier preview the budget held, that play and target is not previewed again until the next token is due or a card of ours waiting then is decided |
| a failed order's vehicle back-off | held back P0 commands on that vehicle | P0 exempt; a P0 command's own key still backs off when it fails |
| a withdrawal for the vehicle's sake | cooled (play, target) on every vehicle | cools that vehicle for that target; only a withdrawal because the premise went cools the pair |
| the ledger after a crash and resume | set aside because `run` changed | verified at every start and feed reset, whatever `run` says: a card kept only if `explain_intent` shows it still ours with the same task and vehicles; memory per vehicle only for the same platform and callsign under the uid; memory per track dropped across a bridge restart (a new run's T-0008 is another contact), said in the panel |
| a later order after our rtl | "someone else's order", a null task read as none | worded by origin (an operator's; another agent's, approved by the policy or by a person), any of them respected; a null or unreadable task is unknown and never re-raises; a supersession is not called a failure |
| a look under sensor coverage | suppressed while an EO/IR or sonar detection was 10 s fresh | suppressed only while the classification rose by 0.02 within 30 s, and at most 120 s per track; an identity conflict always counts |
| a release resuming an order an earlier release copied | the copy's premise always held (a release has none) | the premise of the play that first made the order is checked, through the chain of releases; none known, a hold |
| an approved or queued armed card on a suspect vehicle | left with a `left` line | until murmur-su0 lands, withdrawn from that state only, "withdrawn (armed, nav suspect, murmur-su0)" |
