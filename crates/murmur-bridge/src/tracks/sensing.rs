//! The sensing model: which of our vehicles detect which contacts, with what error and what
//! they can say about them. Detections come only from our own platforms' sensors (from
//! `sensors = [...]` in `sim/platforms.toml`), against the truth on the simulation channel
//! (`truth.rs`); a contact out of every sensor's reach does not exist for the C2.
//!
//! # Detection
//!
//! For a sensor with nominal range `R` (where a signature-1.0 contact is detected with
//! probability 0.5) and a contact of signature `s` at range `r`:
//!
//! ```text
//! x = r / (R * s)        pd(x) = pd_max / (1 + x^6)   for x <= 1.5, else 0
//! ```
//!
//! pd_max is 0.95 for radar and EO/IR, 0.9 for sonar and ESM, 0.98 for an AIS receiver
//! (reception, not search). Radar, EO/IR, AIS and ESM are also limited by the radio
//! horizon, `4.12 km x (sqrt(h_sensor) + sqrt(h_contact))` (heights in metres), which is
//! what keeps a boat's mast-high radar from seeing a small craft 20 km away however good it
//! is on paper. Sonar is the only sensor that hears a submerged contact (and it hears
//! surface ships too); AIS hears only cooperative ships broadcasting; ESM hears only
//! contacts that emit (navigation radar, transponder). Terrain masking is not modelled.
//!
//! # Measurement
//!
//! Position error (1 sigma, metres): radar 30 + 1% of range; EO/IR 15 + 0.5%; AIS 10 (the
//! ship's own GPS); sonar 50 + 10% (bearing-heavy); ESM bearing only, 5 degrees. The
//! noise is Gaussian, drawn deterministically from (seed, tick, vehicle, contact, sensor).
//!
//! # What a sensor can say
//!
//! EO/IR and sonar classify the kind (`ferry`, `small_fast_boat`, `submarine`...), slowly:
//! confidence grows with dwell (`1 - exp(-dwell/tau)`, tau 45 s for EO/IR, 120 s for
//! sonar). Radar gives a size class only (`large_vessel`, `small_craft`, `aircraft`,
//! `small_uas`, `ground_vehicle`, at most 0.5). ESM gives the emitter class. AIS gives an
//! identity at once (MMSI, name, ship type) at 0.85: cooperative, and spoofable. A
//! classifier is wrong with the scenario's `misclassification_rate`, decided once per
//! (track, sensor type) so a wrong call is persistent, as a real misidentification is.
//!
//! Figures are "-class" ballpark values, marked `guess` in the catalog; the point is the
//! structure (range, horizon, dwell, independence, cooperative identity), not the numbers.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::truth::{ContactTruth, Domain};
use crate::geo::haversine_m;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorType {
    Radar,
    EoIr,
    Ais,
    Sonar,
    Esm,
}

impl SensorType {
    pub fn as_str(self) -> &'static str {
        match self {
            SensorType::Radar => "radar",
            SensorType::EoIr => "eo_ir",
            SensorType::Ais => "ais",
            SensorType::Sonar => "sonar",
            SensorType::Esm => "esm",
        }
    }

    fn pd_max(self) -> f64 {
        match self {
            SensorType::Radar | SensorType::EoIr => 0.95,
            SensorType::Sonar | SensorType::Esm => 0.9,
            SensorType::Ais => 0.98,
        }
    }

    /// Bound by the radio/optical horizon.
    fn needs_horizon(self) -> bool {
        self != SensorType::Sonar
    }

    /// Can it detect this contact at all (domain, cooperation, emission)?
    pub fn can_see(self, c: &ContactTruth) -> bool {
        match self {
            SensorType::Sonar => matches!(c.domain, Domain::Subsurface | Domain::Surface),
            SensorType::Ais => c.domain == Domain::Surface && c.ais.is_some(),
            SensorType::Esm => c.emits && c.domain != Domain::Subsurface,
            SensorType::Radar | SensorType::EoIr => c.domain != Domain::Subsurface,
        }
    }

    /// 1-sigma position error at range `r`.
    pub fn sigma_m(self, r: f64) -> f64 {
        match self {
            SensorType::Radar => 30.0 + 0.01 * r,
            SensorType::EoIr => 15.0 + 0.005 * r,
            SensorType::Ais => 10.0,
            SensorType::Sonar => 50.0 + 0.10 * r,
            SensorType::Esm => (r * 5f64.to_radians().tan()).max(200.0),
        }
    }

    /// (maximum classification confidence, dwell time constant in seconds), or None for a
    /// sensor that only locates.
    pub fn classifier(self) -> Option<(f64, f64)> {
        match self {
            SensorType::EoIr => Some((0.95, 45.0)),
            SensorType::Sonar => Some((0.8, 120.0)),
            SensorType::Radar => Some((0.5, 90.0)),
            SensorType::Esm => Some((0.6, 60.0)),
            SensorType::Ais => None,
        }
    }
}

/// One sensor on a platform, from the catalog.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SensorSpec {
    #[serde(rename = "type")]
    pub kind: SensorType,
    pub range_m: f64,
    #[serde(default)]
    pub guess: bool,
}

/// Sensors and mast height per platform id.
#[derive(Debug, Clone, Default)]
pub struct SensorCatalog {
    pub platforms: HashMap<String, PlatformSensors>,
}

#[derive(Debug, Clone, Default)]
pub struct PlatformSensors {
    pub sensors: Vec<SensorSpec>,
    pub mast_m: f64,
}

const DEFAULT_MAST_M: f64 = 3.0;

impl SensorCatalog {
    pub fn from_toml(text: &str) -> Result<SensorCatalog, String> {
        let table: toml::Table = text.parse().map_err(|e| format!("platforms.toml: {e}"))?;
        let mut platforms = HashMap::new();
        for (id, spec) in table {
            let Some(spec) = spec.as_table() else { continue };
            let sensors = match spec.get("sensors") {
                Some(v) => v
                    .clone()
                    .try_into::<Vec<SensorSpec>>()
                    .map_err(|e| format!("platforms.toml [{id}] sensors: {e}"))?,
                None => Vec::new(),
            };
            if let Some(s) = sensors
                .iter()
                .find(|s| s.range_m <= 0.0 || !s.range_m.is_finite())
            {
                return Err(format!("platforms.toml [{id}]: {:?} range_m must be > 0", s.kind));
            }
            let mast_m = spec
                .get("mast_m")
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                .unwrap_or(DEFAULT_MAST_M);
            platforms.insert(id, PlatformSensors { sensors, mast_m });
        }
        Ok(SensorCatalog { platforms })
    }

    pub fn load(path: &Path) -> Result<SensorCatalog, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text)
    }

    pub fn get(&self, platform: &str) -> Option<&PlatformSensors> {
        self.platforms.get(platform)
    }
}

/// One of our vehicles as a sensor carrier: where it says it is (the sensor measures
/// relative to its own navigation, so the track inherits that error) and what it carries.
#[derive(Debug, Clone)]
pub struct Observer {
    pub uid: u64,
    pub lat: f64,
    pub lon: f64,
    pub alt_msl: f64,
    /// height of the sensors above the water or ground, for the horizon
    pub mast_m: f64,
    pub submerged: bool,
    pub sensors: Vec<SensorSpec>,
}

/// What one sensor reported about one contact on one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub contact_id: String,
    pub uid: u64,
    pub sensor: SensorType,
    /// measured position (truth plus noise) and its 1-sigma error
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    pub sigma_m: f64,
    pub range_m: f64,
    /// course and speed over ground as the contact reports them (AIS only), east, north
    pub velocity_mps: Option<(f64, f64)>,
    /// what the sensor calls it (see the module doc), None for AIS
    pub label: Option<String>,
    /// the AIS broadcast as heard
    pub ais: Option<AisHeard>,
    pub domain: Domain,
}

/// The AIS message as received: identity only, nothing about its truthfulness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AisHeard {
    pub mmsi: String,
    pub name: String,
    pub ship_type: String,
}

// ------------------------------------------------------------------ deterministic draws

/// A stable 64-bit hash of the draw's coordinates (seed, tick, vehicle, contact, salt).
fn hash64(seed: u64, tick: i64, uid: u64, contact: &str, salt: u64) -> u64 {
    // FNV-1a over the fields, then splitmix64 to spread the bits
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    for b in seed
        .to_le_bytes()
        .iter()
        .chain(tick.to_le_bytes().iter())
        .chain(uid.to_le_bytes().iter())
        .chain(salt.to_le_bytes().iter())
    {
        eat(*b);
    }
    for b in contact.bytes() {
        eat(b);
    }
    eat(0xff);
    let mut z = h.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform in [0, 1).
pub fn uniform(seed: u64, tick: i64, uid: u64, contact: &str, salt: u64) -> f64 {
    (hash64(seed, tick, uid, contact, salt) >> 11) as f64 / (1u64 << 53) as f64
}

/// Standard normal (Box-Muller from two uniforms).
fn normal(seed: u64, tick: i64, uid: u64, contact: &str, salt: u64) -> f64 {
    let u1 = uniform(seed, tick, uid, contact, salt).max(1e-12);
    let u2 = uniform(seed, tick, uid, contact, salt ^ 0x5555);
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

// ------------------------------------------------------------------ the model

/// Detection probability for `x = r / (R * signature)`.
pub fn pd(x: f64, pd_max: f64) -> f64 {
    if x > 1.5 || !x.is_finite() {
        0.0
    } else {
        pd_max / (1.0 + x.powi(6))
    }
}

/// Radio/optical horizon between two heights, metres (4.12 km per sqrt(m), the standard
/// 4/3-earth figure).
pub fn horizon_m(h1_m: f64, h2_m: f64) -> f64 {
    4120.0 * (h1_m.max(1.0).sqrt() + h2_m.max(1.0).sqrt())
}

/// Height a contact presents to the horizon calculation.
fn contact_height_m(c: &ContactTruth) -> f64 {
    match c.domain {
        Domain::Air => c.alt_m.max(1.0),
        Domain::Surface => match c.kind.as_str() {
            "cargo_ship" | "tanker" => 25.0,
            "ferry" | "patrol_boat" => 10.0,
            "fishing_vessel" | "sailboat" => 5.0,
            _ => 2.0,
        },
        Domain::Ground => 2.0,
        Domain::Subsurface => 0.0,
    }
}

/// Radar size class / ESM emitter class for a kind.
pub fn family(kind: &str) -> &'static str {
    match kind {
        "cargo_ship" | "tanker" | "ferry" | "patrol_boat" | "large_vessel" => "large_vessel",
        "fishing_vessel" | "sailboat" | "small_fast_boat" | "small_craft" => "small_craft",
        "light_aircraft" | "helicopter" | "aircraft" => "aircraft",
        "unknown_uas" | "small_uas" => "small_uas",
        "submarine" | "uuv" | "submerged" => "submerged",
        "vehicle" | "truck" | "ground_vehicle" => "ground_vehicle",
        _ => "unknown",
    }
}

/// The wrong call a misclassifying sensor makes for a kind: the plausible confusion.
pub fn confusion(kind: &str) -> &'static str {
    match kind {
        "small_fast_boat" => "fishing_vessel",
        "fishing_vessel" => "small_fast_boat",
        "sailboat" => "fishing_vessel",
        "ferry" => "cargo_ship",
        "cargo_ship" | "tanker" => "ferry",
        "patrol_boat" => "fishing_vessel",
        "light_aircraft" => "unknown_uas",
        "helicopter" => "light_aircraft",
        "unknown_uas" => "light_aircraft",
        "submarine" => "uuv",
        "uuv" => "submarine",
        "truck" => "vehicle",
        _ => "vehicle",
    }
}

/// What `sensor` calls contact `c`. `wrong` injects the misclassification.
fn label_for(sensor: SensorType, c: &ContactTruth, wrong: bool) -> Option<String> {
    let kind = if wrong { confusion(&c.kind) } else { c.kind.as_str() };
    match sensor {
        SensorType::EoIr | SensorType::Sonar => Some(kind.to_string()),
        SensorType::Radar => Some(family(kind).to_string()),
        SensorType::Esm => Some(
            match c.domain {
                Domain::Air => "aircraft",
                Domain::Surface => "vessel",
                Domain::Ground => "ground_vehicle",
                Domain::Subsurface => "submerged",
            }
            .to_string(),
        ),
        SensorType::Ais => None,
    }
}

/// Settings of one sensing step.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub seed: u64,
    /// the truth message's scenario second: the tick index of every draw
    pub tick: i64,
    pub misclassification_rate: f64,
}

/// Run every (observer, sensor, contact) pair for one tick. `misclassified(track_key,
/// sensor)` says whether this sensor type is wrong about this contact (decided per track,
/// so the caller keeps it stable for a track's life).
pub fn detect(
    p: Params,
    observers: &[Observer],
    contacts: &[ContactTruth],
    misclassified: &dyn Fn(&str, SensorType) -> bool,
) -> Vec<Detection> {
    let mut out = Vec::new();
    for o in observers {
        for (si, s) in o.sensors.iter().enumerate() {
            if o.submerged && s.kind != SensorType::Sonar {
                continue; // nothing but sonar works under water
            }
            for c in contacts {
                if !s.kind.can_see(c) {
                    continue;
                }
                let horiz = haversine_m(o.lat, o.lon, c.lat, c.lon);
                let dz = c.alt_m - o.alt_msl;
                let r = horiz.hypot(dz);
                if s.kind.needs_horizon() {
                    let h_sensor = if o.alt_msl > 20.0 { o.alt_msl } else { o.mast_m };
                    if horiz > horizon_m(h_sensor, contact_height_m(c)) {
                        continue;
                    }
                }
                let x = r / (s.range_m * c.signature.max(0.01));
                let salt = (si as u64) << 8 | s.kind as u64;
                let prob = pd(x, s.kind.pd_max());
                if prob <= 0.0 || uniform(p.seed, p.tick, o.uid, &c.id, salt) >= prob {
                    continue;
                }
                let sigma = s.kind.sigma_m(r);
                let (ne, nn) = (
                    normal(p.seed, p.tick, o.uid, &c.id, salt | 0x1_0000),
                    normal(p.seed, p.tick, o.uid, &c.id, salt | 0x2_0000),
                );
                let lat = c.lat + sigma * nn / 111_320.0;
                let lon = c.lon + sigma * ne / (111_320.0 * c.lat.to_radians().cos());
                let wrong = s.kind.classifier().is_some() && misclassified(&c.id, s.kind);
                // an altitude only where the sensor ranges the contact: a passive sensor
                // (ESM; EO/IR on an air contact) has a bearing and an elevation angle, no
                // range, so no altitude (the decision rules count a track's altitude for a 3D
                // keep-out only when a ranging source backs it)
                let alt_m = match (s.kind, c.domain) {
                    (SensorType::Esm, _) | (SensorType::EoIr, Domain::Air) => None,
                    (_, Domain::Subsurface) => Some(c.alt_m + 5.0 * nn),
                    (SensorType::Radar, Domain::Air) => Some(c.alt_m + 30.0 * ne),
                    _ => Some(c.alt_m),
                };
                out.push(Detection {
                    contact_id: c.id.clone(),
                    uid: o.uid,
                    sensor: s.kind,
                    lat,
                    lon,
                    alt_m,
                    sigma_m: sigma,
                    range_m: r,
                    velocity_mps: (s.kind == SensorType::Ais).then(|| {
                        let a = c.course_deg.to_radians();
                        let v = c.speed_mps
                            + 0.2 * normal(p.seed, p.tick, o.uid, &c.id, salt | 0x3_0000);
                        (v * a.sin(), v * a.cos())
                    }),
                    label: label_for(s.kind, c, wrong),
                    ais: if s.kind == SensorType::Ais {
                        c.ais.as_ref().map(|a| AisHeard {
                            mmsi: a.mmsi.clone(),
                            name: a.name.clone(),
                            ship_type: a.ship_type.clone(),
                        })
                    } else {
                        None
                    },
                    domain: c.domain,
                });
            }
        }
    }
    out
}

/// The per-(contact, sensor type) misclassification draw: a property of the simulation
/// (this sensor is wrong about this contact), stable for the run.
pub fn misclassify_draw(seed: u64, contact_id: &str, sensor: SensorType, rate: f64) -> bool {
    rate > 0.0 && uniform(seed ^ 0xC1A5, -1, 0, contact_id, sensor as u64) < rate
}
