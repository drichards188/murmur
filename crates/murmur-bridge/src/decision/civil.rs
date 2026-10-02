//! UTC calendar arithmetic for the run id and for `MURMUR_AT` (no date crate needed).

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `20261002T153000Z` for a Unix time in seconds.
pub fn compact_utc(unix_s: i64) -> String {
    let (y, mo, d) = civil_from_days(unix_s.div_euclid(86_400));
    let s = unix_s.rem_euclid(86_400);
    format!(
        "{y:04}{mo:02}{d:02}T{:02}{:02}{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// Parse `2026-06-14T10:00Z`, `2026-06-14T10:00:30Z` or `2026-06-14T10:00:30+00:00` (UTC only)
/// into Unix seconds.
pub fn parse_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix("+00:00"))
        .unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, d) = (dp.next()??, dp.next()??, dp.next()??);
    let mut tp = time.split(':').map(|x| x.parse::<i64>().ok());
    let h = tp.next()??;
    let mi = tp.next()??;
    let sec = tp.next().flatten().unwrap_or(0);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 6, 14)), (2026, 6, 14));
        assert_eq!(parse_utc("2026-06-14T10:00Z"), Some(1_781_431_200));
        assert_eq!(parse_utc("2026-06-14T10:00:00+00:00"), Some(1_781_431_200));
        assert_eq!(compact_utc(1_781_431_200), "20260614T100000Z");
        assert_eq!(parse_utc("June"), None);
    }
}
