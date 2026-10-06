//! Evaluation only (`eval/keepout.py`, METRICS.md "Keep-outs"): generated engagement
//! situations through murmur-decide's own nomination checks, read-only. Nothing here is part
//! of the C2 or decides anything for it; the engine's rules are called, never restated.
//!
//! `murmur-keepout-probe [--root DIR] [--rules 6,9,12]` reads one situation per line on
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
//! `no_velocity` (no velocity estimate: anywhere at its domain's top speed, rules 6).

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
use murmur_decide::world::{Fact, NavSource, TrackFacts, VehicleFacts, WorldSnapshot};
use serde::Deserialize;
use serde_json::{Value, json};

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
}

/// The repository's policy, catalog and areas, and the rules configuration per version.
pub struct Env {
    pub catalog: Catalog,
    pub areas: AreaSet,
    pub terrain: PolygonTerrain,
    pub cfgs: BTreeMap<u32, RulesConfig>,
}

/// The scenario runner's clock origin (`scenario::T0`): areas are loaded at it.
pub const T0: Time = Time::from_secs(1_781_000_000);

impl Env {
    /// `sim/policy.toml` under each rules version as the scenario runner sets one
    /// (`--rules N`: the version, and no `[identification]` before rules 9).
    pub fn load(root: &Path, versions: &[u32]) -> Result<Env, String> {
        let catalog = Catalog::load(&root.join("sim/platforms.toml")).map_err(|e| e.to_string())?;
        let areas = AreaSet::load_dir(&root.join("sim/areas"), T0).map_err(|e| e.to_string())?;
        let base = Policy::load(&root.join("sim/policy.toml")).map_err(|e| e.to_string())?;
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
        Ok(Env {
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
    Ok(w)
}

/// One evaluation under one rules version: the engine's checks, the preview, and each
/// protected track's regions against the keep-out.
pub fn evaluate(env: &Env, s: &Situation, base: &WorldSnapshot, version: u32) -> Result<(Value, Option<engage::EngagePreview>), String> {
    let cfg = env.cfgs.get(&version).ok_or_else(|| format!("rules {version} not loaded"))?;
    let spec = nomination(s, base.t);
    let w = base.for_intent_with(&spec, &env.terrain, cfg);
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
        let regions = engage::track_reach(t, p.effect_t, &ec);
        let labels = components(t, &ec);
        if labels.len() != regions.len() {
            mismatch.push(format!("{id}: {} regions, {} components", regions.len(), labels.len()));
        }
        let mut this_in = false;
        for (i, r) in regions.iter().enumerate() {
            let label = labels.get(i).copied().unwrap_or("unknown");
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
    out["margin_m"] = json!(nearest.map(|(m, _)| round1(m)));
    out["nearest"] = json!(nearest.map(|(_, id)| id));
    out["comp"] = json!(comp.iter().map(|(k, v)| (k.to_string(), round1(*v))).collect::<BTreeMap<_, _>>());
    out["intr"] = json!(intr
        .into_iter()
        .take(MAX_LISTED)
        .map(|(m, id, label, r)| json!([id, label, round1(m), round1(r)]))
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

/// A line of input: a situation, or a question about one track's regions.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Input {
    Regions { regions: Box<RegionQuery> },
    Situation(Box<Situation>),
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
    let mut versions = vec![6, 9, 12];
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
            _ => {
                eprintln!("usage: murmur-keepout-probe [--root DIR] [--rules 6,9,12] < situations.jsonl");
                return std::process::ExitCode::from(2);
            }
        }
    }
    let env = match Env::load(&root, &versions) {
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
            Input::Situation(s) => run_situation(&env, &s),
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
