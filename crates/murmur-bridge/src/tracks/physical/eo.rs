//! EO/IR acquisition (WORLD-V3-PLAN 2.3, research section 1): pixels on target against the
//! Johnson-criteria thresholds through the ACQUIRE target transfer probability function, an
//! atmosphere factor from visibility (visible bands) or thermal contrast (IR) along the line of
//! sight's heights (the extinction falls with height: murmur-7vp), the light (a day camera by day
//! only, a low-light camera at a fraction of its range by night) and the thermal crossover at
//! dawn and dusk.

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
    if px.is_nan() || px <= 0.0 {
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

/// The thermal bands' atmospheric range in clear air, m: `ln(dT / 0.1 K) / beta` [10] (none for
/// a contrast at the sensor's 0.1 K).
pub fn r_atm_ir_clear_m(thermal_k: f64) -> f64 {
    if thermal_k <= 0.1 {
        return 0.0;
    }
    (thermal_k / 0.1).ln() / BETA_IR_CLEAR
}

/// The thermal bands' atmospheric range in haze or fog, m: 4.5 times the visible band's [11].
pub fn r_atm_ir_haze_m(visibility_m: f64) -> f64 {
    IR_OVER_VISUAL * r_atm_visible_m(visibility_m)
}

/// The atmosphere's share of a look through `range_m` of sea-level air: `1 / (1 + (R / R_atm)^6)`
/// (the shape [E]). A slant path's `R` is its sea-level equivalent ([`sea_level_m`]).
pub fn atmosphere(range_m: f64, r_atm_m: f64) -> f64 {
    if r_atm_m <= 0.0 {
        return 0.0;
    }
    1.0 / (1.0 + (range_m / r_atm_m).powi(6))
}

/// The aerosol's extinction falls with height as `exp(-h / H)` with this scale height, m [E
/// here]: over the sea the aerosol is held in the marine boundary layer, about 1 km deep by
/// lidar, with far less above its capping inversion; 1.2 km is the middle of the 1 to 1.5 km
/// such profiles take (docs/SENSING.md, the slant path). It carries the visible band's
/// extinction (the surface's visibility) and the thermal bands' in haze.
pub const AEROSOL_SCALE_M: f64 = 1_200.0;
/// The thermal bands' clear-air extinction is mostly water vapour's, whose density falls with a
/// scale height of about 1.3 to 2 km [E here]: the longer, so a camera high up is not given more
/// than the air allows.
pub const VAPOUR_SCALE_M: f64 = 2_000.0;

/// The sea-level path with the extinction of a straight line of sight `range_m` long between
/// heights `h` above the sea (either end first, m) through air whose extinction falls as
/// `exp(-h / scale_m)`: `range_m` times the profile's mean along the line, `exp(-h_lo / H) (1 -
/// exp(-(h_hi - h_lo) / H)) / ((h_hi - h_lo) / H)` (heights below the sea count as at it; the
/// earth's curvature lifts a long path a little, ignored). A horizontal path at the sea is its
/// own length.
pub fn sea_level_m(range_m: f64, h: [f64; 2], scale_m: f64) -> f64 {
    let (lo, hi) = (h[0].min(h[1]).max(0.0), h[0].max(h[1]).max(0.0));
    let base = (-lo / scale_m).exp();
    let x = (hi - lo) / scale_m;
    let mean = if x < 1e-9 { base } else { base * -(-x).exp_m1() / x };
    range_m * mean
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

/// One channel's look at a target `range_m` away (slant) whose visible critical dimension is
/// `dc_m` and thermal contrast `thermal_k`, the line of sight between heights `h` above the sea
/// (the sensor's and the target's, m): the probability, given time, of detecting, recognising and
/// identifying it, in the wide or the narrow field. The atmosphere is the line's own: the
/// visibility and the thermal coefficient are the surface's, and the line meets them as its
/// sea-level equivalent ([`sea_level_m`]); a thermal band is held by the clear air's water vapour
/// or the haze's aerosol, whichever leaves it less. At the sea's level this is the horizontal
/// model unchanged.
#[allow(clippy::too_many_arguments)]
pub fn channel_p(ch: &EoChannel, narrow: bool, range_m: f64, h: [f64; 2], dc_m: f64, thermal_k: f64, cond: &Conditions, sun_deg: f64) -> [f64; 3] {
    let lit = light(ch.band, sun_deg);
    if lit <= 0.0 || dc_m <= 0.0 || range_m <= 0.0 {
        return [0.0; 3];
    }
    let aerosol = sea_level_m(range_m, h, AEROSOL_SCALE_M);
    let (contrast, a) = if ch.band.thermal() {
        let x = crossover(sun_deg);
        let clear = atmosphere(sea_level_m(range_m, h, VAPOUR_SCALE_M), r_atm_ir_clear_m(thermal_k * x));
        (x.sqrt(), clear.min(atmosphere(aerosol, r_atm_ir_haze_m(cond.visibility_m))))
    } else {
        (1.0, atmosphere(aerosol, r_atm_visible_m(cond.visibility_m)))
    };
    let px = dc_m / (range_m * ch.ifov(narrow)) * lit * contrast;
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

    /// A line of sight along the sea.
    const SEA: [f64; 2] = [0.0, 0.0];

    fn ch(band: Band, fov_deg: f64, px: f64, blur_urad: f64) -> EoChannel {
        let r = fov_deg.to_radians();
        EoChannel { band, wide_rad: r, narrow_rad: r, px, blur_rad: blur_urad * 1e-6, panoramic: false }
    }

    /// The model before the slant path (murmur-7vp): the whole path at the surface's visibility,
    /// a thermal band's range the smaller of the clear air's and the haze's.
    fn horizontal(c: &EoChannel, range_m: f64, dc_m: f64, thermal_k: f64, cond: &Conditions, sun_deg: f64) -> [f64; 3] {
        let lit = light(c.band, sun_deg);
        if lit <= 0.0 || dc_m <= 0.0 || range_m <= 0.0 {
            return [0.0; 3];
        }
        let (contrast, r_atm) = if c.band.thermal() {
            let x = crossover(sun_deg);
            let k = thermal_k * x;
            (x.sqrt(), if k <= 0.1 { 0.0 } else { r_atm_ir_clear_m(k).min(r_atm_ir_haze_m(cond.visibility_m)) })
        } else {
            (1.0, r_atm_visible_m(cond.visibility_m))
        };
        let px = dc_m / (range_m * c.ifov(false)) * lit * contrast;
        let a = atmosphere(range_m, r_atm);
        P50_PX.map(|p50| acquire(px, p50) * a)
    }

    /// Along the sea the slant-path atmosphere is the horizontal model, value for value, in every
    /// band, visibility, contrast and light; a mast's few metres change a range by under 1 %.
    #[test]
    fn a_horizontal_path_at_the_sea_is_the_model_before() {
        for band in [Band::Day, Band::Lowlight, Band::Mwir, Band::Lwir] {
            let c = ch(band, 2.0, 1280.0, 20.0);
            for vis in [1_000.0, 5_000.0, 20_000.0, 80_000.0] {
                let cond = Conditions { visibility_m: vis, ..Conditions::default() };
                for sun in [-20.0, 0.0, 5.0, 45.0] {
                    for dt in [0.05, 0.5, 2.0, 10.0] {
                        for r in [10.0, 300.0, 2_000.0, 9_000.0, 25_000.0, 120_000.0] {
                            assert_eq!(channel_p(&c, false, r, SEA, 3.0, dt, &cond, sun), horizontal(&c, r, 3.0, dt, &cond, sun), "{band:?} {vis} {sun} {dt} {r}");
                        }
                    }
                }
                let along = |h: [f64; 2]| range_at(0.5, |r| channel_p(&c, false, r, h, 3.0, 2.0, &cond, 45.0)[0]);
                let (sea, mast) = (along(SEA), along([10.0, 3.0]));
                assert!(mast >= sea && mast < 1.01 * sea, "{band:?} {vis}: {sea} {mast}");
            }
        }
    }

    /// The sea-level equivalent of a line of sight: its own length along the sea, `exp(-h / H)` of
    /// it level at a height, the same either end first, never more than its length, and a
    /// vertical path's whole column `H (1 - exp(-h / H))`.
    #[test]
    fn a_slant_path_meets_the_air_its_heights_hold() {
        assert_eq!(sea_level_m(5_000.0, SEA, AEROSOL_SCALE_M), 5_000.0);
        let level = sea_level_m(5_000.0, [600.0, 600.0], AEROSOL_SCALE_M);
        assert!((level - 5_000.0 * (-0.5f64).exp()).abs() < 1e-6, "{level}");
        assert_eq!(sea_level_m(9_000.0, [30.0, 7_000.0], AEROSOL_SCALE_M), sea_level_m(9_000.0, [7_000.0, 30.0], AEROSOL_SCALE_M));
        let up = sea_level_m(18_000.0, [0.0, 18_000.0], AEROSOL_SCALE_M);
        assert!((up - AEROSOL_SCALE_M * (1.0 - (-15.0f64).exp())).abs() < 1e-6, "{up}");
        for h in [[0.0, 10.0], [5.0, 300.0], [0.0, 20_000.0], [-50.0, 2.0]] {
            assert!(sea_level_m(1_000.0, h, VAPOUR_SCALE_M) <= 1_000.0 + 1e-9, "{h:?}");
            assert!(sea_level_m(1_000.0, h, AEROSOL_SCALE_M) <= sea_level_m(1_000.0, h, VAPOUR_SCALE_M), "{h:?}");
        }
    }

    /// A camera high up looks down through the boundary layer's haze once, not along the whole
    /// slant at the surface's visibility: an RQ-4-class day camera (1,024 px over 0.59 degrees, a
    /// 10 urad blur floor) at 18 km detects, recognises and identifies a vehicle (NATO 2.3 m, 2 K)
    /// on a clear day at about the slant ranges its geometry and blur allow (within 15 %, the
    /// identification within 2 %), where the horizontal model gave it nothing (0.17 straight
    /// down); its MWIR channel (640 px over 1.1 degrees, 30 urad) likewise; and in fog (1 km) the
    /// same camera still sees less than on a clear day.
    #[test]
    fn a_high_camera_looking_down_reaches_what_its_geometry_and_blur_allow() {
        let clear = Conditions::default();
        for c in [ch(Band::Day, 0.59, 1024.0, 10.0), ch(Band::Mwir, 1.1, 640.0, 30.0)] {
            let geometry: Vec<f64> = (0..3).map(|l| range_at(0.5, |r| acquire(2.3 / (r * c.ifov(false)), P50_PX[l]))).collect();
            let slant = |l: usize, cond: &Conditions| range_at(0.5, |r| channel_p(&c, false, r, [18_000.0, 2.0], 2.3, 2.0, cond, 45.0)[l]);
            for (l, g) in geometry.iter().enumerate() {
                let s = slant(l, &clear);
                let tol = if l == 2 { 0.02 } else { 0.15 };
                assert!(s <= *g && s >= (1.0 - tol) * g, "{:?} level {l}: {s} against {g}", c.band);
            }
            let fog = Conditions { visibility_m: 1_000.0, ..Conditions::default() };
            assert!(slant(0, &fog) < 0.9 * slant(0, &clear), "{:?}", c.band);
        }
        // the horizontal model: straight down, 18 km of sea-level air
        let day = ch(Band::Day, 0.59, 1024.0, 10.0);
        let before = horizontal(&day, 18_000.0, 2.3, 2.0, &clear, 45.0)[0];
        let now = channel_p(&day, false, 18_000.0, [18_000.0, 2.0], 2.3, 2.0, &clear, 45.0)[0];
        assert!(before < 0.2 && now > 0.99, "{before} {now}");
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
        assert!(channel_p(&day, false, 2000.0, SEA, 3.5, 3.0, &c, 30.0)[0] > 0.9);
        assert_eq!(channel_p(&day, false, 2000.0, SEA, 3.5, 3.0, &c, -20.0), [0.0; 3]);
        let ll = ch(Band::Lowlight, 10.0, 1920.0, 0.0);
        let night = channel_p(&ll, false, 2000.0, SEA, 3.5, 3.0, &c, -20.0);
        assert!(night[0] > 0.0 && night[2] < channel_p(&ll, false, 2000.0, SEA, 3.5, 3.0, &c, 30.0)[2]);
    }

    #[test]
    fn fog_shortens_a_look_and_thermal_reaches_further_than_visible_in_it() {
        let fog = Conditions { visibility_m: 1_000.0, ..Conditions::default() };
        let day = ch(Band::Day, 2.0, 1920.0, 0.0);
        let ir = ch(Band::Lwir, 2.0, 1920.0, 0.0);
        let r_day = range_at(0.5, |r| channel_p(&day, false, r, SEA, 2.3, 2.0, &fog, 30.0)[0]);
        let r_ir = range_at(0.5, |r| channel_p(&ir, false, r, SEA, 2.3, 2.0, &fog, 30.0)[0]);
        assert!(r_day < 1_000.0 && r_ir > 2.0 * r_day, "{r_day} {r_ir}");
        // the crossover takes most of a thermal camera's reach
        let noon = range_at(0.5, |r| channel_p(&ir, false, r, SEA, 2.3, 2.0, &Conditions::default(), 50.0)[0]);
        let dusk = range_at(0.5, |r| channel_p(&ir, false, r, SEA, 2.3, 2.0, &Conditions::default(), 5.0)[0]);
        assert!(dusk < 0.6 * noon, "{dusk} {noon}");
    }
}
