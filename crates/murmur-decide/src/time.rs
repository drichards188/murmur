//! Time and the injected clock.
//!
//! Every timestamp the decision layer produces comes from a [`Clock`] the caller supplies,
//! so tests and replays control time exactly. Times are integer milliseconds: comparisons
//! (expiry, staleness) and the log text are exact, with no float formatting involved.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const fn from_secs(s: i64) -> Self {
        Time(s.saturating_mul(1000))
    }

    pub const fn plus_ms(self, ms: i64) -> Self {
        Time(self.0.saturating_add(ms))
    }

    /// Milliseconds from `earlier` to `self` (negative if `earlier` is later).
    pub const fn ms_since(self, earlier: Time) -> i64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl std::fmt::Display for Time {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// Source of "now" for the engine.
pub trait Clock {
    fn now(&self) -> Time;
}

/// The wall clock. It can step (NTP, a resumed virtual machine, this WSL host back about
/// 0.65 s every 32 s), so the live bridge gives the engine a clock of its own that never does
/// (`clock::BridgeClock` in murmur-bridge, murmur-dup).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Time {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Time(ms)
    }
}

/// A clock that only moves when told to. Clones share the same time, so a test can keep a
/// handle after giving one to the engine.
#[derive(Clone, Debug, Default)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    pub fn new(start: Time) -> Self {
        ManualClock(Arc::new(AtomicI64::new(start.0)))
    }

    pub fn set(&self, t: Time) {
        self.0.store(t.0, Ordering::SeqCst);
    }

    pub fn advance_ms(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Time {
        Time(self.0.load(Ordering::SeqCst))
    }
}
