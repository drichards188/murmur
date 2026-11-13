//! Tests of the served predictor (`crate::coverage`) on the Golden Gate's baked surface grid
//! (sim/headless/terrain), against the link emulator's own link.

use std::path::PathBuf;
use std::sync::Arc;

use murmur_linkemu::coverage::Reason;

use super::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn gg_grid() -> Arc<Grid> {
    Arc::new(Grid::load(&root().join("sim/headless/terrain/golden-gate.grid.gz")).expect("grid"))
}

fn world() -> Arc<World> {
    let root = root();
    World::new(crate::world::Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: root.join("sim/run/no-such-manifest.json"),
        platforms: root.join("sim/platforms.toml"),
        root,
    })
}

pub(crate) fn coverage(grid: Option<Arc<Grid>>) -> Coverage {
    coverage_of(root().join("sim/links.toml"), grid)
}

fn coverage_of(links: PathBuf, grid: Option<Arc<Grid>>) -> Coverage {
    Coverage::new(Sources {
        links,
        root: root(),
        terrain: grid.map(|g| {
            let s: Arc<dyn Sampler> = Arc::new(GridSampler(g));
            (s, "the Golden Gate's baked surface grid".to_string())
        }),
        space_root: None,
        space: false,
    })
}

pub(crate) fn golden_gate_coverage() -> Coverage {
    coverage(Some(gg_grid()))
}

/// The emulator's own link on one profile, with its own terrain cache (as the headless simulator
/// builds it from the links file), to compare with.
fn emulator_link(profile: &str) -> (murmur_linkemu::link::Link, Arc<TidewaterTerrain>) {
    emulator_link_of(&root().join("sim/links.toml"), profile, None)
}

/// The same from another links file, its stations' altitudes settled against `grid` as the
/// headless emulator settles them.
fn emulator_link_of(
    links: &std::path::Path,
    profile: &str,
    grid: Option<&Grid>,
) -> (murmur_linkemu::link::Link, Arc<TidewaterTerrain>) {
    let mut cfg = LinksConfig::load(links).expect("links");
    let dems: Vec<Option<f64>> = cfg
        .ground_stations
        .iter()
        .map(|g| grid.and_then(|grid| grid.height(g.lat, g.lon)))
        .collect();
    cfg.resolve_ground_alts(&dems);
    let terrain = Arc::new(TidewaterTerrain::new(cfg.terrain.clone()));
    let env = Arc::new(murmur_linkemu::link::Env::new(&cfg, terrain.clone(), 1).expect("env"));
    let raw = cfg.resolve(profile).expect("profile");
    let params = murmur_linkemu::model::Params::from_profile(&raw).expect("params");
    let meta = murmur_linkemu::link::LinkMeta {
        uid: 1,
        sysid: 1,
        port: 0,
        class: profile.into(),
        profile: profile.into(),
        domain: "air".into(),
        platform: "x".into(),
    };
    (murmur_linkemu::link::Link::new(meta, params, env), terrain)
}

/// What the emulator's link does to a packet at `p`: (blocked, range loss, terrain loss).
async fn emulator_at(
    link: &mut murmur_linkemu::link::Link,
    terrain: &TidewaterTerrain,
    sampler: &GridSampler,
    p: &GeoPoint,
) -> (bool, f64, f64) {
    // the position as the link's frame parser sets it (a frame would round it to 1e-7 degrees)
    link.set_position(*p);
    while terrain.sizes().0 > 0 {
        terrain.tick(sampler, 0.0).await;
    }
    link.set_position(*p);
    let c = link.cond(murmur_linkemu::model::Dir::Up, 0);
    (c.blocked.is_some(), c.range_loss, c.terrain_loss)
}

fn gg(lon: f64, lat: f64, alt_m: f64) -> GeoPoint {
    GeoPoint { lat, lon, alt_m }
}

/// The served prediction agrees with the emulator's link on the Golden Gate's grid at sampled
/// points (a lattice over the strait, the Headlands and the city, three altitudes), for both
/// terrain-limited classes; and the known shadows are where they are: behind the Marin Headlands
/// and the city's hills at 85 m (a Puma's), the bay open, the Headlands clear from high up.
#[tokio::test]
async fn agrees_with_the_emulator_on_the_golden_gate_grid() {
    let grid = gg_grid();
    let sampler = GridSampler(grid.clone());
    let c = coverage(Some(grid.clone()));
    let ready = c.ready().await.expect("ready");
    let mut checked = 0;
    for profile in ["los", "mesh"] {
        let (mut link, theirs) = emulator_link(profile);
        let mut pts = Vec::new();
        for i in 0..7 {
            for j in 0..7 {
                for alt in [5.0, 85.0, 300.0] {
                    pts.push((gg(-122.56 + 0.03 * f64::from(j), 37.76 + 0.016 * f64::from(i), alt), 0.0));
                }
            }
        }
        let (preds, _) = c.predict_all(&ready, profile, &pts).await.expect("predict");
        for ((p, _), got) in pts.iter().zip(&preds) {
            let (blocked, range, terrain) = emulator_at(&mut link, &theirs, &sampler, p).await;
            assert!(!got.terrain_unknown, "{p:?}");
            assert_eq!(got.blocked, blocked, "{profile} {p:?}");
            assert!((got.range_loss - range).abs() < 1e-12, "{profile} {p:?}");
            assert!((got.terrain_loss - terrain).abs() < 1e-12, "{profile} {p:?} {got:?} {terrain}");
            checked += 1;
        }
        let downs = preds.iter().filter(|p| p.state == State::Down).count();
        let ups = preds.iter().filter(|p| p.state == State::Up).count();
        assert!(downs > 20 && ups > 20, "{profile}: a lattice of both ({downs} down, {ups} up)");
    }
    assert_eq!(checked, 2 * 147);
    let at = |lon, lat, alt| (gg(lon, lat, alt), 0.0);
    let pts = [
        at(-122.52, 37.845, 85.0),  // behind the Marin Headlands
        at(-122.45, 37.78, 85.0),   // behind the city's hills
        at(-122.42, 37.825, 85.0),  // the bay, east of the bridge
        at(-122.52, 37.845, 600.0), // the Headlands from high up
    ];
    let (p, _) = c.predict_all(&ready, "los", &pts).await.expect("predict");
    assert_eq!((p[0].state, p[0].reason), (State::Down, Some(Reason::Terrain)), "{:?}", p[0]);
    assert!(p[0].obstruction_m.is_some_and(|o| o > 20.0), "{:?}", p[0]);
    assert_eq!((p[1].state, p[1].reason), (State::Down, Some(Reason::Terrain)), "{:?}", p[1]);
    assert_eq!(p[2].state, State::Up, "{:?}", p[2]);
    assert_eq!(p[3].state, State::Up, "{:?}", p[3]);
}

/// A line west across the strait into the Headlands' shadow at a Puma's 85 m: where the link
/// first goes down, how long it stays down at 20 m/s against the LOS failsafe's 5 s, and the
/// sentence. The point answer and a grid over the same ground say the same.
#[tokio::test]
async fn a_route_into_the_headlands_shadow_says_where_and_for_how_long() {
    let w = world();
    let c = golden_gate_coverage();
    let q = json!({"class": "los", "alt": 85.0, "speed_mps": 20.0,
                   "route": [[-122.42, 37.825], [-122.52, 37.845]]});
    let a = c.line(&w, &q).await.expect("route");
    let leg = &a["legs"][0];
    let d = &leg["first_down"];
    let along = d["along_m"].as_f64().expect("down somewhere");
    assert!(along > 3000.0 && along < 8500.0, "{a}");
    assert_eq!(d["reason"], "terrain");
    assert_eq!(d["trips"], true, "{a}");
    assert_eq!(a["failsafe_s"], 5.0);
    assert_eq!(a["holds"], false);
    let s = a["sentence"].as_str().expect("sentence");
    assert!(
        s.starts_with("A los link would lose its link ") && s.contains("its failsafe trips after 5 s"),
        "{s}"
    );
    // the first point the line finds down is down in a point answer too
    let at = d["at"].clone();
    let one = c
        .points(&w, &json!({"class": "los", "alt": 85.0, "point": at}))
        .await
        .expect("point");
    assert_eq!(one["points"][0]["state"], "down", "{one}");
    // and a grid: one letter a cell, the counts adding up, the same answer the second time
    let gq = json!({"class": "los", "alt": 85.0, "bbox": [-122.53, 37.82, -122.41, 37.85], "res": 500});
    let g = c.grid(&w, &gq).await.expect("grid");
    let n = (g["nlat"].as_u64().expect("n") * g["nlon"].as_u64().expect("n")) as usize;
    assert_eq!(g["state"].as_str().expect("state").len(), n);
    assert_eq!(g["p_loss"].as_array().expect("loss").len(), n);
    let counts = &g["counts"];
    let total: u64 = ["up", "degraded", "down", "unknown"]
        .iter()
        .map(|k| counts[*k].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(total as usize, n);
    assert!(
        counts["down"].as_u64().unwrap_or(0) > 0 && counts["up"].as_u64().unwrap_or(0) > 0,
        "{counts}"
    );
    assert_eq!(c.grid(&w, &gq).await.expect("again"), g);
    // a box too large for the resolution is answered coarser, and says so
    let big = c
        .grid(
            &w,
            &json!({"class": "los", "alt": 85.0, "bbox": [-123.0, 37.4, -122.0, 38.2], "res": 50, "max_cells": 900}),
        )
        .await
        .expect("big");
    assert_eq!(big["coarsened"], true);
    assert!(big["nlat"].as_u64().expect("n") * big["nlon"].as_u64().expect("n") <= 900);
}

#[tokio::test]
async fn refuses_what_it_cannot_answer() {
    let w = world();
    let c = coverage(None);
    let e = c
        .points(&w, &json!({"class": "laser", "point": [-122.4, 37.8]}))
        .await
        .expect_err("unknown class");
    assert!(e.1.contains("unknown link class") && e.1.contains("los"), "{e:?}");
    let e = c
        .grid(&w, &json!({"class": "los", "bbox": [1, 2, 0, 3]}))
        .await
        .expect_err("bad box");
    assert_eq!(e.0, 400);
    let e = c
        .points(&w, &json!({"uid": 77, "point": [-122.4, 37.8]}))
        .await
        .expect_err("no vehicle");
    assert_eq!(e.0, 404);
    // no terrain source: the terrain is not checked, and the answer says so
    let p = c
        .points(&w, &json!({"class": "los", "point": [-122.52, 37.845], "alt": 85.0}))
        .await
        .expect("p");
    assert_eq!(p["points"][0]["state"], "up", "{p}");
    assert!(
        p["sources"]["terrain"].as_str().expect("t").starts_with("not checked"),
        "{p}"
    );
}

/// The shipped links with a second station on the Marin Headlands (Hawk Hill, its ground from
/// the DEM), written to a file of this test's own.
fn two_station_links() -> PathBuf {
    let mut t: toml::Table = std::fs::read_to_string(root().join("sim/links.toml"))
        .expect("links")
        .parse()
        .expect("toml");
    let crissy = t.remove("ground_station").expect("the shipped station");
    let mut hawk = toml::Table::new();
    hawk.insert("name".into(), "Hawk Hill".into());
    hawk.insert("lat".into(), 37.8268.into());
    hawk.insert("lon".into(), (-122.4995).into());
    hawk.insert("alt_m".into(), "dem".into());
    t.insert("ground_station".into(), toml::Value::Array(vec![crissy, toml::Value::Table(hawk)]));
    let path = std::env::temp_dir().join(format!("murmur-two-stations-{}.toml", std::process::id()));
    std::fs::write(&path, toml::to_string(&t).expect("write")).expect("write");
    path
}

/// Two stations, Crissy Field and Hawk Hill: the served prediction chooses the station the
/// emulator's link chooses at every sampled point and agrees with it on the loss; the Headlands'
/// shadow from Crissy Field is covered from Hawk Hill; a grid says which station serves each
/// cell, a point answer names it, and the sources list both.
#[tokio::test]
async fn two_stations_agree_with_the_emulator_and_name_the_station() {
    let grid = gg_grid();
    let sampler = GridSampler(grid.clone());
    let links = two_station_links();
    let c = coverage_of(links.clone(), Some(grid.clone()));
    let ready = c.ready().await.expect("ready");
    let mut used = std::collections::BTreeSet::new();
    for profile in ["los", "mesh"] {
        let (mut link, theirs) = emulator_link_of(&links, profile, Some(&grid));
        let mut pts = Vec::new();
        for i in 0..6 {
            for j in 0..6 {
                for alt in [5.0, 85.0] {
                    pts.push((gg(-122.56 + 0.035 * f64::from(j), 37.76 + 0.02 * f64::from(i), alt), 0.0));
                }
            }
        }
        let (preds, _) = c.predict_all(&ready, profile, &pts).await.expect("predict");
        for ((p, _), got) in pts.iter().zip(&preds) {
            let (blocked, range, terrain) = emulator_at(&mut link, &theirs, &sampler, p).await;
            assert!(!got.terrain_unknown, "{p:?}");
            assert_eq!(got.station_index, link.station(), "{profile} {p:?}");
            assert_eq!(got.blocked, blocked, "{profile} {p:?}");
            assert!((got.range_loss - range).abs() < 1e-12, "{profile} {p:?}");
            assert!((got.terrain_loss - terrain).abs() < 1e-12, "{profile} {p:?}");
            used.insert(got.station.clone().expect("a radio link names its station"));
        }
    }
    assert_eq!(used.into_iter().collect::<Vec<_>>(), ["Crissy Field", "Hawk Hill"]);
    let w = world();
    // in the Headlands' shadow from Crissy Field at a Puma's 85 m (down by terrain with it
    // alone), and seen from Hawk Hill: up through it
    let lattice: Vec<(GeoPoint, f64)> = (0..30)
        .flat_map(|i| (0..30).map(move |j| (gg(-122.60 + 0.006 * f64::from(j), 37.80 + 0.004 * f64::from(i), 85.0), 0.0)))
        .collect();
    let alone = golden_gate_coverage();
    let alone_ready = alone.ready().await.expect("ready");
    let (by_one, _) = alone.predict_all(&alone_ready, "los", &lattice).await.expect("one");
    let (by_two, _) = c.predict_all(&ready, "los", &lattice).await.expect("two");
    let k = (0..lattice.len())
        .find(|k| {
            by_one[*k].reason == Some(Reason::Terrain)
                && by_one[*k].state == State::Down
                && by_two[*k].state == State::Up
        })
        .expect("a point the second station covers");
    assert_eq!(by_two[k].station.as_deref(), Some("Hawk Hill"));
    let at = json!([lattice[k].0.lon, lattice[k].0.lat]);
    let one = alone
        .points(&w, &json!({"class": "los", "alt": 85.0, "point": at}))
        .await
        .expect("one");
    assert_eq!(one["points"][0]["state"], "down", "{one}");
    assert_eq!(one["points"][0]["prediction"]["station"], "Crissy Field");
    let two = c
        .points(&w, &json!({"class": "los", "alt": 85.0, "point": at}))
        .await
        .expect("two");
    assert_eq!(two["points"][0]["state"], "up", "{two}");
    assert_eq!(two["points"][0]["prediction"]["station"], "Hawk Hill");
    let src = &two["sources"];
    assert!(src["ground_station"].is_null(), "{src}");
    assert_eq!(src["ground_stations"][1]["name"], "Hawk Hill");
    assert!(src["ground_station_check"].as_str().is_some_and(|s| s.contains("Hawk Hill: ")), "{src}");
    // a grid: one station character a cell, both stations serving some
    let g = c
        .grid(&w, &json!({"class": "los", "alt": 85.0, "bbox": [-122.56, 37.76, -122.38, 37.88], "res": 1000}))
        .await
        .expect("grid");
    let st = g["station"].as_str().expect("station");
    assert_eq!(st.len(), g["state"].as_str().expect("state").len());
    assert!(st.contains('0') && st.contains('1'), "{st}");
    assert_eq!(g["stations"], json!(["Crissy Field", "Hawk Hill"]));
    let _ = std::fs::remove_file(&links);
}

// ---- SATCOM along an order's routes (bd murmur-7ujq) ------------------------------------------

/// A vehicle of `platform` heard just now at `pos` (lat, lon) and `alt_m`, its home where it is.
fn add_vehicle(world: &World, uid: Uid, platform: &str, pos: (f64, f64), alt_m: f64) {
    use crate::link::{LinkStats, Transmit};
    use crate::vehicle::{Identity, VehicleCell};
    struct NoTx;
    impl Transmit for NoTx {
        fn send(&self, _: &crate::mav::MavMessage) {}
        fn send_raw(&self, _: &[u8]) {}
        fn port(&self) -> u16 {
            0
        }
    }
    let mut stats = LinkStats::default();
    stats.on_packet(0, crate::clock::mono(), crate::clock::wall());
    let mut m = serde_json::Map::new();
    m.insert("platform".into(), json!(platform));
    m.insert("callsign".into(), json!(format!("{platform} {uid}")));
    let cell = Arc::new(VehicleCell::new(
        uid,
        (uid % 250) as u8,
        Arc::new(NoTx),
        Arc::new(std::sync::Mutex::new(stats)),
        Some(Arc::new(Identity::from_manifest(&m))),
    ));
    cell.write(|v| {
        v.pos = Some(pos);
        v.alt_msl = Some(alt_m);
        v.home = Some(pos);
        v.home_alt = Some(alt_m);
    });
    world.insert_vehicle(cell);
}

/// `n` vehicles of `platform` spread round a ring of `radius_m` about `center` (lon, lat), each
/// ordered to orbit it as the decision engine lays the order out (`route::route_for`: from where
/// it is to waypoint 1, one lap of the ring's twelve points, back home), uids from `uid0`.
fn ring_order(
    world: &World,
    platform: &str,
    uid0: Uid,
    n: usize,
    center: (f64, f64),
    radius_m: f64,
) -> Vec<VehicleRoute> {
    use murmur_decide::{LegKind, LonLat, Route, Segment};
    let c = LonLat { lon: center.0, lat: center.1 };
    let path = murmur_decide::Task::Orbit { center: c, radius_m }.path().expect("an orbit's path");
    let first = path.points[0];
    let (_, end) = path.end().expect("its end");
    let at = murmur_decide::geo::circle_points(c, radius_m, n);
    at.iter()
        .enumerate()
        .map(|(i, p)| {
            let uid = uid0 + i as Uid;
            add_vehicle(world, uid, platform, (p.lat, p.lon), 0.0);
            let route = Route {
                transit: Some(Segment {
                    kind: LegKind::Transit,
                    from: *p,
                    to: first,
                    from_label: "position".into(),
                    to_label: "waypoint 1".into(),
                    from_vehicle: true,
                }),
                task: Some(path.clone()),
                ret: Some(Segment {
                    kind: LegKind::Return,
                    from: end,
                    to: *p,
                    from_label: "waypoint 1".into(),
                    to_label: "home".into(),
                    from_vehicle: false,
                }),
                ..Route::default()
            };
            VehicleRoute { uid: Some(uid), route: Ok(route) }
        })
        .collect()
}

/// The Golden Gate's pinned element sets (sim/headless/space/golden-gate, committed; the same
/// files as a fetch of 2026-10-02 into sim/space/cache), loaded as the predictor loads the links
/// file's: its time map from `t0`, in the links file's `auto` epoch mode.
fn pinned_space(t0: f64) -> RealSpace {
    use std::io::Read;
    let dir = root().join("sim/headless/space/golden-gate");
    let catalog = murmur_space::Catalog::parse(&std::fs::read_to_string(dir.join("constellations.toml")).expect("catalog"))
        .expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| {
            let file = dir.join("cache").join(format!("{}.json.gz", murmur_space::cache::cache_key(&q)));
            let mut text = String::new();
            flate2::read::GzDecoder::new(std::fs::File::open(&file).expect("element sets"))
                .read_to_string(&mut text)
                .expect("gzip");
            (q, text)
        })
        .collect();
    let space = murmur_space::Space::from_sources(catalog, &sources).expect("space");
    let from = chrono::DateTime::from_timestamp(t0 as i64, 0).expect("t0");
    let tm = space.time_map(murmur_space::EpochMode::Auto, from, from + chrono::Duration::hours(24));
    RealSpace::new(space, tm)
}

/// 2026-10-02T18:00:00Z, the pinned element sets' day, as unix seconds.
const SAT_T0: f64 = 1_790_964_000.0;

/// A position and time as a key, bit for bit.
type Asked = (String, [u64; 4]);

/// A SATCOM source over another, keeping what it is asked and answers: its `sample` calls, each
/// `sample_many` look (class, time, how many points), and every point's answer. `per_point`
/// answers a look one `sample` a point, as the predictor asked before bd murmur-7ujq.
struct Counting {
    inner: Arc<dyn SpaceSource>,
    per_point: bool,
    samples: std::sync::atomic::AtomicUsize,
    looks: Mutex<Vec<(String, u64, usize)>>,
    answers: Mutex<std::collections::BTreeMap<Asked, Result<SatObs, String>>>,
}

impl Counting {
    fn new(inner: Arc<dyn SpaceSource>, per_point: bool) -> Arc<Counting> {
        Arc::new(Counting {
            inner,
            per_point,
            samples: Default::default(),
            looks: Default::default(),
            answers: Default::default(),
        })
    }

    fn keep(&self, class: &str, p: &GeoPoint, t: f64, a: &Result<SatObs, String>) {
        let key = (class.to_string(), [p.lat.to_bits(), p.lon.to_bits(), p.alt_m.to_bits(), t.to_bits()]);
        self.answers.lock().expect("answers").insert(key, a.clone());
    }

    fn samples(&self) -> usize {
        self.samples.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn looks(&self) -> Vec<(String, u64, usize)> {
        self.looks.lock().expect("looks").clone()
    }
}

impl SpaceSource for Counting {
    fn check(&self, class: &str, unix_s: f64) -> Result<(), String> {
        self.inner.check(class, unix_s)
    }

    fn sample(&self, class: &str, pos: &GeoPoint, unix_s: f64) -> Result<SatObs, String> {
        self.samples.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let a = self.inner.sample(class, pos, unix_s);
        self.keep(class, pos, unix_s, &a);
        a
    }

    fn sample_many(&self, class: &str, positions: &[GeoPoint], unix_s: f64) -> Vec<Result<SatObs, String>> {
        if self.per_point {
            return positions.iter().map(|p| self.sample(class, p, unix_s)).collect();
        }
        self.looks.lock().expect("looks").push((class.to_string(), unix_s.to_bits(), positions.len()));
        let out = self.inner.sample_many(class, positions, unix_s);
        for (p, a) in positions.iter().zip(&out) {
            self.keep(class, p, unix_s, a);
        }
        out
    }

    fn outlook(
        &self,
        class: &str,
        pos: &GeoPoint,
        unix_s: f64,
        horizon_s: f64,
    ) -> Result<murmur_linkemu::sat::Outlook, String> {
        self.inner.outlook(class, pos, unix_s, horizon_s)
    }
}

/// A made-up constellation: each answer a function of its class, point and time, a class
/// `broken` failing; `sample_many` is the trait's own, one `sample` a point.
struct Made;

impl SpaceSource for Made {
    fn check(&self, _: &str, _: f64) -> Result<(), String> {
        Ok(())
    }

    fn sample(&self, class: &str, pos: &GeoPoint, unix_s: f64) -> Result<SatObs, String> {
        if class == "broken" {
            return Err(format!("{class} at {unix_s}"));
        }
        Ok(SatObs {
            visible: ((pos.lat * 1e4).abs() as usize + unix_s as usize + class.len()) % 9,
            best: None,
            latency_ms: Some(pos.lon * 1e3 + unix_s + pos.alt_m),
        })
    }

    fn outlook(&self, _: &str, _: &GeoPoint, _: f64, _: f64) -> Result<murmur_linkemu::sat::Outlook, String> {
        Ok(Default::default())
    }
}

/// bd murmur-7ujq: points sharing a class and a time are looked at together, each answer the one
/// a look at that point alone gives (errors and points without satellites in their places), each
/// class and time once (in parts of `SAT_BATCH` points), never one `sample` a point.
#[test]
fn points_sharing_a_class_and_a_time_are_looked_at_together() {
    let made: Arc<dyn SpaceSource> = Arc::new(Made);
    let src = Counting::new(made.clone(), false);
    let classes = [Some("starlink"), Some("iridium"), None, Some("broken")];
    let mut jobs: Vec<(Option<String>, GeoPoint, f64)> = (0..3_000)
        .map(|i| {
            let class = classes[i % classes.len()].map(str::to_string);
            let p = GeoPoint { lat: 37.7 + 1e-4 * i as f64, lon: -122.5 - 1e-4 * i as f64, alt_m: (i % 3) as f64 };
            (class, p, SAT_T0 + SAT_STEP_S * (i % 7) as f64)
        })
        .collect();
    // a grid's worth at one time: more than one look's points
    jobs.extend((0..SAT_BATCH + 500).map(|i| {
        (Some("starlink".to_string()), GeoPoint { lat: 36.0 + 1e-5 * i as f64, lon: -121.0, alt_m: 0.0 }, SAT_T0 - 60.0)
    }));
    let got = sample_grouped(src.as_ref(), &jobs);
    let want: Vec<Option<Result<SatObs, String>>> =
        jobs.iter().map(|(c, p, t)| c.as_ref().map(|c| made.sample(c, p, *t))).collect();
    assert_eq!(got, want);
    assert_eq!(src.samples(), 0, "never one point at a time");
    let looks = src.looks();
    let mut groups: std::collections::BTreeMap<(String, u64), Vec<usize>> = Default::default();
    for (c, t, n) in &looks {
        groups.entry((c.clone(), *t)).or_default().push(*n);
    }
    // three classes at seven times, and the big group at its own time in two parts
    assert_eq!(groups.len(), 3 * 7 + 1, "{looks:?}");
    assert_eq!(looks.len(), 3 * 7 + 2, "{looks:?}");
    assert_eq!(groups[&("starlink".to_string(), (SAT_T0 - 60.0).to_bits())], [SAT_BATCH, 500]);
    let asked: usize = looks.iter().map(|l| l.2).sum();
    assert_eq!(asked, jobs.iter().filter(|j| j.0.is_some()).count());
}

/// The predictor with SATCOM from `source` (the shipped links, no terrain).
fn satcom_coverage(links: PathBuf, source: Arc<dyn SpaceSource>) -> Coverage {
    Coverage::new(Sources { links, root: root(), terrain: None, space_root: None, space: true }).with_space(source)
}

/// bd murmur-7ujq: an order of several vehicles on the real element sets (Saildrones and Corsairs
/// on Starlink, a buoy on Iridium, a Puma on its radio, and a uid with no vehicle) is answered
/// exactly as when every point is looked at alone: the same answer for the order, and every
/// point's SATCOM answer the same, bit for bit. Batched, no point is looked at alone and each
/// class and time once for the whole order: the vehicles share the looks.
#[tokio::test]
async fn an_orders_satellites_are_looked_at_once_a_class_and_time() {
    let w = world();
    let mut routes = ring_order(&w, "saildrone", 101, 3, (-122.70, 37.76), 500.0);
    routes.extend(ring_order(&w, "corsair", 201, 2, (-122.60, 37.75), 600.0));
    routes.extend(ring_order(&w, "buoy", 301, 1, (-122.65, 37.70), 200.0));
    routes.extend(ring_order(&w, "puma", 401, 1, (-122.45, 37.80), 800.0));
    routes.push(VehicleRoute { uid: Some(999), route: Err("not a vehicle".into()) });
    let real: Arc<dyn SpaceSource> = Arc::new(pinned_space(SAT_T0));
    let mut answers = Vec::new();
    let mut sources = Vec::new();
    for per_point in [true, false] {
        let src = Counting::new(real.clone(), per_point);
        let c = satcom_coverage(root().join("sim/links.toml"), src.clone());
        let again: Vec<VehicleRoute> = routes
            .iter()
            .map(|r| VehicleRoute { uid: r.uid, route: r.route.clone() })
            .collect();
        answers.push(c.order_answer_at(&w, again, SAT_T0).await);
        sources.push(src);
    }
    let (alone, batched) = (&answers[0], &answers[1]);
    assert_eq!(batched, alone);
    let vs = batched["vehicles"].as_array().expect("vehicles");
    let profiles: Vec<&str> = vs.iter().map(|v| v["profile"].as_str().unwrap_or("-")).collect();
    assert_eq!(profiles, ["satcom_leo"; 5].into_iter().chain(["satcom_iridium", "los", "-"]).collect::<Vec<_>>());
    assert!(vs[..7].iter().all(|v| v.get("error").is_none()), "{batched}");
    assert_eq!(batched["sources"]["satcom"], SATCOM_SOURCE);
    // every point's SATCOM answer the same, the same points and times asked
    let (alone, batched) = (&sources[0], &sources[1]);
    let a = alone.answers.lock().expect("answers").clone();
    let b = batched.answers.lock().expect("answers").clone();
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (k, x) in &a {
        assert_eq!(Some(x), b.get(k), "{k:?}");
    }
    // and the answers are the constellation's: satellites in view, more or fewer from point to point
    let seen: std::collections::BTreeSet<usize> =
        a.values().filter_map(|x| x.as_ref().ok()).filter(|o| o.best.is_some()).map(|o| o.visible).collect();
    assert!(seen.len() > 3, "{seen:?}");
    assert!(a.keys().any(|k| k.0 == "iridium") && a.keys().any(|k| k.0 == "starlink"));
    // alone: a look a point; batched: none alone, each class and time looked at once
    let points = alone.samples();
    assert!(alone.looks().is_empty());
    assert_eq!(batched.samples(), 0);
    let looks = batched.looks();
    let once: std::collections::BTreeSet<(String, u64)> = looks.iter().map(|l| (l.0.clone(), l.1)).collect();
    assert_eq!(once.len(), looks.len(), "a class and time looked at twice: {looks:?}");
    assert_eq!(looks.iter().map(|l| l.2).sum::<usize>(), points);
    let starlink: Vec<_> = looks.iter().filter(|l| l.0 == "starlink").collect();
    let at: usize = starlink.iter().map(|l| l.2).sum();
    assert!(starlink.len() * 2 < at, "the Starlink vehicles share their looks: {} looks for {at} points", starlink.len());
    // each answer back in its point's place: the order's points laid out as the order lays them,
    // looked at together, each point's answer the one it had alone
    let c = satcom_coverage(root().join("sim/links.toml"), real.clone());
    let ready = c.ready().await.expect("ready");
    let mut laid = Vec::new();
    for r in &routes {
        let (Some(uid), Ok(route)) = (r.uid, &r.route) else { continue };
        let who = c.who(&w, uid, &ready.pred).expect("a vehicle");
        let legs = order_legs(route, &who).expect("legs");
        laid.push((who.profile.clone(), c.lay(&who, legs, SAT_T0).await));
    }
    let sets: Vec<(&str, &[(GeoPoint, f64)])> = laid.iter().map(|(p, l)| (p.as_str(), l.all.as_slice())).collect();
    let mut placed = 0;
    for ((profile, points), (got, satcom)) in sets.iter().zip(c.satellites_many(&ready, &sets).await) {
        assert_eq!(got.len(), points.len());
        assert_eq!(satcom.is_some(), *profile != "los", "{profile}");
        for ((p, t), got) in points.iter().zip(got) {
            let want = ready.pred.sat_class(profile, p).map(|class| {
                let key = (class.to_string(), [p.lat.to_bits(), p.lon.to_bits(), p.alt_m.to_bits(), t.to_bits()]);
                a.get(&key).cloned().expect("asked alone")
            });
            assert_eq!(got, want, "{profile} at {p:?} {t}");
            placed += usize::from(want.is_some());
        }
    }
    assert_eq!(placed, points);
}

/// Timing aid (bd murmur-7ujq): a Saturation surface group's opening order, 100 Saildrones on
/// Starlink round a 5 km ring on the western traffic lane (SURFACE-1, sim/situations/
/// swarm-saturation.toml, on its links file), its dry run's link answer on the real element
/// sets. `MURMUR_7UJQ_ALONE=1` also answers it a point at a time, as before, and compares. Run in
/// release: `cargo test --release -p murmur-bridge a_saturation_surface_order -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "timing aid: run in release with --nocapture"]
async fn a_saturation_surface_order_timed() {
    let w = world();
    let links = root().join("sim/situations/links/swarm-saturation-bay-region.toml");
    let real: Arc<dyn SpaceSource> = Arc::new(pinned_space(SAT_T0));
    let routes = ring_order(&w, "saildrone", 2001, 100, (-122.78, 37.76), 5000.0);
    let alone = std::env::var("MURMUR_7UJQ_ALONE").is_ok_and(|v| v == "1");
    let mut answers = Vec::new();
    for per_point in if alone { vec![false, true] } else { vec![false] } {
        let src = Counting::new(real.clone(), per_point);
        let c = satcom_coverage(links.clone(), src.clone());
        let again: Vec<VehicleRoute> = routes
            .iter()
            .map(|r| VehicleRoute { uid: r.uid, route: r.route.clone() })
            .collect();
        let started = std::time::Instant::now();
        let a = c.order_answer_at(&w, again, SAT_T0).await;
        let took = started.elapsed().as_secs_f64();
        let vs = a["vehicles"].as_array().expect("vehicles");
        assert_eq!(vs.len(), 100);
        assert!(vs.iter().all(|v| v["profile"] == "satcom_leo" && v.get("error").is_none()), "{}", vs[0]);
        let looks = src.looks();
        let points = src.samples() + looks.iter().map(|l| l.2).sum::<usize>();
        eprintln!(
            "100 Saildrones on Starlink, a 5 km ring, {}: {points} points, {} looks, the order's link answer in {took:.2} s on {} threads",
            if per_point { "a point at a time" } else { "batched" },
            if per_point { src.samples() } else { looks.len() },
            std::thread::available_parallelism().map_or(0, |n| n.get())
        );
        answers.push(a);
    }
    if let [batched, alone] = &answers[..] {
        assert_eq!(batched, alone);
        eprintln!("the two answers are the same");
    }
}
