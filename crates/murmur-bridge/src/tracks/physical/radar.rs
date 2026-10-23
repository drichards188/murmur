//! Radar detection (WORLD-V3-PLAN 2.5, research section 2): the fourth-root law about each
//! radar's reference (`R50 = r50 (sigma / sigma_ref)^(1/4)`), a Swerling 1 target drawn once
//! per scan against a 1e-4 false-alarm threshold, the two-ray model for low targets (power
//! falls as R^-8 beyond the crossover `4 pi h_r h_t / lambda`, with both heights above the
//! tangent plane at the reflection point), and sea clutter by grazing angle and sea state (a
//! surface target needs 10 dB over the clutter in its cell).
//!
//! Calibrated on the IMO MSC.192(79) Table 2 anchors (research [19]): a navigation radar
//! with a 15 m antenna sees a 7.5 m^2 small vessel 4 m high at 5.0 NM with Pd 0.8, which under
//! Swerling 1 and Pfa 1e-4 is a 50 % range of 6.72 NM; the other anchors then come out within
//! 15 % (`tests::the_imo_table_2_anchors_reproduce`).

use super::geometry::{EARTH_R_M, K_RADIO};
use super::spec::RadarSensor;

/// The detection threshold over the noise for a false-alarm probability of 1e-4: `ln(1/Pfa)`.
const THRESHOLD: f64 = 9.210_340_371_976_184;
/// What a surface target needs over the sea clutter in its cell, as a power ratio (10 dB).
const SCR_MIN: f64 = 10.0;

/// A Swerling 1 target's detection probability at signal-to-noise `snr` (a power ratio).
pub fn pd_swerling1(snr: f64) -> f64 {
    if snr.is_nan() || snr <= 0.0 {
        return 0.0;
    }
    (-THRESHOLD / (1.0 + snr)).exp()
}

/// The signal-to-noise ratio that gives Pd 0.5 (12.3, 10.9 dB).
pub fn snr50() -> f64 {
    THRESHOLD / std::f64::consts::LN_2 - 1.0
}

/// The two-ray propagation factor for a radar `h_r` and a target `h_t` metres above the sea,
/// `ground_m` apart: 1 short of the crossover, `R_x / R` beyond (power as R^-8), with both
/// heights taken above the tangent plane at the reflection point (spherical earth, k = 4/3).
/// Research section 2 [E].
pub fn two_ray(h_r: f64, h_t: f64, ground_m: f64, lambda_m: f64) -> f64 {
    if h_t <= 0.0 || h_r <= 0.0 {
        return 0.0;
    }
    let d1 = ground_m * h_r / (h_r + h_t);
    let d2 = ground_m - d1;
    let hr = h_r - d1 * d1 / (2.0 * K_RADIO * EARTH_R_M);
    let ht = h_t - d2 * d2 / (2.0 * K_RADIO * EARTH_R_M);
    if hr <= 0.0 || ht <= 0.0 {
        return 0.0;
    }
    let rx = 4.0 * std::f64::consts::PI * hr * ht / lambda_m;
    (rx / ground_m.max(1.0)).min(1.0)
}

/// Sea clutter reflectivity (X band, HH), dB, at a grazing angle and sea state: the research's
/// rounded Nathanson-type table (at most 1, 3 and 10 degrees; sea states 1, 3 and 5),
/// interpolated in the angle's logarithm and in the sea state, and carried on at the table's
/// own slopes beyond it.
pub fn sigma0_db(grazing_deg: f64, sea_state: f64) -> f64 {
    const TABLE: [[f64; 3]; 3] = [[-55.0, -45.0, -38.0], [-48.0, -40.0, -33.0], [-42.0, -34.0, -28.0]];
    let lg = grazing_deg.max(1e-3).log10();
    let (row, f) = if lg <= 0.0 {
        (0, 0.0)
    } else if lg <= 3f64.log10() {
        (0, lg / 3f64.log10())
    } else if lg <= 1.0 {
        (1, (lg - 3f64.log10()) / (1.0 - 3f64.log10()))
    } else {
        (1, 1.0)
    };
    let at = |col: usize| TABLE[row][col] + f * (TABLE[row + 1][col] - TABLE[row][col]);
    let (s1, s3, s5) = (at(0), at(1), at(2));
    let ss = sea_state.clamp(0.0, 9.0);
    if ss <= 1.0 {
        s1 - (1.0 - ss) * (s3 - s1) / 2.0
    } else if ss <= 3.0 {
        s1 + (ss - 1.0) * (s3 - s1) / 2.0
    } else if ss <= 5.0 {
        s3 + (ss - 3.0) * (s5 - s3) / 2.0
    } else {
        s5 + (ss - 5.0) * (s5 - s3) / 2.0
    }
}

/// The sea clutter in a target's cell, m^2: `sigma0 R theta (c tau / 2) sec psi`.
pub fn clutter_m2(r: &RadarSensor, ground_m: f64, h_r: f64, sea_state: f64) -> f64 {
    let psi = (h_r / ground_m.max(1.0) - ground_m / (2.0 * K_RADIO * EARTH_R_M)).max(1e-5);
    let s0 = 10f64.powf(sigma0_db(psi.to_degrees(), sea_state) / 10.0);
    s0 * ground_m * r.beam_rad * r.cell_m / psi.cos().max(0.05)
}

/// One scan's detection probability of a target of `rcs_m2` at slant range `slant_m`
/// (`ground_m` over the ground), its visible height `h_t` (hull-down) above the sea, from a
/// radar `h_r` metres up; a surface target (`sea`) against the clutter of `sea_state` too.
pub fn pd(r: &RadarSensor, rcs_m2: f64, slant_m: f64, ground_m: f64, h_r: f64, h_t: f64, sea: Option<f64>) -> f64 {
    if rcs_m2 <= 0.0 || h_t <= 0.0 {
        return 0.0;
    }
    let f = two_ray(h_r, h_t, ground_m, r.lambda_m);
    let snr = snr50() * (rcs_m2 / r.rcs_ref_m2) * (r.r50_m / slant_m.max(1.0)).powi(4) * f.powi(4);
    if let Some(ss) = sea
        && rcs_m2 < SCR_MIN * clutter_m2(r, ground_m, h_r, ss)
    {
        return 0.0;
    }
    pd_swerling1(snr)
}

#[cfg(test)]
mod tests {
    use super::super::eo::range_at;
    use super::super::geometry::visible_height_m;
    use super::*;

    const NM: f64 = 1852.0;

    /// A navigation radar calibrated on the small-vessel anchor: 5.0 NM at Pd 0.8 for 7.5 m^2.
    pub fn imo_radar() -> RadarSensor {
        let r80 = 5.0 * NM;
        // Pd 0.8 needs SNR 40.3 (16.1 dB), Pd 0.5 12.3: the 50 % range is (40.3/12.3)^(1/4) further
        let snr80 = THRESHOLD / -(0.8f64.ln()) - 1.0;
        RadarSensor {
            r50_m: r80 * (snr80 / snr50()).powf(0.25),
            rcs_ref_m2: 7.5,
            scan_s: 2.5,
            beam_rad: 1.2f64.to_radians(),
            cell_m: 37.0,
            lambda_m: 0.299_792_458 / 9.4,
            height: false,
            placeholder: false,
        }
    }

    /// The range at which a radar 15 m up sees a target of `rcs` and `h` m with Pd 0.8, NM.
    fn r80(r: &RadarSensor, rcs: f64, h: f64) -> f64 {
        range_at(0.8, |g| {
            let v = visible_height_m(h, h, g, 15.0, K_RADIO);
            pd(r, rcs, g, g, 15.0, v, None)
        }) / NM
    }

    /// IMO MSC.192(79) Table 2, X band: every anchor within 15 % (WORLD-V3-PLAN 2.8).
    #[test]
    fn the_imo_table_2_anchors_reproduce() {
        let r = imo_radar();
        for (what, rcs, h, nm) in [
            ("ship > 5,000 GT", 10_000.0, 10.0, 11.0),
            ("ship > 500 GT", 1_000.0, 5.0, 8.0),
            ("small vessel with a reflector", 7.5, 4.0, 5.0),
            ("navigation buoy", 5.0, 3.5, 4.6),
            ("10 m vessel, no reflector", 2.5, 2.0, 3.4),
        ] {
            let got = r80(&r, rcs, h);
            assert!((got / nm - 1.0).abs() <= 0.15, "{what}: {got:.2} NM against IMO's {nm}");
        }
    }

    /// Sea state 5 loses a small boat at 5 km (the research's worked example: 2.5 m^2 against
    /// 0.13 m^2 of clutter at sea state 3, 13 dB; under 10 dB at sea state 5) and keeps a ferry.
    #[test]
    fn sea_state_5_loses_a_small_boat_at_5_km_and_keeps_a_ferry() {
        let r = imo_radar();
        let c3 = clutter_m2(&r, 5_000.0, 15.0, 3.0);
        assert!((c3 - 0.13).abs() < 0.03, "{c3}");
        let boat = |ss| pd(&r, 2.5, 5_000.0, 5_000.0, 15.0, 2.0, Some(ss));
        assert!(boat(3.0) > 0.5 && boat(5.0) == 0.0, "{} {}", boat(3.0), boat(5.0));
        assert!(pd(&r, 2_000.0, 5_000.0, 5_000.0, 15.0, 10.0, Some(5.0)) > 0.99);
    }

    #[test]
    fn the_fourth_root_law_holds_short_of_the_crossover() {
        let r = imo_radar();
        let r50 = |rcs| range_at(0.5, |g| pd(&r, rcs, g, g, 15.0, 100.0, None));
        let ratio = r50(16.0 * 7.5) / r50(7.5);
        assert!((ratio - 2.0).abs() < 0.02, "{ratio}");
    }
}
