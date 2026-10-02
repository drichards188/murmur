//! Operator areas: the approved operating area(s) and no-go zones, from GeoJSON
//! (`sim/areas/*.geojson`).
//!
//! Each feature needs `properties.id` and `properties.kind` (`"approved"` or `"no_go"`);
//! `properties.name` is optional. Geometry is a `Polygon` or `MultiPolygon`. Anything else
//! is a load error: a silently skipped no-go zone would be a fail-open.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::geo::{LonLat, Polygon, TaskPath};
use crate::time::Time;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AreaKind {
    /// Where the operator has approved operations. Leaving it needs a human (policy).
    Approved,
    /// No task path may enter it (geofence rule).
    NoGo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub id: String,
    pub name: String,
    pub kind: AreaKind,
    pub polygons: Vec<Polygon>,
}

impl Area {
    pub fn contains(&self, p: LonLat) -> bool {
        self.polygons.iter().any(|poly| poly.contains(p))
    }

    pub fn segment_touches_boundary(&self, a: LonLat, b: LonLat) -> bool {
        self.polygons
            .iter()
            .any(|poly| poly.segment_touches_boundary(a, b))
    }

    /// Why `path` enters this area, or None if it stays clear.
    pub fn intrusion(&self, path: &TaskPath) -> Option<String> {
        for (i, p) in path.points.iter().enumerate() {
            if self.contains(*p) {
                return Some(format!("waypoint {} is inside {}", i + 1, self.id));
            }
        }
        for (i, a, b) in path.legs() {
            if self.segment_touches_boundary(a, b) {
                let to = if i + 1 == path.points.len() { 1 } else { i + 2 };
                return Some(format!("leg {}-{} crosses {}", i + 1, to, self.id));
            }
        }
        None
    }
}

/// The areas in force, with where and when they were loaded (both are logged as inputs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AreaSet {
    pub source: String,
    pub loaded_t: Time,
    pub areas: Vec<Area>,
}

#[derive(Debug, thiserror::Error)]
pub enum AreaError {
    #[error("{source_name}: {msg}")]
    Invalid { source_name: String, msg: String },
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

impl AreaSet {
    pub fn empty(source: impl Into<String>, loaded_t: Time) -> Self {
        AreaSet {
            source: source.into(),
            loaded_t,
            areas: Vec::new(),
        }
    }

    pub fn approved(&self) -> impl Iterator<Item = &Area> {
        self.areas.iter().filter(|a| a.kind == AreaKind::Approved)
    }

    pub fn no_go(&self) -> impl Iterator<Item = &Area> {
        self.areas.iter().filter(|a| a.kind == AreaKind::NoGo)
    }

    /// Parse one GeoJSON FeatureCollection.
    pub fn from_geojson(source: &str, text: &str, loaded_t: Time) -> Result<Self, AreaError> {
        let bad = |msg: String| AreaError::Invalid {
            source_name: source.to_string(),
            msg,
        };
        let doc: Value = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
        if doc.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
            return Err(bad("expected a GeoJSON FeatureCollection".into()));
        }
        let features = doc
            .get("features")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("missing features".into()))?;
        let mut areas = Vec::new();
        for (i, f) in features.iter().enumerate() {
            areas.push(parse_feature(f).map_err(|m| bad(format!("feature {i}: {m}")))?);
        }
        Ok(AreaSet {
            source: source.to_string(),
            loaded_t,
            areas,
        })
    }

    /// Load every `*.geojson` in a directory, in file-name order, as one set.
    pub fn load_dir(dir: &Path, loaded_t: Time) -> Result<Self, AreaError> {
        let io = |e| AreaError::Io(dir.display().to_string(), e);
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map_err(io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "geojson"))
            .collect();
        files.sort();
        let mut set = AreaSet::empty(dir.display().to_string(), loaded_t);
        let mut names = Vec::new();
        for f in files {
            let text = std::fs::read_to_string(&f)
                .map_err(|e| AreaError::Io(f.display().to_string(), e))?;
            let one = AreaSet::from_geojson(&f.display().to_string(), &text, loaded_t)?;
            set.areas.extend(one.areas);
            names.push(
                f.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        }
        set.source = format!("{} [{}]", dir.display(), names.join(", "));
        Ok(set)
    }
}

fn parse_feature(f: &Value) -> Result<Area, String> {
    let props = f.get("properties").ok_or("missing properties")?;
    let id = props
        .get("id")
        .and_then(Value::as_str)
        .ok_or("properties.id (string) is required")?
        .to_string();
    let name = props
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_string();
    let kind = match props.get("kind").and_then(Value::as_str) {
        Some("approved") => AreaKind::Approved,
        Some("no_go") => AreaKind::NoGo,
        other => {
            return Err(format!(
                "{id}: properties.kind must be \"approved\" or \"no_go\", got {other:?}"
            ));
        }
    };
    let geom = f.get("geometry").ok_or("missing geometry")?;
    let coords = geom.get("coordinates").ok_or("missing coordinates")?;
    let polygons = match geom.get("type").and_then(Value::as_str) {
        Some("Polygon") => vec![parse_polygon(coords)?],
        Some("MultiPolygon") => coords
            .as_array()
            .ok_or("MultiPolygon coordinates must be an array")?
            .iter()
            .map(parse_polygon)
            .collect::<Result<_, _>>()?,
        other => {
            return Err(format!(
                "{id}: geometry must be Polygon or MultiPolygon, got {other:?}"
            ));
        }
    };
    Ok(Area {
        id,
        name,
        kind,
        polygons,
    })
}

fn parse_polygon(v: &Value) -> Result<Polygon, String> {
    let rings = v.as_array().ok_or("polygon must be an array of rings")?;
    let mut parsed = rings.iter().map(parse_ring);
    let exterior = parsed.next().ok_or("polygon has no rings")??;
    let holes = parsed.collect::<Result<Vec<_>, _>>()?;
    Ok(Polygon { exterior, holes })
}

fn parse_ring(v: &Value) -> Result<Vec<LonLat>, String> {
    let pts = v.as_array().ok_or("ring must be an array of positions")?;
    let ring: Vec<LonLat> = pts
        .iter()
        .map(|p| {
            let a = p.as_array().ok_or("position must be an array")?;
            let lon = a.first().and_then(Value::as_f64).ok_or("bad longitude")?;
            let lat = a.get(1).and_then(Value::as_f64).ok_or("bad latitude")?;
            let ll = LonLat::new(lon, lat);
            if ll.is_valid() {
                Ok(ll)
            } else {
                Err("position out of range")
            }
        })
        .collect::<Result<_, _>>()?;
    if ring.len() < 3 {
        return Err("ring needs at least 3 positions".into());
    }
    Ok(ring)
}
