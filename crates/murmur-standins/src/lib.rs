//! Stand-in vehicles: lightweight MAVLink-speaking kinematic models, thousands in one process.
//!
//! The Rust port of `sim/standins.py`. Each stand-in has its own UDP link to the bridge and
//! imitates an ArduPilot autopilot at the protocol level (heartbeat with ArduPilot mode numbers
//! for its MAV_TYPE, telemetry, COMMAND_ACKs, the mission upload protocol, AUTO / RTL / hold),
//! so the bridge treats it exactly like SITL or a real vehicle. See the stand-in contract in
//! docs/AUTHENTICITY.md: a stand-in may simplify how a vehicle moves, never how it talks or fails.
//!
//! Layout: `link` (UDP + MAVLink framing), `commands` (inbound protocol), `mission` (store,
//! upload, AUTO walker), `telemetry` (what is sent when), `kinematics` (shared physics steps),
//! `conditions` (tidewater client), `behavior/*` (one file per imitated autopilot).

pub mod behavior;
pub mod commands;
pub mod conditions;
pub mod config;
pub mod geo;
pub mod kinematics;
pub mod link;
pub mod messages;
pub mod mission;
pub mod telemetry;
pub mod vehicle;
