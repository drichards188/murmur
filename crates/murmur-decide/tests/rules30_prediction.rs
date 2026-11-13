//! Rules 30, stage 2 (docs/NODE-LOSS.md sections 3 and 8, bd murmur-zb1d.2): where a silent
//! vehicle of ours can be. The friendly keep-out counts one whose position is not fresh within a
//! union of hypotheses (its orders, its lost-link failsafe, a drift, its top speed: C, L, D, F),
//! not anywhere its cruise speed reaches.
//!
//! - Scenarios 137, 138 and 143 end as designed under rules 30 (their `min_rules`), and under
//!   rules 29 (`--rules 29`) 137 is refused and 143 let through, as the cruise reach did.
//! - The region's hypotheses, one by one, on a hand-built world: a patrol's corridor, its way
//!   home, a drift growing, its top speed with nothing known, its propulsion out.
//! - What may be running on it ([`silent::in_force`]): the newest order it started, newer ones
//!   whose start is not confirmed, and nothing when the newest it started failed.
//! - The keep-out's explanation names the hypothesis that blocks and says waiting clears
//!   nothing while it is silent, and a confirmation of its loss would.
//! - Replay recomputes what may be running from its own record: a log whose `silent_orders`
//!   were edited is caught.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::engage::{self, EngageWindow, Fallback, FriendlyKind, FriendlyStanding};
use murmur_decide::keepout::{self, Options};
use murmur_decide::replay::replay_str;
use murmur_decide::silent::{self, Hypothesis, InForce, IntentView, RunningOrder};
use murmur_decide::world::NavSource;
use murmur_decide::*;
use murmur_decide::geo::haversine_m;

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const TARGET: &str = "T-0001";
/// HUNTER 1's patrol leg (500 m), 1.3 km east of the target, its south end first.
const LEG_S: LonLat = LonLat::new(-122.4800, 37.8130);
const LEG_N: LonLat = LonLat::new(-122.4800, 37.8175);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scenario(file: &str, rules: Option<u32>) -> scenario::ScenarioResult {
    let opts = scenario::RunOptions {
        rules_version: rules,
        ..scenario::RunOptions::default()
    };
    scenario::run_file(&root().join("sim/scenarios/engage").join(file), &root(), &opts).unwrap()
}

/// Its final state and reason, as words.
fn end(r: &scenario::ScenarioResult) -> (String, String) {
    (r.final_state.clone().unwrap_or_default(), r.final_reason.clone().unwrap_or_default())
}

fn cfg30() -> RulesConfig {
    let mut c = policy().rules;
    c.rules_version = NODE_LOSS_SINCE;
    c
}

/// One of ours, every fact reported at `t`, reporting every second.
fn ours(platform: &str, at: LonLat, t: Time) -> VehicleFacts {
    let mut v = vehicle(platform, at);
    v.autopilot = v.autopilot.map(|f| Fact::new(f.value, t));
    v.position = Some(Fact::new(at, t));
    v.nav_ready = Some(Fact::new(true, t));
    v.nav_source = Some(Fact::new(NavSource::Gnss, t));
    v.gps_fix = Some(Fact::new(3, t));
    v.gps_glitch = Some(Fact::new(false, t));
    v.battery_pct = Some(Fact::new(95.0, t));
    v.velocity_mps = Some(Fact::new([0.0, 0.0], t));
    v.report_interval_ms = Some(1000);
    v
}

fn rtl_plan() -> Fact<LostLinkPlan> {
    Fact::new(
        LostLinkPlan {
            action: LostLinkAction::Rtl,
            timer_s: Some(5.0),
            source: LostLinkSource::Read,
            circle_m: None,
        },
        Time(T0.0 - 600_000),
    )
}

/// HUNTER 1 (a SeaHunter) silent `silent_s` before T0 on its patrol leg's south end, going north
/// at 6 m/s, home `home`, its own failsafe an rtl.
fn hunter(silent_s: i64, home: LonLat) -> VehicleFacts {
    let t = Time(T0.0 - silent_s * 1000);
    let mut v = ours("seahunter", LEG_S, t);
    v.velocity_mps = Some(Fact::new([0.0, 6.0], t));
    v.home = Some(Fact::new(home, Time(T0.0 - 3_600_000)));
    v.lost_link = Some(rtl_plan());
    v
}

/// Its patrol, started (op active).
fn patrol(intent: u64) -> RunningOrder {
    RunningOrder {
        intent: IntentId(intent),
        kind: TaskKind::Patrol,
        unconfirmed: false,
        path: vec![LEG_N, LEG_S],
        loop_from: Some(0),
        circles: Vec::new(),
        home: false,
        fallback: None,
        preload: None,
    }
}

fn declared_target() -> TrackFacts {
    TrackFacts {
        position: Fact::new(TARGET_AT, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 10.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Hostile, T0),
        affiliation_confidence: 0.8,
        identity: Some("small_fast_boat".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into()],
        source_t: [("3:radar".to_string(), T0)].into_iter().collect(),
        domain: Domain::Surface,
        last_seen_t: T0,
        declared_by: Some("op-1".into()),
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: Some([0.0, 0.0]),
        first_seen_t: Some(Time(T0.0 - 900_000)),
        velocity_sigma_mps: None,
    }
}

fn nomination() -> IntentSpec {
    IntentSpec {
        id: IntentId(0),
        created_t: T0,
        source: Source::Human {
            id: OperatorId::new("op-1"),
        },
        task: Task::Engage {
            track: TARGET.into(),
            effect_radius_m: 75.0,
            window: Some(EngageWindow {
                from: None,
                until: T0.plus_ms(600_000),
            }),
            fallback: Fallback::Return,
            weapon: None,
        },
        uids: vec![uid("blade-1")],
        constraints: Constraints::default(),
        rationale: None,
    }
}

/// The 138 picture at the nomination: the Switchblade, the declared target, HUNTER 1 silent on
/// its leg with its patrol in force.
fn world_138(home: LonLat) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), declared_target());
    w.vehicles.insert(uid("blade-1"), ours("switchblade", LonLat::new(-122.4700, 37.8160), T0));
    w.vehicles.insert(uid("hunter-1"), hunter(57, home));
    w.silent_orders.insert(
        uid("hunter-1"),
        InForce {
            orders: vec![patrol(1)],
            unknown: None,
            presumed: false,
        },
    );
    w
}

fn region_at(w: &WorldSnapshot, u: &str, span_s: i64) -> silent::Region {
    let v = &w.vehicles[&uid(u)];
    let t0 = v.position.as_ref().unwrap().t;
    silent::region(&uid(u), v, w, &cfg30(), t0.plus_ms(span_s * 1000), false).unwrap()
}

// ---- the scenarios -------------------------------------------------------------------------

#[test]
fn scenarios_137_138_143_end_as_designed_under_rules_30() {
    for f in [
        "137-silent-patroller-bounded-by-its-route.toml",
        "138-silent-patrollers-way-home-crosses-the-keep-out.toml",
        "143-silent-with-nothing-known-at-top-speed.toml",
    ] {
        let r = scenario(f, None);
        assert!(r.ok && r.replay_ok, "{f}: {:?} {:?}", r.failures, r.replay_mismatches);
        assert!(r.log.lines().next().unwrap().contains(&format!("\"rules_version\":{NODE_LOSS_SINCE},")), "{f}");
    }
    // 137: counted on its leg, 820 m clear, signed through to the effect
    let r = scenario("137-silent-patroller-bounded-by-its-route.toml", None);
    assert!(r.log.contains("nearest hunter-1 at 825 m"), "137 keeps hunter-1 on its leg");
    // 138: refused for its failsafe's way home, named in the check
    let (_, why) = end(&scenario("138-silent-patrollers-way-home-crosses-the-keep-out.toml", None));
    assert!(why.contains("if it lost us too, its rtl failsafe"), "{why}");
    // 143: refused at its top speed, saying nothing places it
    let (_, why) = end(&scenario("143-silent-with-nothing-known-at-top-speed.toml", None));
    assert!(why.contains("anywhere its top speed (25 m/s) reaches"), "{why}");
    assert!(why.contains("neither circling nor still"), "{why}");
}

#[test]
fn rules_29_counted_the_same_silent_vehicles_at_cruise_speed() {
    // 137: refused, its cruise reach over the effect point although its patrol runs 0.8 km clear
    let r = scenario("137-silent-patroller-bounded-by-its-route.toml", Some(HANDOVER_SINCE));
    let (state, why) = end(&r);
    assert_eq!(state, "rejected", "{why}");
    assert!(why.contains("how far it could reach at cruise speed"), "{why}");
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    assert!(!r.log.contains("silent_orders"), "rules 29 logs no silent orders");
    // 143: let through, its cruise reach 330 m clear
    let r = scenario("143-silent-with-nothing-known-at-top-speed.toml", Some(HANDOVER_SINCE));
    let (state, why) = end(&r);
    assert_eq!(state, "pending_approval", "{why}");
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
}

// ---- the region, hypothesis by hypothesis ----------------------------------------------------

#[test]
fn a_patrol_bounds_it_and_its_way_home_is_counted_if_it_lost_us_too() {
    // home on its leg: the patrol's corridor and the way home are the leg itself (widened for its
    // turns and a cross current), however long it is silent; never near the target 1.3 km west
    let w = world_138(LEG_S);
    for span in [30, 120, 600, 3_600] {
        let r = region_at(&w, "hunter-1", span);
        let (edge, part) = r.edge_m(TARGET_AT);
        assert!(edge > 700.0, "{span} s: {edge:.0} m");
        // its reach sets the edge until it covers the leg
        if let Some(i) = part {
            assert!(matches!(r.parts[i].hypothesis, Hypothesis::Order | Hypothesis::Failsafe), "{:?}", r.parts[i]);
        }
        // where it was heard is in it, and the leg's north end once its reach gets there
        assert!(r.margin_m(LEG_S) >= 0.0);
        assert_eq!(r.margin_m(LEG_N) >= 0.0, r.reach.pad_m > haversine_m(LEG_S, LEG_N), "{span} s");
    }
    // home west of the target: the way home from anywhere on the leg crosses it
    let west = LonLat::new(-122.5050, 37.8120);
    let r = region_at(&world_138(west), "hunter-1", 150);
    let (edge, part) = r.edge_m(TARGET_AT);
    let p = &r.parts[part.unwrap()];
    assert!(edge < 275.0, "{edge:.0}");
    assert_eq!((p.hypothesis, p.kind), (Hypothesis::Failsafe, FriendlyKind::FailsafeHome));
    assert!(region_at(&world_138(west), "hunter-1", 60).margin_m(west) < 0.0, "home is 2.2 km off, beyond its reach in a minute");
    assert!(region_at(&world_138(west), "hunter-1", 600).margin_m(west) >= 0.0, "home itself, once in reach");
    // its own failsafe off: the patrol alone, the way home not counted
    let mut w = world_138(west);
    w.vehicles.get_mut(&uid("hunter-1")).unwrap().lost_link.as_mut().unwrap().value.action = LostLinkAction::Continue;
    let (edge, _) = region_at(&w, "hunter-1", 60).edge_m(TARGET_AT);
    assert!(edge > 700.0, "{edge:.0}");
}

#[test]
fn not_knowing_its_failsafe_counts_every_action_its_autopilot_has() {
    // a boat's plan not known: it may rtl (home west: over the target) or hold and drift
    let west = LonLat::new(-122.5050, 37.8120);
    let mut w = world_138(LEG_S);
    w.vehicles.get_mut(&uid("hunter-1")).unwrap().lost_link = None;
    let r = region_at(&w, "hunter-1", 60);
    assert!(r.parts.iter().any(|p| p.hypothesis == Hypothesis::Failsafe && p.kind == FriendlyKind::Drift));
    // a hold on the water stops its motors: adrift at 2.5 m/s from anywhere on the leg, 150 m
    // wider after a minute than its patrol alone (its corridor 329 m either side)
    let east_of_leg = engage::offset(LEG_S, 380.0, 0.0);
    let plan = |a: LostLinkAction| {
        let mut w = world_138(LEG_S);
        w.vehicles.get_mut(&uid("hunter-1")).unwrap().lost_link.as_mut().unwrap().value.action = a;
        region_at(&w, "hunter-1", 60).margin_m(east_of_leg)
    };
    assert!(plan(LostLinkAction::Hold) >= 0.0 && plan(LostLinkAction::Continue) < 0.0);
    assert!(r.margin_m(east_of_leg) >= 0.0, "not known: a hold it may have");
    let w2 = {
        let mut w = world_138(west);
        w.vehicles.get_mut(&uid("hunter-1")).unwrap().lost_link = None;
        w
    };
    let (edge, _) = region_at(&w2, "hunter-1", 150).edge_m(TARGET_AT);
    assert!(edge < 275.0, "an rtl it may have: {edge:.0}");
}

#[test]
fn presumed_lost_it_drifts_and_with_its_propulsion_out_it_only_drifts() {
    let w = world_138(LEG_S);
    let v = &w.vehicles[&uid("hunter-1")];
    let t0 = v.position.as_ref().unwrap().t;
    // presumed lost (stage 3's flag): adrift too, at 2.5 m/s, the disc growing
    let at = |s: i64| t0.plus_ms(s * 1000);
    let r60 = silent::region(&uid("hunter-1"), v, &w, &cfg30(), at(60), true).unwrap();
    let r600 = silent::region(&uid("hunter-1"), v, &w, &cfg30(), at(600), true).unwrap();
    let d = |r: &silent::Region| r.parts.iter().find(|p| p.hypothesis == Hypothesis::Drift).unwrap().shape.pad_m;
    assert!((d(&r60) - 150.0).abs() < 1e-6 && (d(&r600) - 1_500.0).abs() < 1e-6);
    // the target 1.3 km off: clear while the drift is short of it, inside once it grows
    assert!(r60.edge_m(TARGET_AT).0 > 275.0);
    assert!(r600.edge_m(TARGET_AT).0 < 0.0);
    // the same from the presumption the engine's ledger puts in its silent orders
    let mut wp = w.clone();
    wp.silent_orders.get_mut(&uid("hunter-1")).unwrap().presumed = true;
    let rp = silent::region(&uid("hunter-1"), v, &wp, &cfg30(), at(600), false).unwrap();
    assert_eq!(rp, r600);
    // its propulsion out (its own damage report): adrift only, its patrol no longer counted
    let mut v = v.clone();
    v.damage = Some(Fact::new(VehicleDamage { mobility: true, firepower: false }, t0));
    let r = silent::region(&uid("hunter-1"), &v, &w, &cfg30(), at(60), false).unwrap();
    assert!(r.parts.iter().all(|p| p.hypothesis == Hypothesis::Drift), "{:?}", r.parts);
    assert!(r.margin_m(LEG_N) < 0.0, "not on its patrol any more");
}

#[test]
fn with_nothing_known_it_counts_at_its_top_speed() {
    let mut w = world_138(LEG_S);
    // no order of ours on it, under way: nothing places it
    w.silent_orders.insert(uid("hunter-1"), InForce::default());
    let r = region_at(&w, "hunter-1", 60);
    let top = r.parts.iter().find(|p| p.hypothesis == Hypothesis::TopSpeed).unwrap();
    // SeaHunter's max_mps 13 and the water's 2.5
    assert!((top.shape.pad_m - 60.0 * 15.5).abs() < 1e-6, "{}", top.shape.pad_m);
    // still when it was last heard: it holds there
    let mut w = world_138(LEG_S);
    w.silent_orders.insert(uid("hunter-1"), InForce::default());
    let h = w.vehicles.get_mut(&uid("hunter-1")).unwrap();
    h.velocity_mps.as_mut().unwrap().value = [0.0, 0.0];
    let r = region_at(&w, "hunter-1", 60);
    assert!(r.parts.iter().all(|p| p.hypothesis != Hypothesis::TopSpeed), "{:?}", r.parts);
    // not filled (a world the engine did not fill): F, fail closed
    let mut w = world_138(LEG_S);
    w.silent_orders.clear();
    let r = region_at(&w, "hunter-1", 60);
    assert!(r.parts.iter().any(|p| p.hypothesis == Hypothesis::TopSpeed));
}

#[test]
fn a_fresh_vehicle_and_rules_29_are_judged_as_before() {
    let w = world_138(LEG_S);
    let mut fresh = w.clone();
    fresh.vehicles.insert(uid("hunter-1"), ours("seahunter", LEG_S, T0));
    for (world, rules) in [(&fresh, cfg30()), (&w, policy().rules)] {
        let st = engage::friendly_standing(
            &uid("hunter-1"),
            &world.vehicles[&uid("hunter-1")],
            TARGET_AT,
            T0.plus_ms(100_000),
            275.0,
            world,
            &rules,
            None,
            true,
        );
        let FriendlyStanding::Judged { kind, .. } = st else { panic!("{st:?}") };
        assert!(!kind.silent(), "{kind:?}");
    }
}

// ---- what may be running on it ----------------------------------------------------------------

#[test]
fn the_newest_order_it_started_runs_and_an_unconfirmed_one_may() {
    let hold = |id: u64| IntentSpec {
        id: IntentId(id),
        created_t: T0,
        source: Source::Human { id: OperatorId::new("op-1") },
        task: Task::Hold {},
        uids: vec![uid("hunter-1")],
        constraints: Constraints::default(),
        rationale: None,
    };
    let orbit = |id: u64| IntentSpec {
        task: Task::Orbit { center: LEG_N, radius_m: 200.0 },
        ..hold(id)
    };
    let (s1, s2, s3) = (orbit(1), hold(2), orbit(3));
    let u = uid("hunter-1");
    let one: std::collections::BTreeSet<Uid> = [u.clone()].into_iter().collect();
    let none = std::collections::BTreeSet::new();
    let op = |s: &str| -> std::collections::BTreeMap<Uid, String> { [(u.clone(), s.to_string())].into_iter().collect() };
    let (active, starting, failed) = (op("active"), op("starting"), op("failed"));
    let view = |spec: &'static IntentSpec, ops, committed, maybe| IntentView {
        id: spec.id,
        spec,
        state: State::Dispatched,
        ops,
        committed,
        maybe_started: maybe,
        order: None,
        orbit_now: None,
    };
    let (s1, s2, s3): (&'static IntentSpec, &'static IntentSpec, &'static IntentSpec) =
        (Box::leak(Box::new(s1)), Box::leak(Box::new(s2)), Box::leak(Box::new(s3)));
    // 1 started, 2 started: only 2 runs
    let f = silent::in_force_on(&u, &[view(s1, &active, &one, &none), view(s2, &active, &one, &none)]);
    assert_eq!(f.orders.iter().map(|o| o.intent.0).collect::<Vec<_>>(), [2]);
    // 3's start went out, unconfirmed: 3 may run, and so may 2 before it
    let f = silent::in_force_on(
        &u,
        &[view(s1, &active, &one, &none), view(s2, &active, &one, &none), view(s3, &starting, &none, &one)],
    );
    assert_eq!(f.orders.iter().map(|o| (o.intent.0, o.unconfirmed)).collect::<Vec<_>>(), [(3, true), (2, false)]);
    // 2 failed after it started: what it does now is not known (F), 1 no longer counts
    let f = silent::in_force_on(&u, &[view(s1, &active, &one, &none), view(s2, &failed, &one, &none)]);
    assert!(f.orders.is_empty() && f.unknown.as_deref().is_some_and(|w| w.contains("failed after it started")), "{f:?}");
}

// ---- the explanation ----------------------------------------------------------------------------

#[test]
fn the_explanation_names_its_way_home_and_says_waiting_clears_nothing() {
    let west = LonLat::new(-122.5050, 37.8120);
    let w = world_138(west);
    let spec = nomination();
    let named = Options {
        names: [(uid("hunter-1"), "HUNTER 1".to_string())].into_iter().collect(),
        ..Options::default()
    };
    let ex = keepout::explain(&spec, &w, &cfg30(), &named).unwrap();
    assert!(ex.blocked);
    let it = ex.items.iter().find(|i| i.id == "hunter-1").unwrap();
    assert!(it.blocks, "{it:?}");
    let region = it.region.as_ref().unwrap();
    assert_eq!(region.kind, "failsafe_home");
    assert!(region.outline.len() >= 3, "its way home is a polygon: {:?}", region.outline);
    assert!(it.reason.contains("if it lost us too") && it.reason.contains("its way home"), "{}", it.reason);
    assert!(
        it.not_by_waiting.as_deref().is_some_and(|n| n.contains("no one can retask it")),
        "{:?}",
        it.not_by_waiting
    );
    assert!(it.other.as_deref().is_some_and(|o| o.contains("confirming it lost (Review)")), "{:?}", it.other);
    assert!(it.wait.is_none(), "waiting clears nothing");
    // home on its leg: it no longer blocks, and is not listed (0.8 km clear)
    let ex = keepout::explain(&spec, &world_138(LEG_S), &cfg30(), &named).unwrap();
    assert!(ex.items.iter().all(|i| i.id != "hunter-1"), "{:?}", ex.items);
}

#[test]
fn a_vehicle_confirmed_lost_is_left_out_of_the_explanation_as_of_the_check() {
    // its way home crosses the keep-out; confirmed lost (stage 3's ledger), the check leaves it
    // out, and the explanation agrees and lists nothing for it
    let west = LonLat::new(-122.5050, 37.8120);
    let mut w = world_138(west);
    let since = w.vehicles[&uid("hunter-1")].position.as_ref().map(|p| p.t);
    w.nodes_lost.insert(
        uid("hunter-1"),
        LostNode { since, by: OperatorId::new("op-2"), t: T0, from: None, lost_on: None, now: None },
    );
    let ex = keepout::explain(&nomination(), &w, &cfg30(), &Options::default()).unwrap();
    assert!(ex.items.iter().all(|i| i.id != "hunter-1"), "{:?}", ex.items);
}

// ---- replay -------------------------------------------------------------------------------------

/// Renumber and re-hash a log's lines, every other byte kept (a forger's tool).
fn rechain(lines: &[&str]) -> String {
    let mut prev = "0".repeat(64);
    let mut out = String::new();
    for (n, l) in lines.iter().enumerate() {
        let t_at = l.find(",\"t\":").unwrap();
        let p_at = l.find(",\"prev\":\"").unwrap();
        let after = &l[p_at + 9 + 64 + 1..];
        let line = format!("{{\"seq\":{}{},\"prev\":\"{prev}\"{after}", n + 1, &l[t_at..p_at]);
        prev = murmur_decide::hash::sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[test]
fn replay_recomputes_what_may_be_running_and_catches_an_edit() {
    let r = scenario("137-silent-patroller-bounded-by-its-route.toml", None);
    let rep = replay_str(&r.log).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
    // the patrol moved 900 m east in every decision's silent_orders (and nowhere else)
    let was = "\"path\":[[-122.48,37.8175],[-122.48,37.813]],\"loop_from\":0";
    assert!(r.log.contains(was));
    let edited = r.log.replace(was, "\"path\":[[-122.47,37.8175],[-122.47,37.813]],\"loop_from\":0");
    let lines: Vec<&str> = edited.lines().filter(|l| !l.trim().is_empty()).collect();
    let rep = replay_str(&rechain(&lines)).unwrap();
    assert!(
        rep.mismatches.iter().any(|m| m.what.contains("may be running on our silent vehicles differ from replay's record")),
        "{:?}",
        rep.mismatches.iter().map(|m| &m.what).collect::<Vec<_>>()
    );
}
