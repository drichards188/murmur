//! Access windows: when a satellite's constraint (sensor field of regard, terminal elevation
//! mask) holds for a ground target.
//!
//! Search: the sub-satellite point moves at a bounded rate in Earth central angle, and every
//! geometric constraint is contained in a cone of central angle `psi_max` around the target.
//! So while the satellite is far outside that cone we can jump ahead by
//! (psi - psi_max) / max_rate without missing anything, and only step finely near the target;
//! crossings are refined by bisection. A few dozen SGP4 calls per orbit instead of thousands.

use crate::geo::{
    Observer, ViewGeometry, central_angle_for_elevation, central_angle_for_off_nadir, view_geometry,
};
use crate::orbit::{Satellite, State};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Window search settings.
#[derive(Debug, Clone, Copy)]
pub struct SearchParams {
    /// Step inside the near region, seconds. Windows shorter than this can be missed.
    pub fine_step_s: f64,
    /// Bisection tolerance for window edges, seconds.
    pub edge_tol_s: f64,
}

impl Default for SearchParams {
    fn default() -> Self {
        SearchParams {
            fine_step_s: 5.0,
            edge_tol_s: 0.1,
        }
    }
}

/// A geometric constraint expressed so it can be bounded by a central angle.
#[derive(Debug, Clone, Copy)]
pub enum Constraint {
    /// Elevation of the satellite at the target >= min.
    MinElevation(f64),
    /// Off-nadir angle at the satellite <= max (and the target above the horizon).
    MaxOffNadir(f64),
    /// Incidence at the target within [min, max] (SAR).
    Incidence { min: f64, max: f64 },
}

impl Constraint {
    pub fn holds(&self, g: &ViewGeometry) -> bool {
        match *self {
            Constraint::MinElevation(e) => g.elevation_deg >= e,
            Constraint::MaxOffNadir(m) => g.elevation_deg > 0.0 && g.off_nadir_deg <= m,
            Constraint::Incidence { min, max } => {
                g.elevation_deg > 0.0 && g.incidence_deg >= min && g.incidence_deg <= max
            }
        }
    }

    /// Conservative central-angle bound (radians) for a satellite at most `r_max` km from
    /// the Earth's centre.
    pub fn psi_max(&self, r_max: f64) -> f64 {
        let margin = 0.6f64.to_radians();
        let psi = match *self {
            Constraint::MinElevation(e) => central_angle_for_elevation(r_max, (e - 0.5).max(-0.5)),
            Constraint::MaxOffNadir(m) => central_angle_for_off_nadir(r_max, m + 0.5),
            Constraint::Incidence { max, .. } => {
                central_angle_for_elevation(r_max, (90.0 - max - 0.5).max(-0.5))
            }
        };
        psi + margin
    }
}

/// One contiguous interval in propagation time where a predicate held.
#[derive(Debug, Clone, Copy)]
pub struct RawWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// True when the window was cut by the search interval (it may extend further).
    pub clipped_start: bool,
    pub clipped_end: bool,
}

fn secs(d: f64) -> Duration {
    Duration::microseconds((d * 1e6) as i64)
}

fn geometry_at(
    sat: &Satellite,
    target: &Observer,
    t: &DateTime<Utc>,
) -> Option<(State, ViewGeometry)> {
    let st = sat.state(t).ok()?;
    let alt = st.ecef.norm() - crate::geo::EARTH_MEAN_RADIUS_KM;
    let g = view_geometry(target, &st.ecef, alt);
    Some((st, g))
}

/// Find the windows in [t0, t1] (propagation time) where `pred` holds. `psi_max` (radians)
/// must bound the region where `pred` can hold.
pub fn find_windows<F>(
    sat: &Satellite,
    target: &Observer,
    t0: DateTime<Utc>,
    t1: DateTime<Utc>,
    psi_max: f64,
    params: SearchParams,
    pred: F,
) -> Vec<RawWindow>
where
    F: Fn(&DateTime<Utc>, &ViewGeometry) -> bool,
{
    let rate = sat.max_ground_angular_rate();
    // Narrow cones (a nadir strip imager) are crossed in seconds: step at a third of the
    // fastest crossing so short windows are not stepped over.
    let core = (psi_max - 0.6f64.to_radians()).max(0.0);
    let fine = params.fine_step_s.min((2.0 * core / rate / 3.0).max(0.5));
    let params = SearchParams {
        fine_step_s: fine,
        ..params
    };
    let target_dir = target.ecef.unit();
    let eval = |t: &DateTime<Utc>| -> (bool, f64) {
        match sat.state(t) {
            Ok(st) => {
                let psi = st.ecef.angle_to(&target_dir);
                if psi > psi_max {
                    return (false, psi);
                }
                let alt = st.ecef.norm() - crate::geo::EARTH_MEAN_RADIUS_KM;
                let g = view_geometry(target, &st.ecef, alt);
                (pred(t, &g), psi)
            }
            // Decayed or diverged: treat as never visible, skip a minute.
            Err(_) => (false, psi_max + rate * 60.0),
        }
    };
    let ok_only = |t: &DateTime<Utc>| eval(t).0;
    let bisect = |mut lo: DateTime<Utc>, mut hi: DateTime<Utc>, lo_ok: bool| -> DateTime<Utc> {
        // invariant: ok(lo) == lo_ok, ok(hi) != lo_ok
        while (hi - lo).num_microseconds().unwrap_or(0) as f64 > params.edge_tol_s * 1e6 {
            let mid = lo + (hi - lo) / 2;
            if ok_only(&mid) == lo_ok {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        if lo_ok { lo } else { hi }
    };

    let mut out = Vec::new();
    let mut t = t0;
    let (mut prev_ok, mut psi) = eval(&t);
    let mut prev_t = t;
    let mut open: Option<(DateTime<Utc>, bool)> = if prev_ok { Some((t0, true)) } else { None };
    while t < t1 {
        let step = if !prev_ok && psi > psi_max {
            ((psi - psi_max) / rate).max(params.fine_step_s)
        } else {
            params.fine_step_s
        };
        t = (t + secs(step)).min(t1);
        let (ok, p) = eval(&t);
        psi = p;
        if ok != prev_ok {
            let edge = bisect(prev_t, t, prev_ok);
            if ok {
                open = Some((edge, false));
            } else if let Some((s, clipped)) = open.take() {
                out.push(RawWindow {
                    start: s,
                    end: edge,
                    clipped_start: clipped,
                    clipped_end: false,
                });
            }
        }
        prev_ok = ok;
        prev_t = t;
    }
    if let Some((s, clipped)) = open {
        out.push(RawWindow {
            start: s,
            end: t1,
            clipped_start: clipped,
            clipped_end: true,
        });
    }
    out
}

/// [`find_windows`] for several targets at once, with the satellite propagated once per
/// step: the search is driven by the first target (an area's centroid) with its cone
/// widened by the farthest other target, which contains every target's own cone, and
/// each propagated state is judged for every target; each target's window edges are
/// bisected on their own. One result list per target, in the targets' order. For nine
/// points of an area a few hundred km across this costs about one target's search.
pub fn find_windows_multi<F>(
    sat: &Satellite,
    targets: &[Observer],
    t0: DateTime<Utc>,
    t1: DateTime<Utc>,
    psi_max: f64,
    params: SearchParams,
    pred: F,
) -> Vec<Vec<RawWindow>>
where
    F: Fn(&DateTime<Utc>, &ViewGeometry) -> bool,
{
    let n = targets.len();
    if n == 0 {
        return Vec::new();
    }
    let rate = sat.max_ground_angular_rate();
    let core = (psi_max - 0.6f64.to_radians()).max(0.0);
    let fine = params.fine_step_s.min((2.0 * core / rate / 3.0).max(0.5));
    let params = SearchParams {
        fine_step_s: fine,
        ..params
    };
    let center_dir = targets[0].ecef.unit();
    let spread = targets
        .iter()
        .map(|o| o.ecef.unit().angle_to(&center_dir))
        .fold(0.0, f64::max);
    let psi_search = psi_max + spread;
    // one propagation, every target judged
    let eval_all = |t: &DateTime<Utc>| -> (Vec<bool>, f64) {
        match sat.state(t) {
            Ok(st) => {
                let psi = st.ecef.angle_to(&center_dir);
                if psi > psi_search {
                    return (vec![false; n], psi);
                }
                let alt = st.ecef.norm() - crate::geo::EARTH_MEAN_RADIUS_KM;
                let oks = targets
                    .iter()
                    .map(|o| pred(t, &view_geometry(o, &st.ecef, alt)))
                    .collect();
                (oks, psi)
            }
            Err(_) => (vec![false; n], psi_search + rate * 60.0),
        }
    };
    let eval_one = |i: usize, t: &DateTime<Utc>| -> bool {
        geometry_at(sat, &targets[i], t).is_some_and(|(_, g)| pred(t, &g))
    };
    let bisect = |i: usize, mut lo: DateTime<Utc>, mut hi: DateTime<Utc>, lo_ok: bool| -> DateTime<Utc> {
        while (hi - lo).num_microseconds().unwrap_or(0) as f64 > params.edge_tol_s * 1e6 {
            let mid = lo + (hi - lo) / 2;
            if eval_one(i, &mid) == lo_ok {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        if lo_ok { lo } else { hi }
    };

    let mut out: Vec<Vec<RawWindow>> = vec![Vec::new(); n];
    let mut t = t0;
    let (mut prev_ok, mut psi) = eval_all(&t);
    let mut prev_t = t;
    let mut open: Vec<Option<(DateTime<Utc>, bool)>> =
        prev_ok.iter().map(|&ok| ok.then_some((t0, true))).collect();
    while t < t1 {
        let any = prev_ok.iter().any(|&ok| ok);
        let step = if !any && psi > psi_search {
            ((psi - psi_search) / rate).max(params.fine_step_s)
        } else {
            params.fine_step_s
        };
        t = (t + secs(step)).min(t1);
        let (ok, p) = eval_all(&t);
        psi = p;
        for i in 0..n {
            if ok[i] == prev_ok[i] {
                continue;
            }
            let edge = bisect(i, prev_t, t, prev_ok[i]);
            if ok[i] {
                open[i] = Some((edge, false));
            } else if let Some((s, clipped)) = open[i].take() {
                out[i].push(RawWindow {
                    start: s,
                    end: edge,
                    clipped_start: clipped,
                    clipped_end: false,
                });
            }
        }
        prev_ok = ok;
        prev_t = t;
    }
    for (i, o) in open.into_iter().enumerate() {
        if let Some((s, clipped)) = o {
            out[i].push(RawWindow {
                start: s,
                end: t1,
                clipped_start: clipped,
                clipped_end: true,
            });
        }
    }
    out
}

/// Time of maximum elevation within [a, b] (golden-section search; elevation is unimodal
/// over a single pass).
pub fn peak_time(
    sat: &Satellite,
    target: &Observer,
    a: DateTime<Utc>,
    b: DateTime<Utc>,
) -> DateTime<Utc> {
    let el =
        |t: &DateTime<Utc>| geometry_at(sat, target, t).map_or(-90.0, |(_, g)| g.elevation_deg);
    let span = (b - a).num_microseconds().unwrap_or(0) as f64 * 1e-6;
    if span <= 0.2 {
        return a;
    }
    let gr = (5f64.sqrt() - 1.0) / 2.0;
    let (mut lo, mut hi) = (0.0, span);
    let mut x1 = hi - gr * (hi - lo);
    let mut x2 = lo + gr * (hi - lo);
    let mut f1 = el(&(a + secs(x1)));
    let mut f2 = el(&(a + secs(x2)));
    while hi - lo > 0.2 {
        if f1 < f2 {
            lo = x1;
            x1 = x2;
            f1 = f2;
            x2 = lo + gr * (hi - lo);
            f2 = el(&(a + secs(x2)));
        } else {
            hi = x2;
            x2 = x1;
            f2 = f1;
            x1 = hi - gr * (hi - lo);
            f1 = el(&(a + secs(x1)));
        }
    }
    a + secs((lo + hi) / 2.0)
}

pub fn geometry(sat: &Satellite, target: &Observer, t: &DateTime<Utc>) -> Option<ViewGeometry> {
    geometry_at(sat, target, t).map(|(_, g)| g)
}

/// Half-open interval list helpers (times in any frame).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Interval {
    pub fn duration_s(&self) -> f64 {
        (self.end - self.start).num_milliseconds() as f64 / 1000.0
    }
}

/// Union of intervals, sorted and merged.
pub fn union(mut v: Vec<Interval>) -> Vec<Interval> {
    v.sort_by_key(|i| i.start);
    let mut out: Vec<Interval> = Vec::new();
    for i in v {
        match out.last_mut() {
            Some(last) if i.start <= last.end => {
                if i.end > last.end {
                    last.end = i.end;
                }
            }
            _ => out.push(i),
        }
    }
    out
}

/// Gaps of `covered` (merged, sorted) within [from, to].
pub fn gaps(covered: &[Interval], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut cursor = from;
    for c in covered {
        if c.start > cursor {
            out.push(Interval {
                start: cursor,
                end: c.start.min(to),
            });
        }
        cursor = cursor.max(c.end);
        if cursor >= to {
            break;
        }
    }
    if cursor < to {
        out.push(Interval {
            start: cursor,
            end: to,
        });
    }
    out.retain(|g| g.end > g.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 14, h, m, 0)
            .single()
            .expect("t")
    }

    #[test]
    fn union_and_gaps() {
        let v = vec![
            Interval {
                start: t(1, 0),
                end: t(1, 10),
            },
            Interval {
                start: t(1, 5),
                end: t(1, 20),
            },
            Interval {
                start: t(2, 0),
                end: t(2, 5),
            },
        ];
        let u = union(v);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].end, t(1, 20));
        let g = gaps(&u, t(0, 0), t(3, 0));
        assert_eq!(g.len(), 3);
        assert_eq!(
            g[1],
            Interval {
                start: t(1, 20),
                end: t(2, 0)
            }
        );
    }

    #[test]
    fn psi_bounds_are_conservative() {
        let r = 6378.0 + 600.0;
        let c = Constraint::MaxOffNadir(30.0);
        // A target exactly at 30 deg off-nadir sits at the bound minus the margins.
        assert!(c.psi_max(r) > central_angle_for_off_nadir(r, 30.0));
    }
}
