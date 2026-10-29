//! The water for the contacts and our vehicles (world v3, H4): what tidewater's conditions give
//! them live, from the world's baked files instead (eval/headless/bake_world.py).
//!
//! - **Contacts** step with the current at their position (`murmur_contacts::motion::Mover::step`
//!   takes it; only a drifting contact moves with it), as the contacts process does with
//!   `MURMUR_TIDEWATER_URL` (crates/murmur-contacts `main.rs`).
//! - **Our vehicles** that take conditions (`Behavior::wants_conditions`: boats, subs, the
//!   drifter, a UGV, a sailboat, a blimp) get a tidewater sample about once a second, as the
//!   stand-in engine's poller gives them (crates/murmur-standins `conditions.rs`): the elevation
//!   and water level from the world's surface and land/water grids (so the depth under a keel
//!   and the ground under a UGV), the current from the field, the wind the situation drew.
//!
//! The current field is the world's surface current (`[currents] field`), bilinear in space
//! and linear in time between its steps, held at its first and last step outside its window;
//! none outside its box or next to a point without data. The tide in the grids is the one they
//! were baked at. A `current` fault puts a uniform current in place of the field's for a while,
//! for contacts and our vehicles alike. Without a field and without a fault the water is still:
//! a contact steps with no current (`None`), as before world v3.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use murmur_standins::conditions::Sample;
use serde::Deserialize;

use super::terrain::Grid;

#[derive(Debug, Clone, Deserialize)]
struct FieldHeader {
    lat0: f64,
    lon0: f64,
    dlat: f64,
    dlon: f64,
    nlat: usize,
    nlon: usize,
    scale: f64,
    nodata: i16,
    t0: f64,
    dt: f64,
    nt: usize,
}

/// The world's current field: `nt` steps of `nlat x nlon` (east, north) pairs.
pub struct CurrentField {
    h: FieldHeader,
    data: Vec<i16>,
}

impl CurrentField {
    pub fn load(path: &Path) -> Result<CurrentField, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(file)
            .read_to_end(&mut raw)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let nl = raw
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| format!("{}: no header line", path.display()))?;
        let h: FieldHeader =
            serde_json::from_slice(&raw[..nl]).map_err(|e| format!("{}: header: {e}", path.display()))?;
        let body = &raw[nl + 1..];
        if body.len() != h.nt * h.nlat * h.nlon * 4 || h.nt == 0 || h.dt <= 0.0 {
            return Err(format!("{}: {} bytes for {} steps of a {}x{} field", path.display(), body.len(), h.nt, h.nlat, h.nlon));
        }
        let data = body.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
        Ok(CurrentField { h, data })
    }

    fn pair(&self, k: usize, i: usize, j: usize) -> Option<(f64, f64)> {
        let at = ((k * self.h.nlat + i) * self.h.nlon + j) * 2;
        let (e, n) = (*self.data.get(at)?, *self.data.get(at + 1)?);
        (e != self.h.nodata && n != self.h.nodata).then(|| (f64::from(e) * self.h.scale, f64::from(n) * self.h.scale))
    }

    fn bilinear(&self, k: usize, lat: f64, lon: f64) -> Option<(f64, f64)> {
        let y = (lat - self.h.lat0) / self.h.dlat;
        let x = (lon - self.h.lon0) / self.h.dlon;
        if !(y >= 0.0 && x >= 0.0) {
            return None;
        }
        let (i, j) = (y.floor() as usize, x.floor() as usize);
        if i + 1 >= self.h.nlat || j + 1 >= self.h.nlon {
            return None;
        }
        let (fy, fx) = (y - i as f64, x - j as f64);
        let (a, b, c, d) = (self.pair(k, i, j)?, self.pair(k, i, j + 1)?, self.pair(k, i + 1, j)?, self.pair(k, i + 1, j + 1)?);
        let mix = |a: f64, b: f64, c: f64, d: f64| (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy;
        Some((mix(a.0, b.0, c.0, d.0), mix(a.1, b.1, c.1, d.1)))
    }

    /// The current (east, north) m/s at a point and scenario time (unix s).
    pub fn at(&self, lat: f64, lon: f64, t: f64) -> Option<(f64, f64)> {
        let s = ((t - self.h.t0) / self.h.dt).clamp(0.0, (self.h.nt - 1) as f64);
        let k = s.floor() as usize;
        let f = s - k as f64;
        let a = self.bilinear(k, lat, lon)?;
        if k + 1 >= self.h.nt || f <= 0.0 {
            return Some(a);
        }
        let b = self.bilinear(k + 1, lat, lon)?;
        Some((a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f))
    }
}

/// A `current` fault: a uniform current until a scenario time (none: to the end).
#[derive(Debug, Clone, Copy)]
struct Uniform {
    east: f64,
    north: f64,
    from: f64,
    until: Option<f64>,
}

pub struct Water {
    weather: Option<murmur_weather::Store>,
    field: Option<CurrentField>,
    surface: Option<Arc<Grid>>,
    depth: Option<Arc<Grid>>,
    wind_en: Option<[f64; 2]>,
    uniform: Mutex<Option<Uniform>>,
}

impl Water {
    /// Still water: no field, no grids (a `current` fault may still set one).
    pub fn still() -> Water {
        Water { weather: None, field: None, surface: None, depth: None, wind_en: None, uniform: Mutex::new(None) }
    }

    /// The world's water (`spec.water`): its current field, its land/water grid, the drawn wind;
    /// `surface` is the emulator's surface grid.
    pub fn load(spec: &super::spec::WaterSpec, surface: Option<Arc<Grid>>) -> Result<Water, String> {
        let field = spec.currents.as_deref().map(CurrentField::load).transpose()?;
        let depth = spec.land_water.as_deref().map(Grid::load).transpose()?.map(Arc::new);
        Ok(Water { weather: None, field, surface, depth, wind_en: spec.wind_en, uniform: Mutex::new(None) })
    }

    pub fn set_weather(&mut self, config: &murmur_weather::Config, root: &Path) {
        let mut store = murmur_weather::Store::default();
        store.configure(Some(config), Some(root));
        self.weather = Some(store);
    }

    /// A uniform current from `from` (scenario unix s) for `duration_s` (none: to the end), in
    /// place of the field's.
    pub fn set_uniform(&self, east: f64, north: f64, from: f64, duration_s: Option<f64>) {
        *self.uniform.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Uniform { east, north, from, until: duration_s.map(|d| from + d) });
    }

    /// The current (east, north) m/s at a point and scenario time; None: still water (no field
    /// there, no fault).
    pub fn current(&self, lat: f64, lon: f64, t: f64) -> Option<(f64, f64)> {
        let u = *self.uniform.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = u.filter(|u| t >= u.from && u.until.is_none_or(|e| t < e)) {
            return Some((u.east, u.north));
        }
        self.field.as_ref()?.at(lat, lon, t)
    }

    /// What tidewater's `/v1/sample` would answer a vehicle at a point (`conditions::Sample`):
    /// elevation and water level from the grids (outside them, none: out of the area), the
    /// current (0 on land, as tidewater gives), the wind.
    pub fn sample(&self, lat: f64, lon: f64, t: f64) -> Sample {
        let depth = self.depth.as_ref().and_then(|g| g.height(lat, lon));
        let surface = self.surface.as_ref().and_then(|g| g.height(lat, lon));
        let (elevation, level) = match (depth, surface) {
            (None, _) => (None, None),
            // water: the surface is the water's; the bed is the depth below it
            (Some(d), Some(s)) if d > 0.0 => (Some(s - d), Some(s)),
            // land: the surface is the ground's; the water level is the depth (<= 0) above it
            (Some(d), Some(s)) => (Some(s), Some(s + d)),
            // no surface grid: the water level at the datum
            (Some(d), None) => (Some(-d), Some(0.0)),
        };
        let current = if depth.is_some_and(|d| d > 0.0) { self.current(lat, lon, t).unwrap_or((0.0, 0.0)) } else { (0.0, 0.0) };
        let weather = self.weather.as_ref().map(|w| w.at(lat,lon,t,0.0).values);
        Sample {
            lat,
            lon,
            elevation_m: elevation,
            water_level_m: level,
            water_depth_m: depth,
            current_east: Some(current.0),
            current_north: Some(current.1),
            wind_east: weather.and_then(|w| w.wind_east_mps).or(self.wind_en.map(|w| w[0])),
            wind_north: weather.and_then(|w| w.wind_north_mps).or(self.wind_en.map(|w| w[1])),
        }
    }
}
