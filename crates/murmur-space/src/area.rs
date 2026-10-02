//! Areas of interest: a polygon or a point with a radius.

use crate::geo::{Geodetic, central_angle};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Area {
    /// Ring of [lon, lat] (GeoJSON order), not closed.
    Polygon {
        ring: Vec<[f64; 2]>,
    },
    Circle {
        center: [f64; 2],
        radius_km: f64,
    },
}

impl Area {
    pub fn point(lat: f64, lon: f64, radius_km: f64) -> Self {
        Area::Circle {
            center: [lon, lat],
            radius_km,
        }
    }

    /// Target point used for pointing: polygon vertex mean (fine for areas up to ~100 km).
    pub fn centroid(&self) -> Geodetic {
        match self {
            Area::Circle { center, .. } => Geodetic::new(center[1], center[0], 0.0),
            Area::Polygon { ring } => {
                let n = ring.len().max(1) as f64;
                let lon = ring.iter().map(|p| p[0]).sum::<f64>() / n;
                let lat = ring.iter().map(|p| p[1]).sum::<f64>() / n;
                Geodetic::new(lat, lon, 0.0)
            }
        }
    }

    /// Largest distance from the centroid to the area's edge, km.
    pub fn radius_km(&self) -> f64 {
        match self {
            Area::Circle { radius_km, .. } => *radius_km,
            Area::Polygon { ring } => {
                let c = self.centroid();
                ring.iter()
                    .map(|p| {
                        central_angle(&c, &Geodetic::new(p[1], p[0], 0.0))
                            * crate::geo::EARTH_MEAN_RADIUS_KM
                    })
                    .fold(0.0, f64::max)
            }
        }
    }

    /// Ring for display ([lon, lat]), circles approximated with 48 points.
    pub fn ring(&self) -> Vec<[f64; 2]> {
        match self {
            Area::Polygon { ring } => ring.clone(),
            Area::Circle { radius_km, .. } => {
                let c = self.centroid();
                (0..48)
                    .map(|i| {
                        let p = c.destination(f64::from(i) * 7.5, *radius_km);
                        [p.lon_deg, p.lat_deg]
                    })
                    .collect()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaEntry {
    title: Option<String>,
    polygon: Option<Vec<[f64; 2]>>,
    point: Option<[f64; 2]>,
    radius_km: Option<f64>,
    utc_offset_h: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedArea {
    pub name: String,
    pub title: Option<String>,
    pub area: Area,
    /// Display offset for local times (hours from UTC).
    pub utc_offset_h: f64,
}

#[derive(Deserialize)]
struct AreasFile {
    area: BTreeMap<String, AreaEntry>,
}

pub fn load_areas(path: &Path) -> Result<Vec<NamedArea>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_areas(&text)
}

pub fn parse_areas(text: &str) -> Result<Vec<NamedArea>, String> {
    let f: AreasFile = toml::from_str(text).map_err(|e| e.to_string())?;
    f.area
        .into_iter()
        .map(|(name, e)| {
            let area = match (e.polygon, e.point) {
                (Some(ring), None) if ring.len() >= 3 => Area::Polygon { ring },
                (None, Some(p)) => Area::Circle {
                    center: p,
                    radius_km: e.radius_km.unwrap_or(1.0),
                },
                _ => {
                    return Err(format!(
                        "area {name}: needs a polygon (>=3 points) or a point"
                    ));
                }
            };
            Ok(NamedArea {
                name,
                title: e.title,
                area,
                utc_offset_h: e.utc_offset_h.unwrap_or(0.0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_gate_area() {
        let areas = parse_areas(include_str!("../../../sim/space/areas.toml")).expect("parse");
        let gg = areas.iter().find(|a| a.name == "golden-gate").expect("gg");
        let c = gg.area.centroid();
        assert!((c.lat_deg - 37.825).abs() < 1e-6 && (c.lon_deg + 122.47).abs() < 1e-6);
        let r = gg.area.radius_km();
        assert!(r > 9.0 && r < 11.0, "{r}");
    }
}
