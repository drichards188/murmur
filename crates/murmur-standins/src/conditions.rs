//! Real-world water conditions from the tidewater sampling service (`tidewater serve`).
//!
//! One batched `POST /v1/sample` for every marine vehicle about once a second; vehicles read
//! the latest sample for their slot and never wait on HTTP.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Sample {
    pub lat: f64,
    pub lon: f64,
    /// seabed/ground height above datum; None outside the scenario area
    pub elevation_m: Option<f64>,
    pub water_level_m: Option<f64>,
    pub water_depth_m: Option<f64>,
    pub current_east: Option<f64>,
    pub current_north: Option<f64>,
    pub wind_east: Option<f64>,
    pub wind_north: Option<f64>,
}

impl Sample {
    pub fn in_area(&self) -> bool {
        self.elevation_m.is_some()
    }

    /// Enough water under the keel at this tide (tidewater's Sample.navigable).
    pub fn navigable(&self, draft_m: f64) -> bool {
        self.in_area() && self.water_depth_m.is_some_and(|d| d > draft_m)
    }

    pub fn current_east(&self) -> f64 {
        self.current_east.unwrap_or(0.0)
    }

    pub fn current_north(&self) -> f64 {
        self.current_north.unwrap_or(0.0)
    }
}

/// One vehicle's mailbox: it writes where it is, the poller writes what the water is like there.
#[derive(Default)]
pub struct Slot {
    position: Mutex<Option<[f64; 3]>>,
    sample: Mutex<Option<Sample>>,
}

impl Slot {
    pub fn report(&self, lat: f64, lon: f64, depth_m: f64) {
        if let Ok(mut p) = self.position.lock() {
            *p = Some([lat, lon, depth_m]);
        }
    }

    pub fn latest(&self) -> Option<Sample> {
        self.sample.lock().ok().and_then(|s| s.clone())
    }
}

/// Scenario clock: MURMUR_AT plus elapsed wall time times MURMUR_RATE.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub t0_unix: f64,
    pub rate: f64,
    pub wall0: Instant,
}

impl Clock {
    pub fn now(&self) -> f64 {
        self.t0_unix + self.wall0.elapsed().as_secs_f64() * self.rate
    }
}

/// A scenario rate needs a scenario start (murmur-drd): `MURMUR_RATE` other than 1 without
/// `MURMUR_AT` is refused at startup. The stand-ins' GNSS clock would run at the rate from
/// their own start while the bridge's (its decisions, the space segment) runs at 1x from
/// its own: the bridge, the contacts process and the link emulator refuse the same.
pub fn check_scenario_env() -> Result<(), String> {
    scenario_env(
        std::env::var("MURMUR_AT").ok().as_deref(),
        std::env::var("MURMUR_RATE").ok().as_deref(),
    )
}

/// [`check_scenario_env`] on given values (an empty one is unset).
pub fn scenario_env(at: Option<&str>, rate: Option<&str>) -> Result<(), String> {
    let at = at.filter(|s| !s.trim().is_empty());
    let rate = rate.filter(|s| !s.trim().is_empty());
    match (at, rate) {
        (None, Some(r)) if r.trim().parse::<f64>().is_ok_and(|x| x != 1.0) => Err(format!(
            "MURMUR_RATE={r} needs MURMUR_AT: a scenario clock runs at a rate from a start (MURMUR_AT, e.g. 2026-06-14T10:00Z); without MURMUR_AT the scenario clock is the wall clock, at 1x. Set MURMUR_AT, or unset MURMUR_RATE"
        )),
        _ => Ok(()),
    }
}

/// tidewater's parse_time: ISO 8601, `Z` or an offset, naive means UTC, seconds optional.
pub fn parse_time(s: &str) -> Result<f64, String> {
    let s = s.trim();
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Ok(ts.as_millisecond() as f64 / 1000.0);
    }
    let dt: jiff::civil::DateTime = s.parse().map_err(|e| format!("MURMUR_AT={s:?}: {e}"))?;
    let ts = dt
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(|e| e.to_string())?
        .timestamp();
    Ok(ts.as_millisecond() as f64 / 1000.0)
}

pub struct Config {
    pub url: String,
    pub clock: Clock,
}

impl Config {
    /// From MURMUR_TIDEWATER_URL / MURMUR_AT / MURMUR_RATE. None = run without conditions.
    pub fn from_env() -> Option<Config> {
        let url = std::env::var("MURMUR_TIDEWATER_URL")
            .ok()
            .filter(|u| !u.is_empty())?;
        let Some(at) = std::env::var("MURMUR_AT").ok().filter(|a| !a.is_empty()) else {
            println!("MURMUR_TIDEWATER_URL set but no MURMUR_AT; stand-ins run without currents");
            return None;
        };
        let t0_unix = match parse_time(&at) {
            Ok(t) => t,
            Err(e) => {
                println!("{e}; stand-ins run without currents");
                return None;
            }
        };
        let rate = std::env::var("MURMUR_RATE")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(1.0);
        Some(Config {
            url: url.trim_end_matches('/').to_string(),
            clock: Clock {
                t0_unix,
                rate,
                wall0: Instant::now(),
            },
        })
    }
}

#[derive(Serialize)]
struct Request<'a> {
    t: f64,
    points: &'a [[Option<f64>; 3]],
}

#[derive(Deserialize)]
struct Response {
    samples: Vec<Sample>,
}

pub const POLL_PERIOD: Duration = Duration::from_secs(1);

/// First query (awaited, so vehicles start with conditions, as standins.py does), then poll
/// in the background forever. Slots must already hold the spawn positions.
pub async fn start(cfg: Config, slots: Vec<Arc<Slot>>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            println!("tidewater client: {e}; stand-ins run without currents");
            return;
        }
    };
    let endpoint = format!("{}/v1/sample", cfg.url);
    println!(
        "tidewater conditions from {} at t={} x{}",
        cfg.url, cfg.clock.t0_unix, cfg.clock.rate
    );
    let mut last_error = None;
    report(
        sample_once(&client, &endpoint, cfg.clock.now(), &slots).await,
        &mut last_error,
    );
    tokio::spawn(async move {
        let mut every =
            tokio::time::interval_at(tokio::time::Instant::now() + POLL_PERIOD, POLL_PERIOD);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            report(
                sample_once(&client, &endpoint, cfg.clock.now(), &slots).await,
                &mut last_error,
            );
        }
    });
}

/// Say it once per outage; the vehicles keep the last samples meanwhile.
fn report(r: Result<(), String>, last_error: &mut Option<String>) {
    match r {
        Ok(()) => {
            if last_error.take().is_some() {
                println!("tidewater: back");
            }
        }
        Err(e) => {
            if last_error.as_deref() != Some(e.as_str()) {
                println!("tidewater: {e}");
                *last_error = Some(e);
            }
        }
    }
}

/// One batched request. Public so tests can drive it against a mock server.
pub async fn sample_once(
    client: &reqwest::Client,
    endpoint: &str,
    t: f64,
    slots: &[Arc<Slot>],
) -> Result<(), String> {
    let mut asked = Vec::with_capacity(slots.len());
    let mut points = Vec::with_capacity(slots.len());
    for (i, s) in slots.iter().enumerate() {
        if let Some([lat, lon, depth]) = s.position.lock().ok().and_then(|p| *p) {
            asked.push(i);
            points.push([Some(lat), Some(lon), Some(depth)]);
        }
    }
    if points.is_empty() {
        return Ok(());
    }
    let resp = client
        .post(endpoint)
        .json(&Request { t, points: &points })
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        ));
    }
    let body: Response = resp
        .json()
        .await
        .map_err(|e| format!("bad response: {e}"))?;
    if body.samples.len() != asked.len() {
        return Err(format!(
            "asked for {} points, got {} samples",
            asked.len(),
            body.samples.len()
        ));
    }
    for (i, sample) in asked.into_iter().zip(body.samples) {
        if let Ok(mut s) = slots[i].sample.lock() {
            *s = Some(sample);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// murmur-drd: a rate without a start is refused (the GNSS clock ran at it from the
    /// stand-ins' own start while the bridge's ran at 1x); with a start, or at 1x, it runs.
    #[test]
    fn a_rate_needs_a_start() {
        let err = scenario_env(None, Some("10")).expect_err("refused");
        assert!(err.starts_with("MURMUR_RATE=10 needs MURMUR_AT"), "{err}");
        assert!(scenario_env(Some(" "), Some("2")).is_err());
        assert!(scenario_env(None, Some("1")).is_ok());
        assert!(scenario_env(None, None).is_ok());
        assert!(scenario_env(Some("2026-06-14T10:00Z"), Some("10")).is_ok());
    }

    #[test]
    fn parses_the_times_tidewater_accepts() {
        let t = 1_781_431_200.0; // 2026-06-14T10:00:00Z
        assert_eq!(parse_time("2026-06-14T10:00Z").ok(), Some(t));
        assert_eq!(parse_time("2026-06-14T10:00:00+00:00").ok(), Some(t));
        assert_eq!(parse_time("2026-06-14T10:00:00").ok(), Some(t));
        assert_eq!(parse_time("2026-06-14T12:00:00+02:00").ok(), Some(t));
        assert!(parse_time("June 14").is_err());
    }

    #[test]
    fn navigable_needs_area_and_depth() {
        let mut s = Sample {
            elevation_m: Some(-5.0),
            water_depth_m: Some(5.0),
            ..Default::default()
        };
        assert!(s.navigable(2.5));
        assert!(!s.navigable(5.0));
        s.elevation_m = None;
        assert!(!s.navigable(0.5));
    }
}
