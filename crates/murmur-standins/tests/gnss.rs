//! GNSS availability end to end: real constellation geometry (murmur-space's element-set
//! fixture), and jamming and spoofing as scenario events driving each firmware's EKF and
//! failsafe, stepped faster than real time. What a vehicle says is read from its link
//! (`Link::capture`), not from a loopback socket, whose delivery lags such a loop and drops
//! datagrams on a loaded host (bd murmur-ek3); every vehicle draws from a fixed seed.

use std::sync::Arc;

use mavlink::MessageData;
use mavlink::dialects::ardupilotmega::*;
use mavlink::peek_reader::PeekReader;
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::geo::{M_PER_DEG, distance_m};
use murmur_standins::gnss::{self, Env, SkyTable};
use murmur_standins::nav::Ekf;
use murmur_standins::telemetry;
use murmur_standins::vehicle::Vehicle;
use tokio::net::UdpSocket;

const SYSID: u8 = 7;
const HOME: (f64, f64) = (37.8, -122.6);

fn spec(behavior: &str, cruise: f64) -> Spec {
    Spec {
        name: format!("test {behavior}"),
        behavior: behavior.into(),
        cruise_mps: cruise,
        max_mps: cruise * 1.2,
        climb_mps: Some(2.0),
        turn_dps: Some(10.0),
        loiter_radius_m: Some(300.0),
        endurance_h: Some(10.0),
        link: None,
    }
}

fn config(spec: Spec, port: u16, alt: f64) -> VehicleConfig {
    VehicleConfig {
        uid: Some(1),
        sysid: u32::from(SYSID),
        callsign: "TEST 1".into(),
        port,
        home: [HOME.0, HOME.1, alt, 0.0],
        spec,
        params: Default::default(),
        surface_m: None,
    }
}

/// A vehicle to step by hand, what it sends kept on its link ([`texts`]), its socket going
/// to one nobody reads (kept open beside it).
async fn offline(spec: Spec, alt: f64, events: &str) -> (Vehicle, std::net::UdpSocket) {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    let mut v = Vehicle::seeded(config(spec, port, alt), sock, None, 1).expect("vehicle");
    v.link.capture();
    v.gnss = Some(Arc::new(Env {
        events: gnss::parse_events(events).expect("events"),
        ..Env::plain()
    }));
    (v, sink)
}

/// STATUSTEXTs the vehicle sent since the last call, in order.
fn texts(v: &mut Vehicle) -> Vec<String> {
    let mut out = Vec::new();
    for frame in v.link.take_captured() {
        let mut r = PeekReader::new(&frame[..]);
        if let Ok((_, MavMessage::STATUSTEXT(t))) = mavlink::read_v2_msg::<MavMessage, _>(&mut r) {
            out.push(t.text.to_str().unwrap_or("").to_owned());
        }
    }
    out
}

fn run(v: &mut Vehicle, seconds: f64, log: &mut Vec<String>) {
    for _ in 0..(seconds * 10.0).round() as usize {
        v.step();
        log.extend(texts(v));
    }
}

fn gps_raw(v: &Vehicle) -> GPS_RAW_INT_DATA {
    match telemetry::build(v, GPS_RAW_INT_DATA::ID) {
        Some(MavMessage::GPS_RAW_INT(g)) => g,
        _ => panic!("no GPS_RAW_INT"),
    }
}

fn north(lat_m: f64) -> f64 {
    HOME.0 + lat_m / M_PER_DEG
}

fn cmd(v: &mut Vehicle, command: MavCmd, p1: f32, p7: f32) {
    v.handle(&MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        target_system: SYSID,
        target_component: 1,
        command,
        param1: p1,
        param7: p7,
        ..Default::default()
    }));
}

fn waypoints(v: &mut Vehicle, north_m: &[f64], alt: f32) {
    let items = std::iter::once(0.0)
        .chain(north_m.iter().copied())
        .enumerate()
        .map(|(seq, n)| MISSION_ITEM_INT_DATA {
            seq: seq as u16,
            command: MavCmd::MAV_CMD_NAV_WAYPOINT,
            frame: MavFrame::MAV_FRAME_GLOBAL,
            x: (north(n) * 1e7) as i32,
            y: (HOME.1 * 1e7) as i32,
            z: alt,
            ..Default::default()
        })
        .collect();
    v.mission.install(items);
}

const DENY_HERE: &str = r#"
    [[event]]
    name = "jammer"
    at_s = 40
    duration_s = 60
    circle = { lat = 37.8, lon = -122.6, radius_m = 5000 }
    loss = 0.0
    gnss = { effect = "jam" }
"#;

/// ArduCopter: position lost 7 s after the fix (posRetryTimeNoVel), EKF failsafe 1 s later
/// -> LAND; constant position mode at 15 s; the fix back after the jammer clears it all.
#[tokio::test]
async fn copter_lands_when_jammed_and_recovers_after() {
    let (mut v, _sink) = offline(spec("copter", 10.0), 0.0, DENY_HERE).await;
    let mut log = Vec::new();
    v.set_mode("GUIDED");
    cmd(&mut v, MavCmd::MAV_CMD_COMPONENT_ARM_DISARM, 1.0, 0.0);
    cmd(&mut v, MavCmd::MAV_CMD_NAV_TAKEOFF, 0.0, 40.0);
    run(&mut v, 39.0, &mut log);
    assert!(v.airborne(), "{log:?}");
    assert_eq!(v.st.mode, "GUIDED");
    assert!(v.nav.fix && v.nav.ekf == Ekf::Gps);
    assert_eq!(gps_raw(&v).fix_type, GpsFixType::GPS_FIX_TYPE_3D_FIX);
    assert_eq!(
        gps_raw(&v).satellites_visible,
        14,
        "no geometry cache: as before"
    );

    // the jammer comes on at 40 s
    run(&mut v, 2.0, &mut log);
    let g = gps_raw(&v);
    assert_eq!(g.fix_type, GpsFixType::GPS_FIX_TYPE_NO_FIX);
    assert_eq!((g.eph, g.satellites_visible), (u16::MAX, 0));
    let flags = telemetry::ekf_flags(&v);
    assert!(
        flags.contains(EkfStatusFlags::EKF_POS_HORIZ_ABS),
        "EKF3 still claims absolute position before posTimeout"
    );
    run(&mut v, 8.0, &mut log); // 49 s
    let flags = telemetry::ekf_flags(&v);
    assert!(!flags.contains(EkfStatusFlags::EKF_POS_HORIZ_ABS));
    assert!(!flags.contains(EkfStatusFlags::EKF_POS_HORIZ_REL));
    assert_eq!(v.st.mode, "LAND", "{log:?}");
    assert!(
        log.iter().any(|t| t == "EKF variance: position lost"),
        "{log:?}"
    );
    assert!(
        log.iter()
            .any(|t| t == "EKF Failsafe: changed to LAND Mode")
    );
    run(&mut v, 7.0, &mut log); // 56 s
    assert_eq!(v.nav.ekf, Ekf::ConstPos);
    assert!(telemetry::ekf_flags(&v).contains(EkfStatusFlags::EKF_CONST_POS_MODE));
    assert!(log.iter().any(|t| t == "EKF3 IMU0 stopped aiding"));
    // reported and true positions part: it drifts with wind it can no longer see
    run(&mut v, 40.0, &mut log); // 97 s
    let err = v.nav.error_m();
    assert!(err > 5.0, "{err:.1} m");
    let (tlat, tlon, _) = v.truth();
    assert!((distance_m(tlat, tlon, v.st.lat, v.st.lon) - err).abs() < 0.5);

    // the jammer goes at 100 s; the fix is back 10 s later
    run(&mut v, 15.0, &mut log); // 112 s
    assert!(v.nav.fix && v.nav.ekf == Ekf::Gps);
    assert_eq!(v.nav.error_m(), 0.0);
    assert!(log.iter().any(|t| t == "EKF3 IMU0 is using GPS"));
    run(&mut v, 3.0, &mut log);
    assert!(log.iter().any(|t| t == "EKF Failsafe Cleared"), "{log:?}");
    assert_eq!(v.st.mode, "LAND", "ArduCopter stays in LAND");
}

/// ArduRover (boat frame) in AUTO: "EKF variance", HOLD, "EKF failsafe".
#[tokio::test]
async fn boat_holds_when_jammed() {
    let (mut v, _sink) = offline(spec("boat", 3.0), 0.0, DENY_HERE).await;
    let mut log = Vec::new();
    waypoints(&mut v, &[2000.0], 0.0);
    v.st.armed = true;
    v.set_mode("AUTO");
    run(&mut v, 39.0, &mut log);
    assert_eq!(v.st.mode, "AUTO");
    run(&mut v, 10.0, &mut log);
    assert_eq!(v.st.mode, "HOLD", "{log:?}");
    assert!(log.iter().any(|t| t == "EKF variance"), "{log:?}");
    assert!(log.iter().any(|t| t == "EKF failsafe"));
    assert!(v.failsafe_active(), "HEARTBEAT says CRITICAL");
    run(&mut v, 75.0, &mut log);
    assert!(log.iter().any(|t| t == "EKF failsafe cleared"), "{log:?}");
    assert_eq!(v.st.mode, "HOLD");
}

/// ArduPlane flies on in AUTO through a jammer, dead reckoning on airspeed (relative
/// position only); the estimate drifts from the truth and snaps back after leaving it.
#[tokio::test]
async fn plane_dead_reckons_through_a_jammer() {
    // a powered jammer 15 km up the track, on a 300 m hill
    let events = r#"
        [[event]]
        name = "hill jammer"
        at_s = 0
        circle = { lat = 37.935, lon = -122.6, radius_m = 30000 }
        gnss = { effect = "jam", power_w = 0.25, height_m = 300 }
    "#;
    let (mut v, _sink) = offline(spec("plane", 40.0), 1000.0, events).await;
    let mut log = Vec::new();
    waypoints(&mut v, &[40_000.0], 1000.0);
    v.set_mode("AUTO");
    let mut seen = Vec::new();
    let mut worst: f64 = 0.0;
    for _ in 0..(1100 * 10) {
        v.step();
        log.extend(texts(&mut v));
        if seen.last() != Some(&v.nav.ekf) {
            seen.push(v.nav.ekf);
        }
        worst = worst.max(v.nav.error_m());
        if v.nav.ekf == Ekf::DeadReckoning {
            let f = telemetry::ekf_flags(&v);
            assert!(f.contains(EkfStatusFlags::EKF_POS_HORIZ_REL));
            assert!(!f.contains(EkfStatusFlags::EKF_POS_HORIZ_ABS));
            assert_eq!(v.st.mode, "AUTO", "ArduPlane flies on");
        }
    }
    assert_eq!(
        seen,
        vec![Ekf::Gps, Ekf::Coasting, Ekf::DeadReckoning, Ekf::Gps],
        "{log:?}"
    );
    assert!(
        worst > 10.0,
        "error grew while dead reckoning: {worst:.1} m"
    );
    assert_eq!(v.nav.error_m(), 0.0, "back on GPS");
    assert!(!log.iter().any(|t| t.starts_with("EKF Failsafe")));
}

/// Spoofing: the reported position walks off while every flag stays good; only the truth
/// log (or a cross-check) shows it. The boat parks its spoofed estimate on the waypoint, so
/// it really sits the spoofer's offset away from it.
#[tokio::test]
async fn spoofing_drags_a_boat_off_its_waypoint_with_good_flags() {
    let events = r#"
        [[event]]
        name = "spoofer"
        at_s = 10
        duration_s = 300
        circle = { lat = 37.8, lon = -122.6, radius_m = 5000 }
        gnss = { effect = "spoof", walk_mps = 1.0, bearing_deg = 90, max_offset_m = 150 }
    "#;
    let (mut v, _sink) = offline(spec("boat", 3.0), 0.0, events).await;
    let mut log = Vec::new();
    waypoints(&mut v, &[400.0], 0.0);
    v.st.armed = true;
    v.set_mode("AUTO");
    run(&mut v, 250.0, &mut log);
    assert_eq!(v.nav.ekf, Ekf::Gps);
    assert!(telemetry::ekf_flags(&v).contains(EkfStatusFlags::EKF_POS_HORIZ_ABS));
    assert_eq!(gps_raw(&v).fix_type, GpsFixType::GPS_FIX_TYPE_3D_FIX);
    assert!((v.nav.err_e - 150.0).abs() < 1.0, "{}", v.nav.err_e);
    let wp = (north(400.0), HOME.1);
    let reported = distance_m(v.st.lat, v.st.lon, wp.0, wp.1);
    let (tlat, tlon, _) = v.truth();
    let real = distance_m(tlat, tlon, wp.0, wp.1);
    assert!(reported < 30.0, "it thinks it is there: {reported:.0} m");
    assert!((real - 150.0).abs() < 30.0, "it is not: {real:.0} m");
    assert!(tlon < wp.1, "west of it");
    assert!(
        !log.iter()
            .any(|t| t.contains("EKF") || t.contains("GPS") || t.contains("ailsafe")),
        "no warning at all: {log:?}"
    );
    // the spoofer stops at 310 s: the EKF rejects the 150 m jump for 10 s, then resets
    run(&mut v, 65.0, &mut log);
    assert_eq!(v.nav.ekf, Ekf::Glitch);
    assert!(telemetry::ekf_flags(&v).contains(EkfStatusFlags::EKF_GPS_GLITCHING));
    run(&mut v, 10.0, &mut log);
    assert_eq!(v.nav.ekf, Ekf::Gps);
    assert_eq!(v.nav.error_m(), 0.0);
}

/// Geometry from real element sets: the count and DOP change with time and place.
#[test]
fn geometry_varies_with_time_and_position() {
    let fixture = include_str!("../../murmur-space/tests/data/gp_fixture.json");
    let mut catalog =
        murmur_space::Catalog::parse(include_str!("../../../sim/space/constellations.toml"))
            .expect("catalog");
    catalog.classes.retain(|c| c.id == "gps");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, fixture.to_string()))
        .collect();
    let space = murmur_space::Space::from_sources(catalog, &sources).expect("space");
    // 2026-10-02T00:00Z, the fixture's epoch
    let t0 = 1_790_812_800.0;
    let table = SkyTable::covering(&[HOME, (-33.9, 151.2)], t0);
    let block = gnss::compute_block(&space, None, &table, 0).expect("block");
    table.put_block(0, block);
    let mut counts = std::collections::BTreeSet::new();
    let mut hdops = Vec::new();
    for k in 0..120 {
        let t = t0 + f64::from(k) * gnss::TIME_STEP_S;
        let s = table.at(HOME.0, HOME.1, t).expect("sky");
        counts.insert(s.visible);
        hdops.push(s.hdop.unwrap_or(f32::NAN));
        assert!(s.visible >= 5, "{s:?}");
    }
    assert!(
        counts.len() >= 2,
        "visible count changes over an hour: {counts:?}"
    );
    let (lo, hi) = hdops
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &h| (a.min(h), b.max(h)));
    assert!(hi - lo > 0.05 && hi < 3.0, "{lo} {hi}");
    let here = table.at(HOME.0, HOME.1, t0).expect("sky");
    let sydney = table.at(-33.9, 151.2, t0).expect("sky");
    assert_ne!(here, sydney, "another sky on the other side of the world");
}
