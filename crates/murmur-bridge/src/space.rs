//! The space segment over HTTP (`/api/space/*`): murmur-space loaded once at startup and
//! queried off the async runtime, for the console's timeline, theater map and globe inset.
//!
//! Times in and out are scenario time: the scenario clock reads `MURMUR_AT` when the bridge
//! starts and runs at `MURMUR_RATE` (the stand-ins' convention); without `MURMUR_AT` it is
//! the bridge's clock (`clock::wall`: the wall clock at the start plus monotonic time, as the
//! engine reads). Every response carries the `time_map` and `time_shifted`: element sets are
//! current while scenarios are dated, so orbits are propagated a whole number of days later
//! (see crates/murmur-space/README.md, "Epoch honesty"). The shift is pinned for the session
//! so that every view describes the same propagation day.
//!
//! Read-only: tasking queries (`POST /api/space/task`) return options without creating an
//! intent; tasking a collection (`POST /api/intents`, task `collect`) belongs to the
//! decision pipeline, which gets the same loaded segment and scenario clock
//! ([`OnLoaded`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration as StdDuration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use murmur_decide::{ScenarioMap, Time as DTime};
use murmur_space::access;
use murmur_space::area::{Area, NamedArea, load_areas};
use murmur_space::catalog::Capability;
use murmur_space::geo::{EARTH_MEAN_RADIUS_KM, Geodetic, Observer, sun_elevation_deg};
use murmur_space::space::{
    AccessQuery, GnssQuery, LinkQuery, Space, ring, scene_polygon, sensor_constraint,
};
use murmur_space::tasking::{NoCloudModel, Rank, TaskRequest};
use murmur_space::timemap::{EpochMode, TimeMap};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::world::World;

/// Longest window a query may ask for (link windows over Starlink cost ~2 s a day).
const MAX_WINDOW: Duration = Duration::days(3);
/// The console's default look-ahead.
const DEFAULT_WINDOW: Duration = Duration::hours(6);
/// Default cap per class for globe positions (Starlink has ~10,000 objects).
const DEFAULT_POSITIONS_PER_CLASS: usize = 300;
const CACHE_ENTRIES: usize = 96;

const FETCH_HINT: &str = "fetch the element sets once with `cargo run -p murmur-space --release -- fetch` \
     (writes sim/space/cache/, polite to CelesTrak), then restart the bridge";

// ------------------------------------------------------------------ scenario clock

/// `MURMUR_AT` + elapsed x `MURMUR_RATE`, else the bridge's clock. Elapsed is the bridge's
/// clock since it started, the clock the decision engine runs on, so the scenario clock is
/// exactly the [`ScenarioMap`] the engine's space segment gets ([`ScenarioClock::map`]): a
/// rate other than 1 is a linear map, not a fixed offset.
#[derive(Debug, Clone)]
pub struct ScenarioClock {
    pub at: Option<DateTime<Utc>>,
    pub rate: f64,
    /// The bridge's clock (ms) when the clock started: the engine's clock, which the map
    /// starts from (`crate::clock::wall`, never stepping).
    started_ms: i64,
}

/// The bridge's clock in ms (the engine's: [`crate::clock::BridgeClock`]).
fn bridge_ms() -> i64 {
    (crate::clock::wall() * 1000.0).round() as i64
}

impl ScenarioClock {
    pub fn new(at: Option<DateTime<Utc>>, rate: f64) -> ScenarioClock {
        ScenarioClock {
            at,
            rate: if rate.is_finite() && rate > 0.0 {
                rate
            } else {
                1.0
            },
            started_ms: bridge_ms(),
        }
    }

    /// Scenario time as a map from the bridge's clock (identity without `MURMUR_AT`).
    pub fn map(&self) -> ScenarioMap {
        match self.at {
            Some(at) => ScenarioMap {
                engine_origin: DTime(self.started_ms),
                scenario_origin: DTime(at.timestamp_millis()),
                rate: self.rate,
            },
            None => ScenarioMap::IDENTITY,
        }
    }

    pub fn from_env() -> ScenarioClock {
        let at = std::env::var("MURMUR_AT")
            .ok()
            .and_then(|s| match parse_time(&s) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("space: ignoring MURMUR_AT: {e}");
                    None
                }
            });
        let rate = std::env::var("MURMUR_RATE")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1.0);
        ScenarioClock::new(at, rate)
    }

    /// Scenario time now, on the bridge's clock (as the engine reads it: the space API and
    /// the decisions agree on when a pass is).
    pub fn now(&self) -> DateTime<Utc> {
        let now = bridge_ms();
        match self.at {
            Some(at) => {
                let s = self.map().scenario(DTime(now));
                DateTime::from_timestamp_millis(s.0).unwrap_or(at)
            }
            None => DateTime::from_timestamp_millis(now).unwrap_or_else(Utc::now),
        }
    }

    fn json(&self) -> Value {
        json!({
            "now": iso(self.now()),
            "rate": self.rate,
            "source": if self.at.is_some() { "MURMUR_AT" } else { "wall clock" },
            "at": self.at.map(iso),
        })
    }
}

/// RFC 3339 and the shorter forms people type: `2026-06-14T10:00Z`, `2026-06-14T10:00`
/// (UTC), `2026-06-14`.
pub fn parse_time(s: &str) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    let z = s.replacen('z', "Z", 1);
    let z = z
        .strip_suffix('Z')
        .map_or(z.clone(), |b| format!("{b}+00:00"));
    for f in [
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%d %H:%M%:z",
        "%Y-%m-%d %H:%M:%S%:z",
    ] {
        if let Ok(t) = DateTime::parse_from_str(&z, f) {
            return Ok(t.with_timezone(&Utc));
        }
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(t.and_utc());
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        && let Some(t) = d.and_hms_opt(0, 0, 0)
    {
        return Ok(t.and_utc());
    }
    Err(format!(
        "bad time {s:?} (want RFC 3339, e.g. 2026-06-14T10:00:00Z)"
    ))
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

// ------------------------------------------------------------------ state

pub struct SpaceApi {
    world: Arc<World>,
    root: PathBuf,
    pub clock: ScenarioClock,
    space: OnceLock<Result<Arc<Space>, String>>,
    areas: Vec<NamedArea>,
    /// SATCOM classes per platform id, from `space_class` / `space_class_alt` in
    /// sim/platforms.toml (read once at start).
    satcom: HashMap<String, Vec<String>>,
    cache: Mutex<HashMap<String, (Instant, Arc<str>)>>,
}

type Api = Arc<SpaceApi>;

/// Called once the segment has loaded, with the scenario clock as a map from the wall clock
/// (the decision layer gives both to its engine, for collect and cover).
pub type OnLoaded = Box<dyn FnOnce(Arc<Space>, ScenarioMap) + Send>;

/// The routes, with their own state, mergeable into any router. Loading starts on a
/// background thread; until it finishes the endpoints answer 503.
pub fn router<S: Clone + Send + Sync + 'static>(
    world: Arc<World>,
    on_loaded: Option<OnLoaded>,
) -> Router<S> {
    let root = repo_root();
    let api = Arc::new(SpaceApi::new(world, root, ScenarioClock::from_env()));
    let loader = api.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let result = loader.load();
        match &result {
            Ok(s) => println!(
                "space: {} satellites in {} classes loaded in {} ms; time shift {:?} d",
                s.classes.iter().map(|c| c.sats.len()).sum::<usize>(),
                s.classes.len(),
                t0.elapsed().as_millis(),
                s.pinned_shift_days,
            ),
            Err(e) => eprintln!("space: {e}"),
        }
        if let (Ok(s), Some(f)) = (&result, on_loaded) {
            f(s.clone(), loader.clock.map());
        }
        let _ = loader.space.set(result);
    });
    routes(api)
}

fn routes<S: Clone + Send + Sync + 'static>(api: Api) -> Router<S> {
    Router::new()
        .route("/api/space/catalog", get(catalog))
        .route("/api/space/passes", get(passes))
        .route("/api/space/task", post(task))
        .route("/api/space/links", get(links))
        .route("/api/space/links/now", get(links_now))
        .route("/api/space/gnss", get(gnss))
        .route("/api/space/positions", get(positions))
        .route("/api/space/overhead", get(overhead))
        .route("/api/space/track", get(track))
        .with_state(api)
}

fn repo_root() -> PathBuf {
    std::env::var_os("MURMUR_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

impl SpaceApi {
    pub fn new(world: Arc<World>, root: PathBuf, clock: ScenarioClock) -> SpaceApi {
        let areas = load_areas(&root.join("sim/space/areas.toml")).unwrap_or_else(|e| {
            eprintln!("space: no named areas: {e}");
            Vec::new()
        });
        let satcom = load_satcom(&root.join("sim/platforms.toml")).unwrap_or_else(|e| {
            eprintln!("space: no SATCOM classes: {e}");
            HashMap::new()
        });
        SpaceApi {
            world,
            root,
            clock,
            space: OnceLock::new(),
            areas,
            satcom,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn load(&self) -> Result<Arc<Space>, String> {
        let dir = self.root.join("sim/space/cache");
        let empty = std::fs::read_dir(&dir).map_or(true, |mut d| d.next().is_none());
        if empty {
            return Err(format!(
                "space segment unavailable: no element sets in {}; {FETCH_HINT}",
                dir.display()
            ));
        }
        let mut space = Space::load(&self.root.join("sim/space/constellations.toml"), &dir)
            .map_err(|e| format!("space segment unavailable: {e}; {FETCH_HINT}"))?;
        self.pin(&mut space);
        Ok(Arc::new(space))
    }

    /// One time shift for the whole session, chosen for the scenario's first day.
    fn pin(&self, space: &mut Space) {
        let now = self.clock.now();
        space.pin_shift(Some((now, now + Duration::days(1))));
    }

    /// For tests: install an already-built Space.
    #[cfg(test)]
    pub fn install(&self, mut space: Space) {
        self.pin(&mut space);
        let _ = self.space.set(Ok(Arc::new(space)));
    }

    fn space(&self) -> Result<Arc<Space>, ApiError> {
        match self.space.get() {
            Some(Ok(s)) => Ok(s.clone()),
            Some(Err(e)) => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, e.clone())),
            None => Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "space segment still loading; retry in a moment".into(),
            )),
        }
    }

    fn cached(&self, key: &str, ttl: StdDuration) -> Option<Arc<str>> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(key)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, body)| body.clone())
    }

    fn store(&self, key: String, body: Arc<str>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= CACHE_ENTRIES
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(key, (Instant::now(), body));
    }

    /// The session's time map (what positions, tracks and instantaneous links use).
    fn session_map(&self, space: &Space) -> TimeMap {
        let now = self.clock.now();
        space.time_map(EpochMode::Auto, now, now + Duration::days(1))
    }

    /// Fields every response carries.
    fn envelope(&self, tm: &TimeMap, mut body: Map<String, Value>) -> Value {
        body.insert("scenario".into(), self.clock.json());
        body.insert("time_map".into(), json!(tm));
        body.insert("time_shifted".into(), json!(tm.shift_days != 0));
        body.insert("time_note".into(), json!(time_note(tm)));
        Value::Object(body)
    }

    // -------------------------------------------------------------- areas and vehicles

    /// The scenario area: `MURMUR_SPACE_AREA` (a name in sim/space/areas.toml), else the
    /// smallest named area around the fleet, else a 20 km box on the fleet, else the first
    /// named area.
    fn default_area(&self) -> AreaInfo {
        if let Ok(name) = std::env::var("MURMUR_SPACE_AREA")
            && let Some(a) = self.areas.iter().find(|a| a.name == name)
        {
            return AreaInfo::named(a);
        }
        let fleet: Vec<[f64; 2]> = self.fleet().iter().map(|v| [v.lon, v.lat]).collect();
        if let Some(c) = centroid(&fleet) {
            // the smallest area holding a good share of the fleet (HALE aircraft on station
            // offshore pull the centroid out of a harbour box)
            let share = |a: &NamedArea| {
                fleet.iter().filter(|p| contains(&a.area, **p)).count() as f64 / fleet.len() as f64
            };
            let around = self
                .areas
                .iter()
                .filter(|a| matches!(a.area, Area::Polygon { .. }))
                .filter(|a| contains(&a.area, c) || share(a) >= 0.4)
                .min_by(|a, b| a.area.radius_km().total_cmp(&b.area.radius_km()));
            if let Some(a) = around {
                return AreaInfo::named(a);
            }
            let (lon, lat) = (c[0], c[1]);
            let (dlat, dlon) = (0.09, 0.09 / lat.to_radians().cos().max(0.2));
            return AreaInfo {
                name: "fleet".into(),
                title: "20 km box on the fleet".into(),
                area: Area::Polygon {
                    ring: vec![
                        [lon - dlon, lat - dlat],
                        [lon + dlon, lat - dlat],
                        [lon + dlon, lat + dlat],
                        [lon - dlon, lat + dlat],
                    ],
                },
                utc_offset_h: (lon / 15.0).round(),
            };
        }
        self.areas.first().map_or_else(
            || AreaInfo {
                name: "origin".into(),
                title: "no area configured".into(),
                area: Area::point(0.0, 0.0, 10.0),
                utc_offset_h: 0.0,
            },
            AreaInfo::named,
        )
    }

    /// `area=` as a named area, `w,s,e,n` (optionally `bbox:` first), or empty for the
    /// scenario area.
    fn area_param(&self, s: Option<&str>) -> Result<AreaInfo, ApiError> {
        let s = s.map(str::trim).unwrap_or("");
        if s.is_empty() || s == "scenario" {
            return Ok(self.default_area());
        }
        if let Some(a) = self.areas.iter().find(|a| a.name == s) {
            return Ok(AreaInfo::named(a));
        }
        let nums: Vec<f64> = s
            .trim_start_matches("bbox:")
            .split(',')
            .map(|x| x.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| bad(format!("unknown area {s:?} (a name, or w,s,e,n)")))?;
        let [w, so, e, n] = nums[..] else {
            return Err(bad("a box is w,s,e,n"));
        };
        bbox_area(w, so, e, n, self.default_area().utc_offset_h)
    }

    /// An area from a JSON body: a string (as `area_param`), an Area, or {bbox: [w,s,e,n]}.
    fn area_value(&self, v: Option<&Value>) -> Result<AreaInfo, ApiError> {
        match v {
            None | Some(Value::Null) => Ok(self.default_area()),
            Some(Value::String(s)) => self.area_param(Some(s)),
            Some(Value::Object(o)) if o.contains_key("bbox") => {
                let b: [f64; 4] = serde_json::from_value(o["bbox"].clone())
                    .map_err(|e| bad(format!("bbox: {e}")))?;
                bbox_area(b[0], b[1], b[2], b[3], self.default_area().utc_offset_h)
            }
            Some(other) => {
                let area: Area =
                    serde_json::from_value(other.clone()).map_err(|e| bad(format!("area: {e}")))?;
                Ok(AreaInfo {
                    name: "custom".into(),
                    title: "requested area".into(),
                    area,
                    utc_offset_h: self.default_area().utc_offset_h,
                })
            }
        }
    }

    fn fleet(&self) -> Vec<FleetNode> {
        self.world
            .vehicles()
            .iter()
            .filter_map(|cell| {
                let (pos, alt, identity) = cell.read(|v| (v.pos, v.alt_msl, v.identity.clone()));
                let (lat, lon) = pos?;
                let id = identity.as_deref();
                Some(FleetNode {
                    uid: cell.uid,
                    callsign: id.and_then(|i| i.str("callsign")).map(str::to_string),
                    platform: id.and_then(|i| i.str("platform")).map(str::to_string),
                    link: id.and_then(|i| i.str("link")).map(str::to_string),
                    lon,
                    lat,
                    alt_m: alt.unwrap_or(0.0),
                })
            })
            .collect()
    }

    /// The scenario window from query params: `from` (default now), `to` (default +6 h).
    fn window(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
        let from = match from {
            Some(s) if !s.is_empty() => parse_time(s).map_err(bad)?,
            _ => floor_minute(self.clock.now()),
        };
        let to = match to {
            Some(s) if !s.is_empty() => parse_time(s).map_err(bad)?,
            _ => from + DEFAULT_WINDOW,
        };
        if to <= from {
            return Err(bad("window end must be after its start"));
        }
        if to - from > MAX_WINDOW {
            return Err(bad(format!(
                "window longer than {} days",
                MAX_WINDOW.num_days()
            )));
        }
        Ok((from, to))
    }
}

struct FleetNode {
    uid: u64,
    callsign: Option<String>,
    platform: Option<String>,
    link: Option<String>,
    lon: f64,
    lat: f64,
    alt_m: f64,
}

/// The SATCOM classes of every platform that names one: `space_class` first, then the
/// optional `space_class_alt`, from the platform catalog (sim/platforms.toml). The link
/// emulator reads the same field (through the fleet manifest), so the two cannot disagree.
pub fn load_satcom(path: &std::path::Path) -> Result<HashMap<String, Vec<String>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(table
        .iter()
        .filter_map(|(id, spec)| {
            let spec = spec.as_table()?;
            let classes: Vec<String> = ["space_class", "space_class_alt"]
                .iter()
                .filter_map(|k| spec.get(*k)?.as_str().map(String::from))
                .collect();
            (!classes.is_empty()).then(|| (id.clone(), classes))
        })
        .collect())
}

impl SpaceApi {
    /// SATCOM classes of a platform (empty if the catalog names none: no SATCOM views).
    pub fn satcom_classes(&self, platform: Option<&str>) -> Vec<String> {
        platform
            .and_then(|p| self.satcom.get(p))
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone)]
struct AreaInfo {
    name: String,
    title: String,
    area: Area,
    utc_offset_h: f64,
}

impl AreaInfo {
    fn named(a: &NamedArea) -> AreaInfo {
        AreaInfo {
            name: a.name.clone(),
            title: a.title.clone().unwrap_or_else(|| a.name.clone()),
            area: a.area.clone(),
            utc_offset_h: a.utc_offset_h,
        }
    }

    fn json(&self) -> Value {
        let c = self.area.centroid();
        json!({
            "name": self.name,
            "title": self.title,
            "area": self.area,
            "ring": self.area.ring(),
            "center": [c.lon_deg, c.lat_deg],
            "radius_km": self.area.radius_km(),
            "utc_offset_h": self.utc_offset_h,
        })
    }

    fn key(&self) -> String {
        serde_json::to_string(&self.area).unwrap_or_default()
    }
}

fn bbox_area(w: f64, s: f64, e: f64, n: f64, utc_offset_h: f64) -> Result<AreaInfo, ApiError> {
    let ok = (-180.0..=180.0).contains(&w)
        && (-180.0..=180.0).contains(&e)
        && (-90.0..=90.0).contains(&s)
        && (-90.0..=90.0).contains(&n)
        && w < e
        && s < n;
    if !ok {
        return Err(bad("box must be w,s,e,n in degrees with w < e and s < n"));
    }
    let area = Area::Polygon {
        ring: vec![[w, s], [e, s], [e, n], [w, n]],
    };
    if area.radius_km() > 500.0 {
        return Err(bad(
            "area too large (over 500 km from its centre to a corner)",
        ));
    }
    Ok(AreaInfo {
        name: "box".into(),
        title: format!("box {w:.3},{s:.3} to {e:.3},{n:.3}"),
        area,
        utc_offset_h,
    })
}

fn contains(area: &Area, p: [f64; 2]) -> bool {
    match area {
        Area::Circle { .. } => {
            let c = area.centroid();
            c.distance_km(&Geodetic::new(p[1], p[0], 0.0)) <= area.radius_km()
        }
        Area::Polygon { ring } => {
            // ray casting; areas are small and never cross the antimeridian here
            let mut inside = false;
            let mut j = ring.len().wrapping_sub(1);
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[j]);
                if (a[1] > p[1]) != (b[1] > p[1])
                    && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
                {
                    inside = !inside;
                }
                j = i;
            }
            inside
        }
    }
}

fn centroid(points: &[[f64; 2]]) -> Option<[f64; 2]> {
    if points.is_empty() {
        return None;
    }
    let n = points.len() as f64;
    Some([
        points.iter().map(|p| p[0]).sum::<f64>() / n,
        points.iter().map(|p| p[1]).sum::<f64>() / n,
    ])
}

fn floor_minute(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(t.timestamp() - t.timestamp().rem_euclid(60), 0).unwrap_or(t)
}

fn time_note(tm: &TimeMap) -> String {
    if tm.shift_days == 0 {
        return "orbits propagated at scenario time".into();
    }
    format!(
        "TIME-SHIFTED {:+} d: orbits propagated {} days later at the same time of day (elements \
         reference {}); sun at scenario time. Pass counts and geometry are representative; \
         specific pass times are not those of the scenario date.",
        tm.shift_days,
        tm.shift_days,
        tm.reference.format("%Y-%m-%d %H:%MZ"),
    )
}

fn classes_param(s: Option<&str>) -> Vec<String> {
    s.unwrap_or("")
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

fn flag(s: Option<&str>, default: bool) -> bool {
    match s.map(str::trim) {
        Some("1" | "true" | "yes") => true,
        Some("0" | "false" | "no") => false,
        _ => default,
    }
}

// ------------------------------------------------------------------ errors and plumbing

pub struct ApiError(StatusCode, String);

fn bad(e: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, e.into())
}

impl From<murmur_space::SpaceError> for ApiError {
    fn from(e: murmur_space::SpaceError) -> Self {
        ApiError(StatusCode::BAD_REQUEST, e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "detail": self.1 }))).into_response()
    }
}

fn raw_json(body: Arc<str>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Serve from the cache, or compute on a blocking thread (queries use rayon) and cache.
async fn serve<F>(api: Api, key: String, ttl: StdDuration, f: F) -> Response
where
    F: FnOnce(&SpaceApi, &Space) -> Result<Value, ApiError> + Send + 'static,
{
    let space = match api.space() {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if let Some(hit) = api.cached(&key, ttl) {
        return raw_json(hit);
    }
    let worker = api.clone();
    let result = tokio::task::spawn_blocking(move || {
        f(&worker, &space).map(|v| Arc::<str>::from(v.to_string()))
    })
    .await;
    match result {
        Ok(Ok(body)) => {
            if !ttl.is_zero() {
                api.store(key, body.clone());
            }
            raw_json(body)
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn to_map(v: impl serde::Serialize) -> Result<Map<String, Value>, ApiError> {
    match serde_json::to_value(v) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Ok(Map::new()),
        Err(e) => Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

type Params = Query<HashMap<String, String>>;

fn p<'a>(q: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    q.get(k).map(String::as_str)
}

// ------------------------------------------------------------------ handlers

/// Classes, their figures and members, cache freshness, named areas, the scenario area and
/// clock, and the SATCOM class per fleet platform.
async fn catalog(State(api): State<Api>) -> Response {
    serve(api, String::new(), StdDuration::ZERO, |api, space| {
        let tm = api.session_map(space);
        let classes: Vec<Value> = space
            .classes
            .iter()
            .map(|cs| {
                let c = &cs.class;
                let members: Vec<Value> = if cs.sats.len() <= 400 {
                    cs.sats
                        .iter()
                        .map(|s| {
                            json!({
                                "norad_id": s.norad_id,
                                "name": s.name,
                                "label": c.effective_sensor(&s.name).and_then(|e| e.label),
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                json!({
                    "id": c.id, "name": c.name, "capability": c.capability, "role": c.role,
                    "orbit": c.orbit, "satellites": cs.sats.len(), "sensor": c.sensor,
                    "comms": c.comms, "timing": c.timing, "guessed": c.guessed,
                    "sources": c.sources, "notes": c.notes, "members": members,
                })
            })
            .collect();
        let satcom: Map<String, Value> = api
            .fleet()
            .iter()
            .filter(|v| v.link.as_deref() == Some("satcom"))
            .map(|v| {
                (
                    v.uid.to_string(),
                    json!(api.satcom_classes(v.platform.as_deref())),
                )
            })
            .collect();
        let mut body = Map::new();
        body.insert("classes".into(), json!(classes));
        body.insert("reference".into(), json!(iso(space.reference)));
        body.insert("cache".into(), json!(space.cache_meta));
        body.insert(
            "areas".into(),
            json!(
                api.areas
                    .iter()
                    .map(|a| AreaInfo::named(a).json())
                    .collect::<Vec<_>>()
            ),
        );
        body.insert("area".into(), api.default_area().json());
        body.insert("satcom_by_uid".into(), Value::Object(satcom));
        Ok(api.envelope(&tm, body))
    })
    .await
}

/// Imaging access windows over an area, with each window's scene (swath square at the
/// target, along the ground track) and sub-satellite point at its best time, plus sun
/// elevation at the area every 10 minutes for day/night shading. Optical windows in darkness
/// are kept (`daylight_ok: false`) unless `include_night=0`.
async fn passes(State(api): State<Api>, Query(q): Params) -> Response {
    let prep = (|| {
        let area = api.area_param(p(&q, "area"))?;
        let (from, to) = api.window(p(&q, "from"), p(&q, "to"))?;
        Ok::<_, ApiError>((area, from, to))
    })();
    let (area, from, to) = match prep {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let classes = classes_param(p(&q, "classes"));
    let night = flag(p(&q, "include_night"), true);
    let key = format!("passes|{}|{from}|{to}|{classes:?}|{night}", area.key());
    serve(api, key, StdDuration::from_secs(120), move |api, space| {
        let report = space.imaging_access(&AccessQuery {
            area: area.area.clone(),
            from,
            to,
            classes,
            epoch_mode: EpochMode::Auto,
            include_night: night,
        })?;
        let tm = report.time_map;
        let target = Observer::new(area.area.centroid());
        let windows: Vec<Value> = report
            .windows
            .iter()
            .map(|w| {
                let mut v = json!(w);
                if let Some((scene, sub)) = scene_at(space, w.norad_id, w.best_time, &tm, &target) {
                    v["scene"] = json!(scene);
                    v["subpoint"] = json!(sub);
                }
                v
            })
            .collect();
        let mut sun = Vec::new();
        let mut t = from;
        while t <= to {
            sun.push(json!([
                t.timestamp_millis(),
                (sun_elevation_deg(&target, &t) * 10.0).round() / 10.0
            ]));
            t += Duration::minutes(10);
        }
        let mut body = to_map(&report)?;
        body.insert("windows".into(), json!(windows));
        body.insert("sun".into(), json!(sun));
        body.insert("area".into(), area.json());
        Ok(api.envelope(&tm, body))
    })
    .await
}

/// A [lon, lat] ring.
type Ring = Vec<[f64; 2]>;

/// Scene square (class swath, aligned with the ground track) centred on the target, and the
/// sub-satellite point, at `t` (scenario time).
fn scene_at(
    space: &Space,
    norad: u64,
    t: DateTime<Utc>,
    tm: &TimeMap,
    target: &Observer,
) -> Option<(Option<Ring>, [f64; 3])> {
    let (class, sat) = space.satellite(norad)?;
    let tp = tm.to_prop(t);
    let here = sat.state(&tp).ok()?.geodetic();
    let ahead = sat.state(&(tp + Duration::seconds(10))).ok()?.geodetic();
    let heading = here.bearing_to(&ahead);
    let scene = class
        .effective_sensor(&sat.name)
        .and_then(|s| s.swath_km)
        .map(|swath| scene_polygon(&target.geo, swath, heading));
    Some((scene, [here.lon_deg, here.lat_deg, here.alt_km]))
}

#[derive(Deserialize)]
struct TaskBody {
    #[serde(default)]
    capabilities: Vec<Capability>,
    area: Option<Value>,
    from: Option<String>,
    to: Option<String>,
    deadline: Option<String>,
    max_resolution_m: Option<f64>,
    #[serde(default)]
    rank: Rank,
    #[serde(default)]
    classes: Vec<String>,
    include_untaskable: Option<bool>,
    /// Exclusions listed in full (the summary always covers every one).
    exclusions: Option<usize>,
}

/// A tasking query: ranked options and the reasons every other pass was excluded. Read-only;
/// nothing is submitted (the decision pipeline approves collections).
async fn task(
    State(api): State<Api>,
    body: Result<Json<TaskBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let b = match body {
        Ok(Json(b)) => b,
        Err(e) => return ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()).into_response(),
    };
    let prep = (|| {
        let area = api.area_value(b.area.as_ref())?;
        let deadline = b
            .deadline
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(parse_time)
            .transpose()
            .map_err(bad)?;
        let to = b.to.clone().or_else(|| deadline.map(iso));
        let (from, to) = api.window(b.from.as_deref(), to.as_deref())?;
        Ok::<_, ApiError>((area, deadline, from, to))
    })();
    let (area, deadline, from, to) = match prep {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let caps = if b.capabilities.is_empty() {
        vec![Capability::Optical, Capability::Sar]
    } else {
        b.capabilities.clone()
    };
    if let Some(c) = caps.iter().find(|c| !c.is_imaging()) {
        return bad(format!("{c} is not a collection capability (optical, sar)")).into_response();
    }
    let req = TaskRequest {
        capabilities: caps,
        area: area.area.clone(),
        from,
        to,
        now: api.clock.now(),
        product_deadline: deadline,
        max_resolution_m: b.max_resolution_m,
        rank: b.rank,
        classes: b.classes,
        include_untaskable: b.include_untaskable.unwrap_or(true),
        epoch_mode: EpochMode::Auto,
    };
    let limit = b.exclusions.unwrap_or(200);
    serve(api, String::new(), StdDuration::ZERO, move |api, space| {
        let mut result = space.tasking_options(&req, &NoCloudModel)?;
        let total = result.exclusions.len();
        result.exclusions.truncate(limit);
        let tm = result.time_map;
        let mut body = to_map(&result)?;
        body.insert("exclusions_total".into(), json!(total));
        body.insert("area".into(), area.json());
        body.insert(
            "note".into(),
            json!("options only: nothing has been requested; approval arrives with the decision pipeline"),
        );
        Ok(api.envelope(&tm, body))
    })
    .await
}

/// SATCOM link windows, coverage, outages and latency for a fleet vehicle's current position
/// (`uid=`), a point (`lat=&lon=[&alt_m=]`) or the scenario area's centre. `windows=0` leaves
/// out the window list (Starlink has thousands a day); otherwise each class lists at most
/// `max_windows` (default 200).
async fn links(State(api): State<Api>, Query(q): Params) -> Response {
    let prep = (|| {
        let (point, classes, who) = api.point_param(&q)?;
        let (from, to) = api.window(p(&q, "from"), p(&q, "to"))?;
        Ok::<_, ApiError>((point, classes, who, from, to))
    })();
    let (point, mut classes, who, from, to) = match prep {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let asked = classes_param(p(&q, "classes"));
    if !asked.is_empty() {
        classes = asked;
    }
    let with_windows = flag(p(&q, "windows"), true);
    let max_windows: usize = p(&q, "max_windows")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let step: f64 = p(&q, "step")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(60.0)
        .max(10.0);
    let key = format!(
        "links|{:.3},{:.3},{:.1}|{classes:?}|{from}|{to}|{with_windows}|{max_windows}|{step}",
        point.lat_deg, point.lon_deg, point.alt_km
    );
    serve(api, key, StdDuration::from_secs(60), move |api, space| {
        let mut report = space.links(&LinkQuery {
            point,
            from,
            to,
            classes,
            epoch_mode: EpochMode::Auto,
            sample_step_s: step,
            include_samples: false,
        })?;
        let mut totals = Map::new();
        for c in &mut report.classes {
            totals.insert(c.class_id.clone(), json!(c.windows.len()));
            if with_windows {
                c.windows.truncate(max_windows);
            } else {
                c.windows.clear();
            }
        }
        let tm = report.time_map;
        let mut body = to_map(&report)?;
        body.insert("windows_total".into(), Value::Object(totals));
        body.insert("for".into(), who);
        Ok(api.envelope(&tm, body))
    })
    .await
}

impl SpaceApi {
    /// `uid=` (a fleet vehicle's current position and its SATCOM classes), `lat=&lon=`, or
    /// the scenario area's centre.
    fn point_param(
        &self,
        q: &HashMap<String, String>,
    ) -> Result<(Geodetic, Vec<String>, Value), ApiError> {
        if let Some(uid) = p(q, "uid") {
            let uid: u64 = uid
                .trim()
                .parse()
                .map_err(|_| bad("uid must be a number"))?;
            let v = self
                .fleet()
                .into_iter()
                .find(|v| v.uid == uid)
                .ok_or_else(|| {
                    ApiError(
                        StatusCode::NOT_FOUND,
                        format!("no vehicle {uid} with a position"),
                    )
                })?;
            let classes = self.satcom_classes(v.platform.as_deref());
            let who = json!({"uid": v.uid, "callsign": v.callsign, "platform": v.platform, "link": v.link});
            return Ok((
                Geodetic::new(v.lat, v.lon, v.alt_m.max(0.0) / 1000.0),
                classes,
                who,
            ));
        }
        if let (Some(lat), Some(lon)) = (p(q, "lat"), p(q, "lon")) {
            let lat: f64 = lat.parse().map_err(|_| bad("bad lat"))?;
            let lon: f64 = lon.parse().map_err(|_| bad("bad lon"))?;
            let alt: f64 = p(q, "alt_m").and_then(|s| s.parse().ok()).unwrap_or(0.0);
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err(bad("lat/lon out of range"));
            }
            return Ok((
                Geodetic::new(lat, lon, alt / 1000.0),
                Vec::new(),
                json!({"lat": lat, "lon": lon}),
            ));
        }
        let area = self.default_area();
        Ok((area.area.centroid(), Vec::new(), json!({"area": area.name})))
    }
}

/// Every fleet vehicle with a SATCOM link: the best satellite of each of its classes right
/// now (elevation, azimuth, range, latency, and where the satellite is), for link lines.
async fn links_now(State(api): State<Api>) -> Response {
    serve(
        api,
        "links-now".into(),
        StdDuration::from_secs(4),
        |api, space| {
            let tm = api.session_map(space);
            let now = api.clock.now();
            let tp = tm.to_prop(now);
            let mut out = Vec::new();
            for v in api
                .fleet()
                .iter()
                .filter(|v| v.link.as_deref() == Some("satcom"))
            {
                let point = Geodetic::new(v.lat, v.lon, v.alt_m.max(0.0) / 1000.0);
                for class in api.satcom_classes(v.platform.as_deref()) {
                    let Ok(s) = space.link_at(&class, point, now, &tm) else {
                        continue;
                    };
                    let best = s.best.as_ref().map(|b| {
                        let pos = space
                            .satellite(b.norad_id)
                            .and_then(|(_, sat)| sat.state(&tp).ok())
                            .map(|st| {
                                let g = st.geodetic();
                                [g.lon_deg, g.lat_deg, g.alt_km]
                            });
                        json!({
                            "norad_id": b.norad_id, "satellite": b.satellite,
                            "elevation_deg": b.elevation_deg, "azimuth_deg": b.azimuth_deg,
                            "range_km": b.range_km, "latency_ms": b.latency_ms, "position": pos,
                        })
                    });
                    out.push(json!({
                        "uid": v.uid, "callsign": v.callsign, "platform": v.platform,
                        "position": [v.lon, v.lat, v.alt_m], "class_id": class,
                        "visible": s.visible, "best": best,
                    }));
                }
            }
            let mut body = Map::new();
            body.insert("t".into(), json!(iso(now)));
            body.insert("links".into(), json!(out));
            Ok(api.envelope(&tm, body))
        },
    )
    .await
}

/// GNSS visibility and DOP at a vehicle, a point or the scenario area's centre.
async fn gnss(State(api): State<Api>, Query(q): Params) -> Response {
    let prep = (|| {
        let (point, _, who) = api.point_param(&q)?;
        let (from, to) = api.window(p(&q, "from"), p(&q, "to"))?;
        Ok::<_, ApiError>((point, who, from, to))
    })();
    let (point, who, from, to) = match prep {
        Ok(x) => x,
        Err(e) => return e.into_response(),
    };
    let step: f64 = p(&q, "step")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(300.0)
        .max(30.0);
    let class = p(&q, "class").unwrap_or("gps").to_string();
    let key = format!(
        "gnss|{:.3},{:.3}|{from}|{to}|{step}|{class}",
        point.lat_deg, point.lon_deg
    );
    serve(api, key, StdDuration::from_secs(120), move |api, space| {
        let report = space.gnss(&GnssQuery {
            point,
            from,
            to,
            step_s: step,
            class,
            mask_deg: None,
            epoch_mode: EpochMode::Auto,
        })?;
        let tm = report.time_map;
        let mut body = to_map(&report)?;
        body.insert("for".into(), who);
        Ok(api.envelope(&tm, body))
    })
    .await
}

/// Satellite positions at `t` (default now) for the globe, compactly: per class
/// `sats: [[lon, lat, alt_km, norad_id], ...]`, evenly subsampled to `limit` per class
/// (default 300; Starlink has ~10,000). `classes` defaults to every class but the reference.
async fn positions(State(api): State<Api>, Query(q): Params) -> Response {
    let t = match p(&q, "t")
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()
    {
        Ok(t) => t,
        Err(e) => return bad(e).into_response(),
    };
    let classes = classes_param(p(&q, "classes"));
    let limit: usize = p(&q, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_POSITIONS_PER_CLASS)
        .clamp(1, 20_000);
    // positions move ~7 km/s: a 2 s cache is invisible on a globe
    let t_key = t.map_or_else(
        || (api.clock.now().timestamp_millis() / 2000).to_string(),
        iso,
    );
    let key = format!("positions|{t_key}|{classes:?}|{limit}");
    serve(api, key, StdDuration::from_secs(2), move |api, space| {
        let tm = api.session_map(space);
        let t = t.unwrap_or_else(|| api.clock.now());
        let wanted: Vec<&str> = if classes.is_empty() {
            space
                .classes
                .iter()
                .filter(|c| c.class.capability != Capability::Reference)
                .map(|c| c.class.id.as_str())
                .collect()
        } else {
            classes.iter().map(String::as_str).collect()
        };
        let mut out = Vec::new();
        for id in wanted {
            let mut all = space.positions(id, t, &tm, false)?;
            all.sort_by_key(|s| s.norad_id);
            let total = all.len();
            let stride = total.div_ceil(limit).max(1);
            let sats: Vec<Value> = all
                .iter()
                .step_by(stride)
                .map(|s| {
                    let g = &s.position;
                    json!([
                        (g.lon_deg * 1e4).round() / 1e4,
                        (g.lat_deg * 1e4).round() / 1e4,
                        g.alt_km.round(),
                        s.norad_id
                    ])
                })
                .collect();
            let cs = space.class(id)?;
            out.push(json!({
                "class_id": id, "capability": cs.class.capability, "total": total,
                "shown": sats.len(), "sats": sats,
            }));
        }
        let mut body = Map::new();
        body.insert("t".into(), json!(iso(t)));
        body.insert("classes".into(), json!(out));
        Ok(api.envelope(&tm, body))
    })
    .await
}

/// Imaging satellites that can collect over the area right now (or at `t`): field of regard,
/// the scene they would take, and their geometry. What the theater map draws as "passes in
/// progress".
async fn overhead(State(api): State<Api>, Query(q): Params) -> Response {
    let area = match api.area_param(p(&q, "area")) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let t = match p(&q, "t")
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()
    {
        Ok(t) => t,
        Err(e) => return bad(e).into_response(),
    };
    let t_key = t.map_or_else(
        || (api.clock.now().timestamp_millis() / 2000).to_string(),
        iso,
    );
    let key = format!("overhead|{}|{t_key}", area.key());
    serve(api, key, StdDuration::from_secs(2), move |api, space| {
        let tm = api.session_map(space);
        let t = t.unwrap_or_else(|| api.clock.now());
        let tp = tm.to_prop(t);
        let target = Observer::new(area.area.centroid());
        let sun = sun_elevation_deg(&target, &t);
        let mut out = Vec::new();
        for cs in space
            .classes
            .iter()
            .filter(|c| c.class.capability.is_imaging())
        {
            let (sats, _) = space.usable(cs, &tm, t, t);
            for sat in sats {
                let Some(sensor) = cs.class.effective_sensor(&sat.name) else {
                    continue;
                };
                let Some(limit) = sensor_constraint(cs.class.capability, &sensor) else {
                    continue;
                };
                let Some(g) = access::geometry(sat, &target, &tp) else {
                    continue;
                };
                if !limit.holds(&g) {
                    continue;
                }
                let Ok(st) = sat.state(&tp) else { continue };
                let here = st.geodetic();
                let r = st.ecef.norm();
                let psi = match limit {
                    access::Constraint::MaxOffNadir(m) => {
                        murmur_space::geo::central_angle_for_off_nadir(r, m)
                    }
                    access::Constraint::Incidence { max, .. } => {
                        murmur_space::geo::central_angle_for_elevation(r, 90.0 - max)
                    }
                    access::Constraint::MinElevation(e) => {
                        murmur_space::geo::central_angle_for_elevation(r, e)
                    }
                };
                let field = ring(
                    &Geodetic::new(here.lat_deg, here.lon_deg, 0.0),
                    psi * EARTH_MEAN_RADIUS_KM,
                    72,
                );
                let scene = scene_at(space, sat.norad_id, t, &tm, &target).and_then(|(s, _)| s);
                let daylight_ok = (cs.class.capability == Capability::Optical)
                    .then(|| sun >= sensor.min_sun_elevation_deg.unwrap_or(0.0));
                out.push(json!({
                    "class_id": cs.class.id, "capability": cs.class.capability,
                    "satellite": sat.name, "norad_id": sat.norad_id, "label": sensor.label,
                    "subpoint": [here.lon_deg, here.lat_deg, here.alt_km], "geometry": g,
                    "field_of_regard": field, "scene": scene, "daylight_ok": daylight_ok,
                    "taskable": sensor.taskable,
                }));
            }
        }
        let mut body = Map::new();
        body.insert("t".into(), json!(iso(t)));
        body.insert("sun_elevation_deg".into(), json!(sun));
        body.insert("area".into(), area.json());
        body.insert("overhead".into(), json!(out));
        Ok(api.envelope(&tm, body))
    })
    .await
}

/// A satellite's ground track ([lon, lat] every `step` s, default 30) from `from` to `to`
/// (default 50 minutes either side of now).
async fn track(State(api): State<Api>, Query(q): Params) -> Response {
    let Some(norad) = p(&q, "norad").and_then(|s| s.trim().parse::<u64>().ok()) else {
        return bad("norad= is required").into_response();
    };
    let now = floor_minute(api.clock.now());
    let parse = |k: &str, d: DateTime<Utc>| -> Result<DateTime<Utc>, ApiError> {
        p(&q, k)
            .filter(|s| !s.is_empty())
            .map_or(Ok(d), |s| parse_time(s).map_err(bad))
    };
    let (from, to) = match (
        parse("from", now - Duration::minutes(50)),
        parse("to", now + Duration::minutes(50)),
    ) {
        (Ok(a), Ok(b)) if b > a && b - a <= Duration::days(1) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e.into_response(),
        _ => return bad("track window must be positive and at most a day").into_response(),
    };
    let step: f64 = p(&q, "step")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(30.0)
        .max(5.0);
    let key = format!("track|{norad}|{from}|{to}|{step}");
    serve(api, key, StdDuration::from_secs(60), move |api, space| {
        let tm = api.session_map(space);
        let (class, sat) = space
            .satellite(norad)
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no satellite {norad}")))?;
        let points = space.ground_track(norad, from, to, step, &tm)?;
        let mut body = Map::new();
        body.insert("norad_id".into(), json!(norad));
        body.insert("satellite".into(), json!(sat.name));
        body.insert("class_id".into(), json!(class.id));
        body.insert("from".into(), json!(iso(from)));
        body.insert("to".into(), json!(iso(to)));
        body.insert("step_s".into(), json!(step));
        body.insert("track".into(), json!(points));
        Ok(api.envelope(&tm, body))
    })
    .await
}

#[cfg(test)]
mod tests;
