//! A stand-in on a real UDP link, driven like the bridge drives it.

use std::time::Duration;

use mavlink::dialects::ardupilotmega::*;
use mavlink::{MavHeader, peek_reader::PeekReader};
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::vehicle::{self, Vehicle};
use tokio::net::UdpSocket;
use tokio::time::timeout;

struct Gcs {
    sock: UdpSocket,
    seq: u8,
}

impl Gcs {
    async fn send(&mut self, msg: MavMessage) {
        let mut buf = Vec::new();
        let h = MavHeader {
            system_id: 255,
            component_id: 190,
            sequence: self.seq,
        };
        mavlink::write_v2_msg(&mut buf, h, &msg).expect("encode");
        self.seq = self.seq.wrapping_add(1);
        self.sock.send(&buf).await.expect("send");
    }

    /// Next message matching `f`, skipping telemetry.
    async fn expect<T>(&mut self, mut f: impl FnMut(&MavHeader, &MavMessage) -> Option<T>) -> T {
        let mut buf = [0u8; 2048];
        timeout(Duration::from_secs(15), async {
            loop {
                let n = self.sock.recv(&mut buf).await.expect("recv");
                let mut r = PeekReader::new(&buf[..n]);
                if let Ok((h, m)) = mavlink::read_v2_msg::<MavMessage, _>(&mut r)
                    && let Some(t) = f(&h, &m)
                {
                    return t;
                }
            }
        })
        .await
        .expect("timed out waiting for a message")
    }

    async fn command(&mut self, command: MavCmd, p1: f32, p2: f32) -> MavResult {
        let msg = COMMAND_LONG_DATA {
            target_system: 7,
            target_component: 1,
            command,
            param1: p1,
            param2: p2,
            ..Default::default()
        };
        self.send(MavMessage::COMMAND_LONG(msg)).await;
        self.expect(|_, m| match m {
            MavMessage::COMMAND_ACK(a) if a.command == command => Some(a.result),
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

async fn start(behavior: &str, alt: f64) -> Gcs {
    let gcs = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = gcs.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    gcs.connect(sock.local_addr().expect("addr"))
        .await
        .expect("connect");
    let cfg = VehicleConfig {
        uid: Some(300), // above 255: identity on the wire is the sysid
        sysid: 7,
        callsign: "TEST 1".into(),
        port,
        home: [37.8, -122.6, alt, 90.0],
        spec: spec(behavior),
        params: Default::default(),
        surface_m: None,
    };
    tokio::spawn(vehicle::run(
        Vehicle::new(cfg, sock, None).expect("vehicle"),
        Duration::ZERO,
    ));
    Gcs { sock: gcs, seq: 0 }
}

#[tokio::test]
async fn heartbeat_identity_and_modes() {
    let mut g = start("plane", 7600.0).await;
    let hb = g
        .expect(|h, m| match m {
            MavMessage::HEARTBEAT(hb) => Some((h.system_id, h.component_id, hb.clone())),
            _ => None,
        })
        .await;
    assert_eq!((hb.0, hb.1), (7, 1));
    assert_eq!(hb.2.mavtype, MavType::MAV_TYPE_FIXED_WING);
    assert_eq!(hb.2.autopilot, MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA);
    assert_eq!(hb.2.custom_mode, 12, "spawned high: LOITER");
    assert!(
        hb.2.base_mode
            .contains(MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED)
    );

    // a fixed wing refuses to disarm in flight (AP_Arming_Plane::disarm -> FAILED)
    assert_eq!(
        g.command(MavCmd::MAV_CMD_COMPONENT_ARM_DISARM, 0.0, 0.0)
            .await,
        MavResult::MAV_RESULT_FAILED
    );
    assert_eq!(
        g.command(MavCmd::MAV_CMD_DO_SET_MODE, 1.0, 11.0).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let mode = g
        .expect(|_, m| match m {
            MavMessage::HEARTBEAT(hb) => Some(hb.custom_mode),
            _ => None,
        })
        .await;
    assert_eq!(mode, 11, "RTL");
    // no such mode: _set_mode_common returns FAILED
    assert_eq!(
        g.command(MavCmd::MAV_CMD_DO_SET_MODE, 1.0, 9.0).await,
        MavResult::MAV_RESULT_FAILED
    );
    assert_eq!(
        g.command(MavCmd::MAV_CMD_NAV_LAND, 0.0, 0.0).await,
        MavResult::MAV_RESULT_UNSUPPORTED
    );
}

#[tokio::test]
async fn drifter_refuses_missions_and_powered_modes() {
    let mut g = start("drifter", 0.0).await;
    assert_eq!(
        g.command(MavCmd::MAV_CMD_DO_SET_MODE, 1.0, 10.0).await,
        MavResult::MAV_RESULT_FAILED
    );
    assert_eq!(
        g.command(MavCmd::MAV_CMD_DO_SET_MODE, 1.0, 4.0).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let count = MISSION_COUNT_DATA {
        target_system: 7,
        target_component: 1,
        count: 2,
        ..Default::default()
    };
    g.send(MavMessage::MISSION_COUNT(count)).await;
    let ack = g
        .expect(|_, m| match m {
            MavMessage::MISSION_ACK(a) => Some(a.mavtype),
            _ => None,
        })
        .await;
    assert_eq!(ack, MavMissionResult::MAV_MISSION_UNSUPPORTED);
}

#[tokio::test]
#[allow(deprecated)]
async fn mission_upload_then_auto() {
    let mut g = start("boat", 0.0).await;
    let wp = |seq: u16, lat: f64| MISSION_ITEM_INT_DATA {
        target_system: 7,
        target_component: 1,
        seq,
        command: MavCmd::MAV_CMD_NAV_WAYPOINT,
        frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
        x: (lat * 1e7) as i32,
        y: (-122.6 * 1e7) as i32,
        autocontinue: 1,
        ..Default::default()
    };
    let count = MISSION_COUNT_DATA {
        target_system: 7,
        target_component: 1,
        count: 3,
        ..Default::default()
    };
    g.send(MavMessage::MISSION_COUNT(count)).await;
    for want in 0..3u16 {
        // ArduPilot asks for items with (deprecated) MISSION_REQUEST
        let seq = g
            .expect(|_, m| match m {
                MavMessage::MISSION_REQUEST(r) => Some(r.seq),
                _ => None,
            })
            .await;
        assert_eq!(seq, want);
        g.send(MavMessage::MISSION_ITEM_INT(wp(
            seq,
            37.8 + 0.0002 * seq as f64,
        )))
        .await;
    }
    let text = g
        .expect(|_, m| match m {
            MavMessage::STATUSTEXT(t) => t.text.to_str().ok().map(str::to_owned),
            _ => None,
        })
        .await;
    assert_eq!(text, "Flight plan received");
    let ack = g
        .expect(|_, m| match m {
            MavMessage::MISSION_ACK(a) => Some((a.mavtype, a.target_system, a.target_component)),
            _ => None,
        })
        .await;
    assert_eq!(ack, (MavMissionResult::MAV_MISSION_ACCEPTED, 255, 190));

    assert_eq!(
        g.command(MavCmd::MAV_CMD_COMPONENT_ARM_DISARM, 1.0, 0.0)
            .await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    assert_eq!(
        g.command(MavCmd::MAV_CMD_DO_SET_MODE, 1.0, 10.0).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    // first waypoint is 22 m north: a turn and a slow (0.5 m/s2) start, about 10 s
    let reached = g
        .expect(|_, m| match m {
            MavMessage::MISSION_ITEM_REACHED(r) => Some(r.seq),
            _ => None,
        })
        .await;
    assert_eq!(reached, 1);
}
