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
    /// While `down_when` closes this link, carry the traffic on this other profile instead of
    /// dropping it: the vehicle's second radio (an AUV's Iridium or RF link at the surface).
    /// The other profile may not have `down_when`, `space_class` or a `gated_profile` of its
    /// own; its terrain is checked only if this profile checks terrain too.
    pub gated_profile: Option<String>,
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
    /// SATCOM only: id of a murmur-space constellation class (`starlink`, `iridium`, `wgs`,
    /// `ku-geo`). The link then follows real satellite visibility for the vehicle's position
    /// on the scenario clock: down when no satellite is above the mask, one-way latency from
    /// the best satellite's slant range plus the class's network latency, handovers when the
    /// best satellite changes. The profile's own `latency_ms` is the fallback while there is
    /// no answer (or no element sets), and where the class has no latency model (Iridium).
    pub space_class: Option<String>,
    /// SATCOM only: how long the link drops when the best satellite changes (a GUESS in the
    /// shipped profiles; real figures are not public). 0 (default) means handovers are only
    /// counted.
    pub handover_outage_ms: Option<f64>,
    /// The lost-link timer of vehicles on this profile (seconds without a GCS heartbeat
    /// before their GCS failsafe acts). Not used here: `sim/swarm.py` writes it into the
    /// vehicles' parameters. Accepted, inherited through `extends` and checked (> 0).
    pub lost_link_s: Option<f64>,
}

/// The ground station's height above MSL: a number, or `"dem"` for the DEM height at the
/// station (needs tidewater).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum GsAlt {
    Fixed(f64),
    Named(GsAltName),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GsAltName {
    Dem,
}

impl Default for GsAlt {
    fn default() -> Self {
        GsAlt::Fixed(0.0)
    }
}

impl GsAlt {
    /// The configured number; 0 for `"dem"` until it has been resolved.
    pub fn metres(self) -> f64 {
        match self {
            GsAlt::Fixed(m) => m,
            GsAlt::Named(_) => 0.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundStation {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt_m: GsAlt,
}

/// The ground station altitude check against the DEM, at startup.
#[derive(Clone, Debug, PartialEq)]
pub enum GsAltCheck {
    /// `alt_m = "dem"`: the DEM height was used.
    UsedDem(f64),
    /// A fixed altitude within the tolerance of the DEM.
    Agrees { configured: f64, dem: f64 },
    /// A fixed altitude that differs from the DEM by more than the tolerance.
    Differs { configured: f64, dem: f64 },
    /// `"dem"` asked for but no DEM answer: 0 is used.
    DemUnavailable,
    /// Fixed altitude and no DEM to compare with.
    NotChecked,
}

impl GsAltCheck {
    /// A log line, and whether it is a warning.
    pub fn message(&self) -> (bool, String) {
        match self {
            GsAltCheck::UsedDem(d) => (
                false,
                format!("ground station altitude {d:.1} m from the DEM (alt_m = \"dem\")"),
            ),
            GsAltCheck::Agrees { configured, dem } => (
                false,
                format!(
                    "ground station altitude {configured:.1} m agrees with the DEM ({dem:.1} m)"
                ),
            ),
            GsAltCheck::Differs { configured, dem } => (
                true,
                format!(
                    "[ground_station].alt_m = {configured:.1} m but the DEM at the station says {dem:.1} m (more than {GS_ALT_TOLERANCE_M} m apart): the terrain line of sight starts from the wrong height; set alt_m = \"dem\" or fix the number"
                ),
            ),
            GsAltCheck::DemUnavailable => (
                true,
                "[ground_station].alt_m = \"dem\" but no DEM height is available (needs --tidewater and a point inside the DEM): using 0 m"
                    .to_string(),
            ),
            GsAltCheck::NotChecked => (
                false,
                "ground station altitude not checked against the DEM (no tidewater answer)"
                    .to_string(),
            ),
        }
    }
}

/// Metres of difference between the configured station altitude and the DEM before a warning.
pub const GS_ALT_TOLERANCE_M: f64 = 5.0;

/// `[space]`: the SATCOM source (murmur-space) and its refresh cadence.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SpaceCfg {
    /// Constellation catalog and element-set cache (`murmur-space fetch` fills the cache).
    pub catalog: String,
    pub cache: String,
    /// `auto` | `shift` | `direct`: see murmur-space's README (epoch honesty).
    pub epoch: String,
    /// Scenario seconds between recomputations per vehicle.
    pub interval_s: f64,
    /// Lower bound on wall seconds between recomputation rounds (at a high `MURMUR_RATE`
    /// the scenario interval would otherwise be a busy loop).
    pub min_wall_s: f64,
    /// How far ahead (scenario seconds) to look for the next outage or pass, and how often
    /// (scenario seconds) to refresh that. 0 disables the look-ahead.
    pub outlook_s: f64,
    pub outlook_every_s: f64,
}

impl Default for SpaceCfg {
    fn default() -> Self {
        SpaceCfg {
            catalog: "sim/space/constellations.toml".into(),
            cache: "sim/space/cache".into(),
            epoch: "auto".into(),
            interval_s: 5.0,
            min_wall_s: 0.5,
            outlook_s: 1800.0,
            outlook_every_s: 60.0,
        }
    }
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
    /// GNSS effect (jam or spoof) for the stand-in engine, which reads the same file
    /// (crates/murmur-standins/src/gnss.rs). Not used here, except that an event with one
    /// and a circle no longer defaults to loss 1: a GNSS jammer leaves the datalink alone
    /// unless `loss` or `cut` says otherwise.
    pub gnss: Option<toml::Value>,
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
    pub space: SpaceCfg,
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
    /// SATCOM class (sim/platforms.toml `space_class`, carried in the fleet manifest) ->
    /// profile name. Used for a `satcom` vehicle with no `platform_profile` entry, so the
    /// platform catalog is the one place that says which constellation a platform uses.
    #[serde(default)]
    pub space_class_profile: BTreeMap<String, String>,
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

    /// Settles the ground station altitude against the DEM height at the station (`None`: no
    /// DEM). With `alt_m = "dem"` the DEM height is used; with a number it is kept and the
    /// result says whether it agrees within [`GS_ALT_TOLERANCE_M`]. The profiles' antenna
    /// heights (`gs_antenna_m`, the mast) are added on top of whichever it is.
    pub fn resolve_ground_alt(&mut self, dem_m: Option<f64>) -> Option<GsAltCheck> {
        let gs = self.ground_station.as_mut()?;
        Some(match (gs.alt_m, dem_m) {
            (GsAlt::Named(_), Some(d)) => {
                gs.alt_m = GsAlt::Fixed(d);
                GsAltCheck::UsedDem(d)
            }
            (GsAlt::Named(_), None) => {
                gs.alt_m = GsAlt::Fixed(0.0);
                GsAltCheck::DemUnavailable
            }
            (GsAlt::Fixed(c), Some(d)) if (c - d).abs() > GS_ALT_TOLERANCE_M => {
                GsAltCheck::Differs {
                    configured: c,
                    dem: d,
                }
            }
            (GsAlt::Fixed(c), Some(d)) => GsAltCheck::Agrees {
                configured: c,
                dem: d,
            },
            (GsAlt::Fixed(_), None) => GsAltCheck::NotChecked,
        })
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
            let r = self.resolve(name)?;
            let _ = p;
            if let Some(t) = r.lost_link_s
                && !(t.is_finite() && t > 0.0)
            {
                return Err(format!("profile {name}: lost_link_s must be > 0, not {t}"));
            }
            if let Some(g) = &r.gated_profile {
                if r.down_when.is_none() {
                    return Err(format!("profile {name}: gated_profile needs down_when"));
                }
                let gp = self
                    .resolve(g)
                    .map_err(|e| format!("profile {name}: gated_profile: {e}"))?;
                if gp.down_when.is_some()
                    || gp.gated_profile.is_some()
                    || gp.space_class.as_deref().is_some_and(|s| !s.is_empty())
                {
                    return Err(format!(
                        "profile {name}: gated_profile {g} may not have down_when, gated_profile or space_class"
                    ));
                }
            }
        }
        for (plat, prof) in &self.platform_profile {
            if !self.class.contains_key(prof) {
                return Err(format!("platform_profile {plat}: unknown profile {prof}"));
            }
        }
        for (class, prof) in &self.space_class_profile {
            if !self.class.contains_key(prof) {
                return Err(format!(
                    "space_class_profile {class}: unknown profile {prof}"
                ));
            }
            let got = self.resolve(prof)?.space_class;
            if got.as_deref() != Some(class.as_str()) {
                return Err(format!(
                    "space_class_profile {class}: profile {prof} follows {got:?}, not {class}"
                ));
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
                gated_profile,
                terrain,
                terrain_ramp_m,
                gs_antenna_m,
                antenna_m,
                k_factor,
                fresnel_mhz,
                fresnel_clear,
                propagation_mps,
                space_class,
                handover_outage_ms,
                lost_link_s
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
    /// The platform's SATCOM class from the catalog (`space_class`); picks the profile of a
    /// `satcom` vehicle through `[space_class_profile]`.
    #[serde(default)]
    pub space_class: Option<String>,
}

impl Config {
    /// The profile a vehicle uses: an explicit `[platform_profile]` entry, else (SATCOM
    /// vehicles) the profile for its catalog `space_class`, else its link class.
    pub fn profile_for(&self, v: &VehicleSpec) -> String {
        if let Some(p) = self.platform_profile.get(&v.platform) {
            return p.clone();
        }
        if v.link == "satcom"
            && let Some(p) = v
                .space_class
                .as_ref()
                .and_then(|c| self.space_class_profile.get(c))
        {
            return p.clone();
        }
        v.link.clone()
    }
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

    /// The example GNSS scenario (shared with the stand-in engine) is a valid scenario file.
    #[test]
    fn gnss_example_scenario_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut cfg = Config::load(&dir.join("sim/links.toml")).expect("links.toml");
        let before = cfg.event.len();
        cfg.merge_scenario(&dir.join("sim/events/golden-gate-gnss.toml"))
            .expect("scenario");
        assert_eq!(cfg.event.len(), before + 1);
        assert!(cfg.event.iter().any(|e| e.gnss.is_some()));
    }

    /// Every profile a vehicle can start on carries a lost-link timer (sim/swarm.py writes it
    /// into its parameters); SATCOM GEO waits minutes, a LOS radio seconds.
    #[test]
    fn shipped_profiles_carry_lost_link_timers() {
        let c = Config::parse(include_str!("../../../sim/links.toml")).expect("sim/links.toml");
        let t = |p: &str| c.resolve(p).expect(p).lost_link_s;
        assert_eq!(t("los"), Some(5.0));
        assert_eq!(t("satcom"), Some(120.0));
        assert_eq!(t("satcom_ku_geo"), Some(120.0)); // inherited
        for p in ["mesh", "tether", "satcom_leo", "satcom_iridium", "acoustic"] {
            assert!(t(p).is_some(), "{p}");
        }
        assert!(Config::parse("[class.a]\nlost_link_s = 0\n").is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_cycles() {
        assert!(Config::parse("[class.a]\nlosss = 1\n").is_err());
        assert!(Config::parse("[class.a]\nextends = \"b\"\n[class.b]\nextends = \"a\"\n").is_err());
    }
    #[test]
    fn ground_station_altitude_number_or_dem() {
        let num = Config::parse("[ground_station]\nlat = 1\nlon = 2\nalt_m = 5.0\n").expect("num");
        assert_eq!(num.ground_station.expect("gs").alt_m, GsAlt::Fixed(5.0));
        let none = Config::parse("[ground_station]\nlat = 1\nlon = 2\n").expect("none");
        assert_eq!(none.ground_station.expect("gs").alt_m, GsAlt::Fixed(0.0));
        assert!(Config::parse("[ground_station]\nlat = 1\nlon = 2\nalt_m = \"tall\"\n").is_err());

        let toml = "[ground_station]\nlat = 1\nlon = 2\nalt_m = \"dem\"\n";
        let mut dem = Config::parse(toml).expect("dem");
        assert_eq!(
            dem.resolve_ground_alt(Some(4.8)),
            Some(GsAltCheck::UsedDem(4.8))
        );
        assert_eq!(dem.ground_station.as_ref().expect("gs").alt_m.metres(), 4.8);
        let mut dem = Config::parse(toml).expect("dem");
        assert_eq!(
            dem.resolve_ground_alt(None),
            Some(GsAltCheck::DemUnavailable)
        );
        assert_eq!(dem.ground_station.as_ref().expect("gs").alt_m.metres(), 0.0);
        assert!(
            Config::parse("seed = 1\n")
                .expect("no gs")
                .resolve_ground_alt(Some(1.0))
                .is_none()
        );
    }

    #[test]
    fn ground_station_altitude_warns_beyond_five_metres() {
        let mk = |alt: f64| {
            Config::parse(&format!(
                "[ground_station]\nlat = 1\nlon = 2\nalt_m = {alt}\n"
            ))
            .expect("cfg")
        };
        // Within 5 m: agrees, no warning, and the configured number is kept.
        let mut c = mk(5.0);
        let r = c.resolve_ground_alt(Some(4.8)).expect("check");
        assert_eq!(
            r,
            GsAltCheck::Agrees {
                configured: 5.0,
                dem: 4.8
            }
        );
        assert!(!r.message().0);
        let r = mk(9.5).resolve_ground_alt(Some(4.8)).expect("check");
        assert!(
            matches!(r, GsAltCheck::Agrees { .. }),
            "4.7 m apart is fine"
        );
        // More than 5 m: a warning, and the configured number is still what is used.
        let mut c = mk(30.0);
        let r = c.resolve_ground_alt(Some(4.8)).expect("check");
        assert_eq!(
            r,
            GsAltCheck::Differs {
                configured: 30.0,
                dem: 4.8
            }
        );
        assert!(r.message().0);
        assert_eq!(c.ground_station.as_ref().expect("gs").alt_m.metres(), 30.0);
        assert_eq!(
            mk(30.0).resolve_ground_alt(None),
            Some(GsAltCheck::NotChecked)
        );
    }

    #[test]
    fn space_keys_and_section_parse_and_inherit() {
        let c = Config::parse(
            r#"
            [space]
            interval_s = 10
            [class.a]
            space_class = "wgs"
            handover_outage_ms = 5
            [class.b]
            extends = "a"
            space_class = "starlink"
            "#,
        )
        .expect("parse");
        assert_eq!(c.space.interval_s, 10.0);
        assert_eq!(c.space.outlook_every_s, 60.0);
        let b = c.resolve("b").expect("b");
        assert_eq!(b.space_class.as_deref(), Some("starlink"));
        assert_eq!(b.handover_outage_ms, Some(5.0));
    }

    #[test]
    fn explicit_platform_profile_beats_space_class() {
        let c = Config::parse(
            r#"
            [class.satcom]
            space_class = "wgs"
            [class.satcom_leo]
            extends = "satcom"
            space_class = "starlink"
            [space_class_profile]
            wgs = "satcom"
            starlink = "satcom_leo"
            [platform_profile]
            odd = "satcom"
            "#,
        )
        .expect("parse");
        let v = |platform: &str, class: Option<&str>| VehicleSpec {
            uid: 1,
            sysid: 1,
            port: 1,
            link: "satcom".into(),
            domain: String::new(),
            platform: platform.into(),
            callsign: String::new(),
            space_class: class.map(String::from),
        };
        assert_eq!(c.profile_for(&v("a", Some("starlink"))), "satcom_leo");
        assert_eq!(c.profile_for(&v("a", None)), "satcom");
        assert_eq!(c.profile_for(&v("odd", Some("starlink"))), "satcom");
    }

    #[test]
    fn space_class_profile_must_follow_that_class() {
        let e = Config::parse(
            "[class.satcom]\nspace_class = \"wgs\"\n[space_class_profile]\nstarlink = \"satcom\"\n",
        )
        .expect_err("must be rejected");
        assert!(e.contains("starlink"), "{e}");
    }

    #[test]
    fn shipped_event_example_loads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut c = Config::load(&root.join("sim/links.toml")).expect("sim/links.toml");
        c.merge_scenario(&root.join("sim/link-events/golden-gate.toml"))
            .expect("sim/link-events/golden-gate.toml");
        assert_eq!(c.event.len(), 4);
        assert_eq!(
            c.resolve("acoustic").expect("acoustic").gated_profile.as_deref(),
            Some("acoustic_surface")
        );
    }

    #[test]
    fn shipped_links_file_maps_satcom_platforms_to_space_classes() {
        let c = Config::parse(include_str!("../../../sim/links.toml")).expect("sim/links.toml");
        let platforms: toml::Table = include_str!("../../../sim/platforms.toml")
            .parse()
            .expect("sim/platforms.toml");
        let class_of = |plat: &str| {
            let spec = &platforms[plat];
            assert_eq!(spec["link"].as_str(), Some("satcom"), "{plat}");
            let v = VehicleSpec {
                uid: 1,
                sysid: 1,
                port: 1,
                link: "satcom".into(),
                domain: String::new(),
                platform: plat.into(),
                callsign: String::new(),
                space_class: spec["space_class"].as_str().map(String::from),
            };
            c.resolve(&c.profile_for(&v)).expect("profile").space_class
        };
        assert_eq!(class_of("buoy").as_deref(), Some("iridium"));
        for p in ["magura", "corsair", "seahunter", "saildrone"] {
            assert_eq!(class_of(p).as_deref(), Some("starlink"), "{p}");
        }
        for p in ["rq4", "mq9b"] {
            assert_eq!(class_of(p).as_deref(), Some("wgs"), "{p}");
        }
        assert_eq!(
            c.resolve("satcom_ku_geo")
                .expect("ku")
                .space_class
                .as_deref(),
            Some("ku-geo")
        );
    }
}
