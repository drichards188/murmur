//! Rules 21 ([`crate::rules::IDENTIFY_SINCE`], bd murmur-bia, DECISIONS.md "W1 follow-up: an
//! identify order"): an order that sends one vehicle to identify one track.
//!
//! The engine lays the order out as an orbit around the track's position now, at a standoff
//! from which the vehicle's EO/IR identifies a contact of the track's domain ([`plan`]). The
//! vehicle rules (readiness, the platform's turn, the domain rule, the geofence, reserves) judge
//! that orbit as they judge any orbit ([`orbit_spec`]). Once dispatched, the engine steps the
//! order every second ([`crate::Engine::step_identify`]): it moves the orbit with the track
//! ([`recentre`], each move logged before it goes out), and ends the order when the track is
//! identified, lost or out of time ([`outcome`]); the vehicle then goes back to the order the
//! identify replaced on it, if that order passes its checks again, or holds. While the vehicle
//! is on it, its camera's payload manager looks for the track first (docs/SENSING.md,
//! "Pointing"). Every step is logged with the picture it read (`identify.step`), so replay
//! recomputes it.
//!
//! Rules 31 ([`crate::rules::IDENTIFY_CLASS_SINCE`], bd murmur-8vj8): the standoff is taken from
//! the camera's reach of a contact of the size class the picture shows the track as
//! ([`shown_class`]: a detection's size class, a recognition's family, an identification's kind),
//! where the vehicle's facts carry reaches by class (the physical model's), else of its domain as
//! before: a small drone is 0.35 m across where the domain's reference aircraft is 3 m, so a
//! camera identifies one only from much nearer (a Puma about 1.1 km out by day, where it would
//! identify the reference aircraft from nearly 10 km).
//!
//! Rules 32 ([`crate::rules::REACH_NOW_SINCE`], bd murmur-ahss): where the cameras' facts carry
//! physical reaches, the reach is never more than that eighth, and the camera's identification
//! reach under the conditions where the vehicle is now where that is less (its `reach_now` fact,
//! [`crate::reach`]: the physical model's own figure, slant range, of the class's smallest member
//! or the domain's reference contact): the conditions only take reach away. In fog of 50 m
//! visibility a Puma identifies a small drone only within about 160 m, and a camera with only a
//! visible channel identifies nothing at night. The order is refused where that reach is nothing,
//! or nearer than the vehicle's height or its tightest orbit keeps it, and the refusal says the
//! conditions and the reach in them ("in fog of 50 m visibility PUMA 1's camera identifies a
//! small_air contact only within about 160 m: its tightest orbit, 66 m, keeps it beyond that");
//! the plan records the conditions that shortened it ([`Plan::conditions`]). Without a fresh reach
//! now it cannot be evaluated. Why the cap: on a clear day the model's own identification reach of a
//! boat is several times the eighth (a Puma's of a small fast boat about 10.7 km), but the camera's
//! payload manager cues a track only within the catalog's `range_m` (4 km for a Puma), so an orbit
//! planned at the model's figure would be flown where the camera never looks at the track.
//!
//! Rules 23 ([`crate::rules::IDENTIFY_OWN_LOOK_SINCE`], the owner's Harbor watch run of
//! 2026-10-27 17:11Z, whose identify orders another vehicle's camera ended): the look ends identified
//! only on an identification by the vehicle sent, with its own sensors, made after the order went
//! out ([`TrackLevel::own`]). Another vehicle's identification meanwhile is logged as a step
//! ([`Step::Meanwhile`], [`meanwhile_due`]) and said in the outcome; the order goes on.
//!
//! Rules 33 ([`crate::rules::IDENTIFY_CRITERIA_SINCE`], docs/SCENARIO-SETTINGS.md; the F8 boat
//! whose order ended at its first naming at 0.11 while the brief counted 0.6): "identified" is the
//! scenario's. The look ends identified only on the vehicle sent's own look ([`OwnLook`]: its
//! sensor, level and confidence) that meets its track's domain criteria in the policy's `[watch]`
//! ([`crate::watch::DomainCriteria`]): the level at or above the domain's, one of the domain's
//! sensors (AIS never: an identify order is for a look of our own), the confidence at or above the
//! bar, and no identity conflict on the track. A first naming that falls short is logged once for
//! each kind ([`Step::Named`]: "PUMA 1 named it small_fast_boat at 0.11, not yet identified (the
//! bar is 0.6); it goes on") and the order goes on; another vehicle's look is noted meanwhile only
//! when it meets them. The end says the look's confidence and the criteria ("track T-0007
//! identified as small_fast_boat at 0.71 by PUMA 1's eo_ir (the bar is 0.6, identification by
//! EO/IR)"), the timeout the vehicle's best look, and the time on station is the policy's
//! `identify_on_station_s`. Under older rules every word stays as it was ([`crate::legacy`]).

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{SensorFact, SizeClass};
use crate::geo::{LonLat, haversine_m};
use crate::ids::{IntentId, Uid};
use crate::intent::{IntentSpec, Task, TaskKind};
use crate::rules::{CheckResult, Outcome, RuleId, RulesConfig};
use crate::time::Time;
use crate::watch::WatchSettings;
use crate::world::{TrackFacts, VehicleFacts, WorldSnapshot};

/// The share of a camera's detection reach out to which it identifies [E]: the physical model's
/// pixels on a target for an even chance to detect over those to identify (`eo::P50_PX`, 1.5 and
/// 12, Johnson's criteria as the model takes them: docs/SENSING.md, "EO/IR"), where the pixels
/// alone limit the range. Where the air and the optics blur a far contact first, as they do at
/// most of the catalog's detection reaches, the camera identifies further out than this, so an
/// orbit inside it is inside the camera's identification reach. A test in the bridge holds it to
/// the model's figures.
pub const IDENTIFY_PX_SHARE: f64 = 1.5 / 12.0;

/// The share of that identification reach the default standoff takes [E]: slant range from the
/// vehicle to the contact, so a vehicle orbiting there has a margin for the track's own error
/// and the haze of the day.
pub const STANDOFF_SHARE: f64 = 0.7;

/// The orbit moves with the track once the track's predicted position is this share of the
/// standoff from the orbit's centre, and at least [`RECENTRE_MIN_M`] [E]: often enough that the
/// camera keeps it within reach, seldom enough not to re-upload a mission every few seconds.
pub const RECENTRE_SHARE: f64 = 0.5;
pub const RECENTRE_MIN_M: f64 = 100.0;

/// Time on station before an identify order gives up, after the time to get there, under rules
/// below 33 ([`crate::legacy::IDENTIFY_ON_STATION_S`], where the reasons are); from rules 33 the
/// policy's `[watch] identify_on_station_s`.
pub use crate::legacy::IDENTIFY_ON_STATION_S as ON_STATION_S;

/// A cruise speed below this (or none) counts as this for the time to get there, m/s.
const MIN_TRANSIT_MPS: f64 = 1.0;

/// How specific a track's identity is (the bridge's `identity.level`): a size class, a family,
/// a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Detect,
    Recognise,
    Identify,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Detect => "detect",
            Level::Recognise => "recognise",
            Level::Identify => "identify",
        }
    }

    /// Rules 33: the level as a noun in an identify order's or an engagement check's words.
    pub fn noun(self) -> &'static str {
        match self {
            Level::Detect => "detection",
            Level::Recognise => "recognition",
            Level::Identify => "identification",
        }
    }
}

/// Rules 21: a track's identity level as the bridge's tracker settles it, and the sensors whose
/// looks reached identification, as `"<uid>:<sensor>"` (`"ais"` for an AIS identity's kind).
/// [`WorldSnapshot::track_levels`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackLevel {
    pub level: Level,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by: Vec<String>,
    /// Rules 22: when one of our own sensors (not AIS) last identified it (none: none has).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_t: Option<Time>,
    /// Rules 22: the tracker's identity conflict on it, when it has one (an AIS identity our
    /// sensors contradict, a kind two sensors disagree on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    /// Rules 23: each of our vehicles' latest identification of it with its own sensors (not
    /// AIS), by the vehicle's uid: when, and the kind it gave. Rules 33: its recognitions too
    /// (a family names it), each with its sensor, level and confidence ([`OwnLook`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub own: BTreeMap<String, OwnLook>,
}

/// Rules 23: one vehicle's latest identification of a track by its own sensors. Rules 33
/// ([`crate::rules::IDENTIFY_CRITERIA_SINCE`]): its most specific look (an identification over a
/// recognition, then the latest), with the sensor that made it, its level and its confidence, what
/// an identify order's end is judged on ([`outcome`]). The three are filled and logged (with the
/// picture, so its hash covers them) from rules 33 only, and dropped from the facts of older
/// rules, so their pictures hash as they always did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OwnLook {
    pub t: Time,
    /// The kind it identified it as (none where the tracker can't say); rules 33, the family of a
    /// recognition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Rules 33: the sensor that made the look (`eo_ir`, `sonar`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<String>,
    /// Rules 33: the look's level (`recognise`, `identify`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    /// Rules 33: how sure the look is of its kind (0..1), the surest it has been of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

impl OwnLook {
    /// The look as rules below 33 log it: without its details.
    pub fn without_details(&mut self) {
        self.sensor = None;
        self.level = None;
        self.confidence = None;
    }
}

/// The orbit an identify order is flown as, and what it rests on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub track: String,
    pub center: LonLat,
    pub radius_m: f64,
    /// The camera's identification reach the standoff was taken from, slant metres.
    pub reach_m: f64,
    /// Rules 31: the size class that reach is for (the track's as the picture shows it); none:
    /// for a contact of the track's domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_class: Option<SizeClass>,
    /// Rules 32: the conditions where the vehicle is that the reach is for, in words, when they
    /// shorten it (the camera's identification reach now is less than a clear day's planning
    /// reach): "in fog of 50 m visibility", "at night". None: a clear day's reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<String>,
    /// The standoff came from the order (an operator's), not from the reach.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub set_by_order: bool,
    /// Time to get to the orbit at the vehicle's cruise speed, s.
    pub transit_s: f64,
}

/// What an identify order is dispatched with (`intent.dispatched`'s `identify`): its plan, its
/// deadline ([`deadline`] from the dispatch's time) and the order on its vehicle it replaces and
/// goes back to afterwards (the vehicle's newest order then, of a kind it can go back to, still
/// under way or done).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dispatch {
    #[serde(flatten)]
    pub plan: Plan,
    pub deadline_t: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumes: Option<IntentId>,
    /// Rules 22: when it went out, and the identity the track had then (its kind and level),
    /// which a fresh identification is compared with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_t: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// How an identify order's look ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndKind {
    /// The track's identity reached identification (any of our sensors).
    Identified,
    /// The track left the picture or the tracker dropped it (a lost contact).
    Lost,
    /// Not identified by the order's deadline.
    TimedOut,
}

impl EndKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EndKind::Identified => "identified",
            EndKind::Lost => "lost",
            EndKind::TimedOut => "timed_out",
        }
    }
}

/// One logged step of a dispatched identify order (`identify.step`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// The orbit moved with the track: sent as a follow of the order.
    Recentred {
        center: LonLat,
        radius_m: f64,
        checks: Vec<CheckResult>,
    },
    /// The orbit would have moved, but a check failed on the new one: the vehicle stays on the
    /// orbit it flies. Logged once for each reason.
    Held {
        center: LonLat,
        radius_m: f64,
        checks: Vec<CheckResult>,
        reason: String,
    },
    /// Rules 23: another of our vehicles identified the track after the order went out; the order
    /// goes on for its own vehicle's look. Logged once for each vehicle and kind. Rules 33: a look
    /// that meets the track's criteria.
    Meanwhile {
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
        detail: String,
    },
    /// Rules 33: the vehicle sent named the track with a look of its own that falls short of the
    /// criteria (below the bar, a recognition where identification counts, a sensor that does not,
    /// an identity in conflict); the order goes on. Logged once for each kind it names.
    Named {
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
        detail: String,
    },
    /// The look ended; the vehicle goes back to `resumes`'s task when that passed its checks
    /// again (`checks`), else holds (`returns` is the task sent).
    Ended {
        outcome: EndKind,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<Level>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes: Option<IntentId>,
        returns: Task,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<CheckResult>,
    },
}

/// What the engine holds for a dispatched identify order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Run {
    /// The orbit flown now (the dispatch's, then each re-centre's).
    pub plan: Plan,
    pub deadline_t: Time,
    /// The order the identify replaced on its vehicle, which it goes back to.
    pub resumes: Option<IntentId>,
    /// The last re-centre refusal's reason (logged once each).
    pub held: Option<String>,
    /// How the look ended, and what it said.
    pub ended: Option<(EndKind, String)>,
    /// Rules 22: when it went out, and the track's identity then.
    pub sent_t: Option<Time>,
    pub before: Option<String>,
    /// Rules 23: the other vehicles' identifications noted meanwhile, in order.
    pub meanwhile: Vec<Noted>,
    /// Rules 33: the vehicle sent's namings below the criteria noted so far ([`Step::Named`]), in
    /// order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub named: Vec<Noted>,
}

/// Rules 23: another vehicle's identification an identify order noted meanwhile: by whom (its
/// uid), as what, and when it was noted (the step's time; the bridge's inbox alert says it).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Noted {
    pub by: String,
    pub kind: Option<String>,
    pub t: Time,
}

/// An identify order's time on station under `cfg`, s: rules 33, the policy's `[watch]
/// identify_on_station_s`, before them [`ON_STATION_S`]. The deadline's ([`deadline`]), and the
/// link predictor's for an identify order (the bridge's `link_coverage`).
pub fn on_station_s(cfg: &RulesConfig) -> f64 {
    if cfg.identify_criteria() {
        cfg.watch().identify_on_station_s
    } else {
        crate::legacy::IDENTIFY_ON_STATION_S
    }
}

/// The order's deadline: its dispatch, the time to get there, then its time on station
/// ([`on_station_s`]).
pub fn deadline(dispatched_t: Time, plan: &Plan, cfg: &RulesConfig) -> Time {
    dispatched_t.plus_ms(((plan.transit_s + on_station_s(cfg)) * 1000.0).round() as i64)
}

/// The kinds an identify goes back to afterwards: the vehicle tasks one vehicle can be sent
/// again (an engagement never; another identify never: its look is over).
pub fn resumable(kind: TaskKind) -> bool {
    matches!(kind, TaskKind::Orbit | TaskKind::Patrol | TaskKind::Hold | TaskKind::Rtl)
}

fn params(spec: &IntentSpec) -> Option<(&str, Option<f64>)> {
    match &spec.task {
        Task::Identify { track, standoff_m } => Some((track.as_str(), *standoff_m)),
        _ => None,
    }
}

/// The platform's tightest orbit, m: its minimum turn radius (or the fixed-wing default) times
/// the rules' margin, as the platform-limits rule takes it; none for a vehicle that turns in
/// place.
fn min_radius(v: &VehicleFacts, cfg: &RulesConfig) -> Option<f64> {
    let p = v.platform.as_ref()?;
    let fixed_wing = crate::engage::platform_autopilot(v).is_some_and(|a| a.is_fixed_wing());
    let min = p
        .min_turn_radius_m
        .or_else(|| fixed_wing.then_some(cfg.fixed_wing_default_min_turn_radius_m))?;
    Some((min * cfg.orbit_radius_margin).ceil())
}

/// Rules 32: whether `v`'s identify plan reads its cameras' reach now ([`crate::reach`]): under
/// rules 32, with a camera whose facts carry a physical reach.
fn reach_now(v: &VehicleFacts, cfg: &RulesConfig) -> bool {
    cfg.reach_now() && cameras(v).any(|s| s.reach_m.is_some())
}

/// Rules 32: what a contact is called in a camera's reach: "a small_air contact", "a contact in the
/// surface domain".
fn contact_words(class: Option<SizeClass>, domain: crate::catalog::Domain) -> String {
    match class {
        Some(c) => format!("a {} contact", c.as_str()),
        None => format!("a contact in the {} domain", domain.as_str()),
    }
}

/// Rules 32: the conditions in words, then the vehicle: "in fog of 50 m visibility PUMA 1's
/// camera", "PUMA 1's camera" on a clear day.
fn camera_words(world: &WorldSnapshot, uid: &Uid, rn: &crate::reach::ReachNow) -> String {
    let who = name(world, uid.as_str());
    match rn.conditions() {
        Some(c) => format!("{c} {who}'s camera"),
        None => format!("{who}'s camera"),
    }
}

/// Rules 32: a camera's identification reach now in words: "in fog of 50 m visibility PUMA 1's
/// camera identifies a small_air contact only within about 140 m".
fn only_within(
    world: &WorldSnapshot,
    uid: &Uid,
    rn: &crate::reach::ReachNow,
    class: Option<SizeClass>,
    domain: crate::catalog::Domain,
    reach: f64,
) -> String {
    format!("{} identifies {} only within about {reach:.0} m", camera_words(world, uid, rn), contact_words(class, domain))
}

/// Rules 32: why a camera that identifies nothing now is refused: "at night MAGURA 1's camera
/// identifies no contact in the surface domain from where it is now: it has no thermal channel",
/// "MQ-9B 1's camera identifies no small_air contact from where it is now, 7600 m up".
fn nowhere(
    world: &WorldSnapshot,
    uid: &Uid,
    rn: &crate::reach::ReachNow,
    class: Option<SizeClass>,
    domain: crate::catalog::Domain,
    thermal: bool,
) -> String {
    let what = contact_words(class, domain);
    let what = what.strip_prefix("a ").unwrap_or(&what);
    let dark = !thermal && rn.sun_elevation_deg < crate::reach::NIGHT_DEG;
    let up = if rn.alt_m >= 1.0 { format!(", {:.0} m up", rn.alt_m) } else { String::new() };
    format!(
        "{} identifies no {what} from where it is now{up}{}",
        camera_words(world, uid, rn),
        if dark { ": it has no thermal channel" } else { "" }
    )
}

/// The vehicle's EO/IR reach by `reach`, m: the longest of its cameras'.
fn camera_reach(v: &VehicleFacts, reach: impl Fn(&SensorFact) -> Option<f64>) -> Option<f64> {
    cameras(v)
        .filter_map(reach)
        .fold(None, |best: Option<f64>, r| Some(best.map_or(r, |b| b.max(r))))
}

fn cameras(v: &VehicleFacts) -> impl Iterator<Item = &SensorFact> {
    v.platform.iter().flat_map(|p| p.sensors.iter()).filter(|s| s.kind == "eo_ir")
}

/// The words ESM's detections name a contact's domain with (the bridge's `tracks::model`
/// `domain_label`), three of them a family's too.
const ESM_DOMAIN_WORDS: [&str; 4] = ["aircraft", "vessel", "ground_vehicle", "submerged"];

/// Rules 33: the level a picture's label says (a size class's name a detection's, a family's a
/// recognition's, a kind's an identification's); none for a label of none of them. The scenario
/// runner gives a track the file names without a level this one, as the bridge's tracker gives
/// every named track its own.
pub fn label_level(label: &str) -> Option<Level> {
    if SizeClass::named(label).is_some() {
        Some(Level::Detect)
    } else if SizeClass::of_family(label).is_some() {
        Some(Level::Recognise)
    } else if SizeClass::of_kind(label).is_some() {
        Some(Level::Identify)
    } else {
        None
    }
}

/// Rules 31: the size class the picture shows `track` as, from its shown identity read at its
/// level (`level`, the tracker's [`TrackLevel`]): at detection a size class's name, ESM's words
/// for a domain (`aircraft`, `vessel`, `ground_vehicle`, `submerged`) saying no size; at
/// recognition a family's class, at identification a kind's ([`SizeClass::of_label`]); with no
/// level given, any label but ESM's words. None when the identity names no size, or a class of
/// another domain than the track's. A kind or family is read as its whole class, not as itself:
/// the class's reach is its smallest member's ([`crate::catalog::ClassReach`]).
pub fn shown_class(track: &TrackFacts, level: Option<Level>) -> Option<SizeClass> {
    let label = track.identity.as_deref()?;
    let class = match level {
        Some(Level::Detect) => SizeClass::named(label),
        Some(_) => SizeClass::of_label(label),
        None if ESM_DOMAIN_WORDS.contains(&label) => None,
        None => SizeClass::of_label(label),
    }?;
    (class.domain() == track.domain).then_some(class)
}

/// The orbit an identify order is flown as on `world`, or why there is none.
pub fn plan(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Result<Plan, String> {
    let (track_id, standoff) = params(spec).ok_or("not an identify order")?;
    if !cfg.identify() {
        return Err(format!(
            "these decision rules (version {}) take no identify order (from version {})",
            cfg.rules_version,
            crate::rules::IDENTIFY_SINCE
        ));
    }
    let [uid] = spec.uids.as_slice() else {
        return Err("identify names exactly one vehicle".into());
    };
    let v = world.vehicles.get(uid).ok_or("vehicle not in the world model")?;
    let track = live_track(track_id, world)?;
    // rules 22: any live contact may be looked at again; one out of view is not live
    if cfg.identify_second_look() && crate::engage::out_of_view(track, world.t, &cfg.engage_rules()) {
        return Err(format!(
            "track {track_id} is out of view (unseen for {:.0} s): send it to a contact our sensors see now",
            world.t.ms_since(track.last_seen_t) as f64 / 1000.0
        ));
    }
    if let Some(l) = world.track_levels.get(track_id)
        && l.level == Level::Identify
        && !cfg.identify_second_look()
    {
        return Err(format!(
            "track {track_id} is identified already ({})",
            track.identity.as_deref().unwrap_or("its kind")
        ));
    }
    // rules 31: the reach of a contact of the track's shown size class, where the cameras' facts
    // carry reaches by class
    let class = cfg
        .identify_by_class()
        .then(|| shown_class(track, world.track_levels.get(track_id).map(|l| l.level)))
        .flatten()
        .filter(|c| cameras(v).any(|s| s.reach_for_class(*c).is_some()));
    let no_camera = || {
        let platform = v.platform.as_ref().map_or("its platform", |p| p.id.as_str());
        match class {
            Some(c) => format!("{platform} carries no EO/IR that reaches a contact the picture shows as {}", c.as_str()),
            None => format!("{platform} carries no EO/IR that reaches a contact in the {} domain", track.domain.as_str()),
        }
    };
    // an eighth of the cameras' clear-day detection reach of the class (else of the domain)
    let clear = match class {
        Some(c) => camera_reach(v, |s| s.reach_for_class(c).flatten()),
        None => camera_reach(v, |s| s.reach_for(track.domain)),
    }
    .ok_or_else(no_camera)?
        * IDENTIFY_PX_SHARE;
    // rules 32: never more than that, and the cameras' identification reach under the conditions
    // where the vehicle is now where that is less (the conditions only take reach away), where
    // their facts carry physical reaches; `limited`: the reach now, which limits it
    let now = if reach_now(v, cfg) { Some(crate::reach::fresh(v, world, cfg)?) } else { None };
    let (reach, limited) = match now {
        Some(rn) => {
            let cam = crate::reach::identify_now(v, rn, class, track.domain).ok_or_else(no_camera)?;
            if cam.reach_m <= 0.0 {
                return Err(nowhere(world, uid, rn, class, track.domain, cam.thermal));
            }
            if cam.reach_m < clear { (cam.reach_m, Some(rn)) } else { (clear, None) }
        }
        None => (clear, None),
    };
    let of_class = class.map(|c| format!(" for a {} contact", c.as_str())).unwrap_or_default();
    // rules 32: a refusal the conditions caused says them and the camera's reach in them first
    let refuse = |why: String| match limited {
        Some(rn) => format!("{}: {why}", only_within(world, uid, rn, class, track.domain, reach)),
        None => why,
    };
    // the camera's slant range: the vehicle's height over the contact counts
    let height = match (&v.alt_msl_m, track.alt_m) {
        (Some(a), Some(t)) => (a.value - t).abs(),
        (Some(a), None) => a.value.max(0.0),
        _ => 0.0,
    };
    if height >= reach {
        return Err(refuse(match limited {
            Some(_) => format!("its height over the contact, {height:.0} m, is beyond that"),
            None => format!(
                "its height over the contact, {height:.0} m, is beyond its camera's identification reach of about {reach:.0} m{of_class}"
            ),
        }));
    }
    let tightest = min_radius(v, cfg).unwrap_or(0.0);
    let ground = |slant: f64| (slant * slant - height * height).max(0.0).sqrt();
    let radius = match standoff {
        Some(r) => r,
        None => ground(reach * STANDOFF_SHARE).max(tightest).max(1.0).ceil(),
    };
    if standoff.is_none() && radius.hypot(height) > reach {
        return Err(refuse(match limited {
            Some(_) => format!("its tightest orbit, {tightest:.0} m, keeps it beyond that"),
            None => format!(
                "its tightest orbit, {tightest:.0} m, keeps its camera beyond its identification reach of about {reach:.0} m{of_class}"
            ),
        }));
    }
    let center = crate::engage::target_at(track, world.t, &cfg.engage_rules());
    let pos = v.position.as_ref().map(|p| p.value).ok_or("cannot evaluate: no position reported")?;
    let cruise = v
        .platform
        .as_ref()
        .and_then(|p| p.cruise_mps)
        .unwrap_or(MIN_TRANSIT_MPS)
        .max(MIN_TRANSIT_MPS);
    let transit_s = ((haversine_m(pos, center) - radius).max(0.0) / cruise).round();
    Ok(Plan {
        track: track_id.to_string(),
        center,
        radius_m: radius,
        reach_m: reach.round(),
        size_class: class,
        conditions: limited.and_then(|rn| rn.conditions()),
        set_by_order: standoff.is_some(),
        transit_s,
    })
}

/// The track as the picture holds it live, or why not (gone, or a lost contact).
fn live_track<'w>(id: &str, world: &'w WorldSnapshot) -> Result<&'w TrackFacts, String> {
    if world.lost_contacts.contains_key(id) {
        return Err(format!("track {id} is a lost contact (the tracker dropped it)"));
    }
    world.tracks.get(id).ok_or_else(|| format!("track {id} is not in the picture"))
}

/// The orbit `spec` (an identify order) is flown as: the same intent, an orbit at `center`.
pub fn orbit_spec(spec: &IntentSpec, center: LonLat, radius_m: f64) -> IntentSpec {
    IntentSpec {
        task: Task::Orbit { center, radius_m },
        ..spec.clone()
    }
}

/// What the path rules, the terrain and the route read for `spec`: for an identify order whose
/// plan stands on `world`, its orbit; else `spec` itself.
pub fn orbit_view<'s>(spec: &'s IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Cow<'s, IntentSpec> {
    match spec.kind() {
        TaskKind::Identify => match plan(spec, world, cfg) {
            Ok(p) => Cow::Owned(orbit_spec(spec, p.center, p.radius_m)),
            Err(_) => Cow::Borrowed(spec),
        },
        _ => Cow::Borrowed(spec),
    }
}

/// The identify rule: the plan, or why there is none.
pub fn check(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> CheckResult {
    let uid = spec.uids.first();
    match plan(spec, world, cfg) {
        Ok(p) => {
            let level = world
                .track_levels
                .get(&p.track)
                .map_or("not classified".to_string(), |l| l.level.as_str().to_string());
            // rules 32: the conditions the camera's reach is for, when they shorten it
            let conditions = p.conditions.as_deref().map(|c| format!(" {c}")).unwrap_or_default();
            let standoff = if p.set_by_order {
                format!("an orbit of {:.0} m set by the order", p.radius_m)
            } else {
                format!(
                    "an orbit of {:.0} m, inside its EO/IR's identification reach of about {:.0} m{}{conditions}",
                    p.radius_m,
                    p.reach_m,
                    p.size_class.map(|c| format!(" for a {} contact", c.as_str())).unwrap_or_default()
                )
            };
            CheckResult::new(
                RuleId::Identify,
                uid,
                Outcome::Pass,
                format!(
                    "track {} ({level}): {standoff}, around its position now; about {:.0} s to get there",
                    p.track, p.transit_s
                ),
            )
        }
        Err(why) => CheckResult::new(RuleId::Identify, uid, Outcome::Fail, why),
    }
}

/// The identity a track has on `world`, as an identify order compares it: its kind and level.
pub fn identity_now(track: &str, world: &WorldSnapshot) -> Option<String> {
    let kind = world.tracks.get(track).and_then(|t| t.identity.clone())?;
    let level = world.track_levels.get(track).map_or("unclassified", |l| l.level.as_str());
    Some(format!("{kind} ({level})"))
}

/// Rules 22 and 23: what a look is judged against: when the order went out and the track's
/// identity then; rules 23, the vehicle sent (only its own identification ends the look) and the
/// other vehicles' identifications noted meanwhile; rules 33, the scenario's criteria (`criteria`,
/// the policy's `[watch]`) and the vehicle sent's namings below them noted so far.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    pub sent_t: Time,
    pub before: Option<&'a str>,
    pub own: Option<&'a Uid>,
    pub meanwhile: &'a [Noted],
    pub criteria: Option<&'a crate::watch::WatchSettings>,
    pub named: &'a [Noted],
}

impl<'a> Look<'a> {
    /// The look of `run` (none before rules 22), by `uid` from rules 23, against the scenario's
    /// criteria from rules 33.
    pub fn of(run: &'a Run, uid: &'a Uid, cfg: &'a RulesConfig) -> Option<Look<'a>> {
        Some(Look {
            sent_t: run.sent_t?,
            before: run.before.as_deref(),
            own: cfg.identify_own_look().then_some(uid),
            meanwhile: &run.meanwhile,
            criteria: cfg.identify_criteria().then(|| cfg.watch()),
            named: &run.named,
        })
    }
}

/// Rules 23: the callsigns an identify step logs (the bridge's identities on `world`): the vehicle
/// sent, `uid`, and every vehicle that identified the track (`level`).
pub fn callsigns(world: &WorldSnapshot, uid: &Uid, level: Option<&TrackLevel>) -> BTreeMap<Uid, String> {
    let mut uids = vec![uid.clone()];
    uids.extend(level.into_iter().flat_map(|l| l.own.keys().map(Uid::new)));
    uids.into_iter()
        .filter_map(|u| {
            let c = world.identities.get(&u)?.callsign.clone()?;
            Some((u, c))
        })
        .collect()
}

/// Rules 23: what to call one of our vehicles in an identify order's words: its callsign as the
/// step logged it, else `vehicle <uid>`.
pub fn name(world: &WorldSnapshot, uid: &str) -> String {
    world
        .callsigns
        .get(&Uid::new(uid))
        .cloned()
        .unwrap_or_else(|| format!("vehicle {uid}"))
}

/// Rules 23: the other vehicles' identifications of `track` on `world` made after `sent_t` that
/// are not noted yet (a vehicle noted again only when it gives another kind), in uid order.
pub fn meanwhile_due(track: &str, world: &WorldSnapshot, sent: &Uid, sent_t: Time, noted: &[Noted]) -> Vec<(String, Option<String>)> {
    meanwhile_due_by(track, world, sent, sent_t, noted, None)
}

/// [`meanwhile_due`]; rules 33 (`criteria`, the policy's `[watch]`): only the looks that meet the
/// track's domain criteria ([`shortfall`]), as the vehicle sent's own must to end the order.
pub fn meanwhile_due_by(
    track: &str,
    world: &WorldSnapshot,
    sent: &Uid,
    sent_t: Time,
    noted: &[Noted],
    criteria: Option<&WatchSettings>,
) -> Vec<(String, Option<String>)> {
    let Some(l) = world.track_levels.get(track) else { return Vec::new() };
    let domain = world.tracks.get(track).map(|t| t.domain);
    l.own
        .iter()
        .filter(|(by, o)| by.as_str() != sent.as_str() && o.t > sent_t)
        .filter(|(_, o)| match criteria {
            None => true,
            Some(w) => domain.is_some_and(|d| shortfall(o, d, w, l.conflict.as_deref()).is_none()),
        })
        .filter(|(by, o)| !noted.iter().any(|n| n.by == **by && n.kind == o.kind))
        .map(|(by, o)| (by.clone(), o.kind.clone()))
        .collect()
}

/// Rules 23: a [`Step::Meanwhile`]'s words: "HUNTER 1 identified it as cargo_ship meanwhile;
/// PUMA 1 continues for its own look".
pub fn meanwhile_detail(world: &WorldSnapshot, by: &str, kind: Option<&str>, sent: &Uid) -> String {
    format!(
        "{} identified it{} meanwhile; {} continues for its own look",
        name(world, by),
        kind.map(|k| format!(" as {k}")).unwrap_or_default(),
        name(world, sent.as_str())
    )
}

/// [`meanwhile_detail`]; rules 33 (`criteria`): the look's confidence and sensor too ("HUNTER 1
/// identified it as cargo_ship at 0.90 by its eo_ir meanwhile; PUMA 1 continues for its own
/// look").
pub fn meanwhile_detail_by(
    world: &WorldSnapshot,
    track: &str,
    by: &str,
    kind: Option<&str>,
    sent: &Uid,
    criteria: Option<&WatchSettings>,
) -> String {
    let look = world.track_levels.get(track).and_then(|l| l.own.get(by));
    match (criteria, look) {
        (Some(_), Some(o)) => format!(
            "{} identified it{} at {}{} meanwhile; {} continues for its own look",
            name(world, by),
            kind.map(|k| format!(" as {k}")).unwrap_or_default(),
            confidence_words(o),
            sensor_words(o),
            name(world, sent.as_str())
        ),
        _ => meanwhile_detail(world, by, kind, sent),
    }
}

/// Rules 23: the outcome's note of the identifications made meanwhile ("; meanwhile HUNTER 1
/// identified it as cargo_ship"), empty when none.
fn meanwhile_note(world: &WorldSnapshot, noted: &[Noted]) -> String {
    if noted.is_empty() {
        return String::new();
    }
    let each: Vec<String> = noted
        .iter()
        .map(|n| format!("{} identified it{}", name(world, &n.by), n.kind.as_deref().map(|k| format!(" as {k}")).unwrap_or_default()))
        .collect();
    format!("; meanwhile {}", each.join(", "))
}

/// Rules 33: why a vehicle's own look does not identify a track of `domain` by the scenario's
/// criteria (`w`, the policy's `[watch]`; `conflict`, the tracker's identity conflict on it), in
/// the order checked: none when it does.
#[derive(Clone, Debug, PartialEq)]
pub enum Shortfall {
    /// A level below the domain's (a recognition where identification counts).
    Level,
    /// Not one of the domain's sensors (AIS never counts: an identify order is for our own look).
    Sensor,
    /// A confidence below the bar.
    Bar,
    /// The tracker's identity conflict on the track.
    Conflict(String),
}

/// Rules 33: whether `o` meets the domain criteria `w` gives a track of `domain`: none when it
/// does, else the first criterion it falls short of ([`Shortfall`]). A look without its details
/// (a picture of older rules) meets nothing.
pub fn shortfall(o: &OwnLook, domain: crate::catalog::Domain, w: &WatchSettings, conflict: Option<&str>) -> Option<Shortfall> {
    let c = w.domain.get(domain);
    if o.level.is_none_or(|l| l < c.level) {
        return Some(Shortfall::Level);
    }
    let sensor = o.sensor.as_deref().unwrap_or_default();
    if sensor == "ais" || !c.sensors.iter().any(|s| s == sensor) {
        return Some(Shortfall::Sensor);
    }
    if o.confidence.is_none_or(|x| x < w.bar) {
        return Some(Shortfall::Bar);
    }
    conflict.map(|c| Shortfall::Conflict(c.to_string()))
}

/// Rules 33: a sensor type as the words name it.
fn sensor_name(s: &str) -> &str {
    match s {
        "eo_ir" => "EO/IR",
        "esm" => "ESM",
        other => other,
    }
}

/// Rules 33: what identifies a contact of `domain` by `w`, in words: "identification by EO/IR",
/// "recognition by sonar" (AIS never ends an identify order, so it is not named).
pub fn criterion_words(w: &WatchSettings, domain: crate::catalog::Domain) -> String {
    let c = w.domain.get(domain);
    let own: Vec<&str> = c.sensors.iter().filter(|s| s.as_str() != "ais").map(|s| sensor_name(s)).collect();
    let by = match own.split_last() {
        None => "no sensor of ours (AIS alone, which never ends an identify order)".to_string(),
        Some((one, [])) => (*one).to_string(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    };
    format!("{} by {by}", c.level.noun())
}

/// Rules 33: the scenario's criteria for a contact of `domain`, in words: "the bar is 0.6,
/// identification by EO/IR".
pub fn criteria_words(w: &WatchSettings, domain: crate::catalog::Domain) -> String {
    format!("the bar is {}, {}", w.bar, criterion_words(w, domain))
}

/// Rules 33: a look's confidence in words ("0.11").
fn confidence_words(o: &OwnLook) -> String {
    o.confidence.map_or_else(|| "a confidence not known".to_string(), |c| format!("{c:.2}"))
}

/// Rules 33: a look's level with its article ("a recognition").
fn level_article(o: &OwnLook) -> &'static str {
    match o.level {
        Some(Level::Detect) => "a detection",
        Some(Level::Recognise) => "a recognition",
        Some(Level::Identify) => "an identification",
        None => "a look of a level not known",
    }
}

/// Rules 33: " by its eo_ir", or nothing where the look does not say its sensor.
fn sensor_words(o: &OwnLook) -> String {
    o.sensor.as_deref().map(|s| format!(" by its {s}")).unwrap_or_default()
}

/// Rules 33: what the vehicle sent's own look since the order went out named the track as, and
/// how it falls short of the criteria, in words ("named it small_fast_boat at 0.11, not yet
/// identified (the bar is 0.6)").
fn short_words(kind: Option<&str>, o: &OwnLook, why: &Shortfall, w: &WatchSettings, domain: crate::catalog::Domain) -> String {
    let kind = kind.unwrap_or("its kind");
    let at = confidence_words(o);
    match why {
        Shortfall::Bar => format!("named it {kind} at {at}, not yet identified (the bar is {})", w.bar),
        Shortfall::Level => format!(
            "named it {kind} at {at}, {}{}, not yet identified (the criterion is {})",
            level_article(o),
            sensor_words(o),
            criterion_words(w, domain)
        ),
        Shortfall::Sensor => format!(
            "named it {kind} at {at}{}, not yet identified (the criterion is {})",
            sensor_words(o),
            criterion_words(w, domain)
        ),
        Shortfall::Conflict(c) => format!("named it {kind} at {at}, not yet identified (its identity is in conflict: {c})"),
    }
}

/// Rules 33: the vehicle sent's own look at `track` on `world` since the order went out (`look`),
/// with the kind it names, its track's domain and the conflict on it; none where it has none.
fn own_since<'w>(track: &str, world: &'w WorldSnapshot, look: &Look<'_>) -> Option<(&'w OwnLook, Option<String>, &'w TrackFacts, Option<&'w str>)> {
    let u = look.own?;
    let t = world.tracks.get(track)?;
    let l = world.track_levels.get(track)?;
    let o = l.own.get(u.as_str()).filter(|o| o.t > look.sent_t)?;
    let kind = o.kind.clone().or_else(|| t.identity.clone());
    Some((o, kind, t, l.conflict.as_deref()))
}

/// Rules 33: the vehicle sent's own naming of `track` on `world` that falls short of the criteria
/// (`look.criteria`) and is not noted yet for that kind (`look.named`): its uid, the kind and the
/// step's words ("PUMA 1 named it small_fast_boat at 0.11, not yet identified (the bar is 0.6);
/// it goes on"). None under older rules, without such a look, or once noted.
pub fn named_due(track: &str, world: &WorldSnapshot, look: &Look<'_>) -> Option<(String, Option<String>, String)> {
    let w = look.criteria?;
    let u = look.own?;
    let (o, kind, t, conflict) = own_since(track, world, look)?;
    let why = shortfall(o, t.domain, w, conflict)?;
    if look.named.iter().any(|n| n.by == u.as_str() && n.kind == kind) {
        return None;
    }
    let detail = format!("{} {}; it goes on", name(world, u.as_str()), short_words(kind.as_deref(), o, &why, w, t.domain));
    Some((u.to_string(), kind, detail))
}

/// Rules 33: where an identify order's look stands (MCP `get_intents` and `explain_intent`;
/// read-only, never logged): the vehicle sent's best own look since it went out against its
/// track's criteria.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Progress {
    /// The vehicle sent (its uid).
    pub vehicle: String,
    /// Its best own look since the order went out (none: none yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look: Option<OwnLook>,
    /// The scenario's bar, and the level and sensors that count for the track's domain.
    pub bar: f64,
    pub level: Level,
    pub sensors: Vec<String>,
    /// Whether the look meets them (the order ends identified at its next step).
    pub identified: bool,
    pub words: String,
}

/// Rules 33: [`Progress`] of identify order `run` on `uid` at `track` on `world`, the vehicle
/// called `who`; none under older rules, before the order went out, or with the track gone.
pub fn progress(track: &str, world: &WorldSnapshot, run: &Run, uid: &Uid, cfg: &RulesConfig, who: &str) -> Option<Progress> {
    if !cfg.identify_criteria() {
        return None;
    }
    let look = Look::of(run, uid, cfg)?;
    let w = look.criteria?;
    let t = world.tracks.get(track)?;
    let c = w.domain.get(t.domain);
    let own = own_since(track, world, &look);
    let criteria = criteria_words(w, t.domain);
    let (identified, words) = match &own {
        None => (false, format!("{who} has no look of its own at it since the order went out ({criteria})")),
        Some((o, kind, _, conflict)) => match shortfall(o, t.domain, w, *conflict) {
            None => (
                true,
                format!(
                    "{who}'s best look named it {} at {}{}: identified ({criteria})",
                    kind.as_deref().unwrap_or("its kind"),
                    confidence_words(o),
                    sensor_words(o)
                ),
            ),
            Some(why) => {
                let reason = match &why {
                    Shortfall::Bar => "below the bar".to_string(),
                    Shortfall::Level => level_article(o).to_string(),
                    Shortfall::Sensor => "not a sensor that counts".to_string(),
                    Shortfall::Conflict(c) => format!("its identity is in conflict: {c}"),
                };
                let words = format!(
                    "{who}'s best look so far named it {} at {}{}, not yet identified ({reason}; {criteria})",
                    kind.as_deref().unwrap_or("its kind"),
                    confidence_words(o),
                    sensor_words(o)
                );
                (false, words)
            }
        },
    };
    Some(Progress {
        vehicle: uid.to_string(),
        look: own.map(|(o, ..)| o.clone()),
        bar: w.bar,
        level: c.level,
        sensors: c.sensors.iter().filter(|s| s.as_str() != "ais").cloned().collect(),
        identified,
        words,
    })
}

/// Rules 33: [`outcome`] by the scenario's criteria (`w`): identified only on the vehicle sent's
/// own look since the order went out that meets them, saying its confidence and the criteria;
/// at the deadline, the vehicle's best look.
fn outcome_by_criteria(track: &str, world: &WorldSnapshot, deadline_t: Time, k: &Look<'_>, w: &WatchSettings) -> Option<(EndKind, String)> {
    let note = meanwhile_note(world, k.meanwhile);
    let own = own_since(track, world, k);
    let who = k.own.map_or_else(|| "the vehicle sent".to_string(), |u| name(world, u.as_str()));
    if let Some((o, kind, t, conflict)) = &own
        && shortfall(o, t.domain, w, *conflict).is_none()
    {
        let mut detail = format!(
            "track {track} identified as {} at {} by {who}'s {} ({})",
            kind.as_deref().unwrap_or("its kind"),
            confidence_words(o),
            o.sensor.as_deref().unwrap_or("sensor"),
            criteria_words(w, t.domain)
        );
        let was = k.before.unwrap_or("unclassified");
        let before_kind = k.before.and_then(|b| b.split(" (").next());
        let identified_before = k.before.is_some_and(|b| b.ends_with("(identify)"));
        match (identified_before, before_kind == kind.as_deref()) {
            (true, true) => detail.push_str(&format!("; it agrees with its identity before, {was}")),
            (true, false) => detail.push_str(&format!("; it differs from its identity before, {was}")),
            (false, _) => detail.push_str(&format!("; it was {was} before")),
        }
        return Some((EndKind::Identified, format!("{detail}{note}")));
    }
    if let Err(why) = live_track(track, world) {
        return Some((EndKind::Lost, format!("{why}{note}")));
    }
    if world.t >= deadline_t {
        let domain = world.tracks.get(track).map(|t| t.domain);
        let best = match &own {
            Some((o, kind, t, conflict)) => {
                let hint = match shortfall(o, t.domain, w, *conflict) {
                    Some(Shortfall::Level) => format!(", {}", level_article(o)),
                    Some(Shortfall::Conflict(c)) => format!(", its identity in conflict: {c}"),
                    _ => String::new(),
                };
                format!(
                    "{who}'s best look named it {} at {}{}{hint}",
                    kind.as_deref().unwrap_or("its kind"),
                    confidence_words(o),
                    sensor_words(o)
                )
            }
            None => format!("{who} had no look of its own at it since the order went out"),
        };
        let criteria = domain.map(|d| format!(" ({})", criteria_words(w, d))).unwrap_or_default();
        return Some((EndKind::TimedOut, format!("track {track} not identified by the order's deadline; {best}{criteria}{note}")));
    }
    None
}

/// Whether the look is over on `world`, and why: identified, lost, or past `deadline_t`. Rules 22
/// (`look`: when it went out, and the identity before): identified only by our own sensors after
/// it went out, saying whether that agrees with the identity before or contradicts it. Rules 23
/// (`look.own`): only by the vehicle sent, with its own sensors, the identifications of others
/// meanwhile said. Rules 33 (`look.criteria`): only on its own look that meets the track's domain
/// criteria, saying its confidence and the criteria ([`outcome_by_criteria`]).
pub fn outcome(track: &str, world: &WorldSnapshot, deadline_t: Time, look: Option<Look<'_>>) -> Option<(EndKind, String)> {
    if let Some(k) = look
        && let Some(w) = k.criteria
    {
        return outcome_by_criteria(track, world, deadline_t, &k, w);
    }
    let note = look.map(|k| meanwhile_note(world, k.meanwhile)).unwrap_or_default();
    // rules 23: the vehicle's own identification, after it went out
    fn own<'l>(l: &'l TrackLevel, k: &Look<'_>) -> Option<&'l OwnLook> {
        k.own.and_then(|u| l.own.get(u.as_str())).filter(|o| o.t > k.sent_t)
    }
    if let Some(l) = world.track_levels.get(track)
        && match look {
            None => l.level == Level::Identify,
            Some(k) if k.own.is_some() => own(l, &k).is_some(),
            Some(k) => l.level == Level::Identify && l.own_t.is_some_and(|t| t > k.sent_t),
        }
    {
        let kind = match look.and_then(|k| own(l, &k)) {
            Some(o) => o.kind.clone().or_else(|| world.tracks.get(track).and_then(|t| t.identity.clone())),
            None => world.tracks.get(track).and_then(|t| t.identity.clone()),
        };
        let by = match look.and_then(|k| k.own) {
            // rules 23: by the vehicle sent, and with which of its sensors
            Some(u) => {
                let sensors: Vec<&str> = l.by.iter().filter_map(|b| b.strip_prefix(&format!("{u}:"))).collect();
                let whose = name(world, u.as_str());
                if sensors.is_empty() { format!(" by {whose}") } else { format!(" by {whose}'s {}", sensors.join(", ")) }
            }
            None if l.by.is_empty() => String::new(),
            None => format!(" by {}", l.by.join(", ")),
        };
        let mut detail = format!("track {track} identified as {}{by}", kind.as_deref().unwrap_or("its kind"));
        if let Some(Look { before, .. }) = look {
            let was = before.unwrap_or("unclassified");
            let before_kind = before.and_then(|b| b.split(" (").next());
            let identified_before = before.is_some_and(|b| b.ends_with("(identify)"));
            match (&l.conflict, identified_before, before_kind == kind.as_deref()) {
                (Some(c), _, _) => detail.push_str(&format!("; it contradicts its identity before, {was}: {c}")),
                (None, true, true) => detail.push_str(&format!("; it agrees with its identity before, {was}")),
                (None, true, false) => detail.push_str(&format!("; it differs from its identity before, {was}")),
                (None, false, _) => detail.push_str(&format!("; it was {was} before")),
            }
        }
        return Some((EndKind::Identified, format!("{detail}{note}")));
    }
    if let Err(why) = live_track(track, world) {
        return Some((EndKind::Lost, format!("{why}{note}")));
    }
    if world.t >= deadline_t {
        return Some((EndKind::TimedOut, format!("track {track} not identified by the order's deadline{note}")));
    }
    None
}

/// Where the orbit moves to on `world`, when the track has moved far enough from `center`.
pub fn recentre(plan: &Plan, world: &WorldSnapshot, cfg: &RulesConfig) -> Option<LonLat> {
    let track = world.tracks.get(&plan.track)?;
    let now = crate::engage::target_at(track, world.t, &cfg.engage_rules());
    let threshold = (RECENTRE_SHARE * plan.radius_m).max(RECENTRE_MIN_M);
    (haversine_m(now, plan.center) > threshold).then_some(now)
}

/// The order an identify goes back to, as the vehicle's own: `resumes`'s task for that vehicle
/// alone.
pub fn return_spec(resumes: &IntentSpec, identify: &IntentSpec) -> IntentSpec {
    IntentSpec {
        uids: identify.uids.clone(),
        ..resumes.clone()
    }
}

/// What an identify order did after its look, in words: back to `resumes` (the order it went
/// back to), or holding, saying why (`replaced`: the order it replaced, whose `checks` failed).
pub fn then_phrase(resumes: Option<IntentId>, replaced: Option<IntentId>, returns: &Task, checks: &[CheckResult]) -> String {
    match (resumes, replaced) {
        (Some(m), _) => format!("back to {m}'s {}", returns.kind().as_str()),
        (None, Some(m)) => format!("holding: {m} fails its checks now ({})", failing(checks)),
        (None, None) => "holding: no order to go back to".to_string(),
    }
}

/// The failing checks' details, as one line.
pub fn failing(checks: &[CheckResult]) -> String {
    checks
        .iter()
        .filter(|c| c.outcome == Outcome::Fail)
        .map(|c| format!("{}: {}", c.rule.as_str(), c.detail))
        .collect::<Vec<_>>()
        .join("; ")
}
