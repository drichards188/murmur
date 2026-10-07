//! Decision rules 14 (`AIRCRAFT_REACH_SINCE`, murmur-oho): an aircraft with a velocity also
//! counts in a keep-out anywhere it reaches from its speed bound (its speed, `keep_out_sigma` of
//! its velocity's error, and the tracker's lag behind a speed-up), speeding up at a stated bound
//! to the domain's top speed, whether or not the tracker saw it turn; every region of an
//! aircraft adds how far the tracker may lag behind it. The scenario is 83
//! (`sim/scenarios/engage/83-aircraft-may-turn-back.toml`).
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use common::*;
use murmur_decide::catalog::Domain;
use murmur_decide::engage::{
    AIRCRAFT_LAG_M, AIRCRAFT_LAG_S, AIRCRAFT_SPEEDUP_MPS2, ReachKind, aircraft_reach_m, aircraft_speed_bound, keep_out_tracks,
    top_speed_mps, track_reach,
};
use murmur_decide::geo::haversine_m;
use murmur_decide::*;

const AT: LonLat = LonLat::new(-122.4950, 37.8120);

/// A light aircraft followed for ten minutes on a steady course east at 50 m/s (an average
/// velocity, no manoeuvre seen), its velocity known to 2 m/s, its height not ranged.
fn aircraft(at: LonLat) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([50.0, 0.0]),
        pos_sigma_m: 20.0,
        sigma_growth_mps: Some(15.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Neutral, T0),
        affiliation_confidence: 0.8,
        identity: Some("light_aircraft".into()),
        identity_confidence: 0.9,
        sources: vec!["usv-1:radar".into(), "usv-1:eo_ir".into()],
        source_t: BTreeMap::new(),
        domain: Domain::Air,
        last_seen_t: T0,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: Some([50.0, 0.0]),
        first_seen_t: Some(T0.plus_ms(-600_000)),
        velocity_sigma_mps: Some(2.0),
    }
}

/// `m` metres east of `p`.
fn east(p: LonLat, m: f64) -> LonLat {
    LonLat::new(p.lon + m / (111_320.0 * p.lat.to_radians().cos()), p.lat)
}

fn rules(version: u32) -> EngageRules {
    EngageRules {
        rules_version: version,
        ..EngageRules::default()
    }
}

#[test]
fn an_aircraft_counts_anywhere_it_could_turn_or_speed_up_to() {
    let effect_t = T0.plus_ms(60_000);
    let (ec, before) = (rules(AIRCRAFT_REACH_SINCE), rules(AIRCRAFT_REACH_SINCE - 1));
    let t = aircraft(AT);
    let inside = |t: &TrackFacts, ec: &EngageRules, p: LonLat| track_reach(t, effect_t, ec).iter().any(|r| haversine_m(r.center, p) <= r.radius_m);
    // had it turned back, it could be 2.5 km west in a minute; its course says 3 km east
    let west = east(AT, -2_500.0);
    assert!(!inside(&t, &before, west), "rules 13: along its course and its average course only");
    assert!(inside(&t, &ec, west));
    let check = |t: &TrackFacts, ec: &EngageRules, p: LonLat| {
        let mut w = world();
        w.tracks = [("T-7".to_string(), t.clone())].into();
        keep_out_tracks(p, effect_t, 75.0, "T-0001", &w, ec, None)
    };
    assert_eq!(check(&t, &before, west).outcome, Outcome::Pass);
    let c = check(&t, &ec, west);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("may turn or speed up"), "{}", c.detail);
    // its speed bound: 50 m/s, 2 sigmas of its velocity error (2 m/s, above the policy's 1), and
    // the tracker's lag behind a speed-up at the stated bound
    let bound = aircraft_speed_bound(&t, &ec);
    assert!((bound - (50.0 + 4.0 + AIRCRAFT_LAG_S * AIRCRAFT_SPEEDUP_MPS2)).abs() < 1e-9, "{bound}");
    // with no error of its own given (a scenario), the policy's stands in
    let mut plain = t.clone();
    plain.velocity_sigma_mps = None;
    assert!((aircraft_speed_bound(&plain, &ec) - (50.0 + 2.0 + AIRCRAFT_LAG_S * AIRCRAFT_SPEEDUP_MPS2)).abs() < 1e-9);
    // the region: its 2-sigma, the tracker's lag behind it, plus speeding up from the bound at
    // the stated rate to 100 m/s
    let r = track_reach(&t, effect_t, &ec).into_iter().find(|r| r.kind == ReachKind::Aircraft).unwrap();
    let (from, to, reach) = aircraft_reach_m(bound, 60.0);
    let t1 = (100.0 - bound) / AIRCRAFT_SPEEDUP_MPS2;
    assert_eq!((from, to), (bound, 100.0));
    assert!((reach - (bound * t1 + 0.5 * AIRCRAFT_SPEEDUP_MPS2 * t1 * t1 + 100.0 * (60.0 - t1))).abs() < 1e-6, "{reach}");
    assert!((AIRCRAFT_LAG_M - 0.5 * AIRCRAFT_SPEEDUP_MPS2 * AIRCRAFT_LAG_S * AIRCRAFT_LAG_S).abs() < 1e-9);
    assert!((r.radius_m - (40.0 + AIRCRAFT_LAG_M + reach)).abs() < 1e-9, "{}", r.radius_m);
    assert_eq!(r.center, AT);
    // never beyond the top speed's reach, which a track without a velocity counts (an aircraft
    // with the tracker's lag too; a boat without)
    let mut lost = t.clone();
    lost.velocity_mps = None;
    let top = track_reach(&lost, effect_t, &ec);
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].kind, ReachKind::NoVelocity);
    assert!(r.radius_m < top[0].radius_m);
    assert!((top[0].radius_m - (40.0 + AIRCRAFT_LAG_M + top_speed_mps(Domain::Air) * 60.0)).abs() < 1e-9);
    let top13 = track_reach(&lost, effect_t, &before);
    assert!((top13[0].radius_m - (40.0 + top_speed_mps(Domain::Air) * 60.0)).abs() < 1e-9, "rules 13: no lag");
    // the lag never makes it faster than the top speed; its own speed and error may
    let mut fast = t.clone();
    fast.velocity_mps = Some([90.0, 0.0]);
    assert!((aircraft_speed_bound(&fast, &ec) - 100.0).abs() < 1e-9);
    fast.velocity_mps = Some([120.0, 0.0]);
    assert!((aircraft_speed_bound(&fast, &ec) - 124.0).abs() < 1e-9);
    // a short span: a few seconds of speeding up
    let (_, to, reach) = aircraft_reach_m(bound, 3.0);
    assert!((to - (bound + 3.0 * AIRCRAFT_SPEEDUP_MPS2)).abs() < 1e-9);
    assert!((reach - (bound * 3.0 + 0.5 * AIRCRAFT_SPEEDUP_MPS2 * 9.0)).abs() < 1e-9, "{reach}");
    // a boat on the same course is predicted as before
    let mut boat = t.clone();
    boat.domain = Domain::Surface;
    boat.velocity_mps = Some([10.0, 0.0]);
    boat.mean_velocity_mps = Some([10.0, 0.0]);
    assert!(track_reach(&boat, effect_t, &ec).iter().all(|r| r.kind != ReachKind::Aircraft));
    assert_eq!(
        track_reach(&boat, effect_t, &ec).iter().map(|r| r.kind).collect::<Vec<_>>(),
        track_reach(&boat, effect_t, &before).iter().map(|r| r.kind).collect::<Vec<_>>()
    );
}
