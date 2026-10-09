//! The headless simulator (docs/HEADLESS.md; evaluation only): one episode of the bridge's own
//! code, the stand-ins, the link emulator and the contacts, in one process on tokio's paused
//! clock, with every socket in memory.
//!
//! The bridge's source modules (all of `crates/murmur-bridge/src/` but `main.rs`) are compiled
//! here by path and unchanged, as `eval/keepout_probe` compiles three of them: what answers an
//! agent is the bridge's own `mcp::tools::call`, its decision layer, its task layer and its
//! tracker. The only differences are the seams behind `cfg(murmur_headless)`, which only this
//! crate's build script sets (`clock::sim_clock`, `Link::in_memory`, `World::attach_link`). Parts
//! of the bridge the simulator does not start (the console push, CoT, the HTTP listener, the
//! heartbeat thread, the water sampler) stay compiled and unused, hence `dead_code` allowed.
//!
//! Each module is `#[cfg(not(test))]`: a test build of this crate (there is none of the library
//! itself, `test = false`, but `cargo clippy --all-targets` asks for one) would otherwise compile
//! the bridge's own unit tests a second time, which refer to its `main.rs`.

#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/api.rs"]
mod api;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/auth.rs"]
mod auth;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/clock.rs"]
mod clock;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/cot.rs"]
mod cot;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/decision/mod.rs"]
mod decision;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/emu.rs"]
mod emu;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/estimate.rs"]
mod estimate;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/geo.rs"]
mod geo;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/heartbeat.rs"]
mod heartbeat;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/link.rs"]
mod link;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/mav.rs"]
mod mav;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/mcp/mod.rs"]
mod mcp;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/platforms.rs"]
mod platforms;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/push.rs"]
mod push;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/situation.rs"]
mod situation;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/space.rs"]
mod space;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/tasks/mod.rs"]
mod tasks;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/telemetry.rs"]
mod telemetry;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/tracks/mod.rs"]
mod tracks;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/vehicle.rs"]
mod vehicle;
#[cfg(not(test))]
#[allow(dead_code)]
#[path = "../../murmur-bridge/src/world.rs"]
mod world;

#[cfg(not(test))]
pub mod sim;
