//! The emulator's terrain line of sight from a baked grid instead of `tidewater serve`.
//!
//! `sim/headless/terrain/*.grid.gz` (eval/headless/bake_terrain.py, provenance beside it):
//! gzip of one JSON header line (`lat0`, `lon0` of the first sample, `dlat`, `dlon`, `nlat`,
//! `nlon`, `scale` metres per unit, `nodata`), then `nlat x nlon` little-endian i16 surface
//! heights row by row from the south-west: what tidewater's `/v1/sample` answers the emulator
//! (`max(elevation, water level)`), sampled at the scenario's start. The emulator's own
//! [`murmur_linkemu::terrain::TidewaterTerrain`] cache asks this [`Sampler`] exactly as it asks
//! tidewater over HTTP, and the answer comes at once instead of after the request.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use murmur_linkemu::terrain::{SampleFuture, Sampler};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub lat0: f64,
    pub lon0: f64,
    pub dlat: f64,
    pub dlon: f64,
    pub nlat: usize,
    pub nlon: usize,
    pub scale: f64,
    pub nodata: i16,
}

pub struct Grid {
    pub h: Header,
    data: Vec<i16>,
}

impl Grid {
    pub fn load(path: &Path) -> Result<Grid, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(file)
            .read_to_end(&mut raw)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let nl = raw
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| format!("{}: no header line", path.display()))?;
        let h: Header =
            serde_json::from_slice(&raw[..nl]).map_err(|e| format!("{}: header: {e}", path.display()))?;
        let body = &raw[nl + 1..];
        if body.len() != h.nlat * h.nlon * 2 {
            return Err(format!(
                "{}: {} bytes of samples for a {}x{} grid",
                path.display(),
                body.len(),
                h.nlat,
                h.nlon
            ));
        }
        let data = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect();
        Ok(Grid { h, data })
    }

    fn at(&self, i: usize, j: usize) -> Option<f64> {
        let v = *self.data.get(i * self.h.nlon + j)?;
        (v != self.h.nodata).then(|| f64::from(v) * self.h.scale)
    }

    /// The surface height at a point, bilinear between the four samples around it; None
    /// outside the grid or next to a sample without data (as tidewater outside its DEM).
    pub fn height(&self, lat: f64, lon: f64) -> Option<f64> {
        let y = (lat - self.h.lat0) / self.h.dlat;
        let x = (lon - self.h.lon0) / self.h.dlon;
        if !(y >= 0.0 && x >= 0.0) {
            return None;
        }
        let (i, j) = (y.floor() as usize, x.floor() as usize);
        if i + 1 >= self.h.nlat || j + 1 >= self.h.nlon {
            return None;
        }
        let (fy, fx) = (y - i as f64, x - j as f64);
        let a = self.at(i, j)?;
        let b = self.at(i, j + 1)?;
        let c = self.at(i + 1, j)?;
        let d = self.at(i + 1, j + 1)?;
        Some((a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy)
    }
}

/// The grid as the emulator's terrain sampler.
pub struct BakedSampler(pub Arc<Grid>);

impl Sampler for BakedSampler {
    fn sample<'a>(&'a self, _t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a> {
        let out = points.iter().map(|(lat, lon)| self.0.height(*lat, *lon)).collect();
        Box::pin(async move { Ok(out) })
    }
}
