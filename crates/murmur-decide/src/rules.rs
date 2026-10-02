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

use serde::{Deserialize, Serialize};

use crate::areas::Area;
use crate::catalog::{AutopilotKind, Domain};
use crate::geo::haversine_m;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Task, TaskKind};
use crate::route::{Segment, route_for};
use crate::terrain::Surface;
use crate::world::{Fact, TerrainInputs, VehicleFacts, WorldSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleId {
    Capability,
    Readiness,
    PlatformLimits,
    Domain,
    Geofence,
    Reserves,
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
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            max_fact_age_s: 10,
            orbit_radius_margin: 1.1,
            fixed_wing_default_min_turn_radius_m: 60.0,
            reserve_pct: 20.0,
            task_duration_s: 900,
            unknown_endurance: UnknownEndurance::Human,
        }
    }
}

/// All rules, in order.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let mut out = Vec::new();
    out.extend(capability(spec, world));
    out.extend(readiness(spec, world, cfg));
    out.extend(platform_limits(spec, world, cfg));
    out.extend(domain(spec, world));
    out.extend(geofence(spec, world, cfg));
    out.extend(reserves(spec, world, cfg));
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
    what: &str,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Result<&'a T, String> {
    let f = fact
        .as_ref()
        .ok_or_else(|| format!("cannot evaluate: no {what} reported"))?;
    let age_ms = world.t.ms_since(f.t);
    let max_ms = i64::from(cfg.max_fact_age_s) * 1000;
    if age_ms > max_ms {
        return Err(format!(
            "cannot evaluate: {what} is {:.1} s old (max {} s)",
            age_ms as f64 / 1000.0,
            cfg.max_fact_age_s
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
        match fresh(&v.nav_ready, "navigation status", world, cfg) {
            Err(e) => return res(Outcome::Fail, e),
            Ok(false) => return res(Outcome::Fail, "navigation not ready (GPS/EKF)".into()),
            Ok(true) => {}
        }
        if let Task::Track { target } = &spec.task {
            let target_pos = world.vehicles.get(target).map(|t| &t.position);
            match target_pos.map(|p| fresh(p, "target position", world, cfg)) {
                None => return res(Outcome::Fail, format!("track target {target} unknown")),
                Some(Err(e)) => return res(Outcome::Fail, e),
                Some(Ok(_)) => {}
            }
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
        let battery = match fresh(&v.battery_pct, "battery level", world, cfg) {
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
