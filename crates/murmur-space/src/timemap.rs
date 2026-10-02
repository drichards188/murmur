//! Epoch honesty: scenario time versus propagation time.
//!
//! Public element sets are current (October 2026); Murmur's scenarios are dated (Golden Gate,
//! 13-16 June 2026). SGP4 error grows with distance from the element epoch, and LEO
//! satellites manoeuvre, so propagating October elements back four months gives positions
//! that are hundreds to thousands of km wrong along track. Historical element sets need a
//! Space-Track account.
//!
//! Default (`Auto`): when the scenario window is far from the element epochs, shift the
//! propagation by a whole number of days so the window's middle lands on the elements'
//! reference time. A whole-day shift keeps the UTC (and so local) time of day, so
//! sun-synchronous imagers still pass at their real local times. Ground illumination (sun
//! elevation for optical collects) is computed at the *scenario* time, so June stays June.
//! What is lost: the passes are those of the real constellation on the shifted date, not the
//! ones that happened on the scenario date. See the crate README for the error budget.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum EpochMode {
    /// Shift when the window is more than `AUTO_THRESHOLD` from the elements, else direct.
    #[default]
    Auto,
    /// Always shift to the elements' reference day.
    Shift,
    /// Propagate at the scenario time itself, whatever the element age.
    Direct,
}

/// Beyond this distance from the element epoch, `Auto` shifts.
pub const AUTO_THRESHOLD: Duration = Duration::days(2);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeMap {
    pub mode: EpochMode,
    /// propagation time = scenario time + shift_days.
    pub shift_days: i64,
    /// The elements' reference time (median epoch of the loaded element sets).
    pub reference: DateTime<Utc>,
}

impl TimeMap {
    pub fn identity(reference: DateTime<Utc>) -> Self {
        TimeMap {
            mode: EpochMode::Direct,
            shift_days: 0,
            reference,
        }
    }

    pub fn for_window(
        mode: EpochMode,
        reference: DateTime<Utc>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Self {
        let mid = from + (to - from) / 2;
        let gap = reference - mid;
        let shift = match mode {
            EpochMode::Direct => 0,
            EpochMode::Auto if gap.abs() <= AUTO_THRESHOLD => 0,
            EpochMode::Auto | EpochMode::Shift => {
                (gap.num_seconds() as f64 / 86400.0).round() as i64
            }
        };
        TimeMap {
            mode,
            shift_days: shift,
            reference,
        }
    }

    pub fn shift(&self) -> Duration {
        Duration::days(self.shift_days)
    }

    pub fn to_prop(&self, scenario: DateTime<Utc>) -> DateTime<Utc> {
        scenario + self.shift()
    }

    pub fn to_scenario(&self, prop: DateTime<Utc>) -> DateTime<Utc> {
        prop - self.shift()
    }

    /// Largest distance between a propagation instant in the window and the reference, days.
    pub fn max_offset_days(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
        let a = (self.to_prop(from) - self.reference).num_seconds().abs();
        let b = (self.to_prop(to) - self.reference).num_seconds().abs();
        a.max(b) as f64 / 86400.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn june_window_shifts_to_october_keeping_time_of_day() {
        let reference = Utc
            .with_ymd_and_hms(2026, 10, 2, 3, 0, 0)
            .single()
            .expect("t");
        let from = Utc
            .with_ymd_and_hms(2026, 6, 14, 0, 0, 0)
            .single()
            .expect("t");
        let to = Utc
            .with_ymd_and_hms(2026, 6, 15, 0, 0, 0)
            .single()
            .expect("t");
        let m = TimeMap::for_window(EpochMode::Auto, reference, from, to);
        assert_eq!(m.shift_days, 110);
        let t = Utc
            .with_ymd_and_hms(2026, 6, 14, 18, 30, 0)
            .single()
            .expect("t");
        let p = m.to_prop(t);
        assert_eq!((p.hour(), p.minute()), (18, 30));
        assert_eq!(m.to_scenario(p), t);
        assert!(m.max_offset_days(from, to) < 1.0);
    }

    #[test]
    fn near_window_is_direct() {
        let reference = Utc
            .with_ymd_and_hms(2026, 10, 2, 3, 0, 0)
            .single()
            .expect("t");
        let from = Utc
            .with_ymd_and_hms(2026, 10, 2, 0, 0, 0)
            .single()
            .expect("t");
        let m = TimeMap::for_window(EpochMode::Auto, reference, from, from + Duration::days(1));
        assert_eq!(m.shift_days, 0);
    }
}
