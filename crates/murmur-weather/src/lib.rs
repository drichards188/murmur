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
    /// The normalized names of the fields that hold a value.
    pub fn names(self) -> Vec<&'static str> {
        const NAMES: [&str; 9] = [
            "visibility_m",
            "wind_east_mps",
            "wind_north_mps",
            "gust_mps",
            "precipitation_mm",
            "cloud_cover_pct",
            "temperature_c",
            "humidity_pct",
            "pressure_hpa",
        ];
        NAMES
            .into_iter()
            .zip(self.numbers())
            .filter_map(|(n, v)| v.map(|_| n))
            .collect()
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
        self.points.iter().min_by(|a, b| d(a).total_cmp(&d(b)))?.at(t)
    }
}
impl Point {
    /// This point's values at `t`: what every position it is the nearest point to samples.
    fn at(&self, t: f64) -> Option<Values> {
        let p = self;
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
/// One archived point and its values now (`Store::regions`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegionPoint {
    pub lat: f64,
    pub lon: f64,
    pub values: Values,
}
/// The whole field at one instant as the sampler answers it (`Store::at`): a position inside
/// `bounds` takes the values of its nearest point (squared degrees of latitude plus squared
/// degrees of longitude scaled by the cosine of the position's latitude; the first point on a
/// tie), any other position `outside`'s. Scripted values are already overlaid on every one, so
/// a scripted field is the same everywhere. Without an archive `bounds` is `None` and `points`
/// is empty: `outside` holds everywhere. For drawing (bd murmur-vr17); nothing decides on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Regions {
    /// The fields a scripted timeline overrides now, by their normalized names (each the same
    /// everywhere); none before its first frame. Names only: no value, no later frame.
    pub scripted: Vec<&'static str>,
    pub bounds: Option<[f64; 4]>,
    pub points: Vec<RegionPoint>,
    pub outside: Values,
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
    /// The field's regions at `t` (archive time) and `s` (scenario seconds): what `at` answers
    /// anywhere, without sampling a raster.
    pub fn regions(&self, t: f64, s: f64) -> Regions {
        let synthetic = self
            .config
            .as_ref()
            .and_then(|c| c.synthetic(s))
            .unwrap_or_default();
        let over = |v: Option<Values>| v.unwrap_or_default().overlay(synthetic);
        let scripted = synthetic.names();
        match &self.archive {
            Some(a) => Regions {
                scripted,
                bounds: Some(a.bounds),
                points: a
                    .points
                    .iter()
                    .map(|p| RegionPoint {
                        lat: p.lat,
                        lon: p.lon,
                        values: over(if t.is_finite() { p.at(t) } else { None }),
                    })
                    .collect(),
                outside: over(None),
            },
            None => Regions {
                scripted,
                bounds: None,
                points: Vec::new(),
                outside: over(None),
            },
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
    /// The regions answer exactly what `at` samples, everywhere: inside the bounds the nearest
    /// point's values by the sampler's own distance, outside them `outside`, with a scripted
    /// value overlaid on all of them (bd murmur-vr17: the console's map draws these regions).
    #[test]
    fn regions_are_what_the_sampler_samples() {
        // a 3 by 2 grid, wider than tall, each point its own values and one missing an hour
        let row = |t: f64, k: f64| Row {
            t,
            values: Values {
                visibility_m: Some(1000.0 * k + t),
                wind_east_mps: Some(k),
                wind_north_mps: Some(-k),
                cloud_cover_pct: Some(10.0 * k),
                ..Default::default()
            },
        };
        let mut points = Vec::new();
        for (i, lat) in [37.7, 37.9].into_iter().enumerate() {
            for (j, lon) in [-122.6, -122.45, -122.3].into_iter().enumerate() {
                let k = (i * 3 + j) as f64;
                let rows = if k == 4.0 {
                    vec![row(0.0, k), row(7200.0, k)]
                } else {
                    vec![row(0.0, k), row(3600.0, k), row(7200.0, k)]
                };
                points.push(Point { lat, lon, rows });
            }
        }
        let archive = Archive {
            schema: 1,
            source: "test".into(),
            model: "test".into(),
            fetched_at: "now".into(),
            raw_sha256: "x".into(),
            bounds: [-122.65, 37.65, -122.25, 37.95],
            points,
        };
        archive.validate().expect("valid");
        for timeline in [
            vec![],
            vec![Frame {
                at_s: 0.0,
                values: Values {
                    visibility_m: Some(50.0),
                    ..Default::default()
                },
            }],
        ] {
            let mut store = Store {
                config: Some(Config {
                    archive: None,
                    archive_sha256: None,
                    timeline,
                }),
                archive: Some(archive.clone()),
                ..Default::default()
            };
            let [w, s, e, n] = archive.bounds;
            for t in [0.0, 1800.0, 3600.0, 9999.0] {
                let r = store.regions(t, 5.0);
                assert_eq!(r.bounds, Some(archive.bounds));
                for i in 0..=60 {
                    for j in 0..=60 {
                        let lat = s - 0.05 + (n - s + 0.1) * i as f64 / 60.0;
                        let lon = w - 0.05 + (e - w + 0.1) * j as f64 / 60.0;
                        let inside = lat >= s && lat <= n && lon >= w && lon <= e;
                        let want = if inside {
                            let d = |p: &RegionPoint| {
                                (p.lat - lat).powi(2)
                                    + ((p.lon - lon) * lat.to_radians().cos()).powi(2)
                            };
                            r.points
                                .iter()
                                .min_by(|a, b| d(a).total_cmp(&d(b)))
                                .expect("points")
                                .values
                        } else {
                            r.outside
                        };
                        assert_eq!(store.at(lat, lon, t, 5.0).values, want, "{lat} {lon} {t}");
                    }
                }
            }
            store.config = None;
            assert_eq!(store.regions(0.0, 5.0).outside, Values::default());
        }
        let scripted = Store {
            config: Some(Config {
                archive: None,
                archive_sha256: None,
                timeline: vec![Frame {
                    at_s: 0.0,
                    values: Values {
                        visibility_m: Some(50.0),
                        ..Default::default()
                    },
                }],
            }),
            ..Default::default()
        };
        let r = scripted.regions(0.0, 1.0);
        assert_eq!(
            (r.bounds, r.points.len(), r.outside.visibility_m),
            (None, 0, Some(50.0))
        );
        // which fields are scripted, now only: before a timeline's first frame, none
        assert_eq!(r.scripted, vec!["visibility_m"]);
        let later = Store {
            config: Some(Config {
                archive: None,
                archive_sha256: None,
                timeline: vec![Frame {
                    at_s: 100.0,
                    values: Values {
                        cloud_cover_pct: Some(80.0),
                        precipitation_mm: Some(3.0),
                        ..Default::default()
                    },
                }],
            }),
            ..Default::default()
        };
        assert!(later.regions(0.0, 50.0).scripted.is_empty());
        assert_eq!(
            later.regions(0.0, 150.0).scripted,
            vec!["precipitation_mm", "cloud_cover_pct"]
        );
        assert_eq!(scripted.at(37.0, -122.0, 0.0, 1.0).values, r.outside);
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
