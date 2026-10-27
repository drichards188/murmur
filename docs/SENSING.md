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

`sim/platforms.toml`'s sensor entries keep `type` and `range_m` (the legacy model's reach, and
what the decision engine reads in a run on that model; on the physical model it reads the
generated `physical_reach_m`, "The decision engine's reaches" below) and may add the physical
fields (`physical/spec.rs`):

| Sensor | Fields |
|---|---|
| `eo_ir` | `slew_dps`; `channels = [{ band = "day" \| "lowlight" \| "mwir" \| "lwir", wide_deg, narrow_deg, px, blur_urad, panoramic }]` |
| `radar` | `r50_m` and `rcs_ref_m2` (Pd 0.5 per scan), `scan_s` (at most 5 s), `beam_deg`, `cell_m`, `band_ghz` |
| `sonar` | `mode = "passive" \| "active"`, `freq_khz`, `ranges = { <kind> = m }`, `max_m`, `scan_s`; an active sonar that also listens, `passive = { ranges, freq_khz, max_m }` (its band and reach the active mode's unless given) |
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
| RQ-4 | radar 200 km on 10^4 m^2, 5 s, X band, a 1 m range cell, a 3 deg beam, a Doppler mode; EO 1,024 px and MWIR 640 px at 10 and 30 urad, no zoom; ESM -90 dBm, 1 deg | [31,32,29,75]; IFOVs [E]; the rest [E], [E here] |
| MQ-9B | Lynx radar 300 km on 10^4 m^2, 5 s, Ku band, a 1 m range cell, a 3 deg beam, a Doppler mode; MX-20 MWIR 640 px 18.2 to 0.24 deg, spotter 1,920 px 1.07 to 0.2 deg, 57 deg/s; AIS | [34,3,74,75,76]; r50 scaling, the cell, beam, blur floors [E] |
| CCA | AESA radar (3D) 100 km on 1 m^2, 5 s, X band, a 1 m range cell, a 3 deg beam, a Doppler mode; MX-15-class MWIR 26.7 to 0.54 deg, spotter 1.1 to 0.43 deg; ESM -80 dBm, 2 deg | [E]; [35]; [30] |
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
| Sea Hunter | navigation radar on the IMO anchors, 2.5 s; a ring as the Corsair's; AIS; the MS3 hull sonar, active (a submarine at 7.5 km, a UUV at 1.5, to 20 km) and passive (the buoy's ranges per class) | [19,18,43,44,25]; [E]; passive ranges [E here] |
| Saildrone Explorer | a fixed 360 deg ring of 60 deg day cameras, 1,920 px; AIS. **No radar** | [45,46,47]; cameras [E] |
| Buoy | passive, 1 kHz: a cargo ship 20 km, a fast boat 3.5, a submarine 1, a UUV 0.3, a diver 0.1 | [48]; ranges [E] |
| BlueROV2 | Ping360, active 750 kHz, to 50 m, 20 s a turn | [49] |
| REMUS 600 | side-scan as an active sonar, 455 kHz, to 300 m, a UUV at 225 m | [50]; [E] |
| Orca XLUUV | passive, 1 kHz: a cargo ship 20 km, a submarine 1.5; the quieter classes the buoy's | [E]; [E here] |

`range_m` stays the legacy model's reach (and the decision engine's in a run on that model); it
was not changed here. Against the physical model's typical range (`cargo test -p
murmur-bridge catalog_table -- --ignored --nocapture`: a camera's detection of a 5 m target, a
radar's of 100 m^2, ESM a patrol boat's radar, AIS a ferry, each sonar per kind; clear day, sea
state 2, no terrain), these differ by more than 2x, for a later decision (taken 10-24, bd
murmur-1u4: in a run on the physical model the engine reads that model's own reaches, the next
section; `range_m` unchanged): the RQ-4's radar (200
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

## The decision engine's reaches (bd murmur-1u4)

**Where the engine reads a reach.** In one place: an engagement's assessment
(`engage::observe`, `engage::covering_sensor`). After the effect, a vehicle of ours with a fresh
position covers the effect point when one of its sensors that confirms presence or absence in
the target's domain (`engage::sees`: radar and EO/IR for air, ground and surface; sonar for
surface and subsurface; never AIS, cooperative, nor ESM, emitters only) reaches the point;
covered 15 s with no detection is "ceased". The engine's other reasoning about our sensors is by
type, never by range, and reads nothing of the catalog's: a track's altitude counts only when a
recent radar (an aircraft) or a radar or AIS report (a vessel) backs it (`engage::track_altitude`),
EO/IR and sonar are the classifying sources a lost contact's claimant is judged by (`lost.rs`),
and the scenario runner stamps radar and AIS as its ranging sources (`scenario.rs` `ranging`).

**What it reads.** On the legacy model `range_m`, as before. On the physical model the catalog's
`physical_reach_m`: for each radar, EO/IR and sonar and each domain it confirms in, how far out
the physical model's prediction (`physical/predict.rs`, the bridge's own functions), the sensor
pointed at the domain's reference contact in the reference conditions, detects it at least half
the time all the way:

- **Reference contact**: a contact of no known kind of the domain, as `range_m` was the reach of
  a nominal contact (signature 1.0) rather than of any one threat: the model's own signature for
  one (`signature::kind_signature`'s default by domain: surface D_c 5 m, 3 m high, 10 m^2, 2 K,
  140 dB, target strength 10 dB; air 3 m, 2 m, 1 m^2, 5 K; ground the NATO 2.3 m vehicle,
  100 m^2, 2 K; subsurface 120 dB, 10 dB), where the contacts process puts a contact that names
  no altitude (an aircraft 300 m up, under water 30 m deep, else at the surface).
- **Reference conditions**: the catalog table's: a clear day (20 km visibility) with the sun 45
  degrees up, sea state 2, the sonar's reference environment (50 m of water, 5 m/s of wind,
  moderate shipping), no terrain; our vehicle at its catalog operating height (`alt_m`, else
  `typical_alt_m`) and `mast_m`.
- **Detection**: the assessment asks whether the contact is still there, not what it is.
- **All the way**: the first fall below one half out from the sensor (a 1 % grid, then
  bisection), from 1 m out, or a radar's minimum range (40 m from the antenna: IMO MSC.192(79)
  5.4.1 [19]; nearer, the reference contact would be alongside, under the antenna, where the
  sea's specular glare is since sensing round 3). Where the detection probability only falls
  with distance that is the model's D range (`predict::ranges`); where it does not, the disc
  covered throughout, never a crossing beyond a gap. The two differ only for the airborne
  radars against the surface contact: holding still, it is lost in the sea's glare (the steep
  grazing angles' reflectivity and the specular return under the aircraft) within 16.5 km of
  the MQ-9B's nadir and 22.6 km of the CCA's, and seen beyond it to the noise's range; the
  RQ-4's radar sees it nowhere (its glare reaches past its noise range). So they cover no disc
  (no surface reach). Before sensing round 3 a defect in the grazing angle made it come and go
  within 2 km of the nadir, and the reach was that ring's inner edge (0.40 to 0.44 km).

Rounded to 10 m; a domain reached under 5 m is left out (the sensor does not cover there).
Derived, never tuned (W3), never edited by hand: `cargo test -p murmur-bridge
write_physical_reaches -- --ignored --nocapture` writes the line after each platform's
`sensors` and prints the table below, and `tracks::tests::reaches::physical_reaches_are_the_generators`
fails while a committed value is not the generator's (after a change to the physical model or to
a sensor's figures that moves one).

| Sensor | `range_m` | Air | Ground | Surface | Subsurface | Small fast boat, small drone (80 m) |
|---|---|---|---|---|---|---|
| RQ-4 radar (18,000 m) | 200 | 9.32 | 60.7 | none (not seen holding still) | | 0, 0 |
| RQ-4 EO/IR | 50 | 184 | 142 | 198 | | 180, 14.8 |
| MQ-9B radar (7,600 m; also MQ-9B radar-only) | 100 | 29.1 | 94.7 | none (seen 16.5 to 52.7) | | 0, 8.42 |
| MQ-9B EO/IR | 40 | 108 | 87.2 | 87.8 | | 87.7, 32.5 |
| CCA radar (11,000 m) | 150 | 99.5 | 316 | none (seen 22.6 to 178) | | 0, 36.0 |
| CCA EO/IR | 30 | 143 | 113 | 126 | | 123, 20.6 |
| FVR-90 EO/IR | 10 | 19.3 | 14.3 | 15.2 | | 16.8, 10.4 |
| Puma EO/IR | 4 | 16.2 | 14.3 | 14.5 | | 14.3, 9.06 |
| Switchblade EO/IR | 3 | 16.2 | 14.3 | 14.4 | | 14.3, 5.70 |
| X10 EO/IR | 1.5 | 15.8 | 13.9 | 14.1 | | 14.0, 4.94 |
| FPV EO/IR | 0.5 | 0.48 | 0.42 | 0.92 | | 0.64, 0.04 |
| Small airship EO/IR | 8 | 20.8 | 14.9 | 15.1 | | 17.1, 12.4 |
| Logistics and mine-laying UGVs EO/IR | 1, 0.8 | 2.43 | 1.88 | 4.08 | | 2.86, 0.27 |
| Armed UGV EO/IR | 2 | 20.1 | 12.0 | 13.3 | | 12.1, 10.6 |
| Recon UGV EO/IR | 2 | 16.3 | 10.2 | 12.5 | | 11.2, 2.14 |
| Magura EO/IR | 5 | 10.5 | 7.33 | 10.1 | | 8.65, 1.28 |
| Corsair EO/IR | 8 | 11.3 | 8.84 | 12.1 | | 10.7, 1.43 |
| Sea Hunter radar (also radar-only) | 40 | 7.53 | 10.1 | 9.77 | | 7.43, 2.83 |
| Sea Hunter EO/IR | 15 | 11.3 | 8.99 | 13.1 | | 11.7, 1.43 |
| Sea Hunter sonar | 10 | | | 4.87 | 4.87 | 4.87 |
| Saildrone EO/IR | 5 | 3.66 | 2.81 | 6.09 | | 4.28, 0.42 |
| Buoy sonar | 3 | | | 0.37 | 0.12 | 0.09 |
| BlueROV sonar | 0.5 | | | 0.05 | 0.04 | 0.05 |
| REMUS sonar | 3 | | | 0.26 | 0.27 | 0.26 |
| Orca sonar | 8 | | | 0.36 | 0.12 | 0.07 |

(km; blank: the sensor does not confirm a contact in that domain; "seen": where the contact is
detected at least half the time beyond the gap, context; the last column is context, not used:
the same rule against a small fast boat holding still and a small drone 80 m up.)
What moves most against `range_m`: the airborne radars cover no surface contact holding still
(the sea's glare under them; they see one beyond it, and one moving along their line of sight
through their Doppler modes; their cameras cover it 88 to 198 km out); every sonar covers less, most of all on
the kinds it was not built for (the buoy and the Orca a few hundred metres, not 3 and 8 km); the
small aircraft's and the UGVs' cameras cover several times more (14 to 20 km where `range_m`
gave 1.5 to 4); the Sea Hunter's radar 7.5 to 10 km, not 40.

**Which runs read them, and replay.** The bridge builds the decision facts from the catalog with
the reaches of the sensing model in force (`Tracks::sensing_model`: `MURMUR_SENSE_MODEL`, else
the model the last truth message ran on, else the default, physical; `Decision::facts_catalog`,
`murmur_decide::Catalog::reaching`). On the physical model each radar, EO/IR and sonar's facts
carry `reach_m`, which the engine reads instead of `range_m` (`SensorFact::reach_for`; a domain
with no reach there is not covered); on the legacy model (the v1 and v2 worlds, G1's full stack
and the Phase G stacks, all pinned by `MURMUR_SENSE_MODEL=legacy`) they carry none, so those
runs' facts, logs and decisions are byte for byte as before. The facts are logged with every
decision, so replay recomputes either kind of log without the catalog or the sensing model; an
old log has no `reach_m` and replays as it always did. No decision-rules version: the rule (a
seeing sensor reaching the point covers it) is unchanged and only its input differs, and
`TrackFacts`, the tracker and `tests/before_oho` are untouched. The scenario runner reads
`range_m` unless told `--reaches physical` (`RunOptions::reaches`); its tracks are scripted
either way.

**What it changed.** The 94 engine scenarios under every option set tried (the default, rules 5
to 15, the review proxy and its misreading variant, the nominator, `--ident off`, track noise
seeds 1 to 5 and 7, act noise seeds 1 to 3): on the legacy reaches every result and audit log is
as before (the build stamp aside); on the physical reaches every scenario ends as designed and
replays, and the only change is the evidence of three assessments, each still ceased after
15 s: the observer's camera is named in place of its radar, in 48 because the MQ-9B's radar no
longer reaches the surface contact 1.5 km off (0.44 km then, none since sensing round 3, whose
reaches left all 94 results as they were; its camera 87.8), in 72 and 76 because
the Sea Hunter's camera now reaches farther than its radar (13.1 against 9.77 km; both reach the
point 1.2 km off). The scenarios' coverage comes from the engaging Switchblade over the point
and an observer within 1.5 km, so they exercise little of the change. The keep-out
evaluation does not read a reach (the nomination's checks only, on tracks from the legacy model):
its numbers cannot move with them.

**Limits.** A reach is a pointed sensor's in clear daylight: a fixed camera's field, where the
payload manager points, night, haze and terrain are not in it, and the engine does not know the
conditions. `sees` still leaves radar and EO/IR out of a subsurface target's coverage, though the
physical model lets them see a periscope's mast (a type rule: changing it is a rules change).

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

**Where a vehicle senses from, and where it lays its fixes** (both models; murmur-k8i). A
vehicle's sensors see from where it is: range, horizon, terrain, the water and every draw are the
truth's. It lays what they measure off from where it believes it is, so a radar's plot, a
camera's, a sonar's and an ESM bearing's fix carry its navigation error (a GNSS spoofer's walk,
a dead reckoning's drift; `Observer::geolocate`), a 2D radar's slant-range plot is laid off from
the radar's believed position, and an AIS report, the contact's own GPS, carries none. Its
payload manager reckons a place (a cue, its search points, a cue's distance) from where it
believes it is and points there, so a camera cued by its own radar still points right, and one
cued by another vehicle's track points off by its error; a lock follows its contact in the image,
which the vehicle's navigation does not enter. The headless simulator alone knows a vehicle's
truth: it gives the bridge each vehicle's error (the stand-in's own filter, nav.rs), and the
observer is the position the vehicle reports less that error. The error is horizontal (a spoofer
moves no height or heading). Its last step's detections, with their contacts' truth and each
observer's error, are the simulator's `detections` request (evaluation only).

**EO/IR** (`physical/eo.rs`). Pixels on target `p = D_c / (R IFOV)`, `IFOV = max(FOV / px, blur
floor)`; 50 % thresholds 1.5, 6 and 12 px [5]; the ACQUIRE curve `P = x^E / (1 + x^E)`,
`E = 2.7 + 0.7 x` [7,8]; atmosphere `1 / (1 + (R / R_atm)^6)` (shape [E]) with
`R_atm = 0.69 V` in the visible [9] and `ln(dT / 0.1 K) / 0.2 km^-1` in the IR [10], at most 4.5
times the visible's in haze [11]. `R` is the line of sight's sea-level equivalent (the slant
path, below): a horizontal path along the sea is its own length. A day camera has no light below civil twilight, a low-light
one 0.4 of its range at night [E]; a thermal camera's contrast dips to a quarter with the sun 5
degrees up (crossover, shape [E]), which costs it the square root of that in pixels and its
atmospheric range. Time in view: each level accrues as a hazard while the contact is in the
field, at that second's range, light and field (murmur-1z6.6's dev check, below): ACQUIRE's time
law `P (1 - exp(-t / tau))`, `tau = 3.4 / P` s [12], has the initial rate `P / tau = P^2 / 3.4` a
second, and a look accrues it every second it holds the contact, reaching the level when the sum
passes a draw made once per look and level (an exponential). A short look is ACQUIRE's; a longer
one keeps accruing (each second of a moving scene is new evidence) where one draw a look stopped
at `P`. Detection's constant is 3.4 s [12]; recognition's and identification's 1 s once their
dwell is met, recognition a second after the detection, identification after 2 s of the narrow
field ([E here]: at `P = 1` a level comes in the first second its dwell allows two times in
three, so an identification in 2 to 3 s of the narrow field as the research's [E]). A contact
found stays held while its detection chance stays above a share of what it was when found, drawn
once a look (below it, it fades out of the image).

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
tracker keeps it centred) until it is identified, while it stays a candidate (fresh, in reach and
the task area; asked each second) and no cue of a higher class has come (a contact not identified,
against a revisit: it takes the camera at once; a nearer contact of the same class does not, which
would thrash between two contacts and finish neither), and while the identification keeps getting
likelier (the contact closing, the light rising) or did within 10 s; then it searches again. Each camera keeps its own history of what it
identified and what it looked at in vain (a payload manager on its vehicle knows what its camera
did, not another's). Rules 21 (bd murmur-bia, DECISIONS.md "W1 follow-up: an identify order"):
while a vehicle flies an identify order (its task's `look` names a track), its cameras cue the
contacts behind that track first, whenever one is a candidate this camera has not identified,
ahead of the nearest-unidentified rule and of a lock on another contact; the rest of the
manager is unchanged. A look in progress survives the slew and zoom. A camera that cannot slew
looks along its vehicle's heading; a panoramic one does not point. The constants are [E].

A cue to an aircraft whose height no sensor measured (a 2D radar's plot, a camera's or ESM's
fix) is an elevation search (murmur-7vp): a 2D radar measures range and bearing, not elevation,
so the aircraft is somewhere on the arc its slant range allows along that bearing, from just
above the sea to as high as it may fly; the camera steps its wide field through those elevations
bar by bar (four fifths of the field's height a step), lowest first, staring 4 s at each once
settled, as its search steps the task area and as an EO director cued by a 2D surveillance radar
scans in elevation at the cued bearing. The band is 10 m to 8,000 m, or the slant range if lower
([E here]: above the 4 to 6 km service ceilings of the light aircraft and helicopters the air
kinds stand for, and the 7,000 m of the engagement scenarios' highest aircraft). A
height-finding radar's fix (within 10 s) aims at the aircraft itself. A cued camera holds its
cue's pattern (its one aim, or its bars) while each new fix keeps the contact within it, and
plans it again from the fix only when the fix puts the contact outside it (sensing round 3,
below): one aim while the fix stays in the middle of its field (a quarter of its width off the
boresight), a known height while it lies in the current bar's field, a fix with no height while
some place it allows lies in one of the bars' fields (another sensor's fix of the aircraft, a
2D radar's arc or a camera's position, is another line through the same aircraft, not a new
search); a new plan keeps the bar the search had reached. A
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
sea clutter `sigma0 R theta (c tau / 2) sec psi`, a surface target needing 10 dB over it [E]
(sensing round 3, below: `c tau / 2` the radar's range resolution, `psi` the spherical earth's
grazing angle, valid to the nadir; `sigma0` the research's table to 10 degrees, the NRL model's
slope beyond it to 60 [71] and its frequency term for other bands, and the quasi-specular return
near vertical incidence [72,73]); a radar with a Doppler mode sees a surface target moving along
its line of sight faster than the sea clutter's Doppler band (`0.25 + 0.44 U` m/s, `U` the wind
[72]) against the noise alone. Accuracy 30 m or 1 % of range
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
difference in source level or target strength. Passive sonar recognises after 60 s of signal
heard and identifies after 180 s; active sonar localises (5 m + 1 %) and recognises by the track
after 60 s of echoes; a scanning sonar draws once per pass [E]. The evidence is the signal
received, not the time since the first detection (murmur-1z6.6's dev check, below): each
detection counts the draw interval it closes (a second, or a scanning sonar's turn), a run's first
a second, and a run ends after 30 s unheard; a look's confidence grows with it (passive `0.8 (1 -
exp(-s / 120))`, active `0.6 (1 - exp(-s / 60))`). Heard every second it is the run's length, as
before; heard one second in fifteen, fifteen times slower, and a run broken by a 30 s gap starts
again. An active sonar that also listens (Sea Hunter's MS3, "active and passive" [44]) runs its
passive mode beside it: its own draws and evidence, the same wrong calls (one sensor's).

**ESM** (`physical/rf.rs`). An emitter in radio line of sight heard when `Pt Gt lambda^2 /
(4 pi R)^2` reaches the receiver's sensitivity, `0.95 Phi((Pr - S_min) / 3 dB)` a second [28];
a fix along the bearing with the error the bearing implies (as the legacy model).

**What a look yields, and the tracker.** A detection's label is the contact's size class
(`small_surface`, `large_surface`, `small_air`, `large_air`, `small_ground`, `large_ground`,
`subsurface`), a recognition's its family (the legacy model's family vocabulary:
`large_vessel`, `small_craft`, `aircraft`, `small_uas`, `ground_vehicle`, `person`,
`submerged`), an identification's its kind. A wrong call is drawn once per observer, sensor,
contact and level: at recognition twice the scenario's misclassification rate, at
identification the rate itself [E]. ESM's detection names only the domain of the emitter it
hears (`aircraft`, `vessel`, `ground_vehicle`, `submerged`), not its size.

**Identity fusion** (`Track::identity_from_looks`; the defects (a), (c) and (d) of
SCORING-V3.md 15). The tracker keeps each observer's sensor's evidence per level, the surest
each has been, and fuses it by specificity, reliability and weight of evidence:

- *Precedence: the more specific label wins.* A kind (an identification) over the AIS
  identity's kind over a family (a recognition) over a size class (a radar's or a camera's
  detection) over the domain alone (ESM's detection). A less specific label never displaces a
  more specific one, whatever its confidence: ESM's `aircraft` gives way to a radar's
  `small_air` (before, the surer ESM look named the track and the radar's correct size class
  lost, 15 (c)). A lower-level look that agrees (a family's or size class's kind, ESM's domain)
  supports the label (`supported_by`) but never adds to its confidence; one that disagrees is
  superseded, not a conflict, so a closer look corrects a wrong call (murmur-jln).
- *Confidence.* The looks that agree at the chosen level combine as independent, `1 - prod(1 -
  c)` (the AIS identity's 0.85 too when it names the same kind). With other labels at that
  level, the label's belief by Dempster's rule over them, `odds / (1 + sum of every label's
  odds)` with a label's odds `s / (1 - s)`: a dissent lowers it.
- *Conflicts, and when they clear.* A dissent is a conflict when it is a call that counts (0.3)
  and the looks for the claim do not outweigh every look against it: their weight of evidence,
  Shafer's `-ln(1 - support)`, which adds over independent looks, at least twice the dissent's
  (two looks as sure as a stray wrong call outweigh it; one against one stays split). The
  claims checked: the AIS identity's kind against what our sensors identify (or, while no
  identification counts, its family against what they recognise, identifications counting for
  their family), the estimate against another label at its level, and the speed the AIS kind
  can make. Before, any dissent that counted held the track in conflict for good: one wrong
  look among the six or seven cameras that looked at an aircraft, and 32 % of neutral contacts
  within 4 km were shown suspect or in conflict (15 (d)). The AIS identity is the contact's own
  claim: it never weighs in the looks' dispute, only against what they settle on, so a spoof
  stays exposed however many cameras see it. Recency is not weighed: association is by contact
  id and a contact's kind does not change, and a wrong call is drawn once per observer, sensor,
  contact and level, so a camera's newer look of it is no new evidence. A contact every look got
  wrong shows what the looks say, never its truth.
- *The level shown.* A track's `identity.level` (`GET /api/tracks`, MCP `get_tracks`, the
  console, the change feed's `identity_level`) says how specific `identity.kind` is: `detect` a
  size class, or from ESM alone the domain; `recognise` a family; `identify` a kind, a sensor's
  identification or, with none, the AIS identity's. The confidence is that level's: a camera's
  detection reads `small_air 0.99` (a sure size class), which without the level looked resolved
  (15 (a)). The legacy model's tracks carry it too (an EO/IR or sonar kind or the AIS kind at
  `identify`, a radar's or ESM's family at `recognise`); their fusion is unchanged.

The picture also says which sensor reached which level: a track's `identity.looks` (`GET
/api/tracks`, the headless picture) lists each observer's sensor's evidence at each level it
reached, `{uid, sensor, level, label, confidence}` (`detect`, `recognise`, `identify`), the
surest it has been; the physical model's only. `TrackFacts` and the identity interface the
decision engine reads are unchanged (no rules version); what moves is their inputs: the
identity confidence where looks disagree, and the affiliation estimate where a conflict clears.

*The smokes* (2026-10-26; model results only): golden-gate-v3 (20 of ours, its traffic in the
air, on the surface and on the ground, the physical model; conditions drawn per situation: day
18 of 20 for F2 and F8), dev seeds 5380-5389, one replicate, approve-all, the paused clock; F8
(decoys and spoofing) and F2 (drone) with `none` and `oracle-v3-honest`, F15 and F16 (the
engagement chain, the review load) with `none` and their scripted reviewer; 120 episodes, the
code before and after. A contact counts in doubt when its track was shown suspect or in an
identity conflict at any second while it was within 4 km of the mission centre.

| | F2 | F8 | F15 | F16 |
|---|---|---|---|---|
| neutral traffic in doubt, before | 56 of 210 (27 %) | 63 of 211 (30 %) | 74 of 241 (31 %) | 73 of 238 (31 %) |
| after | 34 (16 %) | 37 (18 %) | 44 (18 %) | 48 (20 %) |
| its tracked seconds in doubt, before / after | 13.7 / 3.3 % | 18.7 / 4.0 % | 19.3 / 3.8 % | 19.9 / 4.5 % |
| F8's spoofer (suspect) in conflict | | 2 of 2, 99 % of its seconds, both | | |
| flags true / false; rule declarations true / false | 0 / 0; 0 / 0 | 0 / 0; 0 / 0 | 0 / 6; 10 / 0, both | 5 / 4; 7 / 0, both |
| needs served, `oracle-v3-honest` (`none`) | 0 to 1 of 10 (0, 0) | 5 to 6 of 10 (5, 5) | | |

(`none`'s figures; `oracle-v3-honest`'s doubt moved as `none`'s.) Of `none`'s F2 and F8 traffic
in doubt before, 65 came into doubt by two identifications disagreeing and were still in doubt at
their last second within 4 km; after, 5. What remains is mostly not these defects': a charter
fishing boat in the Golden Gate traffic runs at 8.0 m/s, a fishing vessel's plausible top, and
the tracker's speed noise (quoted 8 to 11 m/s) raises the AIS speed conflict (18 of the 71, 11
still at their last second); cargo ships identified before their AIS is heard are suspect until
it is (12, all cleared); a camera's wrong call against a true AIS identity (a ferry
identified as a cargo ship, say) is a conflict until other looks outweigh it (14, 12 cleared).
F8's look-alikes with a cloned AIS identity or a sportfisher's speed stay in conflict, as they
should, and the spoofer stays exposed.

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
| IMO minimum range: the buoy from 15 m, calm (5.4.1) | Pd over 0.8 from 40 m to 1 NM | from 40 m to 1 NM [19] | inside |
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
it); deterministic; per-observer evidence (a closer look corrects a wrong call). After the first
v3 dev check (each fails before it): a buoy hearing a diver 7 % of the seconds does not recognise
it in fifteen minutes, and hearing it every second recognises it after a minute and identifies it
after three; a fixed camera holding a boat where one look identifies it a third of the time
identifies it within three minutes on every one of twenty seeds; a locked camera keeps a boat
closing from 8 km until it identifies it; one camera's failed cue keeps no other camera off the
contact; Sea Hunter's hull sonar identifies a UUV by listening; a track's JSON says which sensor
reached which level under the physical model and is as before under the legacy one. The slant path
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
  only and judged once on the physical model's second air held-out set, written before either change
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
  The new set's one run (`air_calib_physical_held_out_two`): in the 2D radar and the radar
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

**The first v3 dev check's sensing defects** (murmur-1z6.6's dev check, eval/SCORING-V3.md 11.2
and 11.3; 2026-10-24). Four changes, each physics or bookkeeping, no sensor value tuned (W3):

1. *Sonar evidence* (Sonar, above): a classification integrates the signal heard, not the time
   since the first detection. Before, a run survived 30 s gaps and its confidence grew with its
   length, so a buoy hearing a diver 5 to 8 % of the seconds recognised it as fast as one hearing
   it every second.
2. *EO/IR looks and the payload manager* (EO/IR and Pointing, above): each level accrues as a
   hazard while the contact is in the field (before, one draw a look decided all three levels, so
   a look between the identification and recognition chances never identified while the contact
   stayed); a lock holds while its contact is a candidate and its identification gets likelier
   (before, 10 s at most); and each camera keeps its own history (before, `looked` and
   `identified` were keyed by contact alone, so one camera's failed cue kept every camera off the
   contact for 60 s, an identification for 300 s). A first version let any nearer unidentified
   contact take a lock; it thrashed between contacts in traffic and was dropped.
3. *Sea Hunter's hull sonar listens* (the catalog, above): `passive = { ranges }` on an active
   sonar; the MS3 is "active and passive" [44], its passive figures are the buoy's [E here].
4. *Each look's level in the picture* (What a look yields, above): `identity.looks`, physical
   model only; recorded under scoring v3 (`eval/headless/v3/record.py`).

Need served, pooled over both worlds, on fresh dev seeds 5100-5129 (`none` / `random-valid` /
the basic oracle; one replicate, 1,080 episodes each side, no harness error; model results only),
before (the code of murmur-1z6.6) and after:

| family | before | after |
|---|---|---|
| F1 fast boat | 39 / 40 / 43 of 60 | 39 / 40 / 43 |
| F2 drone (version 3) | 0 / 1 / 31 of 60 | 0 / 1 / 30 |
| F3 low aircraft | 33 / 36 / 46 of 60 | 40 / 38 / 41 |
| F4 UUV | 48 / 48 / 48 of 55 | 40 / 40 / 51 |
| F5 diver | 45 / 45 / 48 of 60 | 25 / 25 / 33 |
| F6 shore vehicle | 28 / 22 / 34 of 60 | 20 / 19 / 34 |

The sonar change does what 11.2 asked: the standing buoys no longer recognise the UUV and the
diver for `none` (F4 Golden Gate 26 to 14 of 28, F5 Golden Gate 15 to 0 of 30), and the oracle's
F4 rose to 51 of 55 (10.1 holds on F4 Golden Gate). Under water the oracle's own sonars now need
their seconds of signal too: F5 Golden Gate's oracle fell from 18 to 3 of 30, its vehicles
recognising the diver 30 to 200 s after the deadline (the active sonars' diver ranges are a few
hundred metres, so an orbit hears it a share of the seconds). The camera changes did not raise
the oracle's air and ground service (F2 31 to 30, F3 46 to 41, F6 34 to 34, all within a
binomial spread of about 4 needs). Taken apart on F2 and F3 (seeds 5100-5129, the oracle and
`none`): the hazard alone, with the old lock and shared history, served 14 and 46 of 60 (from 31
and 46): ACQUIRE's rate makes a short look at a middling chance less likely to identify than the
old draw, which gave the full chance at the dwell; the per-camera history brought F2 back to 30,
chiefly its failed cues (shared identifications with per-camera failed cues: 26), as one camera's
failed cue no longer keeps the oracle's other camera off the drone; the lock
change moved 137 of 240 episodes and left the pooled counts as they were. What still limits the drones
(F2's `looked:puma(detected)`): a camera cued by a 2D radar's plot re-aims, slewing and settling,
whenever the next fix moves the elevation search's current bar past a quarter of the field
(`Cue::aims` is rebuilt from each new fix), so a Puma that had the drone in its wide field at its
first bar (F2 San Diego, seed 5108) was blind 11 of the cue's next 18 s and its cue ended,
keeping it off the drone for a minute; a follow-up (cue from the track, or keep the bar a cue
update falls within), not made here (sensing round 3, below, keeps the pattern).

Legacy byte for byte: every legacy calibration and the anchors (`cargo test`), the golden
episode and v1/v2 null fidelity (`eval/headless/tests`), and a keep-out evaluation (seeds 1 to
3, 20 situations, the radar and all variants: records identical). The physical tuning-set views:
the surface criteria all hold as before; on the air tuning sets the all-sensors mix has 2
keep-out misses 3 s ahead as before, other cases (a 500 m loiter by 113 m, an approach and break
by 53 m, against random turns by 253 m before), and one class's velocity error over its bound
(speed change 6.67, against one turn 7.48 before). No held-out set was run.

**Sensing round 3** (bd murmur-qlt and the cued camera's pattern; 2026-10-25). Two changes, physics
and bookkeeping, no sensor value tuned (W3), judged once on held-out sets written before them.

1. *Airborne radars against the sea* (Radar, above; research "Airborne radar against the sea").
   The derived reaches of the MQ-9B's, RQ-4's and CCA's radars against a surface contact were
   0.40 to 0.44 km, their cameras' 88 to 198 km. The cause was a defect before the clutter model:
   the grazing angle was the small-angle `h / g` read as an angle, hundreds of degrees within a
   few kilometres of an aircraft's nadir, whose secant (capped at 20) came and went; on top of it
   a 15 m cell [E here] and the table's 10-degree reflectivity held to the nadir. Now:
   - the grazing angle is the spherical earth's, exact from the horizon to the nadir
     (`radar::grazing`; a mast's small angles are as before to 1e-4);
   - the cell is the radar's range resolution: 1 m for the three (Lynx resolves 0.1 to 3 m over
     its 3 GHz band [34] and publishes no maritime figure: taken as a sea-search waveform's for
     small targets, 0.8 to 0.95 m [74], and the RQ-4 ISS's wide-area search, 1 m [75]); their
     3 degree beams now have a basis (Lynx's aperture within its 19.5 in gimbal [34]; an X-band
     fighter's 0.7 m [E]);
   - sigma0 is the research's table up to 10 degrees (X band, unchanged: the navigation radars'
     worked example and anchors), the NRL model's slope beyond it to 60 degrees and its
     frequency term for other bands [71] (Ku band about 2 to 4.5 dB over X), and the
     quasi-specular return near vertical incidence (Kirchhoff's geometric-optics limit with Cox
     and Munk's slopes [72,73]: about +13 dB straight down at 5 m/s, as a Ku altimeter measures);
   - a radar with a Doppler mode (`doppler`: Lynx's GMTI, DMTI and maritime moving-target
     tracking [34,76], the RQ-4's GMTI [31], a fighter AESA's pulse-Doppler [E]) sees a surface
     contact moving along its line of sight faster than the sea clutter's band, `0.25 + 0.44 U`
     m/s (the mean shift up or down wind and one 3 dB width [72]), against the noise alone.

   What the Lynx class (7,600 m, Ku band) does now against a 10 m^2 contact in sea state 2:
   holding still, lost in the sea's glare within 16.5 km of its nadir (sigma0 rises from -32
   dB at 20 degrees to -12 dB at 60 and +13 straight down) and seen from there to 52.7 km (the
   noise's range); moving 5 m/s along its line of sight, seen from near the nadir to the noise's
   range (`radar::tests`). The research's estimate (a 2.5 m^2 boat at about 40 km, scaled from 300
   km on 10^4 m^2) is the moving case's; holding still, a 2.5 m^2 boat is under the clutter
   everywhere. The navigation radar's IMO anchors are unchanged (11.45, 9.03, 5.00, 4.51, 3.80
   NM; it is X band and its clutter at a mast's grazing angles is the table's), as are its sea
   state 5 worked example, and a new anchor holds: the IMO buoy is seen from 40 m to 1 NM from
   15 m in calm conditions (MSC.192(79) 5.4.1 [19]).

   The reaches (`write_physical_reaches`): the three airborne radars no longer cover a surface
   contact (0.42, 0.44 and 0.40 km before; none now, seen holding still 16.5 to 52.7 km by the
   MQ-9B and 22.6 to 178 by the CCA, nowhere by the RQ-4); their air and ground reaches, and
   every other sensor's, are as before. A radar's reach now starts at its 40 m minimum range: from
   1 m the Sea Hunter's radar would have lost its 9.77 km surface reach to the specular glare
   under its own antenna, where a contact would be inside its hull. The 94 engine scenarios on
   the physical reaches: every result as before (their assessments already named the MQ-9B's
   camera).

2. *A cued camera holds its search pattern* (Pointing, above). Round 2's dev check found a
   camera cued by a 2D radar's plot re-aiming whenever a new fix moved its current bar. The
   mechanism (traced on F2 San Diego seed 5108): the cue is the contact's latest fix, and fixes
   of one drone came from two Sea Hunter radars and other cameras by turns, each another line
   through the drone (a radar's slant-range arc, a camera's position with no height), so the
   bars were rebuilt every second from another line, the bar the camera was on jumped between
   patterns of one to eleven bars, and it slewed and settled at nearly every fix: two Pumas were
   blind 63 and 70 % of the seconds they were cued. The payload manager now keeps the cue's
   pattern and plans it again only when a fix puts the contact outside it (one aim: outside the
   field's middle, as before; a known height: outside the current bar; a fix with no height:
   nowhere in any bar's field); a new plan keeps the bar the search had reached, and a single
   aim's cue (a surface or ground contact's) behaves as before. On the same seed the Pumas were
   blind 36 % of their cued seconds (the slews and settles between bars), and the need was still
   lost: they found the drone in their lowest bar, did not detect it in its 4 s stare, and walked
   up the pattern's empty bars (`looked:puma(detected)`). Test
   (`physical::tests::a_cued_camera_holds_its_search_while_the_cue_stays_within_it`): a camera
   whose cue alternates each second between a radar's plot and a camera's fix of one aircraft
   steps its bars no faster than a stare, each once, to the pattern's end; before, it re-aimed
   every second for the whole run, never ending the cue.

**The held-out sets, run once** (written before the round, commit 6e08444; never tuned on):

- Surface (`calib_physical_held_out`, seeds 371-400, every class in yet other variants): every
  criterion holds. Per class NEES 1.04 to 1.94, the truth within two sigmas 87.2 to 97.2 % when
  updated, and a keep-out 60 and 100 s ahead holds every case at least as often as the old
  filter's (60 s: 72.2 to 100 % against 1.1 to 100 %; 100 s: 69.0 to 100 % against 0.4 to 100 %).
- Air (`air_calib_physical_held_out_four`, seeds 341-370): every class honest in every mix
  (NEES 0.91 to 1.96, within two sigmas 95.5 to 100 %), the steady classes' velocities too
  (normalized error at most 0.41; a velocity given in 0 to 0.5 % of samples), no altitude from
  sensors that measure none, the tracker alone within its bounds (at most 0.49 % and 176 m 3 s
  ahead, 0.20 % and 556 m 10 s, 0.20 % and 1,471 m 30 s), and a keep-out 10, 30, 60 and 100 s
  ahead holds every case; but a keep-out 3 s ahead missed the approach and break (an approach
  at 70 m/s, then a break 150 degrees left at 8 deg/s to 85 m/s, 750 m up; all sensors) 3 times
  in 3,271 samples, by up to 66 m (the tracker before: 98.9 % held). The second held-out set's
  pattern (the constant-velocity models' lag through a hard turn seen by bearings; the follow-up
  is a turn model), not this round's.

The physical tuning-set views (not criteria): the surface criteria all hold; on the air tuning
sets' all-sensors mix, keep-outs 3 s ahead missed a 500 m loiter once (113 m) and random turns at
40 m/s twice (241 m), the speed change class's velocity error is 2.85 (6.67 after round 2) and
the random turns' tracker alone 352 m 3 s ahead (its bound 300).

The dev check after the round (scoring v3's runner; coverage as SCORING-V3.md section 0: both
worlds, F1 to F6, fresh dev seeds 5130-5159, `none` / `random-valid` / the basic oracle, one
replicate, 1,080 episodes, no harness error; 187 situations by day, 122 at night, 21 in twilight,
30 in fog; model results only; records deleted after this summary). Need served, pooled over both
worlds, beside round 2's on its own fresh seeds (5100-5129, after its fixes):

| family | round 2 (5100-5129) | round 3 (5130-5159) |
|---|---|---|
| F1 fast boat | 39 / 40 / 43 of 60 | 35 / 35 / 47 of 60 |
| F2 drone (version 3) | 0 / 1 / 30 of 60 | 1 / 1 / 30 of 60 |
| F3 low aircraft | 40 / 38 / 41 of 60 | 40 / 40 / 48 of 60 |
| F4 UUV | 40 / 40 / 51 of 55 | 39 / 39 / 55 of 58 |
| F5 diver | 25 / 25 / 33 of 60 | 27 / 27 / 35 of 60 |
| F6 shore vehicle | 20 / 19 / 34 of 60 | 24 / 20 / 34 of 60 |

Per world (`none` / `random-valid` / oracle of 30, F4 Golden Gate of 28): F1 16/16/25 and
19/19/22, F2 1/1/14 and 0/0/16, F3 19/20/27 and 21/20/21, F4 11/11/27 and 28/28/28, F5 0/0/6 and
27/27/29, F6 6/8/12 and 18/12/22 (Golden Gate, San Diego). 10.1 holds only for F4 Golden Gate (the
oracle 96 %, `none` 39 %). The oracle's commonest losses: F2 `looked:puma` (11 of 30) and
`looked:seahunter` (7), F5 `looked:seahunter` and `looked:buoy` (12 and 11 of 25), F6 `looked:lta`
(9 of 26) and `no_look` (6). Lesson 3 flags F3 (`looked:corsair`, 55 % of `none`'s Golden Gate
losses and 67 % of the oracle's; `looked:seahunter` 56 % of `none`'s San Diego), F5 (`looked:buoy`,
93 % and 67 % of `none`'s) and F6 San Diego (`looked:corsair` 58 % of `none`'s), much as
section 11.1's did.

The two rounds' seeds differ, so their difference mixes the round's changes with the seeds'
(a binomial spread of about 4 needs). One paired batch separates them for the families the changes
reach: the base commit's physical model and catalog (6e08444) on the same seeds 5130-5159, F1 to
F3, `none` and the oracle (360 episodes, records deleted). Base to round 3, needs served (gained /
lost of the same situations): F1 `none` 31 to 35 (5 / 1), the oracle 47 to 47 (2 / 2); F2 `none`
1 to 1, the oracle 23 to 30 (12 / 5); F3 `none` 36 to 40 (8 / 4), the oracle 40 to 48 (9 / 1). The
air families' rise is the held pattern's (the radar change reaches an air contact only through the
surface tracks its Doppler modes add, which compete for the cameras' cues), F3's the clearest; on
seed 5108 the drone's loss that remains is the elevation search's own (a Puma finds it in its
lowest bar, does not detect it in the bar's 4 s stare, and walks up bars where it is not). The
airborne radars' new surface tracks did not move the oracle's F1.

Legacy byte for byte: every legacy calibration and the anchors (`cargo test`), the golden
episode and v1/v2 null fidelity (`eval/headless/tests`), and a keep-out evaluation (seeds 1 to
3, 20 situations, the radar and all variants: 23,040 records each, identical to the base
commit's).

**Cost** (`eval/headless/sense_perf.py`): a Golden Gate world of 30 stand-ins of ten types and
40 contacts of every domain, headless, 30 simulated minutes: 91 times real time on the
physical model, 82 on the legacy one (this host, 28 cores, one episode).

**Ground tracks under coarse fixes** (murmur-e3b, 10-25). In the F15 dev check (SCORING-V3.md
12.2) a ground track with no velocity blocked two thirds of the nominations. The cause, from the
smoke's records (seeds 5400-5409, the basic oracle): not gaps between fixes but coarse ones. In
2,634 of the 3,105 (attempt, track) pairs the only fixes of the last 10 s were the airborne
radars' (the MQ-9B's every 5 s, 1-sigma 125 to 200 m: 1 degree of a slant range from 7,600 m;
the RQ-4's coarser); 64 % of the pairs were people, the rest cars and a truck, and a track had a
velocity only while a camera (1 Hz, 25 to 40 m) looked at it. Such fixes cannot tell the IMM's
two models apart, so the manoeuvring model's probability stays at its floor (about 0.15), and
ground's manoeuvring noise, 12 m^2/s^3 (about 13 m/s each way between such fixes, some of it
leaking into the steady model through the interaction), held the mixture's velocity 1-sigma at
5 to 6 m/s, over the 3 m/s gate, where the velocity's actual error was about a third of that
(a velocity NEES of 0.2; surface tracks' is 0.2 to 0.3 in their steady classes too, the IMM's
known caution in quiet periods). Ground's constants had been scaled from the single-model
values and never checked against truth, and no calibration set had a ground contact. The fix
calibrates them as the air's were (`tracks::tests::calib_ground`: people standing and walking,
cars parked, driving straight and a winding road, parking and setting off at the contacts
process's 0.5 m/s^2, as world v3's traffic moves; a MALE aircraft's radar from 7,600 m alone,
with a ship offshore, and with a quadcopter's camera): 0.01 and 1 m^2/s^3, the most velocity
with every ground class of the tuning set (seeds 491-520) honest (`ground_tune_grid`). The gate,
the IMM and the other domains are unchanged. A track's own manoeuvre rate learned from the time
it was seen stationary (a gamma prior on the onset rate) was tried on the tuning sets and
dropped: once ground was calibrated it added no velocity and cost honesty on departures.

The held-out sets, written before the change (commit b3e9ecf) and run once after it (588fd57):

- Ground (`ground_calib_held_out`, seeds 461-490): in every mix every velocity class is honest
  (holding, transit, a stop, a car setting off: velocity NEES 0.19 to 1.26) and a person or car
  standing or keeping its course has a velocity 90 to 91 % of the time under the airborne radar
  alone, 97 to 98 % with more sensors (the tracker before, on the tuning set: 0 % and 4 to 38 %);
  keep-outs 60 and 100 s ahead hold every case at least as often as the old filter's. One class
  fails: a car at 11 m/s up a hairpin road is within two sigmas 78.3, 81.0 and 81.3 % of the
  time when updated (NEES 2.57, 2.44, 2.74; the criterion 83 % and 2.6), its velocity, when
  shown, lagging the bends (velocity NEES 14 to 20, a class not asked for one). The tuning set's
  winding road (9 m/s) held at 86 %. A follow-up: a car that keeps turning is beyond the steady
  model, and the manoeuvring model's 1 m^2/s^3 trades it for the velocity of the contacts that
  do not.
- Surface (`calib_physical_held_out_two`, seeds 431-460, with two contacts only the far radar
  fixes, about 100 m every 2.5 s): every criterion holds but one, not this change's (the surface
  models are untouched): a keep-out 60 s ahead held the 50-degree zigzag 75.0 % of the time, the
  old filter's 76.5 %. Per class NEES 0.98 to 1.92, within two sigmas 87.4 to 97.7 %, the
  steady classes' velocities honest (NEES 0.22 to 1.45).
- Air (`air_calib_physical_held_out_five`, seeds 401-430): every criterion holds.

The F15 smoke, before and after (the same 10 situations, the basic oracle's 1,191 attempts):
attempts with a no-velocity ground blocker 791 of 1,186 blocked (66.7 %) before, 35 (3.0 %)
after; attempts with any ground track among the blockers 808 and 578; nominations passed 4
before and after (aircraft block 97 %: the owner's question, SCORING-V3.md 12.4).

**Aircraft velocity under the physical model** (murmur-e3b's air half, 10-26; found, not changed).
In the same F15 smoke 952 of the 1,191 attempts had an aircraft blocker with no velocity (1,649
blocker-attempt pairs: the hobby drone over Baker Beach 796, the tour helicopters 537, the light
aircraft 316), and in F2 (seeds 5370-5379, both worlds, `none`) the drone had a velocity by its
deadline in 8 of 20. The cause is not ground's:

- *Emitting aircraft* (transponders, a drone's datalink) are fixed by ESM, a position each second
  from each receiver that hears them (the Corsair's, 2 degrees: 200 to 370 m; the RQ-4's, 1 degree
  from 18 km up: 570 to 730 m), and by 2D radars' bearings; cameras fixed them a few per cent of
  the seconds (decision W1's looks: on seed 5403 the three aircraft tracked for 13 to 20 minutes
  had 61 to 163 camera fixes each, against 1,541 to 2,264 ESM fixes). From fixes that coarse a
  constant-velocity filter sure the aircraft never manoeuvres settles at a velocity 1-sigma of
  about 2.4 to 2.8 m/s (0.2 m^2/s^3); the IMM's mixing, the manoeuvring model's spread (13 m/s at
  20 m^2/s^3) flowing into the steady model each second at a manoeuvring probability coarse fixes
  cannot lower below 0.1 to 0.2, puts the steady model at 5.2 to 6.5 m/s and the mixture at 7.2 to
  8.9 (seed 5403's tracks, means over their updates). The world tuning set agrees
  (`calib_air::air_velocity_why`, Golden Gate's fleet without its cameras: the steady model alone
  over 3 m/s in 88 to 100 % of seconds, the mixture 5 to 8 m/s). That 1-sigma is honest: the
  mixture's velocity has a mean normalized error of 0.2 in straight flight (cautious) and 2.4 to
  2.8 on routes that turn every minute or so.
- *The hobby drone* emits nothing: 2D radars' bearings alone, so it is unranged (one radar does
  not range an aircraft; no height bound is assumed) and has no velocity by design.
- Fixed by a camera every second, an aircraft has a velocity 63 to 89 % of the time.

So under the 3 m/s gate an ESM-fixed aircraft has no velocity, and no honest tuning of the IMM
gives it one. A larger gate would show velocities at their honest 5 to 12 m/s, but the engine's
velocity region (rules 6) takes a shown velocity as good to the policy's
`track_velocity_sigma_mps` (1 m/s) and its acceleration term; only rules 14's speed bound reads
the tracker's own `velocity_sigma_mps`. On the physical tuning sets with the world mixes
(`air_tune_grid_physical`) an air gate of 6 or 12 m/s took the criteria's failures from 13 to 42
and 81: the tracker alone 3 and 10 s ahead missing 3 to 31 % of samples by up to 1.5 km (its
bounds 3 and 4 %, 300 and 1,200 m), and steady classes' velocity errors over their 2.6 (one turn
5.49 and hover and go 3.48 at 6 m/s, speed change 3.23 at 12). A real air tracker reports every
established track's velocity with its covariance, and what reads the velocity reads the
covariance; here the velocity region would have to read the velocity's own 1-sigma, a
decision-rules version, so the tracker was left as it was. Nor would it unblock F15: rules 14
counts an aircraft with a velocity from its speed, two of its sigmas and 31.5 m/s of unseen
speed-up, speeding up at 4.5 m/s^2 to 100 m/s, so over F15's 57 s span a helicopter at 45 m/s (5
m/s 1-sigma) reaches 5.68 km and a hovering drone 5.32 km, against 5.70 without a velocity. An
experiment, not kept (the air gate at 12 m/s, the same smokes): attempts with a no-velocity
aircraft blocker 952 to 662 (643 of them the radar-only drone), air track samples with a velocity
47.5 to 87.0 %, F2's drones with a velocity by the deadline 8 to 15 of 20; attempts with an
aircraft blocker 1,152 of 1,186 blocked both times (97 %), nominations passed 4 both times, the
aircraft blockers' shortfall still a median 2.6 km.

In Golden Gate's two mixes the tuning sets find the tracker as it is failing these criteria.
Hard turns seen without a camera, by ESM and bearings: keep-outs 3 s ahead missed five cases, by up to 223 m (round three's
break at 10 deg/s 12 times in 8,083 samples, and once 10 s ahead by 41 m; a sightseeing loop's 6
deg/s turns 4 times, by up to 75 m), more than the accepted break lag's 100 m (DECISIONS.md,
10-25), and the tracker alone missed that break 3 s ahead by up to 333 m (its bound 300). And the
hobby drones circling 60 and 100 m at 4 to 6 m/s, fixed by a camera, whose velocity lags the
circle: the tracker alone (the velocity region) missed 3 s ahead in 10.5 % of samples, by up to
44 m, while rules 14's keep-outs held them. The sixth air held-out set (521-550, `calib_air`) was
written for this change and is unspent.

**Aircraft velocity, the change** (murmur-vni, 10-26, the owner's decision A: the decision engine
reads the tracker's velocity uncertainty). Two parts.

- *Decision rules 18* (`VELOCITY_SIGMA_SINCE`, crates/murmur-decide/README.md): the keep-out's
  regions along a track's velocity and its average velocity grow with the 1-sigma the tracker
  reports (`velocity_sigma_mps`), the policy's 1 m/s a floor; a young track's speed bound adds two
  of the same. A shown velocity no longer has to be good to 1 m/s to be safe to give the engine.
  The air calibration's "tracker alone" is now the current rules' regions without rules 14's
  aircraft region (`calib_air::tracker_regions`; rules 13's until then).
- *The air gate*: an aircraft's velocity is also shown with a 1-sigma up to 20 m/s while the IMM's
  two models agree on it, the spread of their velocities about the mixture's (each weighted by its
  probability) below 1 m/s (`model::AIR_VEL_AGREED_SIGMA_MPS`, `AIR_VEL_AGREE_MPS`); the 3 m/s
  gate stands as before, for every domain. Agreement is the honesty condition. Under coarse fixes
  the models agree in steady flight and the mixture's velocity is honest (cautious, a velocity
  NEES of about 0.2); in a turn, a speed-up or a stop the steady model lags and the manoeuvring
  one follows, and a velocity shown then is off by more than its sigma. A plain larger gate showed
  those: on the tuning sets with the world mixes its failures of the criteria were 17, 18, 27, 47,
  47 and 49 at 4, 5, 6, 8, 10 and 12 m/s (the tracker alone a few seconds ahead, in turns), and at
  10 it failed the legacy calibration's asserting sets. Other honesty conditions tried at 8 m/s
  (the reported, consistency-widened sigma under the gate; the manoeuvring model's probability
  under 0.3; the widening under 1.5): 37 to 45 failures. With agreement at 1 m/s, gates of 8, 10,
  12, 15 and 20 gave 18, 18, 18, 16 and 15; agreement at 1.5, 2 and 3 m/s (gate 10) 22, 31 and 39,
  at 0.5 and 0.75 (gate 12) 15 and 17 with less velocity. Ground and surface tracks are unchanged:
  nothing in their calibrations asked for it.

On the tuning sets (101-220, and the world set on 101-130) with every mix of `WORLD_MIXES`: 13
failures before, 15 after. Every keep-out miss is the same, line for line. The two velocities
over the honesty bound before (speed change 2.85 and hover and go 5.76, with every sensor) are
within it; every class flown straight between manoeuvres is honest. The four new failures are the
tracker alone's largest miss in turns seen without a camera: 3 s ahead 329 to 387 m in random
turns and two routes (the bound 300 m), 10 s ahead 1,206 m in the approach and break (1,200; its
3 s miss, failing before at 333 m, now 470 m). That is the accepted limit's kind (DECISIONS.md,
10-25 and 10-26: allow about 250 m within a few seconds where no camera holds an aircraft), and
rules 14's region, in every keep-out of an aircraft, covers it. The share of samples with a
velocity, the class lines' mean: Golden Gate without cameras 0 to 26 % (its steady classes 0 to
33 %), Golden Gate 51 to 65 % (77 to 83 %), every sensor 0 to 32 %, a radar and EO/IR 0 to 4 %; one
2D radar none (unranged, by design).

The sixth air held-out set (521-550, `air_calib_physical_held_out_six`, every mix of
`WORLD_MIXES`), written before the change and run once after it (a78ab5e):

- *Position*: every class and mix honest when updated, NEES 0.82 to 1.96, within two sigmas 88.3
  to 100 %. No altitude from sensors that measure none.
- *Velocity*: every class flown straight between manoeuvres honest, a velocity NEES of 0.21 to
  1.54. Shown in Golden Gate without cameras 31 to 46 % of the steady classes' samples (straight
  45.5, one turn 44.6, hover and go 39.1, speed change 34.6, a drone inbound 31.1), with them 67 to
  86 %, with every sensor 24 to 30 %. The classes that keep turning are reported, not asserted:
  random turns 3.9 and 4.1, a weave without cameras 10.4, a loiter without cameras 5.3.
- *Keep-outs*: five misses. Three a few metres 3 s ahead (a loiter with every sensor, 1 in 4,412
  by 10 m; a tour helicopter's route without cameras, 1 in 8,091 by 13 m; the approach and break
  with every sensor, 2 in 2,572 by 9 m), and the approach and break without cameras, 24 in 8,085
  by up to 185 m 3 s ahead and 1 by 36 m 10 s ahead: the accepted break lag. Keep-outs 30, 60 and
  100 s ahead held every case.
- *The tracker alone*: five failures, all in hard turns: the approach and break with every sensor
  (3 s ahead 1.83 % of samples by up to 456 m; 10 s, 1.36 % by up to 1,209 m) and without cameras
  (3 s, 2.19 % by up to 353 m), a route without cameras (3 s, 2.14 % by up to 365 m), a weave
  without cameras (3 s, 4.71 % of samples, by up to 158 m, over the 3 % share).

For attribution only, after that one run and with nothing changed after it, the same set with the
air gate at 3 m/s (rules 18 kept): the same three small keep-out misses; the break without
cameras 27 times by up to 203 m 3 s ahead and twice by up to 95 m 10 s ahead; the tracker alone
over its bound once (the break, 313 m); two velocities dishonest (hover and go 10.6, speed change
16.2, with every sensor). So the new gate adds the tracker alone's four failures in hard turns,
narrows the break's keep-out misses, and makes those two velocities honest.

The smokes, the same situations before (main's code) and after:

- F15 (golden-gate-v3, seeds 5400-5409, the basic oracle with its loiter): 1,191 nomination
  attempts both times, 4 passed both times, 1,186 blocked by a keep-out and 1,152 of those by an
  aircraft both times. Attempts with an aircraft blocker with a velocity 850 to 946, with one
  without a velocity 952 to 893, with every aircraft blocker given a velocity 200 to 259;
  blocker-attempt pairs counted by rules 14's aircraft reach 1,680 to 2,009, by no velocity 1,649
  to 1,310. As expected: over F15's spans rules 14's reach from a velocity's speed bound is the
  top-speed reach, so a velocity unblocks nothing there; the radar-only hobby drone stays without
  one (unranged).
- F2 (seeds 5370-5379, both worlds, `none`): the drone's track had a velocity at some sample by its
  need's deadline in 20 of 20 (8 before), at the deadline in 11 of 20 (5).

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
- Our vehicles' truth reaches only the headless simulator's bridge: on the live stack nothing
  carries a stand-in's truth to the bridge (its truth log is a file for the evaluation), so a
  live vehicle under a GNSS spoofer or dead reckoning senses from where it says it is and its
  fixes sit on their contacts' truth, as before murmur-k8i; what it reports, and the
  `gps_glitch` flag, are the same live and headless (the stand-ins' own model).
- EO/IR gives a position, as the legacy model (a passive sensor without a rangefinder is angles
  only: the murmur-oho follow-up stands). Its fix error is the legacy model's.
- The payload manager's constants (its elevation band and bar dwell among them), the
  classification times, the misclassification rates per level and the size-class thresholds
  are estimates. The physical radars have no elevation beam: a 2D radar sees an aircraft at
  any elevation, and a camera cued by it searches up to the slant range.
- The tracker's two models are constant-velocity: through a hard turn (a break at 11 m/s^2,
  the tightest loiter at 6) seen only by bearings they lag, and a keep-out 3 s ahead can miss by
  tens of metres (the second air held-out set's approach and break, above). A coordinated-turn
  model (murmur-0b8) was not confirmed on its held-out set and not adopted; the fourth set
  (341-370) found the same lag (3 of 3,271 samples, up to 66 m). Accepted as a known limit by
  the owner (10-25, DECISIONS.md): allow about 100 m beyond the estimate for a keep-out within a
  few seconds of a manoeuvring aircraft seen only by bearings.
- Radar: no land clutter (ground contacts are seen as on water, terrain masking apart), no
  pulse or scan-to-scan integration, sidelobes or SAR (a radar searches with its real beam, so
  a SAR's fine cross-range cell is not used against the sea), a ship's radar heard by ESM every
  second (its sidelobes) rather than once per turn. Sea clutter is HH; above 10 degrees it is
  the research's 10-degree value carried by the NRL model's slope, held beyond 60 degrees under
  the specular return; the Doppler band is taken at its largest (looking up or down wind: the
  look's angle to the wind is not modelled) and a Doppler radar's own motion as compensated; the
  airborne radars' field of regard (Lynx: -5 to -60 degrees of elevation [34]) is not modelled,
  nor any radar's minimum range (the reaches start at the IMO's 40 m).
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
  what the legacy model reads for v1 and v2), and its hull sonar's passive mode has no published
  figures (the buoy's estimates); REMUS's side-scan listens all round, not to its two
  sides; the Saildrone's cameras image every 5 s, looked through every second here; fixed
  cameras (the Magura's, the FPV's, the UGVs' drive cameras) see along the heading only.
- A tick is a second: a look, a scan pass and a report are resolved to the second.
- Identity fusion: a look's confidence is the chance it reached its level, not a measured
  classifier reliability; the two-to-one weight that settles a dispute and the 0.3 of a call that
  counts are choices [E here]; no recency weighting (perfect association). The AIS speed check
  compares the estimate's speed with a kind's plausible top without its uncertainty, so a fishing
  boat at the top (the Golden Gate's charter, 8 m/s) is in conflict now and then.
