# Authenticity

Every platform gets a **stand-in** (light, runs anywhere, thousands at once) and a recorded path to
**maximum authenticity** (heavier: real firmware, real protocol, high-fidelity dynamics). A fleet
chooses per node. Stand-ins drop internal physics, never the interface: what the C2 sees must
behave like the real thing. Last updated 2026-10-04.

## The stand-in contract

A stand-in may simplify how a vehicle moves. It may not simplify how the vehicle talks or fails.

1. **Speaks its real-world protocol.** MAVLink today; STANAG 4586, UMAA over DDS or JAUS as
   those adapters land (see the protocol groups below). Never a private shortcut to the bridge.
2. **Respects the protocol's real limits.** Example found in testing: a MAVLink system ID is one
   byte, so a MAVLink network holds at most 255 systems. Stand-ins must not paper over this; the
   bridge must key vehicles by (link, system ID), as real deployments do.
3. **Imitates its autopilot's semantics exactly:** mode numbers, mission upload protocol,
   command acknowledgements, what it refuses (a drifting buoy rejects missions; a fixed-wing
   can't hover or disarm in flight).
4. **Real message rates and sizes**, because the link emulator (Phase D) charges for bandwidth.
5. **Real failure modes:** no GPS underwater with navigation drift, GNSS jammed or spoofed,
   running aground, endurance running out, link-loss and navigation-loss failsafe behavior
   per platform.
6. **Truth vs reported state.** The stand-in knows where it really is; it reports where its own
   navigation thinks it is (they diverge for a submerged AUV until it surfaces and gets a fix).
7. **Kinematic limits from the catalog:** speed, climb, turn rate (minimum turn radius),
   endurance.

What stand-ins drop: aerodynamics and hydrodynamics detail, autopilot control loops, sensor
simulation beyond detection models.

### How the Rust engine meets it (`crates/murmur-standins`)

Shared machinery, keyed by the ArduPilot firmware a behavior imitates (from its mode table),
so a new behavior inherits it:

- **Parameters** (`params.rs`): ArduPilot 4.8 names and types (`WP_RADIUS_M`, `RTL_ALTITUDE`,
  `LOIT_MAX_VELX`; never the pre-4.8 `WPNAV_*`, `ALT_HOLD_RTL`), seeded from ArduPilot
  defaults, Murmur's `sim/params/*.parm` and the catalog; PARAM_SET / READ / LIST as
  GCS_Param.cpp; the version banner and AUTOPILOT_VERSION say 4.8.0-dev, as the SITL build.
  Only the parameters a C2 touches, not all ~1,000.
- **Rates** (`telemetry.rs`): heartbeat only until the GCS sets intervals, as ArduPilot with
  `common.parm`. An acoustic-linked vehicle under water sends HEARTBEAT + HIGH_LATENCY2 every
  20 s instead of streaming. What the GCS sets depends on the link (the bridge's
  `crates/murmur-bridge/src/telemetry.rs`): on Iridium-class links it asks for HIGH_LATENCY2
  every 5 s, which ArduPilot and the stand-ins stream like any other message.
- **Commands and refusals** (`commands.rs`): COMMAND_LONG and COMMAND_INT through one table
  that knows which commands each firmware handles (Blimp has no MISSION_START, RTL or
  DO_CHANGE_SPEED and ACKs NAV_TAKEOFF ACCEPTED), mode changes that fail with the firmware's
  words ("No such mode N"; ArduRover's "Flight mode change failed"; ArduSub's "Flight mode
  change failed Auto"; ArduPlane's "Q_ENABLE 0" for a Q mode on a plain plane), and arming
  refusals a behavior supplies (ArduRover: "Arm: Mode not armable" in RTL, SMART_RTL,
  INITIALISING; ArduPlane: "Arm: RTL mode not armable"). A user takeoff command is the
  QuadPlane's: a fixed wing fails it, a sub and a rover answer UNSUPPORTED.
- **Missions** (`mission.rs`): upload and download for mission, fence and rally (reported as
  AP_Mission stores them: commands without a location in the global frame, NAV_LAND's
  param4 as a yaw direction); "Mission: N Type" as each command starts (navigation and DO
  alike, again when AUTO is re-entered), DO_ items run, never flown, with the firmware's own
  texts (Plane "Set airspeed N m/s", Rover "speed: N.N m/s"); a DO item that completes right
  after a waypoint takes the waypoint's MISSION_ITEM_REACHED (GCS_MAVLINK keeps one deferred
  index, seen in SITL); DO_JUMPs say "Mission: 5 Jump 1/1" on every run; loiter, delay, RTL
  and land items per firmware; each firmware's mission end. A new mission sits at item 0
  until AUTO starts, and a finished one keeps its last item as the current one, as
  AP_Mission reports. The texts come from one helper (`behavior/apmission.rs`).
- **ArduRover's modes** (`behavior/rover.rs`, the ground vehicle, the sailboat and the
  motorboat alike): "Reached destination" when an RTL arrives, not when it is entered
  already within WP_RADIUS of home (the motorboat SITL says nothing then); LOITER holding
  within LOIT_RADIUS; MIS_DONE_BEHAVE at the mission's end; the mission reset on a disarm
  outside AUTO.
- **Fixed wing on the ground and at its ends** (`behavior/plane.rs`, checked against the
  plane SITL): the throttle is suppressed on the ground outside an auto takeoff (armed in
  FBWA, it does not move); AP_Mission runs disarmed too (an empty mission in AUTO ends in
  RTL at once); the auto takeoff's texts ("Armed AUTO", "Triggered AUTO", "Holding course",
  the level-off, "Takeoff complete at Nm"); the slope landing's ("Landing approach start",
  "Landing glide slope", "Flare"), after which the plane stays armed with the LAND item
  current until LAND_DISARMDELAY after it last believed it flew ("Throttle disarmed", "Auto
  disarmed", "Distance from LAND point"); leaving AUTO then resets the mission ("In landing
  sequence: mission reset") or restarts the landing sequence at the previous waypoint.
  MISSION_ITEM_REACHED 0 on entering GUIDED. Home follows a disarmed plane or rover
  (HOME_POSITION when it moves), so it is where the vehicle arms.
- **Failures**: battery drained by load over the catalog endurance, battery and GCS-loss
  failsafes per firmware with ArduPilot's texts (`battery.rs`, `failsafe.rs`): ArduPlane's
  "GCS Failsafe On: switched to RTL", armed or not, never during a landing; ArduRover's,
  announced as "Radio Failsafe" (its 50 Hz radio check wins the race to announce a failsafe
  the 10 Hz GCS check raised, as the motorboat SITL shows; "GCS Failsafe Cleared" when the
  GCS is back); ArduSub's "MYGCS: heartbeat lost" then the action (disarm for a tethered
  ROV). The GCS-loss
  (lost-link) timer follows the link: `lost_link_s` per profile in `sim/links.toml` (5 s LOS,
  2 minutes GEO SATCOM, sources and guesses marked there), which `sim/swarm.py` writes into
  stand-ins and SITL vehicles alike as FS_LONG_TIMEOUT (Plane) or FS_GCS_TIMEOUT. An engine
  stall longer than 1 s (the host swapping) is skipped, not caught up, so vehicles don't run
  their failsafe clocks past heartbeats still queued in their sockets (`vehicle.rs`).
- **Truth vs reported** (`nav.rs`): without GPS the estimate drifts (INS heading and DVL scale
  errors, unseen current beyond DVL range; figures are estimates in the 0.1-0.5 % of
  distance class), EKF flags drop to relative position, and a fix after surfacing resets it.
  The truth goes only to `MURMUR_STANDINS_TRUTH` (JSON lines), never on the wire.
- **GNSS** (`gnss.rs`, `nav.rs`, `failsafe.rs`): satellites in view and HDOP/VDOP from the
  real GPS constellation (murmur-space's cached element sets, a 0.1 degree grid over the
  fleet, 30 s steps, computed ahead of the scenario clock in the background; without the
  cache, 14 satellites and HDOP 1.0 as before). GPS_RAW_INT carries the satellites used,
  HDOP/VDOP x100 (as AP_GPS) and h_acc/v_acc = DOP x a UERE that grows as C/N0 drops
  (estimates: 3 m plus code-tracking noise). Jammers and spoofers are `[[event]]`s with a
  `gnss` table (the link emulator's event file; example `sim/events/golden-gate-gnss.toml`):
  J/S from EIRP, free-space loss at L1, radio horizon and an upward antenna's pattern;
  satellites drop below a tracking threshold (28 dB-Hz) and come back only above an
  acquisition one (33 dB-Hz). EKF3 as NavEKF3_core runs it: flags still absolute for
  posRetryTime (7 s, 10 s with velocity aiding), then relative only for a plane (airspeed)
  or a sub (DVL), nothing for a copter, rover, boat or blimp, and "EKF3 IMU0 stopped
  aiding" (constant position mode) at 15 s. EKF failsafes per ekf_check.cpp: Copter "EKF
  variance: position lost", LAND; Rover "EKF variance", HOLD, "EKF failsafe"; Blimp LAND;
  a fixed-wing flies on. Spoofing walks the reported position off the truth with every flag
  good; when it ends the true fix fails the innovation gate (gps_glitching, Copter's "GPS
  Glitch or Compass error") for 10 s, then the EKF resets. Not modelled: terrain, building
  and bridge masking (a uniform elevation mask, `MURMUR_GNSS_MASK_DEG`, stands in), GNSS
  other than GPS, altitude in the geometry, multipath, position noise under degraded
  accuracy, the quadplane's Q-mode EKF failsafe, second EKF lanes.

Murmur-chosen settings where ArduPilot ships a feature off (GCS failsafes, battery
thresholds) are marked in `params.rs`.

### How the Rust bridge reads it (`crates/murmur-bridge`)

- **HIGH_LATENCY2** updates position, depth, heading, speed, battery, mode and waypoint, so a
  submerged AUV's track keeps moving. A link reporting by it is low-rate: it counts as lost
  after 3 expected intervals (60 s at 20 s) instead of 3 s (`link.expected_interval_ms`).
- **Dead reckoning**: a sub or a fixed-wing (plane, VTOL) with relative position only
  (EKF_POS_HORIZ_REL without ABS, or HIGH_LATENCY2's GPS failure flag) is navigation-ready
  with `nav_source = dead_reckoning`, shown as DR in the console. The readiness rule passes
  a sub and makes a fixed-wing `unverified` ("position error grows with time"): an agent's
  task waits for a human, the operator's own goes through flagged, so a plane in a GNSS
  jammer can still be tasked out of it. Copters, rovers, boats and blimps need GNSS (their
  EKF gives up the position, as ArduPilot's does).
- **Hold** commands the mode that keeps a vehicle where it is with no RC transmitter and no
  further messages (`tasks/caps.rs` has the reasoning per kind): BRAKE for a copter (LOITER
  and POSHOLD read the throttle stick, so with no RC they descend, land and disarm), LOITER
  for a boat (HOLD only stops the motors, so a current carries it off), a plane, a VTOL and
  a blimp, HOLD for a ground vehicle, POSHOLD for a sub, STOP for a tracker.
- **Fixed-wing RTL** is done within its RTL circle plus 250 m: RTL_RADIUS (or WP_LOITER_RAD
  when 0), read at link-up, never tighter than the catalog's minimum turn radius.

## Protocol groups (most realistic way a C2 reaches each platform)

| Group | Platforms | Openly implementable? |
|---|---|---|
| MAVLink | ROV, small airship, antenna tracker; many Ukrainian-built UGVs and drones | Yes (built) |
| STANAG 4586 | MQ-9B-class (GA-ASI's GCS is 4586-based), RQ-4-class (interop path), tactical VTOL (plausible) | Yes: public NATO spec, thin open tooling |
| A-GRA / UCI / OMS | CCA | No: controlled distribution. Stays on the generic intent API, documented as a gap |
| JAUS (Army UGV IOP) | US program-of-record UGVs | Feasible: SAE documents are paid; open toolkits exist |
| UMAA over DDS | Sea Hunter-class, REMUS-class, Orca-class (Navy programs) | Probably: public architecture description v1.1, open DDS implementations. Verify how much of the data model is public |
| JANUS (STANAG 4748) | Underwater acoustic link for AUVs and XLUUVs | Yes: open spec, C reference implementation |
| Proprietary | Magura V5, Saildrone, Skydio X10, Switchblade, Puma, Corsair (if not Navy) | No: MAVLink in the sim, labelled as such |
| Pilot RC link | FPV (ExpressLRS-style or fiber), no C2 protocol | n/a: C2 sees it only through a relay |
| Telemetry only | Drifting buoy (Iridium-style short messages) | Yes (no commands exist) |

Video metadata for all ISR aircraft: MISB ST 0601 / STANAG 4609 (open, well supported).

## Per platform

"Today" = how the node runs in the default fleet. The Rust engine (`crates/murmur-standins`)
has a behavior for each of these, named as `behavior` in `sim/platforms.toml`:

| Behavior | Imitates | Checked by |
|---|---|---|
| `plane` | ArduPlane | `sim/tests/standins_vs_sitl_plane_boat_sub.py` (puma vs `arduplane --model plane`: parameters read and set, refused modes and commands, arming, auto takeoff, a mission with DO items and a loiter, LOITER as hold, the GCS-loss failsafe with the LOS timer, RTL, an auto-land and the disarm after it, download, clear); tasking through the bridge `sim/tests/standins_e2e_plane_boat_sub.py` (puma, switchblade) |
| `boat` | ArduRover, boat frame | `sim/tests/standins_vs_sitl_plane_boat_sub.py` (magura vs `ardurover --model motorboat`: the same ground, with a hold-time waypoint and a DO_JUMP, LOITER and HOLD, "Reached destination", the GCS-loss failsafe with the Starlink timer, armed and disarmed); tasking `sim/tests/standins_e2e_plane_boat_sub.py` (magura, corsair) |
| `drifter` | no autopilot: an Iridium-style buoy | `sim/tests/standins_drifter.py` against the catalog: surface-boat identity, nothing streamed unasked, the bridge's Iridium plan (HIGH_LATENCY2 every 5 s, tens of bytes a second), missions and powered modes refused, the Iridium lost-link timer, no movement without conditions, drift with tidewater's current (`--tidewater`) |
| `sub` | ArduSub | `sim/tests/standins_vs_sitl_plane_boat_sub.py` (bluerov vs `ardusub --model vectored`: parameters, refusals, a mission at depth, POSHOLD and GUIDED, SURFACE ending in ALT_HOLD, the GCS-loss disarm with the tether timer, download, clear); tasking `sim/tests/standins_e2e_plane_boat_sub.py` (bluerov) |
| `copter` | ArduCopter | `sim/tests/standins_vs_sitl_air.py` |
| `vtol` | ArduPlane QuadPlane | `sim/tests/standins_vs_sitl_air.py` |
| `blimp` | ArduPilot Blimp | `sim/tests/standins_sitl_blimp_ugv_sailboat.py` |
| `ugv` | ArduRover, skid-steer | `sim/tests/standins_sitl_blimp_ugv_sailboat.py` |
| `sailboat` | ArduRover, sailboat frame | `sim/tests/standins_sitl_blimp_ugv_sailboat.py` |
| `tracker` | ArduPilot AntennaTracker | `sim/tests/standins_vs_sitl_tracker.py` |

Every platform in the catalog can run as a stand-in with `sim = "standin"`.

Differences the comparisons report as notes, by design (the scripts list them under `KNOWN`):

- `RTL_ALTITUDE` is -1 on every plane stand-in (return at the current altitude), Murmur's
  choice so a HALE/MALE aircraft does not descend to ArduPlane's 100 m default; the puma SITL
  flies the default.
- The parameter count: a stand-in holds the parameters a C2 touches, SITL about 1,000.
- A sub stand-in has no GPS under water and dead reckons (contract item 5); ArduSub SITL
  keeps its fix at depth, so GPS_RAW_INT and EKF_STATUS_REPORT differ there (compared at
  the surface, where they agree).
- ArduRover's failsafe text race ("Radio Failsafe" vs "GCS Failsafe") is a race in the
  firmware; the stand-in says what the SITL said every time it was watched.
- The plane's disarm after an auto-land is timed from the flare as SITL did it (about 15 s
  with LAND_DISARMDELAY 20), not from an is_flying model.

### Air

| Platform | Today | Stand-in target | Maximum authenticity would take | Blockers |
|---|---|---|---|---|
| RQ-4 Global Hawk-class | Stand-in (plane) | STANAG 4586 VSM, SATCOM link profile, MISB 0601 metadata | JSBSim flight model of a HALE airframe + 4586 interface + SATCOM latency and bandwidth | Real ground segment is the manufacturer's own; no official RQ-4 JSBSim model |
| MQ-9B SkyGuardian-class | Stand-in (plane) | STANAG 4586 VSM, SATCOM, MISB 0601 | JSBSim MALE airframe + 4586 (matches GA-ASI's 4586-based GCS) | Proprietary flight software |
| CCA (YFQ-42A/44A-class) | Stand-in (plane) | Generic intent API (no open standard) | A-GRA/UCI interface + JSBSim jet model | A-GRA/UCI not public: unattainable openly |
| FVR-90-class VTOL | SITL quadplane | VTOL behavior: vertical takeoff, transition, cruise | ArduPilot QuadPlane SITL tuned to the airframe (mass, speeds) + 4586 if Army/NATO | Real GCS proprietary |
| Puma 3 AE-class | SITL plane | Plane behavior (exists), MISB 0601 from the gimbal | ArduPilot SITL tuned; AV's DDL link profile | AV ground station and datalink proprietary |
| Switchblade 600-class | SITL plane | Plane behavior; engagement abstract (Phase E) | ArduPilot SITL tuned to loiter and dash speeds | AV fire-control system proprietary |
| Skydio X10-class | SITL copter | Copter behavior (hover, waypoints) | ArduPilot or PX4 SITL + Gazebo for vision-based obstacle avoidance; CoT to TAK | Skydio's autonomy stack proprietary |
| FPV strike quad | SITL copter | Copter behavior, short endurance, relay-dependent link | Betaflight SITL + RC link model (ExpressLRS-style, fiber); realistically human-piloted | No C2 protocol exists; C2 visibility only via relays |
| Small airship (LTA) | SITL Blimp | Airship behavior (slow, wind-sensitive) | ArduPilot Blimp SITL (already authentic for small airships) | None |

### Ground

| Platform | Today | Stand-in target | Maximum authenticity would take | Blockers |
|---|---|---|---|---|
| Tracked logistics UGV | SITL rover (skid steer) | Ground behavior: land only, slope limits, road following | JAUS/IOP interface + Gazebo terrain and obstacles; or MAVLink + Starlink link profile for Ukrainian-style builds | SAE JAUS documents are paid |
| Armed UGV | SITL rover | Same; armed flag (Phase C policy) | Same as logistics | Same |
| Mine-laying UGV | SITL rover | Same | Same | Same |
| Small recon UGV | SITL rover | Same | Same | Same |
| Antenna tracker | SITL AntennaTracker | Pointing behavior (pan/tilt rates and limits) | ArduPilot AntennaTracker SITL (already authentic; open stall bug murmur-9bf) | None |

### Surface

| Platform | Today | Stand-in target | Maximum authenticity would take | Blockers |
|---|---|---|---|---|
| Magura V5-class | SITL motorboat | Fast boat behavior, Starlink link profile | Planing-hull dynamics (Gazebo/VRX) + satellite link model | Control software proprietary |
| Corsair-class | SITL motorboat | Boat behavior; UMAA if Navy-procured | UMAA over DDS + VRX hydrodynamics | Proprietary unless Navy UMAA |
| Sea Hunter-class MUSV | Stand-in (boat) | UMAA over DDS, SATCOM | UMAA + a ship maneuvering model (e.g. 3-DOF MMG) + SATCOM | How much of UMAA's data model is public |
| Saildrone-class | SITL sailboat (tidewater wind) | Sailing behavior from a polar diagram, no-go upwind zone | ArduPilot sailboat SITL with real wind (already close) + Iridium/Starlink link | Vendor cloud API proprietary |
| Drifting buoy | Stand-in (drifter, tidewater current) | Add windage and Iridium-style short messages | Drift = current + windage + waves, Iridium short-burst message format | None significant |

### Subsurface

| Platform | Today | Stand-in target | Maximum authenticity would take | Blockers |
|---|---|---|---|---|
| BlueROV2-class ROV | SITL ArduSub (patched seabed and currents) | ROV behavior, tether length limit | ArduSub SITL (already authentic) + Gazebo for camera and sonar | None |
| REMUS 600-class AUV | Stand-in (sub) | **Navigation drift underwater** (INS/DVL error grows, fix on surfacing), JANUS acoustic messages, Iridium when surfaced, UMAA | UMAA over DDS + JANUS acoustic link + INS/DVL error model + ocean currents at depth (tidewater) | UMAA data-model availability |
| Orca XLUUV-class | Stand-in (sub) | Same as REMUS, long endurance | Same | Same |

## In the catalog and the console

`sim/platforms.toml` carries this record per platform: `protocol` (the real-world C2 protocol
from the groups table above), `protocol_note`, `authenticity` (what maximum authenticity would
take) and `authenticity_blocker` (omitted when none). `/api/platforms` passes them through, and
the Platforms panel shows the protocol as a badge (the note is its tooltip) with the authenticity
line and blocker in an expandable row, next to whether the node runs as ArduPilot SITL or a
stand-in. Edit the catalog and this document together.

## Choosing per node

Each platform has a default backend (`sim` in `platforms.toml`); a fleet entry may override it
(`sim = "standin"` or `"sitl"`). Once every platform has a stand-in, any node can run light;
platforms with an ArduPilot model can also run as full firmware.
