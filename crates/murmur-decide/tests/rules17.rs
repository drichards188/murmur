//! Decision rules 17 (`ROUTE_SURFACE_SINCE`, murmur-k8i item 2): the domain rule judges the way a
//! boat, sub or ground vehicle goes, not only its waypoints. Land or water every 20 m along the
//! transit from where it is, the task path's legs and the return home; a mission whose way crosses
//! the wrong surface is refused, saying which leg and where. An rtl, the safety command, is never
//! refused for its way home: it passes with a warning. Before rules 17 only the waypoints were
//! judged, so an orbit across a headland passed (the F7-F13 dev check's Magura ran aground).
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::rules;
use murmur_decide::*;

/// The common test land (south of 37.808) and a headland north from it: a peninsula between
/// -122.480 and -122.470 up to 37.830, water on both sides and off its tip.
fn headland() -> PolygonTerrain {
    let b = |w: f64, s: f64, e: f64, n: f64| {
        Polygon::new(vec![
            LonLat::new(w, s),
            LonLat::new(e, s),
            LonLat::new(e, n),
            LonLat::new(w, n),
        ])
    };
    let mut t = terrain();
    t.land.push(b(-122.480, 37.807, -122.470, 37.830));
    t
}

const WEST: LonLat = LonLat::new(-122.4900, 37.8150);
const EAST: LonLat = LonLat::new(-122.4600, 37.8150);

fn rules_version(v: u32) -> RulesConfig {
    let mut c = policy().rules;
    c.rules_version = v;
    c
}

/// The domain check for `p` on `w`, its terrain sampled as the engine samples it under `c`.
fn domain(c: &RulesConfig, p: Proposal, w: &WorldSnapshot) -> CheckResult {
    let s = spec(human(), p);
    let inputs = w.for_intent_with(&s, &headland(), c);
    let mut v = rules::domain(&s, &inputs, c);
    assert_eq!(v.len(), 1, "{v:?}");
    v.remove(0)
}

/// boat-1 (Sea Hunter) at `at`, its home at `home`.
fn boat(at: LonLat, home: LonLat) -> WorldSnapshot {
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("boat-1")).unwrap();
    v.position = Some(Fact::new(at, T0));
    v.home = Some(Fact::new(home, T0));
    w
}

fn patrol(route: &[[f64; 2]], uids: &[&str]) -> Proposal {
    Proposal::new(
        Task::Patrol {
            route: route.iter().map(|p| LonLat::new(p[0], p[1])).collect(),
        },
        uids.iter().map(|u| uid(u)).collect(),
    )
}

#[test]
fn an_orbit_whose_transit_crosses_land_is_refused_and_passed_before_rules_17() {
    let w = boat(WEST, WEST);
    let r = domain(&rules_version(rules::ROUTE_SURFACE_SINCE), orbit(EAST, 200.0, &["boat-1"]), &w);
    assert_eq!(r.outcome, Outcome::Fail, "{r:?}");
    assert!(
        r.detail.starts_with("transit leg (position to waypoint 1) crosses land at (-122.4"),
        "{}",
        r.detail
    );
    assert!(r.detail.contains("m along it; seahunter needs water"), "{}", r.detail);
    // rules 16 judged the waypoints alone, every one on water
    let old = domain(&rules_version(rules::LOST_CONTACT_SINCE), orbit(EAST, 200.0, &["boat-1"]), &w);
    assert_eq!(old.outcome, Outcome::Pass, "{old:?}");
    assert_eq!(old.detail, "all 12 waypoints on water");
}

#[test]
fn a_patrol_out_around_the_headland_and_back_is_accepted() {
    let c = rules_version(rules::ROUTE_SURFACE_SINCE);
    let w = boat(WEST, WEST);
    let around = [
        [-122.4860, 37.8220], // the west side
        [-122.4840, 37.8330], // off the tip, west
        [-122.4660, 37.8330], // off the tip, east
        [-122.4640, 37.8220], // the east side
        [-122.4655, 37.8335], // back round the tip
        [-122.4845, 37.8335],
    ];
    let r = domain(&c, patrol(&around, &["boat-1"]), &w);
    assert_eq!(r.outcome, Outcome::Pass, "{r:?}");
    assert!(
        r.detail.starts_with("all 6 waypoints on water, and the transit, task path, return ("),
        "{}",
        r.detail
    );
    // the same waypoints in another order cut across the headland: its leg 3-4 is refused
    let across = [around[0], around[1], around[2], around[5], around[4], around[3]];
    let r = domain(&c, patrol(&across, &["boat-1"]), &w);
    assert_eq!(r.outcome, Outcome::Fail, "{r:?}");
    assert!(r.detail.starts_with("task path leg 6-1 crosses land at"), "{}", r.detail);
}

#[test]
fn the_way_home_refuses_a_mission_and_only_warns_on_an_rtl() {
    let c = rules_version(rules::ROUTE_SURFACE_SINCE);
    // east of the headland, home to the west of it
    let w = boat(EAST, WEST);
    let r = domain(&c, orbit(LonLat::new(-122.4600, 37.8200), 200.0, &["boat-1"]), &w);
    assert_eq!(r.outcome, Outcome::Fail, "{r:?}");
    assert!(r.detail.starts_with("return leg (waypoint 1 to home) crosses land at"), "{}", r.detail);
    // the rtl, the safety command, goes as ordered, with a warning a person sees
    let r = domain(&c, rtl(&["boat-1"]), &w);
    assert_eq!(r.outcome, Outcome::Pass, "{r:?}");
    assert!(r.detail.contains("the rtl goes as ordered (a safety command)"), "{}", r.detail);
    let warning = r.warning.clone().expect("a warning");
    assert!(warning.starts_with("rtl path crosses land at (-122.4"), "{warning}");
    assert!(warning.ends_with("along the return leg (position to home): send a safer order"), "{warning}");
    assert!(rules::passed(&[r], TaskKind::Rtl));
    // an rtl whose way can't be checked is not evaluated, never refused
    let mut stale = w.clone();
    stale.vehicles.get_mut(&uid("boat-1")).unwrap().position = None;
    let r = domain(&c, rtl(&["boat-1"]), &stale);
    assert_eq!(r.outcome, Outcome::NotEvaluated, "{r:?}");
    assert!(r.detail.starts_with("path not checked (rtl is a safety command)"), "{}", r.detail);
    // a clear way home: no warning
    let r = domain(&c, rtl(&["boat-1"]), &boat(LonLat::new(-122.4900, 37.8250), WEST));
    assert_eq!((r.outcome, r.warning.is_none()), (Outcome::Pass, true), "{r:?}");
    assert!(r.detail.starts_with("the return on water ("), "{}", r.detail);
}

#[test]
fn a_leg_may_leave_a_pier_but_not_cross_land_beyond_it() {
    let c = rules_version(rules::ROUTE_SURFACE_SINCE);
    let east = orbit(LonLat::new(-122.4600, 37.8200), 200.0, &["boat-1"]);
    // 26 m inside the headland's east shore: a berth the land reads ashore; it leaves it
    let berth = LonLat::new(-122.4703, 37.8200);
    let r = domain(&c, east.clone(), &boat(berth, berth));
    assert_eq!(r.outcome, Outcome::Pass, "{r:?}");
    assert!(
        r.detail.contains("the transit leg (position to waypoint 1) starts off water within 40 m of the vehicle"),
        "{}",
        r.detail
    );
    assert!(r.detail.contains("the return leg (waypoint 1 to home) ends off water within 40 m of home"), "{}", r.detail);
    // 61 m inland: the land beyond the first 40 m is a crossing
    let inland = LonLat::new(-122.4707, 37.8200);
    let r = domain(&c, east, &boat(inland, inland));
    assert_eq!(r.outcome, Outcome::Fail, "{r:?}");
    assert!(r.detail.starts_with("transit leg (position to waypoint 1) crosses land at"), "{}", r.detail);
}

#[test]
fn a_ground_vehicle_keeps_to_land_along_its_way() {
    let c = rules_version(rules::ROUTE_SURFACE_SINCE);
    let mut w = world();
    // mule-1 on the headland: south along it to the shore passes, across the water does not
    let on_headland = LonLat::new(-122.4750, 37.8250);
    let v = w.vehicles.get_mut(&uid("mule-1")).unwrap();
    v.position = Some(Fact::new(on_headland, T0));
    v.home = Some(Fact::new(on_headland, T0));
    let r = domain(&c, orbit(LonLat::new(-122.4750, 37.8020), 100.0, &["mule-1"]), &w);
    assert_eq!(r.outcome, Outcome::Pass, "{r:?}");
    let r = domain(&c, orbit(LonLat::new(-122.4500, 37.8000), 100.0, &["mule-1"]), &w);
    assert_eq!(r.outcome, Outcome::Fail, "{r:?}");
    assert!(r.detail.contains("crosses water at"), "{}", r.detail);
    assert!(r.detail.ends_with("ugv_logistics needs land"), "{}", r.detail);
}

/// The answers along the legs are logged one letter a point, and the rule reads only them: a log
/// whose letters say water where the land is would be judged as they say (replay recomputes the
/// check from the logged inputs), and a leg without its answers can't be judged.
#[test]
fn the_rule_reads_the_logged_answers_along_each_leg() {
    let c = rules_version(rules::ROUTE_SURFACE_SINCE);
    let s = spec(human(), orbit(EAST, 200.0, &["boat-1"]));
    let mut inputs = boat(WEST, WEST).for_intent_with(&s, &headland(), &c);
    let world::TerrainInputs::Sampled { along, .. } = &mut inputs.terrain else {
        panic!("sampled: {:?}", inputs.terrain);
    };
    assert_eq!(along.len(), 14, "transit, 12 ring legs, return");
    assert!(along[0].surfaces.contains('l') && along[0].near_m.is_none(), "{:?}", along[0]);
    along[0].surfaces = along[0].surfaces.replace('l', "w");
    let r = rules::domain(&s, &inputs, &c).remove(0);
    // the return leg is the same way back
    assert!(r.detail.starts_with("return leg (waypoint 1 to home) crosses land"), "{r:?}");
    let world::TerrainInputs::Sampled { along, .. } = &mut inputs.terrain else { unreachable!() };
    along.remove(0);
    let r = rules::domain(&s, &inputs, &c).remove(0);
    assert_eq!(r.outcome, Outcome::Fail);
    assert_eq!(r.detail, "cannot evaluate: no terrain answers along the transit leg (position to waypoint 1)");
}
