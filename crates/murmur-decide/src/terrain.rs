//! Land or water at a point: the input to the domain rule (boats and subs stay on water,
//! UGVs on land).
//!
//! The real implementation will call tidewater's sampling service
//! (`POST /v1/sample {"t", "points": [[lat, lon, null], ...]}`; note lat-first order there):
//! `elevation_m == null` means outside coverage, `water_depth_m > 0` means water at that
//! tide. That needs an HTTP client, so it lives in the bridge, not here.

use serde::{Deserialize, Serialize};

use crate::geo::{LonLat, Polygon};
use crate::time::Time;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum Surface {
    Water { depth_m: f64 },
    Land,
    OutsideCoverage,
}

pub trait TerrainOracle {
    /// One answer per point, in order. `Err` means the oracle as a whole is unavailable,
    /// which the domain rule records as "not evaluated".
    fn sample(&self, t: Time, points: &[LonLat]) -> Result<Vec<Surface>, String>;

    /// One answer for a point that moves every second (an engagement's predicted effect
    /// point): the nearest sample within `within_m`, and where it was taken. `Err` (not
    /// evaluated) when there is none, or when the samples that near disagree (a shoreline).
    /// The default asks for the point itself.
    fn sample_near(&self, t: Time, p: LonLat, within_m: f64) -> Result<NearSample, String> {
        let _ = within_m;
        self.sample(t, &[p])?
            .into_iter()
            .next()
            .map(|surface| NearSample {
                surface,
                at: p,
                distance_m: 0.0,
            })
            .ok_or_else(|| "the terrain oracle gave no answer".into())
    }
}

/// [`TerrainOracle::sample_near`]'s answer.
#[derive(Clone, Debug, PartialEq)]
pub struct NearSample {
    pub surface: Surface,
    /// Where the sample was taken.
    pub at: LonLat,
    /// How far that is from the point asked, metres.
    pub distance_m: f64,
}

impl Surface {
    /// Land, water or outside coverage (a depth difference is not a disagreement).
    pub fn same_kind(&self, other: &Surface) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// No terrain source installed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTerrain;

impl TerrainOracle for NoTerrain {
    fn sample(&self, _t: Time, _points: &[LonLat]) -> Result<Vec<Surface>, String> {
        Err("no terrain oracle installed".into())
    }
}

/// Test double: land inside the given polygons, water (of a fixed depth) elsewhere inside
/// `coverage`, and outside coverage beyond it.
#[derive(Clone, Debug)]
pub struct PolygonTerrain {
    pub coverage: Polygon,
    pub land: Vec<Polygon>,
    pub water_depth_m: f64,
}

impl TerrainOracle for PolygonTerrain {
    fn sample(&self, _t: Time, points: &[LonLat]) -> Result<Vec<Surface>, String> {
        Ok(points
            .iter()
            .map(|p| {
                if !self.coverage.contains(*p) {
                    Surface::OutsideCoverage
                } else if self.land.iter().any(|l| l.contains(*p)) {
                    Surface::Land
                } else {
                    Surface::Water {
                        depth_m: self.water_depth_m,
                    }
                }
            })
            .collect())
    }
}
