//! Several ground stations networked to the C2 (`[[ground_station]]` in the links file).
//!
//! Each station is a radio site with its position and height; the profiles' antenna heights
//! (`gs_antenna_m`) stand on every one. A vehicle's radio link goes through whichever station
//! gives it the best path at that moment, each path judged as the one station's was before:
//! the range curve from that station ([`range_loss`]) and the terrain line of sight to it under
//! the profile's terrain mode ([`terrain_effect`]). Best is the largest share of packets let
//! through, then the nearest, then the first listed. The link (`link.rs`, `Link::set_position`)
//! and the coverage predictor (`coverage.rs`) both choose with [`best`], so a prediction and the
//! emulator agree on the station as they do on the loss.
//!
//! A switch is immediate, at the position report that makes another station better, and costs
//! nothing: no outage, no hold-down. The sites are networked, so the vehicle's uplink is heard by
//! every site in range and the C2 takes the best copy (selection diversity), and the downlink
//! goes out from the site that hears it best. A hold-down would keep a vehicle at the edge on the
//! worse path for its length and make the link depend on its history, which the coverage
//! predictor, a function of the point alone, could no longer match; with no cost to a switch, a
//! vehicle flapping between two sites at the edge changes no packet's fate, only the name the
//! stats show (`station_switches` counts them).
//!
//! A station is used by a profile with a range model, a terrain check or a propagation delay
//! ([`uses_station`]); SATCOM and tethered links use none. An acoustic link, judged on neither
//! range nor terrain, ties on every path and so gets the nearest station: its propagation delay
//! counts from there, as it did from the one.
//!
//! The choice draws nothing: each direction's random stream (`model::DirState`) sees the same
//! draws whichever station carries the link, so with one station every result is as before.

use crate::config::Config;
use crate::geo::GeoPoint;
use crate::model::{Params, range_loss, terrain_effect};

/// One radio site: its name and where its ground is (altitude settled, before the masts).
#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub name: String,
    pub at: GeoPoint,
}

/// The links file's stations, in file order, their altitudes as settled so far
/// (`Config::resolve_ground_alts`).
pub fn from_config(cfg: &Config) -> Vec<Station> {
    cfg.ground_stations
        .iter()
        .enumerate()
        .map(|(i, g)| Station {
            name: cfg.station_name(i),
            at: GeoPoint {
                lat: g.lat,
                lon: g.lon,
                alt_m: g.alt_m.metres(),
            },
        })
        .collect()
}

/// A profile whose link is judged from a station: a range model, a terrain check or a
/// propagation delay. Others (SATCOM, a tether) use none.
pub fn uses_station(p: &Params) -> bool {
    p.range.is_some() || p.terrain.is_some() || p.propagation_mps.is_some()
}

/// The nearest station (the first of equals).
pub fn nearest(ranges: &[f64]) -> Option<usize> {
    let mut out: Option<usize> = None;
    for (i, d) in ranges.iter().enumerate() {
        if out.is_none_or(|j| *d < ranges[j]) {
            out = Some(i);
        }
    }
    out
}

/// Whether the terrain to station `i` is asked for a vehicle whose own profile is `own`, at
/// slant ranges `ranges`: always for the nearest station (so one station is asked wherever
/// the vehicle is, as before), and for the others while the vehicle is inside the profile's
/// `range_max` of them (beyond it the range curve loses every packet, whatever the terrain).
pub fn asks_terrain(own: &Params, ranges: &[f64], i: usize) -> bool {
    match own.range {
        Some((_, max, _)) => nearest(ranges) == Some(i) || ranges.get(i).is_some_and(|d| *d < max),
        None => true,
    }
}

/// The share of packets a path to one station lets through under `active` (the profile
/// carrying the traffic): (1 - range loss)(1 - terrain loss), 0 where the terrain blocks it. An
/// unanswered terrain (`None`) counts open, as the link counts it.
pub fn delivered(active: &Params, range_m: f64, obstruction_m: Option<f64>) -> f64 {
    let rl = active.range.map_or(0.0, |r| range_loss(r, range_m));
    let (blocked, tl) = match (&active.terrain, obstruction_m) {
        (Some(t), Some(o)) => terrain_effect(t, o),
        _ => (false, 0.0),
    };
    if blocked {
        0.0
    } else {
        (1.0 - rl) * (1.0 - tl)
    }
}

/// The station carrying a link under `active`, from each station's slant range and terrain
/// answer: the most delivered, then the nearest, then the first listed. None without stations.
pub fn best(active: &Params, ranges: &[f64], obstructions: &[Option<f64>]) -> Option<usize> {
    let mut out: Option<(usize, f64)> = None;
    for (i, d) in ranges.iter().enumerate() {
        let q = delivered(active, *d, obstructions.get(i).copied().flatten());
        let better = match out {
            None => true,
            Some((j, qj)) => q > qj || (q == qj && *d < ranges[j]),
        };
        if better {
            out = Some((i, q));
        }
    }
    out.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RawProfile;

    fn los() -> Params {
        Params::from_profile(&RawProfile {
            range_ok_km: Some(10.0),
            range_max_km: Some(20.0),
            range_exp: Some(1.0),
            terrain: Some(crate::config::TerrainMode::Ramp),
            terrain_ramp_m: Some(20.0),
            ..RawProfile::default()
        })
        .expect("params")
    }

    #[test]
    fn the_best_path_wins_then_the_nearest_then_the_first() {
        let p = los();
        // inside both clean ranges: the nearest
        assert_eq!(best(&p, &[8_000.0, 5_000.0], &[None, None]), Some(1));
        // equal ranges: the first listed
        assert_eq!(best(&p, &[5_000.0, 5_000.0], &[None, None]), Some(0));
        // the nearer one is behind 10 m of terrain (half lost): the farther, clean, wins
        assert_eq!(
            best(&p, &[12_000.0, 5_000.0], &[Some(-3.0), Some(10.0)]),
            Some(0)
        );
        // both on the range curve, no terrain: the nearer
        assert_eq!(best(&p, &[15_000.0, 18_000.0], &[None, None]), Some(0));
        // every path lost: the nearest
        assert_eq!(best(&p, &[30_000.0, 25_000.0], &[None, None]), Some(1));
        assert_eq!(best(&p, &[], &[]), None);
    }

    #[test]
    fn terrain_is_asked_of_the_nearest_and_of_stations_in_range() {
        let p = los();
        let r = [25_000.0, 30_000.0, 19_000.0, 40_000.0];
        let asked: Vec<usize> = (0..r.len()).filter(|i| asks_terrain(&p, &r, *i)).collect();
        assert_eq!(asked, [2]);
        let r = [25_000.0, 30_000.0];
        let asked: Vec<usize> = (0..r.len()).filter(|i| asks_terrain(&p, &r, *i)).collect();
        assert_eq!(
            asked,
            [0],
            "out of everyone's range: the nearest, as one station is"
        );
        let no_range = Params::from_profile(&RawProfile {
            terrain: Some(crate::config::TerrainMode::Block),
            ..RawProfile::default()
        })
        .expect("params");
        assert!(asks_terrain(&no_range, &[1e9, 2e9], 1));
    }

    #[test]
    fn which_profiles_use_a_station() {
        assert!(uses_station(&los()));
        let sat = Params::from_profile(&RawProfile::default()).expect("params");
        assert!(!uses_station(&sat));
        let acoustic = Params::from_profile(&RawProfile {
            propagation_mps: Some(1500.0),
            ..RawProfile::default()
        })
        .expect("params");
        assert!(uses_station(&acoustic));
    }
}
