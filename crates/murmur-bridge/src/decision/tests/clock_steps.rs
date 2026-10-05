//! murmur-dup: a wall clock that steps, through the bridge. This WSL host's wall clock steps
//! back about 0.65 s every 32 s (0.94 s on an earlier boot) while its monotonic clock runs on;
//! NTP and a resumed virtual machine step it either way. Each test steps this thread's wall
//! clock (`clock::test_clock::step_utc`) where a step would have moved what the bridge
//! decides: before the bridge kept a clock of its own (`clock::wall`, the wall clock at the
//! first read plus monotonic time since), the facts' stamps, the engine's clock and every age
//! taken between them read the wall clock itself, and these tests failed.

use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
use crate::tracks::truth::{ContactTruth, Domain as TDomain, Sensing, TruthAffiliation, TruthMessage};

use super::engagement::{Erig, TARGET, engage_rig_clocked};
use super::*;

/// A copter's heartbeat may be this old (`max_fact_age_s`; three of its 1 s report
/// intervals are less).
const LIMIT_S: f64 = 10.0;

fn op(s: &str) -> Actor {
    Actor::asserted(OperatorId::new(s))
}

fn bridge_engage_rig() -> Erig {
    engage_rig_clocked(None, PathBuf::from("memory"), murmur_decide::AuthMode::Dev, |_| {}, EngineClock::Bridge)
}

/// A second on the bridge's clock: time passes, the truth tick, the decision loop.
async fn tick_on_bridge(e: &Erig) {
    sleep(Duration::from_secs(1)).await;
    e.r.clock.set(snapshot::ms(clock::wall()));
    e.sense(true);
    e.r.d.tick();
}

#[tokio::test(start_paused = true)]
async fn a_wall_clock_stepping_forward_voids_nothing() {
    let r = waiting_rig();
    r.vehicle(7, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let waiting = r.d.submit(&op1(), orbit(7)).await.expect("submit").id;
    assert_eq!(r.state(waiting), State::PendingApproval);
    // an hour forward (an NTP correction, a resumed VM): the vehicle was heard a moment ago
    clock::test_clock::step_utc(3_600.0);
    r.d.tick();
    assert_eq!(r.state(waiting), State::PendingApproval, "{}", r.reason(waiting));
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_vehicle_silent_past_its_limit_is_voided_across_a_backward_step() {
    let r = waiting_rig();
    r.vehicle(7, Firmware::Copter, "switchblade", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    let waiting = r.d.submit(&op1(), orbit(7)).await.expect("submit").id;
    let cell = r.world.vehicles().into_iter().find(|c| c.uid == 7).expect("cell");
    cell.write(|v| v.seen.heartbeat = Some(clock::wall() - (LIMIT_S + 0.5)));
    // this host's step: its silence must not read half a second short of the limit
    clock::test_clock::step_utc(-0.94);
    r.d.tick();
    assert_eq!(r.state(waiting), State::Voided, "{}", r.reason(waiting));
    assert!(r.reason(waiting).contains("lost"), "{}", r.reason(waiting));
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_wall_clock_stepping_forward_leaves_the_facts_fresh() {
    let policy = Policy::load(&root().join("sim/policy.toml")).expect("policy");
    let r = rig_clocked(policy, murmur_decide::AuthMode::Dev, EngineClock::Bridge);
    r.vehicle(9, Firmware::Copter, "x10", (LAT, LON));
    sleep(Duration::from_millis(500)).await;
    clock::test_clock::step_utc(120.0);
    let out = r.d.submit(&op1(), orbit(9)).await.expect("submit");
    let text = out.intent.to_string();
    assert!(!text.contains(" s old (limit"), "{text}");
    assert_eq!(out.state, State::Dispatched, "{text}");
    r.replays();
}

#[tokio::test(start_paused = true)]
async fn a_signers_picture_token_lives_its_time_across_a_forward_step() {
    let e = bridge_engage_rig();
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.picture().await;
    let out = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("nominated");
    assert_eq!(out.state, State::PendingApproval, "{}", out.intent);
    let token = e.look("op-2", out.id).expect("look")["id"].as_u64();
    // the token is good for 15 s; the wall clock jumps 30 s while the signer reads it
    clock::test_clock::step_utc(30.0);
    let o = e.r.d.approve_with(&op("op-2"), out.id.0, None, token, false).await.expect("signs");
    assert_eq!(o.state, State::PendingApproval, "{}", o.intent);
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_wall_clock_stepping_forward_does_not_stop_an_engagement() {
    let e = bridge_engage_rig();
    // en route, 500 m out: no release is ready over these seconds
    let id = e.approved_at("return", (TARGET.0, TARGET.1 + 0.0057)).await;
    tick_on_bridge(&e).await;
    assert_eq!(e.r.state(id), State::Dispatched, "{}", e.r.reason(id));
    // the picture is held every second, at most 5 s apart: a 30 s jump of the wall clock
    // between two of them is no gap
    clock::test_clock::step_utc(30.0);
    tick_on_bridge(&e).await;
    assert_eq!(e.r.state(id), State::Dispatched, "{}", e.r.reason(id));
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[tokio::test(start_paused = true)]
async fn a_wall_clock_stepping_forward_keeps_the_tracks() {
    let e = bridge_engage_rig();
    e.picture().await;
    assert!(e.tracks.facts(clock::wall()).contains_key("T-0001"));
    // past the drop time (180 s): the track was seen a moment ago
    clock::test_clock::step_utc(300.0);
    e.tracks.tick();
    let facts = e.tracks.facts(clock::wall());
    let t = facts.get("T-0001").expect("still tracked");
    assert!(t.confidence > 0.5, "{}", t.confidence);
}

/// A boat at 20 m/s, east, as a radar 1.5 km off sees it, `s` seconds into its run.
fn sense_mover(e: &Erig, s: u32) {
    let t = T0_S + f64::from(s);
    let lon0 = TARGET.1 - 0.02;
    let msg = TruthMessage {
        t,
        s: t,
        seq: u64::from(s),
        scenario: "test".into(),
        seed: 1,
        sensing: Sensing {
            misclassification_rate: 0.0,
        },
        contacts: vec![ContactTruth {
            id: "runner".into(),
            name: "runner".into(),
            affiliation: TruthAffiliation::Unknown,
            domain: TDomain::Surface,
            kind: "small_fast_boat".into(),
            lat: TARGET.0,
            lon: lon0 + 20.0 * f64::from(s) / (111_320.0 * TARGET.0.to_radians().cos()),
            alt_m: 0.0,
            speed_mps: 20.0,
            course_deg: 90.0,
            emits: false,
            signature: 1.0,
            ais: None,
        }],
        acts: Vec::new(),
    };
    let observer = Observer {
        uid: 9,
        lat: TARGET.0 + 0.0135,
        lon: TARGET.1 - 0.01,
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        sensors: vec![SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false }],
    };
    // arrival on the bridge's clock, as `Tracks::on_truth` stamps it
    e.tracks.on_truth_with(msg, &[observer], clock::wall()).expect("sensing");
}

#[tokio::test(start_paused = true)]
async fn a_backward_step_does_not_carry_a_track_back_along_its_course() {
    let e = bridge_engage_rig();
    for s in 0..60 {
        sense_mover(&e, s);
        sleep(Duration::from_secs(1)).await;
    }
    clock::test_clock::step_utc(-0.94);
    let snap = e.r.d.snapshot();
    let (_, track) = snap.tracks.iter().next().expect("tracked");
    let [ve, _] = track.velocity_mps.expect("a velocity");
    assert!(ve > 10.0, "{ve}");
    // the picture never holds a fact from after its own time...
    assert!(snap.t >= track.position.t, "{} < {}", snap.t, track.position.t);
    // ...so the engine never predicts a track back along its course (the step times 20 m/s)
    let now = murmur_decide::engage::track_at(track, snap.t);
    let east_m = (now.lon - track.position.value.lon) * 111_320.0 * now.lat.to_radians().cos();
    assert!(east_m > -0.5, "predicted {east_m:.1} m behind its last fix");
}
