//! Time and rounding shared by the world model, the task layer and the decision layer.
//!
//! **Three clocks** (murmur-dup):
//!
//! - [`mono`]: monotonic seconds, tokio's clock (tests pause and advance it): durations,
//!   timeouts, retries, rates.
//! - [`wall`]: the bridge's clock, seconds since the epoch: the wall clock when it was first
//!   read plus monotonic time since. It never steps. Everything the bridge stamps and later
//!   compares reads it: when each report arrived (the facts the decision layer reads), the
//!   engine's own clock ([`BridgeClock`], so every time in the audit log), track times, the ages
//!   agents and the console are shown, alerts. A wall clock that steps (this WSL host's steps
//!   back about 0.65 s every 32 s; NTP or a resumed virtual machine can step it either way)
//!   moves none of them, and replay, which sees only logged times, sees the durations the
//!   bridge decided on. Where the host's two clocks agree it reads the wall clock; where they
//!   don't it keeps the monotonic clock's rate, as the stand-ins', the contacts' and the link
//!   emulator's scenario clocks do (measured on this host, 2026-10-05: the monotonic clock ran
//!   2.1 % fast against the Windows host's, which the wall clock follows by stepping back, so
//!   after an hour [`wall`] reads about 75 s ahead of [`utc`]).
//! - [`utc`]: the wall clock as the OS has it, for a date another clock reads: CoT events (a
//!   TAK client compares their stale times with its own clock), the resume pointer the next
//!   run reads, the run id.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

/// The wall clock as the OS has it now, seconds since the epoch (it can step).
pub fn utc() -> f64 {
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

/// The bridge's clock, seconds since the epoch: the wall clock at its first read plus
/// monotonic time since (see the module doc). Never steps, never goes back.
pub fn wall() -> f64 {
    #[cfg(test)]
    if let Some(t) = test_clock::pinned() {
        return t;
    }
    static ANCHOR: OnceLock<(f64, f64)> = OnceLock::new();
    let (utc0, mono0) = *ANCHOR.get_or_init(|| (os_utc(), mono()));
    utc0 + (mono() - mono0)
}

/// Monotonic seconds (from a fixed point a little before the bridge started). tokio's clock,
/// so tests can pause and advance it; the margin keeps a paused test runtime, whose clock
/// starts when the runtime does, from reading before the origin.
pub fn mono() -> f64 {
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

/// Python's round(x, n): correctly rounded decimal, ties to even. Snapshots are rounded so
/// tiny jitter doesn't count as a change, and so they match the Python bridge digit for digit.
pub fn round(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python's round(x) with no digits: an integer.
pub fn round_int(x: f64) -> i64 {
    round(x, 0) as i64
}

/// Tests: a wall clock that steps, and the bridge's clock put where a test wants it. All of
/// it is per thread (a `#[tokio::test]` runs its tasks on its own thread), so tests running
/// beside each other never see each other's.
#[cfg(test)]
pub mod test_clock {
    use std::cell::{Cell, RefCell};

    use murmur_decide::{Clock, ManualClock};

    thread_local! {
        static UTC_OFFSET: Cell<f64> = const { Cell::new(0.0) };
        static PIN: Cell<Option<(f64, f64)>> = const { Cell::new(None) };
        static FOLLOW: RefCell<Option<ManualClock>> = const { RefCell::new(None) };
    }

    /// Step this thread's wall clock ([`super::utc`]) by `s` seconds, as this host's time
    /// sync (back about 0.65 s every 32 s), NTP or a resumed virtual machine does.
    pub fn step_utc(s: f64) {
        UTC_OFFSET.with(|o| o.set(o.get() + s));
    }

    pub(super) fn utc_offset() -> f64 {
        UTC_OFFSET.with(Cell::get)
    }

    /// Pin this thread's bridge clock: [`super::wall`] reads `epoch_s` now and advances with
    /// [`super::mono`] (in a paused runtime, only as the test advances time), so what a test
    /// stamps does not depend on the second it started in or on how loaded the host is. A
    /// thread already pinned keeps its clock.
    pub fn pin(epoch_s: f64) {
        let m = super::mono();
        PIN.with(|p| {
            if p.get().is_none() {
                p.set(Some((epoch_s, m)));
            }
        });
    }

    /// Run this thread's bridge clock on `c`, a test's manual engine clock: the facts are
    /// stamped on the engine's clock, as in the bridge (where the engine reads
    /// [`super::wall`]), and both move only when the test moves `c`. The latest call wins
    /// (a test that starts a second bridge on the same vehicles runs on the second's clock).
    pub fn follow(c: &ManualClock) {
        FOLLOW.with(|f| *f.borrow_mut() = Some(c.clone()));
    }

    pub(super) fn pinned() -> Option<f64> {
        if let Some(t) = FOLLOW.with(|f| f.borrow().as_ref().map(|c| c.now().0 as f64 / 1000.0)) {
            return Some(t);
        }
        PIN.with(Cell::get).map(|(e, m0)| e + (super::mono() - m0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// murmur-dup: the wall clock steps back (this host: about 0.65 s every 32 s) and forward
    /// (NTP, a resumed VM); the bridge's clock, and the engine's with it, advances only as
    /// monotonic time does.
    #[tokio::test(start_paused = true)]
    async fn the_bridge_clock_does_not_step_with_the_wall_clock() {
        use murmur_decide::Clock;
        let (w0, u0, e0) = (wall(), utc(), BridgeClock.now());
        tokio::time::sleep(Duration::from_secs(10)).await;
        test_clock::step_utc(-0.94);
        tokio::time::sleep(Duration::from_secs(10)).await;
        test_clock::step_utc(120.0);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let (w1, u1, e1) = (wall(), utc(), BridgeClock.now());
        // (paused: hardly any real time passed, so the wall clock moved by its steps)
        assert!((u1 - u0 - (120.0 - 0.94)).abs() < 0.5, "the wall clock stepped: {}", u1 - u0);
        assert!((w1 - w0 - 30.0).abs() < 1e-6, "30 s passed, the bridge clock says {}", w1 - w0);
        assert!((e1.ms_since(e0) - 30_000).abs() <= 1, "{}", e1.ms_since(e0));
    }
}
