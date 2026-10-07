//! The scenario clock every Murmur process runs (murmur-drd): `MURMUR_AT`, what the scenario
//! clock reads when the process starts, and `MURMUR_RATE`, scenario seconds per second. One
//! reader for all of them (the bridge, the stand-ins, the contacts process, the link
//! emulator; `scripts/up.sh` checks the same shape first), so a start one process accepts
//! every process accepts, and reads as the same instant.
//!
//! - `MURMUR_AT` is an RFC 3339 date-time and nothing else: `2026-06-14T10:00:00Z`, an offset
//!   (`2026-06-14T05:00:00-05:00`), fractional seconds allowed, `T`/`Z` in either case. A
//!   value that is not one is refused (before, each process parsed it with its own library and
//!   rules: one ignored a start another took, or read an offset as UTC).
//! - `MURMUR_RATE` is a positive number (default 1); one other than 1 needs a start.
//!
//! Unset or empty means not given.

use chrono::{DateTime, Utc};

/// What `MURMUR_AT` must look like, for messages.
pub const START_FORMAT: &str = "an RFC 3339 date-time, e.g. 2026-06-14T10:00:00Z";

/// The scenario clock a process runs: its start (none: the wall clock) and its rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScenarioStart {
    pub at: Option<DateTime<Utc>>,
    pub rate: f64,
}

impl ScenarioStart {
    /// The start in unix seconds, if there is one.
    pub fn at_unix(&self) -> Option<f64> {
        self.at.map(|t| t.timestamp_millis() as f64 / 1000.0)
    }
}

/// `s` as an RFC 3339 date-time (`YYYY-MM-DDTHH:MM:SS[.fraction](Z|+HH:MM|-HH:MM)`), in UTC.
pub fn parse_start(s: &str) -> Result<DateTime<Utc>, String> {
    let b = s.as_bytes();
    let digits = |from: usize, to: usize| b.get(from..to).is_some_and(|x| x.iter().all(u8::is_ascii_digit));
    let at = |i: usize, c: &[u8]| b.get(i).is_some_and(|x| c.contains(x));
    let mut ok = digits(0, 4)
        && at(4, b"-")
        && digits(5, 7)
        && at(7, b"-")
        && digits(8, 10)
        && at(10, b"Tt")
        && digits(11, 13)
        && at(13, b":")
        && digits(14, 16)
        && at(16, b":")
        && digits(17, 19);
    let mut i = 19;
    if ok && at(i, b".") {
        let from = i + 1;
        i = from;
        while at(i, b"0123456789") {
            i += 1;
        }
        ok = i > from;
    }
    let zone = b.get(i..).unwrap_or_default();
    ok = ok
        && match zone {
            [b'Z' | b'z'] => true,
            [b'+' | b'-', _, _, b':', _, _] => digits(i + 1, i + 3) && digits(i + 4, i + 6),
            _ => false,
        };
    if !ok {
        return Err(format!("{s:?} is not {START_FORMAT}"));
    }
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| format!("{s:?} is not {START_FORMAT}: {e}"))
}

/// The scenario clock from `MURMUR_AT` and `MURMUR_RATE` as given (None or empty: not
/// given): a start that is not RFC 3339, a rate that is not a positive number, or a rate other
/// than 1 without a start is refused, saying why.
pub fn scenario_env(at: Option<&str>, rate: Option<&str>) -> Result<ScenarioStart, String> {
    let at = match at.filter(|s| !s.is_empty()) {
        Some(s) => Some(parse_start(s).map_err(|e| format!("MURMUR_AT={e}"))?),
        None => None,
    };
    let rate = match rate.filter(|s| !s.is_empty()) {
        Some(r) => match r.parse::<f64>() {
            Ok(x) if x.is_finite() && x > 0.0 => x,
            _ => return Err(format!("MURMUR_RATE={r:?} is not a positive number (scenario seconds per second)")),
        },
        None => 1.0,
    };
    if at.is_none() && rate != 1.0 {
        return Err(format!(
            "MURMUR_RATE={rate} needs MURMUR_AT: a scenario clock runs at a rate from a start (MURMUR_AT, {START_FORMAT}); without MURMUR_AT the scenario clock is the wall clock, at 1x. Set MURMUR_AT, or unset MURMUR_RATE"
        ));
    }
    Ok(ScenarioStart { at, rate })
}

/// [`scenario_env`] on this process's environment.
pub fn from_env() -> Result<ScenarioStart, String> {
    scenario_env(
        std::env::var("MURMUR_AT").ok().as_deref(),
        std::env::var("MURMUR_RATE").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const T: i64 = 1_781_431_200_000; // 2026-06-14T10:00:00Z

    #[test]
    fn a_start_is_rfc_3339_and_nothing_else() {
        for s in [
            "2026-06-14T10:00:00Z",
            "2026-06-14t10:00:00z",
            "2026-06-14T10:00:00.000Z",
            "2026-06-14T05:00:00-05:00",
            "2026-06-14T12:00:00+02:00",
        ] {
            assert_eq!(parse_start(s).unwrap().timestamp_millis(), T, "{s}");
        }
        for s in [
            "2026-06-14T10:00Z",
            "2026-06-14T10:00",
            "2026-06-14",
            "2026-06-14T10:00:00",
            "2026-06-14 10:00:00Z",
            " 2026-06-14T10:00:00Z",
            "2026-06-14T10:00:00Z ",
            "2026-06-14T10:00:00.Z",
            "2026-06-14T10:00:00+0200",
            "2026-13-14T10:00:00Z",
            "2026-06-31T10:00:00Z",
            "June 14",
            "",
        ] {
            assert!(parse_start(s).is_err(), "{s:?}");
        }
    }

    #[test]
    fn the_environment_is_refused_whole_or_taken_whole() {
        let ok = scenario_env(Some("2026-06-14T10:00:00Z"), Some("10")).unwrap();
        assert_eq!((ok.at_unix(), ok.rate), (Some(1_781_431_200.0), 10.0));
        assert_eq!(scenario_env(None, None).unwrap(), ScenarioStart { at: None, rate: 1.0 });
        assert_eq!(scenario_env(Some(""), Some("1")).unwrap().at, None);
        // the review's case: a start that doesn't parse, with a rate (one process ran 1x,
        // another 10x)
        let e = scenario_env(Some("garbage"), Some("10")).unwrap_err();
        assert!(e.starts_with("MURMUR_AT=\"garbage\" is not an RFC 3339 date-time"), "{e}");
        assert!(scenario_env(Some("garbage"), None).is_err());
        assert!(scenario_env(Some("2026-06-14T10:00Z"), None).is_err(), "the short form is not RFC 3339");
        let e = scenario_env(None, Some("10")).unwrap_err();
        assert!(e.starts_with("MURMUR_RATE=10 needs MURMUR_AT"), "{e}");
        for r in ["0", "-1", "fast", "NaN", "inf", " 10"] {
            assert!(scenario_env(Some("2026-06-14T10:00:00Z"), Some(r)).is_err(), "{r}");
        }
    }
}
