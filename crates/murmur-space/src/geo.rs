//! Frames and geometry: TEME to Earth-fixed, WGS84 geodetic, look angles, sun position.
//!
//! Conventions: distances in km, angles in degrees at the API boundary (radians inside),
//! times as `DateTime<Utc>`. UT1 is taken equal to UTC (|UT1-UTC| < 0.9 s, so at most
//! ~0.4 km of Earth rotation at the equator) and polar motion is ignored (~10 m).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::f64::consts::{PI, TAU};

pub const WGS84_A_KM: f64 = 6378.137;
pub const WGS84_F: f64 = 1.0 / 298.257_223_563;
pub const EARTH_MEAN_RADIUS_KM: f64 = 6371.0;
pub const C_KM_S: f64 = 299_792.458;
const AU_KM: f64 = 149_597_870.7;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3(pub [f64; 3]);

impl Vec3 {
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Vec3([x, y, z])
    }
    pub fn x(&self) -> f64 {
        self.0[0]
    }
    pub fn y(&self) -> f64 {
        self.0[1]
    }
    pub fn z(&self) -> f64 {
        self.0[2]
    }
    pub fn dot(&self, o: &Vec3) -> f64 {
        self.0[0] * o.0[0] + self.0[1] * o.0[1] + self.0[2] * o.0[2]
    }
    pub fn norm(&self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn sub(&self, o: &Vec3) -> Vec3 {
        Vec3::new(self.0[0] - o.0[0], self.0[1] - o.0[1], self.0[2] - o.0[2])
    }
    pub fn add(&self, o: &Vec3) -> Vec3 {
        Vec3::new(self.0[0] + o.0[0], self.0[1] + o.0[1], self.0[2] + o.0[2])
    }
    pub fn scale(&self, k: f64) -> Vec3 {
        Vec3::new(self.0[0] * k, self.0[1] * k, self.0[2] * k)
    }
    pub fn unit(&self) -> Vec3 {
        let n = self.norm();
        if n == 0.0 { *self } else { self.scale(1.0 / n) }
    }
    pub fn cross(&self, o: &Vec3) -> Vec3 {
        Vec3::new(
            self.0[1] * o.0[2] - self.0[2] * o.0[1],
            self.0[2] * o.0[0] - self.0[0] * o.0[2],
            self.0[0] * o.0[1] - self.0[1] * o.0[0],
        )
    }
    /// Angle between two vectors, radians.
    pub fn angle_to(&self, o: &Vec3) -> f64 {
        let c = self.dot(o) / (self.norm() * o.norm());
        c.clamp(-1.0, 1.0).acos()
    }
}

/// Geodetic position (WGS84). `alt_km` above the ellipsoid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geodetic {
    pub lat_deg: f64,
    pub lon_deg: f64,
    #[serde(default)]
    pub alt_km: f64,
}

impl Geodetic {
    pub fn new(lat_deg: f64, lon_deg: f64, alt_km: f64) -> Self {
        Geodetic {
            lat_deg,
            lon_deg,
            alt_km,
        }
    }

    pub fn to_ecef(&self) -> Vec3 {
        let lat = self.lat_deg.to_radians();
        let lon = self.lon_deg.to_radians();
        let e2 = WGS84_F * (2.0 - WGS84_F);
        let n = WGS84_A_KM / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        Vec3::new(
            (n + self.alt_km) * lat.cos() * lon.cos(),
            (n + self.alt_km) * lat.cos() * lon.sin(),
            (n * (1.0 - e2) + self.alt_km) * lat.sin(),
        )
    }

    pub fn from_ecef(r: &Vec3) -> Geodetic {
        let e2 = WGS84_F * (2.0 - WGS84_F);
        let p = (r.x() * r.x() + r.y() * r.y()).sqrt();
        let lon = r.y().atan2(r.x());
        let mut lat = r.z().atan2(p * (1.0 - e2));
        let mut alt = 0.0;
        // Converges to well below a millimetre in a few iterations for any orbit altitude.
        for _ in 0..6 {
            let n = WGS84_A_KM / (1.0 - e2 * lat.sin().powi(2)).sqrt();
            alt = if lat.cos().abs() > 1e-9 {
                p / lat.cos() - n
            } else {
                r.z().abs() - n * (1.0 - e2)
            };
            lat = r.z().atan2(p * (1.0 - e2 * n / (n + alt)));
        }
        Geodetic {
            lat_deg: lat.to_degrees(),
            lon_deg: lon.to_degrees(),
            alt_km: alt,
        }
    }

    /// Great-circle distance on a mean sphere, km.
    pub fn distance_km(&self, o: &Geodetic) -> f64 {
        central_angle(self, o) * EARTH_MEAN_RADIUS_KM
    }

    /// Point at a distance and initial bearing along a great circle (mean sphere).
    pub fn destination(&self, bearing_deg: f64, dist_km: f64) -> Geodetic {
        let d = dist_km / EARTH_MEAN_RADIUS_KM;
        let b = bearing_deg.to_radians();
        let lat1 = self.lat_deg.to_radians();
        let lon1 = self.lon_deg.to_radians();
        let lat2 = (lat1.sin() * d.cos() + lat1.cos() * d.sin() * b.cos()).asin();
        let lon2 = lon1 + (b.sin() * d.sin() * lat1.cos()).atan2(d.cos() - lat1.sin() * lat2.sin());
        Geodetic::new(lat2.to_degrees(), wrap180(lon2.to_degrees()), 0.0)
    }

    /// Initial bearing to another point, degrees from north.
    pub fn bearing_to(&self, o: &Geodetic) -> f64 {
        let lat1 = self.lat_deg.to_radians();
        let lat2 = o.lat_deg.to_radians();
        let dlon = (o.lon_deg - self.lon_deg).to_radians();
        let y = dlon.sin() * lat2.cos();
        let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
        (y.atan2(x).to_degrees() + 360.0) % 360.0
    }
}

pub fn wrap180(lon: f64) -> f64 {
    let mut l = (lon + 180.0) % 360.0;
    if l < 0.0 {
        l += 360.0;
    }
    l - 180.0
}

/// Central angle between two geodetic points on a sphere, radians (haversine).
pub fn central_angle(a: &Geodetic, b: &Geodetic) -> f64 {
    let lat1 = a.lat_deg.to_radians();
    let lat2 = b.lat_deg.to_radians();
    let dlat = lat2 - lat1;
    let dlon = (b.lon_deg - a.lon_deg).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * h.sqrt().min(1.0).asin()
}

/// Julian date (UTC used as UT1).
pub fn julian_date(t: &DateTime<Utc>) -> f64 {
    let secs = t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) * 1e-9;
    secs / 86400.0 + 2_440_587.5
}

/// Greenwich mean sidereal time (IAU-82, as used by SGP4's TEME convention), radians.
pub fn gmst(t: &DateTime<Utc>) -> f64 {
    let tut1 = (julian_date(t) - 2_451_545.0) / 36_525.0;
    let secs = -6.2e-6 * tut1.powi(3)
        + 0.093_104 * tut1.powi(2)
        + (876_600.0 * 3600.0 + 8_640_184.812_866) * tut1
        + 67_310.548_41;
    (secs * PI / 180.0 / 240.0).rem_euclid(TAU)
}

/// Rotate a TEME vector into the Earth-fixed (pseudo Earth-fixed, no polar motion) frame.
pub fn teme_to_ecef(r: &Vec3, t: &DateTime<Utc>) -> Vec3 {
    rotate_z(r, gmst(t))
}

fn rotate_z(r: &Vec3, g: f64) -> Vec3 {
    let (s, c) = g.sin_cos();
    Vec3::new(c * r.x() + s * r.y(), -s * r.x() + c * r.y(), r.z())
}

/// Azimuth, elevation and range of a target as seen from an observer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LookAngles {
    pub az_deg: f64,
    pub el_deg: f64,
    pub range_km: f64,
}

/// Precomputed observer frame, so repeated look-angle evaluations are cheap.
#[derive(Debug, Clone, Copy)]
pub struct Observer {
    pub geo: Geodetic,
    pub ecef: Vec3,
    east: Vec3,
    north: Vec3,
    up: Vec3,
}

impl Observer {
    pub fn new(geo: Geodetic) -> Self {
        let lat = geo.lat_deg.to_radians();
        let lon = geo.lon_deg.to_radians();
        let (slat, clat) = lat.sin_cos();
        let (slon, clon) = lon.sin_cos();
        Observer {
            geo,
            ecef: geo.to_ecef(),
            east: Vec3::new(-slon, clon, 0.0),
            north: Vec3::new(-slat * clon, -slat * slon, clat),
            up: Vec3::new(clat * clon, clat * slon, slat),
        }
    }

    /// East, north, up components of a vector from the observer.
    pub fn enu(&self, target_ecef: &Vec3) -> Vec3 {
        let d = target_ecef.sub(&self.ecef);
        Vec3::new(d.dot(&self.east), d.dot(&self.north), d.dot(&self.up))
    }

    pub fn look(&self, target_ecef: &Vec3) -> LookAngles {
        let enu = self.enu(target_ecef);
        let range = enu.norm();
        let el = (enu.z() / range).clamp(-1.0, 1.0).asin();
        let az = enu.x().atan2(enu.y());
        LookAngles {
            az_deg: (az.to_degrees() + 360.0) % 360.0,
            el_deg: el.to_degrees(),
            range_km: range,
        }
    }

    /// Elevation of a direction (unit vector in ECEF), degrees.
    pub fn elevation_of_direction(&self, dir: &Vec3) -> f64 {
        dir.unit()
            .dot(&self.up)
            .clamp(-1.0, 1.0)
            .asin()
            .to_degrees()
    }
}

/// Low-precision sun direction (Astronomical Almanac, ~0.01 deg 1950-2050), unit vector in
/// the Earth-fixed frame, plus distance in km.
pub fn sun_ecef(t: &DateTime<Utc>) -> (Vec3, f64) {
    let n = julian_date(t) - 2_451_545.0;
    let l = (280.460 + 0.985_647_4 * n).to_radians();
    let g = (357.528 + 0.985_600_3 * n).to_radians();
    let lambda = l + (1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.000_000_4 * n).to_radians();
    let dist_au = 1.000_14 - 0.016_71 * g.cos() - 0.000_14 * (2.0 * g).cos();
    let eci = Vec3::new(
        lambda.cos(),
        eps.cos() * lambda.sin(),
        eps.sin() * lambda.sin(),
    );
    (rotate_z(&eci, gmst(t)), dist_au * AU_KM)
}

/// Sun elevation above the horizon at a point, degrees (no refraction).
pub fn sun_elevation_deg(obs: &Observer, t: &DateTime<Utc>) -> f64 {
    let (dir, _) = sun_ecef(t);
    obs.elevation_of_direction(&dir)
}

/// Viewing geometry between a satellite and a ground target, all in degrees/km.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewGeometry {
    /// Elevation of the satellite seen from the target.
    pub elevation_deg: f64,
    /// Azimuth of the satellite seen from the target.
    pub azimuth_deg: f64,
    /// Slant range target to satellite.
    pub range_km: f64,
    /// Angle at the satellite between nadir and the target (look angle).
    pub off_nadir_deg: f64,
    /// Local incidence angle at the target (90 - elevation on the ellipsoid normal).
    pub incidence_deg: f64,
    /// Satellite altitude above the ellipsoid at this instant.
    pub sat_alt_km: f64,
}

pub fn view_geometry(target: &Observer, sat_ecef: &Vec3, sat_alt_km: f64) -> ViewGeometry {
    let look = target.look(sat_ecef);
    let to_target = target.ecef.sub(sat_ecef);
    // Nadir taken as the geocentric direction; the geodetic normal differs by <0.2 deg.
    let nadir = sat_ecef.scale(-1.0);
    let off_nadir = nadir.angle_to(&to_target).to_degrees();
    ViewGeometry {
        elevation_deg: look.el_deg,
        azimuth_deg: look.az_deg,
        range_km: look.range_km,
        off_nadir_deg: off_nadir,
        incidence_deg: 90.0 - look.el_deg,
        sat_alt_km,
    }
}

/// Earth central angle (radians) from the sub-satellite point to where a ground target sees
/// the satellite at `el_deg`, for a satellite at radius `r_sat_km` (spherical Earth).
pub fn central_angle_for_elevation(r_sat_km: f64, el_deg: f64) -> f64 {
    let el = el_deg.to_radians();
    let re = EARTH_MEAN_RADIUS_KM;
    // nadir angle eta from sin(eta) = re/r * cos(el); central angle = 90 - el - eta.
    let eta = (re / r_sat_km * el.cos()).clamp(-1.0, 1.0).asin();
    (PI / 2.0 - el - eta).max(0.0)
}

/// Earth central angle (radians) at which a satellite at `r_sat_km` looks at the ground with
/// off-nadir angle `eta_deg`. Returns the horizon angle if the look misses the Earth.
pub fn central_angle_for_off_nadir(r_sat_km: f64, eta_deg: f64) -> f64 {
    let re = EARTH_MEAN_RADIUS_KM;
    let eta = eta_deg.to_radians();
    let s = r_sat_km / re * eta.sin();
    if s >= 1.0 {
        return (re / r_sat_km).acos();
    }
    let el = s.acos(); // elevation at the ground, since cos(el) = r/re * sin(eta)
    (PI / 2.0 - el - eta).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn geodetic_roundtrip() {
        for &(lat, lon, alt) in &[
            (37.8, -122.47, 0.0),
            (-33.9, 151.2, 550.0),
            (89.9, 10.0, 35786.0),
            (0.0, 0.0, 0.0),
        ] {
            let g = Geodetic::new(lat, lon, alt);
            let back = Geodetic::from_ecef(&g.to_ecef());
            assert!((back.lat_deg - lat).abs() < 1e-9, "{back:?}");
            assert!((back.lon_deg - lon).abs() < 1e-9);
            assert!((back.alt_km - alt).abs() < 1e-6);
        }
    }

    #[test]
    fn gmst_reference() {
        // Vallado, Example 3-5: 1992-08-20 12:14:00 UT1 -> GMST 152.578788 deg.
        let t = Utc
            .with_ymd_and_hms(1992, 8, 20, 12, 14, 0)
            .single()
            .expect("valid");
        let g = gmst(&t).to_degrees();
        assert!((g - 152.578_788).abs() < 1e-5, "{g}");
    }

    #[test]
    fn sun_at_june_solstice_noon() {
        // Around the June 2026 solstice, local solar noon at the Golden Gate: elevation is
        // 90 - (37.8 - 23.44) = 75.6 deg.
        let obs = Observer::new(Geodetic::new(37.8, -122.47, 0.0));
        let t = Utc
            .with_ymd_and_hms(2026, 6, 21, 20, 8, 0)
            .single()
            .expect("valid");
        let el = sun_elevation_deg(&obs, &t);
        assert!((el - 75.6).abs() < 0.3, "{el}");
        let night = Utc
            .with_ymd_and_hms(2026, 6, 21, 8, 8, 0)
            .single()
            .expect("valid");
        assert!(sun_elevation_deg(&obs, &night) < -20.0);
    }

    #[test]
    fn horizon_angles_consistent() {
        let r = WGS84_A_KM + 550.0;
        let psi_el = central_angle_for_elevation(r, 25.0);
        // For the same geometry the off-nadir angle that hits that ring.
        let eta = (EARTH_MEAN_RADIUS_KM / r * 25f64.to_radians().cos())
            .asin()
            .to_degrees();
        let psi_on = central_angle_for_off_nadir(r, eta);
        assert!((psi_el - psi_on).abs() < 1e-9);
        // Starlink at 550 km, 25 deg: footprint radius ~ 940 km.
        let radius = psi_el * EARTH_MEAN_RADIUS_KM;
        assert!((radius - 940.0).abs() < 40.0, "{radius}");
    }
}
