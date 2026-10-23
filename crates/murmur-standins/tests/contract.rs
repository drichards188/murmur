//! The stand-in contract (docs/AUTHENTICITY.md): parameters, message rates, the mission
//! protocol both ways, unknown commands, failsafes, navigation without GPS, the acoustic
//! link. Some tests talk to a running vehicle over UDP; others step a vehicle's physics
//! directly (faster than real time) and read what it sends from its link
//! (`Link::capture`: every frame as it is handed to the socket), not from a loopback socket
//! whose delivery lags such a loop and drops datagrams on a loaded host (bd murmur-ek3).
//! Every vehicle draws from a fixed seed, so its navigation errors are the same every run.

use std::time::Duration;

use mavlink::dialects::ardupilotmega::*;
use mavlink::{MavHeader, MessageData, peek_reader::PeekReader};
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::geo::distance_m;
use murmur_standins::vehicle::{self, Vehicle};
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout};

const SYSID: u8 = 7;

/// How long a frame that came ahead of one sent before it waits for that one before the
/// gap is taken as a loss: no frame is delivered later than this after it arrived, however
/// many are lost (a loaded host's loopback drops some too).
const GAP_WAIT: Duration = Duration::from_secs(5);
/// How far ahead of the one it waits for a frame may be and still wait: past it the gap is
/// taken as a loss at once. Reordering on loopback is a few frames; the sequence number is
/// one byte, so a burst held for long would wrap onto frames still held.
const REORDER_MAX: u8 = 32;

/// The GCS end of a running vehicle's link. It reads the vehicle's frames in the order the
/// vehicle sent them (their MAVLink sequence): a loaded host's loopback can deliver two
/// datagrams the other way round (each CPU queues what its sender sent, and a starved queue
/// waits for its softirq thread while the next CPU's goes at once), and a test of "the
/// texts sent ahead of the ACK" is about the vehicle's order, not the kernel's (bd
/// murmur-ek3: a refusal's text came after its ACK at a load average of 56).
struct Gcs {
    sock: UdpSocket,
    seq: u8,
    /// the vehicle's sequence number of the next frame in its order
    next: Option<u8>,
    /// frames that came ahead of it, by sequence number, and when each arrived
    held: std::collections::BTreeMap<u8, (Vec<u8>, Instant)>,
    /// a frame received and not placed yet: it waits for the held ones to go first
    incoming: Option<Vec<u8>>,
    /// the gap the held frames wait at is given up (lost): they go now, in order
    give_up: bool,
    /// a test of losses: (message id, n): the frame of that id after the next n is lost on
    /// the way
    drop_once: Option<(u32, u32)>,
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

    /// The vehicle's next frame in the order it sent them (see [`Gcs`]).
    async fn raw(&mut self) -> Vec<u8> {
        let mut buf = [0u8; 2048];
        loop {
            if let Some(want) = self.next
                && let Some((f, _)) = self.held.remove(&want)
            {
                self.next = Some(want.wrapping_add(1));
                return f;
            }
            // a frame held GAP_WAIT, or a gap given up: the one waited for is lost; go on
            // from the earliest held
            let oldest = self.held.values().map(|(_, at)| *at).min();
            if !self.held.is_empty() && (self.give_up || oldest.is_some_and(|at| at.elapsed() >= GAP_WAIT)) {
                let want = self.next.unwrap_or(0);
                self.next = self.held.keys().copied().min_by_key(|s| s.wrapping_sub(want));
                continue;
            }
            self.give_up = false;
            let frame = match self.incoming.take() {
                Some(f) => f,
                None => {
                    let n = match oldest {
                        Some(at) => match tokio::time::timeout_at(at + GAP_WAIT, self.sock.recv(&mut buf)).await {
                            Ok(r) => r.expect("recv"),
                            Err(_) => continue,
                        },
                        None => self.sock.recv(&mut buf).await.expect("recv"),
                    };
                    buf[..n].to_vec()
                }
            };
            // MAVLink 2: STX, length, flags, flags, sequence, system, component, message id
            let Some(&seq) = frame.get(4).filter(|_| frame[0] == mavlink::MAV_STX_V2) else {
                return frame;
            };
            if let (Some((id, n)), Some(b)) = (self.drop_once, frame.get(7..10))
                && u32::from_le_bytes([b[0], b[1], b[2], 0]) == id
            {
                self.drop_once = n.checked_sub(1).map(|n| (id, n));
                if n == 0 {
                    continue;
                }
            }
            match self.next {
                Some(want) if seq != want && seq.wrapping_sub(want) < 128 => {
                    if seq.wrapping_sub(want) < REORDER_MAX && !self.held.contains_key(&seq) {
                        // a little ahead of one sent before it: wait for that one
                        self.held.insert(seq, (frame, Instant::now()));
                    } else if self.held.is_empty() {
                        // far ahead: what it would wait for is lost
                        self.next = Some(seq.wrapping_add(1));
                        return frame;
                    } else {
                        // far ahead, or wrapped onto one held: the held go first
                        self.incoming = Some(frame);
                        self.give_up = true;
                    }
                }
                // in order (or one given up on arriving after all: as it comes)
                Some(want) if seq != want => return frame,
                _ => {
                    self.next = Some(seq.wrapping_add(1));
                    return frame;
                }
            }
        }
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
    /// Everything that arrives for `secs`: frames still held at the end for an earlier one
    /// (lost, or late) came inside the window too, and count, in the vehicle's order.
    async fn collect(&mut self, secs: f64) -> Vec<MavMessage> {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        let mut frames = Vec::new();
        while let Ok(d) = tokio::time::timeout_at(end, self.raw()).await {
            frames.push(d);
        }
        let want = self.next.unwrap_or(0);
        let mut held: Vec<(u8, Vec<u8>)> = std::mem::take(&mut self.held)
            .into_iter()
            .map(|(seq, (f, _))| (seq, f))
            .collect();
        held.sort_by_key(|(seq, _)| seq.wrapping_sub(want));
        if let Some((last, _)) = held.last() {
            self.next = Some(last.wrapping_add(1));
        }
        frames.extend(held.into_iter().map(|(_, f)| f));
        frames.extend(self.incoming.take());
        self.give_up = false;
        frames
            .iter()
            .filter_map(|d| mavlink::read_v2_msg::<MavMessage, _>(&mut PeekReader::new(&d[..])).ok())
            .map(|(_, m)| m)
            .collect()
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

    /// A command's ACK result and the STATUSTEXTs sent ahead of it.
    async fn command_with_texts(
        &mut self,
        command: MavCmd,
        p: [f32; 3],
    ) -> (MavResult, Vec<String>) {
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
        let mut texts = Vec::new();
        let result = self
            .expect(|m| match m {
                MavMessage::STATUSTEXT(t) => {
                    texts.push(t.text.to_str().unwrap_or("").to_owned());
                    None
                }
                MavMessage::COMMAND_ACK(a) if a.command == command => Some(a.result),
                _ => None,
            })
            .await;
        (result, texts)
    }

    async fn set_mode(&mut self, number: f32) -> (MavResult, Vec<String>) {
        let custom = f32::from(MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED.bits());
        self.command_with_texts(MavCmd::MAV_CMD_DO_SET_MODE, [custom, number, 0.0])
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
        params: Default::default(),
        surface_m: None,
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
    let v = Vehicle::seeded(config(spec, port, alt), sock, None, SEED).expect("vehicle");
    tokio::spawn(vehicle::run(v, Duration::ZERO));
    Gcs {
        sock: gcs,
        seq: 0,
        next: None,
        held: Default::default(),
        incoming: None,
        give_up: false,
        drop_once: None,
    }
}

/// A vehicle to step by hand: what it sends is kept on its link ([`events`]); its socket
/// goes to one nobody reads.
async fn offline(spec: Spec, alt: f64) -> (Vehicle, UdpSocket) {
    let sink = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    let mut v = Vehicle::seeded(config(spec, port, alt), sock, None, SEED).expect("vehicle");
    v.link.capture();
    (v, sink)
}

/// Every vehicle's draws (its navigation errors): the same every run.
const SEED: u64 = 1;

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

/// A datagram lost on the way holds back none of the frames after it past the window they
/// came in: the GCS reads in the vehicle's order, and what waited for the lost frame is
/// counted with the window (review of 9ab8089: one lost GLOBAL_POSITION_INT left 7 to 11 of
/// the 15 a 5 Hz stream sends in 3 s).
#[tokio::test]
async fn a_lost_frame_holds_back_nothing_past_its_window() {
    let mut g = start(spec("boat"), 0.0).await;
    let gpi = GLOBAL_POSITION_INT_DATA::ID;
    let set = MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL;
    assert_eq!(
        g.command(set, [gpi as f32, 200_000.0, 0.0]).await,
        MavResult::MAV_RESULT_ACCEPTED
    );
    let _ = g.collect(0.5).await;
    // the eighth of the window's, 1.6 s in: what came after it was still held at the end
    g.drop_once = Some((gpi, 7));
    let five_hz = g.collect(3.0).await;
    let n = count(&five_hz, gpi);
    assert!((12..=17).contains(&n), "5 Hz for 3 s, one lost: {n}");
    assert!(g.drop_once.is_none(), "one was lost");
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
async fn refused_mode_changes_and_commands_follow_each_firmware() {
    use MavResult::*;
    // ArduPlane: an unknown mode number says so
    let mut g = start(spec("plane"), 0.0).await;
    let (r, texts) = g.set_mode(99.0).await;
    assert_eq!(
        (r, texts),
        (MAV_RESULT_FAILED, vec!["No such mode 99".to_string()])
    );
    // the number is a uint8 by the time it reaches set_mode: 258 is mode 2
    let (r, texts) = g.set_mode(258.0).await;
    assert_eq!(r, MAV_RESULT_ACCEPTED, "{texts:?}");

    // ArduRover: LEARNING is gone; AUTO with no mission says "Flight mode change failed"
    // after the refusal's own text; no arming in RTL, SMART_RTL or INITIALISING
    let mut g = start(spec("ugv"), 0.0).await;
    let (r, texts) = g.set_mode(2.0).await;
    assert_eq!(
        (r, texts),
        (MAV_RESULT_FAILED, vec!["No such mode 2".to_string()])
    );
    let (r, texts) = g.set_mode(10.0).await;
    assert_eq!(r, MAV_RESULT_FAILED);
    assert_eq!(
        texts,
        ["No Mission. Can't set AUTO.", "Flight mode change failed"]
    );
    let (r, texts) = g.set_mode(6.0).await; // FOLLOW needs FOLL_ENABLE
    assert_eq!(r, MAV_RESULT_FAILED);
    assert_eq!(texts, ["Flight mode change failed"]);
    let arm = MavCmd::MAV_CMD_COMPONENT_ARM_DISARM;
    assert_eq!(g.set_mode(11.0).await.0, MAV_RESULT_ACCEPTED);
    let (r, texts) = g.command_with_texts(arm, [1.0, 0.0, 0.0]).await;
    assert_eq!(
        (r, texts),
        (MAV_RESULT_FAILED, vec!["Arm: Mode not armable".to_string()])
    );
    assert_eq!(g.set_mode(4.0).await.0, MAV_RESULT_ACCEPTED); // HOLD
    assert_eq!(
        g.command_with_texts(arm, [1.0, 0.0, 0.0]).await.0,
        MAV_RESULT_ACCEPTED
    );

    // ArduSub: AUTO with no mission is refused with the mode's name
    let mut g = start(spec("sub"), 0.0).await;
    let (r, texts) = g.set_mode(3.0).await;
    assert_eq!(r, MAV_RESULT_FAILED);
    assert_eq!(texts, ["Flight mode change failed Auto"]);

    // Blimp handles NAV_TAKEOFF (ACCEPTED, disarmed, doing nothing) and has none of
    // MISSION_START, NAV_RETURN_TO_LAUNCH or DO_CHANGE_SPEED
    let mut g = start(spec("blimp"), 20.0).await;
    let takeoff = MavCmd::MAV_CMD_NAV_TAKEOFF;
    assert_eq!(g.command(takeoff, [0.0; 3]).await, MAV_RESULT_ACCEPTED);
    for c in [
        MavCmd::MAV_CMD_MISSION_START,
        MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH,
        MavCmd::MAV_CMD_DO_CHANGE_SPEED,
    ] {
        assert_eq!(
            g.command(c, [0.0, 1.0, 0.0]).await,
            MAV_RESULT_UNSUPPORTED,
            "{c:?}"
        );
    }
    // a plane has DO_CHANGE_SPEED
    let mut g = start(spec("plane"), 0.0).await;
    let change = MavCmd::MAV_CMD_DO_CHANGE_SPEED;
    assert_eq!(
        g.command(change, [0.0, 3.0, 0.0]).await,
        MAV_RESULT_ACCEPTED
    );
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
    // FS_GCS_TIMEOUT 5 s, then FS_TIMEOUT 1.5 s; announced by the radio check, which runs
    // first (failsafe.rs, as the motorboat SITL does with FS_THR_ENABLE on)
    let text = g
        .expect(|m| match m {
            MavMessage::STATUSTEXT(t) => t.text.to_str().ok().map(str::to_owned),
            _ => None,
        })
        .await;
    assert_eq!(text, "Radio Failsafe");
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
        "RTL_ALTITUDE -1: same altitude"
    );
}

/// The Puma's catalog entry as the stand-in gets it (sim/platforms.toml `[puma]`), and the
/// ArduPilot parameters swarm.py gives a Puma stand-in (its `standin_params`).
fn puma() -> Spec {
    Spec {
        name: "Puma 3 AE-class".into(),
        behavior: "plane".into(),
        cruise_mps: 20.0,
        max_mps: 0.0,
        climb_mps: None,
        turn_dps: None,
        loiter_radius_m: None,
        endurance_h: Some(2.5),
        link: Some("los".into()),
    }
}

/// A Puma in the air at 120 m over its home (it spawned there), circling it in LOITER, with
/// `pct` % of its pack left, stepped by hand.
async fn puma_aloft(pct: f64) -> (Vehicle, UdpSocket) {
    puma_aloft_over(pct, None).await
}

/// [`puma_aloft`] over a surface `surface_m` high, as the launcher found it under the spawn.
async fn puma_aloft_over(pct: f64, surface_m: Option<f64>) -> (Vehicle, UdpSocket) {
    let sink = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(("127.0.0.1", port)).await.expect("connect");
    let mut cfg = config(puma(), port, 120.0);
    cfg.surface_m = surface_m;
    cfg.params = [
        ("RTL_AUTOLAND".into(), 1.0),
        ("TKOFF_THR_MINACC".into(), 15.0),
    ]
    .into();
    let mut v = Vehicle::seeded(cfg, sock, None, SEED).expect("vehicle");
    v.link.capture();
    assert!(v.st.armed && v.st.mode == "LOITER", "spawned flying");
    v.battery.consumed_mah = v.capacity_mah() * (1.0 - pct / 100.0);
    (v, sink)
}

fn battery_pct(v: &Vehicle) -> f64 {
    100.0 * (1.0 - v.battery.consumed_mah / v.capacity_mah())
}

/// Step until `done` or `max_s`; the seconds stepped.
fn step_until(v: &mut Vehicle, max_s: f64, done: impl Fn(&Vehicle) -> bool) -> f64 {
    let mut t = 0.0;
    while t < max_s && !done(v) {
        v.step();
        t += 0.1;
    }
    t
}

/// RTL_AUTOLAND from 2 km out: home, the AUTOLAND pattern, down at home, disarmed, the
/// heartbeat saying so; then the pack drains at the disarmed (avionics) load only.
#[tokio::test]
async fn a_puma_sent_home_lands_disarms_and_draws_only_its_avionics_load() {
    let (mut v, _sink) = puma_aloft(25.0).await;
    let home = v.st.home;
    v.st.lon += 2000.0 / (111_320.0 * home.0.to_radians().cos());
    v.set_mode("RTL");
    let start = battery_pct(&v);
    let t_rtl = step_until(&mut v, 600.0, |v| v.st.mode != "RTL");
    assert_eq!(v.st.mode, "AUTOLAND", "after {t_rtl:.0} s of RTL");
    let t_down = step_until(&mut v, 600.0, |v| !v.st.armed);
    assert!(!v.st.armed, "landed and disarmed");
    let d = distance_m(v.st.lat, v.st.lon, home.0, home.1);
    assert!(d < 150.0, "down {d:.0} m from home");
    assert!((v.st.alt - home.2).abs() < 0.01 && v.st.speed < 0.5);
    let used = start - battery_pct(&v);
    // RTL to disarm at about cruise load: 1 % per 90 s
    let flown = t_rtl + t_down;
    assert!(
        (used - flown / 90.0).abs() < 0.6,
        "{used:.2} % in {flown:.0} s"
    );
    assert!(
        battery_pct(&v) > 20.0,
        "home above the reserve: {:.2} %",
        battery_pct(&v)
    );
    let MavMessage::HEARTBEAT(hb) = murmur_standins::telemetry::heartbeat(&v) else {
        panic!("not a heartbeat");
    };
    assert_eq!(hb.custom_mode, 26, "AUTOLAND");
    assert!(
        !hb.base_mode
            .contains(MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED)
    );
    let said = events(&mut v);
    assert!(said.iter().any(|t| t == "Auto disarmed"), "{said:?}");
    // ten minutes on the ground: the idle load (battery.rs IDLE_LOAD, 5 % of cruise's draw)
    let before = v.battery.consumed_mah;
    let at = (v.st.lat, v.st.lon);
    step_until(&mut v, 600.0, |_| false);
    let idle = v.battery.consumed_mah - before;
    let cruise = v.capacity_mah() / (2.5 * 3600.0) * 600.0;
    assert!(
        (idle - 0.05 * cruise).abs() < 0.5,
        "{idle:.1} mAh in 600 s (cruise {cruise:.0})"
    );
    assert!(!v.st.armed && (v.st.lat, v.st.lon) == at && v.st.mode == "AUTOLAND");
}

/// A plane that starts in the air has its home on the surface below it, at the height the
/// launcher found there (Crissy Field's 3 m), says so in HOME_POSITION, and lands on that
/// surface; one that starts on the ground keeps its home where it stands.
#[tokio::test]
async fn a_puma_started_in_the_air_has_its_home_on_the_surface_and_lands_on_it() {
    let (mut v, _sink) = puma_aloft_over(80.0, Some(3.0)).await;
    assert_eq!(v.st.home.2, 3.0);
    assert_eq!(v.st.alt, 120.0, "flying where it spawned");
    let MavMessage::HOME_POSITION(h) =
        murmur_standins::telemetry::build(&v, HOME_POSITION_DATA::ID).expect("home position")
    else {
        panic!("not a home position");
    };
    assert_eq!(h.altitude, 3000, "mm above MSL");
    v.set_mode("RTL");
    step_until(&mut v, 600.0, |v| !v.st.armed);
    assert_eq!(v.st.mode, "AUTOLAND");
    assert!(
        (v.st.alt - 3.0).abs() < 0.01,
        "down on the surface: {}",
        v.st.alt
    );
    // no surface known: the sea's 0 m, as before
    let (v, _sink) = puma_aloft_over(80.0, None).await;
    assert_eq!(v.st.home.2, 0.0);
    // started on the ground: home where it stands, whatever the surface sampled
    let sink = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = sink.local_addr().expect("addr").port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let mut cfg = config(puma(), port, 5.0);
    cfg.surface_m = Some(3.0);
    let v = Vehicle::seeded(cfg, sock, None, SEED).expect("vehicle");
    assert_eq!((v.st.home.2, v.st.armed), (5.0, false));
}

/// The battery's low failsafe (20 %) comes during the landing: ArduPlane never breaks one off
/// (handle_battery_failsafe: already landing in AUTOLAND), so it does not go back to RTL.
#[tokio::test]
async fn the_battery_failsafe_does_not_break_off_a_puma_landing() {
    let (mut v, _sink) = puma_aloft(20.4).await;
    v.set_mode("RTL");
    let mut modes = vec![v.st.mode];
    while v.st.armed && modes.len() < 5 {
        v.step();
        if modes.last() != Some(&v.st.mode) {
            modes.push(v.st.mode);
        }
        assert!(v.now() < 600.0, "never landed: {modes:?}");
    }
    assert_eq!(modes, ["RTL", "AUTOLAND"]);
    let said = events(&mut v);
    let low = said.iter().position(|t| t.starts_with("Battery 1 is low"));
    let flare = said.iter().position(|t| t.starts_with("Flare"));
    assert!(low.is_some() && flare.is_some(), "{said:?}");
}

/// Down again, a Puma refuses the arm an order needs, on the wire: the reason ahead of the
/// FAILED ACK, every time it is asked.
#[tokio::test]
async fn a_landed_puma_refuses_to_arm_and_says_why() {
    let (mut v, _sink) = puma_aloft(80.0).await;
    v.set_mode("RTL");
    step_until(&mut v, 600.0, |v| !v.st.armed);
    assert_eq!(v.st.mode, "AUTOLAND", "a mode ArduPlane arms in");
    v.link.take_captured();
    let arm = MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        target_system: SYSID,
        target_component: 1,
        command: MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
        param1: 1.0,
        ..Default::default()
    });
    for _ in 0..2 {
        v.handle(&arm);
        let mut seen = Vec::new();
        for frame in v.link.take_captured() {
            let mut r = PeekReader::new(&frame[..]);
            match mavlink::read_v2_msg::<MavMessage, _>(&mut r) {
                Ok((_, MavMessage::STATUSTEXT(t))) => {
                    seen.push(t.text.to_str().unwrap_or("").to_owned())
                }
                Ok((_, MavMessage::COMMAND_ACK(a))) => seen.push(format!("{:?}", a.result)),
                _ => {}
            }
        }
        assert_eq!(
            seen,
            [
                "Arm: landed, no crew for a hand launch",
                "MAV_RESULT_FAILED"
            ]
        );
        assert!(!v.st.armed);
    }
}

/// Events as the vehicle sent them since the last call: "IR n" for MISSION_ITEM_REACHED,
/// the STATUSTEXTs (the frames its link handed the socket, in order).
fn events(v: &mut Vehicle) -> Vec<String> {
    let mut out = Vec::new();
    for frame in v.link.take_captured() {
        let mut r = PeekReader::new(&frame[..]);
        match mavlink::read_v2_msg::<MavMessage, _>(&mut r) {
            Ok((_, MavMessage::STATUSTEXT(t))) => out.push(t.text.to_str().unwrap_or("").to_owned()),
            Ok((_, MavMessage::MISSION_ITEM_REACHED(m))) => out.push(format!("IR {}", m.seq)),
            _ => {}
        }
    }
    out
}

/// The motorboat SITL's mission in sim/tests/standins_vs_sitl_plane_boat_sub.py, on the wire:
/// MISSION_ITEM_REACHED for the DO item (it takes the waypoint's), the waypoints and the
/// hold-time waypoint on each pass, AP_Mission's texts in order, "Mission Complete".
#[tokio::test]
async fn boat_mission_with_do_items_twice_as_ardurover_reports_it() {
    let mut s = spec("boat");
    s.cruise_mps = 12.0;
    let (mut v, _sink) = offline(s, 0.0).await;
    let item = |seq: u16, command: MavCmd, north_m: f64, east_m: f64, p1: f32, p2: f32| {
        let located = (north_m, east_m) != (0.0, 0.0);
        MISSION_ITEM_INT_DATA {
            seq,
            command,
            #[allow(deprecated)]
            frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
            x: if located { ((37.8 + north_m / 111_320.0) * 1e7) as i32 } else { 0 },
            y: if located { ((-122.6 + east_m / (111_320.0 * 37.8f64.to_radians().cos())) * 1e7) as i32 } else { 0 },
            param1: p1,
            param2: p2,
            ..Default::default()
        }
    };
    v.mission.install(vec![
        item(0, MavCmd::MAV_CMD_NAV_WAYPOINT, 0.0, 0.0, 0.0, 0.0),
        item(1, MavCmd::MAV_CMD_NAV_WAYPOINT, 0.0, 150.0, 0.0, 0.0),
        item(2, MavCmd::MAV_CMD_DO_CHANGE_SPEED, 0.0, 0.0, 1.0, 6.0),
        item(3, MavCmd::MAV_CMD_NAV_WAYPOINT, 150.0, 150.0, 0.0, 0.0),
        item(4, MavCmd::MAV_CMD_NAV_WAYPOINT, 150.0, 0.0, 5.0, 0.0),
        item(5, MavCmd::MAV_CMD_DO_JUMP, 0.0, 0.0, 1.0, 1.0),
    ]);
    v.st.armed = true;
    v.set_mode("AUTO");
    let mut log = events(&mut v);
    let mut settle = 0;
    for _ in 0..3000 {
        v.step();
        log.extend(events(&mut v));
        if v.mission.complete {
            settle += 1;
            if settle > 10 {
                break;
            }
        }
    }
    let pass = [
        "Mission: 1 WP",
        "IR 2",
        "Reached waypoint #1",
        "Mission: 2 ChangeSpeed",
        "speed: 6.0 m/s",
        "Mission: 3 WP",
        "IR 3",
        "Reached waypoint #3",
        "Mission: 4 WP",
        "Reached waypoint #4. Loiter for 5 seconds",
        "IR 4",
    ];
    let mut want: Vec<&str> = pass.to_vec();
    want.push("Mission: 5 Jump 1/1");
    want.extend(pass);
    want.push("Mission Complete");
    assert_eq!(log, want);
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

/// Step `v` (armed, in `mode`) with the GCS heard once and then silent until its GCS
/// failsafe has had time to act: the modes it went through.
fn lose_the_gcs(v: &mut Vehicle, mode: &'static str) -> Vec<&'static str> {
    v.st.armed = true;
    v.set_mode(mode);
    v.gcs_heartbeat();
    let mut modes = vec![v.st.mode];
    // a 5 s timer (and Rover's 1.5 s FS_TIMEOUT) at the physics rate
    for _ in 0..200 {
        v.step();
        if modes.last() != Some(&v.st.mode) {
            modes.push(v.st.mode);
        }
    }
    modes
}

#[tokio::test]
async fn copter_gcs_failsafe_takes_its_fs_gcs_enable_action() {
    // an engagement's preload for abort or loiter: 7, Brake (else Land)
    let (mut v, _sink) = offline(spec("copter"), 30.0).await;
    v.params.set("FS_GCS_ENABLE", 7.0).expect("set");
    assert_eq!(lose_the_gcs(&mut v, "GUIDED"), ["GUIDED", "BRAKE"]);
    // for return: 1, RTL
    let (mut v, _sink) = offline(spec("copter"), 30.0).await;
    v.params.set("FS_GCS_ENABLE", 1.0).expect("set");
    assert_eq!(lose_the_gcs(&mut v, "GUIDED"), ["GUIDED", "RTL"]);
    // 5: Land
    let (mut v, _sink) = offline(spec("copter"), 30.0).await;
    v.params.set("FS_GCS_ENABLE", 5.0).expect("set");
    assert_eq!(lose_the_gcs(&mut v, "GUIDED"), ["GUIDED", "LAND"]);
    // FS_OPTIONS bit 1 continues a mission in AUTO (an engagement's preload clears it)
    let (mut v, _sink) = offline(spec("copter"), 30.0).await;
    v.params.set("FS_GCS_ENABLE", 7.0).expect("set");
    v.params.set("FS_OPTIONS", 2.0).expect("set");
    assert_eq!(lose_the_gcs(&mut v, "AUTO")[0], "AUTO");
    assert!(v.fs.gcs, "the failsafe is on; the mission continues");
    // ArduCopter's default FS_OPTIONS (bit 4): a pilot-controlled mode carries on
    let (mut v, _sink) = offline(spec("copter"), 30.0).await;
    assert_eq!(v.params.get("FS_OPTIONS"), Some(16.0));
    assert_eq!(lose_the_gcs(&mut v, "LOITER"), ["LOITER"]);
}

/// ArduPilot Blimp 4.8's GCS failsafe (Blimp/events.cpp failsafe_gcs_check) only disarms,
/// and ships off: the stand-in does the same, not the RTL or landing the firmware doesn't
/// have (murmur-3zj).
#[tokio::test]
async fn blimp_gcs_failsafe_is_off_and_only_disarms_when_on() {
    /// `steps` physics steps with the texts they sent, and the modes the vehicle went through.
    fn run(v: &mut Vehicle, steps: usize) -> (Vec<&'static str>, Vec<String>) {
        let mut modes = vec![v.st.mode];
        let mut texts = events(v);
        for _ in 0..steps {
            v.step();
            texts.extend(events(v));
            if modes.last() != Some(&v.st.mode) {
                modes.push(v.st.mode);
            }
        }
        (modes, texts)
    }
    async fn blimp(enable: Option<f32>) -> Vehicle {
        let (mut v, _sink) = offline(spec("blimp"), 30.0).await;
        if let Some(e) = enable {
            v.params.set("FS_GCS_ENABLE", e).expect("set");
        }
        v.st.armed = true;
        v.set_mode("LOITER");
        v.gcs_heartbeat(); // heard once, then silent: 10 s is the 5 s timer twice
        v
    }
    // off, as the SITL Blimp runs: silence changes nothing
    let mut v = blimp(None).await;
    assert_eq!(v.params.get("FS_GCS_ENABLE"), Some(0.0));
    let (modes, texts) = run(&mut v, 100);
    assert_eq!(modes, ["LOITER"]);
    assert!(v.st.armed && !v.fs.gcs, "still armed, no failsafe");
    assert!(
        texts.iter().all(|t| !t.contains("Failsafe") && !t.contains("Disarming")),
        "{texts:?}"
    );
    // on (the documented 5, Land, included): the flag, a disarm, no mode change, no text
    // but the disarm's own
    let mut v = blimp(Some(5.0)).await;
    let (modes, texts) = run(&mut v, 100);
    assert_eq!(modes, ["LOITER"], "no RTL, no LAND");
    assert!(v.fs.gcs && !v.st.armed, "the failsafe flag is up and the motors are disarmed");
    assert!(v.failsafe_active(), "system status critical");
    assert!(texts.iter().any(|t| t == "Disarming motors"), "{texts:?}");
    assert!(texts.iter().all(|t| !t.contains("Failsafe")), "{texts:?}");
    // the GCS heard again: cleared silently
    v.gcs_heartbeat();
    let (_, texts) = run(&mut v, 5);
    assert!(!v.fs.gcs);
    assert!(texts.is_empty(), "{texts:?}");
}

#[tokio::test]
async fn rover_gcs_failsafe_takes_its_fs_action() {
    // an engagement's preload for loiter: FS_ACTION 6, Loiter (else Hold)
    let (mut v, _sink) = offline(spec("boat"), 0.0).await;
    v.params.set("FS_ACTION", 6.0).expect("set");
    assert_eq!(lose_the_gcs(&mut v, "GUIDED"), ["GUIDED", "LOITER"]);
    // for abort: 2, Hold
    let (mut v, _sink) = offline(spec("boat"), 0.0).await;
    v.params.set("FS_ACTION", 2.0).expect("set");
    assert_eq!(lose_the_gcs(&mut v, "GUIDED"), ["GUIDED", "HOLD"]);
}
