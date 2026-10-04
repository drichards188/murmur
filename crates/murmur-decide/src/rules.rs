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
    /// No protected track (anything but the target and declared hostiles) inside the
    /// keep-out at the effect time.
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
}

impl CheckResult {
    fn new(rule: RuleId, uid: Option<&Uid>, outcome: Outcome, detail: impl Into<String>) -> Self {
        CheckResult {
            rule,
            uid: uid.cloned(),
            outcome,
            detail: detail.into(),
            fix: None,
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

/// Rule settings, from `[rules]` in `sim/policy.toml`. Logged with the policy at run start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RulesConfig {
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

    /// `[rules.space]`, or its defaults.
    pub fn space_rules(&self) -> SpaceRules {
        self.space.clone().unwrap_or_default()
    }

    /// `[rules.engage]`, or its defaults.
    pub fn engage_rules(&self) -> EngageRules {
        self.engage.clone().unwrap_or_default()
    }
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            max_fact_age_s: 10,
            fact_age_intervals: None,
            orbit_radius_margin: 1.1,
            fixed_wing_default_min_turn_radius_m: 60.0,
            reserve_pct: 20.0,
            task_duration_s: 900,
            unknown_endurance: UnknownEndurance::Human,
            space: None,
            engage: None,
        }
    }
}

/// All rules, in order.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let mut out = Vec::new();
    match spec.task.targets() {
        Targets::Vehicles => {
            out.extend(capability(spec, world));
            out.extend(readiness(spec, world, cfg));
            out.extend(platform_limits(spec, world, cfg));
            out.extend(domain(spec, world));
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
/// safety commands, and the autopilot applies its own checks to them.
pub fn readiness(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let r = RuleId::Readiness;
    let kind = spec.kind();
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        if is_safety_command(kind) {
            return res(Outcome::Pass, format!("not required for {}", kind.as_str()));
        }
        match fresh(&v.nav_ready, v, "navigation status", world, cfg) {
            Err(e) => return res(Outcome::Fail, e),
            Ok(false) => return res(Outcome::Fail, "navigation not ready (GPS/EKF)".into()),
            Ok(true) => {}
        }
        // dead reckoning (no GNSS) is how a submerged AUV navigates; anyone else needs a fix
        let dead_reckoning =
            v.nav_source.as_ref().map(|f| f.value) == Some(NavSource::DeadReckoning);
        let is_sub = v.autopilot.as_ref().map(|f| f.value) == Some(AutopilotKind::Sub);
        if dead_reckoning && !is_sub {
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

/// Boats and subs keep their waypoints on water, UGVs on land.
pub fn domain(spec: &IntentSpec, world: &WorldSnapshot) -> Vec<CheckResult> {
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
        if let Some(path) = &route.task {
            for area in &no_go {
                if let Some(why) = area.intrusion(path) {
                    return res(
                        Outcome::Fail,
                        format!("task path: {why} (no-go: {})", area.name),
                    );
                }
            }
        }
        let mut leaving: Vec<&str> = Vec::new();
        for seg in route.segments() {
            for area in &no_go {
                if let Some(why) = segment_enters(area, seg) {
                    return res(Outcome::Fail, format!("{why} (no-go: {})", area.name));
                }
                if seg.from_vehicle
                    && area.contains(seg.from)
                    && !leaving.contains(&area.id.as_str())
                {
                    leaving.push(&area.id);
                }
            }
        }
        let mut detail = format!("clear of {} no-go areas ({})", no_go.len(), route.parts());
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
        let used_s = leg_m(&route.transit) / cruise + on_task_s + leg_m(&route.ret) / cruise;
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
    let answer = match &world.space {
        SpaceInputs::NotQueried => {
            return Err("cannot evaluate: the space segment was not queried".into());
        }
        SpaceInputs::Unavailable { reason } => return Err(format!("cannot evaluate: {reason}")),
        SpaceInputs::Answered { answer, .. } => answer,
    };
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
/// `min_level` (default `gnss_min_visible`).
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
        _ => res(
            Outcome::Fail,
            "cannot evaluate: the answer is not for this service".into(),
        ),
    }
}
