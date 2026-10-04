//! `sim/run/standins.json`, written by `sim/swarm.py`.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Fleet {
    #[serde(default = "default_host")]
    pub bridge_host: String,
    pub vehicles: Vec<VehicleConfig>,
}

fn default_host() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct VehicleConfig {
    /// Fleet-unique id. Older manifests lack it; the loader fills it from the position.
    #[serde(default)]
    pub uid: Option<u32>,
    /// MAVLink system id on this vehicle's link (1..250 from swarm.py). Wider than u8 here so
    /// an out-of-range value is reported instead of failing the whole file.
    pub sysid: u32,
    pub callsign: String,
    pub port: u16,
    /// [lat, lon, alt_m (negative = depth), heading_deg]
    pub home: [f64; 4],
    pub spec: Spec,
    /// ArduPilot parameters `sim/swarm.py` sets for this vehicle, as a SITL vehicle gets them
    /// in its defaults file (today the lost-link failsafe timer for its link profile, from
    /// `sim/links.toml`). Applied over the seeded table; names the stand-in doesn't model are
    /// reported and skipped.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
}

/// The platform entry from `sim/platforms.toml`. Only what the kinematics use.
#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    pub name: String,
    pub behavior: String,
    pub cruise_mps: f64,
    #[serde(default)]
    pub max_mps: f64,
    pub climb_mps: Option<f64>,
    pub turn_dps: Option<f64>,
    pub loiter_radius_m: Option<f64>,
    pub endurance_h: Option<f64>,
    /// los | mesh | satcom | acoustic | tether: what carries its telemetry (telemetry.rs)
    #[serde(default)]
    pub link: Option<String>,
}

impl Spec {
    pub fn turn_dps(&self) -> f64 {
        self.turn_dps.unwrap_or(10.0)
    }

    pub fn climb_mps(&self) -> f64 {
        self.climb_mps.unwrap_or(0.0)
    }

    pub fn endurance_h(&self) -> f64 {
        self.endurance_h.unwrap_or(10.0)
    }

    /// Cruise speed over turn rate: the tightest circle it can fly at cruise (0 if unknown).
    /// Same formula as the bridge's `min_turn_radius_m`.
    pub fn min_turn_radius_m(&self) -> f64 {
        match self.turn_dps {
            Some(t) if t > 0.0 && self.cruise_mps > 0.0 => self.cruise_mps / t.to_radians(),
            _ => 0.0,
        }
    }

    /// Talks over an acoustic modem while submerged (AUVs).
    pub fn acoustic(&self) -> bool {
        self.link.as_deref() == Some("acoustic")
    }
}

impl Fleet {
    pub fn load(path: &str) -> Result<Fleet, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let mut fleet: Fleet = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        for (i, v) in fleet.vehicles.iter_mut().enumerate() {
            let uid = *v.uid.get_or_insert(i as u32 + 1);
            if !(1..=255).contains(&v.sysid) {
                // same wrap as swarm.py, so a manifest from before uid/sysid still runs
                let wrapped = (uid.max(1) - 1) % 250 + 1;
                eprintln!(
                    "{}: sysid {} is not a MAVLink system id, using {wrapped}",
                    v.callsign, v.sysid
                );
                v.sysid = wrapped;
            }
        }
        Ok(fleet)
    }
}

impl VehicleConfig {
    pub fn sysid(&self) -> u8 {
        u8::try_from(self.sysid).unwrap_or(1)
    }

    pub fn uid(&self) -> u32 {
        self.uid.unwrap_or(0)
    }
}
