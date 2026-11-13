//! A starting task (`VehicleConfig::start_at`, a fleet's `start`; decision rules 29): the vehicle
//! starts on its task's station, holding there as the bridge's hold would, armed, until the
//! order reaches it; its home stays where its unit spawned it.

use std::sync::Arc;

use murmur_standins::conditions::{Sample, Slot};
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::geo::distance_m;
use murmur_standins::vehicle::Vehicle;
use tokio::net::UdpSocket;

const HOME: (f64, f64) = (37.83, -122.476);
/// the station, about 1.1 km north-east of home
const STATION: (f64, f64) = (37.837, -122.468);

fn spec(behavior: &str, cruise: f64) -> Spec {
    Spec {
        name: format!("test {behavior}"),
        behavior: behavior.into(),
        cruise_mps: cruise,
        max_mps: cruise * 1.5,
        climb_mps: Some(3.0),
        turn_dps: Some(20.0),
        loiter_radius_m: Some(80.0),
        endurance_h: Some(10.0),
        link: None,
    }
}

fn config(spec: Spec, alt: f64, port: u16) -> VehicleConfig {
    VehicleConfig {
        uid: Some(1),
        sysid: 7,
        callsign: "TEST 1".into(),
        port,
        home: [HOME.0, HOME.1, 0.0, 90.0],
        spec,
        params: Default::default(),
        surface_m: None,
        moored: false,
        start_at: Some([STATION.0, STATION.1, alt, 45.0]),
    }
}

/// 20 m of water and a 1 m/s current setting east, whatever the position.
fn water() -> Arc<Slot> {
    let s = Arc::new(Slot::default());
    s.put(Sample {
        lat: STATION.0,
        lon: STATION.1,
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

async fn vehicle(cfg: VehicleConfig, water: Option<Arc<Slot>>) -> (Vehicle, std::net::UdpSocket) {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let mut cfg = cfg;
    cfg.port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", cfg.port)).await.expect("connect");
    let mut v = Vehicle::seeded(cfg, sock, water, 1).expect("vehicle");
    v.link.capture();
    (v, sink)
}

fn run(v: &mut Vehicle, seconds: f64) {
    for _ in 0..(seconds * 10.0).round() as usize {
        v.step();
        v.link.take_captured();
    }
}

fn off_station_m(v: &Vehicle) -> f64 {
    let (lat, lon, _) = v.truth();
    distance_m(STATION.0, STATION.1, lat, lon)
}

#[tokio::test]
async fn a_copter_starts_in_the_air_on_its_station_and_holds_there() {
    let (mut v, _s) = vehicle(config(spec("copter", 12.0), 100.0, 0), None).await;
    assert!(v.st.armed && v.st.mode == "BRAKE", "{} {}", v.st.armed, v.st.mode);
    assert!(v.behavior.airborne(&v.st));
    assert_eq!(v.st.home, (HOME.0, HOME.1, 0.0), "its home is where its unit spawned it");
    run(&mut v, 60.0);
    let (_, _, alt) = v.truth();
    assert!((alt - 100.0).abs() < 2.0, "holds its altitude in BRAKE: {alt}");
    assert!(off_station_m(&v) < 5.0, "holds its station: {}", off_station_m(&v));
    assert!(v.st.armed && v.st.mode == "BRAKE");
}

#[tokio::test]
async fn a_plane_starts_circling_its_station() {
    let (mut v, _s) = vehicle(config(spec("plane", 20.0), 800.0, 0), None).await;
    assert!(v.st.armed && v.st.mode == "LOITER" && v.behavior.airborne(&v.st));
    run(&mut v, 120.0);
    let (_, _, alt) = v.truth();
    assert!((alt - 800.0).abs() < 30.0, "circles at its altitude: {alt}");
    assert!(off_station_m(&v) < 400.0, "circles its station: {}", off_station_m(&v));
    assert!(v.st.armed);
}

/// A plane its unit spawned aloft (Saturation's Pumas along their lanes) keeps its home on the
/// surface below that point, as a plane spawned high without a starting task does.
#[tokio::test]
async fn a_plane_spawned_aloft_has_its_home_on_the_surface_below() {
    let mut cfg = config(spec("plane", 20.0), 330.0, 0);
    cfg.home[2] = 850.0;
    cfg.surface_m = Some(2.0);
    let (v, _s) = vehicle(cfg, None).await;
    assert!(v.st.armed && v.st.mode == "LOITER" && v.behavior.airborne(&v.st));
    assert_eq!(v.st.home, (HOME.0, HOME.1, 2.0));
    let (_, _, alt) = v.truth();
    assert!((alt - 330.0).abs() < 1.0, "starts at its station's height: {alt}");
}

#[tokio::test]
async fn a_boat_holds_its_station_against_the_current() {
    let (mut v, _s) = vehicle(config(spec("boat", 6.0), 0.0, 0), Some(water())).await;
    assert!(v.st.armed && v.st.mode == "LOITER");
    run(&mut v, 120.0);
    // LOITER drives back inside its radius: the 1 m/s current would have carried it 120 m
    assert!(off_station_m(&v) < 40.0, "holds station: {}", off_station_m(&v));
}
