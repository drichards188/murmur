# Murmur contact kinds and what they carry (public sources only)

Research note for the contact catalog (`sim/contacts.toml`), 2026-11-06, bd murmur-y85j.1 (docs/
AUTONOMOUS-NODES.md section 2.1 and stage 1). The companion of docs/research/weapons.md, which
does the same for our platforms, and in its style: every figure carries a source number from the
list at the end, or [E] with its reasoning here. The catalog's `sources` tables give each figure's
tag; a test (`crates/murmur-contacts` `catalog::tests`) fails a figure without one and a reference
this list does not have.

Public, game-level figures only: named classes of craft and weapon, counts, ranges, speeds, warhead
classes and masses. "-class" naming, as the catalog's. No restricted data (JMEM or similar) was used
or approximated. The rule our sensors follow (decision W3) holds here too: **a node's figures are
never authored to make a situation work**; an author moves the node or times its order instead.

Conventions: ranges in metres (slant, launch to effect point), speeds in m/s, masses in kg.
"Count" is rounds carried per load. Hit and damage figures (`effects`) follow docs/research/
weapons.md 10.3 (hit chance per shot or burst) and 10.4 (outcome given a hit), keyed by contact
kind; our vehicles read as one of those kinds (crates/murmur-contacts `damage::ours_kind`: HUNTER 1,
the Sea Hunter, as a fishing vessel).

## 0. What changes the plan (read first)

1. **Saturation's shooter (D7) cannot look like a recon quadcopter.** No public light missile is
   carried by a Group 1 or 2 quadcopter. The lightest drone-fired missile with public figures, the
   LMM (13 kg), has been fired from a Camcopter S-100 and a Flyby Jackal [12], Group 3 airframes. So
   D7's "armed drone kind with a light stand-off missile" is `armed_uas`, which passes for an
   `unknown_uas`, not a `small_drone`: migrating the shooter (stage 6) changes its look, and with it
   the bridge's physical signature, from a small quadcopter to a larger UAS. The alternative keeps
   the look and arms it with what a quadcopter can carry (a dropped grenade, an RPG-class tube), which
   needs it within a few hundred metres of the USV, the geometry D7 declined.
2. **The ATGM's figures move slightly.** The tutorial's per-contact weapon (docs/OPPONENT-WEAPONS.md
   section 3) is 250 m/s and 5,000 m, "between the Konkurs and the Kornet". The catalog takes the
   Kornet-E's published figures: 100 to 5,500 m, more than 250 m/s, a 4.6 kg tandem HEAT warhead [1]
   (the spec's example said 5 kg). Nothing reads the catalog's yet; the tutorial keeps its own line
   until stage 6 migrates it, and its range test is re-taken then as a band test (section 4 of the
   spec). Both of its holding ranges (0.9 to 1.2 km) are inside both bands.
3. **The auxiliary's guns are a merchant ship's, publicly.** No public source found arms a Russian
   Navy replenishment oiler (Project 23630 is 86 m, 12 kn, with no armament listed [19]). The public
   case of a ship of that size class arming against drone boats is Gazprom's LNG ship *Marshal
   Vasilevskiy*: two 12.7 mm Kord machine guns in sandbagged positions, 2026 [18]. `naval_auxiliary`
   carries that. The naval-drone situation's note "fires a rocket" becomes a machine gun's fire in
   stage 6.
4. **The coaster's weapons are an estimate by analogy.** Containerised launchers on merchant hulls
   are public (the IRGC's converted container ship *Shahid Mahdavi* [20]), and deck- or vehicle-
   mounted 107 mm launchers and ZU-23-2 cannon are the commonest improvised mounts [7,16,17], but no
   public source shows a coaster with this pair. `armed_coaster` is [E] as a whole; its weapons'
   figures are sourced.
5. **Four kinds beyond the spec's list**, each named in its text: `armed_skiff` (section 4's
   golden-gate-ident skiff, "an armed kind with a short-range weapon"), `explosive_boat` (section
   2.3's explosive boat), `oneway_uas` ("one-way attack drone kinds"), `technical` (section 1.2).
6. **Schema additions to section 2.1's example:** `sonar_m` in `sensors` (the submarine hears
   before it sees), `expended_by_effect` (as our catalog, for a kind that is its own weapon), and a
   `sources` table per kind (figure to tag) so a test can hold every figure to a source.

## 1. The kinds

Armed variants pass for their civilian twin (`looks_like`, decision D3). Unarmed kinds keep the
signature, emits, turn rate and height the `KINDS` table gave them, unchanged; an armed variant
takes its look-alike's.

| kind | domain | armed | loadout | looks like |
|---|---|---|---|---|
| `cargo_ship`, `tanker`, `ferry`, `fishing_vessel`, `sailboat` | surface | no | | |
| `patrol_boat` | surface | no (harbour police, pilots, fireboats) | | |
| `small_fast_boat` | surface | no | | |
| `armed_fast_boat` | surface | yes | 4 ATGM-class missiles, a 12.7 mm HMG (500 rounds) | `small_fast_boat` |
| `armed_skiff` | surface | yes | 4 RPG-7-class grenades, a 7.62 mm GPMG (600 rounds) | `small_fast_boat` |
| `explosive_boat` | surface | yes, one-way | a 150 kg charge (itself) | `small_fast_boat` |
| `armed_coaster` | surface | yes | a 12-tube 107 mm rocket launcher, a 23 mm twin cannon (400 rounds) | `cargo_ship` |
| `naval_auxiliary` | surface | yes | 12.7 mm HMGs (1,000 rounds) | `tanker` |
| `cutter` | surface | yes | M2 .50 HMGs (1,000 rounds) | `patrol_boat` |
| `light_aircraft`, `helicopter`, `unknown_uas`, `small_drone` | air | no | | |
| `armed_uas` | air | yes | 2 LMM-class light missiles | `unknown_uas` |
| `oneway_uas` | air | yes, one-way | a 50 kg warhead (itself) | `unknown_uas` |
| `oneway_drone` | air | yes, one-way | a PG-7VL warhead (itself) | `small_drone` |
| `submarine` | subsurface | yes | 18 heavyweight torpedoes | |
| `uuv`, `periscope`, `diver` | subsurface | no | | |
| `vehicle`, `truck`, `person` | ground | no | | |
| `technical` | ground | yes | a 12.7 mm HMG (300 rounds) | `truck` |

The submarine is armed by kind: a military submarine carries torpedoes. Both files that use one
(golden-gate, hammerhead-line) keep it on a transit, so nothing fires. The `periscope` kind is the
physical sensing model's view of a submarine's mast, not a node, and stays unarmed.

## 2. Weapons, kind by kind

### 2.1 Fast inshore attack craft (`armed_fast_boat`)

IRGC Navy craft of the Boghammar type carry an RPG-7, three 12.7 mm machine guns, a 106 mm
recoilless rifle and a 107 mm rocket launcher, at 46 kn on 13 m [8]; IRGC craft have been seen with
anti-tank missiles, and the fleet's small craft as a whole with ATGMs, RPGs, machine guns and rocket
launchers [9]. The Kornet's Kvartet mount is marketed for boats [1]. Taken: an ATGM team and a
heavy machine gun, the spec's pair.

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| ATGM-class missile (Kornet-E) | 4 [E: a team's launcher with three reloads in the boat] | 100-5,500 [1] | 250 [1: "> 250 m/s"] | SACLOS, laser beam riding [1] | tandem HEAT, 4.6 kg [1] | reload 30 s [E: 2 to 3 rounds a minute reported for the Kornet-EM [22], the slower end for a team reloading by hand on a moving hull]; `sense = "atgm"` |
| 12.7 mm HMG (DShK/NSV-class) | 500 [E: ten 50-round belts [2,3]] | 0-2,000 [2,3: 2,000 m against ground targets] | 850 [3; NSV 845 [2]] | open sights [E] | 12.7 x 108 API, 0.048 kg [E: a bullet's mass] | bursts of 6 [E: as the M2's 5 to 7 [23]] at 600 rpm [3], one every 5 s [E: as our M2's] |

Top speed 23 m/s (46 kn) [8]; cruise 15 [E].

### 2.2 Skiff with an RPG team (`armed_skiff`)

Pirate skiffs off Somalia fire small arms and RPGs at ships (November 2025: four armed attackers in
a skiff opened fire on a tanker and fired an RPG at it [10]). Taken: an RPG-7 and a general-purpose
machine gun.

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| RPG-7-class grenade launcher (PG-7VL) | 4 [E] | 10 [E: arming] to 330 [5: effective range, PG-7V] | 250 [5; E: 115 m/s from the tube, 295 to 300 in flight [5]; an average over 300 m] | unguided [5] | HEAT, 2.6 kg [5] | reload 10 s [E: a two-man team]; `sense = "rocket"` |
| 7.62 mm GPMG (PKM-class) | 600 [6; E: three 200-round boxes] | 0-1,000 [6] | 825 [6] | open sights [E] | 7.62 x 54R ball, 0.0096 kg [E] | bursts of 8 [E: 5 to 10, as the FN MAG in docs/research/weapons.md] at 650 rpm [6: 600-800], one every 5 s [E] |

Speed 13 m/s top, 9 cruise [E: an outboard skiff of 5 to 7 m, loaded].

### 2.3 Explosive drone boat (`explosive_boat`)

The Houthis' Toofan-1 is reported with a 150 kg warhead at 35 kn; the Toofan-2 400 kg at 41 kn; the
first generation, converted 10 m patrol boats, 454 kg at 45 kn [21]. Taken: the Toofan-1, the
smallest, a `self` weapon (the boat is the weapon). Range 100 km [E: the Magura V5's is 800 km
(docs/research/weapons.md); a bay's needs are far less]; operator video guidance [E: as the Magura].
Speed 18 m/s top [21], 15 cruise [E].

### 2.4 Armed coaster (`armed_coaster`)

[E] as a whole (section 0, item 4).

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| 107 mm rocket launcher (Type 63-class, 12 tubes) | 12 [7: 12 tubes; no reload aboard, E] | 300 [E: arming and the launcher's lowest elevation, laid flat at a ship] to 8,050 [7] | 300 [7; E: 385 m/s at burnout [7], slowed by drag] | unguided [7] | HE-frag, 1.3 kg [7] | reload 1 s [E: a ripple of 12 in some 10 s]; `sense = "rocket"` |
| 23 mm twin cannon (ZU-23-2-class) | 400 [16; E: eight 50-round boxes] | 0-2,000 [16: two km against vehicles] | 970 [16] | optical sight [E] | 23 mm HEI, 0.19 kg [E: the projectile] | bursts of 10 [E: five a barrel] at 2,000 rpm [16: cyclic, a barrel], one every 5 s [E: 400 rpm practical [16] is far above a burst every 5 s; the gun is laid again between bursts] |

Speed 6.5 m/s top, 5 cruise [E: a small freighter].

### 2.5 Naval auxiliary (`naval_auxiliary`)

Section 0, item 3. Two Kord-class guns [18], one bearing on a target on one beam [E].

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| 12.7 mm HMG (Kord-class) | 1,000 [4,18; E: ten 50-round belts a gun, two guns] | 0-2,000 [4] | 860 [4] | open sights [18; E: sandbagged pintles] | 12.7 x 108 API, 0.048 kg [E] | bursts of 6 [23; E] at 625 rpm [4: 600-650], one every 5 s [E] |

Speed 6.2 m/s top [19: 12 kn, Project 23630], 5 cruise [E].

### 2.6 Coast guard patrol boat (`cutter`)

The 87 ft Marine Protector class carries two M2 .50s, 25 to 26 kn [13]. Never ordered to attack in
the shipped situations (an assumed friend).

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| M2 .50 HMG (pintle) | 1,000 [13; E: two guns, 500 rounds each as the R400's [23]] | 0-1,800 [23] | 910 [23: 890-930] | open sights [E] | 12.7 x 99 ball/AP, 0.043 kg [23; E] | bursts of 6 [23: 5-7] at 525 rpm [23: 450-600], one every 5 s [E] |

Speed 13 m/s top [13], 10 cruise [E].

### 2.7 Armed VTOL UAS (`armed_uas`)

The air-to-air situation's "large VTOL drone, some 3 m across" and Saturation's shooter (section 0,
item 1). The Lightweight Multirole Missile: 13 kg, a 3 kg blast-fragmentation or shaped-charge
warhead, over Mach 1.5 (510 m/s), 400 m to beyond 6 km, laser beam riding or semi-active laser,
built against fast inshore attack craft and fired at target drones; tested from the Camcopter
S-100, and the Jackal carries two [12].

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| LMM-class light missile | 2 [12] | 400-6,000 [12] | 510 [12] | laser beam riding [12] | blast-frag, 3 kg [12] | reload 5 s [E: both on their rails; the sight laid again on the target]; `sense = "air_missile"`; targets surface, ground and air [12] |

Speed 45 m/s top, 28 cruise [E: a Group 3 VTOL].

### 2.8 One-way attack drones (`oneway_uas`, `oneway_drone`)

- **Shahed-136-class** (`oneway_uas`): about 185 km/h (51 m/s), 2,500 km (Janes, via [11]), a
  50 kg warhead, GNSS and INS [11]. Cruise 45 [E]. It aims at a coordinate: its hit chance is
  docs/research/weapons.md 10.3's GPS-bomb column. A 1,000 m `eye_m` [E: what a terminal camera on
  some later variants would see; without one, stage 2 gives it a point to strike].
- **FPV with an RPG warhead** (`oneway_drone`): our catalog's `fpv` entry as it stands (docs/
  research/weapons.md section 6 [24], the PG-7VL [5]): 28 to 44 m/s, 10 km, 2.6 kg HEAT, operator
  video. Cruise 25 and top 40 within that band [24].

### 2.9 Submarine (`submarine`)

A Kilo-class boat: six 533 mm tubes and 18 torpedoes (53-65 anti-ship or TEST-71/76), 17 kn
surfaced and 20 kn submerged [15]. The 53-65K: 307.6 kg of HE, 45 kn, 19,000 m, wake homing [14].

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| 533 mm heavyweight torpedo (53-65-class) | 18 [15] | 500 [E: arming run] to 19,000 [14] | 23 [14: 45 kn] | wake homing [14] | blast, 307.6 kg [14] | reload 15 s [15; E: six ready in the tubes fire in quick succession; reloading a tube takes minutes, not modelled apart]; `sense = "torpedo"`; targets surface only (wake homing [14]) |

Speed 10 m/s top [15: 20 kn submerged], 2.5 cruise [E: a quiet transit]. Sensors 8,000 m by
periscope and 10,000 m by sonar against a ship [E].

### 2.10 Technical (`technical`)

Technicals are civilian pickups, the Hilux and Land Cruiser most of all, with heavy machine guns
(DShK, Type 77, M2), ZU-23-2s, recoilless rifles, ATGMs or Type 63 launchers [17]. Taken: a DShK.

| weapon | count | range | speed | guidance | warhead | other |
|---|---|---|---|---|---|---|
| 12.7 mm HMG (DShK-class) | 300 [3; E: six 50-round belts] | 0-2,000 [3] | 850 [3] | open sights [17; E] | 12.7 x 108 API, 0.048 kg [E] | bursts of 6 [23; E] at 600 rpm [3], one every 5 s [E] |

Speed 30 m/s top, 12 cruise [E: a pickup off the motorway].

## 3. Hit and damage figures (`effects`), all [E]

Each weapon's `effects` take docs/research/weapons.md's tables [24] (10.3: hit, one shot or burst,
day, clear, sea state 3 or less; 10.4: destroyed, stopped, disarmed given a hit) with these
adjustments, each an estimate with its reason. The damage model's conditions (10.5: night, fog,
sea state, target speed, range) apply on top, as for ours.

| weapon | hit chances | damage given a hit |
|---|---|---|
| ATGM-class missile | 10.3's missile column x0.8, rounded to 0.05: a gunner holding a beam-riding sight on the target from a rolling hull | 10.4's ~9 kg missile row with 10 to 20 points of the destroyed share moved to undamaged on boats and ships (a 4.6 kg shaped charge holes a hull rather than breaks it), little on vehicles (built against armour); a person 0.90 |
| 12.7 mm HMG on a small boat (`armed_fast_boat`) | 10.3's 12.7/7.62 mm column x0.5: that column assumes a stabilised remote weapon station (< 1 mrad); a pintle on a boat at speed is not one | 10.4's 12.7 mm row |
| 12.7 mm HMG on a ship, a cutter or a technical at the halt | the same column x0.75, rounded to 0.01: a steadier platform, still laid by hand | 10.4's 12.7 mm row |
| 7.62 mm GPMG | the 12.7/7.62 column x0.5, as the boat's HMG | about half the 12.7 mm row's destroyed and stopped shares (a round of a fifth the mass); a drone or a person as the 12.7 mm's |
| RPG-7 (PG-7VL) | short-range hit chances from the 1976 US Army evaluation in [5] (96 % at 100 m, 51 % at 200, 22 % at 300 against a panel crossing at 4 m/s): 0.30 a fast boat, 0.45 to 0.50 a slower boat, 0.70 a ship, 0.40 to 0.45 a vehicle, 0.20 a person; the damage model's missile range rule (full to half the maximum, half at it) gives the fall to 330 m | 10.4's PG-7VL row (the FPV's, without the duds) |
| 107 mm rocket | unguided, aimed by laying the launcher: 0.25 a ship, 0.10 a 40 m vessel, 0.03 a fast boat, 0.05 to 0.07 a vehicle | 1.3 kg of HE-frag [7], about four times a 40 mm grenade: 10.4's loitering-munition-300 row with the stopped share raised |
| 23 mm twin cannon | 10.3's 30/40 mm column x0.7, rounded to 0.05: a manual mount laid from a deck, not a stabilised station | 10.4's 30 mm HEDP row |
| LMM-class light missile | 10.3's missile column (it was built against fast inshore attack craft [12]); 0.30 a small drone and 0.50 a light aircraft (it has been fired at target drones [12]; no public hit rate) | a third of a Hellfire's warhead: destroyed shares 15 to 35 points below the ~9 kg row on boats and vehicles; a drone 0.95 and an aircraft 0.70 destroyed |
| Shahed-136-class | 10.3's GPS-bomb column (it strikes a coordinate [11]) | 10.4's SDB row (16 kg fill; a 50 kg warhead [11] of perhaps 15 to 20 kg of explosive) |
| WBIED, 150 kg | the Magura's hit chances (our catalog: operator video to the last second) | the Magura's 300 kg row for anything smaller than a ship; a ship stopped 0.55 and sunk 0.25 (half the charge) |
| FPV, PG-7VL | our `fpv` entry's, as it stands [24] | the same |
| 53-65 torpedo | 10.3's torpedo column; 0.10 a fast boat (a 45 kn torpedo homing on a 20 m/s boat's wake) | 307.6 kg [14], three times the Mk 54's: anything smaller than a ship destroyed, a ship sunk 0.70 and stopped 0.30 |

## 4. The kinds' own figures, all [E] unless sourced above

Speeds are what a node's own steering reads (stage 2: closing, opening out, fleeing); sensors are
what it perceives ours by (decision D2: a reach and the radio horizon, not the physical model).

| kind | cruise / top, m/s | basis |
|---|---|---|
| cargo ship | 6 / 8 | a coaster's or a freighter's harbour and sea speed (12 to 16 kn) |
| tanker | 6 / 7.5 | as a cargo ship, a little slower |
| ferry | 11 / 18 | the bay's ferries, 20 to 35 kn |
| patrol boat | 10 / 15 | a harbour patrol launch |
| fishing vessel | 4 / 5.5 | a crabber or trawler, 8 to 11 kn |
| sailboat | 3 / 4.5 | a 10 m yacht under sail or motor |
| small fast boat | 12 / 20 | a speedboat or RIB, 25 to 40 kn |
| light aircraft | 55 / 65 | a single piston engine, 105 to 125 kn |
| helicopter | 60 / 75 | a light twin, 115 to 145 kn |
| unknown UAS | 25 / 35 | a Group 2 or 3 UAS |
| small drone | 15 / 20 | a quadcopter |
| UUV | 2 / 2.5 | a REMUS-class vehicle (our catalog's) |
| periscope | 2.5 / 5 | a submarine at periscope depth, slow so as not to leave a feather |
| diver | 0.5 / 1 | finning, with a scooter at most |
| car | 12 / 30 | town and open road |
| truck | 10 / 25 | a laden truck |
| person | 1.4 / 5 | walking, running |

Sensors of the armed kinds [E]: a lookout's or a sight's eye at 2 to 3 m sees ours to about 5 km
(the horizon at 2 m is 5 km) on a fast boat, 4 km on a skiff, 3 km through a drone boat's camera,
3 km from a pickup; a ship's bridge at 10 to 15 m 10 to 12 km and its navigation radar 12 to 15 km
against a vessel; a cutter 8 km and 12 km; a Group 3 UAS's turret 8 km; an FPV's camera 2 km; a
submarine's periscope 8 km and its sonar 10 km against a ship.

## 5. For the next stages

- **Stage 2 (fire control):** `reload_s` (missiles, rockets, torpedoes), `burst_rounds`, `rate_rpm`
  and `burst_interval_s` (guns), `count`, the bands and `targets` are all here; a `self` weapon's
  kind has `expended_by_effect`. Damage on a contact reads its look-alike's figures (its truth
  `kind`); whether a struck contact is "armed" (the firepower share) is still `weapon.is_some()` in
  `effect.rs` and should read the catalog's `armed` once the nodes fire.
- **Stage 3 (bridge sensing):** `sense` names the signature each launch is seen by: `atgm` has
  figures; `rocket` (RPG and 107 mm: a launch flash and a short motor burn), `air_missile` (a
  missile launched from the air, flown in 3D), `gun` (a muzzle flash; no tracked flight) and
  `torpedo` (a launch transient for sonar, or none) need theirs.

## Sources

1. Wikipedia, "9M133 Kornet" (Kornet-E 100-5,500 m, > 250 m/s, 4.6 kg tandem HEAT, SACLOS laser beam riding, 27 kg; Kvartet for vehicles and boats). https://en.wikipedia.org/wiki/9M133_Kornet
2. Wikipedia, "NSV machine gun" (700-800 rpm, 845 m/s, 2,000 m against ground targets, 50-round belts; Utyos-M on patrol craft). https://en.wikipedia.org/wiki/NSV_machine_gun
3. Wikipedia, "DShK" (600 rpm, 850 m/s, 2,000 m effective in the infobox, 2.4 km in the text, 2,500 m maximum, 50-round belts; on cars and boats). https://en.wikipedia.org/wiki/DShK
4. Wikipedia, "Kord machine gun" (600-650 rpm, 860 m/s, 2,000 m, 50-round belts). https://en.wikipedia.org/wiki/Kord_machine_gun
5. Wikipedia, "RPG-7" (330 m effective with the PG-7V; 115 m/s from the tube, 295-300 m/s in flight; the 1976 US Army evaluation's 96/51/22 % at 100/200/300 m against a panel crossing at 4 m/s; PG-7VL 2.6 kg, > 500 mm RHA). https://en.wikipedia.org/wiki/RPG-7
6. Wikipedia, "PK machine gun" (600-800 rpm, 825 m/s, 1,000 m effective, 3,800 m maximum, belts of 25 to 250). https://en.wikipedia.org/wiki/PK_machine_gun
7. Wikipedia, "Type 63 multiple rocket launcher" (12 tubes, 8.05 km, 385 m/s, an 18.8 kg rocket with a 1.3 kg warhead; on technicals and an IRGC pickup). https://en.wikipedia.org/wiki/Type_63_multiple_rocket_launcher
8. Wikipedia, "Boghammar" (IRGC Navy: an RPG-7, three 12.7 mm machine guns, a 106 mm recoilless rifle, a 107 mm rocket launcher; 46 kn; 13 m). https://en.wikipedia.org/wiki/Boghammar
9. StrategyPage, "Sea Transportation: Missiles, Tankers Or Terror", 2021-01-16 (IRGC craft seen with anti-tank missiles), and The National Interest, "Iran Is Building 100-Knot Attack Speedboats" (fast attack craft with ATGMs, RPGs, recoilless rifles, machine guns and rocket launchers); both seen in search summaries, 2026-11-06, not read in full. https://www.strategypage.com/htmw/htseamo/20210116.aspx ; https://nationalinterest.org/node/91036
10. MarineLink, "Pirates board tanker off Somalia" (Ambrey: pirates on a skiff opened fire; an RPG fired at the vessel), November 2025; Arab News (Reuters), "Assailants attack tanker off Somalia in suspected pirate strike", 2025-11-04 (four armed attackers in a skiff). https://www.marinelink.com/news/pirates-board-tanker-off-somalia-highest-532096 ; https://www.arabnews.pk/node/2621322
11. Wikipedia, "HESA Shahed 136" (about 185 km/h, 2,500 km after Janes, a 50 kg warhead, 2.5 m span, GNSS and INS). https://en.wikipedia.org/wiki/HESA_Shahed_136
12. Wikipedia, "Lightweight Multirole Missile" (13 kg, a 3 kg blast-frag or shaped-charge warhead, > Mach 1.5 (510 m/s), 400 m to beyond 6 km, laser beam riding or SAL; against fast inshore attack craft; fired from the Camcopter S-100 and the Jackal, which carries two; fired at Banshee target drones). https://en.wikipedia.org/wiki/Lightweight_Multirole_Missile
13. Wikipedia, "Marine Protector-class coastal patrol boat" (87 ft, two M2 .50s, 25-26 kn). https://en.wikipedia.org/wiki/Marine_Protector-class_coastal_patrol_boat
14. Wikipedia, "53-65 torpedo" (307.6 kg of HE, 45 kn, 19,000 m for the 53-65K, wake homing, 533 mm). https://en.wikipedia.org/wiki/53-65_torpedo
15. Wikipedia, "Kilo-class submarine" (six 533 mm tubes, 18 torpedoes, 17 kn surfaced and 20 kn submerged). https://en.wikipedia.org/wiki/Kilo-class_submarine
16. Wikipedia, "ZU-23-2" (2,000 rpm cyclic and 400 practical a barrel, 970 m/s, 2 to 2.5 km, 50-round boxes; naval versions; on trucks and technicals). https://en.wikipedia.org/wiki/ZU-23-2
17. Wikipedia, "Technical (vehicle)" (heavy machine guns such as the DShK, Type 77 or M2, ZU-23-2s, recoilless rifles, ATGMs, Type 63 launchers; the Hilux and Land Cruiser most favoured). https://en.wikipedia.org/wiki/Technical_(vehicle)
18. The War Zone, "Heavy machine guns appear on key Russian commercial tanker in Baltic", 2026-06-29 (two 12.7 mm Kord guns in sandbagged positions on the *Marshal Vasilevskiy*, against naval drones). https://www.twz.com/sea/heavy-machine-guns-appear-on-key-russian-commercial-tanker-in-baltic
19. Wikipedia, "Project 23630 replenishment oiler" (86 m, 4,700 t, 12 kn; no armament listed). https://en.wikipedia.org/wiki/Project_23630_replenishment_oiler
20. GlobalSecurity, "IRGC-N Shahid Mahdavi" (container-derived launchers on a converted container ship's deck), and USNI News, "Iran Building Drone Aircraft Carrier from Converted Merchant Ship, Photos Show", 2023-01-03; seen in search summaries, 2026-11-06. https://www.globalsecurity.org/military/world/iran/irgcn-shahd-mahdavi.htm ; https://news.usni.org/2023/01/03/iran-building-drone-aircraft-carrier-from-converted-merchant-ship-photos-show
21. H I Sutton, "Yemen Houthi USV Guide" (Toofan-1 150 kg, 35 kt reported; Toofan-2 400 kg, 41 kt; first-generation 10 m boats 454 kg, 45 kt). https://www.hisutton.com/Yemen-Houthi-USV-Guide.html
22. Army Recognition, IDEX and IQDEX 2025 coverage of the Kornet-EM (2 to 3 rounds a minute), seen in a search summary, 2026-11-06; the primary figure not found. https://armyrecognition.com/news/army-news/2025/russia-attracts-iraqi-special-forces-interest-with-new-typhoon-kornet-em-anti-tank-missile-system-at-iqdex-2025
23. Wikipedia, "M2 Browning" (450-600 rpm, bursts of 5-7, 1,800 m effective, 890-930 m/s), as docs/research/weapons.md [26]; the R400's 500 rounds a gun is that note's [23]. https://en.wikipedia.org/wiki/M2_Browning
24. docs/research/weapons.md (this repository): section 6 (the FPV, its sources [34]-[37]) and sections 10.3 to 10.5 (the hit, damage and conditions tables).
