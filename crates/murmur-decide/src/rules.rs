//! Rules (Phase C.2): pure functions of `(intent, &WorldSnapshot, &RulesConfig)`.
//!
//! Fail-closed: a rule that can't evaluate (vehicle unknown, fact missing or stale, area
//! file not loaded, terrain point unanswered) returns `Fail` and says why. Two
//! exceptions, both recorded in the intent's checks:
//! - `NotEvaluated`: deliberately skipped. The domain rule with no terrain oracle
//!   installed (PLAN.md), and an rtl whose return leg can't be checked (rtl is the safety
//!   command, as in the readiness exemption). Blocks only consequential kinds
//!   ([`TaskKind::is_consequential`], i.e. engage in Phase E).
//! - `Unverified`: the rule can't verify the condition, and a human must judge it: the
//!   reserves rule on a platform with no endurance figure. It doesn't block the rules
//!   gate, but policy never auto-approves it ([`crate::policy::decide`]).
//!
//! Output order is fixed (rule order, then the intent's uid order) and detail strings use
//! fixed precision, so recomputing a decision gives byte-identical results.
//!
//! Space kinds (collect, cover) target no vehicle, so the vehicle rules don't apply to them;
//! they get their own, which read only the logged space answer ([`crate::space`]):
//! `space_query` (the segment answered this intent's question), then `feasibility` for a
//! collect (an option is chosen and still feasible: pass in the future, lead time and
//! deadline met) or `coverage` for a cover. A rule set that produced no result at all
//! fails ("no rule evaluated"): an empty check list never passes.

use serde::{Deserialize, Serialize};

use crate::areas::Area;
use crate::catalog::{AutopilotKind, Domain};
use crate::engage::EngageRules;
use crate::geo::haversine_m;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Targets, Task, TaskKind};
use crate::route::{Segment, route_for};
use crate::space::{
    ChosenCollect, CoverService, SpaceAnswer, SpaceResult, SpaceRules, Window, fmt_time, fmt_utc,
    from_utc, matched_option, query_for,
};
use crate::terrain::Surface;
use crate::world::{Fact, NavSource, SpaceInputs, TerrainInputs, VehicleFacts, WorldSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleId {
    Capability,
    Readiness,
    PlatformLimits,
    Domain,
    Geofence,
    Reserves,
    /// Collect, cover: the space segment answered this intent's question.
    SpaceQuery,
    /// Collect: a chosen option that is still feasible.
    Feasibility,
    /// Cover: the service holds over the window.
    Coverage,
    // ---- engage (crate::engage) ----
    /// An operator declared the track hostile (an inferred suspect is not enough).
    TargetDeclared,
    /// Identity and existence confidence above the thresholds; after approval, not
    /// reclassified.
    TargetIdentity,
    /// The track's last detection is recent enough.
    TargetRecency,
    /// The track's 1-sigma uncertainty is within a fraction of the effect radius.
    TargetUncertainty,
    /// The predicted effect point is inside an approved engagement zone.
    EngagementZone,
    /// The effect circle is clear of every no-strike area.
    NoStrike,
    /// None of our vehicles can be inside the keep-out at the effect time.
    KeepOutFriendly,
    /// No protected track (anything but the target and tracks an operator declared
    /// hostile) inside the keep-out at the effect time.
    KeepOutTracks,
    /// The platform is armed (catalog).
    PlatformArmed,
    /// The platform can reach the effect point in the window, with the endurance to do it.
    Reach,
    /// After approval: the track is where the approval's snapshot predicts, within tolerance.
    TrackDeviation,
    /// After approval: the platform's link holds the approval's assumption.
    PlatformLink,
    /// After approval: the window is open and the platform is not late.
    EngageWindow,
    /// Release: the platform is at the effect point.
    Arrival,
    /// Release: the predicted effect time has come.
    EffectTime,
    /// Release: the track is estimated inside the effect radius.
    Alignment,
    /// After approval: the picture was checked recently enough (no stalled watch).
    PictureGap,
    /// The frozen plan still holds: the track is predicted within tolerance of the plan's
    /// effect point at the plan's time.
    Plan,
    /// The signatures on one plan are given within `max_signing_s` of the first.
    SigningTime,
    /// After approval: the platform navigates on GNSS (a release on a dead-reckoned position
    /// is not trusted).
    PlatformNav,
    /// The platform has no other engagement in play (one engagement per platform).
    PlatformFree,
    /// Rules 6: the platform's own lost-link action can be the engagement's fallback (at
    /// nomination), and is confirmed preloaded as it (at the release).
    FallbackPreload,
    /// Rules 21: an identify order's track is live and not identified yet, and the vehicle's
    /// EO/IR identifies a contact of its domain from an orbit around it ([`crate::identify`]).
    Identify,
}

impl RuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleId::Capability => "capability",
            RuleId::Readiness => "readiness",
            RuleId::PlatformLimits => "platform_limits",
            RuleId::Domain => "domain",
            RuleId::Geofence => "geofence",
            RuleId::Reserves => "reserves",
            RuleId::SpaceQuery => "space_query",
            RuleId::Feasibility => "feasibility",
            RuleId::Coverage => "coverage",
            RuleId::TargetDeclared => "target_declared",
            RuleId::TargetIdentity => "target_identity",
            RuleId::TargetRecency => "target_recency",
            RuleId::TargetUncertainty => "target_uncertainty",
            RuleId::EngagementZone => "engagement_zone",
            RuleId::NoStrike => "no_strike",
            RuleId::KeepOutFriendly => "keep_out_friendly",
            RuleId::KeepOutTracks => "keep_out_tracks",
            RuleId::PlatformArmed => "platform_armed",
            RuleId::Reach => "reach",
            RuleId::TrackDeviation => "track_deviation",
            RuleId::PlatformLink => "platform_link",
            RuleId::EngageWindow => "engage_window",
            RuleId::Arrival => "arrival",
            RuleId::EffectTime => "effect_time",
            RuleId::Alignment => "alignment",
            RuleId::PictureGap => "picture_gap",
            RuleId::Plan => "plan",
            RuleId::SigningTime => "signing_time",
            RuleId::PlatformNav => "platform_nav",
            RuleId::PlatformFree => "platform_free",
            RuleId::FallbackPreload => "fallback_preload",
            RuleId::Identify => "identify",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    /// Deliberately skipped (no terrain oracle; an rtl's unchecked return leg). Recorded;
    /// blocks consequential kinds only.
    NotEvaluated,
    /// Could not be verified, and a human must judge it (endurance unknown). Doesn't block
    /// the rules gate (except for consequential kinds); policy routes it to a human and
    /// never auto-approves it.
    Unverified,
}

/// A change that would make a failing intent pass. Suggested, never applied silently.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fix", rename_all = "snake_case")]
pub enum Fix {
    WidenRadius { radius_m: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub rule: RuleId,
    /// The vehicle this result is about (every rule today is per vehicle).
    pub uid: Option<Uid>,
    pub outcome: Outcome,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
    /// Rules 17: what a person must see though the check passed: a safety command (rtl) whose
    /// way home crosses land goes as ordered, and the bridge raises an inbox alert from this
    /// (`path_crosses_land`) while the order runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl CheckResult {
    pub(crate) fn new(rule: RuleId, uid: Option<&Uid>, outcome: Outcome, detail: impl Into<String>) -> Self {
        CheckResult {
            rule,
            uid: uid.cloned(),
            outcome,
            detail: detail.into(),
            fix: None,
            warning: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownEndurance {
    /// No endurance figure: the result is `Unverified` ("not evaluated: endurance
    /// unknown"), which policy routes to a human. A battery already below the reserve
    /// still fails.
    #[default]
    Human,
    /// No endurance figure: the reserves rule can't evaluate, so it fails.
    Fail,
    /// No endurance figure: pass if the battery is above the reserve now (a weaker check,
    /// stated in the detail).
    BatteryNow,
}

/// The version of the decision rules: bumped whenever a change alters a decision (a new
/// check, another threshold or model, another detail text), so replay recomputes every log
/// with the rules it was produced under. `run.started` records it; a log without one was
/// written by version 1. The changelog is in `crates/murmur-decide/README.md`. Version 4
/// changes no check: it logs the operator roster and the engagement order's loiter radius,
/// which replay then binds (a log claiming 3 or earlier can't carry them). Version 5 holds
/// orders for unreachable vehicles in a queue and re-checks them before dispatch
/// ([`crate::queue`]). Version 6 (the engagement part): the platform's lost-link action
/// preloaded as the fallback and confirmed before a release, the keep-outs' prediction
/// model, altitude and accepted unknown friendlies (`crate::engage`). Version 7
/// ([`QUEUE_FOLLOWUPS_SINCE`]): the queue's follow-ups to rules 5 and 6. Version 8 binds
/// every human act to an authenticated person ([`OPERATOR_AUTH_SINCE`]). Version 9
/// ([`RULE_ID_SINCE`]): rule-based identification with human review ([`crate::ident`]).
/// Version 10 ([`PASSKEY_SINCE`]): passkeys (WebAuthn) for logins and step-ups
/// ([`crate::passkey`]). Version 11 ([`SPACE_FOLLOWUPS_SINCE`]): satellite tasks cancelled
/// after dispatch, re-checked against a later space answer, and covers evaluated over the
/// area ([`crate::space`]). Version 12 ([`REVIEW_FOLLOWUPS_SINCE`]): the follow-ups of the
/// reviews of rules 6 to 10. Version 13 ([`SPACE_REPLAY_SINCE`]): when a space answer was
/// computed is logged and bound, a re-check is stamped at its guard's instant, and the pin
/// never changes mid-log. Version 14 ([`AIRCRAFT_REACH_SINCE`]): an aircraft counts in a
/// keep-out anywhere it could turn or speed up to. Version 15 ([`CARRY_FOLLOWUPS_SINCE`]): the
/// follow-ups of rules 12's carries. Version 16 ([`LOST_CONTACT_SINCE`]): a protected contact
/// the tracker drops stays in the keep-out as a lost contact ([`crate::lost`]). Version 17
/// ([`ROUTE_SURFACE_SINCE`]): the domain rule judges the legs a boat, sub or ground vehicle
/// travels, not only its waypoints. Version 18 ([`VELOCITY_SIGMA_SINCE`]): a protected track is
/// predicted with its own velocity error, as the tracker reports it. Version 19
/// ([`ACCEPT_BEHAVING_SINCE`]): an operator may accept a live track as it is behaving now, and
/// the acceptance lapses on its own when the track departs from it. Version 20
/// ([`MUNITION_FLIGHT_SINCE`]): a platform with a catalog munition releases it the time of flight
/// before the plan's time, from within the munition's range, and the effect lands at its impact.
/// Version 21 ([`IDENTIFY_SINCE`]): an operator (or an agent, by policy) may order a vehicle to
/// identify a track. Version 22 ([`ROUTE_PLAN_SINCE`]): a leg that meets the wrong surface or a
/// no-go area is routed around it before the order is refused; and ([`IDENTIFY_SECOND_LOOK_SINCE`])
/// an identify order may go to any live contact, for a fresh look by our own sensors. Version 23
/// ([`IDENTIFY_OWN_LOOK_SINCE`]): an identify order's look ends only on the vehicle sent's own
/// identification.
pub const RULES_VERSION: u32 = IDENTIFY_OWN_LOOK_SINCE;

/// The rules version from which a leg that the domain rule or the geofence would refuse is first
/// routed around what it meets (bd murmur-n6a, the owner's direction of 2026-10-27, DECISIONS.md
/// "Route planning before a refusal"): the transit to a task and a patrol's legs (its loop back
/// included) of a boat, sub or ground vehicle that cross the wrong surface, and of any vehicle that
/// cross a no-go area, get waypoints from a grid planner ([`crate::plan`]) on the same land/water
/// source the rule reads, logged with the decision (`route_plans`, the grid with them) so replay
/// plans again; the routed legs are judged by the same rules, and the order is refused only when
/// no route exists within the planner's bounds, saying why. A return home, an rtl and an
/// engagement's way stay straight (the autopilot's own, or a frozen plan's). Every gate of that
/// change reads this constant, never a literal: another change taking 22 first renumbers it here.
pub const ROUTE_PLAN_SINCE: u32 = 22;

/// The rules version from which an identify order may go to any live contact, one already
/// identified included (bd murmur-n6a's second part, the owner's direction of 2026-10-27): an AIS
/// identity is what a vessel says it is, and an identity our own sensors gave can be wrong, so a
/// second look is a real task. Such an order ends on a fresh identification by our own sensors,
/// made after the order went out (not the identity the contact already had), saying whether it
/// agrees with the identity before or contradicts it (a contradiction raises the tracker's identity
/// conflict and the review, as fusion does); a stale or lost contact is refused as before. Its own
/// constant, so it can move on its own.
pub const IDENTIFY_SECOND_LOOK_SINCE: u32 = 22;

/// The rules version from which an identify order's look ends identified only on an
/// identification by the vehicle sent, with its own sensors, made after the order went out (the
/// owner's Harbor watch run of 2026-10-27 17:11Z: "we didn't have perfect info and yet my ask to
/// inspect was canceled"; four identify orders there ended within a second or two on another
/// vehicle's camera or sonar re-identifying a contact it already held). Another vehicle's
/// identification meanwhile is logged as a step (`identify.step` meanwhile, once for each vehicle
/// and kind) and said in the outcome, and the order goes on; a lost track and the deadline end it
/// as before. The steps that say so name our vehicles by the callsigns they log
/// (`WorldSnapshot::callsigns`). Every gate reads this constant, never a literal.
pub const IDENTIFY_OWN_LOOK_SINCE: u32 = 23;

/// The rules version from which a vehicle may be ordered to identify a track (bd murmur-bia, the
/// owner's direction of 2026-10-27, DECISIONS.md "W1 follow-up: an identify order"): an intent
/// of kind `identify` names one track and one vehicle; the engine lays it out as an orbit around
/// the track at a standoff the vehicle's EO/IR identifies from ([`crate::identify::plan`], the
/// `identify` rule), judges that orbit as any orbit, moves it with the track (`identify.step`
/// recentred, logged before it goes out), and ends it once the track is identified, lost or out
/// of time, sending the vehicle back to the order it replaced (re-checked) or holding it. The
/// vehicle's camera looks for the track first while it is on the order. Earlier versions take no
/// identify order (the rule fails). Every gate of that change reads this constant, never a
/// literal: another change taking 21 first renumbers it in this one line.
pub const IDENTIFY_SINCE: u32 = 21;

/// The rules version from which a platform whose catalog gives it a munition (`munition = { name,
/// speed_mps, range_m }`, [`crate::catalog::MunitionFacts`]) releases it as a munition in flight
/// (bd murmur-64z, the owner's direction of 2026-10-26, DECISIONS.md "Munitions shown in flight
/// and strikes animated"): the munition flies a straight slant line from the platform's reported
/// position and altitude to the effect point at a constant speed, so the effect lands a time of
/// flight after the release ([`crate::engage::shot`]). The release needs the platform within the
/// munition's slant range of the effect point instead of `arrival_m` (the `arrival` check), and
/// the effect time, window and lateness, the target at the point (`alignment`) and the keep-outs
/// at the release are judged at the predicted impact, not now; the `engage.effect` logs the
/// munition's flight (name, launch point and altitude, time of flight, impact time, point and
/// altitude), which replay recomputes from the releasing picture's inputs; the assessment
/// observes from the impact on. A platform without one (a loitering munition is its own) decides
/// exactly as before. Every gate of that change reads this constant, never a literal: another
/// change taking 20 first renumbers it in this one line.
pub const MUNITION_FLIGHT_SINCE: u32 = 20;

/// The rules version from which an operator with engagement authority may accept one live track
/// that blocks an engagement's keep-out as it is behaving now (bd murmur-j0x, the owner's
/// decision B of 2026-10-26, option 2 of docs/ENGAGEMENT-IN-TRAFFIC.md): logged as
/// `engage.accepted` naming the track with its facts then, for that engagement only, at the
/// nomination or before a first signature, with the `accept_unknown` step-up, shown to every
/// signer, revocable by any operator, reset with the plan and never an agent's. The keep-out then
/// counts it along the path accepted for it and its course now
/// ([`crate::engage::behaving_regions`]), not anywhere it could turn or speed up to; the
/// acceptance lapses on its own (`engage.acceptance_lapsed`) when the track's estimate departs
/// from the accepted prediction beyond its uncertainty or it goes out of view
/// ([`crate::engage::behaving_lapse`]). Every gate of that change reads this constant, never a
/// literal: another change taking 19 first renumbers it in this one line.
pub const ACCEPT_BEHAVING_SINCE: u32 = 19;

/// The rules version from which a protected track's keep-out regions read the tracker's own
/// velocity error (murmur-vni, the owner's decision A of 2026-10-26): the region along its
/// velocity and the one along its average velocity grow with the velocity's 1-sigma the tracker
/// reports (`TrackFacts::velocity_sigma_mps`, the least certain direction's), or the policy's
/// `track_velocity_sigma_mps` when that is larger or none is given, and a young track's speed
/// bound adds `keep_out_sigma` of the same ([`crate::engage::track_velocity_error`]). Before, every
/// shown velocity was taken as good to the policy's 1 m/s, so the tracker could show a velocity
/// only once its error was near that (its 3 m/s gate), and aircraft fixed by ESM positions and 2D
/// radars' bearings, whose honest velocity error is 5 to 9 m/s, showed none. Every gate of that
/// change reads this constant, never a literal: another change taking 18 first renumbers it in
/// this one line.
pub const VELOCITY_SIGMA_SINCE: u32 = 18;

/// The rules version from which the domain rule judges the way a boat, a sub or a ground
/// vehicle goes, not only its waypoints (murmur-k8i, item 2): land or water every
/// [`crate::route::LEG_STEP_M`] along each straight leg of its route ([`crate::route`]: the
/// transit from where it is, the task path's legs, the return home; an engagement's transit to
/// its effect point and return; an rtl's way home), the answers logged one letter a point
/// (`along`, [`crate::world::LegSurfaces`]). A mission or a nomination that crosses the wrong
/// surface is refused, saying which leg and where; a leg may leave the wrong surface at the
/// vehicle and reach it at home within [`crate::route::LEG_ENDS_M`] (a pier). An rtl, the safety
/// command, is never refused for its way home: it passes with a `warning` (an inbox alert while
/// it runs), and one whose way can't be checked is not evaluated. Every gate of that change reads
/// this constant, never a literal: another change taking 17 first renumbers it in this one line.
pub const ROUTE_SURFACE_SINCE: u32 = 17;

/// The rules version from which the follow-ups of rules 12's carries decide (murmur-zq3,
/// murmur-imu): a platform spent by its own effect stays spent across a restart (`run.started`'s
/// `carried_from` carries each log's spent platforms, `spent`, and a decision's
/// `spent_carried` fact holds them for the vehicle that was spent); an operator with engagement
/// authority may un-spend a platform once the engagement that spent it ended and the platform
/// was heard since and is heard now, saying why and stepping up in a credentials run
/// (`platform.unspent`), which owes its own lost-link values back again; a nomination on a
/// platform a failed carry may owe values to is refused until that log is read or written off
/// (the engagement's `restore_owed` fact, the `fallback_preload` check); a `.last-run` pointer
/// that can't be read falls back to the station's newest log by the carry chain whose chain and
/// anchors check (`pointer_failed`, the station's `pointer` in `run.started`); a failed carry
/// records the fleet file it read (`fleet_read`: its hash and the armed platforms), which
/// replay compares when at hand; and a void that says it was superseded names an order replay
/// finds, in the state the reason says. Every gate of that change reads this constant, never a
/// literal: another change taking 15 first renumbers it in this one line.
pub const CARRY_FOLLOWUPS_SINCE: u32 = 15;

/// The rules version from which a contact the tracker drops is not forgotten (murmur-533,
/// [`crate::lost`]): the bridge hands the engine each track it drops with its last facts
/// (`track.dropped`); the engine keeps it as a lost contact, which every engagement keep-out
/// counts anywhere it could have reached since its last fix ([`crate::engage::lost_reach`]: a
/// live track's regions and anywhere at its domain's top speed; no cap) unless it is exempt
/// as a live track would be; an operator with engagement authority may accept one for one
/// engagement (`engage.accepted` naming the contact), every signer is shown it and replay
/// checks it; a live track carrying its AIS identity is a claim an operator confirms
/// (`lost.claimed`, `track.reacquired`), void once the claimant turns suspect, is declared or
/// drops (`lost.reinstated`). Every gate of that change reads this constant, never a literal:
/// another change taking 16 first renumbers it in this one line.
pub const LOST_CONTACT_SINCE: u32 = 16;

/// The rules version from which an aircraft with a velocity also counts in a keep-out anywhere
/// it reaches from its own speed bound, speeding up to its domain's top speed, whether or not the
/// tracker saw it turn (murmur-oho: calibrated against the truth, the air tracker gives a
/// straight-flying aircraft a velocity, and an aircraft turns through a right angle in 15 to
/// 30 s, so a keep-out along its course alone would no longer hold one that turns or speeds
/// up; [`crate::engage::track_reach`]). Every gate of that change reads this constant, never a
/// literal: another change taking 14 first renumbers it in this one line.
pub const AIRCRAFT_REACH_SINCE: u32 = 14;

/// The rules version from which replay holds a space answer to when it was computed and a
/// re-check to the instant it was judged at (murmur-der.4, murmur-lsj): every answered space
/// input logs the engine time its answer was computed at (`computed_t`, beside its hash), a
/// collect decides only on an answer computed within [`PREPARED_SPACE_MAX_AGE_MS`] of the
/// check (the `space_query` rule, so replay recomputes it) and asked at the scenario time of
/// that computation (outside a re-check, which keeps the tasking's request time); a re-check's
/// events are logged at the instant its "has the pass come" guard read; and the segment's
/// pinned time shift never changes between two `space.loaded` (the engine refuses a re-pinned
/// segment, replay flags one). Every gate of that change reads this constant, never a
/// literal: another change taking 13 first renumbers it in this one line.
pub const SPACE_REPLAY_SINCE: u32 = 13;

/// How far a collect's space answer may have been computed from the check that decides on
/// it, ms: the engine takes an answer prepared outside its lock only within this, and from
/// rules 13 ([`SPACE_REPLAY_SINCE`]) the `space_query` rule fails a collect whose logged answer
/// was computed farther from its check (a cover's question does not depend on when it was
/// asked, so its age is no concern).
pub const PREPARED_SPACE_MAX_AGE_MS: i64 = 5_000;

/// The rules version from which the follow-ups of the reviews of rules 6 to 10 decide: an
/// expended platform's lost-link values are closed with the reason and the platform is spent
/// (it takes no order and no nomination), and an operator may write off what is owed back
/// (`restore.closed`, murmur-q05); a young track counts as possibly manoeuvring in a keep-out
/// until the tracker has the facts that say otherwise (its `first_seen_t`, murmur-9m0); the
/// scripted operators are fixed at run start (the roster's `scripted`) and every session's
/// method is checked against them (murmur-jfi); the bridge records in `run.started` what it
/// carried over from earlier runs' logs, which logs, their heads and what each still owed,
/// in one line (`carried_from`, murmur-cq3); values carried over hold a mission for the
/// vehicle they were preloaded on (murmur-qjf); and a mission refused for its platform's own
/// values names the older queued orders still standing on its vehicles (murmur-54l). Every
/// gate of that change reads this constant, never a literal: another change taking 12 first
/// renumbers it in this one line.
pub const REVIEW_FOLLOWUPS_SINCE: u32 = 12;

/// The rules version from which a dispatched collect or cover is followed up (murmur-0nv.1.7):
/// an operator may cancel it after dispatch (`operator.action intent.cancel`, the engine's own
/// `op.state cancelled` saying what the cancel means for the pass, transition `cancel` to
/// `aborted`); a later space answer (the element sets reloaded) re-checks it after dispatch
/// (`check.result` phase `dispatched`, then the engine's `op.state rechecked`, or `missed`
/// and the intent failed); and a cover is evaluated at sample points over its area (the
/// centroid and the boundary: `SpaceQuery::CoverLinks`, `CoverGnss`), the service holding
/// only where it holds at every point. Every gate of that change reads this constant, never
/// a literal: another change taking 11 first renumbers it in this one line.
pub const SPACE_FOLLOWUPS_SINCE: u32 = 11;

/// The rules version from which a login or a step-up may be a passkey's (murmur-437): the
/// run logs its relying party and its operators' passkeys as public keys (the roster's
/// `passkeys`), each use logs the assertion with what it authorizes (`passkey` on
/// `session.opened` and `operator.action step_up`, `auth: passkey`), its challenge derived from
/// the run, the session and the act, and the act that quotes a passkey step-up must be the
/// act it was made for; registrations and removals during the run are logged
/// (`passkey.registered`, `passkey.removed`); the policy may require passkey step-ups
/// (`[approval] step_up_factor`) and say how operators sign in (`login_factor`); a study run
/// names its participant (the roster's `study`) and requires the participant's passkey at
/// every step-up unless an explicit override, logged, allows a password. Every gate of that
/// change reads this constant, never a literal: another change taking 10 first renumbers it
/// in this one line.
pub const PASSKEY_SINCE: u32 = 10;

/// The rules version from which identification is the engine's ledger and may be made by
/// rule ([`crate::ident`]): operators' declarations, confirmations and revocations go through
/// the engine (`ident.*` events), the hostile-act rule declares a track hostile when an
/// observed act against ours is attributed to it above the policy's threshold and nothing
/// conflicts, unsure cases are flagged for review, every decision's track picture carries the
/// ledger's declarations with their basis, a rule-declared and unconfirmed track stays
/// protected in another engagement's keep-out, and a revocation stops the engagements on its
/// track. Every change of that version is gated on this constant, never on a literal, so it
/// renumbers in this one line.
pub const RULE_ID_SINCE: u32 = 9;

/// The rules version from which the queue's follow-ups to rules 5 and 6 decide (murmur-85l,
/// murmur-dma, murmur-636): a mission waits for its platform's own lost-link values to be
/// confirmed back after an engagement preloaded them (`restore_owed`), a vehicle start is
/// logged ahead of sending it (`op.state` starting, `ahead`), and an order queued for an
/// engagement's platform supersedes the engagement as a dispatched one does. Every change of
/// that version is gated on this constant, never on a literal, so the version can be
/// renumbered in this one line if another change takes the number first (provisionally 7).
pub const QUEUE_FOLLOWUPS_SINCE: u32 = 7;

/// The rules version from which operators are authenticated people, not asserted names
/// (murmur-der.1): `run.started` records how identities were established (the roster's
/// `auth`: `credentials`, per-person passwords checked by the bridge, or `dev`, identities
/// asserted by a header and never evidence about people); every session says how it was
/// opened (`auth`: `password`, `scripted` or `asserted`) and when it ended
/// (`session.closed`); a signature the policy marks consequential (`step_up`: every
/// engagement, and any human tier with `step_up = true`), and any act `[approval]
/// step_up_acts` lists, quotes a step-up, the operator's credential re-entered at that moment
/// (`operator.action step_up`), in a credentials run; such a tier is never approved at
/// submission. Every gate of that change reads this constant, never a literal, so it can be
/// renumbered in one line when another change takes the number first.
pub const OPERATOR_AUTH_SINCE: u32 = 8;

/// The version of the policy file's schema: bumped when a key is added, renamed or removed.
/// A renamed or removed key stays an optional field of its own, so an old log both parses
/// and re-serializes byte for byte; `run.started` records it (absent: 1), and replay checks
/// that the logged policy uses no key of a later schema ([`crate::EngageRules::schema_keys`]).
pub const POLICY_SCHEMA: u32 = PASSKEY_SCHEMA_SINCE;

/// The policy schema that adds `[approval] step_up_factor` and `login_factor` (with
/// [`PASSKEY_SINCE`]): named so that a branch merging beside another schema bump renumbers it
/// in one line.
pub const PASSKEY_SCHEMA_SINCE: u32 = 7;

/// The policy schema that added a tier's `step_up` key and `[approval] step_up_acts` (with
/// [`OPERATOR_AUTH_SINCE`]).
pub const STEP_UP_SCHEMA_SINCE: u32 = 5;

/// The policy schema that adds `[identification]` (with [`RULE_ID_SINCE`]): named so that a
/// branch merging beside another schema bump renumbers it in one line.
pub const RULE_ID_SCHEMA_SINCE: u32 = 6;

fn current_rules_version() -> u32 {
    RULES_VERSION
}

/// Rule settings, from `[rules]` in `sim/policy.toml`. Logged with the policy at run start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RulesConfig {
    /// The rules version the checks run under: [`RULES_VERSION`] for a run, the logged one
    /// in a replay. Not a policy key, and not logged with the policy (`run.started` carries
    /// it on its own).
    #[serde(skip, default = "current_rules_version")]
    pub rules_version: u32,
    /// A fact older than this (against the snapshot time) can't be relied on.
    pub max_fact_age_s: u32,
    /// Link-aware freshness: a vehicle's limit is the larger of `max_fact_age_s` and this many
    /// of its expected reporting intervals (3 x 20 s = 60 s on an acoustic link). Absent: no
    /// scaling, `max_fact_age_s` for everyone (the behaviour before the key existed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fact_age_intervals: Option<u32>,
    /// Orbit radius must be at least this times the minimum turn radius.
    pub orbit_radius_margin: f64,
    /// For fixed-wing platforms with no catalog turn figures: ArduPlane's WP_LOITER_RAD default.
    pub fixed_wing_default_min_turn_radius_m: f64,
    /// Endurance left after the task, percent of full endurance.
    pub reserve_pct: f64,
    /// Time on task assumed by the reserves estimate (orbits and patrols loop until told
    /// otherwise, so the estimate needs a horizon).
    pub task_duration_s: u32,
    pub unknown_endurance: UnknownEndurance,
    /// Space rules (`[rules.space]`); defaults when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceRules>,
    /// Engagement rules (`[rules.engage]`, Phase E); defaults when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engage: Option<EngageRules>,
    /// Rules 9: the policy's `[identification]` (a top-level section), copied here so the
    /// engagement checks read it (whether a rule-declared target may be nominated before a
    /// confirmation). Not a key of `[rules]` and not logged here: [`crate::Policy::sync`] sets
    /// it from the policy (the engine and replay call it).
    #[serde(skip)]
    pub identification: crate::ident::IdentConfig,
}

impl RulesConfig {
    /// Freshness limit in ms for a vehicle reporting every `interval_ms` (None: unknown), and
    /// whether the link-aware part set it.
    pub fn fact_age_limit_ms(&self, interval_ms: Option<u32>) -> (i64, bool) {
        let base = i64::from(self.max_fact_age_s) * 1000;
        match (self.fact_age_intervals, interval_ms) {
            (Some(n), Some(i)) if i64::from(n) * i64::from(i) > base => {
                (i64::from(n) * i64::from(i), true)
            }
            _ => (base, false),
        }
    }

    /// `[rules.space]`, or its defaults, under this configuration's rules version.
    pub fn space_rules(&self) -> SpaceRules {
        let mut s = self.space.clone().unwrap_or_default();
        s.rules_version = self.rules_version;
        s
    }

    /// Rules 11: space tasks are followed up after dispatch and covers evaluated over the
    /// area ([`SPACE_FOLLOWUPS_SINCE`]).
    pub fn space_followups(&self) -> bool {
        self.rules_version >= SPACE_FOLLOWUPS_SINCE
    }

    /// Rules 9: identification is the engine's ledger (and may be made by rule).
    pub fn rule_id(&self) -> bool {
        self.rules_version >= RULE_ID_SINCE
    }

    /// Rules 10: logins and step-ups may be passkeys' ([`crate::passkey`]).
    pub fn passkeys(&self) -> bool {
        self.rules_version >= PASSKEY_SINCE
    }

    /// Rules 12 ([`REVIEW_FOLLOWUPS_SINCE`]): the follow-ups of the reviews of rules 6 to 10.
    pub fn followups(&self) -> bool {
        self.rules_version >= REVIEW_FOLLOWUPS_SINCE
    }

    /// Rules 13 ([`SPACE_REPLAY_SINCE`]): a space answer's computation time logged and bound,
    /// a re-check stamped at its guard's instant, no pin change mid-log.
    pub fn space_replay(&self) -> bool {
        self.rules_version >= SPACE_REPLAY_SINCE
    }

    /// Rules 15 ([`CARRY_FOLLOWUPS_SINCE`]): the follow-ups of rules 12's carries.
    pub fn carry_followups(&self) -> bool {
        self.rules_version >= CARRY_FOLLOWUPS_SINCE
    }

    /// Rules 16 ([`LOST_CONTACT_SINCE`]): a dropped track stays a lost contact.
    pub fn lost_contacts(&self) -> bool {
        self.rules_version >= LOST_CONTACT_SINCE
    }

    /// Rules 17 ([`ROUTE_SURFACE_SINCE`]): the domain rule judges a route's legs.
    pub fn route_surface(&self) -> bool {
        self.rules_version >= ROUTE_SURFACE_SINCE
    }

    /// Rules 18 ([`VELOCITY_SIGMA_SINCE`]): a protected track is predicted with its own velocity
    /// error.
    pub fn velocity_sigma(&self) -> bool {
        self.rules_version >= VELOCITY_SIGMA_SINCE
    }

    /// Rules 19 ([`ACCEPT_BEHAVING_SINCE`]): a live track may be accepted as behaving now.
    pub fn accept_behaving(&self) -> bool {
        self.rules_version >= ACCEPT_BEHAVING_SINCE
    }

    /// Rules 20 ([`MUNITION_FLIGHT_SINCE`]): a platform's catalog munition flies to the effect
    /// point, released its time of flight early.
    pub fn munition_flight(&self) -> bool {
        self.rules_version >= MUNITION_FLIGHT_SINCE
    }

    /// Rules 21 ([`IDENTIFY_SINCE`]): a vehicle may be ordered to identify a track.
    pub fn identify(&self) -> bool {
        self.rules_version >= IDENTIFY_SINCE
    }

    /// Rules 22 ([`ROUTE_PLAN_SINCE`]): a leg is routed around the wrong surface or a no-go area
    /// before it is refused.
    pub fn route_plan(&self) -> bool {
        self.rules_version >= ROUTE_PLAN_SINCE
    }

    /// Rules 22 ([`IDENTIFY_SECOND_LOOK_SINCE`]): an identify order may go to any live contact,
    /// and ends on a fresh identification by our own sensors.
    pub fn identify_second_look(&self) -> bool {
        self.rules_version >= IDENTIFY_SECOND_LOOK_SINCE
    }

    /// Rules 23 ([`IDENTIFY_OWN_LOOK_SINCE`]): an identify order's look ends identified only on
    /// the vehicle sent's own identification.
    pub fn identify_own_look(&self) -> bool {
        self.rules_version >= IDENTIFY_OWN_LOOK_SINCE
    }

    /// `[rules.engage]`, or its defaults.
    pub fn engage_rules(&self) -> EngageRules {
        let mut e = self.engage.clone().unwrap_or_default();
        e.rules_version = self.rules_version;
        e.nominate_unconfirmed = self.identification.nominate_unconfirmed();
        e
    }
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            rules_version: RULES_VERSION,
            max_fact_age_s: 10,
            fact_age_intervals: None,
            orbit_radius_margin: 1.1,
            fixed_wing_default_min_turn_radius_m: 60.0,
            reserve_pct: 20.0,
            task_duration_s: 900,
            unknown_endurance: UnknownEndurance::Human,
            space: None,
            engage: None,
            identification: crate::ident::IdentConfig::default(),
        }
    }
}

/// All rules, in order.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let mut out = Vec::new();
    match spec.task.targets() {
        Targets::Vehicles if spec.kind() == TaskKind::Identify => {
            // rules 21: the identify rule, then the vehicle rules on the orbit it is flown as
            // (none when there is no plan: the vehicle's own readiness alone)
            out.extend(capability(spec, world));
            out.push(crate::identify::check(spec, world, cfg));
            match crate::identify::orbit_view(spec, world, cfg) {
                std::borrow::Cow::Owned(orbit) => {
                    out.extend(readiness(&orbit, world, cfg));
                    out.extend(platform_limits(&orbit, world, cfg));
                    out.extend(domain(&orbit, world, cfg));
                    out.extend(geofence(&orbit, world, cfg));
                    out.extend(reserves(&orbit, world, cfg));
                }
                std::borrow::Cow::Borrowed(_) => out.extend(readiness(spec, world, cfg)),
            }
        }
        Targets::Vehicles => {
            out.extend(capability(spec, world));
            out.extend(readiness(spec, world, cfg));
            out.extend(platform_limits(spec, world, cfg));
            out.extend(domain(spec, world, cfg));
            out.extend(geofence(spec, world, cfg));
            out.extend(reserves(spec, world, cfg));
            // engage: the engagement checks (crate::engage), after the vehicle rules
            out.extend(crate::engage::evaluate(spec, world, cfg));
        }
        Targets::Space => {
            out.extend(space_query(spec, world, cfg));
            out.extend(feasibility(spec, world, cfg));
            out.extend(coverage(spec, world, cfg));
        }
    }
    if out.is_empty() {
        out.push(CheckResult::new(
            RuleId::Capability,
            None,
            Outcome::Fail,
            "no rule evaluated (no target)",
        ));
    }
    out
}

/// Whether a set of results lets the intent proceed.
pub fn passed(results: &[CheckResult], kind: TaskKind) -> bool {
    results.iter().all(|r| match r.outcome {
        Outcome::Pass => true,
        Outcome::Fail => false,
        Outcome::NotEvaluated | Outcome::Unverified => !kind.is_consequential(),
    })
}

/// The results a human must judge (outcome `Unverified`), as `"rule uid: detail"`.
pub fn unverified(results: &[CheckResult]) -> Vec<String> {
    results
        .iter()
        .filter(|r| r.outcome == Outcome::Unverified)
        .map(|r| match &r.uid {
            Some(u) => format!("{} {u}: {}", r.rule.as_str(), r.detail),
            None => format!("{}: {}", r.rule.as_str(), r.detail),
        })
        .collect()
}

fn per_vehicle(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    rule: RuleId,
    f: impl Fn(&Uid, &VehicleFacts) -> CheckResult,
) -> Vec<CheckResult> {
    spec.uids
        .iter()
        .map(|uid| match world.vehicles.get(uid) {
            Some(v) => f(uid, v),
            None => CheckResult::new(
                rule,
                Some(uid),
                Outcome::Fail,
                "vehicle not in the world model",
            ),
        })
        .collect()
}

/// A fact that exists and is fresh enough, or why not.
pub(crate) fn fresh<'a, T>(
    fact: &'a Option<Fact<T>>,
    v: &VehicleFacts,
    what: &str,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Result<&'a T, String> {
    let f = fact
        .as_ref()
        .ok_or_else(|| format!("cannot evaluate: no {what} reported"))?;
    let age_ms = world.t.ms_since(f.t);
    let (max_ms, scaled) = cfg.fact_age_limit_ms(v.report_interval_ms);
    if age_ms > max_ms {
        let why = match (scaled, v.report_interval_ms) {
            (true, Some(i)) => format!(": link reports every {} s", f64::from(i) / 1000.0),
            _ => String::new(),
        };
        return Err(format!(
            "cannot evaluate: {what} is {:.1} s old (limit {} s{why})",
            age_ms as f64 / 1000.0,
            max_ms as f64 / 1000.0
        ));
    }
    Ok(&f.value)
}

fn is_safety_command(kind: TaskKind) -> bool {
    matches!(kind, TaskKind::Hold | TaskKind::Rtl)
}

/// The platform kind supports the task (the KINDS table), the platform is in the catalog,
/// and a track target exists.
pub fn capability(spec: &IntentSpec, world: &WorldSnapshot) -> Vec<CheckResult> {
    let r = RuleId::Capability;
    let kind = spec.kind();
    per_vehicle(spec, world, r, |uid, v| {
        let fail = |d: String| CheckResult::new(r, Some(uid), Outcome::Fail, d);
        let Some(platform) = &v.platform else {
            return fail("cannot evaluate: platform not in the catalog".into());
        };
        let Some(ap) = &v.autopilot else {
            return fail("cannot evaluate: autopilot type unknown (no heartbeat yet)".into());
        };
        let ap = ap.value;
        if !ap.supports(kind) {
            let list: Vec<_> = ap.tasks().iter().map(|k| k.as_str()).collect();
            let can = if list.is_empty() {
                "nothing".to_string()
            } else {
                list.join(", ")
            };
            return fail(format!(
                "{} ({}) can't {}; supports: {}",
                platform.id,
                ap.as_str(),
                kind.as_str(),
                can
            ));
        }
        if let Task::Track { target } = &spec.task
            && !world.vehicles.contains_key(target)
        {
            return fail(format!("track target {target} is not in the world model"));
        }
        CheckResult::new(
            r,
            Some(uid),
            Outcome::Pass,
            format!("{} supports {}", ap.as_str(), kind.as_str()),
        )
    })
}

/// Navigation is ready and the status is fresh. Hold and RTL are exempt: they are the
/// safety commands, and the autopilot applies its own checks to them. On dead reckoning (no
/// GNSS fix) a sub passes, a fixed-wing (plane, VTOL) is `Unverified` (a human judges it),
/// anything else fails.
pub fn readiness(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let r = RuleId::Readiness;
    let kind = spec.kind();
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        // rules 12: a platform spent by its own effect takes no order, a safety command
        // included, and no nomination (it carries the engagement's lost-link action, and a new
        // preload would read that action as its own values)
        if cfg.followups()
            && let Some(id) = world.expended.get(uid)
        {
            return res(
                Outcome::Fail,
                format!("spent by its own effect (engagement {id}): a spent platform takes no order"),
            );
        }
        // rules 15: spent in an earlier run (carried over at start), the same vehicle unless it
        // is known to be another
        if cfg.carry_followups()
            && let Some(s) = world.spent_carried.get(uid).filter(|s| s.holds())
        {
            return res(Outcome::Fail, s.detail());
        }
        if is_safety_command(kind) {
            return res(Outcome::Pass, format!("not required for {}", kind.as_str()));
        }
        match fresh(&v.nav_ready, v, "navigation status", world, cfg) {
            Err(e) => return res(Outcome::Fail, e),
            Ok(false) => return res(Outcome::Fail, "navigation not ready (GPS/EKF)".into()),
            Ok(true) => {}
        }
        // dead reckoning (no GNSS) is how a submerged AUV navigates. A fixed-wing flies on
        // on airspeed dead reckoning (ArduPlane has no EKF failsafe for it), so it stays
        // taskable, out of a jammer for one, but a human judges a position that drifts.
        // Anyone else needs a fix (their EKF gives up the position, as ArduPilot's does).
        let dead_reckoning =
            v.nav_source.as_ref().map(|f| f.value) == Some(NavSource::DeadReckoning);
        let ap = v.autopilot.as_ref().map(|f| f.value);
        let is_sub = ap == Some(AutopilotKind::Sub);
        let fixed_wing = ap.is_some_and(AutopilotKind::is_fixed_wing);
        if dead_reckoning && !is_sub && !fixed_wing {
            return res(
                Outcome::Fail,
                "navigation on dead reckoning only (no GNSS fix)".into(),
            );
        }
        if let Task::Track { target } = &spec.task {
            let tv = world.vehicles.get(target);
            match tv.map(|t| fresh(&t.position, t, "target position", world, cfg)) {
                None => return res(Outcome::Fail, format!("track target {target} unknown")),
                Some(Err(e)) => return res(Outcome::Fail, e),
                Some(Ok(_)) => {}
            }
        }
        if dead_reckoning && fixed_wing {
            return res(
                Outcome::Unverified,
                "navigation on dead reckoning (no GNSS fix): position error grows with time".into(),
            );
        }
        if dead_reckoning {
            return res(
                Outcome::Pass,
                "navigation ready (dead reckoning, no GNSS fix: position error grows with distance)"
                    .into(),
            );
        }
        res(Outcome::Pass, "navigation ready".into())
    })
}

/// Orbit radius against the platform's minimum turn radius. Returns a suggested radius
/// instead of widening silently (the Python bridge widened silently).
pub fn platform_limits(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Vec<CheckResult> {
    let r = RuleId::PlatformLimits;
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        let Task::Orbit { radius_m, .. } = &spec.task else {
            return res(
                Outcome::Pass,
                format!("no platform limit applies to {}", spec.kind().as_str()),
            );
        };
        let (Some(platform), Some(ap)) = (&v.platform, &v.autopilot) else {
            return res(
                Outcome::Fail,
                "cannot evaluate: platform or autopilot unknown".into(),
            );
        };
        let (min_r, why) = match platform.min_turn_radius_m {
            Some(m) => (m, "minimum turn radius"),
            None if ap.value.is_fixed_wing() => (
                cfg.fixed_wing_default_min_turn_radius_m,
                "default fixed-wing loiter radius",
            ),
            None => {
                return res(
                    Outcome::Pass,
                    format!("{} has no minimum turn radius", ap.value.as_str()),
                );
            }
        };
        let need = (min_r * cfg.orbit_radius_margin).ceil();
        if *radius_m >= need {
            res(
                Outcome::Pass,
                format!(
                    "radius {radius_m:.0} m >= {need:.0} m ({why} {min_r:.0} m x {})",
                    cfg.orbit_radius_margin
                ),
            )
        } else {
            let mut c = res(
                Outcome::Fail,
                format!(
                    "radius {radius_m:.0} m is below {need:.0} m ({why} {min_r:.0} m x {})",
                    cfg.orbit_radius_margin
                ),
            );
            c.fix = Some(Fix::WidenRadius { radius_m: need });
            c
        }
    })
}

/// Which surface a platform's waypoints must be on, if any.
pub fn required_surface(domain: Domain, autopilot: Option<AutopilotKind>) -> Option<&'static str> {
    match domain {
        Domain::Surface | Domain::Subsurface => Some("water"),
        Domain::Ground if autopilot != Some(AutopilotKind::Tracker) => Some("land"),
        _ => None,
    }
}

/// Whether the engine needs to ask the terrain oracle for this intent.
pub fn needs_terrain(spec: &IntentSpec, world: &WorldSnapshot) -> bool {
    spec.task.path().is_some()
        && spec.uids.iter().any(|u| {
            world
                .vehicles
                .get(u)
                .and_then(|v| v.platform.as_ref())
                .is_some_and(|p| required_surface(p.domain, p.autopilot).is_some())
        })
}

/// Boats and subs keep to water, UGVs to land: up to rules 16 their waypoints; from rules 17
/// ([`ROUTE_SURFACE_SINCE`]) the waypoints and every leg of the route ([`domain_route`]).
pub fn domain(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    if cfg.route_surface() {
        return domain_route(spec, world, cfg);
    }
    domain_waypoints(spec, world)
}

/// Rules 17: the waypoints as before (each answered exactly), then the points along every
/// straight leg of each vehicle's route ([`crate::route::Route::legs`], [`crate::route::along`]),
/// in travel order: the transit from its position, the task path's legs, the return home (an
/// engagement's transit to its effect point and return; an rtl's way home). The first point on
/// the wrong surface refuses the order, naming the leg, the point and how far along it is; a leg
/// that starts at the vehicle may leave the wrong surface within [`crate::route::LEG_ENDS_M`],
/// and one that ends at home may reach it there (a pier, a slipway, a grid edge at the berth).
/// A hold or an rtl, the safety commands, are never refused for their way: an rtl whose way home
/// crosses the wrong surface passes with a warning (the bridge alerts on it while it runs), and
/// one whose way can't be checked is not evaluated.
fn domain_route(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    use crate::route::{LEG_ENDS_M, LEG_STEP_M, along};
    let r = RuleId::Domain;
    let kind = spec.kind();
    let safety = is_safety_command(kind);
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        let not_checked = |why: String| {
            if safety {
                res(
                    Outcome::NotEvaluated,
                    format!("path not checked ({} is a safety command): {why}", kind.as_str()),
                )
            } else {
                res(Outcome::Fail, why)
            }
        };
        let Some(platform) = &v.platform else {
            return res(
                Outcome::Fail,
                "cannot evaluate: platform not in the catalog".into(),
            );
        };
        let Some(need) = required_surface(platform.domain, platform.autopilot) else {
            return res(
                Outcome::Pass,
                "no surface constraint for this domain".into(),
            );
        };
        let route = match route_for(spec, v, world, cfg) {
            Ok(route) => route,
            Err(e) => return not_checked(e),
        };
        if route.is_empty() {
            return res(Outcome::Pass, format!("no path for {}", kind.as_str()));
        }
        match &world.terrain {
            TerrainInputs::Unavailable { reason } => {
                return res(
                    Outcome::NotEvaluated,
                    format!("terrain unavailable: {reason}"),
                );
            }
            TerrainInputs::NotQueried | TerrainInputs::NotNeeded => {
                return not_checked("cannot evaluate: terrain was not sampled".into());
            }
            TerrainInputs::Sampled { .. } => {}
        }
        let is = if need == "water" { "not water" } else { "not land" };
        // the waypoints, each answered exactly, as up to rules 16
        let waypoints = route.task.as_ref().map_or(0, |p| p.points.len());
        if let Some(path) = &route.task {
            for (i, p) in path.points.iter().enumerate() {
                let at = format!("waypoint {} ({:.5}, {:.5})", i + 1, p.lon, p.lat);
                let ok = match world.terrain.lookup(*p) {
                    None => {
                        return res(
                            Outcome::Fail,
                            format!("cannot evaluate: no terrain sample for {at}"),
                        );
                    }
                    Some(Surface::OutsideCoverage) => {
                        return res(
                            Outcome::Fail,
                            format!("cannot evaluate: {at} is outside terrain coverage"),
                        );
                    }
                    Some(Surface::Land) => need == "land",
                    Some(Surface::Water { depth_m }) => need == "water" && *depth_m > 0.0,
                };
                if !ok {
                    return res(
                        Outcome::Fail,
                        format!("{at} is {is}; {} needs {need}", platform.id),
                    );
                }
            }
        }
        // the legs, in travel order
        let good = if need == "water" { 'w' } else { 'l' };
        let mut points = 0usize;
        let mut ends: Vec<String> = Vec::new();
        for leg in route.legs() {
            let Some(pts) = along(leg.from, leg.to) else {
                return not_checked(format!(
                    "cannot evaluate: the {} is {:.0} km, longer than the {:.0} km the rule samples",
                    leg.name,
                    haversine_m(leg.from, leg.to) / 1000.0,
                    crate::route::LEG_MAX_M / 1000.0
                ));
            };
            if pts.is_empty() {
                continue;
            }
            let Some(answers) = world.terrain.along_for(leg.from, leg.to) else {
                return not_checked(format!("cannot evaluate: no terrain answers along the {}", leg.name));
            };
            let letters: Vec<char> = answers.surfaces.chars().collect();
            if letters.len() != pts.len() {
                return not_checked(format!(
                    "cannot evaluate: {} terrain answers along the {}, which has {} points",
                    letters.len(),
                    leg.name,
                    pts.len()
                ));
            }
            points += pts.len();
            let len = haversine_m(leg.from, leg.to);
            // a leg from the vehicle may leave the wrong surface it starts on, and one to home
            // may reach it there, each within LEG_ENDS_M and only before (after) the right one
            let first_good = letters.iter().position(|c| *c == good).unwrap_or(letters.len());
            let last_good = letters.iter().rposition(|c| *c == good);
            for (i, ((p, d), c)) in pts.iter().zip(&letters).enumerate() {
                if *c == good {
                    continue;
                }
                if leg.departs && i < first_good && *d <= LEG_ENDS_M {
                    let e = format!("the {} starts off {need} within {LEG_ENDS_M:.0} m of the vehicle", leg.name);
                    if !ends.contains(&e) {
                        ends.push(e);
                    }
                    continue;
                }
                if leg.arrives_home && last_good.is_none_or(|g| i > g) && len - d <= LEG_ENDS_M {
                    let e = format!("the {} ends off {need} within {LEG_ENDS_M:.0} m of home", leg.name);
                    if !ends.contains(&e) {
                        ends.push(e);
                    }
                    continue;
                }
                let at = format!("({:.5}, {:.5}), {d:.0} m along it", p.lon, p.lat);
                if *c == 'o' {
                    return not_checked(format!("cannot evaluate: the {} leaves terrain coverage at {at}", leg.name));
                }
                let crosses = match (need, *c) {
                    ("water", 'd') => "water no deeper than 0 m",
                    ("water", _) => "land",
                    _ => "water",
                };
                let detail = format!("{} crosses {crosses} at {at}; {} needs {need}{}", leg.name, platform.id, plan_note(world, uid, false, false));
                if safety {
                    // the safety command goes as ordered, and a person is told
                    let mut c = res(
                        Outcome::Pass,
                        format!("{detail}: the {} goes as ordered (a safety command)", kind.as_str()),
                    );
                    c.warning = Some(format!(
                        "{} path crosses {crosses} at ({:.5}, {:.5}), {d:.0} m along the {}: send a safer order",
                        kind.as_str(),
                        p.lon,
                        p.lat,
                        leg.name
                    ));
                    return c;
                }
                return res(Outcome::Fail, detail);
            }
        }
        let parts = route.parts();
        let mut detail = if waypoints > 0 {
            format!("all {waypoints} waypoints on {need}, and the {parts} ({points} points every {LEG_STEP_M:.0} m)")
        } else {
            format!("the {parts} on {need} ({points} points every {LEG_STEP_M:.0} m)")
        };
        for e in ends {
            detail.push_str("; ");
            detail.push_str(&e);
        }
        detail.push_str(&plan_note(world, uid, true, false));
        res(Outcome::Pass, detail)
    })
}

/// Rules 22: what the plans for vehicle `uid` did, for a check's detail: `routed` the legs
/// routed around what they met (a passing check), else the ones no route exists for (a failing
/// one), each `"; <summary>"`; `no_go` the ones round a no-go area (the geofence's), else round
/// the wrong surface (the domain rule's); empty when none.
fn plan_note(world: &WorldSnapshot, uid: &Uid, routed: bool, no_go: bool) -> String {
    world
        .route_plans
        .get(uid)
        .into_iter()
        .flatten()
        .filter(|p| p.no_route.is_none() == routed && p.met.starts_with("no-go") == no_go)
        .map(|p| format!("; {}", p.summary()))
        .collect()
}

/// Up to rules 16: boats and subs keep their waypoints on water, UGVs on land.
fn domain_waypoints(spec: &IntentSpec, world: &WorldSnapshot) -> Vec<CheckResult> {
    let r = RuleId::Domain;
    let path = spec.task.path();
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        let Some(platform) = &v.platform else {
            return res(
                Outcome::Fail,
                "cannot evaluate: platform not in the catalog".into(),
            );
        };
        let Some(need) = required_surface(platform.domain, platform.autopilot) else {
            return res(
                Outcome::Pass,
                "no surface constraint for this domain".into(),
            );
        };
        let Some(path) = &path else {
            return res(Outcome::Pass, "no waypoints".into());
        };
        match &world.terrain {
            TerrainInputs::Unavailable { reason } => {
                return res(
                    Outcome::NotEvaluated,
                    format!("terrain unavailable: {reason}"),
                );
            }
            TerrainInputs::NotQueried | TerrainInputs::NotNeeded => {
                return res(
                    Outcome::Fail,
                    "cannot evaluate: terrain was not sampled".into(),
                );
            }
            TerrainInputs::Sampled { .. } => {}
        }
        for (i, p) in path.points.iter().enumerate() {
            let at = format!("waypoint {} ({:.5}, {:.5})", i + 1, p.lon, p.lat);
            let ok = match world.terrain.lookup(*p) {
                None => {
                    return res(
                        Outcome::Fail,
                        format!("cannot evaluate: no terrain sample for {at}"),
                    );
                }
                Some(Surface::OutsideCoverage) => {
                    return res(
                        Outcome::Fail,
                        format!("cannot evaluate: {at} is outside terrain coverage"),
                    );
                }
                Some(Surface::Land) => need == "land",
                Some(Surface::Water { depth_m }) => need == "water" && *depth_m > 0.0,
            };
            if !ok {
                let is = if need == "water" {
                    "not water"
                } else {
                    "not land"
                };
                return res(
                    Outcome::Fail,
                    format!("{at} is {is}; {} needs {need}", platform.id),
                );
            }
        }
        res(
            Outcome::Pass,
            format!("all {} waypoints on {need}", path.points.len()),
        )
    })
}

/// No part of a vehicle's route enters a no-go area: the transit leg from its position to
/// the task, the task path, and the return leg home (see [`crate::route`]). A leg that
/// starts at the vehicle's position inside a no-go area is leaving it, which is allowed
/// (otherwise a vehicle that strayed in could never be brought out). For rtl, a return leg
/// that can't be checked is `NotEvaluated` rather than a failure: rtl is the safety command.
pub fn geofence(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let r = RuleId::Geofence;
    let kind = spec.kind();
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        let not_checked = |why: String| {
            if kind == TaskKind::Rtl {
                res(
                    Outcome::NotEvaluated,
                    format!("return leg not checked (rtl is a safety command): {why}"),
                )
            } else {
                res(Outcome::Fail, why)
            }
        };
        let route = match route_for(spec, v, world, cfg) {
            Ok(route) => route,
            Err(e) => return not_checked(e),
        };
        if route.is_empty() {
            return res(Outcome::Pass, format!("no path for {}", kind.as_str()));
        }
        let Some(areas) = &world.areas else {
            return not_checked("cannot evaluate: no area set loaded".into());
        };
        let no_go: Vec<&Area> = areas.no_go().collect();
        // rules 22: through the waypoints a plan routes its legs through
        if let Some(path) = &route.task_routed() {
            for area in &no_go {
                if let Some(why) = area.intrusion(path) {
                    return res(
                        Outcome::Fail,
                        format!("task path: {why} (no-go: {}){}", area.name, plan_note(world, uid, false, true)),
                    );
                }
            }
        }
        let mut leaving: Vec<&str> = Vec::new();
        for seg in &route.segments() {
            for area in &no_go {
                if let Some(why) = segment_enters(area, seg) {
                    return res(Outcome::Fail, format!("{why} (no-go: {}){}", area.name, plan_note(world, uid, false, true)));
                }
                if seg.from_vehicle
                    && area.contains(seg.from)
                    && !leaving.contains(&area.id.as_str())
                {
                    leaving.push(&area.id);
                }
            }
        }
        let mut detail = format!("clear of {} no-go areas ({}){}", no_go.len(), route.parts(), plan_note(world, uid, true, true));
        if !leaving.is_empty() {
            detail.push_str(&format!(
                "; starts inside {} and leaves it",
                leaving.join(", ")
            ));
        }
        res(Outcome::Pass, detail)
    })
}

/// Why a single leg enters a no-go area, naming the leg, or None.
fn segment_enters(area: &Area, seg: &Segment) -> Option<String> {
    let name = seg.name();
    if area.contains(seg.to) {
        return Some(format!("{name}: {} is inside {}", seg.to_label, area.id));
    }
    if area.contains(seg.from) {
        if seg.from_vehicle {
            // Already inside: the leg leaves it.
            return None;
        }
        return Some(format!("{name}: {} is inside {}", seg.from_label, area.id));
    }
    if area.segment_touches_boundary(seg.from, seg.to) {
        return Some(format!("{name} crosses {}", area.id));
    }
    None
}

/// Endurance left after the task stays above the reserve. Estimate: the transit leg to
/// waypoint 1, time on task (at least one lap, at least the configured horizon), the return
/// leg home (the same legs the geofence checks), all at cruise speed, against battery x
/// full endurance. Hold and rtl are exempt. With no endurance figure the result depends on
/// `unknown_endurance` (default: `Unverified`, routed to a human).
pub fn reserves(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let r = RuleId::Reserves;
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        let fail = |d: String| res(Outcome::Fail, d);
        let Some(path) = spec.task.path().filter(|_| !is_safety_command(spec.kind())) else {
            return res(
                Outcome::Pass,
                format!("not required for {}", spec.kind().as_str()),
            );
        };
        let Some(platform) = &v.platform else {
            return fail("cannot evaluate: platform not in the catalog".into());
        };
        let reserve_pct = spec
            .constraints
            .reserve_pct
            .map_or(cfg.reserve_pct, |c| c.max(cfg.reserve_pct));
        let battery = match fresh(&v.battery_pct, v, "battery level", world, cfg) {
            Ok(b) => *b,
            Err(e) => return fail(e),
        };
        let Some(endurance_h) = platform.endurance_h.filter(|e| *e > 0.0) else {
            return match cfg.unknown_endurance {
                UnknownEndurance::Human if battery >= reserve_pct => res(
                    Outcome::Unverified,
                    format!(
                        "not evaluated: endurance unknown for {}; battery {battery:.0}% >= reserve {reserve_pct:.0}% now, task cost not estimated; needs a human",
                        platform.id
                    ),
                ),
                UnknownEndurance::Fail => fail(format!(
                    "cannot evaluate: no endurance figure for {}",
                    platform.id
                )),
                UnknownEndurance::BatteryNow if battery >= reserve_pct => res(
                    Outcome::Pass,
                    format!(
                        "battery {battery:.0}% >= reserve {reserve_pct:.0}% now (no endurance figure, task cost not estimated)"
                    ),
                ),
                UnknownEndurance::Human | UnknownEndurance::BatteryNow => fail(format!(
                    "battery {battery:.0}% below reserve {reserve_pct:.0}%"
                )),
            };
        };
        let Some(cruise) = platform.cruise_mps.filter(|c| *c > 0.0) else {
            return fail(format!(
                "cannot evaluate: no cruise speed for {}",
                platform.id
            ));
        };
        let route = match route_for(spec, v, world, cfg) {
            Ok(route) => route,
            Err(e) => return fail(e),
        };
        let leg_m = |s: &Option<Segment>| s.as_ref().map_or(0.0, |s| haversine_m(s.from, s.to));
        let lap_m: f64 = path
            .legs()
            .iter()
            .map(|(_, a, b)| haversine_m(*a, *b))
            .sum();
        let on_task_s = (lap_m / cruise)
            .max(f64::from(cfg.task_duration_s))
            .max(f64::from(spec.constraints.duration_s.unwrap_or(0)));
        // rules 22: the transit through the waypoints a plan routes it through
        let used_s = route.transit_m() / cruise + on_task_s + leg_m(&route.ret) / cruise;
        let full_s = endurance_h * 3600.0;
        let left_s = battery / 100.0 * full_s - used_s;
        let reserve_s = reserve_pct / 100.0 * full_s;
        let detail = format!(
            "{:.0} min left after task (task {:.0} min, battery {battery:.0}% of {:.0} min), reserve {:.0} min ({reserve_pct:.0}%)",
            left_s / 60.0,
            used_s / 60.0,
            full_s / 60.0,
            reserve_s / 60.0
        );
        if left_s >= reserve_s {
            res(Outcome::Pass, detail)
        } else {
            fail(detail)
        }
    })
}

// ---- space kinds (collect, cover) -------------------------------------------------------

/// An enum's serde name (`sar`, `off_nadir`), for details.
fn tag<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "?".into(),
    }
}

fn space_check(rule: RuleId, outcome: Outcome, detail: impl Into<String>) -> CheckResult {
    CheckResult::new(rule, None, outcome, detail)
}

/// The space answer the rules may read: present, and the answer to this intent's question
/// (the query rebuilt from the spec at the answer's "now" equals the logged query).
fn checked_answer<'a>(
    spec: &IntentSpec,
    world: &'a WorldSnapshot,
    cfg: &RulesConfig,
) -> Result<&'a SpaceAnswer, String> {
    let (answer, computed_t) = match &world.space {
        SpaceInputs::NotQueried => {
            return Err("cannot evaluate: the space segment was not queried".into());
        }
        SpaceInputs::Unavailable { reason } => return Err(format!("cannot evaluate: {reason}")),
        SpaceInputs::Answered {
            answer, computed_t, ..
        } => (answer, *computed_t),
    };
    // rules 13: a collect decides only on an answer computed within the bound of its check
    // (its request time is when it was asked); a cover's question does not depend on it
    if cfg.space_replay() && matches!(answer.query, crate::space::SpaceQuery::Tasking(_)) {
        let Some(c) = computed_t else {
            return Err("cannot evaluate: the space answer does not say when it was computed".into());
        };
        let off = world.t.ms_since(c);
        if off.abs() > PREPARED_SPACE_MAX_AGE_MS {
            return Err(format!(
                "cannot evaluate: the space answer was computed {:.3} s {} the check, more than {:.0} s",
                off.unsigned_abs() as f64 / 1000.0,
                if off > 0 { "before" } else { "after" },
                PREPARED_SPACE_MAX_AGE_MS as f64 / 1000.0
            ));
        }
    }
    let now = answer.now().unwrap_or(world.t);
    let want = match query_for(&spec.task, now, &cfg.space_rules()) {
        Some(Ok(q)) => q,
        Some(Err(e)) => return Err(format!("cannot evaluate: {e}")),
        None => return Err("cannot evaluate: not a space task".into()),
    };
    let same = match (
        serde_json::to_string(&want),
        serde_json::to_string(&answer.query),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same {
        return Err("cannot evaluate: the space answer is not for this intent's query".into());
    }
    // rules 11: an area answer's points are the query's (a result point dropped, moved or
    // copied over another is not the answer to the question)
    if let Err(why) = crate::space::points_match(&answer.query, &answer.result) {
        return Err(format!("cannot evaluate: {why}"));
    }
    Ok(answer)
}

/// The space segment answered this intent's question (collect, cover). Fail-closed: no
/// segment loaded, a failed query, or an answer to a different question fails. The detail
/// says what was asked of which segment, and flags a time-shifted answer.
pub fn space_query(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Vec<CheckResult> {
    let r = RuleId::SpaceQuery;
    if spec.task.targets() != Targets::Space {
        return Vec::new();
    }
    let answer = match checked_answer(spec, world, cfg) {
        Ok(a) => a,
        Err(e) => return vec![space_check(r, Outcome::Fail, e)],
    };
    let what = match &answer.result {
        SpaceResult::Tasking(t) => format!(
            "tasking: {} feasible options, {} excluded passes",
            t.options.len(),
            t.exclusions.len()
        ),
        SpaceResult::Links(l) => format!(
            "satcom {}: {} windows from {} satellites",
            l.class_id, l.windows, l.satellites
        ),
        SpaceResult::Gnss(g) => format!(
            "gnss {}: {} samples from {} satellites",
            g.class_id, g.samples, g.satellites
        ),
        SpaceResult::CoverLinks(c) => format!(
            "satcom {}: {} sample points over the area, {} satellites",
            c.class_id,
            c.points.len(),
            c.points.first().map_or(0, |p| p.links.satellites)
        ),
        SpaceResult::CoverGnss(c) => format!(
            "gnss {}: {} sample points over the area, {} samples each from {} satellites",
            c.class_id,
            c.points.len(),
            c.points.first().map_or(0, |p| p.gnss.samples),
            c.points.first().map_or(0, |p| p.gnss.satellites)
        ),
    };
    let tm = &answer.time_map;
    let shift = if tm.shift_days == 0 {
        String::new()
    } else {
        format!(
            "; TIME-SHIFTED {:+} d (passes are representative, not the scenario date's)",
            tm.shift_days
        )
    };
    vec![space_check(
        r,
        Outcome::Pass,
        format!(
            "{what}; elements reference {}; {}{shift}",
            fmt_utc(tm.reference),
            answer.segment.short()
        ),
    )]
}

/// A collect commits to a chosen option that is still feasible against the latest answer:
/// the same satellite has a feasible pass within `collect_match_tolerance_s` of the chosen
/// collect time, the collect is still in the future, in the window, after the tasking lead
/// time, and its product meets the deadline. Without a chosen option the intent is a query:
/// it fails, listing the ranked options (the full answer is the intent's `space` and the
/// `space.query` event). Unknown product timing with a deadline is `unverified`.
pub fn feasibility(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Vec<CheckResult> {
    let r = RuleId::Feasibility;
    let Task::Collect {
        window,
        product_deadline,
        chosen,
        ..
    } = &spec.task
    else {
        return Vec::new();
    };
    let fail = |d: String| vec![space_check(r, Outcome::Fail, d)];
    let answer = match checked_answer(spec, world, cfg) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let Some(result) = answer.tasking() else {
        return fail("cannot evaluate: the answer is not a tasking answer".into());
    };
    let Some(chosen) = chosen else {
        return fail(options_summary(result));
    };
    collect_feasible(
        result,
        chosen,
        window,
        *product_deadline,
        &cfg.space_rules(),
    )
}

fn collect_feasible(
    result: &murmur_space::tasking::TaskingResult,
    chosen: &ChosenCollect,
    window: &Window,
    deadline: Option<crate::time::Time>,
    cfg: &SpaceRules,
) -> Vec<CheckResult> {
    let r = RuleId::Feasibility;
    let fail = |d: String| vec![space_check(r, Outcome::Fail, d)];
    let at = fmt_time(chosen.collect_time);
    let who = format!("NORAD {} at {at}", chosen.norad_id);
    let now = from_utc(result.request.now);
    if chosen.collect_time <= now {
        return fail(format!("{who}: the pass is over (now {})", fmt_time(now)));
    }
    if !window.contains(chosen.collect_time) {
        return fail(format!("{who}: outside the collection window"));
    }
    let Some(opt) = matched_option(result, chosen, cfg.collect_match_tolerance_s) else {
        // Say why, from the exclusions of the same satellite near that time.
        let near = result
            .exclusions
            .iter()
            .filter(|e| e.norad_id == chosen.norad_id)
            .min_by_key(|e| from_utc(e.pass_time).ms_since(chosen.collect_time).abs());
        let why = match near {
            Some(e) => format!(
                "nearest pass {} excluded ({}: {})",
                fmt_utc(e.pass_time),
                tag(&e.reason),
                e.detail
            ),
            None => "no pass of that satellite in the window".into(),
        };
        return fail(format!("{who}: no longer feasible; {why}"));
    };
    let earliest = from_utc(opt.earliest_collect);
    if chosen.collect_time < earliest {
        return fail(format!(
            "{who}: tasking lead time not met (earliest collect {})",
            fmt_time(earliest)
        ));
    }
    // The product delay is the class's downlink + processing, the same for every collect.
    let product = opt.product_time.map(|p| {
        chosen
            .collect_time
            .plus_ms(from_utc(p).ms_since(from_utc(opt.collect_time)))
    });
    let res = opt
        .resolution_m
        .map_or("resolution unknown".to_string(), |m| format!("{m:.2} m"));
    let detail = format!(
        "{} {} ({}, {}) collect {at}, product {}, {res}, off-nadir {:.1} deg, incidence {:.1} deg; earliest collect {}",
        opt.satellite,
        opt.norad_id,
        opt.class_id,
        tag(&opt.capability),
        product.map_or("time unknown".to_string(), fmt_time),
        opt.geometry.off_nadir_deg,
        opt.geometry.incidence_deg,
        fmt_time(earliest),
    );
    match (deadline, product) {
        (Some(d), Some(p)) if p > d => fail(format!(
            "{who}: product {} after the deadline {}",
            fmt_time(p),
            fmt_time(d)
        )),
        (Some(d), None) => vec![space_check(
            r,
            Outcome::Unverified,
            format!(
                "{detail}; product timing unknown for {}, deadline {} not checked; needs a human",
                opt.class_id,
                fmt_time(d)
            ),
        )],
        _ => vec![space_check(r, Outcome::Pass, detail)],
    }
}

/// Rules 11, after a re-check that lost the chosen pass: the first ranked options of the
/// later answer still taskable at `now` (collect time at or after now plus the class's lead
/// time, which the answer carries as each option's `earliest_collect` less its request
/// time), for a human to choose from (the engine never re-chooses on its own).
pub(crate) fn fresh_options(result: &murmur_space::tasking::TaskingResult, now: crate::time::Time) -> String {
    const SHOWN: usize = 3;
    let asked = from_utc(result.request.now);
    let still: Vec<&murmur_space::tasking::TaskOption> = result
        .options
        .iter()
        .filter(|o| {
            let lead = from_utc(o.earliest_collect).ms_since(asked).max(0);
            from_utc(o.collect_time) >= now.plus_ms(lead)
        })
        .collect();
    if still.is_empty() {
        return format!("no feasible option left in the window from {}", fmt_time(now));
    }
    let mut s = format!(
        "{} feasible options from {}; a new collect may choose one:",
        still.len(),
        fmt_time(now)
    );
    for o in still.iter().take(SHOWN) {
        s.push_str(&format!(
            " #{} {} {} collect {} product {};",
            o.rank,
            o.satellite,
            o.norad_id,
            fmt_utc(o.collect_time),
            o.product_time.map_or("unknown".to_string(), fmt_utc),
        ));
    }
    s
}

/// "no option chosen: ..." with the first ranked options and the exclusion summary.
fn options_summary(result: &murmur_space::tasking::TaskingResult) -> String {
    const SHOWN: usize = 5;
    let mut s = if result.options.is_empty() {
        "no option chosen: no feasible option in the window".to_string()
    } else {
        format!(
            "no option chosen: {} feasible options; propose or amend to one (norad_id, collect_time):",
            result.options.len()
        )
    };
    for o in result.options.iter().take(SHOWN) {
        s.push_str(&format!(
            " #{} {} {} {} collect {} product {} {};",
            o.rank,
            o.satellite,
            o.norad_id,
            tag(&o.capability),
            fmt_utc(o.collect_time),
            o.product_time.map_or("unknown".to_string(), fmt_utc),
            o.resolution_m
                .map_or("res unknown".to_string(), |m| format!("{m:.2} m")),
        ));
    }
    if !result.exclusion_summary.is_empty() {
        let ex: Vec<String> = result
            .exclusion_summary
            .iter()
            .map(|e| format!("{} {} {}", e.class_id, tag(&e.reason), e.passes))
            .collect();
        s.push_str(&format!(" excluded: {}", ex.join(", ")));
    }
    s
}

/// A cover's service holds over the window: SATCOM coverage fraction at least `min_level`
/// (default `cover_min_fraction`), or GNSS satellites in view at every sample at least
/// `min_level` (default `gnss_min_visible`). Rules 11: over the area, which means at every
/// one of its sample points ([`crate::space::cover_points`]): the least covered point
/// decides, and the detail names it; before, at the centroid alone.
pub fn coverage(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let r = RuleId::Coverage;
    let Task::Cover {
        service, min_level, ..
    } = &spec.task
    else {
        return Vec::new();
    };
    let res = |o, d: String| vec![space_check(r, o, d)];
    let answer = match checked_answer(spec, world, cfg) {
        Ok(a) => a,
        Err(e) => return res(Outcome::Fail, e),
    };
    let sc = cfg.space_rules();
    match (service, &answer.result) {
        (CoverService::Satcom { .. }, SpaceResult::Links(l)) => {
            let need = min_level.unwrap_or(sc.cover_min_fraction);
            let lat = l
                .latency_ms_median
                .map_or("latency unknown".to_string(), |m| {
                    format!("latency median {m:.0} ms")
                });
            let mut d = format!(
                "{}: covered {:.1}% of the window (needs {:.1}%), longest outage {:.0} s, {}-{} in view, {lat}",
                l.class_id,
                l.coverage_fraction * 100.0,
                need * 100.0,
                l.longest_outage_s,
                l.visible_min,
                l.visible_max,
            );
            if let Some(o) = l.outages.first() {
                d.push_str(&format!(
                    "; first outage {} to {} ({} outages)",
                    fmt_utc(o.start),
                    fmt_utc(o.end),
                    l.outages.len()
                ));
            }
            let ok = l.coverage_fraction >= need;
            res(if ok { Outcome::Pass } else { Outcome::Fail }, d)
        }
        (CoverService::Gnss { .. }, SpaceResult::Gnss(g)) => {
            let need = min_level.unwrap_or(f64::from(sc.gnss_min_visible));
            let pdop = g
                .pdop_max
                .map_or("PDOP unknown".to_string(), |p| format!("PDOP max {p:.2}"));
            let mut d = format!(
                "{}: {}-{} satellites in view over {} samples (needs {need:.0} at every sample), {pdop}",
                g.class_id, g.visible_min, g.visible_max, g.samples,
            );
            if let Some(t) = g.visible_min_at {
                d.push_str(&format!("; fewest at {}", fmt_utc(t)));
            }
            let ok = g.samples > 0 && g.visible_min as f64 >= need;
            res(if ok { Outcome::Pass } else { Outcome::Fail }, d)
        }
        (CoverService::Satcom { .. }, SpaceResult::CoverLinks(c)) => {
            let need = min_level.unwrap_or(sc.cover_min_fraction);
            let Some(worst) = c.worst() else {
                return res(
                    Outcome::Fail,
                    "cannot evaluate: the answer has no sample point".into(),
                );
            };
            let w = &worst.links;
            let lat = w
                .latency_ms_median
                .map_or("latency unknown".to_string(), |m| {
                    format!("latency median {m:.0} ms")
                });
            let longest = c
                .points
                .iter()
                .map(|p| p.links.longest_outage_s)
                .fold(0.0, f64::max);
            let vmin = c.points.iter().map(|p| p.links.visible_min).min().unwrap_or(0);
            let vmax = c.points.iter().map(|p| p.links.visible_max).max().unwrap_or(0);
            let mut d = format!(
                "{}: covered {:.1}% of the window at the least covered of {} sample points over the area ({:.4}, {:.4}) (needs {:.1}% at every point; mean {:.1}%), longest outage {:.0} s, {}-{} in view, {lat}",
                c.class_id,
                w.coverage_fraction * 100.0,
                c.points.len(),
                worst.point.lat_deg,
                worst.point.lon_deg,
                need * 100.0,
                c.mean_coverage() * 100.0,
                longest,
                vmin,
                vmax,
            );
            let outages = c.outages();
            if let Some(o) = outages.first() {
                d.push_str(&format!(
                    "; first outage anywhere over the area {} to {} ({} outages merged over the area)",
                    fmt_utc(o.start),
                    fmt_utc(o.end),
                    outages.len()
                ));
            }
            let ok = w.coverage_fraction >= need;
            res(if ok { Outcome::Pass } else { Outcome::Fail }, d)
        }
        (CoverService::Gnss { .. }, SpaceResult::CoverGnss(c)) => {
            let need = min_level.unwrap_or(f64::from(sc.gnss_min_visible));
            let Some(worst) = c.worst() else {
                return res(
                    Outcome::Fail,
                    "cannot evaluate: the answer has no sample point".into(),
                );
            };
            let g = &worst.gnss;
            let pdop = c
                .points
                .iter()
                .filter_map(|p| p.gnss.pdop_max)
                .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
                .map_or("PDOP unknown".to_string(), |p| format!("PDOP max {p:.2}"));
            let samples = c.points.iter().map(|p| p.gnss.samples).min().unwrap_or(0);
            let vmax = c.points.iter().map(|p| p.gnss.visible_max).max().unwrap_or(0);
            let mut d = format!(
                "{}: {}-{} satellites in view over {} samples at the worst of {} sample points over the area ({:.4}, {:.4}) (needs {need:.0} at every sample of every point), {pdop}",
                c.class_id,
                g.visible_min,
                vmax,
                samples,
                c.points.len(),
                worst.point.lat_deg,
                worst.point.lon_deg,
            );
            if let Some(t) = g.visible_min_at {
                d.push_str(&format!("; fewest at {}", fmt_utc(t)));
            }
            let ok = samples > 0 && g.visible_min as f64 >= need;
            res(if ok { Outcome::Pass } else { Outcome::Fail }, d)
        }
        _ => res(
            Outcome::Fail,
            "cannot evaluate: the answer is not for this service".into(),
        ),
    }
}
