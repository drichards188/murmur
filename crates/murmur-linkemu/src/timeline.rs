//! Scenario time. The stand-ins map wall time to scenario time as `MURMUR_AT` (an ISO start)
//! plus elapsed seconds times `MURMUR_RATE`; the emulator uses the same convention, with
//! "elapsed" counted from the emulator's own start (so start it with the `MURMUR_AT` you want
//! the scenario clock to read at that moment). Without `MURMUR_AT` the scenario clock is the
//! wall clock at the start plus elapsed seconds (as the stand-ins' is): it never steps when
//! the wall clock does (murmur-dup: this host's steps back about 0.65 s every 32 s).

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug)]
pub struct Timeline {
    /// Scenario time (unix seconds) at emulator start; None when no scenario clock is set.
    pub at_unix: Option<f64>,
    /// Scenario seconds per wall second.
    pub rate: f64,
    /// The wall clock (unix seconds) when the timeline was made: the scenario clock's start
    /// without `MURMUR_AT`.
    pub wall0: f64,
}

impl Default for Timeline {
    fn default() -> Self {
        Timeline {
            at_unix: None,
            rate: 1.0,
            wall0: wall_unix(),
        }
    }
}

fn wall_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl Timeline {
    /// The scenario clock from `--at` / `MURMUR_AT` and `--rate` / `MURMUR_RATE`, read as every
    /// process reads them (`murmur_space::scenario_clock`, murmur-drd): the start an RFC 3339
    /// date-time and nothing else, the rate a positive number, one other than 1 only with a
    /// start (without one this clock is the wall clock, at 1x). Anything else is refused.
    pub fn new(at: Option<&str>, rate: Option<&str>) -> Result<Timeline, String> {
        let s = murmur_space::scenario_clock::scenario_env(at, rate)?;
        Ok(Timeline {
            at_unix: s.at_unix(),
            rate: s.rate,
            wall0: wall_unix(),
        })
    }

    /// `MURMUR_AT` and `MURMUR_RATE` from the environment.
    pub fn from_env() -> Result<Timeline, String> {
        Timeline::new(
            std::env::var("MURMUR_AT").ok().as_deref(),
            std::env::var("MURMUR_RATE").ok().as_deref(),
        )
    }

    /// The timeline as of now: its wall-clock start (the scenario clock's without
    /// `MURMUR_AT`) read again, at the moment the emulator's elapsed time starts.
    pub fn started(self) -> Timeline {
        Timeline {
            wall0: wall_unix(),
            ..self
        }
    }

    /// Scenario unix time `elapsed_s` wall seconds after the emulator started. Without a
    /// scenario clock this is the wall clock at the start plus `elapsed_s` (it was the wall
    /// clock now, which steps).
    pub fn scenario_unix(&self, elapsed_s: f64) -> f64 {
        match self.at_unix {
            Some(at) => at + elapsed_s * self.rate,
            None => self.wall0 + elapsed_s,
        }
    }

    /// Emulator elapsed seconds at which the scenario clock reads `unix` (may be negative).
    pub fn elapsed_for(&self, unix: f64) -> Result<f64, String> {
        let at = self.at_unix.ok_or(
            "scenario-time events need MURMUR_AT (or --at) to say what the scenario clock reads at start",
        )?;
        Ok((unix - at) / self.rate)
    }

    /// A span of scenario seconds expressed in emulator seconds.
    pub fn span_elapsed(&self, scenario_s: f64) -> f64 {
        scenario_s / self.rate
    }
}

/// Parses `YYYY-MM-DD[T| ]HH:MM[:SS[.fff]]` with an optional `Z` or `+HH:MM` offset (UTC when
/// absent) into unix seconds.
pub fn parse_iso_utc(s: &str) -> Result<f64, String> {
    let bad = || format!("bad ISO time {s:?} (want 2026-06-14T10:00:00Z)");
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' ']).ok_or_else(bad)?;
    let mut d = date.split('-');
    let num = |x: Option<&str>| x.and_then(|v| v.parse::<i64>().ok()).ok_or_else(bad);
    let (y, mo, da) = (num(d.next())?, num(d.next())?, num(d.next())?);
    if d.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return Err(bad());
    }
    // Split the clock from the zone designator.
    let (clock, offset_s) = if let Some(c) = rest.strip_suffix(['Z', 'z']) {
        (c, 0i64)
    } else if let Some(i) = rest.rfind(['+', '-']).filter(|&i| i > 0) {
        let (c, z) = rest.split_at(i);
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let z = &z[1..];
        let (hh, mm) = z.split_once(':').unwrap_or((z, "0"));
        let hh = hh.parse::<i64>().map_err(|_| bad())?;
        let mm = mm.parse::<i64>().map_err(|_| bad())?;
        (c, sign * (hh * 3600 + mm * 60))
    } else {
        (rest, 0)
    };
    let mut c = clock.split(':');
    let h = num(c.next())?;
    let mi = num(c.next())?;
    let sec = c
        .next()
        .map_or(Ok(0.0), |v| v.parse::<f64>().map_err(|_| bad()))?;
    if c.next().is_some()
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0.0..61.0).contains(&sec)
    {
        return Err(bad());
    }
    // Days from civil (Howard Hinnant).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok((days * 86_400 + h * 3600 + mi * 60 - offset_s) as f64 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_with_zone_forms() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00Z"), Ok(0.0));
        // 2026-06-14T10:00:00Z, checked against `date -u -d ... +%s`.
        assert_eq!(parse_iso_utc("2026-06-14T10:00:00Z"), Ok(1_781_431_200.0));
        assert_eq!(parse_iso_utc("2026-06-14T10:00:00"), Ok(1_781_431_200.0));
        assert_eq!(
            parse_iso_utc("2026-06-14 12:00:00+02:00"),
            Ok(1_781_431_200.0)
        );
        assert_eq!(
            parse_iso_utc("2026-06-14T05:00:00-05:00"),
            Ok(1_781_431_200.0)
        );
        assert_eq!(parse_iso_utc("2026-06-14T10:00:30.5Z"), Ok(1_781_431_230.5));
        assert!(parse_iso_utc("tomorrow").is_err());
        assert!(parse_iso_utc("2026-13-01T00:00:00Z").is_err());
    }

    #[test]
    fn scenario_clock_follows_rate() {
        let t = Timeline::new(Some("2026-06-14T10:00:00Z"), Some("10")).expect("tl");
        assert_eq!(t.scenario_unix(6.0), 1_781_431_260.0);
        // Scenario 10:05 is 30 emulator seconds in at 10x.
        let e = t.elapsed_for(parse_iso_utc("2026-06-14T10:05:00Z").expect("iso"));
        assert_eq!(e, Ok(30.0));
        assert!(Timeline::default().elapsed_for(1.0).is_err());
        assert!(Timeline::new(None, Some("0")).is_err());
        // murmur-drd: a rate needs a start (it was ignored without one)
        let err = Timeline::new(None, Some("10")).expect_err("refused");
        assert!(err.starts_with("MURMUR_RATE=10 needs MURMUR_AT"), "{err}");
        assert!(Timeline::new(None, Some("1")).is_ok());
        assert!(Timeline::new(Some(""), None).is_ok());
        // and (the review) a start every process reads alike: RFC 3339 only, an offset the
        // same instant
        assert!(Timeline::new(Some("garbage"), Some("10")).is_err());
        assert!(Timeline::new(Some("2026-06-14T10:00Z"), None).is_err());
        let t = Timeline::new(Some("2026-06-14T05:00:00-05:00"), Some("10")).expect("tl");
        assert_eq!(t.scenario_unix(6.0), 1_781_431_260.0);
    }

    /// murmur-dup: without `MURMUR_AT` the scenario clock (satellite geometry, scenario-time
    /// events) runs on elapsed seconds from the start, not on the wall clock, which steps.
    #[test]
    fn without_a_scenario_clock_time_runs_on_elapsed_seconds() {
        let t = Timeline::default();
        let a = t.scenario_unix(10.0);
        let b = t.scenario_unix(42.5);
        assert_eq!(b - a, 32.5);
        assert!((a - (t.wall0 + 10.0)).abs() < 1e-6);
        // started with the elapsed time's origin, not when the timeline was made
        let made = Timeline {
            wall0: t.wall0 - 30.0,
            ..t
        };
        let s = made.started();
        assert!(s.wall0 >= t.wall0, "{} {}", s.wall0, t.wall0);
        assert_eq!(s.rate, made.rate);
    }
}
