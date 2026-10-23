//! Geometry every sensor of the physical model shares (WORLD-V3-PLAN 2.2): the horizon for an
//! effective earth radius (optical k = 7/6 for EO/IR, radio k = 4/3 for radar, AIS and ESM,
//! research [1]), hull-down (only the part of a target above the sensor's horizon counts), and
//! terrain line of sight from a baked surface grid, with the earth bulge the link emulator
//! uses (`murmur_linkemu::terrain::path_obstruction_m`: `d1 d2 / (2 k R)`).
//!
//! The grid is the format of `sim/headless/terrain/*.grid.gz` (`eval/headless/bake_terrain.py`):
//! gzip of one JSON header line (`lat0`, `lon0` of the first sample, `dlat`, `dlon`, `nlat`,
//! `nlon`, `scale` metres per unit, `nodata`), then `nlat x nlon` little-endian i16 surface
//! heights row by row from the south-west, the higher of the ground and the water.

use std::io::Read;
use std::path::Path;

use serde::Deserialize;

pub const EARTH_R_M: f64 = 6_371_000.0;
/// Effective earth radius factors (research [1], gdal_viewshed's curvature table).
pub const K_OPTICAL: f64 = 7.0 / 6.0;
pub const K_RADIO: f64 = 4.0 / 3.0;
/// Metres per degree of latitude (the flat approximation the sensing model uses throughout).
pub const M_PER_DEG: f64 = 111_320.0;

/// The distance to the horizon from `h` metres above the surface, for effective radius factor
/// `k` (`sqrt(2 k R h)`: 4.12 km x sqrt(h) at k = 4/3, 3.86 at 7/6).
pub fn horizon_m(h: f64, k: f64) -> f64 {
    (2.0 * k * EARTH_R_M * h.max(0.0)).sqrt()
}

/// How much of a target stands above a sensor's horizon (hull-down, research 0): its top at
/// `top_msl`, `ground_m` away from a sensor at `h_s` metres above the sea, the earth's drop
/// beyond the sensor's own horizon, `(R - d_obs)^2 / (2 k R)`, taken off. Metres above the drop
/// line; at most the target's own `height_m`; negative: wholly below the horizon.
pub fn visible_height_m(top_msl: f64, height_m: f64, ground_m: f64, h_s: f64, k: f64) -> f64 {
    let d_obs = horizon_m(h_s, k);
    let drop = if ground_m > d_obs { (ground_m - d_obs).powi(2) / (2.0 * k * EARTH_R_M) } else { 0.0 };
    (top_msl - drop).min(height_m.max(0.0))
}

/// East and north metres from (`lat0`, `lon0`) to (`lat`, `lon`), flat.
pub fn en(lat0: f64, lon0: f64, lat: f64, lon: f64) -> (f64, f64) {
    ((lon - lon0) * M_PER_DEG * lat0.to_radians().cos(), (lat - lat0) * M_PER_DEG)
}

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

/// A baked surface-height grid.
pub struct Grid {
    pub h: Header,
    data: Vec<i16>,
}

/// The most samples one line of sight reads (a 12 km path at the Golden Gate grid's 20 m).
const MAX_SAMPLES: usize = 600;

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
        let h: Header = serde_json::from_slice(&raw[..nl]).map_err(|e| format!("{}: header: {e}", path.display()))?;
        let body = &raw[nl + 1..];
        if body.len() != h.nlat * h.nlon * 2 || h.nlat < 2 || h.nlon < 2 {
            return Err(format!("{}: {} bytes of samples for a {}x{} grid", path.display(), body.len(), h.nlat, h.nlon));
        }
        let data = body.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
        Ok(Grid { h, data })
    }

    /// A grid from samples (tests): heights in metres, row by row from the south-west.
    #[cfg(test)]
    pub fn from_heights(h: Header, heights: &[f64]) -> Grid {
        let data = heights.iter().map(|v| (v / h.scale).round() as i16).collect();
        Grid { h, data }
    }

    fn at(&self, i: usize, j: usize) -> Option<f64> {
        let v = *self.data.get(i * self.h.nlon + j)?;
        (v != self.h.nodata).then(|| f64::from(v) * self.h.scale)
    }

    /// The surface height at a point, bilinear between the four samples around it; None
    /// outside the grid or next to a sample without data.
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

    /// The grid's sample spacing, m (its finer direction).
    fn spacing_m(&self) -> f64 {
        let lat = self.h.lat0 + 0.5 * self.h.dlat * self.h.nlat as f64;
        (self.h.dlat * M_PER_DEG).min(self.h.dlon * M_PER_DEG * lat.to_radians().cos()).max(1.0)
    }

    /// How far the terrain sticks up above the straight ray from `a` to `b` (lat, lon, metres
    /// above the sea), the most over the path's interior samples, with the earth bulge for
    /// each of `ks`: positive, blocked by that much. Samples outside the grid count as open.
    /// One pass reads the surface once for every k.
    pub fn obstruction_m<const N: usize>(&self, a: (f64, f64, f64), b: (f64, f64, f64), ks: [f64; N]) -> [f64; N] {
        let (de, dn) = en(a.0, a.1, b.0, b.1);
        let total = de.hypot(dn);
        let mut worst = [f64::NEG_INFINITY; N];
        let n = ((total / self.spacing_m()).ceil() as usize).clamp(2, MAX_SAMPLES);
        for i in 1..n {
            let f = i as f64 / n as f64;
            let Some(g) = self.height(a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f) else {
                continue;
            };
            let (d1, d2) = (f * total, (1.0 - f) * total);
            let ray = a.2 + (b.2 - a.2) * f;
            for (w, k) in worst.iter_mut().zip(ks) {
                *w = w.max(g + d1 * d2 / (2.0 * k * EARTH_R_M) - ray);
            }
        }
        worst.map(|w| w.max(-10_000.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizons_follow_the_research_constants() {
        // 4.12 km x sqrt(h) radio, 3.86 km x sqrt(h) optical
        assert!((horizon_m(15.0, K_RADIO) - 4120.0 * 15f64.sqrt()).abs() < 50.0);
        assert!((horizon_m(15.0, K_OPTICAL) - 3860.0 * 15f64.sqrt()).abs() < 50.0);
        // a 10 m ship at the radar's horizon plus 5 km is hull-down by 1.5 m
        let d = horizon_m(15.0, K_RADIO) + 5_000.0;
        let v = visible_height_m(10.0, 10.0, d, 15.0, K_RADIO);
        assert!((v - (10.0 - 25e6 / (2.0 * K_RADIO * EARTH_R_M))).abs() < 1e-6, "{v}");
        assert_eq!(visible_height_m(10.0, 10.0, 1_000.0, 15.0, K_RADIO), 10.0);
    }

    /// The bulge is the link emulator's: a flat sea between two 10 m masts 25 km apart is just
    /// clear at k = 4/3 (horizons 13 km each) and blocked at 30 km.
    #[test]
    fn a_flat_sea_blocks_beyond_the_two_horizons() {
        let h = Header { lat0: 37.0, lon0: -123.0, dlat: 0.001, dlon: 0.001, nlat: 400, nlon: 60, scale: 0.1, nodata: -32768 };
        let g = Grid::from_heights(h, &vec![0.0; 400 * 60]);
        let a = (37.01, -122.97, 10.0);
        let near = (37.01 + 25_000.0 / M_PER_DEG, -122.97, 10.0);
        let far = (37.01 + 30_000.0 / M_PER_DEG, -122.97, 10.0);
        assert!(g.obstruction_m(a, near, [K_RADIO])[0] < 0.0);
        assert!(g.obstruction_m(a, far, [K_RADIO])[0] > 0.0);
        // a 100 m ridge between them blocks at any k
        let mut hs = vec![0.0; 400 * 60];
        for j in 0..60 {
            hs[100 * 60 + j] = 100.0;
        }
        let g = Grid::from_heights(Header { lat0: 37.0, lon0: -123.0, dlat: 0.001, dlon: 0.001, nlat: 400, nlon: 60, scale: 0.1, nodata: -32768 }, &hs);
        let [o, r] = g.obstruction_m(a, near, [K_OPTICAL, K_RADIO]);
        assert!(o > 0.0 && r > 0.0, "{o} {r}");
    }
}
