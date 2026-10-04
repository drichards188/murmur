//! One UDP MAVLink link per vehicle and its receive-side health.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use mavlink::Message;
use serde::Serialize;
use tokio::net::UdpSocket;

use crate::clock::{self, round, round_int};
use crate::mav::{self, MavMessage, apm};
use crate::vehicle::VehicleCell;
use crate::world::World;

pub const LOSS_WINDOW_S: i64 = 10;
/// TIMESYNC pings that never came back are forgotten after this long on a fast link.
const TIMESYNC_FORGET_S: f64 = 10.0;

/// How often the bridge pings a link with TIMESYNC and how long it waits for the answer.
/// A link class with a low rate or long queues (an acoustic modem: 300 bps, a 256-byte
/// queue that alone holds ~7 s of airtime, sound at 1500 m/s) answers after more than the
/// 10 s a radio link gets, and a ping every second would fill its downlink by itself, so slow
/// classes are pinged less often and waited for longer (murmur-0nv.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimesyncPolicy {
    /// Seconds between pings (housekeeping ticks).
    pub every_s: u64,
    /// Seconds a ping is remembered for its answer.
    pub forget_s: f64,
}

impl Default for TimesyncPolicy {
    fn default() -> TimesyncPolicy {
        TimesyncPolicy {
            every_s: 1,
            forget_s: TIMESYNC_FORGET_S,
        }
    }
}

impl TimesyncPolicy {
    /// By the platform's link class from the fleet manifest (`link`).
    pub fn for_class(class: Option<&str>) -> TimesyncPolicy {
        match class {
            // ~37 bytes/s of capacity: one ping (~30 bytes) every 30 s; queueing and
            // propagation can take tens of seconds, so wait up to five minutes.
            Some("acoustic") => TimesyncPolicy {
                every_s: 30,
                forget_s: 300.0,
            },
            // GEO and Iridium: ~0.6-2 s round trips plus queueing on the narrow classes.
            Some("satcom") => TimesyncPolicy {
                every_s: 2,
                forget_s: 60.0,
            },
            _ => TimesyncPolicy::default(),
        }
    }
}
/// A streaming link hears at least the 1 Hz HEARTBEAT.
pub const STREAMING_INTERVAL_MS: u32 = 1000;
/// A link that has gone over to HIGH_LATENCY2 (an AUV on an acoustic modem under water)
/// reports this often: the stand-ins' `ACOUSTIC_STATUS_S`, from survey AUVs reporting status
/// acoustically every few tens of seconds (see crates/murmur-standins/src/telemetry.rs).
pub const HIGH_LATENCY_INTERVAL_MS: u32 = 20_000;
/// A link counts as lost after this many expected intervals of silence: 3 s when streaming
/// (the inbox's long-standing "no telemetry for more than 3 s"), 60 s on an acoustic link.
pub const STALE_INTERVALS: f64 = 3.0;

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
    /// How often this link should be heard from: 1 s streaming, 20 s on HIGH_LATENCY2.
    /// Silence longer than `STALE_INTERVALS` of these is a lost link.
    pub expected_interval_ms: u32,
    /// The link emulator's state for this link (absent without an emulator): whether it is
    /// degrading the link now and why. Details at `GET /api/links/{uid}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emu: Option<crate::emu::EmuBrief>,
    /// The telemetry plan the bridge chose for this link (`crate::telemetry`): tier, the
    /// profile it was chosen for, why, and what it costs.
    pub telemetry: Option<crate::telemetry::PlanChoice>,
    /// The task layer's command ACK timeout on this link now (`LinkTiming::ack_timeout`),
    /// rounded to 100 ms.
    pub ack_timeout_ms: u32,
}

impl LinkSnapshot {
    /// Seconds of silence after which the link counts as lost.
    pub fn stale_after_s(&self) -> f64 {
        STALE_INTERVALS * f64::from(self.expected_interval_ms) / 1000.0
    }
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
    /// The vehicle reports by HIGH_LATENCY2 instead of streaming (low-rate link).
    pub high_latency: bool,
    /// An AUV on an acoustic link that may dive at any moment (running a mission, or already
    /// below the surface): its silence is first a dive, and it reports every 20 s from there,
    /// so the link counts as lost only after missed reports (set by the world's housekeeping).
    pub dive_expected: bool,
    /// Ping rate and answer timeout for this link's class.
    pub timesync: TimesyncPolicy,
    /// The link emulator's brief view of this link, when one runs (set by `crate::emu`).
    pub emu: Option<crate::emu::EmuBrief>,
    /// The emulator's figures the timing and the telemetry plan use (set by `crate::emu`).
    pub emu_link: Option<EmuLink>,
    /// Smoothed TIMESYNC round trip and its mean deviation, s (RFC 6298).
    pub srtt_s: Option<f64>,
    pub rttvar_s: f64,
    /// Last autopilot HEARTBEAT (monotonic s) and whether heartbeats are arriving at the
    /// streaming rate (two within `STALE_INTERVALS` s).
    last_heartbeat: Option<f64>,
    heartbeat_streaming: bool,
    /// The telemetry plan chosen for this link (`crate::telemetry::choose`) and what was
    /// last requested from the vehicle.
    pub plan: Option<crate::telemetry::PlanChoice>,
    pub plan_sent: Option<Vec<(u32, f64)>>,
    /// A queue demotion of the plan holds until this monotonic second.
    pub queue_hold_until: Option<f64>,
}

/// What the link emulator says about a link that matters for timing and the telemetry plan
/// (from its `/stats`, via `/api/links`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmuLink {
    pub profile: Option<String>,
    /// The profile carrying traffic now (differs from `profile` for a gated link: a surfaced
    /// AUV on `acoustic_surface`).
    pub active_profile: Option<String>,
    /// `nominal.bandwidth_bps` of `profile`.
    pub bandwidth_bps: Option<u32>,
    /// Added delay up plus down over the recent window, ms.
    pub rtt_ms: Option<f64>,
    /// Recent uplink (vehicle to bridge) packets and the queue drops among them.
    pub up_pkts: u64,
    pub up_queue_drops: u64,
}

/// Floor of every timeout, s: RFC 6298's minimum RTO, and what the task layer always used on
/// fast links.
pub const MIN_TIMEOUT_S: f64 = 1.0;
/// Ceiling of every single wait, s, however slow the link.
pub const MAX_TIMEOUT_S: f64 = 30.0;
/// Allowance for the autopilot to act on a message and answer (a mode change, a mission
/// item written to storage), s. GUESS.
pub const PROCESSING_S: f64 = 0.25;
/// ArduPilot re-requests a mission item after 1 s without it (MissionItemProtocol.cpp
/// `wp_recv_timeout_ms`; the stand-ins' `UPLOAD_RETRY_S`).
pub const VEHICLE_REREQUEST_S: f64 = 1.0;
/// The mission-item wait never drops below what the bridge always used.
pub const MIN_ITEM_TIMEOUT_S: f64 = 1.5;
/// Each retry of a command waits this much longer than the last (RFC 6298 doubles; a slow
/// link loses packets more than it congests, so a gentler step).
pub const BACKOFF: f64 = 1.5;
/// NOT MOVING after this long without movement on a streaming link (`tasks::exec`).
pub const STALL_AFTER_S: f64 = 10.0;

/// Protocol timing for one link: how long the task layer waits for an answer.
///
/// The round trip `rtt` and its variation `var` come from, in order:
/// 1. TIMESYNC, smoothed as TCP does (RFC 6298: `srtt = 7/8 srtt + 1/8 r`,
///    `rttvar = 3/4 rttvar + 1/4 |srtt - r|`, the first sample sets `srtt = r, rttvar = r/2`),
///    unless the emulator's figure is more than twice it (the link just changed, an AUV
///    dived: TIMESYNC on a slow class answers every 30 s and has not caught up);
/// 2. the link emulator's added delay up plus down (`emu_rtt_ms`), with `var = rtt/2`;
/// 3. the link profile's typical round trip (`telemetry::LINKS`), with `var = rtt/2`;
/// 4. nothing known: 0, and the floors apply.
///
/// Then, with `rto = rtt + 4 var` (RFC 6298's retransmission timeout; 3 x rtt before any
/// variation is measured):
/// - ACK timeout for try `n` (0-based) = `min(max(1 s, rto + 0.25 s) x 1.5^n, 30 s)`;
/// - mission item timeout = `clamp(rto + 0.25 s + 1 s, 1.5 s, 30 s)`: the vehicle re-requests
///   a missing item after 1 s itself, so silence for that long means the request, our item
///   and its re-request were all lost;
/// - an item is "in flight" (a re-request for it crossed it and is not answered again) for
///   `rtt + var`;
/// - NOT MOVING after `max(10 s, 2 x report + rto)`, `report` being the seconds between
///   position/speed reports (1 s streaming, 5 s on HIGH_LATENCY2 at the minimal plan, 20 s
///   from an AUV under water);
/// - a state the vehicle reports back (armed) is waited for `max(5 s, 2 x report + rto)`.
///
/// On a LOS radio (30 ms) every figure is the floor, as before; on Iridium (1.4 s measured,
/// ~0.15 s deviation) the ACK timeout is ~2.3 s, and ~4.5 s before the first TIMESYNC answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkTiming {
    pub rtt_s: f64,
    pub var_s: f64,
    /// timesync | emulator | profile | none
    pub source: &'static str,
    /// Seconds between position/speed reports.
    pub report_s: f64,
    /// Seconds between heartbeats (1 streaming, 20 from an AUV under water).
    pub heartbeat_s: f64,
}

impl Default for LinkTiming {
    fn default() -> LinkTiming {
        LinkTiming {
            rtt_s: 0.0,
            var_s: 0.0,
            source: "none",
            report_s: 1.0,
            heartbeat_s: 1.0,
        }
    }
}

impl LinkTiming {
    pub fn rto_s(&self) -> f64 {
        self.rtt_s + 4.0 * self.var_s
    }

    pub fn ack_timeout(&self, attempt: u32) -> Duration {
        let base = (self.rto_s() + PROCESSING_S).max(MIN_TIMEOUT_S);
        let t = base * BACKOFF.powi(attempt.min(16) as i32);
        Duration::from_secs_f64(t.min(MAX_TIMEOUT_S))
    }

    pub fn item_timeout(&self) -> Duration {
        let t = self.rto_s() + PROCESSING_S + VEHICLE_REREQUEST_S;
        Duration::from_secs_f64(t.clamp(MIN_ITEM_TIMEOUT_S, MAX_TIMEOUT_S))
    }

    pub fn in_flight(&self) -> Duration {
        Duration::from_secs_f64((self.rtt_s + self.var_s).min(MAX_TIMEOUT_S))
    }

    pub fn stall_after_s(&self) -> f64 {
        STALL_AFTER_S.max(2.0 * self.report_s + self.rto_s())
    }

    pub fn confirm_wait(&self) -> Duration {
        let t = (2.0 * self.report_s.max(self.heartbeat_s) + self.rto_s()).max(5.0);
        Duration::from_secs_f64(t)
    }
}

impl LinkStats {
    /// What kind of reporting an autopilot message shows: HIGH_LATENCY2 means the vehicle
    /// is on its low-rate link; a stream message (position) means it is streaming again.
    ///
    /// A vehicle asked for HIGH_LATENCY2 on a link that still streams (the minimal plan on
    /// Iridium) keeps its 1 Hz HEARTBEAT, so heartbeats at the streaming rate also mean
    /// streaming: the lost-link check stays at 3 s there. An AUV under water sends its
    /// heartbeat only with each HIGH_LATENCY2, 20 s apart.
    pub fn on_message(&mut self, msg: &MavMessage, now: f64) {
        match msg {
            MavMessage::HEARTBEAT(_) => {
                let gap = self.last_heartbeat.map(|t| now - t);
                self.last_heartbeat = Some(now);
                self.heartbeat_streaming = gap.is_some_and(|g| {
                    g <= STALE_INTERVALS * f64::from(STREAMING_INTERVAL_MS) / 1000.0
                });
                if self.heartbeat_streaming {
                    self.high_latency = false;
                }
            }
            MavMessage::HIGH_LATENCY2(_) => {
                if !self.heartbeat_streaming {
                    self.high_latency = true;
                }
            }
            MavMessage::GLOBAL_POSITION_INT(_) => self.high_latency = false,
            _ => {}
        }
    }

    /// The timing the task layer uses on this link now (see `LinkTiming`).
    pub fn timing(&self) -> LinkTiming {
        let emu = self
            .emu_link
            .as_ref()
            .and_then(|e| e.rtt_ms)
            .map(|ms| ms / 1000.0);
        let (rtt_s, var_s, source) = match (self.srtt_s, emu) {
            (Some(s), Some(e)) if e > 2.0 * s => (e, e / 2.0, "emulator"),
            (Some(s), _) => (s, self.rttvar_s, "timesync"),
            (None, Some(e)) => (e, e / 2.0, "emulator"),
            (None, None) => match self.plan.as_ref().and_then(|p| p.nominal_rtt_s) {
                Some(n) => (n, n / 2.0, "profile"),
                None => (0.0, 0.0, "none"),
            },
        };
        let heartbeat_s = f64::from(self.expected_interval_ms()) / 1000.0;
        LinkTiming {
            rtt_s,
            var_s,
            source,
            report_s: self
                .plan
                .as_ref()
                .map_or(1.0, |p| p.report_s)
                .max(heartbeat_s),
            heartbeat_s,
        }
    }

    pub fn expected_interval_ms(&self) -> u32 {
        if self.high_latency || self.dive_expected {
            HIGH_LATENCY_INTERVAL_MS
        } else {
            STREAMING_INTERVAL_MS
        }
    }

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
            expected_interval_ms: self.expected_interval_ms(),
            emu: self.emu.clone(),
            telemetry: self.plan.clone(),
            ack_timeout_ms: (self.timing().ack_timeout(0).as_secs_f64() * 10.0).round() as u32
                * 100,
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
        let forget = self.timesync.forget_s;
        self.timesync_sent.retain(|_, sent| *sent >= now - forget);
    }

    /// A TIMESYNC reply (tc1 != 0) for one of our pings.
    pub fn pong(&mut self, ts1: i64, now: f64) {
        if let Some(sent) = self.timesync_sent.remove(&ts1) {
            let r = now - sent;
            self.rtt_ms = Some(r * 1000.0);
            match self.srtt_s {
                None => {
                    self.srtt_s = Some(r);
                    self.rttvar_s = r / 2.0;
                }
                Some(s) => {
                    self.rttvar_s = 0.75 * self.rttvar_s + 0.25 * (s - r).abs();
                    self.srtt_s = Some(0.875 * s + 0.125 * r);
                }
            }
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
    /// Seconds between GCS heartbeats on this link (the telemetry plan's `gcs_heartbeat_s`),
    /// read by the heartbeat thread without taking the stats lock.
    gcs_heartbeat_s: AtomicU64,
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
            gcs_heartbeat_s: AtomicU64::new(1),
        }))
    }

    pub fn gcs_heartbeat_s(&self) -> u64 {
        self.gcs_heartbeat_s.load(Ordering::Relaxed).max(1)
    }

    pub fn set_gcs_heartbeat_s(&self, s: u64) {
        self.gcs_heartbeat_s.store(s.max(1), Ordering::Relaxed);
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
                        stats.on_message(&f.msg, now);
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
    fn high_latency_link_is_stale_later() {
        let mut s = LinkStats::default();
        assert_eq!(s.snapshot(0.0).stale_after_s(), 3.0);
        s.on_message(&MavMessage::HIGH_LATENCY2(Default::default()), 0.0);
        let snap = s.snapshot(0.0);
        assert_eq!(snap.expected_interval_ms, 20_000);
        assert_eq!(snap.stale_after_s(), 60.0);
        // heartbeats come with the acoustic status (20 s apart) and don't end it
        s.on_message(&MavMessage::HEARTBEAT(Default::default()), 20.0);
        s.on_message(&MavMessage::HIGH_LATENCY2(Default::default()), 20.0);
        s.on_message(&MavMessage::HEARTBEAT(Default::default()), 40.0);
        assert!(s.high_latency);
        s.on_message(&MavMessage::GLOBAL_POSITION_INT(Default::default()), 41.0);
        assert_eq!(s.snapshot(0.0).stale_after_s(), 3.0);
    }

    #[test]
    fn an_auv_that_may_dive_is_given_its_report_interval() {
        // streaming at the surface, but running a mission at depth: the next thing heard
        // may be the first report from under water, 20 s on
        let mut s = LinkStats {
            dive_expected: true,
            ..LinkStats::default()
        };
        s.on_message(&MavMessage::HEARTBEAT(Default::default()), 0.0);
        s.on_message(&MavMessage::HEARTBEAT(Default::default()), 1.0);
        assert!(!s.high_latency);
        assert_eq!(s.snapshot(1.0).stale_after_s(), 60.0);
    }

    #[test]
    fn high_latency2_with_streaming_heartbeats_is_still_streaming() {
        // the minimal plan: HIGH_LATENCY2 every 5 s, HEARTBEAT every second
        let mut s = LinkStats::default();
        for i in 0..20 {
            let t = f64::from(i);
            s.on_message(&MavMessage::HEARTBEAT(Default::default()), t);
            if i % 5 == 0 {
                s.on_message(&MavMessage::HIGH_LATENCY2(Default::default()), t + 0.1);
            }
            if i > 0 {
                assert!(!s.high_latency, "t={t}");
            }
        }
        assert_eq!(s.snapshot(20.0).stale_after_s(), 3.0);
        // the heartbeats stop but for one with each report (the AUV dived)
        s.on_message(&MavMessage::HEARTBEAT(Default::default()), 40.0);
        s.on_message(&MavMessage::HIGH_LATENCY2(Default::default()), 40.0);
        assert!(s.high_latency);
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

    #[test]
    fn rtt_is_smoothed_rfc6298() {
        let mut s = LinkStats::default();
        s.ping(1, 0.0);
        s.pong(1, 1.4);
        assert_eq!((s.srtt_s, s.rttvar_s), (Some(1.4), 0.7));
        s.ping(2, 10.0);
        s.pong(2, 11.0); // 1.0 s: srtt 1.35, rttvar 0.75 x 0.7 + 0.25 x 0.4 = 0.625
        assert!((s.srtt_s.unwrap_or(0.0) - 1.35).abs() < 1e-9);
        assert!((s.rttvar_s - 0.625).abs() < 1e-9);
        assert_eq!(s.snapshot(11.0).rtt_ms, Some(1000)); // the console still sees the last
    }

    fn timing_for(
        rtt: Option<(f64, f64)>,
        emu_ms: Option<f64>,
        plan: Option<crate::telemetry::PlanChoice>,
    ) -> LinkTiming {
        let mut s = LinkStats::default();
        if let Some((r, v)) = rtt {
            s.srtt_s = Some(r);
            s.rttvar_s = v;
        }
        s.emu_link = emu_ms.map(|ms| EmuLink {
            rtt_ms: Some(ms),
            ..EmuLink::default()
        });
        s.plan = plan;
        s.timing()
    }

    fn secs(d: Duration) -> f64 {
        (d.as_secs_f64() * 1000.0).round() / 1000.0
    }

    #[test]
    fn fast_links_keep_the_old_timeouts() {
        // LOS: 30 ms measured. The floors: 1 s per ACK, 1.5 s per mission item, as before.
        let t = timing_for(Some((0.03, 0.005)), None, None);
        assert_eq!(t.source, "timesync");
        assert_eq!(secs(t.ack_timeout(0)), 1.0);
        assert_eq!(secs(t.ack_timeout(1)), 1.5);
        assert_eq!(secs(t.item_timeout()), 1.5);
        assert_eq!(t.stall_after_s(), 10.0);
        assert_eq!(secs(t.confirm_wait()), 5.0);
        // nothing known at all: the same floors
        let t = timing_for(None, None, None);
        assert_eq!(
            (t.source, secs(t.ack_timeout(0)), secs(t.item_timeout())),
            ("none", 1.0, 1.5)
        );
    }

    #[test]
    fn slow_links_wait_for_their_round_trip() {
        use crate::telemetry::{PlanInput, choose};
        let iridium = choose(&PlanInput {
            link_class: Some("satcom"),
            space_class: Some("iridium"),
            ..PlanInput::default()
        });
        // before any measurement: the profile's 1.4 s, rto 3 x 1.4
        let t = timing_for(None, None, Some(iridium.clone()));
        assert_eq!(t.source, "profile");
        assert_eq!(secs(t.ack_timeout(0)), 4.45);
        assert_eq!(secs(t.item_timeout()), 5.45);
        assert_eq!(t.report_s, 5.0);
        assert_eq!((t.stall_after_s() * 100.0).round() / 100.0, 14.2); // 2 x 5 s + 4.2 s
        // the emulator's measured delay: 1.5 s
        let t = timing_for(None, Some(1500.0), Some(iridium.clone()));
        assert_eq!((t.source, secs(t.ack_timeout(0))), ("emulator", 4.75));
        // TIMESYNC converged: 1.4 s, 0.15 s deviation. Tighter: 1.4 + 0.6 + 0.25
        let t = timing_for(Some((1.4, 0.15)), Some(1500.0), Some(iridium));
        assert_eq!((t.source, secs(t.ack_timeout(0))), ("timesync", 2.25));
        assert_eq!(secs(t.ack_timeout(2)), 5.063);
        assert_eq!(secs(t.in_flight()), 1.55);
        // GEO: 0.6 s
        let t = timing_for(Some((0.6, 0.05)), None, None);
        assert_eq!(secs(t.ack_timeout(0)), 1.05);
        // the AUV dived: TIMESYNC still says 1.4 s, the emulator 12 s: the emulator wins
        let t = timing_for(Some((1.4, 0.15)), Some(12_000.0), None);
        assert_eq!((t.source, t.rtt_s), ("emulator", 12.0));
        assert_eq!(secs(t.ack_timeout(0)), 30.0); // 36.25 s, capped
        assert_eq!(secs(t.ack_timeout(3)), 30.0);
    }

    #[test]
    fn acoustic_rtt_survives_long_queueing() {
        // An acoustic link answers after 40 s (queue plus sound): a radio link's 10 s window
        // would have forgotten the ping; the acoustic policy waits long enough.
        let mut radio = LinkStats::default();
        radio.ping(1, 0.0);
        radio.ping(2, 11.0); // housekeeping keeps pinging; 1 is forgotten
        radio.pong(1, 40.0);
        assert_eq!(radio.rtt_ms, None);
        let mut s = LinkStats {
            timesync: TimesyncPolicy::for_class(Some("acoustic")),
            ..LinkStats::default()
        };
        assert_eq!(s.timesync.every_s, 30);
        s.ping(1, 0.0);
        s.ping(2, 30.0);
        s.pong(1, 40.0);
        assert_eq!(s.snapshot(40.0).rtt_ms, Some(40_000));
        assert_eq!(TimesyncPolicy::for_class(Some("los")), TimesyncPolicy::default());
        assert_eq!(TimesyncPolicy::for_class(None).forget_s, 10.0);
    }
}
