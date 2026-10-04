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

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use murmur_decide::{LonLat, Surface, TerrainOracle, Time};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::civil;
use crate::clock;

/// A cached answer is used this long; the tide moves the shoreline.
const CACHE_TTL_S: f64 = 600.0;
/// A point the cache has no exact answer for takes the nearest cached answer within this
/// many metres. An engagement's predicted effect point moves with its track every second, so
/// the point prefetched and the point checked a moment later differ; tidewater's own depth
/// grid is coarser than this.
const NEAR_M: f64 = 50.0;
const TIMEOUT: Duration = Duration::from_secs(3);

type Key = (i64, i64);

struct Cached {
    surface: Surface,
    at: f64,
}

pub struct Terrain {
    client: Option<Tidewater>,
    cache: Mutex<HashMap<Key, Cached>>,
    last_error: Mutex<Option<String>>,
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
            #[cfg(test)]
            fixed: None,
        }
    }

    pub fn configured(&self) -> bool {
        self.client.is_some()
    }

    /// Fetch whatever the cache can't answer for these points.
    pub async fn prefetch(&self, points: &[LonLat]) {
        let Some(client) = &self.client else { return };
        let now = clock::wall();
        let missing: Vec<LonLat> = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|_, c| now - c.at < CACHE_TTL_S);
            let mut seen = std::collections::HashSet::new();
            // a point within half the near radius of a cached answer needs no new request
            let close = |p: &LonLat| {
                cache.keys().any(|k| {
                    let q = LonLat::new(k.0 as f64 / 1e7, k.1 as f64 / 1e7);
                    murmur_decide::geo::haversine_m(*p, q) <= NEAR_M / 2.0
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
            }
            Err(e) => {
                eprintln!("terrain: {e}");
                *err = Some(e);
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
        if t.client.is_none() {
            let why = t
                .last_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            return Err(
                why.unwrap_or_else(|| "no terrain source (MURMUR_TIDEWATER_URL is not set)".into())
            );
        }
        let cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
        let near = |p: &LonLat| {
            cache
                .iter()
                .map(|(k, c)| {
                    let q = LonLat::new(k.0 as f64 / 1e7, k.1 as f64 / 1e7);
                    (murmur_decide::geo::haversine_m(*p, q), c)
                })
                .filter(|(d, _)| *d <= NEAR_M)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, c)| c.surface.clone())
        };
        points
            .iter()
            .map(|p| cache.get(&p.e7()).map(|c| c.surface.clone()).or_else(|| near(p)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                let why = t
                    .last_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                format!(
                    "tidewater did not answer for every waypoint ({})",
                    why.unwrap_or_else(|| "not fetched".into())
                )
            })
    }
}

/// `POST /v1/sample` against `tidewater serve`.
struct Tidewater {
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

    async fn sample(&self, points: &[LonLat]) -> Result<Vec<Surface>, String> {
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
        Ok(samples.iter().map(surface).collect())
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
    fn chunked_and_plain_responses() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain), Ok((200, b"{}".to_vec())));
        let chunked = b"HTTP/1.1 400 Bad\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Ok((400, b"{}".to_vec())));
    }
}
