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

    /// The surface's height: the ground, or the water over it (the higher of elevation and
    /// water level, as the link emulator reads tidewater); None outside the scenario area.
    pub fn surface_m(&self) -> Option<f64> {
        let e = self.elevation_m?;
        Some(self.water_level_m.map_or(e, |w| e.max(w)))
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

    /// Where the vehicle last said it was ([lat, lon, depth]): for a source of conditions other
    /// than the poller below (the headless simulator's baked water).
    pub fn position(&self) -> Option<[f64; 3]> {
        self.position.lock().ok().and_then(|p| *p)
    }

    /// A sample for the vehicle, as the poller below puts one.
    pub fn put(&self, sample: Sample) {
        if let Ok(mut s) = self.sample.lock() {
            *s = Some(sample);
        }
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

/// The scenario clock as every process reads it (murmur-drd): `MURMUR_AT` an RFC 3339
/// date-time and nothing else, a positive rate, one other than 1 only with a start.
pub use murmur_space::scenario_clock::{ScenarioStart, from_env as scenario_from_env, scenario_env};

/// The scenario clock's environment, refused at startup when it is not one every process
/// reads alike (`murmur_space::scenario_clock`): a start that is not RFC 3339 (the review of
/// murmur-drd: the bridge ignored one it couldn't read while the GNSS clock here ran the rate
/// from the stand-ins' own start), a rate that is not a positive number, a rate other than 1
/// without a start. The bridge, the contacts process and the link emulator refuse the same.
pub fn check_scenario_env() -> Result<(), String> {
    murmur_space::scenario_clock::from_env().map(|_| ())
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
        let (t0_unix, rate) = match murmur_space::scenario_clock::from_env() {
            Ok(s) => match s.at_unix() {
                Some(t) => (t, s.rate),
                None => {
                    println!("MURMUR_TIDEWATER_URL set but no MURMUR_AT; stand-ins run without currents");
                    return None;
                }
            },
            Err(e) => {
                println!("{e}; stand-ins run without currents");
                return None;
            }
        };
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

/// The surface's height at each point now (`Sample::surface_m`; None outside the area), in
/// one request: where a vehicle that starts in the air has its home (`VehicleConfig::
/// surface_m`). Asked once, at start.
pub async fn surfaces(cfg: &Config, points: &[(f64, f64)]) -> Result<Vec<Option<f64>>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    surfaces_at(
        &client,
        &format!("{}/v1/sample", cfg.url),
        cfg.clock.now(),
        points,
    )
    .await
}

/// [`surfaces`] against an endpoint at scenario time `t`. Public so tests can drive it.
pub async fn surfaces_at(
    client: &reqwest::Client,
    endpoint: &str,
    t: f64,
    points: &[(f64, f64)],
) -> Result<Vec<Option<f64>>, String> {
    let slots: Vec<Arc<Slot>> = points
        .iter()
        .map(|&(lat, lon)| {
            let s = Arc::new(Slot::default());
            s.report(lat, lon, 0.0);
            s
        })
        .collect();
    sample_once(client, endpoint, t, &slots).await?;
    Ok(slots
        .iter()
        .map(|s| s.latest().and_then(|x| x.surface_m()))
        .collect())
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
        let s = scenario_env(Some("2026-06-14T10:00:00Z"), Some("10")).expect("taken");
        assert_eq!((s.at_unix(), s.rate), (Some(1_781_431_200.0), 10.0));
        // the review: a start the stand-ins can't read as RFC 3339 is refused too (their GNSS
        // clock ran the rate from their own start while the bridge ignored it), including the
        // short form tidewater's parse_time takes, so every process takes the same starts
        assert!(scenario_env(Some("garbage"), Some("10")).is_err());
        assert!(scenario_env(Some("2026-06-14T10:00Z"), Some("10")).is_err());
        assert!(parse_time("2026-06-14T10:00Z").is_ok(), "event times stay tidewater's");
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
