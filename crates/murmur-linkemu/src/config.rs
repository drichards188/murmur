//! Configuration: link profiles, scripted events and the fleet manifest.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownWhen {
    /// The link is down while the vehicle is below the surface (SATCOM, RF).
    Submerged,
    /// The link is down while the vehicle is at the surface (acoustic modem).
    Surfaced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerrainMode {
    /// No terrain check for this profile.
    Off,
    /// Drop everything while the path is obstructed.
    Block,
    /// Loss rises with obstruction depth: 0 at the ray, 1 at `terrain_ramp_m` deep.
    Ramp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirSel {
    Both,
    Up,
    Down,
}

/// A link profile as written in the file. Every field is optional so profiles can `extends`
/// another one and override a few numbers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProfile {
    pub extends: Option<String>,
    /// Long-run average loss probability (0..1).
    pub loss: Option<f64>,
    /// Mean length of a loss burst in packets (Gilbert-Elliott). 1 or less means independent.
    pub burst_len: Option<f64>,
    /// Loss probability inside the good and bad states (defaults 0 and 1, the Gilbert model).
    pub loss_good: Option<f64>,
    pub loss_bad: Option<f64>,
    /// One-way base latency and uniform +-jitter, in milliseconds.
    pub latency_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    /// Bandwidth cap per direction in bits per second; 0 means unlimited.
    pub bandwidth_bps: Option<u64>,
    /// Override for the bridge to vehicle direction.
    pub down_bandwidth_bps: Option<u64>,
    /// Drop-tail queue size in bytes. Default: a quarter second of line rate (min 1500).
    pub queue_bytes: Option<usize>,
    /// Probability that a delivered packet is delivered twice.
    pub duplicate: Option<f64>,
    /// Allow jitter to reorder packets. Default false.
    pub reorder: Option<bool>,
    /// Hard cap on packets held in flight per direction (bounds memory).
    pub max_inflight: Option<usize>,
    /// Range-dependent loss from the ground station: none up to `range_ok_km`, rising to
    /// certain loss at `range_max_km` as ((d - ok) / (max - ok)) ^ range_exp.
    pub range_ok_km: Option<f64>,
    pub range_max_km: Option<f64>,
    pub range_exp: Option<f64>,
    pub down_when: Option<DownWhen>,
    /// Terrain line of sight to the ground station (needs a tidewater URL). Default: `block`
    /// for profiles with a range model, `off` otherwise.
    pub terrain: Option<TerrainMode>,
    /// `ramp` mode: obstruction depth in metres at which loss reaches 1 (default 30).
    pub terrain_ramp_m: Option<f64>,
    /// Ground station antenna height above `[ground_station].alt_m`, metres (default 0).
    pub gs_antenna_m: Option<f64>,
    /// Vehicle antenna height above its reported altitude, metres (default 0).
    pub antenna_m: Option<f64>,
    /// Effective Earth radius factor for radio refraction (default 4/3).
    pub k_factor: Option<f64>,
    /// Carrier frequency in MHz; with it the path must also clear `fresnel_clear` of the
    /// first Fresnel zone. Omitted means a bare line of sight.
    pub fresnel_mhz: Option<f64>,
    /// Fraction of the first Fresnel radius to keep clear (default 0.6).
    pub fresnel_clear: Option<f64>,
    /// Adds slant range to the ground station divided by this speed to the latency
    /// (1500 for sound in water).
    pub propagation_mps: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundStation {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt_m: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Circle {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCfg {
    pub name: Option<String>,
    /// Start, seconds since the emulator started. Give this or `at_scenario`.
    pub at_s: Option<f64>,
    /// Start in scenario time, ISO 8601 (`2026-06-14T10:05:00Z`); needs `MURMUR_AT`.
    pub at_scenario: Option<String>,
    /// Duration; omitted means until the end of the run. Seconds of the same clock as the
    /// start: emulator seconds with `at_s`, scenario seconds with `at_scenario`.
    pub duration_s: Option<f64>,
    /// End in scenario time (alternative to `duration_s` with `at_scenario`).
    pub until_scenario: Option<String>,
    // Selectors (all that are present must match).
    pub class: Option<String>,
    pub profile: Option<String>,
    pub platform: Option<String>,
    pub domain: Option<String>,
    pub uid: Option<u32>,
    pub uids: Option<Vec<u32>>,
    /// Only vehicles inside this circle are affected (a jammer).
    pub circle: Option<Circle>,
    #[serde(default = "default_dir")]
    pub dir: DirSel,
    // Effects.
    /// Extra independent loss probability. With a circle and no loss given, defaults to 1.
    pub loss: Option<f64>,
    #[serde(default)]
    pub cut: bool,
    #[serde(default)]
    pub extra_latency_ms: f64,
}

fn default_dir() -> DirSel {
    DirSel::Both
}

/// Settings of the tidewater terrain service client (`[terrain]` in the links file).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerrainCfg {
    /// How often pending paths are sent to tidewater, seconds.
    pub interval_s: f64,
    /// Size of the grid cell a vehicle position is rounded to, metres. One profile is
    /// computed per cell (and altitude band) and cached for good.
    pub cell_m: f64,
    /// Altitude band height, metres.
    pub alt_step_m: f64,
    /// Distance between profile samples, metres (the DEM is about 15 m).
    pub sample_spacing_m: f64,
    /// Cap on samples per profile.
    pub max_points: usize,
    /// Most cells computed per interval; the rest wait.
    pub max_cells_per_tick: usize,
    /// Points per HTTP request.
    pub max_points_per_request: usize,
    /// Cache size limit in cells (the cache is cleared when it is exceeded).
    pub cache_max: usize,
    /// Seconds before a request to tidewater is given up.
    pub timeout_s: f64,
}

impl Default for TerrainCfg {
    fn default() -> Self {
        TerrainCfg {
            interval_s: 1.0,
            cell_m: 250.0,
            alt_step_m: 10.0,
            sample_spacing_m: 50.0,
            max_points: 500,
            max_cells_per_tick: 400,
            max_points_per_request: 20_000,
            cache_max: 500_000,
            timeout_s: 10.0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub terrain: TerrainCfg,
    #[serde(default)]
    pub seed: Option<u64>,
    /// Depth in metres below which a vehicle counts as submerged.
    #[serde(default = "default_submerged_depth")]
    pub submerged_depth_m: f64,
    pub ground_station: Option<GroundStation>,
    #[serde(default)]
    pub class: BTreeMap<String, RawProfile>,
    /// platform id -> profile name, to give one platform a different profile than its class.
    #[serde(default)]
    pub platform_profile: BTreeMap<String, String>,
    #[serde(default)]
    pub event: Vec<EventCfg>,
}

fn default_submerged_depth() -> f64 {
    1.0
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Adds the events of a separate scenario file.
    pub fn merge_scenario(&mut self, path: &Path) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Scenario {
            #[serde(default)]
            event: Vec<EventCfg>,
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let s: Scenario = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.event.extend(s.event);
        self.validate()
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The config's events followed by the scenario file's, read fresh from disk (for
    /// reloading while running; profiles are not reloaded).
    pub fn load_events(config: &Path, scenario: Option<&Path>) -> Result<Vec<EventCfg>, String> {
        let mut cfg = Config::load(config)?;
        if let Some(s) = scenario {
            cfg.merge_scenario(s)?;
        }
        Ok(cfg.event)
    }

    fn validate(&self) -> Result<(), String> {
        for (name, p) in &self.class {
            self.resolve(name)?;
            let _ = p;
        }
        for (plat, prof) in &self.platform_profile {
            if !self.class.contains_key(prof) {
                return Err(format!("platform_profile {plat}: unknown profile {prof}"));
            }
        }
        for e in &self.event {
            if let Some(l) = e.loss
                && !(0.0..=1.0).contains(&l)
            {
                return Err(format!("event {:?}: loss {l} outside 0..1", e.name));
            }
            match (&e.at_s, &e.at_scenario) {
                (Some(_), Some(_)) | (None, None) => {
                    return Err(format!(
                        "event {:?}: give exactly one of at_s and at_scenario",
                        e.name
                    ));
                }
                _ => {}
            }
            if e.at_s.is_some_and(|a| a < 0.0) || e.duration_s.is_some_and(|d| d < 0.0) {
                return Err(format!("event {:?}: negative time", e.name));
            }
            if e.at_scenario.is_none() && e.until_scenario.is_some() {
                return Err(format!(
                    "event {:?}: until_scenario needs at_scenario",
                    e.name
                ));
            }
            if e.duration_s.is_some() && e.until_scenario.is_some() {
                return Err(format!(
                    "event {:?}: give duration_s or until_scenario, not both",
                    e.name
                ));
            }
            for t in [&e.at_scenario, &e.until_scenario].into_iter().flatten() {
                crate::timeline::parse_iso_utc(t)
                    .map_err(|m| format!("event {:?}: {m}", e.name))?;
            }
        }
        Ok(())
    }

    /// Flattens a profile and its `extends` chain.
    pub fn resolve(&self, name: &str) -> Result<RawProfile, String> {
        let mut chain = Vec::new();
        let mut cur = name.to_string();
        loop {
            if chain.len() > 8 {
                return Err(format!("profile {name}: extends chain too long or cyclic"));
            }
            let p = self
                .class
                .get(&cur)
                .ok_or_else(|| format!("unknown link profile {cur:?}"))?;
            chain.push(p);
            match &p.extends {
                Some(next) => cur = next.clone(),
                None => break,
            }
        }
        // Walk from the root so that children override parents.
        let mut out = RawProfile::default();
        for p in chain.into_iter().rev() {
            macro_rules! over {
                ($($f:ident),*) => { $( if p.$f.is_some() { out.$f = p.$f.clone(); } )* };
            }
            over!(
                loss,
                burst_len,
                loss_good,
                loss_bad,
                latency_ms,
                jitter_ms,
                bandwidth_bps,
                down_bandwidth_bps,
                queue_bytes,
                duplicate,
                reorder,
                max_inflight,
                range_ok_km,
                range_max_km,
                range_exp,
                down_when,
                terrain,
                terrain_ramp_m,
                gs_antenna_m,
                antenna_m,
                k_factor,
                fresnel_mhz,
                fresnel_clear,
                propagation_mps
            );
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct VehicleSpec {
    pub uid: u32,
    #[serde(default)]
    pub sysid: u32,
    pub port: u16,
    pub link: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub callsign: String,
}

#[derive(Deserialize)]
struct FleetFile {
    vehicles: Vec<VehicleSpec>,
}

pub fn load_fleet(path: &Path) -> Result<Vec<VehicleSpec>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let f: FleetFile =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(f.vehicles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extends_overrides_parent() {
        let c = Config::parse(
            r#"
            [class.a]
            loss = 0.1
            latency_ms = 50
            [class.b]
            extends = "a"
            loss = 0.2
            "#,
        )
        .expect("parse");
        let b = c.resolve("b").expect("resolve");
        assert_eq!(b.loss, Some(0.2));
        assert_eq!(b.latency_ms, Some(50.0));
    }

    #[test]
    fn rejects_unknown_fields_and_cycles() {
        assert!(Config::parse("[class.a]\nlosss = 1\n").is_err());
        assert!(Config::parse("[class.a]\nextends = \"b\"\n[class.b]\nextends = \"a\"\n").is_err());
    }
}
