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

impl VehicleFacts {
    /// Rules 15: when the vehicle was last heard, as its facts show: the newest of its
    /// heartbeat (`autopilot`), position and navigation status reports. None: never heard.
    pub fn heard_t(&self) -> Option<Time> {
        [
            self.autopilot.as_ref().map(|f| f.t),
            self.position.as_ref().map(|f| f.t),
            self.nav_ready.as_ref().map(|f| f.t),
        ]
        .into_iter()
        .flatten()
        .max()
    }
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
    /// Declared hostile (2525 "hostile", red): by an operator, or (rules 9) by the engine's
    /// hostile-act rule over logged facts. Never derived from identity evidence alone.
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
    /// Who declared the affiliation, overriding the estimate: an operator, or (rules 9) the
    /// engine's hostile-act rule (`rule:hostile_act`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
    /// Rules 9: the declaration in force, as the engine's identification ledger holds it: who
    /// or what declared it, on what evidence, confirmed by whom ([`crate::ident::Declaration`]).
    /// Engine-filled before every decision ([`crate::ident::Ledger::fill`]); absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<Box<crate::ident::Declaration>>,
    /// Rules 9: the review flags in force on it (open or confirmed), by number; a proximity
    /// flag raises it to suspect. Engine-filled; absent when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_flags: Vec<u64>,
    /// Rules 9: the tracker saw the contact manoeuvre within its memory (its velocity changed
    /// by more than its noise explains), so a prediction along its velocity a minute ahead is
    /// not to be trusted alone: a keep-out also counts it anywhere it reaches at the fastest
    /// it went ([`crate::engage::track_reach`]). Absent: no manoeuvre seen (or a log written
    /// before it existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manoeuvre: Option<Manoeuvre>,
    /// Rules 9: the track's average velocity over about the last minute (its displacement
    /// over it), east and north m/s: a contact that zigzags or weaves about a course keeps
    /// that average better than the leg it is on, so a keep-out also counts it along this
    /// ([`crate::engage::track_reach`]). Absent: too young a track (or an older log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_velocity_mps: Option<[f64; 2]>,
    /// Rules 12: when the tracker started this track (its first detection): the history behind
    /// its average velocity and its manoeuvre memory begins then. With it, a track that has a
    /// velocity but no average velocity yet is young (the tracker can't yet say whether it
    /// manoeuvred), and a keep-out counts it as possibly manoeuvring
    /// ([`crate::engage::track_reach`]). Absent: a caller that does not say (a scenario, an
    /// older log), judged as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_t: Option<Time>,
    /// Rules 14: the 1-sigma error of `velocity_mps` in its least certain direction, m/s, as the
    /// tracker reports it: how much faster than its velocity says an aircraft may be going
    /// ([`crate::engage::aircraft_reach_m`]); rules 18, also how far off its velocity may carry
    /// any track's predicted position ([`crate::engage::track_velocity_error`]). Absent: no
    /// velocity, a caller that does not say (a scenario that gives none: the policy's
    /// `track_velocity_sigma_mps` stands in), or an older log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_sigma_mps: Option<f64>,
}

/// Rules 16: an AIS identity as received (see [`WorldSnapshot::track_ais`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AisIdentity {
    pub mmsi: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ship_type: String,
}

/// Rules 9: a manoeuvre the tracker saw (see [`TrackFacts::manoeuvre`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manoeuvre {
    /// When it last saw the velocity change beyond its noise.
    pub t: Time,
    /// By how much it changed, m/s (the largest change within the memory).
    pub change_mps: f64,
    /// The fastest the contact went within the memory, with two sigmas of its velocity
    /// error, m/s: how far it may get per second, whichever way it turns.
    pub speed_mps: f64,
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
        /// Rules 17: the answers along each leg of the vehicles' routes ([`LegSurfaces`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        along: Vec<LegSurfaces>,
    },
}

/// Rules 17: what the land/water source answered at the points along one straight leg of a
/// route ([`crate::route::along`]), one letter a point in travel order ([`Surface::letter`]:
/// `w` water deeper than 0, `d` water no deeper, `l` land, `o` outside coverage). The rule works
/// the points out again from the leg's ends, which the logged facts give.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegSurfaces {
    /// The leg's start and end, `[lon, lat]` at MAVLink's 1e-7 degree resolution.
    pub from_e7: [i64; 2],
    pub to_e7: [i64; 2],
    pub surfaces: String,
    /// The farthest a point's answer was taken from the point, m, when the source answered
    /// some from a sample near them (at most [`crate::route::LEG_NEAR_M`]); absent: every point
    /// was answered itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_m: Option<f64>,
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
            TerrainInputs::Sampled { samples, .. } => samples
                .iter()
                .find(|s| s.lon_e7 == lon_e7 && s.lat_e7 == lat_e7)
                .map(|s| &s.surface),
            _ => None,
        }
    }

    /// Rules 17: the answers along the leg from `from` to `to`.
    pub fn along_for(&self, from: LonLat, to: LonLat) -> Option<&LegSurfaces> {
        let (f, t) = (from.e7(), to.e7());
        match self {
            TerrainInputs::Sampled { along, .. } => along
                .iter()
                .find(|l| l.from_e7 == [f.0, f.1] && l.to_e7 == [t.0, t.1]),
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
        /// Rules 13 ([`crate::rules::SPACE_REPLAY_SINCE`]): the engine time the answer was
        /// computed at (outside the engine's lock, or by the engine at the check). Absent
        /// under older rules.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computed_t: Option<Time>,
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
        /// Rules 13: when the answer was computed (engine time); absent in older logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computed_t: Option<Time>,
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
    /// Rules 16: the AIS identity each track's contact broadcasts, as our receivers last heard
    /// it, by track id (what it says it is: never verified, a contact may broadcast another's).
    /// A track that carries a lost contact's AIS is matched to it ([`crate::lost`]). Kept apart
    /// from [`TrackFacts`], whose shape the tracker's comparison copies fix. Absent when none
    /// (and in logs before rules 16).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub track_ais: BTreeMap<String, AisIdentity>,
    /// Rules 21: each track's identity level as the tracker settles it (a size class, a family,
    /// a kind) and the sensors whose looks identified it, by track id: what ends an identify
    /// order ([`crate::identify`]). Kept apart from [`TrackFacts`], whose shape the tracker's
    /// comparison copies fix. Absent when none (and in logs before rules 21).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub track_levels: BTreeMap<String, crate::identify::TrackLevel>,
    /// Engine-filled (rules 22, [`crate::plan`]): the legs of the intent's vehicles routed around
    /// the wrong surface or a no-go area, each with its grid, or why no route exists. The route
    /// layout follows them ([`crate::route::route_for`]). Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub route_plans: BTreeMap<Uid, Vec<crate::plan::PlannedLeg>>,
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
    /// Engine-filled (rules 7, [`crate::rules::QUEUE_FOLLOWUPS_SINCE`]) for a vehicle task:
    /// its vehicles whose own lost-link values an engagement of this run preloaded over and
    /// no confirmed restore has put back yet. A mission waits for them ([`crate::queue::reach`]).
    /// Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub restore_owed: BTreeMap<Uid, crate::queue::RestoreOwed>,
    /// Engine-filled (rules 12): the intent's vehicles spent by their own effect (a loitering
    /// munition after its release: `restore.closed`, expended), by the engagement that released
    /// it. No order or nomination is taken for one (`readiness` fails). Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expended: BTreeMap<Uid, IntentId>,
    /// Engine-filled (rules 15): the intent's vehicles spent by their own effect in an earlier
    /// run (carried over at start), with the vehicle under the uid now: one that is not known to
    /// be another takes no order or nomination (`readiness` fails). Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spent_carried: BTreeMap<Uid, crate::queue::CarriedSpent>,
    /// Rules 12, the bridge's: each fleet vehicle's identity as it knows it now (platform,
    /// callsign, system id, home). Never logged as such: the engine copies the identity of a
    /// vehicle still owed values carried over from an earlier run into that fact
    /// (`restore_owed`, [`crate::queue::CarriedHold`]), the only thing that reads it. Empty
    /// where the caller gives none (scenarios, tests): carried values then hold.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub identities: BTreeMap<Uid, crate::engage::VehicleIdentity>,
    /// Engine-filled for an identify order's steps (rules 23): the callsigns of the vehicle sent
    /// and of the vehicles that identified its track, from [`Self::identities`], so the step's
    /// words can name them and replay says the same. Absent otherwise.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub callsigns: BTreeMap<Uid, String>,
    /// Engine-filled for an engagement's checks (rules 16, [`crate::lost`]): the contacts the
    /// tracker dropped that the engine still holds as lost (`track.dropped`, not retired by
    /// `track.reacquired`), by the track id they were dropped under, each with its facts at the
    /// drop (its identification filled from the ledger as a live track's). The keep-out on
    /// tracks counts each anywhere it could have reached since its last fix. Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lost_contacts: BTreeMap<String, crate::lost::LostContact>,
    /// Engine-filled for an engagement's checks (rules 16): the lost contacts an operator
    /// accepted for it (`engage.accepted` naming a contact), each since the last fix it names.
    /// The keep-out leaves out a contact lost since exactly then. Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lost_accepted: BTreeMap<String, crate::engage::AcceptedUnknown>,
    /// Engine-filled for an engagement's checks (rules 19): the live tracks an operator accepted
    /// for it as behaving now (`engage.accepted` naming a track), each with its facts then. The
    /// keep-out on tracks counts such a track along its accepted path and its course now while
    /// the acceptance holds on this picture ([`crate::engage::behaving_lapse`]). Absent when none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub behaving_accepted: BTreeMap<String, crate::engage::AcceptedBehaving>,
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
    /// The AIS identities heard for its tracks (rules 16; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub track_ais: BTreeMap<String, AisIdentity>,
    /// Its tracks' identity levels (rules 21; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub track_levels: BTreeMap<String, crate::identify::TrackLevel>,
    /// Its vehicles' routed legs (rules 22; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub route_plans: BTreeMap<Uid, Vec<crate::plan::PlannedLeg>>,
    /// An identify step's callsigns (rules 23; absent otherwise).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub callsigns: BTreeMap<Uid, String>,
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
    /// The intent's vehicles still owed their own lost-link values (rules 7; absent when
    /// none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub restore_owed: BTreeMap<Uid, crate::queue::RestoreOwed>,
    /// The intent's vehicles spent by their own effect (rules 12; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expended: BTreeMap<Uid, IntentId>,
    /// The intent's vehicles spent by their own effect in an earlier run (rules 15; absent when
    /// none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spent_carried: BTreeMap<Uid, crate::queue::CarriedSpent>,
    /// An engagement's lost contacts (rules 16; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lost_contacts: BTreeMap<String, crate::lost::LostContact>,
    /// The lost contacts accepted for this engagement (rules 16; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lost_accepted: BTreeMap<String, crate::engage::AcceptedUnknown>,
    /// The live tracks accepted for this engagement as behaving now (rules 19; absent when none).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub behaving_accepted: BTreeMap<String, crate::engage::AcceptedBehaving>,
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
                SpaceInputs::Answered {
                    sha256, computed_t, ..
                } => LoggedSpace::Answered {
                    sha256: sha256.clone(),
                    computed_t: *computed_t,
                },
            },
            tracks: world.tracks.clone(),
            track_ais: world.track_ais.clone(),
            track_levels: world.track_levels.clone(),
            route_plans: world.route_plans.clone(),
            callsigns: world.callsigns.clone(),
            engaged: world.engaged.clone(),
            waiting: world.waiting.clone(),
            plan: world.plan.clone(),
            unknown_accepted: world.unknown_accepted.clone(),
            restore_owed: world.restore_owed.clone(),
            expended: world.expended.clone(),
            spent_carried: world.spent_carried.clone(),
            lost_contacts: world.lost_contacts.clone(),
            lost_accepted: world.lost_accepted.clone(),
            behaving_accepted: world.behaving_accepted.clone(),
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
            LoggedSpace::Answered { sha256, computed_t } => match answer(sha256) {
                Some(a) => SpaceInputs::Answered {
                    sha256: sha256.clone(),
                    answer: Box::new(a.clone()),
                    computed_t: *computed_t,
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
            track_ais: self.track_ais.clone(),
            track_levels: self.track_levels.clone(),
            route_plans: self.route_plans.clone(),
            callsigns: self.callsigns.clone(),
            engaged: self.engaged.clone(),
            waiting: self.waiting.clone(),
            plan: self.plan.clone(),
            unknown_accepted: self.unknown_accepted.clone(),
            restore_owed: self.restore_owed.clone(),
            expended: self.expended.clone(),
            spent_carried: self.spent_carried.clone(),
            identities: BTreeMap::new(),
            lost_contacts: self.lost_contacts.clone(),
            lost_accepted: self.lost_accepted.clone(),
            behaving_accepted: self.behaving_accepted.clone(),
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
            track_ais: self.track_ais.clone(),
            track_levels: self.track_levels.clone(),
            // engine-owned (rules 22): planned for this intent
            route_plans: BTreeMap::new(),
            // engine-owned: filled from its own intents
            engaged: BTreeMap::new(),
            waiting: BTreeMap::new(),
            plan: None,
            unknown_accepted: BTreeMap::new(),
            restore_owed: BTreeMap::new(),
            expended: BTreeMap::new(),
            spent_carried: BTreeMap::new(),
            // rules 12: the bridge's identities of the intent's vehicles (a carried restore's
            // hold reads them)
            identities: spec
                .uids
                .iter()
                .filter_map(|u| self.identities.get(u).map(|i| (u.clone(), i.clone())))
                .collect(),
            // engine-owned (rules 23): filled for an identify order's steps
            callsigns: BTreeMap::new(),
            // engine-owned (rules 16): filled from its ledger of lost contacts
            lost_contacts: BTreeMap::new(),
            lost_accepted: BTreeMap::new(),
            // engine-owned (rules 19): filled from its engagement's acceptances
            behaving_accepted: BTreeMap::new(),
        }
    }

    /// What the engine logs and evaluates for one intent: [`Self::subset_for`] plus terrain
    /// answers for its waypoints from `oracle` (only if some target needs a land/water
    /// check), and from rules 17 for the points along its vehicles' routes. Public so the
    /// bridge can build the same view, e.g. to prefetch terrain.
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
        // rules 21: an identify order's terrain is its orbit's (laid out around the track)
        let orbit = crate::identify::orbit_view(spec, self, cfg);
        let spec = orbit.as_ref();
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
                            along: Vec::new(),
                        }
                    }
                    Err(reason) => TerrainInputs::Unavailable { reason },
                },
            };
            Self::sample_legs(&mut w, spec, oracle, cfg);
            return w;
        }
        let routed = |w: &mut WorldSnapshot| {
            // rules 22: legs the domain rule or the geofence would refuse are routed first, and
            // the points along the routed legs sampled again
            if cfg.route_plan() && !matches!(w.terrain, TerrainInputs::Unavailable { .. }) {
                let plans = crate::plan::plans_for(spec, w, oracle, cfg);
                if !plans.is_empty() {
                    w.route_plans = plans;
                    Self::sample_legs(w, spec, oracle, cfg);
                }
            }
        };
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
                    along: Vec::new(),
                },
                // A wrong-length answer is a broken oracle, not a missing one: no samples,
                // so the domain rule fails rather than skipping.
                Ok(_) => TerrainInputs::Sampled {
                    samples: Vec::new(),
                    along: Vec::new(),
                },
                Err(reason) => TerrainInputs::Unavailable { reason },
            },
            _ => TerrainInputs::NotNeeded,
        };
        Self::sample_legs(&mut w, spec, oracle, cfg);
        routed(&mut w);
        w
    }

    /// Rules 17 ([`crate::rules::ROUTE_SURFACE_SINCE`]): the answers along every leg the domain
    /// rule judges ([`crate::route::surface_legs`]), in one query, beside the waypoints' (or the
    /// effect point's). A source that can't answer them all leaves the terrain unavailable, as
    /// it does for a waypoint (the rule records "not evaluated"). Nothing under older rules.
    fn sample_legs(w: &mut WorldSnapshot, spec: &IntentSpec, oracle: &dyn TerrainOracle, cfg: &RulesConfig) {
        if !cfg.route_surface() || matches!(w.terrain, TerrainInputs::Unavailable { .. }) {
            return;
        }
        let legs: Vec<(crate::route::RouteLeg, Vec<(LonLat, f64)>)> = crate::route::surface_legs(spec, w, cfg)
            .into_iter()
            .filter_map(|l| {
                let pts = crate::route::along(l.from, l.to)?;
                (!pts.is_empty()).then_some((l, pts))
            })
            .collect();
        if legs.is_empty() {
            return;
        }
        let points: Vec<LonLat> = legs.iter().flat_map(|(_, pts)| pts.iter().map(|(p, _)| *p)).collect();
        let answers = match oracle.sample_along(w.t, &points, crate::route::LEG_NEAR_M) {
            Ok(a) if a.len() == points.len() => a,
            // a wrong-length answer is a broken oracle, not a missing one: no answers along the
            // legs, so the rule fails rather than skipping (as for the waypoints)
            Ok(_) => Vec::new(),
            Err(reason) => {
                w.terrain = TerrainInputs::Unavailable { reason };
                return;
            }
        };
        let mut answers = answers.into_iter();
        let along: Vec<LegSurfaces> = legs
            .iter()
            .map(|(l, pts)| {
                let mut near: f64 = 0.0;
                let surfaces: String = answers
                    .by_ref()
                    .take(pts.len())
                    .map(|a| {
                        near = near.max(a.distance_m);
                        a.surface.letter()
                    })
                    .collect();
                let (f, t) = (l.from.e7(), l.to.e7());
                LegSurfaces {
                    from_e7: [f.0, f.1],
                    to_e7: [t.0, t.1],
                    surfaces,
                    near_m: (near > 0.0).then(|| (near * 10.0).round() / 10.0),
                }
            })
            .collect();
        match &mut w.terrain {
            TerrainInputs::Sampled { along: a, .. } => *a = along,
            other => {
                *other = TerrainInputs::Sampled {
                    samples: Vec::new(),
                    along,
                }
            }
        }
    }
}
