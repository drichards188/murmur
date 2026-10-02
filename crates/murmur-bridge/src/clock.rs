//! Time and rounding shared by the world model and the task layer.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

/// Wall clock, seconds since the epoch (what the console compares against).
pub fn wall() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
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

/// Python's round(x, n): correctly rounded decimal, ties to even. Snapshots are rounded so
/// tiny jitter doesn't count as a change, and so they match the Python bridge digit for digit.
pub fn round(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python's round(x) with no digits: an integer.
pub fn round_int(x: f64) -> i64 {
    round(x, 0) as i64
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
}
