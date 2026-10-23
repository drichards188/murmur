# Sensing: the legacy and the physical model

How Murmur turns the truth about contacts into what our sensors report (WORLD-V3-PLAN section 2,
bd murmur-1z6.1). Two models share one tracker, one decision engine and one replay:

- **legacy** (`crates/murmur-bridge/src/tracks/sensing.rs`): a range curve per sensor type
  around the catalog's `range_m`, a radio horizon, classification confidence growing with dwell
  pooled per sensor type. Every record before world v3 is this model's.
- **physical** (`crates/murmur-bridge/src/tracks/physical/`): per-sensor physics, described
  below. Parameters come from `docs/research/sensors.md` ([n] cited there, [E] estimated there);
  what the research does not give is marked `[E here]` in the code with its basis.

Both produce the same `Detection`s for the same tracker (`tracks/model.rs`): `TrackFacts`, the
sensor type names and the identity interface are unchanged, so no decision-rules version was
needed. The physical model adds one thing to a detection, its identification evidence
(`look`: a level and a confidence), which the tracker keeps per observer (below).

## Choosing the model

| Where | What |
|---|---|
| contacts scenario `[sensing] model = "physical"` | the scenario's model, carried in every truth message |
| `MURMUR_SENSE_MODEL=legacy\|physical` on the bridge | overrides the scenario's (as `MURMUR_SENSE_SEED` does) |
| neither | `physical`, the default since the platform catalog carries every sensor's figures (WORLD-V3-PLAN B1) |

The live stack (`make up`, `make goldengate`, `make situation`) and any new world run on the
physical model unless they name the legacy one. The v1 and v2 worlds are pinned to `legacy` by
the evaluation layer, not in their contacts files (the v1 generator's digests hold those byte for
byte): `MURMUR_SENSE_MODEL=legacy` in every headless episode of a v1 or v2 world
(`eval/headless/situations.py` `process_spec`), in a G1 full-stack copy
(`eval/headless/fullstack.py`) and in the Phase G evaluation's stacks (`eval/stack.py`). The
change of the default left their records as they were. A legacy scenario's truth messages carry
nothing new (every new field is left out when unset): its episodes are byte-identical.

The bridge says which model it runs on at the first truth message and on a change (`sensing: the
physical model (the default)`, or `(the scenario)`, `(MURMUR_SENSE_MODEL)`), lists any catalog
sensors running on placeholders (none in `sim/platforms.toml`), and says once if there is no
terrain grid.

## What a scenario gives it

```toml
[sensing]
model = "physical"
terrain = "sim/headless/terrain/golden-gate.grid.gz"   # line of sight; none: earth curvature only
bathymetry = "..."                                     # optional: depth grid (heights below 0)

[conditions]               # each optional; defaults: a clear day, sea state 2, the sonar reference
visibility_m = 20000       # Koschmieder visibility
sea_state = 2              # sea clutter
wind_mps = 5               # sonar ambient noise
shipping = 0.5             # sonar ambient noise, 0 to 1
water_depth_m = 50         # sonar spreading where no bathymetry
sun_elevation_deg = 40     # none: the sun from the scenario clock and the place

[[contact]]                # any contact may override its kind's signature
rcs_m2 = 0.8               # also dc_m, height_m, thermal_k, source_db, target_db,
ais_class = "none"         # ais_class (a, b, none), emitter_dbm
```

Grid paths are relative to the repository. The grid format is `sim/headless/terrain/*.grid.gz`
(`eval/headless/bake_terrain.py`). The sun is computed from the truth message's clock (the
scenario clock, `MURMUR_AT` plus elapsed time) with the Astronomical Almanac's low-precision
formula, refraction ignored.

## What the catalog gives it

`sim/platforms.toml`'s sensor entries keep `type` and `range_m` (the decision engine reads
`range_m`; the legacy model too) and may add the physical fields (`physical/spec.rs`):

| Sensor | Fields |
|---|---|
| `eo_ir` | `slew_dps`; `channels = [{ band = "day" \| "lowlight" \| "mwir" \| "lwir", wide_deg, narrow_deg, px, blur_urad, panoramic }]` |
| `radar` | `r50_m` and `rcs_ref_m2` (Pd 0.5 per scan), `scan_s` (at most 5 s), `beam_deg`, `cell_m`, `band_ghz` |
| `sonar` | `mode = "passive" \| "active"`, `freq_khz`, `ranges = { <kind> = m }`, `max_m`, `scan_s` |
| `esm` | `s_min_dbm`, `bearing_deg` |

A sensor without them runs on **placeholders** derived from `range_m`: a camera whose wide field
shows a 5 m target at 1.5 px at `range_m` (a day channel of 1,280 px and an LWIR one of 640, the
narrow field an eighth of the wide, 60 deg/s); a radar seeing 100 m^2 half the time at
`range_m` (scan 2.5 s, 5 s airborne); a passive sonar hearing each kind at `range_m` times its
legacy signature multiplier; an ESM receiver of -80 dBm and 5 degrees. No sensor of
`sim/platforms.toml` runs on them (`tracks::tests::catalog::no_catalog_sensor_runs_on_placeholders`;
`every_catalog_sensor_field_is_one_the_schema_reads` catches a misspelt field, which serde would
pass over).

### The platforms' sensors (WORLD-V3-PLAN section 3, B1)

Every value in the catalog carries a comment with its source: [n] a reference of
`docs/research/sensors.md`, [E] the research's estimate with its basis, [E here] an estimate the
research does not make, with its basis. Values are the research's, never tuned to make a
situation work (decision W3). Ranges below are the research's, at Pd 0.5.

| Platform | Sensors under the physical model | Sources |
|---|---|---|
| RQ-4 | radar 200 km on 10^4 m^2, 5 s; EO 1,024 px and MWIR 640 px at 10 and 30 urad, no zoom; ESM -90 dBm, 1 deg | [31,32,29]; IFOVs [E]; the rest [E], [E here] |
| MQ-9B | Lynx radar 300 km on 10^4 m^2, 5 s; MX-20 MWIR 640 px 18.2 to 0.24 deg, spotter 1,920 px 1.07 to 0.2 deg, 57 deg/s; AIS | [34,3]; r50 scaling, blur floors [E] |
| CCA | AESA radar (3D) 100 km on 1 m^2, 5 s; MX-15-class MWIR 26.7 to 0.54 deg, spotter 1.1 to 0.43 deg; ESM -80 dBm, 2 deg | [E]; [35]; [30] |
| FVR-90 | MX-8: MWIR 640 px 28.4 to 2.75 deg, EO 1,280 px 30 to 1.53 deg | [36]; blur, slew [E] |
| Puma 3 AE | Mantis i45: EO 4,472 px 56 to 1.2 deg, LWIR 640 px 40 to 5.3 deg | [37,38]; blur, slew [E] |
| Switchblade 600 | EO 1,280 px 56 to 3 deg, LWIR 640 px 40 to 12 deg | [39]; narrow fields [E]; wide fields [E here] from [37,38] |
| Skydio X10 | visible 3,840 px 80 to 10.4 deg; Boson+ LWIR 640 px 32.4 deg | [40]; px [E here] |
| FPV | one fixed day camera, 150 deg, 720 px | [E] |
| Small airship | MX-10 class: MWIR 640 px 30 to 1.8 deg, EO and spotter 1,920 px 31 to 0.61 deg; AIS | [4] as [E] |
| Logistics and mine-laying UGVs | fixed drive cameras: day 90 deg 1,920 px, LWIR 50 deg 640 px | [E] |
| Armed UGV | Guardian 2.0: MWIR 640 px 17.6 to 2.2 deg, day 1,920 px 38 to 1.9 deg | [15]; px, blur [E here] |
| Recon UGV | a mast uncooled LWIR 640 px 12 to 4 deg | [E] like [15] |
| Antenna tracker | none | |
| Magura V5 | one fixed day camera, 20 deg, 1,920 px; AIS. **No radar** | [41]; camera [E] |
| Corsair | a fixed 360 deg ring, LWIR 640 px and day 1,920 px at 18 deg; ESM -80 dBm, 2 deg; AIS. **No radar** | [42,13,30]; day camera, ESM [E] |
| Sea Hunter | navigation radar on the IMO anchors, 2.5 s; a ring as the Corsair's; AIS; active hull sonar, a submarine at 7.5 km, a UUV at 1.5, to 20 km | [19,18,43,44,25]; [E] |
| Saildrone Explorer | a fixed 360 deg ring of 60 deg day cameras, 1,920 px; AIS. **No radar** | [45,46,47]; cameras [E] |
| Buoy | passive, 1 kHz: a cargo ship 20 km, a fast boat 3.5, a submarine 1, a UUV 0.3, a diver 0.1 | [48]; ranges [E] |
| BlueROV2 | Ping360, active 750 kHz, to 50 m, 20 s a turn | [49] |
| REMUS 600 | side-scan as an active sonar, 455 kHz, to 300 m, a UUV at 225 m | [50]; [E] |
| Orca XLUUV | passive, 1 kHz: a cargo ship 20 km, a submarine 1.5; the quieter classes the buoy's | [E]; [E here] |

`range_m` stays what the decision engine reads (an assessment's coverage) and the legacy model's
reach; it was not changed here. Against the physical model's typical range (`cargo test -p
murmur-bridge catalog_table -- --ignored --nocapture`: a camera's detection of a 5 m target, a
radar's of 100 m^2, ESM a patrol boat's radar, AIS a ferry, each sonar per kind; clear day, sea
state 2, no terrain), these differ by more than 2x, for a later decision: the RQ-4's radar (200
km against 61) and cameras (50 against 198 out since the slant path, nothing before it), the
CCA's radar (150 against 316), cameras (30 against 126; 10 before the slant path) and ESM (200
against 443), the MQ-9B's cameras (40 against 88; 13 before) and AIS (150
against 373), Sea Hunter's radar (40 against 11) and sonar on a submarine (3.0 against 7.5), the
Puma's, Switchblade's, X10's and UGVs' cameras (0.8 to 4 km against 4 to 14), the Magura's camera
(5 against 10.1), and every sonar on the kinds it was not built for (the BlueROV and REMUS against
surface ships, the buoy and the Orca against a cargo ship or a slow fast boat). The removed radars
(Corsair, Magura, Saildrone) changed none of the 94 engagement scenarios' outcomes (under the
default, `--rules 6`, seeded noise and the review proxy) nor the keep-out evaluation (its fleets'
observers are Sea Hunters; the Magura engages).

## The model

**Signatures** (`physical/signature.rs`, research target table). Per kind: critical dimension
D_c, height, X-band RCS, thermal contrast, source level, target strength, emitter, AIS class.
New kinds: `diver`, `periscope` (a submarine at periscope depth: a mast above the water, a hull
below), `small_drone`, `person`. A small fast boat's source level follows its speed, 134 to
171 dB at 3 to 16 m/s [52]. The truth structs carry no signature field: the table is looked up
by kind, a scenario's overrides by contact id (`sensing.signatures`).

**Geometry** (`physical/geometry.rs`). Horizons at k = 7/6 (EO/IR) and 4/3 (radar, AIS, ESM)
[1]; hull-down: only the part of a target above the sensor's horizon counts (`h_vis`), for the
camera's critical dimension (sqrt of the visible height share) and the radar's target height.
Terrain line of sight from the grid with the link emulator's earth bulge `d1 d2 / (2 k R)`,
cached per observer and contact, recomputed when either end moves 50 m or after 5 s [E].

**EO/IR** (`physical/eo.rs`). Pixels on target `p = D_c / (R IFOV)`, `IFOV = max(FOV / px, blur
floor)`; 50 % thresholds 1.5, 6 and 12 px [5]; the ACQUIRE curve `P = x^E / (1 + x^E)`,
`E = 2.7 + 0.7 x` [7,8]; atmosphere `1 / (1 + (R / R_atm)^6)` (shape [E]) with
`R_atm = 0.69 V` in the visible [9] and `ln(dT / 0.1 K) / 0.2 km^-1` in the IR [10], at most 4.5
times the visible's in haze [11]. `R` is the line of sight's sea-level equivalent (the slant
path, below): a horizontal path along the sea is its own length. A day camera has no light below civil twilight, a low-light
one 0.4 of its range at night [E]; a thermal camera's contrast dips to a quarter with the sun 5
degrees up (crossover, shape [E]), which costs it the square root of that in pixels and its
atmospheric range. Time to detect in view: one draw per look whether the contact is
detectable at all (P) and one when (exponential, `tau = 3.4 / P` s [12]). Recognition after a
second of detection; identification needs the narrow field on the contact for 2 to 3 s [E].

**The slant path** (`eo::sea_level_m`, murmur-7vp). The visibility and the clear air's thermal
coefficient are the surface's; the air above holds less. Over the sea the aerosol is held in
the marine boundary layer, about 1 km deep by lidar (over the subtropical North Atlantic its top
is at about 1.0 km month by month, 2007-2018: Barreto et al., ACP 22, 739-763, 2022, supplement,
https://acp.copernicus.org/articles/22/739/2022/acp-22-739-2022-supplement.pdf; over the Indian
Ocean under 1 km, the aerosol above it thin and below 4 km: the INDOEX ship lidar,
https://ntrs.nasa.gov/api/citations/20010069577/downloads/20010069577.pdf). The model takes the
extinction as falling with height as `exp(-h / H)` [E here]: the aerosol's (the visible bands,
and the thermal bands' haze limit) with `H` = 1.2 km, the middle of the 1 to 1.5 km such
profiles take, and the clear air's thermal coefficient, mostly water vapour's, with 2 km, the
slower of the heights quoted for it (1.3 km fitted from lunar-eclipse photometry,
https://arxiv.org/abs/0706.0660), so a high camera is not given more than the air allows. A
straight line of sight `R` long between heights `h1 < h2` meets `R H (exp(-h1 / H) - exp(-h2
/ H)) / (h2 - h1)` of sea-level air (heights below the sea count as at it; the earth's
curvature, which lifts a long path a little, is ignored); a thermal band is held by the clear
air or the haze, whichever leaves it less. Along the sea it is the horizontal model value for
value (`a_horizontal_path_at_the_sea_is_the_model_before`; a mast's few metres change a range
by under 1 %). Before it the whole slant path was at the surface's visibility: the RQ-4's
cameras at 18 km never reached Pd 0.5 on a vehicle (0.17 straight down), and the MQ-9B's and
the CCA's stopped at about 15 km slant (13 and 10 km out). Now, by day at 20 km visibility (`cargo test -p
murmur-bridge slant_camera_table -- --ignored --nocapture`; slant ranges, the geometry and blur
alone beside them):

| Camera | Target | D / R / I, km | Geometry and blur alone, km |
|---|---|---|---|
| RQ-4 at 18,000 m | vehicle (NATO 2.3 m, 2 K) | 143 / 38.2 / 19.1 | 152 / 38.1 / 19.1 |
| RQ-4 | small fast boat | 181 / 58.1 / 29.0 | 232 / 58.0 / 29.0 |
| MQ-9B at 7,600 m | vehicle | 87.6 / 53.3 / 27.4 | 219 / 54.8 / 27.4 |
| MQ-9B | small fast boat | 88.0 / 71.0 / 41.5 | 333 / 83.3 / 41.7 |
| CCA at 11,000 m | vehicle | 113 / 38.4 / 19.2 | 153 / 38.3 / 19.2 |
| CCA | small fast boat | 124 / 58.1 / 29.2 | 233 / 58.3 / 29.2 |

(before: the RQ-4 nothing; the MQ-9B 15.0 / 13.9 / 13.6 and 17.0 / 16.4 / 13.8; the CCA 15.0 /
13.8 / 12.8 and 17.0 / 16.4 / 13.7). Recognition and identification are now the geometry's and
the blur's; only the long detection ranges, at a few degrees of elevation through tens of
kilometres of the boundary layer, are the atmosphere's
(`a_high_camera_looking_down_reaches_what_its_geometry_and_blur_allow`: from 18 km, an RQ-4-class
camera's D/R/I ranges against a vehicle are within 15 % of its geometry's, identification
within 2 %).

**Pointing** (`Engine::steer`, decision W1). Each EO/IR sensor's payload manager, simulated on
the vehicle: it searches its vehicle's task area in the wide field (an orbit's centre and six
points halfway out, a patrol's route every quarter of the reach, else around the vehicle at
half its reach), 5 s a step, 1 s to settle after any slew or zoom; after 10 s of search it cues
to a track there (a radar's, AIS's or a camera's own fix within 10 s; the nearest not
identified in 5 minutes, else the one looked at longest ago, beyond 2 minutes), slews its wide
field there, and once the wide field has the contact locks on in the narrow field (the image
tracker keeps it centred) until it is identified or 10 s pass, then searches again. A look in
progress survives the slew and zoom. A camera that cannot slew looks along its vehicle's
heading; a panoramic one does not point. The constants are [E].

A cue to an aircraft whose height no sensor measured (a 2D radar's plot, a camera's or ESM's
fix) is an elevation search (murmur-7vp): a 2D radar measures range and bearing, not elevation,
so the aircraft is somewhere on the arc its slant range allows along that bearing, from just
above the sea to as high as it may fly; the camera steps its wide field through those elevations
bar by bar (four fifths of the field's height a step), lowest first, staring 4 s at each once
settled, as its search steps the task area and as an EO director cued by a 2D surveillance radar
scans in elevation at the cued bearing. The band is 10 m to 8,000 m, or the slant range if lower
([E here]: above the 4 to 6 km service ceilings of the light aircraft and helicopters the air
kinds stand for, and the 7,000 m of the engagement scenarios' highest aircraft). A
height-finding radar's fix (within 10 s) aims at the aircraft itself. A cued camera re-aims
only when the cue leaves the middle of its field (a quarter of its width off the boresight); a
locked one follows its contact in closed loop while the line of sight turns no faster than the
gimbal slews: only a slew, a zoom, a search step and a new bar settle (the research's "slew
time = angle / slew rate + about 1 s to settle" is a step's; a stabilised gimbal under its
image tracker keeps the target centred and sharp, and holding a cue that stays in view needs no
step). Before this, a cue aimed at the plot on the sea, and every track update while cued and
every second of a lock slewed and settled again: on the air tuning sets the cameras were blind
93 to 98 % of the time they were cued or locked, and 3.5 to 8.8 % of cues acquired the aircraft
(30 to 70 % now).

**Radar** (`physical/radar.rs`, decision W2). One Swerling 1 draw per scan as the beam passes
the contact's bearing, against a 1e-4 false-alarm threshold; the fourth-root law about each
radar's reference [16]; the two-ray model for low targets, power as R^-8 beyond the crossover
`4 pi h_r h_t / lambda` with both heights above the tangent plane at the reflection point [E];
sea clutter `sigma0 R theta (c tau / 2) sec psi` from the research's sigma0 table by grazing
angle and sea state, a surface target needing 10 dB over it [E]. Accuracy 30 m or 1 % of range
and 1 degree in bearing [19] (the larger given as the fix's 1-sigma). A 2D radar plots an
aircraft at its slant range on the bearing, as the legacy model. Every radar revisits in at
most 5 s: the catalog refuses a slower one.

**AIS** (`physical/rf.rs`). A contact with its transponder on reports at its class's interval
for its speed [22] (turning ignored); heard within the VHF horizon `4.12 km (sqrt h1 + sqrt h2)`
[21] and the terrain, decoded at `exp(-reports a minute / 4,500)` [E].

**Sonar** (`physical/sonar.rs`). `SE = FOM - TL - dNL`, two-way TL for an active sonar,
`Pd = Phi(SE / 8 dB)` [24]; TL spherical to the water depth then cylindrical, plus Thorp
absorption [25,26]; ambient noise from turbulence, shipping, wind and thermal noise [26]. Each
sensor's FOM per kind is calibrated from its reference ranges (50 m of water, 5 m/s wind,
moderate shipping); a kind without one takes the nearest reference kind's shifted by the
difference in source level or target strength. Passive sonar recognises after 60 s of contact
and identifies after 180 s; active sonar localises (5 m + 1 %) and recognises by the track
after 60 s; a scanning sonar draws once per pass [E].

**ESM** (`physical/rf.rs`). An emitter in radio line of sight heard when `Pt Gt lambda^2 /
(4 pi R)^2` reaches the receiver's sensitivity, `0.95 Phi((Pr - S_min) / 3 dB)` a second [28];
a fix along the bearing with the error the bearing implies (as the legacy model).

**What a look yields, and the tracker.** A detection's label is the contact's size class
(`small_surface`, `large_surface`, `small_air`, `large_air`, `small_ground`, `large_ground`,
`subsurface`), a recognition's its family (the legacy model's family vocabulary:
`large_vessel`, `small_craft`, `aircraft`, `small_uas`, `ground_vehicle`, `person`,
`submerged`), an identification's its kind. A wrong call is drawn once per observer, sensor,
contact and level: at recognition twice the scenario's misclassification rate, at
identification the rate itself [E]. The tracker keeps each observer's sensor's evidence per
level (`Track::identity_from_looks`): the most specific level names the track, the looks that
agree at that level combine as independent (`1 - prod(1 - c)`), a lower level never adds to it
and a disagreeing lower level is superseded, not a conflict, so a closer look corrects a wrong
call (murmur-jln). Conflicts are the legacy identity's: AIS against an identification (or a
recognition's family), a speed the AIS kind cannot make, two disagreeing looks at the chosen
level.

**Hostile acts** (rules 9). An act is observed by the sensors that detected its author that
second (radar, EO/IR, sonar), each observer's best fix placing it.

## Acceptance (WORLD-V3-PLAN 2.8)

Anchors (`tracks::tests::physical`, `physical::{radar,rf,sonar}::tests`; the table:
`cargo test -p murmur-bridge anchor_table -- --ignored --nocapture`):

| Anchor | Computed | Published | Tolerance |
|---|---|---|---|
| FLIR M364C person D | 1.02 km | 0.97 km [13] | 40 % |
| FLIR M364C NATO 2.3 m D | 3.12 km | 2.34 km [13] | 40 % |
| EDGE G270 vehicle D / R | 7.62 / 1.93 km | 5.5 / 1.8 km [14] | 40 % |
| MX-20 MWIR vehicle D/R/I | 76.7 / 19.2 / 9.6 km | 77 / 19 / 9.6 km [3] | 5 % |
| MX-20 MWIR person D/R/I | 25.0 / 6.3 / 3.1 km | 25 / 6.3 / 3.1 km | 5 % |
| Mantis i45 LWIR vehicle D/R/I | 10.6 / 2.7 / 1.3 km | 10.6 / 2.6 / 1.3 km [37,38] | 10 % |
| Mantis i45 EO vehicle I | 7.7 km | 7.7 km | 5 % |
| IMO ship > 5,000 GT (Pd 0.8) | 11.45 NM | 11 NM [19] | 15 % |
| IMO ship > 500 GT | 9.03 NM | 8 NM | 15 % |
| IMO small vessel with reflector | 5.00 NM | 5.0 NM | 15 % |
| IMO navigation buoy | 4.51 NM | 4.6 NM | 15 % |
| IMO 10 m vessel | 3.80 NM | 3.4 NM | 15 % |
| AIS from 15 m to a 10 m and a 25 m antenna | 15.6, 19.7 NM | 15-20 NM [21] | inside |
| AIS MQ-9B at 7,600 m to a 10 m antenna | 372 km | about 370 km | 5 % |
| Sentinel diver / UUV / mini-sub (active) | 950 / 1200 / 1500 m | 0.9-1.0 / 1.2 / 1.5 km [27] | 2 % |

The MX-20 and Mantis figures are geometry (the research's own table); the atmosphere caps the
MX-20's long ones at about 15 to 25 km. The IMO ranges come out of one calibration (the small
vessel) with hull-down and the two-ray model; without them the large ships came out 42 % and
70 % long. The Sentinel ranges reproduce by construction (their FOMs are calibrated on them);
the test checks the chain and that a 12 m/s wind shortens them.

Properties (`tracks::tests::physical`): nothing beyond its horizon; nothing behind a ridge; a
fixed camera sees nothing outside its field; identification falls with range; night removes a
day camera's detections; sea state 5 loses a small boat to the navigation radar at 5 km and
keeps a ferry; a camera cued by a radar track identifies it with no order naming it; a camera
cued by a 2D radar to an aircraft 17 degrees above the plot finds it in elevation and identifies
it, and a locked camera follows a crossing boat and identifies it (murmur-7vp: both fail before
it); deterministic; per-observer evidence (a closer look corrects a wrong call). The slant path
(`physical::eo::tests`): along the sea the atmosphere is the horizontal model's value for value
in every band, visibility, contrast and light; a camera at 18 km reaches about the D/R/I ranges
its geometry and blur allow. The tracker (`tracks::tests::calib_air`): two sweeping radars'
bearings 4 s apart do not range an aircraft with no velocity, the same pair in one tick does,
and the legacy model's pair 4 s apart still does.

**The tracker on the physical model** (`calib_physical`, `air_calib_physical`,
`air_calib_physical_held_out`: the held-out sets, evaluated once, never tuned on; the asserting
calibrations stay on the legacy model they were judged on, unchanged; `calib_physical_tuning`,
`air_calib_physical_tuning`, `air_physical_pointing` and `air_physical_misses` are the tuning
sets' views):

- Surface (seeds 66-95): every criterion holds; per class NEES 1.01 to 1.88, the truth within
  two sigmas 87.5 to 98.4 % when updated, and a keep-out 60 and 100 s ahead holds every case at
  least as often as the old filter's. After murmur-7vp, still every criterion: per class NEES
  1.03 to 1.97 (before 1.01 to 1.88), within two sigmas 86.3 to 97.7 % (87.5 to 97.9).
- Air (seeds 221-250, before murmur-7vp; spent for the physical model since): honest per class
  and mix (NEES 0.75 to 1.96, within two sigmas 95.5 to 100 %), but in the all-sensors mix a
  keep-out 3 s ahead missed a loitering aircraft 9 times in 3,742 samples by up to 2,079 m (10 s
  ahead 5 times, 1,671 m), a weave twice (1,212 m), a route once (221 m), a helicopter's hover
  and go once (38 m), the far transit once (87 m); the tracker alone exceeded its bounds there
  for loitering, route and weave; hover and go's velocities had a mean normalized error of
  9.41. The tracker almost never gives an aircraft a velocity in any mix (0 to 0.7 % of
  samples): its tracks are unranged 2D-radar bearing segments, 1 to 3 km of 1-sigma, so
  keep-outs are wide (blocks 89 to 100 % at 60 s). The likely cause, as written then: a 2D
  radar's cue of an aircraft carries no elevation, so the payload manager aims at the plot on
  the sea and rarely acquires the aircraft; a camera cued along a bearing should search in
  elevation (a follow-up, judged on new held-out sets).
- murmur-7vp: the elevation search and the gimbal's following (Pointing, above) were measured on
  the air tuning sets only and judged once on a new held-out set written before them
  (`physical_held_out_cases`, seeds 251-280, every class in yet other variants). The cameras
  now acquire aircraft (above), but the keep-out misses were not the cameras': on the tuning
  sets every miss 3 s ahead (24 in 227,808 samples of the all-sensors mix) came with only the
  USV's and the ship's 2D radars fixing the aircraft in the 10 s before, 23 of them more than
  10 s after a camera's fix; the mix's radars alone, without a camera, miss 21 times on the
  physical model and never on the legacy one. The physical radars plot an aircraft once a scan,
  as the beam passes (a radar's fix in 56 % of the all-sensors mix's seconds; the ship's radar
  sees a 1 m^2 aircraft on about 0.4 to 0.75 of its scans 5 to 7 km out), where the legacy model
  gave every radar a fix nearly every second: two bearings that cross by 30 degrees come
  seconds apart, the tracker takes the pair as ranging it (`model::CROSSING_DEG`,
  `RANGE_FRESH_S`), and the estimate slides along a bearing past its sigma. A second pattern
  shows now that cameras fix aircraft: after a camera's few fixes of the tightest loiter (500 m
  at 55 m/s), the track counts as ranged for 10 s on one radar's bearings and slides along them
  (the radar and EO/IR mix of the tuning sets, 4 misses 3 s ahead by up to 102 m, none before
  the fix). Both are the tracker's (a follow-up; the tracker is unchanged here).
  The new held-out set's one run: in the 2D radar and the radar and EO/IR mixes every criterion
  holds; in the all-sensors mix every class is honest (NEES 0.90 to 1.95 over every mix, within
  two sigmas 96.1 to 100 %), the steady classes' velocities too (normalized error 0.59 to
  1.26), no altitude is given, and a keep-out 30, 60 and 100 s ahead holds every case, but a
  keep-out 3 s ahead missed the weave 4 times in 4,730 samples by up to 1,575 m (10 s ahead 3
  times, 1,130 m) and the hexagon route twice in 4,609 by up to 220 m, and the tracker alone
  exceeded its bounds for the weave (3 s: 0.11 %, 1,685 m; 10 s: 0.06 %, 1,241 m) and the
  route (3 s: 330 m against 300). Each of those misses came with only the same two radars in
  the 10 s before, 13 to 112 s after a camera's fix. Aircraft still almost never get a velocity
  (0 to 0.6 %): a camera's look is a few seconds a cue (decision W1: identify, then search
  again; an identified contact is looked at again after 2 to 5 minutes), never the tens of
  seconds of position fixes the tracker's velocity needs.
- murmur-7vp, the tracker's half (with the slant path above), developed on the air tuning sets
  only and judged once on a third set written before either change
  (`physical_held_out_two_cases`, seeds 281-310, every class in yet other variants; 251-280
  spent). A sweeping radar's plot carries its scan (`Detection::scan_s`) and the tracker
  time-aligns two such radars' bearings before taking them as ranging an aircraft, as a
  multi-sensor tracker fuses asynchronous sensors: each measurement in time order, the earlier
  one's information carried to the later's time by the motion model and widened by what the
  model does not know of the motion, a cross fix only as good as its bearings are
  time-coincident. The earlier line, carried to now, is wider across itself by the filter's
  velocity uncertainty across it times the gap (60 m/s for a track without a velocity), and the
  pair ranges the aircraft only when its cross fix is still as good as two simultaneous bearings
  crossing at 30 degrees give (`model::Bearing::aligned`: `sin theta >= sin 30 * sqrt((s1^2 +
  (sv dt)^2 + s2^2) / (s1^2 + s2^2))`, the rule as it was at `dt` 0). A reset to one radar's
  segment drops the other radars' bearings: their information along it is gone from the
  estimate, which the rule before still counted when a bearing seconds old crossed the next one
  (the tuning sets' misses on straight flight and a turn, for one: the track reset to radar A's
  segment, then taken as ranged by A's next bearing and B's discarded one, slid kilometres along
  A's bearing on no information). And a ranging
  fix holds a track on sweeping radars' bearings 6 s, not 10 (`model::SWEPT_RANGE_FRESH_S`): a
  camera's look of a few seconds gives a velocity measured over a turn, and an aircraft turning
  at 6 to 8 m/s^2 leaves that extrapolation by a bearing's error in 5 to 6 s (the second pattern
  the camera half found, tightest loiters sliding along one radar's bearing). The legacy
  model's radars, which plot every tick, keep the rules as they were (`scan_s` none), so every
  legacy record and calibration is unchanged. On the tuning sets (all-sensors mix, a keep-out
  3 s ahead): 24 misses in 227,869 samples before, 11 with the alignment and the reset (none
  left on two radars' bearings seconds apart; the tightest loiters' remained), 2 with the 6 s
  window (8 s: 6, 7 s: 4, 6 to 4 s: the same 2; in the radar and EO/IR mix the tightest loiter's
  4 misses went at 7 s). Those 2 (random turns at 40 m/s, seed
  190) are a pair taken in one tick crossing at 30.6 degrees with one radar's bearing 3.6 of
  its sigmas off: the cross fix 1.1 km out at 3.2 of its sigmas, a tail of the 30-degree rule,
  and with them the class's tracker alone 3 s ahead at 363 m against its 300. Every class
  stays honest (NEES 0.78 to 1.95 over the mixes, within two sigmas 95.2 to 100 %; before 0.75
  to 1.95, 94.9 to 100 %). The one-turn class's velocities (round two's 120-degree turn at 5
  deg/s: a velocity given in 0.2 % of samples as the turn begins, the steady model's lag)
  have a normalized error of 7.48, as before the change (7.53): a tuning-set failure the change
  neither made nor fixed.
  The third set's one run (`air_calib_physical_held_out_two`): in the 2D radar and the radar
  and EO/IR mixes every criterion holds (NEES 1.66 to 1.96, within two sigmas 98.3 to 100 %, the
  tracker alone at most 0.33 % and 98 m 3 s ahead, every keep-out holding, no velocity and no
  altitude given). In the all-sensors mix every class is honest (NEES 0.92 to 1.78, within two
  sigmas 95.5 to 100 %), the steady classes' velocities too (normalized error 0.23 to 0.54,
  given in 0 to 0.4 % of samples), no altitude is given, the tracker alone holds its bounds (3 s
  ahead at most 0.70 % and 213 m, 10 s 0.07 % and 109 m, 30 s 0.14 % and 604 m), and a keep-out
  10, 30, 60 and 100 s ahead holds every case; but a keep-out 3 s ahead missed the approach and
  break 3 times in 2,837 samples, by up to 102 m (seeds 281, 284 and 304: a break 140 degrees
  left at 7 deg/s while speeding up to 90 m/s, about 11 m/s^2 of turn, seen through most of it
  only by the two radars' bearings, which cross at 32 to 39 degrees in one tick or a second or
  two apart and so range it, while both models' velocity lags the turn: the estimate 540 to 720
  m behind at 3.4 to 3.9 of its sigmas). The constant-velocity models' lag in a hard turn, not
  the bearings' timing: the tracker's follow-up is a turn model. The legacy model is untouched:
  the legacy calibrations' tables (surface and air, tuning and held-out sets) and a keep-out
  evaluation (seeds 1 to 3, 20 situations, the radar and all variants) came out byte for byte
  as at the commit before (d80fd4a), and the golden episode and v1/v2 null fidelity hold. So
  did the surface evaluation on the physical model (seeds 66-95, every criterion holding): its
  contacts on the water give no 2D radar bearings, and its cameras, at most 80 m up, look a few
  kilometres along the sea, where the slant path changes the atmosphere's share by under 1 %.

**Cost** (`eval/headless/sense_perf.py`): a Golden Gate world of 30 stand-ins of ten types and
40 contacts of every domain, headless, 30 simulated minutes: 91 times real time on the
physical model, 82 on the legacy one (this host, 28 cores, one episode).

## Python

`eval/headless/sensing.py` (`SenseModel`, through `murmur-headless --sense`, or the `sense`
request of a running episode) answers, for one of a platform's sensors and one contact under
given conditions: the line of sight (horizon, hull-down, terrain), the chance that the sensor
pointed at it detects, recognises and identifies it, the wide field's search chance and mean
time to detect, a radar's revisit, AIS's interval, and on request the D/R/I ranges along the
line. They are the bridge's own functions (`physical/predict.rs`). What they leave out is the
running model's state: pointing, dwell, the scan or report a draw falls on, misclassification.
The legacy model's Python ports (`v2/labels.py`, `families.py`, `agents_v2.py`,
`agents/playbook/beliefs.py`) are unchanged: they describe the legacy model for v2 records.

## Limits, stated

- Association is still by contact id (perfect association); a gating tracker is later work.
- EO/IR gives a position, as the legacy model (a passive sensor without a rangefinder is angles
  only: the murmur-oho follow-up stands). Its fix error is the legacy model's.
- The payload manager's constants (its elevation band and bar dwell among them), the
  classification times, the misclassification rates per level and the size-class thresholds
  are estimates. The physical radars have no elevation beam: a 2D radar sees an aircraft at
  any elevation, and a camera cued by it searches up to the slant range.
- The tracker's two models are constant-velocity: through a hard turn (a break at 11 m/s^2,
  the tightest loiter at 6) seen only by bearings they lag, and a keep-out 3 s ahead can miss by
  tens of metres (the second air held-out set's approach and break, above); a coordinated-turn
  model is the follow-up.
- Radar: no land clutter (ground contacts are seen as on water, terrain masking apart), no
  pulse integration or sidelobes, a ship's radar heard by ESM every second (its sidelobes)
  rather than once per turn, and the X-band clutter table for every band.
- AIS: turning intervals and class B SO ignored; slot load counts the reports a receiver could
  hear, not their collisions in time.
- Sonar: the reference ranges carry the model (source levels and thresholds are mostly not
  public); no ray paths, layers or convergence zones; bathymetry only from a grid a scenario
  names, else one depth.
- The sun ignores refraction; the thermal crossover and the low-light share are shapes [E].
- The slant path's atmosphere is one exponential profile per kind of extinction, the same
  everywhere: no capping inversion, elevated haze or dust layer, or cloud, and the molecular
  (Rayleigh) part of the visibility thins with the aerosol's 1.2 km rather than its own 8 km
  (a high camera's longest detection ranges are a little long for it); the line of sight is
  straight.
- The catalog: Sea Hunter carries its navigation radar only (a second radar entry would change
  what the legacy model reads for v1 and v2); REMUS's side-scan listens all round, not to its two
  sides; the Saildrone's cameras image every 5 s, looked through every second here; fixed
  cameras (the Magura's, the FPV's, the UGVs' drive cameras) see along the heading only.
- A tick is a second: a look, a scan pass and a report are resolved to the second.
