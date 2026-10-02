//! Small planar geometry on longitude/latitude.
//!
//! Areas in a scenario span kilometres to tens of kilometres, where treating lon/lat as
//! planar for containment and crossing tests is accurate enough and keeps the code small
//! and dependency-free. Distances use haversine.

use serde::{Deserialize, Serialize};

const EARTH_R_M: f64 = 6_371_000.0;
/// Metres per degree of latitude, as the Python bridge uses for orbit rings.
const M_PER_DEG: f64 = 111_320.0;

/// A position. Serialized as `[lon, lat]` (GeoJSON order), like today's task params.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "[f64; 2]", into = "[f64; 2]")]
pub struct LonLat {
    pub lon: f64,
    pub lat: f64,
}

impl From<[f64; 2]> for LonLat {
    fn from(a: [f64; 2]) -> Self {
        LonLat {
            lon: a[0],
            lat: a[1],
        }
    }
}

impl From<LonLat> for [f64; 2] {
    fn from(p: LonLat) -> Self {
        [p.lon, p.lat]
    }
}

impl LonLat {
    pub const fn new(lon: f64, lat: f64) -> Self {
        LonLat { lon, lat }
    }

    pub fn is_valid(&self) -> bool {
        self.lon.is_finite()
            && self.lat.is_finite()
            && self.lat.abs() <= 90.0
            && self.lon.abs() <= 180.0
    }

    /// Integer key at MAVLink's 1e-7 degree resolution. Used to match terrain samples to
    /// waypoints, so a last-bit difference in recomputed trigonometry can't miss a lookup.
    pub fn e7(&self) -> (i64, i64) {
        (
            (self.lon * 1e7).round() as i64,
            (self.lat * 1e7).round() as i64,
        )
    }
}

pub fn haversine_m(a: LonLat, b: LonLat) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_R_M * h.sqrt().asin()
}

/// `n` points on a circle, the ring of waypoints the bridge flies for non-fixed-wing orbits
/// (same construction as `circle_points` in the Python bridge).
pub fn circle_points(center: LonLat, radius_m: f64, n: usize) -> Vec<LonLat> {
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            let dlat = radius_m * a.cos() / M_PER_DEG;
            let dlon = radius_m * a.sin() / (M_PER_DEG * center.lat.to_radians().cos());
            LonLat::new(center.lon + dlon, center.lat + dlat)
        })
        .collect()
}

/// The ground path a task commits a vehicle to: its waypoints, and whether the last one
/// leads back to the first (looped orbits and patrols do).
#[derive(Clone, Debug, PartialEq)]
pub struct TaskPath {
    pub points: Vec<LonLat>,
    pub closed: bool,
}

impl TaskPath {
    /// Legs as (index of start, start, end).
    pub fn legs(&self) -> Vec<(usize, LonLat, LonLat)> {
        let n = self.points.len();
        let mut legs: Vec<_> = (1..n)
            .map(|i| (i - 1, self.points[i - 1], self.points[i]))
            .collect();
        if self.closed && n > 2 {
            legs.push((n - 1, self.points[n - 1], self.points[0]));
        }
        legs
    }

    /// Where the vehicle is when the task ends (and RTL starts): the first point for a
    /// closed loop, the last for an open path. 1-based waypoint number and position.
    pub fn end(&self) -> Option<(usize, LonLat)> {
        if self.closed {
            self.points.first().map(|p| (1, *p))
        } else {
            self.points.last().map(|p| (self.points.len(), *p))
        }
    }
}

/// A polygon with optional holes. Rings may or may not repeat their first point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Polygon {
    pub exterior: Vec<LonLat>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<Vec<LonLat>>,
}

impl Polygon {
    pub fn new(exterior: Vec<LonLat>) -> Self {
        Polygon {
            exterior,
            holes: Vec::new(),
        }
    }

    pub fn contains(&self, p: LonLat) -> bool {
        ring_contains(&self.exterior, p) && !self.holes.iter().any(|h| ring_contains(h, p))
    }

    /// True if the segment touches or crosses any edge of the polygon.
    pub fn segment_touches_boundary(&self, a: LonLat, b: LonLat) -> bool {
        std::iter::once(&self.exterior)
            .chain(self.holes.iter())
            .any(|ring| ring_edges(ring).any(|(c, d)| segments_intersect(a, b, c, d)))
    }
}

fn ring_edges(ring: &[LonLat]) -> impl Iterator<Item = (LonLat, LonLat)> + '_ {
    let n = ring.len();
    (0..n).map(move |i| (ring[i], ring[(i + 1) % n]))
}

/// Even-odd ray casting.
fn ring_contains(ring: &[LonLat], p: LonLat) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    for (a, b) in ring_edges(ring) {
        if (a.lat > p.lat) != (b.lat > p.lat) {
            let x = a.lon + (p.lat - a.lat) * (b.lon - a.lon) / (b.lat - a.lat);
            if p.lon < x {
                inside = !inside;
            }
        }
    }
    inside
}

fn orient(a: LonLat, b: LonLat, c: LonLat) -> f64 {
    (b.lon - a.lon) * (c.lat - a.lat) - (b.lat - a.lat) * (c.lon - a.lon)
}

fn on_segment(a: LonLat, b: LonLat, p: LonLat) -> bool {
    p.lon >= a.lon.min(b.lon)
        && p.lon <= a.lon.max(b.lon)
        && p.lat >= a.lat.min(b.lat)
        && p.lat <= a.lat.max(b.lat)
}

/// Inclusive: touching counts. Callers use it in the conservative direction.
pub fn segments_intersect(a: LonLat, b: LonLat, c: LonLat, d: LonLat) -> bool {
    let (o1, o2, o3, o4) = (
        orient(a, b, c),
        orient(a, b, d),
        orient(c, d, a),
        orient(c, d, b),
    );
    if ((o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0))
        && ((o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0))
    {
        return true;
    }
    (o1 == 0.0 && on_segment(a, b, c))
        || (o2 == 0.0 && on_segment(a, b, d))
        || (o3 == 0.0 && on_segment(c, d, a))
        || (o4 == 0.0 && on_segment(c, d, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Polygon {
        Polygon::new(vec![
            LonLat::new(0.0, 0.0),
            LonLat::new(1.0, 0.0),
            LonLat::new(1.0, 1.0),
            LonLat::new(0.0, 1.0),
        ])
    }

    #[test]
    fn containment_with_hole() {
        let mut p = square();
        assert!(p.contains(LonLat::new(0.5, 0.5)));
        assert!(!p.contains(LonLat::new(1.5, 0.5)));
        p.holes.push(vec![
            LonLat::new(0.4, 0.4),
            LonLat::new(0.6, 0.4),
            LonLat::new(0.6, 0.6),
            LonLat::new(0.4, 0.6),
        ]);
        assert!(!p.contains(LonLat::new(0.5, 0.5)));
        assert!(p.contains(LonLat::new(0.2, 0.2)));
    }

    #[test]
    fn crossing() {
        let p = square();
        assert!(p.segment_touches_boundary(LonLat::new(-1.0, 0.5), LonLat::new(0.5, 0.5)));
        assert!(!p.segment_touches_boundary(LonLat::new(0.2, 0.5), LonLat::new(0.5, 0.5)));
        assert!(p.segment_touches_boundary(LonLat::new(-1.0, 0.0), LonLat::new(0.0, 0.0)));
    }

    #[test]
    fn ring_radius() {
        let c = LonLat::new(-122.47, 37.82);
        for p in circle_points(c, 500.0, 12) {
            let d = haversine_m(c, p);
            assert!((d - 500.0).abs() < 2.0, "{d}");
        }
    }

    #[test]
    fn lonlat_serializes_as_pair() {
        let s = serde_json::to_string(&LonLat::new(-122.5, 37.8)).expect("serialize");
        assert_eq!(s, "[-122.5,37.8]");
    }
}
