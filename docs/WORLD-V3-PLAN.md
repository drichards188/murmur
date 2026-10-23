# World v3: realistic sensors and evaluation across Murmur's breadth

Status: **approved by the owner 2026-10-23, decisions W1-W5 as recommended** (bd epic murmur-1z6). Nothing here is
built yet. Inputs: the sensing code map, the capability inventory and the sensor research
(`docs/research/sensors.md`, public sources, [n] cited and [E] estimated).

## 0. Why

The owner's direction (2026-10-23): build realistic sensors, give every node its accurate
sensors, and evaluate in situations and worlds that cover most of Murmur's real breadth.

Scoring v2 ran about 70,000 simulated episodes in one world: the Golden Gate, 4 vehicles (REAPER
1, HUNTER 1, PUMA 1 and 2), surface contacts only, and only the two Pumas able to identify,
because REAPER and HUNTER were stripped of their cameras and sonar (D8). That shortcut existed
because the sensing model has no field of view, no pointing and no range-dependent
identification, so REAPER's camera identified everything from anywhere. 18 of the 22 platform
types, air, subsurface and ground contacts, satellite tasks, the engagement chain with signers,
GNSS faults, acoustic and Iridium links, fleets above 4 vehicles and every map but the Golden
Gate have never been evaluated. v2's results stay descriptive of that world only; its validation
(murmur-fpv) is paused.

**Rule for every report from here on**: it opens with what the tested world covers (vehicles,
sensors, contact domains, maps, tasks) and names any simplifying shortcut.

## 1. Coverage target

"Most of the breadth" means every row below is exercised by at least one situation family, on
at least two maps where the row is not map-specific, with at least two vehicle types able to
answer each need so no family hangs on one vehicle or one quirk of one map.

| Area | Today | v3 target |
|---|---|---|
| Platform types in headless evaluation | 3 of 22 | at least 18 of 22 (all but those needing SITL-only behaviour) |
| Sensors | range curve, no FOV, pooled identification | per-sensor physics: EO/IR with FOV, pointing, D/R/I by range, atmosphere, day and night; radar horizon, RCS, sea clutter, scan; AIS by class and horizon; sonar equation; ESM by emitter; terrain and earth-curvature masking |
| Contact domains | surface | surface, air, subsurface, ground, divers |
| Maps | Golden Gate | Golden Gate and San Diego (terrain, bathymetry, tide, wind from tidewater) |
| Fleet size headless | 4 | 20 to 30 mixed per world; one 100-node scale world |
| Tasks | orbit, patrol, hold, rtl | plus track, collect and cover (space), the engagement chain (nomination, two signers, keep-outs, assessment) |
| Links | los, GEO, Starlink; cut only | plus mesh, Iridium, acoustic (AUV surfacing), tether; loss, latency, jammer circles |
| Faults | stop, link cut, low battery at start | plus GNSS jam and spoof, battery drain faults, reboot, currents |
| Operator work | approve cards | plus review: declarations, flags, lost contacts, AIS claims, engagement signatures |
| Conditions | fixed | day and night, visibility, sea state, wind (drawn per situation) |

## 2. The sensor model (built in the shared tracks code)

The live bridge, the headless simulator and the keep-out probe compile the same
`crates/murmur-bridge/src/tracks/{sensing,model,truth}.rs`, so one model serves all three.

### 2.1 Contacts carry signatures

Each contact kind (`murmur-contacts` `KINDS`, and the truth structs in both crates) gains: a
critical dimension D_c (m) and height, X-band RCS (m²), thermal contrast ΔT (K), underwater
source level (dB re 1 µPa at 1 m) and target strength, emitters (radar or datalink: power,
band), AIS class (A, B or none). New kinds: diver, periscope (a submarine at periscope depth),
small drone types. Values from the research's target table; a scenario may override per
contact. The old single `signature` multiplier is retired.

### 2.2 Geometry for every sensor

- Optical horizon (k = 7/6) for EO/IR, radio horizon (k = 4/3) for radar, AIS and ESM, with
  hull-down: only the target's visible height counts.
- Terrain line of sight from the map's terrain grid (the Golden Gate's 20 m grid exists; each
  new map bakes one), with the linkemu's earth-bulge correction, cached per pair and refreshed
  when either end moves 50 m or every 5 s.
- Sonar ignores the air horizon and uses water depth from the map's bathymetry.

### 2.3 EO/IR

- Each EO/IR sensor is one or more channels (day, low-light, MWIR or LWIR) with wide and narrow
  fields of view, pixel count, a blur floor and a slew rate (the research's platform table).
- Pixels on target p = D_c / (R × IFOV). The 50 % thresholds are 1.5 px to detect, 6 to
  recognise and 12 to identify. The probability is the ACQUIRE curve P = x^E / (1 + x^E), with
  x = p / p50 and E = 2.7 + 0.7x, times an atmosphere factor from visibility (day) or thermal
  contrast (IR).
- Time to detect within the field of view is exponential, and identification needs the narrow
  field of view on the target for 2 to 3 s.
- Day cameras work by day only. Thermal works day and night, but with a dawn and dusk crossover
  penalty. The sun's position comes from the scenario clock and place.
- **What a look yields**: a detection gives the contact's domain and size class. A recognition
  gives its family (small craft, ship, aircraft, vehicle, person). An identification gives its
  kind. Misclassification is likelier at the recognition level than at the identification
  level, so a close look improves a wrong call (murmur-jln).

### 2.4 Pointing (the payload manager)

A camera sees only inside its current field of view. Each EO/IR sensor is run by a simulated
payload manager on the vehicle. It searches in the wide field of view, sweeping the vehicle's
task area. It cues to radar or AIS tracks there: the nearest unidentified track first, then
the stalest. It switches to the narrow field of view to identify, and returns to search. A
vehicle with no task searches around itself. This needs no change to intents, so no
decision-rules version. **Decision W1** (below) is whether a later phase lets an order name a
look target.

### 2.5 Radar

- R50 = R_ref × (σ / σ_ref)^¼, calibrated per radar so that the IMO MSC.192(79) Table 2 anchors
  reproduce for a navigation radar.
- Cut at the radio horizon with hull-down. Low targets use the two-ray range beyond the
  crossover.
- Sea clutter by sea state from the σ0 table: a small boat needs 10 dB over clutter, so it is
  lost at sea state 5.
- Detection is drawn once per scan, not every second. Navigation radars scan every 1.25 or
  2.5 s.
- Accuracy: 30 m or 1 % of range, and 1° in bearing. Height comes only from a radar with
  `height = true`, as today.
- **Decision W2**: every radar revisits in at most 5 s. This keeps the 10 s out-of-view rule
  (DECISIONS.md 312-326) sound.

### 2.6 AIS, sonar, ESM

- **AIS**: only contacts with a transponder that is on report. Each class has its own reporting
  interval. Range is the VHF horizon between the two antennas, and decoding is reduced by slot
  load. Spoofing is as today.
- **Sonar**: the sonar equation, with signal excess = FOM − TL − ΔNL and Pd = Φ(SE / 8 dB).
  - Transmission loss is spherical to the water depth, then cylindrical, plus Thorp absorption.
  - Ambient noise comes from the wind (tidewater) and shipping.
  - Each sensor and target class has a FOM calibrated from the research's reference ranges in a
    reference environment.
  - Passive sonar classifies over minutes; active sonar localises.
  - The research's corrections are applied: REMUS side-scan 150-300 m a side, BlueROV Ping360
    50 m, buoys and the Orca per target class.
- **ESM**: detects only emitting contacts in radio line of sight whose received power exceeds
  the threshold. It reports a bearing with 1-5° error; for now this becomes a position along
  the bearing with the error the bearing implies, as today.

### 2.7 Identification and the tracker

- Identification evidence is kept per observer and per contact, not pooled per sensor type.
  Independent looks combine.
- **Phase 1 keeps `TrackFacts`, the sensor type names and the identity interface unchanged**,
  so the decision engine, replay and `before_oho` are untouched and no decision-rules version
  is needed. Only what feeds them changes.
- The tracker must still pass `tracks::tests::calib` and `calib_air` on their held-out seeds.
  If it needs retuning, it is tuned on the tuning sets and judged on new held-out sets written
  before tuning (CLAUDE.md).

### 2.8 Acceptance for the sensor model (before any world uses it)

1. **Anchor tests reproduce the published figures**, within the stated tolerances:
   - the IMO Table 2 radar ranges (within 15 %);
   - the FLIR M364C and EDGE G270 detection ranges (within 40 %, the research's own spread);
   - the MX-20 and Mantis i45 D/R/I geometry;
   - AIS horizon ranges;
   - the Sentinel diver-detection ranges for active sonar.
2. **Property tests**:
   - nothing is seen beyond its horizon or behind terrain;
   - a camera sees nothing outside its field of view;
   - identification probability falls with range;
   - night removes day-camera detections;
   - sea state 5 loses a small boat to a navigation radar at 5 km and keeps a ferry.
3. **The tracker calibrations pass** (or are retuned as 2.7 says). The keep-out eval re-runs,
   with METRICS.md updated in a dated note.
4. **The cost holds:** a headless episode with 30 vehicles and 40 contacts runs at least 20× real
   time on this host.
5. **The Python ports are replaced** (labels, families, agents_v2's oracle, the playbook's
   beliefs): Python asks the simulator's sensing through the headless API instead of keeping
   its own copy.

## 3. Platforms get their real sensors

`sim/platforms.toml` gets each type's sensors from the research's platform table, each value
marked cited or estimated in a comment, with the source. Corrections:
- **Corsair:** passive EO/IR and ESM, no radar.
- **Magura:** EO only.
- **Saildrone Explorer:** cameras only.
- **REMUS, BlueROV, buoy, Orca:** sonar as in 2.6.
- **MQ-9B and Sea Hunter:** keep their full kit.

The radar-only variants `mq9b_radar` and `seahunter_radar` stay in the catalog only so v2 records
can be re-read.

## 4. Headless gaps to close

From the capability inventory, each one a work item:

| Item | What |
|---|---|
| H1 land and water | A baked land/water grid per map in the bridge's domain rule (`decision/terrain.rs`), so boat and UGV routes and their engagements are checked headless |
| H2 engagement chain | The headless operator signs engagements (two signers, `EngagementSigners` logic) and does review work: declarations, flags, lost contacts, AIS claims |
| H3 space | Collect and cover on the simulated clock, with pinned element sets per world |
| H4 faults and timelines | GNSS jam and spoof, battery drain faults, reboot, currents for contacts and drifters; situation `[[step]]` timelines |
| H5 maps | Golden Gate's hard-wired reference points, terrain grid, areas file, ground station and the mission's 3 km circle become per-world data |
| H6 scale | 30-vehicle worlds in routine runs, a 100-node world; performance budget as in 2.8 |
| H7 needs and labels | Needs and labels beyond looks: subsurface and air identification, ground, track, collect, cover, engagement decisions, review decisions, reserve across a fleet |

## 5. Worlds

- **Golden Gate v3**: about 25 nodes.
  - Air: REAPER (MQ-9B), RQ-4 on a far orbit, 2 Pumas, 2 X10, FVR-90, the airship.
  - Surface: Sea Hunter, Corsair, Saildrone, Magura (armed).
  - Subsurface: REMUS, BlueROV at a pier, 3 buoys in the channel.
  - Ground: a recon UGV and a logistics UGV on the shore.
  - The antenna tracker, and the space segment.
- **San Diego**: from `sim/fleets/sandiego.toml`, with a tidewater scenario fetched (NOAA DEM
  with bathymetry, CO-OPS tide, open-meteo wind), a terrain grid, areas, ground stations and
  contact traffic (naval base, bay, approaches).
- **Scale world**: a 100-node fleet on one map, for load and allocation under many simultaneous
  needs.

Conditions are drawn per situation: time of day, visibility, wind and sea state, within each
place's recorded ranges.

## 6. Situation families

Each family is generated with continuous draws, as v2's were. Each runs on both maps unless
marked, and each need is answerable by at least two vehicle types.

| # | Family | Tests |
|---|---|---|
| F1 | Fast boat, no AIS | surface identification; pointing REAPER's camera vs sending a Puma or X10 |
| F2 | Unknown drone inbound | air picture: radar cue, short identification ranges, X10 or Puma intercept look |
| F3 | Low aircraft, transponder off | air identification at altitude; height-capable radar |
| F4 | UUV in the channel | buoys detect passively, Sea Hunter localises, REMUS or BlueROV confirm |
| F5 | Diver at a pier | short-range sonar, BlueROV, night |
| F6 | Vehicle at a restricted shore | ground domain: UGV, X10, airship; terrain masking |
| F7 | Three at once (air, surface, subsurface) | allocation across domains |
| F8 | Decoys and AIS spoofing | restraint, in every domain |
| F9 | Night and fog | which sensors still work; thermal crossover |
| F10 | Rising sea state | radar loses small boats; cameras matter |
| F11 | Link trouble | jammer circle, Iridium latency, AUV must surface to report |
| F12 | GNSS jam or spoof | navigation integrity and the C2's handling |
| F13 | Vehicle faults | breakdowns, battery drain and reboots spread across domains |
| F14 | Space | satellite collect to fill a gap, SATCOM or GNSS cover |
| F15 | Engagement chain | hostile act, declaration, nomination, two signers, 2D and 3D keep-outs, assessment, lost contacts |
| F16 | Operator review load | declarations, flags, lost contacts, AIS claims, approval floods together |
| F17 | Scale | 100 nodes, many needs (scale world only) |
| F18 | Quiet | nothing happens; the cost of acting |

## 7. Scoring and validation

Reuse scoring v2's machinery: outcomes, relative comparison, anchors, the ladder of
handicapped oracles, perturbations and the blind construct check. It is rebuilt for v3: the
oracle knows the sensor model (which vehicle can identify what at what range, and where to
point), and labels cover the new action kinds. Lessons written in before any run:

1. **Coverage first.** Every result opens with the world's coverage.
2. **Audit dev before pre-registering.** At least two dev batches. Every judged cell whose
   point estimate leans the wrong way in both is examined and settled before the rule is
   written.
3. **No family hangs on one quirk.** Dev reports, per family, the share of needs lost to a
   single cause (one link shadow, one vehicle). Any family above 50 % is redesigned before
   validation.
4. **Multiplicity control.** The per-family guard is judged with Holm's correction across its
   cells, decided before any run.
5. **One validation, then the paid blind check**, on v3, after its validation passes. The
   blind check's budget is the owner's to set then. Nothing in sections 2-6 spends API money.

## 8. Build order

Background agents in worktrees, one build directory each, nothing pushed by an agent; the
coordinator reviews, merges and pushes.

| Phase | Work | Depends on |
|---|---|---|
| A1 | Sensor model core (2.1-2.7) with its acceptance (2.8) | none |
| A2 | H1 land/water, H5 maps, H4 faults and timelines | none (runs beside A1) |
| A3 | H2 engagement chain and review headless, H3 space | none (runs beside A1) |
| B1 | Platform sensors (3) | A1's sensor schema |
| B2 | Golden Gate v3 and San Diego worlds (5), H6 scale | A2 |
| C1 | Families F1-F18 (6), H7 needs and labels | A1, B1, B2, A3 |
| C2 | Oracle, ladder and perturbations for v3 | C1 |
| D | Dev batches, the audit, pre-registration, validation, then the blind check | C2 |

## 9. Decisions for the owner

- **W1 Pointing.** Phase 1: the payload manager points cameras automatically (2.4), with no
  change to orders. Later, an optional look target in orbit and patrol orders (a decision-rules
  version). Recommended.
- **W2 Radar revisit.** Every radar revisits in at most 5 s, which keeps the 10 s out-of-view
  rule sound. A slower radar would mean reopening that decision. Recommended.
- **W3 Estimated values.** Where a sensor's figures are not published, use the research's
  estimate and mark it [E]. Values are never tuned to make a family work. Recommended.
- **W4 Second map.** San Diego (its fleet exists; terrain, tide and wind are fetchable).
  Recommended.
- **W5 v2.** Retired for new work; its records and results stay as they are, descriptive of the
  4-vehicle world.
