//! The physical sensing step on several threads (bd murmur-9730): the same detections, in the
//! same order, as the step on one, whatever the thread count and however the observers are
//! ordered; and the step's time near Saturation's size (an ignored bench).

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use super::super::physical::conditions::Conditions;
use super::super::physical::geometry::{Grid, Header};
use super::super::physical::{Engine, Step};
use super::super::sensing::{Detection, Observer, SearchArea};
use super::super::truth::{Ais, ContactTruth, Domain, Sensing, TruthAffiliation};
use super::catalog;

/// A deterministic generator for the synthetic worlds (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }
}

/// (lat, lon) moved `east` and `north` metres.
fn at(lat: f64, lon: f64, east: f64, north: f64) -> (f64, f64) {
    (lat + north / 111_320.0, lon + east / (111_320.0 * lat.to_radians().cos()))
}

const CENTER: (f64, f64) = (37.81, -122.47);

/// Our vehicles: `n` of `mix`'s platforms (platform, weight, altitude m) in proportion to their
/// weights, placed at random over a square `half_m` either side of the centre, each with a
/// heading, every seventh with a navigation error, and all but every fifth with a task area (an
/// orbit about where it is, or a patrol across it) its cameras search.
fn fleet(rng: &mut Rng, n: usize, mix: &[(&str, usize, f64)], half_m: f64) -> Vec<Observer> {
    let cat = catalog();
    let total: usize = mix.iter().map(|m| m.1).sum();
    (0..n)
        .map(|i| {
            // the mix in proportion, in a stable order
            let mut k = (i * total / n.max(1)) % total;
            let &(platform, _, alt) = mix.iter().find(|m| {
                if k < m.1 {
                    true
                } else {
                    k -= m.1;
                    false
                }
            })
            .expect("a platform");
            let p = cat.get(platform).expect(platform);
            let (lat, lon) = at(CENTER.0, CENTER.1, rng.range(-half_m, half_m), rng.range(-half_m, half_m));
            let area = match i % 5 {
                0 => None,
                1 | 2 => Some(SearchArea::Orbit { center: at(lat, lon, rng.range(-500.0, 500.0), rng.range(-500.0, 500.0)), radius_m: rng.range(300.0, 1_500.0) }),
                _ => Some(SearchArea::Route { points: vec![at(lat, lon, -2_000.0, rng.range(-500.0, 500.0)), (lat, lon), at(lat, lon, 2_000.0, rng.range(-500.0, 500.0))] }),
            };
            Observer {
                uid: 1_000 + i as u64,
                lat,
                lon,
                nav_error_m: if i % 7 == 3 { (rng.range(-40.0, 40.0), rng.range(-40.0, 40.0)) } else { (0.0, 0.0) },
                alt_msl: alt,
                mast_m: p.mast_m,
                submerged: alt < 0.0,
                sensors: p.sensors.clone(),
                heading_deg: Some(rng.range(0.0, 360.0)),
                area,
            }
        })
        .collect()
}

/// A contact that moves round a circle: its place, course and speed at second `s`.
#[derive(Clone)]
struct Mover {
    c: ContactTruth,
    center: (f64, f64),
    radius_m: f64,
    phase: f64,
}

impl Mover {
    fn at(&self, s: f64) -> ContactTruth {
        let w = self.c.speed_mps / self.radius_m;
        let a = self.phase + w * s;
        let (lat, lon) = at(self.center.0, self.center.1, self.radius_m * a.cos(), self.radius_m * a.sin());
        // moving anticlockwise: the course is the tangent's
        let course = (90.0 - (a + std::f64::consts::FRAC_PI_2).to_degrees()).rem_euclid(360.0);
        ContactTruth { lat, lon, course_deg: course, ..self.c.clone() }
    }
}

fn contact(id: String, kind: &str, domain: Domain, alt_m: f64, speed_mps: f64, emits: bool, ais: bool) -> ContactTruth {
    ContactTruth {
        id: id.clone(),
        name: id.clone(),
        affiliation: TruthAffiliation::Unknown,
        domain,
        kind: kind.into(),
        lat: 0.0,
        lon: 0.0,
        alt_m,
        speed_mps,
        course_deg: 0.0,
        emits,
        signature: 1.0,
        ais: ais.then(|| Ais { mmsi: format!("3669{:05}", id.len() * 7919 % 99_999), name: id.to_uppercase(), ship_type: "cargo".into(), spoofed: false }),
    }
}

/// Swarms of small drones (`swarms` of `per_swarm`, each round its own centre) and `traffic`
/// others of every domain, over the same square.
fn contacts(rng: &mut Rng, swarms: usize, per_swarm: usize, traffic: usize, half_m: f64) -> Vec<Mover> {
    let mut out = Vec::new();
    for k in 0..swarms {
        let center = at(CENTER.0, CENTER.1, rng.range(-half_m, half_m), rng.range(-half_m, half_m));
        for j in 0..per_swarm {
            let c = contact(format!("d{k}-{j}"), "small_drone", Domain::Air, rng.range(60.0, 200.0), rng.range(8.0, 18.0), false, false);
            out.push(Mover { c, center, radius_m: rng.range(200.0, 1_200.0), phase: rng.range(0.0, std::f64::consts::TAU) });
        }
    }
    let kinds: [(&str, Domain, f64, f64, bool, bool); 8] = [
        ("ferry", Domain::Surface, 0.0, 10.0, true, true),
        ("cargo_ship", Domain::Surface, 0.0, 7.0, true, true),
        ("small_fast_boat", Domain::Surface, 0.0, 14.0, false, false),
        ("sailboat", Domain::Surface, 0.0, 3.0, false, false),
        ("light_aircraft", Domain::Air, 600.0, 50.0, true, false),
        ("submarine", Domain::Subsurface, -40.0, 3.0, false, false),
        ("periscope", Domain::Subsurface, -15.0, 2.0, false, false),
        ("ground_vehicle", Domain::Ground, 0.0, 6.0, false, false),
    ];
    for i in 0..traffic {
        let (kind, domain, alt, speed, emits, ais) = kinds[i % kinds.len()];
        let center = at(CENTER.0, CENTER.1, rng.range(-half_m, half_m), rng.range(-half_m, half_m));
        let c = contact(format!("t{i}"), kind, domain, alt, speed, emits, ais);
        out.push(Mover { c, center, radius_m: rng.range(500.0, 3_000.0), phase: rng.range(0.0, std::f64::consts::TAU) });
    }
    out
}

/// A terrain grid over the square: a ridge across its north and a hill, so some lines of sight
/// are blocked.
fn terrain(half_m: f64) -> Grid {
    let (lat0, lon0) = at(CENTER.0, CENTER.1, -half_m - 1_000.0, -half_m - 1_000.0);
    let (lat1, lon1) = at(CENTER.0, CENTER.1, half_m + 1_000.0, half_m + 1_000.0);
    let (nlat, nlon) = (200usize, 200usize);
    let h = Header { lat0, lon0, dlat: (lat1 - lat0) / (nlat - 1) as f64, dlon: (lon1 - lon0) / (nlon - 1) as f64, nlat, nlon, scale: 0.1, nodata: -32768 };
    let mut heights = vec![0.0; nlat * nlon];
    for i in 0..nlat {
        for j in 0..nlon {
            let ridge = if (150..156).contains(&i) && j > 40 { 120.0 } else { 0.0 };
            let (di, dj) = (i as f64 - 70.0, j as f64 - 120.0);
            let hill = 180.0 * (-(di * di + dj * dj) / 120.0).exp();
            heights[i * nlon + j] = ridge + hill;
        }
    }
    Grid::from_heights(h, &heights)
}

fn sensing() -> Sensing {
    Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(35.0), ..Conditions::default() }), ..Sensing::default() }
}

/// An engine for the step on `threads` threads.
fn engine(threads: usize) -> Engine {
    Engine::new(None).with_threads(threads)
}

/// A bathymetry grid over the square: a channel deepening to 110 m across its middle.
fn bathymetry(half_m: f64) -> Grid {
    let (lat0, lon0) = at(CENTER.0, CENTER.1, -half_m - 1_000.0, -half_m - 1_000.0);
    let (lat1, lon1) = at(CENTER.0, CENTER.1, half_m + 1_000.0, half_m + 1_000.0);
    let (nlat, nlon) = (100usize, 100usize);
    let h = Header { lat0, lon0, dlat: (lat1 - lat0) / (nlat - 1) as f64, dlon: (lon1 - lon0) / (nlon - 1) as f64, nlat, nlon, scale: 0.1, nodata: -32768 };
    let heights: Vec<f64> = (0..nlat * nlon).map(|k| -10.0 - 100.0 * (-((k / nlon) as f64 - 50.0).powi(2) / 300.0).exp()).collect();
    Grid::from_heights(h, &heights)
}

/// A world with every kind of sensor (radar with and without height, AIS, ESM, active and
/// passive sonar and one that does both, gimballed, fixed and panoramic cameras, a ground
/// vehicle's), contacts of every domain, close enough that cameras cue, lock and identify and
/// sonars classify within a couple of minutes; one uid named twice.
fn mixed(rng: &mut Rng) -> (Vec<Observer>, Vec<Mover>) {
    let half_m = 3_000.0;
    let mix = [
        ("seahunter", 3, 0.0),
        ("corsair", 3, 0.0),
        ("mq9b_armed", 2, 3_000.0),
        ("rq4", 1, 15_000.0),
        ("puma", 4, 300.0),
        ("x10", 4, 80.0),
        ("saildrone", 2, 0.0),
        ("magura", 2, 0.0),
        ("fpv", 1, 50.0),
        ("lta", 1, 400.0),
        ("ugv_recon", 1, 20.0),
        ("buoy", 2, 0.0),
        ("remus", 2, -30.0),
        ("orca", 1, -60.0),
    ];
    let mut obs = fleet(rng, 30, &mix, half_m);
    // the same vehicle named twice (a fleet that lists it twice): one part of the step, its two
    // observers in their order
    let mut twin = obs[4].clone();
    twin.lat += 0.002;
    obs.insert(17, twin);
    let movers = contacts(rng, 2, 6, 24, half_m);
    (obs, movers)
}

/// `ticks` of `e` on `obs` and `movers`, with the scenario's grids and two vehicles sent to look
/// at contacts (rules 21): every tick's detections, and the engine's state at the end.
fn run(mut e: Engine, obs: &[Observer], movers: &[Mover], ticks: i64) -> (Vec<Vec<Detection>>, String) {
    e.set_terrain(terrain(3_000.0));
    e.set_bathymetry(bathymetry(3_000.0));
    let firsts: HashMap<u64, BTreeSet<String>> = [(obs[1].uid, ["t3".to_string()].into()), (obs[9].uid, ["d1-2".to_string(), "t8".to_string()].into())].into();
    e.set_firsts(firsts);
    let sensing = sensing();
    let mut all = Vec::new();
    for s in 0..ticks {
        let truth: Vec<ContactTruth> = movers.iter().map(|m| m.at(s as f64)).collect();
        let st = Step { seed: 5, tick: s, s: s as f64, t: 1_781_467_800.0 + s as f64, misclassification_rate: 0.1, sensing: &sensing };
        all.push(e.step(&st, obs, &truth));
    }
    (all, e.state_text())
}

/// The step on several threads gives what it gives on one: every tick's detections, in the same
/// order, and the same state after, whatever the observers' order (each order its own world: a
/// cue is the last fix of a contact in observer order) and the thread count (bd murmur-9730).
#[test]
fn the_step_on_several_threads_gives_what_it_gives_on_one() {
    let mut rng = Rng(97);
    let (obs, movers) = mixed(&mut rng);
    let mut shuffled = obs.clone();
    for i in (1..shuffled.len()).rev() {
        shuffled.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    let reversed: Vec<Observer> = obs.iter().rev().cloned().collect();
    let ticks = 100;
    for (name, order) in [("as built", &obs), ("reversed", &reversed), ("shuffled", &shuffled)] {
        let (want, state) = run(engine(1), order, &movers, ticks);
        let n: usize = want.iter().map(Vec::len).sum();
        let sensors: BTreeSet<_> = want.iter().flatten().map(|d| (d.sensor, d.look.map(|l| l.level as u8))).collect();
        // the world exercises what the step does: every sensor type, a camera's identification
        assert!(n > 10_000 && sensors.len() >= 8, "{name}: {n} detections, {sensors:?}");
        for threads in [2, 3, 8] {
            let (got, got_state) = run(engine(threads), order, &movers, ticks);
            for (s, (a, b)) in want.iter().zip(&got).enumerate() {
                assert_eq!(a, b, "{name}, {threads} threads: tick {s}");
            }
            assert_eq!(state, got_state, "{name}, {threads} threads: the state after");
        }
    }
}

/// The step's time per tick, ms, from `warm` ticks on, and a digest of every tick's detections.
fn time_ticks(e: &mut Engine, obs: &[Observer], movers: &[Mover], warm: i64, ticks: i64) -> (Vec<f64>, Vec<u64>, usize) {
    use std::hash::{Hash, Hasher};
    let sensing = sensing();
    let (mut ms, mut digests, mut n) = (Vec::new(), Vec::new(), 0);
    for s in 0..warm + ticks {
        let truth: Vec<ContactTruth> = movers.iter().map(|m| m.at(s as f64)).collect();
        let st = Step { seed: 11, tick: s, s: s as f64, t: 1_781_467_800.0 + s as f64, misclassification_rate: 0.05, sensing: &sensing };
        let t0 = Instant::now();
        let dets = e.step(&st, obs, &truth);
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{dets:?}").hash(&mut h);
        digests.push(h.finish());
        if s >= warm {
            ms.push(dt);
            n += dets.len();
        }
    }
    (ms, digests, n)
}

fn env_list(name: &str, default: &str) -> Vec<usize> {
    std::env::var(name).unwrap_or_else(|_| default.into()).split(',').filter_map(|v| v.trim().parse().ok()).collect()
}

fn env_num(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// Saturation's size (3,602 of ours in its mix, 500 small drones in five swarms and 7 others,
/// no terrain, as its scenario) over a 24 km square: the sensing step's time per tick at each
/// thread count, every count's detections the same as one thread's. Release build:
///
///     MURMUR_BENCH_THREADS=1,2,4,8,16 cargo test --release -p murmur-bridge sense_par_bench -- --ignored --nocapture
///
/// (`MURMUR_BENCH_OBSERVERS`, `MURMUR_BENCH_HALF_M`, `MURMUR_BENCH_WARM` and `MURMUR_BENCH_TICKS`
/// change the world and the run.)
#[test]
#[ignore]
fn sense_par_bench() {
    let n = env_num("MURMUR_BENCH_OBSERVERS", 3_602);
    let (warm, ticks) = (env_num("MURMUR_BENCH_WARM", 20) as i64, env_num("MURMUR_BENCH_TICKS", 40) as i64);
    let half_m = env_num("MURMUR_BENCH_HALF_M", 12_000) as f64;
    let mix = [("x10", 1_700, 100.0), ("puma", 1_300, 800.0), ("saildrone", 200, 0.0), ("corsair", 300, 0.0), ("remus", 100, -30.0), ("mq9b_armed", 2, 7_600.0)];
    let mut rng = Rng(9_730);
    let obs = fleet(&mut rng, n, &mix, half_m);
    let movers = contacts(&mut rng, 5, 100, 7, half_m);
    let mut first: Option<Vec<u64>> = None;
    for threads in env_list("MURMUR_BENCH_THREADS", "1,2,4,8") {
        let mut e = engine(threads);
        let (ms, digests, dets) = time_ticks(&mut e, &obs, &movers, warm, ticks);
        let mut sorted = ms.clone();
        sorted.sort_by(f64::total_cmp);
        let mean = ms.iter().sum::<f64>() / ms.len() as f64;
        println!(
            "bench: {n} observers, {} contacts, {threads} threads: mean {mean:.1} ms, median {:.1}, worst {:.1} ({} detections a tick)",
            movers.len(),
            sorted[sorted.len() / 2],
            sorted[sorted.len() - 1],
            dets / ms.len()
        );
        match &first {
            None => first = Some(digests),
            Some(f) => assert_eq!(f, &digests, "{threads} threads gave other detections than the first count"),
        }
    }
}

