//! EO/IR acquisition (WORLD-V3-PLAN 2.3, research section 1): pixels on target against the
//! Johnson-criteria thresholds through the ACQUIRE target transfer probability function, an
//! atmosphere factor from visibility (visible bands) or thermal contrast (IR), the light (a day
//! camera by day only, a low-light camera at a fraction of its range by night) and the thermal
//! crossover at dawn and dusk.

use super::conditions::Conditions;
use super::spec::{Band, EoChannel};

/// 50 % thresholds across the critical dimension, pixels: detect, recognise, identify [5].
pub const P50_PX: [f64; 3] = [1.5, 6.0, 12.0];
/// The NVL time-to-detect constant: tau = 3.4 / P_inf seconds per dwell (research [12]; the
/// constant is the classic one, not confirmed in a fetched source).
pub const TAU_K_S: f64 = 3.4;
/// Inherent contrast of a target against its background in the visible (research: 0.3).
const C0: f64 = 0.3;
/// The thermal attenuation coefficient in clear air, per metre (0.2 per km, research [10]).
const BETA_IR_CLEAR: f64 = 0.2e-3;
/// In haze or fog IR reaches 3 to 6 times the visual range (research [11]): the middle.
const IR_OVER_VISUAL: f64 = 4.5;
/// A low-light camera's range at night as a share of its day range (research [E] 0.3-0.5).
const LOWLIGHT_NIGHT: f64 = 0.4;
/// Civil twilight: the sun this far below the horizon ends a day camera's light, degrees.
const TWILIGHT_DEG: f64 = 6.0;

/// The ACQUIRE target transfer probability function: `x^E / (1 + x^E)`, `x = p / p50`,
/// `E = 2.7 + 0.7 x` (research [7,8]).
pub fn acquire(px: f64, p50: f64) -> f64 {
    if !(px > 0.0) {
        return 0.0;
    }
    let x = px / p50;
    let xe = x.powf(2.7 + 0.7 * x);
    if xe.is_finite() { xe / (1.0 + xe) } else { 1.0 }
}

/// The visible band's atmospheric range, m: Koschmieder, `V ln(C0 / 0.02) / 3.912` [9].
pub fn r_atm_visible_m(visibility_m: f64) -> f64 {
    visibility_m * (C0 / 0.02).ln() / 3.912
}

/// The thermal bands' atmospheric range, m: `ln(dT / 0.1 K) / beta` in clear air [10], at
/// most 4.5 times the visible band's in haze or fog [11].
pub fn r_atm_ir_m(thermal_k: f64, visibility_m: f64) -> f64 {
    if thermal_k <= 0.1 {
        return 0.0;
    }
    ((thermal_k / 0.1).ln() / BETA_IR_CLEAR).min(IR_OVER_VISUAL * r_atm_visible_m(visibility_m))
}

/// The atmosphere's share of a look at `range_m`: `1 / (1 + (R / R_atm)^6)` (the shape [E]).
pub fn atmosphere(range_m: f64, r_atm_m: f64) -> f64 {
    if r_atm_m <= 0.0 {
        return 0.0;
    }
    1.0 / (1.0 + (range_m / r_atm_m).powi(6))
}

/// The light a band has with the sun at `sun_deg`: its pixels' share (a day camera's none at
/// night, through civil twilight; a low-light camera's 0.4 at night).
pub fn light(band: Band, sun_deg: f64) -> f64 {
    let day = ((sun_deg + TWILIGHT_DEG) / TWILIGHT_DEG).clamp(0.0, 1.0);
    match band {
        Band::Day => day,
        Band::Lowlight => LOWLIGHT_NIGHT + (1.0 - LOWLIGHT_NIGHT) * day,
        Band::Mwir | Band::Lwir => 1.0,
    }
}

/// The thermal crossover: a target's contrast against its background collapses when the sun
/// is low, rising or setting (WORLD-V3-PLAN 2.3; shape [E]: a dip to a quarter of the contrast
/// with the sun 5 degrees up, a few degrees wide).
pub fn crossover(sun_deg: f64) -> f64 {
    1.0 - 0.75 * (-((sun_deg - 5.0) / 5.0).powi(2)).exp()
}

/// One channel's look at a target `range_m` away whose visible critical dimension is `dc_m`
/// and thermal contrast `thermal_k`: the probability, given time, of detecting, recognising
/// and identifying it, in the wide or the narrow field.
pub fn channel_p(ch: &EoChannel, narrow: bool, range_m: f64, dc_m: f64, thermal_k: f64, cond: &Conditions, sun_deg: f64) -> [f64; 3] {
    let lit = light(ch.band, sun_deg);
    if lit <= 0.0 || dc_m <= 0.0 || range_m <= 0.0 {
        return [0.0; 3];
    }
    let (contrast, r_atm) = if ch.band.thermal() {
        let x = crossover(sun_deg);
        (x.sqrt(), r_atm_ir_m(thermal_k * x, cond.visibility_m))
    } else {
        (1.0, r_atm_visible_m(cond.visibility_m))
    };
    let px = dc_m / (range_m * ch.ifov(narrow)) * lit * contrast;
    let a = atmosphere(range_m, r_atm);
    P50_PX.map(|p50| acquire(px, p50) * a)
}

/// The range at which `f(range)` falls to `p` (bisection over 1 m to 1,000 km; `f` falls with
/// range), m.
pub fn range_at(p: f64, f: impl Fn(f64) -> f64) -> f64 {
    let (mut lo, mut hi) = (1.0f64, 1.0e6f64);
    if f(lo) < p {
        return 0.0;
    }
    for _ in 0..80 {
        let mid = (lo * hi).sqrt();
        if f(mid) >= p { lo = mid } else { hi = mid }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(band: Band, fov_deg: f64, px: f64, blur_urad: f64) -> EoChannel {
        let r = fov_deg.to_radians();
        EoChannel { band, wide_rad: r, narrow_rad: r, px, blur_rad: blur_urad * 1e-6, panoramic: false }
    }

    #[test]
    fn acquire_is_half_at_the_threshold_and_steep_around_it() {
        assert!((acquire(1.5, 1.5) - 0.5).abs() < 1e-12);
        assert!(acquire(3.0, 1.5) > 0.94);
        assert!(acquire(0.75, 1.5) < 0.11);
        assert_eq!(acquire(0.0, 1.5), 0.0);
    }

    #[test]
    fn a_day_camera_sees_nothing_at_night_and_lowlight_a_little() {
        let c = Conditions::default();
        let day = ch(Band::Day, 10.0, 1920.0, 0.0);
        assert!(channel_p(&day, false, 2000.0, 3.5, 3.0, &c, 30.0)[0] > 0.9);
        assert_eq!(channel_p(&day, false, 2000.0, 3.5, 3.0, &c, -20.0), [0.0; 3]);
        let ll = ch(Band::Lowlight, 10.0, 1920.0, 0.0);
        let night = channel_p(&ll, false, 2000.0, 3.5, 3.0, &c, -20.0);
        assert!(night[0] > 0.0 && night[2] < channel_p(&ll, false, 2000.0, 3.5, 3.0, &c, 30.0)[2]);
    }

    #[test]
    fn fog_shortens_a_look_and_thermal_reaches_further_than_visible_in_it() {
        let fog = Conditions { visibility_m: 1_000.0, ..Conditions::default() };
        let day = ch(Band::Day, 2.0, 1920.0, 0.0);
        let ir = ch(Band::Lwir, 2.0, 1920.0, 0.0);
        let r_day = range_at(0.5, |r| channel_p(&day, false, r, 2.3, 2.0, &fog, 30.0)[0]);
        let r_ir = range_at(0.5, |r| channel_p(&ir, false, r, 2.3, 2.0, &fog, 30.0)[0]);
        assert!(r_day < 1_000.0 && r_ir > 2.0 * r_day, "{r_day} {r_ir}");
        // the crossover takes most of a thermal camera's reach
        let noon = range_at(0.5, |r| channel_p(&ir, false, r, 2.3, 2.0, &Conditions::default(), 50.0)[0]);
        let dusk = range_at(0.5, |r| channel_p(&ir, false, r, 2.3, 2.0, &Conditions::default(), 5.0)[0]);
        assert!(dusk < 0.6 * noon, "{dusk} {noon}");
    }
}
