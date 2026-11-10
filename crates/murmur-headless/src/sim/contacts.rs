//! The contacts process (crates/murmur-contacts `main.rs`) on the paused clock: the same
//! movers and steps, the same truth message to the bridge's simulation channel
//! (`POST /api/sim/contacts` with the stack's token, through the bridge's own router), the
//! effects from its answer applied the same way, at the same 1 Hz from the process's own start.
//! Every second after the movers stepped is the process's own code (`murmur_contacts::step`,
//! which `main.rs` runs too): the munitions due, the mines, the nodes' orders and fire control,
//! the scripted acts and an armed author's launch, the weapons in flight and their strikes drawn
//! by the damage model; and the answer read back the same way, so a node's `look` gets the
//! bridge's own `ours` (docs/AUTONOMOUS-NODES.md stage 5, bd murmur-y85j.5, murmur-mmxk). Our
//! vehicles that a node's or a scripted weapon's strike hurts take it in the fleet, as a mine's.
//! A timeline's `red` step goes to the intake the process's own API uses
//! (`murmur_contacts::api::Desk`, [`Contacts::order`]), between ticks as there.
//! Its scenario clock starts at `MURMUR_AT` at its own start, as every process's does. Each
//! contact steps with the water's current at its position (world v3: the world's baked field or
//! a `current` fault, `water.rs`; only a drifting contact moves with it), as the process does
//! with tidewater; still water gives none, as before world v3.
//!
//! With the episode's `effects` ([`EffectSim`], docs/HEADLESS.md, engagements) an effect is a
//! draw: it takes the intended contact out (applied as the contacts process applies it) with
//! probability `p_hit`, else it touches nothing; and a platform spent by its own effect leaves
//! the world at it. Every effect, its draw and what it did go to [`Contacts::effects`]
//! (evaluation truth only, never reachable from the bridge's API or the MCP tools). Rules 20: an
//! effect a munition carries is held until its impact (`effect::InFlight`, as the process holds
//! it) and drawn and applied then.
//!
//! The damage model (bd murmur-lbw8.2): an effect whose weapon lists hit and damage figures is
//! drawn as the contacts process draws it (`effect::apply_with`, on the episode seed and the
//! intent: the evaluation's full stack gives its contacts process the episode seed,
//! `MURMUR_CONTACTS_SEED`, so the same outcomes there and here), unless the episode's `effects` give a `p_hit`: then
//! that is the target's hit chance (an evaluation's override, recorded), the damage given a hit
//! and the collateral still the model's. Our vehicles it hurt take their damage in the fleet
//! ([`Ours`]): a lost propulsion steps unpowered, a destroyed one leaves the world.
//!
//! Rules 27 (bd murmur-lbw8.4): the mines the bridge hands over (`mines` in its answer) are held
//! and stepped as the contacts process holds them (`murmur_contacts::mine`), against the contacts
//! and against ours where the bridge says they are (`ours`, which it answers while mines are
//! live); what they do goes to [`Contacts::effects`] (evaluation truth), ours hurt take it in the
//! fleet, and ours struck report it (`impacts`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, header};
use murmur_contacts::api::{Desk, Here};
use murmur_contacts::effect::{self, DamageCtx, EffectApplied, EffectNotice, OursDamaged};
use murmur_contacts::motion::Mover;
use murmur_contacts::node::Nodes;
use murmur_contacts::scenario::{Scenario, SensingModel};
use murmur_contacts::step::{self, Step};
use murmur_contacts::truth::TruthMessage;
use serde_json::{Value, json};
use tokio::time::{Instant, MissedTickBehavior};
use tower::ServiceExt;

use super::fleet::Fleet;
use super::spec::Effects;
use super::water::Water;

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

/// Our vehicles, for the damage model's damage to them: the fleet and each uid's stand-in; and the
/// episode seed, which the model's draws read.
pub struct Ours {
    pub fleet: Arc<Fleet>,
    pub by_uid: BTreeMap<String, usize>,
    pub seed: u64,
}

impl Ours {
    /// Put each hurt vehicle's damage on its stand-in (destroyed: it leaves the world).
    fn take(&self, hurt: &[OursDamaged]) {
        for h in hurt {
            let Some(&i) = self.by_uid.get(&h.uid) else { continue };
            let Some(state) = murmur_standins::damage::State::parse(h.state.as_str()) else { continue };
            if state == murmur_standins::damage::State::Destroyed {
                self.fleet.remove(i);
            }
            self.fleet.write(i, |v| v.take_damage(state));
        }
    }
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
        impact_t: e.impact_t,
        weapon: None,
        damage: Vec::new(),
        ours_damaged: Vec::new(),
        burst: e.burst,
    }
}

/// Apply one effect: as the contacts process does without `fx` (the damage model's draw when its
/// weapon lists figures); with it, an abstract effect's hit is the episode's draw, and a platform
/// spent by its own effect leaves the world. Returns what was applied and, with `fx`, the record
/// of the draw.
fn apply_effect(e: &EffectNotice, movers: &mut [Mover], now: f64, fx: Option<&EffectSim>, ctx: &DamageCtx) -> (EffectApplied, Option<Value>) {
    let Some(fx) = fx else {
        return (effect::apply_with(e, movers, now, Some(ctx)), None);
    };
    let modelled = e.weapon.as_ref().is_some_and(|w| w.modelled());
    let (u, hit, applied) = if modelled {
        let ctx = DamageCtx { hit_override: fx.cfg.p_hit, ..*ctx };
        let a = effect::apply_with(e, movers, now, Some(&ctx));
        let target = a.damage.iter().find(|d| d.role == "target");
        (target.map_or(1.0, |d| d.u_hit), target.is_some_and(|d| d.hit), a)
    } else {
        let u = effect_draw(fx.seed, e.intent);
        let hit = u < fx.cfg.p_hit.unwrap_or(1.0);
        (u, hit, if hit { effect::apply(e, movers, now) } else { missed(e, movers, now) })
    };
    let spent = e.platform.as_ref().and_then(|p| fx.expended.get(p).map(|i| (p.clone(), *i)));
    let removed = match &spent {
        Some((_, i)) if fx.cfg.spend_expended && (hit || !fx.cfg.survives_miss) => {
            fx.fleet.remove(*i);
            true
        }
        _ => false,
    };
    let mut record = json!({
        "intent": e.intent,
        "track": e.track,
        "target_contact": e.target_contact,
        "platform": e.platform,
        "p_hit": if modelled { json!(fx.cfg.p_hit) } else { json!(fx.cfg.p_hit.unwrap_or(1.0)) },
        "u": u,
        "hit": hit,
        "expended_by_effect": spent.is_some(),
        "platform_removed": removed,
        "applied": serde_json::to_value(&applied).unwrap_or(Value::Null),
    });
    // the damage model drew it (an abstract effect's record is as before)
    if modelled {
        record["model"] = json!("damage");
    }
    (applied, Some(record))
}

pub struct Contacts {
    pub scenario: Scenario,
    pub seed: u64,
    /// MURMUR_AT: the scenario clock at the process's start.
    pub t0_unix: f64,
    pub movers: Mutex<Vec<Mover>>,
    /// The nodes and the orders still to come (docs/AUTONOMOUS-NODES.md).
    nodes: Mutex<Nodes>,
    /// The truth side from one second to the next (`murmur_contacts::step`, as live).
    step: Mutex<Step>,
    /// The intake of orders between ticks and their record, as the process's own API keeps
    /// them (`murmur_contacts::api::Desk`): a timeline's `red` step comes here ([`Contacts::order`]).
    desk: Mutex<Desk>,
    /// When the loop started (the contacts' scenario second 0); none before.
    started: Mutex<Option<Instant>>,
    /// The last message's scenario second (None before the first).
    pub s: Mutex<Option<f64>>,
    /// Every step's truth (`log_step`).
    log: Mutex<Vec<Value>>,
    /// Every engagement effect applied with the episode's `effects` (its draw and what it did).
    effects: Mutex<Vec<Value>>,
}

impl Contacts {
    /// The contacts of the scenario at `path` (its seed, else the scenario's), their clock from
    /// `t0_unix`; `fleet`: the run's vehicle uids, against which an order's vehicle target is
    /// checked (as the process checks its fleet manifest).
    pub fn load(
        path: &std::path::Path,
        seed: Option<u64>,
        t0_unix: f64,
        root: &std::path::Path,
        fleet: Option<BTreeSet<u64>>,
    ) -> Result<Contacts, String> {
        let mut scenario = Scenario::load(path)?;
        if let Some(w) = &mut scenario.sensing.weather { w.pin_archive(root)?; }
        let seed = seed.unwrap_or(scenario.seed);
        // how the contacts move, as the process reads it (bd murmur-2of4): the v1 and v2 worlds
        // pin the legacy motion by MURMUR_MOTION_MODEL (eval/headless/situations.py)
        scenario.pin_motion(std::env::var("MURMUR_MOTION_MODEL").ok().as_deref())?;
        let contacts = scenario.contacts()?;
        if contacts.is_empty() {
            return Err(format!("{}: no contacts", path.display()));
        }
        let movers: Vec<Mover> = contacts
            .into_iter()
            .enumerate()
            .map(|(i, c)| Mover::new(c, seed, i as u64))
            .collect();
        // the nodes and their orders, on the land/water grid the scenario names, as the contacts
        // process has them with its API on (`step::nodes`): a timeline's order can come at any
        // second
        let (nodes, _) = step::nodes(&scenario, &movers, root, true);
        // the bridge's sensing model, as the process reads it: under legacy an attack is refused
        let legacy = match std::env::var("MURMUR_SENSE_MODEL").ok().as_deref().map(str::trim) {
            Some("legacy") => true,
            Some("physical") => false,
            _ => scenario.sensing.model == Some(SensingModel::Legacy),
        };
        let desk = Desk::new(&nodes, fleet, legacy);
        let step = Step::new(&scenario, seed, t0_unix);
        Ok(Contacts {
            scenario,
            seed,
            t0_unix,
            movers: Mutex::new(movers),
            nodes: Mutex::new(nodes),
            step: Mutex::new(step),
            desk: Mutex::new(desk),
            started: Mutex::new(None),
            s: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            effects: Mutex::new(Vec::new()),
        })
    }

    /// The nodes (the contacts of armed kinds and those an order names) and the orders to come.
    pub fn nodes(&self) -> MutexGuard<'_, Nodes> {
        self.nodes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One order to the contacts (a timeline's `red` step, docs/AUTONOMOUS-NODES.md 2.5) at the
    /// instant `now`, through the intake the process's own API puts `POST /orders` through
    /// ([`Desk::order_json`]), between ticks as there: `{status, answer}` (`{id, accepted}`, or
    /// `{refused, code}`), as the live runner records it; `{error}` before the contacts started
    /// (live: no process answers yet).
    pub fn order(&self, body: Value, now: Instant) -> Value {
        let Some(start) = *self.started.lock().unwrap_or_else(|e| e.into_inner()) else {
            return json!({ "error": "the contacts have not started: no answer" });
        };
        let s = now.saturating_duration_since(start).as_secs_f64();
        let t = self.t0_unix + s;
        let movers = self.movers();
        let mut nodes = self.nodes();
        let step = self.step.lock().unwrap_or_else(|e| e.into_inner());
        let mut here = Here { scenario: &self.scenario, nodes: &mut nodes, movers: &movers, ours: step.perceived(t), s, t };
        let reply = self.desk.lock().unwrap_or_else(|e| e.into_inner()).order_json(body, &mut here);
        json!({ "status": reply.status, "answer": reply.body })
    }

    /// Every effect applied with the episode's `effects`, in order (evaluation only).
    pub fn effects(&self) -> Vec<Value> {
        self.effects.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn movers(&self) -> MutexGuard<'_, Vec<Mover>> {
        self.movers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The truth log, as the contacts process writes `contacts-truth.jsonl`: one line per step,
    /// its scenario second `s` and every contact `[id, lon, lat, speed, exists]`, and, each only
    /// when the second has any, its message's acts, the weapons in flight, the strikes (with the
    /// damage model's draws) and what the nodes decided (evaluation only).
    fn log_step(&self, s: f64, msg: &TruthMessage) {
        let mut line = {
            let movers = self.movers();
            json!({
                "s": s,
                "contacts": movers.iter().map(|m| json!([
                    m.contact.id, m.st.lon, m.st.lat, m.st.speed_mps, m.st.exists,
                ])).collect::<Vec<_>>(),
            })
        };
        let parts = [
            ("acts", serde_json::to_value(&msg.acts)),
            ("weapons", serde_json::to_value(&msg.weapons)),
            ("impacts", serde_json::to_value(&msg.impacts)),
            ("node_events", serde_json::to_value(&msg.node_events)),
        ];
        for (key, v) in parts {
            if let Ok(v) = v
                && v.as_array().is_some_and(|a| !a.is_empty())
            {
                line[key] = v;
            }
        }
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

/// The process's loop from `start`: a message every second, the first at once, each second
/// stepped by the contacts process's own [`Step`] (the same truth as live). A message before
/// `bridge_up` is lost (no bridge answers yet: the live process logs the error and goes on).
/// `water`: the episode's water, its zero and epoch (its scenario time is the situation's).
#[allow(clippy::too_many_arguments)]
pub async fn run(
    c: Arc<Contacts>,
    api: axum::Router,
    token: Option<String>,
    start: Instant,
    bridge_up: Instant,
    fx: Option<EffectSim>,
    water: (Arc<Water>, Instant, f64),
    ours: Ours,
) {
    let (water, zero, epoch) = water;
    let ctx = DamageCtx { seed: ours.seed, conditions: c.scenario.conditions, hit_override: None };
    let period = Duration::from_secs(1);
    tokio::time::sleep_until(start).await;
    let mut every = tokio::time::interval(period);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last = start;
    *c.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(start);
    loop {
        every.tick().await;
        let now = Instant::now();
        let dt = now.saturating_duration_since(last).as_secs_f64();
        last = now;
        let s = now.saturating_duration_since(start).as_secs_f64();
        let msg = {
            let mut movers = c.movers();
            let mut nodes = c.nodes();
            // effects, munitions in flight, opponent weapons, our mines, where the bridge last said
            // ours were: the truth side from one second to the next, as live
            let mut step = c.step.lock().unwrap_or_else(|e| e.into_inner());
            let t = epoch + now.saturating_duration_since(zero).as_secs_f64();
            for m in movers.iter_mut() {
                let current = water.current(m.st.lat, m.st.lon, t);
                m.step(s, dt, current);
            }
            let second = step.tick(&c.scenario, s, dt, &mut movers, &mut nodes, &ctx, &mut |e, movers, s| {
                land(&c, e, movers, s, fx.as_ref(), &ctx, &ours)
            });
            // what the nodes said this second, for the record of every order (as live)
            c.desk.lock().unwrap_or_else(|e| e.into_inner()).record(&second.msg.node_events);
            // ours hurt by a mine, then by a strike, take it in the fleet
            ours.take(&second.mined);
            ours.take(&second.struck);
            // rules 27: what the mines did, with the effects (evaluation truth)
            if !second.msg.mine_events.is_empty() {
                let mut fx = c.effects.lock().unwrap_or_else(|e| e.into_inner());
                for ev in &second.msg.mine_events {
                    fx.push(json!({ "s": s, "mine": ev }));
                }
            }
            second.msg
        };
        *c.s.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
        c.log_step(s, &msg);
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
        // the simulation channel back, read as live: the mines laid since, where ours are (the
        // bridge's own answer to the message's `aim_at`, `mines_live` and the nodes' `look`), the
        // effects to apply
        if let Ok(answer) = serde_json::from_slice::<Value>(&bytes) {
            let mut movers = c.movers();
            let mut step = c.step.lock().unwrap_or_else(|e| e.into_inner());
            step.answer(
                &answer,
                s,
                &mut movers,
                &mut |e, movers, s| land(&c, e, movers, s, fx.as_ref(), &ctx, &ours),
                &mut |_| {},
            );
        }
    }
}

/// Apply one effect at the contacts' scenario second `s` (its draw with `fx`), recording it; our
/// vehicles it hurt take their damage.
#[allow(clippy::too_many_arguments)]
fn land(c: &Contacts, e: &EffectNotice, movers: &mut [Mover], s: f64, fx: Option<&EffectSim>, ctx: &DamageCtx, ours: &Ours) -> EffectApplied {
    let (a, record) = apply_effect(e, movers, c.t0_unix + s, fx, ctx);
    ours.take(&a.ours_damaged);
    if let Some(mut r) = record {
        // the contacts' scenario second it was applied at
        r["s"] = json!(s);
        c.effects.lock().unwrap_or_else(|e| e.into_inner()).push(r);
    }
    a
}
