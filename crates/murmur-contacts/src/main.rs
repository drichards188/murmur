//! murmur-contacts [sim/scenarios/<name>.toml]: move the scenario's contacts and publish
//! their truth to the bridge's simulation channel and to the evaluation log.
//!
//! Environment:
//!   MURMUR_BRIDGE_URL        the bridge (default http://127.0.0.1:$MURMUR_HTTP_PORT or 8765)
//!   MURMUR_AT, MURMUR_RATE   scenario clock, as the stand-ins (default: now, x1)
//!   MURMUR_TIDEWATER_URL     tidewater serve, so drifting contacts follow the real current
//!   MURMUR_CONTACTS_TRUTH    evaluation log path (default $MURMUR_ROOT/logs/contacts-truth.jsonl)
//!   MURMUR_CONTACTS_HZ       ticks per wall second (default 1)
//!   MURMUR_CONTACTS_SEED     overrides the scenario's seed
//!   MURMUR_SIM_TOKEN         the stack's simulation-channel token (up.sh and the evaluation's
//!                            runner generate one per stack): the bridge refuses truth without it
//!   MURMUR_DAMAGE_FILE       where a strike's damage to our vehicles goes for the stand-ins
//!                            (default $MURMUR_ROOT/sim/run/damage-<MURMUR_HTTP_PORT>.jsonl;
//!                            murmur_standins::damage), emptied at the start
//!   MURMUR_CONTACTS_PORT     this process's own API on 127.0.0.1 (orders to the nodes, what
//!                            they do and perceive; murmur_contacts::api), every request with
//!                            MURMUR_SIM_TOKEN; unset, no API. A port it cannot listen on stops it
//!   MURMUR_FLEET_MANIFEST    this run's fleet (sim/run/fleet.json): the API refuses an order at
//!                            a vehicle not in it (unset: a vehicle target is not checked)
//!   MURMUR_SENSE_MODEL       the bridge's sensing model when set (else the scenario's): under
//!                            `legacy` the API refuses an attack, as a file's orders are

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use murmur_contacts::api::{self, Desk, Here};
use murmur_contacts::effect::{self, DamageCtx, EffectApplied, EffectNotice, OursDamaged};
use murmur_contacts::mine::{MineNotice, Minefields};
use murmur_contacts::motion::Mover;
use murmur_contacts::node::{LandWater, Nodes};
use murmur_contacts::scenario::{Motion, Scenario, SensingModel};
use murmur_contacts::truth::TruthMessage;
use murmur_contacts::weapon::{self, Armory, OursAt};
use murmur_standins::conditions::{self, Slot};

/// Our vehicles from a bridge answer older than this (s) are no longer what a node perceives.
const OURS_FRESH_S: f64 = 5.0;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn main() {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("murmur-contacts: runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(run()) {
        eprintln!("murmur-contacts: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let root = env("MURMUR_ROOT").map(PathBuf::from);
    let path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        root.clone()
            .unwrap_or_default()
            .join("sim/scenarios/golden-gate.toml")
    });
    let mut scenario = Scenario::load(&path)?;
    if scenario.sensing.weather.is_none() && scenario.conditions.is_none()
        && let Some(archive) = env("MURMUR_WEATHER_ARCHIVE")
    {
        scenario.sensing.weather = Some(murmur_weather::Config { archive: Some(archive), ..Default::default() });
    }
    if let Some(w) = &mut scenario.sensing.weather {
        w.pin_archive(root.as_deref().unwrap_or_else(|| std::path::Path::new(".")))?;
    }
    let seed = env("MURMUR_CONTACTS_SEED")
        .and_then(|s| s.parse().ok())
        .unwrap_or(scenario.seed);
    let contacts = scenario.contacts()?;
    if contacts.is_empty() {
        return Err(format!("{}: no contacts", path.display()));
    }

    // scenario clock: MURMUR_AT at MURMUR_RATE, read as every process reads them (an RFC 3339
    // start, a positive rate, one other than 1 only with a start; anything else refused, as
    // the bridge and the stand-ins do: murmur-drd); else the wall clock, at 1x
    let clock = conditions::scenario_from_env()?;
    let rate = clock.rate;
    let t0 = match clock.at_unix() {
        Some(t) => t,
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64()),
    };
    let wall0 = Instant::now();

    let bridge = env("MURMUR_BRIDGE_URL").unwrap_or_else(|| {
        format!(
            "http://127.0.0.1:{}",
            env("MURMUR_HTTP_PORT").unwrap_or_else(|| "8765".into())
        )
    });
    let endpoint = format!("{}/api/sim/contacts", bridge.trim_end_matches('/'));
    // the bridge takes truth (and so acts) only from its own stack's contacts process
    let sim_token = env("MURMUR_SIM_TOKEN");
    if sim_token.is_none() {
        eprintln!("contacts: no MURMUR_SIM_TOKEN: the bridge will refuse the truth channel");
    }
    let log_path = env("MURMUR_CONTACTS_TRUTH").map(PathBuf::from).unwrap_or_else(|| {
        root.clone()
            .unwrap_or_default()
            .join("logs/contacts-truth.jsonl")
    });
    if let Some(dir) = log_path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut log = BufWriter::new(
        File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?,
    );
    // the damage model (bd murmur-lbw8.2): each draw seeded by this run's seed (MURMUR_CONTACTS_SEED,
    // the episode's in the evaluation's stacks, else the scenario's) and the strike;
    // what it does to our vehicles goes to the stand-ins through a file, started empty
    let damage_ctx = DamageCtx { seed, conditions: scenario.conditions, hit_override: None };
    let damage_path = murmur_standins::damage::path();
    if let Some(dir) = damage_path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut damage_out = File::create(&damage_path).map_err(|e| format!("{}: {e}", damage_path.display()))?;
    let hz: f64 = env("MURMUR_CONTACTS_HZ")
        .and_then(|s| s.parse().ok())
        .filter(|h: &f64| *h > 0.0 && *h <= 20.0)
        .unwrap_or(1.0);

    let mut movers: Vec<Mover> = contacts
        .into_iter()
        .enumerate()
        .map(|(i, c)| Mover::new(c, seed, i as u64))
        .collect();
    // drifting surface contacts get a conditions slot (the water at their position)
    let mut slots: Vec<Option<Arc<Slot>>> = Vec::with_capacity(movers.len());
    for m in &movers {
        if matches!(m.contact.motion, Motion::Drift { .. }) {
            let slot = Arc::new(Slot::default());
            slot.report(m.st.lat, m.st.lon, 0.0);
            slots.push(Some(slot));
        } else {
            slots.push(None);
        }
    }
    let wanted: Vec<Arc<Slot>> = slots.iter().flatten().cloned().collect();
    if let Some(cfg) = conditions::Config::from_env()
        && !wanted.is_empty()
    {
        conditions::start(cfg, wanted).await;
    }

    println!(
        "{}: {} contacts, seed {seed}, scenario clock {t0:.0} x{rate}, truth -> {} and {}",
        scenario.title,
        movers.len(),
        endpoint,
        log_path.display()
    );
    for m in &movers {
        println!(
            "  {:24} {:14} {:9} {:16} {}",
            m.contact.id,
            m.contact.kind,
            format!("{:?}", m.contact.affiliation).to_lowercase(),
            format!("{:?}", m.contact.motion).split_whitespace().next().unwrap_or(""),
            m.contact.name
        );
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let period = Duration::from_secs_f64(1.0 / hz);
    let mut every = tokio::time::interval(period);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_wall = wall0;
    let mut seq: u64 = 0;
    let mut last_error: Option<String> = None;
    // effects applied since the last message (Phase E), for the next one and the log
    let mut applied: Vec<EffectApplied> = Vec::new();
    // rules 20: effects whose munition is still in flight, applied at its impact
    let mut in_flight = effect::InFlight::default();
    // rules 9: the scenario second of the last tick (acts in between are committed now)
    let mut prev_s = f64::NEG_INFINITY;
    // opponent weapons (docs/OPPONENT-WEAPONS.md): those in flight, the vehicles they may be aimed
    // at, and where the bridge last said those were
    let mut armory = Armory::default();
    let aim_at = weapon::aim_at(&scenario);
    let mut ours: std::collections::BTreeMap<u64, OursAt> = std::collections::BTreeMap::new();
    let mut prev_t = f64::NEG_INFINITY;
    // rules 27: our minefields, laid by the decision engine and handed over by the bridge
    let mut fields = Minefields::default();
    // autonomous nodes (docs/AUTONOMOUS-NODES.md): the contacts of armed kinds and those the
    // scenario's orders name, steering on the land/water grid it names; and our vehicles as the
    // bridge's latest answer gave them (what a node may perceive), with when
    let mut nodes = Nodes::new(&scenario, &movers);
    // the process's own API (docs/AUTONOMOUS-NODES.md 2.5, decision D1): orders and what the nodes
    // perceive, answered by this loop between its ticks; the channel stays open without it
    let (api_tx, mut api_rx) = tokio::sync::mpsc::channel::<api::Request>(64);
    let api_port = match env("MURMUR_CONTACTS_PORT") {
        Some(p) => Some(p.trim().parse::<u16>().map_err(|_| format!("MURMUR_CONTACTS_PORT={p}: not a port"))?),
        None => None,
    };
    if let Some(port) = api_port {
        let addr = api::serve(port, sim_token.clone(), api_tx.clone()).await?;
        println!("api: orders and nodes on http://{addr} (POST /orders, GET /orders/{{id}}, GET /nodes; X-Murmur-Sim-Token)");
    }
    // orders can come from the file or, with the API, at any second: steer on the grid for either
    if !scenario.orders.is_empty() || api_port.is_some() {
        match &scenario.sensing.terrain {
            Some(t) => match LandWater::load(&root.clone().unwrap_or_default().join(t)) {
                Ok(g) => {
                    println!("nodes: steering keeps to the land/water grid {}", g.source);
                    nodes = nodes.with_grid(g);
                }
                Err(e) => println!("nodes: no land/water grid ({e}): steering is not checked against it"),
            },
            None => println!("nodes: the scenario names no [sensing] terrain: steering is not checked against land and water"),
        }
    }
    let fleet = env("MURMUR_FLEET_MANIFEST").and_then(|p| match api::fleet_uids(std::path::Path::new(&p)) {
        Ok(f) => Some(f),
        Err(e) => {
            println!("api: {e}: an order's vehicle target is not checked against the fleet");
            None
        }
    });
    let legacy = match env("MURMUR_SENSE_MODEL").as_deref().map(str::trim) {
        Some("legacy") => true,
        Some("physical") => false,
        _ => scenario.sensing.model == Some(SensingModel::Legacy),
    };
    let mut desk = Desk::new(&nodes, fleet, legacy);
    let mut ours_now: Vec<OursAt> = Vec::new();
    let mut ours_now_t = f64::NEG_INFINITY;
    loop {
        tokio::select! {
            _ = every.tick() => {}
            Some(req) = api_rx.recv() => {
                // an API request between ticks, on the clock now and what the nodes have now
                let s = Instant::now().duration_since(wall0).as_secs_f64() * rate;
                let fresh: &[OursAt] = if t0 + s - ours_now_t <= OURS_FRESH_S { &ours_now } else { &[] };
                let mut here = Here { scenario: &scenario, nodes: &mut nodes, movers: &movers, ours: fresh, s, t: t0 + s };
                let _ = req.reply.send(desk.answer(req.ask, &mut here));
                continue;
            }
            _ = tokio::signal::ctrl_c() => {
                let _ = log.flush();
                return Ok(());
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(last_wall).as_secs_f64() * rate;
        last_wall = now;
        let s = now.duration_since(wall0).as_secs_f64() * rate;
        for (m, slot) in movers.iter_mut().zip(&slots) {
            let current = slot
                .as_ref()
                .and_then(|s| s.latest())
                .map(|c| (c.current_east(), c.current_north()));
            m.step(s, dt, current);
            if let Some(slot) = slot {
                slot.report(m.st.lat, m.st.lon, 0.0);
            }
        }
        // rules 20: a munition whose impact has come strikes now, before this truth goes out
        for e in in_flight.due(t0 + s) {
            applied.push(land(&e, &mut movers, t0 + s, &damage_ctx, &mut damage_out));
        }
        // rules 27: every live mine against what moved this second
        let mined = fields.step(t0 + s, dt, &mut movers, &ours, &damage_ctx);
        for ev in &mined.events {
            println!(
                "mine {} {}{}{}",
                ev.mine,
                ev.what,
                ev.who.as_deref().map(|w| format!(" by {w}")).unwrap_or_default(),
                ev.damage.first().map(|d| if d.hit { format!(": {}", d.state.as_str()) } else { format!(": not set off (p {:.2})", d.p_hit) }).unwrap_or_default()
            );
        }
        hand_over(&mined.ours_damaged, t0 + s, &mut damage_out);
        // every node with an order runs it: its shots fly from here, its steering is the next step's
        let fresh: &[OursAt] = if t0 + s - ours_now_t <= OURS_FRESH_S { &ours_now } else { &[] };
        let tick = nodes.tick(s, t0 + s, &mut movers, fresh, &mut armory);
        desk.record(&tick.events);
        seq += 1;
        let mut msg = TruthMessage::build(
            t0 + s,
            s,
            seq,
            &scenario.title,
            seed,
            scenario.sensing.clone(),
            &movers,
        );
        msg.effects = std::mem::take(&mut applied);
        // rules 9: the hostile acts of this tick (truth; each once)
        msg.acts = TruthMessage::acts_between(&scenario.acts, prev_s, s, t0, &movers);
        // an armed author launches its weapon (or holds its fire)
        armory.launch(&scenario, &mut msg.acts, &movers, &ours);
        // the nodes' own shots (their flights are in the armory) and what they decided
        msg.acts.extend(tick.acts);
        msg.node_events = tick.events;
        msg.look = nodes.looks(&movers);
        msg.aim_at = aim_at.clone();
        msg.weapons = armory.in_flight(prev_t, msg.t);
        msg.impacts = armory.impacts(msg.t);
        msg.impacts.extend(tick.impacts);
        // rules 27: ours struck by a mine report it as any strike; the rest is the record
        msg.impacts.extend(mined.impacts);
        msg.mine_events = mined.events;
        msg.mines_live = fields.live_domains(msg.t);
        // the damage model on each strike whose weapon lists figures (a node's catalog weapon, or
        // a scripted act's author's)
        let hurt = weapon::strike_by(&mut msg.impacts, &mut movers, &ours, &damage_ctx, |i| {
            armory.take(&i.weapon).or_else(|| weapon::scripted(&scenario, i))
        });
        hand_over(&hurt, msg.t, &mut damage_out);
        for e in &msg.node_events {
            println!(
                "node {} {}{}{}{}",
                e.node,
                e.what,
                e.why.as_deref().map(|w| format!(" ({w})")).unwrap_or_default(),
                e.act.as_deref().map(|a| format!(" {a}")).unwrap_or_default(),
                e.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default()
            );
        }
        for a in &msg.acts {
            println!(
                "act {} at {:.0} s: {} against {}{}{}",
                a.id,
                a.s,
                a.by,
                a.against_vehicle
                    .map(|u| format!("our vehicle {u}"))
                    .or_else(|| a.against_contact.clone())
                    .unwrap_or_default(),
                a.label.as_deref().map(|l| format!(" ({l})")).unwrap_or_default(),
                match (&a.weapon, &a.held) {
                    (Some(w), _) => msg
                        .weapons
                        .iter()
                        .find(|f| f.id == *w)
                        .map(|f| format!(": {} launched, strikes in {:.1} s", f.name, f.impact_t - f.launch_t))
                        .unwrap_or_default(),
                    (None, Some(h)) => format!(": held its fire ({h})"),
                    _ => String::new(),
                }
            );
        }
        for i in &msg.impacts {
            println!(
                "weapon {} struck {} at {:.1}{}",
                i.weapon,
                i.against_vehicle.map(|u| format!("our vehicle {u}")).or_else(|| i.against_contact.clone()).unwrap_or_default(),
                i.t - t0,
                i.damage
                    .as_ref()
                    .map(|d| if d.hit { format!(": hit, {}", d.state.as_str()) } else { format!(": missed (p {:.2})", d.p_hit) })
                    .unwrap_or_default()
            );
        }
        prev_s = s;
        prev_t = msg.t;
        if let Ok(line) = serde_json::to_string(&msg) {
            let _ = writeln!(log, "{line}");
            let _ = log.flush();
        }
        let mut req = client.post(&endpoint).json(&msg);
        if let Some(t) = &sim_token {
            req = req.header("x-murmur-sim-token", t.as_str());
        }
        let r = req.send().await;
        let outcome = match r {
            Ok(resp) if resp.status().is_success() => {
                // the simulation channel back: engagement effects to apply (Phase E)
                let answer = resp.json::<serde_json::Value>().await.ok();
                // rules 27: the mines laid since
                if let Some(list) = answer.as_ref().and_then(|a| a.get("mines")).and_then(|m| m.as_array()) {
                    for m in list {
                        match serde_json::from_value::<MineNotice>(m.clone()) {
                            Ok(m) => {
                                println!("mine {} laid ({}), arms in {:.0} s", m.id(), m.weapon.name, m.arms_t - (t0 + s));
                                fields.lay(m);
                            }
                            Err(err) => println!("mine: unreadable ({err})"),
                        }
                    }
                }
                // opponent weapons: where the vehicles they may be aimed at are now (and, for the
                // nodes' looks, those within reach of one)
                if let Some(list) = answer.as_ref().and_then(|a| a.get("ours")).and_then(|o| o.as_array()) {
                    ours_now.clear();
                    ours_now_t = t0 + s;
                    for o in list {
                        if let Ok(o) = serde_json::from_value::<OursAt>(o.clone()) {
                            ours.insert(o.uid, o.clone());
                            ours_now.push(o);
                        }
                    }
                }
                if let Some(list) = answer.as_ref().and_then(|a| a.get("effects")).and_then(|e| e.as_array()) {
                    for e in list {
                        match serde_json::from_value::<EffectNotice>(e.clone()) {
                            Ok(e) => {
                                if let Some(at) = e.impact_t.filter(|at| *at > t0 + s) {
                                    println!(
                                        "effect (intent {}): munition in flight, strikes in {:.1} s",
                                        e.intent,
                                        at - (t0 + s)
                                    );
                                }
                                in_flight.hold(e);
                            }
                            Err(err) => println!("effect: unreadable ({err})"),
                        }
                    }
                    // at once, unless a munition is still in flight to it (rules 20)
                    for e in in_flight.due(t0 + s) {
                        applied.push(land(&e, &mut movers, t0 + s, &damage_ctx, &mut damage_out));
                    }
                }
                Ok(())
            }
            Ok(resp) => Err(format!("bridge answered {}", resp.status())),
            Err(e) => Err(format!("bridge unreachable: {e}")),
        };
        match outcome {
            Ok(()) => {
                if last_error.take().is_some() {
                    println!("bridge: back");
                }
            }
            Err(e) => {
                if last_error.as_deref() != Some(e.as_str()) {
                    println!("{e} (truth still goes to the log)");
                    last_error = Some(e);
                }
            }
        }
    }
}

/// Our vehicles a strike hurt, to the stand-ins (murmur_standins::damage): one line each.
fn hand_over(hurt: &[OursDamaged], t: f64, out: &mut File) {
    for h in hurt {
        let Some(state) = murmur_standins::damage::State::parse(h.state.as_str()) else { continue };
        let Ok(uid) = h.uid.parse::<u64>() else { continue };
        let order = murmur_standins::damage::Order { uid, state, t, cause: h.cause.clone() };
        println!("damage: our vehicle {uid} {} ({})", h.state.as_str(), h.cause);
        if let Ok(line) = serde_json::to_string(&order) {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

/// Apply one effect at scenario time `now` and say what it did (with the damage model, when its
/// weapon lists figures).
fn land(e: &EffectNotice, movers: &mut [Mover], now: f64, ctx: &DamageCtx, out: &mut File) -> EffectApplied {
    let r = effect::apply_with(e, movers, now, Some(ctx));
    for d in &r.damage {
        println!(
            "effect (intent {}): {} {} {}: {}{}",
            r.intent,
            d.role,
            d.who,
            d.kind,
            if d.hit { format!("hit (p {:.2}), {}", d.p_hit, d.state.as_str()) } else { format!("not hit (p {:.2})", d.p_hit) },
            d.note.as_deref().map(|n| format!("; {n}")).unwrap_or_default()
        );
    }
    hand_over(&r.ours_damaged, now, out);
    println!(
        "effect (intent {}): {} contact(s) inside {:.0} m, intended {} {}, collateral {} contact(s) and {} of ours",
        r.intent,
        r.inside.len(),
        r.radius_m,
        r.target_contact.as_deref().unwrap_or("unknown"),
        if r.target_inside { "inside" } else { "not inside" },
        r.collateral.contacts.len(),
        r.collateral.ours.len()
    );
    r
}
