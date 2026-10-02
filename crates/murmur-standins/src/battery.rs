//! Battery (or fuel, reported as a battery the way MAVLink does): drains with the load so a
//! vehicle at cruise lasts its catalog endurance; failsafe levels as AP_BattMonitor computes
//! them from the remaining capacity (BATT_LOW_MAH / BATT_CRT_MAH) or voltage.
//!
//! Load is a fraction of the cruise draw (estimates, shaped by where the power goes):
//! - disarmed: avionics only, `IDLE_LOAD`;
//! - fixed wing in flight: 0.7 + 0.3 (v / cruise)^2, plus up to +0.5 while climbing at the
//!   catalog climb rate;
//! - multirotor / airship in flight: 0.85 + 0.15 (v / cruise)^2 (hovering costs most of it);
//! - everything else (hulls, tracks): hotel load 0.1 + propulsion 0.9 (v / cruise)^3.
//!
//! The pack is notional: 12 Li-ion cells, 4.15 V full to 3.3 V empty, sagging with current.

use crate::behavior::Firmware;
use crate::config::Spec;
use crate::kinematics::State;

pub const IDLE_LOAD: f64 = 0.05;
const CELLS: f64 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Low,
    Critical,
}

#[derive(Debug, Clone)]
pub struct Battery {
    pub consumed_mah: f64,
    /// amps, last tick
    pub current_a: f64,
    /// the highest failsafe level reached (AP_BattMonitor never steps back down)
    pub level: Level,
}

impl Default for Battery {
    fn default() -> Self {
        Battery {
            consumed_mah: 0.0,
            current_a: 0.0,
            level: Level::Ok,
        }
    }
}

/// Load as a fraction of the cruise draw.
pub fn load(fw: Firmware, spec: &Spec, st: &State, airborne: bool) -> f64 {
    if !st.armed {
        return IDLE_LOAD;
    }
    let cruise = spec.cruise_mps;
    if cruise <= 0.0 {
        return IDLE_LOAD; // nothing to drive (a drifting buoy)
    }
    let v = st.speed.abs() / cruise;
    match fw {
        Firmware::Plane if airborne => {
            let climb = if spec.climb_mps() > 0.0 {
                (st.vz / spec.climb_mps()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            0.7 + 0.3 * v * v + 0.5 * climb
        }
        Firmware::Copter | Firmware::Blimp if airborne => 0.85 + 0.15 * v * v,
        _ => 0.1 + 0.9 * v.powi(3),
    }
}

impl Battery {
    /// Drain for `dt` seconds at `load` (1.0 = the draw that empties the pack in the
    /// catalog endurance).
    pub fn step(&mut self, dt: f64, load: f64, capacity_mah: f64, endurance_h: f64) {
        let ma = capacity_mah / endurance_h.max(1e-3) * load;
        self.current_a = ma / 1000.0;
        self.consumed_mah = (self.consumed_mah + ma * dt / 3600.0).min(capacity_mah.max(0.0));
    }

    pub fn remaining_mah(&self, capacity_mah: f64) -> f64 {
        (capacity_mah - self.consumed_mah).max(0.0)
    }

    /// SYS_STATUS battery_remaining, %.
    pub fn remaining_pct(&self, capacity_mah: f64) -> i8 {
        if capacity_mah <= 0.0 {
            return -1;
        }
        (100.0 * self.remaining_mah(capacity_mah) / capacity_mah).clamp(0.0, 100.0) as i8
    }

    pub fn flat(&self, capacity_mah: f64) -> bool {
        capacity_mah > 0.0 && self.remaining_mah(capacity_mah) <= 0.0
    }

    pub fn voltage(&self, capacity_mah: f64) -> f64 {
        let soc = if capacity_mah > 0.0 {
            self.remaining_mah(capacity_mah) / capacity_mah
        } else {
            1.0
        };
        // 30 mOhm per cell at the notional pack's current
        CELLS * (3.3 + 0.85 * soc) - self.current_a * 0.03 * CELLS / 10.0
    }

    /// The failsafe level now, from (capacity, low mAh, critical mAh, low V, critical V); 0
    /// disables a threshold, as in ArduPilot.
    pub fn assess(&self, cap: f64, low_mah: f64, crt_mah: f64, low_v: f64, crt_v: f64) -> Level {
        let rem = self.remaining_mah(cap);
        let v = self.voltage(cap);
        if (crt_mah > 0.0 && rem < crt_mah) || (crt_v > 0.0 && v < crt_v) {
            Level::Critical
        } else if (low_mah > 0.0 && rem < low_mah) || (low_v > 0.0 && v < low_v) {
            Level::Low
        } else {
            Level::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cruise_lasts_the_catalog_endurance() {
        let mut b = Battery::default();
        let (cap, endurance_h) = (10_000.0, 2.0);
        // 1.9 h at cruise load: still 5 % left
        for _ in 0..(19 * 3600) {
            b.step(0.1, 1.0, cap, endurance_h);
        }
        assert_eq!(b.remaining_pct(cap), 5);
        assert_eq!(b.assess(cap, 2000.0, 1000.0, 0.0, 0.0), Level::Critical);
        for _ in 0..36_000 {
            b.step(0.1, 1.0, cap, endurance_h);
        }
        assert!(b.flat(cap));
        assert_eq!(b.remaining_pct(cap), 0);
        assert!(b.voltage(cap) < 40.0);
    }
}
