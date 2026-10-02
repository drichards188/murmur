//! Dilution of precision from satellite geometry (no signal, clock or jamming model).

use crate::geo::ViewGeometry;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Dop {
    pub gdop: f64,
    pub pdop: f64,
    pub hdop: f64,
    pub vdop: f64,
    pub tdop: f64,
}

/// DOP from the satellites in view (needs at least 4). Geometry rows are the unit
/// line-of-sight vectors in east/north/up plus the receiver clock term.
pub fn dop(view: &[ViewGeometry]) -> Option<Dop> {
    if view.len() < 4 {
        return None;
    }
    let mut n = [[0.0f64; 4]; 4];
    for g in view {
        let (el, az) = (g.elevation_deg.to_radians(), g.azimuth_deg.to_radians());
        let row = [el.cos() * az.sin(), el.cos() * az.cos(), el.sin(), 1.0];
        for i in 0..4 {
            for j in 0..4 {
                n[i][j] += row[i] * row[j];
            }
        }
    }
    let q = invert4(n)?;
    let (e, nn, u, t) = (q[0][0], q[1][1], q[2][2], q[3][3]);
    if e < 0.0 || nn < 0.0 || u < 0.0 || t < 0.0 {
        return None;
    }
    Some(Dop {
        gdop: (e + nn + u + t).sqrt(),
        pdop: (e + nn + u).sqrt(),
        hdop: (e + nn).sqrt(),
        vdop: u.sqrt(),
        tdop: t.sqrt(),
    })
}

/// Gauss-Jordan inverse with partial pivoting; None when singular.
fn invert4(m: [[f64; 4]; 4]) -> Option<[[f64; 4]; 4]> {
    let mut a = [[0.0; 8]; 4];
    for i in 0..4 {
        a[i][..4].copy_from_slice(&m[i]);
        a[i][4 + i] = 1.0;
    }
    for c in 0..4 {
        let p = (c..4).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs()))?;
        if a[p][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        let d = a[c][c];
        for v in a[c].iter_mut() {
            *v /= d;
        }
        for r in 0..4 {
            if r != c {
                let f = a[r][c];
                if f != 0.0 {
                    let pivot_row = a[c];
                    for (v, pv) in a[r].iter_mut().zip(pivot_row.iter()) {
                        *v -= f * pv;
                    }
                }
            }
        }
    }
    let mut out = [[0.0; 4]; 4];
    for i in 0..4 {
        out[i].copy_from_slice(&a[i][4..]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(el: f64, az: f64) -> ViewGeometry {
        ViewGeometry {
            elevation_deg: el,
            azimuth_deg: az,
            range_km: 20000.0,
            off_nadir_deg: 0.0,
            incidence_deg: 90.0 - el,
            sat_alt_km: 20200.0,
        }
    }

    #[test]
    fn ideal_tetrahedron() {
        // One overhead, three at -19.47 deg elevation 120 deg apart (a regular tetrahedron):
        // the best possible 4-satellite geometry, GDOP = sqrt(2.5) = 1.58.
        let v = [
            g(90.0, 0.0),
            g(-19.47, 0.0),
            g(-19.47, 120.0),
            g(-19.47, 240.0),
        ];
        let d = dop(&v).expect("dop");
        assert!((d.gdop - 1.58).abs() < 0.01, "{d:?}");
    }

    #[test]
    fn needs_four_and_spread() {
        assert!(dop(&[g(45.0, 0.0), g(45.0, 90.0), g(45.0, 180.0)]).is_none());
        // Four satellites all in one spot are singular.
        assert!(dop(&[g(45.0, 10.0); 4]).is_none());
        let typical = [
            g(80.0, 0.0),
            g(30.0, 0.0),
            g(30.0, 90.0),
            g(30.0, 180.0),
            g(30.0, 270.0),
            g(15.0, 45.0),
        ];
        let d = dop(&typical).expect("dop");
        assert!(d.pdop > 1.0 && d.pdop < 3.0 && d.hdop < d.pdop);
    }
}
