# Murmur platform loadouts and a coarse damage model (public sources only)

Research note for the catalog (`sim/platforms.toml`), 2026-11-02. Every figure carries a source number
from the list at the end, or [E] with its reasoning. No restricted data (JMEM or similar) was used or
approximated: the damage section is a game-level table built from public test results, public
combat claims and stated reasoning, in the spirit of War Thunder and Command: Modern Operations.

Conventions: ranges in metres, speeds in m/s, masses in kg. "Warhead" is the warhead's total mass
unless "fill" (the explosive alone) is said. "Count" is per platform per sortie or load.

What was taken (bd murmur-lbw8.1, decision rules 24): the loadouts of sections 1 to 8, as
`sim/platforms.toml`'s `loadout` lines record them with these sources ([n] below, [E] as here),
on the owner's choices of 2026-11-02 (docs/DECISIONS.md, "Real weapon loadouts on armed
platforms"): the MQ-9B's 4 AGM-114R and 2 GBU-12, the CCA's 2 AIM-120C, the Magura V5 (the boat
its own charge), the Switchblade 600, the FPV with a PG-7VL, the armed UGV's M2 (inert) and
Javelin, the Orca's 4 Hammerheads and the mine-laying UGV's 20 TM-62M (both inert, both unarmed
until stage 4). The damage model (section 10) was taken in stage 2 (murmur-lbw8.2, decision rules
25): 10.3's hit chances and 10.4's outcomes as each usable weapon's `effects` in the catalog (the
FPV's duds taken out of its damage shares; the Magura's hit chances [E] there, as 10.3 has no
column for an explosive boat), 10.5's multipliers in crates/murmur-contacts `damage.rs` (night and
fog by guidance, a rough sea against a small boat, a missile's range over the ground rather than
the slant [E: a missile released from an aircraft's height trades it for energy], points off for a
fast target in 5 m/s bands [E]; jamming is not a factor yet), and section 11's suggestion of one
table keyed by weapon class taken instead as per-weapon tables, so the logged weapon facts carry
their own figures. Guns were taken in stage 3 (murmur-lbw8.3, decision rules 26): the M2's 10.3
column and 10.4 row for a 12.7 mm burst as its per-burst `effects` (one burst is one shot of the
damage model; a burst's spread is folded into its hit chance, as 10.3's reasoning for the column
says, so no dispersion is modelled apart), bursts of 6 [26] at 525 rpm [26], one every 5 s [E: 72
rounds a minute, the M2's rapid rate of 40 or more [59]], and 10.5's guns rows in `damage.rs` (20
points a 5 m/s band past 15 m/s, x0.6 in fog, x0.6 against a boat in sea state 4-5); a round's
flight (some 2 s to 1,800 m at the muzzle speed [26], about 3 with drag [E]) is not modelled.
Mines were taken in stage 4 (murmur-lbw8.4, decision rules 27): sections 7 and 8's loadouts made
usable (both platforms now armed), with 10.3's mine column and 10.4's TM-62 row as the TM-62M's
`effects` (a car's or a truck's pass, 0.25 and 0.30; given a hit destroyed 0.95 and 0.60,
immobilised 0.05 and 0.40; no figures for a person, [44]'s 150 kg minimum), 10.3's torpedo column
and 10.4's torpedo row against a UUV as the Hammerhead's (0.50; 0.95 destroyed, 0.05 immobilised;
a submarine reads the UUV's figures), and these figures the research does not give, each [E] in
`sim/platforms.toml`: the Hammerhead's hearing, 1,000 m (the Mk 54's 9,100 m [12] is how far it
chases once launched, not how far the capsule listens; nothing on the capsule's sensors is public
[40]); the arming delays, 60 s for the TM-62M and 1,800 s for the Hammerhead; and the longest live
periods, 72 h and 720 h, which are Murmur's bounds and not the mines' (the TM-62M's MVCh-62 fuze
has no self-neutraliser; a lay order's live period, mandatory, stands in for one, docs/DECISIONS.md
"Mines"). A TM-62M is set off by a pass within its 1 m reach plus a vehicle's half-width, 1 m [E];
the magnetic fuze option [44] is not modelled. Section 8's spacing (1 to 4 m [43]) is the lay
order's to choose: its mines go evenly along its line.

## 0. What changes the plan (read first)

1. **The MQ-9B carries no torpedo in any public source.** SeaGuardian's anti-submarine work is
   sonobuoys only (pods, tested January 2025) [11]; the Mk 54 is a P-8 and helicopter weapon [12].
   Drop the Mk 54 from the MQ-9B. Its publicly integrated or ordered weapons are Hellfire,
   GBU-12/Mk 82, GBU-39, Brimstone 3 and Paveway IV, with JSM and Spear in integration [2,3,5].
2. **The CCA is armed in real life, the catalog's `[cca]` is not** (`armed` absent). Both Increment 1
   aircraft carry AIM-120: the YFQ-44A fired one live in July 2026 [15]; the YFQ-42A carries two
   internally [13,14]. Air-to-air only so far; the YFQ-44A has been fit-checked with GBU-54 Laser
   JDAMs, GBU-39s and rocket pods but has flown none of them [16]. Against Murmur's targets, an AIM-120
   is a weapon against drones and light aircraft only.
3. **The Magura V5 is a one-way weapon by design** (300 to 320 kg charge) [19,20], but the catalog's
   `[magura]` has no `expended_by_effect`. Either mark it spent by its effect, or arm it as the
   Sea Dragon variant (two R-73s, the boat survives the shot) [19], or as the V7 (two AIM-9s or a
   machine gun) [19]. These are three different loadouts; pick one per entry.
4. **The mine-laying UGV is unarmed in the catalog** (`armed` absent) although laying mines is an
   effect. Its real analogue is Tencore's TerMIT 2.0, 20 TM-62 anti-tank mines a run [43], which the
   catalog already cites for endurance. No public source shows a THeMIS mine-laying module: Milrem's
   THeMIS in Ukraine clears mines and carries engineers' mines as cargo [45].
5. **The Orca is unarmed itself; its 34 ft, 8 t payload module is a minelayer first** [38,39], meant
   for Hammerhead (a seabed capsule that releases a Mk 54 torpedo at a submarine) [40]. No count is
   published. Arming it makes `[orca]` armed.
6. **The owner's request amends a standing decision.** docs/DECISIONS.md (10-26) keeps effects
   abstract: "a point, a radius, a hit-or-miss draw; no warheads, blast or lethality". Named warheads,
   warhead masses and a damaged/destroyed outcome go past that line. It needs its own DECISIONS.md
   entry (the safeguards are unaffected: this is data about effects, not who authorises them).
7. **Everything else in the catalog is unarmed in real life** (section 9). Saronic's Corsair is
   marketed for "kinetic and non-kinetic effects" with a 1,000 lb payload, but no named weapon is
   public [56]: keep it unarmed.

## 1. MQ-9B SkyGuardian / SeaGuardian (`mq9b_armed`), with the MQ-9A where the B is silent

Airframe (B): 9 external hardpoints (8 wing, 1 centreline), 2,155 kg external and 363 kg internal
payload, 40+ h, 210 KTAS [1]. Per-station limits for the B are not published. The MQ-9A's six
pylons: inner 680 kg each, middle 270 kg each, outer 91 kg each, 1,700 kg total [2]; since 2020 a
Hellfire station can carry two (eight Hellfires on the A) [2].

Evidence of the B's weapons: Canada's MQ-9B buy includes 219 Hellfires and 12 Mk 82s; India's, 170
Hellfires and 310 GBU-39s [2]. The RAF's Protector (an MQ-9B) is cleared for Brimstone 3 and Paveway
IV (June 2026); Spear is under an MoU and JSM in integration [3]. Belgium is arming its MQ-9Bs with
Hellfire and Brimstone [5]; one older report counts 7 weapon stations with three Brimstones each, 21
in all [5, secondary].

| weapon | kind | count | min-max range | speed | profile | guidance | warhead | targets |
|---|---|---|---|---|---|---|---|---|
| AGM-114R Hellfire II (Romeo) | missile | 4 typical, 8 max [2; E: 4 is two dual rails, the A's usual mix with bombs] | 500-8,000 [6] (K/M/N/P: to 11,000 [6]) | ~440 (Mach 1.3) [6]; Murmur's 425 is consistent | direct or lofted; LOAL with an off-board laser [6] | semi-active laser [6] | multi-function, ~9 kg class [E: K's tandem HEAT is 9 kg [6]; R's mass not published] | vehicles, boats, people, light structures |
| AGM-114L Longbow Hellfire | missile | alternative to R | 500-8,000 [6] | ~440 [6] | direct | MMW radar, fire-and-forget, works in smoke and fog [6] | 9 kg tandem HEAT [6] | vehicles, boats (the LCS's anti-FIAC missile [50]) |
| Brimstone 3 | missile | 3 per station on a triple rack, 6-12 a sortie [5, secondary; E: 2-4 racks] | ~500-12,000 [E: MBDA publishes no range; fast-jet figures in the press are higher, a MALE's slow launch lower] | ~450 [E: Hellfire-class rocket, 50 kg, 1.8 m [4]] | direct; salvo | SAL, SAL + MMW radar (fast movers), or MMW fire-and-forget [4] | tandem shaped charge, ~9 kg class [E: same airframe class as Hellfire [4]] | fast boats and FIAC swarms, vehicles [4] |
| GBU-12 Paveway II | bomb | 2 [E: the A's middle stations; GBU-12 230 kg [7] fits the 270 kg limit [2]] | ~1,000-14,800 [7; min E: time to acquire the spot] | ~200 average from 7,600 m [E: free fall, ~40 s] | free fall then terminal glide | semi-active laser (later GPS too) [7] | Mk 82: 241 kg, 87-89 kg tritonal fill [10] | static and slow targets, ships, buildings |
| GBU-38 JDAM | bomb | 2 (alternative to GBU-12) [E: as above] | ~1,000-24,000 [8] | ~200 average [E] | glide | GPS/INS; CEP 5 m GPS, 30 m INS only [8] | Mk 82 [8,10] | fixed targets only (cannot follow a moving boat) [E: GPS aims at a coordinate] |
| GBU-39 SDB | bomb | 4 per station on a rack [E: the standard BRU-61 four-rack; India's order implies it [2]] | ~1,000-111,000 [9] | ~200 average [E: winged glide] | long glide, steep terminal dive | GPS/INS, CEP 1 m; laser SDB adds SAL [9] | 93 kg warhead, 16 kg AFX-757 fill [9] | fixed targets; laser SDB for movers |

**Recommended default for Murmur's maritime engagements** [E]: 4 x AGM-114R plus 2 x GBU-12. The
catalog's existing `munition = { name = "Hellfire-class missile", speed_mps = 425, range_m = 8000 }`
already matches the AGM-114R (Mach 1.3, 8 km). A "Brimstone variant" (8 x Brimstone 3, MMW mode
against fast boats) is the public anti-swarm loadout [3,4].

## 2. CCA (`cca`): YFQ-42A (General Atomics) and YFQ-44A (Anduril)

| aircraft | carriage | public weapons | count |
|---|---|---|---|
| YFQ-42A "Dark Merlin" | internal bay, ~4 m x 0.8 m [14, from a show model] | AIM-120 (captive carry with inert rounds planned; live shot expected late 2026) [13,15] | 2 AIM-120 [14; secondary, not official] |
| YFQ-44A Fury | two underwing hardpoints, no bay [15,16] | AIM-120, fired live July 2026 [15]; fit-checked only: 2 x GBU-54, 2 x LAU-131 rocket pods, GBU-39 [16] | 2 AIM-120 [16]; production FQ-44: 4 hardpoints, 4 AIM-120 [16] |

A compact AIM-9X for internal carriage is in the FY27 budget; one analyst guesses four or more
would fit the YFQ-42A's bay [17, opinion].

| weapon | kind | count | min-max range | speed | profile | guidance | warhead | targets |
|---|---|---|---|---|---|---|---|---|
| AIM-120C/D AMRAAM | missile (air-to-air) | 2 | ~1,000 [E: arming and guidance start] to 90,000 (C) / 160,000 (D, est.) [18] | up to ~1,370 (Mach 4) [18] | lofted, then active terminal | INS + datalink midcourse, active radar terminal [18] | 20 kg blast-fragmentation, proximity fuze [18] | aircraft, drones, cruise missiles |

[E] Against a Group 1 drone (a quadcopter) an AMRAAM is plausible but wasteful; its radar may not
hold a 0.01 m^2 target at range. In Murmur, give the CCA AIM-120s and limit their targets to
`small drone` and `light aircraft`.

## 3. Magura (`magura`): V5, V5 Sea Dragon, V7

Boat: 5.5 m, under 1,000 kg; max 42 kn (22 m/s), cruise 22 kn (11 m/s), range to 800 km [19];
brochure: 320 kg load, 101 km/h burst, 960 km at 45 km/h [20]. Guidance GNSS, inertial and visual,
with automatic target tracking by an EO device [19,20].

| variant | weapon | kind | count | range | speed | profile | guidance | warhead | targets |
|---|---|---|---|---|---|---|---|---|---|
| V5 (strike) | the boat | self | 1 | to 800,000 [19] | 22 (max) [19] | direct, at the waterline | operator video, GNSS/INS; EO tracker [19,20] | up to 300 kg [19] (320 kg load [20]), contact fuze [20] | ships, boats, piers |
| V5 Sea Dragon | R-73 (modified) | missile (surface-to-air) | 2 [19] | ~300 to ~10,000 [E: 30 km is air-launched at altitude [21]; launched from the sea surface into a climbing target, a few km to ~10 km] | Mach 2.5 max (~850) [21] | direct | IR, all-aspect [21] | 7.4 kg [21] | helicopters (a Mi-8 downed 31 Dec 2024 [19]) |
| V7 | AIM-9 (M or similar) | missile (surface-to-air) | 2 [19] | ~1,000 to ~10,000 [E: as the R-73; the infobox's 35 km is air-launched [22]] | Mach 2.5+ [22] | IR [22] | 9.4 kg annular blast-frag (AIM-9M) [22] | aircraft (two Su-30SM claimed, 2 May 2025, unverified [19]) |
| V7 alternative | machine gun on a turret | gun | 1 [19] | 0-1,800 [E: a 12.7 mm, as section 4] | n/a | direct | operator video | 12.7 mm [E] | drones, boats |

Combat record (claims, as listed [19]): several warships sunk (Ivanovets, Tsezar Kunikov, Sergey
Kotov) and boats destroyed; [E] each sinking took several drones, so model the warhead as
crippling, not sinking, a ship of thousands of tonnes per hit (section 10).

## 4. Armed UGV (`ugv_armed`): THeMIS with a remote weapon station

THeMIS has carried Kongsberg's Protector (M2 .50 and Javelin fired remotely in 2019; an M230LF
30 mm shown 2024) [24] and EOS's R400 [23]. The catalog's sights are already the EOS Guardian's.
Protector supports M2, M240/FN MAG, Mk 19, GMG and Javelin but publishes no round counts [25]; the
EOS R400 flyer does [23], so its figures are used here.

| weapon | kind | count (rounds) | min-max range | speed (muzzle) | profile | guidance | warhead | targets |
|---|---|---|---|---|---|---|---|---|
| M2 .50 (12.7 mm HMG) | gun | 500 rds [23] | 0-1,800 effective, 7,400 max [26] | 890-930 [26] | direct; bursts of 5-7 rounds [26] at 450-600 rpm [26] | stabilised sight, <1 mrad [23] | 12.7 x 99 ball/AP [26] | people, cars, boats, small drones |
| FN MAG / M240 (7.62 mm) | gun | 1,000 rds [23] | 0-800 (bipod) / 1,800 (mounted) effective, 3,500 max [28] | 840 [28] | direct; bursts [E: 5-10] at 650-1,000 rpm [28] | as above [23] | 7.62 x 51 [28] | people, soft vehicles, small drones |
| Mk 19 40 mm AGL | gun | 96 rds [23, "40 mm AGL"] | 0-1,500 point, 2,212 max [27] | 230-240 [27] | lobbed; 325-375 rpm cyclic, 40-60 sustained [27] | as above [23] | M430 HEDP: 50 mm RHA, ~5 m kill, ~15 m wound radius [27] | people, light vehicles, boats |
| M230LF 30 mm | gun | 150 rds (single) / 160 with a 7.62 (dual) [23] | 0-2,000 [23, "engagements up to 2 km"] | ~805+ [29] | direct; 200 rpm [29] | as above; proximity rounds for drones [29] | M789 HEDP: 21.5 g shaped charge, >50 mm RHA at 2,500 m [29] | vehicles, boats, drones (Slinger, gun trucks [29]) |
| FGM-148 Javelin | missile | 1 ready [E: the R400 shows one launcher [23]], reload by hand | ~65 [E: published Javelin minimums vary; not on [30]] to 4,750 (vehicle) [30] | ~150 average [E: no figure in [30]; about 30 s to 4.75 km] | top attack (150 m peak) or direct (60 m peak) [30] | IR imaging, fire-and-forget [30] | 8.4 kg tandem HEAT, >760 mm RHA [30] | armour, vehicles, boats, helicopters (direct mode) [30] |

**Recommended default** [E]: R400 dual: M2 (500 rds) + 1 Javelin, the configuration EOS draws
[23]; or Kongsberg's demonstrated M2 + Javelin [24]. A burst = 6 rounds [26]; 500 rds is ~80 bursts.

## 5. Switchblade (`switchblade`): 600 and 300

| weapon | kind | count | range | speed | endurance | profile | guidance | warhead | targets |
|---|---|---|---|---|---|---|---|---|---|
| Switchblade 600 | self | 1 | 40,000+ out, 90,000+ with a relay [31,33] | loiter 31 (113 km/h), dash 51 (185 km/h) [31] | 40+ min [31] | loiter, terminal dive; wave-off until ~4 s before impact [33] | operator video, EO/IR, aided tracker, secure GPS [31] | anti-armour, Javelin-derived [33]; ~8 kg [E: the Javelin's 8.4 kg tandem HEAT [30], no figure published]; munition 15 kg [31] | armour, vehicles, boats (Block 2 lists the sea domain [31]), air defence launchers (a Buk, June 2024 [33]) |
| Switchblade 300 Block 20 | self | 1 | 30,000 with the extended antenna [32] | loiter 28 (101 km/h), sprint 45 (161 km/h) [32] | 20+ min (10-15 min elsewhere on the page) [32] | as above [33] | as above [32,33] | fragmentation or EFP, modular [32]; "a 40 mm grenade" equivalent [33]; ~0.3 kg [E: 40 mm HE class]; munition 1.68 kg [32] | people, soft vehicles, light boats |

The catalog's 28 m/s cruise and 0.7 h match the 600's loiter speed and 40 min [31].

## 6. FPV strike quadcopter (`fpv`)

No single maker; figures are ranges across published types.

| item | value | source |
|---|---|---|
| speed | 28-44 m/s (100-160 km/h) max | [35] (a registry entry 100-140 km/h; KarguFPV 160 km/h) |
| range | 10-20 km by radio; [E] 10-20 km on fibre optic too (spool length) | [35]; [E] |
| endurance | 5-15 min [E: small batteries at high power; the catalog's 0.15 h is in this band] | [E] |
| payload | 1-4 kg | [35] |
| guidance | operator video (analogue or digital radio, or fibre optic); none in the last seconds if the link drops [E] | [36], [E] |

| warhead | kind | count | mass | effect | targets |
|---|---|---|---|---|---|
| PG-7VL (RPG-7 grenade head) | self | 1 | 2.6 kg grenade, 0.73 kg HMX fill | single HEAT, >500 mm RHA [34] | armour, vehicles, boats |
| PG-7VR | self | 1 | 4.5 kg, 1.43 kg fill [34] | tandem HEAT, 600-900 mm RHA [34] (heavy for most FPVs [E]) | armour |
| OG-7V | self | 1 | 2 kg, 0.21 kg fill [34] | fragmentation, 7 m lethal radius against body armour [34] | people, soft vehicles |
| TBG-7V | self | 1 | 4.5 kg, 1.9 kg fill [34] | thermobaric, ~10 m [34] | people in structures |

Profile: low approach, terminal dive or direct into the target's weak side [E]. **Recommended
default** [E]: PG-7VL against vehicles and boats, OG-7V against people.

## 7. Orca XLUUV (`orca`)

Vehicle: 85 ft (26 m) with the module, ~85 t [39]; 8 kn max, 2.5-3 kn cruise, 3,000 m depth
[42]; 6,500 nmi on one fuel module, ~150 nmi on battery [42]. Payload module: 34 ft (~10 m), 8 t,
top and possibly bottom hatches [38,39]. Mine laying is its first mission [38].

| weapon | kind | count | range | speed | profile | guidance | warhead | targets |
|---|---|---|---|---|---|---|---|---|
| Hammerhead (encapsulated Mk 54) | mine (releases a torpedo) | 4 [E: 8 t module; a Mk 54 is 276 kg [12], with capsule, mooring and sensors each about 1.5-2 t, a guess] | torpedo run to ~9,100 [12] | >20 (40+ kn) [12] | lies on the seabed, listens, releases the torpedo [40] | acoustic homing, active or passive/active [12] | 43.9 kg PBXN-103 (~108 kg TNT) [12] | submarines, large UUVs [40] |
| Mk 67 SLMM-class mobile mine (for scale only) | mine | 8-10 [E: ~750-800 kg each [41] into 8 t, if volume allows] | swims ~10 nmi to its site [41] | n/a | bottom influence mine [41] | magnetic, acoustic, pressure [41] | 150 kg HE [41] | ships, submarines |

The Navy has ordered 188 more Hammerheads (2026) [40]; the "Clandestine Delivered Mine" is the
other named payload, with nothing public on it [38]. **Recommended default** [E]: 4 Hammerheads.

## 8. Mine-laying UGV (`ugv_mines`)

| weapon | kind | count | laying | warhead | fuze | targets |
|---|---|---|---|---|---|---|
| TM-62M | mine | 20 (two modules of 10) [43] | one every 1-4 m in rows, a barrier of ~20-80 m a run [43] | 9.5 kg, 7.5 kg TNT [44] | pressure 150-550 kg (mechanical), or magnetic full-width (MVN-72/80) [44] | vehicles; never triggered by a person on foot [44, 150 kg minimum] |

Other public data points: a smaller UGV laid 13-14 TM-62s at 1-2 m; a trailer dispenser 15 at 2-3 m
[43, Fenix bulletins via search]. The Murmur domain is the bay, so [E] a land minefield matters on
roads and the shore only.

## 9. Platforms unarmed in real life

| catalog | real type | armed? | basis |
|---|---|---|---|
| rq4 | RQ-4 Global Hawk | no: HALE ISR, no weapon stations [58] | |
| fvr90 | L3Harris FVR-90 | no: ISR VTOL; payload is sensors [58] | |
| puma | AeroVironment Puma 3 AE | no: hand-launched ISR [58] | |
| x10 | Skydio X10/X10D | no: ISR quadcopter [58] | |
| lta | small airship | no: persistent ISR [58] | |
| ugv_logistics | THeMIS cargo/casevac | no: Ukraine uses it for cargo, casevac, route clearance [45] | |
| ugv_recon | small recon UGV | no [58] | |
| corsair | Saronic Corsair | no named weapon: "kinetic and non-kinetic effects", 1,000 lb payload [56] | keep unarmed |
| seahunter | Sea Hunter MDUSV | no: "not designed to carry weaponry"; tests were MCM and ASW payloads [57] | |
| saildrone | Saildrone Explorer | no: ocean data/ISR [58] | |
| buoy | drifting buoy | no | |
| bluerov | BlueROV2 | no: hobby/commercial ROV [58] | |
| remus | REMUS 600 | no: survey and mine-countermeasure AUV [58] | |
| tracker | antenna tracker | no | |

## 10. Damage model

### 10.1 How games structure it (models, briefly)

- **War Thunder**: no hit points for ground vehicles. Each vehicle is a set of modules (engine,
  transmission, tracks, gun breech and barrel, turret drive, ammunition, fuel) and individually
  modelled crew; a hit damages what the shell or fragments reach; a vehicle is destroyed when
  fewer than two crew remain (with exceptions) or by an ammunition or fuel explosion; damaged
  modules impose penalties (an engine loses power, a turret drive stops) [54]. Ships: compartments,
  each with its crew; damage disables a compartment and its crew, floods breaches, and crew loss
  or lost buoyancy sinks the ship [55]. The shooter's aim and ballistics decide the hit; there is no
  hit-probability table.
- **Command: Modern Operations**: abstract. Each weapon has a base probability of hit (PH or PoK),
  modified by range (a rocket missile peaks at 50% of its maximum range and falls to half its base
  PH at maximum; ramjets keep it to 75%), target speed and manoeuvre, crew proficiency, and fire
  control; a gun's PoK is per burst; unguided weapons use a CEP shrunk by the sight (25-75%) [52].
  A player's log shows base 80% minus a 10-point target-speed penalty [53]. Damage is in Damage
  Points, 1 DP = 1 kg TNT, against a unit's DP pool, with blast and fragmentation from near misses
  and separate damage to components (sensors, propulsion, weapons) [52].
- **Proposal for Murmur** [E]: CMO's structure for the draw (base P(hit) by weapon class and
  target kind, times condition multipliers, each logged), and a War Thunder-lite outcome with four
  states: undamaged, damaged-mobility (slowed or stopped; a boat dead in the water), damaged-
  firepower (its weapon or sensor out; meaningful only for armed targets), destroyed (sunk,
  burnt out, killed). One draw for hit, one for outcome; both replayable from the seed.

### 10.2 Public anchors

| fact | source |
|---|---|
| Hellfire CEP under 0.91 m | [6] |
| GBU-12 CEP 1.1 m over 99 guided drops (maker); unguided 94 m | [7] |
| JDAM CEP 5 m with GPS, 30 m INS only; SDB 1 m | [8,9] |
| Vietnam LGBs: ~48-50% direct hits (unguided 5.5%), miss distance 7 m against 136 m | [47] |
| Brimstone prototype hit and sank a 6 m inflatable at 20 kn in sea state 3; dual-mode rounds destroyed and sank a 40 ft FIAC with a single shot | [49] |
| LCS: four Longbow Hellfires hit four fast inshore attack craft (2018); a 2015 series reportedly 7 of 8 (secondary) | [50] |
| Javelin: maker claims >94% "engagement success"; an analyst estimates ~7% kills per shot in Ukraine (contested) | [51] |
| FPV: 20-40% effective (a Ukrainian commander, both sides); a Russian claim of 10 in 100 reach the target; a veteran's 43% when all goes right, 20-30% otherwise, ~10% of hits fail to detonate (secondary) | [36,37] |
| Blast AT mines under tracks mostly immobilise (tracks are ~20% of a tank's width); under the hull they often kill the crew | [46] |
| Magura V5 (300 kg) sank several warships, each, by public accounts, after several boats | [19], [E] on "several" |
| Laser guidance loses lock in cloud, fog, precipitation, smoke; Hellfire's high trajectory is limited by low ceilings | [48] |
| MMW Hellfire works in smoke and fog | [6] |

### 10.3 Probability of hit per shot (one missile, bomb, loitering munition, FPV, mine encounter, or one gun burst)

All [E], built on the anchors above. Assumes a valid shot: in range, target designated or locked,
day, clear, sea state 3 or less, no jamming. "-" means the weapon cannot engage that kind (draw 0).

| target kind | missile SAL/MMW/IR (Hellfire, Brimstone, Javelin) | guided bomb, laser (GBU-12) | guided bomb, GPS (GBU-38/39) | gun burst (12.7/7.62 mm, RWS) | gun burst (30/40 mm) | loitering munition (Switchblade) | FPV | mine (TM-62 encounter) | air-to-air missile | torpedo / Hammerhead |
|---|---|---|---|---|---|---|---|---|---|---|
| small fast boat (25+ kn) | 0.75 | 0.30 | 0.02 | 0.20 | 0.30 | 0.55 | 0.30 | - | - | - |
| fishing boat | 0.90 | 0.70 | 0.30 (if slow/anchored 0.85) | 0.50 | 0.60 | 0.75 | 0.40 | - | - | 0.70 |
| sailboat | 0.85 | 0.70 | 0.30 | 0.50 | 0.60 | 0.75 | 0.40 | - | - | 0.50 |
| cargo ship | 0.95 | 0.90 | 0.80 | 0.90 | 0.90 | 0.90 | 0.50 | - | - | 0.85 |
| car (moving) | 0.85 | 0.45 | 0.05 (parked 0.85) | 0.40 | 0.50 | 0.70 | 0.35 | 0.25 pressure / 0.90 magnetic | - | - |
| truck | 0.90 | 0.55 | 0.05 (parked 0.90) | 0.50 | 0.60 | 0.75 | 0.40 | 0.30 / 0.90 | - | - |
| small drone (Group 1-2) | - | - | - | 0.03 | 0.15 (proximity rounds) | - | - | - | 0.60 | - |
| light aircraft | 0.10 (Hellfire/Javelin vs low, slow) | - | - | 0.05 | 0.15 | - | - | - | 0.80 | - |
| UUV (submerged) | - | - | - | - | - | - | - | - | - | 0.50 |
| person / diver | 0.80 on land; diver - | 0.70 on land (blast) | 0.70 static | 0.30 | 0.45 (area) | 0.65 (300) | 0.30 | 0 (needs 150 kg [44]) | - | - |

Reasoning, by column:
- Missile: Hellfire/Brimstone tests against FIAC hit almost every time [49,50], but those were
  trials; 0.75 against a manoeuvring small boat and 0.85-0.95 against slower or larger targets
  leaves room for seeker, designation and motor failures (the Javelin gap between ">94%
  engagement success" and far lower combat kill rates [51]). Not built for air targets.
- Laser bomb: Vietnam's ~50% direct hits [47] is the floor for a static target with an old
  seeker; modern CEP 1.1 m [7] supports 0.9 against a ship. Against movers it depends on the bomb's
  slow terminal manoeuvre: 0.3 against a fast boat.
- GPS bomb: aims at a coordinate [8]; a moving target is a near miss unless it is slow or stopped.
- Guns: a burst of ~6 [26] from a <1 mrad mount [23] puts rounds within ~1 m at 1 km; small and
  fast targets are missed most of the time. CMO's informal gun-burst PoK against air targets is
  ~0.01 [52,53]; 0.03 here assumes an RWS's tracker, 0.15 with 30 mm proximity rounds [29].
- Loitering munition: operator in the loop to the last 4 s [33], a slower dive than a missile:
  below missiles, above FPVs. No public hit rate.
- FPV: the 20-40% band [36] at its middle; boats and ships bigger and slower than vehicles.
- Mine: pressure fuze only when a wheel or track crosses it (~20-30% of a pass [46], [E]);
  magnetic full-width [44] almost always.
- Air-to-air: no reliable public combat Pk for AIM-120 was found [E]; 0.6 against a small drone
  (tiny radar and IR signature) and 0.8 against a non-manoeuvring light aircraft.
- Torpedo: a lightweight torpedo [12] against a quiet UUV at 0.5 [E]; ships are easy to hear.

### 10.4 Outcome given a hit

Columns: undamaged / damaged-mobility / damaged-firepower / destroyed; rows sum to 1. All [E],
reasoned from warhead size [6,10,12,18,27,29,30,34,44] against target size; "firepower" applies
only to armed targets and is 0 otherwise.

| weapon class (warhead) | small fast boat (armed) | fishing boat | sailboat | cargo ship | car | truck (armed: technical) | small drone | light aircraft | UUV | person / diver |
|---|---|---|---|---|---|---|---|---|---|---|
| missile, ~9 kg HEAT or multi-function (Hellfire, Brimstone, Javelin) | 0.00/0.10/0.05/0.85 [49 sank both] | 0.05/0.40/0/0.55 | 0.05/0.30/0/0.65 | 0.55/0.45/0/0.00 | 0.00/0.10/0/0.90 | 0.00/0.15/0.05/0.80 | - | 0.00/0.10/0/0.90 | - | 0/0/0/1.0 (casualty) |
| guided bomb, Mk 82 class (89 kg fill) | 0/0/0/1.0 | 0/0.05/0/0.95 | 0/0/0/1.0 | 0.30/0.60/0/0.10 | 0/0/0/1.0 | 0/0/0/1.0 | - | - | - | 0/0/0/1.0 |
| guided bomb, SDB (16 kg fill) | 0/0.05/0/0.95 | 0/0.30/0/0.70 | 0/0.20/0/0.80 | 0.55/0.45/0/0 | 0/0/0/1.0 | 0/0.05/0/0.95 | - | - | - | 0/0/0/1.0 |
| gun burst, 12.7/7.62 mm | 0.45/0.30/0.15/0.10 | 0.80/0.17/0/0.03 | 0.85/0.13/0/0.02 | 0.99/0.01/0/0 | 0.40/0.40/0/0.20 | 0.50/0.30/0.10/0.10 | 0.10/0/0/0.90 | 0.30/0.40/0/0.30 | - | 0.20/0/0/0.80 |
| gun burst, 30 mm HEDP / 40 mm HEDP | 0.20/0.35/0.15/0.30 | 0.50/0.40/0/0.10 | 0.60/0.32/0/0.08 | 0.95/0.05/0/0 | 0.10/0.40/0/0.50 | 0.20/0.40/0.10/0.30 | 0.05/0/0/0.95 | 0.10/0.30/0/0.60 | - | 0.10/0/0/0.90 |
| loitering munition 600 (~8 kg tandem HEAT) | as missile | as missile | as missile | 0.60/0.40/0/0 | as missile | as missile | - | - | - | as missile |
| loitering munition 300 (40 mm grenade class) | 0.30/0.40/0.10/0.20 | 0.60/0.35/0/0.05 | 0.60/0.35/0/0.05 | 0.98/0.02/0/0 | 0.20/0.50/0/0.30 | 0.40/0.40/0.10/0.10 | - | - | - | 0.20/0/0/0.80 |
| FPV, PG-7VL (2.6 kg, 0.73 kg HMX) | 0.10/0.35/0.10/0.45 | 0.30/0.50/0/0.20 | 0.30/0.45/0/0.25 | 0.95/0.05/0/0 | 0.05/0.35/0/0.60 | 0.15/0.40/0.05/0.40 | - | - | - | 0.20/0/0/0.80 (OG-7V) |
| FPV dud (no detonation) | 10% of FPV hits become "undamaged" before the split [37] | | | | | | | | | |
| mine, TM-62 (7.5 kg TNT) | - | - | - | - | 0/0.05/0/0.95 | 0/0.40/0/0.60 | - | - | - | - |
| Magura V5 charge (300 kg) | 0/0/0/1.0 | 0/0/0/1.0 | 0/0/0/1.0 | 0.05/0.50/0/0.45 | - | - | - | - | - | - |
| air-to-air missile (AIM-120 20 kg, AIM-9 9.4 kg, R-73 7.4 kg, blast-frag) | - | - | - | - | - | - | 0.05/0/0/0.95 | 0.05/0.15/0/0.80 | - | - |
| torpedo, Mk 54 / Hammerhead (~108 kg TNT eq.) | - | 0/0.10/0/0.90 | 0/0/0/1.0 | 0.10/0.50/0/0.40 | - | - | - | - | 0/0.05/0/0.95 | - |

Reasoning in brief: a 9 kg warhead sinks a small boat (both Brimstone FIAC trials [49]) but only
damages a ship of thousands of tonnes; a 300 kg charge cripples or sinks a ship, though the public
sinkings took several boats [19]. A 12.7 mm burst rarely sinks anything but stops engines and
kills crew; a small drone dies to any hit. Light vehicles die to almost anything heavier than a
grenade; a TM-62 destroys a car and immobilises or destroys a truck (blast under the wheels [46]).

### 10.5 Conditions (multipliers on P(hit))

| condition | effect | basis |
|---|---|---|
| night | SAL, MMW, IR, thermal-sighted guns: x1.0 (the MQ-9's MWIR turret; Brimstone "true day/night" [4]; R400 thermal ID >4 km [23]); operator-video munitions with IR: x0.9; FPV on a day camera: x0.5; Magura V5 (EO by day, catalog note): x0.3 | [4,23], [E] |
| cloud below the shooter, fog | SAL weapons: no shot, or x0.3 if the seeker sees the spot only late; MMW (Hellfire L, Brimstone mode 2/3): x0.9; GPS bombs: x1.0 on fixed targets; IR seekers and EO operator video: x0.5; guns: x0.6 | [48], [6], [8], [E] |
| smoke, dust | SAL x0.5 (attenuation, false spots); MMW x0.95 | [48], [6] |
| sea state 4-5 (small targets) | missiles x0.85 (wave masking, seeker clutter); guns from a vessel or against a boat x0.6; FPV against boats x0.8; above 5, no small-boat engagements [E] | anchor: Brimstone's hit at SS3 [49]; [E] above it |
| target speed | CMO-style: -10 points per band above ~15 m/s for SAL missiles and loitering munitions, -20 for guns and FPVs; MMW modes x0.95 (built for fast movers [4]); GPS bombs against anything moving: 0.02-0.05 | [52,53], [4], [8] |
| range | CMO's rule for rocket missiles: full PH to 50% of maximum range, falling linearly to half at maximum | [52] |
| jamming | radio-linked FPVs x0.5 (the 43% to 20-30% spread [37]); fibre-optic FPVs x1.0; GPS bombs x0.6 (INS CEP 30 m [8]); Switchblade "secure GPS" x0.9 [31] | [8,31,37], [E] |

## 11. A data shape that fits the catalog (suggestion) [E]

The existing `munition = { name, speed_mps, range_m }` covers flight. A loadout could extend it:

```toml
loadout = [
  { name = "AGM-114R Hellfire II", kind = "missile", class = "missile_sal", count = 4,
    min_m = 500, max_m = 8000, speed_mps = 425, profile = "direct", guidance = "laser",
    warhead = "multi-function", warhead_kg = 9, targets = ["boat", "vehicle", "person"] },
  { name = "GBU-12 Paveway II", kind = "bomb", class = "bomb_laser", count = 2,
    min_m = 1000, max_m = 14800, speed_mps = 200, profile = "glide", guidance = "laser",
    warhead = "Mk 82 GP", warhead_kg = 241, targets = ["ship", "boat", "vehicle", "structure"] },
]
```

with the P(hit) and outcome tables keyed by `class` and target kind in one data file (not per
platform), so a change to the damage model is one place.

## Sources

1. General Atomics Aeronautical Systems, "MQ-9B SkyGuardian" datasheet (010923). https://www.ga-asi.com/images/products/aircraft_systems/pdf/mq9b-skyguardian-datasheet-02012023.pdf
2. Wikipedia, "General Atomics MQ-9 Reaper" (pylon limits, 2020 dual Hellfire, Canada and India MQ-9B weapon packages). https://en.wikipedia.org/wiki/General_Atomics_MQ-9_Reaper
3. AIN Online, "General Atomics adds more weapons to SkyGuardian", 2026-07-22 (Protector cleared for Brimstone 3 and Paveway IV; Spear MoU; JSM). https://www.ainonline.com/aviation-news/defense/2026-07-22/general-atomics-adds-more-weapons-sky-guardian
4. MBDA, "Brimstone" datasheet 2023-08-v05 (50 kg, 1.8 m, three modes, tandem shaped charge, MQ-9B). https://mbdainc.com/wp-content/uploads/2025/03/2023-BRIMSTONE-datasheet.pdf
5. Army Recognition, "Belgium moves toward armed MQ-9B with U.S. Hellfire and UK Brimstone missiles", 2026 (seen in search summary; the 7 stations x 3 Brimstone figure is from an older article in the same results). https://armyrecognition.com/news/aerospace-news/2026/belgium-moves-toward-armed-mq-9b-with-u-s-hellfire-and-uk-brimstone-missiles-for-nato-strike-missions
6. Wikipedia, "AGM-114 Hellfire" (variants, ranges, Mach 1.3, CEP, Longbow MMW in smoke and fog). https://en.wikipedia.org/wiki/AGM-114_Hellfire
7. Wikipedia, "GBU-12 Paveway II" (230 kg, 14.8 km, CEP 1.1 m). https://en.wikipedia.org/wiki/GBU-12_Paveway_II
8. Wikipedia, "Joint Direct Attack Munition" (GBU-38 253.6 kg, 24 km, CEP 5 m / 30 m; GBU-54). https://en.wikipedia.org/wiki/Joint_Direct_Attack_Munition
9. Wikipedia, "GBU-39 Small Diameter Bomb" (129 kg, 16 kg AFX-757, 111 km, CEP 1 m). https://en.wikipedia.org/wiki/GBU-39_Small_Diameter_Bomb
10. Wikipedia, "Mark 82 bomb" (241 kg, 87-89 kg tritonal). https://en.wikipedia.org/wiki/Mark_82_bomb
11. The Defense Post, "GA-ASI tests SeaGuardian anti-submarine warfare", 2025-02-27 (sonobuoy dispensing, no torpedo). https://thedefensepost.com/2025/02/27/test-seaguardian-anti-submarine-warfare/
12. Wikipedia, "Mark 54 Lightweight Torpedo" (276 kg, 43.9 kg PBXN-103, 40+ kn, 9.1 km). https://en.wikipedia.org/wiki/Mark_54_Lightweight_Torpedo
13. Air & Space Forces Magazine, "CCAs start carrying inert missiles, live fire shots expected later this year". https://www.airandspaceforces.com/cca-inert-weapons-tests-live-fire
14. European Security & Defence, "Paris Air Show 2025: GA-ASI YFQ-42A model provides new clues to capabilities" (bay ~4 x 0.8 m, two AMRAAM-size missiles; from a model). https://euro-sd.com/2025/06/news/air/45059/ga-asi-unveils-yfq-42a-model/
15. The War Zone, "YFQ-44A Fury fighter drone has fired its first AIM-120 AMRAAM missile", July 2026. https://www.twz.com/air/yfq-44a-fury-fighter-drone-has-fired-its-first-aim-120-amraam-missile
16. The Aviationist, "Anduril's YFQ-44A Fury expands its weapons options with air-to-ground munitions", 2026-09-16. https://theaviationist.com/2026/09/16/andurils-yfq-44a-fury-air-to-ground-munitions/
17. Air & Space Forces Magazine, "Smaller AIM-9X could give drone wingmen more firepower". https://www.airandspaceforces.com/smaller-aim-9x-cca-more-firepower/
18. Wikipedia, "AIM-120 AMRAAM" (mass, 20 kg warhead, Mach 4, ranges, guidance). https://en.wikipedia.org/wiki/AIM-120_AMRAAM
19. Wikipedia, "MAGURA V5" (V5, Sea Dragon, V7, combat record and claims). https://en.wikipedia.org/wiki/MAGURA_V5
20. European Security & Defence, "MAGURA V5 at Eurosatory 2024" (brochure: 320 kg, 101 km/h, 960 km). https://euro-sd.com/2024/06/major-news/38905/magura-v5-at-eurosatory-2024-2/
21. Wikipedia, "Vympel R-73" (105 kg, 7.4 kg warhead, Mach 2.5, 0.3-30 km air-launched). https://en.wikipedia.org/wiki/Vympel_R-73
22. Wikipedia, "AIM-9 Sidewinder" (AIM-9M 86 kg, 9.4 kg WDU-17/B; AIM-9X same warhead). https://en.wikipedia.org/wiki/AIM-9_Sidewinder
23. EOS, "R400" flyer R400-05/2024 (ammunition per weapon, <1 mrad, Javelin option, sensor ranges). https://militarysystems-tech.com/sites/militarysystems/files/supplier_docs/03-EOS-Defence-R400-flyer.pdf
24. Kongsberg, "Displays remote weapon station on UGV" (2019) and Contact, "Unmanned ground vehicle fires Javelin and guns" (2019); Milrem at Eurosatory 2024 (THeMIS with M230LF). https://www.kongsberg.com/news/news-archive/2019/displays-remote-weapon-station-on-ugv/ ; https://contactairlandandsea.com/2019/09/07/unmanned-ground-vehicle-fires-javelin-and-guns/
25. Wikipedia, "Protector RWS" (weapons supported; no round counts). https://en.wikipedia.org/wiki/Protector_RWS
26. Wikipedia, "M2 Browning" (450-600 rpm, bursts of 5-7, 1,800 m effective, 7,400 m max). https://en.wikipedia.org/wiki/M2_Browning
27. Wikipedia, "Mk 19 grenade launcher" (rates, 1,500 m point, 2,212 m max, M430 effects). https://en.wikipedia.org/wiki/Mk_19_grenade_launcher
28. Wikipedia, "FN MAG" (650-1,000 rpm, 800/1,800 m effective, 3,500 m max). https://en.wikipedia.org/wiki/FN_MAG
29. Wikipedia, "M230 chain gun" (M230LF 200 rpm, M789 HEDP, counter-UAS rounds and systems). https://en.wikipedia.org/wiki/M230_chain_gun
30. Wikipedia, "FGM-148 Javelin" (8.4 kg tandem HEAT, ranges, top/direct attack). https://en.wikipedia.org/wiki/FGM-148_Javelin
31. AeroVironment, "Switchblade 600" product page. https://www.avinc.com/solution/switchblade-600/
32. AeroVironment, "Switchblade 300 Block 20" product page. https://www.avinc.com/solution/switchblade-300-block-20/
33. Wikipedia, "AeroVironment Switchblade" (Javelin-derived warhead, wave-off, combat use). https://en.wikipedia.org/wiki/AeroVironment_Switchblade
34. Wikipedia, "RPG-7" (PG-7VL, PG-7VR, OG-7V, TBG-7V masses and effects). https://en.wikipedia.org/wiki/RPG-7
35. FPV figures: Ukraine GUR "War & Sanctions" UAV registry entry (100-140 km/h, 2-4 kg) https://war-sanctions.gur.gov.ua/en/uav/486 ; STM, "KarguFPV" (160 km/h, 10 km, 1.4 kg) https://www.stm.com.tr/en/kargufpv-en
36. Ukrinform, "Efficiency of FPV drone use by AFU, Russian forces: 20-40%, Ukrainian military". https://ukrinform.net/rubric-ato/3938501-efficiency-of-fpv-drone-use-by-afu-russian-forces-2040-ukrainian-military.html
37. Secondary reports of FPV hit rates (a Russian counter-drone commander's "10 of 100"; a former Slovak officer's 43% / 20-30% / ~10% duds), seen in search summaries; primary texts not read. Kyiv Post analysis https://www.kyivpost.com/analysis/32482
38. The War Zone, "Navy's 85-foot Orca unmanned submarine will be a minelayer first". https://www.twz.com/navys-85-foot-orca-unmanned-submarine-will-be-a-minelayer-first
39. Naval News, "First look at the US Navy's Orca XLUUV with massive payload module", June 2024. https://www.navalnews.com/naval-news/2024/06/our-first-look-at-the-us-navys-orca-xluuv-fitted-with-payload-module/
40. The Defense Post, "US Navy Hammerhead mines", 2026-08-03 (encapsulated Mk 54; 188 more ordered). https://thedefensepost.com/2026/08/03/us-navy-hammerhead-mines/
41. FAS, "Mk 67 Submarine Launched Mobile Mine", and Wikipedia, "Submarine Launched Mobile Mines". https://man.fas.org/dod-101/sys/ship/weaps/mk-67.htm ; https://en.wikipedia.org/wiki/Submarine_Launched_Mobile_Mines
42. Naval Technology, "Orca XLUUV" (8 kn max, 2.5-3 kn cruise, 3,000 m, 6,500 nmi). https://www.naval-technology.com/projects/orca-xluuv/
43. UNITED24 Media, "Ukraine's TerMIT ground drone adapts to TM-62 mines with new mine-laying module", 2026-03-04 (citing Militarnyi and Tencore); Fenix Insight bulletins on TM-62-laying UGVs. https://united24media.com/latest-news/ukraines-termit-ground-drone-adapts-to-tm-62-mines-with-new-mine-laying-module-16464
44. Wikipedia, "TM-62 mine" (9.5 kg, 7.5 kg TNT, fuzes, 150-550 kg trigger). https://en.wikipedia.org/wiki/TM-62_mine
45. Ukrinform, "Ukraine to receive six THeMIS unmanned demining vehicles from Milrem Robotics"; Milrem, "Ukrainian forces are taking full advantage of their THeMIS UGVs" (2024). https://www.ukrinform.net/rubric-defense/3995367-ukraine-to-receive-six-themis-unmanned-demining-vehicles-from-milrem-robotics.html ; https://milremrobotics.com/ukrainian-forces-are-taking-full-advantage-of-their-themis-ugvs/
46. Wikipedia, "Anti-tank mine" (track hits immobilise; tracks ~20% of width; belly hits kill crews). https://en.wikipedia.org/wiki/Anti-tank_mine
47. Wikipedia, "Laser-guided bomb" (Vietnam direct-hit rates, miss distances). https://en.wikipedia.org/wiki/Laser-guided_bomb
48. US Army FM 34-81-1, Appendix F (weather effects: cloud, fog, precipitation on laser designation; Hellfire ceilings), via GlobalSecurity. https://www.globalsecurity.org/intell/library/policy/army/fm/34-81-1/appf.htm
49. UK Defence Journal, "Brimstone missile impresses against fast inshore attack craft" (6 m inflatable at 20 kn, SS3; 40 ft FIAC). https://ukdefencejournal.org.uk/brimstone-missile-impresses-fast-inshore-attack-craft/
50. USNI News, "USS Milwaukee fires LCS surface-to-surface missile module developmental test", 2018-05-16. https://news.usni.org/2018/05/16/uss-milwaukee-fires-lcs-surface-surface-missile-module-developmental-test
51. Lockheed Martin, "40,000 Javelin missiles delivered and counting" (2014, >94%); D. Hambling's critique, summarised at lucorg.com, "Reassessing Saint Javelin". https://news.lockheedmartin.com/2014-12-02-40-000-Javelin-Missiles-Delivered-and-Counting ; https://www.lucorg.com/?p=18562
52. Matrix Games, "Command: Modern Operations Mega-FAQ" (PH by range, gun PoK per burst, CEP and sights, 1 DP = 1 kg TNT). https://command.matrixgames.com/?page_id=2920
53. Steam Community, Command: Modern Air/Naval Operations discussion "Missile defence grossly overpowered" (combat log: base 80%, -10 target speed). https://steamcommunity.com/app/321410/discussions/0/1470841715961806980/
54. War Thunder Wiki, "Ground vehicle modules" (modules, crew, destroyed below two crew). https://wiki.warthunder.com/mechanics/4775-ground-vehicle-modules
55. War Thunder Wiki, "Ship crew mechanics" (compartments, crew loss). https://wiki.warthunder.com/640-ship-crew-mechanics
56. Defense Daily, "Saronic introduces Corsair" (24 ft, 1,000 lb payload, 35+ kn, "kinetic and non-kinetic effects"). https://defensedaily.com/saronic-introduces-corsair-an-asv-with-range-and-speed-to-keep-pace-with-surface-fleet/unmanned-systems
57. Military Times, "This unmanned submarine chaser is closer to reality", 2018-02-05 (Sea Hunter not designed to carry weapons). https://www.militarytimes.com/unmanned/uas/2018/02/05/this-unmanned-submarine-chaser-is-closer-to-reality
58. Standard public descriptions of the type (maker pages and Wikipedia: RQ-4 Global Hawk, L3Harris FVR-90, AeroVironment Puma 3 AE, Skydio X10D, Saildrone Explorer, Blue Robotics BlueROV2, HII REMUS 600); not re-read in this session, as each is an ISR, survey or hobby type with no weapon option offered.
59. US Army FM 3-21.8 (2007), the M2's rates of fire (sustained under 40 rounds a minute, rapid 40 or more, cyclic 450 to 550), seen in a search summary of its specifications table, 2026-11-02; the manual itself not read. https://www.marines.mil/Portals/1/Publications/FM%203-21.8%20%20The%20Infantry%20Rifle%20Platoon%20and%20Squad_7.pdf
