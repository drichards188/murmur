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
//! Rules 23 ([`crate::rules::IDENTIFY_OWN_LOOK_SINCE`], the owner's Harbor watch run of
//! 2026-10-27 17:11Z, whose identify orders another vehicle's camera ended): the look ends identified
//! only on an identification by the vehicle sent, with its own sensors, made after the order went
//! out ([`TrackLevel::own`]). Another vehicle's identification meanwhile is logged as a step
//! ([`Step::Meanwhile`], [`meanwhile_due`]) and said in the outcome; the order goes on.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::Domain;
use crate::geo::{LonLat, haversine_m};
use crate::ids::{IntentId, Uid};
use crate::intent::{IntentSpec, Task, TaskKind};
use crate::rules::{CheckResult, Outcome, RuleId, RulesConfig};
use crate::time::Time;
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

/// Time on station before an identify order gives up, after the time to get there [E]: a camera
/// takes seconds a look (docs/SENSING.md: 3.4 s to detect, 1 s more to identify, a second to
/// settle after each slew), so five minutes on station is many looks.
pub const ON_STATION_S: f64 = 300.0;

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
}

/// Rules 21: a track's identity level as the bridge's tracker settles it, and the sensors whose
/// looks reached identification, as `"<uid>:<sensor>"` (`"ais"` for an AIS identity's kind).
/// [`WorldSnapshot::track_levels`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// AIS), by the vehicle's uid: when, and the kind it gave.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub own: BTreeMap<String, OwnLook>,
}

/// Rules 23: one vehicle's latest identification of a track by its own sensors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnLook {
    pub t: Time,
    /// The kind it identified it as (none where the tracker can't say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// The orbit an identify order is flown as, and what it rests on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub track: String,
    pub center: LonLat,
    pub radius_m: f64,
    /// The camera's identification reach the standoff was taken from, slant metres.
    pub reach_m: f64,
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
    /// goes on for its own vehicle's look. Logged once for each vehicle and kind.
    Meanwhile {
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
    /// Rules 23: the other vehicles' identifications noted meanwhile, in order: by whom, as what.
    pub meanwhile: Vec<(String, Option<String>)>,
}

/// The order's deadline: its dispatch, the time to get there, then [`ON_STATION_S`].
pub fn deadline(dispatched_t: Time, plan: &Plan) -> Time {
    dispatched_t.plus_ms(((plan.transit_s + ON_STATION_S) * 1000.0).round() as i64)
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

/// The vehicle's EO/IR reach for a contact in `domain`, m: the longest of its cameras'.
fn camera_reach(v: &VehicleFacts, domain: Domain) -> Option<f64> {
    v.platform
        .as_ref()?
        .sensors
        .iter()
        .filter(|s| s.kind == "eo_ir")
        .filter_map(|s| s.reach_for(domain))
        .fold(None, |best: Option<f64>, r| Some(best.map_or(r, |b| b.max(r))))
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
    let Some(eo) = camera_reach(v, track.domain) else {
        let platform = v.platform.as_ref().map_or("its platform", |p| p.id.as_str());
        return Err(format!(
            "{platform} carries no EO/IR that reaches a contact in the {} domain",
            track.domain.as_str()
        ));
    };
    let reach = eo * IDENTIFY_PX_SHARE;
    // the camera's slant range: the vehicle's height over the contact counts
    let height = match (&v.alt_msl_m, track.alt_m) {
        (Some(a), Some(t)) => (a.value - t).abs(),
        (Some(a), None) => a.value.max(0.0),
        _ => 0.0,
    };
    if height >= reach {
        return Err(format!(
            "its height over the contact, {height:.0} m, is beyond its camera's identification reach of about {reach:.0} m"
        ));
    }
    let tightest = min_radius(v, cfg).unwrap_or(0.0);
    let ground = |slant: f64| (slant * slant - height * height).max(0.0).sqrt();
    let radius = match standoff {
        Some(r) => r,
        None => ground(reach * STANDOFF_SHARE).max(tightest).max(1.0).ceil(),
    };
    if standoff.is_none() && radius.hypot(height) > reach {
        return Err(format!(
            "its tightest orbit, {tightest:.0} m, keeps its camera beyond its identification reach of about {reach:.0} m"
        ));
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
            let standoff = if p.set_by_order {
                format!("an orbit of {:.0} m set by the order", p.radius_m)
            } else {
                format!(
                    "an orbit of {:.0} m, inside its EO/IR's identification reach of about {:.0} m",
                    p.radius_m, p.reach_m
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
/// other vehicles' identifications noted meanwhile.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    pub sent_t: Time,
    pub before: Option<&'a str>,
    pub own: Option<&'a Uid>,
    pub meanwhile: &'a [(String, Option<String>)],
}

impl<'a> Look<'a> {
    /// The look of `run` (none before rules 22), by `uid` from rules 23.
    pub fn of(run: &'a Run, uid: &'a Uid, cfg: &RulesConfig) -> Option<Look<'a>> {
        Some(Look {
            sent_t: run.sent_t?,
            before: run.before.as_deref(),
            own: cfg.identify_own_look().then_some(uid),
            meanwhile: &run.meanwhile,
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
pub fn meanwhile_due(track: &str, world: &WorldSnapshot, sent: &Uid, sent_t: Time, noted: &[(String, Option<String>)]) -> Vec<(String, Option<String>)> {
    let Some(l) = world.track_levels.get(track) else { return Vec::new() };
    l.own
        .iter()
        .filter(|(by, o)| by.as_str() != sent.as_str() && o.t > sent_t)
        .filter(|(by, o)| !noted.iter().any(|(b, k)| b == *by && *k == o.kind))
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

/// Rules 23: the outcome's note of the identifications made meanwhile ("; meanwhile HUNTER 1
/// identified it as cargo_ship"), empty when none.
fn meanwhile_note(world: &WorldSnapshot, noted: &[(String, Option<String>)]) -> String {
    if noted.is_empty() {
        return String::new();
    }
    let each: Vec<String> = noted
        .iter()
        .map(|(by, k)| format!("{} identified it{}", name(world, by), k.as_deref().map(|k| format!(" as {k}")).unwrap_or_default()))
        .collect();
    format!("; meanwhile {}", each.join(", "))
}

/// Whether the look is over on `world`, and why: identified, lost, or past `deadline_t`. Rules 22
/// (`look`: when it went out, and the identity before): identified only by our own sensors after
/// it went out, saying whether that agrees with the identity before or contradicts it. Rules 23
/// (`look.own`): only by the vehicle sent, with its own sensors, the identifications of others
/// meanwhile said.
pub fn outcome(track: &str, world: &WorldSnapshot, deadline_t: Time, look: Option<Look<'_>>) -> Option<(EndKind, String)> {
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
