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
//! Rules 17: the points along each leg of a route are fetched from where the vehicles are just
//! before the decision (`murmur_decide::route::along_points`); a point the vehicle's moving has
//! shifted since is answered from the nearest cached sample within
//! [`murmur_decide::route::LEG_NEAR_M`] (the log says how far, per leg).
//!
//! Without tidewater, a baked land/water grid (`MURMUR_LAND_WATER_GRID`, world v3's H1; the
//! headless simulator gives it from its world) answers instead, at once and from the grid
//! alone: tidewater's water depth (water level minus elevation) sampled once on a regular grid
//! (eval/headless/bake_world.py, [`Grid`]). A waypoint is the depth there, bilinear between the
//! four samples around it (outside coverage next to a sample without data, as tidewater outside
//! its DEM); an effect point is the nearest grid sample within reach, and not answered when the
//! samples that near disagree, as with the cache. The tide stays at the time it was baked. With
//! a tidewater URL the grid is not read: live behaves as before.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use murmur_decide::{LonLat, NearSample, Surface, TerrainOracle, Time};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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
    /// The baked land/water grid, read only without tidewater (module doc).
    grid: Option<Arc<Grid>>,
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
            grid: None,
            cache: Mutex::default(),
            last_error: Mutex::new(None),
            failed_at: Mutex::new(None),
            fixed: Some(map),
        }
    }

    /// `url`: tidewater serve, e.g. `http://127.0.0.1:8774`; None: no terrain source.
    #[cfg(test)]
    pub fn new(url: Option<&str>) -> Terrain {
        Terrain::with_grid(url, None)
    }

    /// [`Terrain::new`], and without a tidewater URL the baked land/water grid at `grid` (its
    /// water depth, [`Grid`]) as the source; a grid that can't be read leaves no source, said
    /// once. With a URL the grid is not read.
    pub fn with_grid(url: Option<&str>, grid: Option<&Path>) -> Terrain {
        let (client, mut err) = match url.map(Tidewater::new) {
            None => (None, None),
            Some(Ok(c)) => (Some(c), None),
            Some(Err(e)) => (None, Some(e)),
        };
        let grid = match (url, grid) {
            (None, Some(p)) => match Grid::load(p) {
                Ok(g) => Some(Arc::new(g)),
                Err(e) => {
                    err = Some(format!("land/water grid: {e}"));
                    None
                }
            },
            _ => None,
        };
        if let Some(e) = &err {
            eprintln!("terrain: {e}; the domain rule will record \"not evaluated\"");
        }
        Terrain {
            client,
            grid,
            cache: Mutex::default(),
            last_error: Mutex::new(err),
            failed_at: Mutex::new(None),
            #[cfg(test)]
            fixed: None,
        }
    }

    pub fn configured(&self) -> bool {
        self.client.is_some() || self.grid.is_some()
    }

    /// The baked grid answers (no tidewater): which, for the startup line.
    pub fn grid_source(&self) -> Option<String> {
        self.grid.as_ref().map(|g| g.source.clone())
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
        // one of this store's locks at a time, never one under another: the oracle reads the
        // cache and then the last error, under the decision lock
        match result {
            Ok(surfaces) => {
                {
                    let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                    for (p, surface) in missing.iter().zip(surfaces) {
                        cache.insert(p.e7(), Cached { surface, at: now });
                    }
                }
                *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *self.failed_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            Err(e) => {
                eprintln!("terrain: {e}");
                *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
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
        if let Some(g) = t.grid.as_ref().filter(|_| t.client.is_none()) {
            return Ok(points.iter().map(|p| g.surface(*p)).collect());
        }
        // the cache guard is dropped before `not_answered` takes the last error
        let found = {
            let cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
            points
                .iter()
                .map(|p| cache.get(&p.e7()).map(|c| c.surface.clone()))
                .collect::<Option<Vec<_>>>()
        };
        found.ok_or_else(|| t.not_answered("every waypoint"))
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
        if let Some(g) = t.grid.as_ref().filter(|_| t.client.is_none()) {
            let near = g.samples_within(p, within_m);
            let near: Vec<(f64, LonLat, &Surface)> = near.iter().map(|(d, q, s)| (*d, *q, s)).collect();
            return nearest_agreeing(&near, within_m)
                .unwrap_or_else(|| Err(format!("no land/water grid sample within {within_m:.0} m of the effect point")));
        }
        // the cache guard is dropped before `not_answered` takes the last error
        let found = {
            let cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
            let near: Vec<(f64, LonLat, &Surface)> = cache
                .iter()
                .map(|(k, c)| {
                    let q = LonLat::new(k.0 as f64 / 1e7, k.1 as f64 / 1e7);
                    (murmur_decide::geo::haversine_m(p, q), q, &c.surface)
                })
                .filter(|(d, _, _)| *d <= within_m)
                .collect();
            nearest_agreeing(&near, within_m)
        };
        found.ok_or_else(|| t.not_answered("the effect point"))?
    }

    /// Rules 17: the points along a route's legs. The grid answers each itself; the cache each
    /// from its own sample or, when the vehicle moved since they were fetched (the points are
    /// measured from the leg's far end, so each moved little), from the nearest cached sample
    /// within `within_m`, and says how far. A point with neither fails the query (not evaluated).
    fn sample_along(&self, at: Time, points: &[LonLat], within_m: f64) -> Result<Vec<NearSample>, String> {
        let t = self.0;
        #[cfg(test)]
        if t.fixed.is_some() {
            return self.sample(at, points).map(|s| {
                points
                    .iter()
                    .zip(s)
                    .map(|(p, surface)| NearSample { surface, at: *p, distance_m: 0.0 })
                    .collect()
            });
        }
        let _ = at;
        t.source()?;
        if let Some(g) = t.grid.as_ref().filter(|_| t.client.is_none()) {
            return Ok(points
                .iter()
                .map(|p| NearSample { surface: g.surface(*p), at: *p, distance_m: 0.0 })
                .collect());
        }
        // the cache guard is dropped before `not_answered` takes the last error
        let found = {
            let cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
            points
                .iter()
                .map(|p| match cache.get(&p.e7()) {
                    Some(c) => Some(NearSample { surface: c.surface.clone(), at: *p, distance_m: 0.0 }),
                    None => nearest_cached(&cache, *p, within_m),
                })
                .collect::<Option<Vec<_>>>()
        };
        found.ok_or_else(|| t.not_answered("every point along the route"))
    }
}

/// The nearest cached sample within `within_m` of `p` (a cheap box test on the keys first).
fn nearest_cached(cache: &HashMap<Key, Cached>, p: LonLat, within_m: f64) -> Option<NearSample> {
    let (lon_e7, lat_e7) = p.e7();
    let dlat = (within_m / 111_320.0 * 1e7).ceil() as i64;
    let dlon = (within_m / (111_320.0 * p.lat.to_radians().cos().max(0.01)) * 1e7).ceil() as i64;
    cache
        .iter()
        .filter(|(k, _)| (k.0 - lon_e7).abs() <= dlon && (k.1 - lat_e7).abs() <= dlat)
        .map(|(k, c)| {
            let q = LonLat::new(k.0 as f64 / 1e7, k.1 as f64 / 1e7);
            (murmur_decide::geo::haversine_m(p, q), q, &c.surface)
        })
        .filter(|(d, _, _)| *d <= within_m)
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.e7().cmp(&b.1.e7())))
        .map(|(d, q, s)| NearSample { surface: s.clone(), at: q, distance_m: d })
}

impl Terrain {
    /// `Err` when there is no terrain source at all.
    fn source(&self) -> Result<(), String> {
        if self.client.is_some() || self.grid.is_some() {
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

/// A baked grid's header (eval/headless/bake_world.py): the first sample's `lat0`, `lon0`, the
/// spacing `dlat`, `dlon` (degrees), the size `nlat` x `nlon`, `scale` metres a unit and the
/// `nodata` value (where tidewater had no data).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GridHeader {
    pub lat0: f64,
    pub lon0: f64,
    pub dlat: f64,
    pub dlon: f64,
    pub nlat: usize,
    pub nlon: usize,
    pub scale: f64,
    pub nodata: i16,
}

/// A baked grid: gzip of one JSON header line ([`GridHeader`]), then `nlat x nlon`
/// little-endian i16 row by row from the south-west. The world's land/water grid holds
/// tidewater's water depth (water level minus elevation: above 0 water, else land), its surface
/// grid the surface height the headless link emulator's terrain line of sight reads
/// (crates/murmur-headless/src/sim/terrain.rs, which reads it with this).
pub struct Grid {
    pub h: GridHeader,
    data: Vec<i16>,
    /// The file it was read from (said at startup).
    pub source: String,
}

impl Grid {
    pub fn load(path: &Path) -> Result<Grid, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(file)
            .read_to_end(&mut raw)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let nl = raw
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| format!("{}: no header line", path.display()))?;
        let h: GridHeader =
            serde_json::from_slice(&raw[..nl]).map_err(|e| format!("{}: header: {e}", path.display()))?;
        let body = &raw[nl + 1..];
        if body.len() != h.nlat * h.nlon * 2 {
            return Err(format!(
                "{}: {} bytes of samples for a {}x{} grid",
                path.display(),
                body.len(),
                h.nlat,
                h.nlon
            ));
        }
        let data = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect();
        Ok(Grid {
            h,
            data,
            source: path.display().to_string(),
        })
    }

    fn at(&self, i: usize, j: usize) -> Option<f64> {
        let v = *self.data.get(i * self.h.nlon + j)?;
        (v != self.h.nodata).then(|| f64::from(v) * self.h.scale)
    }

    /// The value at a point, bilinear between the four samples around it; None outside the
    /// grid or next to a sample without data (as tidewater outside its DEM).
    pub fn height(&self, lat: f64, lon: f64) -> Option<f64> {
        let y = (lat - self.h.lat0) / self.h.dlat;
        let x = (lon - self.h.lon0) / self.h.dlon;
        if !(y >= 0.0 && x >= 0.0) {
            return None;
        }
        let (i, j) = (y.floor() as usize, x.floor() as usize);
        if i + 1 >= self.h.nlat || j + 1 >= self.h.nlon {
            return None;
        }
        let (fy, fx) = (y - i as f64, x - j as f64);
        let a = self.at(i, j)?;
        let b = self.at(i, j + 1)?;
        let c = self.at(i + 1, j)?;
        let d = self.at(i + 1, j + 1)?;
        Some((a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy)
    }

    /// What a land/water grid says is at a point: water as deep as its depth there, land, or
    /// outside coverage (tidewater's `surface`).
    pub fn surface(&self, p: LonLat) -> Surface {
        depth_surface(self.height(p.lat, p.lon))
    }

    /// Every sample of a land/water grid within `r_m` of `p`: how far, where, what it says.
    pub fn samples_within(&self, p: LonLat, r_m: f64) -> Vec<(f64, LonLat, Surface)> {
        let h = &self.h;
        let reach = |d_deg: f64| (r_m / (d_deg * 111_320.0)).ceil() as i64 + 1;
        let (di, dj) = (reach(h.dlat), reach(h.dlon * p.lat.to_radians().cos().max(0.01)));
        let ci = ((p.lat - h.lat0) / h.dlat).round() as i64;
        let cj = ((p.lon - h.lon0) / h.dlon).round() as i64;
        let mut out = Vec::new();
        for i in (ci - di).max(0)..=(ci + di).min(h.nlat as i64 - 1) {
            for j in (cj - dj).max(0)..=(cj + dj).min(h.nlon as i64 - 1) {
                let q = LonLat::new(h.lon0 + j as f64 * h.dlon, h.lat0 + i as f64 * h.dlat);
                let d = murmur_decide::geo::haversine_m(p, q);
                if d <= r_m {
                    out.push((d, q, depth_surface(self.at(i as usize, j as usize))));
                }
            }
        }
        out
    }
}

/// A water depth as tidewater's surface: none is outside coverage, above 0 water, else land.
fn depth_surface(depth_m: Option<f64>) -> Surface {
    match depth_m {
        None => Surface::OutsideCoverage,
        Some(d) if d > 0.0 => Surface::Water { depth_m: d },
        Some(_) => Surface::Land,
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
        // the scenario clock as every process reads it (murmur_space::scenario_clock; the
        // bridge refused anything else at startup)
        let at = murmur_space::scenario_clock::from_env()
            .ok()
            .and_then(|s| Some((s.at_unix()?, clock::wall(), s.rate)));
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

    /// Review B1: the oracle (under the decision lock) never holds the cache while it takes
    /// the last error. A writer that holds the last error and then wants the cache (as
    /// `fetch` once did) deadlocked against a lookup that missed; now the lookup lets the
    /// cache go first, for both kinds of query.
    #[test]
    fn the_oracle_never_holds_the_cache_while_taking_the_last_error() {
        use std::sync::Arc;
        use std::sync::mpsc::channel;
        let t = Arc::new(Terrain::new(Some("http://127.0.0.1:9")));
        assert!(t.configured());
        for near in [false, true] {
            let (held_tx, held_rx) = channel();
            let writer = {
                let t = t.clone();
                std::thread::spawn(move || {
                    let _err = t.last_error.lock().unwrap_or_else(|e| e.into_inner());
                    held_tx.send(()).ok();
                    std::thread::sleep(Duration::from_millis(300));
                    let _cache = t.cache.lock().unwrap_or_else(|e| e.into_inner());
                })
            };
            held_rx.recv().expect("the writer holds the last error");
            let (done_tx, done_rx) = channel();
            let reader = {
                let t = t.clone();
                std::thread::spawn(move || {
                    let p = LonLat::new(-122.49, 37.81);
                    let r = if near {
                        t.oracle().sample_near(Time(0), p, 50.0).map(|_| ())
                    } else {
                        t.oracle().sample(Time(0), &[p]).map(|_| ())
                    };
                    done_tx.send(r.is_err()).ok();
                })
            };
            let missed = done_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("no deadlock: the oracle held the cache while taking the last error");
            assert!(missed, "nothing cached: the lookup says so");
            writer.join().expect("writer");
            reader.join().expect("reader");
        }
    }

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

    /// Rules 17: a point along a leg is answered from its own cached sample, or, the vehicle
    /// having moved since the points were fetched, from the nearest cached one within reach,
    /// saying how far; with none that near the query fails (the rule: not evaluated).
    #[test]
    fn points_along_a_leg_take_the_nearest_cached_sample_within_reach() {
        let mut t = Terrain::new(Some("http://127.0.0.1:9"));
        let p = LonLat::new(-122.47, 37.82);
        t.cache.get_mut().expect("cache").insert(p.e7(), Cached { surface: Surface::Land, at: clock::wall() });
        let o = t.oracle();
        let own = o.sample_along(Time(0), &[p], 25.0).expect("its own");
        assert_eq!((own[0].surface.clone(), own[0].distance_m), (Surface::Land, 0.0));
        let q = murmur_decide::engage::offset(p, 20.0, 0.0);
        let near = o.sample_along(Time(0), &[q], 25.0).expect("near");
        assert_eq!((near[0].surface.clone(), near[0].at.e7()), (Surface::Land, p.e7()));
        assert!((near[0].distance_m - 20.0).abs() < 0.5, "{}", near[0].distance_m);
        let far = murmur_decide::engage::offset(p, 30.0, 0.0);
        let e = o.sample_along(Time(0), &[q, far], 25.0).expect_err("one too far");
        assert!(e.contains("every point along the route"), "{e}");
    }

    /// A 3 x 4 land/water grid at 0.001 degrees: water depth in decimetres, the west column
    /// land, the north-east sample without data.
    fn small_grid(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;
        let header = serde_json::json!({"lat0": 37.80, "lon0": -122.50, "dlat": 0.001, "dlon": 0.001,
            "nlat": 3, "nlon": 4, "scale": 0.1, "nodata": -32768});
        let rows: [[i16; 4]; 3] = [[-50, 100, 120, 140], [-40, 110, 130, 150], [-30, 120, 140, -32768]];
        let mut raw = format!("{header}\n").into_bytes();
        for r in rows {
            for v in r {
                raw.extend_from_slice(&v.to_le_bytes());
            }
        }
        let path = dir.join("lw.grid.gz");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).expect("gzip");
        std::fs::write(&path, gz.finish().expect("gzip")).expect("write");
        path
    }

    /// H1: without tidewater the baked grid answers the domain rule: water as deep as the grid
    /// says, land, outside coverage beyond it or next to a sample without data; an effect point
    /// from the nearest sample unless the samples near it disagree (a shoreline). With a URL the
    /// grid is not read (live as before), and no grid is no source.
    #[test]
    fn the_land_water_grid_answers_without_tidewater() {
        let dir = std::env::temp_dir().join(format!("murmur-lw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = small_grid(&dir);
        let t = Terrain::with_grid(None, Some(&path));
        assert!(t.configured());
        assert!(t.grid_source().is_some_and(|s| s.ends_with("lw.grid.gz")));
        let o = t.oracle();
        let water = LonLat::new(-122.4985, 37.8005);
        let land = LonLat::new(-122.50, 37.801);
        let out = LonLat::new(-122.40, 37.80);
        let edge = LonLat::new(-122.4975, 37.8015); // next to the sample without data
        let got = o.sample(Time(0), &[water, land, out, edge]).expect("answered");
        match &got[0] {
            Surface::Water { depth_m } => assert!((depth_m - 11.5).abs() < 0.01, "{depth_m}"),
            s => panic!("{s:?}"),
        }
        assert_eq!(got[1], Surface::Land);
        assert_eq!(got[2], Surface::OutsideCoverage);
        assert_eq!(got[3], Surface::OutsideCoverage);
        // an effect point well inside the water: the nearest sample, and where it was
        let n = o.sample_near(Time(0), LonLat::new(-122.498, 37.801), 50.0).expect("near");
        assert!(matches!(n.surface, Surface::Water { .. }));
        assert!(n.distance_m < 1.0, "{}", n.distance_m);
        // on the shoreline: land and water both within reach
        let e = o.sample_near(Time(0), LonLat::new(-122.4995, 37.801), 80.0).expect_err("shoreline");
        assert!(e.contains("shoreline"), "{e}");
        // with tidewater the grid is not read; no grid and no tidewater: no source
        assert!(Terrain::with_grid(Some("http://127.0.0.1:9"), Some(&path)).grid_source().is_none());
        let none = Terrain::with_grid(None, None);
        assert!(!none.configured());
        assert!(none.oracle().sample(Time(0), &[water]).is_err());
        // a grid that can't be read is no source, and says why
        let bad = Terrain::with_grid(None, Some(&dir.join("missing.gz")));
        assert!(!bad.configured());
        let e = bad.oracle().sample(Time(0), &[water]).expect_err("no source");
        assert!(e.contains("land/water grid"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chunked_and_plain_responses() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain), Ok((200, b"{}".to_vec())));
        let chunked = b"HTTP/1.1 400 Bad\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Ok((400, b"{}".to_vec())));
    }
}
