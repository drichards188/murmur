# Opponent weapons: a modelled shot instead of an abstract act

Status: **approved and built 2026-10-29** (bd murmur-wcvp), with the owner's decisions on the
open questions (section 11) and one change to the console (section 8: the estimated flight). The
design's measurement and the as-built one are ignored probes in
`crates/murmur-bridge/src/tracks/tests/weapon_probe.rs`; section 12 records what was built and how
it was checked.

Since 2026-11-06 (docs/AUTONOMOUS-NODES.md, bd murmur-y85j) a contact's weapons are its catalog
kind's (`sim/contacts.toml`) and it fires them by its own fire control when an `[[order]]` tells it
to attack (section 16). The per-contact `weapon = {...}` and the scripted `[[act]]` this document
began with are the old path: loaded only from a file that says why (`scripted_acts`), never in a
situation. Everything below about how a shot is flown, sensed and drawn holds for both.

## 1. Why

The owner, on the tutorial: "setup the opponent in that tutorial with a weapon it deploys instead
of the abstract. same as the hellfire model it in a way that safeguards dont trip. its a missing
part of the situation. how and when is it known that our unit has been attacked."

Today a hostile act (`[[act]]`, rules 9) is a time, an author and a victim. At that second the
bridge counts it observed by every radar, EO/IR or sonar that detected the author in the same
tick, places the author where the best of them did, and the engine attributes and may declare
(attribution 0.9 or more). Nothing flies. The console's act visual draws a 1 s streak from author
to victim that reads like a weapon's path though none was modelled, and the model cannot say
how, or when, the C2 learns that one of ours was attacked.

## 2. The decision in short

A contact may carry a weapon, and each act it commits launches it. It flies straight from the
shooter to its victim at its speed, and the strike is a time and a report. Without figures (as
built on 10-29) that is all: no warhead, blast, damage or miss.
With figures it is drawn by the damage model (section 13, 2026-11-02). Three cues can tell us,
each through the physical sensing model:

1. **the launch**, seen by an EO/IR channel that has the shooter in its field;
2. **the weapon in flight**, a small fast object our radars and cameras may plot;
3. **the strike**, known to the victim itself.

The earliest is when we know; the strike minus the earliest is the warning time. The weapon's
fixes form a *weapon picture* kept apart from the track picture, so the decision engine decides
nothing differently and **the rules version does not change** (section 7).

## 3. Truth (`crates/murmur-contacts`)

**The file.** Opt-in per contact; an act by an armed contact launches its weapon, any other act
stays abstract and behaves byte for byte as today (since 2026-11-06 the old path, loaded only under
a top-level `scripted_acts = "<why>"`; a live node is armed by its kind and ordered, section 16):

```toml
[[contact]]
id = "fastboat-raider"
# ...
weapon = { name = "ATGM-class missile", kind = "atgm", speed_mps = 250, range_m = 5000 }
```

**The figures** (rounded public figures, "-class" naming as the catalog's). A small fast boat's
plausible stand-off weapon is a crew-served anti-tank guided missile, as small craft have fired
at ships: the Kornet and Konkurs classes reach 4 to 5.5 km, flying them in about 20 s (200 to 275
m/s on average). Taken: **250 m/s, 5,000 m**, so the tutorial's 0.9 to 1.2 km takes 3.6 to 4.8 s.
Its signature, in the bridge beside the contact kinds (`atgm`), every figure [E here]:

| | value | basis |
|---|---|---|
| flight height | 4 m above the sea | a launcher about 2 m up to a ship's superstructure |
| radar cross-section | 0.01 m^2 | a 150 mm body nose-on; side-on a 1.2 m body is nearer 0.1 |
| critical dimension, contrast: head-on (its victim) | 0.15 m, 30 K | the body's diameter; the plume hidden behind it |
| side-on (anyone else) | 0.4 m, 100 K | sqrt(0.15 x 1.2 m); the motor's plume in view |
| the launch: flash, plume and smoke | 2 m, 300 K | a boost motor's plume |

**The flight.** At the act's second the shooter launches at a lead point: the victim's position
plus its velocity times the time of flight (one iteration; the victim moves some 20 m in 4 s). It
flies a straight line at constant speed and strikes at the lead point at a time fixed at launch.
The contacts process does not know where our vehicles are, so a truth message from a scenario
with weapon acts against our vehicles names them (`aim_at: [1]`) and the bridge's answer on the
simulation channel carries their positions and velocities (`ours`, the positions our vehicles
report, as an effect's `ours` already carries). Without weapons neither field exists.

**Range.** A launch beyond the weapon's range is refused at scenario load when the victim is a
contact (both positions are truth at the act's second). Against one of ours the position is
known only at the act's second, so it is checked then: the shooter holds its fire and the truth
log says so (`held: 6.2 km, beyond 5.0 km`). The tutorial's own test checks its fleet's patrol
leg against the boat's holding point and the weapon's range, so the shipped file never holds.

**The truth message** gains `weapons` (each in flight: id, act, shooter, name, kind, launch time
and point, lead point, speed, impact time, victim) and `impacts` (each strike, at the first tick
at or after its impact time). Both are left out when empty: a scenario without weapons sends the
same bytes as before. The evaluation truth log records them.

## 4. What our sensors make of it

All of it runs in the bridge's physical model (`tracks/physical`), with its own figures, its own
draws (salts apart from every contact draw) and the payload managers' pointing as it is. It reads
that state and never writes it: the same truth with and without a weapon gives the same contact
detections (a Phase 2 test).

**4.1 The launch.** At the tick containing the launch, every EO/IR channel with the shooter in
its field (a panoramic ring always; a gimbal only if its current field holds it and it is not
slewing) and in optical line of sight sees the launch with the channel's steady-state detection
probability for the plume (`eo::channel_p`, detect level): a flash is a transient that a camera's
change detection catches without the search time a still contact needs (ACQUIRE's tau 3.4 s is
for finding a contact in clutter). One draw per act, observer and sensor. Seen, it places the
author as an EO/IR detection would (15 m plus 0.5 % of range, laid off from where the vehicle
believes it is). The act is reported to the engine then, as today (`ActReport`, the same struct,
`sensors` naming the cameras): the engine attributes and may declare exactly as for an abstract
act. **What changes for a weapon act:** radar and sonar no longer observe it (a radar sees the
boat, not the shot; it may see the weapon). Abstract acts keep today's rule.

**4.2 The weapon in flight.** Each truth message covers the second since the last, and the
weapon's position at any instant is known from its launch, lead point and speed, so:

- *radar:* each scan's pass over the weapon's bearing inside the second is one Swerling 1 draw
  at that instant's geometry (`radar::pd`, the catalog's figures, the scan phase the contacts'
  radar step uses), against the sea clutter in its cell (it flies at 4 m, inside the cell; a
  Doppler radar sees it clear of the clutter's band, as a moving surface contact);
- *EO/IR:* each channel with it in its field accrues detection as the engine's looks do
  (`P^2 / tau` a second, in 0.1 s steps over the in-flight part of the second), head-on figures
  for its victim and side-on for anyone else; once detected, a fix each tick it stays in view.

A weapon fix carries the sensor's own error. Fixes go to the weapon picture only (section 5):
never to the tracker, never a cue for a camera (a weapon is gone before a cue's slew, settle and
dwell would see it, and a camera pulled off the boat would cost the boat's identification).

**4.3 The strike.** The victim knows it was struck. At the first truth tick at or after the
impact (within a second), the bridge logs it as the victim's own report: a `vehicle.event` of
kind `struck` in the audit log ("struck at 10:01:24.6Z"), whatever was seen before, and an inbox
alert. Only a hit is reported (the impact's `hit`, the damage model's draw; absent for a weapon
without figures, which strikes as before): nothing on an unmanned vehicle tells it rounds went
past, so a miss gives no event, no alert and no struck mark, and its weapon track ends lost as
its cues stop (bd murmur-aset). A wreck struck again reports nothing either. A strike on a
contact gives no cue (we know only what our sensors see).

**4.4 When we know.** The earliest of: the launch seen (4.1); a weapon track that says whom it is
coming at (4.2: two fixes, or one with the launch fix, give its line); the strike (4.3). The
bridge keeps each attack's timeline from its own observations, never from truth: "launch seen by
HUNTER 1 EO/IR at 1:20.4; weapon tracked by REAPER 1 radar from 1:22.0; HUNTER 1 struck at
1:24.4; warning 4.0 s".

**4.5 Measured** (the probe, by day, sun 45 degrees, sea state 2, the catalog's sensors; radar
over 200 scan phases, so plots are expected values):

| observer | launch seen | radar plots in flight | camera detects it in flight |
|---|---|---|---|
| HUNTER 1, 0.9 / 1.0 / 1.2 km (flight 3.6 / 4.0 / 4.8 s), its ring camera always on the boat | 1.00 at every range | **0**: the nose-on 0.01 m^2 is under ten times the clutter in its cell (0.005 m^2 half way); as a clutter-free air target it would get 1.4 to 1.9 (P(at least one) 1.00) | 0.45 to 0.46, mostly in the last second (0.27 a second or more before impact); never recognised |
| REAPER 1 at 7,600 m, 1.2 to 6.4 km out (7.7 to 9.9 km slant), its Doppler radar scanning every 5 s | 1.00 if its camera has the boat in its field | 0.33 to 0.58 (at most one: the scan outlasts the flight) | 0.69 to 0.70 if on it the whole flight |
| REAPER 1's camera, its payload manager left to itself over ten loiters | in its field 7 % of seconds (all wide field) | | |

So in the tutorial, by day: **we know at the launch**, from HUNTER 1's ring camera, almost always;
the weapon is then plotted by REAPER 1's radar about half the time and by HUNTER 1's camera about
half the time, mostly too late to matter; HUNTER 1 knows of the strike 3.6 to 4.8 s after the
launch. The HUNTER 1 radar result hinges on one figure no source pins (the nose-on RCS): at 0.05
m^2 or more its radar would plot the weapon once or twice. By night the flash is easier still; in
fog the ring camera's reach is the question (not measured here).

## 5. The weapon picture (bridge)

A weapon track `W-<n>` (`crates/murmur-bridge/src/tracks/weapons.rs`) is known by stages, as a
crew learns it (section 14, 2026-11-06, which replaced the class, speed and target the launch used to
report by construction): a launch, then a weapon, then a class, each on evidence our sensors
gathered; its fixes are associated by gating, never by the truth's weapon id. It ends **expended**
when one of ours reports a strike and the strike is put on it (section 14), else **lost** 10 s after
its last cue; either way it is shown for a minute, then forgotten. The feed carries each track's
first cue and its warning (strike less first cue).

The feed's fields (`weapons[]`, engine ms): `id`, `report` (the act report the seen launch
became), `stage` (`plot`, `launch`, `weapon`, `class`), `class` (the threat library's name, null
below the class stage), `class_symbol`, `evidence` (`{basis, short, text}`: `speed`, `height`,
`shooter`), `weapon_by` (`{speed_mps, over_mps, from_launch, closing_on}`: why it is taken for a
weapon), `speed_mps` and `speed_sigma_mps` (measured, null without two points in time),
`velocity_mps`, `kin_t` and `kin_point` (where and when the measured line puts it), `track` (the
contact track it is, when one of the tracker's passed the weapon test), `shooter` (the contact
track the launch was placed on), `aimed_at`, `aim_basis` (`track` or `reach`) and `eta_t` (null
without a basis), `nominal_mps` (the class's), `launch` (`t` reported, `flash_t`, `point`,
`sigma_m`, `sensors`, `observer {uid, point}`, `bearing_deg`), `fixes`, `status`, `ended_t`,
`struck`, `first_cue_t`, `warning_s`.

It never enters `TrackFacts`, the decision facts, the keep-outs, the attribution, the escort
rule, the lost contacts or MCP `get_tracks`. It is served on the tracks feed and on `GET
/api/tracks` as `weapons` (absent when empty; the agents' `get_tracks` builds its own answer
without it) and joined into the inbox's acts as each attack's timeline (`attack`).

## 6. Safeguards, and how each is kept

- **Rules 16, lost contacts.** A weapon track is never a track the tracker can drop: it is not
  in the track picture, so it never becomes a lost contact in every keep-out until accepted,
  never logs a `track.dropped`, and replay's "a track left the picture without a drop" cannot
  arise. It ends `expended` at the strike, in its own picture.
- **Rules 16 / `lost_reach`, out of view.** Not a track, so never counted at an air track's top
  speed anywhere after 10 s unseen; REAPER 1's nomination and release never see it.
- **Rules 9, attribution.** A weapon fix near the boat is no candidate author (a weapon track as
  a candidate would split the attribution with the boat's and turn a declaration into a flag).
- **Rules 18, 19.** No velocity regions, no acceptance: nothing to lapse.
- **Rules 20.** Our munition is the engine's; the opponent's is truth. They do not meet.
- **Replay and anchors.** The engine logs the same `ident.act` it logs for an abstract act and
  the strike as a `vehicle.event`, which replay already reads (an engagement's stop may quote a
  vehicle event on its own platform: a strike on an engaging platform would be quotable, as any
  vehicle event). Nothing new to recompute, nothing new to flag; anchors as before.
- **Tracker calibration, `before_oho`, `reaches`.** Weapons are not contacts: the contacts list,
  the detections, `TrackFacts`, the catalog's reaches and the calibration seeds are untouched.
  The weapon's signature lives beside the contact kinds, not among them, so `reaches` reads
  nothing new.
- **The payload manager.** Weapon fixes cue no camera; weapon sensing reads gimbal state only, so
  the boat's identification (the tutorial's target-identity margin) is as before.
- **Byte for byte without a weapon.** The truth message, the simulation channel's answer, the
  bridge's feeds and the audit log are unchanged when no contact carries a weapon: the headless
  golden episode, the v1 and v2 worlds' digests and every engagement scenario stand, with no
  re-take (no rules version either).
- **One report per act.** A weapon act reaches the engine at most once (at the launch, if seen);
  the strike is not a second act, so no double count toward `second_observation` corroboration.
- **The legacy model.** A contacts file that names the legacy model and a weapon is refused at
  load; with `MURMUR_SENSE_MODEL=legacy` forced (the eval stacks), the bridge says once that it
  senses weapon acts as abstract ones, and does.
- **No damage without figures.** A weapon whose scenario lists no `effects` changes nothing
  about its victim; one that does is drawn by the damage model (section 13), and the victim
  learns of its damage only by its own report.

## 7. The rules version: unchanged, and what would change it

Nothing the engine decides changes: a seen launch is an `ActReport` like any other, attributed and
declared by the same rule; the strike is a vehicle event. A rules version (and a re-take of the
golden digests, as every one since rules 19) would be needed only for either of these, both left
for the owner (section 11):

- **an unplaced strike in the engine's review:** when no sensor saw the launch, the C2 knows only
  that one of ours was struck. Proposed here as a bridge inbox alert ("HUNTER 1 struck; no sensor
  placed the shooter; declare by hand if you know who"). In the engine it would be an act with no
  author fix, flagged `untracked` and never declared: a new report shape.
- **attribution from a weapon track:** a back-track gives a line, not a point, unless the launch
  time is known, and when it is (the flash was seen) the flash already places the shooter to some
  20 m. A line attribution is a new rule; the console draws the back-track for the person instead.

## 8. The console

- **The weapon in flight** (`web/src/weapon.ts`, `weaponLayers.ts`), from the weapon picture only,
  never from truth, and only as far as its stage (section 14; until 2026-11-06 the launch brought
  the class, speed and target by construction): a seen **launch** is a flash and a marker where the
  camera placed the shooter, with the dashed bearing line from the observer that saw it ("LAUNCH ·
  bearing 270° from HUNTER 1"); a **weapon** is the hostile weapon symbol of unknown kind
  (`weaponSymbols.ts`) with its measured speed ("WEAPON? · 243 m/s"); a **class** is the class's
  symbol with its evidence ("ATGM-class: 250 m/s, from T-0004"). The symbol is dashed (predicted)
  where its place is an estimate and solid at a fix. The owner asked to *see* it like our Hellfire
  (10-29), so an **estimated flight** is drawn, dashed, with the symbol moving along it and a
  countdown ("EST. ATGM · 2.1 s"), but only on a basis: a target and strike time the bridge derived
  from the measured line, or, with a class and no line, from its reach and nominal speed. A measured
  line with no target is drawn as its heading 3 s ahead. The weapon's own fixes are solid marks
  (each with its 1-sigma circle, joined from the launch fix). A lone fix (`plot`) is not drawn as a
  weapon. The strike: a flash and a ring on the victim.
- **The banner**, while one of ours may be under attack: "LAUNCH · launch, bearing 270° from HUNTER
  1 from T-0004 · seen by HUNTER 1 EO/IR · weapon not yet tracked: type, speed and target unknown";
  then "INCOMING · weapon, type unknown, 243 m/s · ..." or "INCOMING · ATGM-class: 250 m/s, from
  T-0004 at HUNTER 1, strike expected in 2.0 s · seen by HUNTER 1 EO/IR · fixed by REAPER 1 radar";
  after, "HUNTER 1 STRUCK · launch seen by HUNTER 1 EO/IR · ATGM-class: ... · 4.1 s of warning · any
  damage is its own report (Inbox)", held 45 s as the act banner is (it said "no damage is
  modelled" until the damage model, 2026-11-02). Its tooltip explains the stages.
- **Abstract acts drawn as reports:** a burst at the author's fix and a dashed "fired on" arrow to
  the victim, labelled "Act A-1: reported fire on HUNTER 1", the banner "HOSTILE ACT REPORTED"; the
  streak is gone. A weapon act draws no arrow ("Act A-1: launch at HUNTER 1"): its weapon is drawn.
- **Reduced motion:** no moving glyph, streak or flash: the line, the last fix and the labels,
  static.

## 9. The tutorial

Since 2026-11-06 (docs/AUTONOMOUS-NODES.md section 8) the boat is an `armed_fast_boat` with its
kind's real magazine, ordered to attack HUNTER 1 every 90 s from 1:15 (its contacts file's
`[[order]]`s): its four ATGMs one at a time, then two HMG bursts each time. Every strike on HUNTER 1
is drawn and recorded and never applied, and every strike on the boat destroys it: training aids
under the contacts file's `[training]` (`spared`, `strike_outcome`), refused anywhere but a
tutorial. The bullets below are the design of 10-29.

- The boat carries the ATGM-class missile and fires on HUNTER 1 from 1:20, then **every 90 s**
  (13 attacks to 20:00, instead of 19 acts a minute apart): each is visible, so a person waiting at
  the step meets one within a minute and a half. HUNTER 1's leg (0.9 to 1.2 km) is well inside the
  5 km range; the run's day, positions and identity margin stay as measured on 10-29.
- The coach card's act step teaches the answer: the launch (which camera saw it, and why HUNTER
  1's ring always has the boat in view), the flight (which of our sensors plotted the weapon, and
  that HUNTER 1's navigation radar cannot in the clutter), the strike (HUNTER 1 knows it was hit),
  the warning time, and that the seen launch is what lets the engine declare by rule. It says that
  no damage is modelled.
- hostile-act-engagement and its loitering-munition sibling are not touched.

## 10. How it is checked (Phase 2)

Unit tests for the truth (lead point, range refusals, byte-identical messages without weapons),
the weapon sensing (each cue against the probe's figures, contact detections unchanged by a
weapon), the weapon picture (line, inbound, expended, lost; never in the decision facts); the
console's pure modules (`*.check.ts`); `make check`. Live: the tutorial on this worktree's ports,
walked in a browser through an attack, the declaration, the nomination, the signatures, the strike
and the assessment; `murmur-replay <log> --anchors <file>` clean; screenshots of the weapon in
flight, the banner and the card's act step.

## 11. The owner's decisions (2026-10-29), and the shortcuts they accept

1. **A strike with no seen launch** gives the inbox alert only (high: "no sensor saw the launch, so
   no shooter is placed and the rule declares nothing; declare by hand if you know who fired"). No
   rules version; the golden is not re-taken.
2. **The nose-on RCS stays 0.01 m^2.** HUNTER 1's navigation radar not seeing an ATGM-class weapon
   in the sea clutter is the honest answer; the coach card says so.
3. **Classification by construction: replaced by identity by stages** (bd murmur-ju1h, 2026-11-06,
   section 14). It was: the weapon picture associated by the weapon's truth id, and a seen launch
   reported the class, nominal speed and heading. Now a launch is a launch, a weapon is told by its
   measured kinematics and associated by gating, a class needs named evidence, and the type is out
   of reach. What remains by construction is narrower: the plot streams are split by the truth (a
   weapon's own fixes come only to the weapon picture, contacts' detections only to the tracker,
   whose tracks the weapon picture reads), and the flash's instant is the truth's.
4. **Shortcut: the strike report skips the victim's link** (bd murmur-3nkc). It reaches the C2 at
   the first truth tick at or after the impact, not through the link emulator (latency, loss).
5. **Shortcut: the flash is scored as a camera look** (bd murmur-nl15): the Johnson-pixel model on
   a 2 m, 300 K plume, not a point source's radiometry. Fine at the tutorial's 1 to 10 km.
6. **Headless worlds refuse weapons (they run them since docs/AUTONOMOUS-NODES.md stage 5), and
   MCP agents do not see the weapon picture** (follow-up bd
   murmur-mmxk).
7. **No back-track attribution.** A weapon track's line back to its origin is a line, not a point,
   without the launch time; when the launch is seen the flash places the shooter better anyway.

## 12. As built and checked (2026-10-29)

**Code.** Truth: `crates/murmur-contacts/src/{scenario,weapon,truth,main}.rs` (the headless
simulator refuses a contact with a weapon). Bridge: `tracks/physical/weapon.rs` (the launch and
in-flight sensing), `tracks/weapons.rs` (the weapon picture), `tracks/{mod,truth}.rs` (the wiring,
`ours` on the simulation channel, the strike's vehicle event, the `struck` alert). Console:
`web/src/{weapon,weaponLayers}.ts`, `hostileAct.ts` and `actLayers.ts` (abstract acts as reports),
the banner in `App.tsx`, the coach card (`Tutorial.tsx`, `tutorial.ts`: the act step holds an
attack's timeline 8 s after its strike before it moves on; the confirm step repeats the latest).

**Through the bridge as built** (the ignored `weapon_pipeline_probe`, 50 attacks, the tutorial's
geometry by day, REAPER 1 loitering): every launch seen, all by HUNTER 1's ring camera (REAPER 1's
camera never: its payload manager was not on the boat at a launch); the weapon fixed in flight in
31 of 50 (REAPER 1's radar 20, HUNTER 1's camera 17), the first fix 0 to 3.3 s before the strike.

**Live** (two tutorial runs on this worktree's ports, physical model, by day, 16 attacks seen):

| attack | launch seen | first weapon fix | strike | warning |
|---|---|---|---|---|
| W-4 (run 1) | HUNTER 1 EO/IR at launch | REAPER 1 radar at +1.8 s (152 m 1-sigma), then HUNTER 1 EO/IR at +3.9 s | +4.0 s | 4.0 s |
| W-7 (run 1) | HUNTER 1 EO/IR | HUNTER 1 EO/IR at +2.0 s, REAPER 1 radar at +2.1 s | +3.7 s | 3.7 s |
| W-2 (run 1) | HUNTER 1 EO/IR | none | +4.2 s | 4.2 s |

Every launch in both runs was seen by HUNTER 1's camera alone (radar and sonar no longer count for a
weapon act); the weapon was fixed in flight in 7 of 10 and 4 of 6 attacks. Attributions from the
camera's fix alone: 0.9929 (run 1, A-1, declared by rule), then 0.9929, 0.9967, 0.9688, 0.9953,
0.6482, 0.9951, 0.9959, 0.997 and 1.00; run 2: 0.9926 (A-1, declared), 0.993, 0.9967, 0.9677,
0.9953, 0.682. Each run's sixth act was attributed below the threshold (the flash's fix drawn 65 m
off, three of its 20 m sigmas: the same seed, the same draw) and flagged for review, as rules 9
does for any loose fix. In run 1 the person (op-1, by this walk) confirmed the declaration and
nominated REAPER 1 while a weapon was in flight and another had just struck: the nomination's 18
checks and both signatures' 20 passed, the engagement released, struck and was assessed "ceased";
the audit log holds no weapon track in any picture, only the `struck` vehicle events.
`murmur-replay --anchors` on both logs: "ANCHORS OK", "every decision reproduced exactly".

## 13. Damage (2026-11-02, bd murmur-lbw8.2)

The owner reversed "no damage" (docs/DECISIONS.md "Warheads and damage"). A contact's weapon may
list figures, in the catalog's shape, by the kind of what it strikes:

```toml
weapon = { name = "ATGM-class missile", kind = "atgm", speed_mps = 250, range_m = 5000,
           guidance = "semi_active_laser", warhead_kg = 9,
           effects = { small_fast_boat = { p_hit = 0.75, p_kill = 0.85, p_mobility = 0.10, p_firepower = 0.05 } } }
```

Our vehicles count as a kind by platform and domain (`damage::ours_kind`, [E]: the large
aircraft a light aircraft, other aircraft a small drone, the Sea Hunter a fishing boat, the
Saildrone a sailboat, other boats a small fast boat, a UGV a car, under water a UUV); the bridge's
answer (`ours`) says what each is. At the strike, on the act's own stream of the run's seed, the
truth side draws a hit against that kind's figure in the conditions (`damage::hit_chance`), then
the damage: destroyed, propulsion lost (`mobility`), weapons out of action (`firepower`, an armed
victim only), or none. The record is the impact's `damage` in the truth log; the bridge never
reads it.

What the victim does then is the vehicle simulation's: the contacts process appends the damage to
`sim/run/damage-<http port>.jsonl` (`MURMUR_DAMAGE_FILE`), which the stand-in engine follows. A
stand-in whose propulsion is lost steps unpowered (a boat drifts, a UGV stops, an aircraft comes
down), one whose weapons are out moves as before, and each says so itself over its link, a
STATUSTEXT `DAMAGE mobility: propulsion lost` or `DAMAGE firepower: weapons out of action`, at once
and every 30 s; a destroyed one goes silent. The bridge turns the report into a `vehicle.event` of
kind `damage`, an inbox alert `damaged` (high, while the run lasts) and the vehicle's `damage`
fact, which the engine's weapon check reads (rules 25: weapons out of action refuse the loadout;
a one-way platform without its propulsion can't strike). A destroyed vehicle reports nothing: the
operator has the strike report and then its silence (the lost-link alert), as with a real loss.

**SITL vehicles.** On 11-02 a SITL vehicle read nothing of the damage file: its strike was drawn
and recorded, and nothing else followed. Since 2026-11-06 (stage 8, docs/AUTONOMOUS-NODES.md
section 10) `sim/sitl_damage.py`, a thread of `sim/swarm.py`, acts the file out for them as far as
SITL allows: propulsion lost through SITL's own `SIM_ENGINE_FAIL` (with `SIM_ENGINE_MUL` 0) on the
kind's propulsion outputs, which serves rovers, boats and subs as well as planes and copters (a
plane glides down, a copter falls, a UGV and a boat stop, a sub rises), the same `DAMAGE` report
over the vehicle's own link, and a destroyed vehicle's process stopped, so it goes silent. A
sailboat (its sail draws on the wind) and an antenna tracker only report. On 11-02 the headless
simulator refused opponent weapons (murmur-mmxk), so there only our own strikes' collateral
damaged ours.

The shipped scenarios' weapons listed no figures on 11-02 (the tutorial's boat fired every 90 s at
HUNTER 1, which would not have survived the tutorial with them). Since 2026-11-06 every attacker
fires its catalog kind's weapons, with figures, so every strike on ours is drawn (the tutorial's
HUNTER 1 is spared by its `[training]`), and the headless simulator runs opponent weapons too
(docs/AUTONOMOUS-NODES.md stage 5), its strikes on ours taken by the episode's fleet.

## 14. Identity by stages (2026-11-06, bd murmur-ju1h)

The owner asked how a real crew learns what is coming at it, then asked for that to be modelled.
A crew's call is by class and time to impact ("missile, bearing 245, four seconds"); the exact type
comes, if at all, from an emitter library, a known shooter, or debris afterwards. Until this date
the weapon picture classified by construction (section 11 item 3): the launch handed it the
weapon's class, nominal speed and victim from the truth, and the console showed `EST. ATGM` at once.
Now the picture (`crates/murmur-bridge/src/tracks/weapons.rs`) knows a weapon only as far as our
sensors take it, and nothing in it reads the truth's weapon name, speed or victim (a test gives the
truth a name, speed and victim of its own and finds none of them served).

**The ladder.**

1. **launch**: a camera saw the flash (section 4.1, unchanged). The weapon track holds the time
   reported, the flash's instant, where the cue placed the shooter (its 1-sigma), the cameras, and
   the observer that made the fix (the bearing from it is served). The shooter is the live contact
   track nearest the flash's fix within 3 sigma of the two errors plus 25 m, if any. No class,
   speed or target.
2. **weapon**: fixes our sensors made of the weapon join a weapon track by **gating**: on a track
   with a measured line, within 3 sigma of the predicted position (its speed taken as no surer
   than 25 % of itself: a weapon boosts, and a line from two or three fixes is less sure than its
   formal error) plus 30 m; on a single earlier point (a flash, a lone fix), within 1,200 m/s
   times the interval plus 3 sigma (`V_MAX_MPS`, over any weapon here and a supersonic missile's
   average). The line is a weighted least-squares constant velocity through the flash and the
   fixes. It is taken for a weapon when its measured speed passes its domain's threshold
   (`weapon_speed_mps`, [E]: 60 m/s at the surface or on the ground, faster than any boat or road
   vehicle; 150 m/s in the air, faster than helicopters, light aircraft and airliners near the
   ground; 20 m/s under water; a fix without a height is judged at the surface threshold) and it
   either came from a seen launch or closes on one of ours: its closest approach to one of ours,
   ahead within 60 s, passes within 200 m (or twice its own error there). A fix that joins
   nothing starts a `plot` track: not called a weapon, not drawn as one, but kept so a strike can
   be put on it. The **contact tracks** the tracker holds are put to the same test each second
   (read only; the tracker is untouched): one whose estimated speed passes its domain's threshold
   and whose line closes on one of ours is a weapon track too, `track` naming it.
3. **class**: only on evidence, each piece named in the feed's `evidence`: the measured speed,
   known within 25 % (1-sigma), inside exactly one class's band of the threat library, with its
   measured height (when a sensor measured one) inside the class's and the shot's length within its
   reach (`physical::weapon::weapon_signature`'s `threat`: then the ATGM-class, 150 to 350 m/s, 0
   to 50 m up, 5.5 km, at boats and vehicles, all [E]; five classes since section 15); and/or the
   shooter's track identified (the tracker's identity level `identify`) as a kind our order of
   battle says carries the class (`carried_by`: a small fast boat carries ATGM-class missiles, [E]:
   a table of what our side believes such craft carry, not the contact's truth; several classes
   since section 15). Both present, they must agree; the speed fitting no class, or two, names none
   unless the shooter narrows it. An assessment once made holds.
   A launch from an identified shooter is classed at once, before any fix: the launch stage itself
   carries no class.
4. **type**: out of reach and not served. Nothing modelled tells it: no seeker emitter library, no
   laser warning receiver, no debris. That is the next step if it is wanted (an ESM channel
   against a guided weapon's seeker, a laser warner against a laser-guided one).

**Target and strike time.** Derived or absent: from the measured line, the one of ours its closest
approach passes nearest within the gate above (`aim_basis` `track`, `eta_t` the closest approach);
with a class and no line, the only one of ours of the class's target domains within its reach of
the launch, at its nominal speed (`reach`; two such vehicles, none). A **strike** one of ours reports
is put on the weapon track heading for it (expected strike nearest), else on the one whose last
cue, within 30 s, could have reached it, else on a new track. The strike's vehicle event keeps its
text ("struck (W-1): launch seen by 1:eo_ir 4.3 s before"); its W number and warning are the
picture's, as before, so they match the old ones whenever the association does (every shipped
scenario: one weapon in flight at a time).

**What can be confused, and what is still a shortcut.**

- A fast jet running at one of ours is taken for a weapon, and, without a measured height (a 2D
  radar's track), its speed fits the ATGM-class: the honest confusion of a speed-and-geometry
  threat warning. No shipped contact flies faster than 90 m/s, so none is taken today.
- A weapon is missed as one when it gets no second point in time (a launch unseen and one fix), or
  its measured speed falls under the threshold, or, launched unseen, its line closes on nothing.
  A slow one-way drone (40 to 60 m/s) is never taken for a weapon: it stays an air contact.
- Two weapons in flight at once may swap fixes where their gates overlap, and a strike may be put on
  the wrong one: the picture's association, not the truth's.
- A fix without a height cannot tell one of ours overhead from one below: its closest approach is
  judged across the ground.
- Still by construction: the plot streams are split by the truth (a weapon's own fixes come only to
  this picture, never to the contact tracker; contacts' detections only to the tracker, whose
  tracks this picture reads), and the flash's instant is the truth's. The threat library held one
  class, so a speed fit was a fit, not yet a choice between classes; section 15 makes it one.

**Unchanged.** The act observation (`observe_launch`, the `ActReport`, the `ident.act` it logs),
every decision input and every logged event's shape; no rules version. Contact detections with a
weapon in the truth are the same as without. MCP agents still do not see the picture (murmur-mmxk);
headless still refuses weapons; the legacy model is as it was.

**Checked.** Unit tests (`tracks::weapons::tests`): a launch then fixes climb the ladder; a class
needs evidence (a loose radar speed names none, an identified shooter does, an unidentified one
does not); an unidentified shooter leaves the class to the speed (launch, weapon, then class);
a fast aircraft running at ours is taken for a weapon, the same aircraft 3 km up or at 90 m/s is
not; two fixes with no launch become a weapon only closing on ours. Through the bridge
(`tracks::tests::weapons`): nothing of the truth's weapon is served, the strike's event text is as
before, the track picture is the same as a run without the shot. The ignored `weapon_pipeline_probe`
(50 attacks, the tutorial's geometry by day, the boat identified by then): all 50 at the class
stage at the strike, 21 with speed evidence beside the shooter's, 29 on the shooter alone (19 of
them never fixed in flight); the target from the line in 22, from the reach in 28; every attack's
fixes on one track. Read from the same tallies, without the shooter's identification about 21
would reach the class by speed, about 10 stop at weapon and 19 at the launch (not run as such).

**Live** (the tutorial on this worktree's situation ports, 2026-11-06, the boat identified as a
small fast boat by HUNTER 1's camera, radar and sonar before the first launch; snapshots of `GET
/api/tracks` `weapons` every 0.25 s):

| attack | at the launch | then | strike, warning |
|---|---|---|---|
| W-1 | class ATGM-class from T-0001, bearing 244° from HUNTER 1, aimed at HUNTER 1 by reach, strike expected in 4.1 s | +2.8 s HUNTER 1 EO/IR fix: 251 ± 9 m/s, evidence speed and shooter, aimed by its line | struck, 4.3 s |
| W-2 | the same, bearing 230° | +2.7 s REAPER 1 radar fix (180 m): 263 ± 75 m/s, too loose to be evidence; aimed by its line | struck, 4.2 s |
| W-3 | the same, bearing 258° | +3.0 s HUNTER 1 EO/IR fix: 246 ± 8 m/s, evidence speed and shooter | struck, 3.8 s |

The strikes' vehicle events read as before ("struck (W-2): launch seen by 1:eo_ir 4.2 s before");
`murmur-replay --anchors`: "ANCHORS OK", "every decision reproduced exactly".

## 15. Every class (2026-11-06, bd murmur-y85j.3)

docs/AUTONOMOUS-NODES.md stage 3 (section 5) and section 2.7 items 3 and 6: the contact catalog
(`sim/contacts.toml`, stage 1) arms every kind that is armed in real life, and its loadouts name
five ways our sensors may see a shot (`sense`): `atgm`, `rocket`, `air_missile`, `gun`, `torpedo`.
The bridge had figures for the first only. It now has them for all five
(`crates/murmur-bridge/src/tracks/physical/weapon.rs` `weapon_signature`), judges every flight in
three dimensions (the contacts side's flights carry real heights from stage 2: from or at an
aircraft, under the water), keeps a threat library of five classes, and answers a node's `look`.

**The rule that holds it together.** The truth's weapon `kind` selects which physics applies to
what is sensed (a plume, a muzzle flash, a launch transient; a body a radar can plot or not), as
`atgm` did. Nothing else of the truth's weapon is read: its kind, name, speed and victim are never
served and never used to classify it. The weapon picture knows only what the physics gave it: a
launch, and what kind of launch the sensor told it was; fixes; the shooter's track and its
identity. A test gives the truth a name of its own and finds neither it nor any `kind` in the feed.

### 15.1 What each class shows, and to which sensor

Every figure is [E] with its basis beside it in the code, on the public figures of
docs/research/opponent-weapons.md (its source numbers in brackets).

| class (`sense`) | the launch | in flight | figures |
|---|---|---|---|
| `atgm` (section 3) | flash, plume and smoke, 2 m at 300 K; told as a missile's at the recognition level | radar 0.01 m^2; camera head-on 0.15 m at 30 K, side-on 0.4 m at 100 K | unchanged |
| `rocket` (an RPG-7's PG-7VL, a 107 mm rocket) | a booster's blast and smoke cloud and a motor's flash, as the ATGM's (the RPG's smoke cloud gives the shooter away [5]); told as a rocket's at the recognition level | radar 0.005 m^2 (a 93 to 107 mm body [5, 7] nose-on); head-on 0.1 m at 30 K (a nose at 250 to 300 m/s heats some v^2/2c_p = 30 to 45 K); side-on 0.3 m at 60 K (the motor burns out early: the RPG's sustainer lights 10 m out [5], the 107 mm's top speed is its burnout's [7]) | |
| `air_missile` (the LMM-class, from a UAS) | a motor lighting off the rail, as the ATGM's; told as a missile's | radar 0.005 m^2 (a 76 mm body [12]); head-on 0.08 m at 60 K (510 m/s [12] heats its nose some 130 K); side-on 0.31 m at 150 K (sqrt(0.076 x 1.3 m) [12], its two-stage motor's plume [12] in view) | |
| `gun` (7.62 to 23 mm, a burst) | a muzzle flash, 0.5 m at 200 K, one look a burst (each flash about a millisecond, six to ten in under a second, with their smoke; a camera's frame averages each down from a thousand kelvin and more); told as gunfire as soon as caught (its rhythm) | nothing: a round's some 1e-4 m^2 is under every radar of ours, and a tracer is seen, not placed | |
| `torpedo` (the 53-65-class) | a launch transient at 155 dB re 1 uPa at 1 m (a tube's impulse and the motor's start), heard by a listening sonar; told as one when heard; nothing for a camera | running at 45 kn [14], 145 dB (above a UUV's 124 and a submarine on battery's 100, a little under a fast boat's 150 at speed: `physical/signature.rs`); a listening sonar's draw a second, a passive fix (50 m plus 10 % of range, no depth); nothing for a radar or a camera | the source levels have no public figure (a ship's sonar hears a torpedo late, US patent 4,313,181; flow noise grows 1.5 to 2 dB a knot past 10 to 20 kn, a sonar course's rule for ships): the least sourced figures here |

What each of our sensors can and cannot see:

| sensor | the launch | the weapon in flight |
|---|---|---|
| EO/IR camera (a panoramic ring always; a gimbal where its payload manager points, not slewing) | a missile's, a rocket's, a gun's (the flash in its field and in optical line of sight); never a torpedo's | an ATGM, a rocket, an air-launched missile (accrued as a look, head-on or side-on by the angle off its nose in 3D); never a gun's rounds or a torpedo |
| radar | never (it sees the shooter, not the shot) | an ATGM, a rocket, an air-launched missile above the water: within 15 m of the sea (`SEA_SKIM_M`) against the clutter in its cell, as a boat (a Doppler radar clear of the clutter's band by its radial speed in 3D), above it as an air target; never a gun's rounds or anything under water |
| sonar that listens (passive, or an active sonar's passive mode) | a torpedo's launch transient | a torpedo running |
| active sonar without a passive mode, AIS, ESM | never | never |

**Three dimensions.** Slant ranges, both heights, the horizon (`visible_height_m` at k = 7/6 and
4/3) and the terrain at every instant judged; a camera's head-on test is the angle between the
weapon's 3D velocity and the line to the observer. A height-finding radar measures the weapon's
height (30 m 1-sigma, as a contact's) and lays its plot at the true ground range; a 2D radar lays
its slant range off as if the weapon were on the sea's surface (an airborne radar's plot of a
weapon at height falls short by what its height takes off the slant, an error the plot's sigma
does not carry). A sonar's range is the slant through the water, observer's depth to the weapon's.

**The launch's kind** (`physical::weapon::LaunchLook`). One draw per observer and sensor decides
both: under the channel's detection probability the launch is caught, under its recognition
probability (never more) the kind of motor launch is told: a missile's (a plume that flies off and
burns on as it is steered) or a rocket's (a booster's blast, a motor that burns out early). A gun's
flashes are told as soon as caught, by their rhythm (milliseconds each at the gun's rate, nothing
flying off); a sonar's transient is a launch transient when heard at all. Johnson's recognition
criterion stands in for "watching the plume fly", a shortcut.

**A gun's episode.** The contacts side sends one act per firing episode, each burst its own flight
under it with the `rounds` it fired. Each burst's launch is sensed on its own (the act's own weapon
on its act's stream and where the act placed its author; later bursts on their own stream, from
their muzzle). The act is reported once, as before: at its first burst, or, when no sensor caught
that one, at the first later burst caught (the record says which: `seen_at`; an act unseen is kept
ten minutes for this). Each burst caught is a launch in the weapon picture under the act's one
report; each burst's strike is the victim's report, put on the burst heading for it. The bridge
never reads `rounds`: a burst is one flash however many rounds it holds.

**A `self` weapon** (a one-way drone, an explosive boat) has no flight: the contacts side commits an
abstract act at its strike (sensed as one, by whatever detects its author that second) and an
impact. The picture puts the strike on a weapon track of its own, with no launch or fix.

### 15.2 The threat library and `carried_by`

| class | speed band, nominal, m/s | heights, m | reach, m | fired at | travels | its launch |
|---|---|---|---|---|---|---|
| ATGM-class | 150 to 350, 250 | 0 to 50 | 5,500 | boats, vehicles | air | missile |
| Rocket-class | 180 to 420, 300 (the RPG's 115 to 295 [5], the 107 mm's 385 at burnout [7]) | 0 to 2,000 (laid at 8 km a 107 mm arcs to 1 to 2 km) | 8,050 [7] | boats, vehicles | air | rocket |
| Air missile-class | 400 to 750, 510 [12] (the Hellfire class's 425, our catalog) | 0 to 6,000 | 8,000 (over 6 km [12], the Hellfire class 8) | boats, vehicles, aircraft [12] | air | missile |
| Gun-class | 600 to 1,000, 800 (825 to 970 at the muzzle [2, 3, 4, 6, 16, 23], 500 to 600 at 2 km) | 0 to 2,000 | 2,000 [2, 3, 4, 16] | boats, vehicles, aircraft | air | muzzle |
| Torpedo-class | 15 to 35, 23 [14] | -300 to 0 | 19,000 [14] | ships, submarines | water | transient |

The bands overlap where real weapons do: a measured 250 m/s is an ATGM's or a rocket's and names
neither; 510 is only the air-launched missile's, 380 only the rocket's, 160 only the ATGM's, 23 in
the water only the torpedo's. A class is named only when the evidence leaves exactly one, among the
classes that travel where the weapon was sensed (a sonar's fixes or transient: the water) and that
launch as it was told to: the launch's kind, the speed and height in a band, and the shooter's
identity. A muzzle flash or a launch transient names its class by itself; a missile's launch needs
the speed or the shooter (an ATGM's or an air-launched missile's).

`carried_by` (`tracks/weapons.rs`) is our side's belief about a kind, [E] on the research note's
public cases, never the contact's truth. With decision D3 an armed craft is identified only as its
look-alike, so the belief covers every craft of the kind:

| identified as | may carry | basis |
|---|---|---|
| small fast boat | ATGM-class, Rocket-class, Gun-class | small craft have fired anti-tank missiles, RPGs, rockets and machine guns at ships [8, 9, 10] |
| cargo ship | Rocket-class, Gun-class | containerised launchers on merchant hulls [20]; improvised rocket and cannon mounts [7, 16] |
| tanker, patrol boat, truck | Gun-class | machine guns against drone boats [18]; coast guard boats [13]; technicals [17] |
| unknown UAS | Air missile-class | Group 3 UAS carry light missiles [12] |
| submarine, periscope | Torpedo-class | [15] |

So the shooter alone pins a class only if the launch's kind leaves one of those it may carry: a
small fast boat's missile launch is the ATGM-class, its muzzle flash the Gun-class, its rocket
launch the Rocket-class; its launch caught but not told waits for the speed, and a speed of 250
m/s (both an ATGM's and a rocket's) leaves it at "weapon, type unknown".

**The weapon test by domain.** A weapon track is judged under water when a sonar heard it (20 m/s),
in the air when a measured height puts it over 50 m (`AIR_FROM_M`, 150 m/s), else at the surface
(60 m/s, the lower of the two, so a low weapon is not missed for want of a height). With a measured
height (and its rate from two or more), a line's closest approach to one of ours counts the heights
there: a missile 3 km up does not close on a boat below its line.

### 15.3 The answer to a node's look

A truth message may carry `look: [{ node, lat, lon, alt_m, reach_m }]` (absent when empty); the
answer's `ours` then carries every one of ours within any look's reach, the straight line with
heights, where each reports it is (`Tracks::ours_looked_at`), as well as those `aim_at` names and,
under live mines, ours of their domains: the union, each once (`Tracks::ours_answer`). The node
keeps what its own sensors and the radio horizon allow (docs/AUTONOMOUS-NODES.md 2.4); the bridge
answers by reach alone. `node_events` and a contact's `variant` are never read (serde ignores them,
tested).

### 15.4 Measured

The ignored `classes_probe` (`tracks/tests/weapon_probe.rs`): a shooter `d` west of HUNTER 1 (a boat
on the water, a UAS 300 m up, a submarine 30 m down), by day, sun 45 degrees, 200 seeds; REAPER 1
7,600 m up 4 km south of the flight's middle, its camera never on it (no payload manager in the
probe), so only its radar counts.

| class | HUNTER 1 at 500 / 1,000 / 2,000 / 4,000 m: launch caught, told | fixed in flight at 1,000 m (of 200 flights) |
|---|---|---|
| ATGM-class | 1.00, 1.00 / 1.00, 0.95 / 1.00, 0.46 / 0.95, 0.04 | HUNTER 1 camera 102, REAPER 1 radar 79 |
| Rocket-class | the same (the same launch figures) | HUNTER 1 camera 74, REAPER 1 radar 45 |
| Air missile-class, from 300 m | the same | HUNTER 1 radar 149 (an air target, clear of the clutter), camera 32, REAPER 1 radar 29 |
| Gun-class | 1.00 / 0.95 / 0.46 / 0.04, each told when caught | none, by design |
| Torpedo-class, from 30 m down | 0.98 / 0.94 / 0.86 / 0.72 by its hull sonar's passive mode | HUNTER 1 sonar in all 200 |

So in the tutorial's geometry (0.9 to 1.2 km) a small fast boat's ATGM launch is told as a
missile's about 95 % of the time and, the boat identified, classed at once as before; the other 5
% wait for a speed that, at 250 m/s, names neither of the ATGM and the rocket. Its HMG bursts (stage
6) are caught about 95 % of the time at 1 km, each the Gun-class at once.

### 15.5 Shortcuts, stated

- The truth splits the streams (section 14) and picks the physics by the weapon's kind; a launch's
  kind is told by a Johnson recognition draw standing in for watching its plume fly.
- A burst is one flash however many rounds it fired; a gun's rounds are never fixed, so a gun's
  speed band is never met by a measured speed.
- A 2D radar's plot of a weapon at height is laid off on the sea's surface, short of it.
- A torpedo's launch and running source levels have no public figure; an active sonar without a
  passive mode hears neither, and nothing pings a torpedo.
- The console draws every weapon at 4 m over the water (`FLY_ALT_M`), an air-launched one too:
  the picture's launch fix has no height, and only a height-finding radar's fixes carry one (served
  as `alt_m`, not yet drawn).

**Unchanged.** No rules version; no decision input or logged event: a weapon act's report is the
same `ActReport` (a test round-trips it through the engine's own type, which refuses unknown
fields) and logs the same `ident.act`; contact detections and the track picture are the same, second
by second, with each class in the truth (tested); the tracker and the legacy model untouched;
headless refused weapons until stage 5 (built since: section 16).

**Checked.** `tracks::weapons::tests`: a speed fit chooses between classes; an identified small
fast boat waits for evidence until its launch is told; a missile at an aircraft's height passes the
air's test and closes on the aircraft, not on the boat below; the rungs and the jet's confusion as
before. `tracks::tests::weapons`: every class leaves the detections and the track picture as they
were and is caught by its own sensors (the camera; the sonar for the torpedo); a gun's episode is
one act, its bursts launches with no fixes; a gun act unseen at its first burst is reported at a
later one; a torpedo is heard and never seen; a look is answered with ours in reach and the union
with `aim_at`; the truth message reads `look` and `rounds` and ignores `node_events`. Console:
`weaponSymbols.check.ts` (the classes' kinds fly as missiles), `weapon.check.ts` (every class's
short name, a gun's burst classed by its muzzle flash).

## 16. Orders: how a node fires (2026-11-06, docs/AUTONOMOUS-NODES.md stages 1 to 7)

The live path since bd murmur-y85j. A contact of an armed kind (`sim/contacts.toml`: its
`loadout` in the shape of our catalog's `WeaponFacts`, each figure sourced in
docs/research/opponent-weapons.md) carries its kind's weapons and rounds; it fires only when an
order tells it to attack, and then by its own fire control (`crates/murmur-contacts/src/node.rs`):
the target perceived (our vehicles as our C2 hears them within its sensors' reach and horizon,
asked for in the truth message's `look`), the weapon named or the first of its loadout for the
target's domain with rounds left, the range band, the reload or burst interval, the rounds. Out of
band it closes (or holds, with `stay`); out of rounds it follows its kind's `on_empty`; unseen for
60 s the order ends `target_lost`. Each shot is a launch as in sections 3 to 5, its act id
`<contact>-<n>` in firing order, so the damage draws (`act:<id>`) stay the same from run to run.

A file times the order; it never creates a weapon in the air, sets where one flies or decides what
it hits:

```toml
[[order]]
at_s = 200                       # the contacts' clock, as an act's was
node = "fastboat-raider"
do = "attack"                    # transit | hold | orbit | track | attack | break_off | flee | resume
target = { vehicle = 1 }         # or { contact = "<id>" }
weapon = "ATGM-class missile"    # optional: else the first of its loadout for the target's domain
rounds = 1                       # optional: a salvo's cap
stay = true                      # fire only from where it is
label = "attacks the sensor ship (HUNTER 1)"   # truth-side note
```

The same order reaches a running stack through the contacts process's own API (`POST /orders` on
`MURMUR_CONTACTS_PORT`, `X-Murmur-Sim-Token`; `crates/murmur-contacts/src/api.rs`) or a
situation's timeline `red` step; never through the bridge. Headless runs the same code in process
(`murmur_contacts::step`, AUTONOMOUS-NODES stage 5).

**The old path, guarded** (stage 7, bd murmur-y85j.7). An `[[act]]` or a contact's own
`weapon = {...}` loads only from a file with a top-level `scripted_acts = "<why>"`, refused
otherwise by the contacts crate wherever the file is loaded (`scenario.rs` `old_path`); and a
situation refuses its contacts file if it keeps the old path at all, marked or not
(`eval/situation.py` `_contacts_guard`). What stays on it, and why, is AUTONOMOUS-NODES.md
section 9's table: engine scenarios and the ident evaluation (which never reach the contacts
crate), two headless operations families, and the crate's own tests of the scripted path.
