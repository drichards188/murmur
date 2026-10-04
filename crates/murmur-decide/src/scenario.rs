//! Engagement scenarios (Phase E exit: every invariant has a scenario that tries to break
//! it). A scenario is a TOML file (`sim/scenarios/engage/*.toml`) describing a small world
//! (our vehicles, perception tracks, operators with or without engagement authority, an
//! agent), a timeline of actions (declare, nominate, approve, abort, a track or a vehicle
//! changing, a link going silent, an agent trying to approve), and the designed outcome.
//! [`run`] plays it against a real [`Engine`] on a [`ManualClock`] with the repo's policy,
//! catalog and areas, steps engagements every tick as the bridge does, replays the audit
//! log, and, if the scenario names a `tamper`, attacks the log (with a recomputed hash chain,
//! the strongest attacker short of an external anchor) and checks that replay catches it.
//!
//! No physics and no truth process: tracks move along their velocity and are "detected"
//! every tick unless silenced; the engaging platform flies straight to the effect point at
//! its catalog cruise speed once dispatched, and follows its fallback when stopped (hold in
//! place, return home, or orbit where it is). After the effect the target track stops being
//! detected (`after_effect = "out_of_play"`, the default) or keeps being detected
//! (`"persists"`). It is a decision-layer harness: the live stack (bridge, stand-ins,
//! contacts) runs the same engine.
//!
//! Deterministic: same file, same options, same log. [`RunOptions::seed`] adds seeded
//! Gaussian noise to the track estimates (`track_noise_m`), for batch runs.
//!
//! The format is documented on [`Scenario`] and in `sim/scenarios/engage/README.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::areas::AreaSet;
use crate::audit::{EventBody, MemorySink, parse_log};
use crate::authority::{Grants, OperatorSession, Roster};
use crate::catalog::{Catalog, Domain};
use crate::engage::{self, EngageWindow, EngagementEvent, Fallback, offset};
use crate::engine::{Engine, Order};
use crate::geo::{LonLat, haversine_m};
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::{AgentId, IntentId, OperatorId, Uid};
use crate::intent::{Proposal, Rationale, State, Task};
use crate::policy::Policy;
use crate::replay;
use crate::rules::Outcome;
use crate::terrain::PolygonTerrain;
use crate::time::{ManualClock, Time};
use crate::world::{Affiliation, Fact, TrackFacts, VehicleFacts, WorldSnapshot};

/// Engine time at scenario second 0.
pub const T0: Time = Time::from_secs(1_781_000_000);

/// A scenario file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub title: String,
    /// The invariant it attacks (a rule id such as `keep_out_tracks`, or `two_person`,
    /// `agent_cannot_authorize`, `audit`).
    pub invariant: String,
    /// The attack, in a sentence.
    pub attack: String,
    /// The designed end of the engagement: `rejected`, `voided`, `aborted`, `done`,
    /// `pending_approval`, `expired`.
    pub expect: String,
    /// The check whose failure ends it (rejected, voided, aborted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_rule: Option<String>,
    /// `persists`, `ceased` or `unknown` (done).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_assessment: Option<String>,
    /// A refusal the timeline must have met (a substring of the engine's error), e.g.
    /// "may not approve it".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_refusals: Vec<String>,
    /// The signatures must have been dropped at least once (`approval.reset`): the frozen
    /// plan stopped holding.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_reset: bool,
    /// After the run, attack the log and require replay to catch it:
    /// `proposer_approval`, `agent_approval`, `suppress_void`, `edit_snapshot`,
    /// `drop_signature`, `edit_effect`, `strip_approvers`, `drop_reset`, `agent_order_auto`,
    /// `drop_ack`, `loosen_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tamper: Option<String>,
    /// Seconds to run (default: the last step plus 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<u32>,
    /// What the target does after the effect: `out_of_play` (default: no more detections)
    /// or `persists`.
    #[serde(default = "out_of_play")]
    pub after_effect: String,
    /// Paths relative to the repo root (defaults: sim/policy.toml, sim/platforms.toml,
    /// sim/areas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub areas: Option<String>,
    #[serde(default, rename = "operator")]
    pub operators: Vec<OperatorDef>,
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentDef>,
    #[serde(default, rename = "vehicle")]
    pub vehicles: Vec<VehicleDef>,
    #[serde(default, rename = "track")]
    pub tracks: Vec<TrackDef>,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

fn out_of_play() -> String {
    "out_of_play".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorDef {
    pub id: String,
    /// Engagement authority (`MURMUR_ENGAGE_OPERATORS` on the bridge).
    #[serde(default)]
    pub engage: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDef {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleDef {
    pub uid: String,
    /// Catalog id (`sim/platforms.toml`).
    pub platform: String,
    /// `[lon, lat]`.
    pub at: [f64; 2],
    /// East, north m/s.
    #[serde(default)]
    pub velocity: [f64; 2],
    #[serde(default = "battery")]
    pub battery: f64,
    #[serde(default = "interval")]
    pub report_interval_ms: u32,
}

fn battery() -> f64 {
    95.0
}

fn interval() -> u32 {
    1000
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackDef {
    pub id: String,
    pub at: [f64; 2],
    #[serde(default)]
    pub velocity: [f64; 2],
    #[serde(default = "sigma")]
    pub sigma_m: f64,
    pub affiliation: Affiliation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default = "conf")]
    pub identity_confidence: f64,
    #[serde(default = "conf")]
    pub confidence: f64,
    #[serde(default = "surface")]
    pub domain: Domain,
    /// Seconds since the last detection at the start.
    #[serde(default)]
    pub age_s: f64,
    /// No detections at all (a track the sensors lost).
    #[serde(default)]
    pub silent: bool,
    #[serde(default)]
    pub sources: Vec<String>,
}

fn sigma() -> f64 {
    15.0
}

fn conf() -> f64 {
    0.9
}

fn surface() -> Domain {
    Domain::Surface
}

/// One timeline entry: at `at_s` seconds, do `action`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Step {
    pub at_s: u32,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// An operator declares a track's affiliation (`hostile` is never inferred).
    Declare {
        track: String,
        affiliation: Affiliation,
        by: String,
    },
    /// An operator (`by`) or an agent (`agent`, with a rationale) nominates an engagement.
    Nominate {
        #[serde(default)]
        by: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
        platform: String,
        track: String,
        radius_m: f64,
        fallback: Fallback,
        #[serde(default)]
        window_s: Option<u32>,
    },
    /// An operator with engagement authority asks for the picture of the last nomination
    /// (a picture token, as the console does before a signature); the next `approve` by them
    /// quotes it.
    Look { by: String },
    /// An operator signs the last nomination (with engagement authority if they have it;
    /// without it the engine refuses), quoting the picture token from their last `look`, or
    /// one asked for now (the console's look-then-sign in one step).
    Approve { by: String },
    /// An operator approves the last `task` (an order waiting in the inbox); with
    /// `acknowledge`, accepting that its dispatch stops an engagement in play.
    ApproveTask {
        by: String,
        #[serde(default)]
        acknowledge: bool,
    },
    /// Another order on some of our vehicles (hold or rtl) from an operator (`by`) or an
    /// agent (`agent`, with a rationale). Dispatched at once if its tier lets it run (as on
    /// the bridge), superseding any engagement on those vehicles.
    Task {
        #[serde(default)]
        by: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
        task: String,
        uids: Vec<String>,
    },
    /// The loop stalls: no engagement step for `seconds` (a bridge that stopped holding the
    /// picture).
    Stall { seconds: u32 },
    Reject { by: String },
    /// Any operator stops it.
    Abort { by: String },
    /// An agent tries to approve: there is no agent path to approval (the runner records
    /// that the agent's port has no such method, and the intent must not move).
    AgentApprove { agent: String },
    /// A track changes (or appears): any field given replaces the current one.
    Track {
        id: String,
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        velocity: Option<[f64; 2]>,
        #[serde(default)]
        affiliation: Option<Affiliation>,
        #[serde(default)]
        declared_by: Option<String>,
        #[serde(default)]
        undeclare: bool,
        #[serde(default)]
        identity: Option<String>,
        #[serde(default)]
        identity_confidence: Option<f64>,
        #[serde(default)]
        sigma_m: Option<f64>,
        #[serde(default)]
        domain: Option<Domain>,
        /// Stop (true) or resume (false) detections.
        #[serde(default)]
        silent: Option<bool>,
        /// The tracker's velocity estimate is off by this much (east, north m/s) from the
        /// contact's real velocity, until set again: an estimate that wobbles, not a turn.
        #[serde(default)]
        velocity_error: Option<[f64; 2]>,
    },
    /// One of our vehicles changes: position, velocity, link (silent: no reports; a report
    /// interval: the link class changed), navigation source (`gnss`, `dead_reckoning`).
    Vehicle {
        uid: String,
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        velocity: Option<[f64; 2]>,
        #[serde(default)]
        silent: Option<bool>,
        #[serde(default)]
        report_interval_ms: Option<u32>,
        #[serde(default)]
        nav: Option<crate::world::NavSource>,
        /// The GPS receiver's fix type (3: a 3D fix).
        #[serde(default)]
        gps_fix: Option<u8>,
        /// The navigation filter flags the GPS as glitching.
        #[serde(default)]
        gps_glitch: Option<bool>,
        /// Fly a circle `[lon, lat, radius_m]` at the speed of its velocity (the bridge
        /// reports it as the vehicle's `circling` fact); a radius of 0 stops circling.
        #[serde(default)]
        circling: Option<[f64; 3]>,
        /// Report a circle `[lon, lat, radius_m]` the vehicle does not fly (a wrong or stale
        /// fact: it keeps its own position and velocity); a radius of 0 stops reporting it.
        #[serde(default)]
        reported_circle: Option<[f64; 3]>,
    },
    /// Check the last nomination now.
    Expect {
        #[serde(default)]
        state: Option<String>,
        #[serde(default)]
        rule: Option<String>,
        #[serde(default)]
        concurrences: Option<usize>,
        /// The state of the last `task` step's intent.
        #[serde(default)]
        task_state: Option<String>,
    },
}

/// Options for batch runs.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Seed for the track estimate noise (none: no noise).
    pub seed: Option<u64>,
    /// 1-sigma Gaussian noise added to every track position estimate each tick, metres
    /// (independent around the truth).
    pub track_noise_m: f64,
    /// 1-sigma error of every track's velocity estimate, m/s, correlated over about 10 s
    /// (as a tracker's smoothed velocity wobbles: a first-order Gauss-Markov process).
    pub velocity_noise_mps: f64,
}

/// What a run produced.
#[derive(Clone, Debug, Serialize)]
pub struct ScenarioResult {
    pub file: String,
    pub title: String,
    pub invariant: String,
    pub attack: String,
    /// The designed outcome held, replay reproduced every decision, and a tamper (if any)
    /// was caught.
    pub ok: bool,
    pub failures: Vec<String>,
    pub expect: String,
    pub final_state: Option<String>,
    pub final_reason: Option<String>,
    /// The checks that failed at the end (submission checks for a rejection, the picture
    /// for a void or abort).
    pub failed_rules: Vec<String>,
    pub assessment: Option<String>,
    pub refusals: Vec<String>,
    pub fallback: Option<String>,
    pub replay_ok: bool,
    pub replay_events: usize,
    pub replay_mismatches: Vec<String>,
    pub tamper: Option<String>,
    /// Replay caught the forged log (none: no tamper, or nothing in this run's log for it to
    /// edit, see `tamper_not_applicable`).
    pub tamper_detected: Option<bool>,
    /// Why the tamper had nothing to edit in this run's log (a seeded run can end
    /// differently from the designed one).
    pub tamper_not_applicable: Option<String>,
    pub tamper_mismatches: Vec<String>,
    /// How many times the signatures were dropped (`approval.reset`).
    pub resets: usize,
    /// The audit log, for the harness to keep.
    #[serde(skip)]
    pub log: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0}: {1}")]
    Parse(String, toml::de::Error),
    #[error("{0}: {1}")]
    Setup(String, String),
}

/// Load one scenario file.
pub fn load(path: &Path) -> Result<Scenario, ScenarioError> {
    let name = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| ScenarioError::Io(name.clone(), e))?;
    toml::from_str(&text).map_err(|e| ScenarioError::Parse(name, e))
}

/// Every `*.toml` in a directory, in name order.
pub fn files(dir: &Path) -> Result<Vec<PathBuf>, ScenarioError> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| ScenarioError::Io(dir.display().to_string(), e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    Ok(v)
}

/// Load and run a file; `root` is the repo (for sim/policy.toml and friends).
pub fn run_file(path: &Path, root: &Path, opts: &RunOptions) -> Result<ScenarioResult, ScenarioError> {
    let sc = load(path)?;
    run(&sc, &path.display().to_string(), root, opts)
}

// ---- the run --------------------------------------------------------------------------------

struct VehicleSim {
    def: VehicleDef,
    nav: crate::world::NavSource,
    gps_fix: u8,
    gps_glitch: bool,
    /// the circle reported for it (center, radius m), and whether it flies it
    circling: Option<(LonLat, f64)>,
    circling_flown: bool,
    pos: LonLat,
    vel: [f64; 2],
    home: LonLat,
    silent: bool,
    interval: u32,
    last_t: Time,
    /// where the platform is flying (dispatched engagement, or fallback home)
    goal: Option<LonLat>,
    orbit: bool,
}

struct TrackSim {
    facts: TrackFacts,
    /// Where the contact really is and how it moves (the estimates are these plus the
    /// run's noise and any scripted velocity error).
    truth: LonLat,
    truth_vel: [f64; 2],
    vel_error: [f64; 2],
    /// The correlated part of the velocity estimate's error (`velocity_noise_mps`).
    vel_noise: [f64; 2],
    silent: bool,
}

/// Golden Gate land/water: land south of the Presidio shore and on Marin, water between.
fn terrain() -> PolygonTerrain {
    use crate::geo::Polygon;
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

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        let u1 = self.next().max(1e-12);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Play a scenario. `file` is only for the result.
pub fn run(
    sc: &Scenario,
    file: &str,
    root: &Path,
    opts: &RunOptions,
) -> Result<ScenarioResult, ScenarioError> {
    let setup = |e: String| ScenarioError::Setup(file.to_string(), e);
    let rel = |p: &Option<String>, d: &str| root.join(p.as_deref().unwrap_or(d));
    let policy = Policy::load(&rel(&sc.policy, "sim/policy.toml")).map_err(|e| setup(e.to_string()))?;
    let catalog =
        Catalog::load(&rel(&sc.catalog, "sim/platforms.toml")).map_err(|e| setup(e.to_string()))?;
    let areas = AreaSet::load_dir(&rel(&sc.areas, "sim/areas"), T0).map_err(|e| setup(e.to_string()))?;
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    // the operators the file lists, those with engagement authority on the engagement list
    // (the bridge's MURMUR_OPERATORS and MURMUR_ENGAGE_OPERATORS)
    let roster = Roster::new(
        sc.operators.iter().map(|o| OperatorId::new(&o.id)).collect(),
        sc.operators
            .iter()
            .filter(|o| o.engage)
            .map(|o| OperatorId::new(&o.id))
            .collect(),
    )
    .map_err(setup)?;
    let mut engine = Engine::with_areas(
        policy,
        Some(areas.clone()),
        roster,
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .map_err(|e| setup(e.to_string()))?;
    let terrain = terrain();

    let mut sessions: BTreeMap<String, OperatorSession> = BTreeMap::new();
    for o in &sc.operators {
        let s = engine
            .open_operator_session_with(OperatorId::new(&o.id), Grants { engage: o.engage })
            .map_err(|e| setup(e.to_string()))?;
        sessions.insert(o.id.clone(), s);
    }
    let mut vehicles: BTreeMap<String, VehicleSim> = BTreeMap::new();
    for v in &sc.vehicles {
        if catalog.get(&v.platform).is_none() {
            return Err(setup(format!("vehicle {}: platform {} not in the catalog", v.uid, v.platform)));
        }
        let at = LonLat::new(v.at[0], v.at[1]);
        vehicles.insert(
            v.uid.clone(),
            VehicleSim {
                def: v.clone(),
                nav: crate::world::NavSource::Gnss,
                gps_fix: 3,
                gps_glitch: false,
                circling: None,
                circling_flown: false,
                pos: at,
                vel: v.velocity,
                home: at,
                silent: false,
                interval: v.report_interval_ms,
                last_t: T0,
                goal: None,
                orbit: false,
            },
        );
    }
    let mut tracks: BTreeMap<String, TrackSim> = BTreeMap::new();
    for t in &sc.tracks {
        let mut sim = track_sim(t);
        // a tracker fed noisier detections says so in its position uncertainty
        if opts.seed.is_some() {
            sim.facts.pos_sigma_m = sim.facts.pos_sigma_m.max(opts.track_noise_m);
        }
        tracks.insert(t.id.clone(), sim);
    }

    let end_s = sc
        .end_s
        .unwrap_or_else(|| sc.steps.iter().map(|s| s.at_s).max().unwrap_or(0) + 10);
    let mut rng = Rng(opts.seed.unwrap_or(0));
    let mut failures: Vec<String> = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    let mut last: Option<IntentId> = None;
    let mut target_out: Option<String> = None;
    let mut fallback_seen: Option<String> = None;
    let mut engaging: BTreeMap<IntentId, (String, LonLat)> = BTreeMap::new();
    let mut extra = Extra::default();

    for s in 0..=end_s {
        let now = T0.plus_ms(i64::from(s) * 1000);
        clock.set(now);
        // kinematics: the contacts move along their velocity; a detection estimates where
        // one is (with independent noise around the truth each time), unless silent
        for (id, tr) in &mut tracks {
            let [e, n] = tr.truth_vel;
            if s > 0 {
                tr.truth = offset(tr.truth, e, n);
            }
            let silent = tr.silent || target_out.as_deref() == Some(id.as_str());
            if s > 0 && !silent {
                // the velocity estimate: the real one, plus a scripted error, plus a
                // wobble correlated over about 10 s
                if opts.velocity_noise_mps > 0.0 && opts.seed.is_some() {
                    let rho = (-0.1f64).exp();
                    let k = (1.0 - rho * rho).sqrt() * opts.velocity_noise_mps;
                    tr.vel_noise = [
                        rho * tr.vel_noise[0] + k * rng.gauss(),
                        rho * tr.vel_noise[1] + k * rng.gauss(),
                    ];
                }
                tr.facts.velocity_mps = Some([
                    tr.truth_vel[0] + tr.vel_error[0] + tr.vel_noise[0],
                    tr.truth_vel[1] + tr.vel_error[1] + tr.vel_noise[1],
                ]);
                let noisy = if opts.track_noise_m > 0.0 && opts.seed.is_some() {
                    offset(
                        tr.truth,
                        rng.gauss() * opts.track_noise_m,
                        rng.gauss() * opts.track_noise_m,
                    )
                } else {
                    tr.truth
                };
                tr.facts.position = Fact::new(noisy, now);
                tr.facts.last_seen_t = now;
            } else if s > 0 {
                // unobserved: the estimate stays where it was last seen
            }
        }
        // our vehicles
        for v in vehicles.values_mut() {
            let cruise = catalog
                .get(&v.def.platform)
                .and_then(|p| p.cruise_mps)
                .unwrap_or(0.0);
            if let Some(goal) = v.goal {
                let d = haversine_m(v.pos, goal);
                let step = cruise.min(d);
                if d > 1.0 && step > 0.0 {
                    let (ge, gn) = engage::enu(v.pos, goal);
                    let k = step / d;
                    v.vel = [ge / d * cruise, gn / d * cruise];
                    v.pos = offset(v.pos, ge * k, gn * k);
                } else {
                    v.vel = [0.0, 0.0];
                    if !v.orbit {
                        v.goal = None;
                    }
                }
            } else if let Some((c, radius)) = v.circling.filter(|_| s > 0 && v.circling_flown) {
                // a second around the circle, clockwise, at its speed
                let speed = v.vel[0].hypot(v.vel[1]);
                let (ce, cn) = engage::enu(c, v.pos);
                let a = ce.atan2(cn) + speed / radius.max(1.0);
                v.pos = offset(c, radius * a.sin(), radius * a.cos());
                v.vel = [speed * a.cos(), -speed * a.sin()];
            } else if s > 0 {
                v.pos = offset(v.pos, v.vel[0], v.vel[1]);
            }
            if !v.silent {
                v.last_t = now;
            }
        }
        // the timeline
        for step in sc.steps.iter().filter(|st| st.at_s == s) {
            act(
                &step.action,
                &mut engine,
                &sessions,
                &mut tracks,
                &mut vehicles,
                &catalog,
                &areas,
                now,
                &terrain,
                &mut last,
                &mut refusals,
                &mut fallback_seen,
                &mut failures,
                &mut engaging,
                &mut extra,
                s,
            );
        }
        // the bridge's loop: expire, then hold the picture / release / observe
        let _ = engine.expire_due();
        let world = snapshot(&vehicles, &tracks, &catalog, &areas, now);
        let outcome = if s < extra.stall_until {
            crate::engine::StepOutcome::default()
        } else {
            engine.step_engagements(&world, &terrain)
        };
        for (id, e) in &outcome.errors {
            failures.push(format!("t={s}: step_engagements {id}: {e}"));
        }
        {
            {
                for ev in outcome.events {
                    match ev {
                        EngagementEvent::Stopped(f) => {
                            fallback_seen = Some(f.fallback.as_str().into());
                            if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
                                match f.fallback {
                                    Fallback::Abort => {
                                        v.goal = None;
                                        v.vel = [0.0, 0.0];
                                    }
                                    Fallback::Return => {
                                        v.goal = Some(v.home);
                                        v.orbit = false;
                                    }
                                    Fallback::Loiter => {
                                        v.goal = Some(v.pos);
                                        v.orbit = true;
                                    }
                                }
                            }
                            engaging.remove(&f.intent);
                        }
                        EngagementEvent::Effect(e) => {
                            if sc.after_effect != "persists" {
                                target_out = Some(e.track.clone());
                            }
                        }
                        EngagementEvent::Voided { .. } | EngagementEvent::Assessed { .. } => {}
                    }
                }
            }
        }
        // anything approved goes out at once, as on the bridge
        dispatch_approved(&mut engine, &mut vehicles, &mut engaging, &mut failures, s);
    }

    // the outcome
    let intent = last.and_then(|id| engine.intent(id));
    let final_state = intent.map(|i| state_str(i.state()));
    let final_reason = intent.and_then(|i| i.history().last().map(|h| h.reason.clone()));
    let mut failed_rules: Vec<String> = Vec::new();
    if let Some(i) = intent {
        let from_watch = i
            .engagement()
            .and_then(|e| e.watch.as_ref())
            .filter(|w| !w.holds)
            .map(|w| w.results.clone());
        let results = from_watch.unwrap_or_else(|| i.checks().to_vec());
        for r in results {
            if r.outcome != Outcome::Pass && !failed_rules.contains(&r.rule.as_str().to_string()) {
                failed_rules.push(r.rule.as_str().into());
            }
        }
    }
    let assessment = intent
        .and_then(|i| i.engagement())
        .and_then(|e| e.assessment.as_ref())
        .map(|a| a.result.as_str().to_string());
    if final_state.as_deref() != Some(sc.expect.as_str()) {
        failures.push(format!(
            "expected the engagement to end {}, it is {} ({})",
            sc.expect,
            final_state.as_deref().unwrap_or("absent"),
            final_reason.as_deref().unwrap_or("")
        ));
    }
    if let Some(rule) = &sc.expect_rule
        && !failed_rules.contains(rule)
    {
        failures.push(format!(
            "expected check {rule} to fail, failed: [{}]",
            failed_rules.join(", ")
        ));
    }
    if let Some(a) = &sc.expect_assessment
        && assessment.as_deref() != Some(a.as_str())
    {
        failures.push(format!(
            "expected assessment {a}, got {}",
            assessment.as_deref().unwrap_or("none")
        ));
    }
    for want in &sc.expect_refusals {
        if !refusals.iter().any(|r| r.contains(want.as_str())) {
            failures.push(format!("expected a refusal containing {want:?}, got {refusals:?}"));
        }
    }
    let text = log.text();
    let resets = text
        .lines()
        .filter(|l| l.contains("\"type\":\"approval.reset\""))
        .count();
    if sc.expect_reset && resets == 0 {
        failures.push("expected the signatures to be dropped (approval.reset), they never were".into());
    }
    let report = replay::replay_str(&text).map_err(|e| setup(e.to_string()))?;
    let replay_mismatches: Vec<String> = report
        .mismatches
        .iter()
        .map(|m| format!("seq {}: {}", m.seq, m.what))
        .collect();
    if !report.ok() {
        failures.push(format!("replay: {}", replay_mismatches.join("; ")));
    }
    let mut tamper_not_applicable = None;
    let (tamper_detected, tamper_mismatches) = match &sc.tamper {
        None => (None, Vec::new()),
        Some(kind) => match tamper(&text, kind) {
            Err(e) => {
                failures.push(format!("tamper {kind}: {e}"));
                (Some(false), Vec::new())
            }
            Ok(Tampered::NotApplicable(why)) => {
                tamper_not_applicable = Some(why);
                (None, Vec::new())
            }
            Ok(Tampered::Forged(forged)) => {
                let r = replay::replay_str(&forged).map_err(|e| setup(e.to_string()))?;
                let ms: Vec<String> = r
                    .mismatches
                    .iter()
                    .map(|m| format!("seq {}: {}", m.seq, m.what))
                    .collect();
                if r.ok() {
                    failures.push(format!("tamper {kind}: replay did not catch it"));
                }
                (Some(!r.ok()), ms)
            }
        },
    };
    Ok(ScenarioResult {
        file: file.to_string(),
        title: sc.title.clone(),
        invariant: sc.invariant.clone(),
        attack: sc.attack.clone(),
        ok: failures.is_empty(),
        failures,
        expect: sc.expect.clone(),
        final_state,
        final_reason,
        failed_rules,
        assessment,
        refusals,
        fallback: fallback_seen,
        replay_ok: report.ok(),
        replay_events: report.events,
        replay_mismatches,
        tamper: sc.tamper.clone(),
        tamper_detected,
        tamper_not_applicable,
        tamper_mismatches,
        resets,
        log: text,
    })
}

fn track_sim(t: &TrackDef) -> TrackSim {
    let seen = T0.plus_ms(-((t.age_s * 1000.0).round() as i64));
    let speed = t.velocity[0].hypot(t.velocity[1]);
    TrackSim {
        truth: LonLat::new(t.at[0], t.at[1]),
        truth_vel: t.velocity,
        vel_error: [0.0, 0.0],
        vel_noise: [0.0, 0.0],
        facts: TrackFacts {
            position: Fact::new(LonLat::new(t.at[0], t.at[1]), seen),
            alt_m: None,
            velocity_mps: Some(t.velocity),
            pos_sigma_m: t.sigma_m,
            sigma_growth_mps: Some((0.3 * speed).max(1.0)),
            confidence: t.confidence,
            affiliation: Fact::new(t.affiliation, seen),
            affiliation_confidence: 0.8,
            identity: t.identity.clone(),
            identity_confidence: t.identity_confidence,
            sources: t.sources.clone(),
            domain: t.domain,
            last_seen_t: seen,
            declared_by: t.declared_by.clone(),
        },
        silent: t.silent,
    }
}

fn state_str(s: State) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The world the engine reads at `now`.
fn snapshot(
    vehicles: &BTreeMap<String, VehicleSim>,
    tracks: &BTreeMap<String, TrackSim>,
    catalog: &Catalog,
    areas: &AreaSet,
    now: Time,
) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(now);
    w.areas = Some(areas.clone());
    for (uid, v) in vehicles {
        let p = catalog.get(&v.def.platform).cloned();
        let ap = p.as_ref().and_then(|p| p.autopilot);
        let t = v.last_t;
        w.vehicles.insert(
            Uid::new(uid),
            VehicleFacts {
                autopilot: ap.map(|a| Fact::new(a, t)),
                nav_ready: Some(Fact::new(true, t)),
                position: Some(Fact::new(v.pos, t)),
                home: Some(Fact::new(v.home, T0)),
                battery_pct: Some(Fact::new(v.def.battery, t)),
                nav_source: Some(Fact::new(v.nav, t)),
                gps_fix: Some(Fact::new(v.gps_fix, t)),
                gps_glitch: Some(Fact::new(v.gps_glitch, t)),
                gps_fix_from: None,
                circling: v.circling.map(|(center, radius_m)| {
                    let source = Some(crate::world::CircleSource::OrbitTask);
                    let circle = crate::world::Circle {
                        center,
                        radius_m,
                        source,
                        center_t: None,
                    };
                    Fact::new(circle, t)
                }),
                report_interval_ms: Some(v.interval),
                velocity_mps: Some(Fact::new(v.vel, t)),
                platform: p,
                booted_t: None,
            },
        );
    }
    for (id, tr) in tracks {
        let mut f = tr.facts.clone();
        // uncertainty and existence as the tracker carries them to now
        let age = now.ms_since(f.last_seen_t) as f64 / 1000.0;
        f.pos_sigma_m += engage::sigma_growth(&f) * age.max(0.0);
        f.confidence *= (-age.max(0.0) / 60.0).exp();
        w.tracks.insert(id.clone(), f);
    }
    w
}

#[allow(clippy::too_many_arguments)]
fn act(
    a: &Action,
    engine: &mut Engine,
    sessions: &BTreeMap<String, OperatorSession>,
    tracks: &mut BTreeMap<String, TrackSim>,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    catalog: &Catalog,
    areas: &AreaSet,
    now: Time,
    terrain: &PolygonTerrain,
    last: &mut Option<IntentId>,
    refusals: &mut Vec<String>,
    fallback_seen: &mut Option<String>,
    failures: &mut Vec<String>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    extra: &mut Extra,
    s: u32,
) {
    let world = |tracks: &BTreeMap<String, TrackSim>, vehicles: &BTreeMap<String, VehicleSim>| {
        snapshot(vehicles, tracks, catalog, areas, now)
    };
    let session = |by: &str| sessions.get(by);
    match a {
        Action::Declare {
            track,
            affiliation,
            by,
        } => {
            let Some(sess) = session(by) else {
                return failures.push(format!("t={s}: unknown operator {by}"));
            };
            let Some(t) = tracks.get_mut(track) else {
                return failures.push(format!("t={s}: unknown track {track}"));
            };
            t.facts.affiliation = Fact::new(*affiliation, now);
            t.facts.declared_by = Some(by.clone());
            let _ = engine.record_operator_action(
                sess.authority(),
                "track.declare",
                &format!("track {track} declared {}", affiliation.as_str()),
            );
        }
        Action::Nominate {
            by,
            agent,
            rationale,
            platform,
            track,
            radius_m,
            fallback,
            window_s,
        } => {
            let task = Task::Engage {
                track: track.clone(),
                effect_radius_m: *radius_m,
                window: window_s.map(|w| EngageWindow {
                    from: None,
                    until: now.plus_ms(i64::from(w) * 1000),
                }),
                fallback: *fallback,
            };
            let mut proposal = Proposal::new(task, vec![Uid::new(platform)]);
            let w = world(tracks, vehicles);
            let out = match (by, agent) {
                (Some(by), _) => match session(by) {
                    Some(sess) => {
                        proposal.rationale = rationale.as_deref().map(Rationale::new).transpose().ok().flatten();
                        engine
                            .submit(sess.authority(), proposal, &w, terrain)
                            .map(|i| i.id())
                    }
                    None => return failures.push(format!("t={s}: unknown operator {by}")),
                },
                (None, Some(agent)) => {
                    let Ok(r) = Rationale::new(rationale.clone().unwrap_or_default()) else {
                        refusals.push("an agent's proposal needs a rationale".into());
                        return;
                    };
                    engine
                        .agent_port(AgentId::new(agent))
                        .propose(proposal, r, &w, terrain)
                        .map(|i| i.id())
                }
                (None, None) => return failures.push(format!("t={s}: nominate needs by or agent")),
            };
            match out {
                Ok(id) => *last = Some(id),
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Look { by } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: look: no nomination or unknown operator {by}"));
            };
            let Some(auth) = sess.engagement_authority() else {
                refusals.push(format!("{by} has no engagement authority: no picture to sign"));
                return;
            };
            let w = world(tracks, vehicles);
            match engine.issue_picture(id, auth, &w) {
                Ok(t) => {
                    extra.looked.insert(by.clone(), t.id);
                }
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Approve { by } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: approve: no nomination or unknown operator {by}"));
            };
            let w = world(tracks, vehicles);
            let r = match sess.engagement_authority() {
                Some(auth) => {
                    // the picture from their last look, or one now
                    let token = match extra.looked.remove(by) {
                        Some(t) => Ok(t),
                        None => engine.issue_picture(id, auth, &w).map(|t| t.id),
                    };
                    token.and_then(|t| engine.approve_engagement(id, auth, None, t, &w, terrain).map(|_| ()))
                }
                None => engine.approve(id, sess.authority(), None, &w, terrain).map(|_| ()),
            };
            if let Err(e) = r {
                refusals.push(e.to_string());
            }
        }
        Action::ApproveTask { by, acknowledge } => {
            let (Some(id), Some(sess)) = (extra.last_task, session(by)) else {
                return failures.push(format!("t={s}: approve_task: no task or unknown operator {by}"));
            };
            let w = world(tracks, vehicles);
            let r = if *acknowledge {
                engine.approve_acknowledging(id, sess.authority(), None, &w, terrain)
            } else {
                engine.approve(id, sess.authority(), None, &w, terrain)
            };
            if let Err(e) = r {
                refusals.push(e.to_string());
            }
        }
        Action::Task {
            by,
            agent,
            rationale,
            task,
            uids,
        } => {
            let t = match task.as_str() {
                "hold" => Task::Hold {},
                "rtl" => Task::Rtl {},
                other => return failures.push(format!("t={s}: task {other}: hold or rtl")),
            };
            let proposal = Proposal::new(t, uids.iter().map(Uid::new).collect());
            let w = world(tracks, vehicles);
            let out = match (by, agent) {
                (Some(by), _) => match session(by) {
                    Some(sess) => engine.submit(sess.authority(), proposal, &w, terrain).map(|i| i.id()),
                    None => return failures.push(format!("t={s}: unknown operator {by}")),
                },
                (None, Some(agent)) => {
                    let Ok(r) = Rationale::new(rationale.clone().unwrap_or_default()) else {
                        refusals.push("an agent's proposal needs a rationale".into());
                        return;
                    };
                    engine
                        .agent_port(AgentId::new(agent))
                        .propose(proposal, r, &w, terrain)
                        .map(|i| i.id())
                }
                (None, None) => return failures.push(format!("t={s}: task needs by or agent")),
            };
            match out {
                Ok(id) => extra.last_task = Some(id),
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Stall { seconds } => extra.stall_until = s + seconds,
        Action::Reject { by } => {
            if let (Some(id), Some(sess)) = (*last, session(by))
                && let Err(e) = engine.reject(id, sess.authority(), None)
            {
                refusals.push(e.to_string());
            }
        }
        Action::Abort { by } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: abort: no nomination or unknown operator {by}"));
            };
            match engine.abort_engagement(id, sess.authority(), "operator abort") {
                Ok(Some(f)) => {
                    *fallback_seen = Some(f.fallback.as_str().into());
                    if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
                        match f.fallback {
                            Fallback::Abort => {
                                v.goal = None;
                                v.vel = [0.0, 0.0];
                            }
                            Fallback::Return => {
                                v.goal = Some(v.home);
                                v.orbit = false;
                            }
                            Fallback::Loiter => {
                                v.goal = Some(v.pos);
                                v.orbit = true;
                            }
                        }
                    }
                    engaging.remove(&id);
                }
                Ok(None) => {}
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::AgentApprove { agent } => {
            // The agent's whole surface is AgentPort: propose, preview, withdraw, record,
            // read. None of them approves; the attempt is recorded and the intent must not
            // move.
            let before = last.and_then(|id| engine.intent(id)).map(|i| i.state());
            let _ = engine
                .agent_port(AgentId::new(agent))
                .record_action("intent.approve.attempt", "no agent path to approval exists");
            refusals.push(format!(
                "agent {agent}: no approve method on AgentPort (the attempt cannot be expressed)"
            ));
            let after = last.and_then(|id| engine.intent(id)).map(|i| i.state());
            if before != after {
                failures.push(format!("t={s}: the intent moved on an agent's attempt"));
            }
        }
        Action::Track {
            id,
            at,
            velocity,
            affiliation,
            declared_by,
            undeclare,
            identity,
            identity_confidence,
            sigma_m,
            domain,
            silent,
            velocity_error,
        } => {
            let t = tracks.entry(id.clone()).or_insert_with(|| {
                track_sim(&TrackDef {
                    id: id.clone(),
                    at: at.unwrap_or([0.0, 0.0]),
                    velocity: velocity.unwrap_or([0.0, 0.0]),
                    sigma_m: sigma_m.unwrap_or(15.0),
                    affiliation: affiliation.unwrap_or(Affiliation::Unknown),
                    declared_by: None,
                    identity: identity.clone(),
                    identity_confidence: identity_confidence.unwrap_or(0.9),
                    confidence: 0.9,
                    domain: domain.unwrap_or(Domain::Surface),
                    age_s: 0.0,
                    silent: false,
                    sources: vec!["sensor".into()],
                })
            });
            if let Some(p) = at {
                t.truth = LonLat::new(p[0], p[1]);
                t.facts.position = Fact::new(t.truth, now);
                t.facts.last_seen_t = now;
            }
            if let Some(v) = velocity {
                t.truth_vel = *v;
                t.facts.velocity_mps = Some(*v);
                t.facts.sigma_growth_mps = Some((0.3 * v[0].hypot(v[1])).max(1.0));
            }
            if let Some(err) = velocity_error {
                t.vel_error = *err;
                t.facts.velocity_mps = Some([
                    t.truth_vel[0] + err[0] + t.vel_noise[0],
                    t.truth_vel[1] + err[1] + t.vel_noise[1],
                ]);
            }
            if let Some(a) = affiliation {
                t.facts.affiliation = Fact::new(*a, now);
            }
            if let Some(b) = declared_by {
                t.facts.declared_by = Some(b.clone());
            }
            if *undeclare {
                t.facts.declared_by = None;
                t.facts.affiliation = Fact::new(Affiliation::Suspect, now);
            }
            if let Some(k) = identity {
                t.facts.identity = Some(k.clone());
            }
            if let Some(c) = identity_confidence {
                t.facts.identity_confidence = *c;
            }
            if let Some(sg) = sigma_m {
                t.facts.pos_sigma_m = *sg;
            }
            if let Some(d) = domain {
                t.facts.domain = *d;
            }
            if let Some(si) = silent {
                t.silent = *si;
            }
        }
        Action::Vehicle {
            uid,
            at,
            velocity,
            silent,
            report_interval_ms,
            nav,
            gps_fix,
            gps_glitch,
            circling,
            reported_circle,
        } => {
            let Some(v) = vehicles.get_mut(uid) else {
                return failures.push(format!("t={s}: unknown vehicle {uid}"));
            };
            if let Some(p) = at {
                v.pos = LonLat::new(p[0], p[1]);
            }
            if let Some(vel) = velocity {
                v.vel = *vel;
            }
            if let Some(si) = silent {
                v.silent = *si;
            }
            if let Some(i) = report_interval_ms {
                v.interval = *i;
            }
            if let Some(n) = nav {
                v.nav = *n;
            }
            if let Some(f) = gps_fix {
                v.gps_fix = *f;
            }
            if let Some(g) = gps_glitch {
                v.gps_glitch = *g;
            }
            if let Some([lon, lat, radius]) = circling {
                v.circling = (*radius > 0.0).then(|| (LonLat::new(*lon, *lat), *radius));
                v.circling_flown = true;
            }
            if let Some([lon, lat, radius]) = reported_circle {
                v.circling = (*radius > 0.0).then(|| (LonLat::new(*lon, *lat), *radius));
                v.circling_flown = false;
            }
        }
        Action::Expect {
            state,
            rule,
            concurrences,
            task_state,
        } => {
            if let Some(want) = task_state {
                let got = extra.last_task.and_then(|id| engine.intent(id)).map(|i| state_str(i.state()));
                if got.as_deref() != Some(want.as_str()) {
                    failures.push(format!(
                        "t={s}: expected the task {want}, got {}",
                        got.as_deref().unwrap_or("none")
                    ));
                }
            }
            let i = last.and_then(|id| engine.intent(id));
            let got = i.map(|i| state_str(i.state()));
            if let Some(want) = state
                && got.as_deref() != Some(want.as_str())
            {
                failures.push(format!(
                    "t={s}: expected {want}, got {} ({})",
                    got.as_deref().unwrap_or("none"),
                    i.and_then(|i| i.history().last().map(|h| h.reason.clone()))
                        .unwrap_or_default()
                ));
            }
            if let Some(n) = concurrences
                && i.map(|i| i.concurrences().len()) != Some(*n)
            {
                failures.push(format!(
                    "t={s}: expected {n} concurrences, got {:?}",
                    i.map(|i| i.concurrences().len())
                ));
            }
            if let Some(r) = rule {
                let failed = i.is_some_and(|i| {
                    i.checks()
                        .iter()
                        .chain(
                            i.engagement()
                                .and_then(|e| e.watch.as_ref())
                                .map(|w| w.results.as_slice())
                                .unwrap_or(&[]),
                        )
                        .any(|c| c.rule.as_str() == r && c.outcome != Outcome::Pass)
                });
                if !failed {
                    failures.push(format!("t={s}: expected check {r} to fail"));
                }
            }
        }
    }
}

/// Runner state the timeline changes besides the world.
#[derive(Default)]
struct Extra {
    /// No engagement step before this second.
    stall_until: u32,
    /// The last `task` step's intent.
    last_task: Option<IntentId>,
    /// The picture token each operator's last `look` got.
    looked: BTreeMap<String, u64>,
}

fn dispatch_approved(
    engine: &mut Engine,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    failures: &mut Vec<String>,
    s: u32,
) {
    let ready: Vec<(IntentId, bool, Vec<Uid>)> = engine
        .intents()
        .filter(|i| matches!(i.state(), State::Approved | State::AutoApproved))
        .map(|i| (i.id(), matches!(i.spec().task, Task::Engage { .. }), i.spec().uids.clone()))
        .collect();
    for (id, is_engage, uids) in ready {
        if !is_engage {
            // another order on these vehicles: as on the bridge, it stands and supersedes
            // any engagement on them (no fallback commanded)
            match engine.dispatch(id) {
                Ok(o) => {
                    let hit: Vec<IntentId> = engine
                        .intents()
                        .filter(|i| {
                            i.id() != id
                                && matches!(i.spec().task, Task::Engage { .. })
                                && matches!(
                                    i.state(),
                                    State::PendingApproval | State::Approved | State::Dispatched
                                )
                                && i.spec().uids.iter().any(|u| uids.contains(u))
                        })
                        .map(|i| i.id())
                        .collect();
                    for e in hit {
                        if let Err(err) = engine.supersede_engagement(e, id) {
                            failures.push(format!("t={s}: supersede {e}: {err}"));
                        }
                        engaging.remove(&e);
                    }
                    for u in &o.uids {
                        if let Some(v) = vehicles.get_mut(u.as_str()) {
                            match o.task {
                                Task::Rtl {} => {
                                    v.goal = Some(v.home);
                                    v.orbit = false;
                                }
                                _ => {
                                    v.goal = None;
                                    v.vel = [0.0, 0.0];
                                }
                            }
                        }
                    }
                }
                Err(e) => failures.push(format!("t={s}: dispatch {id}: {e}")),
            }
            continue;
        }
        match engine.dispatch_order(id) {
            Ok(Order::Engage(o)) => {
                if let Some(v) = vehicles.get_mut(o.uid.as_str()) {
                    v.goal = Some(o.effect_point);
                    v.orbit = true;
                }
                // as on the bridge: older orders still waiting for this vehicle are void
                let stale: Vec<IntentId> = engine
                    .intents()
                    .filter(|i| {
                        i.id() != id
                            && !matches!(i.spec().task, Task::Engage { .. })
                            && matches!(i.state(), State::PendingApproval | State::Approved)
                            && i.spec().uids.contains(&o.uid)
                    })
                    .map(|i| i.id())
                    .collect();
                for other in stale {
                    if let Err(e) = engine.void(other, &format!("superseded: {id} was dispatched to vehicle {}", o.uid)) {
                        failures.push(format!("t={s}: void {other}: {e}"));
                    }
                }
                // and, as the bridge does, any other engagement in play on it (none can be:
                // one engagement per platform, the platform_free check)
                let others: Vec<IntentId> = engine
                    .intents()
                    .filter(|i| {
                        i.id() != id
                            && matches!(i.spec().task, Task::Engage { .. })
                            && matches!(i.state(), State::PendingApproval | State::Approved | State::Dispatched)
                            && i.spec().uids.contains(&o.uid)
                    })
                    .map(|i| i.id())
                    .collect();
                for other in others {
                    failures.push(format!("t={s}: engagement {other} was in play on {} when {id} was dispatched", o.uid));
                    if let Err(e) = engine.supersede_engagement(other, id) {
                        failures.push(format!("t={s}: supersede {other}: {e}"));
                    }
                    engaging.remove(&other);
                }
                engaging.insert(id, (o.uid.to_string(), o.effect_point));
            }
            Ok(_) => failures.push(format!("t={s}: {id} dispatched as something else")),
            Err(e) => failures.push(format!("t={s}: dispatch {id}: {e}")),
        }
    }
}

// ---- log attacks ------------------------------------------------------------------------

/// A log attack's result.
#[derive(Clone, Debug, PartialEq)]
pub enum Tampered {
    /// The forged log, its chain recomputed.
    Forged(String),
    /// The log has nothing this attack edits (no reset to hide, no signature to drop...).
    NotApplicable(String),
}

/// Attack a log the way a tamperer who can write the file would, recomputing `seq` and the
/// hash chain so the chain itself verifies: only recomputation can catch it. `Err`: an
/// unknown attack.
pub fn tamper(text: &str, kind: &str) -> Result<Tampered, String> {
    match forge(text, kind) {
        Ok(forged) => Ok(Tampered::Forged(forged)),
        Err(e) if e.starts_with("unknown tamper") || e.starts_with("log:") => Err(e),
        Err(e) => Ok(Tampered::NotApplicable(e)),
    }
}

fn forge(text: &str, kind: &str) -> Result<String, String> {
    let mut events = parse_log(text).map_err(|e| format!("log: {e}"))?;
    let engage_intent = events.iter().find_map(|e| match &e.body {
        EventBody::IntentCreated { intent } if matches!(intent.task, Task::Engage { .. }) => {
            Some(intent.clone())
        }
        _ => None,
    });
    let spec = engage_intent.ok_or("no engagement in the log")?;
    match kind {
        "proposer_approval" | "agent_approval" => {
            let who = match (kind, &spec.source) {
                ("proposer_approval", crate::intent::Source::Human { id }) => id.clone(),
                ("proposer_approval", _) => return Err("the engagement was not proposed by a human".into()),
                _ => OperatorId::new("agent-planner"),
            };
            let mut done = false;
            for e in &mut events {
                if let EventBody::ApprovalGranted { intent, by, .. } = &mut e.body
                    && *intent == spec.id
                {
                    *by = who.clone();
                    done = true;
                }
            }
            if !done {
                return Err("no approval.granted to forge".into());
            }
        }
        "strip_approvers" => {
            // a log that says one approver was enough: the policy's engagement tier (if the
            // file had one) and the decision without `approvers`, and the first signature
            // gone
            let mut decided = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::RunStarted { policy, .. } => {
                        for t in &mut policy.tiers {
                            if t.name == "engagement" {
                                t.approvers = None;
                            }
                        }
                    }
                    EventBody::PolicyDecision {
                        intent, decision, ..
                    } if *intent == spec.id => {
                        decision.approvers = 1;
                        decided = true;
                    }
                    _ => {}
                }
            }
            let before = events.len();
            events.retain(|e| {
                !matches!(&e.body, EventBody::ApprovalConcurred { intent, .. } if *intent == spec.id)
            });
            if !decided || events.len() == before {
                return Err("no engagement decision and signature to strip".into());
            }
        }
        "agent_order_auto" => {
            // an agent's order on the engaging platform made to look auto-approved: the
            // decision says the hold/rtl tier and the engagement-in-play facts are gone
            let order = events
                .iter()
                .find_map(|e| match &e.body {
                    EventBody::IntentCreated { intent }
                        if intent.source.is_agent()
                            && matches!(intent.task, Task::Hold {} | Task::Rtl {}) =>
                    {
                        Some(intent.id)
                    }
                    _ => None,
                })
                .ok_or("no agent order in the log")?;
            let mut done = false;
            for e in &mut events {
                if let EventBody::PolicyDecision {
                    intent,
                    inputs,
                    decision,
                } = &mut e.body
                    && *intent == order
                {
                    inputs.engaged.clear();
                    decision.tier = crate::policy::Tier::Auto;
                    decision.rule = "low-risk".into();
                    done = true;
                }
            }
            if !done {
                return Err("no decision on the agent's order".into());
            }
        }
        "drop_ack" => {
            // an approval that stopped an engagement, without saying so
            let mut done = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::ApprovalGranted { supersedes, .. }
                    | EventBody::ApprovalConcurred { supersedes, .. }
                        if !supersedes.is_empty() =>
                    {
                        supersedes.clear();
                        done = true;
                    }
                    _ => {}
                }
            }
            if !done {
                return Err("no acknowledged approval to strip".into());
            }
        }
        "loosen_policy" => {
            // a log whose policy says picture tokens are good for a day
            let mut done = false;
            for e in &mut events {
                if let EventBody::RunStarted { policy, .. } = &mut e.body {
                    let mut r = policy.rules.engage_rules();
                    r.token_ttl_s = Some(86_400.0);
                    r.seen_max_age_s = None;
                    policy.rules.engage = Some(r);
                    done = true;
                }
            }
            if !done {
                return Err("no run.started".into());
            }
        }
        "drop_reset" => {
            // hide that the picture changed between signatures
            let before = events.len();
            events.retain(|e| {
                !matches!(&e.body, EventBody::ApprovalReset { intent, .. } if *intent == spec.id)
            });
            if events.len() == before {
                return Err("no approval.reset to drop".into());
            }
        }
        "drop_signature" => {
            let before = events.len();
            events.retain(|e| {
                !matches!(&e.body, EventBody::ApprovalConcurred { intent, .. } if *intent == spec.id)
            });
            if events.len() == before {
                return Err("no approval.concurred to drop".into());
            }
        }
        "suppress_void" => {
            // drop the stop and its transition, and make the failed picture look clean
            let stop = events
                .iter()
                .position(|e| matches!(&e.body, EventBody::EngageStopped { intent, .. } if *intent == spec.id))
                .ok_or("no engage.stopped to suppress")?;
            let tr = events[stop..]
                .iter()
                .position(|e| matches!(&e.body, EventBody::IntentState { intent, .. } if *intent == spec.id))
                .map(|i| i + stop)
                .ok_or("no transition after the stop")?;
            events.remove(tr);
            events.remove(stop);
            for e in &mut events {
                if let EventBody::EngagePicture { intent, holds, .. } = &mut e.body
                    && *intent == spec.id
                {
                    *holds = true;
                }
            }
        }
        "edit_snapshot" | "edit_effect" => {
            let mut done = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::ApprovalGranted {
                        intent,
                        snapshot: Some(snap),
                        ..
                    } if *intent == spec.id && kind == "edit_snapshot" => {
                        // move the predicted effect point 300 m north (as if the approver had
                        // seen it there), consistently in the dispatch order too
                        snap.preview.effect_point = offset(snap.preview.effect_point, 0.0, 300.0);
                        done = true;
                    }
                    EventBody::IntentDispatched {
                        intent,
                        engage: Some(o),
                        ..
                    } if *intent == spec.id && kind == "edit_snapshot" => {
                        o.effect_point = offset(o.effect_point, 0.0, 300.0);
                    }
                    EventBody::EngageEffect { intent, point, .. }
                        if *intent == spec.id && kind == "edit_effect" =>
                    {
                        *point = offset(*point, 0.0, 300.0);
                        done = true;
                    }
                    _ => {}
                }
            }
            if !done {
                return Err(format!("nothing to {kind}"));
            }
        }
        other => return Err(format!("unknown tamper {other:?}")),
    }
    // renumber and re-chain
    let mut prev = GENESIS.to_string();
    let mut out = String::new();
    for (i, e) in events.iter_mut().enumerate() {
        e.seq = i as u64 + 1;
        e.prev = prev.clone();
        let line = serde_json::to_string(&*e).map_err(|err| err.to_string())?;
        prev = sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}
