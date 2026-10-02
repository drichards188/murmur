//! SGP4 propagation of public element sets (CelesTrak OMM JSON or TLE lines).

use crate::geo::{Geodetic, Vec3, teme_to_ecef};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Earth's gravitational parameter (WGS84), km^3/s^2.
const MU_KM3_S2: f64 = 398_600.441_8;

#[derive(Debug, thiserror::Error)]
pub enum OrbitError {
    #[error("element set {0}: {1}")]
    Elements(String, String),
    #[error("propagation of {0}: {1}")]
    Propagation(String, String),
}

/// Which SGP4 variant to run. `Improved` is the default in current reference code (Vallado
/// 2006 "i" mode) and what Skyfield uses; `Afspc` reproduces the published verification
/// vectors bit for bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sgp4Mode {
    #[default]
    Improved,
    Afspc,
}

/// One satellite: identity, element epoch and an initialised SGP4 propagator.
#[derive(Clone)]
pub struct Satellite {
    pub name: String,
    pub norad_id: u64,
    pub intl_designator: Option<String>,
    pub epoch: DateTime<Utc>,
    /// Revolutions per day, from the element set.
    pub mean_motion_rev_day: f64,
    pub inclination_deg: f64,
    pub eccentricity: f64,
    elements: sgp4::Elements,
    constants: sgp4::Constants,
    mode: Sgp4Mode,
}

impl std::fmt::Debug for Satellite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Satellite")
            .field("name", &self.name)
            .field("norad_id", &self.norad_id)
            .field("epoch", &self.epoch)
            .finish()
    }
}

/// Satellite state at an instant: TEME (inertial-ish) and Earth-fixed position, km and km/s.
#[derive(Debug, Clone, Copy)]
pub struct State {
    pub teme_pos: Vec3,
    pub teme_vel: Vec3,
    pub ecef: Vec3,
}

impl State {
    pub fn geodetic(&self) -> Geodetic {
        Geodetic::from_ecef(&self.ecef)
    }
}

impl Satellite {
    pub fn from_elements(elements: sgp4::Elements, mode: Sgp4Mode) -> Result<Self, OrbitError> {
        let name = elements
            .object_name
            .clone()
            .unwrap_or_else(|| format!("NORAD {}", elements.norad_id));
        let constants = match mode {
            Sgp4Mode::Improved => sgp4::Constants::from_elements(&elements),
            Sgp4Mode::Afspc => sgp4::Constants::from_elements_afspc_compatibility_mode(&elements),
        }
        .map_err(|e| OrbitError::Elements(name.clone(), e.to_string()))?;
        Ok(Satellite {
            norad_id: elements.norad_id,
            intl_designator: elements.international_designator.clone(),
            epoch: elements.datetime.and_utc(),
            mean_motion_rev_day: elements.mean_motion,
            inclination_deg: elements.inclination,
            eccentricity: elements.eccentricity,
            name,
            elements,
            constants,
            mode,
        })
    }

    pub fn from_tle(
        name: Option<&str>,
        line1: &str,
        line2: &str,
        mode: Sgp4Mode,
    ) -> Result<Self, OrbitError> {
        let el =
            sgp4::Elements::from_tle(name.map(str::to_string), line1.as_bytes(), line2.as_bytes())
                .map_err(|e| {
                    OrbitError::Elements(name.unwrap_or("tle").to_string(), e.to_string())
                })?;
        Satellite::from_elements(el, mode)
    }

    /// Parse a CelesTrak GP JSON array (OMM keywords) into satellites. Entries SGP4 cannot
    /// initialise (decayed, malformed) are returned as errors alongside the good ones.
    pub fn parse_omm_json(
        json: &str,
        mode: Sgp4Mode,
    ) -> Result<(Vec<Satellite>, Vec<OrbitError>), serde_json::Error> {
        let raw: Vec<serde_json::Value> = serde_json::from_str(json)?;
        let mut sats = Vec::with_capacity(raw.len());
        let mut errs = Vec::new();
        for v in raw {
            let label = v
                .get("OBJECT_NAME")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string();
            match serde_json::from_value::<sgp4::Elements>(v) {
                Ok(el) => match Satellite::from_elements(el, mode) {
                    Ok(s) => sats.push(s),
                    Err(e) => errs.push(e),
                },
                Err(e) => errs.push(OrbitError::Elements(label, e.to_string())),
            }
        }
        Ok((sats, errs))
    }

    pub fn minutes_since_epoch(&self, t: &DateTime<Utc>) -> f64 {
        (*t - self.epoch)
            .num_microseconds()
            .map_or(f64::NAN, |us| us as f64 / 60e6)
    }

    /// Age of the element set at `t` (positive when `t` is after the epoch), days.
    pub fn element_age_days(&self, t: &DateTime<Utc>) -> f64 {
        self.minutes_since_epoch(t) / 1440.0
    }

    /// Raw SGP4 output (TEME, km and km/s) at minutes since epoch.
    pub fn propagate_minutes(&self, minutes: f64) -> Result<sgp4::Prediction, OrbitError> {
        let t = sgp4::MinutesSinceEpoch(minutes);
        match self.mode {
            Sgp4Mode::Improved => self.constants.propagate(t),
            Sgp4Mode::Afspc => self.constants.propagate_afspc_compatibility_mode(t),
        }
        .map_err(|e| OrbitError::Propagation(self.name.clone(), e.to_string()))
    }

    pub fn state(&self, t: &DateTime<Utc>) -> Result<State, OrbitError> {
        let p = self.propagate_minutes(self.minutes_since_epoch(t))?;
        let pos = Vec3(p.position);
        Ok(State {
            teme_pos: pos,
            teme_vel: Vec3(p.velocity),
            ecef: teme_to_ecef(&pos, t),
        })
    }

    /// Rough launch date: the international designator's year, refined for recent launches
    /// by counting back the revolution number at epoch (5 digits in TLEs, so it wraps for
    /// objects older than ~18 years at LEO rates; only trusted when that cannot happen).
    pub fn estimated_launch(&self) -> Option<DateTime<Utc>> {
        let year: i32 = self.intl_designator.as_deref()?.get(0..4)?.parse().ok()?;
        let start_of_year = chrono::NaiveDate::from_ymd_opt(year, 1, 1)?
            .and_hms_opt(0, 0, 0)?
            .and_utc();
        let revs = self.elements.revolution_number as f64;
        let days = revs / self.mean_motion_rev_day.max(0.1);
        let by_revs = self.epoch - chrono::Duration::seconds((days * 86400.0) as i64);
        if days < 3000.0 && by_revs >= start_of_year {
            Some(by_revs)
        } else {
            Some(start_of_year)
        }
    }

    /// Nominal orbit period, minutes.
    pub fn period_min(&self) -> f64 {
        1440.0 / self.mean_motion_rev_day
    }

    /// Semi-major axis from the mean motion (Kepler, km). Good enough for bounds.
    pub fn semi_major_axis_km(&self) -> f64 {
        let n = self.mean_motion_rev_day * std::f64::consts::TAU / 86400.0;
        (MU_KM3_S2 / (n * n)).cbrt()
    }

    /// Upper bound on the geocentric radius over the orbit (apogee plus a margin), km.
    pub fn max_radius_km(&self) -> f64 {
        self.semi_major_axis_km() * (1.0 + self.eccentricity) + 25.0
    }

    pub fn min_radius_km(&self) -> f64 {
        self.semi_major_axis_km() * (1.0 - self.eccentricity) - 25.0
    }

    /// Upper bound on how fast the sub-satellite point moves in Earth central angle, rad/s:
    /// orbital angular rate at perigee plus Earth rotation.
    pub fn max_ground_angular_rate(&self) -> f64 {
        let a = self.semi_major_axis_km();
        let rp = a * (1.0 - self.eccentricity);
        let vp = (MU_KM3_S2 * (2.0 / rp - 1.0 / a)).sqrt();
        vp / rp * 1.05 + 7.292_115e-5
    }

    pub fn elements(&self) -> &sgp4::Elements {
        &self.elements
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Cases {
        list: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Case {
        line1: String,
        line2: String,
        states: Vec<toml::Value>,
    }

    /// Vallado's SGP4 verification set (SGP4-VER.TLE / tcppver.out, Revisiting Spacetrack
    /// Report #3, AIAA 2006-6753), as packaged with the `sgp4` crate.
    #[test]
    fn verification_vectors_afspc() {
        let cases: Cases =
            toml::from_str(include_str!("../tests/data/sgp4_verification.toml")).expect("parse");
        let mut checked = 0;
        let mut errors_matched = 0;
        for c in &cases.list {
            let sat = Satellite::from_tle(None, &c.line1, &c.line2, Sgp4Mode::Afspc).expect("init");
            for s in &c.states {
                let time = s.get("time").and_then(toml::Value::as_float).expect("time");
                if let Some(pos) = s.get("position").and_then(|p| p.as_array()) {
                    let vel = s.get("velocity").and_then(|p| p.as_array()).expect("vel");
                    let p = sat.propagate_minutes(time).expect("propagate");
                    for i in 0..3 {
                        let want = pos[i].as_float().expect("f");
                        let wantv = vel[i].as_float().expect("f");
                        assert!((p.position[i] - want).abs() < 1e-6, "{} t={time}", c.line1);
                        assert!((p.velocity[i] - wantv).abs() < 1e-9);
                    }
                    checked += 1;
                } else {
                    assert!(
                        sat.propagate_minutes(time).is_err(),
                        "expected error {} t={time}",
                        c.line1
                    );
                    errors_matched += 1;
                }
            }
        }
        assert!(checked > 500, "{checked}");
        assert!(errors_matched > 0);
    }
}
