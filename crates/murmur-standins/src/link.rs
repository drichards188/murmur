//! One UDP MAVLink link per vehicle: MAVLink 2 out with its own sequence counter, v1 or v2 in.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use mavlink::dialects::ardupilotmega::MavMessage;
use mavlink::error::MessageReadError;
use mavlink::{MavHeader, peek_reader::PeekReader};
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
}

impl Link {
    pub fn new(sock: UdpSocket, sysid: u8) -> Self {
        Link {
            sock,
            sysid,
            seq: 0,
            buf: Vec::with_capacity(280),
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
            match self.sock.try_send(&self.buf) {
                Ok(_) => SENT.fetch_add(1, Ordering::Relaxed),
                Err(_) => SEND_ERRORS.fetch_add(1, Ordering::Relaxed),
            };
        }
        self.seq = self.seq.wrapping_add(1);
    }
}

/// A UDP socket connected to the bridge's port for this vehicle. No explicit bind: an explicit
/// bind to port 0 took ~12 ms each here (small ephemeral range), connect's autobind is instant.
pub fn connect(host: &str, port: u16) -> std::io::Result<UdpSocket> {
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
    sock.connect(&addr.into())?;
    sock.set_nonblocking(true)?;
    UdpSocket::from_std(sock.into())
}

/// Every MAVLink message in one datagram. Garbage and unknown message ids are skipped.
pub fn parse(datagram: &[u8]) -> Vec<(MavHeader, MavMessage)> {
    let mut reader = PeekReader::new(Cursor::new(datagram));
    let mut out = Vec::new();
    loop {
        match mavlink::read_any_msg::<MavMessage, _>(&mut reader) {
            Ok(m) => out.push(m),
            Err(MessageReadError::Io(_)) => break,
            Err(_) => continue,
        }
    }
    out
}
