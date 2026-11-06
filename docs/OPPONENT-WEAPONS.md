# Opponent weapons: a modelled shot instead of an abstract act

Status: **approved and built 2026-10-29** (bd murmur-wcvp), with the owner's decisions on the
open questions (section 11) and one change to the console (section 8: the estimated flight). The
design's measurement and the as-built one are ignored probes in
`crates/murmur-bridge/src/tracks/tests/weapon_probe.rs`; section 12 records what was built and how
it was checked.

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
built on 10-29, and still the tutorial's boat) that is all: no warhead, blast, damage or miss.
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
stays abstract and behaves byte for byte as today:

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
alert. A strike on a contact gives no cue (we know only what our sensors see).

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
6. **Headless worlds refuse weapons, and MCP agents do not see the weapon picture** (follow-up bd
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

**What SITL lacks.** A SITL vehicle does not read the damage file: no hook in this pass makes
ArduPilot lose its propulsion (SITL's `SIM_ENGINE_FAIL` and `SIM_ENGINE_MUL` exist for planes
and copters, nothing equivalent for rovers, boats and subs), go silent or send the report. Its
strike is drawn and recorded, and nothing else follows. The headless simulator refuses opponent
weapons still (murmur-mmxk), so there only our own strikes' collateral damages ours.

The shipped scenarios' weapons list no figures (the tutorial's boat fires every 90 s at HUNTER
1, which would not survive the tutorial with them): their strikes change nothing, as before.

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
   reach (`physical::weapon::weapon_signature`'s `threat`: today the ATGM-class, 150 to 350 m/s, 0
   to 50 m up, 5.5 km, at boats and vehicles, all [E]); and/or the shooter's track identified (the
   tracker's identity level `identify`) as a kind our order of battle says carries the class
   (`carried_by`: a small fast boat carries ATGM-class missiles, [E]: a table of what our side
   believes such craft carry, not the contact's truth). Both present, they must agree; the speed
   fitting no class, or two, names none unless the shooter narrows it. An assessment once made holds.
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
  tracks this picture reads), and the flash's instant is the truth's. The threat library holds one
  class, so a speed fit is a fit, not yet a choice between classes (the code names a class only
  when exactly one fits; with one in the library that path is untried).

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
