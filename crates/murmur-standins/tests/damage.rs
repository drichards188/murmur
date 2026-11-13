//! A strike's damage to one of ours (bd murmur-lbw8.2, `murmur_standins::damage`): the stand-in
//! reads it from the damage board and acts it out. A vehicle that lost its propulsion or its
//! weapons still talks and says so itself (a STATUSTEXT `DAMAGE ...`, repeated every 30 s); a
//! destroyed one goes silent: it sends nothing and answers nothing.

use std::sync::Arc;

use mavlink::dialects::ardupilotmega::*;
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::damage::{Board, State};
use murmur_standins::link::{Inbound, parse_all};
use murmur_standins::vehicle::Vehicle;
use tokio::net::UdpSocket;

const SYSID: u8 = 7;

fn boat(port: u16) -> VehicleConfig {
    VehicleConfig {
        uid: Some(3),
        sysid: u32::from(SYSID),
        callsign: "TEST 3".into(),
        port,
        home: [37.83, -122.476, 0.0, 90.0],
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
        moored: false,
        start_at: None,
    }
}

async fn vehicle() -> (Vehicle, std::net::UdpSocket) {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    let mut v = Vehicle::seeded(boat(port), sock, None, 1).expect("vehicle");
    v.link.capture();
    (v, sink)
}

/// Steps for `seconds`; the texts it sent and how many frames.
fn run(v: &mut Vehicle, seconds: f64) -> (Vec<String>, usize) {
    let mut texts = Vec::new();
    let mut frames = 0;
    for _ in 0..(seconds * 10.0).round() as usize {
        v.step();
        for f in v.link.take_captured() {
            frames += 1;
            for m in parse_all(&f) {
                if let Inbound::Msg(_, MavMessage::STATUSTEXT(t)) = m {
                    let s: String = t.text.iter().take_while(|c| **c != 0).map(|c| *c as char).collect();
                    texts.push(s);
                }
            }
        }
    }
    (texts, frames)
}

/// Frames it sends at once in answer to a command (a COMMAND_ACK, at least).
fn answers(v: &mut Vehicle) -> usize {
    v.link.take_captured();
    v.handle(&MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        target_system: SYSID,
        target_component: 1,
        command: MavCmd::MAV_CMD_REQUEST_MESSAGE,
        param1: 0.0,
        ..Default::default()
    }));
    v.link.take_captured().len()
}

#[tokio::test]
async fn a_damaged_stand_in_reports_itself_and_a_destroyed_one_goes_silent() {
    let (mut v, _sink) = vehicle().await;
    let board = Arc::new(Board::default());
    v.damage_board = Some(board.clone());
    let (texts, _) = run(&mut v, 5.0);
    assert!(!texts.iter().any(|t| t.starts_with("DAMAGE")));
    assert!(answers(&mut v) > 0);
    // another vehicle's damage changes nothing here
    board.put(4, State::Destroyed);
    let (texts, _) = run(&mut v, 2.0);
    assert!(!texts.iter().any(|t| t.starts_with("DAMAGE")));
    // its own: said at once, and again every 30 s
    board.put(3, State::Mobility);
    let (texts, _) = run(&mut v, 1.0);
    assert_eq!(texts.iter().filter(|t| t.as_str() == "DAMAGE mobility: propulsion lost").count(), 1, "{texts:?}");
    assert!(v.immobile() && answers(&mut v) > 0, "a stopped vehicle still talks");
    let (texts, _) = run(&mut v, 31.0);
    assert_eq!(texts.iter().filter(|t| t.starts_with("DAMAGE mobility")).count(), 1, "{texts:?}");
    board.put(3, State::Firepower);
    let (texts, _) = run(&mut v, 1.0);
    assert!(texts.iter().any(|t| t == "DAMAGE firepower: weapons out of action"), "{texts:?}");
    // destroyed: nothing out, nothing answered
    board.put(3, State::Destroyed);
    let (texts, frames) = run(&mut v, 31.0);
    assert!(v.destroyed() && texts.is_empty() && frames == 0, "{texts:?} {frames}");
    assert_eq!(answers(&mut v), 0, "a destroyed vehicle answers nothing");
}
