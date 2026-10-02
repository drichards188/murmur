//! Great-circle helpers and a small normal-distribution source, same formulas as standins.py.

const EARTH_R: f64 = 6_371_000.0;
/// Metres per degree of latitude, the flat-earth step the Python engine integrates with.
pub const M_PER_DEG: f64 = 111_320.0;

pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_R * a.sqrt().asin()
}

/// Python's `(a + 180) % 360 - 180` (floored modulo).
pub fn wrap180(a: f64) -> f64 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// xorshift64* with Box-Muller: enough for drifter jitter, no dependency.
pub struct Gauss {
    state: u64,
    spare: Option<f64>,
}

impl Gauss {
    pub fn new(seed: u64) -> Self {
        Gauss {
            state: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            spare: None,
        }
    }

    fn uniform(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let r = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // (0, 1]: never 0, so ln() below is finite
        ((r >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    pub fn sample(&mut self, mu: f64, sigma: f64) -> f64 {
        if let Some(z) = self.spare.take() {
            return mu + sigma * z;
        }
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        let t = std::f64::consts::TAU * u2;
        self.spare = Some(r * t.sin());
        mu + sigma * r * t.cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_reference_values() {
        // values from standins.py's bearing_deg / distance_m
        assert!((bearing_deg(37.8, -122.5, 37.81, -122.49) - 38.309_252).abs() < 1e-5);
        assert!((distance_m(37.8, -122.5, 37.81, -122.49) - 1_417.140_149).abs() < 1e-3);
        assert_eq!(wrap180(190.0), -170.0);
        assert_eq!(wrap180(-190.0), 170.0);
        assert_eq!(wrap180(180.0), -180.0);
    }

    #[test]
    fn gauss_has_the_right_spread() {
        let mut g = Gauss::new(7);
        let n = 20_000;
        let xs: Vec<f64> = (0..n).map(|_| g.sample(0.0, 0.02)).collect();
        let mean = xs.iter().sum::<f64>() / n as f64;
        let sd = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
        assert!(mean.abs() < 0.001);
        assert!((sd - 0.02).abs() < 0.001);
    }
}
