//! End-to-end checks on a fixed element-set fixture (tests/data/gp_fixture.json, CelesTrak GP
//! data of 2026-10-02), so they run without a network or cache.

use chrono::{DateTime, Duration, TimeZone, Utc};
use murmur_space::access::{self, Constraint, SearchParams};
use murmur_space::area::Area;
use murmur_space::catalog::{Capability, Catalog};
use murmur_space::geo::{Geodetic, Observer};
use murmur_space::space::{AccessQuery, GnssQuery, LinkQuery, Space};
use murmur_space::tasking::{ExclusionReason, NoCloudModel, Rank, TaskRequest};
use murmur_space::timemap::EpochMode;

const FIXTURE: &str = include_str!("data/gp_fixture.json");
const GG_BRIDGE: (f64, f64) = (37.8199, -122.4783);

/// Satellites launched after a past scenario date are left out of it.
#[test]
fn launch_estimates() {
    let sp = space();
    let (_, iss) = sp.satellite(25544).expect("iss");
    let launch = iss.estimated_launch().expect("launch");
    assert_eq!(launch.format("%Y").to_string(), "1998");
    for cs in &sp.classes {
        for s in &cs.sats {
            assert!(
                s.estimated_launch().is_some_and(|l| l <= s.epoch),
                "{}",
                s.name
            );
        }
    }
}

fn space() -> Space {
    let catalog =
        Catalog::parse(include_str!("../../../sim/space/constellations.toml")).expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, FIXTURE.to_string()))
        .collect();
    Space::from_sources(catalog, &sources).expect("space")
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("time")
        .with_timezone(&Utc)
}

/// ISS passes (10 deg mask) over the Golden Gate Bridge against Skyfield 1.55 with the same
/// element set (see tools/crosscheck_skyfield.py): rise and set within 0.5 s.
#[test]
fn iss_passes_match_skyfield() {
    let sp = space();
    let rep = sp
        .links(&LinkQuery {
            point: Geodetic::new(GG_BRIDGE.0, GG_BRIDGE.1, 0.0),
            from: t("2026-10-02T00:00:00Z"),
            to: t("2026-10-04T00:00:00Z"),
            classes: vec!["iss".into()],
            epoch_mode: EpochMode::Direct,
            sample_step_s: 600.0,
            include_samples: false,
        })
        .expect("links");
    let w = &rep.classes[0].windows;
    let skyfield = [
        ("2026-10-02T00:47:58.208Z", "2026-10-02T00:54:03.690Z"),
        ("2026-10-02T02:24:53.087Z", "2026-10-02T02:30:40.474Z"),
        ("2026-10-02T17:30:27.934Z", "2026-10-02T17:36:05.723Z"),
        ("2026-10-02T19:06:51.755Z", "2026-10-02T19:13:08.043Z"),
        ("2026-10-02T20:47:04.309Z", "2026-10-02T20:48:13.370Z"),
        ("2026-10-03T00:01:02.374Z", "2026-10-03T00:06:22.730Z"),
        ("2026-10-03T01:37:26.581Z", "2026-10-03T01:43:58.323Z"),
        ("2026-10-03T16:44:24.182Z", "2026-10-03T16:48:07.434Z"),
        ("2026-10-03T18:19:24.385Z", "2026-10-03T18:26:06.136Z"),
        ("2026-10-03T19:58:34.326Z", "2026-10-03T20:01:48.064Z"),
        ("2026-10-03T23:14:10.846Z", "2026-10-03T23:18:29.765Z"),
    ];
    assert_eq!(w.len(), skyfield.len(), "{w:#?}");
    for (mine, (rise, set)) in w.iter().zip(skyfield) {
        let dr = (mine.start - t(rise)).num_milliseconds().abs();
        let ds = (mine.end - t(set)).num_milliseconds().abs();
        assert!(
            dr < 500 && ds < 500,
            "{} rise {dr} ms, set {ds} ms",
            mine.start
        );
    }
}

/// The adaptive search finds the same windows as a brute-force 1 s scan.
#[test]
fn adaptive_search_matches_brute_force() {
    let sp = space();
    let target = Observer::new(Geodetic::new(37.825, -122.47, 0.0));
    let from = t("2026-10-02T00:00:00Z");
    let to = from + Duration::hours(24);
    let mut checked = 0;
    for cs in &sp.classes {
        for sat in cs.sats.iter().take(6) {
            let c = match cs.class.capability {
                Capability::Optical => Constraint::MaxOffNadir(30.0),
                Capability::Sar => Constraint::Incidence {
                    min: 25.0,
                    max: 40.0,
                },
                _ => Constraint::MinElevation(10.0),
            };
            let fast = access::find_windows(
                sat,
                &target,
                from,
                to,
                c.psi_max(sat.max_radius_km()),
                SearchParams::default(),
                |_, g| c.holds(g),
            );
            let mut slow: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
            let mut open: Option<DateTime<Utc>> = None;
            let mut tt = from;
            while tt <= to {
                let ok = access::geometry(sat, &target, &tt).is_some_and(|g| c.holds(&g));
                match (ok, open) {
                    (true, None) => open = Some(tt),
                    (false, Some(s)) => {
                        slow.push((s, tt));
                        open = None;
                    }
                    _ => {}
                }
                tt += Duration::seconds(1);
            }
            if let Some(s) = open {
                slow.push((s, to));
            }
            // Windows under the 5 s fine step may be missed by design; compare the rest.
            let slow: Vec<_> = slow
                .into_iter()
                .filter(|(a, b)| (*b - *a).num_seconds() >= 6)
                .collect();
            assert_eq!(
                fast.iter()
                    .filter(|w| (w.end - w.start).num_seconds() >= 6)
                    .count(),
                slow.len(),
                "{} {}",
                cs.class.id,
                sat.name
            );
            for (f, s) in fast
                .iter()
                .filter(|w| (w.end - w.start).num_seconds() >= 6)
                .zip(&slow)
            {
                assert!(
                    (f.start - s.0).num_milliseconds().abs() <= 1100,
                    "{}",
                    sat.name
                );
                assert!(
                    (f.end - s.1).num_milliseconds().abs() <= 1100,
                    "{}",
                    sat.name
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 20);
}

#[test]
fn optical_is_excluded_at_night_sar_is_not() {
    let sp = space();
    // A night window at the Golden Gate (local 22:00-05:00 PDT).
    let req = TaskRequest {
        capabilities: vec![Capability::Optical, Capability::Sar],
        area: Area::point(37.825, -122.47, 5.0),
        from: t("2026-10-02T05:00:00Z"),
        to: t("2026-10-02T12:00:00Z"),
        now: t("2026-10-02T03:00:00Z"),
        product_deadline: None,
        max_resolution_m: None,
        rank: Rank::EarliestProduct,
        classes: vec![],
        include_untaskable: true,
        epoch_mode: EpochMode::Direct,
    };
    let res = sp.tasking_options(&req, &NoCloudModel).expect("tasking");
    assert!(
        res.options.iter().all(|o| o.capability == Capability::Sar),
        "optical option at night"
    );
    assert!(
        res.exclusions
            .iter()
            .any(|e| e.reason == ExclusionReason::Night)
    );
    // Ranked by product time.
    for w in res.options.windows(2) {
        assert!(w[0].product_time <= w[1].product_time);
    }
}

#[test]
fn deadline_and_lead_time_are_enforced() {
    let sp = space();
    let from = t("2026-10-02T14:00:00Z");
    let base = TaskRequest {
        capabilities: vec![Capability::Optical, Capability::Sar],
        area: Area::point(37.825, -122.47, 5.0),
        from,
        to: from + Duration::hours(12),
        now: from,
        product_deadline: None,
        max_resolution_m: None,
        rank: Rank::BestResolution,
        classes: vec![],
        include_untaskable: false,
        epoch_mode: EpochMode::Direct,
    };
    let all = sp.tasking_options(&base, &NoCloudModel).expect("tasking");
    assert!(!all.options.is_empty());
    for o in &all.options {
        assert!(o.collect_time >= o.earliest_collect);
        assert!(o.taskable);
    }
    for w in all.options.windows(2) {
        assert!(w[0].resolution_m.unwrap_or(f64::MAX) <= w[1].resolution_m.unwrap_or(f64::MAX));
    }
    let deadline = from + Duration::hours(6);
    let tight = sp
        .tasking_options(
            &TaskRequest {
                product_deadline: Some(deadline),
                ..base.clone()
            },
            &NoCloudModel,
        )
        .expect("tasking");
    assert!(tight.options.len() < all.options.len());
    assert!(
        tight
            .options
            .iter()
            .all(|o| o.product_time.is_some_and(|p| p <= deadline))
    );
    assert!(
        tight
            .exclusions
            .iter()
            .any(|e| e.reason == ExclusionReason::ProductTooLate)
    );
}

#[test]
fn geo_link_is_continuous_with_geo_latency() {
    let sp = space();
    let rep = sp
        .links(&LinkQuery {
            point: Geodetic::new(GG_BRIDGE.0, GG_BRIDGE.1, 0.0),
            from: t("2026-10-02T00:00:00Z"),
            to: t("2026-10-02T06:00:00Z"),
            classes: vec!["wgs".into()],
            epoch_mode: EpochMode::Direct,
            sample_step_s: 300.0,
            include_samples: true,
        })
        .expect("links");
    let c = &rep.classes[0];
    assert!((c.coverage_fraction - 1.0).abs() < 1e-9);
    assert!(c.visible_min >= 2);
    let lat = c.latency_ms_median.expect("latency");
    // Bent pipe: up and down at ~37,000-41,000 km each.
    assert!((240.0..280.0).contains(&lat), "{lat}");
    assert_eq!(c.samples.len(), 73);
}

#[test]
fn gps_visibility_and_dop() {
    let sp = space();
    let rep = sp
        .gnss(&GnssQuery {
            point: Geodetic::new(GG_BRIDGE.0, GG_BRIDGE.1, 0.0),
            from: t("2026-10-02T00:00:00Z"),
            to: t("2026-10-03T00:00:00Z"),
            step_s: 900.0,
            class: "gps".into(),
            mask_deg: None,
            epoch_mode: EpochMode::Direct,
        })
        .expect("gnss");
    assert!(rep.visible_min >= 6, "{}", rep.visible_min);
    assert!(rep.pdop_max.expect("pdop") < 4.0);
}

/// A June scenario window is time-shifted to the elements' epoch; windows come back in
/// scenario (June) time with June sun.
#[test]
fn june_scenario_is_shifted_and_reported_in_scenario_time() {
    let sp = space();
    let from = Utc
        .with_ymd_and_hms(2026, 6, 14, 7, 0, 0)
        .single()
        .expect("t");
    let to = from + Duration::days(1);
    let rep = sp
        .imaging_access(&AccessQuery {
            area: Area::point(37.825, -122.47, 5.0),
            from,
            to,
            classes: vec![],
            epoch_mode: EpochMode::Auto,
            include_night: true,
        })
        .expect("access");
    assert!(rep.time_map.shift_days > 100);
    assert!(!rep.windows.is_empty());
    for w in &rep.windows {
        assert!(w.start >= from && w.end <= to);
        assert!(w.element_age_days.abs() < 2.0);
    }
    // Sun-synchronous WV-3 passes by day near late morning local time, and the June sun is
    // high then.
    let wv3: Vec<_> = rep
        .windows
        .iter()
        .filter(|w| w.satellite.starts_with("WORLDVIEW-3") && w.daylight_ok == Some(true))
        .collect();
    for w in wv3 {
        assert!(w.sun_elevation_deg > 40.0, "{w:?}");
    }
}
