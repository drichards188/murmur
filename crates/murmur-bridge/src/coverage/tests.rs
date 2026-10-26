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
    Coverage::new(Sources {
        links: root().join("sim/links.toml"),
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
    let cfg = LinksConfig::load(&root().join("sim/links.toml")).expect("links");
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
