//! Decision rules 34, stage 2 (docs/SWARMS.md section 13): a group order's layout. The claims:
//! the same facts give the same layout, whatever order the members come in; no member is laid out
//! outside its band (a layout that cannot keep them in it and apart is refused, saying the most
//! that fit); no two members' straight paths to their slots cross; a re-form moves only the
//! neighbours of its gaps, never a member whose link is not healthy; and Saturation's 36 opening
//! orders are laid out as section 3.3 says. Unit and property tests on seeded random cases and on
//! Saturation's own orders, fleet and catalog.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use murmur_decide::catalog::Catalog;
use murmur_decide::group::{self, Band, GroupSpec, Layout, Member, Spread};
use murmur_decide::{LonLat, Task, Uid};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn catalog() -> Catalog {
    Catalog::load(&root().join("sim/platforms.toml")).unwrap().formed()
}

/// A seeded xorshift: the same cases every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

const M_PER_DEG: f64 = 111_320.0;

fn offset(p: LonLat, east: f64, north: f64) -> LonLat {
    LonLat::new(p.lon + east / (M_PER_DEG * p.lat.to_radians().cos()), p.lat + north / M_PER_DEG)
}

fn xy(origin: LonLat, p: LonLat) -> (f64, f64) {
    ((p.lon - origin.lon) * M_PER_DEG * origin.lat.to_radians().cos(), (p.lat - origin.lat) * M_PER_DEG)
}

/// `n` members of platform `pid`, numbered from `first`, at `at(i)`.
fn members(c: &Catalog, pid: &str, n: usize, first: usize, at: impl Fn(usize) -> Option<LonLat>) -> Vec<Member> {
    let p = c.get(pid).unwrap();
    let figures = group::figures(p);
    let fixed_wing = p.autopilot.is_some_and(|a| a.is_fixed_wing());
    (0..n)
        .map(|i| Member {
            uid: Uid::new(format!("{pid}-{:04}", first + i)),
            at: at(i),
            alt_m: None,
            domain: p.domain,
            fixed_wing,
            turn_m: if fixed_wing { figures.loiter_m } else { p.min_turn_radius_m },
            figures,
        })
        .collect()
}

fn ring(center: LonLat, radius_m: f64) -> Task {
    Task::Orbit { center, radius_m }
}

fn spec(alt_m: Option<f64>) -> GroupSpec {
    GroupSpec {
        alt_m,
        ..GroupSpec::default()
    }
}

const GATE: LonLat = LonLat::new(-122.4783, 37.8199);

/// Every member's altitude inside its band (or the layout refused, with nothing laid out).
fn inside_bands(lay: &Layout, ms: &[Member], alt_m: Option<f64>) -> Result<(), String> {
    if lay.refused.is_some() {
        return if lay.shares.is_empty() { Ok(()) } else { Err("a refused layout lays members out".into()) };
    }
    for m in ms {
        let s = lay.shares.get(&m.uid).ok_or(format!("{} not laid out", m.uid))?;
        let f = m.figures;
        let ok = match (f.band, s.alt_m) {
            (Band::Surface, None) => true,
            (Band::AboveGround { min_m, max_m }, Some(a)) => a >= min_m - 0.05 && a <= max_m + 0.05,
            (Band::AroundOrder { half_m }, Some(a)) => {
                let base = alt_m.or(f.cruise_alt_m).unwrap_or(half_m);
                (a - base).abs() <= half_m + 0.05
            }
            (Band::Depth { top_m, .. }, Some(a)) => -a >= top_m - 0.05,
            _ => false,
        };
        if !ok {
            return Err(format!("{} at {:?} outside its band {:?}", m.uid, s.alt_m, f.band));
        }
    }
    Ok(())
}

#[test]
fn the_same_facts_give_the_same_layout() {
    let c = catalog();
    let mut rng = Rng(7);
    let ms: Vec<Member> = members(&c, "x10", 40, 0, |_| None)
        .into_iter()
        .map(|mut m| {
            m.at = Some(offset(GATE, rng.range(-900.0, 900.0), rng.range(-900.0, 900.0)));
            m
        })
        .collect();
    let task = ring(GATE, 150.0);
    let a = group::layout(&task, &spec(Some(60.0)), &ms);
    assert!(a.refused.is_none(), "{:?}", a.refused);
    let mut shuffled = ms.clone();
    shuffled.reverse();
    shuffled.swap(3, 17);
    let b = group::layout(&task, &spec(Some(60.0)), &shuffled);
    assert_eq!(a, b, "the order the members come in changes nothing");
    assert_eq!(a, group::layout(&task, &spec(Some(60.0)), &ms), "and twice is the same");
    // every slot taken once
    let slots: BTreeSet<u32> = a.shares.values().map(|s| s.slot).collect();
    assert_eq!(slots.len(), 40);
}

/// Seeded random rings and routes of each of Saturation's types, some too dense or too high for
/// their band: every layout that holds keeps each member in its band, and every one that does not
/// is refused with nothing laid out and the most that fit.
#[test]
fn no_member_is_laid_out_outside_its_band() {
    let c = catalog();
    let mut rng = Rng(11);
    let mut held = 0;
    let mut refused = 0;
    for case in 0..300 {
        let pid = ["x10", "puma", "remus", "saildrone", "corsair"][case % 5];
        let n = 2 + (rng.next() % 90) as usize;
        let center = offset(GATE, rng.range(-2000.0, 2000.0), rng.range(-2000.0, 2000.0));
        let ms = members(&c, pid, n, 0, |i| Some(offset(center, (i as f64) * 7.0, -400.0)));
        let alt = match pid {
            "x10" => Some(rng.range(20.0, 140.0)),
            "puma" => Some(rng.range(300.0, 1200.0)),
            "remus" => Some(rng.range(5.0, 80.0)),
            _ => None,
        };
        let task = if case % 3 == 0 {
            Task::Patrol {
                route: vec![center, offset(center, rng.range(-3000.0, 3000.0), rng.range(-3000.0, 3000.0))],
            }
        } else {
            ring(center, rng.range(40.0, 900.0))
        };
        let lay = group::layout(&task, &spec(alt), &ms);
        inside_bands(&lay, &ms, alt).unwrap_or_else(|e| panic!("case {case} ({pid}, {n}): {e}"));
        match &lay.refused {
            Some(why) => {
                refused += 1;
                assert!(lay.shares.is_empty());
                assert!(why.contains("holds") || why.contains("outside") || why.contains("no layer") || why.contains("above its band"), "case {case}: {why}");
            }
            None => {
                held += 1;
                assert_eq!(lay.shares.len(), n);
                assert_eq!(group::separation(&lay, &ms), None, "case {case}: laid out apart");
            }
        }
    }
    assert!(held > 100 && refused > 20, "both kinds of case met: {held} held, {refused} refused");
}

/// Two segments' proper crossing on the plane.
fn crosses(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> bool {
    let o = |p: (f64, f64), q: (f64, f64), r: (f64, f64)| (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
    let (d1, d2, d3, d4) = (o(a, b, c), o(a, b, d), o(c, d, a), o(c, d, b));
    d1 * d2 < -1e-6 && d3 * d4 < -1e-6
}

/// Members scattered round a ring are paired with its slots by minimum total distance, so no two
/// straight paths to their slots cross (exchanging the ends of two crossing paths would shorten
/// their sum), seeded cases of every spread that lays out.
#[test]
fn no_two_paths_to_the_slots_cross() {
    let c = catalog();
    let mut rng = Rng(23);
    for case in 0..60 {
        let n = 5 + (rng.next() % 40) as usize;
        let center = GATE;
        let pts: Vec<LonLat> = (0..n).map(|_| offset(center, rng.range(-1500.0, 1500.0), rng.range(-1500.0, 1500.0))).collect();
        let ms = members(&c, "corsair", n, 0, |i| Some(pts[i]));
        let task = if case % 2 == 0 {
            ring(center, rng.range(300.0, 900.0))
        } else {
            Task::Patrol {
                route: vec![offset(center, -2000.0, 0.0), offset(center, 0.0, 1500.0), offset(center, 2000.0, -300.0)],
            }
        };
        let lay = group::layout(&task, &GroupSpec::default(), &ms);
        assert!(lay.refused.is_none(), "case {case}: {:?}", lay.refused);
        let paths: Vec<((f64, f64), (f64, f64))> = ms
            .iter()
            .map(|m| (xy(center, m.at.unwrap()), xy(center, lay.shares[&m.uid].at.unwrap())))
            .collect();
        for (i, p) in paths.iter().enumerate() {
            for q in &paths[i + 1..] {
                assert!(!crosses(p.0, p.1, q.0, q.1), "case {case}: paths cross");
            }
        }
    }
}

#[test]
fn too_many_for_the_band_is_refused_with_the_most_that_fit() {
    let c = catalog();
    // 100 quadcopters on a 30 m ring: 188 m round, 12 a layer at 15 m apart, 10 layers needed
    // from 100 m in a band that ends at 120 m (three layers: 100, 110, 120 m)
    let ms = members(&c, "x10", 100, 0, |_| None);
    let lay = group::layout(&ring(GATE, 30.0), &spec(Some(100.0)), &ms);
    let why = lay.refused.expect("refused");
    assert!(why.contains("a 30 m ring holds 36 air members in 3 layer(s) (100 m, 110 m, 120 m)"), "{why}");
    assert!(why.contains("100 do not fit"), "{why}");
    // a base outside the band: no layer at all
    let lay = group::layout(&ring(GATE, 400.0), &spec(Some(300.0)), &ms);
    assert!(lay.refused.unwrap().contains("no layer fits"));
    // Saildrones on a small ring: concentric rings only within a quarter of the radius
    let sd = members(&c, "saildrone", 100, 0, |_| None);
    let lay = group::layout(&ring(GATE, 150.0), &GroupSpec::default(), &sd);
    let why = lay.refused.expect("refused");
    assert!(why.contains("a 150 m ring holds"), "{why}");
}

#[test]
fn a_hold_is_never_refused_and_warns_of_overlapping_circles() {
    let c = catalog();
    let ms = members(&c, "puma", 3, 0, |i| Some(offset(GATE, i as f64 * 100.0, 0.0)));
    let lay = group::layout(&Task::Hold {}, &GroupSpec::default(), &ms);
    assert!(lay.refused.is_none());
    assert_eq!(lay.shares.len(), 3);
    assert!(lay.shares.values().all(|s| s.alt_m.is_none()), "each holds at its own height");
    assert_eq!(lay.warnings.len(), 1, "{:?}", lay.warnings);
    assert!(lay.warnings[0].starts_with("3 pair(s) of fixed wings hold on loiter circles that overlap"), "{:?}", lay.warnings);
    let apart = members(&c, "puma", 2, 0, |i| Some(offset(GATE, i as f64 * 1000.0, 0.0)));
    assert!(group::layout(&Task::Hold {}, &GroupSpec::default(), &apart).warnings.is_empty());
    let home = group::layout(&Task::Rtl {}, &GroupSpec::default(), &ms);
    assert!(home.refused.is_none() && home.warnings.is_empty());
}

// ---- Saturation (docs/SWARMS.md section 3.3) ---------------------------------------------------

struct Swarm {
    platform: String,
    count: usize,
    cruise_alt_m: Option<f64>,
    at: LonLat,
}

/// Each opening order: its id, its swarm, its task.
type Orders = Vec<(String, String, Task)>;

fn saturation() -> (BTreeMap<String, Swarm>, Orders) {
    let fleet: toml::Table = std::fs::read_to_string(root().join("sim/fleets/swarm-saturation.toml")).unwrap().parse().unwrap();
    let ll = |t: &toml::Value| LonLat::new(t["lon"].as_float().unwrap(), t["lat"].as_float().unwrap());
    let mut swarms = BTreeMap::new();
    for s in fleet["swarm"].as_array().unwrap() {
        let at = s.get("center").or_else(|| s.get("from")).map(ll).unwrap();
        swarms.insert(
            s["id"].as_str().unwrap().to_string(),
            Swarm {
                platform: s["platform"].as_str().unwrap().into(),
                count: s["count"].as_integer().unwrap() as usize,
                cruise_alt_m: s.get("cruise_alt_m").and_then(|v| v.as_integer().map(|i| i as f64).or(v.as_float())),
                at,
            },
        );
    }
    let sit: toml::Table = std::fs::read_to_string(root().join("sim/situations/swarm-saturation-packed.toml")).unwrap().parse().unwrap();
    let pt = |v: &toml::Value| {
        let a = v.as_array().unwrap();
        LonLat::new(a[0].as_float().unwrap(), a[1].as_float().unwrap())
    };
    let mut orders = Vec::new();
    for o in sit["order"].as_array().unwrap() {
        let swarm = o["vehicles"]["swarm"].as_str().unwrap().to_string();
        let task = match o["task"].as_str().unwrap() {
            "orbit" => ring(pt(&o["center"]), o["radius_m"].as_integer().map(|i| i as f64).unwrap_or_else(|| o["radius_m"].as_float().unwrap())),
            "patrol" => Task::Patrol {
                route: o["route"].as_array().unwrap().iter().map(pt).collect(),
            },
            other => panic!("{other}"),
        };
        orders.push((o["id"].as_str().unwrap().to_string(), swarm, task));
    }
    (swarms, orders)
}

/// The members of one of Saturation's swarms, where it spawns.
fn swarm_members(c: &Catalog, s: &Swarm, name: &str) -> Vec<Member> {
    let mut ms = members(c, &s.platform, s.count, 0, |i| Some(offset(s.at, (i % 10) as f64 * 8.0, (i / 10) as f64 * 8.0)));
    for (i, m) in ms.iter_mut().enumerate() {
        m.uid = Uid::new(format!("{name}-{i:03}"));
    }
    ms
}

#[test]
fn saturations_orders_are_laid_out_as_section_3_3_says() {
    let c = catalog();
    let (swarms, orders) = saturation();
    assert_eq!(orders.len(), 36, "Saturation's 36 opening orders");
    let mut laid = BTreeMap::new();
    for (id, swarm, task) in &orders {
        let s = &swarms[swarm];
        let ms = swarm_members(&c, s, swarm);
        let lay = group::layout(task, &spec(s.cruise_alt_m), &ms);
        assert!(lay.refused.is_none(), "{id}: {:?}", lay.refused);
        assert_eq!(lay.shares.len(), 100, "{id}");
        inside_bands(&lay, &ms, s.cruise_alt_m).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(group::separation(&lay, &ms), None, "{id}");
        laid.insert(swarm.clone(), (lay, ms, task.clone()));
    }
    let alts = |lay: &Layout| -> BTreeSet<i64> { lay.shares.values().filter_map(|s| s.alt_m).map(|a| a.round() as i64).collect() };
    // PICKET-1, a 400 m ring of X10s: 25 m apart, one layer at 100 m
    let (p1, ..) = &laid["PICKET-1"];
    assert_eq!(p1.domains[0].layers, 1);
    assert_eq!(p1.domains[0].spacing_m.map(f64::round), Some(25.0));
    assert_eq!(alts(p1), BTreeSet::from([100]));
    // PICKET-6, 300 m: 19 m apart, one layer
    let (p6, ..) = &laid["PICKET-6"];
    assert_eq!((p6.domains[0].layers, p6.domains[0].spacing_m.map(f64::round)), (1, Some(19.0)));
    // SURFACE-1, a 700 m ring of Saildrones: 44 m apart is under their 50 m, so two rings at 650
    // and 750 m, 50 members on each, 82 and 94 m apart
    let (s1, ..) = &laid["SURFACE-1"];
    assert_eq!(s1.domains[0].rings_m, vec![650.0, 750.0]);
    for (r, apart) in [(650.0, 82.0), (750.0, 94.0)] {
        let on: Vec<_> = s1.shares.values().filter(|s| s.ring_m == Some(r)).collect();
        assert_eq!(on.len(), 50, "{r} m");
        assert_eq!((std::f64::consts::TAU * r / 50.0).round(), apart);
    }
    // UNDERSEA-1, a 300 m ring of REMUS, 19 m apart: three neighbours in a row in three layers,
    // 40, 45 and 50 m deep, 57 m apart in each, as section 3.3 has it; 100 is not a multiple of
    // three, so where the ring closes one block of four takes a fourth layer for one member, 55 m
    // deep (else the last and the first would be 19 m apart at one depth)
    let (u1, ..) = &laid["UNDERSEA-1"];
    assert_eq!(u1.domains[0].layers, 4);
    assert_eq!(alts(u1), BTreeSet::from([-55, -50, -45, -40]));
    assert_eq!(u1.shares.values().filter(|s| s.layer == 3).count(), 1);
    assert_eq!((std::f64::consts::TAU * 300.0 / 33.0).round(), 57.0, "33 in each of the three, 57 m apart");
    // SEARCH-1, a 6.7 km lane of Pumas: a 14.3 km racetrack, its legs 300 m apart (two of the
    // Puma's 150 m loiter radii), 143 m apart along it, one layer at 800 m
    let (sr, ..) = &laid["SEARCH-1"];
    let d = &sr.domains[0];
    assert_eq!(d.racetrack_m, Some(300.0));
    assert_eq!((d.path_m.unwrap() / 100.0).round() / 10.0, 14.3);
    assert_eq!(d.spacing_m.map(f64::round), Some(143.0));
    assert_eq!((d.layers, alts(sr)), (1, BTreeSet::from([800])));
    // a member's task is the racetrack, entered at its own place
    let (lay, ms, task) = &laid["SEARCH-1"];
    let first = &ms[0].uid;
    let Some(Task::Patrol { route }) = group::member_task(task, &spec(Some(800.0)), lay, first) else { panic!("a patrol") };
    assert_eq!(route.len(), 2 + 2 * 8, "two legs and two ends of seven points each, plus their ends");
    assert!(lay.shares[first].entry_m.is_some());
}

/// RELAY-1 as stations (the transit kind's spread, a later stage's order): 4.8 km of the shore,
/// 100 Pumas 48 m apart, whose 300 m loiter circles need 7 layers 30 m apart, 180 m of the 300 m
/// band: allowed.
#[test]
fn the_relay_as_stations_takes_seven_layers_inside_the_band() {
    let c = catalog();
    let (swarms, orders) = saturation();
    let (_, swarm, task) = orders.iter().find(|(id, ..)| id == "relay-1").unwrap();
    let s = &swarms[swarm];
    let ms = swarm_members(&c, s, swarm);
    let g = GroupSpec {
        spread: Some(Spread::Stations),
        alt_m: s.cruise_alt_m,
        ..GroupSpec::default()
    };
    let lay = group::layout(task, &g, &ms);
    assert!(lay.refused.is_none(), "{:?}", lay.refused);
    let d = &lay.domains[0];
    assert_eq!(d.layers, 7);
    assert_eq!(d.spacing_m.map(f64::round), Some(48.0));
    let alts: BTreeSet<i64> = lay.shares.values().filter_map(|s| s.alt_m).map(|a| a.round() as i64).collect();
    assert_eq!(alts, BTreeSet::from([710, 740, 770, 800, 830, 860, 890]), "180 m of the band, centred on 800 m");
}

// ---- re-forming (docs/SWARMS.md section 4.3) ----------------------------------------------------

/// 40 quadcopters on a 400 m ring (63 m apart); four drop, scattered. The re-form spreads the
/// neighbours of each gap into it until no gap is 1.5 times the new spacing, moves no one far from
/// a gap, moves fewer than half, and a neighbour whose link is not healthy keeps its slot.
#[test]
fn a_reform_moves_only_the_neighbours_of_its_gaps() {
    let c = catalog();
    let ms = members(&c, "x10", 40, 0, |i| Some(offset(GATE, i as f64 * 3.0, -600.0)));
    let task = ring(GATE, 400.0);
    let g = spec(Some(60.0));
    let first = group::layout(&task, &g, &ms);
    assert!(first.refused.is_none());
    // the members in ring order, by slot
    let mut by_slot: Vec<(u32, Uid)> = first.shares.iter().map(|(u, s)| (s.slot, u.clone())).collect();
    by_slot.sort();
    let dropped: BTreeSet<Uid> = [3usize, 13, 24, 31].iter().map(|i| by_slot[*i].1.clone()).collect();
    let left: Vec<Member> = ms.iter().filter(|m| !dropped.contains(&m.uid)).cloned().collect();
    let r = group::reform(&task, &g, &first, &left, &BTreeSet::new());
    assert!(r.layout.refused.is_none(), "{:?}", r.layout.refused);
    assert!(r.largest_gap_m < group::GAP_FACTOR * r.even_spacing_m, "{} m against {} m", r.largest_gap_m, r.even_spacing_m);
    assert!(!r.moved.is_empty() && r.moved.len() < left.len() / 2, "moved {} of {}", r.moved.len(), left.len());
    // each one moved was near a gap: within three places of a dropped member in the first layout
    let place: BTreeMap<&Uid, usize> = by_slot.iter().enumerate().map(|(i, (_, u))| (u, i)).collect();
    let gaps: Vec<usize> = [3usize, 13, 24, 31].to_vec();
    for u in &r.moved {
        let i = place[u];
        let near = gaps.iter().any(|g| {
            let d = (i as i64 - *g as i64).rem_euclid(40);
            d.min(40 - d) <= 3
        });
        assert!(near, "{u} (place {i}) moved, far from every gap");
    }
    // pin a neighbour of the first gap: it keeps its share
    let neighbour = by_slot[4].1.clone();
    let pinned = BTreeSet::from([neighbour.clone()]);
    let r2 = group::reform(&task, &g, &first, &left, &pinned);
    assert!(r2.layout.refused.is_none(), "{:?}", r2.layout.refused);
    assert!(!r2.moved.contains(&neighbour));
    assert_eq!(r2.layout.shares[&neighbour].phase_deg, first.shares[&neighbour].phase_deg);
    // and the same facts give the same re-form
    assert_eq!(r2, group::reform(&task, &g, &first, &left, &pinned));
    // a member back joins in a gap
    let mut with_back = left.clone();
    let back = ms.iter().find(|m| m.uid == by_slot[13].1).unwrap().clone();
    with_back.push(back.clone());
    let mut prev = first.clone();
    prev.shares.remove(&back.uid);
    let r3 = group::reform(&task, &g, &prev, &with_back, &BTreeSet::new());
    assert!(r3.layout.refused.is_none(), "{:?}", r3.layout.refused);
    assert!(r3.moved.contains(&back.uid), "a member back takes a new share");
}
