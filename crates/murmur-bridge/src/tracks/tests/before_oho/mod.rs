//! The sensing model and tracker as they were before the air calibration (murmur-oho), for
//! its comparison only (`calib_air`): `model.rs` and `sensing.rs` are verbatim copies of
//! `tracks/model.rs` and `tracks/sensing.rs` at efd332f (a 2D radar's fix of an aircraft
//! centred on its true ground position; the air constants scaled from the surface ones). Both
//! run on the same truth and draws as today's.

pub mod model;
pub mod sensing;
pub use crate::tracks::truth;
