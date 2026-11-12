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
