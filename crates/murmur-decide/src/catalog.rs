//! Platform facts from the catalog (`sim/platforms.toml`) and the capability table.
//!
//! The bridge copies a vehicle's [`PlatformFacts`] into each [`crate::WorldSnapshot`], so the
//! facts a rule read (armed, turn radius, endurance) are logged with the decision and replay
//! needs no catalog file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::intent::TaskKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Air,
    Ground,
    Surface,
    Subsurface,
}

/// Which autopilot a vehicle runs (or imitates), which sets what it can be tasked with.
/// Mirrors `KINDS` in the Python bridge's `tasks.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotKind {
    Plane,
    Vtol,
    Copter,
    Blimp,
    Boat,
    Ugv,
    Sub,
    Tracker,
    /// A drifting buoy: telemetry only, it rejects every task.
    Drifter,
}

impl AutopilotKind {
    pub fn tasks(self) -> &'static [TaskKind] {
        use TaskKind::*;
        match self {
            AutopilotKind::Plane
            | AutopilotKind::Vtol
            | AutopilotKind::Copter
            | AutopilotKind::Blimp
            | AutopilotKind::Boat
            | AutopilotKind::Ugv
            | AutopilotKind::Sub => &[Orbit, Patrol, Hold, Rtl, Engage],
            AutopilotKind::Tracker => &[Track, Hold],
            AutopilotKind::Drifter => &[],
        }
    }

    pub fn supports(self, kind: TaskKind) -> bool {
        self.tasks().contains(&kind)
    }

    /// Fixed-wing kinds fly orbits as a loiter circle with a physical minimum radius.
    pub fn is_fixed_wing(self) -> bool {
        matches!(self, AutopilotKind::Plane | AutopilotKind::Vtol)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AutopilotKind::Plane => "plane",
            AutopilotKind::Vtol => "vtol",
            AutopilotKind::Copter => "copter",
            AutopilotKind::Blimp => "blimp",
            AutopilotKind::Boat => "boat",
            AutopilotKind::Ugv => "ugv",
            AutopilotKind::Sub => "sub",
            AutopilotKind::Tracker => "tracker",
            AutopilotKind::Drifter => "drifter",
        }
    }

    /// From a catalog entry: SITL `vehicle` or stand-in `behavior` (as `SITL_KIND` /
    /// `STANDIN_KIND` in the Python bridge).
    pub fn from_catalog(sim: &str, vehicle_or_behavior: &str) -> Option<Self> {
        Some(match (sim, vehicle_or_behavior) {
            (_, "plane") => AutopilotKind::Plane,
            ("sitl", "vtol") => AutopilotKind::Vtol,
            ("sitl", "copter") => AutopilotKind::Copter,
            ("sitl", "blimp") => AutopilotKind::Blimp,
            ("sitl", "tracker") => AutopilotKind::Tracker,
            (_, "boat") | ("sitl", "sailboat") => AutopilotKind::Boat,
            ("sitl", "ugv") => AutopilotKind::Ugv,
            (_, "sub") => AutopilotKind::Sub,
            ("standin", "drifter") => AutopilotKind::Drifter,
            _ => return None,
        })
    }
}

/// What the decision layer needs to know about a platform type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlatformFacts {
    /// Catalog id, e.g. `"switchblade"`.
    pub id: String,
    pub domain: Domain,
    /// `armed = true` in the catalog: any task needs a human (policy).
    pub armed: bool,
    pub cruise_mps: Option<f64>,
    /// cruise speed / turn rate, rounded to metres (as the Python bridge computes it).
    pub min_turn_radius_m: Option<f64>,
    pub endurance_h: Option<f64>,
    /// The autopilot the catalog says this platform runs. The live kind comes from the
    /// vehicle's heartbeat ([`crate::VehicleFacts::autopilot`]).
    pub autopilot: Option<AutopilotKind>,
    /// The platform's sensors (`sensors = [...]` in the catalog): which of our vehicles can
    /// cover a point, for an engagement's assessment. Absent in logs written before it
    /// existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensors: Vec<SensorFact>,
    /// The catalog's climb rate, m/s (`climb_mps`): rules 5 count one of our vehicles as able
    /// to change altitude at least this fast (or `vertical_rate_mps`, the larger) when they
    /// read a keep-out in 3D. Absent in logs written before it existed and for platforms
    /// without the figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub climb_mps: Option<f64>,
}

/// One sensor from the catalog: its type (`radar`, `eo_ir`, `ais`, `sonar`, `esm`) and its
/// nominal range (where a nominal contact is detected half the time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorFact {
    #[serde(rename = "type")]
    pub kind: String,
    pub range_m: f64,
}

#[derive(Deserialize)]
struct RawPlatform {
    domain: Domain,
    sim: String,
    vehicle: Option<String>,
    behavior: Option<String>,
    #[serde(default)]
    armed: bool,
    cruise_mps: Option<f64>,
    turn_dps: Option<f64>,
    endurance_h: Option<f64>,
    climb_mps: Option<f64>,
    #[serde(default)]
    sensors: Vec<RawSensor>,
}

#[derive(Deserialize)]
struct RawSensor {
    #[serde(rename = "type")]
    kind: String,
    range_m: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("platform catalog: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("platform catalog {0}: {1}")]
    Io(String, std::io::Error),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub platforms: BTreeMap<String, PlatformFacts>,
}

impl Catalog {
    pub fn from_toml(text: &str) -> Result<Self, CatalogError> {
        let raw: BTreeMap<String, RawPlatform> = toml::from_str(text)?;
        let platforms = raw
            .into_iter()
            .map(|(id, r)| {
                let min_turn_radius_m = match (r.cruise_mps, r.turn_dps) {
                    (Some(v), Some(dps)) if v > 0.0 && dps > 0.0 => {
                        Some((v / dps.to_radians()).round())
                    }
                    _ => None,
                };
                let which = r.vehicle.as_deref().or(r.behavior.as_deref()).unwrap_or("");
                let facts = PlatformFacts {
                    id: id.clone(),
                    domain: r.domain,
                    armed: r.armed,
                    cruise_mps: r.cruise_mps,
                    min_turn_radius_m,
                    endurance_h: r.endurance_h,
                    autopilot: AutopilotKind::from_catalog(&r.sim, which),
                    sensors: r
                        .sensors
                        .into_iter()
                        .map(|s| SensorFact {
                            kind: s.kind,
                            range_m: s.range_m,
                        })
                        .collect(),
                    climb_mps: r.climb_mps.filter(|c| c.is_finite() && *c > 0.0),
                };
                (id, facts)
            })
            .collect();
        Ok(Catalog { platforms })
    }

    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CatalogError::Io(path.display().to_string(), e))?;
        Self::from_toml(&text)
    }

    pub fn get(&self, id: &str) -> Option<&PlatformFacts> {
        self.platforms.get(id)
    }
}
