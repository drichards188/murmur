//! The GCS heartbeat, on a thread of its own (murmur-nb1).
//!
//! ArduPilot and the stand-ins time their GCS (lost-link) failsafe from the last HEARTBEAT
//! of MAV_GCS_SYSID, so heartbeats that go out late put every vehicle into failsafe at once.
//! They used to go out from `World::housekeeping`, a task on the tokio runtime that also
//! re-reads the manifest, binds ports, chooses each link's telemetry plan and asks for
//! parameters: whatever held up the runtime or that loop (busy workers, a write stuck behind
//! a busy disk, a contended lock) held up the heartbeat with it. This thread does one thing:
//! it sleeps to the next whole period on the monotonic clock, sends the heartbeat to every
//! link with a vehicle whose period is due (`Link::gcs_heartbeat_s`, set by the housekeeping
//! from the telemetry plan: 1 s, 5 s to an AUV under water) and records how late the round
//! went out. It holds no lock longer than a copy, never prints and never touches a file (a
//! stuck write would stall it), and skips a round it overslept rather than sending two.
//!
//! Jitter monitoring: per minute, the worst send delay (from a round's due time to its last
//! send), the longest gap between rounds and the rounds skipped, kept for an hour and served
//! by `/api/stats` as `gcs_heartbeat`. [`report`], a runtime task, logs a minute whose worst
//! delay reaches [`LOG_DELAY_MS`] or that skipped a round, and the worst of every
//! [`LOG_EVERY_MIN`] minutes regardless.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::clock;
use crate::link::Transmit;
use crate::mav::{MavMessage, apm};
use crate::world::World;

/// One round per second: the shortest heartbeat period any link gets.
pub const PERIOD: Duration = Duration::from_secs(1);
/// Seconds per record.
const MINUTE: Duration = Duration::from_secs(60);
/// Records kept (an hour).
const KEEP_MINUTES: usize = 60;
/// A minute whose worst send delay reaches this is logged.
pub const LOG_DELAY_MS: f64 = 100.0;
/// The worst minute of this many is logged even when nothing was late.
pub const LOG_EVERY_MIN: usize = 10;

pub fn gcs_heartbeat() -> MavMessage {
    MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
        custom_mode: 0,
        mavtype: apm::MavType::MAV_TYPE_GCS,
        autopilot: apm::MavAutopilot::MAV_AUTOPILOT_INVALID,
        base_mode: apm::MavModeFlag::empty(),
        system_status: apm::MavState::MAV_STATE_ACTIVE,
        mavlink_version: 3,
    })
}

/// One minute of rounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Minute {
    /// Wall clock (s since the epoch) when the minute closed; for the open one, its last round.
    pub t: f64,
    pub rounds: u32,
    /// Rounds skipped: the thread woke after the next one was already due.
    pub missed: u32,
    /// Heartbeats sent.
    pub sent: u64,
    /// Worst time from a round's due time to its last send, ms.
    pub max_delay_ms: f64,
    /// Longest time between the starts of two rounds, ms: the longest silence a vehicle on a
    /// 1 s heartbeat saw from the bridge (about 1000 when all is well).
    pub max_gap_ms: f64,
}

impl Minute {
    fn add(&mut self, r: &Round) {
        self.rounds += 1;
        self.missed += r.missed;
        self.sent += r.sent;
        self.max_delay_ms = self.max_delay_ms.max(r.delay_ms);
        if let Some(g) = r.gap_ms {
            self.max_gap_ms = self.max_gap_ms.max(g);
        }
        self.t = r.wall;
    }

    fn late(&self) -> bool {
        self.max_delay_ms >= LOG_DELAY_MS || self.missed > 0
    }

    fn line(&self, what: &str) -> String {
        format!(
            "gcs heartbeat: worst send delay {:.0} ms, longest gap {:.2} s, {} rounds skipped over {what} ({} rounds, {} heartbeats)",
            self.max_delay_ms,
            self.max_gap_ms / 1000.0,
            self.missed,
            self.rounds,
            self.sent
        )
    }
}

/// What one round measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Round {
    pub wall: f64,
    pub delay_ms: f64,
    pub gap_ms: Option<f64>,
    pub sent: u64,
    pub missed: u32,
}

#[derive(Default)]
struct Inner {
    open: Minute,
    opened: Option<Instant>,
    closed: VecDeque<Minute>,
    /// Records closed since [`report`] last looked.
    unreported: Vec<Minute>,
    worst_delay_ms: f64,
}

/// The heartbeat thread's measurements, shared with the API and the log.
#[derive(Default)]
pub struct Stats {
    inner: Mutex<Inner>,
}

impl Stats {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add a round that finished at `at` (closing the minute when it is full).
    pub fn record(&self, r: Round, at: Instant) {
        let mut s = self.lock();
        s.worst_delay_ms = s.worst_delay_ms.max(r.delay_ms);
        let opened = *s.opened.get_or_insert(at);
        if at.saturating_duration_since(opened) >= MINUTE && s.open.rounds > 0 {
            let done = std::mem::take(&mut s.open);
            if s.closed.len() == KEEP_MINUTES {
                s.closed.pop_front();
            }
            s.closed.push_back(done);
            s.unreported.push(done);
            s.opened = Some(at);
        }
        s.open.add(&r);
    }

    /// Minutes closed since the last call.
    fn take_unreported(&self) -> Vec<Minute> {
        std::mem::take(&mut self.lock().unreported)
    }

    /// For `/api/stats`: the closed minutes (oldest first), the open one, the worst delay
    /// since the bridge started.
    pub fn json(&self) -> Value {
        let s = self.lock();
        json!({
            "thread": "dedicated",
            "period_s": PERIOD.as_secs_f64(),
            "worst_delay_ms": s.worst_delay_ms,
            "current": s.open,
            "minutes": s.closed,
        })
    }
}

/// Round `k`'s due time is `start + k * PERIOD`. Woken at `now` for round `k`: the round to
/// send (the latest one due) and how many were skipped to get there.
pub fn catch_up(start: Instant, k: u64, now: Instant) -> (u64, u32) {
    let behind = now.saturating_duration_since(start).as_nanos() / PERIOD.as_nanos();
    let latest = u64::try_from(behind).unwrap_or(u64::MAX).max(k);
    (latest, u32::try_from(latest - k).unwrap_or(u32::MAX))
}

fn due(start: Instant, k: u64) -> Instant {
    start + PERIOD * u32::try_from(k).unwrap_or(u32::MAX)
}

static STATS: OnceLock<Arc<Stats>> = OnceLock::new();

/// The running thread's measurements (None before [`spawn`]).
pub fn stats() -> Option<&'static Arc<Stats>> {
    STATS.get()
}

/// Start the thread. The heartbeat goes to every link whose vehicle has spoken (the link
/// knows where to reply), as before, when `k` is a multiple of the link's period.
pub fn spawn(world: Arc<World>) -> std::io::Result<Arc<Stats>> {
    let stats = STATS.get_or_init(Arc::default).clone();
    start(world, stats.clone())?;
    Ok(stats)
}

fn start(world: Arc<World>, stats: Arc<Stats>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("gcs-heartbeat".into())
        .spawn(move || run(&world, &stats))?;
    Ok(())
}

fn ms(d: Duration) -> f64 {
    clock::round(d.as_secs_f64() * 1000.0, 1)
}

fn run(world: &World, stats: &Stats) {
    let msg = gcs_heartbeat();
    let start = Instant::now();
    let mut k: u64 = 0;
    let mut last: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let next = due(start, k);
        if next > now {
            std::thread::sleep(next - now);
        }
        let (round, missed) = catch_up(start, k, Instant::now());
        k = round;
        let began = Instant::now();
        let mut sent = 0;
        for link in world.links() {
            if link.vehicle.get().is_some() && k.is_multiple_of(link.gcs_heartbeat_s()) {
                link.send(&msg);
                sent += 1;
            }
        }
        let done = Instant::now();
        let r = Round {
            wall: clock::wall(),
            delay_ms: ms(done.saturating_duration_since(due(start, k))),
            gap_ms: last.map(|l| ms(began.saturating_duration_since(l))),
            sent,
            missed,
        };
        stats.record(r, done);
        last = Some(began);
        k += 1;
    }
}

/// Log what the thread measured (see the module doc). A runtime task: if the runtime is
/// what was starved, the line comes late, the heartbeats did not.
pub async fn report(stats: Arc<Stats>) {
    let mut every = tokio::time::interval(Duration::from_secs(10));
    let mut quiet: Vec<Minute> = Vec::new();
    loop {
        every.tick().await;
        for m in stats.take_unreported() {
            if m.late() {
                println!("{}", m.line("the last minute"));
            } else {
                quiet.push(m);
            }
            if quiet.len() >= LOG_EVERY_MIN {
                let worst = quiet
                    .iter()
                    .copied()
                    .max_by(|a, b| a.max_delay_ms.total_cmp(&b.max_delay_ms))
                    .unwrap_or_default();
                println!(
                    "{}",
                    worst.line(&format!(
                        "the worst of the last {} quiet minutes",
                        quiet.len()
                    ))
                );
                quiet.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(delay_ms: f64, gap_ms: Option<f64>, missed: u32) -> Round {
        Round {
            wall: 1.0,
            delay_ms,
            gap_ms,
            sent: 3,
            missed,
        }
    }

    #[test]
    fn a_late_wake_skips_to_the_latest_round() {
        let t0 = Instant::now();
        // on time, or late by less than a period: the same round
        assert_eq!(catch_up(t0, 5, t0 + Duration::from_millis(5_200)), (5, 0));
        // woke 3.5 s after round 5 was due: rounds 6, 7 and 8 are due too, send 8 only
        assert_eq!(catch_up(t0, 5, t0 + Duration::from_millis(8_500)), (8, 3));
        // early (never, but harmless)
        assert_eq!(catch_up(t0, 5, t0 + Duration::from_millis(4_900)), (5, 0));
    }

    #[test]
    fn minutes_close_and_keep_the_worst() {
        let s = Stats::default();
        let t0 = Instant::now();
        s.record(round(2.0, None, 0), t0);
        s.record(round(350.0, Some(1350.0), 0), t0 + Duration::from_secs(1));
        s.record(round(1.0, Some(4000.0), 3), t0 + Duration::from_secs(5));
        assert!(s.take_unreported().is_empty());
        // a minute after the first round: the open minute closes, this round opens the next
        s.record(round(4.0, Some(1000.0), 0), t0 + Duration::from_secs(60));
        let closed = s.take_unreported();
        assert_eq!(closed.len(), 1);
        let m = closed[0];
        assert_eq!((m.rounds, m.missed, m.sent), (3, 3, 9));
        assert_eq!((m.max_delay_ms, m.max_gap_ms), (350.0, 4000.0));
        assert!(m.late());
        let j = s.json();
        assert_eq!(j["worst_delay_ms"], 350.0);
        assert_eq!(j["minutes"].as_array().map(Vec::len), Some(1));
        assert_eq!(j["current"]["rounds"], 1);
        assert_eq!(j["current"]["max_delay_ms"], 4.0);
        assert!(s.take_unreported().is_empty());
    }

    /// What reached the vehicle's socket so far: (GCS heartbeats, SYSTEM_TIME markers).
    fn drain(sock: &std::net::UdpSocket) -> (usize, usize) {
        let mut buf = [0u8; 2048];
        let (mut beats, mut marks) = (0, 0);
        while let Ok(n) = sock.recv(&mut buf) {
            for f in crate::mav::decode(&buf[..n]) {
                match f.msg {
                    MavMessage::HEARTBEAT(h) if h.mavtype == apm::MavType::MAV_TYPE_GCS => {
                        beats += 1
                    }
                    MavMessage::SYSTEM_TIME(_) => marks += 1,
                    _ => {}
                }
            }
        }
        (beats, marks)
    }

    /// The old heartbeat was a task on the runtime; this one is not. With the runtime blocked,
    /// a runtime task's once-a-second marker stops and the thread's heartbeats go on.
    #[test]
    fn heartbeats_keep_time_while_the_runtime_is_blocked() {
        use crate::link::Transmit;
        use crate::world::Config;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.local_addr())
            .expect("free port")
            .port();
        let world = World::new(Config {
            base_port: port,
            http_port: 0,
            default_ports: 1,
            manifest: "/nonexistent/fleet.json".into(),
            platforms: "/nonexistent/platforms.toml".into(),
            root: env!("CARGO_MANIFEST_DIR").into(),
        });
        rt.block_on(world.ensure_ports());
        // the vehicle speaks first (an autopilot heartbeat binds the link to it)
        let vehicle = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
        let hello = MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
            mavtype: apm::MavType::MAV_TYPE_FIXED_WING,
            autopilot: apm::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            ..Default::default()
        });
        let frame = crate::mav::encode_as(&hello, 1, crate::mav::AUTOPILOT_COMPID, 0);
        vehicle.send_to(&frame, ("127.0.0.1", port)).expect("send");
        let link = rt.block_on(async {
            for _ in 0..100 {
                if let Some(l) = world
                    .links()
                    .into_iter()
                    .find(|l| l.vehicle.get().is_some())
                {
                    return l;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("the link never bound its vehicle");
        });
        // a runtime task sending a marker every second, as the old heartbeat did
        let marker = link.clone();
        rt.spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(1));
            loop {
                every.tick().await;
                marker.send(&MavMessage::SYSTEM_TIME(Default::default()));
            }
        });
        let stats = Arc::new(Stats::default());
        start(world.clone(), stats.clone()).expect("thread");
        vehicle.set_nonblocking(true).expect("nonblocking");
        rt.block_on(async { tokio::time::sleep(Duration::from_millis(1500)).await });
        let (beats, marks) = drain(&vehicle);
        assert!(
            beats >= 1 && marks >= 1,
            "running: {beats} beats, {marks} marks"
        );
        // block the runtime for 3.2 s: nothing on it runs
        rt.block_on(async { std::thread::sleep(Duration::from_millis(3200)) });
        let (beats, marks) = drain(&vehicle);
        assert_eq!(marks, 0, "a runtime task ran while the runtime was blocked");
        assert!(beats >= 3, "{beats} heartbeats in 3.2 s");
        let j = stats.json();
        assert!(j["current"]["max_gap_ms"].as_f64() < Some(1500.0), "{j}");
        assert_eq!(j["current"]["missed"], 0);
    }

    #[test]
    fn an_hour_is_kept() {
        let s = Stats::default();
        let t0 = Instant::now();
        for i in 0..=(KEEP_MINUTES as u64 + 5) {
            s.record(round(1.0, Some(1000.0), 0), t0 + MINUTE * i as u32);
        }
        assert_eq!(
            s.json()["minutes"].as_array().map(Vec::len),
            Some(KEEP_MINUTES)
        );
    }
}
