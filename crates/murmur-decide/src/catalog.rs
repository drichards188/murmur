//! Platform facts from the catalog (`sim/platforms.toml`) and the capability table.
//!
//! The bridge copies a vehicle's [`PlatformFacts`] into each [`crate::WorldSnapshot`], so the
//! facts a rule read (armed, turn radius, endurance, a sensor's reach) are logged with the
//! decision and replay needs no catalog file. A sensor's reach is the one of the sensing model
//! the run is on ([`Reaches`], [`Catalog::reaching`]).

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
    /// The catalog's climb rate, m/s (`climb_mps`): rules 6 count one of our vehicles as able
    /// to change altitude at least this fast (or `vertical_rate_mps`, the larger) when they
    /// read a keep-out in 3D. Absent in logs written before it existed and for platforms
    /// without the figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub climb_mps: Option<f64>,
    /// The catalog's top speed, m/s (`max_mps`): rules 6 count a fixed wing of ours as able
    /// to dive at it down `FIXED_WING_DIVE_DEG`. Absent where the catalog has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mps: Option<f64>,
    /// The catalog's `expended_by_effect = true`: the platform is spent by its own effect (a
    /// loitering munition, a one-way strike drone). Rules 12 close what is owed back to it
    /// right after its engagement's release (`restore.closed`, `expended`): there is no
    /// platform left to put its own lost-link values back on. Absent (false) elsewhere and in
    /// older logs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expended_by_effect: bool,
    /// Rules 20: the munition the platform launches at an engagement's release (the catalog's
    /// `munition = { name, speed_mps, range_m }`), which flies to the effect point and strikes
    /// there a time of flight later ([`crate::engage::shot`]). Absent for a platform that
    /// carries none and for one spent by its own effect (a loitering munition is its own), and
    /// in older logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub munition: Option<MunitionFacts>,
}

/// Rules 20: a munition from the catalog: what it is called, how fast it flies (a constant
/// speed along a straight slant line from the platform to the effect point) and how far, the
/// slant distance from launch to the effect point it reaches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MunitionFacts {
    pub name: String,
    pub speed_mps: f64,
    pub range_m: f64,
}

/// One sensor from the catalog: its type (`radar`, `eo_ir`, `ais`, `sonar`, `esm`) and its
/// nominal range (where a nominal contact is detected half the time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorFact {
    #[serde(rename = "type")]
    pub kind: String,
    /// The legacy sensing model's reach (and the engine's in a run on that model).
    pub range_m: f64,
    /// In a run on the physical sensing model, the engine's reach by the domain of what it
    /// looks for: the catalog's `physical_reach_m`, derived from the physical model
    /// (docs/SENSING.md, "The decision engine's reaches"; [`Catalog::reaching`]). Absent in a
    /// run on the legacy model, for a sensor the catalog derives none for, and in every log
    /// before it: the engine reads `range_m` then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach_m: Option<SensorReach>,
}

impl SensorFact {
    /// How far this sensor reaches a contact in `domain`, m: its physical reach for the domain
    /// when it carries them (none: it does not reach one there at all), else `range_m`.
    pub fn reach_for(&self, domain: Domain) -> Option<f64> {
        match &self.reach_m {
            Some(r) => r.get(domain),
            None => Some(self.range_m),
        }
    }
}

/// A sensor's reach by the domain of the contact, m (none: it does not reach one there).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorReach {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsurface: Option<f64>,
}

impl SensorReach {
    pub fn get(&self, domain: Domain) -> Option<f64> {
        match domain {
            Domain::Air => self.air,
            Domain::Ground => self.ground,
            Domain::Surface => self.surface,
            Domain::Subsurface => self.subsurface,
        }
    }
}

/// Which reach of each sensor the decision facts carry: the sensing model the run is on
/// (bd murmur-1u4). The engine reads whatever the facts carry, and the facts are logged with
/// each decision, so replay needs neither the catalog nor the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reaches {
    /// `range_m` alone, as every record before world v3 (v1, v2, the Phase G stacks).
    #[default]
    Legacy,
    /// `range_m` and the physical model's reaches by domain (`physical_reach_m`).
    Physical,
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
    max_mps: Option<f64>,
    #[serde(default)]
    expended_by_effect: bool,
    munition: Option<MunitionFacts>,
    #[serde(default)]
    sensors: Vec<RawSensor>,
    /// Each sensor's reach under the physical model, by its type and the contact's domain
    /// (generated: docs/SENSING.md, "The decision engine's reaches").
    #[serde(default)]
    physical_reach_m: BTreeMap<String, SensorReach>,
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
    #[error("platform catalog: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// Each platform's facts with `range_m` alone ([`Reaches::Legacy`]).
    pub platforms: BTreeMap<String, PlatformFacts>,
    /// Each platform's `physical_reach_m`, by sensor type: what [`Catalog::reaching`] puts
    /// on its sensors for a run on the physical model.
    physical_reach: BTreeMap<String, BTreeMap<String, SensorReach>>,
}

impl Catalog {
    pub fn from_toml(text: &str) -> Result<Self, CatalogError> {
        let mut raw: BTreeMap<String, RawPlatform> = toml::from_str(text)?;
        let mut physical_reach = BTreeMap::new();
        for (id, r) in &mut raw {
            for (kind, reach) in &r.physical_reach_m {
                if !r.sensors.iter().any(|s| s.kind == *kind) {
                    return Err(CatalogError::Invalid(format!(
                        "[{id}] physical_reach_m.{kind}: the platform carries no {kind}"
                    )));
                }
                for d in [Domain::Air, Domain::Ground, Domain::Surface, Domain::Subsurface] {
                    if let Some(m) = reach.get(d)
                        && !(m.is_finite() && m > 0.0)
                    {
                        return Err(CatalogError::Invalid(format!(
                            "[{id}] physical_reach_m.{kind}.{}: {m} m (a reach is above 0; leave out a domain it does not reach)",
                            format!("{d:?}").to_lowercase()
                        )));
                    }
                }
            }
            if let Some(m) = &r.munition {
                if !(m.speed_mps.is_finite() && m.speed_mps > 0.0 && m.range_m.is_finite() && m.range_m > 0.0) {
                    return Err(CatalogError::Invalid(format!(
                        "[{id}] munition: speed_mps and range_m are above 0 (got {} m/s, {} m)",
                        m.speed_mps, m.range_m
                    )));
                }
                if r.expended_by_effect {
                    return Err(CatalogError::Invalid(format!(
                        "[{id}] munition: a platform spent by its own effect is its own munition"
                    )));
                }
            }
            let reach = std::mem::take(&mut r.physical_reach_m);
            if !reach.is_empty() {
                physical_reach.insert(id.clone(), reach);
            }
        }
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
                            reach_m: None,
                        })
                        .collect(),
                    climb_mps: r.climb_mps.filter(|c| c.is_finite() && *c > 0.0),
                    max_mps: r.max_mps.filter(|c| c.is_finite() && *c > 0.0),
                    expended_by_effect: r.expended_by_effect,
                    munition: r.munition,
                };
                (id, facts)
            })
            .collect();
        Ok(Catalog {
            platforms,
            physical_reach,
        })
    }

    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CatalogError::Io(path.display().to_string(), e))?;
        Self::from_toml(&text)
    }

    pub fn get(&self, id: &str) -> Option<&PlatformFacts> {
        self.platforms.get(id)
    }

    /// This catalog with its sensors' facts for a run on `reaches`' sensing model: on the
    /// legacy one `range_m` alone, as the catalog is loaded (its facts, and so every decision
    /// logged with them, as before the physical reaches existed); on the physical one each
    /// sensor the catalog derives reaches for carries them too (`reach_m`), which the engine
    /// then reads instead of `range_m` ([`SensorFact::reach_for`]).
    pub fn reaching(&self, reaches: Reaches) -> Catalog {
        let mut c = self.clone();
        for (id, p) in &mut c.platforms {
            for s in &mut p.sensors {
                s.reach_m = match reaches {
                    Reaches::Legacy => None,
                    Reaches::Physical => self
                        .physical_reach
                        .get(id)
                        .and_then(|by_kind| by_kind.get(&s.kind))
                        .cloned(),
                };
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const TEXT: &str = r#"
[usv]
domain = "surface"
sim = "standin"
behavior = "boat"
sensors = [{ type = "radar", range_m = 40000 }, { type = "ais", range_m = 50000 }, { type = "sonar", range_m = 10000 }]
physical_reach_m = { radar = { air = 7530, surface = 9770 }, sonar = {} }
"#;

    /// Loaded, the facts carry `range_m` alone and serialize as every log before the physical
    /// reaches; on the physical model each sensor with derived reaches carries them, and one
    /// without (AIS) none.
    #[test]
    fn the_legacy_facts_are_as_before_and_the_physical_ones_carry_the_reaches() {
        let c = Catalog::from_toml(TEXT).unwrap();
        assert_eq!(c, c.reaching(Reaches::Legacy));
        let radar = &c.get("usv").unwrap().sensors[0];
        assert_eq!(serde_json::to_string(radar).unwrap(), r#"{"type":"radar","range_m":40000.0}"#);
        assert_eq!(radar.reach_for(Domain::Surface), Some(40_000.0));
        assert_eq!(radar.reach_for(Domain::Ground), Some(40_000.0));

        let p = c.reaching(Reaches::Physical);
        let s = &p.get("usv").unwrap().sensors;
        assert_eq!(s[0].reach_for(Domain::Surface), Some(9_770.0));
        assert_eq!(s[0].reach_for(Domain::Air), Some(7_530.0));
        // a domain it is given no reach in: it does not reach a contact there
        assert_eq!(s[0].reach_for(Domain::Ground), None);
        assert_eq!(s[1].reach_m, None);
        assert_eq!(s[1].reach_for(Domain::Surface), Some(50_000.0));
        assert_eq!(s[2].reach_m, Some(SensorReach::default()));
        assert_eq!(s[2].reach_for(Domain::Subsurface), None);
        // logged and read back as it was
        let json = serde_json::to_string(&s[0]).unwrap();
        assert_eq!(json, r#"{"type":"radar","range_m":40000.0,"reach_m":{"air":7530.0,"surface":9770.0}}"#);
        assert_eq!(serde_json::from_str::<SensorFact>(&json).unwrap(), s[0]);
        // and back to the legacy facts
        assert_eq!(p.reaching(Reaches::Legacy), c);
    }

    /// A reach for a sensor the platform does not carry, a reach of 0 and a misspelt domain are
    /// refused.
    #[test]
    fn a_bad_physical_reach_is_refused() {
        for (bad, why) in [
            ("physical_reach_m = { eo_ir = { air = 10 } }", "carries no eo_ir"),
            ("physical_reach_m = { radar = { air = 0 } }", "a reach is above 0"),
            ("physical_reach_m = { radar = { surfce = 10 } }", "surfce"),
        ] {
            let text = TEXT.replace(
                "physical_reach_m = { radar = { air = 7530, surface = 9770 }, sonar = {} }",
                bad,
            );
            let e = Catalog::from_toml(&text).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }
}
