//! Time and rounding shared by the world model, the task layer and the decision layer.
//!
//! **Three clocks** (murmur-dup):
//!
//! - [`mono`]: monotonic seconds, tokio's clock (tests pause and advance it): durations,
//!   timeouts, retries, rates.
//! - [`wall`]: the bridge's clock, seconds since the epoch. It never steps and never goes
//!   back, and it stays within about [`DEAD_BAND_S`] (plus one of the wall clock's own steps)
//!   of the wall clock: it runs at the monotonic clock's rate while within [`DEAD_BAND_S`] of
//!   [`utc`], and at that rate times 1 - [`SLEW`] or 1 + [`SLEW`] while outside it, until it
//!   is back ([`Steer`]). Everything the bridge stamps and later compares reads it: when each
//!   report arrived (the facts the decision layer reads), the engine's own clock
//!   ([`BridgeClock`], so every time in the audit log), track times, the ages agents and the
//!   console are shown, alerts. A wall clock that steps moves none of them by a step, and
//!   replay, which sees only logged times, sees the durations the bridge decided on, each
//!   within 5 % of the monotonic one.
//! - [`utc`]: the wall clock as the OS has it, for a date another clock reads: CoT events (a
//!   TAK client compares their stale times with its own clock), the resume pointer the next
//!   run reads, the run id, and the wall-clock time each anchor records beside the bridge's
//!   (so any time in a log maps to UTC).
//!
//! **What this host does** (WSL, measured 2026-10-05 on one boot): for the first two hours
//! or so after boot the monotonic clock ran fast against the Windows clock (2.1 % in the first
//! minutes), and time sync pulled the wall clock back with a step every 32 s (0.64 to 0.67 s
//! at first, 0.46 to 0.81 s an hour and a half in); at 2 h 52 min the monotonic clock ran
//! +0.011 % idle and 0.000 % under load against Windows, and the wall clock had stopped
//! stepping (none in 300 s; none in 130 s at 3 h 10 min). A clock that only followed the
//! monotonic one would have run 75 s an hour ahead of the wall clock early on, so a later run,
//! starting at the wall clock, would have found the deadlines it carried that far in the
//! future; the bound keeps every run's times within about 2 s of the wall clock in both
//! states. [`SLEW`] (5 %) is more than twice the worst drift measured, so the bound holds
//! while drifting; [`DEAD_BAND_S`] (1 s) is more than the largest step, so a single step of
//! the sawtooth does not move the clock at all. A large jump of the wall clock (an NTP
//! correction, a resumed virtual machine whose monotonic clock paused while the host slept)
//! is slewed, never stepped: catching up a gap of `g` seconds takes `g / SLEW` (20 `g`)
//! seconds, a minute's gap 20 minutes, and the anchors' (bridge, UTC) pairs say where the
//! log stood meanwhile.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

/// The most the bridge's clock runs faster or slower than the monotonic clock while it
/// steers back to the wall clock: 5 %.
pub const SLEW: f64 = 0.05;
/// Within this of the wall clock the bridge's clock runs at the monotonic rate, s.
pub const DEAD_BAND_S: f64 = 1.0;

/// The wall clock as the OS has it now, seconds since the epoch (it can step).
pub fn utc() -> f64 {
    #[cfg(test)]
    if let Some(t) = test_clock::utc() {
        return t;
    }
    #[cfg(murmur_headless)]
    if let Some(t) = sim_clock::now() {
        return t;
    }
    let t = os_utc();
    #[cfg(test)]
    let t = t + test_clock::utc_offset();
    t
}

fn os_utc() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The bridge's clock as readings of the monotonic and the wall clock make it (see the
/// module doc): the monotonic rate within [`DEAD_BAND_S`] of the wall clock, the monotonic
/// rate times 1 - [`SLEW`] or 1 + [`SLEW`] outside it (never past the wall clock), never back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Steer {
    mono: f64,
    wall: f64,
}

impl Steer {
    /// Starting at the wall clock's `utc` when the monotonic clock reads `mono`.
    pub fn new(mono: f64, utc: f64) -> Steer {
        Steer { mono, wall: utc }
    }

    /// The bridge's clock when the monotonic clock reads `mono` and the wall clock `utc`.
    pub fn read(&mut self, mono: f64, utc: f64) -> f64 {
        let dm = (mono - self.mono).max(0.0);
        let free = self.wall + dm;
        let behind = utc - free;
        let w = if behind > DEAD_BAND_S {
            (self.wall + dm * (1.0 + SLEW)).min(utc)
        } else if behind < -DEAD_BAND_S {
            (self.wall + dm * (1.0 - SLEW)).max(utc)
        } else {
            free
        };
        self.mono = self.mono.max(mono);
        self.wall = w.max(self.wall);
        self.wall
    }
}

/// The bridge's clock, seconds since the epoch (see the module doc and [`Steer`]). Never
/// steps, never goes back, within about 2 s of the wall clock.
pub fn wall() -> f64 {
    #[cfg(test)]
    if let Some(t) = test_clock::wall() {
        return t;
    }
    #[cfg(murmur_headless)]
    if let Some(t) = sim_clock::now() {
        return t;
    }
    static STEER: OnceLock<Mutex<Steer>> = OnceLock::new();
    let s = STEER.get_or_init(|| Mutex::new(Steer::new(mono(), os_utc())));
    let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
    s.read(mono(), os_utc())
}

/// Monotonic seconds (from a fixed point a little before the bridge started). tokio's clock,
/// so tests can pause and advance it; the margin keeps a paused test runtime, whose clock
/// starts when the runtime does, from reading before the origin.
pub fn mono() -> f64 {
    #[cfg(murmur_headless)]
    if let Some(m) = sim_clock::mono() {
        return m;
    }
    static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(|| {
        let now = std::time::Instant::now();
        now.checked_sub(Duration::from_secs(3600)).unwrap_or(now)
    });
    Instant::now()
        .into_std()
        .saturating_duration_since(origin)
        .as_secs_f64()
}

/// The engine's clock in the bridge: [`wall`] in integer milliseconds, so the audit log's
/// times and the stamps on the facts it decides on are one timeline that never steps.
#[derive(Clone, Copy, Debug, Default)]
pub struct BridgeClock;

impl murmur_decide::Clock for BridgeClock {
    fn now(&self) -> murmur_decide::Time {
        murmur_decide::Time((wall() * 1000.0).round() as i64)
    }
}

/// [`utc`] in integer milliseconds: the wall-clock time an anchor records beside the
/// bridge's.
#[derive(Clone, Copy, Debug, Default)]
pub struct UtcClock;

impl murmur_decide::Clock for UtcClock {
    fn now(&self) -> murmur_decide::Time {
        murmur_decide::Time((utc() * 1000.0).round() as i64)
    }
}

/// Python's round(x, n): correctly rounded decimal, ties to even. Snapshots are rounded so
/// tiny jitter doesn't count as a change, and so they match the Python bridge digit for digit.
pub fn round(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python's round(x) with no digits: an integer.
pub fn round_int(x: f64) -> i64 {
    round(x, 0) as i64
}

/// The headless simulator's clock (crates/murmur-headless, docs/HEADLESS.md): compiled only
/// where the `murmur_headless` cfg is set, which only that crate's build script does, so the
/// bridge's own build never has it. Once a simulation sets its epoch (inside its paused tokio
/// runtime), [`mono`] reads 3600 s plus tokio's time since, and [`wall`] and [`utc`] the epoch
/// plus tokio's time since: every clock of the bridge runs exactly on simulated time, from a
/// fixed origin (no real instant leaks into a fractional second), with no steering and no step.
#[cfg(murmur_headless)]
pub mod sim_clock {
    use std::sync::Mutex;

    use tokio::time::Instant;

    /// Where [`super::mono`] stands when the simulation starts (as the bridge's own origin
    /// puts it an hour before its start).
    pub const MONO_AT_START_S: f64 = 3600.0;

    /// (epoch, tokio's instant when it was set)
    static START: Mutex<Option<(f64, Instant)>> = Mutex::new(None);

    /// From now on the bridge's clock reads `epoch_s` plus tokio's time since this call.
    pub fn set_epoch(epoch_s: f64) {
        *START.lock().unwrap_or_else(|e| e.into_inner()) = Some((epoch_s, Instant::now()));
    }

    fn since() -> Option<(f64, f64)> {
        let s = *START.lock().unwrap_or_else(|e| e.into_inner());
        s.map(|(epoch, t0)| {
            // outside the simulation's runtime tokio's clock is the real one: a read there
            // would leak real time into the simulation, so it is a bug in the simulator
            assert!(
                tokio::runtime::Handle::try_current().is_ok(),
                "the simulated clock was read outside the simulation's runtime"
            );
            (epoch, Instant::now().saturating_duration_since(t0).as_secs_f64())
        })
    }

    pub(super) fn now() -> Option<f64> {
        since().map(|(epoch, d)| epoch + d)
    }

    pub(super) fn mono() -> Option<f64> {
        since().map(|(_, d)| MONO_AT_START_S + d)
    }
}

/// Tests: a wall clock that steps or drifts, and the bridge's clock put where a test wants
/// it. All of it is per thread: a `#[tokio::test]` runs its tasks on its own thread, so tests
/// running beside each other never see each other's. A `multi_thread` runtime's workers
/// (`decision/tests/auth.rs`) and `spawn_blocking` threads do not see the test thread's
/// clock: what they stamp reads the process's own.
#[cfg(test)]
pub mod test_clock {
    use std::cell::{Cell, RefCell};

    use murmur_decide::{Clock, ManualClock};

    use super::Steer;

    /// A pinned thread's wall clock (`epoch` when the monotonic clock read `m0`, running at
    /// 1 - `drift` of the monotonic rate) and its bridge clock.
    #[derive(Clone, Copy)]
    struct Pinned {
        epoch: f64,
        m0: f64,
        drift: f64,
        steer: Steer,
    }

    thread_local! {
        static UTC_OFFSET: Cell<f64> = const { Cell::new(0.0) };
        static PIN: Cell<Option<Pinned>> = const { Cell::new(None) };
        static FOLLOW: RefCell<Option<ManualClock>> = const { RefCell::new(None) };
        static ANCHOR: Cell<Option<(f64, f64)>> = const { Cell::new(None) };
    }

    /// Step this thread's wall clock ([`super::utc`]) by `s` seconds, as this host's time
    /// sync (back 0.5 to 0.9 s every 32 s early after boot), NTP or a resumed virtual
    /// machine does.
    pub fn step_utc(s: f64) {
        UTC_OFFSET.with(|o| o.set(o.get() + s));
    }

    pub(super) fn utc_offset() -> f64 {
        UTC_OFFSET.with(Cell::get)
    }

    /// Pin this thread's clocks: the wall clock reads `epoch_s` now and advances with
    /// [`super::mono`] (in a paused runtime, only as the test advances time), plus its steps;
    /// the bridge's clock is steered toward it as in the bridge. What a test stamps does not
    /// depend on the second it started in or on how loaded the host is. A thread already
    /// pinned keeps its clocks.
    pub fn pin(epoch_s: f64) {
        pin_drifting(epoch_s, 0.0);
    }

    /// [`pin`] with the monotonic clock running `drift` (0.021: 2.1 %) fast against the wall
    /// clock, as this host's did early after boot.
    pub fn pin_drifting(epoch_s: f64, drift: f64) {
        let m = super::mono();
        PIN.with(|p| {
            if p.get().is_none() {
                p.set(Some(Pinned {
                    epoch: epoch_s,
                    m0: m,
                    drift,
                    steer: Steer::new(m, epoch_s + utc_offset()),
                }));
            }
        });
    }

    /// A new bridge process on this thread: its clock starts at the wall clock again (the
    /// pinned wall clock runs on).
    pub fn restart() {
        let m = super::mono();
        if let Some(u) = utc() {
            PIN.with(|p| {
                if let Some(mut pinned) = p.get() {
                    pinned.steer = Steer::new(m, u);
                    p.set(Some(pinned));
                }
            });
        }
    }

    /// Run this thread's bridge clock on `c`, a test's manual engine clock: the facts are
    /// stamped on the engine's clock, as in the bridge (where the engine reads
    /// [`super::wall`]), and both move only when the test moves `c`. The latest call wins
    /// (a test that starts a second bridge on the same vehicles runs on the second's clock).
    pub fn follow(c: &ManualClock) {
        FOLLOW.with(|f| *f.borrow_mut() = Some(c.clone()));
    }

    /// The pinned wall clock, if this thread is pinned.
    pub(super) fn utc() -> Option<f64> {
        PIN.with(Cell::get)
            .map(|p| p.epoch + (super::mono() - p.m0) * (1.0 - p.drift) + utc_offset())
    }

    /// The bridge's clock on this thread: the manual clock it follows, its pinned clocks,
    /// else the wall clock at the thread's first read plus its own monotonic time since (a
    /// paused runtime's time is the thread's own; one clock shared by every test thread
    /// would mix them).
    pub(super) fn wall() -> Option<f64> {
        if let Some(t) = FOLLOW.with(|f| f.borrow().as_ref().map(|c| c.now().0 as f64 / 1000.0)) {
            return Some(t);
        }
        let m = super::mono();
        if let Some(u) = utc() {
            return PIN.with(|p| {
                let mut pinned = p.get()?;
                let w = pinned.steer.read(m, u);
                p.set(Some(pinned));
                Some(w)
            });
        }
        let (u0, m0) = ANCHOR.with(|a| {
            if a.get().is_none() {
                a.set(Some((super::os_utc() + utc_offset(), m)));
            }
            a.get().unwrap_or((0.0, m))
        });
        Some(u0 + (m - m0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headless simulator's seams (`murmur_headless`: its clock here, the in-memory link,
    /// `World::attach_link`) exist only where crates/murmur-headless compiles these files by
    /// path; the bridge's own build, of which this test is a part, never sets the cfg (no build
    /// script in this crate does, and none may: eval/headless/tests checks that too).
    #[test]
    fn the_bridge_build_has_no_headless_seams() {
        const { assert!(!cfg!(murmur_headless)) };
    }

    #[test]
    fn rounds_like_python() {
        assert_eq!(round_int(0.5), 0);
        assert_eq!(round_int(1.5), 2);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(-2.5), -2);
        assert_eq!(round(2.675, 2), 2.67); // binary 2.67499999...
        assert_eq!(round(-122.4783216, 6), -122.478322);
        assert_eq!(round(12.04, 0), 12.0);
    }

    /// The host's two clocks as they were early after boot (the larger steps, an earlier
    /// boot's): the wall clock runs with the monotonic clock and time sync steps it back
    /// 0.94 s every 32 s, so the monotonic clock runs 2.9 % fast against it. Read every
    /// quarter second for three hours, the bridge's clock never goes back, runs within 5 % of
    /// the monotonic rate, and stays within 2 s of the wall clock (the monotonic clock alone:
    /// over 300 s ahead by then).
    #[test]
    fn a_drifting_monotonic_clock_and_a_stepping_wall_clock_stay_bounded() {
        let wall_at = |m: f64| 1_781_431_200.0 + m - 0.94 * (m / 32.0).floor();
        let mut s = Steer::new(0.0, wall_at(0.0));
        let (mut prev_m, mut prev_w) = (0.0, wall_at(0.0));
        let mut worst: f64 = 0.0;
        for k in 1..=(3 * 3600 * 4) {
            let m = f64::from(k) * 0.25;
            let u = wall_at(m);
            let w = s.read(m, u);
            let rate = (w - prev_w) / (m - prev_m);
            assert!(w >= prev_w, "back at {m}");
            assert!((1.0 - SLEW - 1e-5..=1.0 + SLEW + 1e-5).contains(&rate), "rate {rate} at {m}");
            worst = worst.max((w - u).abs());
            (prev_m, prev_w) = (m, w);
        }
        assert!(worst <= 2.0, "{worst:.2} s from the wall clock");
        let mono_only = 10_800.0 - (wall_at(10_800.0) - wall_at(0.0));
        assert!(mono_only > 300.0, "{mono_only}");
    }

    /// The wall clock jumps an hour forward (a resumed VM whose monotonic clock paused) or
    /// back (an NTP correction): the bridge's clock slews, 5 % at most, never steps; an hour
    /// forward is caught up in 20 hours.
    #[test]
    fn a_large_jump_of_the_wall_clock_is_slewed_never_stepped() {
        let t0 = 1_781_431_200.0;
        let mut s = Steer::new(0.0, t0);
        assert_eq!(s.read(10.0, t0 + 10.0), t0 + 10.0);
        // an hour forward
        let w = s.read(11.0, t0 + 11.0 + 3600.0);
        assert!((w - (t0 + 11.05)).abs() < 1e-9, "{}", w - t0);
        let mut m = 11.0;
        let mut caught = None;
        while m < 30.0 * 3600.0 {
            m += 1.0;
            let u = t0 + m + 3600.0;
            let before = s.wall;
            let w = s.read(m, u);
            assert!(w - before <= 1.05 + 1e-5 && w >= before);
            if caught.is_none() && (u - w).abs() <= DEAD_BAND_S {
                caught = Some(m);
            }
        }
        let caught = caught.expect("caught up");
        assert!((caught - 3600.0 / SLEW).abs() < 30.0, "caught up after {caught} s");
        // an hour back: never back, 5 % slower until it meets the wall clock again
        let mut s = Steer::new(0.0, t0);
        let w1 = s.read(1.0, t0 + 1.0 - 3600.0);
        assert!((w1 - (t0 + 0.95)).abs() < 1e-9);
    }

    /// murmur-dup: in the bridge (a pinned test thread here) steps of the wall clock move the
    /// bridge's clock by no step: a 0.94 s step back is inside the dead band, two minutes
    /// forward is slewed at 5 %.
    #[tokio::test(start_paused = true)]
    async fn the_bridge_clock_does_not_step_with_the_wall_clock() {
        use murmur_decide::Clock;
        test_clock::pin(1_781_431_200.0);
        let (w0, u0, e0) = (wall(), utc(), BridgeClock.now());
        tokio::time::sleep(Duration::from_secs(10)).await;
        test_clock::step_utc(-0.94);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let w1 = wall();
        assert!((w1 - w0 - 20.0).abs() < 1e-6, "inside the dead band: {}", w1 - w0);
        test_clock::step_utc(120.0);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let (w2, u2, e2) = (wall(), utc(), BridgeClock.now());
        assert!((u2 - u0 - (30.0 - 0.94 + 120.0)).abs() < 1e-6, "the wall clock stepped: {}", u2 - u0);
        assert!((w2 - w1 - 10.5).abs() < 1e-6, "slewed, not stepped: {}", w2 - w1);
        assert!((e2.ms_since(e0) - 30_500).abs() <= 1, "{}", e2.ms_since(e0));
    }
}
