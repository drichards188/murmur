//! The conditions the physical model senses in (WORLD-V3-PLAN 2.3-2.6, section 5): visibility,
//! sea state, wind, shipping noise, the water's depth, and the sun from the scenario clock and
//! the place. A scenario or world gives them in `[conditions]`; what it leaves out is a clear
//! day's (20 km visibility), sea state 2 and the sonar's reference environment (5 m/s wind,
//! 50 m of water, moderate shipping).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Conditions {
    /// Meteorological visibility, m (Koschmieder's 2 % contrast range).
    pub visibility_m: f64,
    /// Douglas sea state, 0 to 9 (sea clutter).
    pub sea_state: f64,
    /// Wind speed at the surface, m/s (sonar ambient noise).
    pub wind_mps: f64,
    /// Shipping activity, 0 to 1 (sonar ambient noise).
    pub shipping: f64,
    /// Water depth where there is no bathymetry, m (sonar spreading).
    pub water_depth_m: f64,
    /// The sun's elevation, degrees; none: from the scenario clock and the place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sun_elevation_deg: Option<f64>,
}

impl Default for Conditions {
    fn default() -> Conditions {
        Conditions {
            visibility_m: 20_000.0,
            sea_state: 2.0,
            wind_mps: 5.0,
            shipping: 0.5,
            water_depth_m: 50.0,
            sun_elevation_deg: None,
        }
    }
}

impl Conditions {
    /// The sun's elevation at `lat`, `lon` at unix time `t`, degrees: the stated one, or the
    /// sun's position from the clock.
    pub fn sun_at(&self, t: f64, lat: f64, lon: f64) -> f64 {
        self.sun_elevation_deg.unwrap_or_else(|| sun_elevation_deg(t, lat, lon))
    }

    /// Out-of-range values brought in (a scenario's typo should not make a negative fog).
    pub fn sane(mut self) -> Conditions {
        let d = Conditions::default();
        let pick = |v: f64, lo: f64, hi: f64, def: f64| if v.is_finite() { v.clamp(lo, hi) } else { def };
        self.visibility_m = pick(self.visibility_m, 50.0, 200_000.0, d.visibility_m);
        self.sea_state = pick(self.sea_state, 0.0, 9.0, d.sea_state);
        self.wind_mps = pick(self.wind_mps, 0.0, 60.0, d.wind_mps);
        self.shipping = pick(self.shipping, 0.0, 1.0, d.shipping);
        self.water_depth_m = pick(self.water_depth_m, 1.0, 11_000.0, d.water_depth_m);
        self.sun_elevation_deg = self.sun_elevation_deg.filter(|e| e.is_finite()).map(|e| e.clamp(-90.0, 90.0));
        self
    }
}

/// The sun's elevation above the horizon, degrees, at unix time `t` (s) and a place: the
/// low-precision solar position of the Astronomical Almanac (about 0.01 degrees in declination
/// over 1950-2050), refraction ignored.
pub fn sun_elevation_deg(t: f64, lat: f64, lon: f64) -> f64 {
    let d = t / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    let g = (357.528 + 0.985_600_3 * d).to_radians();
    let l = 280.460 + 0.985_647_4 * d;
    let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.000_000_4 * d).to_radians();
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos());
    let dec = (eps.sin() * lambda.sin()).asin();
    let gmst_deg = (280.460_618_37 + 360.985_647_366_29 * d).rem_euclid(360.0);
    let h = (gmst_deg + lon).to_radians() - ra;
    let phi = lat.to_radians();
    (phi.sin() * dec.sin() + phi.cos() * dec.cos() * h.cos()).asin().to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At the Golden Gate on 2026-06-14 the sun is high at solar noon (about 75.5 degrees: the
    /// latitude 37.8 against a declination of 23.3) and down at 3 a.m.; on the equinox it rises
    /// about 06:00 local mean time.
    #[test]
    fn the_sun_is_up_by_day_and_down_by_night() {
        let noon = 1_781_467_800.0; // 2026-06-14T20:10Z, solar noon at -122.48
        let e = sun_elevation_deg(noon, 37.82, -122.48);
        assert!((e - 75.5).abs() < 0.7, "{e}");
        let night = 1_781_431_200.0; // 2026-06-14T10:00Z, 03:00 PDT
        assert!(sun_elevation_deg(night, 37.82, -122.48) < -20.0);
        // 2026-03-20T14:10Z: local mean time 06:00 at -122.5
        let dawn = sun_elevation_deg(1_774_015_800.0, 37.82, -122.5);
        assert!(dawn.abs() < 2.0, "{dawn}");
        let c = Conditions { sun_elevation_deg: Some(-30.0), ..Conditions::default() };
        assert_eq!(c.sun_at(noon, 37.82, -122.48), -30.0);
    }
}
