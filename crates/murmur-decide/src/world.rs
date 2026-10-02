//! The world as the decision layer sees it: a snapshot the bridge builds from its world
//! model. Every vehicle fact carries the time it was observed, so rules can refuse stale
//! facts and the log shows how old each input was.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::areas::AreaSet;
use crate::catalog::{AutopilotKind, PlatformFacts};
use crate::geo::LonLat;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Task};
use crate::rules::needs_terrain;
use crate::terrain::{Surface, TerrainOracle};
use crate::time::Time;

/// A value and when it was observed (for telemetry: when the message carrying it arrived).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fact<T> {
    pub value: T,
    pub t: Time,
}

impl<T> Fact<T> {
    pub fn new(value: T, t: Time) -> Self {
        Fact { value, t }
    }
}

/// What the bridge knows about one vehicle. `None` means "not known", which rules treat as
/// a reason to fail, never as a default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleFacts {
    /// From the catalog, via the fleet manifest. None if the vehicle isn't in the manifest.
    pub platform: Option<PlatformFacts>,
    /// From the heartbeat's MAV_TYPE.
    pub autopilot: Option<Fact<AutopilotKind>>,
    /// Navigation ready (GPS + EKF, or the EKF flags for submerged vehicles).
    pub nav_ready: Option<Fact<bool>>,
    pub position: Option<Fact<LonLat>>,
    pub home: Option<Fact<LonLat>>,
    /// 0..100.
    pub battery_pct: Option<Fact<f64>>,
}

/// Terrain answers for a task's waypoints. The engine fills this in from a
/// [`crate::TerrainOracle`] before evaluating rules; the bridge leaves it `NotQueried`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TerrainInputs {
    #[default]
    NotQueried,
    /// No target needs a land/water check, or the task has no waypoints.
    NotNeeded,
    /// No oracle installed or it failed as a whole: the domain rule records "not evaluated".
    Unavailable {
        reason: String,
    },
    Sampled {
        samples: Vec<TerrainSample>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainSample {
    pub lon_e7: i64,
    pub lat_e7: i64,
    pub surface: Surface,
}

impl TerrainInputs {
    pub fn lookup(&self, p: LonLat) -> Option<&Surface> {
        let (lon_e7, lat_e7) = p.e7();
        match self {
            TerrainInputs::Sampled { samples } => samples
                .iter()
                .find(|s| s.lon_e7 == lon_e7 && s.lat_e7 == lat_e7)
                .map(|s| &s.surface),
            _ => None,
        }
    }
}

/// The input to every rule and policy decision. Logged (reduced to what one intent can
/// read) with each decision, so replay recomputes from exactly this.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldSnapshot {
    /// When the snapshot was taken; staleness is measured against it.
    pub t: Time,
    pub vehicles: BTreeMap<Uid, VehicleFacts>,
    /// None: no area file loaded. Rules that need areas then fail closed.
    pub areas: Option<AreaSet>,
    #[serde(default)]
    pub terrain: TerrainInputs,
}

impl WorldSnapshot {
    pub fn new(t: Time) -> Self {
        WorldSnapshot {
            t,
            ..Default::default()
        }
    }

    /// The part of the world an intent's rules can read: its vehicles (and a track target),
    /// the areas, and terrain. This is what gets logged.
    pub fn subset_for(&self, spec: &IntentSpec) -> WorldSnapshot {
        let mut wanted: Vec<&Uid> = spec.uids.iter().collect();
        if let Task::Track { target } = &spec.task {
            wanted.push(target);
        }
        let vehicles = wanted
            .into_iter()
            .filter_map(|u| self.vehicles.get(u).map(|v| (u.clone(), v.clone())))
            .collect();
        WorldSnapshot {
            t: self.t,
            vehicles,
            areas: self.areas.clone(),
            terrain: self.terrain.clone(),
        }
    }

    /// What the engine logs and evaluates for one intent: [`Self::subset_for`] plus terrain
    /// answers for its waypoints from `oracle` (only if some target needs a land/water
    /// check). Public so the bridge can build the same view, e.g. to prefetch terrain.
    pub fn for_intent(&self, spec: &IntentSpec, oracle: &dyn TerrainOracle) -> WorldSnapshot {
        let mut w = self.subset_for(spec);
        w.terrain = match spec.task.path() {
            Some(path) if needs_terrain(spec, &w) => match oracle.sample(w.t, &path.points) {
                Ok(answers) if answers.len() == path.points.len() => TerrainInputs::Sampled {
                    samples: path
                        .points
                        .iter()
                        .zip(answers)
                        .map(|(p, surface)| {
                            let (lon_e7, lat_e7) = p.e7();
                            TerrainSample {
                                lon_e7,
                                lat_e7,
                                surface,
                            }
                        })
                        .collect(),
                },
                // A wrong-length answer is a broken oracle, not a missing one: no samples,
                // so the domain rule fails rather than skipping.
                Ok(_) => TerrainInputs::Sampled {
                    samples: Vec::new(),
                },
                Err(reason) => TerrainInputs::Unavailable { reason },
            },
            _ => TerrainInputs::NotNeeded,
        };
        w
    }
}
