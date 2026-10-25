//! A catalog sensor as the physical model reads it (WORLD-V3-PLAN 2.3-2.6): the fields a
//! `sensors = [...]` entry of `sim/platforms.toml` may carry besides `type` and `range_m`, and
//! the sensor they resolve to. A sensor without them runs on placeholders derived from its
//! `range_m` (marked `placeholder`, listed by [`placeholders`]) until the platform catalog's
//! real figures replace them (WORLD-V3-PLAN section 3).
//!
//! ```toml
//! { type = "eo_ir", range_m = 4000, slew_dps = 90, channels = [
//!     { band = "day", wide_deg = 56, narrow_deg = 1.2, px = 4472, blur_urad = 25 },
//!     { band = "lwir", wide_deg = 40, narrow_deg = 5.3, px = 640 } ] }
//! { type = "radar", range_m = 12000, r50_m = 12450, rcs_ref_m2 = 7.5, scan_s = 2.5, beam_deg = 1.2, cell_m = 37 }
//! { type = "sonar", range_m = 3000, mode = "passive", freq_khz = 1, ranges = { cargo_ship = 20000, submarine = 1000 } }
//! { type = "sonar", range_m = 10000, mode = "active", freq_khz = 10, ranges = { submarine = 7500 },
//!   passive = { ranges = { submarine = 1000 } } }
//! { type = "esm", range_m = 300000, s_min_dbm = -80, bearing_deg = 2 }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::tracks::sensing::{SensorSpec, SensorType};

/// The longest radar revisit the physical model accepts, s (decision W2: it keeps the 10 s
/// out-of-view rule sound).
pub const MAX_RADAR_REVISIT_S: f64 = 5.0;

/// An EO/IR channel's waveband.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// A visible camera: daylight only.
    Day,
    /// A low-light camera: about 0.4 of its daylight range at night (research [E] 0.3-0.5).
    Lowlight,
    Mwir,
    Lwir,
}

impl Band {
    pub fn thermal(self) -> bool {
        matches!(self, Band::Mwir | Band::Lwir)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Band::Day => "day",
            Band::Lowlight => "lowlight",
            Band::Mwir => "mwir",
            Band::Lwir => "lwir",
        }
    }
}

/// One EO/IR channel as the catalog gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub band: Band,
    /// Horizontal field of view at its widest and narrowest, degrees (no zoom: the same).
    pub wide_deg: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrow_deg: Option<f64>,
    /// Horizontal pixels.
    pub px: f64,
    /// The blur floor (stabilisation and optics), microradians: the effective IFOV is never
    /// finer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur_urad: Option<f64>,
    /// A ring of fixed cameras covering every bearing, each `wide_deg` wide: no pointing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub panoramic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SonarMode {
    Passive,
    Active,
}

/// The physical model's fields of a catalog sensor (all optional; flattened into the entry).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhysicalFields {
    /// EO/IR channels, co-boresighted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<Channel>,
    /// EO/IR gimbal slew rate, degrees per second; 0: a fixed camera looking ahead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slew_dps: Option<f64>,
    /// Radar: the range at which a target of `rcs_ref_m2` is detected half the time on a scan
    /// (Swerling 1, Pfa 1e-4), m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r50_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcs_ref_m2: Option<f64>,
    /// Radar or scanning sonar: one revolution, s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_s: Option<f64>,
    /// Radar azimuth beamwidth, degrees, and range cell, m (sea clutter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beam_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_m: Option<f64>,
    /// Radar frequency, GHz (the two-ray model's wavelength).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band_ghz: Option<f64>,
    /// Sonar: passive or active, its frequency, its reference ranges (Pd 0.5 in the reference
    /// environment: 50 m of water, 5 m/s wind, moderate shipping) by contact kind, and its
    /// instrumented range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SonarMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_khz: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ranges: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_m: Option<f64>,
    /// Sonar: an active sonar whose array also listens, its passive mode (a hull sonar that is
    /// "active and passive"): its own reference ranges by kind, and its band and instrumented
    /// range where they are not the active mode's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passive: Option<PassiveFields>,
    /// ESM: the least received power it detects, dBm, and its bearing error, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s_min_dbm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearing_deg: Option<f64>,
}

/// An active sonar's passive mode as the catalog gives it (`passive = { ... }` on the entry).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassiveFields {
    /// Its band, kHz; none: the active mode's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_khz: Option<f64>,
    /// Its reference ranges by kind (Pd 0.5 in the reference environment), m.
    pub ranges: BTreeMap<String, f64>,
    /// Its instrumented range, m; none: the active mode's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_m: Option<f64>,
}

/// An EO/IR channel resolved: angles in radians.
#[derive(Debug, Clone, PartialEq)]
pub struct EoChannel {
    pub band: Band,
    pub wide_rad: f64,
    pub narrow_rad: f64,
    pub px: f64,
    pub blur_rad: f64,
    pub panoramic: bool,
}

impl EoChannel {
    /// The effective instantaneous field of view in the wide or the narrow field, rad.
    pub fn ifov(&self, narrow: bool) -> f64 {
        let fov = if narrow { self.narrow_rad } else { self.wide_rad };
        (fov / self.px).max(self.blur_rad)
    }

    /// Whether it zooms (a narrow field narrower than the wide one).
    pub fn zooms(&self) -> bool {
        self.narrow_rad < self.wide_rad * 0.999
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EoSensor {
    pub channels: Vec<EoChannel>,
    /// Degrees per second; 0: fixed, looking along the vehicle's heading.
    pub slew_dps: f64,
    pub placeholder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadarSensor {
    pub r50_m: f64,
    pub rcs_ref_m2: f64,
    pub scan_s: f64,
    pub beam_rad: f64,
    pub cell_m: f64,
    pub lambda_m: f64,
    /// Measures elevation too (the catalog's `height`).
    pub height: bool,
    pub placeholder: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SonarSensor {
    pub mode: SonarMode,
    pub freq_khz: f64,
    /// Reference ranges by kind, m, in the reference environment.
    pub ranges: Vec<(String, f64)>,
    pub max_m: f64,
    /// A scanning sonar's revolution, s (one draw per pass); none: every second.
    pub scan_s: Option<f64>,
    pub placeholder: bool,
    /// An active sonar's passive mode (its array listening on every bearing, a draw a second).
    pub passive: Option<Box<SonarSensor>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EsmSensor {
    pub s_min_dbm: f64,
    pub bearing_deg: f64,
    pub placeholder: bool,
}

/// A catalog sensor as the physical model runs it.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    EoIr(EoSensor),
    Radar(RadarSensor),
    Ais,
    Sonar(SonarSensor),
    Esm(EsmSensor),
}

impl Resolved {
    pub fn placeholder(&self) -> bool {
        match self {
            Resolved::EoIr(s) => s.placeholder,
            Resolved::Radar(s) => s.placeholder,
            Resolved::Sonar(s) => s.placeholder,
            Resolved::Esm(s) => s.placeholder,
            Resolved::Ais => false,
        }
    }
}

/// The legacy model's per-kind signature multipliers (`murmur-contacts` `KINDS`): a placeholder
/// sonar hears each kind at its `range_m` times this, as the legacy model did.
const LEGACY_SONAR_KINDS: [(&str, f64); 9] = [
    ("cargo_ship", 1.6),
    ("tanker", 1.6),
    ("ferry", 1.2),
    ("patrol_boat", 0.9),
    ("fishing_vessel", 0.7),
    ("sailboat", 0.5),
    ("small_fast_boat", 0.35),
    ("submarine", 0.3),
    ("uuv", 0.15),
];

/// A placeholder camera's nominal target, its critical dimension m: a placeholder detects it at
/// about `range_m` in its wide field.
const PLACEHOLDER_DC_M: f64 = 5.0;
/// A placeholder radar's reference target, m^2: detected half the time at `range_m`.
const PLACEHOLDER_RCS_M2: f64 = 100.0;

/// The sensor `spec` resolves to on a platform that flies (`airborne`) or not. Its own fields
/// where it gives them; placeholders from `range_m` where it gives none.
pub fn resolve(spec: &SensorSpec, airborne: bool) -> Resolved {
    let f = &spec.physical;
    match spec.kind {
        SensorType::EoIr => {
            if f.channels.is_empty() {
                // a day camera and an LWIR camera of half its pixels, the wide field showing the
                // nominal target at 1.5 pixels at range_m, the narrow an eighth of it
                let px = 1280.0;
                let wide = (px * PLACEHOLDER_DC_M / (1.5 * spec.range_m.max(1.0))).clamp(1f64.to_radians(), 60f64.to_radians());
                let blur = if spec.range_m < 10_000.0 { 30e-6 } else { 10e-6 };
                let ch = |band, px| EoChannel { band, wide_rad: wide, narrow_rad: wide / 8.0, px, blur_rad: blur, panoramic: false };
                return Resolved::EoIr(EoSensor {
                    channels: vec![ch(Band::Day, px), ch(Band::Lwir, px / 2.0)],
                    slew_dps: 60.0,
                    placeholder: true,
                });
            }
            Resolved::EoIr(EoSensor {
                channels: f
                    .channels
                    .iter()
                    .map(|c| {
                        let wide = c.wide_deg.to_radians();
                        EoChannel {
                            band: c.band,
                            wide_rad: wide,
                            narrow_rad: c.narrow_deg.map_or(wide, |n| n.to_radians().min(wide)),
                            px: c.px,
                            blur_rad: c.blur_urad.unwrap_or(0.0) * 1e-6,
                            panoramic: c.panoramic,
                        }
                    })
                    .collect(),
                slew_dps: f.slew_dps.unwrap_or(60.0),
                placeholder: false,
            })
        }
        SensorType::Radar => {
            let placeholder = f.r50_m.is_none();
            Resolved::Radar(RadarSensor {
                r50_m: f.r50_m.unwrap_or(spec.range_m),
                rcs_ref_m2: f.rcs_ref_m2.unwrap_or(PLACEHOLDER_RCS_M2),
                scan_s: f.scan_s.unwrap_or(if airborne { 5.0 } else { 2.5 }),
                beam_rad: f.beam_deg.unwrap_or(1.2).to_radians(),
                cell_m: f.cell_m.unwrap_or(if airborne { 15.0 } else { 37.0 }),
                lambda_m: 0.299_792_458 / f.band_ghz.unwrap_or(9.4),
                height: spec.height,
                placeholder,
            })
        }
        SensorType::Ais => Resolved::Ais,
        SensorType::Sonar => {
            let placeholder = f.ranges.is_empty();
            let ranges = if placeholder {
                LEGACY_SONAR_KINDS.iter().map(|(k, s)| ((*k).to_string(), spec.range_m * s)).collect()
            } else {
                f.ranges.iter().map(|(k, r)| (k.clone(), *r)).collect()
            };
            let (mode, freq_khz, max_m) = (f.mode.unwrap_or(SonarMode::Passive), f.freq_khz.unwrap_or(1.0), f.max_m.unwrap_or(f64::INFINITY));
            let passive = f.passive.as_ref().filter(|_| mode == SonarMode::Active).map(|p| {
                Box::new(SonarSensor {
                    mode: SonarMode::Passive,
                    freq_khz: p.freq_khz.unwrap_or(freq_khz),
                    ranges: p.ranges.iter().map(|(k, r)| (k.clone(), *r)).collect(),
                    max_m: p.max_m.unwrap_or(max_m),
                    scan_s: None,
                    placeholder: false,
                    passive: None,
                })
            });
            Resolved::Sonar(SonarSensor { mode, freq_khz, ranges, max_m, scan_s: f.scan_s, placeholder, passive })
        }
        SensorType::Esm => Resolved::Esm(EsmSensor {
            s_min_dbm: f.s_min_dbm.unwrap_or(-80.0),
            bearing_deg: f.bearing_deg.unwrap_or(5.0),
            placeholder: f.s_min_dbm.is_none(),
        }),
    }
}

/// A catalog sensor's physical fields checked: positive figures, and (decision W2) no radar
/// revisiting slower than [`MAX_RADAR_REVISIT_S`].
pub fn validate(spec: &SensorSpec) -> Result<(), String> {
    let f = &spec.physical;
    let pos = |v: Option<f64>, what: &str| match v {
        Some(x) if !(x.is_finite() && x > 0.0) => Err(format!("{what} must be > 0")),
        _ => Ok(()),
    };
    for c in &f.channels {
        pos(Some(c.wide_deg), "a channel's wide_deg")?;
        pos(c.narrow_deg, "a channel's narrow_deg")?;
        pos(Some(c.px), "a channel's px")?;
        if c.narrow_deg.is_some_and(|n| n > c.wide_deg) {
            return Err("a channel's narrow_deg is wider than its wide_deg".into());
        }
        if c.blur_urad.is_some_and(|b| !(b.is_finite() && b >= 0.0)) {
            return Err("a channel's blur_urad must be >= 0".into());
        }
    }
    if f.slew_dps.is_some_and(|s| !(s.is_finite() && s >= 0.0)) {
        return Err("slew_dps must be >= 0".into());
    }
    pos(f.r50_m, "r50_m")?;
    pos(f.rcs_ref_m2, "rcs_ref_m2")?;
    pos(f.scan_s, "scan_s")?;
    pos(f.beam_deg, "beam_deg")?;
    pos(f.cell_m, "cell_m")?;
    pos(f.band_ghz, "band_ghz")?;
    pos(f.freq_khz, "freq_khz")?;
    pos(f.max_m, "max_m")?;
    pos(f.bearing_deg, "bearing_deg")?;
    if let Some((k, _)) = f.ranges.iter().find(|(_, r)| !(r.is_finite() && **r > 0.0)) {
        return Err(format!("ranges.{k} must be > 0"));
    }
    if let Some(p) = &f.passive {
        if spec.kind != SensorType::Sonar || f.mode != Some(SonarMode::Active) {
            return Err("a passive mode belongs to an active sonar (mode = \"active\")".into());
        }
        pos(p.freq_khz, "passive.freq_khz")?;
        pos(p.max_m, "passive.max_m")?;
        if p.ranges.is_empty() {
            return Err("a passive mode needs its reference ranges".into());
        }
        if let Some((k, _)) = p.ranges.iter().find(|(_, r)| !(r.is_finite() && **r > 0.0)) {
            return Err(format!("passive.ranges.{k} must be > 0"));
        }
    }
    if spec.kind == SensorType::Radar && f.scan_s.is_some_and(|s| s > MAX_RADAR_REVISIT_S) {
        return Err(format!(
            "a radar revisits every {} s at most (decision W2: the 10 s out-of-view rule needs a revisit within {MAX_RADAR_REVISIT_S} s)",
            f.scan_s.unwrap_or_default()
        ));
    }
    Ok(())
}

/// Every platform's sensors that run on placeholders, `platform:type`, sorted.
pub fn placeholders(catalog: &crate::tracks::sensing::SensorCatalog) -> Vec<String> {
    let mut out: Vec<String> = catalog
        .platforms
        .iter()
        .flat_map(|(id, p)| {
            p.sensors
                .iter()
                .filter(|s| resolve(s, false).placeholder())
                .map(move |s| format!("{id}:{}", s.kind.as_str()))
        })
        .collect();
    out.sort();
    out
}
