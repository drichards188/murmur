//! Geometry helpers and the terrain hook.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
    /// Metres above mean sea level; negative is depth.
    pub alt_m: f64,
}

const EARTH_R_M: f64 = 6_371_000.0;

/// Great-circle ground distance in metres.
pub fn haversine_m(a: &GeoPoint, b: &GeoPoint) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_R_M * h.sqrt().asin()
}

/// Straight-line distance including the altitude difference.
pub fn slant_m(a: &GeoPoint, b: &GeoPoint) -> f64 {
    let g = haversine_m(a, b);
    let dz = a.alt_m - b.alt_m;
    (g * g + dz * dz).sqrt()
}

/// Line-of-sight oracle. The real implementation will sample tidewater's DEM; until then
/// `NoTerrain` says everything can see everything.
pub trait Terrain: Send + Sync {
    fn line_of_sight(&self, from: &GeoPoint, to: &GeoPoint) -> bool;
}

pub struct NoTerrain;

impl Terrain for NoTerrain {
    fn line_of_sight(&self, _from: &GeoPoint, _to: &GeoPoint) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_degree_of_latitude_is_about_111_km() {
        let a = GeoPoint {
            lat: 37.0,
            lon: -122.0,
            alt_m: 0.0,
        };
        let b = GeoPoint {
            lat: 38.0,
            lon: -122.0,
            alt_m: 0.0,
        };
        let d = haversine_m(&a, &b);
        assert!((d - 111_195.0).abs() < 200.0, "{d}");
    }
}
