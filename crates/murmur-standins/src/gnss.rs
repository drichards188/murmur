//! GNSS reception: what a vehicle's GPS receiver sees, from real constellation geometry and
//! the scenario's jammers and spoofers.
//!
//! Three inputs, combined per vehicle on every physics tick (cheap: a table lookup and a few
//! multiplications; nothing here blocks the 10 Hz loop):
//!
//! 1. **Geometry** (`SkyTable`): satellites above the elevation mask and their HDOP / VDOP,
//!    from murmur-space (real GPS element sets, SGP4) on a coarse grid over the fleet's area
//!    (`GRID_STEP_DEG`) and a time step (`TIME_STEP_S`), computed by a background thread in
//!    one-hour blocks ahead of the scenario clock. Without the element-set cache (or before
//!    the first block is ready) the receiver sees what stand-ins always reported: 14
//!    satellites, HDOP 1.0 (`Sky::LEGACY`). GPS only (the catalog has no Galileo, GLONASS or
//!    BeiDou yet), so counts are lower than a multi-GNSS receiver's. Altitude is ignored
//!    (an aircraft sees the same sky as the ground below it).
//! 2. **Masking**: a uniform elevation mask (`MURMUR_GNSS_MASK_DEG`, default the GPS class's
//!    5 degrees). Terrain, buildings and bridges are not modelled; raise the mask to stand in
//!    for a valley or an urban canyon.
//! 3. **Interference** (`[[event]]`s with a `gnss` table, the link emulator's event format):
//!    - jamming raises the noise floor. J/S at the antenna from free-space path loss at L1
//!      (1575.42 MHz), the jammer's EIRP and height, the vehicle's altitude, a radio horizon
//!      (4/3 Earth) and an antenna that is weaker towards the horizon and below it. Each
//!      satellite's C/N0 then falls as (C/N0)eff = 1 / (1/(C/N0) + (J/S)/(Q Rc)) (Kaplan &
//!      Hegarty, Understanding GPS, ch. 9). A satellite is tracked while its C/N0 stays above
//!      `TRACK_DBHZ`, acquired again only above `ACQUIRE_DBHZ`. Four tracked with geometry:
//!      3D fix; three: 2D; fewer: no fix. Accuracy (GPS_RAW_INT h_acc) is HDOP x a UERE that
//!      grows with the code-tracking noise as C/N0 drops.
//!    - spoofing (simple): inside the footprint the receiver is captured and its position
//!      walks off the truth at `walk_mps` on `bearing_deg`, with the satellite count, DOP and
//!      fix all still good, so the EKF follows it and nothing in the flags shows it (see
//!      nav.rs for what happens when the capture ends).
//!
//! Every figure marked "estimate" is a Murmur choice in the range the literature gives, not
//! a measurement of a particular receiver.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use mavlink::dialects::ardupilotmega::GpsFixType;
use serde::Deserialize;

use crate::config::VehicleConfig;
use crate::geo::{M_PER_DEG, distance_m};

/// GPS L1 carrier, Hz.
pub const L1_HZ: f64 = 1_575.42e6;
/// Minimum received L1 C/A power, dBW (IS-GPS-200, -158.5 dBW for a 3 dBi antenna).
pub const SIGNAL_DBW: f64 = -158.5;
/// C/A code chip rate, chips/s.
pub const CHIP_RATE: f64 = 1.023e6;
/// C/A chip length, m.
pub const CHIP_M: f64 = 293.05;
/// Spectral separation coefficient for a broadband noise jammer (estimate; 1 for a
/// narrowband one, about 1.5 to 2 for noise spread over the C/A band).
pub const Q: f64 = 1.5;
/// Open-sky C/N0 of a satellite at mid elevation, dB-Hz (estimate: 40-50 is typical).
pub const CN0_NOMINAL_DBHZ: f64 = 45.0;
/// Satellites in view spread from this much below to `CN0_HIGH_DB` above nominal (low ones
/// are weaker through the antenna pattern and the longer path; estimate).
pub const CN0_LOW_DB: f64 = -9.0;
pub const CN0_HIGH_DB: f64 = 3.0;
/// A tracked satellite is lost below this C/N0, dB-Hz (estimate for a consumer receiver).
pub const TRACK_DBHZ: f64 = 28.0;
/// A lost satellite is acquired again only above this, dB-Hz (estimate).
pub const ACQUIRE_DBHZ: f64 = 33.0;
/// Single-frequency user range error without the tracking noise, m 1-sigma (estimate:
/// signal-in-space error plus residual ionosphere and troposphere).
pub const UERE_M: f64 = 3.0;
/// Code-tracking loop: bandwidth Hz, early-late spacing chips, integration s (estimate).
pub const DLL_BW_HZ: f64 = 1.0;
pub const DLL_SPACING: f64 = 0.5;
pub const DLL_T_S: f64 = 0.02;
/// Gain of the vehicle's antenna towards a jammer at the horizon, dBi, falling by
/// `ANT_BELOW_DB` more for one straight below (an upward patch; estimate).
pub const ANT_HORIZON_DBI: f64 = -3.0;
pub const ANT_BELOW_DB: f64 = 12.0;
/// Antenna height of a vehicle at the surface or on the ground, m.
pub const VEHICLE_ANTENNA_M: f64 = 1.5;
/// J/S of a jammer given without a power: denial anywhere in its circle, dB.
pub const DENY_JS_DB: f64 = 60.0;
/// Default jammer antenna height, m above MSL.
pub const JAMMER_HEIGHT_M: f64 = 10.0;

/// Geometry grid: cell size, and the margin around the fleet's spawn points, degrees.
pub const GRID_STEP_DEG: f64 = 0.1;
pub const GRID_MARGIN_DEG: f64 = 0.5;
/// At most this many cells a side (a wider fleet gets a coarser grid).
pub const GRID_MAX: usize = 30;
/// Geometry time step and block length, s.
pub const TIME_STEP_S: f64 = 30.0;
pub const BLOCK_S: f64 = 3600.0;
/// Blocks kept computed ahead of the scenario clock.
pub const LOOKAHEAD_BLOCKS: i64 = 1;

/// Satellites in view and their geometry at one place and time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sky {
    pub visible: u8,
    pub hdop: Option<f32>,
    pub vdop: Option<f32>,
}

impl Sky {
    /// What stand-ins reported before there was a constellation model.
    pub const LEGACY: Sky = Sky {
        visible: 14,
        hdop: Some(1.0),
        vdop: Some(1.0),
    };
}

/// One tick's reception.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reception {
    /// in view above the mask
    pub visible: u8,
    /// tracked (what GPS_RAW_INT's satellites_visible carries, as ArduPilot's num_sats)
    pub tracked: u8,
    pub fix: GpsFixType,
    pub hdop: Option<f64>,
    pub vdop: Option<f64>,
    /// 1-sigma position accuracy, m
    pub h_acc_m: Option<f64>,
    pub v_acc_m: Option<f64>,
    /// mean C/N0 of the tracked satellites, dB-Hz
    pub cn0_dbhz: f64,
    /// total jammer-to-signal ratio at the antenna, dB (None: no jammer reaches it)
    pub js_db: Option<f64>,
}

impl Reception {
    pub fn fix3d(&self) -> bool {
        self.fix as u8 >= GpsFixType::GPS_FIX_TYPE_3D_FIX as u8
    }

    /// No antenna above water (a submerged sub): no GPS at all.
    pub fn none() -> Reception {
        Reception {
            visible: 0,
            tracked: 0,
            fix: GpsFixType::GPS_FIX_TYPE_NO_GPS,
            hdop: None,
            vdop: None,
            h_acc_m: None,
            v_acc_m: None,
            cn0_dbhz: 0.0,
            js_db: None,
        }
    }
}

fn db(x: f64) -> f64 {
    10.0 * x.log10()
}

fn lin(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

/// Code-tracking (DLL) thermal noise, m 1-sigma, at a C/N0 (linear, Hz): Kaplan & Hegarty
/// eq. 5.79 for a coherent early-minus-late discriminator.
pub fn dll_sigma_m(cn0: f64) -> f64 {
    let d = DLL_SPACING;
    let v = DLL_BW_HZ * d / (2.0 * cn0) * (1.0 + 2.0 / (DLL_T_S * cn0 * (2.0 - d)));
    CHIP_M * v.sqrt()
}

/// The receiver's view this tick. `open_sky`: the antenna is above water. `js`: total J/S,
/// linear (0 = none). `locked`: it has a fix now (tracking threshold) or is looking for one
/// (acquisition threshold).
pub fn receive(sky: Sky, open_sky: bool, js: f64, locked: bool) -> Reception {
    if !open_sky {
        return Reception::none();
    }
    let n = usize::from(sky.visible);
    let noise = js / (Q * CHIP_RATE);
    let threshold = lin(if locked { TRACK_DBHZ } else { ACQUIRE_DBHZ });
    let mut tracked = 0usize;
    let mut sum_db = 0.0;
    for i in 0..n {
        let spread = if n > 1 {
            CN0_LOW_DB + (CN0_HIGH_DB - CN0_LOW_DB) * i as f64 / (n - 1) as f64
        } else {
            0.0
        };
        let cn0 = lin(CN0_NOMINAL_DBHZ + spread);
        let eff = 1.0 / (1.0 / cn0 + noise);
        if eff >= threshold {
            tracked += 1;
            sum_db += db(eff);
        }
    }
    let cn0_dbhz = if tracked > 0 {
        sum_db / tracked as f64
    } else {
        0.0
    };
    // losing satellites thins the geometry: DOP grows about as 1/sqrt(n) (estimate)
    let thin = if tracked > 0 {
        (n as f64 / tracked as f64).sqrt()
    } else {
        f64::INFINITY
    };
    let hdop = sky.hdop.map(|h| f64::from(h) * thin);
    let vdop = sky.vdop.map(|v| f64::from(v) * thin);
    let fix = match tracked {
        t if t >= 4 && hdop.is_some_and(f64::is_finite) => GpsFixType::GPS_FIX_TYPE_3D_FIX,
        3 => GpsFixType::GPS_FIX_TYPE_2D_FIX,
        _ => GpsFixType::GPS_FIX_TYPE_NO_FIX,
    };
    let uere = if tracked > 0 {
        UERE_M.hypot(dll_sigma_m(lin(cn0_dbhz)))
    } else {
        f64::INFINITY
    };
    let has_fix = fix as u8 >= GpsFixType::GPS_FIX_TYPE_2D_FIX as u8;
    Reception {
        visible: sky.visible,
        tracked: tracked as u8,
        fix,
        hdop: hdop.filter(|_| has_fix),
        vdop: vdop.filter(|_| fix as u8 >= GpsFixType::GPS_FIX_TYPE_3D_FIX as u8),
        h_acc_m: hdop.filter(|_| has_fix).map(|h| h * uere),
        v_acc_m: vdop
            .filter(|_| fix as u8 >= GpsFixType::GPS_FIX_TYPE_3D_FIX as u8)
            .map(|v| v * uere),
        cn0_dbhz,
        js_db: (js > 0.0).then(|| db(js)),
    }
}

// ------------------------------------------------------------------ interference

#[derive(Debug, Clone, Deserialize)]
pub struct Circle {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Jam,
    Spoof,
}

/// The `gnss` table of an `[[event]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GnssCfg {
    pub effect: Effect,
    /// jam: EIRP, W. Omitted: no fix anywhere in the circle.
    pub power_w: Option<f64>,
    /// jam: antenna height above MSL, m
    pub height_m: Option<f64>,
    /// spoof: how fast the reported position walks off the truth, m/s
    pub walk_mps: Option<f64>,
    /// spoof: direction of the walk, degrees true
    pub bearing_deg: Option<f64>,
    /// spoof: the walk stops at this offset, m
    pub max_offset_m: Option<f64>,
}

/// An `[[event]]` as the link emulator reads it (crates/murmur-linkemu/src/config.rs), only
/// the fields that matter here; the comms effects (loss, cut, latency) are its business.
#[derive(Debug, Clone, Deserialize)]
struct RawEvent {
    name: Option<String>,
    at_s: Option<f64>,
    at_scenario: Option<String>,
    duration_s: Option<f64>,
    until_scenario: Option<String>,
    class: Option<String>,
    profile: Option<String>,
    platform: Option<String>,
    domain: Option<String>,
    uid: Option<u32>,
    uids: Option<Vec<u32>>,
    circle: Option<Circle>,
    gnss: Option<GnssCfg>,
}

#[derive(Deserialize)]
struct EventFile {
    #[serde(default)]
    event: Vec<RawEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Window {
    /// engine seconds since start
    Engine(f64, f64),
    /// scenario unix seconds
    Scenario(f64, f64),
}

#[derive(Debug, Clone)]
pub struct Jammer {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    pub power_w: Option<f64>,
    pub height_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spoof {
    pub walk_mps: f64,
    pub bearing_deg: f64,
    pub max_offset_m: f64,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Jam(Jammer),
    Spoof(Spoof, Option<Circle>),
}

#[derive(Debug, Clone)]
pub struct Event {
    pub name: String,
    pub window: Window,
    pub kind: Kind,
    /// link class (sim/platforms.toml `link`), as the link emulator's `class`
    pub class: Option<String>,
    pub uids: Option<Vec<u32>>,
}

impl Event {
    fn active(&self, engine_s: f64, scenario_t: f64) -> bool {
        match self.window {
            Window::Engine(a, b) => engine_s >= a && engine_s < b,
            Window::Scenario(a, b) => scenario_t >= a && scenario_t < b,
        }
    }

    fn selects(&self, cfg: &VehicleConfig) -> bool {
        self.uids.as_ref().is_none_or(|u| u.contains(&cfg.uid()))
            && self
                .class
                .as_ref()
                .is_none_or(|c| cfg.spec.link.as_deref() == Some(c.as_str()))
    }
}

/// The GNSS events of an event file (`[[event]]`s without a `gnss` table are ignored).
pub fn parse_events(text: &str) -> Result<Vec<Event>, String> {
    let file: EventFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (i, e) in file.event.into_iter().enumerate() {
        let Some(g) = e.gnss else { continue };
        let name = e.name.clone().unwrap_or_else(|| format!("event {i}"));
        if e.profile.is_some() || e.platform.is_some() || e.domain.is_some() {
            return Err(format!(
                "{name}: a GNSS event selects by circle, class, uid or uids (profile, \
                 platform and domain are link-emulator selectors the stand-in engine \
                 does not see)"
            ));
        }
        let window = match (e.at_s, &e.at_scenario) {
            (Some(a), None) => {
                if e.until_scenario.is_some() {
                    return Err(format!("{name}: until_scenario needs at_scenario"));
                }
                Window::Engine(a, e.duration_s.map_or(f64::INFINITY, |d| a + d))
            }
            (None, Some(s)) => {
                let a = crate::conditions::parse_time(s)?;
                let b = match (&e.until_scenario, e.duration_s) {
                    (Some(_), Some(_)) => {
                        return Err(format!("{name}: give duration_s or until_scenario"));
                    }
                    (Some(u), None) => crate::conditions::parse_time(u)?,
                    (None, Some(d)) => a + d,
                    (None, None) => f64::INFINITY,
                };
                Window::Scenario(a, b)
            }
            _ => return Err(format!("{name}: give exactly one of at_s and at_scenario")),
        };
        let kind = match g.effect {
            Effect::Jam => {
                let Some(c) = e.circle.clone() else {
                    return Err(format!("{name}: a GNSS jammer needs a circle"));
                };
                if g.walk_mps.is_some() || g.bearing_deg.is_some() || g.max_offset_m.is_some() {
                    return Err(format!(
                        "{name}: walk_mps, bearing_deg, max_offset_m are spoof fields"
                    ));
                }
                if g.power_w.is_some_and(|p| p <= 0.0) {
                    return Err(format!("{name}: power_w must be positive"));
                }
                Kind::Jam(Jammer {
                    lat: c.lat,
                    lon: c.lon,
                    radius_m: c.radius_m,
                    power_w: g.power_w,
                    height_m: g.height_m.unwrap_or(JAMMER_HEIGHT_M),
                })
            }
            Effect::Spoof => {
                if g.power_w.is_some() || g.height_m.is_some() {
                    return Err(format!("{name}: power_w and height_m are jam fields"));
                }
                Kind::Spoof(
                    Spoof {
                        walk_mps: g.walk_mps.unwrap_or(1.0).max(0.0),
                        bearing_deg: g.bearing_deg.unwrap_or(90.0),
                        max_offset_m: g.max_offset_m.unwrap_or(f64::INFINITY),
                    },
                    e.circle.clone(),
                )
            }
        };
        let uids = match (e.uid, e.uids) {
            (Some(u), Some(mut us)) => {
                us.push(u);
                Some(us)
            }
            (Some(u), None) => Some(vec![u]),
            (None, us) => us,
        };
        out.push(Event {
            name,
            window,
            kind,
            class: e.class,
            uids,
        });
    }
    Ok(out)
}

/// J/S of one jammer at a vehicle, dB; None when out of its circle or below the horizon.
pub fn js_db(j: &Jammer, lat: f64, lon: f64, alt_m: f64) -> Option<f64> {
    let d = distance_m(j.lat, j.lon, lat, lon);
    if d > j.radius_m {
        return None;
    }
    let Some(p) = j.power_w else {
        return Some(DENY_JS_DB);
    };
    let hj = j.height_m.max(0.0);
    let hv = alt_m.max(VEHICLE_ANTENNA_M);
    // radio horizon, 4/3 Earth: 4.12 km x (sqrt(h1) + sqrt(h2)), heights in m
    if d > 4_120.0 * (hj.sqrt() + hv.sqrt()) {
        return None;
    }
    let slant = d.hypot(hv - hj).max(1.0);
    let lambda = 299_792_458.0 / L1_HZ;
    let fspl = 20.0 * (4.0 * std::f64::consts::PI * slant / lambda).log10();
    let el = (hj - hv).atan2(d).to_degrees(); // the jammer as seen from the vehicle
    let gain = ANT_HORIZON_DBI + ANT_BELOW_DB * el.min(0.0) / 90.0;
    Some(db(p) - fspl + gain - SIGNAL_DBW)
}

/// The ground distance from a jammer at which a vehicle at `alt_m` loses its fix (J/S where
/// even the strongest satellite drops below the tracking threshold), m. For logs and docs.
pub fn denial_radius_m(j: &Jammer, alt_m: f64) -> f64 {
    let cn0 = lin(CN0_NOMINAL_DBHZ + CN0_HIGH_DB);
    let need = db((1.0 / lin(TRACK_DBHZ) - 1.0 / cn0) * Q * CHIP_RATE);
    let (mut lo, mut hi) = (0.0, j.radius_m);
    if js_db(j, j.lat + hi / M_PER_DEG, j.lon, alt_m).is_some_and(|x| x >= need) {
        return hi;
    }
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if js_db(j, j.lat + mid / M_PER_DEG, j.lon, alt_m).is_some_and(|x| x >= need) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// What the scenario does to one vehicle's GNSS this tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct Interference {
    /// total J/S, linear
    pub js: f64,
    pub spoof: Option<Spoof>,
}

// ------------------------------------------------------------------ geometry table

/// Satellites in view and DOP on a lat/lon grid and time steps, filled in blocks.
pub struct SkyTable {
    pub lat0: f64,
    pub lon0: f64,
    pub step_deg: f64,
    pub nlat: usize,
    pub nlon: usize,
    /// scenario unix time of block 0
    pub t0: f64,
    blocks: RwLock<Vec<Option<Arc<Vec<Sky>>>>>,
}

impl SkyTable {
    /// A grid covering `points` (lat, lon) plus the margin.
    pub fn covering(points: &[(f64, f64)], t0: f64) -> SkyTable {
        let lat_min = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        let lat_max = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
        let lon_min = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let lon_max = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let (lat_min, lat_max, lon_min, lon_max) = if lat_min.is_finite() {
            (
                (lat_min - GRID_MARGIN_DEG).max(-89.0),
                (lat_max + GRID_MARGIN_DEG).min(89.0),
                lon_min - GRID_MARGIN_DEG,
                lon_max + GRID_MARGIN_DEG,
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };
        let span = (lat_max - lat_min).max(lon_max - lon_min);
        let step = GRID_STEP_DEG.max(span / (GRID_MAX - 1) as f64);
        let n = |a: f64, b: f64| ((b - a) / step).ceil() as usize + 1;
        SkyTable {
            lat0: lat_min,
            lon0: lon_min,
            step_deg: step,
            nlat: n(lat_min, lat_max),
            nlon: n(lon_min, lon_max),
            t0,
            blocks: RwLock::new(Vec::new()),
        }
    }

    pub fn points(&self) -> Vec<(f64, f64)> {
        let mut out = Vec::with_capacity(self.nlat * self.nlon);
        for i in 0..self.nlat {
            for j in 0..self.nlon {
                out.push((
                    self.lat0 + i as f64 * self.step_deg,
                    self.lon0 + j as f64 * self.step_deg,
                ));
            }
        }
        out
    }

    fn samples_per_block() -> usize {
        (BLOCK_S / TIME_STEP_S).round() as usize
    }

    pub fn block_of(&self, t: f64) -> i64 {
        ((t - self.t0) / BLOCK_S).floor() as i64
    }

    pub fn has_block(&self, k: i64) -> bool {
        k >= 0
            && self
                .blocks
                .read()
                .is_ok_and(|b| b.get(k as usize).is_some_and(Option::is_some))
    }

    /// Install block `k`: `skies[sample][point]` with points in `points()` order.
    pub fn put_block(&self, k: i64, skies: Vec<Vec<Sky>>) {
        let Ok(k) = usize::try_from(k) else { return };
        let per = Self::samples_per_block();
        let mut flat = Vec::with_capacity(per * self.nlat * self.nlon);
        for s in 0..per {
            for p in 0..self.nlat * self.nlon {
                flat.push(
                    skies
                        .get(s)
                        .and_then(|row| row.get(p))
                        .copied()
                        .unwrap_or(Sky::LEGACY),
                );
            }
        }
        if let Ok(mut b) = self.blocks.write() {
            if b.len() <= k {
                b.resize(k + 1, None);
            }
            b[k] = Some(Arc::new(flat));
        }
    }

    /// The nearest grid cell and time step; None when that block isn't computed (yet) or the
    /// table is being written this instant (never waits).
    pub fn at(&self, lat: f64, lon: f64, t: f64) -> Option<Sky> {
        let k = usize::try_from(self.block_of(t)).ok()?;
        let block = self.blocks.try_read().ok()?.get(k)?.clone()?;
        let into = t - self.t0 - k as f64 * BLOCK_S;
        let s = ((into / TIME_STEP_S).round() as usize).min(Self::samples_per_block() - 1);
        let cell = |x: f64, x0: f64, n: usize| {
            (((x - x0) / self.step_deg).round().max(0.0) as usize).min(n - 1)
        };
        let i = cell(lat, self.lat0, self.nlat);
        let j = cell(lon, self.lon0, self.nlon);
        block.get((s * self.nlat + i) * self.nlon + j).copied()
    }
}

/// Fills a SkyTable from murmur-space in the background, keeping it ahead of the clock.
struct Filler {
    space: murmur_space::Space,
    mask_deg: Option<f64>,
}

/// Block `k` of a table from murmur-space's GPS class: `[sample][point]`.
pub fn compute_block(
    space: &murmur_space::Space,
    mask_deg: Option<f64>,
    table: &SkyTable,
    k: i64,
) -> Result<Vec<Vec<Sky>>, String> {
    {
        let from = table.t0 + k as f64 * BLOCK_S;
        let per = SkyTable::samples_per_block();
        let to = from + (per - 1) as f64 * TIME_STEP_S;
        let ts = |t: f64| {
            chrono::DateTime::from_timestamp(t.floor() as i64, 0)
                .ok_or_else(|| format!("time {t} out of range"))
        };
        let (from_t, to_t) = (ts(from)?, ts(to)?);
        let points = table.points();
        let mut by_point = Vec::with_capacity(points.len());
        for (lat, lon) in points {
            let q = murmur_space::GnssQuery {
                point: murmur_space::Geodetic::new(lat, lon, 0.0),
                from: from_t,
                to: to_t,
                step_s: TIME_STEP_S,
                class: "gps".into(),
                mask_deg,
                epoch_mode: murmur_space::EpochMode::Auto,
            };
            let r = space.gnss(&q).map_err(|e| e.to_string())?;
            by_point.push(
                r.samples
                    .iter()
                    .map(|s| Sky {
                        visible: s.visible.min(255) as u8,
                        hdop: s.dop.map(|d| d.hdop as f32),
                        vdop: s.dop.map(|d| d.vdop as f32),
                    })
                    .collect::<Vec<_>>(),
            );
        }
        // transpose to [sample][point]
        Ok((0..per)
            .map(|s| {
                by_point
                    .iter()
                    .map(|row| row.get(s).copied().unwrap_or(Sky::LEGACY))
                    .collect()
            })
            .collect())
    }
}

/// Load the GPS class from the space catalog and its cached element sets (no fetching).
fn load_space(catalog: &Path, cache: &Path) -> Result<murmur_space::Space, String> {
    let mut cat = murmur_space::Catalog::load(catalog).map_err(|e| e.to_string())?;
    cat.classes.retain(|c| c.id == "gps");
    if cat.classes.is_empty() {
        return Err(format!("{}: no gps class", catalog.display()));
    }
    let store = murmur_space::cache::Cache::new(cache);
    let mut sources = Vec::new();
    for q in cat.queries() {
        let (text, _) = store.read(&q).map_err(|e| e.to_string())?;
        sources.push((q, text));
    }
    murmur_space::Space::from_sources(cat, &sources).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ environment

/// Everything GNSS that the vehicles share: the scenario clock, the geometry table, the
/// interference events.
pub struct Env {
    /// scenario unix time at engine start (MURMUR_AT, else the wall clock)
    pub t0_unix: f64,
    /// scenario seconds per engine second (MURMUR_RATE)
    pub rate: f64,
    pub sky: Option<Arc<SkyTable>>,
    pub events: Vec<Event>,
}

impl Env {
    /// No geometry, no events: every receiver sees `Sky::LEGACY`.
    pub fn plain() -> Env {
        Env {
            t0_unix: 0.0,
            rate: 1.0,
            sky: None,
            events: Vec::new(),
        }
    }

    pub fn scenario_t(&self, engine_s: f64) -> f64 {
        self.t0_unix + engine_s * self.rate
    }

    pub fn sky_at(&self, lat: f64, lon: f64, engine_s: f64) -> Option<Sky> {
        self.sky.as_ref()?.at(lat, lon, self.scenario_t(engine_s))
    }

    /// Jamming and spoofing at a vehicle's true position.
    pub fn interference(
        &self,
        cfg: &VehicleConfig,
        engine_s: f64,
        (lat, lon, alt): (f64, f64, f64),
    ) -> Interference {
        let mut out = Interference::default();
        if self.events.is_empty() {
            return out;
        }
        let t = self.scenario_t(engine_s);
        for e in &self.events {
            if !e.active(engine_s, t) || !e.selects(cfg) {
                continue;
            }
            match &e.kind {
                Kind::Jam(j) => {
                    if let Some(x) = js_db(j, lat, lon, alt) {
                        out.js += lin(x);
                    }
                }
                Kind::Spoof(s, circle) => {
                    let inside = circle
                        .as_ref()
                        .is_none_or(|c| distance_m(c.lat, c.lon, lat, lon) <= c.radius_m);
                    if inside && out.spoof.is_none() {
                        out.spoof = Some(*s);
                    }
                }
            }
        }
        out
    }

    /// From the environment: MURMUR_AT / MURMUR_RATE (scenario clock), MURMUR_EVENTS (event
    /// file), sim/links.toml's own events, MURMUR_GNSS_MASK_DEG, and the space catalog and
    /// cache (MURMUR_SPACE_CATALOG, MURMUR_SPACE_CACHE; defaults under MURMUR_ROOT or the
    /// working directory). Starts the geometry thread. Says once what it runs with.
    pub fn from_env(homes: &[(f64, f64)]) -> Env {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let root = var("MURMUR_ROOT").map_or_else(|| PathBuf::from("."), PathBuf::from);
        let path =
            |k: &str, default: &str| var(k).map_or_else(|| root.join(default), PathBuf::from);
        let at = var("MURMUR_AT");
        let t0_unix = match at.as_deref().map(crate::conditions::parse_time) {
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                println!("gnss: {e}; scenario clock = wall clock");
                wall_unix()
            }
            None => wall_unix(),
        };
        let rate = var("MURMUR_RATE")
            .and_then(|r| r.parse().ok())
            .unwrap_or(1.0);
        let mut events = Vec::new();
        let links = path("MURMUR_LINKS", "sim/links.toml");
        let mut files = vec![links];
        if let Some(e) = var("MURMUR_EVENTS") {
            files.push(PathBuf::from(e));
        }
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            match parse_events(&text) {
                Ok(mut es) => {
                    for e in &es {
                        println!("gnss: event {:?} from {}", e.name, f.display());
                        if let Kind::Jam(j) = &e.kind {
                            println!(
                                "gnss:   jammer at {:.4},{:.4}, circle {:.0} m: no fix within {:.0} m at the surface, {:.0} m at 1,000 m",
                                j.lat,
                                j.lon,
                                j.radius_m,
                                denial_radius_m(j, 0.0),
                                denial_radius_m(j, 1000.0)
                            );
                        }
                        if matches!(e.window, Window::Scenario(..)) && at.is_none() {
                            println!(
                                "gnss:   {:?} is on the scenario clock: set MURMUR_AT",
                                e.name
                            );
                        }
                    }
                    events.append(&mut es);
                }
                Err(e) => println!("gnss: {}: {e}; its GNSS events are ignored", f.display()),
            }
        }
        let mask_deg = var("MURMUR_GNSS_MASK_DEG").and_then(|m| m.parse().ok());
        let catalog = path("MURMUR_SPACE_CATALOG", "sim/space/constellations.toml");
        let cache = path("MURMUR_SPACE_CACHE", "sim/space/cache");
        let sky = match load_space(&catalog, &cache) {
            Ok(mut space) => {
                let day = |t: f64| chrono::DateTime::from_timestamp(t as i64, 0);
                if let (Some(a), Some(b)) = (day(t0_unix), day(t0_unix + 86_400.0)) {
                    space.pin_shift(Some((a, b)));
                }
                let table = Arc::new(SkyTable::covering(homes, t0_unix));
                println!(
                    "gnss: GPS geometry from {} ({} satellites), {}x{} grid of {:.2} deg, {} s steps, mask {}",
                    cache.display(),
                    space.classes.first().map_or(0, |c| c.sats.len()),
                    table.nlat,
                    table.nlon,
                    table.step_deg,
                    TIME_STEP_S,
                    mask_deg.map_or_else(|| "5 deg (class default)".into(), |m| format!("{m} deg"))
                );
                spawn_filler(Filler { space, mask_deg }, table.clone(), t0_unix, rate);
                Some(table)
            }
            Err(e) => {
                println!(
                    "gnss: no GPS geometry ({e}); receivers report 14 satellites, HDOP 1.0 as before (run `murmur-space fetch` once for real geometry)"
                );
                None
            }
        };
        Env {
            t0_unix,
            rate,
            sky,
            events,
        }
    }
}

fn wall_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The geometry thread: the block the scenario clock is in, then the next, forever.
fn spawn_filler(filler: Filler, table: Arc<SkyTable>, t0: f64, rate: f64) {
    let wall0 = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("gnss-geometry".into())
        .spawn(move || {
            let mut said = false;
            let mut failed: Option<String> = None;
            loop {
                let now = t0 + wall0.elapsed().as_secs_f64() * rate;
                let k0 = table.block_of(now).max(0);
                for k in k0..=k0 + LOOKAHEAD_BLOCKS {
                    if table.has_block(k) {
                        continue;
                    }
                    let started = Instant::now();
                    match compute_block(&filler.space, filler.mask_deg, &table, k) {
                        Ok(b) => {
                            table.put_block(k, b);
                            if !said {
                                said = true;
                                println!(
                                    "gnss: geometry ready ({} ms per hour of scenario)",
                                    started.elapsed().as_millis()
                                );
                            }
                        }
                        Err(e) => {
                            if failed.as_deref() != Some(e.as_str()) {
                                println!("gnss: geometry: {e}");
                                failed = Some(e);
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    if let Err(e) = spawned {
        println!("gnss: geometry thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jammer(power_w: Option<f64>, height_m: f64, radius_m: f64) -> Jammer {
        Jammer {
            lat: 37.83,
            lon: -122.50,
            radius_m,
            power_w,
            height_m,
        }
    }

    #[test]
    fn open_sky_is_a_3d_fix_with_the_geometry_dop() {
        let sky = Sky {
            visible: 9,
            hdop: Some(0.9),
            vdop: Some(1.4),
        };
        let r = receive(sky, true, 0.0, true);
        assert_eq!(r.fix, GpsFixType::GPS_FIX_TYPE_3D_FIX);
        assert_eq!(r.tracked, 9);
        assert!((r.hdop.unwrap_or(0.0) - 0.9).abs() < 1e-6);
        // h_acc = HDOP x UERE, tracking noise small at open-sky C/N0
        let h = r.h_acc_m.unwrap_or(0.0);
        assert!(h > 0.9 * UERE_M && h < 0.9 * UERE_M * 1.1, "{h}");
        assert!(r.js_db.is_none());
        let under = receive(sky, false, 0.0, true);
        assert_eq!(under.fix, GpsFixType::GPS_FIX_TYPE_NO_GPS);
        assert_eq!(under.tracked, 0);
    }

    #[test]
    fn tracking_noise_grows_as_cn0_drops() {
        let strong = dll_sigma_m(lin(45.0));
        let weak = dll_sigma_m(lin(28.0));
        assert!(strong < 1.0 && weak > 5.0, "{strong} {weak}");
    }

    #[test]
    fn rising_js_degrades_then_denies_with_hysteresis() {
        let sky = Sky {
            visible: 10,
            hdop: Some(1.0),
            vdop: Some(1.5),
        };
        let at = |js_db: f64, locked| receive(sky, true, lin(js_db), locked);
        let clean = at(0.0, true);
        let degraded = at(30.0, true);
        assert!(degraded.fix3d() && degraded.tracked == 10);
        assert!(degraded.h_acc_m > clean.h_acc_m, "accuracy worsens");
        assert!(degraded.cn0_dbhz < clean.cn0_dbhz - 10.0);
        let denied = at(40.0, true);
        assert_eq!(denied.fix, GpsFixType::GPS_FIX_TYPE_NO_FIX);
        assert_eq!(denied.tracked, 0);
        assert!(denied.h_acc_m.is_none() && denied.hdop.is_none());
        // between the thresholds: a receiver with a fix keeps it, one without can't get one
        assert!(at(31.0, true).fix3d());
        assert!(!at(31.0, false).fix3d());
    }

    #[test]
    fn jammer_denies_inside_and_not_outside() {
        let j = jammer(Some(0.25), 280.0, 25_000.0);
        let near = js_db(&j, 37.83 + 2_000.0 / M_PER_DEG, -122.50, 100.0).unwrap_or(0.0);
        let far = js_db(&j, 37.83 + 20_000.0 / M_PER_DEG, -122.50, 100.0).unwrap_or(0.0);
        assert!(near > far + 15.0, "{near} {far}");
        let sky = Sky::LEGACY;
        assert!(
            !receive(sky, true, lin(near), true).fix3d(),
            "{near} dB inside"
        );
        assert!(
            receive(sky, true, lin(far), true).fix3d(),
            "{far} dB further out"
        );
        // outside the circle: nothing at all
        assert!(js_db(&j, 37.83 + 30_000.0 / M_PER_DEG, -122.50, 100.0).is_none());
        // below the radio horizon: nothing either
        let low = jammer(Some(100.0), 2.0, 100_000.0);
        assert!(js_db(&low, 37.83 + 20_000.0 / M_PER_DEG, -122.50, 0.0).is_none());
        // no power given: denial anywhere in the circle
        let deny = jammer(None, 2.0, 5_000.0);
        let x = js_db(&deny, 37.83 + 4_000.0 / M_PER_DEG, -122.50, 0.0).unwrap_or(0.0);
        assert!(!receive(sky, true, lin(x), true).fix3d());
        // the denial radius is where the fix goes
        let r = denial_radius_m(&j, 100.0);
        assert!(r > 3_000.0 && r < 15_000.0, "{r}");
        let inside = js_db(&j, 37.83 + 0.9 * r / M_PER_DEG, -122.50, 100.0).unwrap_or(0.0);
        let outside = js_db(&j, 37.83 + 1.2 * r / M_PER_DEG, -122.50, 100.0).unwrap_or(0.0);
        assert!(!receive(sky, true, lin(inside), true).fix3d());
        assert!(receive(sky, true, lin(outside), true).fix3d());
    }

    #[test]
    fn an_antenna_above_the_jammer_is_shielded() {
        let j = jammer(Some(1.0), 10.0, 100_000.0);
        let at = |alt: f64| js_db(&j, 37.83 + 10_000.0 / M_PER_DEG, -122.50, alt);
        let level = at(10.0).unwrap_or(0.0);
        let above = at(8_000.0).unwrap_or(0.0);
        assert!(above < level - 3.0, "{level} {above}");
    }

    const EVENTS: &str = r#"
        seed = 1
        [[event]]
        name = "30% loss"
        at_s = 10
        loss = 0.3

        [[event]]
        name = "jammer"
        at_scenario = "2026-06-14T10:05:00Z"
        duration_s = 600
        circle = { lat = 37.83, lon = -122.50, radius_m = 20000 }
        gnss = { effect = "jam", power_w = 0.25, height_m = 280 }

        [[event]]
        name = "spoofer"
        at_s = 0
        uid = 3
        circle = { lat = 37.80, lon = -122.45, radius_m = 3000 }
        gnss = { effect = "spoof", walk_mps = 2, bearing_deg = 45 }
    "#;

    #[test]
    fn events_parse_and_comms_only_events_are_skipped() {
        let es = parse_events(EVENTS).expect("parses");
        assert_eq!(es.len(), 2);
        assert!(matches!(es[0].kind, Kind::Jam(_)));
        let Window::Scenario(a, b) = es[0].window else {
            panic!("scenario window");
        };
        assert_eq!(b - a, 600.0);
        assert!(matches!(es[1].kind, Kind::Spoof(s, _) if s.walk_mps == 2.0));
        assert_eq!(es[1].uids, Some(vec![3]));
        assert!(parse_events("[[event]]\nat_s = 0\ngnss = { effect = \"jam\" }").is_err());
        assert!(
            parse_events("[[event]]\nat_s = 0\ncircle = { lat = 0, lon = 0, radius_m = 1 }\ngnss = { effect = \"jam\", walk_mps = 1 }").is_err()
        );
        assert!(
            parse_events("[[event]]\nat_s = 0\nprofile = \"los\"\ngnss = { effect = \"spoof\" }")
                .is_err()
        );
    }

    #[test]
    fn sky_table_lookup_by_cell_and_time() {
        let t = SkyTable::covering(&[(37.8, -122.5)], 1000.0);
        assert!(t.at(37.8, -122.5, 1000.0).is_none(), "not computed yet");
        let per = SkyTable::samples_per_block();
        let pts = t.points();
        let skies: Vec<Vec<Sky>> = (0..per)
            .map(|s| {
                pts.iter()
                    .map(|&(lat, _)| Sky {
                        visible: (s % 5) as u8 + if lat > 37.8 { 10 } else { 4 },
                        hdop: Some(1.0),
                        vdop: Some(1.0),
                    })
                    .collect()
            })
            .collect();
        t.put_block(0, skies);
        assert!(t.has_block(0) && !t.has_block(1));
        let a = t.at(37.8, -122.5, 1000.0).map(|s| s.visible);
        let b = t
            .at(37.8, -122.5, 1000.0 + 2.0 * TIME_STEP_S)
            .map(|s| s.visible);
        let c = t.at(38.2, -122.5, 1000.0).map(|s| s.visible);
        assert_eq!(a, Some(4));
        assert_eq!(b, Some(6));
        assert_eq!(c, Some(10));
        assert!(t.at(37.8, -122.5, 1000.0 + BLOCK_S + 1.0).is_none());
    }

    /// sim/events/golden-gate-gnss.toml parses, and its comments' figures hold.
    #[test]
    fn golden_gate_example_matches_its_description() {
        let es = parse_events(include_str!("../../../sim/events/golden-gate-gnss.toml"))
            .expect("example");
        assert_eq!(es.len(), 1);
        let Kind::Jam(j) = &es[0].kind else {
            panic!("a jammer");
        };
        let km = |m: f64| (m / 1000.0).round();
        let surface = km(denial_radius_m(j, 0.0));
        assert!((22.0..=23.0).contains(&surface), "{surface}");
        // at 7,600 m: denied at 5 km, not straight above, not at 15 km
        let sky = Sky::LEGACY;
        let at = |d_m: f64, alt: f64, locked: bool| {
            let x = js_db(j, j.lat + d_m / M_PER_DEG, j.lon, alt).unwrap_or(0.0);
            receive(sky, true, lin(x), locked).fix3d()
        };
        assert!(!at(5_000.0, 7_600.0, true));
        assert!(at(500.0, 7_600.0, true));
        assert!(at(15_000.0, 7_600.0, true));
        // reacquisition: blocked at 30 km, possible at 34 km
        assert!(!at(30_000.0, 7_600.0, false));
        assert!(at(34_000.0, 7_600.0, false));
        // 11,000 m keeps its fix everywhere
        for d in [0.0, 3_000.0, 6_000.0, 10_000.0, 20_000.0] {
            assert!(at(d, 11_000.0, true), "{d}");
        }
    }
}
