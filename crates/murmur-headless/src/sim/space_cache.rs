//! SATCOM geometry for the emulator's links, kept between episodes.
//!
//! `sat::refresh` asks murmur-space which satellite serves a terminal every `interval_s` (5 s):
//! for a Starlink terminal that propagates the whole constellation, about 7 ms a look, which
//! alone was a fifth of a Harbor watch episode. Every episode of a scenario looks at the same
//! instants (the refresh runs on the episode's clock from t = 0) and its terminals keep to a
//! few kilometres, so the looks repeat: this source answers each look at the terminal's
//! position rounded to [`QUANT_DEG`] (about 100 m; altitude to [`QUANT_ALT_M`]), always, cached
//! or not, so an episode's answers do not depend on what the cache held. The difference from
//! the emulator, which looks at the exact position, is that rounding: well under a tenth of a
//! degree of elevation to a satellite 550 km up (docs/HEADLESS.md). Looks the episode computed
//! are appended to the cache file at the end (one line each, so concurrent episodes append
//! whole lines).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use murmur_linkemu::geo::GeoPoint;
use murmur_linkemu::sat::{BestSat, Outlook, SatObs, SpaceSource};
use serde_json::{Value, json};

pub const QUANT_DEG: f64 = 0.001;
pub const QUANT_ALT_M: f64 = 100.0;

type Key = (String, i64, i64, i64, i64);

pub struct CachedSpace {
    inner: Arc<dyn SpaceSource>,
    mem: Mutex<HashMap<Key, SatObs>>,
    new: Mutex<Vec<String>>,
    file: Option<PathBuf>,
}

fn key(class: &str, pos: &GeoPoint, unix_s: f64) -> (Key, GeoPoint) {
    let q = |x: f64, s: f64| (x / s).round() as i64;
    let (la, lo, al) = (q(pos.lat, QUANT_DEG), q(pos.lon, QUANT_DEG), q(pos.alt_m, QUANT_ALT_M));
    let at = GeoPoint {
        lat: la as f64 * QUANT_DEG,
        lon: lo as f64 * QUANT_DEG,
        alt_m: al as f64 * QUANT_ALT_M,
    };
    ((class.to_string(), la, lo, al, (unix_s * 1000.0).round() as i64), at)
}

fn to_json(k: &Key, o: &SatObs) -> String {
    json!({
        "k": [k.0, k.1, k.2, k.3, k.4],
        "visible": o.visible,
        "latency_ms": o.latency_ms,
        "best": o.best.as_ref().map(|b| json!({
            "norad_id": b.norad_id, "name": b.name, "elevation_deg": b.elevation_deg, "range_km": b.range_km,
        })),
    })
    .to_string()
}

fn from_json(v: &Value) -> Option<(Key, SatObs)> {
    let k = v["k"].as_array()?;
    let key = (
        k.first()?.as_str()?.to_string(),
        k.get(1)?.as_i64()?,
        k.get(2)?.as_i64()?,
        k.get(3)?.as_i64()?,
        k.get(4)?.as_i64()?,
    );
    let best = match &v["best"] {
        Value::Null => None,
        b => Some(BestSat {
            norad_id: b["norad_id"].as_u64()?,
            name: b["name"].as_str()?.to_string(),
            elevation_deg: b["elevation_deg"].as_f64()?,
            range_km: b["range_km"].as_f64()?,
        }),
    };
    Some((
        key,
        SatObs {
            visible: usize::try_from(v["visible"].as_u64()?).ok()?,
            best,
            latency_ms: v["latency_ms"].as_f64(),
        },
    ))
}

impl CachedSpace {
    pub fn new(inner: Arc<dyn SpaceSource>, file: Option<PathBuf>) -> CachedSpace {
        let mut mem = HashMap::new();
        if let Some(f) = &file
            && let Ok(fh) = std::fs::File::open(f)
        {
            for line in std::io::BufReader::new(fh).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && let Some((k, o)) = from_json(&v)
                {
                    mem.insert(k, o);
                }
            }
        }
        CachedSpace {
            inner,
            mem: Mutex::new(mem),
            new: Mutex::new(Vec::new()),
            file,
        }
    }

    /// Append the looks this episode computed.
    pub fn save(&self) {
        let Some(f) = &self.file else { return };
        let lines = std::mem::take(&mut *self.new.lock().unwrap_or_else(|e| e.into_inner()));
        if lines.is_empty() {
            return;
        }
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
            for l in lines {
                let _ = fh.write_all(format!("{l}\n").as_bytes());
            }
        }
    }
}

impl SpaceSource for CachedSpace {
    fn check(&self, class: &str, unix_s: f64) -> Result<(), String> {
        self.inner.check(class, unix_s)
    }

    fn sample(&self, class: &str, pos: &GeoPoint, unix_s: f64) -> Result<SatObs, String> {
        let (k, at) = key(class, pos, unix_s);
        if let Some(o) = self.mem.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            return Ok(o.clone());
        }
        let o = self.inner.sample(class, &at, unix_s)?;
        self.new.lock().unwrap_or_else(|e| e.into_inner()).push(to_json(&k, &o));
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).insert(k, o.clone());
        Ok(o)
    }

    fn outlook(&self, class: &str, pos: &GeoPoint, unix_s: f64, horizon_s: f64) -> Result<Outlook, String> {
        self.inner.outlook(class, pos, unix_s, horizon_s)
    }
}
