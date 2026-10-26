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
//! A sensor measures from where its vehicle is and the vehicle lays the fix off from where it
//! believes it is, so every fix but AIS's carries the vehicle's navigation error
//! ([`Observer::geolocate`]: a GNSS spoofer's walk, a dead reckoning's drift; none outside the
//! headless simulator, which alone knows a vehicle's truth).
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
//!
//! This is the `legacy` model, chosen per contacts scenario (`[sensing] model`) or by
//! `MURMUR_SENSE_MODEL`. The `physical` model (`physical/`, docs/SENSING.md), the default,
//! shares the types here (a catalog sensor's physical fields, an observer's heading and task
//! area, a detection's `look`), which this model leaves unread and empty.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::physical::geometry::M_PER_DEG;
use super::physical::spec::PhysicalFields;
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
    /// A radar that measures elevation too (3D: a fire-control or air-surveillance radar):
    /// it gives an aircraft's height. A navigation or surface-search radar (2D, the default)
    /// gives none.
    #[serde(default)]
    pub height: bool,
    /// The physical model's figures (`physical::spec`); none: placeholders from `range_m`.
    #[serde(flatten)]
    pub physical: PhysicalFields,
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
            for s in &sensors {
                super::physical::spec::validate(s).map_err(|e| format!("platforms.toml [{id}] {}: {e}", s.kind.as_str()))?;
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

/// One of our vehicles as a sensor carrier: where it is, what it carries, and how far its own
/// navigation is off (murmur-k8i). Its sensors see from where it is (`lat`, `lon`: range,
/// horizon, terrain and the water are the truth's); it places what they measure relative to
/// itself from where it believes it is (`believed`), so its fixes carry its navigation error, a
/// GNSS spoofer's walk or a dead reckoning's drift (`geolocate`), and its payload manager points
/// at a world position from there too. Only the headless simulator knows a vehicle's truth (the
/// stand-in's own navigation filter, crates/murmur-standins/src/nav.rs); elsewhere `lat`, `lon`
/// are where it says it is and the error is none.
#[derive(Debug, Clone, Default)]
pub struct Observer {
    pub uid: u64,
    pub lat: f64,
    pub lon: f64,
    /// Where the vehicle believes it is less where it is, metres east and north (its estimate's
    /// error: what it reports is this far off its truth).
    pub nav_error_m: (f64, f64),
    pub alt_msl: f64,
    /// height of the sensors above the water or ground, for the horizon
    pub mast_m: f64,
    pub submerged: bool,
    pub sensors: Vec<SensorSpec>,
    /// The physical model's payload manager: where the vehicle heads (degrees true), and the
    /// area its task covers, which its cameras search (none: around itself).
    pub heading_deg: Option<f64>,
    pub area: Option<SearchArea>,
}

impl Observer {
    /// Where the vehicle believes it is (lat, lon): where it is, moved by its navigation error.
    pub fn believed(&self) -> (f64, f64) {
        self.geolocate(self.lat, self.lon)
    }

    /// A position this vehicle measured relative to itself (a radar's plot, a camera's or a
    /// sonar's fix, an ESM bearing's), as it places it: moved by its navigation error, the
    /// measurement being laid off from where it believes it is. Without an error, the position
    /// itself, unchanged.
    pub fn geolocate(&self, lat: f64, lon: f64) -> (f64, f64) {
        let (e, n) = self.nav_error_m;
        if e == 0.0 && n == 0.0 {
            return (lat, lon);
        }
        (lat + n / M_PER_DEG, lon + e / (M_PER_DEG * lat.to_radians().cos()))
    }
}

/// The area a vehicle's task covers (physical model, WORLD-V3-PLAN 2.4): an orbit's circle or
/// a patrol's route, (lat, lon).
#[derive(Debug, Clone, PartialEq)]
pub enum SearchArea {
    Orbit { center: (f64, f64), radius_m: f64 },
    Route { points: Vec<(f64, f64)> },
}

/// How far a look got (physical model, WORLD-V3-PLAN 2.3): a detection gives the contact's
/// domain and size class, a recognition its family, an identification its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Detect,
    Recognise,
    Identify,
}

impl Level {
    /// Its name in the picture (`identity.level`, a look's `level`).
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Detect => "detect",
            Level::Recognise => "recognise",
            Level::Identify => "identify",
        }
    }
}

/// The physical model's evidence in one detection: the level its label is at and how sure
/// this look is of it. The tracker keeps it per observer and sensor, not pooled per sensor
/// type (`model::Track::identity`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub level: Level,
    pub confidence: f64,
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
    /// When the fix was taken, by the clock of the truth it was drawn from (the message's
    /// `t`); None: unknown (the tracker then dates it by its arrival).
    pub measured_t: Option<f64>,
    /// A 2D radar's plot of an aircraft: where the radar was (lat, lon). The radar measured
    /// the slant range and the bearing, not the height, so the plot (`lat`, `lon`) is at the
    /// slant range on the bearing: the aircraft is on that bearing, no farther out.
    pub slant_from: Option<(f64, f64)>,
    /// The physical model's identification evidence for `label`; None from the legacy model.
    pub look: Option<Look>,
    /// A sweeping radar's plot: its revolution, s (the physical model's radars plot a contact
    /// once a scan, as the beam passes, so two radars' plots of it come seconds apart and the
    /// tracker time-aligns them: `model::Track::update`). None: a sensor that reports every tick
    /// (the legacy model's radars, whose pairs the tracker takes as it always has).
    pub scan_s: Option<f64>,
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
pub fn normal(seed: u64, tick: i64, uid: u64, contact: &str, salt: u64) -> f64 {
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
        // the physical model's kinds (no legacy scenario has them)
        "small_drone" => "small_uas",
        "periscope" => "submerged",
        "diver" | "person" => "person",
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
        // the physical model's kinds (no legacy scenario has them)
        "small_drone" => "light_aircraft",
        "periscope" => "submarine",
        "diver" => "uuv",
        "person" => "vehicle",
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
                // a 2D radar measures an aircraft's slant range and bearing, not its height: it
                // plots it at the slant range on the bearing, farther out than it is by the
                // height's share (its error along the range and across it the same as for a
                // contact on the water, whose plot is where it is)
                let slant = s.kind == SensorType::Radar && c.domain == Domain::Air && !s.height;
                let (lat, lon) = if slant {
                    let m_per_lon = 111_320.0 * o.lat.to_radians().cos();
                    let (de, dn) = ((c.lon - o.lon) * m_per_lon, (c.lat - o.lat) * 111_320.0);
                    let bearing = de.atan2(dn) + sigma * ne / r.max(1.0);
                    let range = (r + sigma * nn).max(0.0);
                    (o.lat + range * bearing.cos() / 111_320.0, o.lon + range * bearing.sin() / m_per_lon)
                } else {
                    (c.lat + sigma * nn / 111_320.0, c.lon + sigma * ne / (111_320.0 * c.lat.to_radians().cos()))
                };
                // measured from where the vehicle is, laid off from where it believes it is (its
                // navigation error); AIS is the contact's own GPS position, whatever ours believes
                let (lat, lon) = if s.kind == SensorType::Ais { (lat, lon) } else { o.geolocate(lat, lon) };
                let wrong = s.kind.classifier().is_some() && misclassified(&c.id, s.kind);
                // no range, no altitude: a contact on the water ranged by a radar (or
                // reporting itself over AIS) is at sea level; an aircraft's height comes
                // only from a radar that measures elevation (`height`: a 2D navigation or
                // surface-search radar, as on our USVs, gives none); EO/IR and ESM have a
                // bearing, no range; sonar's fix is bearing-heavy (no depth); a ground
                // contact's 0 m is not its height (no terrain under it here)
                let alt_m = match (s.kind, c.domain) {
                    (SensorType::Radar | SensorType::Ais, Domain::Surface) => Some(c.alt_m),
                    (SensorType::Radar, Domain::Air) if s.height => Some(c.alt_m + 30.0 * ne),
                    _ => None,
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
                    measured_t: None,
                    slant_from: slant.then(|| o.believed()),
                    look: None,
                    scan_s: None,
                });
            }
        }
    }
    out
}

/// Rules 9: how our sensors observed an abstract hostile act: which of them (`"<uid>:<sensor>"`)
/// and where the best of them placed its author, with that sensor's 1-sigma error.
#[derive(Debug, Clone, PartialEq)]
pub struct ActObservation {
    pub sensors: Vec<String>,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    /// Every other observer's own fix (its best sensor), for corroboration.
    pub fixes: Vec<ActFix>,
}

/// One further observer's fix of an act's author.
#[derive(Debug, Clone, PartialEq)]
pub struct ActFix {
    pub observer: u64,
    pub sensors: Vec<String>,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
}

/// One observer's best fix of an act (lat, lon, its 1-sigma, m) and its sensors that saw it.
type ObserverFix = ((f64, f64, f64), Vec<String>);

/// Rules 9: what our sensors make of an abstract hostile act by contact `actor` (no weapon is
/// modelled: the act is an event at the actor's position, now). A sensor that could detect the
/// actor at this moment observes it with the same range, horizon, domain and probability as a
/// detection (one seeded draw per act and sensor, so a run replays): radar, EO/IR, and sonar
/// for a contact on or under the water; not AIS (a broadcast, not an observation) nor ESM (a
/// bearing). Each places the author as its detection would, with that error drawn: a 2D radar
/// observing an aircraft measured only its slant range and bearing, so it places the author at
/// the middle of the bearing segment the slant range bounds, its error the segment's spread, as
/// the tracker reads such a fix (murmur-oho). The observer with the smallest error places the
/// act's author; every other observer gives its own fix. None: nobody saw it (the C2 never
/// learns of it).
pub fn observe_act(p: Params, observers: &[Observer], actor: &ContactTruth, act_id: &str) -> Option<ActObservation> {
    let key = format!("{act_id}/{}", actor.id);
    let mut sensors = Vec::new();
    // each observer's best fix (lat, lon, its error), and its sensors that saw it
    let mut per_observer: std::collections::BTreeMap<u64, ObserverFix> = std::collections::BTreeMap::new();
    for o in observers {
        for (si, s) in o.sensors.iter().enumerate() {
            if !matches!(s.kind, SensorType::Radar | SensorType::EoIr | SensorType::Sonar) {
                continue;
            }
            if o.submerged && s.kind != SensorType::Sonar {
                continue;
            }
            if !s.kind.can_see(actor) {
                continue;
            }
            let horiz = haversine_m(o.lat, o.lon, actor.lat, actor.lon);
            let r = horiz.hypot(actor.alt_m - o.alt_msl);
            if s.kind.needs_horizon() {
                let h_sensor = if o.alt_msl > 20.0 { o.alt_msl } else { o.mast_m };
                if horiz > horizon_m(h_sensor, contact_height_m(actor)) {
                    continue;
                }
            }
            let x = r / (s.range_m * actor.signature.max(0.01));
            let salt = 0xAC70_0000 | (si as u64) << 8 | s.kind as u64;
            let prob = pd(x, s.kind.pd_max());
            if prob <= 0.0 || uniform(p.seed, p.tick, o.uid, &key, salt) >= prob {
                continue;
            }
            sensors.push(format!("{}:{}", o.uid, s.kind.as_str()));
            let sigma = s.kind.sigma_m(r);
            let (ne, nn) = (
                normal(p.seed, p.tick, o.uid, &key, salt | 0x1_0000),
                normal(p.seed, p.tick, o.uid, &key, salt | 0x2_0000),
            );
            let fix = if s.kind == SensorType::Radar && actor.domain == Domain::Air && !s.height {
                // the plot at the measured slant range and bearing, read as the middle of the
                // segment it bounds, give or take its spread (`model::SEGMENT_SIGMAS`, 3)
                let m_per_lon = 111_320.0 * o.lat.to_radians().cos();
                let (de, dn) = ((actor.lon - o.lon) * m_per_lon, (actor.lat - o.lat) * 111_320.0);
                let bearing = de.atan2(dn) + sigma * ne / r.max(1.0);
                let hi = (r + sigma * nn).max(0.0) + 2.0 * sigma;
                (o.lat + 0.5 * hi * bearing.cos() / 111_320.0, o.lon + 0.5 * hi * bearing.sin() / m_per_lon, (hi / 3.0).max(sigma))
            } else {
                (actor.lat + sigma * nn / 111_320.0, actor.lon + sigma * ne / (111_320.0 * actor.lat.to_radians().cos()), sigma)
            };
            // laid off from where the vehicle believes it is, as its detections are
            let fix = {
                let (lat, lon) = o.geolocate(fix.0, fix.1);
                (lat, lon, fix.2)
            };
            let e = per_observer.entry(o.uid).or_insert((fix, Vec::new()));
            if fix.2 < e.0.2 {
                e.0 = fix;
            }
            e.1.push(format!("{}:{}", o.uid, s.kind.as_str()));
        }
    }
    // the observer with the smallest error places the author (the first of equals)
    let (&uid, &((lat, lon, sigma), _)) = per_observer.iter().min_by(|a, b| a.1.0.2.total_cmp(&b.1.0.2))?;
    // every other observer's own fix (its best sensor), each drawn on its own
    let fixes: Vec<ActFix> = per_observer
        .into_iter()
        .filter(|(o, _)| *o != uid)
        .map(|(o, ((lat, lon, sig), ss))| ActFix {
            observer: o,
            sensors: ss,
            lat,
            lon,
            sigma_m: (sig * 10.0).round() / 10.0,
        })
        .collect();
    Some(ActObservation {
        sensors,
        lat,
        lon,
        sigma_m: (sigma * 10.0).round() / 10.0,
        fixes,
    })
}

/// The per-(contact, sensor type) misclassification draw: a property of the simulation
/// (this sensor is wrong about this contact), stable for the run.
pub fn misclassify_draw(seed: u64, contact_id: &str, sensor: SensorType, rate: f64) -> bool {
    rate > 0.0 && uniform(seed ^ 0xC1A5, -1, 0, contact_id, sensor as u64) < rate
}
