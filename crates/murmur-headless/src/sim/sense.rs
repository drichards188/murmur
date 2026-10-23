//! The physical sensing model's predictions for Python (evaluation only; WORLD-V3-PLAN 2.8
//! item 5, the v3 oracle): `crate::tracks::physical::predict` on one observer's sensors and one
//! contact, the bridge's own catalog and functions, served on an episode's channel (`sense`)
//! and by `murmur-headless --sense` (one request per line on stdin, one answer per line on
//! stdout, no episode). eval/headless/sensing.py is the client.
//!
//! A request: `observer` (`platform`, `lat`, `lon`, `alt_m`, and optionally `heading_deg`,
//! `mast_m`), `sensor` (a type: `eo_ir`, `radar`, `ais`, `sonar`, `esm`; none: every sensor of
//! the platform), `target` (`kind`, `domain`, `lat`, `lon`, and optionally `alt_m`,
//! `speed_mps`, `emits`, `ais` (true: its transponder is on), `signature` (the overrides a
//! scenario gives a contact)), `conditions` (a scenario's `[conditions]`), `t` (unix seconds of
//! the scenario clock, for the sun; not needed when the conditions state the sun's
//! elevation), `terrain` and `bathymetry` (grid paths, relative to the repository),
//! `ranges` (true: also the D/R/I ranges along the line of sight), `ais_load_per_min`. The
//! answer: `predictions`, one per sensor, each the prediction and with `ranges` its
//! `ranges_m` (detect, recognise, identify).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};

use crate::tracks::physical::conditions::Conditions;
use crate::tracks::physical::geometry::Grid;
use crate::tracks::physical::predict::{self, Ask};
use crate::tracks::physical::signature::SignatureOverride;
use crate::tracks::sensing::{Observer, SensorCatalog, SensorType};
use crate::tracks::truth::{Ais, ContactTruth, Domain, TruthAffiliation};

type Cache<T> = Mutex<HashMap<PathBuf, Arc<T>>>;

/// Catalogs and grids loaded once per path.
fn catalogs() -> &'static Cache<SensorCatalog> {
    static C: OnceLock<Cache<SensorCatalog>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn grids() -> &'static Cache<Grid> {
    static C: OnceLock<Cache<Grid>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn cached<T>(cache: &Cache<T>, path: &Path, load: impl FnOnce(&Path) -> Result<T, String>) -> Result<Arc<T>, String> {
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = c.get(path) {
        return Ok(v.clone());
    }
    let v = Arc::new(load(path)?);
    c.insert(path.to_path_buf(), v.clone());
    Ok(v)
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn need(v: &Value, key: &str, what: &str) -> Result<f64, String> {
    num(v, key).ok_or_else(|| format!("sense: {what}.{key} (a number)"))
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value, what: &str) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("sense: {what}: {e}"))
}

/// One request (the module doc), against the catalog under `root`.
pub fn sense(req: &Value, root: &Path) -> Result<Value, String> {
    let catalog = cached(catalogs(), &root.join("sim/platforms.toml"), SensorCatalog::load)?;
    let ov = &req["observer"];
    let platform = ov["platform"].as_str().ok_or("sense: observer.platform")?;
    let ps = catalog.get(platform).ok_or_else(|| format!("sense: no platform {platform:?} in the catalog"))?;
    let observer = Observer {
        uid: 1,
        lat: need(ov, "lat", "observer")?,
        lon: need(ov, "lon", "observer")?,
        alt_msl: num(ov, "alt_m").unwrap_or(0.0),
        mast_m: num(ov, "mast_m").unwrap_or(ps.mast_m),
        heading_deg: num(ov, "heading_deg"),
        ..Observer::default()
    };
    let tv = &req["target"];
    let domain: Domain = parse(&tv["domain"], "target.domain")?;
    let target = ContactTruth {
        id: "target".into(),
        name: String::new(),
        affiliation: TruthAffiliation::Unknown,
        domain,
        kind: tv["kind"].as_str().ok_or("sense: target.kind")?.to_string(),
        lat: need(tv, "lat", "target")?,
        lon: need(tv, "lon", "target")?,
        alt_m: num(tv, "alt_m").unwrap_or(0.0),
        speed_mps: num(tv, "speed_mps").unwrap_or(0.0),
        course_deg: num(tv, "course_deg").unwrap_or(0.0),
        emits: tv["emits"].as_bool().unwrap_or(false),
        signature: 1.0,
        ais: tv["ais"].as_bool().unwrap_or(false).then(|| Ais {
            mmsi: "000000000".into(),
            name: String::new(),
            ship_type: "other".into(),
            spoofed: false,
        }),
    };
    let over: Option<SignatureOverride> = match tv.get("signature") {
        Some(v) if !v.is_null() => Some(parse(v, "target.signature")?),
        _ => None,
    };
    let conditions: Conditions = match req.get("conditions") {
        Some(v) if !v.is_null() => parse(v, "conditions")?,
        _ => Conditions::default(),
    };
    let t = match num(req, "t") {
        Some(t) => t,
        None if conditions.sun_elevation_deg.is_some() => 0.0,
        None => return Err("sense: t (unix seconds of the scenario clock), or conditions.sun_elevation_deg".into()),
    };
    let grid = |key: &str| -> Result<Option<Arc<Grid>>, String> {
        match req[key].as_str() {
            Some(p) => {
                let path = Path::new(p);
                let path = if path.is_relative() { root.join(path) } else { path.to_path_buf() };
                cached(grids(), &path, Grid::load).map(Some)
            }
            None => Ok(None),
        }
    };
    let (terrain, bathymetry) = (grid("terrain")?, grid("bathymetry")?);
    let only: Option<SensorType> = match req.get("sensor") {
        Some(v) if !v.is_null() => Some(parse(v, "sensor")?),
        _ => None,
    };
    let mut out = Vec::new();
    for spec in ps.sensors.iter().filter(|s| only.is_none_or(|k| s.kind == k)) {
        let ask = Ask {
            spec,
            observer: &observer,
            target: &target,
            over: over.as_ref(),
            conditions,
            t,
            terrain: terrain.as_deref(),
            bathymetry: bathymetry.as_deref(),
            ais_load_per_min: num(req, "ais_load_per_min").unwrap_or(0.0),
        };
        let mut p = serde_json::to_value(predict::predict(&ask)).map_err(|e| format!("sense: {e}"))?;
        if req["ranges"].as_bool() == Some(true) {
            p["ranges_m"] = json!(predict::ranges(&ask));
        }
        out.push(p);
    }
    Ok(json!({ "platform": platform, "predictions": out }))
}

/// `murmur-headless --sense`: requests on stdin, answers on stdout, one per line, until stdin
/// closes (`{ok: false, error}` for a request that fails).
pub fn run_stdio(root: &Path) -> Result<(), String> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(req) => match sense(&req, root) {
                Ok(mut v) => {
                    v["ok"] = json!(true);
                    v
                }
                Err(e) => json!({ "ok": false, "error": e }),
            },
            Err(e) => json!({ "ok": false, "error": format!("request: {e}") }),
        };
        writeln!(stdout, "{answer}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
