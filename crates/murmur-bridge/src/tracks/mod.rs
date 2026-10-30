//! Tracks (Phase C2): what the bridge believes about contacts that are not ours, built
//! only from our own sensors. Truth and perception are kept apart on purpose:
//!
//! - **Truth** arrives from the contacts process on the simulation channel, `POST
//!   /api/sim/contacts` (`truth.rs`). It is stored here, read by the sensing model and by
//!   nothing else, and served back only on `GET /api/sim/truth` when `MURMUR_EVAL_TRUTH=1`
//!   (an evaluation hook, off by default; the console and agents never read it).
//! - **Perception**: the sensing model (`sensing.rs`) decides, per tick, which of our
//!   vehicles' sensors detect which contacts and with what error; the tracker (`model.rs`)
//!   folds detections into tracks with confidence, identity and affiliation estimates,
//!   sources and `last_seen_t`. Tracks go out on `GET /api/tracks`, as the WebSocket
//!   `{"type": "tracks"}` message, into the decision world snapshot (`WorldSnapshot.tracks`)
//!   and to the CoT export.
//!
//! Association uses the simulation's contact id (kept private, never serialized): perfect
//! association is a stated simplification; a gating tracker is future work. A track that
//! was dropped (no detection for `MURMUR_TRACK_DROP_S`, default 180 s) and whose contact is
//! seen again starts a new track with a new id, as a real tracker would.
//!
//! **Drops** (rules 16, murmur-533): a dropped track is handed to the decision engine with
//! its facts at the drop ([`Tracks::take_drops`], logged there as `track.dropped`; the engine
//! keeps it as a lost contact that every engagement's keep-out still counts). Until the engine
//! has taken it, it stays in the decision facts ([`Tracks::facts`]), so no decision falls
//! between the drop and the record. The console is shown the engine's lost contacts
//! ([`Tracks::mirror_lost`], `lost` in `GET /api/tracks` and the tracks message).
//!
//! Deterministic: every draw is a hash of (seed, scenario second, vehicle, contact, sensor),
//! so the same scenario, seed and fleet positions give the same detections. The seed and
//! misclassification rate come from the scenario (in the truth message); `MURMUR_SENSE_SEED`
//! and `MURMUR_SENSE_MISCLASS` override them on the bridge.
//!
//! **Two sensing models** (WORLD-V3-PLAN section 2, docs/SENSING.md): `legacy` (`sensing.rs`,
//! every record before world v3) and `physical` (`physical/`, the default). The scenario names
//! one (`[sensing] model`, in the truth message) and `MURMUR_SENSE_MODEL` overrides it on the
//! bridge (the v1 and v2 worlds and the Phase G stacks name `legacy` there); the physical model
//! keeps state across messages (pointing, looks, scans) and reads the scenario's terrain grid,
//! conditions and signature overrides from the same message. Both feed the same tracker.
//!
//! **Effects** (Phase E): an engagement's abstract effect (a point, a radius, a time) goes
//! back to the contacts process on the same labelled channel, in the answer to its next
//! `POST /api/sim/contacts` (`effects`), with the contact the target track was associated
//! with (the private association, never serialized elsewhere) and where our vehicles
//! reported themselves, for the evaluation truth log's collateral record. The contacts
//! process marks the contacts inside the radius; nothing here models damage. Rules 20: an
//! effect a munition carries strikes at its impact (`impact_t`, put on the contacts' own
//! scenario clock as the answer goes out: the truth message's time plus the impact's time from
//! now), and the contacts process holds it until then.
//!
//! Operators may declare a track's affiliation (`POST /api/tracks/{id}/declare`, the
//! signed-in operator as for intents; a dev run's header). From rules 9 every declaration is
//! the decision engine's identification ledger (an operator's, or the engine's hostile-act
//! rule's on an act these sensors observed: `sense_acts`); this store only mirrors it for
//! display (`mirror`), and the decision facts carry the estimate alone. The tracker never
//! derives hostile.

pub mod model;
pub mod physical;
pub mod reach;
pub mod sensing;
#[cfg(test)]
mod tests;
pub mod truth;
pub mod weapons;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use murmur_decide::{Affiliation, AisIdentity, LonLat, Time, TrackFacts};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use self::model::{Declared, Track};
use self::sensing::{ActObservation, Detection, Observer, Params, SearchArea, SensorCatalog, misclassify_draw};
use self::truth::{SensingModel, TruthMessage};
use crate::clock::{self, round};
use crate::decision::Decision;
use crate::world::World;

/// Env overrides and switches.
#[derive(Debug, Clone)]
pub struct Settings {
    pub seed: Option<u64>,
    pub misclassification_rate: Option<f64>,
    pub eval_truth: bool,
    pub drop_s: f64,
    /// `MURMUR_SENSE_MODEL` (`legacy` or `physical`): the sensing model whatever the scenario
    /// names.
    pub model: Option<SensingModel>,
    /// The simulation channel's token (`MURMUR_SIM_TOKEN`, generated per stack by `up.sh` and
    /// the evaluation's runner and given to this bridge and the contacts process only): `POST
    /// /api/sim/contacts` carries it (`X-Murmur-Sim-Token`, checked before the body is read)
    /// or is refused; none set, the channel is closed. Never logged or served. It stops an
    /// HTTP client, not a process of the same user (which can read either process's
    /// environment).
    pub sim_token: Option<String>,
}

impl Settings {
    pub fn from_env() -> Settings {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        Settings {
            seed: var("MURMUR_SENSE_SEED").and_then(|s| s.trim().parse().ok()),
            misclassification_rate: var("MURMUR_SENSE_MISCLASS")
                .and_then(|s| s.trim().parse().ok())
                .map(|r: f64| r.clamp(0.0, 1.0)),
            eval_truth: var("MURMUR_EVAL_TRUTH").is_some_and(|v| {
                !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no")
            }),
            drop_s: var("MURMUR_TRACK_DROP_S")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(180.0f64)
                .max(model::STALE_S),
            model: var("MURMUR_SENSE_MODEL").and_then(|s| match s.parse() {
                Ok(m) => Some(m),
                Err(e) => {
                    println!("MURMUR_SENSE_MODEL: {e}; the scenario's model");
                    None
                }
            }),
            sim_token: var("MURMUR_SIM_TOKEN").map(|t| t.trim().to_string()),
        }
    }
}

/// The header the contacts process sends its token in.
pub const SIM_TOKEN_HEADER: &str = "x-murmur-sim-token";

/// Whether a request on the simulation channel carries the stack's token `want` (compared as
/// digests, in constant time). Without a token configured the channel is closed. Every route
/// of the channel uses this one check: `POST /api/sim/contacts` here, `POST
/// /api/sim/situation` (`crate::situation`).
pub fn sim_token_check(want: Option<&str>, headers: &HeaderMap) -> Result<(), &'static str> {
    let Some(want) = want.filter(|t| !t.is_empty()) else {
        return Err("the simulation channel is closed: no MURMUR_SIM_TOKEN was set for this bridge");
    };
    let given = headers
        .get(SIM_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or("the simulation channel takes the stack's token (X-Murmur-Sim-Token)")?;
    if murmur_auth::keys::ct_eq(&murmur_auth::keys::digest(given.trim()), &murmur_auth::keys::digest(want)) {
        Ok(())
    } else {
        Err("wrong simulation token")
    }
}

struct Inner {
    /// (wall seconds received, the message): truth, for the sensing model and the eval hook
    truth: Option<(f64, TruthMessage)>,
    tracks: BTreeMap<String, Track>,
    /// contact id -> track id: the simulation-side association, never serialized
    by_contact: HashMap<String, String>,
    next_no: u64,
    /// detections folded in on the last truth message (for the POST answer and stats)
    last_detections: usize,
    /// abstract effects waiting for the contacts process (the next POST's answer), and the
    /// ones already handed over (evaluation view)
    effects: Vec<Value>,
    effects_sent: Vec<Value>,
    /// rules 9: hostile acts our sensors observed, waiting for the decision engine (its loop
    /// takes them), the truth acts already sensed, and what became of each (evaluation view:
    /// the truth act, its author's contact and track, the report it became, or unobserved)
    acts: Vec<murmur_decide::ActReport>,
    acts_seen: std::collections::BTreeSet<String>,
    act_records: Vec<Value>,
    /// rules 16: tracks dropped and not yet taken by the decision engine, each with its facts
    /// at the drop (still in the decision facts until taken)
    dropped: BTreeMap<String, (TrackFacts, Option<AisIdentity>)>,
    /// rules 16: the engine's lost contacts, for display ([`Tracks::mirror_lost`])
    lost: Vec<Value>,
    /// The headless simulator's evaluation view of the last sensing step: its detections, and
    /// each observer's uid, position and navigation error ([`Tracks::last_step_json`]).
    #[cfg(murmur_headless)]
    last_step: (Vec<Detection>, Vec<StepObserver>),
    /// Opponent weapons (docs/OPPONENT-WEAPONS.md): the weapon picture, apart from the tracks;
    /// the strikes our vehicles report, waiting to go to their vehicles' events; where each
    /// observer was at the last truth message (a launch's expected strike)
    weapons: weapons::Picture,
    strikes: Vec<(u64, String)>,
    ours_at: HashMap<u64, (f64, f64)>,
}

/// Opponent weapons' sensing state, kept with the physical model's (locked only while it is):
/// the last truth message's clock (each message covers the second since), each camera's
/// evidence on each weapon, and whether the legacy model's word on weapons was said.
#[derive(Default)]
struct WeaponSense {
    prev_t: Option<f64>,
    cams: HashMap<(String, u64, usize), physical::weapon::EoWeapon>,
    noted_legacy: bool,
}

/// What a sensor carrier is (bd murmur-4tq's reach display names it).
struct Carrier {
    platform: String,
    callsign: Option<String>,
    domain: String,
}

/// A vehicle's reach answer is computed again at least this often, s.
pub const REACH_MAX_AGE_S: f64 = 5.0;

/// What a vehicle's reach answer was computed from.
#[derive(Debug, Clone, PartialEq)]
struct ReachKey {
    platform: String,
    alt_m: f64,
    submerged: bool,
    /// the conditions, the sun stated
    cond: physical::conditions::Conditions,
}

impl ReachKey {
    /// Close enough that the answer stands: the same platform and medium, the height within 10 m
    /// or 5 %, the visibility within 5 %, the wind within 0.5 m/s, the sun within half a degree,
    /// the rest unchanged.
    fn close(&self, o: &ReachKey) -> bool {
        let (a, b) = (&self.cond, &o.cond);
        let sun = |c: &physical::conditions::Conditions| c.sun_elevation_deg.unwrap_or(f64::NAN);
        self.platform == o.platform
            && self.submerged == o.submerged
            && (self.alt_m - o.alt_m).abs() <= 10.0f64.max(0.05 * self.alt_m.abs())
            && (a.visibility_m - b.visibility_m).abs() <= 0.05 * a.visibility_m
            && (a.wind_mps - b.wind_mps).abs() <= 0.5
            && (sun(a) - sun(b)).abs() <= 0.5
            && a.sea_state == b.sea_state
            && a.shipping == b.shipping
            && a.water_depth_m == b.water_depth_m
    }
}

/// A vehicle's reach answer as last computed (bridge wall clock).
struct ReachEntry {
    at: f64,
    key: ReachKey,
    value: Value,
}

/// An observer of the last sensing step: its uid, where it sensed from (lat, lon), its
/// navigation error (metres east and north).
#[cfg(murmur_headless)]
type StepObserver = (u64, f64, f64, (f64, f64));

pub struct Tracks {
    catalog: SensorCatalog,
    settings: Settings,
    inner: Mutex<Inner>,
    /// The physical sensing model's state across truth messages (pointing, looks, scans, the
    /// terrain), its grid paths relative to the repository.
    physical: Mutex<physical::Engine>,
    weapon_sense: Mutex<WeaponSense>,
    /// The sensing model the last message ran on (said when it changes).
    last_model: Mutex<Option<SensingModel>>,
    /// bd murmur-4tq: each platform's clear-day reach by sensor type (the catalog's
    /// `physical_reach_m`), and each vehicle's reach now as last computed (`reach_json`).
    clear_day: HashMap<String, HashMap<String, murmur_decide::SensorReach>>,
    reach_cache: Mutex<HashMap<u64, ReachEntry>>,
    /// The headless simulator's word that a vehicle has left the world (a platform spent by
    /// its own effect): it senses nothing from then on, whatever its link's age.
    #[cfg(murmur_headless)]
    gone: OnceLock<Gone>,
    /// The headless simulator's word on how far a vehicle's own navigation is off its truth
    /// (murmur-k8i): it senses from its truth and lays its fixes off from what it believes.
    #[cfg(murmur_headless)]
    nav_error: OnceLock<NavError>,
    updates: watch::Sender<Arc<String>>,
}

#[cfg(murmur_headless)]
type Gone = Box<dyn Fn(u64) -> bool + Send + Sync>;
/// A vehicle's navigation error now, metres east and north (what it believes less what is);
/// None: not one of the simulator's.
#[cfg(murmur_headless)]
type NavError = Box<dyn Fn(u64) -> Option<(f64, f64)> + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The tracker's association of contacts to the tracks it holds (evaluation only), in the
/// store's order.
fn association_of(inner: &Inner) -> Vec<Value> {
    inner
        .by_contact
        .iter()
        .filter(|(_, t)| inner.tracks.contains_key(*t))
        .map(|(c, t)| json!({ "contact_id": c, "track_id": t }))
        .collect()
}

impl Tracks {
    #[cfg(test)]
    pub fn new(catalog: SensorCatalog, settings: Settings) -> Tracks {
        Self::with_root(catalog, settings, None)
    }

    /// `root`: the repository, which a scenario's grid paths are relative to.
    pub fn with_root(catalog: SensorCatalog, settings: Settings, root: Option<PathBuf>) -> Tracks {
        let t = Tracks {
            catalog,
            settings,
            physical: Mutex::new(physical::Engine::new(root)),
            weapon_sense: Mutex::new(WeaponSense::default()),
            last_model: Mutex::new(None),
            clear_day: HashMap::new(),
            reach_cache: Mutex::new(HashMap::new()),
            #[cfg(murmur_headless)]
            gone: OnceLock::new(),
            #[cfg(murmur_headless)]
            nav_error: OnceLock::new(),
            inner: Mutex::new(Inner {
                truth: None,
                tracks: BTreeMap::new(),
                by_contact: HashMap::new(),
                next_no: 1,
                last_detections: 0,
                effects: Vec::new(),
                effects_sent: Vec::new(),
                acts: Vec::new(),
                acts_seen: std::collections::BTreeSet::new(),
                act_records: Vec::new(),
                dropped: BTreeMap::new(),
                lost: Vec::new(),
                #[cfg(murmur_headless)]
                last_step: (Vec::new(), Vec::new()),
                weapons: weapons::Picture::default(),
                strikes: Vec::new(),
                ours_at: HashMap::new(),
            }),
            updates: watch::channel(Arc::new(String::new())).0,
        };
        t.publish(clock::wall());
        t
    }

    /// From `sim/platforms.toml` and the environment; a bad catalog is an error (the sensing
    /// model would silently see nothing).
    pub fn load(platforms: &Path) -> Result<Tracks, String> {
        // the repository: sim/platforms.toml's grandparent
        let root = platforms.parent().and_then(Path::parent).map(Path::to_path_buf);
        let text = std::fs::read_to_string(platforms).map_err(|e| format!("{}: {e}", platforms.display()))?;
        Tracks::with_root(SensorCatalog::load(platforms)?, Settings::from_env(), root).with_clear_day(&text)
    }

    /// The catalog's clear-day reaches (`physical_reach_m`) from the platforms file's text, for
    /// the reach display (bd murmur-4tq).
    pub fn with_clear_day(mut self, platforms_toml: &str) -> Result<Tracks, String> {
        let catalog = murmur_decide::Catalog::from_toml(platforms_toml).map_err(|e| format!("platforms.toml: {e}"))?;
        self.clear_day = catalog
            .reaching(murmur_decide::Reaches::Physical)
            .platforms
            .iter()
            .map(|(id, p)| (id.clone(), p.sensors.iter().filter_map(|s| s.reach_m.clone().map(|r| (s.kind.clone(), r))).collect::<HashMap<_, _>>()))
            .filter(|(_, m)| !m.is_empty())
            .collect();
        Ok(self)
    }

    /// The headless simulator says which vehicles have left the world (`gone(uid)`): from then
    /// on they are no sensor carriers, at once, not when their links go stale.
    #[cfg(murmur_headless)]
    pub fn set_gone(&self, gone: impl Fn(u64) -> bool + Send + Sync + 'static) {
        let _ = self.gone.set(Box::new(gone));
    }

    /// The headless simulator says how far each vehicle's navigation is off its truth
    /// (`nav_error(uid)`, metres east and north: the stand-in's own filter, a GNSS spoofer's walk
    /// or a dead reckoning's drift): its sensors then see from its truth, the position it
    /// reports less that error, and it lays their fixes off from what it believes
    /// (`sensing::Observer`). Without it (the bridge itself, which never knows our vehicles'
    /// truth), an observer is where the vehicle says it is.
    #[cfg(murmur_headless)]
    pub fn set_nav_error(&self, nav_error: impl Fn(u64) -> Option<(f64, f64)> + Send + Sync + 'static) {
        let _ = self.nav_error.set(Box::new(nav_error));
    }

    pub fn eval_truth_enabled(&self) -> bool {
        self.settings.eval_truth
    }

    /// Rules 16: how long a track goes without a detection before it is dropped (s, the run's
    /// `MURMUR_TRACK_DROP_S`, every domain alike), logged in `run.started` for replay.
    pub fn drop_s(&self) -> f64 {
        self.settings.drop_s
    }

    /// The tracker's memories for the keep-out explanation, this run's drop time included.
    pub fn memory(&self) -> murmur_decide::keepout::TrackerMemory {
        model::memory(self.settings.drop_s)
    }

    /// Whether a request on the simulation channel carries this stack's token (compared as
    /// digests, in constant time). Without a token configured the channel is closed.
    pub fn sim_token_ok(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        sim_token_check(self.settings.sim_token.as_deref(), headers)
    }

    /// Our vehicles as sensor carriers: those with a position and a live link, each where it
    /// says it is, or (the headless simulator, which knows its navigation error) at its truth
    /// with that error, for its fixes (`sensing::Observer`).
    fn observers(&self, world: &World) -> Vec<Observer> {
        self.carriers(world).into_iter().map(|(o, _)| o).collect()
    }

    /// [`Tracks::observers`], each with what the vehicle is: its platform, callsign and domain.
    fn carriers(&self, world: &World) -> Vec<(Observer, Carrier)> {
        let mono = clock::mono();
        // a link's last packet is on the bridge's wall clock (`LinkSnapshot::last_rx_t`), as
        // the console and the estimate read it: a vehicle silent past its stale age senses
        // nothing (before review B2 this compared it with the monotonic clock, so every
        // vehicle ever heard kept sensing from its last position)
        let wall = clock::wall();
        world
            .vehicles()
            .into_iter()
            .filter_map(|cell| {
                let link = cell.link_snapshot(mono);
                let fresh = link
                    .last_rx_t
                    .is_some_and(|t| wall - t <= link.stale_after_s());
                if !fresh {
                    return None;
                }
                #[cfg(murmur_headless)]
                if self.gone.get().is_some_and(|g| g(cell.uid)) {
                    return None;
                }
                // the task's area, for the physical model's payload manager (the task manager's
                // lock is not held while the vehicle is read)
                let area = world.tasks.task_of(cell.uid).and_then(|(kind, p)| match kind {
                    crate::tasks::TaskKind::Orbit => p.center.map(|c| SearchArea::Orbit {
                        center: (c[1], c[0]),
                        radius_m: p.radius.unwrap_or(200.0),
                    }),
                    crate::tasks::TaskKind::Patrol => p.route.filter(|r| r.len() >= 2).map(|r| SearchArea::Route {
                        points: r.iter().map(|q| (q[1], q[0])).collect(),
                    }),
                    _ => None,
                });
                // where the vehicle's navigation is off its truth, where the simulator knows it
                // (read before the vehicle's cell, whose lock is not held meanwhile)
                #[cfg(murmur_headless)]
                let nav_error_m = self.nav_error.get().and_then(|f| f(cell.uid)).unwrap_or((0.0, 0.0));
                #[cfg(not(murmur_headless))]
                let nav_error_m = (0.0, 0.0);
                cell.read(|v| {
                    // where it says it is: what it believes; its sensors see from its truth
                    let (lat, lon) = v.pos?;
                    let (e, n) = nav_error_m;
                    let (lat, lon) = if e == 0.0 && n == 0.0 {
                        (lat, lon)
                    } else {
                        let m = physical::geometry::M_PER_DEG;
                        (lat - n / m, lon - e / (m * lat.to_radians().cos()))
                    };
                    let id = v.identity.as_deref()?;
                    let platform = id.str("platform")?;
                    let ps = self.catalog.get(platform)?;
                    if ps.sensors.is_empty() {
                        return None;
                    }
                    let alt = v.alt_msl.unwrap_or(0.0);
                    let submerged = id.str("domain") == Some("subsurface") && alt < -0.5;
                    let carrier = Carrier {
                        platform: platform.to_string(),
                        callsign: id.str("callsign").map(str::to_string),
                        domain: id.str("domain").unwrap_or("").to_string(),
                    };
                    Some((
                        Observer {
                            uid: cell.uid,
                            lat,
                            lon,
                            nav_error_m,
                            alt_msl: alt,
                            mast_m: ps.mast_m,
                            submerged,
                            sensors: ps.sensors.clone(),
                            heading_deg: v.heading.or(v.yaw),
                            area,
                        },
                        carrier,
                    ))
                })
            })
            .collect()
    }

    /// A truth message arrived: run the sensing step against our vehicles and fold the
    /// detections in. Returns (contacts, detections, tracks).
    pub fn on_truth(&self, msg: TruthMessage, world: &World) -> Result<(usize, usize, usize), String> {
        msg.validate()?;
        let observers = self.observers(world);
        // rules 21: each vehicle on an identify order cues the contacts behind its track first
        let looks: Vec<(u64, String)> = observers
            .iter()
            .filter_map(|o| world.tasks.task_of(o.uid).and_then(|(_, p)| p.look).map(|t| (o.uid, t)))
            .collect();
        let firsts: HashMap<u64, std::collections::BTreeSet<String>> = {
            let inner = lock(&self.inner);
            looks.into_iter().map(|(u, t)| (u, Self::contacts_of(&inner, &t))).collect()
        };
        lock(&self.physical).set_firsts(firsts);
        let now = clock::wall();
        let out = self.on_truth_with(msg, &observers, now);
        // opponent weapons: a strike is its victim's own report, logged as its vehicle event
        let strikes = std::mem::take(&mut lock(&self.inner).strikes);
        for (uid, text) in strikes {
            if let Some(cell) = world.vehicles().into_iter().find(|c| c.uid == uid) {
                cell.write(|v| v.report_struck(text));
            }
        }
        out
    }

    /// Opponent weapons: where the vehicles a truth message names (`aim_at`) are now, for the
    /// simulation channel's answer (`ours`): position, height and velocity, as each reports it.
    pub fn ours_for(&self, world: &World, uids: &[u64]) -> Vec<Value> {
        if uids.is_empty() {
            return Vec::new();
        }
        world
            .vehicles()
            .into_iter()
            .filter(|c| uids.contains(&c.uid))
            .filter_map(|c| {
                c.read(|v| {
                    let (lat, lon) = v.pos?;
                    let [ve, vn] = v.vel_en.unwrap_or([0.0, 0.0]);
                    Some(json!({ "uid": c.uid, "lat": lat, "lon": lon, "alt_m": v.alt_msl.unwrap_or(0.0), "ve": ve, "vn": vn }))
                })
            })
            .collect()
    }

    /// The sensing step with explicit observers and clock (tests drive this directly).
    pub fn on_truth_with(
        &self,
        msg: TruthMessage,
        observers: &[Observer],
        now: f64,
    ) -> Result<(usize, usize, usize), String> {
        msg.validate()?;
        let params = Params {
            seed: self.settings.seed.unwrap_or(msg.seed),
            tick: msg.s.floor() as i64,
            misclassification_rate: self
                .settings
                .misclassification_rate
                .unwrap_or(msg.sensing.misclassification_rate),
        };
        let seed = params.seed;
        let rate = params.misclassification_rate;
        let model = self.model_for(&msg);
        // opponent weapons (docs/OPPONENT-WEAPONS.md): under the physical model, each launch our
        // cameras saw and each fix of a weapon in flight, after the contacts' step (whose pointing
        // they read, never change)
        let mut launches: HashMap<String, ActObservation> = HashMap::new();
        let mut weapon_fixes: Vec<physical::weapon::WeaponFix> = Vec::new();
        let mut detections = match model {
            SensingModel::Legacy => {
                if !msg.weapons.is_empty() {
                    let mut ws = lock(&self.weapon_sense);
                    if !ws.noted_legacy {
                        ws.noted_legacy = true;
                        println!("sensing: opponent weapons need the physical model: under the legacy one their acts are sensed as abstract acts, the weapons not at all");
                    }
                }
                sensing::detect(params, observers, &msg.contacts, &|contact, sensor| misclassify_draw(seed, contact, sensor, rate))
            }
            SensingModel::Physical => {
                let mut engine = lock(&self.physical);
                engine.configure(&msg.sensing);
                let step = physical::Step {
                    seed,
                    tick: params.tick,
                    s: msg.s,
                    t: msg.t,
                    misclassification_rate: rate,
                    sensing: &msg.sensing,
                };
                let dets = engine.step(&step, observers, &msg.contacts);
                if !msg.weapons.is_empty() || msg.acts.iter().any(|a| a.weapon.is_some()) {
                    for a in msg.acts.iter().filter(|a| a.held.is_none()) {
                        let Some(w) = a.weapon.as_ref().and_then(|id| msg.weapons.iter().find(|w| w.id == *id)) else { continue };
                        let Some(shooter) = msg.contacts.iter().find(|c| c.id == a.by) else { continue };
                        let mut shooter = shooter.clone();
                        (shooter.lat, shooter.lon) = (a.lat, a.lon);
                        if let Some(o) = engine.observe_launch(&step, observers, &shooter, &a.id, &w.kind) {
                            launches.insert(a.id.clone(), o);
                        }
                    }
                    let mut ws = lock(&self.weapon_sense);
                    let prev_t = ws.prev_t.unwrap_or(f64::NEG_INFINITY);
                    weapon_fixes = engine.sense_weapons(&step, prev_t, observers, &msg.weapons, &mut ws.cams);
                    let live: std::collections::HashSet<&str> = msg.weapons.iter().map(|w| w.id.as_str()).collect();
                    ws.cams.retain(|k, _| live.contains(k.0.as_str()));
                }
                lock(&self.weapon_sense).prev_t = Some(msg.t);
                dets
            }
        };
        // each fix dated by the clock of the truth it was drawn from: the filter's intervals
        // are the contact's own, whatever the bridge's wall clock does meanwhile (on this
        // host it steps back about a second every half minute)
        for d in &mut detections {
            d.measured_t = Some(msg.t);
        }
        let n_contacts = msg.contacts.len();
        let n_tracks = {
            let mut inner = lock(&self.inner);
            #[cfg(murmur_headless)]
            {
                inner.last_step = (
                    detections.clone(),
                    observers.iter().map(|o| (o.uid, o.lat, o.lon, o.nav_error_m)).collect(),
                );
            }
            self.fold(&mut inner, &detections, now);
            // rules 9: the hostile acts of this tick, as our sensors observe them (after the
            // fold: an act by a contact first seen this tick is attributed to its new track)
            let physical = (model == SensingModel::Physical).then_some((detections.as_slice(), &launches));
            self.sense_acts(&mut inner, params, observers, &msg, now, physical);
            // opponent weapons: the weapon picture takes this second's fixes and strikes
            self.sense_strikes(&mut inner, observers, &msg, &weapon_fixes, now);
            inner.truth = Some((now, msg));
            inner.last_detections = detections.len();
            self.drop_old(&mut inner, now);
            inner.tracks.len()
        };
        self.publish(now);
        Ok((n_contacts, detections.len(), n_tracks))
    }

    /// The sensing model in force: `MURMUR_SENSE_MODEL`, else the one the last truth message
    /// ran on, else the default (physical). The decision facts carry our sensors' reaches under
    /// it (`murmur_decide::Reaches`, bd murmur-1u4).
    pub fn sensing_model(&self) -> SensingModel {
        self.settings.model.or(*lock(&self.last_model)).unwrap_or_default()
    }

    /// The sensing model a message runs on: `MURMUR_SENSE_MODEL`, else the scenario's, else the
    /// default (physical). Said at the first message and when it changes.
    fn model_for(&self, msg: &TruthMessage) -> SensingModel {
        let m = self.settings.model.or(msg.sensing.model).unwrap_or_default();
        let mut last = lock(&self.last_model);
        if *last != Some(m) {
            let why = if self.settings.model.is_some() {
                "MURMUR_SENSE_MODEL"
            } else if msg.sensing.model.is_some() {
                "the scenario"
            } else {
                "the default"
            };
            println!("sensing: the {} model ({why})", m.as_str());
            if m == SensingModel::Physical {
                let ph = physical::spec::placeholders(&self.catalog);
                if !ph.is_empty() {
                    println!("sensing: placeholder figures (from range_m) for {}", ph.join(", "));
                }
            }
            *last = Some(m);
        }
        m
    }

    /// Rules 9: each new act in a truth message, observed by our sensors or not
    /// (`sensing::observe_act`). An observed one becomes a report for the decision engine
    /// (`A-<n>`, at the bridge's clock now): whom it was against (our vehicle, the track its
    /// victim is associated with, or something we hold no track of), which sensors saw it,
    /// where its author appeared to be and that position's error. Every act, observed or not,
    /// is kept for the evaluation view with the truth behind it. Under the physical model
    /// (`physical`: this tick's detections) the sensors that detected the act's author this
    /// tick observed it (`physical::observe_act`).
    ///
    /// Opponent weapons (docs/OPPONENT-WEAPONS.md): under the physical model an act that launched
    /// a weapon is observed by the cameras that saw the launch (`physical`'s second part,
    /// `physical::Engine::observe_launch`), not by whatever detected its author; its report joins
    /// the weapon picture. An act held (its author did not fire) is not sensed at all.
    fn sense_acts(
        &self,
        inner: &mut Inner,
        params: Params,
        observers: &[Observer],
        msg: &TruthMessage,
        now: f64,
        physical: Option<(&[Detection], &HashMap<String, ActObservation>)>,
    ) {
        for a in &msg.acts {
            if !inner.acts_seen.insert(a.id.clone()) {
                continue;
            }
            let actor_track = inner.by_contact.get(&a.by).cloned();
            if let Some(why) = &a.held {
                inner.act_records.push(json!({
                    "act": a.id, "s": a.s, "by": a.by, "by_track": actor_track,
                    "against_vehicle": a.against_vehicle, "against_contact": a.against_contact,
                    "observed": false, "held": why,
                }));
                continue;
            }
            let weapon = a.weapon.as_ref().and_then(|id| msg.weapons.iter().find(|w| w.id == *id));
            let actor = msg.contacts.iter().find(|c| c.id == a.by);
            let observed = actor.and_then(|c| match physical {
                Some((_, launches)) if weapon.is_some() => launches.get(&a.id).cloned(),
                Some((dets, _)) => physical::observe_act(dets, &c.id),
                None => {
                    // the author where the act happened (the contacts process says where)
                    let mut c = c.clone();
                    c.lat = a.lat;
                    c.lon = a.lon;
                    sensing::observe_act(params, observers, &c, &a.id)
                }
            });
            let Some(obs) = observed else {
                inner.act_records.push(json!({
                    "act": a.id, "s": a.s, "by": a.by, "by_track": actor_track,
                    "against_vehicle": a.against_vehicle, "against_contact": a.against_contact,
                    "observed": false, "weapon": a.weapon,
                }));
                continue;
            };
            let n = inner.act_records.iter().filter(|r| r["observed"] == true).count() + 1;
            let report_id = format!("A-{n}");
            let against = match (a.against_vehicle, &a.against_contact) {
                (Some(uid), _) => murmur_decide::Against::Vehicle {
                    uid: crate::decision::snapshot::duid(uid),
                },
                (None, Some(c)) => match inner.by_contact.get(c) {
                    Some(t) => murmur_decide::Against::Track { track: t.clone() },
                    None => murmur_decide::Against::Unknown,
                },
                (None, None) => murmur_decide::Against::Unknown,
            };
            let report = murmur_decide::ActReport {
                act: report_id.clone(),
                against,
                observed_t: Time((now * 1000.0).round() as i64),
                sensors: obs.sensors.clone(),
                location: LonLat::new(obs.lon, obs.lat),
                sigma_m: obs.sigma_m,
                fixes: obs
                    .fixes
                    .iter()
                    .map(|f| murmur_decide::ident::Fix {
                        observer: f.observer.to_string(),
                        sensors: f.sensors.clone(),
                        location: LonLat::new(f.lon, f.lat),
                        sigma_m: f.sigma_m,
                    })
                    .collect(),
                // the channel it came in on (its token checked)
                channel: Some("sim:contacts".into()),
            };
            inner.act_records.push(json!({
                "act": a.id, "s": a.s, "by": a.by, "by_track": actor_track,
                "against_vehicle": a.against_vehicle, "against_contact": a.against_contact,
                "observed": true, "report": report_id, "sensors": obs.sensors,
                "sigma_m": obs.sigma_m, "observed_t": report.observed_t.0, "weapon": a.weapon,
            }));
            // a seen launch: what it reports (by construction: the weapon's class, its nominal
            // speed, which of ours it heads for) joins the weapon picture
            if let (Some(w), Some(_)) = (weapon, physical) {
                inner.weapons.launch_seen(&w.id, now, &obs, &report_id, &w.name, w.speed_mps, w.against_vehicle);
            }
            inner.acts.push(report);
        }
    }

    /// Opponent weapons: this message's weapon fixes and strikes into the weapon picture (at the
    /// bridge's time: `now` less each instant's age in the truth's clock). A strike on one of ours
    /// waits for its vehicle's events (`Tracks::on_truth`); the picture ages.
    fn sense_strikes(&self, inner: &mut Inner, observers: &[Observer], msg: &TruthMessage, fixes: &[physical::weapon::WeaponFix], now: f64) {
        for o in observers {
            inner.ours_at.insert(o.uid, (o.lat, o.lon));
        }
        let offset = now - msg.t;
        inner.weapons.fixes(fixes, offset);
        for i in &msg.impacts {
            let Some(uid) = i.against_vehicle else { continue };
            let w = inner.weapons.struck(&i.weapon, uid, i.t + offset);
            let text = match (&w.launch, w.first_cue_t()) {
                (Some(l), Some(first)) => format!(
                    "struck ({}): launch seen by {} {:.1} s before; no damage is modelled",
                    w.id,
                    l.sensors.join(", "),
                    i.t + offset - first
                ),
                (None, Some(first)) => format!(
                    "struck ({}): no sensor saw the launch; the weapon was fixed {:.1} s before; no damage is modelled",
                    w.id,
                    i.t + offset - first
                ),
                _ => format!("struck ({}): no sensor saw the launch or the weapon; no damage is modelled", w.id),
            };
            println!("weapons: vehicle {uid} {text}");
            inner.strikes.push((uid, text));
        }
        inner.weapons.age(now);
    }

    /// Opponent weapons: the inbox's alerts for the strikes in the weapon picture (shown while it
    /// shows them, a minute): high when no sensor saw the launch (no author placed: the rule can
    /// declare nothing, a person must), else medium. `name`: a vehicle's callsign.
    pub fn strike_alerts(&self, name: &dyn Fn(u64) -> Option<String>) -> Vec<crate::decision::alerts::Alert> {
        use crate::decision::alerts::{Alert, Severity};
        lock(&self.inner)
            .weapons
            .tracks
            .iter()
            .filter_map(|w| {
                let (uid, t) = w.struck?;
                let callsign = name(uid);
                let who = callsign.clone().unwrap_or_else(|| format!("vehicle {uid}"));
                // "1:eo_ir" as "HUNTER 1 EO/IR"
                let seen = |ss: &[String]| {
                    ss.iter()
                        .map(|s| {
                            let (u, k) = s.split_once(':').unwrap_or(("", s));
                            let k = if k == "eo_ir" { "EO/IR".to_string() } else { k.replace('_', " ") };
                            let who = u.parse::<u64>().ok().and_then(name).unwrap_or_else(|| format!("vehicle {u}"));
                            format!("{who} {k}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let (severity, detail) = match (&w.launch, &w.report) {
                    (Some(l), Some(r)) => (
                        Severity::Medium,
                        format!(
                            "{who} struck ({}): the launch was seen by {} ({r}){}",
                            w.id,
                            seen(&l.sensors),
                            w.warning_s().map(|s| format!(", {s:.1} s of warning")).unwrap_or_default()
                        ),
                    ),
                    _ => (
                        Severity::High,
                        format!(
                            "{who} struck ({}): no sensor saw the launch, so no shooter is placed and the rule declares nothing; declare by hand if you know who fired{}",
                            w.id,
                            w.warning_s().map(|s| format!(" (the weapon was fixed {s:.1} s before)")).unwrap_or_default()
                        ),
                    ),
                };
                Some(Alert {
                    id: format!("struck:{}", w.id),
                    kind: "struck",
                    severity,
                    uid,
                    callsign,
                    detail,
                    since_t: t,
                    op_id: None,
                    intent_id: None,
                })
            })
            .collect()
    }

    /// Rules 9: the acts observed since the last call, for the decision engine (each once).
    pub fn take_acts(&self) -> Vec<murmur_decide::ActReport> {
        std::mem::take(&mut lock(&self.inner).acts)
    }

    /// Rules 9: mirror the decision engine's identification ledger for display: each track's
    /// declaration in force (with its basis), the review flags in force on it, and the one
    /// that raised it to suspect. Decisions never read this mirror (the engine fills the
    /// picture from its ledger). Republishes if anything changed.
    pub fn mirror(&self, ledger: &murmur_decide::ident::Ledger) {
        let now = clock::wall();
        {
            let mut inner = lock(&self.inner);
            for (id, t) in &mut inner.tracks {
                t.declared = ledger.declaration(id).map(Declared::from_ledger);
                t.review = ledger
                    .flags()
                    .filter(|(f, s)| s.in_force() && f.tracks.iter().any(|x| x == id))
                    .map(|(f, s)| model::ReviewMark {
                        flag: f.id,
                        kind: f.kind.as_str().into(),
                        status: s.name().into(),
                        t: f.t.0 as f64 / 1000.0,
                        evidence: f.evidence.clone(),
                    })
                    .collect();
                t.raised = ledger.suspect_since(id).and_then(|_| {
                    ledger
                        .flags()
                        .filter(|(f, s)| s.in_force() && f.kind.raises_suspect() && f.tracks.iter().any(|x| x == id))
                        .map(|(f, _)| format!("F-{}: {}", f.id, f.evidence))
                        .next()
                });
            }
        }
        self.publish(now);
    }

    fn fold(&self, inner: &mut Inner, detections: &[Detection], now: f64) {
        for d in detections {
            let existing = inner
                .by_contact
                .get(&d.contact_id)
                .filter(|id| inner.tracks.contains_key(*id))
                .cloned();
            match existing {
                Some(id) => {
                    if let Some(t) = inner.tracks.get_mut(&id) {
                        t.update(d, now);
                    }
                }
                None => {
                    // rules 16: an id is never reused within a run (numbered up from 1, never
                    // reset), so the engine judges a lost contact even with its id in a picture
                    // (`engage::lost_standing`) and replay flags a dropped id back with another fix
                    let no = inner.next_no;
                    inner.next_no += 1;
                    let id = format!("T-{no:04}");
                    inner
                        .tracks
                        .insert(id.clone(), Track::new(id.clone(), no, d, now));
                    inner.by_contact.insert(d.contact_id.clone(), id);
                }
            }
        }
    }

    fn drop_old(&self, inner: &mut Inner, now: f64) {
        let drop_s = self.settings.drop_s;
        let gone: Vec<String> = inner
            .tracks
            .values()
            .filter(|t| t.age_s(now) > drop_s)
            .map(|t| t.id.clone())
            .collect();
        for id in gone {
            // rules 16: its facts at the drop, for the decision engine (the estimate only: the
            // engine fills the identification from its ledger)
            if let Some(t) = inner.tracks.remove(&id) {
                inner.dropped.insert(id.clone(), (t.facts(now), t.ais_identity()));
            }
            inner.by_contact.retain(|_, v| *v != id);
        }
    }

    /// Rules 16: the tracks dropped since the last call, each with its facts at the drop, for
    /// the decision engine (`Engine::record_track_drops`); from now on they are the engine's
    /// lost contacts and leave the decision facts. The caller holds the decision lock across
    /// this and the record, so no decision reads a picture without them in either.
    pub fn take_drops(&self) -> Vec<(String, TrackFacts, Option<AisIdentity>)> {
        std::mem::take(&mut lock(&self.inner).dropped)
            .into_iter()
            .map(|(id, (f, ais))| (id, f, ais))
            .collect()
    }

    /// Rules 16: drops the decision engine could not log (`Tracks::take_drops` handed them
    /// over), back for the next tick; they stay in the decision facts meanwhile.
    pub fn restore_drops(&self, drops: Vec<(String, TrackFacts, Option<AisIdentity>)>) {
        let mut inner = lock(&self.inner);
        for (id, f, ais) in drops {
            inner.dropped.entry(id).or_insert((f, ais));
        }
    }

    /// Rules 16: show the engine's lost contacts (the console's map and tracks list); each
    /// entry as [`crate::decision::Decision::lost_json`] builds it. Republishes if they
    /// changed.
    pub fn mirror_lost(&self, lost: Vec<Value>) {
        let changed = {
            let mut inner = lock(&self.inner);
            let changed = inner.lost != lost;
            inner.lost = lost;
            changed
        };
        if changed {
            self.publish(clock::wall());
        }
    }

    /// Once a second: drop forgotten tracks and republish if anything changed (a track
    /// going stale changes its state and symbol).
    pub fn tick(&self) {
        let now = clock::wall();
        {
            let mut inner = lock(&self.inner);
            self.drop_old(&mut inner, now);
            inner.weapons.age(now);
        }
        self.publish(now);
    }

    pub async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.tick();
        }
    }

    /// Every track as public JSON (perception only).
    pub fn tracks_json(&self, now: f64) -> Vec<Value> {
        lock(&self.inner)
            .tracks
            .values()
            .map(|t| t.json(now))
            .collect()
    }

    /// Rules 16: the engine's lost contacts as last mirrored.
    pub fn lost_json(&self) -> Vec<Value> {
        lock(&self.inner).lost.clone()
    }

    /// Public environmental sample only. Never serialize the truth message or its schedule.
    pub fn weather_json(&self, lat: f64, lon: f64, now: f64) -> Value {
        let stamp = {
            let inner = lock(&self.inner);
            inner
                .truth
                .as_ref()
                .map(|(rx, m)| (*rx, m.t, m.s, m.sensing.conditions))
        };
        let Some((rx, t, s, fixed)) = stamp else {
            return json!({"available": false, "note": "Waiting for scenario weather"});
        };
        let weather = lock(&self.physical).weather.at(lat, lon, t, s);
        let baseline = fixed.unwrap_or_default().sane();
        let effective = physical::conditions::Conditions {
            visibility_m: weather.values.visibility_m.unwrap_or(baseline.visibility_m),
            wind_mps: weather.values.wind_mps().unwrap_or(baseline.wind_mps),
            ..baseline
        }.sane();
        json!({"available": weather.values != murmur_weather::Values::default(),
            "lat": lat, "lon": lon, "t": t, "age_s": (now-rx).max(0.0), "stale": now-rx > 10.0,
            "weather": weather,
            "sensing_model": self.sensing_model().as_str(),
            "sensing": {"visibility_m": effective.visibility_m, "wind_mps": effective.wind_mps},
            "hourly_interval_end_t": (t / 3600.0).ceil() * 3600.0,
            "note": "Scenario surface weather, not an operational observation or forecast. Nearest weather grid point; visibility and mean wind affect physical sensing. Missing inputs use the stated fixed sensing baseline. Clouds, rain and gusts are information only; no cloud-layer occlusion or weather-driven communications loss."})
    }

    /// A bounded environmental raster at one simulation instant, south row first.
    pub fn weather_grid_json(&self, bounds: [f64; 4], now: f64) -> Value {
        let stamp = {
            let inner = lock(&self.inner);
            inner.truth.as_ref().map(|(rx, m)| (*rx, m.t, m.s, m.sensing.conditions))
        };
        let Some((rx, t, s, fixed)) = stamp else {
            return json!({"available": false, "note": "Waiting for scenario weather"});
        };
        let (nlon, nlat) = (32, 24);
        let [w, south, e, n] = bounds;
        let engine = lock(&self.physical);
        let meta = engine.weather.at((south+n)/2.0, (w+e)/2.0, t, s);
        let mut cells = Vec::with_capacity(nlon*nlat);
        for row in 0..nlat {
            for col in 0..nlon {
                let lat = south + (row as f64+0.5)*(n-south)/nlat as f64;
                let lon = w + (col as f64+0.5)*(e-w)/nlon as f64;
                cells.push(engine.weather.at(lat,lon,t,s).values);
            }
        }
        json!({"available": cells.iter().any(|v| *v != murmur_weather::Values::default()),
            "bounds": bounds, "nlon": nlon, "nlat": nlat, "cells": cells,
            "t": t, "age_s": (now-rx).max(0.0), "stale": now-rx > 10.0,
            "source": meta.source, "model": meta.model, "synthetic": meta.synthetic,
            "archive_sha256": meta.archive_sha256,
            // bd murmur-4tq: the sun at the grid's centre now (the scenario's stated sun, else the clock's)
            "sun_elevation_deg": round(fixed.unwrap_or_default().sane().sun_at(t, (south+n)/2.0, (w+e)/2.0), 1),
            "note": "Current surface-weather model. Nearest archived grid point; uniform scripted weather. Current values only; no contact data."})
    }

    /// `GET /api/sensing/reach` (bd murmur-4tq, [`reach`]; information only, never logged): each
    /// vehicle's sensors' reach now against a reference contact, beside the catalog's clear-day
    /// reach, in the conditions the physical model would sense in where it is, and what keeps it
    /// short. Only `uid`'s if given. Each vehicle's answer is kept and computed again after
    /// [`REACH_MAX_AGE_S`] or as soon as its height, weather or sun moved enough
    /// ([`ReachKey::close`]); nothing is locked while it computes (the physical model only while
    /// the weather samples are read). Under the legacy model there is nothing to show: weather
    /// and light change no detection there.
    pub fn reach_json(&self, world: &World, uid: Option<u64>, now: f64) -> Value {
        let model = self.sensing_model();
        if model == SensingModel::Legacy {
            return json!({"available": true, "model": "legacy", "vehicles": [],
                "note": "Legacy sensing: weather and light change no detection; each sensor reaches its catalog range_m."});
        }
        let stamp = {
            let inner = lock(&self.inner);
            inner.truth.as_ref().map(|(rx, m)| (*rx, m.t, m.s, m.sensing.clone()))
        };
        let Some((rx, t, s, sensing)) = stamp else {
            return json!({"available": false, "model": model.as_str(), "vehicles": [], "note": "Waiting for the first sensing step"});
        };
        let carriers: Vec<(Observer, Carrier)> = self
            .carriers(world)
            .into_iter()
            .filter(|(o, _)| uid.is_none_or(|u| u == o.uid))
            .collect();
        // each observer's conditions, read under the physical model's lock and nothing more
        let conds: Vec<(physical::conditions::Conditions, Option<murmur_weather::Sample>)> = {
            let engine = lock(&self.physical);
            carriers.iter().map(|(o, _)| engine.conditions_at(&sensing, o.lat, o.lon, t, s)).collect()
        };
        let mut out = Vec::with_capacity(carriers.len());
        for ((o, c), (mut cond, sample)) in carriers.into_iter().zip(conds) {
            let sun = cond.sun_at(t, o.lat, o.lon);
            cond.sun_elevation_deg = Some(sun);
            let key = ReachKey { platform: c.platform.clone(), alt_m: o.alt_msl, submerged: o.submerged, cond };
            let cached = lock(&self.reach_cache)
                .get(&o.uid)
                .filter(|e| now >= e.at && now - e.at < REACH_MAX_AGE_S && e.key.close(&key))
                .map(|e| (e.at, e.value.clone()));
            let (at, mut value) = match cached {
                Some(hit) => hit,
                None => {
                    let value = self.reach_vehicle(&o, &c, &cond, t, sample.as_ref());
                    lock(&self.reach_cache).insert(o.uid, ReachEntry { at: now, key, value: value.clone() });
                    (now, value)
                }
            };
            value["age_s"] = json!(round((now - at).max(0.0), 1));
            out.push(value);
        }
        if uid.is_none() {
            let live: std::collections::HashSet<u64> = out.iter().filter_map(|v| v["uid"].as_u64()).collect();
            lock(&self.reach_cache).retain(|u, _| live.contains(u));
        }
        json!({"available": true, "model": model.as_str(), "t": t, "age_s": round((now - rx).max(0.0), 1), "stale": now - rx > 10.0,
            "reference": {"kind": reach::REFERENCE_KIND, "air_alt_m": 300, "subsurface_depth_m": 30},
            "vehicles": out,
            "note": "Information only: never logged and never a check; the decision engine still assumes each sensor's clear-day reach (the catalog's physical_reach_m). Each reach is a disc against a reference contact of no known kind (an aircraft 300 m up, a submarine 30 m down, else at the surface) with the sensor pointed at it: no terrain, the sonar's water the scenario's stated depth. limited_by names the input whose clear-day value gives back the most reach (docs/WEATHER.md)."})
    }

    /// One vehicle's reach answer.
    fn reach_vehicle(&self, o: &Observer, c: &Carrier, cond: &physical::conditions::Conditions, t: f64, sample: Option<&murmur_weather::Sample>) -> Value {
        let clear = self.clear_day.get(&c.platform);
        let sensors: Vec<Value> = o
            .sensors
            .iter()
            .filter_map(|spec| reach::sensor_now(spec, o, cond, t, clear.and_then(|m| m.get(spec.kind.as_str())), &c.domain))
            .map(|sn| {
                let domains: Vec<Value> = sn
                    .domains
                    .iter()
                    .map(|d| {
                        json!({
                            "domain": reach::domain_name(d.domain),
                            "detect_m": round(d.detect_m, 0),
                            "recognise_m": round(d.recognise_m, 0),
                            "identify_m": round(d.identify_m, 0),
                            "clear_day_m": d.clear_day_m,
                            "ratio": d.ratio().map(|r| round(r, 3)),
                            "limited_by": d.limited_by,
                            "factors": d.factors.iter().map(|(f, m)| json!({"factor": f, "detect_m": round(*m, 0)})).collect::<Vec<_>>(),
                            "horizon_limited": d.horizon_limited,
                        })
                    })
                    .collect();
                json!({"type": sn.kind, "primary_domain": reach::domain_name(sn.primary), "domains": domains})
            })
            .collect();
        let sun = cond.sun_elevation_deg.unwrap_or(45.0);
        json!({
            "uid": o.uid,
            "callsign": c.callsign,
            "platform": c.platform,
            "alt_msl_m": round(o.alt_msl, 1),
            "submerged": o.submerged,
            "conditions": {
                "visibility_m": round(cond.visibility_m, 0),
                "sun_elevation_deg": round(sun, 1),
                "light": reach::light(sun),
                "wind_mps": round(cond.wind_mps, 1),
                "sea_state": cond.sea_state,
                "synthetic": sample.is_some_and(|w| w.synthetic),
                "source": sample.map(|w| w.source.clone()),
                "model": sample.and_then(|w| w.model.clone()),
            },
            "sensors": sensors,
        })
    }

    /// `GET /api/tracks`.
    pub fn api_json(&self, now: f64) -> Value {
        let tracks = self.tracks_json(now);
        json!({
            "t": round(now, 2),
            "count": tracks.len(),
            "stale_after_s": model::STALE_S,
            "drop_after_s": self.settings.drop_s,
            "tracks": tracks,
            // rules 16: contacts the tracker dropped that the keep-outs still count
            "lost": self.lost_json(),
        })
    }

    /// The WebSocket message, `{"type": "tracks", ...}`.
    fn message(&self, now: f64) -> String {
        let mut msg = json!({
            "type": "tracks",
            "t": round(now, 2),
            "stale_after_s": model::STALE_S,
            "tracks": self.tracks_json(now),
            "lost": self.lost_json(),
        });
        // opponent weapons: the weapon picture, when there is one (absent otherwise)
        let weapons = {
            let inner = lock(&self.inner);
            inner.weapons.json(|u| inner.ours_at.get(&u).copied())
        };
        if !weapons.is_empty() {
            msg["weapons"] = Value::Array(weapons);
        }
        msg.to_string()
    }

    /// Push the tracks to WebSocket clients if they changed. Built only while anyone watches,
    /// as `push::pusher`'s frames are, and for a newcomer at [`Self::subscribe`]: every track's
    /// JSON three times a second for no one was most of a large headless episode's time
    /// (murmur-4yj), where no console ever connects.
    fn publish(&self, now: f64) {
        if self.updates.receiver_count() == 0 {
            return;
        }
        let msg = self.message(now);
        self.updates.send_if_modified(|cur| {
            if **cur == msg {
                false
            } else {
                *cur = Arc::new(msg);
                true
            }
        });
    }

    /// A WebSocket client's view of the tracks: the message as of now first (none is built
    /// while no one watches), then every change.
    pub fn subscribe(&self) -> watch::Receiver<Arc<String>> {
        let rx = self.updates.subscribe();
        self.publish(clock::wall());
        rx
    }

    /// Every track's last fix and estimated affiliation, for the console's cluster counts
    /// (`crate::push`).
    pub fn marks(&self) -> Vec<crate::push::TrackMark> {
        lock(&self.inner)
            .tracks
            .values()
            .map(|t| crate::push::TrackMark {
                lon: t.lon,
                lat: t.lat,
                affiliation: t.affiliation().0,
            })
            .collect()
    }

    /// The decision layer's track facts at wall time `now`: every track, and (rules 16) every
    /// track dropped that the engine has not taken yet, as it was at its drop.
    pub fn facts(&self, now: f64) -> BTreeMap<String, TrackFacts> {
        let inner = lock(&self.inner);
        inner
            .tracks
            .values()
            .map(|t| (t.id.clone(), t.facts(now)))
            .chain(inner.dropped.iter().map(|(id, (f, _))| (id.clone(), f.clone())))
            .collect()
    }

    /// Rules 16: the AIS identity heard for each track of [`Self::facts`] (a dropped one's as
    /// at its drop), for `WorldSnapshot::track_ais`.
    pub fn ais(&self) -> BTreeMap<String, AisIdentity> {
        let inner = lock(&self.inner);
        inner
            .tracks
            .values()
            .filter_map(|t| t.ais_identity().map(|a| (t.id.clone(), a)))
            .chain(inner.dropped.iter().filter_map(|(id, (_, a))| a.clone().map(|a| (id.clone(), a))))
            .collect()
    }

    /// Rules 21: each live track's identity level and the looks that identified it, for
    /// `WorldSnapshot::track_levels` (what ends an identify order).
    pub fn levels(&self) -> BTreeMap<String, murmur_decide::identify::TrackLevel> {
        use murmur_decide::identify::{Level as L, TrackLevel};
        let inner = lock(&self.inner);
        inner
            .tracks
            .values()
            .filter_map(|t| {
                let level = match t.identity().level? {
                    sensing::Level::Detect => L::Detect,
                    sensing::Level::Recognise => L::Recognise,
                    sensing::Level::Identify => L::Identify,
                };
                let id = t.identity();
                Some((
                    t.id.clone(),
                    TrackLevel {
                        level,
                        by: t.identified_by(),
                        // rules 22: when our own sensors last identified it, and its conflict
                        own_t: t.own_identified_t().map(|s| murmur_decide::Time((s * 1000.0).round() as i64)),
                        conflict: id.conflict,
                        // rules 23: and each of our vehicles' own latest identification
                        own: t
                            .own_identifications()
                            .into_iter()
                            .map(|(u, (s, kind))| {
                                let at = murmur_decide::Time((s * 1000.0).round() as i64);
                                (u.to_string(), murmur_decide::identify::OwnLook { t: at, kind: Some(kind) })
                            })
                            .collect(),
                    },
                ))
            })
            .collect()
    }

    /// Rules 21: the contacts behind track `track` (the truth ids its detections came from).
    fn contacts_of(inner: &Inner, track: &str) -> std::collections::BTreeSet<String> {
        inner.by_contact.iter().filter(|(_, t)| t.as_str() == track).map(|(c, _)| c.clone()).collect()
    }

    /// Tracks for the CoT exporter.
    pub fn cot_samples(&self, now: f64) -> Vec<crate::cot::TrackSample> {
        lock(&self.inner)
            .tracks
            .values()
            .map(|t| {
                let (plat, plon) = t.predicted(now);
                let (aff, _, why) = t.affiliation();
                let id = t.identity();
                crate::cot::TrackSample {
                    no: t.no,
                    id: t.id.clone(),
                    sidc: t.sidc(now),
                    lat: plat,
                    lon: plon,
                    alt_m: t.alt_m,
                    sigma_m: t.sigma_at(now),
                    course: t.course_deg(),
                    speed: t.speed_mps(),
                    confidence: t.confidence_at(now),
                    affiliation: aff.as_str(),
                    reason: why,
                    kind: id.kind,
                    level: id.level.map(sensing::Level::as_str),
                    identity_confidence: id.confidence,
                    ais_name: id.ais.map(|a| a.name),
                    sources: t
                        .sources
                        .values()
                        .map(|s| format!("{}:{}", s.uid, s.sensor.as_str()))
                        .collect(),
                    last_seen_t: t.last_seen_t,
                    stale: t.is_stale(now),
                    fused: t.sources.len() > 1,
                }
            })
            .collect()
    }

    /// Whether there is a track `id`.
    pub fn has(&self, id: &str) -> bool {
        lock(&self.inner).tracks.contains_key(id)
    }

    /// An engagement's abstract effect, for the contacts process (see the module docs).
    pub fn queue_effect(&self, e: &murmur_decide::EffectOrder, ours: Vec<Value>) {
        let mut inner = lock(&self.inner);
        let contact = inner
            .by_contact
            .iter()
            .find(|(_, t)| **t == e.track)
            .map(|(c, _)| c.clone());
        let mut effect = json!({
            "intent": e.intent.0,
            "track": e.track,
            "target_contact": contact,
            "t": e.t.0 as f64 / 1000.0,
            "point": [e.point.lon, e.point.lat],
            "radius_m": e.radius_m,
            "platform": e.uid.as_str(),
            "ours": ours,
        });
        // rules 20: the munition in flight (engine times); its impact goes on the contacts'
        // clock when the effect is handed over (`take_effects_at`)
        if let Some(m) = &e.munition {
            effect["munition"] = serde_json::to_value(m).unwrap_or(Value::Null);
        }
        inner.effects.push(effect);
    }

    /// The effects to hand the contacts process now (each once), as queued (tests).
    #[cfg(test)]
    pub(crate) fn take_effects(&self) -> Vec<Value> {
        let mut inner = lock(&self.inner);
        let out: Vec<Value> = std::mem::take(&mut inner.effects);
        inner.effects_sent.extend(out.iter().cloned());
        out
    }

    /// [`Self::take_effects`] in the answer to a truth message of scenario time `truth_t` (unix
    /// s) read at bridge time `now` (s): rules 20, an effect a munition carries strikes at its
    /// impact, which the contacts process reads on its own scenario clock (`impact_t`: the
    /// message's time plus the impact's time from now; at once if it has passed).
    pub(crate) fn take_effects_at(&self, truth_t: f64, now: f64) -> Vec<Value> {
        let mut inner = lock(&self.inner);
        let mut out: Vec<Value> = std::mem::take(&mut inner.effects);
        for e in &mut out {
            let impact = e
                .get("munition")
                .and_then(|m| m.get("impact_t"))
                .and_then(Value::as_f64)
                .map(|ms| ms / 1000.0);
            if let Some(impact) = impact {
                e["impact_t"] = json!(truth_t + (impact - now).max(0.0));
            }
        }
        inner.effects_sent.extend(out.iter().cloned());
        out
    }

    /// Evaluation only: truth and perception side by side, with the association.
    pub fn eval_json(&self, now: f64) -> Value {
        let inner = lock(&self.inner);
        let pairs = association_of(&inner);
        json!({
            "t": round(now, 2),
            "truth": inner.truth.as_ref().map(|(rx, m)| json!({ "received_t": round(*rx, 2), "message": m })),
            "tracks": inner.tracks.values().map(|t| t.json(now)).collect::<Vec<_>>(),
            "association": pairs,
            "last_detections": inner.last_detections,
            "effects_sent": inner.effects_sent,
            // rules 9: every hostile act, observed (and the report it became) or not
            "acts": inner.act_records,
        })
    }

    /// Evaluation only: [`Self::eval_json`]'s `association` alone, for the headless simulator's
    /// truth and picture samplers, which read nothing else of it (building every track's JSON
    /// and the last truth message for them was a share of a large episode's time, murmur-4yj).
    #[cfg(murmur_headless)]
    pub fn association_json(&self) -> Vec<Value> {
        association_of(&lock(&self.inner))
    }

    /// The last sensing step, for the headless simulator's evaluation only (never served by the
    /// bridge): each detection with its contact's truth in that step's message, and each
    /// observer (its uid, where it was, its navigation error, metres east and north).
    #[cfg(murmur_headless)]
    pub fn last_step_json(&self) -> Value {
        let inner = lock(&self.inner);
        let msg = inner.truth.as_ref().map(|(_, m)| m);
        let truth: HashMap<&str, (f64, f64)> =
            msg.map(|m| m.contacts.iter().map(|c| (c.id.as_str(), (c.lat, c.lon))).collect()).unwrap_or_default();
        let (dets, obs) = &inner.last_step;
        json!({
            "s": msg.map(|m| m.s),
            "detections": dets.iter().map(|d| json!({
                "uid": d.uid, "sensor": d.sensor.as_str(), "contact": d.contact_id,
                "lat": d.lat, "lon": d.lon, "sigma_m": d.sigma_m, "slant": d.slant_from.is_some(),
                "truth": truth.get(d.contact_id.as_str()).map(|(lat, lon)| [lat, lon]),
            })).collect::<Vec<_>>(),
            "observers": obs.iter().map(|(uid, lat, lon, (e, n))| json!({
                "uid": uid, "lat": lat, "lon": lon, "nav_error_m": [e, n],
            })).collect::<Vec<_>>(),
        })
    }

    #[cfg(test)]
    pub fn track(&self, id: &str) -> Option<Track> {
        lock(&self.inner).tracks.get(id).cloned()
    }
}

/// The process-wide store (as `cot::inbound()`), created by `init` at startup.
static STORE: OnceLock<Arc<Tracks>> = OnceLock::new();

/// Load the sensor catalog and start the store. Called once from main; a second call is a
/// no-op.
pub fn init(platforms: &Path) -> Result<Arc<Tracks>, String> {
    if let Some(s) = STORE.get() {
        return Ok(s.clone());
    }
    let t = Arc::new(Tracks::load(platforms)?);
    let _ = STORE.set(t.clone());
    Ok(t)
}

/// The store, if `init` ran (tests and tools without it get None).
pub fn store() -> Option<Arc<Tracks>> {
    STORE.get().cloned()
}

// ------------------------------------------------------------------ HTTP

#[derive(Clone)]
struct Api {
    tracks: Arc<Tracks>,
    world: Arc<World>,
    decision: Arc<Decision>,
    auth: Arc<crate::auth::Auth>,
}

fn error(status: StatusCode, detail: impl Into<String>) -> Response {
    (status, Json(json!({ "detail": detail.into() }))).into_response()
}

/// `/api/tracks`, `/api/tracks/{id}`, `/api/tracks/{id}/declare`, and the simulation
/// channel `/api/sim/contacts` (in) and `/api/sim/truth` (evaluation only).
pub fn router<S: Clone + Send + Sync + 'static>(
    tracks: Arc<Tracks>,
    world: Arc<World>,
    decision: Arc<Decision>,
    auth: Arc<crate::auth::Auth>,
) -> Router<S> {
    Router::new()
        .route("/api/tracks", get(list))
        .route("/api/tracks/{id}", get(one))
        .route("/api/tracks/{id}/declare", post(declare))
        .route("/api/tracks/{id}/confirm", post(confirm))
        .route("/api/tracks/{id}/revoke", post(revoke))
        .route("/api/review", get(review))
        .route("/api/review/{flag}/resolve", post(resolve))
        .route("/api/review/lost/{track}/confirm", post(confirm_lost))
        .route("/api/sim/contacts", post(sim_contacts))
        .route("/api/sim/truth", get(sim_truth))
        .with_state(Api {
            tracks,
            world,
            decision,
            auth,
        })
}

async fn list(State(api): State<Api>) -> Response {
    Json(api.tracks.api_json(clock::wall())).into_response()
}

async fn one(State(api): State<Api>, AxPath(id): AxPath<String>) -> Response {
    let now = clock::wall();
    match api.tracks.tracks_json(now).into_iter().find(|t| t["id"] == id) {
        Some(t) => Json(t).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("unknown track {id}")),
    }
}

#[derive(Deserialize)]
struct DeclareBody {
    affiliation: Affiliation,
    #[serde(default)]
    note: Option<String>,
    /// Rules 8: the operator's password again, when the policy marks a hostile declaration
    /// consequential (`[approval] step_up_acts`); checked and dropped, never logged.
    #[serde(default)]
    password: Option<String>,
    /// Rules 10: or a passkey's assertion made for declaring this track hostile (logged).
    #[serde(default)]
    passkey: Option<serde_json::Value>,
    /// Rules 9: the review flag this declaration answers (in force on the track).
    #[serde(default)]
    flag: Option<u64>,
}

async fn declare(
    State(api): State<Api>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    body: Result<Json<DeclareBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let op = match api.auth.actor(&headers, true) {
        Ok(op) => op,
        Err(e) => return error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::FORBIDDEN), e.detail),
    };
    // rules 8: a hostile declaration re-enters the credential when the policy lists it,
    // checked before anything changes
    let hostile = body.affiliation == Affiliation::Hostile;
    let needed = hostile && api.decision.steps_up(murmur_decide::StepUpAct::DeclareHostile);
    let proof = crate::auth::Proof { password: body.password, passkey: body.passkey };
    if proof.is_some() && !hostile {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "a password or passkey goes with a hostile declaration (a step-up) only");
    }
    let step = match api.auth.step_up(&op, &format!("declare {id} hostile"), proof, needed).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if !api.tracks.has(&id) {
        return error(StatusCode::NOT_FOUND, format!("unknown track {id}"));
    }
    // rules 9: through the engine's identification ledger (logged as `ident.declared` under
    // the operator's session before anything changes); the tracks mirror it for display
    match api
        .decision
        .declare(&op, step, &id, body.affiliation, body.note.clone(), body.flag)
    {
        Ok(v) => Json(track_answer(&api, &id, v)).into_response(),
        Err(e) => error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), e.detail),
    }
}

/// The track as it stands now, with what the identification call did.
fn track_answer(api: &Api, id: &str, outcome: Value) -> Value {
    let now = clock::wall();
    let track = api.tracks.tracks_json(now).into_iter().find(|t| t["id"] == id);
    let mut v = track.unwrap_or_else(|| json!({ "id": id }));
    v["identification"] = outcome;
    v
}

#[derive(Deserialize)]
struct ConfirmBody {
    /// Rules 8: the password again, when `[approval] step_up_acts` lists
    /// `confirm_declaration` (off by default); checked and dropped, never logged.
    #[serde(default)]
    password: Option<String>,
    /// Rules 10: or a passkey's assertion made for this confirmation (logged).
    #[serde(default)]
    passkey: Option<serde_json::Value>,
}

/// Rules 9: `POST /api/tracks/{id}/confirm`: any operator confirms the rule declaration in
/// force on the track (`ident.confirmed`, under their session).
async fn confirm(
    State(api): State<Api>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    body: Option<Json<ConfirmBody>>,
) -> Response {
    let body = body.map(|Json(b)| b).unwrap_or(ConfirmBody { password: None, passkey: None });
    let op = match api.auth.actor(&headers, true) {
        Ok(op) => op,
        Err(e) => return error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::FORBIDDEN), e.detail),
    };
    let needed = api.decision.steps_up(murmur_decide::StepUpAct::ConfirmDeclaration);
    let proof = crate::auth::Proof { password: body.password, passkey: body.passkey };
    let step = match api.auth.step_up(&op, &format!("confirm {id}'s declaration"), proof, needed).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    match api.decision.confirm(&op, step, &id) {
        Ok(v) => Json(track_answer(&api, &id, v)).into_response(),
        Err(e) => error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), e.detail),
    }
}

#[derive(Deserialize)]
struct RevokeBody {
    #[serde(default)]
    note: Option<String>,
}

/// Rules 9: `POST /api/tracks/{id}/revoke`: any operator revokes the declaration in force on
/// the track (an operator's or the rule's; it only restricts). Every engagement in play on
/// the track is stopped first (cause revoked; the fallback where one was under way).
async fn revoke(
    State(api): State<Api>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    body: Option<Json<RevokeBody>>,
) -> Response {
    let note = body.and_then(|Json(b)| b.note);
    let op = match api.auth.actor(&headers, true) {
        Ok(op) => op,
        Err(e) => return error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::FORBIDDEN), e.detail),
    };
    match api.decision.revoke(&op, &id, note) {
        Ok(v) => Json(track_answer(&api, &id, v)).into_response(),
        Err(e) => error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), e.detail),
    }
}

#[derive(Deserialize)]
struct ResolveBody {
    resolution: murmur_decide::FlagResolution,
    #[serde(default)]
    note: Option<String>,
}

/// Rules 9: `POST /api/review/{flag}/resolve` `{resolution: confirmed|dismissed}`: any
/// operator confirms or dismisses a review flag (`ident.flag_resolved`).
async fn resolve(
    State(api): State<Api>,
    AxPath(flag): AxPath<u64>,
    headers: HeaderMap,
    body: Result<Json<ResolveBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let op = match api.auth.actor(&headers, true) {
        Ok(op) => op,
        Err(e) => return error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::FORBIDDEN), e.detail),
    };
    match api.decision.resolve_flag(&op, flag, body.resolution, body.note) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), e.detail),
    }
}

#[derive(Deserialize)]
struct ConfirmLostBody {
    by_track: String,
}

/// Rules 16: `POST /api/review/lost/{track}/confirm` `{by_track}`: any operator confirms, under
/// their own session, that live track `by_track` is lost contact `track` again (its claim, an
/// AIS identity: `track.reacquired`); 409 while anything contradicts it. Agents have no route
/// here and no MCP tool for it.
async fn confirm_lost(
    State(api): State<Api>,
    AxPath(track): AxPath<String>,
    headers: HeaderMap,
    body: Result<Json<ConfirmLostBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let op = match api.auth.actor(&headers, true) {
        Ok(op) => op,
        Err(e) => return error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::FORBIDDEN), e.detail),
    };
    match api.decision.confirm_lost_claim(&op, &track, &body.by_track) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), e.detail),
    }
}

/// Rules 9: `GET /api/review`: the identification review items (rule declarations awaiting
/// an operator, flags in force, the latest resolved).
async fn review(State(api): State<Api>) -> Response {
    Json(api.decision.review_json()).into_response()
}

/// The largest truth message the simulation channel reads, bytes (axum's default for a JSON
/// body).
const SIM_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// The simulation channel in. Truth: stored for the sensing model, never served back
/// except by `sim_truth` with `MURMUR_EVAL_TRUTH=1`.
async fn sim_contacts(State(api): State<Api>, request: axum::extract::Request) -> Response {
    // only the stack's own contacts process: truth reaches the sensing model, and through it
    // acts reach the identification rule. The token is checked on the headers before the
    // body is read at all.
    if let Err(why) = api.tracks.sim_token_ok(request.headers()) {
        return error(StatusCode::UNAUTHORIZED, why);
    }
    let bytes = match axum::body::to_bytes(request.into_body(), SIM_BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::PAYLOAD_TOO_LARGE, format!("truth message: {e}")),
    };
    let msg: TruthMessage = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, format!("truth message: {e}")),
    };
    let truth_t = msg.t;
    let aim_at = msg.aim_at.clone();
    match api.tracks.on_truth(msg, &api.world) {
        Ok((contacts, detections, tracks)) => {
            let mut answer = json!({
                "ok": true,
                "contacts": contacts,
                "detections": detections,
                "tracks": tracks,
                // the simulation channel back: abstract effects for the truth side to apply (rules
                // 20: a munition's at its impact, on the truth message's clock)
                "effects": api.tracks.take_effects_at(truth_t, clock::wall()),
            });
            // opponent weapons: where the vehicles they may be aimed at are (only when asked)
            if !aim_at.is_empty() {
                answer["ours"] = Value::Array(api.tracks.ours_for(&api.world, &aim_at));
            }
            Json(answer).into_response()
        }
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

async fn sim_truth(State(api): State<Api>) -> Response {
    if !api.tracks.eval_truth_enabled() {
        return error(
            StatusCode::NOT_FOUND,
            "the evaluation truth endpoint is off; start the bridge with MURMUR_EVAL_TRUTH=1",
        );
    }
    Json(api.tracks.eval_json(clock::wall())).into_response()
}
