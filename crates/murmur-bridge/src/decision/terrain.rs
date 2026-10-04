//! Land or water for the domain rule, from tidewater's sampling service.
//!
//! The engine asks its [`TerrainOracle`] synchronously, inside the decision lock. So the
//! bridge fetches an intent's waypoints first (async, `POST /v1/sample`), caches the
//! answers, and the oracle answers only from the cache. A waypoint the cache can't answer
//! makes the whole query fail, which the domain rule records as "not evaluated"
//! (`MURMUR_TIDEWATER_URL` unset, tidewater down, or a timeout): the documented path for a
//! missing terrain source, blocking only consequential kinds.
//!
//! Scenario time: tidewater only answers inside its scenario window, and the water depth
//! there depends on the tide. With `MURMUR_AT` set (as `make goldengate` does), the time asked
//! is `MURMUR_AT` plus the time since the bridge started (times `MURMUR_RATE`), the same
//! clock the vehicles' conditions run on; without it, the current time. Either way it is
//! clamped into the scenario window from `/v1/info`. The surface and depth each decision
//! read are logged with it, so replay doesn't depend on any of this.
//!
//! Waypoints are answered exactly (the point fetched). An engagement's predicted effect
//! point moves with its track every second, so it alone is answered from the nearest
//! sample within [`murmur_decide::engage::TERRAIN_NEAR_M`] (the log says where that sample
//! was and how far), and not at all when the samples that near disagree (a shoreline).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use murmur_decide::{LonLat, NearSample, Surface, TerrainOracle, Time};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::civil;
use crate::clock;

/// A cached answer is used this long; the tide moves the shoreline.
const CACHE_TTL_S: f64 = 600.0;
/// An effect point within this many metres of a cached sample is not fetched again: the
/// samples it is answered from are at most [`murmur_decide::engage::TERRAIN_NEAR_M`] away,
/// and a shoreline shows as neighbours that disagree.
const NEAR_FETCH_M: f64 = murmur_decide::engage::TERRAIN_NEAR_M / 2.0;
const TIMEOUT: Duration = Duration::from_secs(3);
/// After tidewater fails, background fetches wait this long before asking again.
const BACKOFF_S: f64 = 5.0;

type Key = (i64, i64);

struct Cached {
    surface: Surface,
    at: f64,
}

pub struct Terrain {
    client: Option<Tidewater>,
    cache: Mutex<HashMap<Key, Cached>>,
    last_error: Mutex<Option<String>>,
    /// When tidewater last failed (wall s).
    failed_at: Mutex<Option<f64>>,
    /// Tests: a fixed land/water map instead of tidewater.
    #[cfg(test)]
    fixed: Option<murmur_decide::PolygonTerrain>,
}

impl Terrain {
    #[cfg(test)]
    pub fn fixed(map: murmur_decide::PolygonTerrain) -> Terrain {
        Terrain {
            client: None,
            cache: Mutex::default(),
            last_error: Mutex::new(None),
            failed_at: Mutex::new(None),
            fixed: Some(map),
        }
    }

    /// `url`: tidewater serve, e.g. `http://127.0.0.1:8774`; None: no terrain source.
    pub fn new(url: Option<&str>) -> Terrain {
        let (client, err) = match url.map(Tidewater::new) {
            None => (None, None),
            Some(Ok(c)) => (Some(c), None),
            Some(Err(e)) => (None, Some(e)),
        };
        if let Some(e) = &err {
            eprintln!("terrain: {e}; the domain rule will record \"not evaluated\"");
        }
        Terrain {
            client,
            cache: Mutex::default(),
            last_error: Mutex::new(err),
            failed_at: Mutex::new(None),
            #[cfg(test)]
            fixed: None,
        }
    }

    pub fn configured(&self) -> bool {
        self.client.is_some()
    }

    /// Fetch whatever the cache can't answer exactly for these points (waypoints).
    pub async fn prefetch(&self, points: &[LonLat]) {
        self.fetch(points, 0.0).await;
    }

    /// Fetch the effect points no cached sample is near ([`NEAR_FETCH_M`]).
    pub async fn prefetch_near(&self, points: &[LonLat]) {
        self.fetch(points, NEAR_FETCH_M).await;
    }

    /// Tidewater failed less than [`BACKOFF_S`] ago: a background fetch should wait.
    pub fn backing_off(&self) -> bool {
        let at = *self.failed_at.lock().unwrap_or_else(|e| e.into_inner());
        at.is_some_and(|t| clock::wall() - t < BACKOFF_S)
    }

    async fn fetch(&self, points: &[LonLat], skip_within_m: f64) {
        let Some(client) = &self.client else { return };
        let now = clock::wall();
        let missing: Vec<LonLat> = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|_, c| now - c.at < CACHE_TTL_S);
            let mut seen = std::collections::HashSet::new();
            let close = |p: &LonLat| {
                skip_within_m > 0.0
                    && cache.keys().any(|k| {
                        let q = LonLat::new(k.0 as f64 / 1e7, k.1 as f64 / 1e7);
                        murmur_decide::geo::haversine_m(*p, q) <= skip_within_m
                    })
            };
            points
                .iter()
                .filter(|p| !cache.contains_key(&p.e7()) && !close(p) && seen.insert(p.e7()))
                .copied()
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let result = client.sample(&missing).await;
        let mut err = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(surfaces) => {
                let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                for (p, surface) in missing.iter().zip(surfaces) {
                    cache.insert(p.e7(), Cached { surface, at: now });
                }
                *err = None;
                *self.failed_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            Err(e) => {
                eprintln!("terrain: {e}");
                *err = Some(e);
                *self.failed_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(clock::wall());
            }
        }
    }

    /// The synchronous oracle the engine calls: answers from the cache only.
    pub fn oracle(&self) -> CacheOracle<'_> {
        CacheOracle(self)
    }
}

pub struct CacheOracle<'a>(&'a Terrain);

impl TerrainOracle for CacheOracle<'_> {
    fn sample(&self, at: Time, points: &[LonLat]) -> Result<Vec<Surface>, String> {
        let t = self.0;
        #[cfg(test)]
        if let Some(map) = &t.fixed {
            return map.sample(at, points);
        }
        let _ = at; // tidewater is asked at scenario time when prefetching
        t.source()?;
        let cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
        points
            .iter()
            .map(|p| cache.get(&p.e7()).map(|c| c.surface.clone()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| t.not_answered("every waypoint"))
    }

    fn sample_near(&self, at: Time, p: LonLat, within_m: f64) -> Result<NearSample, String> {
        let t = self.0;
        #[cfg(test)]
        if t.fixed.is_some() {
            return self.sample(at, &[p]).map(|s| NearSample {
                surface: s[0].clone(),
                at: p,
                distance_m: 0.0,
            });
        }
        let _ = at;
        t.source()?;
        let cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
        let near: Vec<(f64, LonLat, &Surface)> = cache
            .iter()
            .map(|(k, c)| {
                let q = LonLat::new(k.0 as f64 / 1e7, k.1 as f64 / 1e7);
                (murmur_decide::geo::haversine_m(p, q), q, &c.surface)
            })
            .filter(|(d, _, _)| *d <= within_m)
            .collect();
        nearest_agreeing(&near, within_m).ok_or_else(|| t.not_answered("the effect point"))?
    }
}

impl Terrain {
    /// `Err` when there is no terrain source at all.
    fn source(&self) -> Result<(), String> {
        if self.client.is_some() {
            return Ok(());
        }
        let why = self
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Err(why.unwrap_or_else(|| "no terrain source (MURMUR_TIDEWATER_URL is not set)".into()))
    }

    fn not_answered(&self, what: &str) -> String {
        let why = self
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        format!(
            "tidewater did not answer for {what} ({})",
            why.unwrap_or_else(|| "not fetched".into())
        )
    }
}

/// The nearest of the samples within `within_m` (`None`: there are none), unless they
/// disagree on land, water or coverage (a shoreline, or a coverage edge): then `Err`.
fn nearest_agreeing(
    near: &[(f64, LonLat, &Surface)],
    within_m: f64,
) -> Option<Result<NearSample, String>> {
    let (d, q, s) = near.iter().min_by(|a, b| a.0.total_cmp(&b.0))?;
    if let Some((d2, _, other)) = near.iter().find(|(_, _, o)| !s.same_kind(o)) {
        return Some(Err(format!(
            "samples within {within_m:.0} m of the effect point disagree ({} at {d:.0} m, {} at {d2:.0} m): a shoreline, not evaluated",
            kind(s),
            kind(other)
        )));
    }
    Some(Ok(NearSample {
        surface: (*s).clone(),
        at: *q,
        distance_m: *d,
    }))
}

fn kind(s: &Surface) -> &'static str {
    match s {
        Surface::Water { .. } => "water",
        Surface::Land => "land",
        Surface::OutsideCoverage => "outside coverage",
    }
}

/// Water conditions at a point, for the drift of a silent surface vehicle
/// (`crate::estimate`): the current and the wind, [east, north] m/s; None outside the
/// scenario area or when tidewater has no figure.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Water {
    pub in_area: bool,
    pub current_en: Option<[f64; 2]>,
    pub wind_en: Option<[f64; 2]>,
}

/// `POST /v1/sample` against `tidewater serve`.
pub(crate) struct Tidewater {
    addr: String,
    /// (scenario start, real start, rate) when `MURMUR_AT` is set
    at: Option<(f64, f64, f64)>,
    range: tokio::sync::OnceCell<Option<(f64, f64)>>,
}

impl Tidewater {
    fn new(url: &str) -> Result<Tidewater, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("MURMUR_TIDEWATER_URL {url:?} must start with http://"))?;
        let host = rest.split('/').next().unwrap_or(rest).trim();
        if host.is_empty() {
            return Err(format!("MURMUR_TIDEWATER_URL {url:?} has no host"));
        }
        let addr = if host.contains(':') {
            host.to_string()
        } else {
            format!("{host}:80")
        };
        let at = std::env::var("MURMUR_AT")
            .ok()
            .and_then(|s| civil::parse_utc(&s))
            .map(|at| {
                let rate = std::env::var("MURMUR_RATE")
                    .ok()
                    .and_then(|r| r.parse::<f64>().ok())
                    .filter(|r| r.is_finite() && *r > 0.0)
                    .unwrap_or(1.0);
                (at as f64, clock::wall(), rate)
            });
        Ok(Tidewater {
            addr,
            at,
            range: tokio::sync::OnceCell::new(),
        })
    }

    async fn scenario_t(&self) -> f64 {
        let now = clock::wall();
        let t = match self.at {
            Some((at, start, rate)) => at + (now - start) * rate,
            None => now,
        };
        let range = self
            .range
            .get_or_init(|| async {
                let (status, body) = request(&self.addr, "GET", "/v1/info", None).await.ok()?;
                let v: Value = serde_json::from_slice(&body)
                    .ok()
                    .filter(|_| status == 200)?;
                let r = &v["time_range"];
                Some((r["start_unix"].as_f64()?, r["end_unix"].as_f64()?))
            })
            .await;
        match range {
            Some((a, b)) => t.clamp(*a, *b),
            None => t,
        }
    }

    /// The client the silent-vehicle estimate's water sampler uses, from
    /// `MURMUR_TIDEWATER_URL`; None without one (the estimate then says so).
    pub(crate) fn from_env() -> Option<Tidewater> {
        let url = std::env::var("MURMUR_TIDEWATER_URL").ok()?;
        match Tidewater::new(url.trim()) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("water sampler: {e}");
                None
            }
        }
    }

    /// Current and wind at each point.
    pub(crate) async fn water(&self, points: &[LonLat]) -> Result<Vec<Water>, String> {
        Ok(self.sample_raw(points).await?.iter().map(water).collect())
    }

    async fn sample(&self, points: &[LonLat]) -> Result<Vec<Surface>, String> {
        Ok(self.sample_raw(points).await?.iter().map(surface).collect())
    }

    async fn sample_raw(&self, points: &[LonLat]) -> Result<Vec<Value>, String> {
        let t = self.scenario_t().await;
        let pts: Vec<Value> = points
            .iter()
            .map(|p| serde_json::json!([p.lat, p.lon, null]))
            .collect();
        let body = serde_json::json!({ "t": t, "points": pts }).to_string();
        let (status, resp) =
            request(&self.addr, "POST", "/v1/sample", Some(body.as_bytes())).await?;
        let v: Value =
            serde_json::from_slice(&resp).map_err(|e| format!("tidewater reply: {e}"))?;
        if status != 200 {
            return Err(format!(
                "tidewater status {status}: {}",
                v["error"].as_str().unwrap_or("?")
            ));
        }
        let samples = v["samples"]
            .as_array()
            .filter(|s| s.len() == points.len())
            .ok_or("tidewater reply has the wrong number of samples")?;
        Ok(samples.clone())
    }
}

fn water(s: &Value) -> Water {
    let pair = |e: &str, n: &str| Some([s[e].as_f64()?, s[n].as_f64()?]);
    Water {
        in_area: !s["elevation_m"].is_null(),
        current_en: pair("current_east", "current_north"),
        wind_en: pair("wind_east", "wind_north"),
    }
}

/// `elevation_m: null` is outside coverage; `water_depth_m > 0` is water; else land.
fn surface(s: &Value) -> Surface {
    if s["elevation_m"].is_null() {
        return Surface::OutsideCoverage;
    }
    match s["water_depth_m"].as_f64() {
        Some(d) if d > 0.0 => Surface::Water { depth_m: d },
        _ => Surface::Land,
    }
}

/// One HTTP/1.1 request with `Connection: close`, read to the end (tidewater is local).
async fn request(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let work = async {
        let mut s = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("tidewater {addr}: {e}"))?;
        let b = body.unwrap_or(&[]);
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            b.len()
        );
        s.write_all(head.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        s.write_all(b).await.map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).await.map_err(|e| e.to_string())?;
        parse_response(&raw)
    };
    tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| format!("tidewater {addr}: timeout"))?
}

fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("incomplete HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("bad HTTP status line")?;
    let body = &raw[split + 4..];
    if !head.contains("transfer-encoding: chunked") {
        return Ok((status, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let nl = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunk framing")?;
        let size_text = String::from_utf8_lossy(&rest[..nl]).to_string();
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        rest = &rest[nl + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        if rest.len() < size {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_from_samples() {
        let s = serde_json::json!({"elevation_m": -20.0, "water_depth_m": 21.0,
            "current_east": 1.2, "current_north": -0.4, "wind_east": 5.0, "wind_north": 0.0});
        assert_eq!(
            water(&s),
            Water { in_area: true, current_en: Some([1.2, -0.4]), wind_en: Some([5.0, 0.0]) }
        );
        let out = serde_json::json!({"elevation_m": null, "water_depth_m": null});
        assert_eq!(water(&out), Water::default());
    }

    #[test]
    fn surfaces_from_samples() {
        let s = |v: Value| surface(&v);
        assert_eq!(
            s(serde_json::json!({"elevation_m": null, "water_depth_m": null})),
            Surface::OutsideCoverage
        );
        assert_eq!(
            s(serde_json::json!({"elevation_m": -12.0, "water_depth_m": 13.5})),
            Surface::Water { depth_m: 13.5 }
        );
        assert_eq!(
            s(serde_json::json!({"elevation_m": 20.0, "water_depth_m": 0.0})),
            Surface::Land
        );
    }

    #[test]
    fn no_source_is_an_error_the_rule_records_as_not_evaluated() {
        let t = Terrain::new(None);
        let r = t.oracle().sample(Time(0), &[LonLat::new(-122.47, 37.82)]);
        assert!(r.is_err_and(|e| e.contains("MURMUR_TIDEWATER_URL")));
    }

    #[test]
    fn effect_points_take_a_near_sample_unless_the_neighbours_disagree() {
        let q1 = LonLat::new(-122.4702, 37.82);
        let q2 = LonLat::new(-122.4704, 37.82);
        let water = Surface::Water { depth_m: 12.0 };
        let deeper = Surface::Water { depth_m: 14.0 };
        // none near: not answered
        assert!(nearest_agreeing(&[], 50.0).is_none());
        // the nearest of agreeing samples, with where it was taken
        let near = [(18.0, q1, &water), (35.0, q2, &deeper)];
        let s = nearest_agreeing(&near, 50.0).expect("near").expect("agree");
        assert_eq!((s.surface, s.at, s.distance_m), (water.clone(), q1, 18.0));
        // a shoreline: land and water both within reach
        let near = [(18.0, q1, &water), (35.0, q2, &Surface::Land)];
        let e = nearest_agreeing(&near, 50.0).expect("near").expect_err("shoreline");
        assert!(e.contains("shoreline"), "{e}");
    }

    #[test]
    fn waypoints_are_answered_exactly() {
        let mut t = Terrain::new(Some("http://127.0.0.1:9"));
        assert!(t.configured());
        let p = LonLat::new(-122.47, 37.82);
        t.cache.get_mut().expect("cache").insert(
            p.e7(),
            Cached {
                surface: Surface::Land,
                at: clock::wall(),
            },
        );
        let o = t.oracle();
        assert_eq!(o.sample(Time(0), &[p]), Ok(vec![Surface::Land]));
        // 20 m away: a waypoint is not answered from a neighbour, an effect point is
        let q = murmur_decide::engage::offset(p, 20.0, 0.0);
        assert!(o.sample(Time(0), &[q]).is_err());
        let n = o.sample_near(Time(0), q, 50.0).expect("near sample");
        assert_eq!(n.surface, Surface::Land);
        assert!((n.distance_m - 20.0).abs() < 0.5, "{}", n.distance_m);
    }

    #[test]
    fn chunked_and_plain_responses() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain), Ok((200, b"{}".to_vec())));
        let chunked = b"HTTP/1.1 400 Bad\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Ok((400, b"{}".to_vec())));
    }
}
