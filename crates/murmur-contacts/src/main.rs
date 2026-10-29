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

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use murmur_contacts::effect::{self, EffectApplied, EffectNotice};
use murmur_contacts::motion::Mover;
use murmur_contacts::scenario::{Motion, Scenario};
use murmur_contacts::truth::TruthMessage;
use murmur_standins::conditions::{self, Slot};

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
    loop {
        tokio::select! {
            _ = every.tick() => {}
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
            applied.push(land(&e, &mut movers, t0 + s));
        }
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
        for a in &msg.acts {
            println!(
                "act {} at {:.0} s: {} against {}{}",
                a.id,
                a.s,
                a.by,
                a.against_vehicle
                    .map(|u| format!("our vehicle {u}"))
                    .or_else(|| a.against_contact.clone())
                    .unwrap_or_default(),
                a.label.as_deref().map(|l| format!(" ({l})")).unwrap_or_default()
            );
        }
        prev_s = s;
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
                if let Ok(answer) = resp.json::<serde_json::Value>().await
                    && let Some(list) = answer.get("effects").and_then(|e| e.as_array())
                {
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
                        applied.push(land(&e, &mut movers, t0 + s));
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

/// Apply one effect at scenario time `now` and say what it did.
fn land(e: &EffectNotice, movers: &mut [Mover], now: f64) -> EffectApplied {
    let r = effect::apply(e, movers, now);
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
