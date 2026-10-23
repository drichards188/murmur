//! A hull tied up at its pier (`VehicleConfig::moored`, a fleet unit's `moored`): the current
//! does not carry it off until it first arms, then it drifts like any other; a hull that is not
//! moored drifts from the start, as before.

use std::sync::Arc;

use mavlink::dialects::ardupilotmega::*;
use murmur_standins::conditions::{Sample, Slot};
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::geo::distance_m;
use murmur_standins::vehicle::Vehicle;
use tokio::net::UdpSocket;

const SYSID: u8 = 7;
const HOME: (f64, f64) = (37.83, -122.476);

fn boat(moored: bool, port: u16) -> VehicleConfig {
    VehicleConfig {
        uid: Some(1),
        sysid: u32::from(SYSID),
        callsign: "TEST 1".into(),
        port,
        home: [HOME.0, HOME.1, 0.0, 90.0],
        spec: Spec {
            name: "test boat".into(),
            behavior: "boat".into(),
            cruise_mps: 6.0,
            max_mps: 8.0,
            climb_mps: None,
            turn_dps: Some(10.0),
            loiter_radius_m: None,
            endurance_h: Some(10.0),
            link: None,
        },
        params: Default::default(),
        surface_m: None,
        moored,
    }
}

/// 20 m of water and a 1 m/s current setting east, whatever the position.
fn water() -> Arc<Slot> {
    let s = Arc::new(Slot::default());
    s.put(Sample {
        lat: HOME.0,
        lon: HOME.1,
        elevation_m: Some(-20.0),
        water_level_m: Some(0.0),
        water_depth_m: Some(20.0),
        current_east: Some(1.0),
        current_north: Some(0.0),
        wind_east: None,
        wind_north: None,
    });
    s
}

async fn vehicle(moored: bool) -> (Vehicle, std::net::UdpSocket) {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    let mut v = Vehicle::seeded(boat(moored, port), sock, Some(water()), 1).expect("vehicle");
    v.link.capture();
    (v, sink)
}

fn run(v: &mut Vehicle, seconds: f64) {
    for _ in 0..(seconds * 10.0).round() as usize {
        v.step();
        v.link.take_captured();
    }
}

fn moved_m(v: &Vehicle) -> f64 {
    let (lat, lon, _) = v.truth();
    distance_m(HOME.0, HOME.1, lat, lon)
}

#[tokio::test]
async fn a_moored_hull_stays_until_it_casts_off() {
    let (mut free, _a) = vehicle(false).await;
    run(&mut free, 60.0);
    assert!((50.0..70.0).contains(&moved_m(&free)), "not moored: drifts about 60 m, {}", moved_m(&free));

    let (mut tied, _b) = vehicle(true).await;
    run(&mut tied, 60.0);
    assert!(moved_m(&tied) < 1.0, "moored: stays, {}", moved_m(&tied));
    assert!(tied.moored);
    // it arms (casts off) and is disarmed again: from then on the current carries it
    tied.handle(&MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        target_system: SYSID,
        target_component: 1,
        command: MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
        param1: 1.0,
        ..Default::default()
    }));
    run(&mut tied, 1.0);
    assert!(tied.st.armed && !tied.moored, "armed: cast off");
    let at = moved_m(&tied);
    run(&mut tied, 30.0);
    assert!(moved_m(&tied) - at > 20.0, "after casting off it drifts: {} m", moved_m(&tied) - at);
}
