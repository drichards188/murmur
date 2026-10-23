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
| neither | `legacy` (the default until the platform catalog's figures land, WORLD-V3-PLAN B1) |

The v1 and v2 worlds are pinned to `legacy` by the evaluation layer, not in their contacts files
(the v1 generator's digests hold those byte for byte): `MURMUR_SENSE_MODEL=legacy` in every
headless episode of a v1 or v2 world (`eval/headless/situations.py` `process_spec`), in a G1
full-stack copy (`eval/headless/fullstack.py`) and in the Phase G evaluation's stacks
(`eval/stack.py`). A later change of the default leaves their records as they were. A legacy
scenario's truth messages carry nothing new (every new field is left out when unset): its
episodes are byte-identical.

The bridge says which model it runs on (`sensing: the physical model (the scenario)`), lists the
catalog sensors running on placeholders, and says once if there is no terrain grid.

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
legacy signature multiplier; an ESM receiver of -80 dBm and 5 degrees. Real figures today:
the Puma (Mantis i45), the MQ-9B (MX-20 and Lynx), Sea Hunter's navigation radar, the BlueROV's
Ping360 and the buoy. The platform catalog (WORLD-V3-PLAN section 3, B1) fills the rest.

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
times the visible's in haze [11]. A day camera has no light below civil twilight, a low-light
one 0.4 of its range at night [E]; a thermal camera's contrast dips to a quarter with the sun 5
degrees up (crossover, shape [E]), which costs it the square root of that in pixels and its
atmospheric range. Time to detect in view: one draw per look whether the contact is
detectable at all (P) and one when (exponential, `tau = 3.4 / P` s [12]). Recognition after a
second of detection; identification needs the narrow field on the contact for 2 to 3 s [E].

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
keeps a ferry; a camera cued by a radar track identifies it with no order naming it;
deterministic; per-observer evidence (a closer look corrects a wrong call).

**The tracker on the physical model** (`calib_physical`, `air_calib_physical`: the held-out
sets, evaluated once, never tuned on; the asserting calibrations stay on the legacy model they
were judged on, unchanged):

- Surface (seeds 66-95): every criterion holds; per class NEES 1.01 to 1.88, the truth within
  two sigmas 87.5 to 98.4 % when updated, and a keep-out 60 and 100 s ahead holds every case at
  least as often as the old filter's.
- Air (seeds 221-250): honest per class and mix (NEES 0.75 to 1.96, within two sigmas 95.5 to
  100 %), but in the all-sensors mix a keep-out 3 s ahead missed a loitering aircraft 9 times in
  3,742 samples by up to 2,079 m (10 s ahead 5 times, 1,671 m), a weave twice (1,212 m), a route
  once (221 m), a helicopter's hover and go once (38 m), the far transit once (87 m); the
  tracker alone exceeded its bounds there for loitering, route and weave; hover and go's
  velocities had a mean normalized error of 9.41. The tracker almost never gives an aircraft a
  velocity in any mix (0 to 0.7 % of samples): its tracks are unranged 2D-radar bearing
  segments, 1 to 3 km of 1-sigma, so keep-outs are wide (blocks 89 to 100 % at 60 s). The likely cause: a 2D radar's cue of an aircraft carries no elevation, so
  the payload manager aims at the plot on the sea and rarely acquires the aircraft; a camera
  cued along a bearing should search in elevation (a follow-up, judged on new held-out sets).

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
- The payload manager's constants, the classification times, the misclassification rates per
  level and the size-class thresholds are estimates; a 2D radar's cue of an aircraft carries
  no elevation (above).
- Radar: no land clutter (ground contacts are seen as on water, terrain masking apart), no
  pulse integration or sidelobes, a ship's radar heard by ESM every second (its sidelobes)
  rather than once per turn, and the X-band clutter table for every band.
- AIS: turning intervals and class B SO ignored; slot load counts the reports a receiver could
  hear, not their collisions in time.
- Sonar: the reference ranges carry the model (source levels and thresholds are mostly not
  public); no ray paths, layers or convergence zones; bathymetry only from a grid a scenario
  names, else one depth.
- The sun ignores refraction; the thermal crossover and the low-light share are shapes [E].
- Most platforms run on placeholders until the catalog's figures replace them (B1).
- A tick is a second: a look, a scan pass and a report are resolved to the second.
