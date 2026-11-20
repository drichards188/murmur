# Grafenwoehr 12 November 2026 demo: public figures for Murmur

Research note, 2026-11-20, for docs/SALVO.md (bd murmur-4nik). Public sources only; nothing
non-public, export-controlled or operationally sensitive was sought. Figures are rounded; each
carries a reference number [n] or [E] (estimated, with its reasoning). Labels on references: **high** (primary read this session),
**medium** (reputable secondary read, or a search summary of a reputable source), **low**
(search summary only, or memory not checked). Vendor claims are marked "vendor". Closed
programmes are marked "closed", open-source work "open".

Format follows docs/research/weapons.md. Existing Murmur entries ([fpv], [switchblade],
[ugv_armed], [ugv_logistics]) were not re-researched; they are the baseline the picks compare to.
The sources already read by the owner (armyrecognition 15 Nov, dronexl 16 Nov, thedefensewatch
17 Nov, aerospaceglobalnews 19 Nov) were not re-read. defence-blog.com was not retried.

## Summary of picks

| platform | pick | basis |
|---|---|---|
| HIV3-class launcher (on the R3NEGADE) | 6 tubes, 3 drones a tube (18 in all) [E from caption + Stripes count]; one drone every 2 s, a full salvo in about 40 s [E]; reload by hand 20 min [E]; 250 kg [E] | 1.1, 2 |
| Strike drone | 10-inch-class tube-launched FPV/folding quad; cruise 20 m/s, dash 35 m/s [E]; 10 km, 15 min [E]; 1 kg warhead [E]; fixed day camera; terminal lock from 500 m | 3 |
| R3NEGADE-class UGV | about 1.5 t [E], 6 m/s road, 3 m/s off road [E]; payload 750 kg [E from THeMIS]; leaves a 20-ft container in 90 s [E] | 4 |
| TRV-150 marker drone | 68 kg payload, 30 m/s, 8 to 45 km by load (take 14 km loaded), 36 min | 4.2 |

## 1. The demo itself

New facts beyond the four reports already read, each from a source below.

| item | what was found | source |
|---|---|---|
| Date, place, organisers | 12 Nov 2026, Grafenwoehr Training Area; DEVCOM Armaments Center with USAREUR-AF and 7th ATC; led by Troy Brannon (DEVCOM AC strategic adviser) | [1], [2] |
| Release | army.mil, Capt. Angelo Mejia, 17 Nov 2026 | [1] |
| Sequence (Army) | a drone dropped smoke on a target; a group of drones from an HIV3 swarm launcher on a R3NEGADE "conducted an autonomous strike on a target"; 173rd soldiers struck a target with an FPV drone | [1] |
| Drone count | Army release: none given. Stars and Stripes: a swarm of 18 | [1], [2] |
| Cadence, reload | not stated anywhere found (Army, Stripes, Defence Industry Europe, Interesting Engineering, Daily Wire, Global Defense Corp) | [1]-[6] |
| Distance launcher to target | not stated. Stripes: smoke rose "from over a mile downrange" (so the marked target was more than about 1.6 km from the observers' view, not necessarily from the launcher) | [2] |
| Target type | not stated in the Army release. Stripes: targets "spread across" the area, some strikes "simulated killing vehicle crews" | [1], [2] |
| Designation and the smoke's role | Army: smoke "onto a target", no purpose given. Daily Wire says the TRV-150 "dropped smoke to mark the target", the only source that gives it a role (its own gloss, not quoted from the Army) | [1], [5] |
| Who selected or authorised the strike | not described in the Army release, Stripes, Defence Industry Europe, Daily Wire. Stripes: "up to ground commanders how and where the systems are used" (a general remark, not about this strike). The release calls the strike "autonomous" | [1], [2], [5] |
| Operators | the release names Sgt. Dean Aguirre and Sgt. James Hoban (3-504 PIR); Stripes names Aguirre and Cpl. Tyler Dooros, flying FPV drones from a hide with goggles and game-style controllers. No count of people for the swarm launch | [1], [2] |
| Duration | Stripes: the attacks ended "within a few minutes" | [2] |
| Programme | R3ACT (Remote Rapid Response Autonomous Contingency Team), PM Alex Tomosieski; modular air and ground platforms in containers so commanders can pre-position them. Stripes ties it to the Eastern Flank Deterrence Initiative. Army officials: a technology evaluation, no fielding decision or timeline | [1], [2], [7] |
| Other items | TRV-400 on static display with the developmental FIR3FLY anti-armour payload; a captioned image of a R3NEGADE with a CROWS | [1] |

**Contradictions and gaps against the reports already read, and against each other**

- Javelins: the Army release says one R3NEGADE carried "a remotely operated weapon station with a
  Javelin missile". Stripes says a UGV with **two** Javelin launchers came out of the trees. Neither
  says a missile was fired. Static versus fired stays open.
- Which vehicle: Stripes says a "second UGV, launched from a pre-positioned container, fired a
  swarm of 18". The Army says the HIV3 vehicle came out of a container (the 20-foot figure is
  Daily Wire's and Interesting Engineering's, not in the Army text read) and a different R3NEGADE
  carried the Javelin. The order "first" and "second" differs by source.
- Stripes adds what the Army release omits: "resupply drones armed with rockets" attacked
  alongside one-way attack drones, then strikes simulating crew kills. That is more than one
  swarm strike.
- Global Defense Corp's headline says the swarm came from a USV; its body says UGV. A headline
  error [6].
- The "six-tube" caption and a count by tube could not be verified (the captions are not in the
  text the fetch returned). The only count is Stripes' 18. Interesting Engineering calls the
  drones "FPV drones"; the Army says only "a group of drones".
- No source gives cadence, a launcher-to-target distance, a target type, or who authorised.
  The Army has not released the HIV3's communications, drone specifications, accuracy or
  jamming resistance [8] (low, a search summary of an aggregator).

## 2. HIV3-class launchers: public comparables

| launcher (status) | tubes or bays | drones per salvo | seconds between launches | reload | mass class | source |
|---|---|---|---|---|---|---|
| ISS Aerospace HAL10 (UK, vendor; firing test Mar 2026) | 10 bays, cassette 5x2; drone WASP, tube-launched, rocket-boosted | 10, "rapid succession" | not stated | not stated | "compact" | [9] medium |
| Auterion Nemyx VLS (vendor; announced 5 Oct 2026; first integration partners only) | 2x2 or 4x4 tube arrays or an ISO container; separate tubes for 10-inch FPV and for interceptors; an elevator in each tube gives 30 % more drones per volume | not stated (Janes: Auterion did not confirm a swarm number) | not stated | not stated | multi-hour battery | [10] medium, [11] medium |
| SpearUAV Viper MCL (Israel, vendor; 2023) | at least 4 canisters, configurable | "simultaneously or in sequence" | not stated | not stated | not stated; fits any manned or unmanned platform | [12] medium |
| DZYNE BlitzBox (US, vendor; May 2026) | rails, not tubes; 10-ft box 16 drones, 40-ft box up to 100 (in development); 4-rail carried launcher | 16 or 100 | not stated; software sequences launches against collisions | boxes store spare drones to reload launchers | 10-ft and 40-ft ISO boxes | [13] medium |
| China Swarm-2 / "Atlas" (state media, demo 25 Mar 2026) | 48 fixed-wing drones on one launcher vehicle; a command vehicle controls up to 96 | 48 | **3** (one every 3 s, order adjustable) | not stated | truck | [14] medium (state media via a trade outlet) |
| China 48-tube truck launcher (2020 test) | 48 tubes on a 6x6 | 48 | not found | not found | 6x6 light vehicle | [15] low |
| Estonia DroneHive | no result found | | | | | search found nothing; not verified |

What it gives Murmur: tube or cassette counts run from 4 to 48 on a vehicle; the only public
launch interval is China's 3 s (state media, not independent); no vendor publishes reload time.
Salvo of 10 to 18 is the open-source-reported range for a land-vehicle launcher.

**Picks (all [E])**: tubes 6, drones 18 (3 a tube, stacked), interval 2 s (between the Chinese
3 s and "rapid succession"; a 40 s salvo keeps the 18 inside one "few minutes" strike), reload
by hand, 20 min (no source; a UGV has no loader), mass 250 kg (18 small quads at 3 to 8 kg with
tubes and a frame; rides on a UGV of 750 kg payload, part 4).

## 3. The drones and terminal guidance

### 3.1 Drone figures (comparables; the Army gave none for the HIV3 drone)

| item | figure | source |
|---|---|---|
| FPV strike quad, speed | 28 to 44 m/s max (existing Murmur entry) | weapons.md [35] |
| WASP (HAL10's drone, ISS Aerospace, vendor) | payload up to 1 kg; range up to 45 km; 30 min unloaded, 22 min at full payload | [9] |
| Blitz (DZYNE, vendor; a fixed-wing, not a quad) | cruise 40 to 75 KEAS (20 to 39 m/s); 80 km and 1 h standard battery, 150 km and 2 h extended; payload up to 2.3 kg (5 lb); 0.9 kg (2 lb) fragmentation warhead option | [13] |
| Vyriy-10 class 10-inch FPV with terminal guidance (Ukraine, vendor) | about $450 | [16] |
| Camera | a small fixed day camera; existing Murmur [fpv] sensor (150 degrees, 720 px) stands | sim/platforms.toml |

**Picks [E]**: cruise 20 m/s, dash 35 m/s, range 10 km (a launcher-borne short-range drone, not
a 45 km WASP), endurance 15 min, warhead 1 kg class (between the WASP's 1 kg and Blitz's 0.9 kg
frag; weapons.md's 2.6 kg PG-7VL stays the FPV entry), fixed camera.

### 3.2 Autonomous terminal guidance ("last mile")

| item | what it does | figure | source |
|---|---|---|---|
| The Fourth Law TFL-1 (Ukraine, vendor) | the operator flies in; in the last 500 m an onboard computer takes over, the operator stops controlling | handoff 500 m (550 yards); module $50 to $100; fielded in the Vyriy-10-TFL-1 | [16] medium, [17] medium |
| Fourth Law claims (vendor) | "2 to 4 times" strike effectiveness, 2 to 5 times mission success; cost up 10 to 20 % | no absolute baseline given | [16] |
| TFL-1 trial and combat reports | 10 of 10 hits in trial launches; a brigade reported about 50 % in its first 20 combat uses | small samples | [17] low (search summary) |
| Auterion Skynode S (vendor; with SkyFall Shrike) | the operator designates the target on the camera feed, then switches to terminal guidance; fire and forget, no GPS or link needed | designation up to about half a mile (800 m) before the switch; vendor claim of a 100 % hit rate in first deployments against 20 to 40 % for manually flown drones that lost the link | [18] low, one claim traced to the CEO |
| Auterion's statement on people | "humans assign targets and authorize engagement, while the software handles navigation, swarm coordination, and terminal guidance" | | [18] low; Auterion Jan 2026 in SWARMS.md 12 |
| Dwarf Engineering Narsil, TAF Last Mile (Ukraine, vendors) | homing modules; tests on a stationary and a moving target; high accuracy in electronic warfare claimed | no numbers | [17] low |

Independent, controlled with-and-without hit rates were not found. Every figure above is a
vendor's or a small field report. For Murmur: terminal lock-on range 400 to 800 m; the selection
is a person's box on the video, the drone only tracks and dives; the useful modelled effect is
that a lost link (which kills a manual FPV) no longer kills the strike in the last 500 m.

## 4. R3NEGADE-class UGV and the TRV-150

### 4.1 UGV

No maker, specification or weight for the R3NEGADE was found in any public source (searched
twice; the Army release has none). Comparable (closed or open? the THeMIS is a commercial,
closed-source product): Milrem THeMIS, speed 20 km/h (5.6 m/s), tow 80 km/h; rated payload 750 kg,
maximum 1,200 kg; 247 x 204 x 117 cm; 1,650 kg [19] medium.

| item | pick | basis |
|---|---|---|
| mass | 1.5 t | [E] THeMIS-class |
| speed | 6 m/s road, 3 m/s off road | [19]; off-road halved [E] |
| payload | 750 kg (launcher 250 kg, plus a weapon station or a Javelin pair about 60 kg [E]) | [19] |
| 20-ft container | internal about 5.9 x 2.35 x 2.39 m [E, ISO standard dimensions from memory, low]: holds the UGV plus a pallet of spare drones; one UGV and its launcher fits with room to turn nowhere, so it drives straight out | [E] |
| time to deploy from the container | 90 s from the door opening to the vehicle clear [E: doors, ramp, a remote start; no figure public] | [E] |
| existing Murmur entries | [ugv_armed] (THeMIS plus a weapon station) is the nearest; a launcher variant changes loadout, not mobility | sim/platforms.toml |

### 4.2 TRV-150 (Malloy, with BAE Systems; heavy-lift quadcopter)

| item | figure | source |
|---|---|---|
| payload | 68 kg (150 lb) | [20] medium (Malloy, BAE and Royal Navy pages agree) |
| speed | about 30 m/s (108 km/h); BAE says 60 mph (27 m/s); one source 97 km/h | [20] |
| range | 8 to 45 km by load and weather; about 14 km at full payload; about 70 km light | [20] |
| endurance | 36 min (up to 40) | [20] |
| smoke precedent | yes: TRV-150s carried an M75 smoke module into a breaching rehearsal and smoke machines to screen armour (Fort Hood, Fort Stewart, 2026 reports) | [21] low (search summaries of Janes, Defence-UA, DVIDS) |
| smoke as a target mark | no precedent found beyond Daily Wire's wording | [5] |
| armed trial | a three-shot APKWS launcher on a TRV-150 tested May 2026 (separate from the demo) | [22] low |

Pick: 30 m/s, 14 km loaded, 36 min, payload 68 kg; a smoke canister of about 5 kg [E].

## 5. Human authorisation of a salvo or swarm strike

### 5.1 Findings since 2026-11-10

| finding | source | label |
|---|---|---|
| **CCW GGE draft elements, rolling text of 18 Dec 2025, measure 7B**: human judgement and control is to be ensured by "limiting or otherwise restricting the scale of the operation, which includes the types of targets, duration, geographical scope and **the number of engagements** that LAWS can undertake autonomously". Also 7C: mission parameters must not be modified by the system without human intervention. This is the first text found that counts engagements. | [23] (read, text extracted from the PDF) | high |
| The GGE reached a draft report in July 2026 (CCW/GGE.1/2026/CRP.1); the ICRC's August 2026 working paper says the set of elements is a sufficient baseline and cites para 36 (human judgement and control, sub-measures (a) to (d)), para 32 (no use where effects "cannot be anticipated and limited") and para 39(c). The sub-measure text was not extracted from the paper; the 2026 paragraph numbers differ from the 2025 text's. It says nothing about swarms in what was read. The CCW Review Conference met 16 to 20 Nov 2026; its outcome was not found. | [24] (read in part) | high for what is quoted |
| **Defense One, 13 Oct 2026 (FASCOM)**: autonomous systems will help commanders decide, with "appropriate levels of human judgment" before weapons are fired; an Army official (Miller, as the fetch gave the name) wants "humans on the loop for decisions" and says that for superhuman speed and scale, counter-battery and air and missile defence, "if we are taking thousands of drones or hundreds of rounds, we can't put that on a human". | [25] (read through a summary) | medium |
| Army release on FASCOM, 1 and 2 Oct 2026, and the vice chief's piece (13 Oct): "Technology doesn't fight; Soldiers do". Nothing on salvo authorisation. | [26], [27] | high |
| AUTOWARCOM announced 30 Sep 2026 (Hegseth, a four-star command, stand-up by 1 Oct 2027). TWZ's account has no word on who approves strikes, swarms or operator ratios. | [28] | medium |
| 173rd operators' strike was hand-piloted; the swarm strike is called "autonomous" in the release; no one says who authorised it (part 1). | [1] | high |
| China's Swarm-2: the kill chain is one flow from detection to strike; "does not specify how much human authorization is required" | [14] | medium |
| DZYNE: Blitz runs operator-in-the-loop (needs a control link) and "currently no fully autonomous swarming"; drones do not talk to each other after launch; coordinated effects are planned before launch. | [13] | medium, vendor |
| Skynode/Auterion: humans assign targets and authorise engagement. | [18] | low, vendor |
| Congress: proposed safeguards for human oversight of AI weapons (WUNC, 5 Oct 2026). | [29] | low |

### 5.2 Doctrine on one approval, many effects

- **DoDD 3000.09 (2023)**: semi-autonomous systems engage "individual targets or specific target
  groups" an operator selected. CNAS treats a two-stage munition (a person selects a group of
  concentrated targets, each submunition then picks one) as semi-autonomous. Just Security
  (2024 or later) says it is "unclear how DoD interprets" the target-group language and that the
  classes lack nuance. So one selection of a group is inside the directive; a swarm of 18 drones
  against one marked target is arguably within it; what is not settled is a group that the
  system itself forms [30], [31] (medium; the directive's own PDF was not read this session, but
  SWARMS.md 12 read it in full).
- **Fire missions** (low: search summaries of FM 3-09.21 and FM 6-60 chapters; the passages were
  not read, the following is from memory): a call for fire has one observer decision that fixes
  the target (location, description), the method of engagement (area, precision, danger close)
  and the method of fire and control (at my command, when ready, time on target, fire for
  effect, rounds per tube); one clearance covers all the rounds of a mission, bounded by target
  area, a time window and a round count, and the observer can adjust or end it ("cease loading",
  "check firing", "end of mission"). An MLRS battery fires a ripple of rockets from one mission.
  This is the closest existing analogy: one human decision, many effects, bounded by place, time
  and count. [32] (low).
- **CCW and ICRC**: the bounds a decision needs, as the draft elements list them: target types,
  duration, geographical scope and the number of engagements [23]; the ICRC (2021) asks for limits
  on duration, geographic scope and scale "in relation to a specific attack" [33].

### 5.3 What this means for the design question

The strongest finding is [23] plus [25]: the draft international text names **the number of
engagements** as one of four limits that make human judgement meaningful, and an Army official
says plainly that at "hundreds of rounds" a human cannot be asked per round, so the Army expects
humans "on the loop". Taken together, the public position is that a salvo is **one human
decision, bounded**, not many; the bounds that make it meaningful (what the draft and the fires
doctrine both give) are: a designated target or target group named by the person, a place, a
time window, a ceiling on munitions or engagements, and a way to end it ("cease"). Murmur's
variable (SWARMS.md 12: how many decisions one swarm engagement needs) can be set from one
decision with all four bounds to many decisions; no source says one decision without those
bounds is enough. Nothing found says the 12 Nov strike met any of them, or what they were.

## 6. A Grafenwoehr world

Centre [E]: about 49.70 N, 11.93 E (Grafenwoehr Training Area, Upper Palatinate, Bavaria). Box:
about 10 x 10 km around the impact areas (the exact impact-area outline is not taken from a
public source here).

| layer | source | licence | size for a 10 x 10 km box | note |
|---|---|---|---|---|
| terrain, 30 m | Copernicus GLO-30 DEM (COG tiles, 1 degree; N49 E011 and E012) | free under the Copernicus DEM licence for general public use; attribution; some country tiles withheld, not Germany | the box is about 330 x 500 cells, under 1 MB; the 1-degree tile it sits in is about 25 to 30 MB [E] | EGM2008 heights, a surface model (trees, roofs) | [34] medium |
| terrain, 1 m | Bayern's open DGM1 (Bayerische Vermessungsverwaltung, Open Data) | CC BY 4.0 [35] | 100 million cells; about 100 one-km GeoTIFF tiles at 2 to 4 MB each, 200 to 400 MB [E] | a bare-earth model, DHHN2016 heights; direct ZIP for small areas, Metalink for large; tile size in km not confirmed |
| terrain, other | SRTM 1 arc-second (about 30 m, a US product) | public domain | a few hundred KB for the box [E] | older, noisier than GLO-30; no need |
| land cover | ESA WorldCover 10 m (2020 and 2021) [E, from memory, not searched]; Bavaria's ATKIS and CORINE for polygons | CC BY 4.0 (WorldCover) [E] | under 5 MB for the box [E] | the training area is mostly open grass and woods, with built-up areas and roads; a "surface" layer for the domain rule, trees as obstacles for sight lines would need DSM minus DGM |
| surface weather, archived | DWD Open Data (CDC observations; hourly wind, visibility, temperature at stations) | free, no registration [36]; the licence text was not read (the GeoNutzV, as I recall, [E]) | kilobytes a station-day | no DWD station found in Grafenwoehr itself; nearest found is Weiden in der Oberpfalz (id 5397 per a third-party site, about 13 km, low) [37]; the Army's own Grafenwoehr field station (WMO 10687) is not DWD's |
| gridded weather | Open-Meteo archive APIs (ERA5 and ERA5-Land via the "archive" API; high-resolution archived forecast models via the "historical-forecast" API) | free noncommercial use with attribution (as tidewater already notes) [38] | a 5 x 4 point grid for a few days is under 1 MB | Murmur's tidewater already calls Open-Meteo; ICON-D2 (about 2 km, Europe) as the model name is from memory [E]; ERA5 itself is 0.25 degree, too coarse for a 10 km box |
| ERA5 direct | Copernicus Climate Data Store | Copernicus licence, free, account needed [E] | tens of MB for a month at hourly | only if Open-Meteo is not enough |

### What tidewater would need

Read from tidewater/README.md and tidewater/tidewater/{fetch,weather}.py (not run).

- **Terrain grid**: `fetch.dem_grid` asks NCEI DEMs (NAVD 88) with ETOPO as fallback; it has no
  German reader. A new source (GLO-30 or DGM1 COG or GeoTIFF read, resampled to the scenario's
  `resolution_m`, a vertical datum note instead of `etopo_to_navd88_m`) is the one real piece of
  work. Land only: no bathymetry, no tide, no current (no navigable water at Grafenwoehr; the
  domain rule (rules 17) would treat it as all land), so the tide and currents layers must be
  optional or off. The terrain file's `placement` and `source` keys also need a German value.
- **Wind, visibility, temperature**: weather.py already reads Open-Meteo with a `model` and
  `source` from the scenario and keeps visibility, gust, precipitation, cloud, temperature,
  humidity and pressure. Only the model name and the point grid change (an ICON-D2 or ERA5 run
  instead of `gfs_hrrr`). Whether Open-Meteo serves visibility for ICON-D2 was not checked.
- **Other**: a links file with German radio sites; a ground height for the UGV stand-ins read from
  the DGM1 as docs say (CLAUDE.md, "A stand-in UGV's altitude"); a contacts file for the
  target area; SITL at a Bavarian home point. ArduPilot's own terrain files are not involved.

Cost, in size: under 1 MB (GLO-30 box) to about 400 MB (DGM1 box) of terrain, under 5 MB of land
cover, under 2 MB of weather; minutes of fetch. In work: one new terrain reader and a
land-only scenario path, a few days [E]; no change to the decision layers.

## References

1. https://www.army.mil/article/296792/us_army_evaluates_unmanned_capabilities_during_live_fire_demonstration_in_germany ; "US Army evaluates unmanned capabilities during live-fire demonstration in Germany", Capt. Angelo Mejia, 17 Nov 2026. **high** (read).
2. https://www.stripes.com/branches/army/2026-11-13/containers-drones-munitions-test-germany-23453535.html ; Stars and Stripes, 13 Nov 2026. **high** (read through a summarising fetch; quotes are the fetch's).
3. https://defence-industry.eu/u-s-army-launches-drones-from-an-unmanned-ground-vehicle-in-live-fire-test-in-germany/ ; Defence Industry Europe, Nov 2026. **medium**.
4. https://interestingengineering.com/military/us-army-renegade-ugv-drone-swarm ; Interesting Engineering, Nov 2026. **medium**.
5. https://www.dailywire.com/news/armys-new-war-robot-rolls-out-of-a-shipping-container-with-a-deadly-surprise ; Daily Wire, Nov 2026. **medium** (the only source with the smoke as a mark).
6. https://www.globaldefensecorp.com/2026/11/18/the-u-s-armys-173rd-airborne-brigade-demonstrates-a-drone-swarm-launched-from-a-usv-in-germany/ ; 18 Nov 2026 (headline says USV). **medium**.
7. Programme remarks: same as [1] and [2] (Tomosieski).
8. https://interestingengineering.com/videos/us-armys-r3negade-robot-vehicle-launches-18-drone-swarm-javelin-in-germany-live-fire-test and a search summary on missing Army performance data. **low**.
9. https://armyrecognition.com/news/army-news/2026/uks-iss-aerospace-reveals-hal10-launcher-for-salvo-deployment-of-10-wasp-modular-drones-to-support-land-ops ; 6 Apr 2026. **medium**, vendor figures.
10. https://dronelife.com/2026/10/06/auterion-introduces-universal-nemyx-vertical-launch-system/ ; 6 Oct 2026. **medium**, vendor.
11. https://www.janes.com/defence-intelligence-insights/defence-news/air/auterion-nemyx-vls-uses-same-technology-as-interceptor-rifle ; Janes. **low** (search summary).
12. https://defence-industry.eu/spearuav-unveils-revolutionary-launcher-for-viper-300-drones/ ; 4 Sep 2023. **medium**, vendor.
13. https://www.twz.com/news-features/blitzbox-packs-100-weaponized-drones-into-an-unassuming-container ; TWZ, 19 May 2026. **medium**, vendor.
14. https://www.defaiya.com/news/Products/Products/2026/03/29/china-unveils-96-unit-autonomous-kill-chain-drone-swarm-with-3-second-launch-speed ; 29 Mar 2026. **medium**, state media via a trade outlet.
15. https://www.twz.com/37062/china-conducts-test-of-massive-suicide-drone-swarm-launched-from-a-box-on-a-truck ; TWZ, 2020. **low** (search summary).
16. https://www.kyivpost.com/post/60152 ; Kyiv Post, 15 Sep 2025. **high** (read), vendor claims.
17. https://united24media.com/latest-news/ukrainian-ai-drone-maker-reveals-game-changing-tfl-1-autonomous-drone-already-hitting-russian-targets-9874 and https://militarnyi.com/en/news/dwarf-engineering-presents-narsil-drone-homing-module/ ; search summary of hit-rate reports. **low**.
18. https://www.theregister.com/2025/07/29/auterion_drone_ai_ceo_interview/ and https://breakingdefense.com/2024/06/skynode-s-auterion-autonomy-kit-lets-attack-drones-fly-through-jamming/ ; search summary. **low**, vendor. Date of the 50,000-drone deliveries unclear (July 2025 or July 2026 in results).
19. https://milremrobotics.com/product/themis-transport ; Milrem THeMIS. **medium** (search summary of the maker's page).
20. https://www.baesystems.com/en-uk/article/royal-navy-declares-t150-ready-for-operations and https://malloyaeronautics.com/t150 (page moved; not read) ; search summary. **medium**.
21. https://www.janes.com/defence-intelligence-insights/defence-news/security/us-army-fields-new-breaching-obscuration-tech ; https://en.defence-ua.com/events/borrowing_from_ukraine_us_army_trains_to_shield_advancing_armor_with_drone_deployed_smoke-17572.html ; search summaries. **low**.
22. https://www.army.mil/article/292791 and https://www.militarytimes.com/news/your-military/2026/05/28/army-turns-resupply-drone-into-rocket-launcher-in-new-test/ ; May 2026. **low**.
23. https://italiarappdisarmo.esteri.it/wp-content/uploads/2026/03/CCW-GGE-LAWS-Rolling-Text-status-18-December-2025.pdf ; CCW GGE LAWS rolling text, 18 Dec 2025, measure 7B. **high** (read).
24. https://www.icrc.org/sites/default/files/2026-08/ICRC_working_paper_GGE_LAWS_August-2026-ICRC.pdf ; ICRC working paper, Aug 2026. **high** for the introduction and section 2 (read), the rest not read.
25. https://www.defenseone.com/technology/2026/10/army-converts-futures-and-concepts-command-futures-and-autonomy-command/416576/ ; Defense One, 13 Oct 2026. **medium** (read through a summarising fetch).
26. https://www.army.mil/article/295913/army_announces_futures_and_autonomous_systems_command ; Oct 2026. **medium** (search summary).
27. https://www.army.mil/article/296083 ; "Future Focused: Vice Chief of Staff of the Army Details Army Autonomy Advancements", 13 Oct 2026. **high** (read through a summarising fetch).
28. https://www.twz.com/news-features/vision-for-new-autonomous-warfare-command-laid-out-by-pentagon ; 30 Sep 2026. **high** (read through a summarising fetch).
29. https://www.wunc.org/military/2026-10-05/drones-ai-warfare-congress-considers-safeguards ; 5 Oct 2026. **low**.
30. https://www.cnas.org/publications/reports/proposed-dod-principles-for-the-combat-employment-of-weapon-systems-with-autonomous-functionalities ; CNAS, 28 Apr 2021. **medium** (search summary).
31. https://www.justsecurity.org/143209/the-pentagons-autonomous-weapons-definitions-dont-need-an-overhaul-they-need-an-update/ ; Just Security. **medium** (search summary). Also https://agora.eto.tech/instrument/50 (the directive text, read for SWARMS.md 12).
32. https://www.globalsecurity.org/military/library/policy/army/fm/3-09-21/ch5.htm and https://www.globalsecurity.org/military/library/policy/army/fm/6-60/Ch5.htm ; fire mission chapters. **low** (search summaries; the doctrine summary above is from memory).
33. https://www.icrc.org/en/document/icrc-position-autonomous-weapon-systems ; ICRC, 2021 (read for SWARMS.md 12). **high**.
34. https://documentation.dataspace.copernicus.eu/APIs/SentinelHub/Data/DEM/resources/license/License-COPDEM-30.pdf and https://copernicus-dem-30m.s3.amazonaws.com/readme.html ; Copernicus GLO-30. **medium** (search summary).
35. https://saga-gis.sourceforge.io/saga_tool_doc/9.13.0/io_webservices_6.html and https://geodaten.bayern.de/odd/m/3/pdf/geotiff-kachel_punktreduzierung.pdf ; Bavarian DGM1 (CC BY 4.0, 1 m). **medium** (search summary).
36. https://opendata.dwd.de/README.txt ; DWD Open Data. **medium** (search summary).
37. https://www.meteopool.org/en/wetterstation-weiden-in-der-oberpfalz-deutscher-wetterdienst-dwd-5397-wmo-10688-id246 ; third-party station page. **low**.
38. tidewater/tidewater/weather.py and docs/WEATHER.md ; Open-Meteo terms as Murmur records them. **high** (read locally).
