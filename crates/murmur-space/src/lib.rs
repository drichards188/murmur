//! Murmur's space segment: satellites as taskable infrastructure, not flown nodes.
//!
//! The operator asks for a capability over an area and a time window (imagery or SAR, SATCOM
//! coverage, GNSS assurance); this crate computes what is feasible from real public orbits and
//! approximate sensor constraints, with timing, and why the rest is not. See README.md for
//! the integration contract and the epoch (time shift) approach.

pub mod access;
pub mod area;
pub mod cache;
pub mod catalog;
pub mod geo;
pub mod gnss;
pub mod orbit;
pub mod scenario_clock;
pub mod space;
pub mod tasking;
pub mod timemap;

pub use area::{Area, NamedArea};
pub use catalog::{Capability, Catalog, SatClass};
pub use geo::Geodetic;
pub use orbit::{Satellite, Sgp4Mode};
pub use space::{
    AccessQuery, AccessReport, GnssQuery, GnssReport, LinkQuery, LinkReport, Space, SpaceError,
};
pub use tasking::{CloudModel, NoCloudModel, Rank, TaskRequest, TaskingResult};
pub use timemap::{EpochMode, TimeMap};
