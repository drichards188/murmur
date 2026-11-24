# One decision, many releases: the salvo

> **Spec (bd murmur-4nik), revised 2026-11-20, built as decision rules 35, policy schema 10.** The
> first version planned a replica of the US Army's HIV3 demo with new platforms and a new munition;
> two build attempts on it were stopped by safety safeguards, and the owner narrowed the scope to
> the command and control on weapons Murmur already has ("do what you can, put placeholders where you
> have to"). The salvo (stage 1) is built and, since 2026-11-24, the default decision rules (section 10);
> the group control of ordinary tasks (stages 2 and 3, docs/SWARMS.md) is
> rules 34, the default since 2026-11-20. The owner's choices of 2026-11-20 (bd murmur-4nik.2 and .5) replaced
> the Salvo range situation of the first revision by the HIV3 salvo situation (section 7). Every
> invented number is marked [E].

On 12 November 2026 the US Army launched about 18 drones from an unmanned ground vehicle in what it
called an "autonomous strike", and nobody has said whether that was one human decision or eighteen
(docs/research/hiv3.md part 1). That is the question Murmur exists to make measurable, and Murmur
cannot express it: an engagement names one platform and one track and releases one effect on two
signatures (`intent.rs:518`, DECISIONS.md "One engagement per platform"), so several releases are
several nominations and twice as many signatures, one after another.

This spec adds the **salvo**: one plan, signed by two people, that releases up to N rounds of a
weapon the platform already carries, bounded by the target, the effect circle, the window and a
round ceiling the scenario's policy sets, with a Cease, every release checked at its own time. It
is shown in a new situation, **HIV3 salvo** (section 7), on the drone-launcher UGV the owner added
for it (bd murmur-4nik.5: an abstract effect, no figure of what a hit does) against two static
vessels. The salvo works on any catalog weapon that flies and has rounds, so a weapon added later
works with it unchanged. docs/SWARMS.md section 6 deferred exactly this ("a salvo ... turn[s] one human
decision into many effects ... and need[s] its own spec").

## Decisions

Decided (the owner, 2026-11-20):

1. **Place: the Golden Gate.** The range is the open-water engagement zone west of the bridge
   (`sim/areas/golden-gate.geojson`), whose areas and links every situation already uses.
2. **The platform and weapon the demo needs.** The salvo is tied to no particular weapon: any
   catalog weapon that flies and has rounds salvoes the same way, so the situation names the
   platform and weapon it is shown on, and where the demo needs something Murmur lacks it is added
   and the situation says so. For this one the owner added (2026-11-20, bd murmur-4nik.5) the
   drone-launcher UGV (`[ugv_launcher]`) and its HIV3-class launcher, from the rounded kinematic
   figures of docs/research/hiv3.md, drawn as small quadcopters that pop up, cruise and dive on the
   console's map, their effect abstract (no hit, kill or damage figures).
3. **A salvo is one bounded decision.** One nomination and two signatures cover up to N rounds,
   bounded by the target (one declared track), the effect circle, the window, the round ceiling and
   a Cease; the engine judges every round at its own release. **The scenario's policy sets the most
   rounds one plan may carry** (rules 33's single source), so a ceiling of 1 is today's chain and
   an evaluation can set the two side by side.
4. **A spent one-way striker leaves the world live**, as headless (done: 58ee062, DECISIONS.md
   2026-11-20).

Adopted as recommended (the owner, 2026-11-20: "do what you can"):

5. **A ceiling above 1 is a loosening.** The standing value is 1; a scenario that raises it says why
   under `loosened` (rules 33), and the card, the record and the report state it. It multiplies what
   one human decision releases, which is what the CCW draft's "number of engagements" limit is about
   (docs/research/hiv3.md [23]). Guns are unchanged: rules 26's bursts stay bounded by the magazine
   and the window, as decided.
6. **Rounds in flight are committed.** A released round cannot be recalled, as a missile today;
   Cease stops further releases only.
7. **A round whose checks fail at its release is held, not the plan voided**, as a gun's unseen
   target holds fire (rules 26): the fire waits, logged with the reason, and goes on when the
   round's checks pass, until the window closes, the ceiling is reached, the target leaves the
   picture or someone ceases.

Chosen in this revision (the owner may change them):

8. **The spacing between releases is the plan's, not a weapon figure.** `[rules.engage]
   salvo_spacing_s`, a standing minimum of 5 s [E]; a nomination may ask for more, never less. It is
   how fast one decision releases, a control setting, and it needs no new figure in the catalog. A
   file that sets it below the standing 5 s is a loosening too (the owner, 2026-11-20: the HIV3
   situation asks 2 s), named in `loosened` like the ceiling.
9. **The targets are declared by an operator**, the existing path (DECISIONS.md: any
   operator may declare any track): the person live, a scripted `declare` step on autoplay, the brief
   saying it is the range's own procedure (there is no hostile act on a range).

## 1. Shape

The gun's firing window (rules 26, DECISIONS.md "Guns: bursts from a counted magazine") is the
model: one signed plan, many releases, "no new human step per burst ... and every safeguard per
burst". The salvo extends it to rounds that fly. Its gate (`SALVO_SINCE`) takes the next free
rules version when it is built (rules 34 went to the group orders, so it is **rules 35**, policy
schema 10), with `RULES_NEWEST` one ahead (CLAUDE.md "A rules version built in stages").

- `Task::Engage` gains `rounds` (how many the plan may release; absent is 1, today's). REST and MCP
  `propose_intent` take it; the nomination form asks for it beside the weapon.
- The policy: `[rules.engage] max_rounds_per_plan` (standing 1, floor 1; above 1 a loosening,
  decision 5) and `salvo_spacing_s` (decision 8). A nomination asking more rounds than the ceiling,
  or than the platform has left of the weapon, is refused, saying which.
- A salvo needs a weapon with a flight (rules 20 and 24: a missile, a bomb, a torpedo); a gun keeps
  its bursts, and a `self` weapon is the platform, one round.
- The preview gives the plan's release times (the first, then each spacing) and each round's
  effect time (its release plus its flight); the frozen plan and the picture token carry `rounds`,
  the spacing and the ceiling, so a signer signs exactly that.

## 2. Its life

- **The release is round 1**, judged as today's release (the weapon's band, keep-outs and
  alignment at its effect time, the window). Then each spacing the engine steps the fire (on the
  tick, where `step_fire` runs): **each round is a release of its own**, judged at its own time:
  target declared, identity at the bar, the plan's circle, deviation from the frozen plan,
  keep-outs at its effect time (ours, protected tracks, lost contacts, silent vehicles' regions),
  the link, navigation, rounds left, the preload read back. A failing round is held, logged with
  its reason (decision 7).
- **Each round has its own flight** (`Engagement.munition` becomes a list; the effect order and the
  console's munition layer too) and its own effect on the truth side, drawn on its own stream
  (`effect:<intent>:r<n>` [E: the name]) with the weapon's existing figures.
- **The fire ends** when the rounds asked for are out, the ledger is empty, the window closes, the
  target leaves the picture, someone ceases, or the assessment says ceased or unknown (section 3).
  `engage.fire_ended` with the reason, as a gun's.
- **Cease** is the gun's stop: any operator, at once, from the card, REST or the inbox; it stops
  further releases. Rounds in flight go on (decision 6) and are assessed.

## 3. Assessment

Observed from the **last impact**, not the last release (a gun's runs from its last burst: its
bursts have no flight). While rounds are in flight a target seen again is not a verdict. Rounds
still in flight when the target is out of play strike where it was and find nothing: the card shows
them as rounds that had nothing to strike.

## 4. Audit, replay, tampers, scenarios

- Events: `engage.round` (round n, its flight, rounds left) beside `engage.burst`; the hold reasons;
  `engage.fire_ended`. Every engagement decision keeps logging the `rounds` fact (rules 24's
  ledger).
- Replay recomputes each round from the picture it was released on, binds rounds to the spacing,
  and checks the count against the plan and the ceiling against the logged policy. The engagement
  stays one platform (`spec.uids.first()` everywhere stays right).
- Tampers: a round after a cease, a round past the plan's count, a ceiling edited in the logged
  policy.
- Scenarios 174 to 180; runner keys `rounds` on `nominate`, `cease_at_round`, `expect_rounds`,
  `expect_rounds_held`.

## 5. Console and MCP

- The card: rounds asked, released, in flight, struck, held (with reasons), the ceiling and why it
  was raised (`loosened`), and Cease. The map: every round in flight.
- MCP: `propose_intent`'s `rounds`; `get_nominations` and `explain_intent` carry the fire's
  progress. A new parameter changes the tool list, so the headless golden and paused digests are
  re-taken at the flip (CLAUDE.md "Weather on the map": the MCP note), not before.
- Agents propose a salvo and never sign it, as today.

## 6. What does not change

The chain: a declared target, a nomination, two signatures by operators with engagement authority,
never the proposer, never an agent, a step-up at each signature in a credentials run. One platform
and one track per engagement, one engagement per platform. Every safeguard per round.

## 7. The situation: HIV3 salvo (`sim/situations/hiv3-salvo.toml`)

The owner's choices of 2026-11-20 (bd murmur-4nik.2 and .5) replaced the MQ-9B-class "Salvo range"
of the first revision:

- **Fleet** (`sim/fleets/hiv3-salvo.toml`): HIVE 1, the drone-launcher UGV (`ugv_launcher`, one
  weapon, the "HIV3-class launcher": 18 rounds, 300 to 10,000 m, 20 m/s, dive profile, an abstract
  effect), on the bluff at the bridge's south end where shore-battery's BULL 1 stands; HUNTER 1, a
  sensor ship, patrolling 900 m or more from either target, whose camera identifies them and covers
  each strike point for the assessment. An armed platform gets no opening order.
- **Targets** (`sim/scenarios/hiv3-salvo.toml`): two small fast boats at anchor, static, inside the
  engagement zone, 1,720 m and 1,170 m from HIVE 1 and 670 m apart (more than either keep-out, the
  effect radius plus 200 m); the range is closed to shipping, so no neutral can hold a keep-out. A
  strike is abstract: whatever is inside its radius at the impact leaves play, nothing is drawn.
- **Policy** (`sim/situations/policies/hiv3-salvo.toml`, extends `watch.toml`): `max_rounds_per_plan`
  18 and `salvo_spacing_s` 2, each named under `loosened` with its reason. Run with the standing
  policy it is today's chain: one round a plan, 5 s apart.
- **Timeline:** a scripted declaration of each target (op-planner, the note saying it is a script: a
  range has no hostile act, so no rule declares and no person is needed; the situation is therefore
  autoplay, and a live run refuses a `declare` step); plan 1 asks 6 of the 18 rounds on target A
  (12 left), op-2 and op-3 sign once, the rounds go 2 s apart, every one in the air before the first
  strikes (flights of some 85 s), the first strike removes the target and the other five find
  nothing, the assessment runs from the last impact; plan 2, once HIVE 1 is free (one engagement per
  platform), asks 6 on target B and a scripted operator ceases it after the third round (9 left):
  rounds 4 to 6 are never released, the three in flight strike. The step keys are `rounds`,
  `spacing_s` and `cease_after_rounds` (eval/engage.py `EngageAdapter.cease_after`: a scripted
  op-planner, never a person, aborts the engagement once that many rounds are out).
- **The placeholder:** the brief and the file say that the platform and weapon are the scenario's
  choice, and that any catalog weapon with rounds and a flight salvoes the same way. Older rules
  refuse its policy, saying so (a raised ceiling or a shorter spacing is read from rules 35 only).

## 8. What it lets us measure

The same situation at ceiling 1 and at ceiling 18: time from declaration to effect, rounds released
against what the target needed, rounds that struck nothing, holds and their reasons, whether a Cease
came and when, and whether the signers could say what they had approved. No paid or scored runs
until the evaluation's scoring is approved (docs/PLAN.md); this makes the comparison possible, it
does not quote results.

## 9. Prior art and doctrine

From docs/research/hiv3.md part 5 (and docs/SWARMS.md 12). The CCW group's rolling text of 18
December 2025 limits "the types of targets, duration, geographical scope and **the number of
engagements** that LAWS can undertake autonomously" [23]; an Army official told Defense One (13
October 2026) that at "hundreds of rounds" a human cannot decide each one and the Army wants humans
"on the loop" [25]; DoDD 3000.09 lets a semi-autonomous system engage "individual targets or
specific target groups" an operator selected (read in full for docs/SWARMS.md 12; how DoD reads
"target groups" is unclear, [30, 31]); fire missions clear many rounds with one decision bounded by
target, area, time and round count, ended by "cease" or "end of mission" [32, low: from memory, not
read]. The salvo's bounds and its Cease follow from these; the evaluation (section 8) is what
Murmur adds.

## 10. Stages

| stage | what | moves decisions | state |
|---|---|---|---|
| 0 | spent one-way strikers leave the world live | no | done (58ee062) |
| 1 | the salvo (sections 1 to 6) and the HIV3 salvo situation (section 7) | rules 35 (policy schema 10), the default since 2026-11-24 (the owner's go-ahead) | built on branch `salvo-volley` (brought up to main on 2026-11-24): the engine, replay, scenarios 174-180, REST, MCP, the console card and map, the situation, the situation headless (`eval/headless/tests/test_salvo.py`), the flip with its fixtures and re-taken digests; a live run of the situation and the browser are not yet checked |
| 2 | `transit` (docs/SWARMS.md 3.2): go to a point and hold, for any vehicle | rules 34 | building with stage 3 |
| 3 | group control of ordinary tasks (docs/SWARMS.md: group orders, member classes, re-forming; bd murmur-3kqi) | rules 34 | building |

## 11. Not checked

- A live run of the HIV3 salvo situation (stack, launcher, the scripted Cease through the runner) and
  the console in a browser: the engine, replay and scenarios are checked, the situation loads and
  its policy is read under rules 35, but the stack was not started where it was built.
- Headless, the situation is checked on seed 1 only (world `golden-gate-v3`, so that it runs on
  the physical sensing model under its own policy, as live): the timeline's `rounds`,
  `spacing_s`, `after_step` and `cease_after_rounds` as the live runner takes them, and on seed 1
  the director's notes as written (plan 1's six rounds, plan 2 ceased after its third, 9 left), the
  log replaying with its anchors.
- Whether the abstract effect's "first strike removes the target, the rest find nothing" is the
  picture the owner wants of a series; a weapon with `effects` draws each round on its own stream
  and can leave a target standing.
- A salvo's `target_gone` end needs the tracker to drop the track (180 s unseen), so with flights of
  tens of seconds the assessment usually ends the fire first (scenario 178 uses a 9 km flight to
  reach it).
