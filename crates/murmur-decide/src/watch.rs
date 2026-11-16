//! The scenario's settings for the watch (docs/SCENARIO-SETTINGS.md section 3): the policy's
//! `[watch]` table, one source per scenario for what counts as identified, in doubt and
//! approaching, read by the engine, the bridge, the agents and the scoring alike.
//!
//! ```toml
//! [watch]
//! bar = 0.6                     # identity confidence that counts as identified
//! neutral_after_bar = true      # no neutral or assumed-friend call by classification below it
//! approach_horizon_s = 300      # how far ahead a contact counts as approaching
//! identify_on_station_s = 300   # an identify order's time on station
//! stale_s = 10                  # a track unseen this long is out of view
//! still_mps = 0.5               # a shown speed under this is no velocity
//! look_m = 1000                 # an orbit this near a contact is a look on it
//! [watch.no_velocity_mps]
//! air = 25.0                    # a contact shown without velocity comes straight in at this
//! [watch.domain.surface]        # and air, ground, subsurface
//! level = "identify"            # recognise or identify, never detect (the floor)
//! sensors = ["eo_ir", "ais"]
//! grace_s = 60
//! [watch.area]                  # optional: the scenario's circle
//! name = "the Golden Gate Bridge"
//! center = [-122.4783, 37.8199] # [lon, lat]
//! radius_m = 3000
//! ```
//!
//! Every key is optional in a file and takes its standing value ([`WatchSettings::default`],
//! `sim/policy.toml`'s `[watch]`); the policy writes them all out, so a run logs what it decided
//! under. Logged from rules 33 ([`crate::rules::SCENARIO_SETTINGS_SINCE`], policy schema 8); under
//! older rules nothing reads them, and what they meant is [`WatchSettings::legacy`], the values
//! frozen in `sim/policy-v0-watch.toml`, whatever the file says (the bridge refuses a file that
//! says otherwise, the area aside: [`crate::Policy::fits_rules`]).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::catalog::Domain;
use crate::identify::Level;

/// The sensor types a domain's criteria may name: the catalog's (`sim/platforms.toml`).
pub const SENSOR_TYPES: [&str; 5] = ["ais", "eo_ir", "esm", "radar", "sonar"];

/// The frozen v0 file: the watch every run before rules 33 was briefed and graded by.
const V0_FILE: &str = include_str!("../../../sim/policy-v0-watch.toml");

/// `[watch]`: what counts as identified, in doubt and approaching in this scenario.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WatchSettings {
    /// The identity confidence that counts as identified (0.3..1).
    pub bar: f64,
    /// No neutral or assumed-friend call by classification below the bar.
    pub neutral_after_bar: bool,
    /// How far ahead a contact on its shown velocity counts as approaching, s.
    pub approach_horizon_s: f64,
    /// An identify order's time on station, s.
    pub identify_on_station_s: f64,
    /// A track unseen this long is stale (out of view), s.
    pub stale_s: f64,
    /// A shown speed under this is no velocity, m/s.
    pub still_mps: f64,
    /// An orbit this near a contact is a look on it, m.
    pub look_m: f64,
    /// A contact of these domains shown without velocity comes straight in at this, m/s (a
    /// domain not named: judged where it is).
    pub no_velocity_mps: BTreeMap<Domain, f64>,
    /// What identifies a contact of each domain, and by when.
    pub domain: WatchDomains,
    /// The scenario's circle, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<WatchArea>,
}

/// `[watch.domain.<domain>]` for each domain, in the mission's order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WatchDomains {
    pub surface: DomainCriteria,
    pub air: DomainCriteria,
    pub ground: DomainCriteria,
    pub subsurface: DomainCriteria,
}

/// What identifies a contact of one domain: its identity at `level` or better, supported by one
/// of `sensors`, within `grace_s` of its coming inside the circle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainCriteria {
    pub level: Level,
    pub sensors: Vec<String>,
    pub grace_s: f64,
}

/// `[watch.area]`: the scenario's circle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchArea {
    pub name: String,
    /// [lon, lat], degrees.
    pub center: [f64; 2],
    pub radius_m: f64,
}

fn criteria(level: Level, sensors: &[&str], grace_s: f64) -> DomainCriteria {
    DomainCriteria {
        level,
        sensors: sensors.iter().map(|s| (*s).to_string()).collect(),
        grace_s,
    }
}

impl Default for WatchDomains {
    /// The standing criteria (`sim/policy.toml`).
    fn default() -> Self {
        WatchDomains {
            surface: criteria(Level::Identify, &["eo_ir", "ais"], 60.0),
            air: criteria(Level::Identify, &["eo_ir"], 30.0),
            ground: criteria(Level::Identify, &["eo_ir"], 60.0),
            subsurface: criteria(Level::Recognise, &["sonar"], 300.0),
        }
    }
}

impl Default for WatchSettings {
    /// The standing values (`sim/policy.toml`'s `[watch]`, a test holds them together): today's,
    /// but the tracker's neutral call waits for the bar and the horizon is the scoring's 300 s.
    fn default() -> Self {
        WatchSettings {
            bar: 0.6,
            neutral_after_bar: true,
            approach_horizon_s: 300.0,
            identify_on_station_s: crate::identify::ON_STATION_S,
            stale_s: crate::engage::OUT_OF_VIEW_S,
            still_mps: 0.5,
            look_m: 1000.0,
            no_velocity_mps: BTreeMap::from([(Domain::Air, 25.0)]),
            domain: WatchDomains::default(),
            area: None,
        }
    }
}

impl WatchDomains {
    pub fn get(&self, d: Domain) -> &DomainCriteria {
        match d {
            Domain::Surface => &self.surface,
            Domain::Air => &self.air,
            Domain::Ground => &self.ground,
            Domain::Subsurface => &self.subsurface,
        }
    }

    fn each(&self) -> [(Domain, &DomainCriteria); 4] {
        [
            (Domain::Surface, &self.surface),
            (Domain::Air, &self.air),
            (Domain::Ground, &self.ground),
            (Domain::Subsurface, &self.subsurface),
        ]
    }
}

impl WatchSettings {
    /// What rules below 33 mean, whatever a policy file says: the `[watch]` of
    /// `sim/policy-v0-watch.toml` (frozen, its sha256 pinned by the harness), no area.
    pub fn legacy() -> &'static WatchSettings {
        static LEGACY: OnceLock<WatchSettings> = OnceLock::new();
        LEGACY.get_or_init(|| {
            let file: toml::Table = toml::from_str(V0_FILE).expect("sim/policy-v0-watch.toml is TOML");
            let watch = file.get("watch").cloned().expect("sim/policy-v0-watch.toml has a [watch]");
            watch.try_into().expect("sim/policy-v0-watch.toml's [watch] is in the schema")
        })
    }

    /// Every bound, and the floor's part of it: no domain identified at `detect`.
    pub fn validate(&self) -> Result<(), String> {
        let within = |v: f64, lo: f64, hi: f64, n: &str| {
            if v.is_finite() && v >= lo && v <= hi {
                Ok(())
            } else {
                Err(format!("watch.{n} must be within {lo}..{hi}, got {v}"))
            }
        };
        within(self.bar, 0.3, 1.0, "bar")?;
        within(self.approach_horizon_s, 10.0, 3600.0, "approach_horizon_s")?;
        within(self.identify_on_station_s, 10.0, 3600.0, "identify_on_station_s")?;
        within(self.stale_s, 1.0, 120.0, "stale_s")?;
        within(self.still_mps, 0.0, 5.0, "still_mps")?;
        within(self.look_m, 10.0, 20_000.0, "look_m")?;
        for (d, v) in &self.no_velocity_mps {
            within(*v, 0.1, 400.0, &format!("no_velocity_mps.{}", d.as_str()))?;
        }
        for (d, c) in self.domain.each() {
            let at = format!("watch.domain.{}", d.as_str());
            if c.level == Level::Detect {
                return Err(format!(
                    "{at}.level = \"detect\": a detection identifies nothing; recognise or identify (the floor, no override)"
                ));
            }
            if c.sensors.is_empty() {
                return Err(format!("{at}.sensors names no sensor"));
            }
            for (i, s) in c.sensors.iter().enumerate() {
                if !SENSOR_TYPES.contains(&s.as_str()) {
                    return Err(format!("{at}.sensors: {s:?} is not a sensor type ({})", SENSOR_TYPES.join(", ")));
                }
                if c.sensors[..i].contains(s) {
                    return Err(format!("{at}.sensors names {s:?} twice"));
                }
            }
            within(c.grace_s, 0.0, 3600.0, &format!("domain.{}.grace_s", d.as_str()))?;
        }
        if let Some(a) = &self.area {
            if a.name.trim().is_empty() {
                return Err("watch.area.name is empty".into());
            }
            within(a.center[0], -180.0, 180.0, "area.center's longitude")?;
            within(a.center[1], -90.0, 90.0, "area.center's latitude")?;
            within(a.radius_m, 10.0, 200_000.0, "area.radius_m")?;
        }
        Ok(())
    }

    /// The keys whose values differ from `other`'s, the area aside, as dotted paths.
    pub fn differs_from(&self, other: &WatchSettings) -> Vec<String> {
        let mut out = Vec::new();
        let mut key = |same: bool, k: &str| {
            if !same {
                out.push(format!("watch.{k}"));
            }
        };
        key(self.bar == other.bar, "bar");
        key(self.neutral_after_bar == other.neutral_after_bar, "neutral_after_bar");
        key(self.approach_horizon_s == other.approach_horizon_s, "approach_horizon_s");
        key(self.identify_on_station_s == other.identify_on_station_s, "identify_on_station_s");
        key(self.stale_s == other.stale_s, "stale_s");
        key(self.still_mps == other.still_mps, "still_mps");
        key(self.look_m == other.look_m, "look_m");
        key(self.no_velocity_mps == other.no_velocity_mps, "no_velocity_mps");
        for ((d, a), (_, b)) in self.domain.each().into_iter().zip(other.domain.each()) {
            key(a == b, &format!("domain.{}", d.as_str()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_is_the_frozen_file_and_differs_from_the_standing_values_only_in_the_neutral_call() {
        let l = WatchSettings::legacy();
        assert!(l.area.is_none());
        assert!(l.validate().is_ok());
        assert_eq!(l.differs_from(&WatchSettings::default()), ["watch.neutral_after_bar"]);
    }
}
