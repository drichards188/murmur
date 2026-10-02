//! Rules (Phase C.2): pure functions of `(intent, &WorldSnapshot, &RulesConfig)`.
//!
//! Fail-closed: a rule that can't evaluate (vehicle unknown, fact missing or stale, area
//! file not loaded, terrain point unanswered) returns `Fail` and says why. The single
//! exception is the one PLAN.md names: with no terrain oracle installed, the domain rule
//! returns `NotEvaluated`, which is recorded and blocks only consequential kinds
//! ([`TaskKind::is_consequential`], i.e. engage in Phase E).
//!
//! Output order is fixed (rule order, then the intent's uid order) and detail strings use
//! fixed precision, so recomputing a decision gives byte-identical results.

use serde::{Deserialize, Serialize};

use crate::catalog::{AutopilotKind, Domain};
use crate::geo::{LonLat, haversine_m};
use crate::ids::Uid;
use crate::intent::{IntentSpec, Task, TaskKind};
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
    /// Deliberately skipped (no terrain oracle). Recorded; blocks consequential kinds only.
    NotEvaluated,
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
    /// The vehicle this result is about; None for whole-intent rules (geofence).
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
    /// No endurance figure: the reserves rule can't evaluate, so it fails.
    #[default]
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
            unknown_endurance: UnknownEndurance::Fail,
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
    out.push(geofence(spec, world));
    out.extend(reserves(spec, world, cfg));
    out
}

/// Whether a set of results lets the intent proceed.
pub fn passed(results: &[CheckResult], kind: TaskKind) -> bool {
    results.iter().all(|r| match r.outcome {
        Outcome::Pass => true,
        Outcome::Fail => false,
        Outcome::NotEvaluated => !kind.is_consequential(),
    })
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
fn fresh<'a, T>(
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

/// No task path enters a no-go area.
pub fn geofence(spec: &IntentSpec, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::Geofence;
    let Some(path) = spec.task.path() else {
        return CheckResult::new(r, None, Outcome::Pass, "no waypoints");
    };
    let Some(areas) = &world.areas else {
        return CheckResult::new(
            r,
            None,
            Outcome::Fail,
            "cannot evaluate: no area set loaded",
        );
    };
    let mut n = 0;
    for area in areas.no_go() {
        n += 1;
        if let Some(why) = area.intrusion(&path) {
            return CheckResult::new(
                r,
                None,
                Outcome::Fail,
                format!("{why} (no-go: {})", area.name),
            );
        }
    }
    CheckResult::new(r, None, Outcome::Pass, format!("clear of {n} no-go areas"))
}

/// Endurance left after the task stays above the reserve. Estimate: transit to the task,
/// time on task (at least one lap, at least the configured horizon), transit home, all at
/// cruise speed, against battery x full endurance.
pub fn reserves(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let r = RuleId::Reserves;
    per_vehicle(spec, world, r, |uid, v| {
        let res = |o, d: String| CheckResult::new(r, Some(uid), o, d);
        let fail = |d: String| res(Outcome::Fail, d);
        let Some(path) = spec.task.path() else {
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
                UnknownEndurance::BatteryNow => fail(format!(
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
        let pos = match fresh(&v.position, "position", world, cfg) {
            Ok(p) => *p,
            Err(e) => return fail(e),
        };
        let Some(home) = &v.home else {
            return fail("cannot evaluate: no home position".into());
        };
        let start = nearest(pos, &path.points);
        let lap_m: f64 = path
            .legs()
            .iter()
            .map(|(_, a, b)| haversine_m(*a, *b))
            .sum();
        let on_task_s = (lap_m / cruise)
            .max(f64::from(cfg.task_duration_s))
            .max(f64::from(spec.constraints.duration_s.unwrap_or(0)));
        let used_s =
            haversine_m(pos, start) / cruise + on_task_s + haversine_m(start, home.value) / cruise;
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

fn nearest(from: LonLat, points: &[LonLat]) -> LonLat {
    points
        .iter()
        .copied()
        .min_by(|a, b| haversine_m(from, *a).total_cmp(&haversine_m(from, *b)))
        .unwrap_or(from)
}
