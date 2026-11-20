//! Rules 34, stage 5 (docs/SWARMS.md section 13; bd murmur-4nik, murmur-3kqi faults 1 and 2): the
//! task layer flies what was logged. A group order of stand-ins of one kind goes through the
//! bridge (submitted, approved at submission, dispatched as its member orders) and each stand-in's
//! own autopilot is asked what it holds: its mission at its share's altitude (never the task
//! layer's stack of one altitude step per vehicle), entering its ring where its share's phase says
//! (a fixed wing too: a waypoint ring of its own, never one loiter shared by the group), or its
//! route as far along as its share's entry says.

use std::time::Duration;

use murmur_decide::{GROUP_ORDER_SINCE, IntentId, LonLat};
use tokio::time::sleep;

use super::lost_link::{AIR, LINKS, SURFACE, Wired, lock, rig_rules, wire};
use super::*;
use crate::mav::{self, apm};

/// The stand-ins' system id on their links (lost_link's).
const SYSID: u8 = 7;

/// Its status, GPS, navigation filter and position streamed (the world's telemetry plan asks
/// for them on a running bridge).
fn stream(w: &Wired) {
    for id in [1.0, 24.0, 33.0, 193.0] {
        let p = [id, 500_000.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        w.cell.send(&mav::command_long(SYSID, apm::MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL, 0, p));
    }
}

/// One mission item as the stand-in holds it: its command, [lat, lon] and altitude.
type Held = (apm::MavCmd, [f64; 2], f64);

/// The mission the stand-in holds, and the DO_JUMP's target (its loop's first item).
fn held(w: &Wired) -> (Vec<Held>, Option<usize>) {
    let s = lock(&w.standin);
    let items: Vec<_> = s
        .mission
        .items
        .iter()
        .map(|it| (it.command, [f64::from(it.x) / 1e7, f64::from(it.y) / 1e7], f64::from(it.z)))
        .collect();
    let jump = s
        .mission
        .items
        .iter()
        .find(|it| it.command == apm::MavCmd::MAV_CMD_DO_JUMP)
        .map(|it| it.param1 as usize);
    (items, jump)
}

/// Bearing from `c` to `p`, degrees clockwise from north (flat, as the layout measures it).
fn bearing(c: LonLat, p: [f64; 2]) -> f64 {
    let x = (p[1] - c.lon) * c.lat.to_radians().cos();
    let y = p[0] - c.lat;
    x.atan2(y).to_degrees().rem_euclid(360.0)
}

/// `n` stand-ins of `platform` wired to a rig deciding under rules 34, near the sea station at
/// `alt` m, streaming.
async fn fleet(rig: &Rig, platform: &str, n: u64, alt: f64) -> Vec<Wired> {
    let mut out = Vec::new();
    for k in 0..n {
        let at = [37.825 + 0.0006 * k as f64, -122.47, alt, 90.0];
        let w = wire(rig, 41 + k, platform, LINKS, at).await;
        stream(&w);
        out.push(w);
    }
    sleep(Duration::from_secs(3)).await;
    out
}

/// Submit `req` as op-1 (approved at submission), dispatched as its member orders; each
/// member's share and member task, by bridge uid.
async fn dispatched(rig: &Rig, req: IntentRequest) -> (IntentId, BTreeMap<Uid, (murmur_decide::Task, murmur_decide::group::Share)>) {
    let out = rig.d.submit(&op1(), req).await.expect("submitted");
    assert_eq!(out.state, State::Dispatched, "{}", out.intent);
    let core = rig.d.lock();
    let shares = core
        .engine
        .intents()
        .filter(|i| i.spec().parent() == Some(out.id))
        .filter_map(|i| {
            let u = snapshot::bridge_uid(i.spec().uids.first()?)?;
            Some((u, (i.spec().task.clone(), i.spec().share()?.clone())))
        })
        .collect();
    (out.id, shares)
}

/// Wait (tokio time) until every stand-in holds a mission with a loop.
async fn missions(ws: &[Wired], limit_s: u64) {
    for _ in 0..limit_s {
        if ws.iter().all(|w| held(w).1.is_some()) {
            return;
        }
        sleep(Duration::from_secs(1)).await;
    }
    panic!("not every stand-in holds its mission after {limit_s} s");
}

fn orbit_req(uids: &[Uid], center: LonLat, radius_m: f64) -> IntentRequest {
    let mut r = simple("orbit", uids[0]);
    r.uids = uids.to_vec();
    r.params.center = Some([center.lon, center.lat]);
    r.params.radius = Some(radius_m);
    r
}

/// Each stand-in's ring: its waypoints (the loop) at its share's altitude, the first where its
/// share's phase says.
fn check_ring(ws: &[Wired], shares: &BTreeMap<Uid, (murmur_decide::Task, murmur_decide::group::Share)>, center: LonLat, what: &str) {
    for w in ws {
        let (task, share) = shares.get(&w.uid).unwrap_or_else(|| panic!("{what}: {} has no member order", w.uid));
        let (items, jump) = held(w);
        let from = jump.expect("a loop");
        let ring: Vec<&Held> = items[from..].iter().filter(|i| i.0 == apm::MavCmd::MAV_CMD_NAV_WAYPOINT).collect();
        assert_eq!(ring.len(), 12, "{what} {}: a ring of 12 waypoints: {items:?}", w.uid);
        assert!(
            !items.iter().any(|i| i.0 == apm::MavCmd::MAV_CMD_NAV_LOITER_UNLIM),
            "{what} {}: no loiter shared by the group",
            w.uid
        );
        let want_alt = share.alt_m.unwrap_or(0.0);
        for (_, _, z) in &ring {
            assert!((z - want_alt).abs() < 0.1, "{what} {}: flies {z} m, its share {want_alt} m", w.uid);
        }
        let phase = share.phase_deg.expect("a ring share's phase");
        let b = bearing(center, ring[0].1);
        let off = (b - phase).rem_euclid(360.0).min((phase - b).rem_euclid(360.0));
        assert!(off < 1.0, "{what} {}: enters the ring at {b:.1} degrees, its share {phase} ({task:?})", w.uid);
    }
}

/// The task layer flies each member's share as logged, per kind it flies: a quadcopter's ring
/// (its share's altitude and phase), a fixed wing's (a waypoint ring of its own, not the group's
/// one loiter), and a boat's patrol entered as far along its route as its share says (a hold and
/// an rtl have no share to fly: each member holds where it is, or goes to its own home). A sub's
/// share is an altitude as an aircraft's, negative; the rig's 12 m of water holds no AUV layer
/// (the depth rule leaves both out), so no sub is flown here.
#[tokio::test(start_paused = true)]
async fn each_member_flies_its_share_as_logged() {
    // quadcopters: three X10s round a 200 m ring
    let rig = rig_rules(GROUP_ORDER_SINCE);
    let ws = fleet(&rig, "x10", 3, AIR).await;
    let center = LonLat::new(-122.4720, 37.8270);
    let uids: Vec<Uid> = ws.iter().map(|w| w.uid).collect();
    let (_, shares) = dispatched(&rig, orbit_req(&uids, center, 200.0)).await;
    assert_eq!(shares.len(), 3, "three member orders");
    missions(&ws, 60).await;
    check_ring(&ws, &shares, center, "x10");
    // fixed wings: three Pumas round a 700 m ring
    let rig = rig_rules(GROUP_ORDER_SINCE);
    let ws = fleet(&rig, "puma", 3, AIR).await;
    let center = LonLat::new(-122.4800, 37.8300);
    let uids: Vec<Uid> = ws.iter().map(|w| w.uid).collect();
    let (_, shares) = dispatched(&rig, orbit_req(&uids, center, 700.0)).await;
    assert_eq!(shares.len(), 3);
    missions(&ws, 60).await;
    check_ring(&ws, &shares, center, "puma");
    // boats: three Magura patrol a triangle, each entering its share's distance along it
    let rig = rig_rules(GROUP_ORDER_SINCE);
    let ws = fleet(&rig, "magura", 3, SURFACE).await;
    let uids: Vec<Uid> = ws.iter().map(|w| w.uid).collect();
    let route = vec![[-122.4740, 37.8260], [-122.4680, 37.8260], [-122.4710, 37.8300]];
    let mut req = simple("patrol", uids[0]);
    req.uids = uids.clone();
    req.params.route = Some(route);
    let (_, shares) = dispatched(&rig, req).await;
    assert_eq!(shares.len(), 3);
    missions(&ws, 60).await;
    for w in &ws {
        let (task, share) = shares.get(&w.uid).expect("a member order");
        let murmur_decide::Task::Patrol { route } = task else { panic!("{task:?}") };
        let entry = share.entry_m.expect("a route share's entry");
        let want = murmur_decide::group::entered_route(route, entry);
        let (items, jump) = held(w);
        let first = items[jump.expect("a loop")].1;
        let at = share.at.expect("its slot's point");
        assert!((first[0] - want[0].lat).abs() < 2e-7 && (first[1] - want[0].lon).abs() < 2e-7, "{}: enters at {first:?}, want {:?}", w.uid, want[0]);
        assert!((first[0] - at.lat).abs() < 2e-6 && (first[1] - at.lon).abs() < 2e-6, "{}: the entry is its slot's point {at:?}", w.uid);
        let loop_n = items[jump.unwrap_or(0)..].iter().filter(|i| i.0 == apm::MavCmd::MAV_CMD_NAV_WAYPOINT).count();
        assert_eq!(loop_n, want.len(), "{}: the whole loop from its entry", w.uid);
        assert!(items.iter().all(|i| i.0 != apm::MavCmd::MAV_CMD_NAV_WAYPOINT || i.2.abs() < 0.1), "on the surface");
    }
}

/// Under rules 33 the task layer stacks same-kind vehicles of one order as before (murmur-3kqi's
/// fault 1, kept for older runs): the second X10 of one orbit flies one altitude step higher;
/// under rules 34 the same order is a group whose members fly their shares, the task layer
/// stacking nothing.
#[tokio::test(start_paused = true)]
async fn rules_33_stacks_as_before_and_rules_34_never() {
    let rig = rig_rules(GROUP_ORDER_SINCE - 1);
    let ws = fleet(&rig, "x10", 2, AIR).await;
    let uids: Vec<Uid> = ws.iter().map(|w| w.uid).collect();
    let out = rig.d.submit(&op1(), orbit_req(&uids, LonLat::new(-122.4720, 37.8270), 200.0)).await.expect("submitted");
    assert_eq!(out.state, State::Dispatched);
    assert!(rig.d.lock().engine.intents().all(|i| i.spec().parent().is_none()), "one intent, as before");
    missions(&ws, 60).await;
    let alts: Vec<f64> = ws.iter().map(|w| held(w).0.last().map_or(0.0, |i| i.2)).collect();
    let rings: Vec<f64> = ws
        .iter()
        .map(|w| {
            let (items, jump) = held(w);
            items[jump.unwrap_or(0)].2
        })
        .collect();
    assert!((rings[1] - rings[0]).abs() >= 4.9, "stacked apart under rules 33: {rings:?} ({alts:?})");
    let rig = rig_rules(GROUP_ORDER_SINCE);
    let ws = fleet(&rig, "x10", 2, AIR).await;
    let uids: Vec<Uid> = ws.iter().map(|w| w.uid).collect();
    let (_, shares) = dispatched(&rig, orbit_req(&uids, LonLat::new(-122.4720, 37.8270), 200.0)).await;
    missions(&ws, 60).await;
    for w in &ws {
        let (items, jump) = held(w);
        let want = shares[&w.uid].1.alt_m.unwrap_or(0.0);
        assert!((items[jump.unwrap_or(0)].2 - want).abs() < 0.1, "its share's altitude, nothing stacked");
    }
}

// ---- transit (stage 7) -------------------------------------------------------------------------

/// Metres between two points (flat; the tests' distances are short).
fn metres(a: (f64, f64), b: (f64, f64)) -> f64 {
    let x = (a.1 - b.1) * 111_320.0 * a.0.to_radians().cos();
    let y = (a.0 - b.0) * 111_320.0;
    x.hypot(y)
}

/// Rules 34, stage 7: a transit flies its route once and holds where it ends, by the vehicle's
/// type, each stand-in's own autopilot doing the holding: a quadcopter hovers on the point, a
/// fixed wing loiters round it on its loiter circle, a boat and an AUV hold station on it, a
/// ground vehicle stops on it, and a blimp (whose firmware skips the hold item) holds at its last
/// waypoint as its mission's end does. Each mission ends with the hold at the route's last point;
/// the order is done there, and a minute and a half later each one still holds there, in its
/// mission mode.
#[tokio::test(start_paused = true)]
async fn a_transit_holds_where_its_route_ends_by_type() {
    struct Case {
        platform: &'static str,
        at: [f64; 4],
        /// the route, [lon, lat]
        route: [[f64; 2]; 2],
        /// how far from the end it holds: at most (and, for a fixed wing's circle, at least)
        within_m: f64,
        circles: bool,
        /// still once it holds (a quadcopter, a ground vehicle)
        still: bool,
    }
    let (lat, lon) = (37.825, -122.47);
    let water = [[lon - 0.0030, lat + 0.0010], [lon - 0.0050, lat + 0.0030]];
    let cases = [
        Case { platform: "x10", at: [lat, lon, AIR, 90.0], route: water, within_m: 5.0, circles: false, still: true },
        Case { platform: "puma", at: [lat, lon, AIR, 90.0], route: water, within_m: 230.0, circles: true, still: false },
        Case { platform: "corsair", at: [lat, lon, SURFACE, 90.0], route: water, within_m: 15.0, circles: false, still: false },
        Case { platform: "remus", at: [lat, lon, SURFACE, 90.0], route: water, within_m: 20.0, circles: false, still: false },
        Case { platform: "lta", at: [lat, lon, AIR, 90.0], route: water, within_m: 30.0, circles: false, still: false },
        // on the rig's land, the Presidio's box
        Case { platform: "ugv_recon", at: [37.800, -122.460, SURFACE, 90.0], route: [[-122.4615, 37.8010], [-122.4630, 37.8025]], within_m: 10.0, circles: false, still: true },
    ];
    for (i, c) in cases.iter().enumerate() {
        let rig = rig_rules(GROUP_ORDER_SINCE);
        let w = wire(&rig, 60 + i as Uid, c.platform, LINKS, c.at).await;
        stream(&w);
        sleep(Duration::from_secs(3)).await;
        let mut req = simple("transit", w.uid);
        req.params.route = Some(c.route.to_vec());
        let out = rig.d.submit(&op1(), req).await.unwrap_or_else(|e| panic!("{}: {e:?}", c.platform));
        assert_eq!(out.state, State::Dispatched, "{}: {}", c.platform, out.intent);
        let id = out.id;
        let end = (c.route[1][1], c.route[1][0]);
        let mut done = false;
        for _ in 0..600 {
            rig.d.tick();
            sleep(Duration::from_secs(1)).await;
            if rig.d.lock().engine.intent(id).is_some_and(|i| i.state() == State::Done) {
                done = true;
                break;
            }
        }
        let (items, jump) = held(&w);
        assert!(done, "{}: the transit never ended done; it holds {items:?}", c.platform);
        assert!(jump.is_none(), "{}: a transit never loops", c.platform);
        let last = items.last().expect("a mission");
        assert_eq!(last.0, apm::MavCmd::MAV_CMD_NAV_LOITER_UNLIM, "{}: it ends holding", c.platform);
        assert!(metres((last.1[0], last.1[1]), end) < 0.05, "{}: its hold is the route's last point", c.platform);
        // a minute and a half on (a boat or an AUV overshoots and comes back, a fixed wing settles
        // on its circle), it holds there, in its mission mode
        for _ in 0..90 {
            rig.d.tick();
            sleep(Duration::from_secs(1)).await;
        }
        let (pos, mode, speed) = {
            let s = lock(&w.standin);
            ((s.st.lat, s.st.lon), s.st.mode, s.st.speed)
        };
        let d = metres(pos, end);
        assert!(d <= c.within_m, "{}: {d:.0} m from the route's end, holding within {} m", c.platform, c.within_m);
        if c.circles {
            assert!((d - 150.0).abs() < 10.0 && speed > 5.0, "{}: a fixed wing circles its hold on its 150 m loiter circle, {d:.0} m out at {speed:.1} m/s", c.platform);
        }
        if c.still {
            assert!(speed < 0.5, "{}: still on its hold, {speed:.1} m/s", c.platform);
        }
        assert_eq!(mode, "AUTO", "{}: still in its mission mode, holding", c.platform);
        assert_eq!(rig.d.lock().engine.intent(id).map(|i| i.state()), Some(State::Done), "{}", c.platform);
    }
}

/// Rules 34: a group transit through the bridge, each member's own order flown as its member
/// order says: three quadcopters each straight to its own station along a line, at its share's
/// altitude, holding there; and an agent proposes no transit yet (the operator orders one).
#[tokio::test(start_paused = true)]
async fn a_group_transit_flies_each_member_to_its_station() {
    let rig = rig_rules(GROUP_ORDER_SINCE);
    let ws = fleet(&rig, "x10", 3, AIR).await;
    let uids: Vec<Uid> = ws.iter().map(|w| w.uid).collect();
    let route = vec![[-122.4660, 37.8270], [-122.4600, 37.8270]];
    let mut req = simple("transit", uids[0]);
    req.uids = uids.clone();
    req.params.route = Some(route.clone());
    let (gid, shares) = dispatched(&rig, req.clone()).await;
    assert_eq!(shares.len(), 3);
    for _ in 0..240 {
        rig.d.tick();
        sleep(Duration::from_secs(1)).await;
        if rig.d.lock().engine.intent(gid).is_some_and(|i| i.state() == State::Done) {
            break;
        }
    }
    assert_eq!(rig.d.lock().engine.intent(gid).map(|i| i.state()), Some(State::Done), "every member holds at its station");
    for w in &ws {
        let (task, share) = &shares[&w.uid];
        let murmur_decide::Task::Transit { route: mine } = task else { panic!("{task:?}") };
        let at = share.at.expect("its station");
        assert_eq!(mine.len(), 1, "straight to its station");
        let (items, _) = held(w);
        let last = items.last().expect("a mission");
        assert_eq!(last.0, apm::MavCmd::MAV_CMD_NAV_LOITER_UNLIM);
        assert!((last.2 - share.alt_m.unwrap_or(0.0)).abs() < 0.1, "{}: its share's altitude", w.uid);
        let pos = {
            let s = lock(&w.standin);
            (s.st.lat, s.st.lon)
        };
        assert!(metres(pos, (at.lat, at.lon)) < 5.0, "{}: holds at its station", w.uid);
    }
    let agent = murmur_decide::AgentId::new("agent-planner");
    req.rationale = Some("move the picket".into());
    let e = rig.d.agent_propose(&agent, req).await.expect_err("an agent's transit");
    assert_eq!(e.status, 422, "{}", e.detail);
}
