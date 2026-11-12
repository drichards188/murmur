//! What the track store does under its lock with a picture shaped like Saturation's
//! (`tracks::saturation`, bd murmur-jvdc), timed: a timing aid, not a check, `cargo test
//! --release -p murmur-bridge picture_scale -- --ignored --nocapture` (`MURMUR_SCALE=0.5`
//! halves the observers per track, `2` doubles them). Beside it, the check that the feed
//! message its memos make is the one made afresh.

use std::time::Instant;

use serde_json::{Value, json};

use super::super::model::{Declared, STALE_S, Track};
use super::super::saturation::{det, picture, track};
use super::super::sensing::{Level, SensorType};
use super::super::truth::Domain;
use super::super::{FEED_SOURCES, Tracks, lock};
use super::{T0, catalog, settings};
use crate::clock::round;

/// The feed message as it was made before the tracks' JSON was kept (`json!(..).to_string()`).
fn message_afresh(tracks: &Tracks, now: f64) -> String {
    let inner = lock(&tracks.inner);
    let mut msg = json!({
        "type": "tracks",
        "t": round(now, 2),
        "stale_after_s": STALE_S,
        "tracks": inner.tracks.values().map(|t| t.json_with(now, Some(FEED_SOURCES))).collect::<Vec<_>>(),
        "lost": inner.lost.clone(),
    });
    let weapons = inner.weapons.json();
    if !weapons.is_empty() {
        msg["weapons"] = Value::Array(weapons);
    }
    msg.to_string()
}

/// bd murmur-jvdc: the feed message is the one `json!` makes, byte for byte, whatever its tracks'
/// kept JSON: kept, after a detection, after the mirror changes a track, and once tracks go stale.
#[test]
fn the_feed_message_is_the_one_made_afresh() {
    let tracks = Tracks::new(catalog(), settings());
    {
        let mut inner = lock(&tracks.inner);
        for no in 1..=3 {
            // more sources than the feed keeps, and fewer
            let t = track(no, Domain::Air, [40, 5, 20][no as usize - 1], 1, ["small_air", "small_uas", "small_drone"], false, T0);
            inner.tracks.insert(t.id.clone(), t);
        }
        let ship = track(4, Domain::Surface, 30, 0, ["large_surface", "large_vessel", "cargo_ship"], true, T0);
        inner.tracks.insert(ship.id.clone(), ship);
        inner.lost = vec![json!({"id": "T-0009", "out_of_view_s": 12.0, "regions": [], "affiliation": "unknown"})];
    }
    let last_seen = lock(&tracks.inner).tracks.values().map(|t| t.last_seen_t).fold(0.0, f64::max);
    let check = |now: f64| assert_eq!(tracks.message(now), message_afresh(&tracks, now), "at {now}");
    let now = last_seen + 1.0;
    check(now);
    // from the memos
    check(now + 0.5);
    // a detection changes one track
    {
        let mut inner = lock(&tracks.inner);
        let t = inner.tracks.get_mut("T-0002").expect("T-0002");
        t.update(&det("c2", 77, SensorType::EoIr, Domain::Air, "small_uas", Some(Level::Recognise), last_seen + 1.2), last_seen + 1.2);
    }
    check(now + 1.0);
    // the mirror changes another
    {
        let mut inner = lock(&tracks.inner);
        let t = inner.tracks.get_mut("T-0003").expect("T-0003");
        let declared = Declared {
            affiliation: murmur_decide::Affiliation::Hostile,
            by: "op-1".into(),
            t: now,
            note: None,
            id: Some(1),
            rule: false,
            evidence: None,
            confirmed_by: None,
            basis: Some("declared by op-1".into()),
        };
        assert!(t.set_mirror(Some(declared.clone()), Vec::new(), None));
        assert!(!t.set_mirror(Some(declared), Vec::new(), None), "unchanged");
    }
    check(now + 1.5);
    // stale
    check(last_seen + STALE_S + 5.0);
}

/// `f` once to warm up, then `runs` times: the median run and the fastest (the host is shared).
fn time<R>(what: &str, runs: u32, mut f: impl FnMut() -> R) -> R {
    let mut out = f();
    let mut ms: Vec<f64> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            out = f();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    println!("{what:<34} {:>9.1} ms median {:>9.1} ms fastest", ms[ms.len() / 2], ms[0]);
    out
}

#[test]
#[ignore = "a timing aid: run it by name with --ignored --nocapture, in release"]
fn picture_scale() {
    let scale: f64 = std::env::var("MURMUR_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let tracks = Tracks::new(catalog(), settings());
    let built = Instant::now();
    tracks.insert_tracks(picture(1, scale, T0));
    {
        let inner = lock(&tracks.inner);
        let sources: usize = inner.tracks.values().map(|t| t.sources.len()).sum();
        println!(
            "picture: {} tracks, {sources} sources (scale {scale}), built in {:.1} s",
            inner.tracks.len(),
            built.elapsed().as_secs_f64()
        );
    }
    let now = T0 + 3_000.0;
    // `MURMUR_SCALE_SPIN=<what>:<s>`: that reader alone, again and again for so long (to attach
    // a profiler to)
    if let Some((what, secs)) = std::env::var("MURMUR_SCALE_SPIN").ok().as_deref().and_then(|v| v.split_once(':')) {
        let until = Instant::now() + std::time::Duration::from_secs_f64(secs.parse().unwrap_or(30.0));
        while Instant::now() < until {
            match what {
                "message" => drop(tracks.message(now)),
                "facts" => drop(tracks.facts(now)),
                "levels" => drop(tracks.levels()),
                _ => drop(tracks.api_json(now)),
            }
        }
        return;
    }
    // what holds the store's lock, each on its own: the feed message after a detection of every
    // track (the fold's), and with nothing changed since the last (the tracker's second, the
    // decision loop's mirrors)
    let mut ms: Vec<f64> = (0..15)
        .map(|k| {
            let at = T0 + 2_990.0 + f64::from(k) * 0.01;
            for t in lock(&tracks.inner).tracks.values_mut() {
                let domain = t.domain;
                t.update(&det("x", 1, SensorType::EoIr, domain, "small_air", Some(Level::Detect), at), at);
            }
            let start = Instant::now();
            drop(tracks.message(now));
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    println!("{:<34} {:>9.1} ms median {:>9.1} ms fastest", "message (every track changed)", ms[7], ms[0]);
    let msg = time("message (nothing changed)", 15, || tracks.message(now));
    println!("{:<34} {:>9.1} MB", "  its size", msg.len() as f64 / 1e6);
    time("marks (push frame)", 15, || tracks.marks());
    // the decision layer's snapshot: facts, ais, levels
    let facts = time("facts (decision snapshot)", 15, || tracks.facts(now));
    time("ais (decision snapshot)", 15, || tracks.ais());
    time("levels (decision snapshot)", 15, || tracks.levels());
    time("api_json (GET /api/tracks)", 3, || tracks.api_json(now));
    time("  of it under the lock (a copy)", 3, || lock(&tracks.inner).tracks.values().cloned().collect::<Vec<Track>>());
    // what the engine does with the facts, outside this lock: a clone (each `world.tracks
    // .clone()`) and its drop
    let copy = time("facts clone (engine)", 15, || facts.clone());
    let start = Instant::now();
    drop(copy);
    println!("{:<34} {:>9.1} ms", "facts drop (engine)", start.elapsed().as_secs_f64() * 1000.0);
}
