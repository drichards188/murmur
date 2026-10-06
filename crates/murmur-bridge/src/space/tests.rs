use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use murmur_space::catalog::Catalog;
use tower::ServiceExt;

use crate::world::Config;

const FIXTURE: &str = include_str!("../../../murmur-space/tests/data/gp_fixture.json");

fn fixture_space() -> Space {
    let catalog =
        Catalog::parse(include_str!("../../../../sim/space/constellations.toml")).expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, FIXTURE.to_string()))
        .collect();
    Space::from_sources(catalog, &sources).expect("space")
}

fn world() -> Arc<World> {
    World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: PathBuf::from("/nonexistent/fleet.json"),
        platforms: PathBuf::from("/nonexistent/platforms.toml"),
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    })
}

fn api(at: &str, loaded: bool) -> Api {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let clock = ScenarioClock::new(Some(parse_time(at).expect("time")), 1.0);
    let api = Arc::new(SpaceApi::new(world(), root, clock));
    if loaded {
        api.install(fixture_space());
    }
    api
}

async fn get_json(api: &Api, uri: &str) -> (StatusCode, Value) {
    request(api, Request::get(uri).body(Body::empty()).expect("req")).await
}

async fn request(api: &Api, req: Request<Body>) -> (StatusCode, Value) {
    let res = routes::<()>(api.clone())
        .oneshot(req)
        .await
        .expect("response");
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 64 << 20).await.expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[test]
fn satcom_classes_come_from_the_platform_catalog() {
    let api = api("2026-06-14T10:00:00Z", false);
    let c = |p: &str| api.satcom_classes(Some(p));
    assert_eq!(c("buoy"), ["iridium"]);
    assert_eq!(c("saildrone"), ["starlink", "iridium"]);
    assert_eq!(c("magura"), ["starlink"]);
    assert_eq!(c("rq4"), ["wgs", "ku-geo"]);
    assert_eq!(c("mq9b"), ["wgs", "ku-geo"]);
    assert!(c("puma").is_empty());
    assert!(api.satcom_classes(None).is_empty());
}

#[test]
fn parses_the_time_forms_people_type() {
    let want = parse_time("2026-06-14T10:00:00Z").expect("rfc3339");
    assert_eq!(parse_time("2026-06-14T10:00Z").expect("short"), want);
    assert_eq!(parse_time("2026-06-14T10:00").expect("naive"), want);
    assert_eq!(parse_time("2026-06-14T03:00-07:00").expect("offset"), want);
    assert_eq!(
        parse_time("2026-06-14").expect("date"),
        want - Duration::hours(10)
    );
    assert!(parse_time("June 14").is_err());
}

#[test]
fn scenario_clock_runs_at_its_rate() {
    let at = parse_time("2026-06-14T10:00Z").expect("time");
    let c = ScenarioClock::new(Some(at), 60.0);
    let ahead = c.now() - at;
    assert!(ahead >= Duration::zero() && ahead < Duration::minutes(1));
    assert_eq!(ScenarioClock::new(Some(at), -1.0).rate, 1.0);
}

#[test]
fn pinned_shift_keeps_every_window_on_one_day() {
    let mut space = fixture_space();
    let t = parse_time("2026-06-14T09:00Z").expect("time");
    // unpinned, windows with middles twelve hours apart can land on different days
    let shifts: Vec<i64> = (0..24)
        .map(|h| {
            let from = t + Duration::hours(h);
            space
                .time_map(EpochMode::Auto, from, from + Duration::hours(1))
                .shift_days
        })
        .collect();
    assert!(shifts.iter().any(|s| *s != shifts[0]), "{shifts:?}");
    let pinned = space
        .pin_shift(Some((t, t + Duration::days(1))))
        .expect("pinned");
    for h in 0..24 {
        let from = t + Duration::hours(h);
        let tm = space.time_map(EpochMode::Auto, from, from + Duration::hours(1));
        assert_eq!(tm.shift_days, pinned);
    }
    // direct mode is never shifted
    assert_eq!(space.time_map(EpochMode::Direct, t, t).shift_days, 0);
}

#[tokio::test]
async fn unloaded_space_answers_503_with_a_hint() {
    let api = api("2026-06-14T17:00Z", false);
    *api.space.write().expect("space lock") = Some(Err(format!(
        "space segment unavailable: no element sets; {FETCH_HINT}"
    )));
    let (status, body) = get_json(&api, "/api/space/catalog").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or("")
            .contains("murmur-space --release -- fetch")
    );
}

#[tokio::test]
async fn passes_carry_time_map_scenes_and_sun() {
    let api = api("2026-06-14T16:00Z", true);
    let (status, body) = get_json(
        &api,
        "/api/space/passes?area=golden-gate&from=2026-06-14T16:00Z&to=2026-06-14T22:00Z",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["time_shifted"], true);
    assert!(
        body["time_note"]
            .as_str()
            .unwrap_or("")
            .starts_with("TIME-SHIFTED")
    );
    let windows = body["windows"].as_array().expect("windows");
    assert!(!windows.is_empty());
    for w in windows {
        assert!(w["subpoint"].is_array(), "{w}");
    }
    assert!(windows.iter().any(|w| w["scene"].is_array()));
    assert_eq!(body["sun"].as_array().map(Vec::len), Some(37));
    assert_eq!(body["area"]["name"], "golden-gate");
}

#[tokio::test]
async fn bad_windows_and_areas_are_400() {
    let api = api("2026-06-14T16:00Z", true);
    for uri in [
        "/api/space/passes?from=2026-06-14T16:00Z&to=2026-06-14T15:00Z",
        "/api/space/passes?from=2026-06-14T16:00Z&to=2026-06-20T15:00Z",
        "/api/space/passes?area=atlantis",
        "/api/space/passes?area=1,2,3",
        "/api/space/track",
    ] {
        let (status, _) = get_json(&api, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn task_returns_ranked_options_and_exclusions_without_submitting() {
    let api = api("2026-06-14T14:00Z", true);
    let body = json!({
        "capabilities": ["sar", "optical"],
        "area": {"bbox": [-122.56, 37.77, -122.38, 37.88]},
        "deadline": "2026-06-14T23:00Z",
        "rank": "best_resolution",
        "exclusions": 5,
    });
    let req = Request::post("/api/space/task")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("req");
    let (status, out) = request(&api, req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let options = out["options"].as_array().expect("options");
    assert!(!options.is_empty());
    assert_eq!(options[0]["rank"], 1);
    assert!(out["exclusions"].as_array().map_or(0, Vec::len) <= 5);
    assert!(out["exclusions_total"].as_u64().is_some());
    assert!(
        out["request"]["now"]
            .as_str()
            .unwrap_or("")
            .starts_with("2026-06-14T14:00:0")
    );
    assert!(
        out["note"]
            .as_str()
            .unwrap_or("")
            .contains("nothing has been requested")
    );

    let req = Request::post("/api/space/task")
        .header("content-type", "application/json")
        .body(Body::from(json!({"capabilities": ["satcom"]}).to_string()))
        .expect("req");
    assert_eq!(request(&api, req).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn positions_are_subsampled_per_class() {
    let api = api("2026-06-14T16:00Z", true);
    let (status, body) = get_json(&api, "/api/space/positions?limit=5").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let classes = body["classes"].as_array().expect("classes");
    assert!(!classes.is_empty());
    assert!(classes.iter().all(|c| c["class_id"] != "iss"));
    for c in classes {
        let shown = c["sats"].as_array().map_or(0, Vec::len);
        assert!(shown <= 5, "{c}");
        assert_eq!(c["shown"].as_u64(), Some(shown as u64));
    }
}

#[tokio::test]
async fn links_gnss_track_and_overhead_answer() {
    let api = api("2026-06-14T16:00Z", true);
    let (status, body) = get_json(
        &api,
        "/api/space/links?lat=37.8&lon=-122.6&classes=wgs&windows=0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["classes"][0]["windows"].as_array().map(Vec::len),
        Some(0)
    );
    let (status, _) = get_json(&api, "/api/space/links?uid=7").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = get_json(&api, "/api/space/gnss?step=3600").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get_json(&api, "/api/space/overhead").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["overhead"].is_array());
    let (status, body) = get_json(&api, "/api/space/links/now").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let norad = fixture_space().classes[0].sats[0].norad_id;
    let (status, body) = get_json(&api, &format!("/api/space/track?norad={norad}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["track"].as_array().map_or(0, Vec::len) > 100);
}

#[test]
fn polygon_contains() {
    let a = Area::Polygon {
        ring: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    };
    assert!(contains(&a, [0.5, 0.5]));
    assert!(!contains(&a, [1.5, 0.5]));
}
