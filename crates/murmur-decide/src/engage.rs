//! The engagement decision chain (Phase E): nominate, check, preview, authorize, hold the
//! picture, execute with abort, assess. A C2 decision study, not a weapon model: the effect
//! is an abstract event (a point, a radius, a time). Nothing here models a munition, a
//! warhead, blast, lethality, terminal guidance or any optimisation of an aim point.
//!
//! ## The chain
//!
//! 1. **Nominate.** A human or an agent proposes [`crate::Task::Engage`]: a perception track,
//!    one armed platform, an effect radius, an optional window and a link-loss
//!    [`Fallback`]. An agent must give a rationale (as for every agent proposal).
//! 2. **Check** ([`evaluate`]), fail closed, each with a precise detail:
//!    `target_declared` (an operator declared the track hostile; an inferred suspect is not
//!    enough), `target_identity` (identity and existence confidence thresholds),
//!    `target_recency` (last detection age), `target_uncertainty` (1-sigma now within a
//!    fraction of the effect radius), `engagement_zone` (the predicted effect point is inside
//!    an approved engagement zone), `no_strike` (the effect circle is clear of every no-strike
//!    area), `keep_out_friendly` (none of our vehicles can be inside the keep-out at the
//!    effect time), `keep_out_tracks` (no other track but a declared hostile, with its
//!    uncertainty, inside the keep-out at the effect time), `platform_armed`, and `reach`
//!    (catalog cruise speed, intercept inside the window, endurance, the effect point on the
//!    surface a boat or ground vehicle needs). The generic vehicle rules (capability,
//!    readiness, geofence on the transit and return legs) run too. Engage is consequential:
//!    `not_evaluated` and `unverified` block.
//! 3. **Preview** ([`preview`]): a straight-line intercept at the platform's catalog cruise
//!    speed against the track's projected motion (constant velocity from its last estimate),
//!    after a dispatch delay; the predicted effect point and time; the track's uncertainty
//!    at that time as an ellipse along its course. Stored on the intent and logged with each
//!    check (replay recomputes it).
//! 4. **Authorize**: only an operator session opened with engagement authority
//!    ([`crate::EngagementAuthority`]), never the proposer, and as many distinct approvers as
//!    the tier asks (`approvers = 2`). See [`crate::Engine::approve_engagement`].
//! 5. **Hold the picture** ([`picture`]): the final approval stores an [`EngageSnapshot`]
//!    (the track's facts, the preview, the platform's link assumption). Every step after
//!    approval re-checks the picture against it and voids (before dispatch) or aborts (after)
//!    when the track moved beyond tolerance from the snapshot's prediction, was reclassified
//!    or undeclared, went stale, a protected track or one of our vehicles is predicted inside
//!    the keep-out, the platform's link degraded past the approval's assumption, the window
//!    closed or the platform was late.
//! 6. **Execute with abort**: the platform is sent to the effect point through the task
//!    layer (a loiter at the point); until the effect any operator may abort, and the
//!    recorded fallback is commanded. The effect is released ([`picture`]'s `ready`) when the
//!    picture holds, the predicted time has come, the platform is at the point and the track
//!    is estimated inside the effect radius. It is an abstract event, logged and sent to the
//!    contacts process (truth), which marks contacts inside the radius.
//! 7. **Assess** ([`observe`], [`assess`]): a sensor of ours must cover the effect point
//!    afterwards. The target track either persists (detected after the effect), ceased (no
//!    detection while covered for `assess_dwell_s`), or is unknown (not covered long enough
//!    within `assess_window_s`).
//!
//! Every function here is pure: the engine logs the inputs and the results, and
//! `murmur-replay` recomputes them.

use serde::{Deserialize, Serialize};

use crate::areas::{Area, AreaKind};
use crate::catalog::{Domain, SensorFact};
use crate::geo::{LonLat, haversine_m};
use crate::ids::{IntentId, OperatorId, SessionId, Uid};
use crate::intent::{IntentSpec, Task};
use crate::rules::{CheckResult, Outcome, RuleId, RulesConfig, fresh, required_surface};
use crate::terrain::Surface;
use crate::time::Time;
use crate::world::{Affiliation, TerrainInputs, TrackFacts, WorldSnapshot};

/// Metres per degree of latitude (as `crate::geo`).
const M_PER_DEG: f64 = 111_320.0;

// ---- parameters -------------------------------------------------------------------------

/// What the platform does when the engagement stops before the effect: an operator's
/// abort, a void (the picture changed), link loss, the window closing. Recorded with the
/// nomination and in the approval; never a silent continue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// Break off and hold where it is (the autopilot's hold: loiter, brake, station keep).
    Abort,
    /// Return home (RTL).
    Return,
    /// Break off into a holding orbit around its current position, available for a new
    /// decision.
    Loiter,
}

impl Fallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Fallback::Abort => "abort",
            Fallback::Return => "return",
            Fallback::Loiter => "loiter",
        }
    }
}

/// When the effect may happen, engine time (integer ms).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngageWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Time>,
    pub until: Time,
}

/// `[rules.engage]` in `sim/policy.toml`. Logged with the policy at run start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngageRules {
    /// The track's identity confidence must be at least this (0..1).
    pub min_identity_confidence: f64,
    /// The track's existence confidence now must be at least this (0..1).
    pub min_track_confidence: f64,
    /// The track's last detection may be at most this old, seconds.
    pub max_track_age_s: f64,
    /// The track's 1-sigma position uncertainty now must be at most this times the effect
    /// radius.
    pub max_sigma_fraction: f64,
    /// Largest effect radius a nomination may ask for, metres.
    pub max_effect_radius_m: f64,
    /// Keep-out radius beyond the effect radius, metres: none of our vehicles and no
    /// protected track may be predicted inside `effect_radius_m + keep_out_m` of the effect
    /// point at the effect time.
    pub keep_out_m: f64,
    /// A protected track counts with this many sigma of its predicted uncertainty.
    pub keep_out_sigma: f64,
    /// A nomination without a window may happen until this long after it was made, s.
    pub default_window_s: u32,
    /// The preview assumes the platform starts this long after the decision (mission
    /// upload, arming, takeoff), seconds.
    pub dispatch_delay_s: f64,
    /// The intercept may take at most this long, seconds.
    pub max_transit_s: f64,
    /// A platform with no endurance figure may engage only if the transit takes at most
    /// this long and its battery is above the reserve now (a weaker check, stated in the
    /// detail).
    pub unknown_endurance_max_transit_s: f64,
    /// After approval, the track may deviate from the snapshot's prediction by at most
    /// `max(deviation_fraction x effect radius, deviation_min_m)`.
    pub deviation_fraction: f64,
    pub deviation_min_m: f64,
    /// The approval assumes the platform is heard at least every `max(link_gap_s,
    /// link_gap_intervals x its report interval at approval)`; longer silence, or a report
    /// interval more than twice the approval's, voids.
    pub link_gap_s: f64,
    pub link_gap_intervals: f64,
    /// The platform is at the effect point within this distance, metres (it loiters there).
    pub arrival_m: f64,
    /// The effect not released this long after the predicted time: abort, seconds.
    pub late_s: f64,
    /// A detection this soon after the effect does not count as "persists" (the truth side
    /// applies the effect on its next tick), seconds.
    pub assess_grace_s: f64,
    /// Ceased: no detection of the target while a sensor of ours covered the effect point
    /// for this long, seconds.
    pub assess_dwell_s: f64,
    /// Unknown: the point was not covered long enough within this long after the effect.
    pub assess_window_s: f64,
}

impl Default for EngageRules {
    fn default() -> Self {
        EngageRules {
            min_identity_confidence: 0.6,
            min_track_confidence: 0.5,
            max_track_age_s: 5.0,
            max_sigma_fraction: 0.5,
            max_effect_radius_m: 250.0,
            keep_out_m: 200.0,
            keep_out_sigma: 2.0,
            default_window_s: 600,
            dispatch_delay_s: 20.0,
            max_transit_s: 900.0,
            unknown_endurance_max_transit_s: 600.0,
            deviation_fraction: 1.0,
            deviation_min_m: 50.0,
            link_gap_s: 5.0,
            link_gap_intervals: 3.0,
            arrival_m: 150.0,
            late_s: 60.0,
            assess_grace_s: 3.0,
            assess_dwell_s: 15.0,
            assess_window_s: 120.0,
        }
    }
}

impl EngageRules {
    pub fn validate(&self) -> Result<(), String> {
        let frac = |v: f64, n: &str| {
            if v.is_finite() && (0.0..=1.0).contains(&v) {
                Ok(())
            } else {
                Err(format!("rules.engage.{n} must be 0..1"))
            }
        };
        frac(self.min_identity_confidence, "min_identity_confidence")?;
        frac(self.min_track_confidence, "min_track_confidence")?;
        for (v, n) in [
            (self.max_track_age_s, "max_track_age_s"),
            (self.max_sigma_fraction, "max_sigma_fraction"),
            (self.max_effect_radius_m, "max_effect_radius_m"),
            (self.max_transit_s, "max_transit_s"),
            (self.arrival_m, "arrival_m"),
            (self.assess_dwell_s, "assess_dwell_s"),
            (self.assess_window_s, "assess_window_s"),
            (self.link_gap_s, "link_gap_s"),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(format!("rules.engage.{n} must be > 0"));
            }
        }
        for (v, n) in [
            (self.keep_out_m, "keep_out_m"),
            (self.keep_out_sigma, "keep_out_sigma"),
            (self.dispatch_delay_s, "dispatch_delay_s"),
            (self.deviation_fraction, "deviation_fraction"),
            (self.deviation_min_m, "deviation_min_m"),
            (self.late_s, "late_s"),
            (self.assess_grace_s, "assess_grace_s"),
            (self.link_gap_intervals, "link_gap_intervals"),
            (
                self.unknown_endurance_max_transit_s,
                "unknown_endurance_max_transit_s",
            ),
        ] {
            if !(v.is_finite() && v >= 0.0) {
                return Err(format!("rules.engage.{n} must be >= 0"));
            }
        }
        if self.default_window_s == 0 {
            return Err("rules.engage.default_window_s must be > 0".into());
        }
        Ok(())
    }
}

// ---- records ------------------------------------------------------------------------------

/// An uncertainty ellipse: 1-sigma semi-axes in metres, the major axis along `bearing_deg`
/// (degrees clockwise from north).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ellipse {
    pub center: LonLat,
    pub semi_major_m: f64,
    pub semi_minor_m: f64,
    pub bearing_deg: f64,
}

/// The preview of an engagement: the platform's path forward and the predicted effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngagePreview {
    /// The world time the preview was computed at.
    pub t: Time,
    /// The effective window (the nomination's, or the default from its creation).
    pub window: EngageWindow,
    /// Where the platform starts, and when the preview assumes it starts moving.
    pub start: LonLat,
    pub start_t: Time,
    pub cruise_mps: f64,
    /// The platform's path forward: a straight line from its position to the effect point.
    pub path: Vec<LonLat>,
    pub distance_m: f64,
    /// The track's projected motion from its last estimate to the effect time.
    pub track_path: Vec<LonLat>,
    pub track_speed_mps: f64,
    /// The predicted effect point and time.
    pub effect_point: LonLat,
    pub effect_t: Time,
    pub effect_radius_m: f64,
    /// Where none of our vehicles and no protected track may be: effect radius + keep-out.
    pub keep_out_m: f64,
    /// The track's 1-sigma uncertainty now, and at the effect time along and across its
    /// course (the tracker's growth law along, half of it across).
    pub track_sigma_m: f64,
    pub ellipse: Ellipse,
}

/// The picture an approval rests on: what the approver saw, held against everything after.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngageSnapshot {
    /// When the final approval's re-check read the world.
    pub t: Time,
    pub track_id: String,
    /// The track's facts then: position and when it was observed, velocity, uncertainty,
    /// identity and confidence, affiliation, declared by, last seen.
    pub track: TrackFacts,
    /// The preview then: the effect point and time the platform is sent to.
    pub preview: EngagePreview,
    /// The platform's report interval then (the link the approval assumed), and the longest
    /// silence the approval tolerates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_interval_ms: Option<u32>,
    pub link_gap_limit_ms: i64,
    pub fallback: Fallback,
}

/// The abstract effect, as released.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectRecord {
    pub t: Time,
    pub point: LonLat,
    pub radius_m: f64,
    /// The predicted time it was planned for.
    pub planned_t: Time,
}

/// Who or what stopped an engagement before its effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopCause {
    /// An operator aborted it (any operator may).
    Operator { by: OperatorId, session: SessionId },
    /// The picture no longer held: the last `engage.picture` failed (replay recomputes it).
    Picture,
    /// The bridge stopped it for a reason outside the picture (the platform was retasked,
    /// its task failed, it was lost).
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StopRecord {
    pub t: Time,
    pub cause: StopCause,
    pub reason: String,
    /// The fallback commanded (none if it stopped before dispatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Fallback>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessResult {
    /// The target track was detected after the effect.
    Persists,
    /// No detection of the target while our sensors covered the effect point long enough.
    Ceased,
    /// Not covered long enough to say.
    Unknown,
}

impl AssessResult {
    pub fn as_str(self) -> &'static str {
        match self {
            AssessResult::Persists => "persists",
            AssessResult::Ceased => "ceased",
            AssessResult::Unknown => "unknown",
        }
    }
}

/// One observation step after the effect: who covered the effect point, and whether the
/// target track was detected since the effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObserveStep {
    /// Our vehicles with a fresh position and a sensor that sees the target's domain within
    /// its nominal range of the effect point, as `"<uid>:<sensor> <range> m"`.
    pub covered_by: Vec<String>,
    /// The target track is in the picture.
    pub track_present: bool,
    /// Its last detection, if after the effect plus the grace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_after_effect_t: Option<Time>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub t: Time,
    pub result: AssessResult,
    pub evidence: String,
    /// Seconds the effect point was covered after the effect.
    pub covered_s: f64,
}

/// The last verdict of the picture watch, for the console ("picture changed").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PictureVerdict {
    pub t: Time,
    pub holds: bool,
    pub ready: bool,
    pub results: Vec<CheckResult>,
}

/// An engagement intent's record beyond the generic intent (on [`crate::Intent`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Engagement {
    /// The preview the last check computed (submission, then each approval re-check).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<EngagePreview>,
    /// The picture held at the final approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<EngageSnapshot>,
    /// The last hold-the-picture verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch: Option<PictureVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<EffectRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopRecord>,
    /// Observation after the effect: seconds covered so far, and the last step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observe: Option<ObserveStep>,
    pub covered_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    /// Time of the last observe step (for the coverage sum).
    #[serde(skip)]
    pub(crate) last_observe: Option<(Time, bool)>,
}

/// What [`crate::Engine::dispatch_order`] hands the bridge for an approved engagement: send
/// the platform to loiter at the effect point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngageOrder {
    pub intent: IntentId,
    pub uid: Uid,
    pub track: String,
    pub effect_point: LonLat,
    pub effect_t: Time,
    pub effect_radius_m: f64,
    pub fallback: Fallback,
    pub window_until: Time,
}

/// The fallback the bridge must command when an engagement stops after dispatch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FallbackOrder {
    pub intent: IntentId,
    pub uid: Uid,
    pub fallback: Fallback,
    pub reason: String,
}

/// The released effect, for the bridge to send on the simulation (truth) channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectOrder {
    pub intent: IntentId,
    pub uid: Uid,
    pub track: String,
    pub point: LonLat,
    pub radius_m: f64,
    pub t: Time,
}

/// What [`crate::Engine::step_engagements`] did, for the bridge to act on.
#[derive(Clone, Debug, PartialEq)]
pub enum EngagementEvent {
    /// Voided before dispatch (nothing to command).
    Voided { intent: IntentId, reason: String },
    /// Aborted after dispatch: command the fallback.
    Stopped(FallbackOrder),
    /// The effect was released: send it on the truth channel.
    Effect(EffectOrder),
    /// The assessment finished the intent.
    Assessed {
        intent: IntentId,
        result: AssessResult,
    },
}

// ---- geometry -----------------------------------------------------------------------------

/// East and north metres of `p` from `origin` (local flat approximation).
pub fn enu(origin: LonLat, p: LonLat) -> (f64, f64) {
    (
        (p.lon - origin.lon) * M_PER_DEG * origin.lat.to_radians().cos(),
        (p.lat - origin.lat) * M_PER_DEG,
    )
}

/// `p` moved `e` metres east and `n` metres north.
pub fn offset(p: LonLat, e: f64, n: f64) -> LonLat {
    LonLat::new(
        p.lon + e / (M_PER_DEG * p.lat.to_radians().cos()),
        p.lat + n / M_PER_DEG,
    )
}

/// A track's position at `t`, carried along its velocity from its last estimate (the
/// tracker's own dead reckoning).
pub fn track_at(track: &TrackFacts, t: Time) -> LonLat {
    match track.velocity_mps {
        Some([e, n]) => {
            let dt = secs(t.ms_since(track.position.t));
            offset(track.position.value, e * dt, n * dt)
        }
        None => track.position.value,
    }
}

/// The tracker's growth rate of the position uncertainty while unobserved, m/s.
pub fn sigma_growth(track: &TrackFacts) -> f64 {
    track
        .sigma_growth_mps
        .unwrap_or_else(|| (0.3 * speed(track)).max(1.0))
}

fn speed(track: &TrackFacts) -> f64 {
    track.velocity_mps.map_or(0.0, |[e, n]| e.hypot(n))
}

/// A track's 1-sigma uncertainty at `t`, from its value at the snapshot time `now`.
pub fn track_sigma_at(track: &TrackFacts, now: Time, t: Time) -> f64 {
    track.pos_sigma_m + sigma_growth(track) * secs(t.ms_since(now)).max(0.0)
}

fn secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

fn ms(s: f64) -> i64 {
    (s * 1000.0).round() as i64
}

/// Distance from `p` to an area, metres: 0 inside.
pub fn distance_to_area(p: LonLat, area: &Area) -> f64 {
    if area.contains(p) {
        return 0.0;
    }
    let mut best = f64::INFINITY;
    for poly in &area.polygons {
        for ring in std::iter::once(&poly.exterior).chain(poly.holes.iter()) {
            let n = ring.len();
            for i in 0..n {
                let (a, b) = (enu(p, ring[i]), enu(p, ring[(i + 1) % n]));
                best = best.min(point_segment(a, b));
            }
        }
    }
    best
}

/// Distance from the origin to segment a-b (flat metres).
fn point_segment(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (-(a.0 * dx + a.1 * dy) / len2).clamp(0.0, 1.0)
    };
    (a.0 + t * dx).hypot(a.1 + t * dy)
}

/// "+95.0 s" relative to `now`.
fn rel(t: Time, now: Time) -> String {
    format!("{:+.1} s", secs(t.ms_since(now)))
}

// ---- the nomination's facts -----------------------------------------------------------------

/// The engage parameters of a spec (None for other kinds).
pub fn params(spec: &IntentSpec) -> Option<(&str, f64, Option<EngageWindow>, Fallback)> {
    match &spec.task {
        Task::Engage {
            track,
            effect_radius_m,
            window,
            fallback,
        } => Some((track.as_str(), *effect_radius_m, *window, *fallback)),
        _ => None,
    }
}

/// The window in force: the nomination's, or `default_window_s` from its creation.
pub fn effective_window(spec: &IntentSpec, cfg: &EngageRules) -> Option<EngageWindow> {
    let (_, _, window, _) = params(spec)?;
    Some(window.unwrap_or(EngageWindow {
        from: None,
        until: spec
            .created_t
            .plus_ms(i64::from(cfg.default_window_s) * 1000),
    }))
}

fn platform_of(spec: &IntentSpec) -> Option<&Uid> {
    spec.uids.first()
}

// ---- preview --------------------------------------------------------------------------------

/// The preview at the world's time: a straight-line intercept at the platform's catalog
/// cruise speed, starting `dispatch_delay_s` from now, against the track carried along its
/// estimated velocity. `Err` says why there is none.
pub fn preview(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Result<EngagePreview, String> {
    let ec = cfg.engage_rules();
    let (track_id, radius, _, _) = params(spec).ok_or("not an engagement")?;
    let window = effective_window(spec, &ec).ok_or("not an engagement")?;
    let uid = platform_of(spec).ok_or("no platform named")?;
    let v = world
        .vehicles
        .get(uid)
        .ok_or_else(|| format!("platform {uid} is not in the world model"))?;
    let platform = v
        .platform
        .as_ref()
        .ok_or_else(|| format!("platform {uid} is not in the catalog"))?;
    let cruise = platform
        .cruise_mps
        .filter(|c| *c > 0.0)
        .ok_or_else(|| format!("no cruise speed for {} in the catalog", platform.id))?;
    let start = *fresh(&v.position, v, "platform position", world, cfg)?;
    let track = world
        .tracks
        .get(track_id)
        .ok_or_else(|| format!("track {track_id} is not in the picture"))?;
    let start_t = world.t.plus_ms(ms(ec.dispatch_delay_s));
    let at_start = track_at(track, start_t);
    let (rx, ry) = enu(start, at_start);
    let [ve, vn] = track.velocity_mps.unwrap_or([0.0, 0.0]);
    // |r0 + v tau| = cruise tau, smallest tau >= 0
    let a = ve * ve + vn * vn - cruise * cruise;
    let b = 2.0 * (rx * ve + ry * vn);
    let c = rx * rx + ry * ry;
    let tau = if c == 0.0 {
        Some(0.0)
    } else if a.abs() < 1e-9 {
        (b < 0.0).then(|| -c / b)
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            None
        } else {
            let sq = disc.sqrt();
            let r1 = (-b - sq) / (2.0 * a);
            let r2 = (-b + sq) / (2.0 * a);
            [r1, r2]
                .into_iter()
                .filter(|r| r.is_finite() && *r >= 0.0)
                .min_by(f64::total_cmp)
        }
    };
    let tau = tau.ok_or_else(|| {
        format!(
            "no intercept: the platform at {cruise:.1} m/s cannot catch the track at {:.1} m/s",
            ve.hypot(vn)
        )
    })?;
    let mut effect_t = start_t.plus_ms(ms(tau));
    if let Some(from) = window.from
        && from > effect_t
    {
        effect_t = from;
    }
    let effect_point = track_at(track, effect_t);
    let distance_m = haversine_m(start, effect_point);
    let can_reach = distance_m <= cruise * secs(effect_t.ms_since(start_t)) + 1.0;
    if !can_reach {
        return Err(format!(
            "no intercept: the track is predicted {distance_m:.0} m away when the window opens, beyond reach at {cruise:.1} m/s"
        ));
    }
    let sigma_now = track.pos_sigma_m;
    let g = sigma_growth(track);
    let dt = secs(effect_t.ms_since(world.t)).max(0.0);
    let spd = speed(track);
    let (major, minor, bearing) = if spd >= 0.3 {
        (
            sigma_now + g * dt,
            sigma_now + 0.5 * g * dt,
            ve.atan2(vn).to_degrees().rem_euclid(360.0),
        )
    } else {
        (sigma_now + g * dt, sigma_now + g * dt, 0.0)
    };
    Ok(EngagePreview {
        t: world.t,
        window,
        start,
        start_t,
        cruise_mps: cruise,
        path: vec![start, effect_point],
        distance_m,
        track_path: vec![track_at(track, world.t), effect_point],
        track_speed_mps: spd,
        effect_point,
        effect_t,
        effect_radius_m: radius,
        keep_out_m: radius + ec.keep_out_m,
        track_sigma_m: sigma_now,
        ellipse: Ellipse {
            center: effect_point,
            semi_major_m: major,
            semi_minor_m: minor,
            bearing_deg: bearing,
        },
    })
}

// ---- the checks ----------------------------------------------------------------------------

fn check(rule: RuleId, uid: Option<&Uid>, outcome: Outcome, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        rule,
        uid: uid.cloned(),
        outcome,
        detail: detail.into(),
        fix: None,
    }
}

fn describe_track(id: &str, t: &TrackFacts) -> String {
    format!(
        "{id} ({}, {})",
        t.affiliation.value.as_str(),
        t.identity.as_deref().unwrap_or("unclassified")
    )
}

/// The engagement checks at submission and at each approval (the generic vehicle rules
/// run alongside, from [`crate::rules::evaluate`]). Fixed order, fixed precision.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let ec = cfg.engage_rules();
    let Some((track_id, radius, _, _)) = params(spec) else {
        return Vec::new();
    };
    let uid = platform_of(spec);
    let mut out = Vec::new();
    let track = world.tracks.get(track_id);
    out.extend(target_checks(track_id, track, world, &ec, radius));
    let pv = preview(spec, world, cfg);
    let no_preview = |rule| {
        check(
            rule,
            None,
            Outcome::Fail,
            format!(
                "cannot evaluate: no preview ({})",
                pv.as_ref().err().map_or("", String::as_str)
            ),
        )
    };
    if radius > ec.max_effect_radius_m {
        out.push(check(
            RuleId::EngagementZone,
            None,
            Outcome::Fail,
            format!(
                "effect radius {radius:.0} m is above the policy's maximum {:.0} m",
                ec.max_effect_radius_m
            ),
        ));
    } else {
        match &pv {
            Ok(p) => out.push(zone_check(p, world)),
            Err(_) => out.push(no_preview(RuleId::EngagementZone)),
        }
    }
    match &pv {
        Ok(p) => {
            out.push(no_strike_check(p, world));
            out.push(keep_out_friendly(
                p.effect_point,
                p.effect_t,
                p.keep_out_m,
                uid,
                world,
                cfg,
            ));
            out.push(keep_out_tracks(
                p.effect_point,
                p.effect_t,
                p.keep_out_m,
                track_id,
                world,
                &ec,
            ));
        }
        Err(_) => {
            out.push(no_preview(RuleId::NoStrike));
            out.push(no_preview(RuleId::KeepOutFriendly));
            out.push(no_preview(RuleId::KeepOutTracks));
        }
    }
    out.push(armed_check(uid, world));
    out.push(reach_check(uid, &pv, world, cfg, &ec));
    out
}

/// target_declared, target_identity, target_recency, target_uncertainty.
fn target_checks(
    id: &str,
    track: Option<&TrackFacts>,
    world: &WorldSnapshot,
    ec: &EngageRules,
    radius: f64,
) -> Vec<CheckResult> {
    let Some(t) = track else {
        let gone = format!("cannot evaluate: track {id} is not in the picture");
        return [
            RuleId::TargetDeclared,
            RuleId::TargetIdentity,
            RuleId::TargetRecency,
            RuleId::TargetUncertainty,
        ]
        .into_iter()
        .map(|r| check(r, None, Outcome::Fail, gone.clone()))
        .collect();
    };
    vec![
        declared_check(id, t),
        identity_check(id, t, ec),
        recency_check(id, t, world, ec),
        uncertainty_check(id, t, ec, radius),
    ]
}

fn declared_check(id: &str, t: &TrackFacts) -> CheckResult {
    let r = RuleId::TargetDeclared;
    match (&t.declared_by, t.affiliation.value) {
        (Some(by), Affiliation::Hostile) => check(
            r,
            None,
            Outcome::Pass,
            format!("{id} declared hostile by {by}"),
        ),
        (Some(by), a) => check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} is declared {} by {by}, not hostile",
                a.as_str()
            ),
        ),
        (None, a) => check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} is {} by estimate; hostile must be declared by an operator",
                a.as_str()
            ),
        ),
    }
}

fn identity_check(id: &str, t: &TrackFacts, ec: &EngageRules) -> CheckResult {
    let r = RuleId::TargetIdentity;
    let kind = t.identity.as_deref().unwrap_or("unclassified");
    if t.identity_confidence < ec.min_identity_confidence {
        return check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} identity {kind} at {:.2} is below {:.2}",
                t.identity_confidence, ec.min_identity_confidence
            ),
        );
    }
    if t.confidence < ec.min_track_confidence {
        return check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} existence confidence {:.2} is below {:.2}",
                t.confidence, ec.min_track_confidence
            ),
        );
    }
    check(
        r,
        None,
        Outcome::Pass,
        format!(
            "{id} identity {kind} at {:.2} (>= {:.2}), existence {:.2} (>= {:.2}), {} source(s)",
            t.identity_confidence,
            ec.min_identity_confidence,
            t.confidence,
            ec.min_track_confidence,
            t.sources.len()
        ),
    )
}

fn recency_check(id: &str, t: &TrackFacts, world: &WorldSnapshot, ec: &EngageRules) -> CheckResult {
    let age = secs(world.t.ms_since(t.last_seen_t));
    let o = if age <= ec.max_track_age_s {
        Outcome::Pass
    } else {
        Outcome::Fail
    };
    check(
        RuleId::TargetRecency,
        None,
        o,
        format!(
            "{id} last detected {age:.1} s ago (limit {:.1} s)",
            ec.max_track_age_s
        ),
    )
}

fn uncertainty_check(id: &str, t: &TrackFacts, ec: &EngageRules, radius: f64) -> CheckResult {
    let limit = ec.max_sigma_fraction * radius;
    let o = if t.pos_sigma_m <= limit {
        Outcome::Pass
    } else {
        Outcome::Fail
    };
    check(
        RuleId::TargetUncertainty,
        None,
        o,
        format!(
            "{id} 1-sigma {:.1} m (limit {limit:.1} m = {:.2} x effect radius {radius:.0} m)",
            t.pos_sigma_m, ec.max_sigma_fraction
        ),
    )
}

fn zone_check(p: &EngagePreview, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::EngagementZone;
    let Some(areas) = &world.areas else {
        return check(r, None, Outcome::Fail, "cannot evaluate: no area set loaded");
    };
    let zones: Vec<&Area> = areas
        .areas
        .iter()
        .filter(|a| a.kind == AreaKind::EngagementZone)
        .collect();
    if zones.is_empty() {
        return check(
            r,
            None,
            Outcome::Fail,
            "no engagement zone is defined: nothing may be engaged",
        );
    }
    let at = format!(
        "effect point ({:.5}, {:.5})",
        p.effect_point.lon, p.effect_point.lat
    );
    match zones.iter().find(|z| z.contains(p.effect_point)) {
        Some(z) => check(r, None, Outcome::Pass, format!("{at} is inside {} ({})", z.id, z.name)),
        None => check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{at} is outside every engagement zone ({})",
                zones.iter().map(|z| z.id.as_str()).collect::<Vec<_>>().join(", ")
            ),
        ),
    }
}

fn no_strike_check(p: &EngagePreview, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::NoStrike;
    let Some(areas) = &world.areas else {
        return check(r, None, Outcome::Fail, "cannot evaluate: no area set loaded");
    };
    let list: Vec<&Area> = areas
        .areas
        .iter()
        .filter(|a| a.kind == AreaKind::NoStrike)
        .collect();
    let mut nearest: Option<(f64, &Area)> = None;
    for a in &list {
        let d = distance_to_area(p.effect_point, a);
        if d <= p.effect_radius_m {
            let how = if d == 0.0 {
                "the effect point is inside it".to_string()
            } else {
                format!(
                    "{d:.0} m from the effect point, inside the effect radius {:.0} m",
                    p.effect_radius_m
                )
            };
            return check(
                r,
                None,
                Outcome::Fail,
                format!("no-strike area {} ({}): {how}", a.id, a.name),
            );
        }
        if nearest.is_none_or(|(n, _)| d < n) {
            nearest = Some((d, a));
        }
    }
    let near = nearest.map_or(String::new(), |(d, a)| {
        format!("; nearest {} at {d:.0} m", a.id)
    });
    check(
        r,
        None,
        Outcome::Pass,
        format!(
            "effect circle ({:.0} m) clear of {} no-strike area(s){near}",
            p.effect_radius_m,
            list.len()
        ),
    )
}

/// None of our vehicles but the engaging platform can be inside the keep-out at `effect_t`:
/// with a fresh velocity, its position carried along it; otherwise its last position and how
/// far it could go at catalog cruise speed since. No position at all fails.
pub fn keep_out_friendly(
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    platform: Option<&Uid>,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> CheckResult {
    let r = RuleId::KeepOutFriendly;
    let mut nearest: Option<(f64, &Uid)> = None;
    let mut count = 0;
    for (uid, v) in &world.vehicles {
        if Some(uid) == platform {
            continue;
        }
        count += 1;
        let Some(pos) = &v.position else {
            return check(
                r,
                Some(uid),
                Outcome::Fail,
                format!("vehicle {uid} has no position: cannot clear the keep-out"),
            );
        };
        let vel = v
            .velocity_mps
            .as_ref()
            .filter(|_| fresh(&v.position, v, "position", world, cfg).is_ok())
            .and_then(|f| fresh(&v.velocity_mps, v, "velocity", world, cfg).ok().map(|_| f.value));
        let (d, how) = match vel {
            Some([e, n]) => {
                let dt = secs(effect_t.ms_since(pos.t)).max(0.0);
                let p = offset(pos.value, e * dt, n * dt);
                (haversine_m(p, point), "predicted along its velocity")
            }
            None => {
                let cruise = v
                    .platform
                    .as_ref()
                    .and_then(|p| p.cruise_mps)
                    .filter(|c| c.is_finite() && *c >= 0.0);
                let Some(cruise) = cruise else {
                    return check(
                        r,
                        Some(uid),
                        Outcome::Fail,
                        format!(
                            "vehicle {uid}: no fresh velocity and no cruise speed: cannot clear the keep-out"
                        ),
                    );
                };
                let dt = secs(effect_t.ms_since(pos.t)).max(0.0);
                (
                    (haversine_m(pos.value, point) - cruise * dt).max(0.0),
                    "how far it could reach at cruise speed",
                )
            }
        };
        if d < keep_out_m {
            return check(
                r,
                Some(uid),
                Outcome::Fail,
                format!(
                    "vehicle {uid} could be {d:.0} m from the effect point at the effect time ({how}); keep-out {keep_out_m:.0} m"
                ),
            );
        }
        if nearest.is_none_or(|(n, _)| d < n) {
            nearest = Some((d, uid));
        }
    }
    let near = nearest.map_or(String::new(), |(d, u)| format!("; nearest {u} at {d:.0} m"));
    check(
        r,
        None,
        Outcome::Pass,
        format!("{count} vehicle(s) of ours clear of the {keep_out_m:.0} m keep-out at the effect time{near}"),
    )
}

/// No track but the target and declared hostiles may be inside the keep-out at `effect_t`,
/// with `keep_out_sigma` of its predicted uncertainty: neutral, unknown, suspect and assumed
/// friend tracks are protected, stale ones too (their uncertainty has grown).
pub fn keep_out_tracks(
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    target: &str,
    world: &WorldSnapshot,
    ec: &EngageRules,
) -> CheckResult {
    let r = RuleId::KeepOutTracks;
    let mut nearest: Option<(f64, &str)> = None;
    let mut count = 0;
    for (id, t) in &world.tracks {
        if id == target || t.affiliation.value == Affiliation::Hostile {
            continue;
        }
        count += 1;
        let p = track_at(t, effect_t);
        let sigma = track_sigma_at(t, world.t, effect_t);
        let d = haversine_m(p, point);
        let edge = d - ec.keep_out_sigma * sigma;
        if edge < keep_out_m {
            return check(
                r,
                None,
                Outcome::Fail,
                format!(
                    "track {} predicted {d:.0} m from the effect point at the effect time ({:.0}-sigma {:.0} m); keep-out {keep_out_m:.0} m",
                    describe_track(id, t),
                    ec.keep_out_sigma,
                    ec.keep_out_sigma * sigma
                ),
            );
        }
        if nearest.is_none_or(|(n, _)| edge < n) {
            nearest = Some((edge, id));
        }
    }
    let near = nearest.map_or(String::new(), |(d, id)| format!("; nearest {id} at {d:.0} m"));
    check(
        r,
        None,
        Outcome::Pass,
        format!("{count} protected track(s) clear of the {keep_out_m:.0} m keep-out at the effect time{near}"),
    )
}

fn armed_check(uid: Option<&Uid>, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::PlatformArmed;
    let Some(uid) = uid else {
        return check(r, None, Outcome::Fail, "no platform named");
    };
    match world.vehicles.get(uid).and_then(|v| v.platform.as_ref()) {
        None => check(
            r,
            Some(uid),
            Outcome::Fail,
            "cannot evaluate: platform not in the catalog",
        ),
        Some(p) if p.armed => check(r, Some(uid), Outcome::Pass, format!("{} is armed", p.id)),
        Some(p) => check(
            r,
            Some(uid),
            Outcome::Fail,
            format!("{} is not an armed platform (armed = false in the catalog)", p.id),
        ),
    }
}

fn reach_check(
    uid: Option<&Uid>,
    pv: &Result<EngagePreview, String>,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    ec: &EngageRules,
) -> CheckResult {
    let r = RuleId::Reach;
    let fail = |d: String| check(r, uid, Outcome::Fail, d);
    let p = match pv {
        Ok(p) => p,
        Err(e) => return fail(format!("cannot evaluate: {e}")),
    };
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return fail("cannot evaluate: platform not in the world model".into());
    };
    let Some(platform) = &v.platform else {
        return fail("cannot evaluate: platform not in the catalog".into());
    };
    let transit = secs(p.effect_t.ms_since(world.t));
    let window = p.window;
    if p.effect_t > window.until {
        return fail(format!(
            "the effect is predicted {}, after the window closes ({})",
            rel(p.effect_t, world.t),
            rel(window.until, world.t)
        ));
    }
    if transit > ec.max_transit_s {
        return fail(format!(
            "intercept takes {transit:.0} s, above the limit {:.0} s",
            ec.max_transit_s
        ));
    }
    let battery = match fresh(&v.battery_pct, v, "battery level", world, cfg) {
        Ok(b) => *b,
        Err(e) => return fail(e),
    };
    let reserve = cfg.reserve_pct;
    let back_m = v
        .home
        .as_ref()
        .map_or(0.0, |h| haversine_m(p.effect_point, h.value));
    let endurance = match platform.endurance_h.filter(|e| *e > 0.0) {
        Some(h) => {
            let full = h * 3600.0;
            let usable = battery / 100.0 * full - reserve / 100.0 * full;
            let need = transit + back_m / p.cruise_mps;
            if need > usable {
                return fail(format!(
                    "{:.0} km at {:.0} m/s and {:.1} km home need {need:.0} s; battery {battery:.0}% of {:.0} min leaves {usable:.0} s above the {reserve:.0}% reserve",
                    p.distance_m / 1000.0,
                    p.cruise_mps,
                    back_m / 1000.0,
                    full / 60.0
                ));
            }
            format!("endurance: needs {need:.0} s of {usable:.0} s above the {reserve:.0}% reserve")
        }
        None => {
            if battery < reserve {
                return fail(format!(
                    "endurance unknown for {} and battery {battery:.0}% is below the {reserve:.0}% reserve",
                    platform.id
                ));
            }
            if transit > ec.unknown_endurance_max_transit_s {
                return fail(format!(
                    "endurance unknown for {}: the intercept takes {transit:.0} s, above {:.0} s allowed without an endurance figure",
                    platform.id, ec.unknown_endurance_max_transit_s
                ));
            }
            format!(
                "endurance unknown for {}: battery {battery:.0}% >= reserve {reserve:.0}%, transit {transit:.0} s <= {:.0} s (task cost not estimated)",
                platform.id, ec.unknown_endurance_max_transit_s
            )
        }
    };
    let surface = match required_surface(platform.domain, platform.autopilot) {
        None => String::new(),
        Some(need) => match &world.terrain {
            TerrainInputs::Sampled { .. } => match world.terrain.lookup(p.effect_point) {
                Some(Surface::Land) if need == "land" => "; effect point on land".into(),
                Some(Surface::Water { depth_m }) if need == "water" && *depth_m > 0.0 => {
                    "; effect point on water".into()
                }
                Some(Surface::OutsideCoverage) | None => {
                    return fail(format!(
                        "cannot evaluate: no terrain answer for the effect point ({:.5}, {:.5})",
                        p.effect_point.lon, p.effect_point.lat
                    ));
                }
                Some(_) => {
                    return fail(format!(
                        "the effect point ({:.5}, {:.5}) is not {need}; {} needs {need}",
                        p.effect_point.lon, p.effect_point.lat, platform.id
                    ));
                }
            },
            TerrainInputs::Unavailable { reason } => {
                return check(
                    r,
                    uid,
                    Outcome::NotEvaluated,
                    format!(
                        "terrain unavailable ({reason}): {} needs the effect point on {need}",
                        platform.id
                    ),
                );
            }
            TerrainInputs::NotQueried | TerrainInputs::NotNeeded => {
                return fail("cannot evaluate: terrain was not sampled for the effect point".into());
            }
        },
    };
    check(
        r,
        uid,
        Outcome::Pass,
        format!(
            "{:.0} m at {:.1} m/s: effect {} (window closes {}); {endurance}{surface}",
            p.distance_m,
            p.cruise_mps,
            rel(p.effect_t, world.t),
            rel(window.until, world.t)
        ),
    )
}

/// Whether the platform needs a land or water answer for the effect point, and the point.
pub fn terrain_point(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Option<LonLat> {
    let uid = platform_of(spec)?;
    let p = world.vehicles.get(uid)?.platform.as_ref()?;
    required_surface(p.domain, p.autopilot)?;
    preview(spec, world, cfg).ok().map(|p| p.effect_point)
}

// ---- the snapshot and the picture -----------------------------------------------------------

/// The picture the final approval holds: the track's facts and the preview at the
/// approval's re-check, and the platform's link then.
pub fn snapshot(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Result<EngageSnapshot, String> {
    let ec = cfg.engage_rules();
    let (track_id, _, _, fallback) = params(spec).ok_or("not an engagement")?;
    let track = world
        .tracks
        .get(track_id)
        .ok_or_else(|| format!("track {track_id} is not in the picture"))?;
    let preview = preview(spec, world, cfg)?;
    let interval = platform_of(spec)
        .and_then(|u| world.vehicles.get(u))
        .and_then(|v| v.report_interval_ms);
    let by_link = interval.map_or(0, |i| (ec.link_gap_intervals * f64::from(i)).round() as i64);
    Ok(EngageSnapshot {
        t: world.t,
        track_id: track_id.to_string(),
        track: track.clone(),
        preview,
        report_interval_ms: interval,
        link_gap_limit_ms: ms(ec.link_gap_s).max(by_link),
        fallback,
    })
}

/// Hold the picture: the checks after approval against the snapshot, and (once dispatched)
/// whether the effect may be released now. Returns the results, whether the picture holds
/// (every picture check passes) and whether the release conditions are met too.
pub fn picture(
    spec: &IntentSpec,
    snap: &EngageSnapshot,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    dispatched: bool,
) -> (Vec<CheckResult>, bool, bool) {
    let ec = cfg.engage_rules();
    let p = &snap.preview;
    let uid = platform_of(spec);
    let mut picture_checks = Vec::new();
    let track = world.tracks.get(&snap.track_id);
    let id = snap.track_id.as_str();
    // declared, reclassified, recency
    match track {
        None => {
            picture_checks.push(check(
                RuleId::TargetDeclared,
                None,
                Outcome::Fail,
                format!("track {id} is no longer in the picture"),
            ));
        }
        Some(t) => {
            picture_checks.push(declared_check(id, t));
            picture_checks.push(reclass_check(id, &snap.track, t, &ec));
            picture_checks.push(recency_check(id, t, world, &ec));
            picture_checks.push(deviation_check(id, snap, t, world, &ec));
        }
    }
    picture_checks.push(keep_out_friendly(
        p.effect_point,
        p.effect_t,
        p.keep_out_m,
        uid,
        world,
        cfg,
    ));
    picture_checks.push(keep_out_tracks(
        p.effect_point,
        p.effect_t,
        p.keep_out_m,
        id,
        world,
        &ec,
    ));
    picture_checks.push(link_check(uid, snap, world));
    picture_checks.push(window_check(snap, world, &ec, dispatched));
    let holds = picture_checks.iter().all(|c| c.outcome == Outcome::Pass);
    let mut out = picture_checks;
    let mut ready = false;
    if dispatched && holds {
        let release = release_checks(uid, snap, track, world, &ec);
        ready = release.iter().all(|c| c.outcome == Outcome::Pass);
        out.extend(release);
    }
    (out, holds, ready)
}

fn reclass_check(id: &str, then: &TrackFacts, now: &TrackFacts, ec: &EngageRules) -> CheckResult {
    let r = RuleId::TargetIdentity;
    if then.identity.is_some() && then.identity != now.identity {
        return check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} reclassified: {} at approval, now {}",
                then.identity.as_deref().unwrap_or("unclassified"),
                now.identity.as_deref().unwrap_or("unclassified")
            ),
        );
    }
    identity_check(id, now, ec)
}

fn deviation_check(
    id: &str,
    snap: &EngageSnapshot,
    now: &TrackFacts,
    world: &WorldSnapshot,
    ec: &EngageRules,
) -> CheckResult {
    let expected = track_at(&snap.track, world.t);
    let estimate = track_at(now, world.t);
    let d = haversine_m(expected, estimate);
    let tol = (ec.deviation_fraction * snap.preview.effect_radius_m).max(ec.deviation_min_m);
    let o = if d <= tol { Outcome::Pass } else { Outcome::Fail };
    check(
        RuleId::TrackDeviation,
        None,
        o,
        format!(
            "{id} is {d:.0} m from where the approval's snapshot predicts it now (tolerance {tol:.0} m)"
        ),
    )
}

fn link_check(uid: Option<&Uid>, snap: &EngageSnapshot, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::PlatformLink;
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return check(r, uid, Outcome::Fail, "platform is no longer in the world model");
    };
    let Some(pos) = &v.position else {
        return check(r, uid, Outcome::Fail, "platform has no position");
    };
    let age = world.t.ms_since(pos.t);
    if age > snap.link_gap_limit_ms {
        return check(
            r,
            uid,
            Outcome::Fail,
            format!(
                "link lost: no position for {:.1} s (the approval assumed at most {:.1} s)",
                secs(age),
                secs(snap.link_gap_limit_ms)
            ),
        );
    }
    if let (Some(then), Some(now)) = (snap.report_interval_ms, v.report_interval_ms)
        && u64::from(now) > 2 * u64::from(then)
    {
        return check(
            r,
            uid,
            Outcome::Fail,
            format!(
                "link degraded: reports every {:.1} s, the approval assumed every {:.1} s",
                f64::from(now) / 1000.0,
                f64::from(then) / 1000.0
            ),
        );
    }
    check(
        r,
        uid,
        Outcome::Pass,
        format!(
            "platform heard {:.1} s ago (limit {:.1} s)",
            secs(age),
            secs(snap.link_gap_limit_ms)
        ),
    )
}

fn window_check(snap: &EngageSnapshot, world: &WorldSnapshot, ec: &EngageRules, dispatched: bool) -> CheckResult {
    let r = RuleId::EngageWindow;
    let p = &snap.preview;
    if world.t > p.window.until {
        return check(
            r,
            None,
            Outcome::Fail,
            format!("the window closed {} ago", rel(world.t, p.window.until).trim_start_matches('+')),
        );
    }
    let late_at = p.effect_t.plus_ms(ms(ec.late_s));
    if dispatched && world.t > late_at {
        return check(
            r,
            None,
            Outcome::Fail,
            format!(
                "the effect was not released within {:.0} s of the predicted time",
                ec.late_s
            ),
        );
    }
    check(
        r,
        None,
        Outcome::Pass,
        format!(
            "effect predicted {}, window closes {}",
            rel(p.effect_t, world.t),
            rel(p.window.until, world.t)
        ),
    )
}

/// arrival, effect time, alignment, uncertainty: whether the effect may be released now.
fn release_checks(
    uid: Option<&Uid>,
    snap: &EngageSnapshot,
    track: Option<&TrackFacts>,
    world: &WorldSnapshot,
    ec: &EngageRules,
) -> Vec<CheckResult> {
    let p = &snap.preview;
    let mut out = Vec::new();
    // platform at the point
    let arrival = match uid.and_then(|u| world.vehicles.get(u)).and_then(|v| v.position.as_ref()) {
        Some(pos) => {
            let d = haversine_m(pos.value, p.effect_point);
            let o = if d <= ec.arrival_m { Outcome::Pass } else { Outcome::Fail };
            check(
                RuleId::Arrival,
                uid,
                o,
                format!("platform {d:.0} m from the effect point (within {:.0} m to release)", ec.arrival_m),
            )
        }
        None => check(RuleId::Arrival, uid, Outcome::Fail, "platform has no position"),
    };
    out.push(arrival);
    let from = p.window.from.map_or(p.effect_t, |f| f.max(p.effect_t));
    let o = if world.t >= from { Outcome::Pass } else { Outcome::Fail };
    out.push(check(
        RuleId::EffectTime,
        None,
        o,
        format!("predicted effect time {}", rel(from, world.t)),
    ));
    match track {
        Some(t) => {
            let d = haversine_m(track_at(t, world.t), p.effect_point);
            let o = if d <= p.effect_radius_m { Outcome::Pass } else { Outcome::Fail };
            out.push(check(
                RuleId::Alignment,
                None,
                o,
                format!(
                    "{} estimated {d:.0} m from the effect point (effect radius {:.0} m)",
                    snap.track_id, p.effect_radius_m
                ),
            ));
            out.push(uncertainty_check(&snap.track_id, t, ec, p.effect_radius_m));
        }
        None => out.push(check(
            RuleId::Alignment,
            None,
            Outcome::Fail,
            format!("{} is not in the picture", snap.track_id),
        )),
    }
    out
}

// ---- assessment -----------------------------------------------------------------------------

/// Which sensor types can confirm the presence or absence of a contact in `domain`. AIS is
/// left out (cooperative, and the target may spoof it), ESM too (only emitters).
fn sees(sensor: &str, domain: Domain) -> bool {
    match domain {
        Domain::Subsurface => sensor == "sonar",
        Domain::Surface => matches!(sensor, "radar" | "eo_ir" | "sonar"),
        Domain::Air | Domain::Ground => matches!(sensor, "radar" | "eo_ir"),
    }
}

/// One observation step after the effect.
pub fn observe(
    snap: &EngageSnapshot,
    effect: &EffectRecord,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> ObserveStep {
    let ec = cfg.engage_rules();
    let domain = snap.track.domain;
    let mut covered_by = Vec::new();
    for (uid, v) in &world.vehicles {
        let Ok(pos) = fresh(&v.position, v, "position", world, cfg) else {
            continue;
        };
        let Some(p) = &v.platform else { continue };
        let d = haversine_m(*pos, effect.point);
        let best: Option<&SensorFact> = p
            .sensors
            .iter()
            .filter(|s| sees(&s.kind, domain) && s.range_m >= d)
            .max_by(|a, b| a.range_m.total_cmp(&b.range_m));
        if let Some(s) = best {
            covered_by.push(format!("{uid}:{} {:.0} m", s.kind, d));
        }
    }
    let track = world.tracks.get(&snap.track_id);
    let grace = effect.t.plus_ms(ms(ec.assess_grace_s));
    let seen = track
        .map(|t| t.last_seen_t)
        .filter(|t| *t > grace);
    ObserveStep {
        covered_by,
        track_present: track.is_some(),
        seen_after_effect_t: seen,
        sources: match (seen, track) {
            (Some(_), Some(t)) => t.sources.clone(),
            _ => Vec::new(),
        },
    }
}

/// The coverage added by a step at `t` after one at `prev`: the time between two covered
/// steps, at most 5 s (a gap in steps is not coverage).
pub fn coverage_increment(prev: Option<(Time, bool)>, t: Time, covered: bool) -> f64 {
    match prev {
        Some((pt, true)) if covered => secs(t.ms_since(pt)).clamp(0.0, 5.0),
        _ => 0.0,
    }
}

/// The verdict after a step, if there is one yet.
pub fn assess(
    snap: &EngageSnapshot,
    effect: &EffectRecord,
    step: &ObserveStep,
    covered_s: f64,
    t: Time,
    cfg: &RulesConfig,
) -> Option<(AssessResult, String)> {
    let ec = cfg.engage_rules();
    let id = &snap.track_id;
    if let Some(seen) = step.seen_after_effect_t {
        return Some((
            AssessResult::Persists,
            format!(
                "{id} detected {} after the effect by {}",
                rel(seen, effect.t),
                if step.sources.is_empty() {
                    "our sensors".to_string()
                } else {
                    step.sources.join(", ")
                }
            ),
        ));
    }
    if covered_s >= ec.assess_dwell_s {
        return Some((
            AssessResult::Ceased,
            format!(
                "no detection of {id} where expected: the effect point was covered for {covered_s:.1} s after the effect (by {}){}",
                step.covered_by.join(", "),
                if step.track_present { "" } else { "; the track has been dropped" }
            ),
        ));
    }
    if t >= effect.t.plus_ms(ms(ec.assess_window_s)) {
        return Some((
            AssessResult::Unknown,
            format!(
                "the effect point was covered for only {covered_s:.1} s of the {:.0} s needed within {:.0} s after the effect",
                ec.assess_dwell_s, ec.assess_window_s
            ),
        ));
    }
    None
}
