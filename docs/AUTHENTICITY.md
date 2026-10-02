# Authenticity

Every platform gets a **stand-in** (light, runs anywhere, thousands at once) and a recorded path to
**maximum authenticity** (heavier: real firmware, real protocol, high-fidelity dynamics). A fleet
chooses per node. Stand-ins drop internal physics, never the interface: what the C2 sees must
behave like the real thing. Last updated 2026-10-02.

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
5. **Real failure modes:** no GPS underwater with navigation drift, running aground, endurance
   running out, link-loss failsafe behavior per platform.
6. **Truth vs reported state.** The stand-in knows where it really is; it reports where its own
   navigation thinks it is (they diverge for a submerged AUV until it surfaces and gets a fix).
7. **Kinematic limits from the catalog:** speed, climb, turn rate (minimum turn radius),
   endurance.

What stand-ins drop: aerodynamics and hydrodynamics detail, autopilot control loops, sensor
simulation beyond detection models.

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

"Today" = how the node runs in the default fleet. Stand-in behaviors still missing from
`sim/standins.py`:
copter (hover), VTOL (vertical takeoff then fixed-wing), airship, ground vehicle (land only),
sailboat (wind-driven), antenna tracker.

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

## Choosing per node

Each platform has a default backend (`sim` in `platforms.toml`); a fleet entry may override it
(`sim = "standin"` or `"sitl"`). Once every platform has a stand-in, any node can run light;
platforms with an ArduPilot model can also run as full firmware.
