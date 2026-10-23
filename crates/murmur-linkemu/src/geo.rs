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

/// How a radio path is judged: antenna heights above the reported altitudes, the effective
/// Earth radius factor and an optional Fresnel-zone clearance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LosParams {
    pub gs_antenna_m: f64,
    pub antenna_m: f64,
    /// Effective Earth radius factor (4/3 for a standard atmosphere).
    pub k: f64,
    /// Carrier frequency; 0 means a bare line of sight with no Fresnel clearance.
    pub fresnel_mhz: f64,
    /// Fraction of the first Fresnel radius to keep clear.
    pub fresnel_clear: f64,
}

impl Default for LosParams {
    fn default() -> Self {
        LosParams {
            gs_antenna_m: 0.0,
            antenna_m: 0.0,
            k: 4.0 / 3.0,
            fresnel_mhz: 0.0,
            fresnel_clear: 0.6,
        }
    }
}

impl LosParams {
    pub fn key(&self) -> [u64; 5] {
        [
            self.gs_antenna_m.to_bits(),
            self.antenna_m.to_bits(),
            self.k.to_bits(),
            self.fresnel_mhz.to_bits(),
            self.fresnel_clear.to_bits(),
        ]
    }
}

/// Terrain oracle. It must never block: the real implementation answers from a cache that a
/// background task fills from tidewater's DEM.
pub trait Terrain: Send + Sync {
    /// How far the terrain (plus Earth bulge and any Fresnel clearance) sticks up above the
    /// straight ray from the ground station to the vehicle, in metres: positive means
    /// obstructed by that much, negative means clear by that much. `None` means no answer
    /// yet (the caller treats the path as open and counts it).
    fn obstruction_m(&self, vehicle: &GeoPoint, gs: &GeoPoint, p: &LosParams) -> Option<f64>;

    /// [`Terrain::obstruction_m`] for a vehicle at rest on the surface (a landed, disarmed
    /// aircraft: `link.rs` `Link::at_rest`): at its own altitude, where a moving vehicle's is
    /// rounded to a cache band that can put one on the ground metres under it.
    fn obstruction_at_rest_m(
        &self,
        vehicle: &GeoPoint,
        gs: &GeoPoint,
        p: &LosParams,
    ) -> Option<f64> {
        self.obstruction_m(vehicle, gs, p)
    }

    /// False for the no-op terrain: links then report the check as off.
    fn enabled(&self) -> bool {
        true
    }

    /// Counters for `/stats`, if the implementation keeps any.
    fn stats(&self) -> Option<serde_json::Value> {
        None
    }
}

/// No terrain data configured: everything can see everything.
pub struct NoTerrain;

impl Terrain for NoTerrain {
    fn obstruction_m(&self, _: &GeoPoint, _: &GeoPoint, _: &LosParams) -> Option<f64> {
        None
    }

    fn enabled(&self) -> bool {
        false
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
