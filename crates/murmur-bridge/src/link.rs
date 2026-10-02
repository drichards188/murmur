//! One UDP MAVLink link per vehicle and its receive-side health.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use mavlink::Message;
use serde::Serialize;
use tokio::net::UdpSocket;

use crate::clock::{self, round, round_int};
use crate::mav::{self, MavMessage, apm};
use crate::vehicle::VehicleCell;
use crate::world::World;

pub const LOSS_WINDOW_S: i64 = 10;
/// Pings that never came back are forgotten after this long.
const TIMESYNC_FORGET_S: f64 = 10.0;

/// Somewhere MAVLink can be sent: a vehicle's UDP link, or a fake one in tests.
pub trait Transmit: Send + Sync {
    /// Encode and send as the GCS (255/190).
    fn send(&self, msg: &MavMessage);
    /// Relay a frame as-is (keeps the original sender's sysid), e.g. positions to a tracker.
    fn send_raw(&self, frame: &[u8]);
    /// Identifies the link (its UDP port), so a relay can be removed again.
    fn port(&self) -> u16;
}

/// What the console shows about a link. Changes only when a packet arrives or the loss/RTT
/// estimate moves, so the bridge can send it only on change; the client works out age from
/// last_rx_t.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkSnapshot {
    pub last_rx_t: Option<f64>,
    pub loss_pct: Option<i64>,
    pub rtt_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkCounters {
    pub rx_total: u64,
    pub tx_total: u64,
    pub by_type: BTreeMap<&'static str, u64>,
}

/// Receive-side link health: silence, packet loss from MAVLink sequence gaps, RTT.
#[derive(Debug, Default)]
pub struct LinkStats {
    last_rx_wall: Option<f64>,
    last_seq: Option<u8>,
    /// (monotonic second, received, expected)
    buckets: VecDeque<(i64, u32, u32)>,
    pub rtt_ms: Option<f64>,
    pub rx_total: u64,
    pub tx_total: u64,
    pub by_type: BTreeMap<&'static str, u64>,
    /// TIMESYNC ts1 -> monotonic send time
    timesync_sent: HashMap<i64, f64>,
}

impl LinkStats {
    /// A packet from the autopilot component, with its MAVLink sequence number.
    pub fn on_packet(&mut self, seq: u8, now: f64, wall: f64) {
        self.last_rx_wall = Some(wall);
        self.rx_total += 1;
        let expected = match self.last_seq {
            None => 1,
            Some(last) => {
                let gap = seq.wrapping_sub(last);
                if gap == 0 || gap > 128 {
                    // duplicate or reordered packet (a jump "backwards"), not loss.
                    // Counting it as a 250-packet gap would wreck the estimate.
                    return;
                }
                u32::from(gap)
            }
        };
        self.last_seq = Some(seq);
        let sec = now.floor() as i64;
        match self.buckets.back_mut() {
            Some(b) if b.0 == sec => {
                b.1 += 1;
                b.2 += expected;
            }
            _ => self.buckets.push_back((sec, 1, expected)),
        }
        while self
            .buckets
            .front()
            .is_some_and(|b| b.0 <= sec - LOSS_WINDOW_S)
        {
            self.buckets.pop_front();
        }
    }

    pub fn snapshot(&self, now: f64) -> LinkSnapshot {
        let recent = self
            .buckets
            .iter()
            .filter(|b| b.0 as f64 > now - LOSS_WINDOW_S as f64);
        let (received, expected) = recent.fold((0u32, 0u32), |(r, e), b| (r + b.1, e + b.2));
        LinkSnapshot {
            last_rx_t: self.last_rx_wall.map(|t| round(t, 1)),
            loss_pct: (expected > 0)
                .then(|| round_int(100.0 * f64::from(expected - received) / f64::from(expected))),
            rtt_ms: self.rtt_ms.map(round_int),
        }
    }

    pub fn counters(&self) -> LinkCounters {
        LinkCounters {
            rx_total: self.rx_total,
            tx_total: self.tx_total,
            by_type: self.by_type.clone(),
        }
    }

    /// Remember a TIMESYNC ping by its ts1.
    pub fn ping(&mut self, ts1: i64, now: f64) {
        self.timesync_sent.insert(ts1, now);
        self.timesync_sent
            .retain(|_, sent| *sent >= now - TIMESYNC_FORGET_S);
    }

    /// A TIMESYNC reply (tc1 != 0) for one of our pings.
    pub fn pong(&mut self, ts1: i64, now: f64) {
        if let Some(sent) = self.timesync_sent.remove(&ts1) {
            self.rtt_ms = Some((now - sent) * 1000.0);
        }
    }
}

/// UDP endpoint for one vehicle. The vehicle sends to us; we reply to its source address.
pub struct Link {
    pub port: u16,
    socket: UdpSocket,
    remote: Mutex<Option<SocketAddr>>,
    seq: AtomicU8,
    pub stats: Arc<Mutex<LinkStats>>,
    pub vehicle: OnceLock<Arc<VehicleCell>>,
}

impl Link {
    pub async fn bind(port: u16) -> std::io::Result<Arc<Link>> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).await?;
        Ok(Arc::new(Link {
            port,
            socket,
            remote: Mutex::new(None),
            seq: AtomicU8::new(0),
            stats: Arc::default(),
            vehicle: OnceLock::new(),
        }))
    }

    fn remote(&self) -> Option<SocketAddr> {
        *self.remote.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stats(&self) -> std::sync::MutexGuard<'_, LinkStats> {
        self.stats.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn send_timesync(&self) {
        let ts1 = (clock::wall() * 1e9) as i64;
        self.stats().ping(ts1, clock::mono());
        self.send(&MavMessage::TIMESYNC(apm::TIMESYNC_DATA {
            tc1: 0,
            ts1,
            ..Default::default()
        }));
    }

    /// Receive loop: link stats for every autopilot packet, then the packet to its vehicle.
    /// The first autopilot HEARTBEAT binds the link to a vehicle (resolved by the world).
    pub async fn run(self: Arc<Self>, world: Arc<World>) {
        let mut buf = vec![0u8; 65536];
        loop {
            let (n, addr) = match self.socket.recv_from(&mut buf).await {
                Ok(r) => r,
                // e.g. ICMP port unreachable surfacing as an error on Linux: not fatal
                Err(_) => continue,
            };
            *self.remote.lock().unwrap_or_else(|e| e.into_inner()) = Some(addr);
            let frames = mav::decode(&buf[..n]);
            if frames.is_empty() {
                continue;
            }
            let (now, wall) = (clock::mono(), clock::wall());
            {
                let mut stats = self.stats();
                for f in &frames {
                    if f.compid == mav::AUTOPILOT_COMPID {
                        stats.on_packet(f.seq, now, wall);
                    }
                    *stats.by_type.entry(f.msg.message_name()).or_default() += 1;
                }
            }
            for f in frames {
                let vehicle = match self.vehicle.get() {
                    Some(v) => v,
                    None => {
                        if !matches!(f.msg, MavMessage::HEARTBEAT(_))
                            || f.compid != mav::AUTOPILOT_COMPID
                        {
                            continue;
                        }
                        let v = world.on_new_vehicle(&self, f.sysid);
                        self.vehicle.get_or_init(|| v)
                    }
                };
                if f.sysid == vehicle.sysid {
                    vehicle.handle(&f.msg, &f.raw, now);
                }
            }
        }
    }
}

impl Transmit for Link {
    fn send(&self, msg: &MavMessage) {
        let Some(remote) = self.remote() else { return };
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let buf = mav::encode(msg, seq);
        if self.socket.try_send_to(&buf, remote).is_ok() {
            self.stats().tx_total += 1;
        }
    }

    fn send_raw(&self, frame: &[u8]) {
        let Some(remote) = self.remote() else { return };
        if self.socket.try_send_to(frame, remote).is_ok() {
            self.stats().tx_total += 1;
        }
    }

    fn port(&self) -> u16 {
        self.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(s: &mut LinkStats, seqs: &[u8], t: f64) {
        for &q in seqs {
            s.on_packet(q, t, 1000.0 + t);
        }
    }

    #[test]
    fn no_loss_when_contiguous() {
        let mut s = LinkStats::default();
        feed(&mut s, &[10, 11, 12, 13], 5.0);
        let snap = s.snapshot(5.5);
        assert_eq!(snap.loss_pct, Some(0));
        assert_eq!(snap.last_rx_t, Some(1005.0));
        assert_eq!(s.rx_total, 4);
    }

    #[test]
    fn gaps_count_as_loss() {
        let mut s = LinkStats::default();
        // 0, then 4 (3 lost), then 5: received 3 of expected 1 + 4 + 1 = 6
        feed(&mut s, &[0, 4, 5], 1.0);
        assert_eq!(s.snapshot(1.2).loss_pct, Some(50));
    }

    #[test]
    fn reorder_and_duplicates_are_not_loss() {
        let mut s = LinkStats::default();
        feed(&mut s, &[1, 2, 3, 3, 2, 4], 1.0);
        // the duplicate 3 and the late 2 are ignored, not counted as 255/254-packet gaps
        assert_eq!(s.snapshot(1.0).loss_pct, Some(0));
        assert_eq!(s.rx_total, 6);
    }

    #[test]
    fn sequence_wraps() {
        let mut s = LinkStats::default();
        feed(&mut s, &[253, 254, 255, 0, 1], 1.0);
        assert_eq!(s.snapshot(1.0).loss_pct, Some(0));
        feed(&mut s, &[3], 1.0); // seq 2 lost
        assert_eq!(s.snapshot(1.0).loss_pct, Some(14)); // 6 of 7
    }

    #[test]
    fn window_forgets_old_seconds() {
        let mut s = LinkStats::default();
        feed(&mut s, &[0, 50], 1.0); // big loss at t=1
        feed(&mut s, &[51, 52], 20.0);
        assert_eq!(s.snapshot(20.0).loss_pct, Some(0));
        // nothing heard for a while: no estimate rather than a stale one
        assert_eq!(s.snapshot(40.0).loss_pct, None);
    }

    #[test]
    fn timesync_rtt() {
        let mut s = LinkStats::default();
        s.ping(42, 1.0);
        s.pong(41, 1.1); // not ours
        assert_eq!(s.rtt_ms, None);
        s.pong(42, 1.025);
        assert_eq!(s.snapshot(1.1).rtt_ms, Some(25));
        s.ping(43, 2.0);
        s.ping(44, 13.0); // 43 is forgotten
        s.pong(43, 13.1);
        assert_eq!(s.snapshot(13.1).rtt_ms, Some(25));
    }
}
