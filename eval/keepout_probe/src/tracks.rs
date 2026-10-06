//! The bridge's own sensing model and tracker, compiled here from the bridge's files by path
//! and unchanged (read-only): `crates/murmur-bridge/src/tracks/{model,sensing,truth}.rs`. Their
//! only references outside these three files are `crate::clock::round` and
//! `crate::geo::haversine_m`, which `main.rs` provides (the second by including the bridge's
//! `geo.rs`). Parts the probe does not use (the console's JSON, identity, acts) stay compiled
//! and unused.

#![allow(dead_code)]

#[path = "../../../crates/murmur-bridge/src/tracks/model.rs"]
pub mod model;
#[path = "../../../crates/murmur-bridge/src/tracks/sensing.rs"]
pub mod sensing;
#[path = "../../../crates/murmur-bridge/src/tracks/truth.rs"]
pub mod truth;
