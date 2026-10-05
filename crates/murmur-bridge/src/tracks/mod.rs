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
//! Deterministic: every draw is a hash of (seed, scenario second, vehicle, contact, sensor),
//! so the same scenario, seed and fleet positions give the same detections. The seed and
//! misclassification rate come from the scenario (in the truth message); `MURMUR_SENSE_SEED`
//! and `MURMUR_SENSE_MISCLASS` override them on the bridge.
//!
//! **Effects** (Phase E): an engagement's abstract effect (a point, a radius, a time) goes
//! back to the contacts process on the same labelled channel, in the answer to its next
//! `POST /api/sim/contacts` (`effects`), with the contact the target track was associated
//! with (the private association, never serialized elsewhere) and where our vehicles
//! reported themselves, for the evaluation truth log's collateral record. The contacts
//! process marks the contacts inside the radius; nothing here models damage.
//!
//! Operators may declare a track's affiliation (`POST /api/tracks/{id}/declare`, the
//! signed-in operator as for intents; a dev run's header); the declaration is logged as an
//! `operator.action` and overrides the estimate. Hostile is never derived by the model.

pub mod model;
pub mod sensing;
#[cfg(test)]
mod tests;
pub mod truth;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use murmur_decide::{Affiliation, TrackFacts};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use self::model::{Declared, Track};
use self::sensing::{Detection, Observer, Params, SensorCatalog, misclassify_draw};
use self::truth::TruthMessage;
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
        }
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
}

pub struct Tracks {
    catalog: SensorCatalog,
    settings: Settings,
    inner: Mutex<Inner>,
    updates: watch::Sender<Arc<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Tracks {
    pub fn new(catalog: SensorCatalog, settings: Settings) -> Tracks {
        let t = Tracks {
            catalog,
            settings,
            inner: Mutex::new(Inner {
                truth: None,
                tracks: BTreeMap::new(),
                by_contact: HashMap::new(),
                next_no: 1,
                last_detections: 0,
                effects: Vec::new(),
                effects_sent: Vec::new(),
            }),
            updates: watch::channel(Arc::new(String::new())).0,
        };
        t.publish(clock::wall());
        t
    }

    /// From `sim/platforms.toml` and the environment; a bad catalog is an error (the sensing
    /// model would silently see nothing).
    pub fn load(platforms: &Path) -> Result<Tracks, String> {
        Ok(Tracks::new(SensorCatalog::load(platforms)?, Settings::from_env()))
    }

    pub fn eval_truth_enabled(&self) -> bool {
        self.settings.eval_truth
    }

    /// Our vehicles as sensor carriers: those with a position and a live link.
    fn observers(&self, world: &World) -> Vec<Observer> {
        let mono = clock::mono();
        world
            .vehicles()
            .into_iter()
            .filter_map(|cell| {
                let link = cell.link_snapshot(mono);
                let fresh = link
                    .last_rx_t
                    .is_some_and(|t| mono - t <= link.stale_after_s());
                if !fresh {
                    return None;
                }
                cell.read(|v| {
                    let (lat, lon) = v.pos?;
                    let id = v.identity.as_deref()?;
                    let platform = id.str("platform")?;
                    let ps = self.catalog.get(platform)?;
                    if ps.sensors.is_empty() {
                        return None;
                    }
                    let alt = v.alt_msl.unwrap_or(0.0);
                    let submerged = id.str("domain") == Some("subsurface") && alt < -0.5;
                    Some(Observer {
                        uid: cell.uid,
                        lat,
                        lon,
                        alt_msl: alt,
                        mast_m: ps.mast_m,
                        submerged,
                        sensors: ps.sensors.clone(),
                    })
                })
            })
            .collect()
    }

    /// A truth message arrived: run the sensing step against our vehicles and fold the
    /// detections in. Returns (contacts, detections, tracks).
    pub fn on_truth(&self, msg: TruthMessage, world: &World) -> Result<(usize, usize, usize), String> {
        msg.validate()?;
        let observers = self.observers(world);
        let now = clock::wall();
        self.on_truth_with(msg, &observers, now)
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
        let detections = sensing::detect(params, observers, &msg.contacts, &|contact, sensor| {
            misclassify_draw(seed, contact, sensor, rate)
        });
        let n_contacts = msg.contacts.len();
        let n_tracks = {
            let mut inner = lock(&self.inner);
            self.fold(&mut inner, &detections, now);
            inner.truth = Some((now, msg));
            inner.last_detections = detections.len();
            self.drop_old(&mut inner, now);
            inner.tracks.len()
        };
        self.publish(now);
        Ok((n_contacts, detections.len(), n_tracks))
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
            inner.tracks.remove(&id);
            inner.by_contact.retain(|_, v| *v != id);
        }
    }

    /// Once a second: drop forgotten tracks and republish if anything changed (a track
    /// going stale changes its state and symbol).
    pub fn tick(&self) {
        let now = clock::wall();
        {
            let mut inner = lock(&self.inner);
            self.drop_old(&mut inner, now);
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

    /// `GET /api/tracks`.
    pub fn api_json(&self, now: f64) -> Value {
        let tracks = self.tracks_json(now);
        json!({
            "t": round(now, 2),
            "count": tracks.len(),
            "stale_after_s": model::STALE_S,
            "drop_after_s": self.settings.drop_s,
            "tracks": tracks,
        })
    }

    /// The WebSocket message, `{"type": "tracks", ...}`.
    fn message(&self, now: f64) -> String {
        json!({
            "type": "tracks",
            "t": round(now, 2),
            "stale_after_s": model::STALE_S,
            "tracks": self.tracks_json(now),
        })
        .to_string()
    }

    fn publish(&self, now: f64) {
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

    pub fn subscribe(&self) -> watch::Receiver<Arc<String>> {
        self.updates.subscribe()
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

    /// The decision layer's track facts at wall time `now`.
    pub fn facts(&self, now: f64) -> BTreeMap<String, TrackFacts> {
        lock(&self.inner)
            .tracks
            .values()
            .map(|t| (t.id.clone(), t.facts(now)))
            .collect()
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

    /// An operator's declaration. Returns the track's JSON.
    pub fn declare(
        &self,
        id: &str,
        affiliation: Affiliation,
        by: &str,
        note: Option<String>,
        now: f64,
    ) -> Option<Value> {
        let mut inner = lock(&self.inner);
        let t = inner.tracks.get_mut(id)?;
        t.declared = Some(Declared {
            affiliation,
            by: by.to_string(),
            t: now,
            note,
        });
        let v = t.json(now);
        drop(inner);
        self.publish(now);
        Some(v)
    }

    /// An engagement's abstract effect, for the contacts process (see the module docs).
    pub fn queue_effect(&self, e: &murmur_decide::EffectOrder, ours: Vec<Value>) {
        let mut inner = lock(&self.inner);
        let contact = inner
            .by_contact
            .iter()
            .find(|(_, t)| **t == e.track)
            .map(|(c, _)| c.clone());
        inner.effects.push(json!({
            "intent": e.intent.0,
            "track": e.track,
            "target_contact": contact,
            "t": e.t.0 as f64 / 1000.0,
            "point": [e.point.lon, e.point.lat],
            "radius_m": e.radius_m,
            "platform": e.uid.as_str(),
            "ours": ours,
        }));
    }

    /// The effects to hand the contacts process now (each once).
    pub(crate) fn take_effects(&self) -> Vec<Value> {
        let mut inner = lock(&self.inner);
        let out: Vec<Value> = std::mem::take(&mut inner.effects);
        inner.effects_sent.extend(out.iter().cloned());
        out
    }

    /// Evaluation only: truth and perception side by side, with the association.
    pub fn eval_json(&self, now: f64) -> Value {
        let inner = lock(&self.inner);
        let pairs: Vec<Value> = inner
            .by_contact
            .iter()
            .filter(|(_, t)| inner.tracks.contains_key(*t))
            .map(|(c, t)| json!({ "contact_id": c, "track_id": t }))
            .collect();
        json!({
            "t": round(now, 2),
            "truth": inner.truth.as_ref().map(|(rx, m)| json!({ "received_t": round(*rx, 2), "message": m })),
            "tracks": inner.tracks.values().map(|t| t.json(now)).collect::<Vec<_>>(),
            "association": pairs,
            "last_detections": inner.last_detections,
            "effects_sent": inner.effects_sent,
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
    if body.password.is_some() && !hostile {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "a password goes with a hostile declaration (a step-up) only");
    }
    let step = match api.auth.step_up(&op, &format!("declare {id} hostile"), body.password, needed).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    if !api.tracks.has(&id) {
        return error(StatusCode::NOT_FOUND, format!("unknown track {id}"));
    }
    // logged first: a declaration the log refuses (no step-up where one is needed) changes
    // nothing
    if let Err(e) = api.decision.record_declaration(&op, step, &id, body.affiliation.as_str(), body.note.as_deref()) {
        return error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), e.detail);
    }
    let now = clock::wall();
    let Some(track) = api.tracks.declare(
        &id,
        body.affiliation,
        op.operator().as_str(),
        body.note.clone(),
        now,
    ) else {
        return error(StatusCode::NOT_FOUND, format!("unknown track {id}"));
    };
    Json(track).into_response()
}

/// The simulation channel in. Truth: stored for the sensing model, never served back
/// except by `sim_truth` with `MURMUR_EVAL_TRUTH=1`.
async fn sim_contacts(
    State(api): State<Api>,
    body: Result<Json<TruthMessage>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(msg) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    match api.tracks.on_truth(msg, &api.world) {
        Ok((contacts, detections, tracks)) => Json(json!({
            "ok": true,
            "contacts": contacts,
            "detections": detections,
            "tracks": tracks,
            // the simulation channel back: abstract effects for the truth side to apply
            "effects": api.tracks.take_effects(),
        }))
        .into_response(),
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
