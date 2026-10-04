//! Small spherical-earth helpers (same formulas as the Python bridge).

use std::f64::consts::PI;

pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = (p2 - p1, (lon2 - lon1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

/// n points on a circle, (lat, lon), starting at `phase` radians from north.
pub fn circle_points(lat: f64, lon: f64, radius_m: f64, n: usize, phase: f64) -> Vec<(f64, f64)> {
    (0..n)
        .map(|k| {
            let a = phase + 2.0 * PI * k as f64 / n as f64;
            let dlat = radius_m * a.cos() / 111_320.0;
            let dlon = radius_m * a.sin() / (111_320.0 * lat.to_radians().cos());
            (lat + dlat, lon + dlon)
        })
        .collect()
}

/// The point `dist_m` from (lat, lon) along `bearing_deg` (flat earth, like
/// [`circle_points`]: fine for the few kilometres a navigation target is away).
pub fn destination(lat: f64, lon: f64, bearing_deg: f64, dist_m: f64) -> (f64, f64) {
    let a = bearing_deg.to_radians();
    let dlat = dist_m * a.cos() / 111_320.0;
    let dlon = dist_m * a.sin() / (111_320.0 * lat.to_radians().cos());
    (lat + dlat, lon + dlon)
}

pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_and_bearings() {
        let d = haversine_m(37.8, -122.4, 37.81, -122.4);
        assert!((d - 1111.95).abs() < 0.1, "{d}");
        assert!((bearing_deg(37.8, -122.4, 37.81, -122.4) - 0.0).abs() < 1e-9);
        assert!((bearing_deg(37.8, -122.4, 37.8, -122.41) - 270.0).abs() < 0.01);
        for (la, lo) in circle_points(37.8, -122.4, 200.0, 12, 0.3) {
            assert!((haversine_m(37.8, -122.4, la, lo) - 200.0).abs() < 1.5);
        }
    }
}
