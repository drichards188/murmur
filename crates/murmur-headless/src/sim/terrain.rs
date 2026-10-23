//! The emulator's terrain line of sight from a baked grid instead of `tidewater serve`.
//!
//! The world's surface grid (`[terrain] surface` in sim/worlds/<name>.toml, baked by
//! eval/headless/bake_world.py, provenance beside it): what tidewater's `/v1/sample` answers the
//! emulator (`max(elevation, water level)`), sampled at the scenario's start. It is read with
//! the bridge's own [`Grid`] (crates/murmur-bridge/src/decision/terrain.rs, which reads the
//! world's land/water grid for the domain rule the same way). The emulator's own
//! [`murmur_linkemu::terrain::TidewaterTerrain`] cache asks this [`Sampler`] exactly as it asks
//! tidewater over HTTP, and the answer comes at once instead of after the request.

use std::sync::Arc;

use murmur_linkemu::terrain::{SampleFuture, Sampler};

pub use crate::decision::terrain::{Grid, GridHeader};

/// The grid as the emulator's terrain sampler.
pub struct BakedSampler(pub Arc<Grid>);

impl Sampler for BakedSampler {
    fn sample<'a>(&'a self, _t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a> {
        let out = points.iter().map(|(lat, lon)| self.0.height(*lat, *lon)).collect();
        Box::pin(async move { Ok(out) })
    }
}
