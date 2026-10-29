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

A contact may carry a weapon, and each act it commits launches it. The weapon is kinematics
only: it flies straight from the shooter to its victim at its speed, and the strike is a time and
a report (no warhead, blast, damage or miss, as our own Hellfire-class munition). Three cues can
tell us, each through the physical sensing model:

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

A weapon track `W-<n>` per weapon (`crates/murmur-bridge/src/tracks/weapons.rs`), its fixes and
its strike associated by the truth's weapon id, as the tracker associates detections by contact id
(by construction, shortcut 3 below). It holds the launch fix (time, point, error, the cameras) if
seen, and what the launch reports by construction: the weapon's class, its nominal speed and which
of ours it heads for, so the expected strike (`eta_t`) is the launch time plus the distance at that
speed; then its own fixes. It ends **expended** at its victim's strike report (a weapon nobody saw
gets a track at its strike, so the strike is in the picture), else **lost** 10 s after its last cue;
either way it is shown for a minute, then forgotten. The feed carries each track's first cue and
its warning (strike less first cue).

It never enters `TrackFacts`, the decision facts, the keep-outs, the attribution, the escort
rule, the lost contacts or MCP `get_tracks`. It is served on the tracks feed as `weapons` (absent
when empty) and joined into the inbox's acts as each attack's timeline (`attack`).

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
- **No damage.** The strike changes nothing about the victim; the card and the docs say so.

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
  never from truth. The owner asked to *see* it like our Hellfire, and a weapon track is usually
  zero to two fixes, so (the coordinator's change, 10-29): when a launch was seen, an **estimated
  flight** is drawn from the launch fix to the vehicle the launch heads for, dashed and labelled as
  an estimate ("EST. ATGM · 2.1 s", a countdown to the expected strike), with a weapon glyph moving
  along it as our munition's does; the class and speed come with the launch observation (by
  construction, shortcut 3; the banner's tooltip says so). The weapon's own fixes are solid marks
  (each with its 1-sigma circle, joined from the launch fix) when they come. No launch seen: only
  the fixes, if any, and the strike at the victim. The strike: a flash and a ring on the victim.
- **The banner**, while one of ours is under a detected attack: "INCOMING · ATGM at HUNTER 1,
  strike expected in 2.0 s · launch seen by HUNTER 1 EO/IR · weapon fixed by REAPER 1 radar";
  after, "HUNTER 1 STRUCK · launch seen by HUNTER 1 EO/IR · 4.1 s of warning · no damage is
  modelled", held 45 s as the act banner is.
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
3. **Shortcut: classification by construction** (bd murmur-ju1h). The weapon picture is fed apart
   from the contact tracker by the truth's lists and associated by the weapon's truth id; a seen
   launch reports the class, nominal speed and heading (the console's estimate). It stands for a
   threat-warning channel that tells a weapon by its speed and origin; a fast aircraft taken for a
   weapon, or a weapon missed as one, is not modelled.
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
