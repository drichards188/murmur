# One decision, many effects: the HIV3 replica and the salvo

> **Spec, decided in part (the owner, 2026-11-20; bd murmur-4nik).** Stage 0 (no rules change)
> is being built. Stages 1 and 2 (decision rules 34) wait for the owner's read of this file.
> Stage 3 (carried drones, a swarm strike) gets its own spec on top of docs/SWARMS.md. Every
> invented number is marked [E]; figures with a source cite docs/research/hiv3.md [n].

On 12 November 2026 the US Army showed at Grafenwoehr an unmanned ground vehicle (R3NEGADE) that
drove out of a 20-foot shipping container, while a heavy-lift drone (TRV-150) dropped smoke on the
target; the vehicle's HIV3 launcher then sent about 18 FPV strike drones in what the Army called an
"autonomous strike" [1, 2]. Nobody has said who chose the target, who authorized the strike, or
whether 18 drones were one human decision or 18 [1]. That is the question Murmur exists to make
measurable, and Murmur cannot express the demo today: an engagement names one platform and one
track and releases one effect on two signatures (`intent.rs:518`, DECISIONS.md "One engagement per
platform"), so 18 drones would be 18 nominations and 36 signatures, one at a time.

This spec does three things: replicates the demo as a situation at War Thunder fidelity (stage
0, today's engine), adds the **salvo**, one signed plan bounded by target, place, time and a round
ceiling, that releases many rounds each checked at its own strike (stage 1, rules 34), and sets out
what carried drones and swarm strikes need later (stage 3). docs/SWARMS.md section 6 deferred
exactly this ("a salvo ... or a swarm strike ... turn one human decision into many effects ... and
need their own spec"); this is that spec for the salvo, and the start of it for the swarm.

## Decisions for the owner

Decided (the owner, 2026-11-20):

1. **Place: a Golden Gate land range**, in the Marin Headlands, with its own areas and links. No
   Grafenwoehr world (docs/research/hiv3.md part 6 sizes one, should it be wanted later).
2. **An HIV3 drone is a round of the launcher's weapon** (route A), not a vehicle. The launcher
   UGV is one platform; each drone a munition with a flight (rules 20), counted in the rounds
   ledger (rules 24), fired from a ground firing position (rules 28). Carried drones as vehicles
   (route B) are stage 3.
3. **A salvo is one bounded decision.** One nomination and two signatures cover up to N rounds,
   bounded by the target (one declared track), the effect circle, the window, the round ceiling
   and a Cease; the engine judges every round at its own strike. **The scenario's policy sets the
   most rounds one plan may carry** (rules 33's single source), so a ceiling of 1 is today's chain
   and an evaluation can set the two side by side.
4. **A spent one-way striker leaves the world live**, as it does headless (section 7). Rules 30
   presumes it lost on its own effect and a person confirms, as decided (DECISIONS.md D6).
5. **Build order:** this spec, then stage 0 at once; stages 1 and 2 after the owner's read.

Open, each with a recommendation:

6. **Is a ceiling above 1 a loosening?** Recommended: **yes.** The standing value is 1; a scenario
   that raises it says why under `loosened` (rules 33), and the card, the record and the report
   state it. It multiplies the effects one human decision releases, which is what the CCW draft's
   "number of engagements" limit is about [23]. Alternative: a standing value above 1 (6, one
   volley [E]), unremarked. Guns are unchanged either way: rules 26's bursts are bounded by the
   magazine and the window, as decided.
7. **Rounds in flight are committed.** Recommended for route A: a launched round cannot be
   recalled (as a missile today); Cease stops further launches only. A stated shortcut: a real FPV
   can be waved off before its terminal lock. Route B (stage 3) revisits it.
8. **A round whose checks fail at its launch is held, not the plan voided.** Recommended, as a
   gun's unseen target holds fire (rules 26): the fire waits, logged with the reason, and goes on
   when the round's checks pass, until the window closes, the ceiling is reached or someone ceases.
   Alternative: the first failing round ends the fire.
9. **The smoke is display, plus an optional camera cue**, from the marker drone's own `mark` order
   (stage 2); no hit factor, since no public figure supports one [5]. Never a timeline claim of
   smoke (the owner's rule: situations give orders, never effects).
10. **Autonomous terminal guidance changes hit figures only**, never authority: a new `guidance`
    string with its own figures and night, fog and speed factors on the truth side (section 3.3).
11. **The range target is declared by an operator**, the existing path (DECISIONS.md: any operator
    may declare any track): the person live, a scripted `declare` step on autoplay. A destroyed
    target leaves play, so rounds still in flight then miss: honest overkill, shown on the card.
12. **Hand-flown and remotely operated are guidance figures only.** Murmur flies every platform by
    autopilot and releases on the signed plan; the demo's hand-flown FPV is today's `[fpv]` with
    `operator_video_day`, the Javelin today's `[ugv_armed]`. A stated shortcut.
13. **Three targets at least 300 m apart** (the 275 m keep-out: 75 m radius plus 200 m), one for
    the salvo, one for the hand-flown FPV, one for the Javelin, so no strike blocks another.

## 1. The demo, and what is public

| element | reported | the replica |
|---|---|---|
| launcher vehicle | R3NEGADE UGV, out of a 20-ft container [1, 2]; no public figures | R3NEGADE-class: about 1.5 t, 6 m/s road, 3 m/s off road [E from THeMIS-class, 4.1]; spawns at the container's door (stage 0), drives out by `transit` (stage 2) |
| launcher | HIV3, "six-tube" (a caption, unverified) | 6 tubes x 3 = 18 rounds [E]; one launch every 2 s [E: China's Swarm-2 at 3 s the only public interval]; no reload in the scenario (20 min by hand [E]) |
| drones | FPV; count 18 (Stars and Stripes only; the Army gives none) [2] | 10-inch-class tube-launched quad: cruise 20 m/s, dash 35 m/s, 10 km, 1 kg warhead [E, 3.1]; autonomous terminal lock from about 500 m [3.2] |
| marking | TRV-150 dropped smoke on the target [1]; "to mark the target" in one outlet only [5] | TRV-150-class: 68 kg payload, 30 m/s, 36 min [4.2]; a smoke marker by its own `mark` order (stage 2); display, optional cue |
| target | "a designated target"; type and distance not public | three static trucks [E], 2 to 4 km from the launcher [E], at least 300 m apart |
| beside it | a hand-flown FPV strike (173rd Airborne); a second UGV with a remotely operated Javelin (fired or not: reports differ) | `[fpv]` and `[ugv_armed]` as they are |
| human role | not reported; "autonomous strike" undefined | the chain as decided: a declaration, a nomination, two signatures, never the proposer, never an agent |

## 2. Fidelity

War Thunder level: each figure is a rounded public number, or a public comparable's marked [E],
with no more precision than its source supports, and no attempt to model a real system's
engineering. Murmur models the decision, not the weapon: what matters is that the figures put the
person in a recognizable situation (how long a salvo takes, how far it flies, what can go wrong),
not that a hit chance is right to the percent. Where the Army gave nothing (the launcher, the
drones, the target), the replica says what it assumed, in the brief and in the catalog's notes.

## 3. The replica on today's engine (stage 0)

No rules change, no decision moves. It runs the demo end to end honestly, every effect a weapon a
platform carries, released by the engine on a plan two people signed, drawn on the truth side.

### 3.1 Platforms (sim/platforms.toml)

- **`ugv_launcher`, "Drone-launcher UGV (R3NEGADE-class)"**: armed, a stand-in `ugv` (no SITL: a
  ground vehicle's firing position works today, shore-battery's BULL 1), a day and thermal sight
  [E] able to identify a truck at the range's distances (the FPV's own camera reaches a small ground
  target only at 130 m and identifies nothing in time), link `mesh` [E]. Loadout: **"HIV3-class
  launcher"**, `kind = "missile"` [a stated shortcut until stage 1 decides whether a `drone` kind
  is needed], count 18, `min_range_m` 300 [E], `max_range_m` 10,000, `speed_mps` 20, `profile =
  "dive"`, guidance `autonomous_terminal_eo`, warhead 1 kg [E], targets ground and surface, effects
  by kind (3.3). Rules 28 gives it a firing position: where it stands when the target is within its
  band.
- **`trv150`, "Heavy-lift drone (TRV-150-class)"**: unarmed copter stand-in, 30 m/s, 36 min, a
  small camera [E] (no public figure; it navigates by one), link `mesh`. Its marker is stage 2's.
- `physical_reach_m` regenerated for both (`write_physical_reaches`).

### 3.2 The range

- **Place:** the Marin Headlands' open valleys (Rodeo or Gerbode, inside tidewater's Golden Gate
  box and the approved area) [inferred; the line of sight is to be computed]. Hawk Hill very
  likely hides them from the Crissy Field radios, so the situation brings **its own links file**
  with a `[[ground_station]]` on the Headlands (several stations: CLAUDE.md "Links"), checked with
  `scripts/layout_reach.py`, and **its own areas directory** (a land `engagement_zone` over the
  range; the Gate's only zone is open water), never in `sim/areas`.
- **Vehicles at the DEM's height** (the UGV altitude gotcha): every UGV spawned at tidewater's
  `terrain.npz` height, or it reports 0 m and its link is judged from sea level.
- **Targets:** three static `truck` contacts [E: no armour kind exists; trucks carry figures for
  every weapon here], no traffic on the range, the contacts' seed chosen for the default run's
  intent numbers (hit draws are seeded by it).

### 3.3 Autonomous terminal guidance

A new guidance string, `autonomous_terminal_eo` [E], on the HIV3 round. Figures in
docs/research/weapons.md as a 10.3 column and 10.5 rows: p_hit against a truck or car about 0.55
[E: between the one field report of about 50 % in 20 uses and the vendors' "2 to 4 times" a
hand-flown FPV's 20 to 40 %, 3.2], night x0.5 (a day camera; x1.0 with thermal), fog x0.5, the
speed bands as the FPV's. The truth side's three factor tables (`crates/murmur-contacts/src/
damage.rs:249-285`) must learn the string: an unknown string falls through to factor 1.0 and gets
no penalty at all (a finding: a test that every catalog guidance string is in all three tables).

### 3.4 The situation (`sim/situations/hiv3-range.toml`)

- Fleet: the launcher UGV, `ugv_armed` (Javelin), `fpv` (hand-flown, `operator_video_day`), the
  TRV-150; all forced stand-ins.
- Opening: the brief names the container and the range; the TRV-150 flies over the salvo's
  target and clears to at least 300 m [E] before the first strike (an orbit or patrol stand-in
  for stage 2's mark; it must be outside the keep-out at every strike, or the strike holds).
- Declarations: the three targets declared by the person (live) or a scripted `declare` step
  (autoplay), saying it is the range's own procedure; there is no hostile act on a range.
- Strikes: the launcher's drones, **each its own nomination and two signatures**, released one at
  a time (one engagement per platform; the launcher is free again once a round is released). As
  many as fit the situation's time (at least six, one volley [E]); the brief says how many and
  why. Then the hand-flown FPV and the Javelin on their own targets.
- Who acts: the person proposes and declares live (`op-1`, never a script); `op-2` and `op-3`
  sign (scripted signers, as the tutorial); autoplay's `agent-scripted` nominates.
- What it shows: the cost the salvo question is about. N drones take 2N signatures in series,
  and the target is likely destroyed or assessed before most of them fly.

### 3.5 Checks

`make check`, then one autoplay run of the situation (`make situation NAME=hiv3-range
OPERATOR=autoplay AI=playbook`) read from its summary and audit log: every nomination signed by
two, each release a weapon round in the ledger, each strike drawn, nothing scripted.

## 4. The salvo (stage 1, decision rules 34)

The gun's firing window (rules 26, DECISIONS.md "Guns: bursts from a counted magazine") is the
model: one signed plan, many releases, "no new human step per burst ... and every safeguard per
burst". The salvo extends it to rounds that fly. Gate `SALVO_SINCE` (34), built in stages with
`RULES_NEWEST` one ahead (CLAUDE.md "A rules version built in stages").

### 4.1 Shape

- `Task::Engage` gains `rounds` (how many the plan may launch; absent is 1, today's). REST and
  MCP `propose_intent` take it; the nomination form asks for it beside the weapon.
- The weapon's catalog entry gains `launch_interval_s` (2 for the HIV3 [E]); a weapon without it
  cannot salvo (`rounds` > 1 refused, saying why).
- The policy: `[rules.engage] max_rounds_per_plan`, standing 1, floor 1; a scenario raising it
  lists it under `loosened` (decision 6). A nomination asking more is refused, naming the ceiling.
- The preview gives the plan's launch times (release, then every interval) and each round's
  strike time (launch plus its flight from the firing position, rules 20 and 28); the frozen plan
  and the picture token carry `rounds`, the interval and the ceiling, so a signer signs exactly
  that.

### 4.2 Its life

- **The release is round 1**, judged as today's missile release (slant range in the band, keep-
  outs and alignment at its strike, the window). Then each `launch_interval_s` the engine steps
  the fire (`step_fire`'s place on the tick): **each round is a release of its own**, judged at
  its own strike time: target declared, identity at the bar, the plan's circle, deviation from the
  frozen plan, keep-outs at its strike (ours, protected tracks, lost contacts, silent vehicles'
  regions), the link, navigation, rounds left, the preload read back. A failing round is held,
  logged with its reason (decision 8).
- **Each round has its own flight** (`Engagement.munition` becomes a list; the effect order and
  the console's munition layer too), its own draw stream on the truth side (`effect:<intent>:
  r<n>` [E: the name]), its own collateral draws within its blast radius, ours included.
- **The fire ends** when the rounds asked for are out, the ledger is empty, the window closes,
  someone ceases, or the assessment says ceased or unknown (4.3). `engage.fire_ended` with the
  reason, as a gun's.
- **Cease** is the gun's stop: any operator, at once, from the card, REST or the inbox; it stops
  further launches. Rounds in flight go on (decision 7) and are drawn and assessed.

### 4.3 Assessment

Observed from the **last impact**, not the last launch (a gun's runs from its last burst: its
bursts have no flight). While rounds are in flight a target seen again is not a verdict. A target
destroyed by round k leaves play; rounds k+1 and on strike where it was and draw against what is
there (a miss; collateral as drawn): the card shows them as rounds that had nothing to strike.

### 4.4 Audit, replay, tampers, scenarios

- Events: `engage.round` (round n, its flight, rounds left) beside `engage.burst`; the hold
  reasons; `engage.fire_ended`. Every engagement decision keeps logging the `rounds` fact (rules
  24's ledger).
- Replay recomputes each round from the picture it was released on, binds rounds to the interval,
  and checks the count against the plan and the ceiling against the logged policy. The 14 sites
  that take `spec.uids.first()` stay single-platform (route A is one platform).
- Tampers: a round after a cease, a round past the plan's count, a moved impact, a ceiling edited
  in the logged policy.
- Scenarios from 158; runner keys `rounds` on `nominate`, `cease_at_round`, `expect_rounds`,
  `expect_rounds_held`.

### 4.5 Console and MCP

- The card: rounds asked, launched, in flight, struck, held (with reasons), the ceiling and why it
  was raised (`loosened`), and Cease. The map: every round in flight.
- MCP: `propose_intent`'s `rounds`; `get_nominations` and `explain_intent` carry the fire's
  progress. A new parameter changes the tool list, so the headless golden and paused digests are
  re-taken at the flip (CLAUDE.md "Weather on the map": the MCP note).
- Agents propose a salvo and never sign it, as today.

### 4.6 What does not change

The chain: a declared target, a nomination, two signatures by operators with engagement authority,
never the proposer, never an agent, a step-up at each signature in a credentials run. One platform
and one track per engagement, one engagement per platform. Every safeguard per round.

## 5. Drive-out and mark (stage 2, rules 34)

- **`transit`** (docs/SWARMS.md 3.2's design): go along a route to a point and hold there. The
  launcher UGV's drive out of the container to its firing position; being armed, it waits for a
  person (the armed-platform tier). Policy schema 9 (its `when.kind`).
- **`mark`**: a vehicle whose catalog entry carries a marker (the TRV-150-class: smoke, count [E])
  flies to a point, drops it, and leaves; the truth side holds a marker (place, time, a life of
  some minutes [E]) and the map draws it. Optionally a cue to our cameras' payload managers toward
  the point (the physical model already cues by detections), which must stay independent of the
  sensing step's thread count (`tracks::tests::sense_par`). Never a decision input; no hit factor.
  An obscurant (visibility down at the point) is left out: no public figure, and it would hurt our
  own target recency.

## 6. Carried drones and a swarm strike (stage 3, its own spec)

What route B needs, from the code audit (logs/hiv3/gap-audit.md, kept outside git):

- **Carriage.** A fleet key naming a vehicle's carrier; stand-ins that ride their carrier until
  launched (as `moored` holds until armed), in the stand-in engine and the headless fleet; the
  console folding them into the carrier. A stowed drone that never reports fails every engagement
  closed today (`snapshot.rs:180`): either it reports from its tube, or a new vehicle fact says it
  is carried (a rules version).
- **A salvo of platforms.** Member engagements under one signed plan (docs/SWARMS.md decision 1's
  group order, applied to engagements): each member a one-platform engagement, so preload, restore,
  spent marks and node loss stay per member.
- **Lanes and slots laid out by the engine** (SWARMS.md 3.1), logged and replayed; members exempt
  from each other's keep-outs only on their own lanes and slots (today every member would block
  every other: the keep-out skips only the one platform, `engage.rs:3060`).
- **Release at lock-on** (the lock range, not `arrival_m`), a wave-off before it, link loss before
  it a void as today.
- **Member classes against a minimum** (SWARMS.md 4.1), and the group-order faults of bd
  murmur-3kqi that a salvo of vehicles meets (one member's stale fact or failsafe failing the rest,
  shared lanes, altitude stacking, an 18-member card).

## 7. Spent strikers leave the world (stage 0)

Live, a one-way platform (FPV, Switchblade, Magura) keeps reporting after its own strike: the
effect excludes the striker (`crates/murmur-contacts/src/effect.rs:529`; the bridge's
`send_effect`) and the stand-ins act out only damage sent to them. The headless simulator takes it
out of the world (`crates/murmur-headless/src/sim/spec.rs:117`, `mod.rs:799-806`). Live will match:
the striker goes silent at its effect (the damage path's destroyed, which the stand-ins and SITL's
watcher already act out), rules 30 presumes it lost on its own effect, and a person with engagement
authority confirms (op-2 on autoplay where `confirm_node_losses` is set). Until confirmed it counts
in keep-outs as a drift region (`silent.rs`), as decided. It changes the live runs of
hostile-act-fpv, hostile-act-loitering-munition and hostile-act-naval-drone; the headless digests
do not move. A Magura that survives its own effect (`magura_pre_loadout`) keeps today's behaviour.

## 8. What it lets us measure

With the ceiling in the scenario's policy, the same situation runs at ceiling 1 (36 signatures for
18 drones, in series) and at 18 (one plan): time from declaration to effect, rounds launched
against what the target needed, rounds that struck nothing, holds and their reasons, whether a
Cease came and when, and whether the signers could say what they had approved. No paid or scored
runs until the evaluation's scoring is approved (docs/PLAN.md); the replica and the salvo are
built so the comparison is possible, not to quote results.

## 9. Prior art and doctrine

From docs/research/hiv3.md part 5 (and docs/SWARMS.md 12): no public source says who authorized
the Grafenwoehr strike. The CCW group's rolling text of 18 December 2025 limits "the types of
targets, duration, geographical scope and **the number of engagements** that LAWS can undertake
autonomously" [23]; an Army official told Defense One (13 October 2026) that at "hundreds of
rounds" a human cannot decide each one and the Army wants humans "on the loop" [25]; DoDD
3000.09 lets a semi-autonomous system engage "individual targets or specific target groups" an
operator selected (read in full for docs/SWARMS.md 12; how DoD reads "target groups" is unclear,
[30, 31]); fire missions clear many rounds with one decision bounded by target, area,
time and round count, ended by "cease" or "end of mission" [32, low: from memory, not read]. The
fielded and vendor systems found say a person assigns targets and authorizes, and software flies
(Auterion, DZYNE: "no fully autonomous swarming") [13, 18]. The salvo's four bounds and its Cease
follow from these; the evaluation (section 8) is what Murmur adds.

## 10. Stages

| stage | what | moves decisions | size [E] |
|---|---|---|---|
| 0 | catalog (`ugv_launcher`, `trv150`), the guidance string and its factors, weapons.md, spent strikers leave the world live, the `hiv3-range` situation (fleet, contacts, areas, links, policy, timeline, autoplay) | no | M |
| 1 | the salvo: `rounds`, the policy ceiling, per-round releases and flights, holds, Cease, assessment from the last impact; audit, replay, tampers, scenarios, runner, console, MCP; the situation re-taken as one plan | rules 34 | L |
| 2 | `transit` and `mark`; policy schema 9; the situation's drive-out and smoke | rules 34, flipped with stage 1 | M |
| 3 | carried drones and a swarm strike: its own spec on docs/SWARMS.md | 35 or later | XL |

## 11. Not checked

- The Headlands' line of sight to a station there and the UGV sight's reach to a truck by day
  (stage 0 computes both).
- Whether a one-point `transit` route parses for a static contact.
- The live claim that a spent FPV keeps reporting was read from the code, not seen in a run
  (stage 0 sees it before changing it).
