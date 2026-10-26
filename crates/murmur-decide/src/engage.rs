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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::areas::{Area, AreaKind};
use crate::catalog::{AutopilotKind, Domain, SensorFact};
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
    /// Rules 9: `[identification] nominate_unconfirmed` (copied by
    /// [`RulesConfig::engage_rules`]; not a key of this section): a target declared by rule may
    /// pass `target_declared` before an operator confirms it.
    #[serde(skip)]
    pub nominate_unconfirmed: bool,
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
    /// Rules 6: the 1-sigma error of a protected track's velocity estimate, m/s. A track is
    /// predicted along its velocity with this error times the span on top of its own
    /// position uncertainty and its manoeuvre noise ([`track_prediction_sigma`]). Default 1
    /// (schema 4; basis: the Phase G velocity-noise sweep, docs in the README). From rules 18 a
    /// floor: the tracker's own error counts when larger ([`track_velocity_error`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_velocity_sigma_mps: Option<f64>,
    /// Rules 6: a protected track's manoeuvre as acceleration noise, 1-sigma per domain,
    /// m/s^2 (a constant-velocity prediction with white acceleration noise: the position
    /// error grows as this squared times span^3 / 3 at the 1 s update). Missing domains take
    /// their default: surface and subsurface 0.2, air and ground 2.0 (schema 4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_accel_mps2: Option<BTreeMap<Domain, f64>>,
    /// Rules 6: how fast an aircraft track, or one of our vehicles (at least its catalog
    /// climb rate), may change altitude over a prediction, m/s. Default 10 (schema 4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_rate_mps: Option<f64>,
    /// Rules 6: the keep-out above and below the effect's altitude beyond the effect radius,
    /// metres, where both altitudes are known (else the keep-out is a cylinder of unlimited
    /// height, as before). Default `keep_out_m` (schema 4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_out_vertical_m: Option<f64>,
    /// Rules 6: how old a track's altitude, and the ranging sensor's last hit behind it, may
    /// be for a 3D keep-out to count it, s (older: 2D). Default 10 (schema 4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_alt_max_age_s: Option<f64>,
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

    /// [`EngageRules::track_velocity_sigma_mps`] or its default.
    pub fn track_velocity_sigma(&self) -> f64 {
        self.track_velocity_sigma_mps.unwrap_or(1.0)
    }

    /// [`EngageRules::track_accel_mps2`] for `domain`, or its default.
    pub fn track_accel(&self, domain: Domain) -> f64 {
        self.track_accel_mps2
            .as_ref()
            .and_then(|m| m.get(&domain).copied())
            .unwrap_or(match domain {
                Domain::Surface | Domain::Subsurface => 0.2,
                Domain::Air | Domain::Ground => 2.0,
            })
    }

    /// [`EngageRules::vertical_rate_mps`] or its default.
    pub fn vertical_rate(&self) -> f64 {
        self.vertical_rate_mps.unwrap_or(10.0)
    }

    /// [`EngageRules::keep_out_vertical_m`] or its default (`keep_out_m`).
    pub fn keep_out_vertical(&self) -> f64 {
        self.keep_out_vertical_m.unwrap_or(self.keep_out_m)
    }

    /// [`EngageRules::track_alt_max_age_s`] or its default.
    pub fn track_alt_max_age(&self) -> f64 {
        self.track_alt_max_age_s.unwrap_or(10.0)
    }

    /// The decisions of rules version 2 on (see the changelog).
    fn v2(&self) -> bool {
        self.rules_version >= 2
    }

    /// The decisions of rules version 3 on (see the changelog).
    fn v3(&self) -> bool {
        self.rules_version >= 3
    }

    /// The decisions of rules version 6 on (see the changelog): the lost-link preload, the
    /// keep-outs' prediction model and altitude, accepted unknown friendlies.
    pub fn v6(&self) -> bool {
        self.rules_version >= 6
    }

    /// The decisions of rules 9 on (rule-based identification, [`crate::ident`]): the
    /// declaration's basis in `target_declared`, a rule-declared and unconfirmed track
    /// protected in another engagement's keep-out.
    pub fn v9(&self) -> bool {
        self.rules_version >= crate::rules::RULE_ID_SINCE
    }

    /// The decisions of rules 12 on ([`crate::rules::REVIEW_FOLLOWUPS_SINCE`]): a young track
    /// counts as possibly manoeuvring in a keep-out ([`track_reach`]).
    pub fn followups(&self) -> bool {
        self.rules_version >= crate::rules::REVIEW_FOLLOWUPS_SINCE
    }

    /// The decisions of rules 14 on ([`crate::rules::AIRCRAFT_REACH_SINCE`]): an aircraft with a
    /// velocity also counts anywhere it reaches at its own speed ([`track_reach`]).
    pub fn aircraft_reach(&self) -> bool {
        self.rules_version >= crate::rules::AIRCRAFT_REACH_SINCE
    }

    /// The decisions of rules 16 on ([`crate::rules::LOST_CONTACT_SINCE`]): a contact the
    /// tracker dropped counts in the keep-out as a lost contact ([`crate::lost`]), and a track
    /// out of view, lost or live, counts anywhere at its domain's top speed ([`out_of_view`]).
    pub fn lost(&self) -> bool {
        self.rules_version >= crate::rules::LOST_CONTACT_SINCE
    }

    /// The decisions of rules 18 on ([`crate::rules::VELOCITY_SIGMA_SINCE`]): a protected track
    /// is predicted with its own velocity error ([`track_velocity_error`]).
    pub fn own_velocity_error(&self) -> bool {
        self.rules_version >= crate::rules::VELOCITY_SIGMA_SINCE
    }

    /// The decisions of rules 19 on ([`crate::rules::ACCEPT_BEHAVING_SINCE`]): a live track an
    /// operator accepted as behaving now counts along its accepted path and its course now, until
    /// the acceptance lapses.
    pub fn accept_behaving(&self) -> bool {
        self.rules_version >= crate::rules::ACCEPT_BEHAVING_SINCE
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
        if self.track_velocity_sigma_mps.is_some() {
            keys.push((4, "track_velocity_sigma_mps"));
        }
        if self.track_accel_mps2.is_some() {
            keys.push((4, "track_accel_mps2"));
        }
        if self.vertical_rate_mps.is_some() {
            keys.push((4, "vertical_rate_mps"));
        }
        if self.keep_out_vertical_m.is_some() {
            keys.push((4, "keep_out_vertical_m"));
        }
        if self.track_alt_max_age_s.is_some() {
            keys.push((4, "track_alt_max_age_s"));
        }
        keys
    }
}

impl Default for EngageRules {
    fn default() -> Self {
        EngageRules {
            rules_version: crate::rules::RULES_VERSION,
            nominate_unconfirmed: false,
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
            track_velocity_sigma_mps: None,
            track_accel_mps2: None,
            vertical_rate_mps: None,
            keep_out_vertical_m: None,
            track_alt_max_age_s: None,
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
        within(self.track_velocity_sigma(), 0.1, 10.0, "track_velocity_sigma_mps")?;
        for d in [Domain::Air, Domain::Ground, Domain::Surface, Domain::Subsurface] {
            within(self.track_accel(d), 0.01, 20.0, "track_accel_mps2")?;
        }
        within(self.vertical_rate(), 0.5, 100.0, "vertical_rate_mps")?;
        within(self.keep_out_vertical(), 50.0, 20_000.0, "keep_out_vertical_m")?;
        within(self.track_alt_max_age(), 1.0, 300.0, "track_alt_max_age_s")?;
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
    /// Rules 9: an operator revoked the target's declaration (`declaration`, on `track`): the
    /// engagement is stopped in the revocation's step, before anything else, with the fallback
    /// the stop rules give (none before dispatch).
    Revoked {
        by: OperatorId,
        session: SessionId,
        track: String,
        declaration: u64,
    },
}

impl StopCause {
    /// Its kind: `operator`, `picture`, `system`, `superseded`, `revoked` (what the
    /// evaluation counts).
    pub fn kind(&self) -> &'static str {
        match self {
            StopCause::Operator { .. } => "operator",
            StopCause::Picture => "picture",
            StopCause::System => "system",
            StopCause::Superseded { .. } => "superseded",
            StopCause::Revoked { .. } => "revoked",
        }
    }
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
    /// Rules 9: how it was declared (operator or rule, the rule's evidence, confirmed by
    /// whom): the basis travels with the picture token and the frozen plan, so a signer sees
    /// "declared by rule hostile_act on act A-1" or "declared by op-1", and a change of it
    /// (a confirmation, a revocation) is a change of what was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<Box<crate::ident::Declaration>>,
}

impl TrackIdentity {
    pub fn of(id: &str, t: &TrackFacts) -> Self {
        TrackIdentity {
            id: id.to_string(),
            identity: t.identity.clone(),
            affiliation: t.affiliation.value,
            declared_by: t.declared_by.clone(),
            declaration: t.declaration.clone(),
        }
    }

    fn describe(&self) -> String {
        let basis = match &self.declaration {
            Some(d) if d.is_rule() => format!(
                " (D-{}, {})",
                d.id,
                if d.confirmed.is_some() { "confirmed" } else { "not confirmed" }
            ),
            Some(d) => format!(" (D-{})", d.id),
            None => String::new(),
        };
        format!(
            "{} {} by {}{basis}",
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
    /// Rules 6: the lost vehicles accepted for the engagement when it was shown (every signer
    /// sees them); a signature quoting a token whose set differs from the engagement's now is
    /// refused.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_accepted: BTreeMap<Uid, AcceptedUnknown>,
    /// Rules 16: the lost contacts accepted for it when it was shown, by track id (every signer
    /// sees them; a signature quoting a token whose set differs from the engagement's now is
    /// refused).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lost_accepted: BTreeMap<String, AcceptedUnknown>,
    /// Rules 19: the live tracks accepted for it as behaving now when it was shown, by track id,
    /// each with its facts then (every signer sees them; a signature quoting a token whose set
    /// differs from the engagement's now, an acceptance given, revoked or lapsed since, is
    /// refused).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub behaving_accepted: BTreeMap<String, AcceptedBehaving>,
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
    /// its reach of the effect point ([`covering_sensor`]: `range_m`, or on the physical
    /// sensing model the catalog's reach for the domain), as `"<uid>:<sensor> <range> m"`.
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
    /// Rules 6: the lost-link action the platform carries while the engagement is under way
    /// ([`preload_for`] on the final approval's world); the engagement order carries it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preload: Option<LostLinkPreload>,
    /// Rules 6: the platform's report of setting it (and reading it back), and of restoring
    /// its own values afterwards.
    /// Rules 6: the write-ahead of the preload: the platform's own values, logged before
    /// anything was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intended: Option<PreloadRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preloaded: Option<PreloadRecord>,
    /// Rules 6: the last re-read of it near the release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<PreloadRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored: Option<PreloadRecord>,
    /// Rules 6: our vehicles an operator accepted as lost for this engagement.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub accepted: BTreeMap<Uid, AcceptedUnknown>,
    /// Rules 16: the lost contacts an operator accepted for this engagement, by the track id
    /// each was dropped under (`since`: its last fix).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub accepted_lost: BTreeMap<String, AcceptedUnknown>,
    /// Rules 19: the live tracks an operator accepted for this engagement as behaving now, by
    /// track id, each with its facts then; dropped when revoked, lapsed or reset with the plan.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub accepted_behaving: BTreeMap<String, AcceptedBehaving>,
    /// Rules 19: the acceptances as behaving now that lapsed on their own, in order (every signer
    /// and the card are shown them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lapsed: Vec<LapseRecord>,
}

/// Rules 6: an operator's explicit acceptance that one of our vehicles is lost (its
/// position unknown since its last report, `since`; none: it never reported one), for one
/// engagement (`engage.accepted`). The friendly keep-out then leaves that vehicle out, but
/// only while it is lost since exactly that report: one that reports again counts as any
/// other, and a newer loss needs a new acceptance. Never automatic, never an agent's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedUnknown {
    pub since: Option<Time>,
    pub by: OperatorId,
}

/// Rules 6: how far apart an acceptance's logged world and the event may be, ms (both are the
/// engine's clock at the call; replay binds them).
pub const ACCEPT_WORLD_SKEW_MS: i64 = 5_000;

/// Rules 6: whether vehicle `v` is lost at `t` since exactly its report at `since`: its
/// position is not fresh by the link-aware age limit the checks use, and the last one it
/// reported is at `since` (none: it never reported one). The engine refuses an acceptance
/// otherwise; replay recomputes it from the vehicle's facts logged with `engage.accepted`.
pub fn lost_since_report(
    v: Option<&crate::world::VehicleFacts>,
    since: Option<Time>,
    t: Time,
    cfg: &RulesConfig,
) -> Result<(), String> {
    let Some(v) = v else {
        return Err("not one of ours in the world model".into());
    };
    let last = v.position.as_ref().map(|p| p.t);
    if let Some(at) = last {
        let (max_ms, _) = cfg.fact_age_limit_ms(v.report_interval_ms);
        if t.ms_since(at) <= max_ms {
            return Err("its position is fresh: it is not lost".into());
        }
    }
    if last != since {
        return Err(format!(
            "lost since {}, not {}: accept what the check shows",
            last.map_or("none".to_string(), |t| t.0.to_string()),
            since.map_or("none".to_string(), |t| t.0.to_string())
        ));
    }
    Ok(())
}

/// Rules 16: whether lost contact `id` is in force (`lost`, the engine's ledger's) since
/// exactly its last fix at `since`. The engine refuses an acceptance otherwise; replay checks it
/// against its own ledger and the contact's facts logged with `engage.accepted`.
pub fn contact_lost_since(id: &str, lost: Option<&crate::lost::LostContact>, since: Option<Time>) -> Result<(), String> {
    let Some(l) = lost else {
        return Err(format!("{id} is not a lost contact (the tracker has not dropped it, or it was seen again)"));
    };
    if since != Some(l.since()) {
        return Err(format!(
            "{id} is out of view since {}, not {}: accept what the check shows",
            l.since().0,
            since.map_or("none".to_string(), |t| t.0.to_string())
        ));
    }
    Ok(())
}

/// Rules 19 ([`crate::rules::ACCEPT_BEHAVING_SINCE`], bd murmur-j0x): an operator's explicit
/// acceptance of one live track as it is behaving now, for one engagement (`engage.accepted`
/// naming the `track`, with its facts then). The keep-out on tracks then counts it along the path
/// accepted for it and along its course now ([`behaving_regions`]), not anywhere it could turn or
/// speed up to, while the acceptance holds: it lapses on its own ([`behaving_lapse`]) when the
/// track's estimate departs from the accepted prediction beyond its uncertainty or the track goes
/// out of view, and a new one needs a person again. Never automatic, never an agent's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptedBehaving {
    pub by: OperatorId,
    /// The world's time when it was accepted.
    pub t: Time,
    /// The track's facts then (as decisions read them, its identification filled): the
    /// estimate the prediction starts from, its position with its time, its velocity and the
    /// velocity's error.
    pub facts: TrackFacts,
}

/// Rules 19: why an acceptance as behaving now lapsed ([`behaving_lapse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LapseKind {
    /// The track is no longer in the picture (dropped: a lost contact from then on).
    Gone,
    /// No detection for [`OUT_OF_VIEW_S`]: nothing checks its course.
    OutOfView,
    /// Its estimated position left the accepted prediction's region.
    Position,
    /// Its estimated velocity departed from the accepted one beyond their uncertainty.
    Velocity,
}

impl LapseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LapseKind::Gone => "gone",
            LapseKind::OutOfView => "out_of_view",
            LapseKind::Position => "position",
            LapseKind::Velocity => "velocity",
        }
    }
}

/// Rules 19: an acceptance's lapse, with the numbers in words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lapse {
    pub kind: LapseKind,
    pub detail: String,
}

/// Rules 19: a lapse as the engagement records it (for the card and every signer): which track,
/// when, why, and who had accepted it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LapseRecord {
    pub track: String,
    pub t: Time,
    pub kind: LapseKind,
    pub detail: String,
    pub accepted_by: OperatorId,
}

/// Rules 19: whether live track `id` (`t`, the picture's; none: not in it) may be accepted as
/// behaving now at `now` for an engagement on `target`: in the picture, protected (not the
/// target, not a declared hostile), in view (seen within [`OUT_OF_VIEW_S`]) and with a velocity
/// (a course to accept). The engine refuses an acceptance otherwise; replay recomputes it from the
/// facts logged with `engage.accepted`.
pub fn behaving_acceptable(id: &str, t: Option<&TrackFacts>, now: Time, target: &str, ec: &EngageRules) -> Result<(), String> {
    let Some(t) = t else {
        return Err(format!("{id} is not in the picture"));
    };
    if id == target {
        return Err(format!("{id} is the target: it is not protected"));
    }
    if track_exempt(id, t, target, ec) {
        return Err(format!("{id} is declared hostile: it is not protected"));
    }
    let unseen = secs(now.ms_since(t.last_seen_t)).max(0.0);
    if unseen >= OUT_OF_VIEW_S {
        return Err(format!(
            "{id} is out of view (no detection for {unseen:.0} s): only a track in view may be accepted as behaving now"
        ));
    }
    if t.velocity_mps.is_none() {
        return Err(format!("{id} has no velocity estimate: there is no course to accept"));
    }
    Ok(())
}

/// Rules 19: where the path accepted for a track puts it at `t` (its accepted velocity carried
/// from its accepted position).
pub fn accepted_at(a: &AcceptedBehaving, t: Time) -> LonLat {
    track_at(&a.facts, t)
}

/// Rules 19: whether the acceptance `a` of track `id` has lapsed on the picture at `now` (`live`,
/// the track's facts in it; none: it is not in the picture). Lapsed when, in this order:
///
/// - the track is no longer in the picture (`gone`);
/// - it is out of view, no detection for [`OUT_OF_VIEW_S`] (`out_of_view`);
/// - its estimated position departs from where the accepted path puts it at the estimate's time
///   by more than `keep_out_sigma` times their combined 1-sigma: the accepted prediction's
///   ([`track_prediction_sigma`] of the accepted facts over the time since, rules 18's velocity
///   error and the domain's white-noise acceleration included) and the estimate's own
///   (`position`): the accepted confidence, the one the keep-out counts it with;
/// - its estimated velocity departs from the accepted one by more than `keep_out_sigma` times
///   their combined 1-sigma: each velocity's error ([`track_velocity_error`]) and the velocity
///   the white-noise acceleration builds over the time since (`q t`, the same model's)
///   (`velocity`); while the tracker gives it no velocity (it withholds one while its models
///   disagree, as in a turn, and for an aircraft on coarse fixes now and then in straight flight),
///   only its position is held to the accepted path, and a turn shows there, later.
///
/// None: it holds. A lapse is for good: the engine logs it and drops the acceptance, and a new
/// one needs a person. The owner's decision names these two tests, a departure beyond the
/// prediction's uncertainty and out of view; a velocity the tracker withholds for a moment is
/// neither (lapsing on it, as first built, ended an acceptance of a steady aircraft every 10 to
/// 20 s in a headless episode on the legacy sensing model).
pub fn behaving_lapse(id: &str, a: &AcceptedBehaving, live: Option<&TrackFacts>, now: Time, ec: &EngageRules) -> Option<Lapse> {
    let lapse = |kind: LapseKind, detail: String| Some(Lapse { kind, detail });
    let Some(t) = live else {
        return lapse(LapseKind::Gone, format!("{id} is no longer in the picture"));
    };
    let unseen = secs(now.ms_since(t.last_seen_t)).max(0.0);
    if unseen >= OUT_OF_VIEW_S {
        return lapse(
            LapseKind::OutOfView,
            format!("{id} out of view (no detection for {unseen:.0} s): its course can no longer be checked"),
        );
    }
    let k = ec.keep_out_sigma;
    let since = secs(t.position.t.ms_since(a.facts.position.t)).max(0.0);
    let predicted = accepted_at(a, t.position.t);
    let d = haversine_m(predicted, t.position.value);
    let sigma_p = track_prediction_sigma(&a.facts, since, ec).hypot(t.pos_sigma_m);
    if d > k * sigma_p {
        return lapse(
            LapseKind::Position,
            format!(
                "{id} is {d:.0} m from where its accepted course puts it {since:.0} s on, beyond the {:.0} m it was accepted with ({k:.0}-sigma)",
                k * sigma_p
            ),
        );
    }
    // its velocity, while the tracker gives it one
    let [ve, vn] = t.velocity_mps?;
    let [ae, an] = a.facts.velocity_mps.unwrap_or([0.0, 0.0]);
    let dv = (ve - ae).hypot(vn - an);
    let accel = ec.track_accel(t.domain);
    let sigma_v = (track_velocity_error(&a.facts, ec).powi(2) + track_velocity_error(t, ec).powi(2) + accel * accel * since).sqrt();
    if dv > k * sigma_v {
        return lapse(
            LapseKind::Velocity,
            format!(
                "{id}'s velocity is {dv:.1} m/s off the accepted {:.1} m/s ({:.1} m/s now), beyond the {:.1} m/s it was accepted with ({k:.0}-sigma, {since:.0} s on)",
                ae.hypot(an),
                ve.hypot(vn),
                k * sigma_v
            ),
        );
    }
    None
}

/// Rules 19: the acceptance of live track `id` (`t`) as behaving now in force for this decision
/// (`world.behaving_accepted`): Ok(Some) while it holds, Ok(None) when there is none (or before
/// rules 19), Err with the lapse when the picture lapses it (the engine logs that at its next
/// step; until then the decision judges the track as if none had been given).
pub fn behaving_in_force<'w>(
    id: &str,
    t: &TrackFacts,
    world: &'w WorldSnapshot,
    ec: &EngageRules,
) -> Result<Option<&'w AcceptedBehaving>, Lapse> {
    if !ec.accept_behaving() {
        return Ok(None);
    }
    let Some(a) = world.behaving_accepted.get(id) else {
        return Ok(None);
    };
    match behaving_lapse(id, a, Some(t), world.t, ec) {
        Some(l) => Err(l),
        None => Ok(Some(a)),
    }
}

/// Rules 6: which vehicle a preload was set on, as the bridge knows it (its manifest and its
/// telemetry), logged with the write-ahead (`engage.preload` intent) and with values carried
/// over to another run (`platform.restore`): those go back only to a vehicle that matches.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sysid: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<LonLat>,
}

impl VehicleIdentity {
    /// How far apart two homes may be and still be the same vehicle's, m.
    pub const HOME_TOLERANCE_M: f64 = 500.0;

    /// Whether `now` (the vehicle that has the uid now) is this one: every field this one
    /// knows must be known there and equal (the home within [`Self::HOME_TOLERANCE_M`]).
    /// `Err` says why not (or what is not known yet).
    pub fn matches(&self, now: &VehicleIdentity) -> Result<(), String> {
        fn same<T: PartialEq + std::fmt::Debug>(what: &str, was: &Option<T>, is: &Option<T>) -> Result<(), String> {
            match (was, is) {
                (None, _) => Ok(()),
                (Some(_), None) => Err(format!("its {what} is not known yet")),
                (Some(a), Some(b)) if a == b => Ok(()),
                (Some(a), Some(b)) => Err(format!("its {what} is {b:?}, not {a:?}")),
            }
        }
        same("platform", &self.platform, &now.platform)?;
        same("callsign", &self.callsign, &now.callsign)?;
        same("system id", &self.sysid, &now.sysid)?;
        match (self.home, now.home) {
            (None, _) => Ok(()),
            (Some(_), None) => Err("its home is not known yet".into()),
            (Some(a), Some(b)) => {
                let d = haversine_m(a, b);
                if d <= Self::HOME_TOLERANCE_M {
                    Ok(())
                } else {
                    Err(format!("its home is {d:.0} m from the one preloaded"))
                }
            }
        }
    }

    /// Rules 12: why `now` is known to be another vehicle than this one, if it is: its
    /// platform, callsign or system id, where both know it, differs. The home is never enough:
    /// ArduPilot (and the stand-ins) set home again at every arming, so the very vehicle a
    /// preload was set on reports another home after a restart once it armed elsewhere (a live
    /// run after a crash: the same USV, 1215 m away). Unlike [`Self::matches`], a field `now`
    /// does not know yet is no difference (fail closed: a carried restore's hold stays, and the
    /// restore is tried, until the vehicle is known to be another).
    pub fn differs(&self, now: &VehicleIdentity) -> Option<String> {
        fn other<T: PartialEq + std::fmt::Debug>(what: &str, was: &Option<T>, is: &Option<T>) -> Option<String> {
            match (was, is) {
                (Some(a), Some(b)) if a != b => Some(format!("its {what} is {b:?}, not {a:?}")),
                _ => None,
            }
        }
        other("platform", &self.platform, &now.platform)
            .or_else(|| other("callsign", &self.callsign, &now.callsign))
            .or_else(|| other("system id", &self.sysid, &now.sysid))
    }

    /// Rules 15 (an un-spend): why `now` (the vehicle under the uid now, if known) is not shown
    /// to be this vehicle, if it is not: every one of the platform, the callsign and the system
    /// id this identity knows must be known and the same now (the home never counts: it is set
    /// again at every arming), and it must know at least one of them. Stricter than
    /// [`Self::differs`], which holds a mission until a vehicle is known to be another: an
    /// un-spend reopens values for a vehicle, so it waits until that vehicle is known to be the
    /// one (fail closed).
    pub fn same_vehicle(&self, now: Option<&VehicleIdentity>) -> Result<(), String> {
        fn same<T: PartialEq + std::fmt::Debug>(what: &str, was: &Option<T>, is: Option<&Option<T>>) -> Result<(), String> {
            match (was, is) {
                (None, _) => Ok(()),
                (Some(a), Some(Some(b))) if a == b => Ok(()),
                (Some(a), Some(Some(b))) => Err(format!("its {what} is {b:?}, not {a:?}")),
                (Some(a), _) => Err(format!("its {what} is not known yet (the spent one's was {a:?})")),
            }
        }
        if self.platform.is_none() && self.callsign.is_none() && self.sysid.is_none() {
            return Err("the vehicle that was spent is not known by platform, callsign or system id".into());
        }
        same("platform", &self.platform, now.map(|n| &n.platform))?;
        same("callsign", &self.callsign, now.map(|n| &n.callsign))?;
        same("system id", &self.sysid, now.map(|n| &n.sysid))
    }
}

/// Rules 6: one parameter a lost-link preload sets, and its value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSetting {
    pub name: String,
    pub value: f64,
}

/// Rules 6: bits a lost-link preload clears in a bitmask parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamBits {
    pub name: String,
    pub clear: u32,
}

/// Rules 6: what the platform's own lost-link (GCS) failsafe must be while an engagement is
/// under way, so that the approval's fallback happens even when the order to fly it can't
/// arrive: the failsafe enabled with the action that is the fallback, anything that would
/// override it cleared, and its timer no longer than the approval's link-gap limit. ArduPilot
/// 4.8 parameter names ([`preload_for`]). The bridge sets these at dispatch, reads them back
/// (`engage.preload`), and restores the platform's own values when the engagement ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LostLinkPreload {
    /// The firmware whose parameters these are (ArduPlane, ArduCopter, ArduRover, ArduSub).
    pub firmware: String,
    pub fallback: Fallback,
    /// What the platform does on its own when it loses the GCS, in its firmware's words.
    pub action: String,
    /// Parameters set to exactly these values.
    pub set: Vec<ParamSetting>,
    /// Bits cleared (ArduCopter's FS_OPTIONS bit 1: continue the mission on GCS loss).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clear_bits: Vec<ParamBits>,
    /// The lost-link timer, and the longest it may be while the engagement is under way, s
    /// (the platform's own value if shorter).
    pub timer: String,
    pub timer_max_s: f64,
}

/// Rules 6: whether an `engage.preload` report is the write-ahead of a preload (`intent`), the
/// preload (`set`), a re-read near the release (`verify`) or the platform's own values put
/// back (`restore`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreloadPhase {
    /// The platform's own values, read and logged before anything is written: from here on
    /// they are owed back (a bridge that stops before the set's report still knows them).
    Intent,
    Set,
    /// Read back again while the engagement is under way, near its release: still in force.
    Verify,
    Restore,
}

/// Rules 6: a preload or restore as the platform answered it (`engage.preload`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PreloadRecord {
    pub t: Time,
    pub phase: PreloadPhase,
    /// The platform's own values before the preload (set), what was read back after.
    pub before: BTreeMap<String, f64>,
    pub readback: BTreeMap<String, f64>,
    pub confirmed: bool,
    pub detail: String,
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
    /// Rules 6: the lost-link action the platform must carry while the engagement is under
    /// way, the fallback as its own failsafe ([`LostLinkPreload`]); nothing is released
    /// until the platform confirms it (`engage.preload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preload: Option<Box<LostLinkPreload>>,
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

// ---- rules 6: the fallback as the platform's own lost-link action -----------------------

/// Rules 6: the shortest lost-link timer a preload sets, s: the low end of ArduPilot 4.8's
/// documented FS_GCS_TIMEOUT range (Copter, Rover, Blimp; Plane's FS_LONG_TIMEOUT goes to
/// 1 s). The GCS heartbeat is 1 s on every link an armed platform uses (`telemetry.rs`).
pub const PRELOAD_TIMER_MIN_S: f64 = 2.0;

/// Which autopilot a vehicle runs: its heartbeat's, else the catalog's.
pub fn platform_autopilot(v: &crate::world::VehicleFacts) -> Option<AutopilotKind> {
    v.autopilot
        .as_ref()
        .map(|a| a.value)
        .or_else(|| v.platform.as_ref().and_then(|p| p.autopilot))
}

/// The longest silence an approval tolerates from its platform, ms: `link_gap_s`, or
/// `link_gap_intervals` of its report interval when longer ([`snapshot`] records it).
pub fn link_gap_limit_ms(v: Option<&crate::world::VehicleFacts>, ec: &EngageRules) -> i64 {
    let interval = v.and_then(|v| v.report_interval_ms);
    let by_link = interval.map_or(0, |i| (ec.link_gap_intervals * f64::from(i)).round() as i64);
    ms(ec.link_gap_s).max(by_link)
}

/// Rules 6: the platform's lost-link action that is `fallback`, in ArduPilot 4.8's terms, or
/// why there is none (fail closed: the nomination can't carry that fallback on that platform):
///
/// - ArduPlane (and QuadPlane in fixed-wing modes): `FS_GCS_ENABL` 1 (heartbeat), and
///   `FS_LONG_ACTN` 1 for return (RTL). It has no action that holds where it is
///   (`FS_LONG_ACTN`: 0 continue the mission, 1 RTL, 2 glide, 3 parachute, 4 AUTO,
///   5 AUTOLAND): abort and loiter can't be preloaded.
/// - ArduCopter: `FS_GCS_ENABLE` 1 (RTL) for return, 7 (brake, else land) for abort and
///   loiter (it has no circling lost-link action: a multirotor's hold is BRAKE, holding its
///   position); `FS_OPTIONS` bit 1 (continue the mission in AUTO on GCS loss) cleared.
/// - ArduRover (boats, UGVs): `FS_GCS_ENABLE` 1 (not 2, which continues in AUTO) and
///   `FS_ACTION` 1 (RTL) for return, 2 (Hold) for abort, 6 (Loiter, else Hold) for loiter.
/// - ArduSub: `FS_GCS_ENABLE` 3 (depth hold) for abort and loiter; it has no return.
/// - Blimp, AntennaTracker and a drifter: none. (Blimp 4.8's GCS failsafe can only disarm
///   it; the parameter's documented Land is not implemented. Rules 6 logged the refusal as
///   "only lost-link action is Land"; a check detail is compared by replay, so that wording
///   stays for rules 6 logs, and rules 7 says it disarms: murmur-a9h.)
///
/// The timer (`FS_LONG_TIMEOUT` on Plane, `FS_GCS_TIMEOUT` elsewhere) is at most the
/// approval's link-gap limit (whole seconds, at least [`PRELOAD_TIMER_MIN_S`]), so the
/// platform takes its fallback about when the engine stops the engagement for silence;
/// ArduRover acts `FS_TIMEOUT` (1.5 s) after its timer.
pub fn preload_for(
    fallback: Fallback,
    autopilot: Option<AutopilotKind>,
    link_gap_limit_ms: i64,
) -> Result<LostLinkPreload, String> {
    let timer_max_s = (link_gap_limit_ms as f64 / 1000.0)
        .ceil()
        .max(PRELOAD_TIMER_MIN_S);
    let set = |v: &[(&str, f64)]| {
        v.iter()
            .map(|(n, x)| ParamSetting {
                name: (*n).to_string(),
                value: *x,
            })
            .collect::<Vec<_>>()
    };
    let Some(ap) = autopilot else {
        return Err("its autopilot is unknown: its lost-link action can't be set".into());
    };
    type Parts = (&'static str, &'static str, Vec<ParamSetting>, Vec<ParamBits>, &'static str);
    let (firmware, action, params, clear_bits, timer): Parts = match (ap, fallback) {
        (AutopilotKind::Plane | AutopilotKind::Vtol, Fallback::Return) => (
            "ArduPlane",
            "RTL",
            set(&[("FS_GCS_ENABL", 1.0), ("FS_LONG_ACTN", 1.0)]),
            Vec::new(),
            "FS_LONG_TIMEOUT",
        ),
        (AutopilotKind::Plane | AutopilotKind::Vtol, f) => {
            return Err(format!(
                "ArduPlane 4.8 has no lost-link action that holds where it is (FS_LONG_ACTN: 0 continue the mission, 1 RTL, 2 glide, 3 parachute, 4 AUTO, 5 AUTOLAND): fallback {} can't be its own failsafe; only return can",
                f.as_str()
            ));
        }
        (AutopilotKind::Copter, f) => (
            "ArduCopter",
            if f == Fallback::Return { "RTL" } else { "Brake or Land" },
            set(&[("FS_GCS_ENABLE", if f == Fallback::Return { 1.0 } else { 7.0 })]),
            vec![ParamBits {
                name: "FS_OPTIONS".into(),
                clear: 2,
            }],
            "FS_GCS_TIMEOUT",
        ),
        (AutopilotKind::Boat | AutopilotKind::Ugv, f) => {
            let (action, value) = match f {
                Fallback::Return => ("RTL", 1.0),
                Fallback::Abort => ("Hold", 2.0),
                Fallback::Loiter => ("Loiter or Hold", 6.0),
            };
            (
                "ArduRover",
                action,
                set(&[("FS_GCS_ENABLE", 1.0), ("FS_ACTION", value)]),
                Vec::new(),
                "FS_GCS_TIMEOUT",
            )
        }
        (AutopilotKind::Sub, Fallback::Abort | Fallback::Loiter) => (
            "ArduSub",
            "Depth hold",
            set(&[("FS_GCS_ENABLE", 3.0)]),
            Vec::new(),
            "FS_GCS_TIMEOUT",
        ),
        (AutopilotKind::Sub, Fallback::Return) => {
            return Err(
                "ArduSub 4.8 has no lost-link return (FS_GCS_ENABLE: warn, disarm, depth hold, surface): fallback return can't be its own failsafe".into(),
            );
        }
        (AutopilotKind::Blimp, f) => {
            // The text is a logged check detail that replay compares byte for byte, so it
            // stays as rules 6 wrote it although it misreads the firmware: Blimp 4.8's GCS
            // failsafe can only disarm (Blimp/events.cpp failsafe_gcs_check), and its
            // documented Land (FS_GCS_ENABLE 5) is not implemented. The verdict is the same
            // either way (no preload); the next rules version corrects the wording.
            return Err(format!(
                "Blimp 4.8's only lost-link action is Land (FS_GCS_ENABLE 5): fallback {} can't be its own failsafe",
                f.as_str()
            ));
        }
        (AutopilotKind::Tracker | AutopilotKind::Drifter, _) => {
            return Err(format!("a {} has no lost-link action", ap.as_str()));
        }
    };
    Ok(LostLinkPreload {
        firmware: firmware.into(),
        fallback,
        action: action.into(),
        set: params,
        clear_bits,
        timer: timer.into(),
        timer_max_s,
    })
}

impl LostLinkPreload {
    /// Every parameter it reads and sets, in order: the set ones, the bitmasks, the timer.
    pub fn names(&self) -> Vec<&str> {
        self.set
            .iter()
            .map(|p| p.name.as_str())
            .chain(self.clear_bits.iter().map(|b| b.name.as_str()))
            .chain(std::iter::once(self.timer.as_str()))
            .collect()
    }

    /// "ArduPlane RTL (FS_GCS_ENABL 1, FS_LONG_ACTN 1; FS_LONG_TIMEOUT at most 5 s)".
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self
            .set
            .iter()
            .map(|p| format!("{} {}", p.name, p.value))
            .collect();
        parts.extend(
            self.clear_bits
                .iter()
                .map(|b| format!("{} bits {} clear", b.name, b.clear)),
        );
        format!(
            "{} {} ({}; {} at most {} s)",
            self.firmware,
            self.action,
            parts.join(", "),
            self.timer,
            self.timer_max_s
        )
    }

    /// Whether `readback` (each parameter as the platform reports it) shows it in force: every
    /// set parameter at its value, every cleared bit clear, the timer above 0 and at most
    /// `timer_max_s`. `Err` says the first thing that differs. A parameter the platform did
    /// not report fails (unknown is not in force); a bitmask it does not have is clear.
    pub fn holds(&self, readback: &BTreeMap<String, f64>) -> Result<(), String> {
        for p in &self.set {
            match readback.get(&p.name) {
                Some(v) if (v - p.value).abs() < 0.01 => {}
                Some(v) => return Err(format!("{} reads {v}, not {}", p.name, p.value)),
                None => return Err(format!("{} was not read back", p.name)),
            }
        }
        for b in &self.clear_bits {
            if let Some(v) = readback.get(&b.name)
                && (v.round() as i64) & i64::from(b.clear) != 0
            {
                return Err(format!("{} reads {v}: bits {} are not clear", b.name, b.clear));
            }
        }
        match readback.get(&self.timer) {
            Some(v) if *v > 0.0 && *v <= self.timer_max_s + 0.01 => Ok(()),
            Some(v) => Err(format!(
                "{} reads {v} s, not within 0..{} s",
                self.timer, self.timer_max_s
            )),
            None => Err(format!("{} was not read back", self.timer)),
        }
    }
}

/// Rules 6: the preload an approved engagement's order carries, from the final signature's
/// world (the platform's autopilot) and its snapshot (the fallback, the link-gap limit).
/// None before rules 6, or when there is none (the `fallback_preload` check has failed the
/// signature then, so it is never approved without one).
pub fn order_preload(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    snap: &EngageSnapshot,
    cfg: &RulesConfig,
) -> Option<LostLinkPreload> {
    if cfg.rules_version < 6 {
        return None;
    }
    let v = platform_of(spec).and_then(|u| world.vehicles.get(u))?;
    preload_for(snap.fallback, platform_autopilot(v), snap.link_gap_limit_ms).ok()
}

/// Rules 6: whether a restore put back the platform's own values (`before`, as the preload
/// read them): every parameter it had read, as it was. `Err` says what differs.
pub fn restored(before: &BTreeMap<String, f64>, readback: &BTreeMap<String, f64>) -> Result<(), String> {
    if before.is_empty() {
        return Err("no values of its own were read before the preload".into());
    }
    for (name, want) in before {
        match readback.get(name) {
            Some(v) if (v - want).abs() < 0.01 => {}
            Some(v) => return Err(format!("{name} reads {v}, not its own {want}")),
            None => return Err(format!("{name} was not read back")),
        }
    }
    Ok(())
}

/// Rules 6, at nomination and on the plan: the platform's own lost-link action can be the
/// engagement's fallback ([`preload_for`]).
fn preload_check(uid: Option<&Uid>, fallback: Fallback, world: &WorldSnapshot, ec: &EngageRules) -> CheckResult {
    let r = RuleId::FallbackPreload;
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return check(r, uid, Outcome::Fail, "cannot evaluate: platform not in the world model");
    };
    // rules 15 (murmur-imu): an earlier log whose carry failed may owe this platform its own
    // lost-link values (it was an armed platform of that run's fleet): it may still carry an
    // earlier engagement's lost-link action, which this engagement's preload would read as its
    // own and later put back as such. Refused until that log is read or written off.
    if ec.rules_version >= crate::rules::CARRY_FOLLOWUPS_SINCE
        && let Some(c) = uid
            .and_then(|u| world.restore_owed.get(u))
            .filter(|o| o.holds())
            .and_then(|o| o.carried.as_ref())
    {
        return check(
            r,
            uid,
            Outcome::Fail,
            format!(
                "{} could not be read at the start and may owe this platform its own lost-link values (an armed platform of that run's fleet): it may still carry an earlier engagement's lost-link action, which a preload now would read as its own; nominate it once that log is read or an operator writes it off",
                c.from
            ),
        );
    }
    match preload_for(fallback, platform_autopilot(v), link_gap_limit_ms(Some(v), ec)) {
        Ok(p) => check(
            r,
            uid,
            Outcome::Pass,
            format!(
                "fallback {} as the platform's own lost-link action: {}; set and read back at dispatch, nothing released before",
                fallback.as_str(),
                p.describe()
            ),
        ),
        // rules 7 (murmur-a9h): Blimp 4.8 has no lost-link Land; its GCS failsafe disarms
        Err(_) if ec.rules_version >= crate::rules::QUEUE_FOLLOWUPS_SINCE && platform_autopilot(v) == Some(AutopilotKind::Blimp) => check(r, uid, Outcome::Fail, format!("Blimp 4.8's only lost-link action is to disarm (its GCS failsafe, off as shipped): fallback {} can't be its own failsafe", fallback.as_str())),
        Err(why) => check(r, uid, Outcome::Fail, why),
    }
}

/// Rules 6: at the release, the platform's lost-link action must have been read back as the
/// fallback this recently, ms (the bridge re-reads it every 5 s once the plan's time is
/// within [`PRELOAD_VERIFY_LEAD_S`]): a value changed on the vehicle since the dispatch (by
/// another station, a later job) shows before anything is released.
pub const PRELOAD_FRESH_MS: i64 = 10_000;
/// Rules 6: how long before the release window opens the bridge starts re-reading the
/// preload, s.
pub const PRELOAD_VERIFY_LEAD_S: f64 = 30.0;
/// Rules 6: how often the bridge re-reads the preload once it does, ms.
pub const PRELOAD_REREAD_MS: i64 = 5_000;

/// Rules 6: whether the preload of an engagement under way is due to be read again: its set
/// confirmed, the release window opening within [`PRELOAD_VERIFY_LEAD_S`] (or open), and its
/// last read-back at least [`PRELOAD_REREAD_MS`] old. The bridge and the scenario runner
/// read it again then (`engage.preload` verify), so the release check finds it fresh.
pub fn preload_verify_due(e: &Engagement, now: Time, cfg: &RulesConfig) -> bool {
    if !e.preloaded.as_ref().is_some_and(|r| r.confirmed) {
        return false;
    }
    let Some(snap) = &e.snapshot else { return false };
    let (from, until) = release_window(&snap.preview, &cfg.engage_rules());
    if now < from.plus_ms(-ms(PRELOAD_VERIFY_LEAD_S)) || now > until {
        return false;
    }
    let last = e.verified.as_ref().or(e.preloaded.as_ref()).map(|r| r.t);
    last.is_none_or(|t| now.ms_since(t) >= PRELOAD_REREAD_MS)
}

/// The engagement's last read-back of its preload: the time of the latest `set` or
/// `verify` report, if that report confirmed it (none if the latest did not, or none came).
pub fn preload_confirmed_at(e: &Engagement) -> Option<Time> {
    let latest = match (&e.preloaded, &e.verified) {
        (_, Some(v)) => Some(v),
        (Some(s), None) => Some(s),
        (None, None) => None,
    };
    latest.filter(|r| r.confirmed).map(|r| r.t)
}

/// Rules 6, at the release: the platform confirmed its lost-link action is the fallback, by a
/// read-back at most [`PRELOAD_FRESH_MS`] old (`preloaded`: the preload and when it was last
/// read back confirmed; none until then).
fn preloaded_check(uid: Option<&Uid>, preloaded: Option<(&LostLinkPreload, Time)>, now: Time) -> CheckResult {
    let r = RuleId::FallbackPreload;
    match preloaded {
        Some((p, at)) if now.ms_since(at) <= PRELOAD_FRESH_MS => check(
            r,
            uid,
            Outcome::Pass,
            format!(
                "the platform's lost-link action is the fallback, read back {:.1} s ago: {}",
                secs(now.ms_since(at)),
                p.describe()
            ),
        ),
        Some((_, at)) => check(
            r,
            uid,
            Outcome::Fail,
            format!(
                "the platform's lost-link action was last read back {:.1} s ago: at the release it must be at most {:.0} s old (read again every {:.0} s from {:.0} s before the window opens)",
                secs(now.ms_since(at)),
                secs(PRELOAD_FRESH_MS),
                secs(PRELOAD_REREAD_MS),
                PRELOAD_VERIFY_LEAD_S
            ),
        ),
        None => check(
            r,
            uid,
            Outcome::Fail,
            "the platform has not confirmed its lost-link action is the fallback: nothing is released",
        ),
    }
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
        warning: None,
    }
}

fn describe_track(id: &str, t: &TrackFacts) -> String {
    // rules 9: a rule declaration not confirmed by an operator is protected, and says so
    let basis = match &t.declaration {
        Some(d) if d.unconfirmed_rule() => format!(
            " by rule, D-{} not confirmed by an operator: protected",
            d.id
        ),
        _ => String::new(),
    };
    format!(
        "{id} ({}{basis}, {})",
        t.affiliation.value.as_str(),
        t.identity.as_deref().unwrap_or("unclassified")
    )
}

/// The engagement checks at submission and at each approval (the generic vehicle rules
/// run alongside, from [`crate::rules::evaluate`]). Fixed order, fixed precision.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let ec = cfg.engage_rules();
    let Some((track_id, radius, _, fallback)) = params(spec) else {
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
            let alt = effect_alt(track, world.t, p.effect_t, p.effect_radius_m, &ec);
            out.push(keep_out_friendly(
                p.effect_point,
                p.effect_t,
                p.keep_out_m,
                uid,
                world,
                cfg,
                alt,
            ));
            out.push(keep_out_tracks(
                p.effect_point,
                p.effect_t,
                p.keep_out_m,
                track_id,
                world,
                &ec,
                alt,
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
    if ec.v6() {
        out.push(preload_check(uid, fallback, world, &ec));
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
        declared_check(id, t, ec),
        identity_check(id, t, ec),
        recency_check(id, t, world, ec),
        uncertainty_check(id, t, ec, radius),
    ]
}

fn declared_check(id: &str, t: &TrackFacts, ec: &EngageRules) -> CheckResult {
    let r = RuleId::TargetDeclared;
    // rules 9: the declaration's basis (operator or rule, and the rule's evidence) in the
    // detail every signer reads; a rule declaration not confirmed by an operator passes only
    // where the policy lets one be nominated before a confirmation
    if ec.v9()
        && let Some(d) = &t.declaration
    {
        let basis = d.describe();
        return match t.affiliation.value {
            Affiliation::Hostile if d.unconfirmed_rule() && !ec.nominate_unconfirmed => check(
                r,
                None,
                Outcome::Fail,
                format!("{id} {basis}: the policy needs an operator's confirmation before a nomination"),
            ),
            Affiliation::Hostile if d.unconfirmed_rule() => check(
                r,
                None,
                Outcome::Pass,
                format!("{id} {basis} (the policy lets a rule declaration be nominated before a confirmation)"),
            ),
            Affiliation::Hostile => check(r, None, Outcome::Pass, format!("{id} {basis}")),
            a => check(
                r,
                None,
                Outcome::Fail,
                format!("{id} is {basis}, not hostile ({})", a.as_str()),
            ),
        };
    }
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
        (None, a) if ec.v9() => {
            let raised = if t.review_flags.is_empty() {
                String::new()
            } else {
                format!(" (under review: flag {})", flags_list(&t.review_flags))
            };
            check(
                r,
                None,
                Outcome::Fail,
                format!(
                    "{id} is {} by estimate{raised}; hostile must be declared, by an operator or by rule on an observed hostile act",
                    a.as_str()
                ),
            )
        }
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

/// Review flag numbers as `F-1, F-3`.
fn flags_list(flags: &[u64]) -> String {
    flags.iter().map(|f| format!("F-{f}")).collect::<Vec<_>>().join(", ")
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

#[allow(clippy::too_many_arguments)]
pub fn keep_out_friendly(
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    platform: Option<&Uid>,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    alt: Option<EffectAlt>,
) -> CheckResult {
    let r = RuleId::KeepOutFriendly;
    let ec = cfg.engage_rules();
    let mut nearest: Option<(f64, &Uid)> = None;
    let mut count = 0;
    // rules 6: lost vehicles an operator accepted, and vehicles clear by altitude
    let mut accepted: Vec<String> = Vec::new();
    let mut by_alt: Vec<String> = Vec::new();
    for (uid, v) in &world.vehicles {
        if Some(uid) == platform {
            continue;
        }
        let (d, how, clear_by_alt, alt_known) =
            match friendly_standing(uid, v, point, effect_t, keep_out_m, world, cfg, alt, true) {
                FriendlyStanding::Accepted { note } => {
                    accepted.push(note);
                    continue;
                }
                FriendlyStanding::Unclearable { detail, .. } => {
                    return check(r, Some(uid), Outcome::Fail, detail);
                }
                FriendlyStanding::Judged {
                    d,
                    how,
                    by_alt,
                    alt_known,
                    ..
                } => (d, how, by_alt, alt_known),
            };
        count += 1;
        if d < keep_out_m {
            // rules 6: in 3D where its altitude (fresh) and the effect's are known: clear when
            // even climbing or descending at its vertical rate it stays outside the keep-out
            // above or below the effect
            if let Some((a, c, m)) = clear_by_alt {
                by_alt.push(format!(
                    "{uid} at {a:.0} m ({c:.0} m outside the vertical keep-out with {m:.0} m of altitude uncertainty)"
                ));
                continue;
            }
            return check(
                r,
                Some(uid),
                Outcome::Fail,
                format!(
                    "vehicle {uid} could be {d:.0} m from the effect point at the effect time ({how}); keep-out {keep_out_m:.0} m{}",
                    vertical_note(alt.filter(|_| ec.v6()), alt_known)
                ),
            );
        }
        if nearest.is_none_or(|(n, _)| d < n) {
            nearest = Some((d, uid));
        }
    }
    let near = nearest.map_or(String::new(), |(d, u)| format!("; nearest {u} at {d:.0} m"));
    let mut tail = String::new();
    if !by_alt.is_empty() {
        tail.push_str(&format!("; clear by altitude: {}", by_alt.join(", ")));
    }
    if !accepted.is_empty() {
        tail.push_str(&format!("; position accepted as unknown: {}", accepted.join(", ")));
    }
    check(
        r,
        None,
        Outcome::Pass,
        format!("{count} vehicle(s) of ours clear of the {keep_out_m:.0} m keep-out at the effect time{near}{tail}"),
    )
}

/// Which region of one of our vehicles the friendly keep-out counts ([`friendly_standing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendlyKind {
    /// Along its velocity, less an uncertainty growing over the span.
    Velocity,
    /// Rules 3: anywhere on the circle it flies, less the same uncertainty.
    Circling,
    /// No fresh velocity: anywhere its cruise speed reaches.
    CruiseReach,
    /// Its position is not fresh (its link is lost or late): anywhere its cruise speed
    /// reaches since its last report.
    Lost,
    /// It never reported a position.
    NoPosition,
    /// No fresh velocity and no cruise speed (or nothing to predict it from).
    Unpredictable,
}

impl FriendlyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FriendlyKind::Velocity => "velocity",
            FriendlyKind::Circling => "circling",
            FriendlyKind::CruiseReach => "cruise_reach",
            FriendlyKind::Lost => "lost",
            FriendlyKind::NoPosition => "no_position",
            FriendlyKind::Unpredictable => "unpredictable",
        }
    }
}

/// How one of our vehicles stands against an engagement's keep-out, as
/// [`keep_out_friendly`] judges each vehicle but the platform (see [`friendly_standing`]).
#[derive(Clone, Debug, PartialEq)]
pub enum FriendlyStanding {
    /// Rules 6: lost since exactly the report an operator accepted for this engagement: left
    /// out (the check's note).
    Accepted { note: String },
    /// It can't clear the keep-out at all: the check fails with this detail.
    Unclearable { kind: FriendlyKind, detail: String },
    /// Where it could be at the effect time: a region (`center`, `radius_m`, for an
    /// explanation) whose near edge is `d` from the point (0 inside it: the check's distance),
    /// said `how`; when `d` is inside the keep-out and both altitudes are known, whether it is
    /// clear by altitude (its altitude, how far outside the vertical keep-out, its margin).
    Judged {
        kind: FriendlyKind,
        center: LonLat,
        radius_m: f64,
        d: f64,
        how: std::borrow::Cow<'static, str>,
        by_alt: Option<(f64, f64, f64)>,
        /// It reported an altitude (whether or not fresh), for the detail's note.
        alt_known: bool,
    },
}

/// Rules 3: the circle a vehicle reports, when it holds the vehicle on `world` (what
/// [`friendly_standing`] predicts it on). A vehicle flying a circle stays on it; its
/// momentary heading, carried a minute or more ahead, would put it kilometres away in any
/// direction. The circle holds only a vehicle that is on it: its position, reported with the
/// circle, within the circle's radius and a small slack of the center. Otherwise the circle
/// is wrong or stale, and the vehicle is predicted from where it is. Each report is judged on
/// its own freshness: the position by the usual link-aware rule, the center by its own age.
pub fn circle_holding<'v>(
    v: &'v crate::world::VehicleFacts,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Option<&'v crate::world::Fact<crate::world::Circle>> {
    let ec = cfg.engage_rules();
    let pos = v.position.as_ref()?;
    let pos_fresh = fresh(&v.position, v, "position", world, cfg).is_ok();
    v.circling.as_ref().filter(|_| ec.v3()).filter(|c| {
        let center_fresh = match c.value.center_t {
            Some(ct) => world.t.ms_since(ct) <= CIRCLE_CENTER_MAX_AGE_MS,
            None => fresh(&v.circling, v, "circle", world, cfg).is_ok(),
        };
        pos_fresh
            && center_fresh
            && haversine_m(pos.value, c.value.center) <= c.value.radius_m + CIRCLE_SLACK_M
    })
}

/// One of our vehicles (`uid`, not the platform) against a keep-out reaching `keep_out_m`
/// from `point` at `effect_t`: what [`keep_out_friendly`] reads for each. With a fresh
/// velocity it is carried along it, less an uncertainty growing over the span; a vehicle a
/// circle holds (rules 3) is anywhere on its disc (or carried at most [`CIRCLE_CARRY_S`]
/// along its velocity, the nearer counting); without a fresh velocity, as far as its cruise
/// speed reaches; lost (rules 6), the same from its last report, unless an operator accepted
/// exactly that loss for this engagement. `words`: with the detail's words (`how`; else
/// empty where they would be formatted).
#[allow(clippy::too_many_arguments)]
pub fn friendly_standing(
    uid: &Uid,
    v: &crate::world::VehicleFacts,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    alt: Option<EffectAlt>,
    words: bool,
) -> FriendlyStanding {
    let ec = cfg.engage_rules();
    let pos_fresh = fresh(&v.position, v, "position", world, cfg).is_ok();
    // rules 6: lost since exactly the report an operator accepted for this engagement
    if ec.v6()
        && !pos_fresh
        && let Some(a) = world.unknown_accepted.get(uid)
        && a.since == v.position.as_ref().map(|p| p.t)
    {
        return FriendlyStanding::Accepted {
            note: format!("{uid}: {}, accepted by {}", lost_since(v, world), a.by),
        };
    }
    let Some(pos) = &v.position else {
        return FriendlyStanding::Unclearable {
            kind: FriendlyKind::NoPosition,
            detail: if ec.v6() {
                format!(
                    "vehicle {uid} has no position: cannot clear the keep-out ({}: an operator may accept it as unknown)",
                    lost_since(v, world)
                )
            } else {
                format!("vehicle {uid} has no position: cannot clear the keep-out")
            },
        };
    };
    let reported = v.circling.as_ref().filter(|_| ec.v3());
    let circling = circle_holding(v, world, cfg);
    let vel = v
        .velocity_mps
        .as_ref()
        .filter(|_| pos_fresh)
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
    // each candidate: (distance of its near edge, the words, kind, centre, radius)
    type Region = (f64, std::borrow::Cow<'static, str>, FriendlyKind, LonLat, f64);
    let disc: Option<Region> = circling.map(|c| {
        // anywhere on its disc, slack included, with the same uncertainty growth as any
        // of ours (it may be retasked) since the older of its reports; fail closed on
        // the near edge
        let dt = secs(effect_t.ms_since(if c.t < pos.t { c.t } else { pos.t })).max(0.0);
        let margin = ec.keep_out_sigma * ec.friendly_growth() * dt;
        (
            (haversine_m(c.value.center, point) - c.value.radius_m - CIRCLE_SLACK_M - margin).max(0.0),
            if words {
                std::borrow::Cow::<'static, str>::from(format!(
                    "circling within {:.0} m of its center, less its uncertainty",
                    c.value.radius_m
                ))
            } else {
                std::borrow::Cow::Borrowed("")
            },
            FriendlyKind::Circling,
            c.value.center,
            c.value.radius_m + CIRCLE_SLACK_M + margin,
        )
    });
    let here: Option<Region> = match vel {
        Some([e, n]) => {
            // along its velocity, with an uncertainty that grows with the span (it may
            // turn or be retasked); fail closed on the near edge
            let p = offset(pos.value, e * carry, n * carry);
            let margin = ec.keep_out_sigma * ec.friendly_growth() * span;
            let k = FriendlyKind::Velocity;
            Some(if circling.is_some() {
                (
                    (haversine_m(p, point) - margin).max(0.0),
                    if words {
                        format!(
                            "predicted along its velocity for {CIRCLE_CARRY_S:.0} s, then on its circle, less its uncertainty"
                        )
                        .into()
                    } else {
                        "".into()
                    },
                    k,
                    p,
                    margin,
                )
            } else if off_circle {
                (
                    (haversine_m(p, point) - margin).max(0.0),
                    "predicted along its velocity, less its uncertainty; its reported circle does not hold its position".into(),
                    k,
                    p,
                    margin,
                )
            } else if ec.v2() {
                (
                    (haversine_m(p, point) - margin).max(0.0),
                    "predicted along its velocity, less its uncertainty".into(),
                    k,
                    p,
                    margin,
                )
            } else {
                (haversine_m(p, point), "predicted along its velocity".into(), k, p, 0.0)
            })
        }
        None => {
            let cruise = v
                .platform
                .as_ref()
                .and_then(|p| p.cruise_mps)
                .filter(|c| c.is_finite() && *c >= 0.0);
            let Some(cruise) = cruise else {
                return FriendlyStanding::Unclearable {
                    kind: FriendlyKind::Unpredictable,
                    detail: format!(
                        "vehicle {uid}: no fresh velocity and no cruise speed: cannot clear the keep-out"
                    ),
                };
            };
            Some((
                (haversine_m(pos.value, point) - cruise * carry).max(0.0),
                if ec.v6() && !pos_fresh {
                    if words {
                        format!(
                            "how far it could reach at cruise speed; {}: an operator may accept it as unknown",
                            lost_since(v, world)
                        )
                        .into()
                    } else {
                        "".into()
                    }
                } else {
                    "how far it could reach at cruise speed".into()
                },
                if pos_fresh {
                    FriendlyKind::CruiseReach
                } else {
                    FriendlyKind::Lost
                },
                pos.value,
                cruise * carry,
            ))
        }
    };
    let (d, how, kind, center, radius_m) = match (disc, here) {
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
            return FriendlyStanding::Unclearable {
                kind: FriendlyKind::Unpredictable,
                detail: format!("vehicle {uid}: cannot predict it: cannot clear the keep-out"),
            };
        }
    };
    let mut by_alt = None;
    if d < keep_out_m
        && let Some(e) = alt.filter(|_| ec.v6())
        && let Ok(a) = fresh(&v.alt_msl_m, v, "altitude", world, cfg)
    {
        let alt_t = v.alt_msl_m.as_ref().map_or(pos.t, |f| f.t);
        let alt_span = secs(effect_t.ms_since(alt_t)).max(0.0);
        let rate = friendly_vertical_mps(v, &ec);
        let m = ec.keep_out_sigma * VEHICLE_ALT_SIGMA_M + rate * alt_span;
        if let Some(c) = vertical_clear(&e, *a, m) {
            by_alt = Some((*a, c, m));
        }
    }
    FriendlyStanding::Judged {
        kind,
        center,
        radius_m,
        d,
        how,
        by_alt,
        alt_known: v.alt_msl_m.is_some(),
    }
}

/// How long one of our vehicles has been lost, for an operator to accept: "position unknown
/// for 312 s (since t=1781000123000)" (engine ms, what an acceptance names), or "no position
/// ever reported (since none)".
fn lost_since(v: &crate::world::VehicleFacts, world: &WorldSnapshot) -> String {
    match &v.position {
        Some(p) => format!(
            "position unknown for {:.0} s (since t={})",
            secs(world.t.ms_since(p.t)),
            p.t.0
        ),
        None => "no position ever reported (since none)".into(),
    }
}

/// Why a keep-out that failed in 2D was not read in 3D (rules 6), for the detail.
fn vertical_note(alt: Option<EffectAlt>, known: bool) -> &'static str {
    match (alt, known) {
        (Some(_), true) => "; within the vertical keep-out too",
        (Some(_), false) => "; its altitude is unknown, so the keep-out has no ceiling",
        (None, _) => "",
    }
}

// ---- rules 6: the keep-outs' prediction model and altitude --------------------------------

/// Rules 6: a protected track without a velocity estimate could be anywhere it reaches at
/// its domain's top speed, m/s (fail closed on missing data): 100 for aircraft (a light
/// aircraft or small UAS cruises at 50 to 70), 30 on the ground, 25 on the surface (a fast
/// small boat at about 50 kt), 15 under water.
pub fn top_speed_mps(domain: Domain) -> f64 {
    match domain {
        Domain::Air => 100.0,
        Domain::Ground => 30.0,
        Domain::Surface => 25.0,
        Domain::Subsurface => 15.0,
    }
}

/// Rules 6: the 1-sigma error of a track's altitude estimate, m: the sensing model's radar
/// height error on aircraft (30, `tracks/sensing.rs`), 5 for a sonar depth, 10 for a surface
/// or ground contact (its height above the datum).
pub fn track_alt_sigma_m(domain: Domain) -> f64 {
    match domain {
        Domain::Air => 30.0,
        Domain::Subsurface => 5.0,
        Domain::Surface | Domain::Ground => 10.0,
    }
}

/// Rules 6: how fast a protected track may change altitude, m/s: an aircraft at
/// `vertical_rate_mps`, a vessel on the surface not at all, anything on the ground or under
/// water at 2 (a road's grade, a submarine's depth change).
pub fn track_vertical_mps(domain: Domain, ec: &EngageRules) -> f64 {
    match domain {
        Domain::Air => ec.vertical_rate(),
        Domain::Surface => 0.0,
        Domain::Ground | Domain::Subsurface => 2.0,
    }
}

/// Rules 6: the 1-sigma error of one of our vehicles' altitude (GNSS vertical), m.
pub const VEHICLE_ALT_SIGMA_M: f64 = 10.0;

/// Rules 6: the steepest dive counted for one of our fixed wings, degrees: ArduPlane's default
/// pitch-down limit (`PTCH_LIM_MIN_DEG` -25, ArduPlane/config.h `PITCH_MIN`).
pub const FIXED_WING_DIVE_DEG: f64 = 25.0;

/// Rules 6: how fast one of our vehicles may climb or descend, m/s, either way: the largest of
/// `vertical_rate_mps`, the catalog's climb rate, and for a fixed wing its dive at its top
/// speed (`max_mps`, else cruise) down [`FIXED_WING_DIVE_DEG`] (a MALE aircraft at 105 m/s:
/// 44 m/s).
pub fn friendly_vertical_mps(v: &crate::world::VehicleFacts, ec: &EngageRules) -> f64 {
    let p = v.platform.as_ref();
    let climb = p.and_then(|p| p.climb_mps).unwrap_or(0.0);
    let fixed_wing = platform_autopilot(v).is_some_and(AutopilotKind::is_fixed_wing);
    let dive = if fixed_wing {
        p.and_then(|p| p.max_mps.or(p.cruise_mps)).unwrap_or(0.0) * FIXED_WING_DIVE_DEG.to_radians().sin()
    } else {
        0.0
    };
    ec.vertical_rate().max(climb).max(dive)
}

/// Rules 6: the 1-sigma error of a protected track's predicted position `span_s` after its
/// estimate, along its velocity: a constant-velocity prediction with white acceleration
/// noise (Bar-Shalom, Li and Kirubarajan, Estimation with Applications to Tracking and
/// Navigation, 2001, the continuous white-noise acceleration model), its three parts
/// independent: the tracker's own 1-sigma now (`pos_sigma_m`, already grown for the track's
/// age), the velocity estimate's error times the span (`track_velocity_sigma_mps`), and the
/// manoeuvres it may make, `q span^3 / 3` with `q` the domain's acceleration noise squared
/// times the 1 s update (`track_accel_mps2`). Not linear in speed times time: a fast track on
/// a steady course is as predictable as a slow one. Rules 18: the velocity's error is the
/// tracker's own when larger than the policy's ([`track_velocity_error`]).
pub fn track_prediction_sigma(t: &TrackFacts, span_s: f64, ec: &EngageRules) -> f64 {
    let span = span_s.max(0.0);
    let v = track_velocity_error(t, ec) * span;
    let a = ec.track_accel(t.domain);
    let q = a * a; // m^2/s^3 at the 1 s update
    (t.pos_sigma_m.powi(2) + v * v + q * span.powi(3) / 3.0).sqrt()
}

/// The 1-sigma error, m/s, a protected track's velocity is predicted with (the regions along its
/// velocity and its average velocity, [`track_prediction_sigma`]; a young track's speed bound).
/// Rules 18 ([`crate::rules::VELOCITY_SIGMA_SINCE`], murmur-vni): the tracker's own, in its least
/// certain direction (`velocity_sigma_mps`), or the policy's `track_velocity_sigma_mps` when that
/// is larger or none is given (a scenario, a caller that does not say), the same reading as
/// rules 14's speed bound ([`aircraft_speed_bound`]); before, the policy's alone, whatever the
/// tracker said: every shown velocity was taken as good to 1 m/s, which held only while the
/// tracker showed none worse than its gate allowed (3 m/s) and the gate kept aircraft fixed by
/// coarse sensors from showing any.
pub fn track_velocity_error(t: &TrackFacts, ec: &EngageRules) -> f64 {
    let policy = ec.track_velocity_sigma();
    if ec.own_velocity_error() {
        t.velocity_sigma_mps.unwrap_or(0.0).max(policy)
    } else {
        policy
    }
}

/// One region a protected track may be in at the effect time, as a keep-out counts it: a
/// centre, how far from it (`keep_out_sigma` of its uncertainty included), and how.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackReach {
    pub center: LonLat,
    pub radius_m: f64,
    /// In words, as a check's detail quotes it (empty from [`track_regions`]).
    pub how: String,
    /// Which region it is: the checks read the geometry; an explanation
    /// ([`crate::keepout`]) names the region.
    pub kind: ReachKind,
}

/// Which of a protected track's regions a [`TrackReach`] is (see [`track_reach`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachKind {
    /// Rules 6: along its velocity of the moment, with its predicted uncertainty.
    Velocity,
    /// Rules 6: no velocity estimate: anywhere it reaches at its domain's top speed.
    NoVelocity,
    /// Rules 9: along its average velocity of the last minute.
    MeanVelocity,
    /// Rules 9: the tracker saw it manoeuvre: anywhere it reaches at the fastest it went.
    Manoeuvre,
    /// Rules 12: too young for an average velocity: anywhere it reaches at its domain's
    /// plausible top speed.
    Young,
    /// Rules 14: an aircraft: anywhere it reaches from its own speed, speeding up to its top.
    Aircraft,
    /// Rules 16: out of view (a lost contact, or a live track unseen [`OUT_OF_VIEW_S`]):
    /// anywhere it reaches at its domain's top speed since its last fix ([`lost_reach`]).
    OutOfView,
    /// Rules 19: along the path an operator accepted for it as behaving now
    /// ([`behaving_regions`]).
    Accepted,
}

impl ReachKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReachKind::Velocity => "velocity",
            ReachKind::NoVelocity => "no_velocity",
            ReachKind::MeanVelocity => "mean_velocity",
            ReachKind::Manoeuvre => "manoeuvre",
            ReachKind::Young => "young",
            ReachKind::Aircraft => "aircraft",
            ReachKind::OutOfView => "out_of_view",
            ReachKind::Accepted => "accepted",
        }
    }
}

/// Rules 6 and 9: where a protected track may be at `effect_t`, from its estimate over the
/// span since it. Rules 6: along its velocity with `keep_out_sigma` of
/// [`track_prediction_sigma`], or, without a velocity estimate, anywhere it reaches at its
/// domain's top speed. Rules 9: also along its average velocity of the last minute
/// ([`TrackFacts::mean_velocity_mps`]), with the same uncertainty, and a track the tracker
/// saw manoeuvre within its memory ([`TrackFacts::manoeuvre`]) anywhere it reaches at the
/// fastest it went, whichever way it turns: a minute ahead, a contact that zigzags, weaves or
/// sprints and stops is not where its velocity of the moment says (the tracker of rules 9
/// follows a manoeuvre within seconds, so its velocity is the current leg's). Rules 12
/// (murmur-9m0): a young track, one the tracker started ([`TrackFacts::first_seen_t`]) too
/// recently to have an average velocity, can't yet be said not to manoeuvre (the facts above
/// do not exist yet: before, it fell back to the one region along its velocity), so it also
/// counts anywhere it reaches at its domain's plausible top speed ([`top_speed_mps`]), or at
/// its own speed bound (its speed and `keep_out_sigma` of its velocity error) when that is
/// faster: fail closed, at the cost of larger keep-outs for its first minute or so. Rules 14
/// (murmur-oho): an aircraft turns through a right angle in 15 to 30 s and speeds up whenever
/// it likes, so its course says little of where it will be even seconds ahead; with a velocity
/// it also counts anywhere it reaches from its speed bound ([`aircraft_speed_bound`]), speeding
/// up at [`AIRCRAFT_SPEEDUP_MPS2`] to its top speed ([`aircraft_reach_m`]), whether or not the
/// tracker has seen it turn. Rules 18 (murmur-vni): the regions along its velocity and its average
/// velocity, and a young track's speed bound, read the velocity's error the tracker reports when
/// larger than the policy's ([`track_velocity_error`]), so a velocity known only to 6 m/s widens
/// them as it should rather than counting as good to 1. The keep-out holds only if every region
/// clears it.
pub fn track_reach(t: &TrackFacts, effect_t: Time, ec: &EngageRules) -> Vec<TrackReach> {
    reach(t, effect_t, ec, true)
}

/// [`track_reach`] without the words (each `how` empty): the same regions, for a scan over
/// many effect times ([`crate::keepout`]).
pub fn track_regions(t: &TrackFacts, effect_t: Time, ec: &EngageRules) -> Vec<TrackReach> {
    reach(t, effect_t, ec, false)
}

/// Rules 16: a track is out of view after this long without a detection, s, and from then on
/// a keep-out counts it anywhere it could have got to since its last fix ([`out_of_view`]).
/// The bridge's tracker calls a track stale at the same age (`tracks::model::STALE_S`: the
/// console fades it) and drops it at 180 s (then a lost contact, [`crate::lost`]). Why 10 s:
/// the sensing model draws a detection every second for every sensor in reach, with a
/// probability of at least 0.47 within a sensor's nominal range and 0.94 within half of it, so
/// ten misses in a row (at most 0.16%, and 10^-12 at half range) mean the sensors are not
/// seeing the contact, not a gap between fixes; an AIS report comes every 2 to 10 s underway.
/// Below it a contact that turned back unseen is at most about twice its speed times the gap
/// off its course (100 m at 5 m/s), which the regions along its course already allow for
/// (their acceleration-noise term is that size 40 s ahead); above it nothing bounds where it
/// went but its speed. Until the drop the track is in the picture and the tracker would say
/// nothing of a turn it has not seen (the review of murmur-533: a ferry steaming away, unseen
/// 170 s, let a nomination through that the same ferry unseen 182 s, lost, blocked).
pub const OUT_OF_VIEW_S: f64 = 10.0;

/// Rules 16: whether a track is out of view at `now`: no detection for [`OUT_OF_VIEW_S`]
/// (never before rules 16).
pub fn out_of_view(t: &TrackFacts, now: Time, ec: &EngageRules) -> bool {
    ec.lost() && secs(now.ms_since(t.last_seen_t)) >= OUT_OF_VIEW_S
}

/// Rules 16: how fast a track out of view may be going since its last fix, m/s, and how far
/// beyond its estimate the tracker may lag, m: its domain's top speed ([`top_speed_mps`], what
/// a track without a velocity counts at: 25 m/s on the surface, 100 in the air), or its own
/// bound when that is faster (its speed and `keep_out_sigma` of its velocity's error, the
/// tracker's or the policy's whichever is larger, or the fastest it went when the tracker saw
/// it manoeuvre); for an aircraft (rules 14) also [`AIRCRAFT_LAG_M`].
pub fn out_of_view_speed(t: &TrackFacts, ec: &EngageRules) -> (f64, f64) {
    let k = ec.keep_out_sigma;
    let own = t.velocity_mps.map_or(0.0, |[e, n]| {
        e.hypot(n) + k * t.velocity_sigma_mps.unwrap_or(0.0).max(ec.track_velocity_sigma())
    });
    let own = t.manoeuvre.filter(|_| ec.v9()).map_or(own, |m| m.speed_mps.max(own));
    let lag = if ec.aircraft_reach() && t.domain == Domain::Air { AIRCRAFT_LAG_M } else { 0.0 };
    (top_speed_mps(t.domain).max(own), lag)
}

/// Rules 16: where a lost contact may be at `effect_t`, from its facts at the drop: the regions
/// a live track gets from them ([`track_reach`]) and anywhere it reaches since its last fix at
/// its domain's top speed, or its own bound if faster ([`out_of_view_speed`]), whichever way it
/// turned and however it sped up. No cap: fail closed. The first version counted only the
/// regions along its course (a prediction for a contact that keeps its course), the second
/// also anywhere at its own speed bound (a prediction for one that keeps its speed): nothing
/// checks either of a contact nobody sees, so neither is a bound (murmur-533's measurement and
/// its review, `eval/METRICS.md` "Out of view"). The precedents count the same way: a track
/// without a velocity at its domain's top speed, a lost vehicle of ours at its class's speed.
/// A live track out of view counts the same disc ([`track_standing`], [`OUT_OF_VIEW_S`]). A
/// region already counted at the last fix at least as far (no velocity, young) stands for it.
pub fn lost_reach(t: &TrackFacts, effect_t: Time, ec: &EngageRules) -> Vec<TrackReach> {
    regions_at(t, effect_t, ec, true, true)
}

/// [`lost_reach`] without the words (each `how` empty).
pub fn lost_regions(t: &TrackFacts, effect_t: Time, ec: &EngageRules) -> Vec<TrackReach> {
    regions_at(t, effect_t, ec, false, true)
}

/// Rules 16: the regions a keep-out counts a live track in at `effect_t`, judged at `now`:
/// those of [`track_reach`] and, when it is out of view then ([`out_of_view`]), anywhere at its
/// domain's top speed since its last fix, as a lost contact ([`lost_reach`]). Anything that
/// judges or draws a live track's keep-out regions reads this (or [`track_standing`]), never
/// [`track_reach`] alone.
pub fn track_reach_at(t: &TrackFacts, now: Time, effect_t: Time, ec: &EngageRules) -> Vec<TrackReach> {
    regions_at(t, effect_t, ec, true, out_of_view(t, now, ec))
}

/// [`track_reach_at`] without the words (each `how` empty).
pub fn track_regions_at(t: &TrackFacts, now: Time, effect_t: Time, ec: &EngageRules) -> Vec<TrackReach> {
    regions_at(t, effect_t, ec, false, out_of_view(t, now, ec))
}

/// The regions of [`track_reach`] and, with `unseen` (rules 16), the out-of-view disc.
fn regions_at(t: &TrackFacts, effect_t: Time, ec: &EngageRules, words: bool, unseen: bool) -> Vec<TrackReach> {
    let mut out = reach(t, effect_t, ec, words);
    if !(unseen && ec.lost()) {
        return out;
    }
    let span = secs(effect_t.ms_since(t.position.t)).max(0.0);
    let k = ec.keep_out_sigma;
    let (speed, lag) = out_of_view_speed(t, ec);
    let radius_m = k * t.pos_sigma_m + lag + speed * span;
    let covered = out.iter().any(|r| {
        matches!(r.kind, ReachKind::NoVelocity | ReachKind::Young) && r.center == t.position.value && r.radius_m >= radius_m
    });
    if !covered {
        out.push(TrackReach {
            center: t.position.value,
            radius_m,
            how: if words {
                let lag = if lag > 0.0 { format!(" and the tracker's lag ({lag:.0} m)") } else { String::new() };
                format!(
                    "out of view, so whichever way it turned or sped up since its last fix: anywhere it reaches at {speed:.1} m/s in {span:.0} s, {radius_m:.0} m with its {k:.0}-sigma{lag}"
                )
            } else {
                String::new()
            },
            kind: ReachKind::OutOfView,
        });
    }
    out
}

/// Rules 19: the regions a keep-out counts a live track in at `effect_t` while an operator's
/// acceptance of it as behaving now holds (`a`, [`behaving_lapse`] none on this picture): along
/// the path accepted for it (its accepted velocity from its accepted position, with
/// `keep_out_sigma` of [`track_prediction_sigma`] of the accepted facts over the span since its
/// accepted estimate: rules 18's velocity error and the domain's white-noise acceleration, and
/// for an aircraft the tracker's lag, [`AIRCRAFT_LAG_M`]) and along its course now (its velocity
/// and its average velocity, as any track's: [`track_reach`]'s `velocity` and `mean_velocity`).
/// Not anywhere it could turn or speed up to (an aircraft's reach, a young track's, a
/// manoeuvre's): that is the judgment the operator took, and the lapse watches it. The keep-out
/// holds for it only if every region clears it.
pub fn behaving_regions(a: &AcceptedBehaving, t: &TrackFacts, effect_t: Time, ec: &EngageRules, words: bool) -> Vec<TrackReach> {
    let k = ec.keep_out_sigma;
    let span = secs(effect_t.ms_since(a.facts.position.t)).max(0.0);
    let sigma = track_prediction_sigma(&a.facts, span, ec);
    let lag = if ec.aircraft_reach() && a.facts.domain == Domain::Air { AIRCRAFT_LAG_M } else { 0.0 };
    let radius_m = k * sigma + lag;
    let mut out = vec![TrackReach {
        center: accepted_at(a, effect_t),
        radius_m,
        how: if words {
            let v = a.facts.velocity_mps.map_or(0.0, |[e, n]| e.hypot(n));
            let lag = if lag > 0.0 { format!(" and the tracker's lag ({lag:.0} m)") } else { String::new() };
            format!(
                "accepted as behaving now by {} at t={}: on its accepted course at {v:.1} m/s, {k:.0}-sigma {:.0} m over {span:.0} s (its velocity known to {:.1} m/s){lag}",
                a.by,
                a.t.0,
                k * sigma,
                track_velocity_error(&a.facts, ec)
            )
        } else {
            String::new()
        },
        kind: ReachKind::Accepted,
    }];
    out.extend(
        reach(t, effect_t, ec, words)
            .into_iter()
            .filter(|r| matches!(r.kind, ReachKind::Velocity | ReachKind::MeanVelocity)),
    );
    out
}

/// Rules 19: [`track_standing`] for a track accepted as behaving now whose acceptance holds: by
/// [`behaving_regions`].
#[allow(clippy::too_many_arguments)]
pub fn behaving_standing(
    a: &AcceptedBehaving,
    t: &TrackFacts,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    now: Time,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
    words: bool,
) -> Option<TrackStanding> {
    standing(t, behaving_regions(a, t, effect_t, ec, words), point, effect_t, keep_out_m, now, ec, alt)
}

/// How one live track stands against a keep-out for this decision: by [`behaving_standing`] while
/// an acceptance of it as behaving now holds on `world` (rules 19), else by [`track_standing`].
/// Returns the standing, the acceptance in force, and the lapse when the picture lapses one.
#[allow(clippy::too_many_arguments)]
pub fn live_standing<'w>(
    id: &str,
    t: &TrackFacts,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    world: &'w WorldSnapshot,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
    words: bool,
) -> (Option<TrackStanding>, Option<&'w AcceptedBehaving>, Option<Lapse>) {
    match behaving_in_force(id, t, world, ec) {
        Ok(Some(a)) => (behaving_standing(a, t, point, effect_t, keep_out_m, world.t, ec, alt, words), Some(a), None),
        Ok(None) => (track_standing(t, point, effect_t, keep_out_m, world.t, ec, alt, words), None, None),
        Err(l) => (track_standing(t, point, effect_t, keep_out_m, world.t, ec, alt, words), None, Some(l)),
    }
}

/// The regions of [`track_reach`]; `words`: with each region's `how`.
fn reach(t: &TrackFacts, effect_t: Time, ec: &EngageRules, words: bool) -> Vec<TrackReach> {
    let span = secs(effect_t.ms_since(t.position.t)).max(0.0);
    let k = ec.keep_out_sigma;
    let Some([ve, vn]) = t.velocity_mps else {
        let top = top_speed_mps(t.domain);
        // rules 14: and an aircraft as far again as the tracker may lag behind it
        let lag = if ec.aircraft_reach() && t.domain == Domain::Air { AIRCRAFT_LAG_M } else { 0.0 };
        let radius_m = k * t.pos_sigma_m + lag + top * span;
        return vec![TrackReach {
            center: t.position.value,
            radius_m,
            how: if words {
                let lag = if lag > 0.0 { format!(" and the tracker's lag ({lag:.0} m)") } else { String::new() };
                format!(
                    "no velocity estimate: anywhere it reaches at {top:.0} m/s in {span:.0} s, {radius_m:.0} m with its {k:.0}-sigma{lag}"
                )
            } else {
                String::new()
            },
            kind: ReachKind::NoVelocity,
        }];
    };
    let sigma = track_prediction_sigma(t, span, ec);
    let v_err = track_velocity_error(t, ec);
    let mut out = vec![TrackReach {
        center: track_at(t, effect_t),
        radius_m: k * sigma,
        how: if words {
            // rules 18: and the velocity error it was predicted with (the tracker's, or the
            // policy's floor)
            let at = if ec.own_velocity_error() { format!(" at {v_err:.1} m/s") } else { String::new() };
            format!(
                "{k:.0}-sigma {:.0} m over {span:.0} s: tracker {:.0} m, velocity {:.0} m{at}, manoeuvre {:.0} m",
                k * sigma,
                t.pos_sigma_m,
                v_err * span,
                ec.track_accel(t.domain) * (span.powi(3) / 3.0).sqrt()
            )
        } else {
            String::new()
        },
        kind: ReachKind::Velocity,
    }];
    if let Some([e, n]) = t.mean_velocity_mps.filter(|_| ec.v9()) {
        let dt = secs(effect_t.ms_since(t.position.t));
        out.push(TrackReach {
            center: offset(t.position.value, e * dt, n * dt),
            radius_m: k * sigma,
            how: if words {
                format!(
                    "along its average velocity of the last minute ({:.1} m/s), {k:.0}-sigma {:.0} m over {span:.0} s",
                    e.hypot(n),
                    k * sigma
                )
            } else {
                String::new()
            },
            kind: ReachKind::MeanVelocity,
        });
    }
    if let Some(m) = t.manoeuvre.filter(|_| ec.v9()) {
        let radius_m = k * t.pos_sigma_m + m.speed_mps * span;
        out.push(TrackReach {
            center: t.position.value,
            radius_m,
            how: if words {
                format!(
                    "manoeuvring (its velocity changed by {:.1} m/s, last {:.0} s before its estimate): anywhere it reaches at {:.1} m/s in {span:.0} s, {radius_m:.0} m with its {k:.0}-sigma",
                    m.change_mps,
                    secs(t.position.t.ms_since(m.t)).max(0.0),
                    m.speed_mps
                )
            } else {
                String::new()
            },
            kind: ReachKind::Manoeuvre,
        });
    }
    // rules 12: young, so a manoeuvre is not ruled out yet
    if ec.followups()
        && t.mean_velocity_mps.is_none()
        && let Some(first) = t.first_seen_t
    {
        let tracked = secs(t.position.t.ms_since(first)).max(0.0);
        let top = top_speed_mps(t.domain);
        // rules 18: its own velocity error when larger than the policy's
        let bound = ve.hypot(vn) + k * v_err;
        let speed = top.max(bound);
        let radius_m = k * t.pos_sigma_m + speed * span;
        out.push(TrackReach {
            center: t.position.value,
            radius_m,
            how: if words {
                format!(
                    "young (tracked {tracked:.0} s, no average velocity yet: a manoeuvre is not ruled out): anywhere it reaches at {speed:.1} m/s in {span:.0} s, {radius_m:.0} m with its {k:.0}-sigma"
                )
            } else {
                String::new()
            },
            kind: ReachKind::Young,
        });
    }
    // rules 14: an aircraft may turn or speed up at any moment
    if ec.aircraft_reach() && t.domain == Domain::Air {
        let (from, to, reach) = aircraft_reach_m(aircraft_speed_bound(t, ec), span);
        let radius_m = k * t.pos_sigma_m + AIRCRAFT_LAG_M + reach;
        out.push(TrackReach {
            center: t.position.value,
            radius_m,
            how: if words {
                format!(
                    "an aircraft, which may turn or speed up at any moment: anywhere it reaches from {from:.1} m/s, up to {to:.1} m/s, in {span:.0} s, {radius_m:.0} m with its {k:.0}-sigma and the tracker's lag ({AIRCRAFT_LAG_M:.0} m)"
                )
            } else {
                String::new()
            },
            kind: ReachKind::Aircraft,
        });
    }
    out
}

/// Rules 14: how fast an aircraft may speed up, m/s^2: a bound, not a sigma, over every source
/// of aircraft motion here. The contacts process (the truth generator) speeds an aircraft up at
/// 0.5 m/s^2 (`murmur-contacts`, `ACCEL_MPS2`) and stops it at once (which shortens its reach);
/// the catalog's aircraft, as their stand-ins fly them (modelled on ArduPilot's SITL), at up to
/// 2.5 (a copter's `WP_ACC`), 3 (a plane; a VTOL in forward flight) and 4.2 (a VTOL in its
/// transition from hover, `TRANSITION_ACC`); the air calibration's hardest speed-up is a
/// helicopter leaving its hover at 3 (`tracks::tests::calib_air`). A dive is faster: it is not
/// modelled, as an aircraft's altitude is not where a 2D keep-out looks. The policy's air
/// `track_accel_mps2` (2 m/s^2) is a white-noise sigma for the prediction along a velocity, not
/// a bound.
pub const AIRCRAFT_SPEEDUP_MPS2: f64 = 4.5;

/// Rules 14: how long the tracker may take to take in an aircraft's manoeuvre, s: the aircraft
/// counts as if it had been speeding up at [`AIRCRAFT_SPEEDUP_MPS2`] that long unseen, so its
/// speed that much faster than its bound says and its position [`AIRCRAFT_LAG_M`] beyond its
/// estimate. Measured, not derived: the bridge's air tracker (`tracks::tests::calib_air`) takes
/// in a helicopter leaving its hover at 3 m/s^2 or an aircraft speeding up at 1 m/s^2 only
/// after 7 to 13 s (it has said 2 m/s of a helicopter already going 24), and falls up to 300 m
/// behind a turn of 6 to 8 deg/s when EO/IR loses it; on the air tuning sets a keep-out 3 to 100
/// s ahead held every aircraft every time from 5 s up (4 s missed one by 2 m, 3 s by up to 32
/// m). 7 s, as margin.
pub const AIRCRAFT_LAG_S: f64 = 7.0;

/// Rules 14: how far beyond its estimate an aircraft may be when the tracker has not yet taken
/// in its manoeuvre, m: [`AIRCRAFT_LAG_S`] of speeding up at [`AIRCRAFT_SPEEDUP_MPS2`] (110 m).
/// Every region of an aircraft counts it, with a velocity or without.
pub const AIRCRAFT_LAG_M: f64 = 0.5 * AIRCRAFT_SPEEDUP_MPS2 * AIRCRAFT_LAG_S * AIRCRAFT_LAG_S;

/// Rules 14: the fastest a protected aircraft with a velocity may be going now, m/s: its speed,
/// `keep_out_sigma` of its velocity's error in its least certain direction (the tracker's
/// `velocity_sigma_mps`, or the policy's `track_velocity_sigma_mps` when larger or not given),
/// and the tracker's lag behind a speed-up ([`AIRCRAFT_LAG_S`] at [`AIRCRAFT_SPEEDUP_MPS2`]), the
/// lag no faster than the domain's top speed.
pub fn aircraft_speed_bound(t: &TrackFacts, ec: &EngageRules) -> f64 {
    let speed = t.velocity_mps.map_or(0.0, |[e, n]| e.hypot(n));
    let sigma = t.velocity_sigma_mps.unwrap_or(0.0).max(ec.track_velocity_sigma());
    let bound = speed + ec.keep_out_sigma * sigma;
    (bound + AIRCRAFT_LAG_S * AIRCRAFT_SPEEDUP_MPS2).min(top_speed_mps(Domain::Air).max(bound))
}

/// Rules 14: how far an aircraft going at `speed` (m/s, [`aircraft_speed_bound`]) may get in
/// `span` s, whichever way it turns: speeding up at [`AIRCRAFT_SPEEDUP_MPS2`] to its domain's top
/// speed ([`top_speed_mps`]). Returns the speed it starts from, the speed it may reach by then,
/// and the distance.
pub fn aircraft_reach_m(speed: f64, span: f64) -> (f64, f64, f64) {
    let top = top_speed_mps(Domain::Air).max(speed);
    let a = AIRCRAFT_SPEEDUP_MPS2;
    let t1 = ((top - speed) / a).clamp(0.0, span.max(0.0));
    let reached = speed + a * t1;
    (speed, reached, speed * t1 + 0.5 * a * t1 * t1 + reached * (span.max(0.0) - t1))
}

/// Rules 6: the effect's altitude for a 3D keep-out: the target's estimate (MSL) with its
/// uncertainty at the effect time (`keep_out_sigma` of its altitude error, and how far it may
/// climb or descend by then), and how far above and below it the keep-out reaches (the
/// effect radius plus `keep_out_vertical_m`). None (the keep-out stays 2D, a cylinder of
/// unlimited height) before rules 6, without the target, or when its altitude is unknown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectAlt {
    pub alt_m: f64,
    pub margin_m: f64,
    pub half_height_m: f64,
}

/// Rules 6: a track's altitude where a 3D keep-out may use it, and when it was measured, or
/// None (2D). It must be recent (its own time, `alt_t`, at most `track_alt_max_age_s` before
/// `now`) and backed by a sensor that ranges the contact which detected it that recently
/// (`source_t`): radar for an aircraft (the sensing model gives an aircraft's height only from
/// a radar that measures elevation), radar or the vessel's own AIS report for a vessel (on the
/// surface its height is sea level). An altitude from passive sensors only (EO/IR, ESM: a
/// bearing, no range) does not count, nor one a radar measured long ago while EO/IR keeps
/// the position fresh. Never a ground track's (the sensing model gives ground contacts 0 m
/// whatever the terrain, and a height above the terrain is not one above mean sea level: a
/// terrain-referenced height is future work) nor a subsurface one's (sonar's fix is
/// bearing-heavy: no depth).
pub fn track_altitude(t: &TrackFacts, now: Time, ec: &EngageRules) -> Option<(f64, Time)> {
    let alt = t.alt_m.filter(|a| a.is_finite())?;
    let at = t.alt_t?;
    let max_ms = ms(ec.track_alt_max_age());
    if now.ms_since(at) > max_ms {
        return None;
    }
    let recent = |kind: &str| {
        t.source_t
            .iter()
            .any(|(s, st)| s.rsplit(':').next() == Some(kind) && now.ms_since(*st) <= max_ms)
    };
    let backed = match t.domain {
        Domain::Air => recent("radar"),
        Domain::Surface => recent("radar") || recent("ais"),
        Domain::Ground | Domain::Subsurface => false,
    };
    backed.then_some((alt, at))
}

/// See [`EffectAlt`].
pub fn effect_alt(
    target: Option<&TrackFacts>,
    now: Time,
    effect_t: Time,
    effect_radius_m: f64,
    ec: &EngageRules,
) -> Option<EffectAlt> {
    if !ec.v6() {
        return None;
    }
    let t = target?;
    let (alt_m, alt_t) = track_altitude(t, now, ec)?;
    // the margin grows from when the altitude was measured, not the position's time
    let span = secs(effect_t.ms_since(alt_t)).max(0.0);
    Some(EffectAlt {
        alt_m,
        margin_m: ec.keep_out_sigma * track_alt_sigma_m(t.domain)
            + track_vertical_mps(t.domain, ec) * span,
        half_height_m: effect_radius_m + ec.keep_out_vertical(),
    })
}

/// How far (m) something at `alt_m`, give or take `margin_m`, stays outside the keep-out above
/// or below the effect; None when its band reaches into the keep-out's.
fn vertical_clear(e: &EffectAlt, alt_m: f64, margin_m: f64) -> Option<f64> {
    let top = e.alt_m + e.margin_m + e.half_height_m;
    let bottom = e.alt_m - e.margin_m - e.half_height_m;
    let c = (alt_m - margin_m - top).max(bottom - (alt_m + margin_m));
    (c > 0.0).then_some(c)
}

/// Whether [`keep_out_tracks`] leaves track `id` out: the target, and other tracks an
/// operator declared hostile (an affiliation without a declaration is protected); rules 9: a
/// track declared by rule and not confirmed by an operator stays protected unless it is the
/// target (conservative).
pub fn track_exempt(id: &str, t: &TrackFacts, target: &str, ec: &EngageRules) -> bool {
    let declared_hostile = if ec.v9() {
        crate::ident::exempt_in_keep_out(t)
    } else {
        t.affiliation.value == Affiliation::Hostile && t.declared_by.is_some()
    };
    id == target || declared_hostile
}

/// Rules 6 on: how one protected track stands against a keep-out reaching `keep_out_m` from
/// `point` at `effect_t`, as [`keep_out_tracks`] judges each track it does not exempt: its
/// regions ([`track_reach`], or [`track_regions`] without `words`), the one whose edge is
/// nearest the point (the first of equals), and, when that edge is inside the keep-out,
/// whether it is clear by altitude (where the effect's altitude, `alt`, and its own recent
/// ranged altitude at `now` are both known). None when it has no region (never: every track
/// has at least one). Rules 16: a track out of view at `now` ([`out_of_view`]) also counts
/// anywhere at its domain's top speed since its last fix ([`track_reach_at`]).
#[allow(clippy::too_many_arguments)]
pub fn track_standing(
    t: &TrackFacts,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    now: Time,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
    words: bool,
) -> Option<TrackStanding> {
    standing(t, regions_at(t, effect_t, ec, words, out_of_view(t, now, ec)), point, effect_t, keep_out_m, now, ec, alt)
}

/// Rules 16: [`track_standing`] for a lost contact, by its regions as one ([`lost_reach`]):
/// always out of view.
#[allow(clippy::too_many_arguments)]
pub fn lost_track_standing(
    t: &TrackFacts,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    now: Time,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
    words: bool,
) -> Option<TrackStanding> {
    standing(t, regions_at(t, effect_t, ec, words, true), point, effect_t, keep_out_m, now, ec, alt)
}

/// How a track stands by `regions` (see [`track_standing`]).
#[allow(clippy::too_many_arguments)]
fn standing(
    t: &TrackFacts,
    regions: Vec<TrackReach>,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    now: Time,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
) -> Option<TrackStanding> {
    let (nearest, edge, d) = regions
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let d = haversine_m(r.center, point);
            (i, d - r.radius_m, d)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    let known = track_altitude(t, now, ec);
    let mut by_alt = None;
    // in 3D where both altitudes are known (recent and ranged), the margin growing from the
    // altitude's own time
    if edge < keep_out_m
        && let (Some(e), Some((a, alt_t))) = (alt, known)
    {
        let alt_span = secs(effect_t.ms_since(alt_t)).max(0.0);
        let m = ec.keep_out_sigma * track_alt_sigma_m(t.domain) + track_vertical_mps(t.domain, ec) * alt_span;
        if let Some(c) = vertical_clear(&e, a, m) {
            by_alt = Some((a, c, m));
        }
    }
    Some(TrackStanding {
        regions,
        nearest,
        d,
        edge,
        by_alt,
        alt_known: known.is_some(),
    })
}

/// See [`track_standing`].
#[derive(Clone, Debug, PartialEq)]
pub struct TrackStanding {
    pub regions: Vec<TrackReach>,
    /// The region whose edge is nearest the point (an index into `regions`).
    pub nearest: usize,
    /// Its centre's distance from the point, m.
    pub d: f64,
    /// Its edge's distance from the point (`d` less its radius; negative when the point is
    /// inside it), m. The keep-out holds for this track when it is at least `keep_out_m`.
    pub edge: f64,
    /// Inside the keep-out in 2D, but clear of it by altitude: its altitude, how far outside
    /// the vertical keep-out, and its altitude margin, m.
    pub by_alt: Option<(f64, f64, f64)>,
    /// Whether its altitude counted for a 3D keep-out (recent and ranged).
    pub alt_known: bool,
}

impl TrackStanding {
    /// Whether this track keeps a keep-out of `keep_out_m` from holding.
    pub fn blocks(&self, keep_out_m: f64) -> bool {
        self.edge < keep_out_m && self.by_alt.is_none()
    }
}

/// No track but the target and tracks an operator declared hostile (`declared_by` set) may be
/// inside the keep-out at `effect_t`, with `keep_out_sigma` of its predicted uncertainty:
/// neutral, unknown, suspect and assumed friend tracks are protected, a hostile affiliation
/// nobody declared too, stale ones too (their uncertainty has grown). Another declared
/// hostile is exempt like the target: an open design question (DECISIONS.md).
///
/// Rules 6: the uncertainty is [`track_prediction_sigma`] (before: the tracker's growth law,
/// `max(1, 0.3 x speed)` times the span, which made a fast track on a steady course block
/// kilometres away); a track without a velocity estimate could be anywhere it reaches at its
/// domain's top speed; and where the target's and the track's altitudes are both known the
/// keep-out has a ceiling and a floor ([`effect_alt`]).
#[allow(clippy::too_many_arguments)]
pub fn keep_out_tracks(
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    target: &str,
    world: &WorldSnapshot,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
) -> CheckResult {
    let r = RuleId::KeepOutTracks;
    let mut nearest: Option<(f64, &str)> = None;
    let mut count = 0;
    let mut by_alt: Vec<String> = Vec::new();
    // rules 19: the tracks accepted as behaving now whose acceptance holds on this picture
    let mut behaving: Vec<String> = Vec::new();
    for (id, t) in &world.tracks {
        if track_exempt(id, t, target, ec) {
            continue;
        }
        count += 1;
        if !ec.v6() {
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
            continue;
        }
        // rules 6: from its estimate, over the span since it (rules 9: a manoeuvring track
        // also anywhere it reaches at the fastest it went); the region nearest the point. Rules
        // 19: a track accepted as behaving now, while the acceptance holds on this picture, along
        // its accepted path and its course now
        let (standing, accepted_now, lapse) = live_standing(id, t, point, effect_t, keep_out_m, world, ec, alt, true);
        let Some(s) = standing else {
            continue;
        };
        if let Some(a) = accepted_now {
            behaving.push(format!("{id} by {} (on its course from t={})", a.by, a.facts.position.t.0));
        }
        let lapsed = lapse.map_or(String::new(), |l| format!("; its acceptance as behaving now lapsed: {}", l.detail));
        let edge = s.edge;
        if edge < keep_out_m {
            // in 3D where both altitudes are known (recent and ranged: track_altitude), the
            // margin growing from the altitude's own time
            if let Some((a, c, m)) = s.by_alt {
                by_alt.push(format!(
                    "{id} at {a:.0} m ({c:.0} m outside the vertical keep-out with {m:.0} m of altitude uncertainty)"
                ));
                continue;
            }
            let (d, how) = (s.d, &s.regions[s.nearest].how);
            // rules 16: a track out of view says so, and for how long
            let detail = if s.regions[s.nearest].kind == ReachKind::OutOfView {
                format!(
                    "track {} out of view (no detection for {:.0} s) could be {d:.0} m from the effect point at the effect time ({how}); keep-out {keep_out_m:.0} m{}",
                    describe_track(id, t),
                    secs(world.t.ms_since(t.last_seen_t)).max(0.0),
                    vertical_note(alt, s.alt_known)
                )
            } else if let Some(a) = accepted_now {
                // rules 19: an accepted track still in the way on its accepted path or its course
                format!(
                    "track {} accepted as behaving now by {} predicted {d:.0} m from the effect point at the effect time ({how}); keep-out {keep_out_m:.0} m{}",
                    describe_track(id, t),
                    a.by,
                    vertical_note(alt, s.alt_known)
                )
            } else {
                format!(
                    "track {} predicted {d:.0} m from the effect point at the effect time ({how}); keep-out {keep_out_m:.0} m{}",
                    describe_track(id, t),
                    vertical_note(alt, s.alt_known)
                )
            };
            return check(r, None, Outcome::Fail, format!("{detail}{lapsed}"));
        }
        if nearest.is_none_or(|(n, _)| edge < n) {
            nearest = Some((edge, id));
        }
    }
    // rules 16: the contacts the tracker dropped, anywhere they could have reached since their
    // last fix, unless an operator accepted exactly that loss for this engagement
    let mut lost_count = 0;
    let mut accepted: Vec<String> = Vec::new();
    if ec.lost() {
        for (id, l) in &world.lost_contacts {
            match lost_standing(id, l, point, effect_t, keep_out_m, target, world, ec, alt, true) {
                LostStanding::Exempt => {}
                LostStanding::Accepted { note } => accepted.push(note),
                LostStanding::Judged(s) => {
                    lost_count += 1;
                    if s.edge < keep_out_m {
                        if let Some((a, c, m)) = s.by_alt {
                            by_alt.push(format!(
                                "{id} at {a:.0} m ({c:.0} m outside the vertical keep-out with {m:.0} m of altitude uncertainty)"
                            ));
                            continue;
                        }
                        let (d, how) = (s.d, &s.regions[s.nearest].how);
                        return check(
                            r,
                            None,
                            Outcome::Fail,
                            format!(
                                "lost contact {} could be {d:.0} m from the effect point at the effect time ({how}); keep-out {keep_out_m:.0} m{}: {}",
                                describe_track(id, &l.facts),
                                vertical_note(alt, s.alt_known),
                                lost_note(id, l, world.t)
                            ),
                        );
                    }
                    if nearest.is_none_or(|(n, _)| s.edge < n) {
                        nearest = Some((s.edge, id));
                    }
                }
            }
        }
    }
    let near = nearest.map_or(String::new(), |(d, id)| format!("; nearest {id} at {d:.0} m"));
    let by_alt = if by_alt.is_empty() {
        String::new()
    } else {
        format!("; clear by altitude: {}", by_alt.join(", "))
    };
    let lost = if lost_count > 0 {
        format!(" and {lost_count} lost contact(s)")
    } else {
        String::new()
    };
    let accepted = if accepted.is_empty() {
        String::new()
    } else {
        format!("; lost contacts accepted for it: {}", accepted.join(", "))
    };
    let behaving = if behaving.is_empty() {
        String::new()
    } else {
        format!("; accepted as behaving now: {}", behaving.join(", "))
    };
    check(
        r,
        None,
        Outcome::Pass,
        format!("{count} protected track(s){lost} clear of the {keep_out_m:.0} m keep-out at the effect time{near}{by_alt}{accepted}{behaving}"),
    )
}

/// Rules 16: how a lost contact is out of view, for a check's detail and an operator to accept:
/// "T-0003 out of view since t=1781000010000 (300 s), dropped by the tracker at
/// t=1781000190000; an operator with engagement authority may accept it for this engagement".
pub fn lost_note(id: &str, l: &crate::lost::LostContact, now: Time) -> String {
    format!(
        "{id} out of view since t={} ({:.0} s), dropped by the tracker at t={}; an operator with engagement authority may accept it for this engagement",
        l.since().0,
        secs(now.ms_since(l.since())).max(0.0),
        l.dropped_t.0
    )
}

/// Rules 16: how one lost contact stands against a keep-out (see [`lost_standing`]).
#[derive(Clone, Debug, PartialEq)]
pub enum LostStanding {
    /// Exempt as a live track with its identification would be (the target, a declared
    /// hostile).
    Exempt,
    /// Lost since exactly the fix an operator accepted for this engagement: left out.
    Accepted { note: String },
    /// Judged by its regions as a track ([`track_standing`] on its facts at the drop).
    Judged(TrackStanding),
}

/// Rules 16: lost contact `id` (`l`) against a keep-out reaching `keep_out_m` from `point` at
/// `effect_t`, as [`keep_out_tracks`] reads it: left out when it is exempt (the target, a
/// declared hostile) or an operator accepted exactly this loss for the engagement
/// (`world.lost_accepted`); else its regions from its facts at the drop, growing from its last
/// fix ([`lost_reach`]: also anywhere at its domain's top speed; no cap). Judged even when its
/// id is in the picture: the bridge's tracker never reuses an id (`T-NNNN`, numbered up), so a
/// track under a dropped id is either a snapshot taken before the drop was handed to the engine,
/// with exactly the drop's facts (the same disc judged twice), or not the tracker's; it never
/// stands for the lost contact (the review of the merged branch: a fresh fix 30 km away under
/// the id hid it).
#[allow(clippy::too_many_arguments)]
pub fn lost_standing(
    id: &str,
    l: &crate::lost::LostContact,
    point: LonLat,
    effect_t: Time,
    keep_out_m: f64,
    target: &str,
    world: &WorldSnapshot,
    ec: &EngageRules,
    alt: Option<EffectAlt>,
    words: bool,
) -> LostStanding {
    if track_exempt(id, &l.facts, target, ec) {
        return LostStanding::Exempt;
    }
    if let Some(a) = world.lost_accepted.get(id)
        && a.since == Some(l.since())
    {
        return LostStanding::Accepted {
            note: format!(
                "{id}: out of view since t={} ({:.0} s), accepted by {}",
                l.since().0,
                secs(world.t.ms_since(l.since())).max(0.0),
                a.by
            ),
        };
    }
    match lost_track_standing(&l.facts, point, effect_t, keep_out_m, world.t, ec, alt, words) {
        Some(s) => LostStanding::Judged(s),
        // never: every track has a region
        None => LostStanding::Exempt,
    }
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
    let alt = effect_alt(world.tracks.get(track_id), world.t, p.effect_t, p.effect_radius_m, &ec);
    out.push(keep_out_friendly(
        p.effect_point,
        p.effect_t,
        p.keep_out_m,
        uid,
        world,
        cfg,
        alt,
    ));
    out.push(keep_out_tracks(
        p.effect_point,
        p.effect_t,
        p.keep_out_m,
        track_id,
        world,
        &ec,
        alt,
    ));
    out.push(armed_check(uid, world));
    if ec.v2() {
        out.push(free_check(uid, world));
    }
    if ec.v6()
        && let Some((_, _, _, fallback)) = params(spec)
    {
        out.push(preload_check(uid, fallback, world, &ec));
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
    accepted: &BTreeMap<Uid, AcceptedUnknown>,
    lost_accepted: &BTreeMap<String, AcceptedUnknown>,
    behaving_accepted: &BTreeMap<String, AcceptedBehaving>,
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
    // rules 6: the lost vehicles accepted for it, as the signer saw them
    if token.unknown_accepted != *accepted {
        return Err(
            "the lost vehicles accepted for it changed since you looked: look again".into(),
        );
    }
    // rules 16: and the lost contacts
    if token.lost_accepted != *lost_accepted {
        return Err("the lost contacts accepted for it changed since you looked: look again".into());
    }
    // rules 19: and the live tracks accepted as behaving now (one given, revoked or lapsed)
    if token.behaving_accepted != *behaving_accepted {
        return Err("the tracks accepted for it as behaving now changed since you looked: look again".into());
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
    let v = platform_of(spec).and_then(|u| world.vehicles.get(u));
    Ok(EngageSnapshot {
        t: world.t,
        track_id: track_id.to_string(),
        track: track.clone(),
        preview,
        report_interval_ms: v.and_then(|v| v.report_interval_ms),
        link_gap_limit_ms: link_gap_limit_ms(v, &ec),
        fallback,
    })
}

/// Hold the picture: the checks after approval against the snapshot, and (once dispatched)
/// whether the effect may be released now. Returns the results, whether the picture holds
/// (every picture check passes) and whether the release conditions are met too.
///
/// `prev_t` is when the picture was last checked (the last `engage.picture`, or the final
/// approval): a gap longer than `max_picture_gap_s` fails it, since nothing held it then.
/// `preloaded` (rules 6) is the lost-link preload and when the platform last confirmed it by
/// reading it back (`engage.preload` set or verify), none until then: the release needs a
/// read-back at most [`PRELOAD_FRESH_MS`] old.
pub fn picture(
    spec: &IntentSpec,
    snap: &EngageSnapshot,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    dispatched: bool,
    prev_t: Time,
    preloaded: Option<(&LostLinkPreload, Time)>,
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
            picture_checks.push(declared_check(id, t, &ec));
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
    let alt = effect_alt(track, world.t, effect_at, p.effect_radius_m, &ec);
    picture_checks.push(keep_out_friendly(
        p.effect_point,
        effect_at,
        p.keep_out_m,
        uid,
        world,
        cfg,
        alt,
    ));
    picture_checks.push(picture_keep_out_tracks(snap, world, cfg));
    picture_checks.push(link_check(uid, snap, world));
    picture_checks.push(nav_check(uid, world, cfg));
    picture_checks.push(window_check(snap, world, &ec, dispatched));
    let holds = picture_checks.iter().all(|c| c.outcome == Outcome::Pass);
    let mut out = picture_checks;
    let mut ready = false;
    if dispatched && holds {
        let mut release = release_checks(uid, snap, track, world, cfg);
        // rules 6: the fallback is the platform's own lost-link action, read back
        if ec.v6() {
            release.push(preloaded_check(uid, preloaded, world.t));
        }
        ready = release.iter().all(|c| c.outcome == Outcome::Pass);
        out.extend(release);
    }
    (out, holds, ready)
}

/// The keep-out of tracks an engagement in play is held to ([`picture`]'s, on `world`): rules
/// 9 also reads it to find the engagements a revoked declaration changes.
pub fn picture_keep_out_tracks(snap: &EngageSnapshot, world: &WorldSnapshot, cfg: &RulesConfig) -> CheckResult {
    let ec = cfg.engage_rules();
    let p = &snap.preview;
    let track = world.tracks.get(&snap.track_id);
    // the plan's time, or now once it has come
    let effect_at = if ec.v2() {
        p.effect_t.max(world.t)
    } else {
        p.effect_t
    };
    let alt = effect_alt(track, world.t, effect_at, p.effect_radius_m, &ec);
    keep_out_tracks(p.effect_point, effect_at, p.keep_out_m, &snap.track_id, world, &ec, alt)
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
        let alt = effect_alt(track, world.t, world.t, p.effect_radius_m, ec);
        out.push(keep_out_friendly(p.effect_point, world.t, p.keep_out_m, uid, world, cfg, alt));
        out.push(keep_out_tracks(
            p.effect_point,
            world.t,
            p.keep_out_m,
            &snap.track_id,
            world,
            ec,
            alt,
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
/// left out (cooperative, and the target may spoof it), ESM too (only emitters). Public for
/// the catalog's generator of the physical reaches (one per sensor and domain it sees).
pub fn sees(sensor: &str, domain: Domain) -> bool {
    match domain {
        Domain::Subsurface => sensor == "sonar",
        Domain::Surface => matches!(sensor, "radar" | "eo_ir" | "sonar"),
        Domain::Air | Domain::Ground => matches!(sensor, "radar" | "eo_ir"),
    }
}

/// The sensor of platform `p` that covers a contact in `domain` `d` metres away, for the
/// assessment: of the types that see the domain ([`sees`]), the one reaching farthest there, if
/// that is at least `d`. A sensor's reach is `range_m`, or in a run on the physical sensing
/// model the reach by domain the catalog derives from that model, which the facts then carry
/// (logged with the decision; [`crate::SensorFact::reach_for`]).
pub fn covering_sensor(p: &crate::PlatformFacts, domain: Domain, d: f64) -> Option<&SensorFact> {
    p.sensors
        .iter()
        .filter(|s| sees(&s.kind, domain))
        .filter_map(|s| s.reach_for(domain).map(|r| (s, r)))
        .filter(|(_, r)| *r >= d)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s)
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
        if let Some(s) = covering_sensor(p, domain, d) {
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
