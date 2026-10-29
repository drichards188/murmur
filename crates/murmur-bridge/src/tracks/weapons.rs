//! The weapon picture (docs/OPPONENT-WEAPONS.md): each opponent weapon as our sensors know it,
//! kept apart from the track picture. A weapon track `W-<n>` holds the launch our cameras saw
//! (its fix, the cameras, the act report it became) and the weapon's own fixes from our radars
//! and cameras, and ends **expended** when its victim reports the strike, or **lost** 10 s after
//! its last cue with none; ended, it is shown for a minute, then forgotten.
//!
//! Never a track: it is not in the decision facts, so it is never a lost contact (rules 16),
//! never counted out of view at an air track's top speed, never a keep-out item, an attribution
//! candidate or an escort, and replay never sees it leave a picture. Nothing the engine decides
//! reads it; the console draws it and the inbox shows each strike.
//!
//! **By construction** (the design's shortcuts, bd murmur-ju1h): a weapon's fixes are told from
//! contacts' by the truth's lists and associated by its truth id (the tracker associates
//! detections by contact id alike), and a seen launch reports the weapon's class, its nominal
//! speed and which of ours it heads for, standing for a threat-warning channel that tells a
//! weapon by its speed and origin. A strike is reported at the truth tick at or after it, not
//! over the victim's link (bd murmur-3nkc).
//!
//! Times are the bridge's clock (seconds here, engine ms in the feed): a truth instant is
//! placed at the bridge's receipt of its message less its age then.

use serde_json::{Value, json};

use super::physical::weapon::WeaponFix;
use super::sensing::ActObservation;

/// A weapon track with no cue for this long and no strike ends lost, s.
const LOST_AFTER_S: f64 = 10.0;
/// An ended weapon track is shown this long, then forgotten, s.
const SHOWN_S: f64 = 60.0;

/// The launch as our cameras saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    pub sensors: Vec<String>,
}

/// One fix of the weapon in flight.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    /// `"<uid>:<sensor>"`
    pub sensor: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    InFlight,
    Expended,
    Lost,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::InFlight => "in_flight",
            Status::Expended => "expended",
            Status::Lost => "lost",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeaponTrack {
    pub id: String,
    /// the truth's weapon id: the association, never served
    weapon: String,
    /// the act report the seen launch became (`A-<n>`)
    pub report: Option<String>,
    /// what a seen launch reports (by construction): the class, its nominal speed, which of ours
    /// it heads for
    pub class: Option<String>,
    pub speed_mps: Option<f64>,
    pub aimed_at: Option<u64>,
    pub launch: Option<Launch>,
    pub fixes: Vec<Fix>,
    pub status: Status,
    pub ended_t: Option<f64>,
    /// the strike its victim reported: (uid, when)
    pub struck: Option<(u64, f64)>,
}

impl WeaponTrack {
    /// When it was first known of (the launch seen, else its first fix, else the strike).
    pub fn first_cue_t(&self) -> Option<f64> {
        let a = self.launch.as_ref().map(|l| l.t);
        let b = self.fixes.first().map(|f| f.t);
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn last_cue_t(&self) -> Option<f64> {
        self.fixes.last().map(|f| f.t).into_iter().chain(self.launch.as_ref().map(|l| l.t)).reduce(f64::max)
    }

    /// The warning our sensors gave: from the first cue to the strike, s.
    pub fn warning_s(&self) -> Option<f64> {
        Some(self.struck?.1 - self.first_cue_t()?)
    }

    /// When the strike is expected: by the launch's report, the launch time plus the distance
    /// to the vehicle it heads for at its nominal speed (`at`: where that vehicle is).
    pub fn eta_t(&self, at: impl Fn(u64) -> Option<(f64, f64)>) -> Option<f64> {
        let l = self.launch.as_ref()?;
        let (lat, lon) = at(self.aimed_at?)?;
        let d = crate::geo::haversine_m(l.lat, l.lon, lat, lon);
        Some(l.t + d / self.speed_mps.filter(|s| *s > 0.0)?)
    }
}

/// Every weapon track, the next number, and the strikes waiting to be reported.
#[derive(Debug, Default)]
pub struct Picture {
    pub tracks: Vec<WeaponTrack>,
    next_no: u64,
}

fn ms(t: f64) -> i64 {
    (t * 1000.0).round() as i64
}

impl Picture {
    fn track_mut(&mut self, weapon: &str) -> &mut WeaponTrack {
        if let Some(i) = self.tracks.iter().position(|t| t.weapon == weapon) {
            return &mut self.tracks[i];
        }
        self.next_no += 1;
        self.tracks.push(WeaponTrack {
            id: format!("W-{}", self.next_no),
            weapon: weapon.to_string(),
            report: None,
            class: None,
            speed_mps: None,
            aimed_at: None,
            launch: None,
            fixes: Vec::new(),
            status: Status::InFlight,
            ended_t: None,
            struck: None,
        });
        self.tracks.last_mut().expect("pushed")
    }

    /// A launch our cameras saw (`obs`, at bridge time `t`), and what it reports.
    #[allow(clippy::too_many_arguments)]
    pub fn launch_seen(&mut self, weapon: &str, t: f64, obs: &ActObservation, report: &str, class: &str, speed_mps: f64, aimed_at: Option<u64>) {
        let w = self.track_mut(weapon);
        w.launch = Some(Launch { t, lat: obs.lat, lon: obs.lon, sigma_m: obs.sigma_m, sensors: obs.sensors.clone() });
        w.report = Some(report.to_string());
        w.class = Some(class.to_string());
        w.speed_mps = Some(speed_mps);
        w.aimed_at = aimed_at;
    }

    /// The weapon fixes of one truth message (`offset`: the bridge's time less the truth's).
    pub fn fixes(&mut self, fixes: &[WeaponFix], offset: f64) {
        for f in fixes {
            let w = self.track_mut(&f.weapon);
            if w.status != Status::InFlight {
                continue;
            }
            w.fixes.push(Fix { t: f.t + offset, lat: f.lat, lon: f.lon, sigma_m: (f.sigma_m * 10.0).round() / 10.0, sensor: format!("{}:{}", f.uid, f.sensor.as_str()) });
        }
    }

    /// Our vehicle `uid` reports it was struck at bridge time `t` (by the truth's weapon
    /// `weapon`): its weapon track ends expended (one is made for a weapon nobody saw, so the
    /// strike is in the picture). Returns the track.
    pub fn struck(&mut self, weapon: &str, uid: u64, t: f64) -> WeaponTrack {
        let w = self.track_mut(weapon);
        w.struck = Some((uid, t));
        w.status = Status::Expended;
        w.ended_t = Some(t);
        w.clone()
    }

    /// End lost what has had no cue for [`LOST_AFTER_S`]; forget what ended [`SHOWN_S`] ago.
    /// Whether anything changed.
    pub fn age(&mut self, now: f64) -> bool {
        let before = self.tracks.clone();
        for w in &mut self.tracks {
            if w.status == Status::InFlight && w.last_cue_t().is_some_and(|t| now - t > LOST_AFTER_S) {
                w.status = Status::Lost;
                w.ended_t = Some(now);
            }
        }
        self.tracks.retain(|w| w.ended_t.is_none_or(|e| now - e <= SHOWN_S));
        self.tracks != before
    }

    /// The feed's `weapons` (engine ms): each track with what our sensors have of it. `at`:
    /// where one of ours is now, for the expected strike.
    pub fn json(&self, at: impl Fn(u64) -> Option<(f64, f64)>) -> Vec<Value> {
        self.tracks
            .iter()
            .map(|w| {
                json!({
                    "id": w.id,
                    "report": w.report,
                    "class": w.class,
                    "speed_mps": w.speed_mps,
                    "aimed_at": w.aimed_at.map(|u| u.to_string()),
                    "launch": w.launch.as_ref().map(|l| json!({
                        "t": ms(l.t), "point": [l.lon, l.lat], "sigma_m": l.sigma_m, "sensors": l.sensors,
                    })),
                    "fixes": w.fixes.iter().map(|f| json!({
                        "t": ms(f.t), "point": [f.lon, f.lat], "sigma_m": f.sigma_m, "sensor": f.sensor,
                    })).collect::<Vec<_>>(),
                    "status": w.status.as_str(),
                    "ended_t": w.ended_t.map(ms),
                    "struck": w.struck.map(|(u, t)| json!({ "uid": u.to_string(), "t": ms(t) })),
                    "eta_t": w.eta_t(&at).map(ms),
                    "first_cue_t": w.first_cue_t().map(ms),
                    "warning_s": w.warning_s().map(|s| (s * 10.0).round() / 10.0),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::sensing::SensorType;

    fn obs() -> ActObservation {
        ActObservation { sensors: vec!["1:eo_ir".into()], lat: 37.812, lon: -122.495, sigma_m: 20.0, fixes: Vec::new() }
    }

    /// A seen launch, a fix, a strike: one track with the warning from the launch; a weapon no
    /// one saw is in the picture at its strike; a track with no cue for 10 s ends lost; ended
    /// ones are forgotten after a minute.
    #[test]
    fn a_weapon_track_runs_from_its_launch_to_its_strike() {
        let mut p = Picture::default();
        p.launch_seen("a1:weapon", 100.0, &obs(), "A-1", "ATGM-class missile", 250.0, Some(1));
        let fix = WeaponFix { weapon: "a1:weapon".into(), uid: 2, si: 0, sensor: SensorType::Radar, t: 1_000_001.5, lat: 37.812, lon: -122.49, sigma_m: 30.0 };
        p.fixes(&[fix], 100.0 - 1_000_000.0);
        let w = &p.tracks[0];
        assert_eq!((w.id.as_str(), w.fixes.len(), w.fixes[0].t), ("W-1", 1, 101.5));
        assert_eq!(w.fixes[0].sensor, "2:radar");
        let eta = w.eta_t(|_| Some((37.812, -122.495 + 1000.0 / (111_320.0 * 37.812f64.to_radians().cos())))).expect("eta");
        assert!((eta - 104.0).abs() < 0.05, "{eta}");
        let s = p.struck("a1:weapon", 1, 104.1);
        assert_eq!(s.status, Status::Expended);
        assert!((s.warning_s().expect("warning") - 4.1).abs() < 1e-9);
        let unseen = p.struck("a2:weapon", 1, 200.0);
        assert_eq!((unseen.id.as_str(), unseen.first_cue_t(), unseen.warning_s()), ("W-2", None, None));
        p.launch_seen("a3:weapon", 300.0, &obs(), "A-2", "ATGM-class missile", 250.0, Some(1));
        assert!(p.age(305.0));
        assert_eq!(p.tracks.iter().filter(|w| w.status == Status::InFlight).count(), 1);
        p.age(311.0);
        assert_eq!(p.tracks.last().map(|w| w.status), Some(Status::Lost));
        p.age(400.0);
        assert!(p.tracks.is_empty());
    }
}
