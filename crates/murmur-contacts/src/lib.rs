//! Scenario contacts that are not ours (Phase C2): ferries, cargo ships, fishing boats,
//! light aircraft, unknown UAS, a submarine, a suspect fast boat. They are **truth**: the
//! simulator knows where every contact is and what it is. The bridge only ever learns
//! what our own sensors detect (its tracks are the perception), so the two are kept in
//! separate processes and on a separate, labelled channel:
//!
//! - `sim/scenarios/<name>.toml` lists the contacts and their motion ([`scenario`]);
//! - [`motion`] moves them: waypoint routes (looping or back and forth, with dwell at the
//!   ends), transits, loiters, station keeping, and drift with the tidewater current;
//! - [`truth`] is the message shape published to the bridge's simulation channel
//!   (`POST /api/sim/contacts`) and written to the evaluation log (JSON lines).
//!
//! Why its own binary and not a stand-in: stand-ins are *our* vehicles and speak their real
//! protocol to the bridge (docs/AUTHENTICITY.md). Contacts are not ours and have no protocol
//! to the C2 at all; what the C2 knows about them must come through the sensing model. Keeping
//! them out of the stand-in engine keeps that contract clean, and keeps truth out of any
//! process that talks MAVLink. The engine's conditions client and geo helpers are reused as a
//! library.
//!
//! Deterministic: motion jitter comes from a seeded generator per contact, and scenario time
//! runs from `MURMUR_AT` at `MURMUR_RATE`, so a run with the same scenario, seed and clock
//! produces the same truth.

pub mod motion;
pub mod scenario;
pub mod truth;
