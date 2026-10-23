//! The contacts process (crates/murmur-contacts `main.rs`) on the paused clock: the same
//! movers and steps, the same truth message to the bridge's simulation channel
//! (`POST /api/sim/contacts` with the stack's token, through the bridge's own router), the
//! effects from its answer applied the same way, at the same 1 Hz from the process's own start.
//! Its scenario clock starts at `MURMUR_AT` at its own start, as every process's does. Each
//! contact steps with the water's current at its position (world v3: the world's baked field or
//! a `current` fault, `water.rs`; only a drifting contact moves with it), as the process does
//! with tidewater; still water gives none, as before world v3.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, header};
use murmur_contacts::effect::{self, EffectApplied, EffectNotice};
use murmur_contacts::motion::Mover;
use murmur_contacts::scenario::Scenario;
use murmur_contacts::truth::TruthMessage;
use serde_json::{Value, json};
use tokio::time::{Instant, MissedTickBehavior};
use tower::ServiceExt;

use super::water::Water;

pub struct Contacts {
    pub scenario: Scenario,
    pub seed: u64,
    /// MURMUR_AT: the scenario clock at the process's start.
    pub t0_unix: f64,
    pub movers: Mutex<Vec<Mover>>,
    /// The last message's scenario second (None before the first).
    pub s: Mutex<Option<f64>>,
    /// Every step's truth (`log_step`).
    log: Mutex<Vec<Value>>,
}

impl Contacts {
    pub fn load(path: &std::path::Path, seed: Option<u64>, t0_unix: f64) -> Result<Contacts, String> {
        let scenario = Scenario::load(path)?;
        let seed = seed.unwrap_or(scenario.seed);
        let contacts = scenario.contacts()?;
        if contacts.is_empty() {
            return Err(format!("{}: no contacts", path.display()));
        }
        let movers = contacts
            .into_iter()
            .enumerate()
            .map(|(i, c)| Mover::new(c, seed, i as u64))
            .collect();
        Ok(Contacts {
            scenario,
            seed,
            t0_unix,
            movers: Mutex::new(movers),
            s: Mutex::new(None),
            log: Mutex::new(Vec::new()),
        })
    }

    pub fn movers(&self) -> MutexGuard<'_, Vec<Mover>> {
        self.movers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The truth log, as the contacts process writes `contacts-truth.jsonl`: one line per step,
    /// its scenario second `s` and every contact `[id, lon, lat, speed, exists]` (evaluation only).
    fn log_step(&self, s: f64) {
        let line = {
            let movers = self.movers();
            json!({
                "s": s,
                "contacts": movers.iter().map(|m| json!([
                    m.contact.id, m.st.lon, m.st.lat, m.st.speed_mps, m.st.exists,
                ])).collect::<Vec<_>>(),
            })
        };
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(line);
    }

    /// The truth log's lines from index `from` on.
    pub fn log_from(&self, from: usize) -> Vec<Value> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).iter().skip(from).cloned().collect()
    }

    /// Truth now: every contact as the movers have it (evaluation only: never reachable from
    /// the bridge's API or the MCP tools).
    pub fn truth(&self) -> Value {
        let movers = self.movers();
        json!({
            "s": *self.s.lock().unwrap_or_else(|e| e.into_inner()),
            "seed": self.seed,
            "scenario": self.scenario.title,
            "contacts": movers.iter().map(|m| json!({
                "id": m.contact.id,
                "name": m.contact.name,
                "affiliation": m.contact.affiliation,
                "domain": m.contact.domain,
                "kind": m.contact.kind,
                "ais": m.contact.ais.is_some(),
                "exists": m.st.exists,
                "lat": m.st.lat,
                "lon": m.st.lon,
                "alt_m": m.st.alt_m,
                "speed_mps": m.st.speed_mps,
                "course_deg": m.st.course_deg,
                "start_s": m.contact.start_s,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The process's loop from `start`: a message every second, the first at once. A message before
/// `bridge_up` is lost (no bridge answers yet: the live process logs the error and goes on).
/// `water`: the episode's water, its zero and epoch (its scenario time is the situation's).
pub async fn run(
    c: Arc<Contacts>,
    api: axum::Router,
    token: Option<String>,
    start: Instant,
    bridge_up: Instant,
    water: (Arc<Water>, Instant, f64),
) {
    let (water, zero, epoch) = water;
    let period = Duration::from_secs(1);
    tokio::time::sleep_until(start).await;
    let mut every = tokio::time::interval(period);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last = start;
    let mut seq: u64 = 0;
    let mut applied: Vec<EffectApplied> = Vec::new();
    let mut prev_s = f64::NEG_INFINITY;
    loop {
        every.tick().await;
        let now = Instant::now();
        let dt = now.saturating_duration_since(last).as_secs_f64();
        last = now;
        let s = now.saturating_duration_since(start).as_secs_f64();
        let mut msg = {
            let mut movers = c.movers();
            let t = epoch + now.saturating_duration_since(zero).as_secs_f64();
            for m in movers.iter_mut() {
                let current = water.current(m.st.lat, m.st.lon, t);
                m.step(s, dt, current);
            }
            seq += 1;
            let mut msg = TruthMessage::build(
                c.t0_unix + s,
                s,
                seq,
                &c.scenario.title,
                c.seed,
                c.scenario.sensing,
                &movers,
            );
            msg.acts = TruthMessage::acts_between(&c.scenario.acts, prev_s, s, c.t0_unix, &movers);
            msg
        };
        *c.s.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
        c.log_step(s);
        msg.effects = std::mem::take(&mut applied);
        prev_s = s;
        if now < bridge_up {
            continue;
        }
        let Ok(body) = serde_json::to_string(&msg) else {
            continue;
        };
        let mut req = Request::builder()
            .method(Method::POST)
            .uri("/api/sim/contacts")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = &token {
            req = req.header("x-murmur-sim-token", t.as_str());
        }
        let Ok(req) = req.body(Body::from(body)) else {
            continue;
        };
        let resp = match api.clone().oneshot(req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(bytes) = axum::body::to_bytes(resp.into_body(), 16 << 20).await else {
            continue;
        };
        if let Ok(answer) = serde_json::from_slice::<Value>(&bytes)
            && let Some(list) = answer.get("effects").and_then(|e| e.as_array())
        {
            let mut movers = c.movers();
            for e in list {
                if let Ok(e) = serde_json::from_value::<EffectNotice>(e.clone()) {
                    applied.push(effect::apply(&e, &mut movers, c.t0_unix + s));
                }
            }
        }
    }
}
