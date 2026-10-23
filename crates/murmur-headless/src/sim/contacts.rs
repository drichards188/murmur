//! The contacts process (crates/murmur-contacts `main.rs`) on the paused clock: the same
//! movers and steps, the same truth message to the bridge's simulation channel
//! (`POST /api/sim/contacts` with the stack's token, through the bridge's own router), the
//! effects from its answer applied the same way, at the same 1 Hz from the process's own start.
//! Its scenario clock starts at `MURMUR_AT` at its own start, as every process's does. Drifting
//! contacts get no current (no tidewater).
//!
//! With the episode's `effects` ([`EffectSim`], docs/HEADLESS.md, engagements) an effect is a
//! draw: it takes the intended contact out (applied as the contacts process applies it) with
//! probability `p_hit`, else it touches nothing; and a platform spent by its own effect leaves
//! the world at it. Every effect, its draw and what it did go to [`Contacts::effects`]
//! (evaluation truth only, never reachable from the bridge's API or the MCP tools).

use std::collections::BTreeMap;
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

use super::fleet::Fleet;
use super::spec::Effects;

/// An engagement's effect in the simulation (`Spec::effects`): the draw, and the platforms
/// spent by their own effect (their uid as the bridge names it, and their stand-in).
pub struct EffectSim {
    pub cfg: Effects,
    /// The episode seed: each effect's draw is a hash of it and the intent.
    pub seed: u64,
    /// The fleet's platforms whose catalog says `expended_by_effect`: uid -> stand-in index.
    pub expended: BTreeMap<String, usize>,
    pub fleet: Arc<Fleet>,
}

/// A uniform in [0, 1) from (seed, intent): splitmix64 over the two, so an effect's draw
/// depends on nothing but the episode and the engagement.
pub fn effect_draw(seed: u64, intent: u64) -> f64 {
    let mut z = seed ^ intent.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x00EF_FEC7_0000_0000;
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// A missed effect: nothing inside the radius is touched; what was inside is recorded.
fn missed(e: &EffectNotice, movers: &[Mover], now: f64) -> EffectApplied {
    let [lon, lat] = e.point;
    let inside: Vec<(String, f64)> = movers
        .iter()
        .filter(|m| m.st.exists)
        .filter_map(|m| {
            let d = murmur_standins::geo::distance_m(lat, lon, m.st.lat, m.st.lon);
            (d <= e.radius_m).then(|| (m.contact.id.clone(), (d * 10.0).round() / 10.0))
        })
        .collect();
    let target_inside = e
        .target_contact
        .as_ref()
        .is_some_and(|t| inside.iter().any(|(id, _)| id == t));
    EffectApplied {
        intent: e.intent,
        track: e.track.clone(),
        t: e.t,
        applied_t: now,
        point: e.point,
        radius_m: e.radius_m,
        target_contact: e.target_contact.clone(),
        platform: e.platform.clone(),
        target_inside,
        inside,
        out_of_play: Vec::new(),
        collateral: Default::default(),
    }
}

/// Apply one effect: as the contacts process does without `fx`; with it, the draw decides
/// whether it hits, and a platform spent by its own effect leaves the world. Returns what was
/// applied and, with `fx`, the record of the draw.
fn apply_effect(e: &EffectNotice, movers: &mut [Mover], now: f64, fx: Option<&EffectSim>) -> (EffectApplied, Option<Value>) {
    let Some(fx) = fx else {
        return (effect::apply(e, movers, now), None);
    };
    let u = effect_draw(fx.seed, e.intent);
    let hit = u < fx.cfg.p_hit;
    let applied = if hit { effect::apply(e, movers, now) } else { missed(e, movers, now) };
    let spent = e.platform.as_ref().and_then(|p| fx.expended.get(p).map(|i| (p.clone(), *i)));
    let removed = match &spent {
        Some((_, i)) if fx.cfg.spend_expended && (hit || !fx.cfg.survives_miss) => {
            fx.fleet.remove(*i);
            true
        }
        _ => false,
    };
    let record = json!({
        "intent": e.intent,
        "track": e.track,
        "target_contact": e.target_contact,
        "platform": e.platform,
        "p_hit": fx.cfg.p_hit,
        "u": u,
        "hit": hit,
        "expended_by_effect": spent.is_some(),
        "platform_removed": removed,
        "applied": serde_json::to_value(&applied).unwrap_or(Value::Null),
    });
    (applied, Some(record))
}

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
    /// Every engagement effect applied with the episode's `effects` (its draw and what it did).
    effects: Mutex<Vec<Value>>,
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
            effects: Mutex::new(Vec::new()),
        })
    }

    /// Every effect applied with the episode's `effects`, in order (evaluation only).
    pub fn effects(&self) -> Vec<Value> {
        self.effects.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
pub async fn run(
    c: Arc<Contacts>,
    api: axum::Router,
    token: Option<String>,
    start: Instant,
    bridge_up: Instant,
    fx: Option<EffectSim>,
) {
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
            for m in movers.iter_mut() {
                m.step(s, dt, None);
            }
            seq += 1;
            let mut msg = TruthMessage::build(
                c.t0_unix + s,
                s,
                seq,
                &c.scenario.title,
                c.seed,
                c.scenario.sensing.clone(),
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
                    let (a, record) = apply_effect(&e, &mut movers, c.t0_unix + s, fx.as_ref());
                    applied.push(a);
                    if let Some(mut r) = record {
                        // the contacts' scenario second it was applied at
                        r["s"] = json!(s);
                        c.effects.lock().unwrap_or_else(|e| e.into_inner()).push(r);
                    }
                }
            }
        }
    }
}
