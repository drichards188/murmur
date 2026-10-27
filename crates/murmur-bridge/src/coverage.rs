//! Predicted link coverage, served (bd murmur-hei; docs/LINK-COVERAGE.md section 3): the link
//! emulator's own predictor (`murmur_linkemu::coverage`) on the links file the emulator reads,
//! with its own terrain cache over the same surface heights (tidewater's DEM, or the world's
//! baked surface grid in the headless simulator) and its own SATCOM source over the same element
//! sets. Everything here is computed outside the decision lock and is information only: the
//! engine never reads it, nothing is logged, no check or rule depends on it.
//!
//! - `GET /api/links/coverage?class=los&alt=100&bbox=w,s,e,n&res=250` a grid for the map (one
//!   letter a cell for the state and the reason, and the loss), or `point=lon,lat` one point;
//!   `uid=` for a vehicle's own profile instead of a class, `above_surface=1` for an altitude
//!   above the terrain or water, `at=` (RFC 3339, scenario time) for SATCOM;
//! - `POST /api/links/coverage` the same as JSON, and a `route` (a line of [lon, lat], from the
//!   vehicle with `uid`), an `order` (a proposed order as the dry run takes it: each vehicle's
//!   transit, task path and return, as the decision engine's route geometry lays them out,
//!   `murmur_decide::route::route_for`), or `queries` (several at once);
//! - the same order answer inside the dry run (`POST /api/intents/preview`) and the agents'
//!   `preview_intent`, as `link_coverage`; and the agents' `get_link_coverage` tool.
//!
//! Terrain, as the emulator: tidewater over HTTP (`MURMUR_TIDEWATER_URL`), else a baked surface
//! grid (`MURMUR_SURFACE_GRID`; the headless simulator gives its world's), else not checked
//! (`terrain` says so). Terrain does not move, so each 250 m cell's answer is cached for good;
//! a grid answer for a class without satellites is cached too. The links file is
//! `MURMUR_LINKS`, else `sim/links.toml`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use murmur_linkemu::config::{Config as LinksConfig, VehicleSpec};
use murmur_linkemu::coverage::{
    DEGRADED_LOSS, DOWN_LOSS, Failsafe, HOLDS_P, LegSamples, ON_STATION_S, Part, Prediction,
    Predictor, ROUTE_STEP_M, State, along, route_answer, sentence,
};
use murmur_linkemu::geo::{GeoPoint, NoTerrain, Terrain};
use murmur_linkemu::sat::{RealSpace, SatObs, SpaceSource};
use murmur_linkemu::terrain::{HttpSampler, SampleFuture, Sampler, TidewaterTerrain, dem_height};
use serde_json::{Value, json};

use crate::decision::terrain::Grid;
use crate::vehicle::Uid;
use crate::world::World;

/// The most cells a grid answer holds; a finer resolution over a larger box is coarsened.
pub const MAX_CELLS: usize = 40_000;
/// The most queries one `queries` call answers.
pub const MAX_QUERIES: usize = 64;
/// The most rounds of terrain fetching one answer waits for (each up to the links file's
/// `[terrain] max_cells_per_tick` cells); what is still unanswered then is said, never guessed.
const WARM_ROUNDS: usize = 200;
/// Along a route the satellites are looked at this often (s of travel) at most: visibility
/// changes over minutes.
const SAT_STEP_S: f64 = 30.0;
/// Grid answers kept.
const GRID_CACHE: usize = 16;

/// What every answer says about itself.
pub const NOTE: &str = "predicted from the link emulator's own geometry (range from the ground \
station, terrain line of sight, SATCOM visibility, the depth gate); information only: no check \
or rule reads it. Scripted events (jammers, cuts) are not predicted, and the emulator's random \
draws decide each packet.";

/// A baked surface grid as a terrain sampler (the emulator's in the headless simulator reads
/// the same grid the same way).
pub struct GridSampler(pub Arc<Grid>);

impl Sampler for GridSampler {
    fn sample<'a>(&'a self, _t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a> {
        let out = points.iter().map(|(lat, lon)| self.0.height(*lat, *lon)).collect();
        Box::pin(async move { Ok(out) })
    }
}

/// Where the predictor's inputs come from.
pub struct Sources {
    /// The links file.
    pub links: PathBuf,
    /// The repository, for the links file's relative `[space]` paths.
    pub root: PathBuf,
    /// Surface heights for the terrain line of sight, and what they are (for answers).
    pub terrain: Option<(Arc<dyn Sampler>, String)>,
    /// A pinned space snapshot's root (`<root>/sim/space/...`), as the headless emulator reads.
    pub space_root: Option<PathBuf>,
    /// SATCOM follows the satellites (else the profiles' static numbers, as `--no-space`).
    pub space: bool,
}

impl Sources {
    /// As `scripts/up.sh` configures the bridge and the emulator.
    pub fn from_env(root: &std::path::Path) -> Sources {
        let links = std::env::var_os("MURMUR_LINKS")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("sim/links.toml"));
        let url = std::env::var("MURMUR_TIDEWATER_URL").ok().filter(|u| !u.trim().is_empty());
        let terrain: Option<(Arc<dyn Sampler>, String)> = match url {
            Some(u) => match HttpSampler::new(u.trim(), 10.0) {
                Ok(s) => Some((Arc::new(s), format!("tidewater at {}", u.trim()))),
                Err(e) => {
                    eprintln!("coverage: {e}; terrain not checked");
                    None
                }
            },
            None => match std::env::var_os("MURMUR_SURFACE_GRID").map(PathBuf::from) {
                Some(p) => match Grid::load(&p) {
                    Ok(g) => Some((Arc::new(GridSampler(Arc::new(g))), format!("surface grid {}", p.display()))),
                    Err(e) => {
                        eprintln!("coverage: {e}; terrain not checked");
                        None
                    }
                },
                None => None,
            },
        };
        Sources {
            links,
            root: root.to_path_buf(),
            terrain,
            space_root: None,
            space: true,
        }
    }
}

struct Ready {
    links: LinksConfig,
    pred: Predictor,
    terrain: TidewaterTerrain,
    /// The ground station's altitude against the DEM, as the emulator logs it.
    gs_check: Option<String>,
}

/// The predictor, its terrain cache and its SATCOM source, made on first use.
pub struct Coverage {
    src: Sources,
    clock: crate::space::ScenarioClock,
    ready: tokio::sync::OnceCell<Result<Arc<Ready>, String>>,
    space: tokio::sync::OnceCell<Result<Arc<RealSpace>, String>>,
    grids: Mutex<VecDeque<(String, Arc<Value>)>>,
}

/// A vehicle as the route answer needs it.
#[derive(Clone, Debug)]
struct Who {
    uid: Uid,
    callsign: String,
    profile: String,
    domain: String,
    speed_mps: f64,
    /// Where it is now (its reported altitude).
    pos: Option<GeoPoint>,
    /// The altitude its orders fly at, above mean sea level, and how it was found.
    alt_msl: Option<f64>,
    alt_basis: &'static str,
}

/// One vehicle's route for an order, laid out by the decision engine's route geometry.
pub struct VehicleRoute {
    pub uid: Option<Uid>,
    pub route: Result<murmur_decide::Route, String>,
}

/// A leg as a line: its part, its name, whether it is flown again and again, its [(lon, lat)].
type Line = (Part, String, bool, Vec<(f64, f64)>);
/// A leg sampled: its part, name and loop, its points with how far along each is, its length.
type Sampled = (Part, String, bool, Vec<(f64, GeoPoint)>, f64);

/// A query's target: a vehicle's own profile, or a class or profile.
enum Target {
    Vehicle(Box<Who>),
    Profile(String),
}

fn bad(m: impl Into<String>) -> (u16, String) {
    (400, m.into())
}

fn round_to(x: f64, n: i32) -> f64 {
    let f = 10f64.powi(n);
    (x * f).round() / f
}

impl Coverage {
    pub fn new(src: Sources) -> Coverage {
        Coverage {
            src,
            clock: crate::space::ScenarioClock::from_env(),
            ready: tokio::sync::OnceCell::new(),
            space: tokio::sync::OnceCell::new(),
            grids: Mutex::new(VecDeque::new()),
        }
    }

    pub fn from_env(root: &std::path::Path) -> Coverage {
        Coverage::new(Sources::from_env(root))
    }

    /// Scenario time now, unix seconds.
    fn now_unix(&self) -> f64 {
        self.clock.now().timestamp_millis() as f64 / 1000.0
    }

    async fn ready(&self) -> Result<Arc<Ready>, String> {
        self.ready
            .get_or_init(|| async {
                let mut links = LinksConfig::load(&self.src.links)?;
                // the ground station's altitude settled against the DEM before anything is
                // predicted from it, as the emulator settles it before building its links
                let dem = match (&self.src.terrain, &links.ground_station) {
                    (Some((s, _)), Some(gs)) => dem_height(s.as_ref(), self.now_unix(), gs.lat, gs.lon)
                        .await
                        .ok()
                        .flatten(),
                    _ => None,
                };
                let gs_check = links.resolve_ground_alt(dem).map(|c| c.message().1);
                let pred = Predictor::new(&links)?;
                let terrain = TidewaterTerrain::new(links.terrain.clone());
                Ok(Arc::new(Ready {
                    links,
                    pred,
                    terrain,
                    gs_check,
                }))
            })
            .await
            .clone()
    }

    /// The SATCOM source, loaded once on first need (as the emulator loads it: the links file's
    /// `[space]`, its time map from the scenario's start).
    async fn space(&self, ready: &Ready) -> Result<Arc<RealSpace>, String> {
        if !self.src.space {
            return Err("SATCOM prediction off: the profiles' static numbers".into());
        }
        self.space
            .get_or_init(|| async {
                let mut cfg = ready.links.space.clone();
                let resolve = |p: &str| -> String {
                    let pb = PathBuf::from(p);
                    if pb.is_absolute() {
                        p.to_string()
                    } else {
                        self.src.root.join(pb).to_string_lossy().into_owned()
                    }
                };
                match &self.src.space_root {
                    Some(root) => {
                        let dir = root.join("sim/space");
                        cfg.catalog = dir.join("constellations.toml").to_string_lossy().into_owned();
                        cfg.cache = dir.join("cache").to_string_lossy().into_owned();
                    }
                    None => {
                        cfg.catalog = resolve(&cfg.catalog);
                        cfg.cache = resolve(&cfg.cache);
                    }
                }
                let start = self
                    .clock
                    .at
                    .map(|a| a.timestamp_millis() as f64 / 1000.0)
                    .unwrap_or_else(|| self.now_unix());
                load_space(cfg, start).await.map(Arc::new)
            })
            .await
            .clone()
    }

    fn terrain_name(&self) -> String {
        match &self.src.terrain {
            Some((_, name)) => name.clone(),
            None => "not checked (no MURMUR_TIDEWATER_URL or surface grid), as the emulator without one".into(),
        }
    }

    fn terrain_of<'a>(&self, ready: &'a Ready) -> &'a dyn Terrain {
        if self.src.terrain.is_some() {
            &ready.terrain
        } else {
            &NoTerrain
        }
    }

    /// Fetches the terrain cells `points` need on `profile` (each cell once, kept for good).
    async fn warm(&self, ready: &Ready, profile: &str, points: &[GeoPoint]) {
        let Some((sampler, _)) = &self.src.terrain else { return };
        let (Some(gs), Some(los)) = (ready.pred.ground_station(), ready.pred.los(profile)) else {
            return;
        };
        for p in points {
            let _ = ready.terrain.obstruction_m(p, &gs, &los);
        }
        let t = self.now_unix();
        for _ in 0..WARM_ROUNDS {
            if ready.terrain.sizes().0 == 0 {
                break;
            }
            if ready.terrain.tick(sampler.as_ref(), t).await == 0 {
                break; // the source failed: what is unanswered stays unknown, and says so
            }
        }
    }

    /// Surface heights at points (the sampler's: terrain, or the water where higher).
    async fn surface(&self, points: &[(f64, f64)]) -> Vec<Option<f64>> {
        let Some((sampler, _)) = &self.src.terrain else {
            return vec![None; points.len()];
        };
        let ll: Vec<(f64, f64)> = points.iter().map(|(lon, lat)| (*lat, *lon)).collect();
        let mut out = Vec::with_capacity(ll.len());
        for chunk in ll.chunks(20_000) {
            match sampler.sample(self.now_unix(), chunk).await {
                Ok(h) if h.len() == chunk.len() => out.extend(h),
                _ => out.extend(std::iter::repeat_n(None, chunk.len())),
            }
        }
        out
    }

    /// The constellation's answers at points at one time each (`None` where the profile has no
    /// satellites there).
    async fn satellites(
        &self,
        ready: &Ready,
        profile: &str,
        points: &[(GeoPoint, f64)],
    ) -> (Vec<Option<Result<SatObs, String>>>, Option<String>) {
        let classes: Vec<Option<String>> = points
            .iter()
            .map(|(p, _)| ready.pred.sat_class(profile, p).map(str::to_string))
            .collect();
        if classes.iter().all(Option::is_none) {
            return (vec![None; points.len()], None);
        }
        let space = match self.space(ready).await {
            Ok(s) => s,
            Err(e) => {
                let out = classes.iter().map(|c| c.as_ref().map(|_| Err(e.clone()))).collect();
                return (out, Some(e));
            }
        };
        let jobs: Vec<(Option<String>, GeoPoint, f64)> = classes
            .into_iter()
            .zip(points.iter())
            .map(|(c, (p, t))| (c, *p, *t))
            .collect();
        let out = run_blocking(move || {
            jobs.iter()
                .map(|(c, p, t)| c.as_ref().map(|c| space.sample(c, p, *t)))
                .collect::<Vec<_>>()
        })
        .await;
        (out, Some("murmur-space: satellites of the class above the terminal's mask".into()))
    }

    fn sources(&self, ready: &Ready, satcom: Option<String>) -> Value {
        json!({
            "links": self.src.links.display().to_string(),
            "terrain": self.terrain_name(),
            "terrain_cell_m": ready.links.terrain.cell_m,
            "terrain_alt_step_m": ready.links.terrain.alt_step_m,
            "ground_station": ready.pred.ground_station().map(|g| json!([g.lon, g.lat, g.alt_m])),
            "ground_station_check": ready.gs_check,
            "satcom": satcom,
        })
    }

    // ---- vehicles ---------------------------------------------------------------------------

    fn who(&self, world: &World, uid: Uid, pred: &Predictor) -> Option<Who> {
        let cell = world.vehicles().into_iter().find(|c| c.uid == uid)?;
        Some(cell.read(|v| {
            let ident = v.identity.as_deref();
            let platform = ident.and_then(|i| i.str("platform"));
            let lim = crate::estimate::platform_limits(world, platform, v.kind);
            let link = ident
                .and_then(|i| i.str("link"))
                .map(str::to_string)
                .or(lim.link.clone())
                .unwrap_or_default();
            let space_class = ident
                .and_then(|i| i.str("space_class"))
                .map(str::to_string)
                .or(lim.space_class.clone());
            let spec = VehicleSpec {
                uid: u32::try_from(uid).unwrap_or(u32::MAX),
                sysid: 0,
                port: 0,
                link,
                domain: lim.domain.clone(),
                platform: platform.unwrap_or_default().to_string(),
                callsign: String::new(),
                space_class,
            };
            // home's altitude: reported, else the position's altitude less its height above home
            let home_alt = v.home_alt.or(match (v.alt_msl, v.alt_rel) {
                (Some(a), Some(r)) => Some(a - r),
                _ => None,
            });
            // the altitude the task layer gives its missions (`tasks::mission_alt`, slot 0)
            let rel = v.kind.and_then(crate::tasks::caps).map(|c| {
                crate::tasks::mission_alt(c, ident.and_then(|i| i.num("cruise_alt_m")), home_alt, 0)
            });
            let (alt_msl, alt_basis) = match (rel, home_alt) {
                (Some(r), Some(h)) => (Some(h + r), "its mission altitude"),
                _ => (v.alt_msl, "its altitude now"),
            };
            Who {
                uid,
                callsign: ident
                    .and_then(|i| i.str("callsign"))
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("uid {uid}")),
                profile: pred.profile_for(&spec),
                domain: lim.domain.clone(),
                speed_mps: lim.cruise_mps.max(0.5),
                pos: v.pos.map(|(lat, lon)| GeoPoint {
                    lat,
                    lon,
                    alt_m: v.alt_msl.unwrap_or(0.0),
                }),
                alt_msl,
                alt_basis,
            }
        }))
    }

    /// The altitudes a vehicle flies or sails at the points: an aircraft's mission altitude; a
    /// ground vehicle on the surface there; a boat or a submarine at its mission depth.
    async fn altitudes(&self, who: &Who, points: &[(f64, f64)]) -> Vec<f64> {
        let fixed = who.alt_msl.or(who.pos.map(|p| p.alt_m)).unwrap_or(0.0);
        if who.domain != "ground" {
            return vec![fixed; points.len()];
        }
        self.surface(points)
            .await
            .into_iter()
            .map(|h| h.unwrap_or(fixed))
            .collect()
    }

    fn failsafe(&self, ready: &Ready, profile: &str, active: &str) -> Option<Failsafe> {
        let lost_link_s = ready.pred.profile(profile)?.lost_link_s?;
        let heartbeat_s = crate::telemetry::LINKS
            .iter()
            .find(|r| r.profile == active)
            .map_or(1.0, |r| r.gcs_heartbeat_s as f64);
        Some(Failsafe {
            lost_link_s,
            heartbeat_s,
        })
    }

    // ---- answers ----------------------------------------------------------------------------

    /// Predictions at points on `profile` (altitudes given), terrain fetched first, SATCOM at
    /// each point's time.
    async fn predict_all(
        &self,
        ready: &Ready,
        profile: &str,
        points: &[(GeoPoint, f64)],
    ) -> Result<(Vec<Prediction>, Option<String>), String> {
        let geo: Vec<GeoPoint> = points.iter().map(|(p, _)| *p).collect();
        self.warm(ready, profile, &geo).await;
        let (sats, satcom) = self.satellites(ready, profile, points).await;
        let terrain = self.terrain_of(ready);
        let mut out = Vec::with_capacity(points.len());
        for ((p, _), s) in points.iter().zip(&sats) {
            out.push(ready.pred.predict(profile, p, terrain, s.as_ref())?);
        }
        Ok((out, satcom))
    }

    /// Points: one prediction each.
    async fn points(&self, world: &World, q: &Value) -> Result<Value, (u16, String)> {
        let ready = self.ready().await.map_err(|e| (503, e))?;
        let target = self.target(world, &ready, q)?;
        let pts = lonlats(q.get("points").or_else(|| q.get("point")).ok_or_else(|| bad("point or points"))?)?;
        let profile = target.profile().to_string();
        let alts = match q_alt(q) {
            Some(a) => self.with_alt(&pts, a, q_above(q)).await,
            None => match &target {
                Target::Vehicle(w) => self.altitudes(w, &pts).await,
                Target::Profile(_) => self.with_alt(&pts, DEFAULT_ALT_M, false).await,
            },
        };
        let t = self.at(q)?;
        let at: Vec<(GeoPoint, f64)> = pts
            .iter()
            .zip(&alts)
            .map(|((lon, lat), a)| (GeoPoint { lat: *lat, lon: *lon, alt_m: *a }, t))
            .collect();
        let (preds, satcom) = self.predict_all(&ready, &profile, &at).await.map_err(|e| (400, e))?;
        let items: Vec<Value> = at
            .iter()
            .zip(preds)
            .map(|((p, _), pr)| {
                json!({
                    "point": [p.lon, p.lat],
                    "alt_m": round_to(p.alt_m, 1),
                    "state": pr.state,
                    "p_loss": round_to(pr.p_loss, 3),
                    "reason": pr.reason,
                    "prediction": pr,
                })
            })
            .collect();
        let mut out = json!({
            "predicted": true,
            "profile": profile,
            "failsafe_s": ready.pred.profile(&profile).and_then(|p| p.lost_link_s),
            "points": items,
            "thresholds": {"degraded": DEGRADED_LOSS, "down": DOWN_LOSS},
            "sources": self.sources(&ready, satcom),
            "note": NOTE,
        });
        target.describe(&mut out);
        Ok(out)
    }

    async fn with_alt(&self, pts: &[(f64, f64)], alt: f64, above_surface: bool) -> Vec<f64> {
        if !above_surface {
            return vec![alt; pts.len()];
        }
        self.surface(pts)
            .await
            .into_iter()
            .map(|h| h.unwrap_or(0.0) + alt)
            .collect()
    }

    fn at(&self, q: &Value) -> Result<f64, (u16, String)> {
        match q.get("at").and_then(Value::as_str) {
            Some(s) => crate::space::parse_time(s)
                .map(|t| t.timestamp_millis() as f64 / 1000.0)
                .map_err(|e| bad(format!("at: {e}"))),
            None => Ok(self.now_unix()),
        }
    }

    fn target(&self, world: &World, ready: &Ready, q: &Value) -> Result<Target, (u16, String)> {
        if let Some(uid) = q.get("uid").and_then(Value::as_u64) {
            let w = self
                .who(world, uid, &ready.pred)
                .ok_or_else(|| (404, format!("no vehicle with uid {uid}")))?;
            if ready.pred.profile(&w.profile).is_none() {
                return Err((422, format!("{} has no link profile {:?} in the links file", w.callsign, w.profile)));
            }
            return Ok(Target::Vehicle(Box::new(w)));
        }
        let class = q
            .get("class")
            .or_else(|| q.get("profile"))
            .and_then(Value::as_str)
            .ok_or_else(|| bad("give uid (a vehicle) or class (a link class or profile)"))?;
        if ready.pred.profile(class).is_none() {
            let known: Vec<&str> = ready.pred.profiles().map(|p| p.name.as_str()).collect();
            return Err(bad(format!("unknown link class or profile {class:?}; known: {}", known.join(", "))));
        }
        Ok(Target::Profile(class.to_string()))
    }

    /// A grid over a box.
    async fn grid(&self, world: &World, q: &Value) -> Result<Value, (u16, String)> {
        let ready = self.ready().await.map_err(|e| (503, e))?;
        let target = self.target(world, &ready, q)?;
        let profile = target.profile().to_string();
        let bbox = q_bbox(q)?;
        let alt = q_alt(q).unwrap_or(match &target {
            Target::Vehicle(w) => w.alt_msl.unwrap_or(DEFAULT_ALT_M),
            Target::Profile(_) => DEFAULT_ALT_M,
        });
        let above = q_above(q);
        let mut res = q.get("res").or_else(|| q.get("res_m")).and_then(Value::as_f64).unwrap_or(ready.links.terrain.cell_m);
        if !(res.is_finite() && res > 0.0) {
            return Err(bad("res: metres, more than 0"));
        }
        let [w, s, e, n] = bbox;
        let lat_mid = 0.5 * (s + n);
        let cells = |res: f64| {
            let dlat = res / 111_320.0;
            let dlon = dlat / lat_mid.to_radians().cos().max(0.05);
            (((n - s) / dlat).ceil().max(1.0) as usize, ((e - w) / dlon).ceil().max(1.0) as usize, dlat, dlon)
        };
        let max = q
            .get("max_cells")
            .and_then(Value::as_u64)
            .map_or(MAX_CELLS, |m| (m as usize).clamp(1, MAX_CELLS));
        let (mut nlat, mut nlon, mut dlat, mut dlon) = cells(res);
        let coarsened = nlat * nlon > max;
        while nlat * nlon > max {
            res *= ((nlat * nlon) as f64 / max as f64).sqrt() * 1.01;
            (nlat, nlon, dlat, dlon) = cells(res);
        }
        let t = self.at(q)?;
        let sat = ready.pred.profile(&profile).is_some_and(|p| p.params.space_class.is_some());
        let key = format!("{profile}|{alt}|{above}|{w},{s},{e},{n}|{res}");
        if !sat
            && let Some(hit) = self.grids.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key)
        {
            let mut v = (*hit.1).clone();
            target.describe(&mut v);
            return Ok(v);
        }
        let mut pts = Vec::with_capacity(nlat * nlon);
        for i in 0..nlat {
            for j in 0..nlon {
                pts.push((w + (j as f64 + 0.5) * dlon, s + (i as f64 + 0.5) * dlat));
            }
        }
        let alts = self.with_alt(&pts, alt, above).await;
        let at: Vec<(GeoPoint, f64)> = pts
            .iter()
            .zip(&alts)
            .map(|((lon, lat), a)| (GeoPoint { lat: *lat, lon: *lon, alt_m: *a }, t))
            .collect();
        let (preds, satcom) = self.predict_all(&ready, &profile, &at).await.map_err(|e| (400, e))?;
        let mut state = String::with_capacity(preds.len());
        let mut reason = String::with_capacity(preds.len());
        let mut loss = Vec::with_capacity(preds.len());
        let (mut up, mut deg, mut down, mut unknown) = (0, 0, 0, 0);
        for p in &preds {
            if p.terrain_unknown {
                state.push('?');
                unknown += 1;
            } else {
                state.push(p.state.letter());
                match p.state {
                    State::Up => up += 1,
                    State::Degraded => deg += 1,
                    State::Down => down += 1,
                }
            }
            reason.push(p.reason.map_or('-', |r| r.letter()));
            loss.push(json!(round_to(p.p_loss, 3)));
        }
        let out = json!({
            "predicted": true,
            "profile": profile,
            "alt_m": alt,
            "alt_ref": if above { "surface" } else { "msl" },
            "at": murmur_linkemu::sat::iso(t),
            "bbox": [w, s, e, n],
            "res_m": round_to(res, 1),
            "coarsened": coarsened,
            "lat0": s + 0.5 * dlat,
            "lon0": w + 0.5 * dlon,
            "dlat": dlat,
            "dlon": dlon,
            "nlat": nlat,
            "nlon": nlon,
            "order": "rows south to north, each west to east: cell (i, j) is at lat0 + i dlat, lon0 + j dlon",
            "state": state,
            "reason": reason,
            "p_loss": loss,
            "counts": {"up": up, "degraded": deg, "down": down, "unknown": unknown},
            "legend": {
                "state": {"u": "up", "d": "degraded", "x": "down", "?": "terrain not answered yet (counted open, as the emulator does)"},
                "reason": {"-": "none", "l": "the link's own loss", "r": "range", "t": "terrain", "s": "no satellite",
                           "w": "submerged", "a": "at the surface"},
            },
            "thresholds": {"degraded": DEGRADED_LOSS, "down": DOWN_LOSS},
            "failsafe_s": ready.pred.profile(&profile).and_then(|p| p.lost_link_s),
            "sources": self.sources(&ready, satcom),
            "note": NOTE,
        });
        if !sat && unknown == 0 {
            let mut g = self.grids.lock().unwrap_or_else(|e| e.into_inner());
            g.push_back((key, Arc::new(out.clone())));
            while g.len() > GRID_CACHE {
                g.pop_front();
            }
        }
        let mut out = out;
        target.describe(&mut out);
        Ok(out)
    }

    /// A line of points flown in order (from the vehicle with `uid`), or a loop.
    async fn line(&self, world: &World, q: &Value) -> Result<Value, (u16, String)> {
        let ready = self.ready().await.map_err(|e| (503, e))?;
        let target = self.target(world, &ready, q)?;
        let mut pts = lonlats(q.get("route").ok_or_else(|| bad("route"))?)?;
        let looped = q.get("loop").and_then(Value::as_bool).unwrap_or(false);
        let (who, from_vehicle) = match &target {
            Target::Vehicle(w) => ((**w).clone(), q.get("from_vehicle").and_then(Value::as_bool).unwrap_or(true)),
            Target::Profile(p) => (
                Who {
                    uid: 0,
                    callsign: format!("A {p} link"),
                    profile: p.clone(),
                    domain: "air".into(),
                    speed_mps: 20.0,
                    pos: None,
                    alt_msl: Some(DEFAULT_ALT_M),
                    alt_basis: "the default altitude",
                },
                false,
            ),
        };
        let mut who = who;
        if let Some(a) = q_alt(q) {
            who.alt_msl = Some(a);
            who.alt_basis = "the altitude asked";
        }
        if let Some(v) = q.get("speed_mps").and_then(Value::as_f64).filter(|v| *v > 0.0) {
            who.speed_mps = v;
        }
        let mut legs = Vec::new();
        if from_vehicle {
            let pos = who.pos.ok_or_else(|| (409, format!("{} has no position", who.callsign)))?;
            if let Some(first) = pts.first() {
                legs.push((Part::Transit, "transit leg (position to point 1)".to_string(), false, vec![(pos.lon, pos.lat), *first]));
            }
        }
        if looped {
            if let Some(first) = pts.first().copied() {
                pts.push(first);
            }
            legs.push((Part::Task, "the loop (one lap)".to_string(), true, pts));
        } else {
            legs.push((Part::Transit, "the route".to_string(), false, pts));
        }
        let mut v = self.answer_legs(&ready, &who, legs, self.at(q)?).await;
        v["sources"] = self.sources(&ready, v["sources"]["satcom"].as_str().map(str::to_string));
        v["note"] = json!(NOTE);
        v["predicted"] = json!(true);
        Ok(v)
    }

    /// The answer for an order: each vehicle's route as the decision engine lays it out.
    pub async fn order_answer(&self, world: &World, routes: Vec<VehicleRoute>) -> Value {
        let ready = match self.ready().await {
            Ok(r) => r,
            Err(e) => return json!({ "predicted": false, "error": e, "note": NOTE }),
        };
        let mut vehicles = Vec::new();
        let mut satcom = None;
        for vr in routes {
            let Some(uid) = vr.uid else { continue };
            let Some(who) = self.who(world, uid, &ready.pred) else {
                vehicles.push(json!({ "uid": uid, "error": format!("no vehicle with uid {uid}") }));
                continue;
            };
            if ready.pred.profile(&who.profile).is_none() {
                vehicles.push(json!({ "uid": uid, "callsign": who.callsign,
                    "error": format!("no link profile {:?} in the links file", who.profile) }));
                continue;
            }
            let route = match vr.route {
                Ok(r) => r,
                Err(e) => {
                    vehicles.push(json!({ "uid": uid, "callsign": who.callsign, "error": e }));
                    continue;
                }
            };
            let ll = |p: murmur_decide::LonLat| (p.lon, p.lat);
            let mut legs = Vec::new();
            if let Some(s) = &route.transit {
                // rules 22: through the waypoints a plan routes it by
                let mut pts = vec![ll(s.from)];
                pts.extend(route.transit_via.iter().copied().map(ll));
                pts.push(ll(s.to));
                legs.push((Part::Transit, s.name(), false, pts));
            }
            if let Some(path) = &route.task_routed() {
                let mut pts: Vec<(f64, f64)> = path.points.iter().copied().map(ll).collect();
                if path.closed
                    && let Some(f) = pts.first().copied()
                {
                    pts.push(f);
                }
                let name = if path.closed { "task path (one lap)" } else { "task path" };
                legs.push((Part::Task, name.to_string(), path.closed, pts));
            }
            if let Some(s) = &route.ret {
                legs.push((Part::Return, s.name(), false, vec![ll(s.from), ll(s.to)]));
            }
            if legs.is_empty() {
                // a hold or a track: it stays where it is, at the altitude it is at
                match who.pos {
                    Some(p) => legs.push((Part::Here, "where it is".to_string(), false, vec![(p.lon, p.lat)])),
                    None => {
                        vehicles.push(json!({ "uid": uid, "callsign": who.callsign, "error": "no position" }));
                        continue;
                    }
                }
            }
            let v = self.answer_legs(&ready, &who, legs, self.now_unix()).await;
            if let Some(s) = v["sources"]["satcom"].as_str() {
                satcom = Some(s.to_string());
            }
            vehicles.push(v);
        }
        json!({
            "predicted": true,
            "vehicles": vehicles,
            "holds_when": format!(
                "the chance its lost-link failsafe trips on the way (the transit, then {:.0} minutes on the task path) is at most {:.0} %",
                ON_STATION_S / 60.0,
                100.0 * HOLDS_P
            ),
            "sources": self.sources(&ready, satcom),
            "note": NOTE,
        })
    }

    /// Legs as polylines of (lon, lat): sampled, predicted and answered for one vehicle.
    async fn answer_legs(
        &self,
        ready: &Ready,
        who: &Who,
        legs: Vec<Line>,
        t0: f64,
    ) -> Value {
        let here = legs.iter().all(|l| l.0 == Part::Here);
        // every point of every leg, with how far along its leg
        let mut flat: Vec<(f64, f64)> = Vec::new();
        for (_, _, _, line) in &legs {
            flat.extend(line.iter().copied());
        }
        let alts = if here {
            vec![who.pos.map_or(0.0, |p| p.alt_m); flat.len()]
        } else {
            self.altitudes(who, &flat).await
        };
        let mut k = 0;
        let mut shaped: Vec<Sampled> = Vec::new();
        for (part, name, looped, line) in legs {
            let pts: Vec<GeoPoint> = line
                .iter()
                .map(|(lon, lat)| {
                    let a = alts[k];
                    k += 1;
                    GeoPoint { lat: *lat, lon: *lon, alt_m: a }
                })
                .collect();
            let mut samples: Vec<(f64, GeoPoint)> = Vec::new();
            let mut base = 0.0;
            if pts.len() == 1 {
                samples.push((0.0, pts[0]));
            }
            for w in pts.windows(2) {
                let seg = along(&w[0], &w[1], ROUTE_STEP_M);
                let len = seg.last().map_or(0.0, |s| s.1);
                for (i, (p, d)) in seg.into_iter().enumerate() {
                    if i == 0 && !samples.is_empty() {
                        continue; // the joint, once
                    }
                    samples.push((base + d, p));
                }
                base += len;
            }
            shaped.push((part, name, looped, samples, base));
        }
        // each point's time: SATCOM looked at coarsely along the way (visibility changes over
        // minutes), terrain at every point
        let v = who.speed_mps.max(0.5);
        let mut elapsed = 0.0;
        let mut all: Vec<(GeoPoint, f64)> = Vec::new();
        for (_, _, _, samples, len) in &shaped {
            for (d, p) in samples {
                let t = t0 + ((elapsed + d) / v / SAT_STEP_S).floor() * SAT_STEP_S;
                all.push((*p, t));
            }
            elapsed += len;
        }
        let (preds, satcom) = match self.predict_all(ready, &who.profile, &all).await {
            Ok(x) => x,
            Err(e) => return json!({ "uid": who.uid, "callsign": who.callsign, "error": e }),
        };
        let mut it = preds.into_iter();
        let mut leg_samples = Vec::new();
        for (part, name, looped, samples, len) in shaped {
            let s: Vec<(f64, GeoPoint, Prediction)> = samples
                .into_iter()
                .map(|(d, p)| (d, p, it.next().unwrap_or_else(|| unreachable_prediction(&who.profile))))
                .collect();
            leg_samples.push(LegSamples {
                part,
                name,
                looped,
                length_m: len,
                samples: s,
            });
        }
        let active = leg_samples
            .iter()
            .find(|l| l.part != Part::Return)
            .or(leg_samples.first())
            .and_then(|l| l.samples.first())
            .map_or(who.profile.clone(), |s| s.2.profile.clone());
        let fs = self.failsafe(ready, &who.profile, &active);
        let ans = route_answer(&leg_samples, v, fs.as_ref());
        let text = sentence(&who.callsign, &ans);
        let mut out = serde_json::to_value(&ans).unwrap_or(Value::Null);
        if let Some(o) = out.as_object_mut() {
            o.insert("uid".into(), json!(who.uid));
            o.insert("callsign".into(), json!(who.callsign));
            o.insert("profile".into(), json!(who.profile));
            o.insert("active_profile".into(), json!(active));
            o.insert(
                "alt_m".into(),
                json!(if here { who.pos.map(|p| round_to(p.alt_m, 1)) } else { who.alt_msl.map(|a| round_to(a, 1)) }),
            );
            o.insert(
                "alt_basis".into(),
                json!(if here {
                    "its altitude now"
                } else if who.domain == "ground" {
                    "the surface along the way"
                } else {
                    who.alt_basis
                }),
            );
            o.insert("sentence".into(), json!(text));
            o.insert("sources".into(), json!({ "satcom": satcom }));
        }
        out
    }
}

/// Never used: the predictions come one a point.
fn unreachable_prediction(profile: &str) -> Prediction {
    Prediction {
        state: State::Up,
        p_loss: 0.0,
        reason: None,
        profile: profile.to_string(),
        base_loss: 0.0,
        range_loss: 0.0,
        terrain_loss: 0.0,
        blocked: false,
        range_m: None,
        obstruction_m: None,
        terrain_unknown: true,
        satcom: None,
    }
}

/// A class's default altitude, m above mean sea level (a small aircraft's).
pub const DEFAULT_ALT_M: f64 = 100.0;

impl Target {
    fn profile(&self) -> &str {
        match self {
            Target::Vehicle(w) => &w.profile,
            Target::Profile(p) => p,
        }
    }

    fn describe(&self, out: &mut Value) {
        if let Target::Vehicle(w) = self {
            out["uid"] = json!(w.uid);
            out["callsign"] = json!(w.callsign);
        }
    }
}

fn q_alt(q: &Value) -> Option<f64> {
    q.get("alt").or_else(|| q.get("alt_m")).and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
}

fn q_above(q: &Value) -> bool {
    match q.get("above_surface").or_else(|| q.get("alt_ref")) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.as_str(), "1" | "true" | "surface"),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        _ => false,
    }
}

fn q_bbox(q: &Value) -> Result<[f64; 4], (u16, String)> {
    let v: Vec<f64> = match q.get("bbox") {
        Some(Value::String(s)) => s.split(',').filter_map(|x| x.trim().parse().ok()).collect(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_f64).collect(),
        _ => return Err(bad("bbox: w,s,e,n in degrees")),
    };
    match v[..] {
        [w, s, e, n] if w < e && s < n && (-180.0..=180.0).contains(&w) && (-90.0..=90.0).contains(&s) => Ok([w, s, e, n]),
        _ => Err(bad("bbox: w,s,e,n in degrees, w < e and s < n")),
    }
}

/// [[lon, lat], ...], a single [lon, lat], or a "lon,lat" string (the query's `point`).
fn lonlats(v: &Value) -> Result<Vec<(f64, f64)>, (u16, String)> {
    let one = |v: &Value| -> Option<(f64, f64)> {
        match v {
            Value::Array(a) if a.len() == 2 => Some((a[0].as_f64()?, a[1].as_f64()?)),
            Value::String(s) => {
                let mut it = s.split(',').map(|x| x.trim().parse::<f64>());
                Some((it.next()?.ok()?, it.next()?.ok()?))
            }
            _ => None,
        }
    };
    let pts: Vec<(f64, f64)> = match one(v) {
        Some(p) => vec![p],
        None => v
            .as_array()
            .ok_or_else(|| bad("points: [[lon, lat], ...]"))?
            .iter()
            .map(one)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad("points: [[lon, lat], ...]"))?,
    };
    if pts.is_empty() || pts.len() > MAX_CELLS {
        return Err(bad(format!("between 1 and {MAX_CELLS} points")));
    }
    if pts.iter().any(|(lon, lat)| !((-180.0..=180.0).contains(lon) && (-90.0..=90.0).contains(lat))) {
        return Err(bad("a point outside [-180, 180] x [-90, 90]"));
    }
    Ok(pts)
}

/// Work on the satellites: off the async runtime live; at once in the headless simulator (its
/// paused clock must not move while it runs).
async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    #[cfg(not(murmur_headless))]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.await {
            Ok(v) => v,
            Err(_) => panic!("coverage: the satellite worker died"),
        }
    }
    #[cfg(murmur_headless)]
    {
        f()
    }
}

async fn load_space(cfg: murmur_linkemu::config::SpaceCfg, start: f64) -> Result<RealSpace, String> {
    run_blocking(move || RealSpace::load(&cfg, start)).await
}

/// One query (module doc): a grid (`bbox`), points (`point`, `points`), a route (`route`), an
/// order (`order`, through the decision layer's route geometry; `brief` without each leg), several
/// (`queries`), or with only `uid` the vehicle where it is now.
pub async fn query(
    world: &World,
    decision: &crate::decision::Decision,
    q: &Value,
) -> Result<Value, (u16, String)> {
    if !q.is_object() {
        return Err(bad("a JSON object"));
    }
    let cov = world.coverage();
    if let Some(qs) = q.get("queries") {
        let qs = qs.as_array().ok_or_else(|| bad("queries: an array of queries"))?;
        if qs.len() > MAX_QUERIES {
            return Err(bad(format!("at most {MAX_QUERIES} queries a call")));
        }
        let mut out = Vec::with_capacity(qs.len());
        for one in qs {
            if one.get("queries").is_some() {
                out.push(json!({ "error": "queries do not nest" }));
                continue;
            }
            out.push(match Box::pin(query(world, decision, one)).await {
                Ok(v) => v,
                Err((status, e)) => json!({ "error": e, "status": status }),
            });
        }
        return Ok(json!({ "answers": out }));
    }
    if let Some(order) = q.get("order") {
        let mut o = order.clone();
        let Some(obj) = o.as_object_mut() else {
            return Err(bad("order: an object, as preview_intent takes it"));
        };
        if let Some(r) = obj.remove("radius_m") {
            obj.entry("radius").or_insert(r);
        }
        if let Some(t) = obj.remove("track_id") {
            obj.entry("track").or_insert(t);
        }
        if !obj.contains_key("uids")
            && let Some(u) = q.get("uid")
        {
            obj.insert("uids".into(), json!([u]));
        }
        let req: crate::decision::IntentRequest =
            serde_json::from_value(o).map_err(|e| bad(format!("order: {e}")))?;
        let mut out = decision.link_coverage_for(&req).await.map_err(|e| (e.status, e.detail))?;
        // brief: each vehicle's verdict, failsafe and sentence without its legs (a caller ranking
        // many choices a tick)
        if q.get("brief").and_then(Value::as_bool) == Some(true)
            && let Some(vs) = out.get_mut("vehicles").and_then(Value::as_array_mut)
        {
            for v in vs.iter_mut().filter_map(Value::as_object_mut) {
                v.remove("legs");
                v.remove("sources");
            }
        }
        return Ok(out);
    }
    if q.get("bbox").is_some() {
        return cov.grid(world, q).await;
    }
    if q.get("route").is_some() {
        return cov.line(world, q).await;
    }
    if q.get("point").is_some() || q.get("points").is_some() {
        return cov.points(world, q).await;
    }
    if let Some(uid) = q.get("uid").and_then(Value::as_u64) {
        // where it is now, at its altitude now
        let ready = cov.ready().await.map_err(|e| (503, e))?;
        let w = cov
            .who(world, uid, &ready.pred)
            .ok_or_else(|| (404, format!("no vehicle with uid {uid}")))?;
        let p = w.pos.ok_or_else(|| (409, format!("{} has no position", w.callsign)))?;
        let mut one = q.clone();
        one["point"] = json!([p.lon, p.lat]);
        if q_alt(q).is_none() {
            one["alt_m"] = json!(p.alt_m);
        }
        return cov.points(world, &one).await;
    }
    Err(bad("give bbox (a grid), point or points, route, order, or queries, with uid or class"))
}

/// `GET /api/links/coverage?...`: the query string as a query.
pub fn from_query_string(params: &std::collections::HashMap<String, String>) -> Value {
    let mut q = serde_json::Map::new();
    for (k, v) in params {
        let val = match k.as_str() {
            "uid" => v.parse::<u64>().map_or_else(|_| json!(v), |u| json!(u)),
            "alt" | "alt_m" | "res" | "res_m" | "speed_mps" => {
                v.parse::<f64>().map_or_else(|_| json!(v), |x| json!(x))
            }
            _ => json!(v),
        };
        q.insert(k.clone(), val);
    }
    Value::Object(q)
}

#[cfg(test)]
pub(crate) mod tests;
