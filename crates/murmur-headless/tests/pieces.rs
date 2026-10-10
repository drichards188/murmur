//! The simulator's own pieces (the episode as a whole is tested from Python,
//! eval/headless/tests/test_episode.py): the baked terrain grid, and the SATCOM look cache that
//! must answer the same whether it held a look or not.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use murmur_headless::sim::space_cache::{CachedSpace, QUANT_DEG};
use murmur_headless::sim::terrain::Grid;
use murmur_linkemu::geo::GeoPoint;
use murmur_linkemu::sat::{BestSat, Outlook, SatObs, SpaceSource};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A tool result reads as the live session reads it (agents/scripted_agent.py `content`).
#[test]
fn a_tool_result_reads_as_the_live_session_reads_it() {
    use murmur_headless::sim::tool_result_json;
    use serde_json::json;
    let structured = json!({"structuredContent": {"a": 1}, "content": [{"type": "text", "text": "{\"b\": 2}"}]});
    assert_eq!(tool_result_json(&structured), json!({"a": 1}));
    let text_json = json!({"content": [{"type": "text", "text": "{\"b\": 2}"}]});
    assert_eq!(tool_result_json(&text_json), json!({"b": 2}));
    let text = json!({"content": [{"type": "image", "data": ""}, {"type": "text", "text": "not json"}]});
    assert_eq!(tool_result_json(&text), json!({"text": "not json"}));
    assert_eq!(tool_result_json(&json!({"content": []})), json!({}));
}

#[test]
fn the_grid_has_the_presidio_and_the_strait() {
    let g = Grid::load(&repo().join("sim/headless/terrain/golden-gate.grid.gz")).expect("grid");
    // tidewater answered 81.1 m at (37.80, -122.47) and the water level (0.44 m) mid-strait
    let land = g.height(37.80, -122.47).expect("land");
    assert!((land - 81.1).abs() < 8.0, "{land}");
    let sea = g.height(37.82, -122.48).expect("sea");
    assert!((sea - 0.44).abs() < 0.2, "{sea}");
    assert_eq!(g.height(37.0, -122.47), None, "outside the grid");
}

/// A source whose answer is a function of where and when it is asked, and that counts asks.
struct Fake(AtomicU64);

impl SpaceSource for Fake {
    fn check(&self, _: &str, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn sample(&self, _: &str, pos: &GeoPoint, unix_s: f64) -> Result<SatObs, String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(SatObs {
            visible: 3,
            best: Some(BestSat {
                norad_id: 7,
                name: "S".into(),
                elevation_deg: pos.lat * 1000.0 + pos.lon,
                range_km: unix_s,
            }),
            latency_ms: Some(pos.alt_m),
        })
    }
    fn outlook(&self, _: &str, _: &GeoPoint, _: f64, _: f64) -> Result<Outlook, String> {
        Ok(Outlook::default())
    }
}

#[test]
fn a_cached_look_is_the_look_it_would_have_computed() {
    let dir = std::env::temp_dir().join(format!("murmur-headless-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("c.jsonl");
    let p = GeoPoint { lat: 37.82013, lon: -122.47044, alt_m: 3.0 };
    let near = GeoPoint { lat: 37.82021, lon: -122.47049, alt_m: 12.0 };
    let fresh = Arc::new(Fake(AtomicU64::new(0)));
    let a = CachedSpace::new(fresh.clone(), Some(file.clone()));
    let x = a.sample("starlink", &p, 1000.0).expect("look");
    let y = a.sample("starlink", &near, 1000.0).expect("look");
    assert_eq!(x, y, "both round to the same point");
    assert_eq!(fresh.0.load(Ordering::Relaxed), 1);
    // computed at the rounded point, not the exact one
    let e = x.best.as_ref().expect("best").elevation_deg;
    let want = (37.82013f64 / QUANT_DEG).round() * QUANT_DEG * 1000.0 + (-122.47044f64 / QUANT_DEG).round() * QUANT_DEG;
    assert!((e - want).abs() < 1e-9, "{e} {want}");
    a.save();
    // a later episode: from the file, the same answer, nothing computed
    let later = Arc::new(Fake(AtomicU64::new(0)));
    let b = CachedSpace::new(later.clone(), Some(file));
    assert_eq!(b.sample("starlink", &p, 1000.0).expect("look"), x);
    assert_eq!(later.0.load(Ordering::Relaxed), 0);
    // another instant is another look
    let z = b.sample("starlink", &p, 1005.0).expect("look");
    assert_ne!(z, x);
    let _ = std::fs::remove_dir_all(&dir);
}
