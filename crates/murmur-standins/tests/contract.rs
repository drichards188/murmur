//! The stand-in contract (docs/AUTHENTICITY.md): parameters, message rates, the mission
//! protocol both ways, unknown commands, failsafes, navigation without GPS, the acoustic
//! link. Some tests talk to a running vehicle over UDP; others step a vehicle's physics
//! directly (faster than real time).

use std::time::Duration;

use mavlink::dialects::ardupilotmega::*;
use mavlink::{MavHeader, MessageData, peek_reader::PeekReader};
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::geo::distance_m;
use murmur_standins::vehicle::{self, Vehicle};
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout};

const SYSID: u8 = 7;

struct Gcs {
    sock: UdpSocket,
    seq: u8,
}

impl Gcs {
    async fn send(&mut self, msg: MavMessage) {
        self.send_as(255, msg).await;
    }

    async fn send_as(&mut self, sysid: u8, msg: MavMessage) {
        let mut buf = Vec::new();
        let h = MavHeader {
            system_id: sysid,
            component_id: 190,
            sequence: self.seq,
        };
        mavlink::write_v2_msg(&mut buf, h, &msg).expect("encode");
        self.seq = self.seq.wrapping_add(1);
        self.sock.send(&buf).await.expect("send");
    }

    async fn raw(&mut self) -> Vec<u8> {
        let mut buf = [0u8; 2048];
        let n = self.sock.recv(&mut buf).await.expect("recv");
        buf[..n].to_vec()
    }

    /// Next message matching `f`, skipping the rest.
    async fn expect<T>(&mut self, mut f: impl FnMut(&MavMessage) -> Option<T>) -> T {
        timeout(Duration::from_secs(20), async {
            loop {
                let d = self.raw().await;
                let mut r = PeekReader::new(&d[..]);
                if let Ok((_, m)) = mavlink::read_v2_msg::<MavMessage, _>(&mut r)
                    && let Some(t) = f(&m)
                {
                    return t;
                }
            }
        })
        .await
        .expect("timed out waiting for a message")
    }

    /// Everything that arrives for `secs`.
    async fn collect(&mut self, secs: f64) -> Vec<MavMessage> {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        let mut out = Vec::new();
        while let Ok(d) = tokio::time::timeout_at(end, self.raw()).await {
            let mut r = PeekReader::new(&d[..]);
            if let Ok((_, m)) = mavlink::read_v2_msg::<MavMessage, _>(&mut r) {
                out.push(m);
            }
        }
        out
    }

    async fn command(&mut self, command: MavCmd, p: [f32; 3]) -> MavResult {
        let msg = COMMAND_LONG_DATA {
            target_system: SYSID,
            target_component: 1,
            command,
            param1: p[0],
            param2: p[1],
            param3: p[2],
            ..Default::default()
        };
        self.send(MavMessage::COMMAND_LONG(msg)).await;
        self.expect(|m| match m {
            MavMessage::COMMAND_ACK(a) if a.command == command => Some(a.result),
            _ => None,
        })
        .await
    }

    async fn read_param(&mut self, name: &str, index: i16) -> MavMessage {
        let msg = PARAM_REQUEST_READ_DATA {
            param_index: index,
            target_system: SYSID,
            target_component: 1,
            param_id: name.into(),
        };
        self.send(MavMessage::PARAM_REQUEST_READ(msg)).await;
        self.expect(|m| match m {
            MavMessage::PARAM_VALUE(_) | MavMessage::PARAM_ERROR(_) => Some(m.clone()),
            _ => None,
        })
        .await
    }
}

fn spec(behavior: &str) -> Spec {
    Spec {
        name: format!("test {behavior}"),
        behavior: behavior.into(),
        cruise_mps: 5.0,
        max_mps: 6.0,
        climb_mps: Some(1.0),
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
        home: [37.8, -122.6, alt, 90.0],
        spec,
    }
}

/// A vehicle running in real time, and the GCS end of its link.
async fn start(spec: Spec, alt: f64) -> Gcs {
    let gcs = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = gcs.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    gcs.connect(sock.local_addr().expect("addr"))
        .await
        .expect("connect");
    let v = Vehicle::new(config(spec, port, alt), sock, None).expect("vehicle");
    tokio::spawn(vehicle::run(v, Duration::ZERO));
    Gcs { sock: gcs, seq: 0 }
}

/// A vehicle to step by hand (its link goes to a socket nobody reads).
async fn offline(spec: Spec, alt: f64) -> (Vehicle, UdpSocket) {
    let sink = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    let v = Vehicle::new(config(spec, port, alt), sock, None).expect("vehicle");
    (v, sink)
}

fn count(msgs: &[MavMessage], id: u32) -> usize {
    msgs.iter().filter(|m| m.message_id() == id).count()
}

use mavlink::Message;

#[tokio::test]
async fn parameters_read_set_and_list() {
    let mut g = start(spec("plane"), 7600.0).await;
    let MavMessage::PARAM_VALUE(p) = g.read_param("WP_LOITER_RAD", -1).await else {
        panic!("no PARAM_VALUE");
    };
    assert_eq!(p.param_value, 300.0, "seeded from the catalog");
    assert_eq!(p.param_type, MavParamType::MAV_PARAM_TYPE_INT16);
    let total = p.param_count;
    assert!(total > 20);
    let MavMessage::PARAM_VALUE(first) = g.read_param("", 0).await else {
        panic!("no PARAM_VALUE by index");
    };
    assert_eq!(first.param_index, 0);
    let MavMessage::PARAM_ERROR(e) = g.read_param("NO_SUCH_PARAM", -1).await else {
        panic!("expected PARAM_ERROR");
    };
    assert_eq!(e.error, MavParamError::MAV_PARAM_ERROR_DOES_NOT_EXIST);

    // set persists (and rounds like AP_Param)
    let set = PARAM_SET_DATA {
        param_value: 449.996,
        target_system: SYSID,
        target_component: 1,
        param_id: "WP_LOITER_RAD".into(),
        param_type: MavParamType::MAV_PARAM_TYPE_REAL32,
    };
    g.send(MavMessage::PARAM_SET(set)).await;
    let echo = g
        .expect(|m| match m {
            MavMessage::PARAM_VALUE(p) => Some(p.param_value),
            _ => None,
        })
        .await;
    assert_eq!(echo, 450.0);
    let MavMessage::PARAM_VALUE(p) = g.read_param("WP_LOITER_RAD", -1).await else {
        panic!("no PARAM_VALUE");
    };
    assert_eq!(p.param_value, 450.0);

    // the whole list: every index once, with the count
    let list = PARAM_REQUEST_LIST_DATA {
        target_system: SYSID,
        target_component: 1,
    };
    g.send(MavMessage::PARAM_REQUEST_LIST(list)).await;
    let mut seen = std::collections::BTreeSet::new();
    while seen.len() < usize::from(total) {
        let (i, n) = g
            .expect(|m| match m {
                MavMessage::PARAM_VALUE(p) if p.param_index != u16::MAX => {
                    Some((p.param_index, p.param_count))
                }
                _ => None,
            })
            .await;
        assert_eq!(n, total);
        seen.insert(i);
    }
    assert_eq!(seen.last().copied(), Some(total - 1));
}

#[tokio::test]
async fn message_rates_follow_set_message_interval() {
    let mut g = start(spec("boat"), 0.0).await;
    // stream groups at 0 (common.parm): heartbeat only
    let quiet = g.collect(2.5).await;
    assert!(count(&quiet, HEARTBEAT_DATA::ID) >= 2);
    assert_eq!(count(&quiet, GLOBAL_POSITION_INT_DATA::ID), 0);
    assert_eq!(count(&quiet, SYS_STATUS_DATA::ID), 0);

    let gpi = GLOBAL_POSITION_INT_DATA::ID as f32;
    let set = MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL;
    assert_eq!(
        g.command(set, [gpi, 200_000.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let five_hz = g.collect(3.0).await;
    let n = count(&five_hz, GLOBAL_POSITION_INT_DATA::ID);
    assert!((13..=17).contains(&n), "5 Hz for 3 s: {n}");
    assert_eq!(
        g.command(set, [gpi, -1.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let _ = g.collect(0.3).await;
    assert_eq!(
        count(&g.collect(1.5).await, GLOBAL_POSITION_INT_DATA::ID),
        0,
        "-1 stops it"
    );

    // 0 restores the default (off for anything but the heartbeat)
    let hb = HEARTBEAT_DATA::ID as f32;
    assert_eq!(
        g.command(set, [hb, 0.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    // a message ArduPilot has but a stand-in doesn't make
    assert_eq!(
        g.command(set, [36.0, 1e6, 0.0]).await,
        MavResult::MAV_RESULT_DENIED
    );

    // REQUEST_MESSAGE: once
    let req = MavCmd::MAV_CMD_REQUEST_MESSAGE;
    let av = AUTOPILOT_VERSION_DATA::ID as f32;
    assert_eq!(
        g.command(req, [av, 0.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let after = g.collect(1.5).await;
    assert_eq!(count(&after, AUTOPILOT_VERSION_DATA::ID), 1);
}

#[tokio::test]
#[allow(deprecated)]
async fn mission_download_and_mission_types() {
    let mut g = start(spec("boat"), 0.0).await;
    let wp = |seq: u16, lat: f64, ty: MavMissionType, command: MavCmd| MISSION_ITEM_INT_DATA {
        target_system: SYSID,
        target_component: 1,
        seq,
        command,
        frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
        x: (lat * 1e7) as i32,
        y: (-122.6 * 1e7) as i32,
        autocontinue: 1,
        mission_type: ty,
        ..Default::default()
    };
    let mission = MavMissionType::MAV_MISSION_TYPE_MISSION;
    let count = |n, ty| MISSION_COUNT_DATA {
        target_system: SYSID,
        target_component: 1,
        count: n,
        mission_type: ty,
        ..Default::default()
    };
    g.send(MavMessage::MISSION_COUNT(count(2, mission))).await;
    for _ in 0..2 {
        let seq = g
            .expect(|m| match m {
                MavMessage::MISSION_REQUEST(r) => Some(r.seq),
                _ => None,
            })
            .await;
        let nav = MavCmd::MAV_CMD_NAV_WAYPOINT;
        g.send(MavMessage::MISSION_ITEM_INT(wp(seq, 37.801, mission, nav)))
            .await;
    }
    g.expect(|m| matches!(m, MavMessage::MISSION_ACK(_)).then_some(()))
        .await;

    let list = MISSION_REQUEST_LIST_DATA {
        target_system: SYSID,
        target_component: 1,
        mission_type: mission,
    };
    g.send(MavMessage::MISSION_REQUEST_LIST(list)).await;
    let n = g
        .expect(|m| match m {
            MavMessage::MISSION_COUNT(c) => Some(c.count),
            _ => None,
        })
        .await;
    assert_eq!(n, 2);
    let req = |seq| MISSION_REQUEST_INT_DATA {
        seq,
        target_system: SYSID,
        target_component: 1,
        mission_type: mission,
    };
    g.send(MavMessage::MISSION_REQUEST_INT(req(0))).await;
    let home = g
        .expect(|m| match m {
            MavMessage::MISSION_ITEM_INT(i) => Some(i.clone()),
            _ => None,
        })
        .await;
    assert_eq!((home.seq, home.frame), (0, MavFrame::MAV_FRAME_GLOBAL));
    assert_eq!(home.x, 378_000_000, "item 0 is home");
    g.send(MavMessage::MISSION_REQUEST_INT(req(1))).await;
    let one = g
        .expect(|m| match m {
            MavMessage::MISSION_ITEM_INT(i) => Some(i.clone()),
            _ => None,
        })
        .await;
    assert_eq!(one.frame, MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT);
    assert_eq!(one.x, 378_010_000);
    g.send(MavMessage::MISSION_REQUEST_INT(req(9))).await;
    let bad = g
        .expect(|m| match m {
            MavMessage::MISSION_ACK(a) => Some(a.mavtype),
            _ => None,
        })
        .await;
    assert_eq!(bad, MavMissionResult::MAV_MISSION_INVALID_SEQUENCE);

    // fence: only fence items; a waypoint in a fence upload is refused
    let fence = MavMissionType::MAV_MISSION_TYPE_FENCE;
    g.send(MavMessage::MISSION_COUNT(count(1, fence))).await;
    g.expect(|m| {
        matches!(m, MavMessage::MISSION_REQUEST(r) if r.mission_type == fence).then_some(())
    })
    .await;
    let nav = MavCmd::MAV_CMD_NAV_WAYPOINT;
    g.send(MavMessage::MISSION_ITEM_INT(wp(0, 37.8, fence, nav)))
        .await;
    let r = g
        .expect(|m| match m {
            MavMessage::MISSION_ACK(a) => Some((a.mavtype, a.mission_type)),
            _ => None,
        })
        .await;
    assert_eq!(r, (MavMissionResult::MAV_MISSION_UNSUPPORTED, fence));
    // rally points are accepted and can be read back
    let rally = MavMissionType::MAV_MISSION_TYPE_RALLY;
    g.send(MavMessage::MISSION_COUNT(count(1, rally))).await;
    g.expect(|m| {
        matches!(m, MavMessage::MISSION_REQUEST(r) if r.mission_type == rally).then_some(())
    })
    .await;
    let point = MavCmd::MAV_CMD_NAV_RALLY_POINT;
    g.send(MavMessage::MISSION_ITEM_INT(wp(0, 37.81, rally, point)))
        .await;
    let r = g
        .expect(|m| match m {
            MavMessage::MISSION_ACK(a) => Some((a.mavtype, a.mission_type)),
            _ => None,
        })
        .await;
    assert_eq!(r, (MavMissionResult::MAV_MISSION_ACCEPTED, rally));
}

#[tokio::test]
async fn unknown_command_ids_are_acked_unsupported() {
    let mut g = start(spec("boat"), 0.0).await;
    // a COMMAND_LONG for command 31999, which the dialect has no name for
    let mut raw = mavlink::MAVLinkV2MessageRaw::new();
    let msg = MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        target_system: SYSID,
        target_component: 1,
        command: MavCmd::MAV_CMD_NAV_WAYPOINT,
        param1: 1.0,
        ..Default::default()
    });
    let h = MavHeader {
        system_id: 255,
        component_id: 190,
        sequence: 0,
    };
    raw.serialize_message(h, &msg);
    let mut frame = raw.raw_bytes().to_vec();
    let len = frame[1] as usize;
    frame[10 + 28..10 + 30].copy_from_slice(&31_999u16.to_le_bytes());
    let crc = mavlink::calculate_crc(&frame[1..10 + len], COMMAND_LONG_DATA::EXTRA_CRC);
    frame[10 + len..12 + len].copy_from_slice(&crc.to_le_bytes());
    g.sock.send(&frame).await.expect("send");
    let ack = timeout(Duration::from_secs(5), async {
        loop {
            let d = g.raw().await;
            // MAVLink 2, COMMAND_ACK (77)
            if d[0] == 0xFD && d[7] == 77 {
                return d;
            }
        }
    })
    .await
    .expect("no ack");
    let payload = &ack[10..10 + ack[1] as usize];
    assert_eq!(u16::from_le_bytes([payload[0], payload[1]]), 31_999);
    assert_eq!(payload[2], MavResult::MAV_RESULT_UNSUPPORTED as u8);
    let len = ack[1] as usize;
    let crc = mavlink::calculate_crc(&ack[1..10 + len], COMMAND_ACK_DATA::EXTRA_CRC);
    assert_eq!(&ack[10 + len..12 + len], &crc.to_le_bytes(), "valid frame");
}

#[tokio::test]
async fn gcs_heartbeat_loss_triggers_the_rover_failsafe() {
    let mut g = start(spec("boat"), 0.0).await;
    let arm = MavCmd::MAV_CMD_COMPONENT_ARM_DISARM;
    assert_eq!(
        g.command(arm, [1.0, 0.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let gcs_hb = HEARTBEAT_DATA {
        mavtype: MavType::MAV_TYPE_GCS,
        autopilot: MavAutopilot::MAV_AUTOPILOT_INVALID,
        ..Default::default()
    };
    g.send(MavMessage::HEARTBEAT(gcs_hb.clone())).await;
    // FS_GCS_TIMEOUT 5 s, then FS_TIMEOUT 1.5 s
    let text = g
        .expect(|m| match m {
            MavMessage::STATUSTEXT(t) => t.text.to_str().ok().map(str::to_owned),
            _ => None,
        })
        .await;
    assert_eq!(text, "GCS Failsafe");
    let hb = g
        .expect(|m| match m {
            MavMessage::HEARTBEAT(h) => Some((h.custom_mode, h.system_status)),
            _ => None,
        })
        .await;
    assert_eq!(hb, (11, MavState::MAV_STATE_CRITICAL), "RTL, critical");
    g.send(MavMessage::HEARTBEAT(gcs_hb.clone())).await;
    let text = g
        .expect(|m| match m {
            MavMessage::STATUSTEXT(t) => t.text.to_str().ok().map(str::to_owned),
            _ => None,
        })
        .await;
    assert_eq!(text, "GCS Failsafe Cleared");
}

#[tokio::test]
async fn battery_failsafe_rover_low_rtl_then_critical_hold() {
    let mut s = spec("boat");
    s.endurance_h = Some(0.002); // 7.2 s at cruise load
    let (mut v, _sink) = offline(s, 0.0).await;
    v.st.armed = true;
    v.set_mode("MANUAL");
    let mut modes = Vec::new();
    for _ in 0..400 {
        // full load: hold cruise speed
        v.st.speed = 5.0;
        v.step();
        if modes.last() != Some(&v.st.mode) {
            modes.push(v.st.mode);
        }
    }
    assert_eq!(modes, ["MANUAL", "RTL", "HOLD"]);
    assert_eq!(v.battery.level, murmur_standins::battery::Level::Critical);
}

#[tokio::test]
async fn plane_rtl_circles_home_at_the_turn_bounded_radius() {
    let mut s = spec("plane");
    // MQ-9B-class: 90 m/s, 3 deg/s: no circle under 1,719 m
    s.cruise_mps = 90.0;
    s.max_mps = 105.0;
    s.turn_dps = Some(3.0);
    s.climb_mps = Some(10.0);
    s.loiter_radius_m = Some(3500.0);
    let (mut v, _sink) = offline(s, 7600.0).await;
    // RTL_RADIUS 0 -> WP_LOITER_RAD; asking for 150 m still flies the airframe's minimum
    assert!(v.params.set("WP_LOITER_RAD", 150.0).is_ok());
    let home = v.st.home;
    // start 20 km east of home
    v.st.lon += 20_000.0 / (111_320.0 * home.0.to_radians().cos());
    v.set_mode("RTL");
    let mut dists = Vec::new();
    for i in 0..30_000 {
        v.step();
        if i > 20_000 {
            dists.push(distance_m(v.st.lat, v.st.lon, home.0, home.1));
        }
    }
    let lo = dists.iter().cloned().fold(f64::MAX, f64::min);
    let hi = dists.iter().cloned().fold(0.0, f64::max);
    assert!(
        lo > 1_500.0 && hi < 2_100.0,
        "circling home at {lo:.0}..{hi:.0} m"
    );
    assert!(
        (v.st.alt - 7600.0).abs() < 1.0,
        "ALT_HOLD_RTL -1: same altitude"
    );
}

#[tokio::test]
async fn boat_holds_at_the_last_waypoint_when_the_mission_ends() {
    let (mut v, _sink) = offline(spec("boat"), 0.0).await;
    let item = |seq: u16, north_m: f64| MISSION_ITEM_INT_DATA {
        seq,
        command: MavCmd::MAV_CMD_NAV_WAYPOINT,
        #[allow(deprecated)]
        frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
        x: ((37.8 + north_m / 111_320.0) * 1e7) as i32,
        y: (-122.6 * 1e7) as i32,
        ..Default::default()
    };
    v.mission.install(vec![item(0, 0.0), item(1, 150.0)]);
    v.st.armed = true;
    v.set_mode("AUTO");
    for _ in 0..1500 {
        v.step();
    }
    assert!(v.mission.complete);
    assert_eq!(v.st.mode, "AUTO", "Rover stays in AUTO");
    let d = distance_m(v.st.lat, v.st.lon, 37.8 + 150.0 / 111_320.0, -122.6);
    assert!(d < 25.0, "holding at the last waypoint, {d:.0} m off");
    assert!(v.st.speed.abs() < 0.5, "stopped, not circling");
}

#[tokio::test]
async fn submerged_auv_dead_reckons_and_reports_little() {
    let mut s = spec("sub");
    s.link = Some("acoustic".into());
    s.cruise_mps = 2.0;
    s.climb_mps = Some(0.5);
    let mut g = start(s, -40.0).await;
    let set = MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL;
    let gpi = GLOBAL_POSITION_INT_DATA::ID as f32;
    // the plan is accepted (as by ArduPilot), but nothing streams under water
    assert_eq!(
        g.command(set, [gpi, 500_000.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let msgs = g.collect(5.0).await;
    assert_eq!(count(&msgs, GLOBAL_POSITION_INT_DATA::ID), 0);
    assert!(count(&msgs, HEARTBEAT_DATA::ID) <= 1, "one status per 20 s");
    // EKF: relative position only (no GPS); GPS: no fix
    let req = MavCmd::MAV_CMD_REQUEST_MESSAGE;
    let ekf = EKF_STATUS_REPORT_DATA::ID as f32;
    assert_eq!(
        g.command(req, [ekf, 0.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let flags = g
        .expect(|m| match m {
            MavMessage::EKF_STATUS_REPORT(e) => Some(e.flags),
            _ => None,
        })
        .await;
    assert!(flags.contains(EkfStatusFlags::EKF_POS_HORIZ_REL));
    assert!(!flags.contains(EkfStatusFlags::EKF_POS_HORIZ_ABS));
    let gps = GPS_RAW_INT_DATA::ID as f32;
    assert_eq!(
        g.command(req, [gps, 0.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let fix = g
        .expect(|m| match m {
            MavMessage::GPS_RAW_INT(r) => Some(r.fix_type),
            _ => None,
        })
        .await;
    assert_eq!(fix as u8, 0);
}

#[tokio::test]
async fn auv_estimate_drifts_from_truth_until_it_surfaces() {
    let mut s = spec("sub");
    s.cruise_mps = 2.0;
    s.climb_mps = Some(0.5);
    let (mut v, _sink) = offline(s, -40.0).await;
    let far = |north_m: f64| ((37.8 + north_m / 111_320.0) * 1e7) as i32;
    let item = |seq: u16, north_m: f64, z: f32| MISSION_ITEM_INT_DATA {
        seq,
        command: MavCmd::MAV_CMD_NAV_WAYPOINT,
        frame: MavFrame::MAV_FRAME_GLOBAL,
        x: far(north_m),
        y: (-122.6 * 1e7) as i32,
        z,
        ..Default::default()
    };
    // 3 km north at 40 m, then up
    v.mission.install(vec![
        item(0, 0.0, -40.0),
        item(1, 3_000.0, -40.0),
        item(2, 3_010.0, 0.0),
    ]);
    v.st.armed = true;
    v.set_mode("AUTO");
    let mut worst: f64 = 0.0;
    for _ in 0..16_000 {
        v.step();
        worst = worst.max(v.nav.error_m());
    }
    let (tlat, tlon, _) = v.truth();
    assert!(worst > 1.0, "error grew under water: {worst:.2} m");
    assert!(v.nav.gps, "fix again at the surface");
    assert_eq!(v.nav.error_m(), 0.0);
    assert!(
        distance_m(tlat, tlon, v.st.lat, v.st.lon) < 0.01,
        "estimate = truth again"
    );
}
