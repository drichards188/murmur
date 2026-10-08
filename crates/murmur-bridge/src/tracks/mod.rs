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
//! **Effects** (Phase E): an engagement's abstract effect (a point, a radius, a time) goes
//! back to the contacts process on the same labelled channel, in the answer to its next
//! `POST /api/sim/contacts` (`effects`), with the contact the target track was associated
//! with (the private association, never serialized elsewhere) and where our vehicles
//! reported themselves, for the evaluation truth log's collateral record. The contacts
//! process marks the contacts inside the radius; nothing here models damage.
//!
//! Operators may declare a track's affiliation (`POST /api/tracks/{id}/declare`, the
//! signed-in operator as for intents; a dev run's header). From rules 9 every declaration is
//! the decision engine's identification ledger (an operator's, or the engine's hostile-act
//! rule's on an act these sensors observed: `sense_acts`); this store only mirrors it for
//! display (`mirror`), and the decision facts carry the estimate alone. The tracker never
//! derives hostile.

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
use murmur_decide::{Affiliation, AisIdentity, LonLat, Time, TrackFacts};
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
                acts: Vec::new(),
                acts_seen: std::collections::BTreeSet::new(),
                act_records: Vec::new(),
                dropped: BTreeMap::new(),
                lost: Vec::new(),
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
        let mut detections = sensing::detect(params, observers, &msg.contacts, &|contact, sensor| {
            misclassify_draw(seed, contact, sensor, rate)
        });
        // each fix dated by the clock of the truth it was drawn from: the filter's intervals
        // are the contact's own, whatever the bridge's wall clock does meanwhile (on this
        // host it steps back about a second every half minute)
        for d in &mut detections {
            d.measured_t = Some(msg.t);
        }
        let n_contacts = msg.contacts.len();
        let n_tracks = {
            let mut inner = lock(&self.inner);
            self.fold(&mut inner, &detections, now);
            // rules 9: the hostile acts of this tick, as our sensors observe them (after the
            // fold: an act by a contact first seen this tick is attributed to its new track)
            self.sense_acts(&mut inner, params, observers, &msg, now);
            inner.truth = Some((now, msg));
            inner.last_detections = detections.len();
            self.drop_old(&mut inner, now);
            inner.tracks.len()
        };
        self.publish(now);
        Ok((n_contacts, detections.len(), n_tracks))
    }

    /// Rules 9: each new act in a truth message, observed by our sensors or not
    /// (`sensing::observe_act`). An observed one becomes a report for the decision engine
    /// (`A-<n>`, at the bridge's clock now): whom it was against (our vehicle, the track its
    /// victim is associated with, or something we hold no track of), which sensors saw it,
    /// where its author appeared to be and that position's error. Every act, observed or not,
    /// is kept for the evaluation view with the truth behind it.
    fn sense_acts(&self, inner: &mut Inner, params: Params, observers: &[Observer], msg: &TruthMessage, now: f64) {
        for a in &msg.acts {
            if !inner.acts_seen.insert(a.id.clone()) {
                continue;
            }
            let actor = msg.contacts.iter().find(|c| c.id == a.by);
            let actor_track = inner.by_contact.get(&a.by).cloned();
            let observed = actor.and_then(|c| {
                // the author where the act happened (the contacts process says where)
                let mut c = c.clone();
                c.lat = a.lat;
                c.lon = a.lon;
                sensing::observe_act(params, observers, &c, &a.id)
            });
            let Some(obs) = observed else {
                inner.act_records.push(json!({
                    "act": a.id, "s": a.s, "by": a.by, "by_track": actor_track,
                    "against_vehicle": a.against_vehicle, "against_contact": a.against_contact,
                    "observed": false,
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
                "sigma_m": obs.sigma_m, "observed_t": report.observed_t.0,
            }));
            inner.acts.push(report);
        }
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
        json!({
            "type": "tracks",
            "t": round(now, 2),
            "stale_after_s": model::STALE_S,
            "tracks": self.tracks_json(now),
            "lost": self.lost_json(),
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
            // rules 9: every hostile act, observed (and the report it became) or not
            "acts": inner.act_records,
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
