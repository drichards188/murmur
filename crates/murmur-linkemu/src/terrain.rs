//! Terrain line of sight from tidewater's DEM.
//!
//! The packet path never waits for HTTP. A link asks `Terrain::obstruction_m` with its
//! vehicle position; the answer comes from a cache keyed by grid cell (about `cell_m`
//! metres), altitude band and antenna parameters. A miss queues the cell and returns `None`
//! ("unknown", treated as open and counted). A background task wakes every `interval_s`,
//! takes the queued cells, samples the terrain profile between the ground station and each
//! cell centre, all cells in as few `POST /v1/sample` requests as fit, and fills the cache.
//! Terrain does not change, so an answer is kept for good.

use crate::config::TerrainCfg;
use crate::geo::{GeoPoint, LosParams, Terrain, haversine_m};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const EARTH_R_M: f64 = 6_371_000.0;
const M_PER_DEG: f64 = 111_320.0;

/// How far the terrain sticks up above the ray, the maximum over the profile.
///
/// `surface` holds the surface height (metres above MSL) at the interior sample points, which
/// are evenly spaced along a path of `total_m` between an antenna at `h_a_m` and one at
/// `h_b_m` (heights above MSL). The terrain is raised by the Earth bulge for an effective
/// radius of `k` Earth radii, `d1 * d2 / (2 k R)`, and, if `fresnel_mhz` is set, by
/// `fresnel_clear` of the first Fresnel radius. Positive: obstructed by that many metres.
pub fn path_obstruction_m(
    surface: &[f64],
    total_m: f64,
    h_a_m: f64,
    h_b_m: f64,
    p: &LosParams,
) -> f64 {
    let n = surface.len() + 1;
    let mut worst = f64::NEG_INFINITY;
    for (i, &g) in surface.iter().enumerate() {
        let f = (i + 1) as f64 / n as f64;
        let d1 = f * total_m;
        let d2 = total_m - d1;
        let bulge = d1 * d2 / (2.0 * p.k * EARTH_R_M);
        let fresnel = if p.fresnel_mhz > 0.0 {
            // First Fresnel radius, metres: 17.32 sqrt(d1 d2 / (f D)) with km and GHz.
            let (d1k, d2k) = (d1 / 1000.0, d2 / 1000.0);
            p.fresnel_clear
                * 17.32
                * (d1k * d2k / (p.fresnel_mhz / 1000.0 * (total_m / 1000.0))).sqrt()
        } else {
            0.0
        };
        let ray = h_a_m + (h_b_m - h_a_m) * f;
        worst = worst.max(g + bulge + fresnel - ray);
    }
    worst.max(-10_000.0)
}

pub type SampleFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Option<f64>>, String>> + Send + 'a>>;

/// Where surface heights come from. Returns, per point, the height of the surface a radio
/// path must clear (terrain, or the water surface where that is higher), `None` outside the
/// DEM.
pub trait Sampler: Send + Sync {
    fn sample<'a>(&'a self, t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a>;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Key {
    ci: i64,
    cj: i64,
    /// the altitude band (`alt_step_m`), or for a vehicle at rest its altitude in decimetres
    band: i64,
    at_rest: bool,
    p: [u64; 5],
}

/// A vehicle at rest's altitude in the key, to this resolution (m).
const AT_REST_STEP_M: f64 = 0.1;

struct Req {
    key: Key,
    gs: GeoPoint,
    p: LosParams,
}

#[derive(Default)]
struct State {
    cache: HashMap<Key, f64>,
    pending: VecDeque<Req>,
    queued: HashSet<Key>,
}

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    request_errors: AtomicU64,
    points: AtomicU64,
    points_outside: AtomicU64,
    cells_answered: AtomicU64,
    misses: AtomicU64,
    last_request_ms: AtomicU64,
}

pub struct TidewaterTerrain {
    cfg: TerrainCfg,
    state: Mutex<State>,
    n: Counters,
    last_error: Mutex<String>,
}

impl TidewaterTerrain {
    pub fn new(cfg: TerrainCfg) -> TidewaterTerrain {
        TidewaterTerrain {
            cfg,
            state: Mutex::new(State::default()),
            n: Counters::default(),
            last_error: Mutex::new(String::new()),
        }
    }

    fn key(&self, v: &GeoPoint, gs: &GeoPoint, p: &LosParams, at_rest: bool) -> Key {
        let dlat = self.cfg.cell_m / M_PER_DEG;
        let dlon = dlat / gs.lat.to_radians().cos().max(0.05);
        let step = if at_rest {
            AT_REST_STEP_M
        } else {
            self.cfg.alt_step_m
        };
        Key {
            ci: (v.lat / dlat).floor() as i64,
            cj: (v.lon / dlon).floor() as i64,
            band: (v.alt_m / step).round() as i64,
            at_rest,
            p: p.key(),
        }
    }

    fn band_alt_m(&self, k: &Key) -> f64 {
        let step = if k.at_rest {
            AT_REST_STEP_M
        } else {
            self.cfg.alt_step_m
        };
        k.band as f64 * step
    }

    /// Cells waiting for an answer and cells answered.
    pub fn sizes(&self) -> (usize, usize) {
        self.state
            .lock()
            .map_or((0, 0), |s| (s.pending.len(), s.cache.len()))
    }

    /// The sample points of one cell: the ground station to the cell centre, interior points.
    fn profile_points(&self, r: &Req) -> (Vec<(f64, f64)>, f64, f64, f64) {
        let dlat = self.cfg.cell_m / M_PER_DEG;
        let dlon = dlat / r.gs.lat.to_radians().cos().max(0.05);
        let centre = GeoPoint {
            lat: (r.key.ci as f64 + 0.5) * dlat,
            lon: (r.key.cj as f64 + 0.5) * dlon,
            alt_m: self.band_alt_m(&r.key),
        };
        let total = haversine_m(&r.gs, &centre);
        let n = ((total / self.cfg.sample_spacing_m).ceil() as usize)
            .clamp(2, self.cfg.max_points.max(2));
        let pts = (1..n)
            .map(|i| {
                let f = i as f64 / n as f64;
                (
                    r.gs.lat + (centre.lat - r.gs.lat) * f,
                    r.gs.lon + (centre.lon - r.gs.lon) * f,
                )
            })
            .collect();
        (
            pts,
            total,
            r.gs.alt_m + r.p.gs_antenna_m,
            centre.alt_m + r.p.antenna_m,
        )
    }

    /// One round of work: sample the queued cells at scenario time `t` and fill the cache.
    /// Returns the number of cells answered.
    pub async fn tick(&self, sampler: &dyn Sampler, t: f64) -> usize {
        let batch: Vec<Req> = {
            let Ok(mut st) = self.state.lock() else {
                return 0;
            };
            let n = st.pending.len().min(self.cfg.max_cells_per_tick.max(1));
            st.pending.drain(..n).collect()
        };
        if batch.is_empty() {
            return 0;
        }
        let profiles: Vec<_> = batch.iter().map(|r| self.profile_points(r)).collect();
        let mut done = 0;
        let mut i = 0;
        while i < batch.len() {
            // Pack cells into one request up to the point limit (at least one cell).
            let mut j = i;
            let mut npts = 0;
            while j < batch.len()
                && (j == i || npts + profiles[j].0.len() <= self.cfg.max_points_per_request)
            {
                npts += profiles[j].0.len();
                j += 1;
            }
            let pts: Vec<(f64, f64)> = profiles[i..j]
                .iter()
                .flat_map(|p| p.0.iter().copied())
                .collect();
            let started = std::time::Instant::now();
            self.n.requests.fetch_add(1, Relaxed);
            match sampler.sample(t, &pts).await {
                Ok(h) if h.len() == pts.len() => {
                    self.n.points.fetch_add(pts.len() as u64, Relaxed);
                    self.n
                        .last_request_ms
                        .store(started.elapsed().as_millis() as u64, Relaxed);
                    let mut off = 0;
                    let mut st = match self.state.lock() {
                        Ok(s) => s,
                        Err(_) => return done,
                    };
                    if st.cache.len() + (j - i) > self.cfg.cache_max {
                        st.cache.clear();
                    }
                    for k in i..j {
                        let (ref p, total, h_a, h_b) = profiles[k];
                        let surf: Vec<f64> = h[off..off + p.len()]
                            .iter()
                            .map(|v| {
                                v.unwrap_or_else(|| {
                                    self.n.points_outside.fetch_add(1, Relaxed);
                                    0.0
                                })
                            })
                            .collect();
                        off += p.len();
                        let o = path_obstruction_m(&surf, total, h_a, h_b, &batch[k].p);
                        st.cache.insert(batch[k].key, o);
                        st.queued.remove(&batch[k].key);
                        done += 1;
                    }
                    self.n.cells_answered.fetch_add((j - i) as u64, Relaxed);
                }
                other => {
                    let msg = match other {
                        Ok(h) => format!(
                            "tidewater returned {} samples for {} points",
                            h.len(),
                            pts.len()
                        ),
                        Err(e) => e,
                    };
                    self.n.request_errors.fetch_add(1, Relaxed);
                    if let Ok(mut le) = self.last_error.lock() {
                        *le = msg;
                    }
                    // Put every cell not yet answered back at the front and wait for the
                    // next round.
                    if let Ok(mut st) = self.state.lock() {
                        for r in batch.into_iter().skip(i).rev() {
                            st.pending.push_front(r);
                        }
                    }
                    return done;
                }
            }
            i = j;
        }
        done
    }

    /// The background loop: every `interval_s`, answer what is queued. `clock` gives the
    /// scenario unix time for the request.
    pub async fn run(
        self: Arc<Self>,
        sampler: Arc<dyn Sampler>,
        clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) {
        let mut every =
            tokio::time::interval(Duration::from_secs_f64(self.cfg.interval_s.max(0.05)));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            self.tick(&*sampler, clock()).await;
        }
    }
}

impl TidewaterTerrain {
    fn cached_or_queued(&self, key: Key, gs: &GeoPoint, p: &LosParams) -> Option<f64> {
        let mut st = self.state.lock().ok()?;
        if let Some(o) = st.cache.get(&key) {
            return Some(*o);
        }
        if st.queued.len() < 1_000_000 && st.queued.insert(key) {
            self.n.misses.fetch_add(1, Relaxed);
            st.pending.push_back(Req {
                key,
                gs: *gs,
                p: *p,
            });
        }
        None
    }
}

impl Terrain for TidewaterTerrain {
    fn obstruction_m(&self, vehicle: &GeoPoint, gs: &GeoPoint, p: &LosParams) -> Option<f64> {
        self.cached_or_queued(self.key(vehicle, gs, p, false), gs, p)
    }

    /// Keyed at the vehicle's own altitude (to `AT_REST_STEP_M`), not its band.
    fn obstruction_at_rest_m(
        &self,
        vehicle: &GeoPoint,
        gs: &GeoPoint,
        p: &LosParams,
    ) -> Option<f64> {
        self.cached_or_queued(self.key(vehicle, gs, p, true), gs, p)
    }

    fn stats(&self) -> Option<Value> {
        let (pending, cached) = self.sizes();
        let err = self
            .last_error
            .lock()
            .map(|e| e.clone())
            .unwrap_or_default();
        Some(json!({
            "source": "tidewater",
            "cells_cached": cached,
            "cells_pending": pending,
            "cells_answered": self.n.cells_answered.load(Relaxed),
            "cache_misses": self.n.misses.load(Relaxed),
            "requests": self.n.requests.load(Relaxed),
            "request_errors": self.n.request_errors.load(Relaxed),
            "points_sampled": self.n.points.load(Relaxed),
            "points_outside_dem": self.n.points_outside.load(Relaxed),
            "last_request_ms": self.n.last_request_ms.load(Relaxed),
            "last_error": if err.is_empty() { Value::Null } else { json!(err) },
        }))
    }
}

// ---- tidewater over HTTP -------------------------------------------------------------------

/// `POST /v1/sample` against `tidewater serve`.
pub struct HttpSampler {
    addr: String,
    timeout: Duration,
    /// Valid scenario time range, learned from `/v1/info` on first use (None if unavailable).
    range: Mutex<Option<(f64, f64)>>,
    range_tried: Mutex<bool>,
}

impl HttpSampler {
    pub fn new(url: &str, timeout_s: f64) -> Result<HttpSampler, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("tidewater url {url:?} must start with http://"))?;
        let addr = rest.split('/').next().unwrap_or(rest).trim();
        if addr.is_empty() {
            return Err(format!("tidewater url {url:?} has no host"));
        }
        let addr = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("{addr}:80")
        };
        Ok(HttpSampler {
            addr,
            timeout: Duration::from_secs_f64(timeout_s.max(0.5)),
            range: Mutex::new(None),
            range_tried: Mutex::new(false),
        })
    }

    async fn learn_range(&self) {
        {
            let Ok(mut tried) = self.range_tried.lock() else {
                return;
            };
            if *tried {
                return;
            }
            *tried = true;
        }
        if let Ok((200, body)) =
            http_request(&self.addr, "GET", "/v1/info", None, self.timeout).await
            && let Ok(v) = serde_json::from_slice::<Value>(&body)
        {
            let r = &v["time_range"];
            if let (Some(a), Some(b)) = (r["start_unix"].as_f64(), r["end_unix"].as_f64())
                && let Ok(mut g) = self.range.lock()
            {
                *g = Some((a, b));
            }
        }
    }
}

impl Sampler for HttpSampler {
    fn sample<'a>(&'a self, t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a> {
        Box::pin(async move {
            self.learn_range().await;
            // tidewater refuses times outside its scenario; elevation does not depend on
            // time, so clamp rather than fail.
            let t = match self.range.lock().ok().and_then(|g| *g) {
                Some((a, b)) => t.clamp(a, b),
                None => t,
            };
            let mut body = String::with_capacity(points.len() * 30 + 40);
            body.push_str(&format!("{{\"t\":{t},\"points\":["));
            for (i, (lat, lon)) in points.iter().enumerate() {
                if i > 0 {
                    body.push(',');
                }
                body.push_str(&format!("[{lat:.6},{lon:.6},null]"));
            }
            body.push_str("]}");
            let (status, resp) = http_request(
                &self.addr,
                "POST",
                "/v1/sample",
                Some(body.as_bytes()),
                self.timeout,
            )
            .await?;
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
                .ok_or("tidewater reply has no samples")?;
            Ok(samples
                .iter()
                .map(|s| {
                    let e = s["elevation_m"].as_f64()?;
                    let w = s["water_level_m"].as_f64().unwrap_or(f64::NEG_INFINITY);
                    Some(e.max(w))
                })
                .collect())
        })
    }
}

/// The surface height (metres above MSL; terrain, or the water level where that is higher) at
/// one point, or `None` outside the DEM. Used at startup to check the ground station altitude.
pub async fn dem_height(
    sampler: &dyn Sampler,
    t: f64,
    lat: f64,
    lon: f64,
) -> Result<Option<f64>, String> {
    let v = sampler.sample(t, &[(lat, lon)]).await?;
    Ok(v.first().copied().flatten())
}

/// A minimal HTTP/1.1 client: one request, `Connection: close`, reads to the end.
pub async fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let work = async {
        let mut s = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect {addr}: {e}"))?;
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
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| format!("tidewater {addr}: timeout"))?
}

fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("incomplete HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("bad HTTP status line")?;
    let mut chunked = false;
    let mut len = None;
    for l in lines {
        let l = l.to_ascii_lowercase();
        if let Some(v) = l.strip_prefix("content-length:") {
            len = v.trim().parse::<usize>().ok();
        } else if l.starts_with("transfer-encoding:") && l.contains("chunked") {
            chunked = true;
        }
    }
    let body = &raw[split + 4..];
    if chunked {
        let mut out = Vec::new();
        let mut rest = body;
        loop {
            let nl = rest
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or("bad chunk framing")?;
            let size = usize::from_str_radix(
                String::from_utf8_lossy(&rest[..nl])
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim(),
                16,
            )
            .map_err(|_| "bad chunk size")?;
            rest = &rest[nl + 2..];
            if size == 0 {
                break;
            }
            if rest.len() < size + 2 {
                return Err("truncated chunk".into());
            }
            out.extend_from_slice(&rest[..size]);
            rest = &rest[size + 2..];
        }
        return Ok((status, out));
    }
    let body = match len {
        Some(n) if body.len() >= n => &body[..n],
        Some(_) => return Err("truncated HTTP body".into()),
        None => body,
    };
    Ok((status, body.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gs() -> GeoPoint {
        GeoPoint {
            lat: 37.8068,
            lon: -122.446,
            alt_m: 5.0,
        }
    }

    #[test]
    fn flat_sea_is_clear_at_short_range_and_hidden_by_the_horizon_at_long_range() {
        let p = LosParams::default();
        // 5 km, both antennas 50 m up, sea level terrain: the bulge is ~ 0.5 * 2500^2/(2*4/3*6371e3) = 0.18 m.
        let near = vec![0.0; 99];
        assert!(path_obstruction_m(&near, 5_000.0, 50.0, 50.0, &p) < -49.0);
        // Radio horizon of a 10 m antenna with k = 4/3 is 4.12 * sqrt(4/3 * 10) km ~ 15 km, so a
        // sea-level target at 25 km is far beyond it.
        let far = vec![0.0; 499];
        let o = path_obstruction_m(&far, 25_000.0, 10.0, 0.0, &p);
        assert!(o > 3.0, "{o}");
        // A tall vehicle raises the horizon: at 25 km, 300 m up is visible from 10 m.
        let o = path_obstruction_m(&far, 25_000.0, 10.0, 300.0, &p);
        assert!(o < 0.0, "{o}");
    }

    #[test]
    fn ridge_between_the_ends_obstructs_by_its_height_above_the_ray() {
        let p = LosParams {
            k: 1.0e9, // flat Earth, to isolate the ridge
            ..LosParams::default()
        };
        let mut h = vec![0.0; 99];
        h[49] = 80.0; // mid path, ends at 20 m
        let o = path_obstruction_m(&h, 10_000.0, 20.0, 20.0, &p);
        assert!((o - 60.0).abs() < 0.01, "{o}");
        // Raise the vehicle to 160 m: the ray at the ridge is 90 m, clear by 10.
        let o = path_obstruction_m(&h, 10_000.0, 20.0, 160.0, &p);
        assert!((o + 10.0).abs() < 0.01, "{o}");
    }

    #[test]
    fn fresnel_clearance_demands_more_than_a_bare_ray() {
        let mut h = vec![0.0; 99];
        h[49] = 20.0;
        let bare = LosParams {
            k: 1.0e9,
            ..LosParams::default()
        };
        let fres = LosParams {
            fresnel_mhz: 900.0,
            ..bare
        };
        let a = path_obstruction_m(&h, 10_000.0, 30.0, 30.0, &bare);
        let b = path_obstruction_m(&h, 10_000.0, 30.0, 30.0, &fres);
        assert!(a < 0.0, "bare ray clears the ridge: {a}");
        // F1 at the middle of 10 km at 900 MHz is 17.32*sqrt(25/(0.9*10)) = 28.9 m; 60% = 17.3.
        assert!((b - a - 17.3).abs() < 0.2, "{a} {b}");
        assert!(b > a);
    }

    #[test]
    fn parses_content_length_and_chunked_responses() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(parse_response(r), Ok((200, b"hello".to_vec())));
        let r = b"HTTP/1.1 400 Bad\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_response(r), Ok((400, b"abcde".to_vec())));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhi").is_err());
    }

    /// A DEM that is flat at sea level except for a north-south ridge at one longitude.
    struct RidgeSampler {
        ridge_lon: f64,
        height: f64,
        fail: std::sync::atomic::AtomicBool,
        calls: AtomicU64,
    }

    impl Sampler for RidgeSampler {
        fn sample<'a>(&'a self, _t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Relaxed);
                if self.fail.load(Relaxed) {
                    return Err("down".into());
                }
                Ok(points
                    .iter()
                    .map(|(_, lon)| {
                        Some(if (lon - self.ridge_lon).abs() < 0.0015 {
                            self.height
                        } else {
                            0.0
                        })
                    })
                    .collect())
            })
        }
    }

    fn cfg() -> TerrainCfg {
        TerrainCfg {
            cell_m: 100.0,
            sample_spacing_m: 50.0,
            ..TerrainCfg::default()
        }
    }

    fn veh(lon: f64, alt: f64) -> GeoPoint {
        GeoPoint {
            lat: 37.8068,
            lon,
            alt_m: alt,
        }
    }

    #[tokio::test]
    async fn unknown_until_the_worker_answers_then_ridge_blocks_and_height_clears() {
        let t = TidewaterTerrain::new(cfg());
        let s = RidgeSampler {
            ridge_lon: -122.43,
            height: 100.0,
            fail: Default::default(),
            calls: Default::default(),
        };
        let p = LosParams {
            gs_antenna_m: 10.0,
            ..LosParams::default()
        };
        let behind = veh(-122.40, 50.0); // ridge between, ray ~ 30 m high at the ridge
        let above = veh(-122.40, 400.0);
        let east_of_gs_clear = veh(-122.44, 50.0); // no ridge in between
        for v in [&behind, &above, &east_of_gs_clear] {
            assert_eq!(t.obstruction_m(v, &gs(), &p), None);
        }
        // Asking again does not queue a second request for the same cell.
        assert_eq!(t.obstruction_m(&behind, &gs(), &p), None);
        assert_eq!(t.sizes().0, 3);
        assert_eq!(t.tick(&s, 0.0).await, 3);
        assert_eq!(s.calls.load(Relaxed), 1, "all cells in one request");
        let ob = t.obstruction_m(&behind, &gs(), &p).expect("answered");
        assert!(ob > 40.0, "{ob}");
        let oa = t.obstruction_m(&above, &gs(), &p).expect("answered");
        assert!(oa < 0.0, "{oa}");
        let oc = t
            .obstruction_m(&east_of_gs_clear, &gs(), &p)
            .expect("answered");
        assert!(oc < 0.0, "{oc}");
        // Nearby position in the same cell shares the answer.
        let again = veh(-122.40 + 0.0002, 52.0);
        assert_eq!(t.obstruction_m(&again, &gs(), &p), Some(ob));
        assert_eq!(t.sizes(), (0, 3));
    }

    /// A vehicle at rest is judged at its own altitude: down at 3.5 m, behind a 20 m ridge, the
    /// banded check rounds it to 0 m (more obstructed by the 3.5 m it lost); at rest it keeps
    /// them. Its own cell entry: the band's answer for a moving vehicle there is unchanged.
    #[tokio::test]
    async fn a_vehicle_at_rest_keeps_its_own_altitude_where_the_band_rounds_it_away() {
        let t = TidewaterTerrain::new(cfg());
        let s = RidgeSampler {
            ridge_lon: -122.43,
            height: 20.0,
            fail: Default::default(),
            calls: Default::default(),
        };
        let p = LosParams {
            gs_antenna_m: 10.0,
            k: 1.0e9, // flat Earth, to isolate the ridge
            ..LosParams::default()
        };
        let down = veh(-122.40, 3.5);
        assert_eq!(t.obstruction_m(&down, &gs(), &p), None);
        assert_eq!(t.obstruction_at_rest_m(&down, &gs(), &p), None);
        assert_eq!(t.tick(&s, 0.0).await, 2, "two cells: banded and at rest");
        let banded = t.obstruction_m(&down, &gs(), &p).expect("answered");
        let rest = t.obstruction_at_rest_m(&down, &gs(), &p).expect("answered");
        // the ridge is about 35 % of the way out: the ray over it rises by that share of 3.5 m
        let lift = banded - rest;
        assert!(
            (3.5 * 0.3..3.5 * 0.4).contains(&lift),
            "the ray at the ridge rises by its share of the 3.5 m: {banded} vs {rest}"
        );
        assert!(rest > 0.0 && rest < banded);
    }

    #[tokio::test]
    async fn a_failed_request_keeps_the_cells_queued_and_is_counted() {
        let t = TidewaterTerrain::new(cfg());
        let s = RidgeSampler {
            ridge_lon: 0.0,
            height: 0.0,
            fail: std::sync::atomic::AtomicBool::new(true),
            calls: Default::default(),
        };
        let p = LosParams::default();
        assert_eq!(t.obstruction_m(&veh(-122.40, 50.0), &gs(), &p), None);
        assert_eq!(t.tick(&s, 0.0).await, 0);
        assert_eq!(t.sizes(), (1, 0));
        let st = t.stats().expect("stats");
        assert_eq!(st["request_errors"], 1);
        assert_eq!(st["last_error"], "down");
        s.fail.store(false, Relaxed);
        assert_eq!(t.tick(&s, 0.0).await, 1);
        assert!(t.obstruction_m(&veh(-122.40, 50.0), &gs(), &p).is_some());
    }

    #[tokio::test]
    async fn requests_are_split_at_the_point_limit() {
        let t = TidewaterTerrain::new(TerrainCfg {
            max_points_per_request: 200,
            ..cfg()
        });
        let s = RidgeSampler {
            ridge_lon: 0.0,
            height: 0.0,
            fail: Default::default(),
            calls: Default::default(),
        };
        let p = LosParams::default();
        // These are 2 to 5 km out: 46 to 100 points each; three fit in 200, so 4 cells take 2 requests.
        for i in 0..4 {
            let v = veh(-122.42 + f64::from(i) * 0.01, 50.0);
            assert_eq!(t.obstruction_m(&v, &gs(), &p), None);
        }
        assert_eq!(t.tick(&s, 0.0).await, 4);
        assert_eq!(s.calls.load(Relaxed), 2);
    }

    #[tokio::test]
    async fn http_sampler_talks_to_a_tidewater_shaped_server() {
        use tokio::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:18772").await.expect("bind");
        tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((mut c, _)) = l.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 65536];
                let mut got = 0;
                // Read until the whole request (headers plus Content-Length body) is in.
                loop {
                    let n = c.read(&mut buf[got..]).await.unwrap_or(0);
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let cl = text
                            .to_ascii_lowercase()
                            .split("content-length:")
                            .nth(1)
                            .and_then(|r| {
                                r.split("\r\n")
                                    .next()
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if got >= i + 4 + cl || n == 0 {
                            break;
                        }
                    } else if n == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf[..got]).to_string();
                let body = if text.starts_with("GET /v1/info") {
                    r#"{"time_range":{"start_unix":1000.0,"end_unix":2000.0}}"#.to_string()
                } else {
                    assert!(text.contains("\"t\":2000"), "time is clamped: {text}");
                    r#"{"t":2000,"samples":[{"elevation_m":12.0,"water_level_m":1.0},{"elevation_m":-5.0,"water_level_m":1.5},{"elevation_m":null,"water_level_m":null}]}"#.to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = c.write_all(resp.as_bytes()).await;
            }
        });
        let s = HttpSampler::new("http://127.0.0.1:18772", 5.0).expect("sampler");
        let out = s
            .sample(9_999_999.0, &[(37.8, -122.4), (37.8, -122.41), (0.0, 0.0)])
            .await
            .expect("sample");
        assert_eq!(out, vec![Some(12.0), Some(1.5), None]);
    }
}
