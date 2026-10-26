//! Evaluation only (`eval/keepout.py`, METRICS.md "Keep-outs"): generated engagement
//! situations through murmur-decide's own nomination checks, read-only. Nothing here is part
//! of the C2 or decides anything for it; the engine's rules are called, never restated.
//!
//! `murmur-keepout-probe [--root DIR] [--rules 6,9,12,14] [--keep-out-m M]` reads one situation per line on
//! stdin and writes one result per line on stdout for each of its evaluations.
//!
//! **A situation** (JSON): `id`, `t_ms` (engine time now), `target` (a track id), `radius_m`,
//! `fallback`, `tracks` (track id to [`TrackFacts`], the engine's own schema: the generator
//! writes the facts a tracker would report), `vehicles` (ours besides the engaging platform:
//! `uid`, `platform` (catalog id), `at`, `velocity`, `report_age_s`, the age of its last
//! report) and `evals` (where the engaging platform is: `key`, `platform`, `at`).
//!
//! **Each evaluation** builds the world the bridge would hand the engine (our vehicles as the
//! scenario runner reports them, the engaging platform `strike-1` fresh at `at`), a nomination
//! of `target` by `op-1` on `strike-1`, and for each rules version runs
//! [`rules::evaluate`] on [`WorldSnapshot::for_intent_with`] (the checks `Engine::submit`
//! runs at a nomination, with the Golden Gate terrain the scenario runner uses) and
//! [`engage::preview`]. Then, for the keep-out on tracks, it takes every protected track's
//! regions from [`engage::track_reach`] and applies the keep-out's own comparison (a region's
//! near edge inside the keep-out) to each region alone: which component reaches in, and by how
//! much (`margin_m`: the near edge's distance outside the keep-out, negative inside). The
//! regions together must give the engine's verdict; an evaluation where they do not is
//! flagged (`mismatch`), never silently fixed. Components: `current` (along the velocity,
//! rules 6), `mean` (along the average velocity, rules 9), `manoeuvre` (anywhere at the
//! fastest it went, rules 9), `young` (anywhere at its domain's top speed, rules 12),
//! `aircraft` (an aircraft: anywhere it reaches from its own speed, speeding up to the top
//! speed, rules 14), `no_velocity` (no velocity estimate: anywhere at its domain's top speed,
//! rules 6).
//!
//! Rules 16 (murmur-533): the contacts the tracker dropped (`lost`, from the real tracker's
//! drops) are the engine's lost contacts under rules 16 and absent before (as the bridge's
//! picture had them): their regions ([`engage::lost_reach`]: a live track's, and anywhere at
//! its domain's top speed) are component `lost`. Under rules 16 each evaluation with lost
//! contacts also says how the keep-out on tracks would have gone with them all accepted
//! (`accepted`: what an acceptance, or any rule clearing them all, gives), held only until
//! 600 s after their drop (`held600`), or counted only with their last fix within 3 km of the
//! effect point (`near3km`), or counted by the regions along their course alone, as a live
//! track's (`heading`: the first version of the lost contacts, before murmur-533's finding;
//! given to the engine as live tracks with their facts at the drop): `lost_variants`, the
//! engine's own check on those pictures.

use std::collections::BTreeMap;
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use murmur_decide::areas::AreaSet;
use murmur_decide::catalog::Catalog;
use murmur_decide::engage::{self, EngageRules, Fallback};
use murmur_decide::geo::{LonLat, Polygon, haversine_m};
use murmur_decide::ids::{IntentId, OperatorId, Uid};
use murmur_decide::intent::{Constraints, IntentSpec, Source, Task};
use murmur_decide::policy::Policy;
use murmur_decide::rules::{self, CheckResult, Outcome, RuleId, RulesConfig};
use murmur_decide::terrain::PolygonTerrain;
use murmur_decide::time::Time;
use murmur_decide::lost::LostContact;
use murmur_decide::world::{Fact, NavSource, TrackFacts, VehicleFacts, WorldSnapshot};
use serde::Deserialize;
use serde_json::{Value, json};

mod realtrack;
mod tracks;

/// The bridge's `clock::round` (crates/murmur-bridge/src/clock.rs, whose wall clock needs
/// tokio), for the tracker included by path: Python's round(x, digits).
mod clock {
    pub fn round(x: f64, digits: usize) -> f64 {
        format!("{x:.digits$}").parse().unwrap_or(x)
    }
}

/// The bridge's own `geo.rs`, which its sensing model calls.
#[allow(dead_code)]
#[path = "../../../crates/murmur-bridge/src/geo.rs"]
mod geo;

/// The engaging platform's uid in every evaluation.
pub const STRIKE: &str = "strike-1";
/// The nominating operator (dev ids, as the scenario runner's).
pub const NOMINATOR: &str = "op-1";
/// At most this many intruding regions are listed per evaluation (all are counted).
const MAX_LISTED: usize = 12;

#[derive(Clone, Debug, Deserialize)]
pub struct VehicleIn {
    pub uid: String,
    pub platform: String,
    pub at: [f64; 2],
    #[serde(default)]
    pub velocity: [f64; 2],
    /// Seconds since its last report: beyond the policy's freshness it is lost.
    #[serde(default)]
    pub report_age_s: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EvalIn {
    pub key: String,
    pub platform: String,
    pub at: [f64; 2],
}

#[derive(Clone, Debug, Deserialize)]
pub struct Situation {
    pub id: String,
    pub t_ms: i64,
    pub target: String,
    pub radius_m: f64,
    pub fallback: Fallback,
    pub tracks: BTreeMap<String, TrackFacts>,
    #[serde(default)]
    pub vehicles: Vec<VehicleIn>,
    pub evals: Vec<EvalIn>,
    /// The real-tracker option ([`realtrack`]): the protected contacts' facts come from the
    /// bridge's tracker on their motion instead of `tracks` (the target's stay as given).
    #[serde(default)]
    pub tracker: Option<realtrack::TrackerIn>,
    /// Rules 16: the contacts the tracker dropped, as the engine keeps them (from the real
    /// tracker's drops).
    #[serde(default)]
    pub lost: BTreeMap<String, LostContact>,
}

/// The repository's policy, catalog and areas, and the rules configuration per version.
pub struct Env {
    pub catalog: Catalog,
    pub areas: AreaSet,
    pub terrain: PolygonTerrain,
    pub cfgs: BTreeMap<u32, RulesConfig>,
    /// The catalog's sensors, for the real-tracker option.
    pub sensors: tracks::sensing::SensorCatalog,
}

/// The scenario runner's clock origin (`scenario::T0`): areas are loaded at it.
pub const T0: Time = Time::from_secs(1_781_000_000);

impl Env {
    /// `sim/policy.toml` under each rules version as the scenario runner sets one
    /// (`--rules N`: the version, and no `[identification]` before rules 9).
    pub fn load(root: &Path, versions: &[u32]) -> Result<Env, String> {
        Self::load_with(root, versions, None)
    }

    /// [`Env::load`] with `[rules.engage] keep_out_m` set to `keep_out_m` (a copy of the policy
    /// with another buffer, inside the bounds a policy file must meet).
    pub fn load_with(root: &Path, versions: &[u32], keep_out_m: Option<f64>) -> Result<Env, String> {
        let catalog = Catalog::load(&root.join("sim/platforms.toml")).map_err(|e| e.to_string())?;
        let areas = AreaSet::load_dir(&root.join("sim/areas"), T0).map_err(|e| e.to_string())?;
        let mut base = Policy::load(&root.join("sim/policy.toml")).map_err(|e| e.to_string())?;
        if let Some(k) = keep_out_m {
            let mut e = base.rules.engage.clone().unwrap_or_default();
            e.keep_out_m = k;
            e.validate()?;
            base.rules.engage = Some(e);
        }
        let mut cfgs = BTreeMap::new();
        for &v in versions {
            if v == 0 || v > rules::RULES_VERSION {
                return Err(format!("rules version {v}: this build decides 1..={}", rules::RULES_VERSION));
            }
            let mut p = base.clone();
            p.rules.rules_version = v;
            if v < rules::RULE_ID_SINCE {
                p.identification = None;
                p.sync();
            }
            cfgs.insert(v, p.rules);
        }
        let sensors = tracks::sensing::SensorCatalog::load(&root.join("sim/platforms.toml"))?;
        Ok(Env {
            sensors,
            catalog,
            areas,
            terrain: terrain(),
            cfgs,
        })
    }
}

/// Golden Gate land and water, as the scenario runner's (`scenario::terrain`): land south of
/// the Presidio shore and on Marin, water between.
pub fn terrain() -> PolygonTerrain {
    let b = |w: f64, s: f64, e: f64, n: f64| {
        Polygon::new(vec![
            LonLat::new(w, s),
            LonLat::new(e, s),
            LonLat::new(e, n),
            LonLat::new(w, n),
        ])
    };
    PolygonTerrain {
        coverage: b(-122.60, 37.70, -122.30, 37.95),
        land: vec![b(-122.60, 37.70, -122.30, 37.800), b(-122.60, 37.832, -122.46, 37.95)],
        water_depth_m: 20.0,
    }
}

/// One of our vehicles as the scenario runner reports it (`scenario::snapshot`): every fact
/// stamped with its last report, a 1 s link, a 3D fix, home where it is.
pub fn vehicle(catalog: &Catalog, platform: &str, at: [f64; 2], velocity: [f64; 2], last: Time) -> Result<VehicleFacts, String> {
    let p = catalog
        .get(platform)
        .cloned()
        .ok_or_else(|| format!("platform {platform} is not in the catalog"))?;
    let pos = LonLat::new(at[0], at[1]);
    Ok(VehicleFacts {
        autopilot: p.autopilot.map(|a| Fact::new(a, last)),
        nav_ready: Some(Fact::new(true, last)),
        position: Some(Fact::new(pos, last)),
        home: Some(Fact::new(pos, T0)),
        battery_pct: Some(Fact::new(95.0, last)),
        nav_source: Some(Fact::new(NavSource::Gnss, last)),
        gps_fix: Some(Fact::new(3, last)),
        gps_glitch: Some(Fact::new(false, last)),
        gps_fix_from: None,
        circling: None,
        report_interval_ms: Some(1000),
        velocity_mps: Some(Fact::new(velocity, last)),
        alt_msl_m: None,
        platform: Some(p),
        booted_t: None,
    })
}

/// The nomination: `target` by [`NOMINATOR`] on [`STRIKE`], no window (the policy's default).
pub fn nomination(s: &Situation, now: Time) -> IntentSpec {
    IntentSpec {
        id: IntentId(1),
        created_t: now,
        source: Source::Human {
            id: OperatorId::new(NOMINATOR),
        },
        task: Task::Engage {
            track: s.target.clone(),
            effect_radius_m: s.radius_m,
            window: None,
            fallback: s.fallback,
        },
        uids: vec![Uid::new(STRIKE)],
        constraints: Constraints::default(),
        rationale: None,
    }
}

/// What a keep-out region is, by the order [`engage::track_reach`] gives them in under these
/// rules: along the velocity (or, without one, the top-speed disc alone), then along the
/// average velocity, the manoeuvre disc and the young disc where they apply.
pub fn components(t: &TrackFacts, ec: &EngageRules) -> Vec<&'static str> {
    if t.velocity_mps.is_none() {
        return vec!["no_velocity"];
    }
    let mut out = vec!["current"];
    if ec.v9() && t.mean_velocity_mps.is_some() {
        out.push("mean");
    }
    if ec.v9() && t.manoeuvre.is_some() {
        out.push("manoeuvre");
    }
    if ec.followups() && t.mean_velocity_mps.is_none() && t.first_seen_t.is_some() {
        out.push("young");
    }
    if ec.aircraft_reach() && t.domain == murmur_decide::catalog::Domain::Air {
        out.push("aircraft");
    }
    out
}

fn outcome(o: Outcome) -> &'static str {
    match o {
        Outcome::Pass => "pass",
        Outcome::Fail => "fail",
        Outcome::NotEvaluated => "not_evaluated",
        Outcome::Unverified => "unverified",
    }
}

fn find(checks: &[CheckResult], rule: RuleId) -> Option<&CheckResult> {
    checks.iter().find(|c| c.rule == rule)
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The world for one evaluation: our vehicles and the engaging platform at `at`.
pub fn world(env: &Env, s: &Situation, e: &EvalIn, now: Time) -> Result<WorldSnapshot, String> {
    let mut w = WorldSnapshot::new(now);
    w.areas = Some(env.areas.clone());
    for v in &s.vehicles {
        let last = now.plus_ms(-((v.report_age_s * 1000.0).round() as i64));
        w.vehicles
            .insert(Uid::new(&v.uid), vehicle(&env.catalog, &v.platform, v.at, v.velocity, last)?);
    }
    w.vehicles
        .insert(Uid::new(STRIKE), vehicle(&env.catalog, &e.platform, e.at, [0.0, 0.0], now)?);
    w.tracks = s.tracks.clone();
    w.lost_contacts = s.lost.clone();
    Ok(w)
}

/// How far a lost contact's last fix may be from the effect point for the `near3km` variant
/// to count it, m.
pub const NEAR_LOST_M: f64 = 3000.0;
/// How long after its drop the `held600` variant still counts a lost contact, ms.
pub const HELD_MS: i64 = 600_000;

/// The live tracks out of view judged by their course alone, as before the review of
/// murmur-533 (rules 16 counts one unseen [`engage::OUT_OF_VIEW_S`] at its domain's top speed):
/// the engine reads each as seen now, its facts otherwise as they are.
fn stale_in_view(w: &WorldSnapshot) -> WorldSnapshot {
    let mut w = w.clone();
    let now = w.t;
    for t in w.tracks.values_mut() {
        t.last_seen_t = t.last_seen_t.max(now);
    }
    w
}

/// The `heading` variant (the first version of the lost contacts): the lost contacts counted by
/// the regions a live track in view gets from the same facts ([`engage::track_reach`], along
/// its course): the engine reads each as a live track seen now, under its id, with its facts at
/// the drop; and the live tracks out of view by their course too ([`stale_in_view`]).
fn as_live_tracks(w: &WorldSnapshot) -> WorldSnapshot {
    let mut w = stale_in_view(w);
    let now = w.t;
    for (id, mut l) in std::mem::take(&mut w.lost_contacts) {
        l.facts.last_seen_t = now;
        w.tracks.entry(id).or_insert(l.facts);
    }
    w
}

/// The `own_speed` variant (the second version, before the review): as [`as_live_tracks`], and
/// each lost contact also anywhere at its own speed bound since its last fix (its speed and
/// `keep_out_sigma` of the policy's velocity error, or the fastest it went): the engine reads
/// that as a manoeuvre's reach from its last fix at that speed.
fn at_own_speed(w: &WorldSnapshot, ec: &engage::EngageRules) -> WorldSnapshot {
    let mut w = stale_in_view(w);
    let now = w.t;
    for (id, mut l) in std::mem::take(&mut w.lost_contacts) {
        if let Some([e, n]) = l.facts.velocity_mps {
            let bound = e.hypot(n) + ec.keep_out_sigma * ec.track_velocity_sigma();
            let speed = l.facts.manoeuvre.map_or(bound, |m| m.speed_mps.max(bound));
            l.facts.manoeuvre = Some(murmur_decide::Manoeuvre {
                t: l.facts.position.t,
                change_mps: 0.0,
                speed_mps: speed,
            });
        }
        l.facts.last_seen_t = now;
        w.tracks.entry(id).or_insert(l.facts);
    }
    w
}

/// A region's component label: what it is (rules 16: `out_of_view` for a live track unseen
/// [`engage::OUT_OF_VIEW_S`], counted anywhere at its domain's top speed).
fn label_of(kind: engage::ReachKind) -> &'static str {
    match kind {
        engage::ReachKind::Velocity => "current",
        engage::ReachKind::MeanVelocity => "mean",
        engage::ReachKind::Manoeuvre => "manoeuvre",
        engage::ReachKind::Young => "young",
        engage::ReachKind::NoVelocity => "no_velocity",
        engage::ReachKind::Aircraft => "aircraft",
        engage::ReachKind::OutOfView => "out_of_view",
        // rules 19: a track accepted as behaving now (no probe world accepts one)
        engage::ReachKind::Accepted => "accepted",
    }
}

/// Rules 16: the keep-out on tracks of `spec` on `w` with the lost contacts `keep` keeps.
fn kot_with(spec: &IntentSpec, w: &WorldSnapshot, cfg: &RulesConfig, keep: impl Fn(&LostContact) -> bool) -> &'static str {
    let mut w = w.clone();
    w.lost_contacts.retain(|_, l| keep(l));
    let checks = rules::evaluate(spec, &w, cfg);
    find(&checks, RuleId::KeepOutTracks).map_or("absent", |c| outcome(c.outcome))
}

/// One evaluation under one rules version: the engine's checks, the preview, and each
/// protected track's regions against the keep-out.
pub fn evaluate(env: &Env, s: &Situation, base: &WorldSnapshot, version: u32) -> Result<(Value, Option<engage::EngagePreview>), String> {
    let cfg = env.cfgs.get(&version).ok_or_else(|| format!("rules {version} not loaded"))?;
    let spec = nomination(s, base.t);
    let mut w = base.for_intent_with(&spec, &env.terrain, cfg);
    // rules 16: the engine fills its lost contacts into an engagement's inputs (subset_for
    // leaves them to it); before, a dropped track was simply not in the picture
    if cfg.lost_contacts() {
        w.lost_contacts = base.lost_contacts.clone();
    }
    let checks = rules::evaluate(&spec, &w, cfg);
    let preview = engage::preview(&spec, &w, cfg).ok();
    let mut failed: Vec<&str> = Vec::new();
    for c in &checks {
        if c.outcome != Outcome::Pass && !failed.contains(&c.rule.as_str()) {
            failed.push(c.rule.as_str());
        }
    }
    let kot = find(&checks, RuleId::KeepOutTracks);
    let kof = find(&checks, RuleId::KeepOutFriendly);
    let mut out = json!({
        "kot": kot.map(|c| outcome(c.outcome)),
        "kof": kof.map(|c| outcome(c.outcome)),
        "failed": failed,
    });
    if let Some(c) = kof.filter(|c| c.outcome != Outcome::Pass) {
        out["kof_uid"] = json!(c.uid.as_ref().map(|u| u.to_string()));
        out["kof_detail"] = json!(c.detail);
    }
    if let Some(c) = kot.filter(|c| c.outcome != Outcome::Pass) {
        out["kot_detail"] = json!(c.detail.chars().take(400).collect::<String>());
    }
    let Some(p) = &preview else {
        return Ok((out, None));
    };
    let ec = cfg.engage_rules();
    let mut comp: BTreeMap<&str, f64> = BTreeMap::new();
    let mut intr: Vec<(f64, String, &str, f64)> = Vec::new();
    let mut nearest: Option<(f64, &str)> = None;
    let mut tracks_in = 0;
    let mut fails = false;
    let mut mismatch = Vec::new();
    for (id, t) in &w.tracks {
        // the generated situations declare nothing but the target: every other track is
        // protected (a mismatch below would show otherwise)
        if id == &s.target {
            continue;
        }
        // as the keep-out reads it at the decision's time (rules 16: out of view too)
        let regions = engage::track_reach_at(t, w.t, p.effect_t, &ec);
        let labels = components(t, &ec);
        if labels.len() != regions.iter().filter(|r| r.kind != engage::ReachKind::OutOfView).count() {
            mismatch.push(format!("{id}: {} regions, {} components", regions.len(), labels.len()));
        }
        let mut this_in = false;
        for r in &regions {
            let label = label_of(r.kind);
            let edge = haversine_m(r.center, p.effect_point) - r.radius_m;
            let margin = edge - p.keep_out_m;
            let e = comp.entry(label).or_insert(f64::INFINITY);
            *e = e.min(margin);
            if nearest.is_none_or(|(m, _)| margin < m) {
                nearest = Some((margin, id.as_str()));
            }
            // the keep-out's own comparison (engage::keep_out_tracks)
            if edge < p.keep_out_m {
                this_in = true;
                intr.push((margin, id.clone(), label, r.radius_m));
            }
        }
        if this_in {
            tracks_in += 1;
            fails = true;
        }
    }
    // rules 16: the lost contacts' regions (component `lost`), as the keep-out reads them
    let mut lost_in = 0;
    for (id, l) in &w.lost_contacts {
        for r in engage::lost_reach(&l.facts, p.effect_t, &ec) {
            let edge = haversine_m(r.center, p.effect_point) - r.radius_m;
            let margin = edge - p.keep_out_m;
            let e = comp.entry("lost").or_insert(f64::INFINITY);
            *e = e.min(margin);
            if nearest.is_none_or(|(m, _)| margin < m) {
                nearest = Some((margin, id.as_str()));
            }
            if edge < p.keep_out_m {
                if !intr.iter().any(|x| x.1 == *id) {
                    lost_in += 1;
                }
                fails = true;
                intr.push((margin, id.clone(), "lost", r.radius_m));
            }
        }
    }
    if !w.lost_contacts.is_empty() {
        out["n_lost"] = json!(w.lost_contacts.len());
        out["n_lost_intr"] = json!(lost_in);
        let point = p.effect_point;
        let now = w.t;
        let heading = kot_with(&spec, &as_live_tracks(&w), cfg, |_| true);
        out["lost_variants"] = json!({
            "accepted": kot_with(&spec, &w, cfg, |_| false),
            "held600": kot_with(&spec, &w, cfg, |l| now.ms_since(l.dropped_t) <= HELD_MS),
            "near3km": kot_with(&spec, &w, cfg, |l| haversine_m(l.facts.position.value, point) <= NEAR_LOST_M),
            "heading": heading,
            "own_speed": kot_with(&spec, &at_own_speed(&w, &ec), cfg, |_| true),
            "stale_course": kot_with(&spec, &stale_in_view(&w), cfg, |_| true),
        });
    } else if cfg.lost_contacts() {
        // no lost contact: the variants differ only in how a live track out of view counts
        let course = kot_with(&spec, &stale_in_view(&w), cfg, |_| true);
        out["lost_variants"] = json!({
            "heading": course,
            "own_speed": course,
            "stale_course": course,
        });
    }
    // the regions together give the engine's verdict (2D: the generated tracks carry no
    // altitude), or the evaluation says so
    let engine_fails = kot.is_some_and(|c| c.outcome != Outcome::Pass);
    if engine_fails != fails {
        mismatch.push(format!(
            "engine keep_out_tracks {}, regions {}",
            kot.map_or("absent", |c| outcome(c.outcome)),
            if fails { "fail" } else { "pass" }
        ));
    }
    intr.sort_by(|a, b| a.0.total_cmp(&b.0));
    let n_intr = intr.len();
    // margins unrounded: a region 4 cm inside is inside
    out["margin_m"] = json!(nearest.map(|(m, _)| m));
    out["nearest"] = json!(nearest.map(|(_, id)| id));
    out["comp"] = json!(comp.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_, _>>());
    out["intr"] = json!(intr
        .into_iter()
        .take(MAX_LISTED)
        .map(|(m, id, label, r)| json!([id, label, m, round1(r)]))
        .collect::<Vec<_>>());
    out["n_intr"] = json!(n_intr);
    out["n_tracks_intr"] = json!(tracks_in);
    out["keep_out_m"] = json!(p.keep_out_m);
    if !mismatch.is_empty() {
        out["mismatch"] = json!(mismatch);
    }
    Ok((out, preview))
}

/// Every evaluation of a situation under every loaded rules version, one JSON value each.
pub fn run_situation(env: &Env, s: &Situation) -> Vec<Value> {
    let now = Time(s.t_ms);
    let mut lines = Vec::new();
    for e in &s.evals {
        let mut line = json!({"id": s.id, "eval": e.key, "platform": e.platform});
        let base = match world(env, s, e, now) {
            Ok(w) => w,
            Err(err) => {
                line["error"] = json!(err);
                lines.push(line);
                continue;
            }
        };
        let mut by_rules = serde_json::Map::new();
        let mut first: Option<engage::EngagePreview> = None;
        let mut differs = false;
        for &v in env.cfgs.keys() {
            match evaluate(env, s, &base, v) {
                Ok((r, pv)) => {
                    match (&first, &pv) {
                        (None, Some(p)) => first = Some(p.clone()),
                        (Some(a), Some(b)) if a.effect_point != b.effect_point || a.effect_t != b.effect_t => differs = true,
                        (Some(_), None) => differs = true,
                        _ => {}
                    }
                    by_rules.insert(v.to_string(), r);
                }
                Err(err) => {
                    line["error"] = json!(err);
                }
            }
        }
        match &first {
            Some(p) => {
                line["preview"] = json!({
                    "effect": [p.effect_point.lon, p.effect_point.lat],
                    "effect_t_ms": p.effect_t.0,
                    "tte_s": round1(p.effect_t.ms_since(now) as f64 / 1000.0),
                    "distance_m": round1(p.distance_m),
                    "keep_out_m": p.keep_out_m,
                });
            }
            None => line["preview"] = Value::Null,
        }
        if differs {
            line["preview_differs"] = json!(true);
        }
        line["rules"] = Value::Object(by_rules);
        lines.push(line);
    }
    lines
}

/// The situation with its protected contacts' facts from the bridge's tracker (when it asks
/// for it), and the tracker's account of them.
pub fn with_tracker(env: &Env, mut s: Situation) -> Result<(Situation, Option<Value>), String> {
    let Some(input) = s.tracker.take() else {
        return Ok((s, None));
    };
    let now_s = s.t_ms as f64 / 1000.0;
    let sim = realtrack::simulate(&env.sensors, &input, now_s).map_err(|e| format!("{}: {e}", s.id))?;
    let target = s.tracks.remove(&s.target);
    s.tracks = sim.facts;
    if let Some(t) = target {
        s.tracks.insert(s.target.clone(), t);
    }
    // rules 16: what the engine keeps of the tracks the bridge dropped
    s.lost = sim
        .lost
        .into_iter()
        .map(|(id, (facts, at))| {
            let dropped_t = Time((at * 1000.0).round() as i64);
            (id, LostContact { dropped_t, facts, ais: None })
        })
        .collect();
    Ok((s, Some(sim.stats)))
}

/// A line of input: a situation, or a question about one track's regions.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Input {
    Regions { regions: Box<RegionQuery> },
    MotionCheck { motion_check: MotionCheck },
    Situation(Box<Situation>),
}

/// Where the probe's copy of each motion ([`realtrack::Motion`]) puts it at `times`: the
/// generator's own formulas must agree (eval/tests/test_keepout.py).
#[derive(Clone, Debug, Deserialize)]
pub struct MotionCheck {
    pub motions: Vec<realtrack::Motion>,
    pub times: Vec<f64>,
}

/// How large each of `track`'s keep-out regions is (`keep_out_sigma` included) and how far its
/// centre is from where the track is now, `spans_s` after its estimate, under every loaded
/// rules version: what the keep-out counts it as, for the report.
#[derive(Clone, Debug, Deserialize)]
pub struct RegionQuery {
    pub name: String,
    pub track: TrackFacts,
    pub spans_s: Vec<f64>,
}

pub fn region_sizes(env: &Env, q: &RegionQuery) -> Value {
    let mut by_rules = serde_json::Map::new();
    for (v, cfg) in &env.cfgs {
        let ec = cfg.engage_rules();
        let labels = components(&q.track, &ec);
        let spans: Vec<Value> = q
            .spans_s
            .iter()
            .map(|s| {
                let t = q.track.position.t.plus_ms((s * 1000.0).round() as i64);
                let regions = engage::track_reach(&q.track, t, &ec);
                let mut out = serde_json::Map::new();
                for (i, r) in regions.iter().enumerate() {
                    let label = labels.get(i).copied().unwrap_or("unknown");
                    out.insert(
                        label.to_string(),
                        json!({"radius_m": round1(r.radius_m), "offset_m": round1(haversine_m(r.center, q.track.position.value))}),
                    );
                }
                json!({"span_s": s, "regions": out})
            })
            .collect();
        by_rules.insert(v.to_string(), Value::Array(spans));
    }
    json!({"regions": q.name, "rules": by_rules})
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut root = PathBuf::from(".");
    let mut versions = vec![6, 9, 12, 14, 16];
    let mut keep_out_m: Option<f64> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => root = args.next().map(PathBuf::from).unwrap_or(root),
            "--rules" => {
                versions = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
            }
            // a copy of the policy with another keep-out buffer, metres
            "--keep-out-m" => keep_out_m = args.next().and_then(|v| v.parse().ok()),
            _ => {
                eprintln!("usage: murmur-keepout-probe [--root DIR] [--rules 6,9,12,14,16] [--keep-out-m M] < situations.jsonl");
                return std::process::ExitCode::from(2);
            }
        }
    }
    let env = match Env::load_with(&root, &versions, keep_out_m) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("murmur-keepout-probe: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    for (n, line) in stdin.lock().lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("murmur-keepout-probe: stdin: {e}");
                return std::process::ExitCode::from(2);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let input: Input = match serde_json::from_str(&line) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("murmur-keepout-probe: situation on line {}: {e}", n + 1);
                return std::process::ExitCode::from(2);
            }
        };
        let lines = match input {
            Input::Regions { regions } => vec![region_sizes(&env, &regions)],
            Input::MotionCheck { motion_check } => vec![json!({
                "positions": motion_check
                    .motions
                    .iter()
                    .map(|m| motion_check.times.iter().map(|t| m.pos(*t)).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            })],
            Input::Situation(s) => {
                let id = s.id.clone();
                match with_tracker(&env, *s) {
                    Ok((s, stats)) => {
                        let mut lines = run_situation(&env, &s);
                        if let Some(st) = stats {
                            lines.push(json!({"id": s.id, "tracker": st}));
                        }
                        lines
                    }
                    Err(e) => vec![json!({"id": id, "error": e})],
                }
            }
        };
        for v in lines {
            if writeln!(out, "{v}").is_err() {
                return std::process::ExitCode::from(2);
            }
        }
    }
    if out.flush().is_err() {
        return std::process::ExitCode::from(2);
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use murmur_decide::catalog::Domain;
    use murmur_decide::world::{Affiliation, Manoeuvre};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    const AT: [f64; 2] = [-122.505, 37.812];

    fn east(m: f64) -> [f64; 2] {
        let p = engage::offset(LonLat::new(AT[0], AT[1]), m, 0.0);
        [p.lon, p.lat]
    }

    fn track(at: [f64; 2], vel: Option<[f64; 2]>, now: Time) -> TrackFacts {
        TrackFacts {
            position: Fact::new(LonLat::new(at[0], at[1]), now),
            alt_m: None,
            alt_t: None,
            velocity_mps: vel,
            pos_sigma_m: 15.0,
            sigma_growth_mps: Some(1.0),
            confidence: 0.9,
            affiliation: Fact::new(Affiliation::Neutral, now),
            affiliation_confidence: 0.8,
            identity: None,
            identity_confidence: 0.2,
            sources: vec!["usv-1:radar".into()],
            source_t: BTreeMap::new(),
            domain: Domain::Surface,
            last_seen_t: now,
            declared_by: None,
            declaration: None,
            review_flags: Vec::new(),
            manoeuvre: None,
            mean_velocity_mps: vel,
            first_seen_t: Some(now.plus_ms(-600_000)),
            velocity_sigma_mps: None,
        }
    }

    fn situation(extra: Vec<(&str, TrackFacts)>, platform_m: f64) -> Situation {
        let now = T0;
        let mut target = track(AT, Some([0.0, 0.0]), now);
        target.affiliation = Fact::new(Affiliation::Hostile, now);
        target.declared_by = Some("op-1".into());
        target.identity = Some("small_fast_boat".into());
        target.identity_confidence = 0.8;
        let mut tracks: BTreeMap<String, TrackFacts> = BTreeMap::new();
        tracks.insert("T-0000".into(), target);
        for (id, t) in extra {
            tracks.insert(id.into(), t);
        }
        Situation {
            id: "test".into(),
            t_ms: now.0,
            target: "T-0000".into(),
            radius_m: 75.0,
            fallback: Fallback::Return,
            tracks,
            vehicles: Vec::new(),
            evals: vec![EvalIn {
                key: "e".into(),
                platform: "magura".into(),
                at: east(-platform_m),
            }],
            tracker: None,
            lost: BTreeMap::new(),
        }
    }

    fn verdicts(s: &Situation) -> BTreeMap<String, (String, Value)> {
        let env = Env::load(&root(), &[6, 9, 12]).expect("env");
        let lines = run_situation(&env, s);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert!(l.get("error").is_none(), "{l}");
        let mut out = BTreeMap::new();
        for (v, r) in l["rules"].as_object().expect("rules") {
            assert!(r.get("mismatch").is_none(), "{r}");
            out.insert(v.clone(), (r["kot"].as_str().unwrap_or("").to_string(), r.clone()));
        }
        out
    }

    #[test]
    fn nobody_protected_never_blocks_on_tracks() {
        let v = verdicts(&situation(Vec::new(), 1200.0));
        for (rules, (kot, r)) in v {
            assert_eq!(kot, "pass", "rules {rules}: {r}");
            assert!(r["margin_m"].is_null());
        }
    }

    #[test]
    fn a_young_track_blocks_from_rules_12_only() {
        // 1.5 km away, holding, tracked 20 s: its young disc (25 m/s) reaches the keep-out
        // at a span of about a minute; along its velocity it stays put
        let now = T0;
        let mut t = track(east(1500.0), Some([0.0, 0.0]), now);
        t.mean_velocity_mps = None;
        t.first_seen_t = Some(now.plus_ms(-20_000));
        let v = verdicts(&situation(vec![("C-0001", t)], 1200.0));
        assert_eq!(v["6"].0, "pass");
        assert_eq!(v["9"].0, "pass");
        assert_eq!(v["12"].0, "fail");
        let intr = v["12"].1["intr"].as_array().expect("intr").clone();
        assert_eq!(intr[0][1], "young");
    }

    #[test]
    fn a_manoeuvring_track_blocks_from_rules_9() {
        let now = T0;
        let mut t = track(east(1000.0), Some([0.0, 3.0]), now);
        t.manoeuvre = Some(Manoeuvre {
            t: now,
            change_mps: 6.0,
            speed_mps: 7.0,
        });
        let v = verdicts(&situation(vec![("C-0001", t)], 1200.0));
        assert_eq!(v["6"].0, "pass");
        assert_eq!(v["9"].0, "fail");
        assert_eq!(v["12"].0, "fail");
        assert_eq!(v["9"].1["intr"][0][1], "manoeuvre");
        // the manoeuvre component is the one inside; the current one is not
        assert!(v["9"].1["comp"]["manoeuvre"].as_f64().expect("m") < 0.0);
        assert!(v["9"].1["comp"]["current"].as_f64().expect("c") > 0.0);
    }

    #[test]
    fn a_contact_next_to_the_target_blocks_under_every_rules() {
        let now = T0;
        let t = track(east(100.0), Some([0.0, 0.0]), now);
        let v = verdicts(&situation(vec![("C-0001", t)], 1200.0));
        for (rules, (kot, r)) in v {
            assert_eq!(kot, "fail", "rules {rules}: {r}");
            assert_eq!(r["intr"][0][1], "current");
        }
    }

    /// Rules 16: a contact the tracker dropped 400 m from the target counts from rules 16 only
    /// (component `lost`), and with it accepted the keep-out holds again.
    #[test]
    fn a_dropped_contact_blocks_from_rules_16_until_accepted() {
        let now = T0;
        let mut s = situation(Vec::new(), 1200.0);
        let mut f = track(east(400.0), Some([0.0, 0.0]), Time(now.0 - 300_000));
        f.pos_sigma_m = 190.0;
        s.lost.insert(
            "C-0001".into(),
            LostContact {
                dropped_t: Time(now.0 - 119_000),
                facts: f,
                ais: None,
            },
        );
        let env = Env::load(&root(), &[14, 16]).expect("env");
        let lines = run_situation(&env, &s);
        let r = &lines[0]["rules"];
        assert_eq!(r["14"]["kot"], "pass", "{r}");
        assert_eq!(r["16"]["kot"], "fail", "{r}");
        assert!(r["16"].get("mismatch").is_none(), "{r}");
        assert_eq!(r["16"]["intr"][0][1], "lost");
        assert_eq!(r["16"]["n_lost_intr"], 1);
        assert_eq!(r["16"]["lost_variants"]["accepted"], "pass");
        assert_eq!(r["16"]["lost_variants"]["held600"], "fail");
        assert_eq!(r["16"]["lost_variants"]["near3km"], "fail");
        assert_eq!(r["16"]["lost_variants"]["heading"], "fail");
    }

    /// The review of murmur-533: a lost contact 3 km east, last seen leaving at 3 m/s 200 s ago:
    /// its own speed bound (5 m/s) keeps it clear of the keep-out by the effect (the `own_speed`
    /// variant passes), its domain's top speed (25 m/s, shipped) does not; a live track out of
    /// view as long counts the same, and by its course alone (`stale_course`) it passes. The
    /// probe's motion for a contact that speeds up after it went out of view is the generator's.
    #[test]
    fn a_lost_contact_counts_at_the_top_speed_and_the_variants_say_what_less_would_do() {
        let now = T0;
        let env = Env::load(&root(), &[14, 16]).expect("env");
        let mut f = track(east(3_000.0), Some([3.0, 0.0]), Time(now.0 - 200_000));
        f.mean_velocity_mps = Some([3.0, 0.0]);
        let mut s = situation(Vec::new(), 1200.0);
        s.lost.insert(
            "C-0001".into(),
            LostContact {
                dropped_t: Time(now.0 - 20_000),
                facts: f.clone(),
                ais: None,
            },
        );
        let lines = run_situation(&env, &s);
        let r = &lines[0]["rules"];
        assert_eq!((r["14"]["kot"].clone(), r["16"]["kot"].clone()), (json!("pass"), json!("fail")), "{r}");
        assert!(r["16"].get("mismatch").is_none(), "{r}");
        assert_eq!(r["16"]["lost_variants"]["own_speed"], "pass", "{r}");
        assert_eq!(r["16"]["lost_variants"]["heading"], "pass", "{r}");
        // live, unseen 200 s (as the generator's stale contacts before their drop)
        let mut s = situation(vec![("C-0001", f)], 1200.0);
        s.lost.clear();
        let lines = run_situation(&env, &s);
        let r = &lines[0]["rules"];
        assert_eq!((r["14"]["kot"].clone(), r["16"]["kot"].clone()), (json!("pass"), json!("fail")), "{r}");
        assert!(r["16"].get("mismatch").is_none(), "{r}");
        assert_eq!(r["16"]["intr"][0][1], "out_of_view", "{r}");
        assert_eq!(r["16"]["lost_variants"]["stale_course"], "pass", "{r}");
        let then = realtrack::Motion::Then {
            base: Box::new(realtrack::Motion::Line { p: [0.0, 0.0], v: [3.0, 0.0] }),
            from_s: 100.0,
            p: [300.0, 0.0],
            v: [0.0, -20.0],
        };
        assert_eq!(then.pos(50.0), [150.0, 0.0]);
        assert_eq!(then.pos(110.0), [300.0, -200.0]);
        assert_eq!(then.vel(110.0), [0.0, -20.0]);
    }

    /// murmur-533's finding: a contact last seen steaming away, its course regions clear of the
    /// keep-out, blocks from rules 16; read as a live track in view (the `heading` variant) it
    /// would pass.
    #[test]
    fn a_lost_contact_last_seen_leaving_blocks() {
        let now = T0;
        let env = Env::load(&root(), &[16]).expect("env");
        let mut s = situation(Vec::new(), 1200.0);
        // 700 m east of the target, leaving east at 6 m/s, last seen 300 s ago
        let mut f = track(east(700.0), Some([6.0, 0.0]), Time(now.0 - 300_000));
        f.position.t = Time(now.0 - 300_000);
        f.mean_velocity_mps = Some([6.0, 0.0]);
        s.lost.insert(
            "C-0001".into(),
            LostContact {
                dropped_t: Time(now.0 - 120_000),
                facts: f,
                ais: None,
            },
        );
        let lines = run_situation(&env, &s);
        let r = &lines[0]["rules"]["16"];
        assert_eq!(r["kot"], "fail", "{r}");
        assert!(r.get("mismatch").is_none(), "{r}");
        assert_eq!(r["lost_variants"]["heading"], "pass", "{r}");
    }

    #[test]
    fn components_follow_the_regions_order() {
        let now = T0;
        let env = Env::load(&root(), &[6, 9, 12]).expect("env");
        let mut t = track(east(500.0), Some([1.0, 0.0]), now);
        t.manoeuvre = Some(Manoeuvre {
            t: now,
            change_mps: 3.0,
            speed_mps: 4.0,
        });
        for (v, cfg) in &env.cfgs {
            let ec = cfg.engage_rules();
            let regions = engage::track_reach(&t, now.plus_ms(60_000), &ec);
            let labels = components(&t, &ec);
            assert_eq!(regions.len(), labels.len(), "rules {v}");
            for (r, l) in regions.iter().zip(&labels) {
                let want = match *l {
                    "current" => "-sigma",
                    "mean" => "average velocity",
                    "manoeuvre" => "manoeuvring",
                    "young" => "young",
                    _ => "no velocity",
                };
                assert!(r.how.contains(want), "rules {v}: {l}: {}", r.how);
            }
        }
    }
}
