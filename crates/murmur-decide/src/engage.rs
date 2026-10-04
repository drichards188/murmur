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
//!    the tier asks (at least two). A signer first asks for the picture
//!    ([`crate::Engine::issue_picture`]): a [`PictureToken`] bound to their session, the plan
//!    shown and the track's identity, affiliation and declaration then, valid
//!    `token_ttl_s`; a signature quotes it. The first signature freezes the plan it was shown
//!    ([`FrozenPlan`]: effect point and time, radius, window, platform, fallback, the track's
//!    identity); every later signer signs that exact plan. At each later signature and every
//!    second while signatures wait, the plan is checked again ([`evaluate`] with the plan:
//!    the track predicted within tolerance of the plan's point at its time, zone, no-strike
//!    and keep-outs on the plan's point, the platform still able to be there, the same
//!    identity and declaration, the signatures within `max_signing_s`); if it no longer
//!    holds, every signature is dropped and a new plan must be signed. See
//!    [`crate::Engine::approve_engagement`].
//! 5. **Hold the picture** ([`picture`]): the final approval stores an [`EngageSnapshot`]
//!    (the track's facts, the frozen plan, the platform's link assumption). Every step after
//!    approval re-checks the picture against it and voids (before dispatch) or aborts (after)
//!    when the track moved beyond tolerance from the snapshot's prediction, was reclassified
//!    or undeclared, went stale, a protected track or one of our vehicles is predicted inside
//!    the keep-out, the platform's link degraded past the approval's assumption or it lost
//!    GNSS, the window closed or the platform was late.
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

    /// The task the bridge commands for it: hold, rtl, or an orbit around where the
    /// platform is. Its report (`op.state fallback`, `"<fallback> commanded (<task>): ..."`)
    /// names both, and replay checks them against the stop.
    pub fn task(self) -> &'static str {
        match self {
            Fallback::Abort => "hold",
            Fallback::Return => "rtl",
            Fallback::Loiter => "orbit",
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
    /// The rules version the checks run under (from [`RulesConfig::rules_version`]; not a
    /// policy key).
    #[serde(skip, default = "rules_version_now")]
    pub rules_version: u32,
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
    /// The picture must be checked at least this often while an engagement is in play; a
    /// longer gap (a stalled loop) fails the picture, seconds.
    pub max_picture_gap_s: f64,
    /// What an unreleased build called `token_ttl_s`. A renamed key stays a field of its
    /// own, read through the accessor, so a log that used the old name parses and
    /// re-serializes byte for byte (a serde alias would parse it but write the new name back,
    /// and replay requires each line to be its event's serialization).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_max_age_s: Option<f64>,
    /// A picture token (what a signer was shown) is valid this long, seconds. Default 15.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_ttl_s: Option<f64>,
    /// Every signature on one plan within this long of the first, seconds; past it the
    /// signatures are dropped and a new plan must be signed.
    pub max_signing_s: f64,
    /// A track's velocity estimate wobbles: a prediction carried over a span is allowed the
    /// track's uncertainty growth (`sigma_growth_mps`) times that span on top of the
    /// deviation tolerance, at most this much, metres (0: none). Default 300 (schema 2,
    /// rules 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_drift_max_m: Option<f64>,
    /// A target slower than this is predicted where it is: a holding target's velocity
    /// estimate wobbles around zero, and carried a minute or two ahead the wobble would move
    /// the effect point by tens of metres, m/s (0: always carried along its estimate).
    /// Default 1.5 (schema 2, rules 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holding_speed_mps: Option<f64>,
    /// One of our vehicles predicted along its velocity is uncertain too (it may turn, be
    /// retasked): it counts with `keep_out_sigma` times its uncertainty, which grows at this
    /// rate over the prediction span, m/s. Not 0.3 times its speed as for tracks: a
    /// fixed wing circling while it waits would then block every engagement within
    /// kilometres. Default 1 (schema 2, rules 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friendly_growth_mps: Option<f64>,
}

fn rules_version_now() -> u32 {
    crate::rules::RULES_VERSION
}

impl EngageRules {
    /// [`EngageRules::token_ttl_s`] (or its old name) or its default.
    pub fn token_ttl(&self) -> f64 {
        self.token_ttl_s.or(self.seen_max_age_s).unwrap_or(15.0)
    }

    /// [`EngageRules::plan_drift_max_m`] or its default.
    pub fn plan_drift_max(&self) -> f64 {
        self.plan_drift_max_m.unwrap_or(300.0)
    }

    /// [`EngageRules::holding_speed_mps`] or its default.
    pub fn holding_speed(&self) -> f64 {
        self.holding_speed_mps.unwrap_or(1.5)
    }

    /// [`EngageRules::friendly_growth_mps`] or its default.
    pub fn friendly_growth(&self) -> f64 {
        self.friendly_growth_mps.unwrap_or(1.0)
    }

    /// The decisions of rules version 2 on (see the changelog).
    fn v2(&self) -> bool {
        self.rules_version >= 2
    }

    /// The decisions of rules version 3 on (see the changelog).
    fn v3(&self) -> bool {
        self.rules_version >= 3
    }

    /// The optional keys this section sets, each with the policy schema that introduced it
    /// (or, for an old name, the first schema whose builds read it): a log claiming an
    /// older schema can't carry them (replay checks).
    pub fn schema_keys(&self) -> Vec<(u32, &'static str)> {
        let mut keys = Vec::new();
        if self.seen_max_age_s.is_some() {
            keys.push((2, "seen_max_age_s"));
        }
        if self.plan_drift_max_m.is_some() {
            keys.push((2, "plan_drift_max_m"));
        }
        if self.holding_speed_mps.is_some() {
            keys.push((2, "holding_speed_mps"));
        }
        if self.friendly_growth_mps.is_some() {
            keys.push((2, "friendly_growth_mps"));
        }
        keys
    }
}

impl Default for EngageRules {
    fn default() -> Self {
        EngageRules {
            rules_version: crate::rules::RULES_VERSION,
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
            max_picture_gap_s: 5.0,
            seen_max_age_s: None,
            token_ttl_s: None,
            max_signing_s: 120.0,
            plan_drift_max_m: None,
            holding_speed_mps: None,
            friendly_growth_mps: None,
        }
    }
}

impl EngageRules {
    /// Every threshold inside a bound, so a policy file can't switch a check off (a zero
    /// confidence, a zero keep-out) or make it meaningless (an unbounded age or gap). The
    /// bounds are the table below; the values in force are logged with the policy.
    pub fn validate(&self) -> Result<(), String> {
        let within = |v: f64, lo: f64, hi: f64, n: &str| {
            if v.is_finite() && v >= lo && v <= hi {
                Ok(())
            } else {
                Err(format!("rules.engage.{n} must be within {lo}..{hi}, got {v}"))
            }
        };
        within(self.min_identity_confidence, 0.3, 1.0, "min_identity_confidence")?;
        within(self.min_track_confidence, 0.3, 1.0, "min_track_confidence")?;
        within(self.max_track_age_s, 1.0, 30.0, "max_track_age_s")?;
        within(self.max_sigma_fraction, 0.05, 1.0, "max_sigma_fraction")?;
        within(self.max_effect_radius_m, 1.0, 1000.0, "max_effect_radius_m")?;
        within(self.keep_out_m, 50.0, 5000.0, "keep_out_m")?;
        within(self.keep_out_sigma, 1.0, 5.0, "keep_out_sigma")?;
        within(self.dispatch_delay_s, 0.0, 300.0, "dispatch_delay_s")?;
        within(self.max_transit_s, 10.0, 7200.0, "max_transit_s")?;
        within(
            self.unknown_endurance_max_transit_s,
            0.0,
            3600.0,
            "unknown_endurance_max_transit_s",
        )?;
        within(self.deviation_fraction, 0.1, 5.0, "deviation_fraction")?;
        within(self.deviation_min_m, 5.0, 500.0, "deviation_min_m")?;
        within(self.link_gap_s, 1.0, 120.0, "link_gap_s")?;
        within(self.link_gap_intervals, 1.0, 10.0, "link_gap_intervals")?;
        within(self.arrival_m, 10.0, 2000.0, "arrival_m")?;
        within(self.late_s, 0.0, 600.0, "late_s")?;
        within(self.assess_grace_s, 0.0, 30.0, "assess_grace_s")?;
        within(self.assess_dwell_s, 1.0, 600.0, "assess_dwell_s")?;
        within(self.assess_window_s, 10.0, 3600.0, "assess_window_s")?;
        within(self.max_picture_gap_s, 1.0, 30.0, "max_picture_gap_s")?;
        if self.token_ttl_s.is_some() && self.seen_max_age_s.is_some() {
            return Err(
                "rules.engage: token_ttl_s and its old name seen_max_age_s are both set".into(),
            );
        }
        within(self.token_ttl(), 1.0, 120.0, "token_ttl_s")?;
        within(self.max_signing_s, 10.0, 900.0, "max_signing_s")?;
        within(self.plan_drift_max(), 0.0, 2000.0, "plan_drift_max_m")?;
        within(self.holding_speed(), 0.0, 5.0, "holding_speed_mps")?;
        within(self.friendly_growth(), 0.1, 10.0, "friendly_growth_mps")?;
        if self.assess_window_s < self.assess_dwell_s {
            return Err("rules.engage.assess_window_s must be at least assess_dwell_s".into());
        }
        if !(1..=86_400).contains(&self.default_window_s) {
            return Err("rules.engage.default_window_s must be within 1..86400".into());
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
    /// The bridge stopped it for a reason outside the picture (its task failed, it was
    /// lost).
    System,
    /// Another order was dispatched to its platform: that order stands and no fallback is
    /// commanded. `by` is that intent and `source` who it came from.
    Superseded {
        by: IntentId,
        source: crate::intent::Source,
    },
}

/// What a signer is shown of the target: its identity, affiliation and who declared it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackIdentity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub affiliation: Affiliation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
}

impl TrackIdentity {
    pub fn of(id: &str, t: &TrackFacts) -> Self {
        TrackIdentity {
            id: id.to_string(),
            identity: t.identity.clone(),
            affiliation: t.affiliation.value,
            declared_by: t.declared_by.clone(),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{} {} by {}",
            self.identity.as_deref().unwrap_or("unclassified"),
            self.affiliation.as_str(),
            self.declared_by.as_deref().unwrap_or("nobody")
        )
    }
}

/// The plan the first signature froze: the effect point and time, radius and window (the
/// preview its signer was shown), on the platform and with the fallback of the nomination,
/// against the track as it was identified then. Every later signature signs it exactly; the
/// approval's snapshot carries it and the release executes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrozenPlan {
    /// When the first signature froze it, and whose it was.
    pub t: Time,
    pub by: OperatorId,
    pub preview: EngagePreview,
    pub track: TrackIdentity,
}

/// What the engine showed a signer ([`crate::Engine::issue_picture`], logged as
/// `picture.issued`): bound to their session, the plan shown (the frozen one, or the preview
/// a first signature would freeze) and the track's identity then, until `expires_t`. A
/// signature quotes it; it is refused if the token expired, is another session's, or its
/// plan or track facts no longer match. Used once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PictureToken {
    pub id: u64,
    pub intent: IntentId,
    pub by: OperatorId,
    pub session: SessionId,
    pub t: Time,
    pub expires_t: Time,
    pub plan: EngagePreview,
    /// The plan was already frozen by an earlier signature when it was shown.
    pub frozen: bool,
    pub track: TrackIdentity,
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
    /// The plan the first signature froze (cleared when the signatures are dropped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<FrozenPlan>,
    /// The picture each signature so far is on (the final one's is `snapshot`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub signed: Vec<SignedPicture>,
    /// The last time signatures were dropped because the picture changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetRecord>,
    /// The picture held at the final approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<EngageSnapshot>,
    /// The radius the platform loiters at over the effect point ([`loiter_radius`] on the
    /// final approval's world); the engagement order carries it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loiter_radius_m: Option<f64>,
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

/// One signature's picture.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SignedPicture {
    pub by: OperatorId,
    pub t: Time,
    pub snapshot: EngageSnapshot,
}

/// Signatures dropped because the picture changed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResetRecord {
    pub t: Time,
    pub dropped: Vec<OperatorId>,
    pub reason: String,
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
    /// The radius the platform loiters at over the effect point ([`loiter_radius`], from its
    /// catalog facts at the final signature). Rules 4 on; absent before (the bridge chose it
    /// and nothing logged it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loiter_radius_m: Option<f64>,
}

/// The radius `uid` loiters at over an effect point: the rules' margin over its catalog
/// minimum turn radius (the fixed-wing default for a fixed wing without one), rounded up to
/// a metre, at least 30 m. The bridge flies the intercept at it; the engine puts it in the
/// engagement order from the final signature's world, so replay recomputes it.
pub fn loiter_radius(uid: &Uid, world: &WorldSnapshot, cfg: &RulesConfig) -> f64 {
    let min = world
        .vehicles
        .get(uid)
        .and_then(|v| v.platform.as_ref())
        .and_then(|p| {
            p.min_turn_radius_m.or_else(|| {
                p.autopilot
                    .filter(|a| a.is_fixed_wing())
                    .map(|_| cfg.fixed_wing_default_min_turn_radius_m)
            })
        });
    min.map_or(30.0, |m| (m * cfg.orbit_radius_margin).ceil())
        .max(30.0)
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

/// The velocity the engagement chain predicts its target with: the track's estimate, or
/// none when it is slower than `holding_speed_mps` (a holding target's estimate wobbles
/// around zero).
pub fn target_velocity(track: &TrackFacts, ec: &EngageRules) -> [f64; 2] {
    let v = track.velocity_mps.unwrap_or([0.0, 0.0]);
    // rules 1: the estimate as it is
    if !ec.v2() {
        return v;
    }
    if v[0].hypot(v[1]) < ec.holding_speed() {
        [0.0, 0.0]
    } else {
        v
    }
}

/// [`track_at`] for the engagement's target: carried along [`target_velocity`].
pub fn target_at(track: &TrackFacts, t: Time, ec: &EngageRules) -> LonLat {
    let [e, n] = target_velocity(track, ec);
    let dt = secs(t.ms_since(track.position.t));
    offset(track.position.value, e * dt, n * dt)
}

/// How far a prediction over `span_s` may be off because the velocity estimate it rests on
/// wobbles: the track's uncertainty growth times the span, at most `plan_drift_max_m`.
fn drift_allowance(track: &TrackFacts, span_s: f64, ec: &EngageRules) -> f64 {
    (sigma_growth(track) * span_s.max(0.0)).min(ec.plan_drift_max())
}

/// Two position estimates of one track differ by their noise alone: twice the 1-sigma of
/// their difference (each estimate's `pos_sigma_m`, bounded by the uncertainty check).
fn noise_allowance(sigma_a: f64, sigma_b: f64) -> f64 {
    2.0 * sigma_a.hypot(sigma_b)
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

/// Distance from `p` to an area's nearest edge, metres (whether `p` is inside or not).
pub fn boundary_distance(p: LonLat, area: &Area) -> f64 {
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

/// An engagement's terrain point (its predicted effect point, for a boat or ground
/// platform) moves with the track every second, so the oracle may answer it from a sample
/// taken this near (logged with where and how far: [`crate::world::SampleFrom`]); samples
/// that near which disagree (a shoreline) leave it not evaluated.
pub const TERRAIN_NEAR_M: f64 = 50.0;

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
    let at_start = target_at(track, start_t, &ec);
    let (rx, ry) = enu(start, at_start);
    let [ve, vn] = target_velocity(track, &ec);
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
    let effect_point = target_at(track, effect_t, &ec);
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
    let spd = ve.hypot(vn);
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
        track_path: vec![target_at(track, world.t, &ec), effect_point],
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
    if let Some(plan) = &world.plan {
        out.extend(plan_checks(spec, plan, world, cfg));
        return out;
    }
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
    if ec.v2() {
        out.push(free_check(uid, world));
    }
    out.push(reach_check(uid, &pv, world, cfg, &ec));
    out
}

/// One engagement per platform: a platform with another engagement in play (pending,
/// approved or under way, `engaged` as the engine logs it) can't take a second one, whose
/// dispatch would stop the first.
fn free_check(uid: Option<&Uid>, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::PlatformFree;
    match uid.and_then(|u| world.engaged.get(u)) {
        Some(other) => check(
            r,
            uid,
            Outcome::Fail,
            format!("platform has engagement {other} in play: one engagement per platform"),
        ),
        None => check(r, uid, Outcome::Pass, "no other engagement in play on the platform"),
    }
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
    // the whole effect circle, not just its centre: inside a zone, at least the effect
    // radius from its boundary
    let margin = |z: &Area| boundary_distance(p.effect_point, z);
    if let Some(z) = zones
        .iter()
        .find(|z| z.contains(p.effect_point) && margin(z) >= p.effect_radius_m)
    {
        return check(
            r,
            None,
            Outcome::Pass,
            format!(
                "{at} with its {:.0} m radius is inside {} ({}), {:.0} m from its edge",
                p.effect_radius_m,
                z.id,
                z.name,
                margin(z)
            ),
        );
    }
    match zones.iter().find(|z| z.contains(p.effect_point)) {
        Some(z) => check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{at} is inside {} but only {:.0} m from its edge: the {:.0} m effect circle crosses it",
                z.id,
                margin(z),
                p.effect_radius_m
            ),
        ),
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
/// Rules 3: how far outside a circle's radius its vehicle may be and still be on it, m. The
/// disc it is predicted on includes it, so the disc is never farther from the effect point
/// than where the vehicle was reported.
pub const CIRCLE_SLACK_M: f64 = 50.0;
/// Rules 3: a circle's center (LOITER, RTL: the navigation target) older than this, as of
/// the world's time, does not hold its vehicle, ms. A task's own center has no age; its
/// fact must be fresh as any.
pub const CIRCLE_CENTER_MAX_AGE_MS: i64 = 12_000;
/// Rules 3: a circling vehicle is also carried from its position along its velocity, for at
/// most this long (beyond it, it turns on its circle), s: the nearer of that and its disc
/// counts, at every span.
pub const CIRCLE_CARRY_S: f64 = 5.0;

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
        let ec = cfg.engage_rules();
        // rules 3: a vehicle flying a circle stays on it; its momentary heading, carried
        // a minute or more ahead, would put it kilometres away in any direction. The circle
        // holds only a vehicle that is on it: its position, reported with the circle, within
        // the circle's radius and a small slack of the center. Otherwise the circle is
        // wrong or stale, and the vehicle is predicted from where it is.
        // Each report is judged on its own freshness: the position by the usual link-aware
        // rule, the center by its own age.
        let reported = v.circling.as_ref().filter(|_| ec.v3());
        let pos_fresh = fresh(&v.position, v, "position", world, cfg).is_ok();
        let circling = reported.filter(|c| {
            let center_fresh = match c.value.center_t {
                Some(ct) => world.t.ms_since(ct) <= CIRCLE_CENTER_MAX_AGE_MS,
                None => fresh(&v.circling, v, "circle", world, cfg).is_ok(),
            };
            pos_fresh
                && center_fresh
                && haversine_m(pos.value, c.value.center) <= c.value.radius_m + CIRCLE_SLACK_M
        });
        let vel = v
            .velocity_mps
            .as_ref()
            .filter(|_| fresh(&v.position, v, "position", world, cfg).is_ok())
            .and_then(|f| fresh(&v.velocity_mps, v, "velocity", world, cfg).ok().map(|_| f.value));
        let span = secs(effect_t.ms_since(pos.t)).max(0.0);
        let off_circle = reported.is_some() && circling.is_none();
        // from where it is: along its velocity (or how far it could reach), over the whole
        // span, or with a circle that holds it for at most CIRCLE_CARRY_S; the uncertainty
        // grows over the whole span either way
        let carry = if circling.is_some() {
            span.min(CIRCLE_CARRY_S)
        } else {
            span
        };
        let disc = circling.map(|c| {
            // anywhere on its disc, slack included, with the same uncertainty growth as any
            // of ours (it may be retasked) since the older of its reports; fail closed on
            // the near edge
            let dt = secs(effect_t.ms_since(if c.t < pos.t { c.t } else { pos.t })).max(0.0);
            let margin = ec.keep_out_sigma * ec.friendly_growth() * dt;
            (
                (haversine_m(c.value.center, point) - c.value.radius_m - CIRCLE_SLACK_M - margin)
                    .max(0.0),
                std::borrow::Cow::<'static, str>::from(format!(
                    "circling within {:.0} m of its center, less its uncertainty",
                    c.value.radius_m
                )),
            )
        });
        let here: Option<(f64, std::borrow::Cow<'static, str>)> = match vel {
            Some([e, n]) => {
                // along its velocity, with an uncertainty that grows with the span (it may
                // turn or be retasked); fail closed on the near edge
                let p = offset(pos.value, e * carry, n * carry);
                let margin = ec.keep_out_sigma * ec.friendly_growth() * span;
                Some(if circling.is_some() {
                    (
                        (haversine_m(p, point) - margin).max(0.0),
                        format!(
                            "predicted along its velocity for {CIRCLE_CARRY_S:.0} s, then on its circle, less its uncertainty"
                        )
                        .into(),
                    )
                } else if off_circle {
                    (
                        (haversine_m(p, point) - margin).max(0.0),
                        "predicted along its velocity, less its uncertainty; its reported circle does not hold its position".into(),
                    )
                } else if ec.v2() {
                    (
                        (haversine_m(p, point) - margin).max(0.0),
                        "predicted along its velocity, less its uncertainty".into(),
                    )
                } else {
                    (haversine_m(p, point), "predicted along its velocity".into())
                })
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
                Some((
                    (haversine_m(pos.value, point) - cruise * carry).max(0.0),
                    "how far it could reach at cruise speed".into(),
                ))
            }
        };
        let (d, how) = match (disc, here) {
            (Some(a), Some(b)) => {
                if b.0 < a.0 {
                    b
                } else {
                    a
                }
            }
            (Some(a), None) | (None, Some(a)) => a,
            // no circle holds it and nothing from its position: fail closed
            (None, None) => {
                return check(
                    r,
                    Some(uid),
                    Outcome::Fail,
                    format!("vehicle {uid}: cannot predict it: cannot clear the keep-out"),
                );
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

/// No track but the target and tracks an operator declared hostile (`declared_by` set) may be
/// inside the keep-out at `effect_t`, with `keep_out_sigma` of its predicted uncertainty:
/// neutral, unknown, suspect and assumed friend tracks are protected, a hostile affiliation
/// nobody declared too, stale ones too (their uncertainty has grown). Another declared
/// hostile is exempt like the target: an open design question (DECISIONS.md).
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
        // exempt: the target, and other tracks an operator declared hostile (an affiliation
        // without a declaration is protected)
        if id == target
            || (t.affiliation.value == Affiliation::Hostile && t.declared_by.is_some())
        {
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
    let tail = match endurance_and_surface(uid, v, platform, p.effect_point, p.distance_m, p.cruise_mps, transit, world, cfg, ec) {
        Ok(t) => t,
        Err(c) => return c,
    };
    check(
        r,
        uid,
        Outcome::Pass,
        format!(
            "{:.0} m at {:.1} m/s: effect {} (window closes {}); {tail}",
            p.distance_m,
            p.cruise_mps,
            rel(p.effect_t, world.t),
            rel(window.until, world.t)
        ),
    )
}

/// The endurance and the surface part of `reach`: the battery (or, without an endurance
/// figure, the weaker check) for a transit of `transit` seconds and the way home from
/// `point`, and `point` on the surface the platform needs. `Ok`: the detail; `Err`: the
/// failed check.
#[allow(clippy::too_many_arguments)]
fn endurance_and_surface(
    uid: Option<&Uid>,
    v: &crate::world::VehicleFacts,
    platform: &crate::catalog::PlatformFacts,
    point: LonLat,
    distance_m: f64,
    cruise_mps: f64,
    transit: f64,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    ec: &EngageRules,
) -> Result<String, CheckResult> {
    let r = RuleId::Reach;
    let fail = |d: String| check(r, uid, Outcome::Fail, d);
    let battery = match fresh(&v.battery_pct, v, "battery level", world, cfg) {
        Ok(b) => *b,
        Err(e) => return Err(fail(e)),
    };
    let reserve = cfg.reserve_pct;
    let back_m = v
        .home
        .as_ref()
        .map_or(0.0, |h| haversine_m(point, h.value));
    let endurance = match platform.endurance_h.filter(|e| *e > 0.0) {
        Some(h) => {
            let full = h * 3600.0;
            let usable = battery / 100.0 * full - reserve / 100.0 * full;
            let need = transit + back_m / cruise_mps;
            if need > usable {
                return Err(fail(format!(
                    "{:.0} km at {:.0} m/s and {:.1} km home need {need:.0} s; battery {battery:.0}% of {:.0} min leaves {usable:.0} s above the {reserve:.0}% reserve",
                    distance_m / 1000.0,
                    cruise_mps,
                    back_m / 1000.0,
                    full / 60.0
                )));
            }
            format!("endurance: needs {need:.0} s of {usable:.0} s above the {reserve:.0}% reserve")
        }
        None => {
            if battery < reserve {
                return Err(fail(format!(
                    "endurance unknown for {} and battery {battery:.0}% is below the {reserve:.0}% reserve",
                    platform.id
                )));
            }
            if transit > ec.unknown_endurance_max_transit_s {
                return Err(fail(format!(
                    "endurance unknown for {}: the intercept takes {transit:.0} s, above {:.0} s allowed without an endurance figure",
                    platform.id, ec.unknown_endurance_max_transit_s
                )));
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
            TerrainInputs::Sampled { .. } => match world.terrain.lookup(point) {
                Some(Surface::Land) if need == "land" => "; effect point on land".into(),
                Some(Surface::Water { depth_m }) if need == "water" && *depth_m > 0.0 => {
                    "; effect point on water".into()
                }
                Some(Surface::OutsideCoverage) | None => {
                    return Err(fail(format!(
                        "cannot evaluate: no terrain answer for the effect point ({:.5}, {:.5})",
                        point.lon, point.lat
                    )));
                }
                Some(_) => {
                    return Err(fail(format!(
                        "the effect point ({:.5}, {:.5}) is not {need}; {} needs {need}",
                        point.lon, point.lat, platform.id
                    )));
                }
            },
            TerrainInputs::Unavailable { reason } => {
                return Err(check(
                    r,
                    uid,
                    Outcome::NotEvaluated,
                    format!(
                        "terrain unavailable ({reason}): {} needs the effect point on {need}",
                        platform.id
                    ),
                ));
            }
            TerrainInputs::NotQueried | TerrainInputs::NotNeeded => {
                return Err(fail(
                    "cannot evaluate: terrain was not sampled for the effect point".into(),
                ));
            }
        },
    };
    Ok(format!("{endurance}{surface}"))
}

// ---- the frozen plan ------------------------------------------------------------------------

/// How late the platform may reach the plan's point and still execute it: `late_s` for a
/// holding track, and for a moving one the time it takes to leave the deviation tolerance
/// (a later effect would be somewhere the plan did not say), seconds.
///
/// The plan gives the signatures `dispatch_delay_s` (the platform was to leave that long
/// after the preview) plus this slack, so the effective signing window is
/// `dispatch_delay_s + plan_slack_s` (never more than `max_signing_s`): with the defaults
/// 80 s for a holding target, 57 s at 2 m/s, 29 s at 8 m/s, 25 s at 15 m/s. A longer
/// `dispatch_delay_s` gives more time to sign at the cost of a later, less certain effect.
pub fn plan_slack_s(p: &EngagePreview, ec: &EngageRules) -> f64 {
    let tol = (ec.deviation_fraction * p.effect_radius_m).max(ec.deviation_min_m);
    if p.track_speed_mps < 0.3 {
        ec.late_s
    } else {
        (tol / p.track_speed_mps).min(ec.late_s)
    }
}

/// While the signatures are collected the platform waits where it is; a fixed wing can't
/// hold still and circles, so its distance to the plan's point swings by up to the circle's
/// diameter. This much more time is allowed for that, seconds (0 for a platform that can
/// hold).
fn waiting_allowance_s(v: &crate::world::VehicleFacts, cfg: &RulesConfig, cruise: f64) -> f64 {
    if cfg.rules_version < 2 {
        return 0.0;
    }
    let Some(p) = &v.platform else { return 0.0 };
    let fixed_wing = v
        .autopilot
        .as_ref()
        .map(|a| a.value)
        .or(p.autopilot)
        .is_some_and(crate::catalog::AutopilotKind::is_fixed_wing);
    if !fixed_wing || cruise <= 0.0 {
        return 0.0;
    }
    let turn = p
        .min_turn_radius_m
        .unwrap_or(cfg.fixed_wing_default_min_turn_radius_m);
    2.0 * turn * cfg.orbit_radius_margin / cruise
}

/// The checks on a frozen plan (after the target checks, which read the track now): the
/// plan's circle in a zone and clear of no-strike, the keep-outs at the plan's point and
/// time, the platform armed and still able to be at the point by the plan's time, the track
/// still identified and declared as when the plan was frozen and predicted within tolerance
/// of the plan's point at its time, and the signatures within `max_signing_s`.
fn plan_checks(
    spec: &IntentSpec,
    plan: &FrozenPlan,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Vec<CheckResult> {
    let ec = cfg.engage_rules();
    let p = &plan.preview;
    let uid = platform_of(spec);
    let track_id = plan.track.id.as_str();
    let mut out = Vec::new();
    if p.effect_radius_m > ec.max_effect_radius_m {
        out.push(check(
            RuleId::EngagementZone,
            None,
            Outcome::Fail,
            format!(
                "effect radius {:.0} m is above the policy's maximum {:.0} m",
                p.effect_radius_m, ec.max_effect_radius_m
            ),
        ));
    } else {
        out.push(zone_check(p, world));
    }
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
    out.push(armed_check(uid, world));
    if ec.v2() {
        out.push(free_check(uid, world));
    }
    out.push(plan_reach_check(uid, p, world, cfg, &ec));
    out.push(plan_track_check(plan, world.tracks.get(track_id), world, &ec));
    let took = secs(world.t.ms_since(plan.t)).max(0.0);
    out.push(check(
        RuleId::SigningTime,
        None,
        if took <= ec.max_signing_s {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        format!(
            "plan frozen by {} {took:.0} s ago (every signature within {:.0} s)",
            plan.by, ec.max_signing_s
        ),
    ));
    out
}

/// The track still identified and declared as when the plan was frozen, and predicted within
/// the deviation tolerance of the plan's point at the plan's time.
fn plan_track_check(
    plan: &FrozenPlan,
    track: Option<&TrackFacts>,
    world: &WorldSnapshot,
    ec: &EngageRules,
) -> CheckResult {
    let r = RuleId::Plan;
    let p = &plan.preview;
    let id = plan.track.id.as_str();
    let Some(t) = track else {
        return check(r, None, Outcome::Fail, format!("{id} is no longer in the picture"));
    };
    let now = TrackIdentity::of(id, t);
    if now != plan.track {
        return check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} changed since the plan was frozen: {} then, {} now",
                plan.track.describe(),
                now.describe()
            ),
        );
    }
    let tol = (ec.deviation_fraction * p.effect_radius_m).max(ec.deviation_min_m);
    if world.t >= p.effect_t {
        return check(
            r,
            None,
            Outcome::Pass,
            format!("the plan's effect time has come ({id} as frozen)"),
        );
    }
    if !ec.v2() {
        // rules 1: carried to the plan's time on today's estimate, the plain tolerance
        let d = haversine_m(track_at(t, p.effect_t), p.effect_point);
        return check(
            r,
            None,
            if d <= tol { Outcome::Pass } else { Outcome::Fail },
            format!(
                "{id} predicted {d:.0} m from the plan's effect point at its time {} (tolerance {tol:.0} m)",
                rel(p.effect_t, world.t)
            ),
        );
    }
    // where the track is now against where the plan's track path puts it now (as the hold
    // after approval reads it): a real deviation shows here
    // where the track is now against where the plan's track path puts it now (as the hold
    // after approval reads it), allowing for the velocity estimate the path rests on
    let estimate = target_at(t, world.t, ec);
    let on_path = plan_path_at(p, world.t);
    let off_now = haversine_m(estimate, on_path);
    let since = secs(world.t.ms_since(p.t)).max(0.0);
    let noise = noise_allowance(p.track_sigma_m, t.pos_sigma_m);
    let tol_now = tol + noise + drift_allowance(t, since, ec);
    if off_now > tol_now {
        return check(
            r,
            None,
            Outcome::Fail,
            format!(
                "{id} is {off_now:.0} m from where the plan's track path puts it now (tolerance {tol:.0} m + {noise:.0} m of position noise + {:.0} m for {since:.0} s of velocity uncertainty)",
                tol_now - tol - noise
            ),
        );
    }
    // and carried to the plan's time on today's velocity estimate: a turn shows at once; a
    // velocity estimate that wobbles does not (both the plan's prediction and today's carry
    // its uncertainty: the tolerance grows with it over both leads, bounded)
    let lead = secs(p.effect_t.ms_since(world.t)).max(0.0);
    let plan_lead = secs(p.effect_t.ms_since(p.t)).max(0.0);
    let drift = drift_allowance(t, lead.hypot(plan_lead), ec);
    let allowed = tol + noise + drift;
    let d = haversine_m(target_at(t, p.effect_t, ec), p.effect_point);
    check(
        r,
        None,
        if d <= allowed { Outcome::Pass } else { Outcome::Fail },
        format!(
            "{id} {off_now:.0} m from the plan's track path now (tolerance {tol_now:.0} m); predicted {d:.0} m from the plan's effect point at its time {} (tolerance {tol:.0} m + {noise:.0} m of position noise + {drift:.0} m of velocity uncertainty)",
            rel(p.effect_t, world.t)
        ),
    )
}

/// Where the plan's track path (the track's position when the plan was previewed, to the
/// effect point at the effect time, a straight line at constant speed) puts the track at
/// `t`.
fn plan_path_at(p: &EngagePreview, t: Time) -> LonLat {
    let a = p.track_path.first().copied().unwrap_or(p.effect_point);
    let span = secs(p.effect_t.ms_since(p.t));
    if span <= 0.0 {
        return p.effect_point;
    }
    let k = secs(t.ms_since(p.t)) / span;
    let (e, n) = enu(a, p.effect_point);
    offset(a, e * k, n * k)
}

/// The platform can still be at the plan's point by the plan's time (plus
/// [`plan_slack_s`]), inside the window and the transit limit, with the endurance, and the
/// point on the surface it needs.
fn plan_reach_check(
    uid: Option<&Uid>,
    p: &EngagePreview,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    ec: &EngageRules,
) -> CheckResult {
    let r = RuleId::Reach;
    let fail = |d: String| check(r, uid, Outcome::Fail, d);
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return fail("cannot evaluate: platform not in the world model".into());
    };
    let Some(platform) = &v.platform else {
        return fail("cannot evaluate: platform not in the catalog".into());
    };
    let pos = match fresh(&v.position, v, "platform position", world, cfg) {
        Ok(p) => *p,
        Err(e) => return fail(e),
    };
    let cruise = p.cruise_mps;
    let distance = haversine_m(pos, p.effect_point);
    let arrive = world.t.plus_ms(ms(distance / cruise));
    let slack = plan_slack_s(p, ec) + waiting_allowance_s(v, cfg, cruise);
    if p.effect_t > p.window.until {
        return fail(format!(
            "the plan's effect {} is after the window closes ({})",
            rel(p.effect_t, world.t),
            rel(p.window.until, world.t)
        ));
    }
    if arrive > p.effect_t.plus_ms(ms(slack)) {
        return fail(format!(
            "the platform, {distance:.0} m away at {cruise:.1} m/s, reaches the plan's point {}, after its time {} (slack {slack:.0} s): the plan no longer holds",
            rel(arrive, world.t),
            rel(p.effect_t, world.t)
        ));
    }
    let transit = secs(arrive.max(p.effect_t).ms_since(world.t)).max(0.0);
    if transit > ec.max_transit_s {
        return fail(format!(
            "the plan's effect is {transit:.0} s away, above the limit {:.0} s",
            ec.max_transit_s
        ));
    }
    let tail = match endurance_and_surface(uid, v, platform, p.effect_point, distance, cruise, transit, world, cfg, ec) {
        Ok(t) => t,
        Err(c) => return c,
    };
    check(
        r,
        uid,
        Outcome::Pass,
        format!(
            "{distance:.0} m at {cruise:.1} m/s: at the plan's point {}, its effect {} (slack {slack:.0} s, window closes {}); {tail}",
            rel(arrive, world.t),
            rel(p.effect_t, world.t),
            rel(p.window.until, world.t)
        ),
    )
}

/// Whether a picture token may be quoted for a signature now: issued for this intent to
/// this operator's session, not expired, showing the plan in force (the frozen one, or none
/// frozen since it was issued) and the track as it is now (identity, affiliation, who
/// declared it). `Err` says why the signer must look again.
#[allow(clippy::too_many_arguments)]
pub fn check_token(
    token: &PictureToken,
    intent: IntentId,
    by: &OperatorId,
    session: SessionId,
    now: Time,
    track_now: Option<&TrackFacts>,
    plan: Option<&FrozenPlan>,
) -> Result<(), String> {
    if token.intent != intent || token.by != *by || token.session != session {
        return Err(format!(
            "picture token {} was issued to {} (session {}) for {}: look again",
            token.id, token.by, token.session.0, token.intent
        ));
    }
    if now > token.expires_t {
        return Err(format!(
            "the picture you were shown expired {:.0} s ago: look again",
            secs(now.ms_since(token.expires_t))
        ));
    }
    match (plan, token.frozen) {
        (Some(p), true) if p.preview == token.plan => {}
        (Some(p), _) => {
            return Err(format!(
                "the plan was frozen by {}'s signature after you looked: look again",
                p.by
            ));
        }
        (None, true) => {
            return Err("the plan you were shown was dropped (the picture changed): look again".into());
        }
        (None, false) => {}
    }
    let Some(t) = track_now else {
        return Err(format!("{} is no longer in the picture", token.track.id));
    };
    let now_id = TrackIdentity::of(&token.track.id, t);
    if now_id != token.track {
        return Err(format!(
            "{} changed since you looked: {} then, {} now: look again",
            token.track.id,
            token.track.describe(),
            now_id.describe()
        ));
    }
    Ok(())
}

/// Whether the nomination's platform needs a land or water answer for its effect point (a
/// boat, a ground vehicle).
pub fn surface_needed(spec: &IntentSpec, world: &WorldSnapshot) -> bool {
    platform_of(spec)
        .and_then(|u| world.vehicles.get(u))
        .and_then(|v| v.platform.as_ref())
        .is_some_and(|p| required_surface(p.domain, p.autopilot).is_some())
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
    if let Some(plan) = &world.plan {
        return Some(plan.preview.effect_point);
    }
    preview(spec, world, cfg).ok().map(|p| p.effect_point)
}

// ---- the snapshot and the picture -----------------------------------------------------------

/// The picture a signature holds: the track's facts at its re-check, the plan it signs (the
/// world's frozen plan; without one, the preview on that world), and the platform's link
/// then.
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
    let preview = match &world.plan {
        Some(plan) => plan.preview.clone(),
        None => preview(spec, world, cfg)?,
    };
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
///
/// `prev_t` is when the picture was last checked (the last `engage.picture`, or the final
/// approval): a gap longer than `max_picture_gap_s` fails it, since nothing held it then.
pub fn picture(
    spec: &IntentSpec,
    snap: &EngageSnapshot,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    dispatched: bool,
    prev_t: Time,
) -> (Vec<CheckResult>, bool, bool) {
    let ec = cfg.engage_rules();
    let p = &snap.preview;
    let uid = platform_of(spec);
    let mut picture_checks = Vec::new();
    let gap = secs(world.t.ms_since(prev_t));
    picture_checks.push(check(
        RuleId::PictureGap,
        None,
        if gap <= ec.max_picture_gap_s {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        format!(
            "picture last checked {gap:.1} s ago (limit {:.1} s)",
            ec.max_picture_gap_s
        ),
    ));
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
    // the keep-outs at the effect: the plan's time, or now once it has come (a release
    // waiting for the platform or the track happens now, and everyone kept moving)
    let effect_at = if ec.v2() {
        p.effect_t.max(world.t)
    } else {
        p.effect_t
    };
    picture_checks.push(keep_out_friendly(
        p.effect_point,
        effect_at,
        p.keep_out_m,
        uid,
        world,
        cfg,
    ));
    picture_checks.push(keep_out_tracks(
        p.effect_point,
        effect_at,
        p.keep_out_m,
        id,
        world,
        &ec,
    ));
    picture_checks.push(link_check(uid, snap, world));
    picture_checks.push(nav_check(uid, world, cfg));
    picture_checks.push(window_check(snap, world, &ec, dispatched));
    let holds = picture_checks.iter().all(|c| c.outcome == Outcome::Pass);
    let mut out = picture_checks;
    let mut ready = false;
    if dispatched && holds {
        let release = release_checks(uid, snap, track, world, cfg);
        ready = release.iter().all(|c| c.outcome == Outcome::Pass);
        out.extend(release);
    }
    (out, holds, ready)
}

fn reclass_check(id: &str, then: &TrackFacts, now: &TrackFacts, ec: &EngageRules) -> CheckResult {
    let r = RuleId::TargetIdentity;
    // any change counts, from unclassified to a kind too
    if then.identity != now.identity {
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
    let expected = target_at(&snap.track, world.t, ec);
    let estimate = target_at(now, world.t, ec);
    let d = haversine_m(expected, estimate);
    let base = (ec.deviation_fraction * snap.preview.effect_radius_m).max(ec.deviation_min_m);
    if !ec.v2() {
        // rules 1: the plain tolerance
        return check(
            RuleId::TrackDeviation,
            None,
            if d <= base { Outcome::Pass } else { Outcome::Fail },
            format!(
                "{id} is {d:.0} m from where the approval's snapshot predicts it now (tolerance {base:.0} m)"
            ),
        );
    }
    // the snapshot's velocity estimate wobbles too: allow for it over the time since
    let since = secs(world.t.ms_since(snap.t)).max(0.0);
    let drift = drift_allowance(&snap.track, since, ec);
    let noise = noise_allowance(snap.track.pos_sigma_m, now.pos_sigma_m);
    let tol = base + noise + drift;
    let o = if d <= tol { Outcome::Pass } else { Outcome::Fail };
    check(
        RuleId::TrackDeviation,
        None,
        o,
        format!(
            "{id} is {d:.0} m from where the approval's snapshot predicts it now (tolerance {base:.0} m + {noise:.0} m of position noise + {drift:.0} m for {since:.0} s of velocity uncertainty)"
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

/// The platform navigates on GNSS: its filter on an absolute position, its receiver with a 3D
/// fix, and no GPS glitch flagged. A fix lost after approval (dead reckoning, no solution,
/// no 3D fix, a glitch) stops the engagement, since the arrival and the release would rest
/// on a position that drifts or lies. Unknown counts as lost.
fn nav_check(uid: Option<&Uid>, world: &WorldSnapshot, cfg: &RulesConfig) -> CheckResult {
    use crate::world::NavSource;
    let r = RuleId::PlatformNav;
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return check(r, uid, Outcome::Fail, "platform is no longer in the world model");
    };
    if cfg.rules_version < 2 {
        // rules 1: the filter's navigation source alone
        return match fresh(&v.nav_source, v, "navigation source", world, cfg) {
            Ok(NavSource::Gnss) => check(r, uid, Outcome::Pass, "platform navigates on GNSS"),
            Ok(NavSource::DeadReckoning) => check(
                r,
                uid,
                Outcome::Fail,
                "platform lost GNSS (dead reckoning): its position, and a release on it, can't be trusted",
            ),
            Ok(NavSource::None) => check(r, uid, Outcome::Fail, "platform has no navigation solution"),
            Err(e) => check(r, uid, Outcome::Fail, format!("cannot evaluate: {e}")),
        };
    }
    match fresh(&v.nav_source, v, "navigation source", world, cfg) {
        Ok(NavSource::Gnss) => {}
        Ok(NavSource::DeadReckoning) => {
            return check(
                r,
                uid,
                Outcome::Fail,
                "platform lost GNSS (dead reckoning): its position, and a release on it, can't be trusted",
            );
        }
        Ok(NavSource::None) => {
            return check(r, uid, Outcome::Fail, "platform has no navigation solution");
        }
        Err(e) => return check(r, uid, Outcome::Fail, format!("cannot evaluate: {e}")),
    }
    // the filter keeps its absolute-position flag for seconds after the fix is gone and
    // while it rejects a glitching (or spoofed) fix: the receiver's own fix and the glitch
    // flag must agree
    match fresh(&v.gps_fix, v, "GPS fix", world, cfg) {
        Ok(f) if *f >= 3 => {}
        Ok(f) => {
            return check(
                r,
                uid,
                Outcome::Fail,
                format!("platform's GPS has no 3D fix (fix type {f})"),
            );
        }
        Err(e) => return check(r, uid, Outcome::Fail, format!("cannot evaluate: {e}")),
    }
    let inferred = v.gps_fix_from == Some(crate::world::GpsFixFrom::HighLatency2);
    match fresh(&v.gps_glitch, v, "GPS glitch flag", world, cfg) {
        // rules 3: a fix inferred from a HIGH_LATENCY2 report says so
        Ok(false) if cfg.rules_version >= 3 && inferred => check(
            r,
            uid,
            Outcome::Pass,
            "platform navigates on GNSS (3D fix: a HIGH_LATENCY2 report without its GPS failure flag; no glitch)",
        ),
        Ok(false) => check(r, uid, Outcome::Pass, "platform navigates on GNSS (3D fix, no glitch)"),
        Ok(true) => check(
            r,
            uid,
            Outcome::Fail,
            "platform's navigation filter flags its GPS as glitching: a release on that position can't be trusted",
        ),
        Err(e) => check(r, uid, Outcome::Fail, format!("cannot evaluate: {e}")),
    }
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

/// How early a plan's effect may be released (seconds before its time: `plan_slack_s` for a
/// moving target, 0 for a holding one) and the earliest instant that allows (never before
/// the window opens). Rules 2 on: rules 1 released from the plan's time, never earlier.
fn release_from(p: &EngagePreview, ec: &EngageRules) -> (f64, Time) {
    let early = if p.track_speed_mps >= 0.3 {
        plan_slack_s(p, ec)
    } else {
        0.0
    };
    let early = if ec.v2() { early } else { 0.0 };
    let earliest = p.effect_t.plus_ms(-ms(early));
    (early, p.window.from.map_or(earliest, |f| f.max(earliest)))
}

/// The plan's time tolerance: the first and last instant its effect may be released at
/// (from `release_from`; until `late_s` after the plan's time, never after the window
/// closes). A picture is ready only inside it ([`picture`]: the effect time and window
/// checks); replay bounds every `engage.effect` by it.
pub fn release_window(p: &EngagePreview, ec: &EngageRules) -> (Time, Time) {
    let (_, from) = release_from(p, ec);
    let until = p.effect_t.plus_ms(ms(ec.late_s)).min(p.window.until);
    (from, until)
}

/// arrival, effect time, alignment, uncertainty: whether the effect may be released now.
fn release_checks(
    uid: Option<&Uid>,
    snap: &EngageSnapshot,
    track: Option<&TrackFacts>,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Vec<CheckResult> {
    let ec = &cfg.engage_rules();
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
    // the plan's time, within the same tolerance as its point: a moving target crosses the
    // deviation tolerance in plan_slack_s, so it may be released that much early (or late)
    // when it is at the point; a holding one waits for the time
    let (early, from) = release_from(p, ec);
    let o = if world.t >= from { Outcome::Pass } else { Outcome::Fail };
    out.push(check(
        RuleId::EffectTime,
        None,
        o,
        if ec.v2() {
            format!(
                "predicted effect time {} (released from {early:.0} s before it)",
                rel(p.effect_t, world.t)
            )
        } else {
            // rules 1: from the plan's time, no earlier
            format!("predicted effect time {}", rel(from, world.t))
        },
    ));
    // released before the plan's time: the keep-outs as they are now (the picture read them at
    // the plan's time)
    if ec.v2() && world.t < p.effect_t {
        out.push(keep_out_friendly(p.effect_point, world.t, p.keep_out_m, uid, world, cfg));
        out.push(keep_out_tracks(
            p.effect_point,
            world.t,
            p.keep_out_m,
            &snap.track_id,
            world,
            ec,
        ));
    }
    match track {
        Some(t) => {
            let d = haversine_m(target_at(t, world.t, ec), p.effect_point);
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
