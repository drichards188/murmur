//! The constellation catalog (`sim/space/constellations.toml`): "-class" entries with
//! approximate public figures, each pointing at public element sets on CelesTrak.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Electro-optical imaging (needs daylight).
    Optical,
    /// Synthetic aperture radar imaging (day/night, through cloud).
    Sar,
    /// Communications relay for a ground, sea or air terminal.
    Satcom,
    /// Navigation signals (visible count and DOP).
    Gnss,
    /// Not a capability: objects used to verify predictions (ISS).
    Reference,
}

impl Capability {
    pub fn is_imaging(self) -> bool {
        matches!(self, Capability::Optical | Capability::Sar)
    }
}

impl std::str::FromStr for Capability {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "optical" | "eo" => Ok(Capability::Optical),
            "sar" | "radar" => Ok(Capability::Sar),
            "satcom" | "comms" => Ok(Capability::Satcom),
            "gnss" | "pnt" => Ok(Capability::Gnss),
            "reference" => Ok(Capability::Reference),
            other => Err(format!(
                "unknown capability {other:?} (optical, sar, satcom, gnss)"
            )),
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Capability::Optical => "optical",
            Capability::Sar => "sar",
            Capability::Satcom => "satcom",
            Capability::Gnss => "gnss",
            Capability::Reference => "reference",
        };
        f.write_str(s)
    }
}

/// Imaging sensor model. Every figure is optional: unknown stays blank.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sensor {
    /// Optical: ground sample distance at nadir, metres (best band, usually panchromatic).
    pub gsd_nadir_m: Option<f64>,
    /// SAR: best-mode resolution, metres (slant plane; ground range degrades with incidence).
    pub resolution_m: Option<f64>,
    /// Swath (or scene width) at nadir, km.
    pub swath_km: Option<f64>,
    /// Largest off-nadir (look) angle the class is tasked at.
    pub max_off_nadir_deg: Option<f64>,
    /// Optical: minimum sun elevation at the target.
    pub min_sun_elevation_deg: Option<f64>,
    /// SAR: usable incidence range at the target.
    pub incidence_min_deg: Option<f64>,
    pub incidence_max_deg: Option<f64>,
    /// False for sensors that only image along a fixed nadir strip (e.g. Dove-class): they
    /// cannot be tasked, but their routine collects still count as access.
    #[serde(default = "yes")]
    pub taskable: bool,
}

fn yes() -> bool {
    true
}

/// Communications link model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comms {
    /// Terminal minimum elevation, degrees.
    pub min_elevation_deg: f64,
    /// Lower masks for satellites flying below an altitude (first match wins, by satellite
    /// mean altitude), e.g. Starlink's 2026 FCC approval.
    #[serde(default)]
    pub min_elevation_steps: Vec<ElevationStep>,
    /// Extra one-way latency beyond the space segment's speed of light (gateway, routing,
    /// processing), ms.
    pub network_latency_ms: Option<f64>,
    /// "bent-pipe" (user -> satellite -> nearby gateway) or "crosslink" (inter-satellite links).
    pub path: Option<String>,
    pub band: Option<String>,
    /// Typical user-terminal rates, Mbit/s (for the link emulator's bandwidth model).
    pub user_down_mbps: Option<f64>,
    pub user_up_mbps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElevationStep {
    pub below_alt_km: f64,
    pub min_elevation_deg: f64,
}

impl Comms {
    /// Terminal mask for a satellite at a given mean altitude.
    pub fn min_elevation_for_alt(&self, alt_km: f64) -> f64 {
        let mut steps: Vec<&ElevationStep> = self.min_elevation_steps.iter().collect();
        steps.sort_by(|a, b| a.below_alt_km.total_cmp(&b.below_alt_km));
        steps
            .into_iter()
            .find(|s| alt_km < s.below_alt_km)
            .map_or(self.min_elevation_deg, |s| s.min_elevation_deg)
    }
}

/// Request-to-product timing. Values are typical, not guarantees.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    /// Minimum time from accepted request to the earliest possible collect, minutes.
    pub tasking_lead_min: Option<f64>,
    /// Collect to data on the ground, minutes.
    pub downlink_latency_min: Option<f64>,
    /// Data on the ground to a usable product, minutes.
    pub processing_min: Option<f64>,
}

/// Per-satellite overrides (matched by object-name prefix), for classes whose members differ.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatOverride {
    pub name_prefix: String,
    pub label: Option<String>,
    pub gsd_nadir_m: Option<f64>,
    pub resolution_m: Option<f64>,
    pub swath_km: Option<f64>,
    pub max_off_nadir_deg: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatClass {
    pub id: String,
    pub name: String,
    pub capability: Capability,
    pub role: String,
    /// Free-text orbit summary for display.
    pub orbit: Option<String>,
    /// CelesTrak GP queries, e.g. "GROUP=starlink" or "NAME=WORLDVIEW".
    pub celestrak: Vec<String>,
    /// Keep only objects whose name starts with one of these (empty keeps all).
    #[serde(default)]
    pub include: Vec<String>,
    /// Drop objects whose name contains one of these (added to the catalog-wide list).
    #[serde(default)]
    pub exclude: Vec<String>,
    pub sensor: Option<Sensor>,
    pub comms: Option<Comms>,
    pub timing: Option<Timing>,
    #[serde(default)]
    pub overrides: Vec<SatOverride>,
    /// Names of fields whose values are our estimates rather than published figures.
    #[serde(default)]
    pub guessed: Vec<String>,
    /// Where the figures come from.
    #[serde(default)]
    pub sources: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogDefaults {
    /// Satellites whose element set is older than this at the propagation time are skipped.
    pub max_element_age_days: f64,
    /// Name substrings dropped from every class (debris, rocket bodies).
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub defaults: CatalogDefaults,
    #[serde(rename = "class")]
    pub classes: Vec<SatClass>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(String, toml::de::Error),
    #[error("catalog: {0}")]
    Invalid(String),
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Catalog, CatalogError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CatalogError::Io(path.display().to_string(), e))?;
        Catalog::parse(&text).map_err(|e| match e {
            CatalogError::Parse(_, err) => CatalogError::Parse(path.display().to_string(), err),
            other => other,
        })
    }

    pub fn parse(text: &str) -> Result<Catalog, CatalogError> {
        let cat: Catalog =
            toml::from_str(text).map_err(|e| CatalogError::Parse("catalog".into(), e))?;
        let mut seen = std::collections::HashSet::new();
        for c in &cat.classes {
            if !seen.insert(c.id.clone()) {
                return Err(CatalogError::Invalid(format!(
                    "duplicate class id {}",
                    c.id
                )));
            }
            match c.capability {
                Capability::Optical | Capability::Sar if c.sensor.is_none() => {
                    return Err(CatalogError::Invalid(format!(
                        "{}: imaging class needs [class.sensor]",
                        c.id
                    )));
                }
                Capability::Satcom | Capability::Gnss | Capability::Reference
                    if c.comms.is_none() =>
                {
                    return Err(CatalogError::Invalid(format!(
                        "{}: needs [class.comms] (min elevation)",
                        c.id
                    )));
                }
                _ => {}
            }
        }
        Ok(cat)
    }

    pub fn class(&self, id: &str) -> Option<&SatClass> {
        self.classes.iter().find(|c| c.id == id)
    }

    /// All distinct CelesTrak queries, in catalog order.
    pub fn queries(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.classes {
            for q in &c.celestrak {
                if !out.contains(q) {
                    out.push(q.clone());
                }
            }
        }
        out
    }

    /// Does an object name belong to a class (include prefixes, exclude substrings)?
    pub fn accepts(&self, class: &SatClass, object_name: &str) -> bool {
        let upper = object_name.to_ascii_uppercase();
        if self
            .defaults
            .exclude
            .iter()
            .chain(class.exclude.iter())
            .any(|x| upper.contains(&x.to_ascii_uppercase()))
        {
            return false;
        }
        class.include.is_empty()
            || class
                .include
                .iter()
                .any(|p| upper.starts_with(&p.to_ascii_uppercase()))
    }
}

/// Effective sensor figures for one satellite after applying overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectiveSensor {
    pub label: Option<String>,
    pub gsd_nadir_m: Option<f64>,
    pub resolution_m: Option<f64>,
    pub swath_km: Option<f64>,
    pub max_off_nadir_deg: Option<f64>,
    pub min_sun_elevation_deg: Option<f64>,
    pub incidence_min_deg: Option<f64>,
    pub incidence_max_deg: Option<f64>,
    pub taskable: bool,
}

impl SatClass {
    pub fn effective_sensor(&self, object_name: &str) -> Option<EffectiveSensor> {
        let s = self.sensor.as_ref()?;
        let mut e = EffectiveSensor {
            label: None,
            gsd_nadir_m: s.gsd_nadir_m,
            resolution_m: s.resolution_m,
            swath_km: s.swath_km,
            max_off_nadir_deg: s.max_off_nadir_deg,
            min_sun_elevation_deg: s.min_sun_elevation_deg,
            incidence_min_deg: s.incidence_min_deg,
            incidence_max_deg: s.incidence_max_deg,
            taskable: s.taskable,
        };
        let upper = object_name.to_ascii_uppercase();
        if let Some(o) = self
            .overrides
            .iter()
            .find(|o| upper.starts_with(&o.name_prefix.to_ascii_uppercase()))
        {
            e.label = o.label.clone();
            e.gsd_nadir_m = o.gsd_nadir_m.or(e.gsd_nadir_m);
            e.resolution_m = o.resolution_m.or(e.resolution_m);
            e.swath_km = o.swath_km.or(e.swath_km);
            e.max_off_nadir_deg = o.max_off_nadir_deg.or(e.max_off_nadir_deg);
        }
        Some(e)
    }

    pub fn is_guessed(&self, field: &str) -> bool {
        self.guessed.iter().any(|g| g == field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_catalog_parses() {
        let text = include_str!("../../../sim/space/constellations.toml");
        let cat = Catalog::parse(text).expect("catalog parses");
        for id in [
            "starlink",
            "iridium",
            "wgs",
            "worldview",
            "skysat",
            "dove",
            "capella",
            "iceye",
            "gps",
        ] {
            assert!(cat.class(id).is_some(), "missing class {id}");
        }
        let wv = cat.class("worldview").expect("wv");
        assert!(!cat.accepts(wv, "WORLDVIEW-2 DEB"));
        assert!(cat.accepts(wv, "WORLDVIEW-3 (WV-3)"));
        let sky = cat.class("skysat").expect("skysat");
        assert!(cat.accepts(sky, "SKYSAT-C12"));
        assert!(!cat.accepts(sky, "FLOCK 4Y-12"));
        // every guessed field name refers to something real in the class
        let known = [
            "gsd_nadir_m",
            "resolution_m",
            "swath_km",
            "max_off_nadir_deg",
            "min_sun_elevation_deg",
            "incidence_min_deg",
            "incidence_max_deg",
            "min_elevation_deg",
            "network_latency_ms",
            "user_down_mbps",
            "user_up_mbps",
            "tasking_lead_min",
            "downlink_latency_min",
            "processing_min",
        ];
        for c in &cat.classes {
            for g in &c.guessed {
                assert!(
                    known.contains(&g.as_str()),
                    "{}: unknown guessed field {g}",
                    c.id
                );
            }
        }
    }
}
