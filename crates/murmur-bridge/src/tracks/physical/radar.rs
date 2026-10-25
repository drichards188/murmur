//! Radar detection (WORLD-V3-PLAN 2.5, research section 2): the fourth-root law about each
//! radar's reference (`R50 = r50 (sigma / sigma_ref)^(1/4)`), a Swerling 1 target drawn once
//! per scan against a 1e-4 false-alarm threshold, the two-ray model for low targets (power
//! falls as R^-8 beyond the crossover `4 pi h_r h_t / lambda`, with both heights above the
//! tangent plane at the reflection point), and sea clutter (a surface target needs 10 dB over
//! the clutter in its cell, unless a radar with a Doppler mode sees it moving clear of the
//! clutter's Doppler band).
//!
//! Sea clutter (murmur-qlt): `sigma0 R theta (c tau / 2) sec psi`, the cell `c tau / 2` the
//! radar's range resolution, the grazing angle `psi` from the spherical earth's geometry (valid
//! at every angle, the nadir included), and `sigma0` by grazing angle, sea state, band and wind:
//! the research's table up to 10 degrees (X band, HH), the NRL model's dependence on grazing angle
//! beyond it to 60 degrees and on frequency throughout, and the quasi-specular return of a rough
//! sea near vertical incidence ([`sigma0`]).
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
/// A radar's minimum range, m from the antenna: a navigation radar must detect a buoy from 40 m
/// out (IMO MSC.192(79) 5.4.1 [19]). The model itself does not cut there; the decision engine's
/// reaches start there (`tracks::tests::reaches`: nearer, a contact is alongside, under the
/// antenna, where the sea's specular glare is).
#[cfg(test)]
pub const MIN_RANGE_M: f64 = 40.0;
/// The band the research's sigma0 table is for, GHz (X band): other bands are shifted from it by
/// the NRL model's frequency dependence.
const TABLE_GHZ: f64 = 9.4;
/// The NRL model's range of validity in grazing angle, degrees (Gregers-Hansen and Mital 2012).
const NRL_MIN_DEG: f64 = 0.1;
const NRL_MAX_DEG: f64 = 60.0;
/// The research's table's last grazing angle, degrees: beyond it the NRL model's dependence on
/// the grazing angle carries it on.
const TABLE_MAX_DEG: f64 = 10.0;
/// Seawater's power reflectivity at normal incidence at X to Ku band, `|R(0)|^2` [E here: the
/// Fresnel coefficient of a permittivity of about 50-60 - j 35-40, 0.59 to 0.63].
const SEA_FRESNEL: f64 = 0.61;

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

/// The sea at a point `ground_m` from a radar `h_r` metres up, seen from the radar (spherical
/// earth of radius k = 4/3): the grazing angle, radians, and the slant range to it, m. Exact at
/// every angle, from the sea's horizon to the nadir (`sin psi = (h cos phi - 2 a sin^2(phi / 2))
/// / R`, `phi` the central angle); a small grazing angle is `h / R - R / (2 a)`, as before.
pub fn grazing(h_r: f64, ground_m: f64) -> (f64, f64) {
    let a = K_RADIO * EARTH_R_M;
    let phi = ground_m / a;
    let s = (0.5 * phi).sin();
    let r = (h_r * h_r + 4.0 * a * (a + h_r) * s * s).sqrt().max(1.0);
    let sin_psi = (h_r * phi.cos() - 2.0 * a * s * s) / r;
    (sin_psi.clamp(-1.0, 1.0).asin(), r)
}

/// Sea clutter reflectivity (X band, HH), dB, at a grazing angle and sea state: the research's
/// rounded Nathanson-type table (at most 1, 3 and 10 degrees; sea states 1, 3 and 5),
/// interpolated in the angle's logarithm and in the sea state, held at its first and last angle,
/// and carried on in the sea state at its own slopes.
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

/// The NRL sea clutter model, HH, dB (Gregers-Hansen and Mital, "An Improved Empirical Model for
/// Radar Sea Clutter Reflectivity", NRL/MR/5310--12-9346, 2012; the research's [17] implements
/// it): a fit to Nathanson's tables for grazing angles of 0.1 to 60 degrees, 0.5 to 35 GHz and
/// sea states 0 to 6, within 2.2 to 2.6 dB of them. Its coefficients as in Gregers-Hansen's own
/// `NRL_SigmaSea`.
pub fn nrl_db(grazing_deg: f64, sea_state: f64, ghz: f64) -> f64 {
    const C: [f64; 5] = [-73.0, 20.781, 7.351, 25.65, 0.0054];
    let psi = grazing_deg;
    let ss = sea_state;
    C[0] + C[1] * psi.to_radians().sin().log10()
        + (27.5 + C[2] * psi) * ghz.log10() / (1.0 + 0.95 * psi)
        + C[3] * (ss + 1.0).powf(1.0 / (2.0 + 0.085 * psi + 0.033 * ss))
        + C[4] * psi * psi
}

/// The quasi-specular return of a rough sea near vertical incidence, m^2/m^2: the Kirchhoff
/// (physical optics) model's geometric-optics limit for a sea whose slopes are Gaussian,
/// `|R(0)|^2 sec^4 theta exp(-tan^2 theta / s^2) / s^2` at incidence `theta = 90 deg - psi`
/// (Barrick 1968; Valenzuela 1978; the model of high grazing angles, 60 to 90 degrees, in
/// Raynal and Doerry, SAND2010-3828, 5.2.1), the mean square slope Cox and Munk's for a clean
/// sea, `s^2 = 0.003 + 5.12e-3 U` (J. Opt. Soc. Am. 44, 838, 1954; `U` the wind, m/s). It is
/// what makes the sea bright under an aircraft (about +13 dB straight down at 5 m/s, as a
/// Ku-band altimeter measures +11 to +14) and is nothing below about 50 degrees.
pub fn specular(grazing_rad: f64, wind_mps: f64) -> f64 {
    let s2 = 0.003 + 5.12e-3 * wind_mps.max(0.0);
    let theta = std::f64::consts::FRAC_PI_2 - grazing_rad.clamp(0.0, std::f64::consts::FRAC_PI_2);
    let (t, c) = (theta.tan(), theta.cos());
    SEA_FRESNEL * (-t * t / s2).exp() / (s2 * c.powi(4))
}

/// Sea clutter reflectivity, m^2/m^2, at a grazing angle (radians), sea state, band and wind
/// (murmur-qlt): the research's table (X band, HH) up to 10 degrees; beyond it, to 60 degrees,
/// the table's 10-degree value carried on by the NRL model's dependence on the grazing angle
/// ([`nrl_db`]: its value less its value at 10 degrees, at X band and the sea state, 0 to 6),
/// held beyond 60 degrees where the model ends; another band shifted by the NRL model's
/// frequency term (`(27.5 + 7.351 psi) log10(f / 9.4) / (1 + 0.95 psi)`, none at X band); and the
/// quasi-specular return ([`specular`]) added, which rules near the nadir. Up to 10 degrees at X
/// band this is the table's value exactly (the navigation radars' worked example and anchors).
pub fn sigma0(grazing_rad: f64, sea_state: f64, ghz: f64, wind_mps: f64) -> f64 {
    let deg = grazing_rad.to_degrees();
    let ss = sea_state.clamp(0.0, 6.0);
    let mut db = if deg <= TABLE_MAX_DEG {
        sigma0_db(deg, sea_state)
    } else {
        let to = deg.min(NRL_MAX_DEG);
        sigma0_db(TABLE_MAX_DEG, sea_state) + nrl_db(to, ss, TABLE_GHZ) - nrl_db(TABLE_MAX_DEG, ss, TABLE_GHZ)
    };
    if ghz != TABLE_GHZ {
        let psi = deg.clamp(NRL_MIN_DEG, NRL_MAX_DEG);
        db += (27.5 + 7.351 * psi) * (ghz / TABLE_GHZ).log10() / (1.0 + 0.95 * psi);
    }
    10f64.powf(db / 10.0) + specular(grazing_rad, wind_mps)
}

/// The sea clutter's Doppler band, m/s along the line of sight: a target whose own speed along
/// it is beyond this is clear of the clutter for a radar with a Doppler mode. The clutter's mean
/// shift looking up or down wind (HH, `0.25 + 0.2 U`) and its 3 dB width (`0.24 U`), `U` the
/// wind, m/s (Raynal and Doerry, "Doppler Characteristics of Sea Clutter", SAND2010-3828,
/// Sandia, 2010, 3.2.1, after Ward, Baker and Watts 1990 and Wetzel 1990; nearly independent of
/// the band and the grazing angle): the band reaches one 3 dB width past the mean shift, the
/// look's angle to the wind not modelled (the shift at its largest) [E here].
pub fn clutter_band_mps(wind_mps: f64) -> f64 {
    0.25 + 0.44 * wind_mps.max(0.0)
}

/// The sea a surface target is seen against: the sea state and the wind, and the target's own
/// speed along the radar's line of sight, m/s (a Doppler mode's).
#[derive(Debug, Clone, Copy)]
pub struct Sea {
    pub state: f64,
    pub wind_mps: f64,
    pub radial_mps: f64,
}

/// The sea clutter in a target's cell `ground_m` from a radar `h_r` up, m^2: `sigma0 R theta
/// (c tau / 2) sec psi`, `c tau / 2` the radar's range resolution (`cell_m`), `R` the slant
/// range, `sec psi` at most 20 (near the nadir the beam's footprint, not the range cell, bounds
/// the cell [E]).
pub fn clutter_m2(r: &RadarSensor, ground_m: f64, h_r: f64, sea_state: f64, wind_mps: f64) -> f64 {
    let (psi, slant) = grazing(h_r, ground_m);
    let psi = psi.max(1e-5);
    let s0 = sigma0(psi, sea_state, r.ghz, wind_mps);
    s0 * slant * r.beam_rad * r.cell_m / psi.cos().max(0.05)
}

/// One scan's detection probability of a target of `rcs_m2` at slant range `slant_m`
/// (`ground_m` over the ground), its visible height `h_t` (hull-down) above the sea, from a
/// radar `h_r` metres up; a surface target (`sea`) against the sea clutter too, unless the radar
/// has a Doppler mode and the target moves along its line of sight faster than the clutter's
/// Doppler band ([`clutter_band_mps`]): then the noise alone limits it.
pub fn pd(r: &RadarSensor, rcs_m2: f64, slant_m: f64, ground_m: f64, h_r: f64, h_t: f64, sea: Option<Sea>) -> f64 {
    if rcs_m2 <= 0.0 || h_t <= 0.0 {
        return 0.0;
    }
    let f = two_ray(h_r, h_t, ground_m, r.lambda_m);
    let snr = snr50() * (rcs_m2 / r.rcs_ref_m2) * (r.r50_m / slant_m.max(1.0)).powi(4) * f.powi(4);
    if let Some(sea) = sea
        && !(r.doppler && sea.radial_mps.abs() > clutter_band_mps(sea.wind_mps))
        && rcs_m2 < SCR_MIN * clutter_m2(r, ground_m, h_r, sea.state, sea.wind_mps)
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
            ghz: 9.4,
            doppler: false,
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

    fn still(ss: f64) -> Option<Sea> {
        Some(Sea { state: ss, wind_mps: 5.0, radial_mps: 0.0 })
    }

    /// Sea state 5 loses a small boat at 5 km (the research's worked example: 2.5 m^2 against
    /// 0.13 m^2 of clutter at sea state 3, 13 dB; under 10 dB at sea state 5) and keeps a ferry.
    #[test]
    fn sea_state_5_loses_a_small_boat_at_5_km_and_keeps_a_ferry() {
        let r = imo_radar();
        let c3 = clutter_m2(&r, 5_000.0, 15.0, 3.0, 5.0);
        assert!((c3 - 0.13).abs() < 0.03, "{c3}");
        let boat = |ss| pd(&r, 2.5, 5_000.0, 5_000.0, 15.0, 2.0, still(ss));
        assert!(boat(3.0) > 0.5 && boat(5.0) == 0.0, "{} {}", boat(3.0), boat(5.0));
        assert!(pd(&r, 2_000.0, 5_000.0, 5_000.0, 15.0, 10.0, still(5.0)) > 0.99);
        // a navigation radar has no Doppler mode: the boat's speed does not take it out of the clutter
        let fast = Some(Sea { state: 5.0, wind_mps: 5.0, radial_mps: 15.0 });
        assert_eq!(pd(&r, 2.5, 5_000.0, 5_000.0, 15.0, 2.0, fast), 0.0);
    }

    #[test]
    fn the_fourth_root_law_holds_short_of_the_crossover() {
        let r = imo_radar();
        let r50 = |rcs| range_at(0.5, |g| pd(&r, rcs, g, g, 15.0, 100.0, None));
        let ratio = r50(16.0 * 7.5) / r50(7.5);
        assert!((ratio - 2.0).abs() < 0.02, "{ratio}");
    }

    /// The grazing angle is the geometry's at every angle (murmur-qlt): the small-angle form it
    /// replaced (`h / R - R / (2 a)` read as an angle) gave an aircraft's radar a grazing angle
    /// of hundreds of degrees within a few km of its nadir, whose cosine came and went.
    #[test]
    fn the_grazing_angle_is_the_geometrys_at_every_angle() {
        let deg = |h: f64, g: f64| grazing(h, g).0.to_degrees();
        // straight down, and 60 degrees from 7,600 m (7,600 / tan 60 = 4,388 m out)
        assert!((deg(7_600.0, 0.0) - 90.0).abs() < 1e-6);
        assert!((deg(7_600.0, 4_388.0) - 60.0).abs() < 0.05, "{}", deg(7_600.0, 4_388.0));
        // 45 degrees, and the slant range
        let (psi, r) = grazing(1_000.0, 1_000.0);
        assert!((psi.to_degrees() - 45.0).abs() < 0.01 && (r - 1_414.2).abs() < 0.5, "{} {r}", psi.to_degrees());
        // a mast's: the small-angle form, h / R less the earth's curvature
        let small = 15.0 / 5_000.0 - 5_000.0 / (2.0 * K_RADIO * EARTH_R_M);
        assert!((grazing(15.0, 5_000.0).0 / small - 1.0).abs() < 1e-4);
        // beyond the radar's horizon the sea is not seen (a negative angle)
        assert!(grazing(15.0, 20_000.0).0 < 0.0);
    }

    /// The reflectivity is the research's table at X band up to 10 degrees, joins it there,
    /// follows the NRL model's slope to 60 degrees, rises toward the nadir with the specular
    /// return, rises with the band, and the NRL function is Gregers-Hansen's.
    #[test]
    fn the_sea_reflectivity_holds_from_the_horizon_to_the_nadir() {
        let db = |deg: f64, ss: f64, ghz: f64| 10.0 * sigma0(deg.to_radians(), ss, ghz, 5.0).log10();
        for (deg, ss) in [(0.2, 2.0), (1.0, 3.0), (3.0, 5.0), (7.0, 1.0), (10.0, 4.0)] {
            assert!((db(deg, ss, TABLE_GHZ) - sigma0_db(deg, ss)).abs() < 1e-9, "{deg} {ss}");
        }
        // continuous at 10 and 60 degrees
        for ss in [1.0, 2.0, 4.0] {
            assert!((db(10.0001, ss, 9.4) - db(9.9999, ss, 9.4)).abs() < 0.01);
            assert!((db(60.01, ss, 9.4) - db(59.99, ss, 9.4)).abs() < 0.05);
        }
        // the NRL model as Gregers-Hansen's NRL_SigmaSea computes it (HH): at 10 GHz, sea state 3,
        // 1 and 30 degrees
        assert!((nrl_db(1.0, 3.0, 10.0) - -43.27).abs() < 0.01, "{}", nrl_db(1.0, 3.0, 10.0));
        assert!((nrl_db(30.0, 3.0, 10.0) - -31.43).abs() < 0.01, "{}", nrl_db(30.0, 3.0, 10.0));
        // brighter with the grazing angle, and much brighter near the nadir
        let mut last = f64::NEG_INFINITY;
        for deg in [5.0, 10.0, 20.0, 40.0, 60.0, 70.0, 80.0, 90.0] {
            let v = db(deg, 2.0, 16.7);
            assert!(v > last, "{deg}: {v} after {last}");
            last = v;
        }
        assert!(db(90.0, 2.0, 16.7) > 10.0 && db(90.0, 2.0, 16.7) < 15.0, "{}", db(90.0, 2.0, 16.7));
        // Ku band a few dB over X band
        let ku = db(20.0, 2.0, 16.7) - db(20.0, 2.0, 9.4);
        assert!(ku > 1.5 && ku < 3.5, "{ku}");
    }

    /// The Lynx class (Ku band, a 3 degree beam, a 1 m range cell, 7,600 m up, 300 km on
    /// 10^4 m^2) against a 10 m^2 boat holding still in sea state 2 (murmur-qlt): lost in the
    /// sea's glare within about 15 km of its nadir, seen from there to the noise's range; with the
    /// 15 m cell the catalog gave it before, nowhere. Moving along the line of sight faster
    /// than the clutter's Doppler band, the boat is seen as far as the noise allows, the nadir's
    /// steep geometry aside (its speed along the line of sight falls with the depression).
    #[test]
    fn an_airborne_maritime_radar_sees_a_boat_beyond_the_seas_glare() {
        let lynx = RadarSensor {
            r50_m: 300_000.0,
            rcs_ref_m2: 10_000.0,
            scan_s: 5.0,
            beam_rad: 3f64.to_radians(),
            cell_m: 1.0,
            lambda_m: 0.299_792_458 / 16.7,
            ghz: 16.7,
            doppler: true,
            height: false,
            placeholder: false,
        };
        let h = 7_600.0;
        let at = |g: f64, cell: f64, radial: f64| {
            let r = RadarSensor { cell_m: cell, ..lynx };
            let slant = g.hypot(h);
            pd(&r, 10.0, slant, g, h, 3.0, Some(Sea { state: 2.0, wind_mps: 5.0, radial_mps: radial }))
        };
        for g in [0.0, 2_000.0, 5_000.0, 10_000.0] {
            assert_eq!(at(g, 1.0, 0.0), 0.0, "{g}");
        }
        for g in [20_000.0, 30_000.0, 45_000.0] {
            assert!(at(g, 1.0, 0.0) > 0.5, "{g}: {}", at(g, 1.0, 0.0));
            assert_eq!(at(g, 15.0, 0.0), 0.0, "{g}: the 15 m cell");
        }
        // moving 5 m/s along the line of sight, clear of the clutter's band at 5 m/s of wind
        // (2.45 m/s): seen near the nadir as far out
        assert!(clutter_band_mps(5.0) < 5.0);
        for g in [2_000.0, 10_000.0, 45_000.0] {
            assert!(at(g, 1.0, 5.0) > 0.5, "{g}: {}", at(g, 1.0, 5.0));
        }
        // and a radar without the mode does not
        let nav = |g: f64| {
            let r = RadarSensor { doppler: false, ..lynx };
            pd(&r, 10.0, g.hypot(h), g, h, 3.0, Some(Sea { state: 2.0, wind_mps: 5.0, radial_mps: 5.0 }))
        };
        assert_eq!(nav(5_000.0), 0.0);
    }
}
