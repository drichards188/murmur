//! An episode's specification: the first line the driver (eval/headless/sim.py) writes. The
//! fleet manifest and the stand-ins' configs are `sim/swarm.py`'s own (the driver computes them
//! with its functions); the bridge's configuration comes from the `MURMUR_*` variables the
//! driver sets for this process, as `scripts/up.sh` sets them for a bridge.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The episode seed: the stand-ins' draws (`Vehicle::seeded`) and, unless `linkemu_seed`
    /// says otherwise, the link emulator's (the driver always says: the situation's seed, the
    /// one the full stack's emulator gets for the same situation, MURMUR_LINKEMU_SEED).
    pub seed: u64,
    /// The bridge's clock at t = 0, unix seconds: the situation's `at`.
    pub epoch: f64,
    /// The repository (sim/ files).
    pub root: PathBuf,
    /// This episode's own files: the bridge's fleet manifest, the audit log and its anchors.
    pub workdir: PathBuf,
    /// The bridge's HTTP port: it names the station (its `.last-run-<port>` pointer); nothing
    /// is bound.
    pub http_port: u16,
    /// Where vehicle i sends (`vehicle_base + i`) and the bridge listens (`bridge_base + i`):
    /// identifiers only, no socket is bound.
    pub vehicle_base: u16,
    pub bridge_base: u16,
    /// `sim/swarm.py`'s fleet manifest entries (vehicle-side ports).
    pub fleet: Vec<Value>,
    /// `sim/run/standins.json`'s vehicles, as `sim/swarm.py` writes them.
    pub standins: Vec<Value>,
    /// The link profiles (`sim/links.toml`, or `MURMUR_LINKS`).
    pub links: PathBuf,
    /// Link `[[event]]`s known at the start (the situation's events files), as JSON objects of
    /// the emulator's own event schema.
    #[serde(default)]
    pub events: Vec<Value>,
    #[serde(default)]
    pub linkemu_seed: Option<u64>,
    /// The baked surface-height grid for the emulator's terrain line of sight; none: terrain
    /// not checked (as an emulator without `--tidewater`).
    #[serde(default)]
    pub terrain_grid: Option<PathBuf>,
    /// SATCOM links follow satellite visibility (murmur-space data in `sim/space/cache`), as
    /// the emulator does unless `--no-space`.
    #[serde(default = "yes")]
    pub space: bool,
    /// The SATCOM look-ahead for the console (`outlook_s`): display only, off by default.
    #[serde(default)]
    pub space_outlook: bool,
    /// Where SATCOM looks are kept between episodes (none: computed afresh; the answers are the
    /// same either way, `space_cache.rs`).
    #[serde(default)]
    pub space_cache: Option<PathBuf>,
    /// The contacts process: its scenario, when it starts (situation seconds) and its seed
    /// (none: the scenario's own, as `MURMUR_CONTACTS_SEED` unset).
    #[serde(default)]
    pub contacts: Option<Contacts>,
    /// When the stand-in engine starts, situation seconds.
    #[serde(default)]
    pub standins_start_s: f64,
    /// When the bridge is up, situation seconds (the full stack's first anchor: its decision
    /// layer started, its loops running, its sockets answering): its loops start then, and a
    /// frame or a contacts message that reaches it before is lost, as on a port nobody holds.
    #[serde(default)]
    pub bridge_start_s: f64,
    /// A stand-in that starts with less than a full battery: uid -> charge, %.
    #[serde(default)]
    pub battery_pct: BTreeMap<u32, f64>,
    /// The world's water (world v3, H4; eval/headless/situations.py `world_spec`): the current
    /// field the contacts drift with, and the conditions our vehicles that take them get
    /// (`water.rs`). None: still water and no conditions, as before world v3.
    #[serde(default)]
    pub water: Option<WaterSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterSpec {
    /// The world's current field (`[currents] field`).
    #[serde(default)]
    pub currents: Option<PathBuf>,
    /// The world's land/water grid (`[terrain] land_water`): depth and elevation.
    #[serde(default)]
    pub land_water: Option<PathBuf>,
    /// The situation's drawn wind, (east, north) m/s, the same everywhere.
    #[serde(default)]
    pub wind_en: Option<[f64; 2]>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contacts {
    pub scenario: PathBuf,
    #[serde(default)]
    pub start_s: f64,
    #[serde(default)]
    pub seed: Option<u64>,
}
