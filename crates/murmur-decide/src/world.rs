//! The world as the decision layer sees it: a snapshot the bridge builds from its world
//! model. Every vehicle fact carries the time it was observed, so rules can refuse stale
//! facts and the log shows how old each input was.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::areas::AreaSet;
use crate::catalog::{AutopilotKind, PlatformFacts};
use crate::geo::LonLat;
use crate::ids::{IntentId, Uid};
use crate::intent::{IntentSpec, Task};
use crate::rules::{RulesConfig, needs_terrain};
use crate::space::SpaceAnswer;
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
    /// What the position rests on. A submerged AUV navigates on dead reckoning (inertial +
    /// DVL, no GNSS), which the bridge counts as navigation-ready for subs only; the
    /// readiness rule records it. Absent in logs written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_source: Option<Fact<NavSource>>,
    /// How often this vehicle's link is expected to report, in ms: 1000 streaming, 20000 on
    /// an acoustic (HIGH_LATENCY2) link. Freshness limits scale with it
    /// (`rules.fact_age_intervals`). Absent in logs written before it existed and for
    /// vehicles whose cadence is unknown; both mean the plain `max_fact_age_s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_interval_ms: Option<u32>,
    /// Velocity east, north in m/s (from course and ground speed), when reported. Engagement
    /// keep-outs carry our vehicles along it to the effect time. Absent in logs written
    /// before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_mps: Option<Fact<[f64; 2]>>,
    /// The GPS receiver's fix type (GPS_RAW_INT: 3 is a 3D fix), and whether the
    /// navigation filter flags the GPS as glitching (EKF_STATUS_REPORT). An engagement's
    /// release needs both (Phase E). Absent in logs written before they existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gps_fix: Option<Fact<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gps_glitch: Option<Fact<bool>>,
    /// Where `gps_fix` comes from when not from the receiver's own report: a HIGH_LATENCY2
    /// report carries no fix type, so one without its GPS failure flag counts as a 3D fix,
    /// and says so here. Absent: the receiver's report (GPS_RAW_INT). Rules 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gps_fix_from: Option<GpsFixFrom>,
    /// The circle the vehicle is flying (an orbit task on station, a fixed wing loitering or
    /// circling home), stamped with the report it rests on: an engagement's friendly
    /// keep-out predicts it anywhere on its disc instead of along its momentary heading.
    /// Absent: not known to be circling. Rules 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circling: Option<Fact<Circle>>,
    /// Altitude above mean sea level (negative: depth), metres, stamped with the position it
    /// came with (GLOBAL_POSITION_INT, HIGH_LATENCY2). Rules 6 read an engagement's keep-out
    /// in 3D where it and the effect's altitude are known; absent (or stale), the keep-out
    /// stays 2D for this vehicle. Absent in logs written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_msl_m: Option<Fact<f64>>,
    /// When the autopilot booted (engine time), estimated by the bridge from the time since
    /// boot its messages carry, and replaced when it detects a reboot. A queued order
    /// compares it with its approval: a vehicle that booted since lost its mission and mode.
    /// Absent before rules 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booted_t: Option<Time>,
}

/// A circle a vehicle flies: its center and its radius with the tolerance it is flown to
/// (the vehicle stays within `radius_m` of `center`). The friendly keep-out trusts it only
/// while the vehicle's position is on it (rules 3).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub center: LonLat,
    pub radius_m: f64,
    /// Where the bridge knows it from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CircleSource>,
    /// When its center was reported (LOITER and RTL: NAV_CONTROLLER_OUTPUT's target); the
    /// fact is stamped with the older of this and the position. None: a task's own center.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center_t: Option<Time>,
}

/// See [`Circle::source`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircleSource {
    /// An orbit task on station, still in the mission's mode.
    OrbitTask,
    /// A fixed wing in LOITER around its navigation target.
    Loiter,
    /// A fixed wing in RTL circling its navigation target (home, or a rally point).
    Rtl,
}

/// See [`VehicleFacts::gps_fix_from`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpsFixFrom {
    /// A HIGH_LATENCY2 report without HL_FAILURE_FLAG_GPS.
    HighLatency2,
}

/// The standard identity (MIL-STD-2525 affiliation) a track is estimated or declared to
/// have. Perception, never truth: the bridge's tracker derives it from what our sensors
/// reported, and an operator may declare one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affiliation {
    /// Nothing supports a judgment yet (2525 "unknown", yellow).
    Unknown,
    /// Not cooperative or inconsistent: a civilian identity that doesn't fit, a dark fast
    /// boat, an unidentified UAS or submarine (2525 "suspect", red dashed).
    Suspect,
    /// Declared hostile by an operator (2525 "hostile", red). Never derived automatically.
    Hostile,
    /// Civilian or cooperative (2525 "neutral", green).
    Neutral,
    /// Cooperative and friendly by its identity (a coast guard cutter on AIS) but not one
    /// of ours (2525 "assumed friend").
    AssumedFriend,
}

impl Affiliation {
    pub fn as_str(self) -> &'static str {
        match self {
            Affiliation::Unknown => "unknown",
            Affiliation::Suspect => "suspect",
            Affiliation::Hostile => "hostile",
            Affiliation::Neutral => "neutral",
            Affiliation::AssumedFriend => "assumed_friend",
        }
    }

    /// The 2525 affiliation letter (second SIDC character).
    pub fn sidc_char(self) -> char {
        match self {
            Affiliation::Unknown => 'U',
            Affiliation::Suspect => 'S',
            Affiliation::Hostile => 'H',
            Affiliation::Neutral => 'N',
            Affiliation::AssumedFriend => 'A',
        }
    }
}

/// What the bridge's tracker knows about one contact that is not ours (Phase C2). Every
/// field is an estimate from our own sensors; staleness (`last_seen_t` against the
/// snapshot's `t`) and `pos_sigma_m` are first class so engagement checks (Phase E) can
/// refuse an old or loose picture. Absent in logs written before tracks existed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackFacts {
    /// Last estimated position and when it was observed.
    pub position: Fact<LonLat>,
    /// Estimated altitude (negative: depth), metres, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
    /// Rules 6: when the altitude was last measured (by a sensor that ranges the contact in
    /// height); absent when unknown. A 3D keep-out counts the altitude only while this is
    /// recent (`track_alt_max_age_s`) and grows its margin from then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_t: Option<Time>,
    /// Estimated velocity east, north in m/s, when the track has enough history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_mps: Option<[f64; 2]>,
    /// 1-sigma horizontal position uncertainty at the snapshot's `t`, metres; grows with
    /// age.
    pub pos_sigma_m: f64,
    /// How fast the tracker grows that uncertainty while the track is unobserved, m/s.
    /// Absent in logs written before it existed (the engagement rules then use the
    /// tracker's law, `max(1, 0.3 x speed)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigma_growth_mps: Option<f64>,
    /// Track existence confidence, 0..1.
    pub confidence: f64,
    pub affiliation: Fact<Affiliation>,
    /// Confidence in the affiliation, 0..1.
    pub affiliation_confidence: f64,
    /// Estimated kind (e.g. "ferry", "small_fast_boat") and confidence, when classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub identity_confidence: f64,
    /// Our sensors behind the track, as "<uid>:<sensor>" (e.g. "3:radar").
    pub sources: Vec<String>,
    /// Rules 6: when each of `sources` last detected the track.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_t: BTreeMap<String, Time>,
    pub domain: crate::catalog::Domain,
    /// When the track was last updated by a detection.
    pub last_seen_t: Time,
    /// An operator declared the affiliation (`by`), overriding the estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
}

/// Where a vehicle's horizontal position comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavSource {
    /// Satellite fix: the navigation filter has an absolute position.
    Gnss,
    /// Relative position only (inertial plus DVL or odometry), error growing with distance.
    DeadReckoning,
    /// No usable position estimate.
    None,
}

impl NavSource {
    pub fn as_str(self) -> &'static str {
        match self {
            NavSource::Gnss => "gnss",
            NavSource::DeadReckoning => "dead_reckoning",
            NavSource::None => "none",
        }
    }
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
    /// When the answer is a nearby sample (an engagement's moving effect point): where it
    /// was taken and how far from the point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<SampleFrom>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampleFrom {
    pub lon_e7: i64,
    pub lat_e7: i64,
    pub distance_m: f64,
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

/// The space segment's answer for a collect or cover. The engine fills this in from its
/// [`crate::SpaceOracle`] before evaluating rules; the bridge leaves it `NotQueried`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SpaceInputs {
    /// Not a space task, or nobody asked: the space rule fails closed on it.
    #[default]
    NotQueried,
    /// No segment loaded, or the query failed: the space rule fails, saying why.
    Unavailable { reason: String },
    /// The answer, and its content hash (how decision inputs reference it in the log).
    Answered {
        sha256: String,
        answer: Box<SpaceAnswer>,
    },
}

impl SpaceInputs {
    pub fn is_not_queried(&self) -> bool {
        matches!(self, SpaceInputs::NotQueried)
    }

    pub fn answer(&self) -> Option<&SpaceAnswer> {
        match self {
            SpaceInputs::Answered { answer, .. } => Some(answer),
            _ => None,
        }
    }
}

/// [`SpaceInputs`] as logged in decision inputs: the answer replaced by its hash (the
/// answer itself is logged once, as `space.query`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoggedSpace {
    #[default]
    NotQueried,
    Unavailable {
        reason: String,
    },
    Answered {
        sha256: String,
    },
}

impl LoggedSpace {
    pub fn is_not_queried(&self) -> bool {
        matches!(self, LoggedSpace::NotQueried)
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
    /// Collect and cover only; engine-filled like `terrain`.
    #[serde(default, skip_serializing_if = "SpaceInputs::is_not_queried")]
    pub space: SpaceInputs,
    /// Tracks of contacts that are not ours, keyed by track id (Phase C2). Perception
    /// only. Empty (and absent in the log) when there are none or in logs from before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tracks: BTreeMap<String, TrackFacts>,
    /// Engine-filled: the intent's vehicles that have an engagement in play (pending,
    /// approved or dispatched), by the engagement's id. An agent's order on one of them
    /// always goes to a human. Absent when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub engaged: BTreeMap<Uid, IntentId>,
    /// Engine-filled (rules 5): the intent's vehicles that have an order a human approved
    /// waiting in the queue, by that intent's id. An agent's order on one of them goes to a
    /// human (it would supersede the human's order). Absent when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waiting: BTreeMap<Uid, IntentId>,
    /// Engine-filled for an engagement's signature (and its hold while signatures wait):
    /// the plan being signed, frozen by the first signature. Its checks read this plan, not a
    /// fresh preview. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Box<crate::engage::FrozenPlan>>,
    /// Engine-filled for an engagement's checks (rules 6): our vehicles whose position an
    /// operator accepted as unknown for it (`engage.accepted`), with the last report it
    /// names. The friendly keep-out leaves out a vehicle that is lost since exactly then.
    /// Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_accepted: BTreeMap<Uid, crate::engage::AcceptedUnknown>,
}

/// A decision's inputs as logged: the [`WorldSnapshot`] with the area set replaced by its
/// content hash ([`AreaSet::content_hash`]). The set itself is logged once, in
/// `run.started` or `areas.loaded`, and replay resolves the reference.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoggedInputs {
    pub t: Time,
    pub vehicles: BTreeMap<Uid, VehicleFacts>,
    /// None: no area set was loaded.
    pub areas_sha256: Option<String>,
    #[serde(default)]
    pub terrain: TerrainInputs,
    /// Collect and cover only (absent otherwise, so vehicle decisions log as before).
    #[serde(default, skip_serializing_if = "LoggedSpace::is_not_queried")]
    pub space: LoggedSpace,
    /// The track picture the decision was taken against (absent when empty).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tracks: BTreeMap<String, TrackFacts>,
    /// The intent's vehicles with an engagement in play (absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub engaged: BTreeMap<Uid, IntentId>,
    /// The intent's vehicles with a human-approved order waiting in the queue (absent when
    /// none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waiting: BTreeMap<Uid, IntentId>,
    /// The plan an engagement signature is on (absent otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Box<crate::engage::FrozenPlan>>,
    /// Our vehicles an operator accepted as lost for this engagement (rules 6; absent when
    /// none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_accepted: BTreeMap<Uid, crate::engage::AcceptedUnknown>,
}

impl LoggedInputs {
    /// `world` with its area set replaced by `areas_sha256` (the caller hashed it).
    pub fn from_world(world: &WorldSnapshot, areas_sha256: Option<String>) -> Self {
        LoggedInputs {
            t: world.t,
            vehicles: world.vehicles.clone(),
            areas_sha256,
            terrain: world.terrain.clone(),
            space: match &world.space {
                SpaceInputs::NotQueried => LoggedSpace::NotQueried,
                SpaceInputs::Unavailable { reason } => LoggedSpace::Unavailable {
                    reason: reason.clone(),
                },
                SpaceInputs::Answered { sha256, .. } => LoggedSpace::Answered {
                    sha256: sha256.clone(),
                },
            },
            tracks: world.tracks.clone(),
            engaged: world.engaged.clone(),
            waiting: world.waiting.clone(),
            plan: world.plan.clone(),
            unknown_accepted: world.unknown_accepted.clone(),
        }
    }

    /// The snapshot again, with `areas` the set the reference resolved to. A space answer
    /// reference can't be resolved here and becomes `Unavailable` (replay uses
    /// [`Self::resolve_with_space`]).
    pub fn resolve(&self, areas: Option<AreaSet>) -> WorldSnapshot {
        self.resolve_with_space(areas, |_| None)
    }

    /// [`Self::resolve`], with space answer references looked up by hash.
    pub fn resolve_with_space<'a>(
        &self,
        areas: Option<AreaSet>,
        answer: impl Fn(&str) -> Option<&'a SpaceAnswer>,
    ) -> WorldSnapshot {
        let space = match &self.space {
            LoggedSpace::NotQueried => SpaceInputs::NotQueried,
            LoggedSpace::Unavailable { reason } => SpaceInputs::Unavailable {
                reason: reason.clone(),
            },
            LoggedSpace::Answered { sha256 } => match answer(sha256) {
                Some(a) => SpaceInputs::Answered {
                    sha256: sha256.clone(),
                    answer: Box::new(a.clone()),
                },
                None => SpaceInputs::Unavailable {
                    reason: format!("space answer {sha256} not resolved"),
                },
            },
        };
        WorldSnapshot {
            t: self.t,
            vehicles: self.vehicles.clone(),
            areas,
            terrain: self.terrain.clone(),
            space,
            tracks: self.tracks.clone(),
            engaged: self.engaged.clone(),
            waiting: self.waiting.clone(),
            plan: self.plan.clone(),
            unknown_accepted: self.unknown_accepted.clone(),
        }
    }
}

impl WorldSnapshot {
    pub fn new(t: Time) -> Self {
        WorldSnapshot {
            t,
            ..Default::default()
        }
    }

    /// The part of the world an intent's rules can read: its vehicles (and a track target),
    /// the areas, terrain and the track picture. This is what gets logged (as
    /// [`LoggedInputs`], the area set replaced by its hash).
    pub fn subset_for(&self, spec: &IntentSpec) -> WorldSnapshot {
        let mut wanted: Vec<&Uid> = spec.uids.iter().collect();
        if let Task::Track { target } = &spec.task {
            wanted.push(target);
        }
        // an engagement's keep-out reads every vehicle of ours
        let vehicles = if matches!(spec.task, Task::Engage { .. }) {
            self.vehicles.clone()
        } else {
            wanted
                .into_iter()
                .filter_map(|u| self.vehicles.get(u).map(|v| (u.clone(), v.clone())))
                .collect()
        };
        WorldSnapshot {
            t: self.t,
            vehicles,
            areas: self.areas.clone(),
            terrain: self.terrain.clone(),
            // Engine-owned: filled from the space oracle for collect and cover.
            space: SpaceInputs::NotQueried,
            // The whole track picture: few tracks, and an engagement decision (Phase E)
            // must be replayable against exactly the picture it saw.
            tracks: self.tracks.clone(),
            // engine-owned: filled from its own intents
            engaged: BTreeMap::new(),
            waiting: BTreeMap::new(),
            plan: None,
            unknown_accepted: BTreeMap::new(),
        }
    }

    /// What the engine logs and evaluates for one intent: [`Self::subset_for`] plus terrain
    /// answers for its waypoints from `oracle` (only if some target needs a land/water
    /// check). Public so the bridge can build the same view, e.g. to prefetch terrain.
    pub fn for_intent(&self, spec: &IntentSpec, oracle: &dyn TerrainOracle) -> WorldSnapshot {
        self.for_intent_with(spec, oracle, &RulesConfig::default())
    }

    /// [`Self::for_intent`] with the rules configuration in force: an engagement's terrain
    /// point (its predicted effect point, for a boat or ground platform) depends on it.
    pub fn for_intent_with(
        &self,
        spec: &IntentSpec,
        oracle: &dyn TerrainOracle,
        cfg: &RulesConfig,
    ) -> WorldSnapshot {
        self.for_intent_plan(spec, oracle, cfg, None)
    }

    /// [`Self::for_intent_with`] for an engagement signature on `plan`: the plan goes into
    /// the inputs (its checks read it) and the terrain point is the plan's effect point.
    pub fn for_intent_plan(
        &self,
        spec: &IntentSpec,
        oracle: &dyn TerrainOracle,
        cfg: &RulesConfig,
        plan: Option<&crate::engage::FrozenPlan>,
    ) -> WorldSnapshot {
        let mut w = self.subset_for(spec);
        w.plan = plan.map(|p| Box::new(p.clone()));
        if matches!(spec.task, Task::Engage { .. }) {
            w.terrain = match crate::engage::terrain_point(spec, &w, cfg) {
                None => TerrainInputs::NotNeeded,
                Some(p) => match oracle.sample_near(w.t, p, crate::engage::TERRAIN_NEAR_M) {
                    Ok(near) => {
                        let (lon_e7, lat_e7) = p.e7();
                        let from = (near.distance_m > 0.0).then(|| {
                            let (lon_e7, lat_e7) = near.at.e7();
                            SampleFrom {
                                lon_e7,
                                lat_e7,
                                distance_m: (near.distance_m * 10.0).round() / 10.0,
                            }
                        });
                        TerrainInputs::Sampled {
                            samples: vec![TerrainSample {
                                lon_e7,
                                lat_e7,
                                surface: near.surface,
                                from,
                            }],
                        }
                    }
                    Err(reason) => TerrainInputs::Unavailable { reason },
                },
            };
            return w;
        }
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
                                from: None,
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
