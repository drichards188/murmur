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
    /// Start, seconds since the emulator started.
    pub at_s: f64,
    /// Duration in seconds; omitted means until the end of the run.
    pub duration_s: Option<f64>,
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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
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
        Ok(())
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
            if e.at_s < 0.0 || e.duration_s.is_some_and(|d| d < 0.0) {
                return Err(format!("event {:?}: negative time", e.name));
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
