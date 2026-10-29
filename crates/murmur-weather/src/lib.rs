//! Replayable surface weather. No network, forecasts, contact truth or radio outages.
//! Archives use UTC unix seconds and SI units; precipitation is the preceding hour's total.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Values {
    pub visibility_m: Option<f64>,
    pub wind_east_mps: Option<f64>,
    pub wind_north_mps: Option<f64>,
    pub gust_mps: Option<f64>,
    pub precipitation_mm: Option<f64>,
    pub cloud_cover_pct: Option<f64>,
    pub temperature_c: Option<f64>,
    pub humidity_pct: Option<f64>,
    pub pressure_hpa: Option<f64>,
}
impl Values {
    fn numbers(self) -> [Option<f64>; 9] {
        [
            self.visibility_m,
            self.wind_east_mps,
            self.wind_north_mps,
            self.gust_mps,
            self.precipitation_mm,
            self.cloud_cover_pct,
            self.temperature_c,
            self.humidity_pct,
            self.pressure_hpa,
        ]
    }
    pub fn validate(self) -> Result<(), String> {
        if self.numbers().into_iter().flatten().any(|v| !v.is_finite())
            || [
                self.visibility_m,
                self.gust_mps,
                self.precipitation_mm,
                self.pressure_hpa,
            ]
            .into_iter()
            .flatten()
            .any(|v| v < 0.0)
            || [self.cloud_cover_pct, self.humidity_pct]
                .into_iter()
                .flatten()
                .any(|v| !(0.0..=100.0).contains(&v))
        {
            return Err("weather values must be finite, nonnegative where required, and percentages within 0..100".into());
        }
        Ok(())
    }
    pub fn wind_mps(self) -> Option<f64> {
        Some(self.wind_east_mps?.hypot(self.wind_north_mps?))
    }
    fn mix(self, b: Self, f: f64, interval: bool) -> Self {
        let lerp = |a: Option<f64>, b: Option<f64>| {
            if f == 0.0 {
                a
            } else if f == 1.0 {
                b
            } else {
                a.zip(b).map(|(a, b)| a + (b - a) * f)
            }
        };
        Self {
            visibility_m: lerp(self.visibility_m, b.visibility_m),
            wind_east_mps: lerp(self.wind_east_mps, b.wind_east_mps),
            wind_north_mps: lerp(self.wind_north_mps, b.wind_north_mps),
            gust_mps: if interval && f > 0.0 { b.gust_mps } else { lerp(self.gust_mps, b.gust_mps) },
            // A total ending at b.t belongs to (a.t,b.t], never interpolate an accumulation.
            precipitation_mm: if interval && f > 0.0 {
                b.precipitation_mm
            } else {
                lerp(self.precipitation_mm, b.precipitation_mm)
            },
            cloud_cover_pct: lerp(self.cloud_cover_pct, b.cloud_cover_pct),
            temperature_c: lerp(self.temperature_c, b.temperature_c),
            humidity_pct: lerp(self.humidity_pct, b.humidity_pct),
            pressure_hpa: lerp(self.pressure_hpa, b.pressure_hpa),
        }
    }
    fn overlay(self, v: Self) -> Self {
        Self {
            visibility_m: v.visibility_m.or(self.visibility_m),
            wind_east_mps: v.wind_east_mps.or(self.wind_east_mps),
            wind_north_mps: v.wind_north_mps.or(self.wind_north_mps),
            gust_mps: v.gust_mps.or(self.gust_mps),
            precipitation_mm: v.precipitation_mm.or(self.precipitation_mm),
            cloud_cover_pct: v.cloud_cover_pct.or(self.cloud_cover_pct),
            temperature_c: v.temperature_c.or(self.temperature_c),
            humidity_pct: v.humidity_pct.or(self.humidity_pct),
            pressure_hpa: v.pressure_hpa.or(self.pressure_hpa),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub at_s: f64,
    pub values: Values,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// File path relative to the repository. Loaded once, never refreshed during a run.
    pub archive: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    /// Uniform synthetic overrides at seconds since contacts start. Hold last frame.
    pub timeline: Vec<Frame>,
}
impl Config {
    /// Pin the artifact in the truth log; readers reject a different file at the same path.
    pub fn pin_archive(&mut self, root: &Path) -> Result<(), String> {
        if let Some(path) = &self.archive {
            let path = root.join(path);
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let digest = format!("{:x}", Sha256::digest(&bytes));
            if self.archive_sha256.as_ref().is_some_and(|expected| *expected != digest) {
                return Err("weather archive checksum mismatch".into());
            }
            self.archive_sha256 = Some(digest);
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut last = -1.0;
        for f in &self.timeline {
            if !f.at_s.is_finite() || f.at_s < 0.0 || f.at_s <= last {
                return Err("weather timeline must have increasing, nonnegative at_s".into());
            }
            f.values.validate()?;
            if f.values.wind_east_mps.is_some() || f.values.wind_north_mps.is_some() {
                return Err("mean wind must come from the shared archive; timeline wind overrides are unsupported".into());
            }
            last = f.at_s;
        }
        Ok(())
    }
    fn synthetic(&self, s: f64) -> Option<Values> {
        let i = self.timeline.partition_point(|f| f.at_s <= s);
        if i == 0 {
            return None;
        }
        let a = &self.timeline[i - 1];
        Some(match self.timeline.get(i) {
            Some(b) => a
                .values
                .mix(b.values, (s - a.at_s) / (b.at_s - a.at_s), false),
            None => a.values,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub t: f64,
    pub values: Values,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
    pub rows: Vec<Row>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Archive {
    pub schema: u32,
    pub source: String,
    pub model: String,
    pub fetched_at: String,
    pub raw_sha256: String,
    pub bounds: [f64; 4],
    pub points: Vec<Point>,
}
impl Archive {
    pub fn validate(&self) -> Result<(), String> {
        let [w, s, e, n] = self.bounds;
        if self.schema != 1
            || !valid_point(s, w)
            || !valid_point(n, e)
            || w >= e
            || s >= n
            || self.points.is_empty()
        {
            return Err("unsupported weather schema or invalid weather bounds/points".into());
        }
        for p in &self.points {
            if !valid_point(p.lat, p.lon) || p.rows.is_empty() {
                return Err("invalid weather point".into());
            }
            let mut last = f64::NEG_INFINITY;
            for r in &p.rows {
                if !r.t.is_finite() || r.t <= last {
                    return Err("weather timestamps must increase".into());
                }
                r.values.validate()?;
                last = r.t;
            }
        }
        Ok(())
    }
    pub fn at(&self, lat: f64, lon: f64, t: f64) -> Option<Values> {
        let [w, s, e, n] = self.bounds;
        if !valid_point(lat, lon) || lat < s || lat > n || lon < w || lon > e || !t.is_finite() {
            return None;
        }
        // Nearest requested grid point: don't manufacture sub-grid detail in a 3 km model.
        let d =
            |p: &Point| (p.lat - lat).powi(2) + ((p.lon - lon) * lat.to_radians().cos()).powi(2);
        let p = self.points.iter().min_by(|a, b| d(a).total_cmp(&d(b)))?;
        let i = p.rows.partition_point(|r| r.t <= t);
        if i == 0 {
            return None;
        }
        let a = &p.rows[i - 1];
        if a.t == t {
            return Some(a.values);
        }
        let b = p.rows.get(i)?; // no extrapolation beyond the archive
        if b.t - a.t > 3600.01 {
            return None;
        } // no bridging missing hours
        Some(a.values.mix(b.values, (t - a.t) / (b.t - a.t), true))
    }
}
pub fn valid_point(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0
}
#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub values: Values,
    pub source: String,
    pub model: Option<String>,
    pub archive_sha256: Option<String>,
    pub synthetic: bool,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct Store {
    config: Option<Config>,
    archive: Option<Archive>,
    digest: Option<String>,
    error: Option<String>,
}
impl Store {
    pub fn configure(&mut self, config: Option<&Config>, root: Option<&Path>) {
        if self.config.as_ref() == config {
            return;
        }
        self.config = config.cloned();
        self.archive = None;
        self.digest = None;
        self.error = None;
        if let Some(c) = config {
            let result = (|| {
                c.validate()?;
                if let Some(path) = &c.archive {
                    let path = root.unwrap_or_else(|| Path::new(".")).join(path);
                    let bytes =
                        std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    let a: Archive = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    a.validate()?;
                    let digest = format!("{:x}", Sha256::digest(&bytes));
                    if c.archive_sha256.as_ref().is_some_and(|expected| *expected != digest) {
                        return Err("weather archive checksum mismatch".into());
                    }
                    self.digest = Some(digest);
                    self.archive = Some(a);
                }
                Ok::<_, String>(())
            })();
            if let Err(e) = result {
                self.error = Some(e);
            }
        }
    }
    pub fn at(&self, lat: f64, lon: f64, t: f64, s: f64) -> Sample {
        let archived = self.archive.as_ref().and_then(|a| a.at(lat, lon, t));
        let synthetic = self.config.as_ref().and_then(|c| c.synthetic(s));
        let mut error = self.error.clone();
        if self.archive.is_some() && archived.is_none() {
            error = Some("Outside archived weather coverage or a missing hour".into());
        }
        Sample {
            values: archived
                .unwrap_or_default()
                .overlay(synthetic.unwrap_or_default()),
            source: if synthetic.is_some() {
                "scripted scenario weather".into()
            } else {
                self.archive
                    .as_ref()
                    .map_or("unavailable".into(), |a| a.source.clone())
            },
            model: self.archive.as_ref().map(|a| a.model.clone()),
            archive_sha256: self.digest.clone(),
            synthetic: synthetic.is_some(),
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive() -> Archive {
        serde_json::from_str(include_str!("../tests/data/weather.json")).expect("fixture")
    }
    #[test]
    fn archive_interpolation_missing_fields_intervals_and_bounds() {
        let a = archive();
        a.validate().expect("valid");
        let v = a.at(37.5, -122.5, 1800.0).expect("covered");
        assert_eq!(v.visibility_m, Some(5500.0));
        assert_eq!((v.wind_east_mps, v.wind_north_mps), (Some(1.0), Some(1.0)));
        assert_eq!(v.precipitation_mm, Some(4.0));
        assert_eq!(v.humidity_pct, None);
        assert_eq!(
            a.at(37.5, -122.5, 0.0).expect("edge").precipitation_mm,
            Some(1.0)
        );
        for (lat, t) in [(36.0, 1800.0), (37.5, -1.0), (37.5, 7201.0)] {
            assert!(a.at(lat, -122.5, t).is_none());
        }
        let mut gap = archive();
        gap.points[0].rows.remove(1);
        assert!(gap.at(37.5, -122.5, 1800.0).is_none());
    }
    #[test]
    fn timeline_ramps_holds_and_does_not_invent_missing_weather() {
        let c = Config {
            archive: None,
            archive_sha256: None,
            timeline: vec![
                Frame {
                    at_s: 10.0,
                    values: Values {
                        visibility_m: Some(10000.0),
                        ..Default::default()
                    },
                },
                Frame {
                    at_s: 20.0,
                    values: Values {
                        visibility_m: Some(50.0),
                        ..Default::default()
                    },
                },
            ],
        };
        c.validate().expect("valid");
        let mut store = Store::default();
        store.configure(Some(&c), None);
        assert_eq!(store.at(37.5, -122.5, 100.0, 0.0).values.visibility_m, None);
        let v = store.at(37.5, -122.5, 100.0, 15.0);
        assert_eq!(v.values.visibility_m, Some(5025.0));
        assert!(v.synthetic);
        assert_eq!(v.values.precipitation_mm, None);
        assert_eq!(
            store.at(37.5, -122.5, 100.0, 50.0).values.visibility_m,
            Some(50.0)
        );
        let mut bad = c;
        bad.timeline[1].at_s = 10.0;
        assert!(bad.validate().is_err());
    }
    #[test]
    fn missing_archive_is_explicit() {
        let mut s = Store::default();
        s.configure(
            Some(&Config {
                archive: Some("/no-such-weather.json".into()),
                ..Default::default()
            }),
            None,
        );
        let w = s.at(37.5, -122.5, 0.0, 0.0);
        assert!(w.error.is_some());
        assert_eq!(w.values, Values::default());
    }
}
