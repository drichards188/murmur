//! SATCOM from real orbits: the murmur-space source against a fixed element-set fixture
//! (tests/data/sat_fixture.json: CelesTrak GP data of 2026-10-02 for the WGS group, 25
//! Starlink satellites and Intelsat 905), so the tests need no network or cache.

use murmur_linkemu::geo::GeoPoint;
use murmur_linkemu::sat::{RealSpace, SpaceSource};
use murmur_linkemu::timeline::parse_iso_utc;
use murmur_space::catalog::Catalog;
use murmur_space::space::Space;
use murmur_space::timemap::TimeMap;

const FIXTURE: &str = include_str!("data/sat_fixture.json");

fn source() -> RealSpace {
    let catalog =
        Catalog::parse(include_str!("../../../sim/space/constellations.toml")).expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, FIXTURE.to_string()))
        .collect();
    let space = Space::from_sources(catalog, &sources).expect("space");
    let tm = TimeMap::identity(space.reference);
    RealSpace::new(space, tm)
}

fn t0() -> f64 {
    parse_iso_utc("2026-10-02T04:00:00Z").expect("iso")
}

fn at(lat: f64, lon: f64) -> GeoPoint {
    GeoPoint {
        lat,
        lon,
        alt_m: 0.0,
    }
}

const GOLDEN_GATE: (f64, f64) = (37.8199, -122.4783);

#[test]
fn geo_classes_are_up_with_slant_range_latency() {
    let s = source();
    let p = at(GOLDEN_GATE.0, GOLDEN_GATE.1);
    let wgs = s.sample("wgs", &p, t0()).expect("wgs");
    assert!(wgs.visible >= 1);
    // Bent pipe over two ~36,000-41,000 km legs, no network allowance: 245-285 ms.
    let l = wgs.latency_ms.expect("latency");
    assert!((245.0..285.0).contains(&l), "wgs latency {l}");

    let ku = s.sample("ku-geo", &p, t0()).expect("ku");
    let best = ku.best.as_ref().expect("Intelsat 905 is above the mask");
    assert_eq!(best.norad_id, 27438);
    // The same geometry plus the class's 20 ms allowance.
    let l = ku.latency_ms.expect("latency");
    assert!((265.0..305.0).contains(&l), "ku latency {l}");
    assert!(best.range_km > 35_000.0 && best.range_km < 42_000.0);
}

#[test]
fn a_terminal_that_cannot_see_the_satellite_is_down() {
    let s = source();
    // Intelsat 905 is over the Atlantic; from the Indian Ocean it is below the horizon.
    let ku = s.sample("ku-geo", &at(0.0, 95.0), t0()).expect("ku");
    assert_eq!(ku.visible, 0);
    assert!(ku.best.is_none());
    assert!(ku.latency_ms.is_none());
    // And it does not come back within an hour: GEO.
    let o = s
        .outlook("ku-geo", &at(0.0, 95.0), t0(), 3600.0)
        .expect("outlook");
    assert_eq!(o.next_up_unix, None);
}

#[test]
fn an_up_geo_link_has_no_outage_ahead() {
    let s = source();
    let o = s
        .outlook("wgs", &at(GOLDEN_GATE.0, GOLDEN_GATE.1), t0(), 3600.0)
        .expect("outlook");
    assert_eq!(o.next_outage_unix, None);
    assert_eq!(o.next_up_unix, None);
}

#[test]
fn unknown_and_unusable_classes_are_refused() {
    let s = source();
    assert!(s.check("nope", t0()).is_err());
    assert!(s.check("wgs", t0()).is_ok());
    // The fixture has no Iridium satellites: the class exists but nothing is usable.
    let e = s.check("iridium", t0()).expect_err("iridium is empty here");
    assert!(e.contains("no usable satellites"), "{e}");
    // Elements two days old and more than max_element_age_days away are stale, not usable.
    assert!(s.check("wgs", t0() + 40.0 * 86400.0).is_err());
}

/// With only 25 Starlink satellites the link has gaps. The look-ahead must agree with what
/// instantaneous sampling says at the edges it reports.
#[test]
fn outlook_agrees_with_sampling_for_a_sparse_leo_constellation() {
    let s = source();
    let p = at(GOLDEN_GATE.0, GOLDEN_GATE.1);
    let horizon = 6.0 * 3600.0;
    let mut checked_up = 0;
    let mut checked_outage = 0;
    let mut t = t0();
    while t < t0() + 3.0 * 3600.0 && (checked_up < 2 || checked_outage < 2) {
        let now = s.sample("starlink", &p, t).expect("sample");
        let o = s.outlook("starlink", &p, t, horizon).expect("outlook");
        if now.best.is_none() {
            assert_eq!(o.next_outage_unix, None);
            if let Some(up) = o.next_up_unix {
                assert!(up > t);
                let before = s.sample("starlink", &p, up - 2.0).expect("before");
                let after = s.sample("starlink", &p, up + 2.0).expect("after");
                assert!(
                    before.best.is_none(),
                    "still down 2 s before the pass at {up}"
                );
                assert!(after.best.is_some(), "up 2 s after the pass starts at {up}");
                checked_up += 1;
            }
        } else {
            assert_eq!(o.next_up_unix, None);
            if let Some(out) = o.next_outage_unix {
                assert!(out > t);
                let before = s.sample("starlink", &p, out - 2.0).expect("before");
                let after = s.sample("starlink", &p, out + 2.0).expect("after");
                assert!(before.best.is_some(), "up 2 s before the outage at {out}");
                assert!(
                    after.best.is_none(),
                    "down 2 s after the outage starts at {out}"
                );
                checked_outage += 1;
            }
        }
        t += 300.0;
    }
    assert!(
        checked_up >= 1 && checked_outage >= 1,
        "fixture should show both kinds of edge (up {checked_up}, outage {checked_outage})"
    );
}

#[test]
fn leo_latency_is_short_and_follows_range() {
    let s = source();
    let p = at(GOLDEN_GATE.0, GOLDEN_GATE.1);
    let mut seen = 0;
    let mut t = t0();
    while t < t0() + 4.0 * 3600.0 {
        let o = s.sample("starlink", &p, t).expect("sample");
        if let (Some(b), Some(ms)) = (o.best, o.latency_ms) {
            // Bent pipe at the user's range both ways plus 8 ms: slant 550-2,500 km.
            let expect = 2.0 * b.range_km / 299_792.458 * 1000.0 + 8.0;
            assert!((ms - expect).abs() < 0.01, "{ms} vs {expect}");
            assert!(
                (8.0..40.0).contains(&ms),
                "latency {ms} at range {}",
                b.range_km
            );
            seen += 1;
        }
        t += 120.0;
    }
    assert!(seen > 0);
}
