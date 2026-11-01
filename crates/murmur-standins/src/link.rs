//! One UDP MAVLink link per vehicle: MAVLink 2 out with its own sequence counter, v1 or v2 in.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use mavlink::dialects::ardupilotmega::{
    COMMAND_ACK_DATA, COMMAND_INT_DATA, COMMAND_LONG_DATA, MISSION_ITEM_INT_DATA, MavMessage,
    MavResult,
};
use mavlink::error::MessageReadError;
use mavlink::{MavHeader, Message, MessageData, peek_reader::PeekReader};
use tokio::net::UdpSocket;

pub const AUTOPILOT_COMPID: u8 = 1;

/// Datagrams sent and failed to send, all links together (the engine's stats line).
pub static SENT: AtomicU64 = AtomicU64::new(0);
pub static SEND_ERRORS: AtomicU64 = AtomicU64::new(0);

pub struct Link {
    pub sock: UdpSocket,
    sysid: u8,
    seq: u8,
    buf: Vec<u8>,
    /// Every frame handed to the socket since [`Link::capture`], for a test to read.
    captured: Option<Vec<Vec<u8>>>,
}

impl Link {
    pub fn new(sock: UdpSocket, sysid: u8) -> Self {
        Link {
            sock,
            sysid,
            seq: 0,
            buf: Vec::with_capacity(280),
            captured: None,
        }
    }

    /// From now on keep a copy of every frame as it is handed to the socket: what a test
    /// that steps a vehicle's physics reads, rather than a loopback socket, whose delivery (a
    /// softirq) lags a loop that runs minutes of physics in milliseconds and drops datagrams
    /// on a loaded host (bd murmur-ek3).
    pub fn capture(&mut self) {
        self.captured.get_or_insert_with(Vec::new);
    }

    /// The frames kept since the last call, in the order they were sent.
    pub fn take_captured(&mut self) -> Vec<Vec<u8>> {
        self.captured.as_mut().map(std::mem::take).unwrap_or_default()
    }

    fn keep(&mut self) {
        if let Some(c) = &mut self.captured {
            c.push(self.buf.clone());
        }
    }

    /// Fire and forget, like a radio: a bridge that isn't up yet just misses it.
    pub fn send(&mut self, msg: &MavMessage) {
        let header = MavHeader {
            system_id: self.sysid,
            component_id: AUTOPILOT_COMPID,
            sequence: self.seq,
        };
        self.buf.clear();
        if mavlink::write_v2_msg(&mut self.buf, header, msg).is_ok() {
            self.keep();
            match self.sock.try_send(&self.buf) {
                Ok(_) => SENT.fetch_add(1, Ordering::Relaxed),
                Err(_) => SEND_ERRORS.fetch_add(1, Ordering::Relaxed),
            };
        }
        self.seq = self.seq.wrapping_add(1);
    }

    /// COMMAND_ACK for a command id the dialect has no name for: rust-mavlink can't encode
    /// such a MavCmd, so the frame is assembled here (same layout and CRC as write_v2_msg).
    pub fn send_ack_for_id(&mut self, command: u16, result: MavResult, to: (u8, u8)) {
        let mut payload = [0u8; 10];
        payload[..2].copy_from_slice(&command.to_le_bytes());
        payload[2] = result as u8;
        payload[8] = to.0;
        payload[9] = to.1;
        // MAVLink 2 drops trailing zero bytes (keeping at least one)
        let len = payload.iter().rposition(|&b| b != 0).map_or(1, |i| i + 1);
        let id = COMMAND_ACK_DATA::ID.to_le_bytes();
        self.buf.clear();
        self.buf.extend_from_slice(&[
            mavlink::MAV_STX_V2,
            len as u8,
            0,
            0,
            self.seq,
            self.sysid,
            AUTOPILOT_COMPID,
            id[0],
            id[1],
            id[2],
        ]);
        self.buf.extend_from_slice(&payload[..len]);
        let crc = mavlink::calculate_crc(&self.buf[1..], COMMAND_ACK_DATA::EXTRA_CRC);
        self.buf.extend_from_slice(&crc.to_le_bytes());
        self.keep();
        match self.sock.try_send(&self.buf) {
            Ok(_) => SENT.fetch_add(1, Ordering::Relaxed),
            Err(_) => SEND_ERRORS.fetch_add(1, Ordering::Relaxed),
        };
        self.seq = self.seq.wrapping_add(1);
    }
}

/// One inbound message: decoded, or one the dialect can't decode whose ids still matter.
#[derive(Debug, Clone)]
// the decoded message is the common case; boxing it would cost an allocation per message
#[allow(clippy::large_enum_variant)]
pub enum Inbound {
    Msg(MavHeader, MavMessage),
    /// COMMAND_LONG / COMMAND_INT with a command id rust-mavlink has no variant for
    UnknownCommand {
        header: MavHeader,
        command: u16,
        target: (u8, u8),
    },
    /// MISSION_ITEM_INT that doesn't decode (unknown command or frame)
    UnknownMissionItem {
        header: MavHeader,
        seq: u16,
    },
}

/// A UDP socket connected to the bridge's port for this vehicle, sending from `src_port` when
/// given (a fixed source port, [`src_port`]), else from an ephemeral one. No explicit bind
/// without one: an explicit bind to port 0 took ~12 ms each here (small ephemeral range),
/// connect's autobind is instant. A fixed port takes nothing from the host's ephemeral range,
/// which every autobound stand-in shares with everything else (about 4,000 ports here).
pub fn connect(host: &str, port: u16, src_port: Option<u16>) -> std::io::Result<UdpSocket> {
    use std::net::ToSocketAddrs;
    let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no address for {host}"),
        )
    })?;
    let sock = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        None,
    )?;
    if let Some(src) = src_port {
        // the address connect's autobind would take: loopback to a loopback peer, else any
        let local: std::net::SocketAddr = match addr.ip() {
            ip if ip.is_loopback() => (ip, src).into(),
            std::net::IpAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, src).into(),
            std::net::IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, src).into(),
        };
        sock.bind(&local.into())
            .map_err(|e| std::io::Error::new(e.kind(), format!("bind source port {src}: {e}")))?;
    }
    sock.connect(&addr.into())?;
    sock.set_nonblocking(true)?;
    UdpSocket::from_std(sock.into())
}

/// The fixed source port of the `index`th stand-in (0-based, in the file's order) when
/// `MURMUR_STANDIN_SRC_BASE` is set: base + index. None (unset or empty): an ephemeral port, as
/// before. A base that is not a port, or a range past 65535, is an error.
pub fn src_port(base: Option<&str>, index: usize) -> Result<Option<u16>, String> {
    let Some(base) = base.map(str::trim).filter(|b| !b.is_empty()) else {
        return Ok(None);
    };
    let b: u16 = base
        .parse()
        .map_err(|_| format!("MURMUR_STANDIN_SRC_BASE={base}: not a UDP port"))?;
    u16::try_from(usize::from(b) + index).map(Some).map_err(|_| {
        format!("MURMUR_STANDIN_SRC_BASE={base}: stand-in {index} would be past port 65535")
    })
}

/// Every MAVLink message in one datagram. Garbage and unknown message ids are skipped.
pub fn parse(datagram: &[u8]) -> Vec<(MavHeader, MavMessage)> {
    parse_all(datagram)
        .into_iter()
        .filter_map(|m| match m {
            Inbound::Msg(h, m) => Some((h, m)),
            _ => None,
        })
        .collect()
}

/// Every MAVLink message in one datagram, including commands and mission items whose enum
/// values the dialect doesn't know (ArduPilot answers those; so must a stand-in).
pub fn parse_all(datagram: &[u8]) -> Vec<Inbound> {
    let mut reader = PeekReader::new(Cursor::new(datagram));
    let mut out = Vec::new();
    loop {
        let raw = match mavlink::read_any_raw_message::<MavMessage, _>(&mut reader) {
            Ok(raw) => raw,
            Err(MessageReadError::Io(_)) => break,
            Err(_) => continue,
        };
        let header = MavHeader {
            sequence: raw.sequence(),
            system_id: raw.system_id(),
            component_id: raw.component_id(),
        };
        let id = raw.message_id();
        match MavMessage::parse(raw.version(), id, raw.payload()) {
            Ok(m) => out.push(Inbound::Msg(header, m)),
            Err(_) => {
                // MAVLink 2 trims trailing zeros: pad back to the full payload
                let mut p = [0u8; 64];
                let n = raw.payload().len().min(64);
                p[..n].copy_from_slice(&raw.payload()[..n]);
                let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
                if id == COMMAND_LONG_DATA::ID || id == COMMAND_INT_DATA::ID {
                    // both: 28 bytes of params/coordinates, then command, target sys/comp
                    out.push(Inbound::UnknownCommand {
                        header,
                        command: u16_at(28),
                        target: (p[30], p[31]),
                    });
                } else if id == MISSION_ITEM_INT_DATA::ID {
                    out.push(Inbound::UnknownMissionItem {
                        header,
                        seq: u16_at(28),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod src_tests {
    use super::*;

    #[test]
    fn source_ports_are_base_plus_index_or_ephemeral() {
        assert_eq!(src_port(None, 3), Ok(None));
        assert_eq!(src_port(Some(" "), 3), Ok(None));
        assert_eq!(src_port(Some("10000"), 0), Ok(Some(10000)));
        assert_eq!(src_port(Some("10000"), 3599), Ok(Some(13599)));
        assert!(src_port(Some("ten"), 0).is_err());
        assert!(src_port(Some("65000"), 600).is_err());
    }

    #[tokio::test]
    async fn a_fixed_source_port_is_what_the_far_end_sees() {
        let far = std::net::UdpSocket::bind("127.0.0.1:0").expect("far end");
        let to = far.local_addr().expect("addr").port();
        // a free port to use as the fixed source: bound, then released
        let src = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.local_addr())
            .expect("probe")
            .port();
        let sock = connect("127.0.0.1", to, Some(src)).expect("connect");
        sock.send(b"x").await.expect("send");
        let mut buf = [0u8; 4];
        let (_, from) = far.recv_from(&mut buf).expect("recv");
        assert_eq!(from.port(), src);
        // the port is the stand-in's while it holds it
        assert!(connect("127.0.0.1", to, Some(src)).is_err());
    }
}
