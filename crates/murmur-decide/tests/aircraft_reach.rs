//! Decision rules 13 (`AIRCRAFT_REACH_SINCE`, murmur-oho): an aircraft with a velocity also
//! counts in a keep-out anywhere it reaches from its own speed, speeding up at the policy's air
//! manoeuvre acceleration to the domain's top speed, whether or not the tracker saw it turn.
//! The scenario is 83 (`sim/scenarios/engage/83-aircraft-may-turn-back.toml`).
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use common::*;
use murmur_decide::catalog::Domain;
use murmur_decide::engage::{ReachKind, aircraft_reach_m, keep_out_tracks, top_speed_mps, track_reach};
use murmur_decide::geo::haversine_m;
use murmur_decide::*;

const AT: LonLat = LonLat::new(-122.4950, 37.8120);

/// A light aircraft followed for ten minutes on a steady course east at 50 m/s (an average
/// velocity, no manoeuvre seen), its height not ranged.
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
    let (ec, ec12) = (rules(AIRCRAFT_REACH_SINCE), rules(REVIEW_FOLLOWUPS_SINCE));
    let t = aircraft(AT);
    let inside = |t: &TrackFacts, ec: &EngageRules, p: LonLat| track_reach(t, effect_t, ec).iter().any(|r| haversine_m(r.center, p) <= r.radius_m);
    // had it turned back, it could be 2.5 km west in a minute; its course says 3 km east
    let west = east(AT, -2_500.0);
    assert!(!inside(&t, &ec12, west), "rules 12: along its course and its average course only");
    assert!(inside(&t, &ec, west));
    let check = |t: &TrackFacts, ec: &EngageRules, p: LonLat| {
        let mut w = world();
        w.tracks = [("T-7".to_string(), t.clone())].into();
        keep_out_tracks(p, effect_t, 75.0, "T-0001", &w, ec, None)
    };
    assert_eq!(check(&t, &ec12, west).outcome, Outcome::Pass);
    let c = check(&t, &ec, west);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("may turn or speed up"), "{}", c.detail);
    // the region: its 2-sigma, plus from 52 m/s (its speed and 2 sigmas of the policy's velocity
    // error) speeding up at 2 m/s^2 to 100 m/s within the minute
    let r = track_reach(&t, effect_t, &ec).into_iter().find(|r| r.kind == ReachKind::Aircraft).unwrap();
    let (from, to, reach) = aircraft_reach_m(52.0, 60.0, &ec);
    assert_eq!((from, to), (52.0, 100.0));
    assert!((reach - (52.0 * 24.0 + 24.0 * 24.0 + 100.0 * 36.0)).abs() < 1e-9, "{reach}");
    assert!((r.radius_m - (40.0 + reach)).abs() < 1e-9, "{}", r.radius_m);
    assert_eq!(r.center, AT);
    // never beyond the top speed's reach, which a track without a velocity counts
    let mut lost = t.clone();
    lost.velocity_mps = None;
    let top = track_reach(&lost, effect_t, &ec);
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].kind, ReachKind::NoVelocity);
    assert!(r.radius_m < top[0].radius_m);
    assert!((top[0].radius_m - (40.0 + top_speed_mps(Domain::Air) * 60.0)).abs() < 1e-9);
    // a short span: little time to turn or speed up
    let (_, to, reach) = aircraft_reach_m(52.0, 5.0, &ec);
    assert_eq!(to, 62.0);
    assert!((reach - (52.0 * 5.0 + 25.0)).abs() < 1e-9, "{reach}");
    // a boat on the same course is predicted as before
    let mut boat = t.clone();
    boat.domain = Domain::Surface;
    boat.velocity_mps = Some([10.0, 0.0]);
    boat.mean_velocity_mps = Some([10.0, 0.0]);
    assert!(track_reach(&boat, effect_t, &ec).iter().all(|r| r.kind != ReachKind::Aircraft));
    assert_eq!(
        track_reach(&boat, effect_t, &ec).iter().map(|r| r.kind).collect::<Vec<_>>(),
        track_reach(&boat, effect_t, &ec12).iter().map(|r| r.kind).collect::<Vec<_>>()
    );
}
