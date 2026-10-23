//! AIS reception and ESM interception (WORLD-V3-PLAN 2.6, research sections 3 and 5).
//!
//! AIS: only a contact whose transponder is on reports, at its class's interval for its speed
//! [22], heard to the VHF horizon between the two antennas (k = 4/3) [21], decoded with the
//! receiver's slot load: `exp(-reports heard per minute / 4,500)` [E].
//!
//! ESM: an emitter in radio line of sight is heard when its received power,
//! `Pt Gt Gr lambda^2 / (4 pi R)^2`, reaches the receiver's sensitivity [28].

use super::geometry::{K_RADIO, horizon_m};
use super::signature::{AisClass, Emitter};

/// AIS slots a minute across both channels (research [E]).
const AIS_SLOTS_PER_MIN: f64 = 4_500.0;
const KNOT_MPS: f64 = 0.514_444;

/// A transponder's reporting interval, s, at `speed_mps` (turning ignored) [22]: class A
/// 3 min at anchor, 10 s to 14 kn, 6 s to 23 kn, 2 s above; class B (CS) 30 s above 2 kn,
/// 3 min below. None: no transponder.
pub fn ais_interval_s(class: AisClass, speed_mps: f64) -> Option<f64> {
    let kn = speed_mps / KNOT_MPS;
    match class {
        AisClass::A if kn < 0.5 => Some(180.0),
        AisClass::A if kn < 14.0 => Some(10.0),
        AisClass::A if kn < 23.0 => Some(6.0),
        AisClass::A => Some(2.0),
        AisClass::B if kn > 2.0 => Some(30.0),
        AisClass::B => Some(180.0),
        AisClass::None => None,
    }
}

/// The VHF range between antennas `h1` and `h2` metres up: `4.12 km (sqrt h1 + sqrt h2)`.
pub fn vhf_range_m(h1: f64, h2: f64) -> f64 {
    horizon_m(h1, K_RADIO) + horizon_m(h2, K_RADIO)
}

/// The share of reports a receiver decodes with `per_min` reports a minute in its reach.
pub fn ais_decode(per_min: f64) -> f64 {
    (-per_min.max(0.0) / AIS_SLOTS_PER_MIN).exp()
}

/// Free-space path loss, dB.
pub fn fspl_db(range_m: f64, freq_ghz: f64) -> f64 {
    let lambda = 0.299_792_458 / freq_ghz;
    20.0 * (4.0 * std::f64::consts::PI * range_m.max(1.0) / lambda).log10()
}

/// The power an omnidirectional receiver gets from `e` at `range_m`, dBm.
pub fn received_dbm(e: &Emitter, range_m: f64) -> f64 {
    e.power_dbm + e.gain_dbi - fspl_db(range_m, e.freq_ghz)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AIS horizons (research [21] and section 3): from a 15 m antenna a ship is heard at 15 to
    /// 20 NM; an MQ-9B at 7,600 m hears a 10 m antenna at about 370 km.
    #[test]
    fn ais_horizons_reproduce() {
        let nm = 1852.0;
        for ship_antenna in [10.0, 25.0] {
            let r = vhf_range_m(15.0, ship_antenna) / nm;
            assert!((15.0..=20.0).contains(&r), "{ship_antenna} m: {r:.1} NM");
        }
        let air = vhf_range_m(7_600.0, 10.0) / 1000.0;
        assert!((air - 370.0).abs() / 370.0 < 0.05, "{air:.0} km");
        assert_eq!(ais_interval_s(AisClass::A, 10.0 * KNOT_MPS), Some(10.0));
        assert_eq!(ais_interval_s(AisClass::B, 1.0), Some(180.0));
        assert!(ais_decode(4_500.0) < 0.37 && ais_decode(0.0) == 1.0);
    }

    /// A drone's 0.5 W datalink is heard to a few km at -80 dBm (research: 1 to 5 km); a ship's
    /// navigation radar far beyond the horizon.
    #[test]
    fn esm_hears_a_datalink_a_few_km_and_a_radar_to_the_horizon() {
        let link = Emitter { power_dbm: 27.0, gain_dbi: 2.0, freq_ghz: 2.4 };
        assert!(received_dbm(&link, 2_000.0) > -80.0 && received_dbm(&link, 5_000.0) < -80.0);
        let radar = Emitter { power_dbm: 74.0, gain_dbi: 30.0, freq_ghz: 9.4 };
        assert!(received_dbm(&radar, 100_000.0) > -60.0);
    }
}
