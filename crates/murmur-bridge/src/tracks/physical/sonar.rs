//! Sonar detection by the sonar equation (WORLD-V3-PLAN 2.6, research section 4): signal
//! excess `SE = FOM - TL - dNL` (two-way TL for an active sonar), `Pd = Phi(SE / 8 dB)` [24].
//! Transmission loss spherical to the water depth, cylindrical beyond, plus Thorp absorption
//! [25,26]; ambient noise from turbulence, shipping, wind and thermal noise, power-summed
//! [26]. Each sensor's figure of merit per contact kind comes from its reference range in a
//! reference environment (50 m of water, 5 m/s wind, moderate shipping); a kind it has no
//! reference for takes the nearest reference kind's, shifted by the difference in source
//! level (passive) or target strength (active). The research's caveat stands: source levels
//! and detection thresholds are mostly not public, so the reference ranges carry the model.

use super::signature::{Signature, kind_signature};
use super::spec::{SonarMode, SonarSensor};
use crate::tracks::truth::Domain;

/// The spread of signal excess, dB: Pd = Phi(SE / 8) [24].
const SE_SIGMA_DB: f64 = 8.0;
/// The reference environment the reference ranges hold in.
pub const REF_DEPTH_M: f64 = 50.0;
pub const REF_WIND_MPS: f64 = 5.0;
pub const REF_SHIPPING: f64 = 0.5;

/// Thorp's absorption, dB per km, at `f_khz` [26].
pub fn thorp_db_per_km(f_khz: f64) -> f64 {
    let f2 = f_khz * f_khz;
    0.11 * f2 / (1.0 + f2) + 44.0 * f2 / (4100.0 + f2) + 2.75e-4 * f2 + 0.003
}

/// One-way transmission loss to `r_m`, dB: spherical (20 log r) to the water depth, then
/// cylindrical (10 log r), and absorption.
pub fn tl_db(r_m: f64, depth_m: f64, f_khz: f64) -> f64 {
    let r = r_m.max(1.0);
    let rt = depth_m.max(1.0);
    let spread = if r <= rt { 20.0 * r.log10() } else { 20.0 * rt.log10() + 10.0 * (r / rt).log10() };
    spread + thorp_db_per_km(f_khz) * r / 1000.0
}

/// Ambient noise, dB re 1 uPa^2/Hz, at `f_khz`: turbulence, shipping (activity 0 to 1), wind
/// (m/s) and thermal noise, power-summed [26] (about 61 dB at 1 kHz in a 5 m/s wind).
pub fn noise_db(f_khz: f64, wind_mps: f64, shipping: f64) -> f64 {
    let lf = f_khz.max(1e-3).log10();
    let turbulence = 17.0 - 30.0 * lf;
    let ships = 40.0 + 20.0 * (shipping - 0.5) + 26.0 * lf - 60.0 * (f_khz + 0.03).log10();
    let wind = 50.0 + 7.5 * wind_mps.max(0.0).sqrt() + 20.0 * lf - 40.0 * (f_khz + 0.4).log10();
    let thermal = -15.0 + 20.0 * lf;
    10.0 * [turbulence, ships, wind, thermal].iter().map(|n| 10f64.powf(n / 10.0)).sum::<f64>().log10()
}

/// The standard normal distribution function (Abramowitz and Stegun 7.1.26, |error| < 1.5e-7).
pub fn phi(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t * (0.254_829_592 + t * (-0.284_496_736 + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 { 0.5 * (1.0 + erf) } else { 0.5 * (1.0 - erf) }
}

/// The level a sonar of `mode` reads off a signature: its source level (passive) or target
/// strength (active).
fn level(mode: SonarMode, s: &Signature) -> Option<f64> {
    match mode {
        SonarMode::Passive => s.source_db,
        SonarMode::Active => s.target_db,
    }
}

/// The loss a reference range stands for: one-way or two-way.
fn loss(mode: SonarMode, r_m: f64, depth_m: f64, f_khz: f64) -> f64 {
    let tl = tl_db(r_m, depth_m, f_khz);
    match mode {
        SonarMode::Passive => tl,
        SonarMode::Active => 2.0 * tl,
    }
}

/// The figure of merit of `sonar` against a contact of `kind` with signature `sig` (dB, the
/// loss it overcomes with signal excess 0 in the reference environment); None: it makes no
/// sound this sonar hears (passive) or returns nothing (active).
pub fn fom_db(sonar: &SonarSensor, kind: &str, sig: &Signature) -> Option<f64> {
    let x = level(sonar.mode, sig)?;
    let reference = |k: &str| {
        // a kind's reference signature: subsurface kinds by their own domain
        let d = if matches!(k, "submarine" | "uuv" | "diver" | "periscope") { Domain::Subsurface } else { Domain::Surface };
        level(sonar.mode, &kind_signature(k, d))
    };
    let fom_of = |r: f64| loss(sonar.mode, r, REF_DEPTH_M, sonar.freq_khz);
    if let Some((_, r)) = sonar.ranges.iter().find(|(k, _)| k == kind) {
        // its own reference, shifted by any difference of this contact from its kind's table
        // (a fast boat's speed, a scenario's override)
        let own = reference(kind).unwrap_or(x);
        return Some(fom_of(*r) + (x - own));
    }
    sonar
        .ranges
        .iter()
        .filter_map(|(k, r)| reference(k).map(|xl| (xl, fom_of(*r))))
        .min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs()))
        .map(|(xl, f)| f + (x - xl))
}

/// The detection probability of a contact `r_m` away (slant) in `depth_m` of water, with the
/// figure of merit `fom`, the ambient noise `dnl_db` above the reference environment's.
pub fn pd(sonar: &SonarSensor, fom: f64, r_m: f64, depth_m: f64, dnl_db: f64) -> f64 {
    if r_m > sonar.max_m {
        return 0.0;
    }
    let se = fom - loss(sonar.mode, r_m, depth_m, sonar.freq_khz) - dnl_db;
    phi(se / SE_SIGMA_DB)
}

#[cfg(test)]
mod tests {
    use super::super::eo::range_at;
    use super::*;

    #[test]
    fn the_research_figures_come_out() {
        // Thorp: 0.07 at 1 kHz, 1.2 at 10, 32 at 90
        assert!((thorp_db_per_km(1.0) - 0.07).abs() < 0.01);
        assert!((thorp_db_per_km(10.0) - 1.2).abs() < 0.05);
        assert!((thorp_db_per_km(90.0) - 32.0).abs() < 1.0);
        // ambient noise at 1 kHz: about 61 dB at 5 m/s, 68 at 10
        assert!((noise_db(1.0, 5.0, 0.5) - 61.0).abs() < 0.6);
        assert!((noise_db(1.0, 10.0, 0.5) - 68.0).abs() < 0.6);
        assert!((phi(0.0) - 0.5).abs() < 1e-7 && (phi(1.96) - 0.975).abs() < 1e-4);
    }

    /// Sentinel-class diver-detection sonar (research [27]): open-circuit diver 0.9-1.0 km,
    /// UUV 1.2 km, mini-submarine 1.5 km, reproduced in the reference environment, and a
    /// windier sea shortens them.
    #[test]
    fn the_sentinel_ranges_reproduce_and_wind_shortens_them() {
        let s = SonarSensor {
            mode: SonarMode::Active,
            freq_khz: 70.0,
            ranges: vec![("diver".into(), 950.0), ("uuv".into(), 1200.0), ("submarine".into(), 1500.0)],
            max_m: f64::INFINITY,
            scan_s: None,
            placeholder: false,
            passive: None,
        };
        let dnl = noise_db(70.0, 12.0, REF_SHIPPING) - noise_db(70.0, REF_WIND_MPS, REF_SHIPPING);
        assert!(dnl > 3.0, "{dnl}");
        for (kind, want) in [("diver", 950.0), ("uuv", 1200.0), ("submarine", 1500.0)] {
            let sig = kind_signature(kind, Domain::Subsurface);
            let fom = fom_db(&s, kind, &sig).expect("heard");
            let r = range_at(0.5, |r| pd(&s, fom, r, REF_DEPTH_M, 0.0));
            assert!((r / want - 1.0).abs() < 0.02, "{kind}: {r:.0} m against {want}");
            let windy = range_at(0.5, |r| pd(&s, fom, r, REF_DEPTH_M, dnl));
            assert!(windy < 0.95 * r, "{kind}: {windy:.0} against {r:.0}");
        }
    }
}
