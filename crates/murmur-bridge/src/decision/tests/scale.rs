//! The decision loop's tick against a picture shaped like Saturation's (`tracks::saturation`,
//! bd murmur-jvdc): a rule declaration in force (the weak-evidence rule reads the picture every
//! second), two lost contacts without AIS (as the live run had), a console watching both feeds.
//! A timing aid, not a check: `cargo test --release -p murmur-bridge tick_scale -- --ignored
//! --nocapture` (`MURMUR_SCALE` as for `picture_scale`). The fleet is the rig's one vehicle, not
//! Saturation's 3,602: what the tick does per vehicle is not timed here.

use std::time::Instant;

use super::engagement::engage_rig_auth_policy;
use super::identification::{act_now, picture, rule_on};
use super::*;
use crate::tracks::saturation;

#[tokio::test(start_paused = true)]
#[ignore = "a timing aid: run it by name with --ignored --nocapture, in release"]
async fn tick_scale() {
    let scale: f64 = std::env::var("MURMUR_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let e = engage_rig_auth_policy(None, PathBuf::from("memory"), murmur_decide::AuthMode::Dev, |p| rule_on(p, false));
    // our vehicle 1 and the boat's track; the boat fires on it and is declared hostile by rule
    picture(&e).await;
    act_now(&e, None);
    // a console watching
    let _tracks_feed = e.tracks.subscribe();
    let _decisions_feed = e.r.d.subscribe();
    let now = e.now_s();
    e.tracks.insert_tracks(saturation::picture(1001, scale, now - 100.0));
    // two drones out of view past the drop time: the tracker drops them, the engine keeps them
    // as lost contacts
    e.tracks.insert_tracks(saturation::picture(2001, 0.1, now - 400.0).into_iter().skip(7).take(2).collect());
    e.tracks.tick();
    e.r.d.tick();
    let lost = e.r.d.lock().engine.lost_contacts().len();
    let mut ms: Vec<f64> = (0..15)
        .map(|_| {
            e.r.clock.advance_ms(1000);
            let start = Instant::now();
            e.r.d.tick();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    println!(
        "decision tick, {} tracks, {lost} lost contacts: {:.1} ms median, {:.1} ms fastest",
        e.tracks.facts(e.now_s()).len(),
        ms[ms.len() / 2],
        ms[0]
    );
    // its parts, as the loops' report has them (the open minute: these ticks and the two before)
    let loops = crate::perf::json();
    for (name, v) in loops.as_object().into_iter().flatten() {
        if name.starts_with("decision") || name.starts_with("tracks_message") {
            println!("  {name:<34} {:>3} runs, mean {:>8} ms", v["open"]["runs"], v["open"]["mean_ms"]);
        }
    }
}

/// One timed run of `f` into `ms`.
fn timed<R>(ms: &mut Vec<f64>, f: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let r = f();
    ms.push(start.elapsed().as_secs_f64() * 1000.0);
    r
}

/// Mean and worst of `ms`, for a report line.
fn mean_worst(ms: &[f64]) -> String {
    if ms.is_empty() {
        return "-".into();
    }
    let worst = ms.iter().copied().fold(0.0, f64::max);
    format!("{:7.1} mean {:7.1} worst ({}x)", ms.iter().sum::<f64>() / ms.len() as f64, worst, ms.len())
}

/// Saturation's bridge as the region run had it once its rule declaration was in force (bd
/// murmur-jvdc), timed: fake autopilots of the run's platforms (3,602, `MURMUR_SCALE_FLEET`),
/// an order to each hundred sent and flown, a sixth of the fleet silent since and a tenth out of
/// its task (the inbox's alerts), Saturation's 507 tracks (`tracks::saturation`), lost contacts
/// (900, `MURMUR_SCALE_LOST`, each seen by as many of ours as a live drone: `MURMUR_SCALE_LOST_SEEN`
/// times 286 cameras), the rule declaration nobody confirmed (the look at what a person
/// alone holds up, every 2 s) and a console watching both feeds. Each second (20,
/// `MURMUR_SCALE_SECS`) it times the decision tick and the console's reads, every two the
/// blocking look. A timing aid, not a check: `cargo test --release -p murmur-bridge
/// saturation_scale -- --ignored --nocapture`.
#[tokio::test(start_paused = true)]
#[ignore = "a timing aid: run it by name with --ignored --nocapture, in release"]
async fn saturation_scale() {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    let var = |k: &str, d: u64| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let fleet = var("MURMUR_SCALE_FLEET", 3602);
    let lost = var("MURMUR_SCALE_LOST", 900);
    let secs = var("MURMUR_SCALE_SECS", 20);
    // how many of ours saw each lost contact, as a share of a live drone's (286 cameras)
    let lost_seen: f64 = std::env::var("MURMUR_SCALE_LOST_SEEN").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let built = Instant::now();
    let e = engage_rig_auth_policy(None, PathBuf::from("memory"), murmur_decide::AuthMode::Dev, |p| rule_on(p, false));
    // our vehicle 1 and the boat's track; the boat fires on it and is declared hostile by rule
    picture(&e).await;
    act_now(&e, None);
    // the boat seen again each second (its track and the declaration on it stay live)
    let second = |e: &super::engagement::Erig| {
        let now = e.now_s();
        super::identification::truth_with(e, now, now - super::T0_S + 2_000.0, Vec::new());
    };
    // the fleet, in the run's mix of platforms whatever its size
    let silent = Arc::new(AtomicBool::new(false));
    let mut groups: BTreeMap<&str, Vec<Uid>> = BTreeMap::new();
    for uid in 2..=fleet {
        let (fw, platform) = match uid * 3602 / fleet {
            0..=1700 => (Firmware::Copter, "x10"),
            1701..=3000 => (Firmware::Copter, "puma"),
            3001..=3200 => (Firmware::Rover, "saildrone"),
            3201..=3500 => (Firmware::Rover, "corsair"),
            3501..=3600 => (Firmware::Sub, "remus"),
            _ => (Firmware::Copter, "mq9b_armed"),
        };
        let mut q = quick();
        if uid % 6 == 0 {
            q.cut = Some(silent.clone());
        }
        if uid % 10 == 3 && !matches!(fw, Firmware::Sub) {
            q.bail_after = Some((15.0, "RTL", "GCS Failsafe"));
        }
        let (lat, lon) = (37.815 + ((uid / 60) % 60) as f64 * 0.00075, -122.47 + (uid % 60) as f64 * 0.0007);
        let cell = spawn_fake(uid, fw, lat, lon, q);
        let mut m = serde_json::Map::new();
        m.insert("platform".into(), json!(platform));
        m.insert("callsign".into(), json!(format!("{platform} {uid}")));
        cell.write(|v| v.identity = Some(Arc::new(Identity::from_manifest(&m))));
        e.r.world.insert_vehicle(cell);
        if platform != "mq9b_armed" {
            groups.entry(platform).or_default().push(uid);
        }
    }
    // the manifest's swarms, a hundred each (the inbox folds their alerts)
    let swarm = |uid: Uid| (uid > 1 && uid < fleet - 1).then(|| format!("SWARM-{}", (uid - 2) / 100 + 1));
    let manifest = json!({
        "vehicles": (1..=fleet).map(|uid| json!({"uid": uid, "sysid": uid % 250 + 1, "port": 20000 + uid, "swarm": swarm(uid)})).collect::<Vec<_>>(),
    });
    *e.r.world.manifest() = crate::world::Manifest::parse(&manifest).0;
    for _ in 0..3 {
        sleep(Duration::from_secs(1)).await;
        e.r.clock.advance_ms(1000);
        second(&e);
        e.r.d.tick();
    }
    // the orders: an orbit (a patrol for the Pumas) for each hundred, by an operator
    let mut sent = 0;
    for (platform, uids) in &groups {
        for chunk in uids.chunks(100) {
            let cells = e.r.world.vehicles();
            let (lat, lon) = cells.iter().find(|c| c.uid == chunk[0]).and_then(|c| c.read(|v| v.pos)).expect("pos");
            let mut req = orbit(chunk[0]);
            req.uids = chunk.to_vec();
            if *platform == "puma" {
                req.task = "patrol".into();
                req.params = TaskParams { route: Some(vec![[lon, lat], [lon + 0.01, lat], [lon + 0.01, lat + 0.005]]), ..TaskParams::default() };
            } else {
                req.params.center = Some([lon + 0.002, lat]);
            }
            match e.r.d.submit(&op1(), req).await {
                Ok(out) if out.state == State::Dispatched => sent += 1,
                Ok(out) => println!("order on {}: {:?}", chunk[0], out.state),
                Err(err) => println!("order on {} refused: {}", chunk[0], err.detail),
            }
        }
    }
    // the orders flown, a tenth bailing out of theirs, then a sixth of the fleet silent
    for s in 0..40 {
        sleep(Duration::from_secs(1)).await;
        e.r.clock.advance_ms(1000);
        second(&e);
        if s == 30 {
            silent.store(true, Ordering::SeqCst);
        }
        e.r.d.tick();
    }
    // Saturation's picture, and the tracks lost since (the tracker drops them at its tick)
    let now = e.now_s();
    e.tracks.insert_tracks(saturation::picture(1001, 1.0, now - 5.0));
    let (mut n, mut first) = (0, 2001);
    while n < lost {
        let take = (lost - n).min(500);
        e.tracks.insert_tracks(saturation::picture(first, lost_seen, now - 400.0).into_iter().skip(7).take(take as usize).collect());
        n += take;
        first += 1000;
    }
    e.tracks.tick();
    e.r.d.tick();
    // a console watching both feeds
    let _tracks_feed = e.tracks.subscribe();
    let _decisions_feed = e.r.d.subscribe();
    e.r.d.refresh_blocking().await;
    let (lost_now, blocking) = {
        let core = e.r.d.lock();
        (core.engine.lost_contacts().len(), e.r.d.blocking_now(&core).len())
    };
    println!(
        "{} vehicles, {sent} orders dispatched, {} ops, {} alerts, {} tracks, {lost_now} lost contacts, blocking {blocking}; built in {:.0} s",
        e.r.world.vehicles().len(),
        e.r.world.tasks.current_ops().len(),
        e.r.d.current_alerts().len(),
        e.tracks.facts(e.now_s()).len(),
        built.elapsed().as_secs_f64()
    );
    let (mut tick, mut look, mut message, mut snapshot, mut inbox, mut alerts, mut frame) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut cache = crate::push::FrameCache::default();
    // beside it, in real time on a thread of its own as the live bridge's truth handler and
    // tracker are: each second `MURMUR_SCALE_FOLD` detections of every live track (50; the live
    // run's were about 290) folded in under the store's lock, the feed published after it and
    // again half a second later (what the decision loop waits for on the store's lock); 0: none
    let per_track = var("MURMUR_SCALE_FOLD", 50);
    let stop = Arc::new(AtomicBool::new(false));
    let now_bits = Arc::new(std::sync::atomic::AtomicU64::new(e.now_s().to_bits()));
    let folder = (per_track > 0).then(|| {
        let (tracks, stop, now_bits) = (e.tracks.clone(), stop.clone(), now_bits.clone());
        std::thread::spawn(move || {
            let (mut fold, mut publish) = (Vec::new(), Vec::new());
            while !stop.load(Ordering::SeqCst) {
                let t = f64::from_bits(now_bits.load(Ordering::SeqCst));
                timed(&mut fold, || tracks.fold_like(1001, per_track, t));
                std::thread::sleep(Duration::from_millis(500));
                timed(&mut publish, || tracks.fold_like(1001, 0, t + 0.5));
                std::thread::sleep(Duration::from_millis(500));
            }
            (fold, publish)
        })
    });
    // the loops' open minute from here
    crate::perf::reset();
    for s in 0..secs {
        let second_start = Instant::now();
        sleep(Duration::from_secs(1)).await;
        e.r.clock.advance_ms(1000);
        now_bits.store(e.now_s().to_bits(), Ordering::SeqCst);
        second(&e);
        e.tracks.tick();
        timed(&mut tick, || e.r.d.tick());
        if s % 2 == 0 {
            let start = Instant::now();
            e.r.d.refresh_blocking().await;
            look.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        timed(&mut message, || e.r.d.message());
        timed(&mut inbox, || e.r.d.inbox_json());
        timed(&mut alerts, || e.r.d.current_alerts());
        timed(&mut snapshot, || e.r.d.snapshot());
        timed(&mut frame, || crate::push::build_frame(&e.r.world, &mut cache));
        // a second of real time a second, as the folds beside it run
        if folder.is_some() {
            std::thread::sleep(Duration::from_secs(1).saturating_sub(second_start.elapsed()));
        }
    }
    stop.store(true, Ordering::SeqCst);
    if let Some(f) = folder {
        let (fold, publish) = f.join().expect("the folds");
        println!("beside it: fold and feed {}; the tracker's feed {}", mean_worst(&fold), mean_worst(&publish));
    }
    {
        let world = e.r.d.snapshot();
        let msg = e.r.d.message();
        let sources: usize = world.tracks.values().map(|t| t.sources.len()).sum();
        println!("message {} bytes; picture {} tracks, {sources} sources", msg.len(), world.tracks.len());
        let v: Value = serde_json::from_str(&msg).expect("json");
        for k in ["inbox", "intents", "space", "operators", "spent"] {
            println!("  {k:<10} {} bytes", v[k].to_string().len());
        }
        let spec = murmur_decide::IntentSpec {
            id: murmur_decide::IntentId(0),
            created_t: world.t,
            source: murmur_decide::Source::Human { id: OperatorId::new("dry-run") },
            task: murmur_decide::Task::Engage { track: "T-0001".into(), effect_radius_m: 75.0, window: None, fallback: murmur_decide::Fallback::Return, weapon: None },
            uids: vec![murmur_decide::Uid::new("1")],
            constraints: Constraints::default(),
            rationale: None,
        };
        let mut a = Vec::new();
        let mut b = Vec::new();
        let mut c = Vec::new();
        for _ in 0..5 {
            let w = timed(&mut a, || world.subset_for(&spec));
            timed(&mut c, || drop(w));
            timed(&mut b, || e.r.d.lock().engine.dry_run_inputs(&spec, &world, &e.r.d.terrain.oracle()));
        }
        let mut veh = Vec::new();
        let mut trk = Vec::new();
        for _ in 0..5 {
            timed(&mut veh, || world.vehicles.clone());
            timed(&mut trk, || world.tracks.clone());
        }
        let cells = e.r.world.vehicles();
        let (mut orbit, mut ident, mut link, mut facts, mut tracks_ms) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let cat = e.r.d.facts_catalog().clone();
        for _ in 0..5 {
            let now = crate::clock::mono();
            timed(&mut orbit, || cells.iter().map(|c| e.r.world.tasks.orbit_of(c)).collect::<Vec<_>>());
            timed(&mut ident, || cells.iter().map(|c| engage::identity_of(c)).collect::<Vec<_>>());
            timed(&mut link, || cells.iter().map(|c| c.link_snapshot(now).expected_interval_ms).sum::<u32>());
            timed(&mut facts, || cells.iter().map(|c| c.read(|v| snapshot::vehicle_facts(v, &cat, now, 1000, None))).collect::<Vec<_>>());
            timed(&mut tracks_ms, || e.tracks.facts(e.now_s()).len());
        }
        println!("snapshot parts: orbit_of {}; identity_of {}; link {}; facts {}; tracks {}", mean_worst(&orbit), mean_worst(&ident), mean_worst(&link), mean_worst(&facts), mean_worst(&tracks_ms));
        println!("subset_for {}; its drop {}; dry_run_inputs {}", mean_worst(&a), mean_worst(&c), mean_worst(&b));
        println!("vehicles clone {}; tracks clone {}", mean_worst(&veh), mean_worst(&trk));
    }
    println!("decision tick         {}", mean_worst(&tick));
    println!("blocking look         {}", mean_worst(&look));
    println!("decisions message     {}", mean_worst(&message));
    println!("  inbox               {}", mean_worst(&inbox));
    println!("  alerts              {}", mean_worst(&alerts));
    println!("snapshot              {}", mean_worst(&snapshot));
    println!("push frame            {}", mean_worst(&frame));
    let loops = crate::perf::json();
    for (name, v) in loops.as_object().into_iter().flatten() {
        if v["open"]["runs"].as_u64().unwrap_or(0) > 0 {
            println!("  {name:<40} {:>3} runs, mean {:>8} ms, worst {:>8} ms", v["open"]["runs"], v["open"]["mean_ms"], v["open"]["worst_ms"]);
        }
    }
}
