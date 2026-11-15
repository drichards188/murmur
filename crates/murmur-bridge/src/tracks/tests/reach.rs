//! The reach display (bd murmur-4tq, `tracks::reach`): on a clear day at a platform's operating
//! height it is the catalog's `physical_reach_m` for every sensor and domain; fog, night and the
//! sea move it as the physical model says and `limited_by` names why; a radar does not see fog;
//! the legacy model shows none.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use murmur_decide::Domain as DDomain;

use super::super::physical::conditions::Conditions;
use super::super::reach::{self, DomainReach, reference_conditions};
use super::super::sensing::{Observer, SensorCatalog, SensorType};
use super::super::truth::SensingModel;
use super::super::{Settings, Tracks};
use super::{BASE, catalog};

const PLATFORMS: &str = include_str!("../../../../../sim/platforms.toml");
/// 2026-06-14T20:10Z (the generator's clock; the sun is the conditions' own).
const T: f64 = 1_781_467_800.0;

/// The catalog's clear-day reaches, by platform and sensor type.
fn clear_day() -> HashMap<String, HashMap<String, murmur_decide::SensorReach>> {
    murmur_decide::Catalog::from_toml(PLATFORMS)
        .expect("catalog")
        .reaching(murmur_decide::Reaches::Physical)
        .platforms
        .into_iter()
        .map(|(id, p)| (id, p.sensors.into_iter().filter_map(|s| s.reach_m.map(|r| (s.kind, r))).collect::<HashMap<_, _>>()))
        .filter(|(_, m)| !m.is_empty())
        .collect()
}

/// A platform at its operating height (a stand-in's `alt_m`, else `typical_alt_m`), as the
/// generator places it, and its domain.
fn at_operating(platform: &str, cat: &SensorCatalog) -> (Observer, String) {
    let table: toml::Table = PLATFORMS.parse().expect("toml");
    let p = table.get(platform).and_then(|p| p.as_table()).expect(platform);
    let num = |k: &str| p.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
    let alt = num("alt_m").or_else(|| num("typical_alt_m")).unwrap_or(0.0);
    let domain = p.get("domain").and_then(|v| v.as_str()).unwrap_or("surface").to_string();
    let ps = cat.get(platform).expect(platform);
    let o = Observer {
        uid: 1,
        lat: BASE.0,
        lon: BASE.1,
        alt_msl: alt,
        mast_m: ps.mast_m,
        submerged: domain == "subsurface" && alt < -0.5,
        sensors: ps.sensors.clone(),
        ..Observer::default()
    };
    (o, domain)
}

fn one(platform: &str, kind: SensorType, d: DDomain, cond: &Conditions) -> DomainReach {
    let cat = catalog();
    let (o, own) = at_operating(platform, &cat);
    let clear = clear_day();
    let spec = o.sensors.iter().find(|s| s.kind == kind).expect("sensor");
    let now = reach::sensor_now(spec, &o, cond, T, clear.get(platform).and_then(|m| m.get(kind.as_str())), &own).expect("a sensor that confirms");
    now.domains.into_iter().find(|r| r.domain == d).expect("domain")
}

/// On the reference day, at its operating height, every sensor's reach in every domain is the
/// catalog's (rounded to 10 m there), and a domain the catalog leaves out is one it does not reach.
#[test]
fn a_clear_day_is_the_catalogs_reach() {
    let cat = catalog();
    let clear = clear_day();
    let mut checked = 0;
    for (platform, by_kind) in &clear {
        let (o, own) = at_operating(platform, &cat);
        for spec in &o.sensors {
            let Some(want) = by_kind.get(spec.kind.as_str()) else { continue };
            let now = reach::sensor_now(spec, &o, &reference_conditions(), T, Some(want), &own).expect("a sensor that confirms");
            for d in &now.domains {
                match want.get(d.domain) {
                    Some(m) => {
                        assert!((d.detect_m - m).abs() <= 10.0, "[{platform}] {} {:?}: {} m now, {m} m in the catalog", now.kind, d.domain, d.detect_m);
                        assert_eq!(d.limited_by, None, "[{platform}] {} {:?}: nothing limits a clear day", now.kind, d.domain);
                        checked += 1;
                    }
                    None => assert!(d.detect_m < 5.0, "[{platform}] {} {:?}: {} m where the catalog has none", now.kind, d.domain, d.detect_m),
                }
            }
        }
    }
    assert!(checked > 50, "{checked} reaches checked");
}

/// Fog at 50 m takes a camera's reach of a surface contact down to a small share, and says fog;
/// haze at 5 km less so, and says haze.
#[test]
fn fog_shrinks_a_camera_and_says_so() {
    let fog = Conditions { visibility_m: 50.0, ..reference_conditions() };
    let r = one("puma", SensorType::EoIr, DDomain::Surface, &fog);
    let ratio = r.ratio().expect("a reference");
    assert!(ratio < 0.2, "PUMA's camera in fog: {} m, {ratio:.2} of a clear day", r.detect_m);
    assert_eq!(r.limited_by, Some("fog"));
    assert!(r.factors.iter().any(|(f, m)| *f == "fog" && (*m - r.clear_day_m.expect("present")).abs() <= 10.0), "{:?}", r.factors);
    assert!(r.identify_m <= r.recognise_m && r.recognise_m <= r.detect_m.max(r.recognise_m));
    let haze = Conditions { visibility_m: 5_000.0, ..reference_conditions() };
    let h = one("puma", SensorType::EoIr, DDomain::Surface, &haze);
    assert!(h.detect_m > r.detect_m, "haze {} m, fog {} m", h.detect_m, r.detect_m);
    if h.limited_by.is_some() {
        assert_eq!(h.limited_by, Some("haze"));
    }
}

/// At night a camera with only a visible channel sees nothing, and says night; at twilight it
/// says twilight. A thermal channel keeps its reach at night.
#[test]
fn night_blinds_a_visible_only_camera() {
    let text = r#"
[daycam]
domain = "air"
alt_m = 120
sensors = [{ type = "eo_ir", range_m = 4000, channels = [{ band = "day", wide_deg = 56, narrow_deg = 1.2, px = 4472, blur_urad = 25 }] }]
"#;
    let cat = SensorCatalog::from_toml(text).expect("catalog");
    let ps = cat.get("daycam").expect("daycam");
    let o = Observer { uid: 1, lat: BASE.0, lon: BASE.1, alt_msl: 120.0, mast_m: ps.mast_m, sensors: ps.sensors.clone(), ..Observer::default() };
    let spec = &o.sensors[0];
    let day = reach::sensor_now(spec, &o, &reference_conditions(), T, None, "air").expect("eo");
    let night = reach::sensor_now(spec, &o, &Conditions { sun_elevation_deg: Some(-30.0), ..reference_conditions() }, T, None, "air").expect("eo");
    let surface = |n: &reach::SensorNow| n.domains.iter().find(|d| d.domain == DDomain::Surface).cloned().expect("surface");
    assert!(surface(&day).detect_m > 5_000.0, "by day {} m", surface(&day).detect_m);
    assert_eq!(surface(&night).detect_m, 0.0, "a day camera at night");
    assert_eq!(surface(&night).limited_by, Some("night"));
    let dusk = reach::sensor_now(spec, &o, &Conditions { sun_elevation_deg: Some(-5.5), ..reference_conditions() }, T, None, "air").expect("eo");
    assert!(surface(&dusk).detect_m < surface(&day).detect_m);
    assert_eq!(surface(&dusk).limited_by, Some("twilight"));
    // PUMA's LWIR channel keeps most of its reach in the dark
    let puma = one("puma", SensorType::EoIr, DDomain::Surface, &Conditions { sun_elevation_deg: Some(-30.0), ..reference_conditions() });
    assert!(puma.detect_m > 0.0, "PUMA's thermal channel at night");
}

/// Fog does not move a radar; a rough sea moves a radar's reach of a surface contact and says sea
/// clutter.
#[test]
fn a_radar_does_not_see_fog() {
    let clear = one("seahunter", SensorType::Radar, DDomain::Surface, &reference_conditions());
    let fog = one("seahunter", SensorType::Radar, DDomain::Surface, &Conditions { visibility_m: 50.0, ..reference_conditions() });
    assert_eq!(clear.detect_m, fog.detect_m, "a radar in fog");
    assert_eq!(fog.limited_by, None);
    let air = one("seahunter", SensorType::Radar, DDomain::Air, &Conditions { visibility_m: 50.0, ..reference_conditions() });
    assert!((air.detect_m - air.clear_day_m.expect("present")).abs() <= 10.0);
    let rough = one("seahunter", SensorType::Radar, DDomain::Surface, &Conditions { sea_state: 6.0, wind_mps: 15.0, ..reference_conditions() });
    assert!(rough.detect_m < clear.detect_m, "rough sea {} m, calm {} m", rough.detect_m, clear.detect_m);
    if rough.detect_m < 0.9 * clear.detect_m {
        assert_eq!(rough.limited_by, Some("sea clutter"));
    }
}

/// A submerged vehicle's sensors but sonar see nothing; a camera's default domain over the water
/// is the surface, a sonar's under it.
#[test]
fn submerged_and_primary_domains() {
    let cat = catalog();
    let (mut o, own) = at_operating("seahunter", &cat);
    let clear = clear_day();
    let by_kind = clear.get("seahunter");
    o.submerged = true;
    for spec in &o.sensors {
        let Some(now) = reach::sensor_now(spec, &o, &reference_conditions(), T, by_kind.and_then(|m| m.get(spec.kind.as_str())), &own) else { continue };
        match spec.kind {
            SensorType::Sonar => assert_eq!(now.primary, DDomain::Subsurface),
            _ => {
                assert_eq!(now.primary, DDomain::Surface);
                assert!(now.domains.iter().all(|d| d.detect_m == 0.0 && d.limited_by == Some("submerged")));
            }
        }
    }
}

/// Under the legacy model the endpoint shows no reaches (weather changes no detection there).
#[test]
fn the_legacy_model_shows_no_reaches() {
    use crate::world::{Config, World};
    let world = World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: PathBuf::from("/nonexistent/fleet.json"),
        platforms: PathBuf::from("/nonexistent/platforms.toml"),
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    });
    let settings = Settings { seed: Some(1), misclassification_rate: Some(0.0), eval_truth: false, drop_s: 180.0, model: Some(SensingModel::Legacy), sim_token: None };
    let tracks = Tracks::new(catalog(), settings).with_clear_day(PLATFORMS).expect("clear day");
    let v = tracks.reach_json(&world, &super::super::ReachOf::All, crate::clock::wall());
    assert_eq!(v["model"], "legacy");
    assert_eq!(v["vehicles"].as_array().map(Vec::len), Some(0));
}

fn test_world() -> Arc<crate::world::World> {
    use crate::world::{Config, World};
    World::new(Config {
        base_port: 0,
        http_port: 0,
        default_ports: 0,
        manifest: PathBuf::from("/nonexistent/fleet.json"),
        platforms: PathBuf::from("/nonexistent/platforms.toml"),
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    })
}

/// A puma heard just now at 120 m, uid 1.
fn add_puma(world: &crate::world::World) {
    add_vehicle(world, 1, "puma", "air", 120.0, (BASE.0, BASE.1));
}

/// A vehicle of `platform` heard just now.
fn add_vehicle(world: &crate::world::World, uid: u64, platform: &str, domain: &str, alt: f64, pos: (f64, f64)) {
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
    m.insert("platform".into(), serde_json::json!(platform));
    m.insert("callsign".into(), serde_json::json!(if platform == "puma" && uid == 1 { "PUMA 1".to_string() } else { format!("{platform} {uid}") }));
    m.insert("domain".into(), serde_json::json!(domain));
    let cell = Arc::new(VehicleCell::new(uid, (uid % 250) as u8, Arc::new(NoTx), Arc::new(Mutex::new(stats)), Some(Arc::new(Identity::from_manifest(&m)))));
    cell.write(|v| {
        v.pos = Some(pos);
        v.alt_msl = Some(alt);
    });
    world.insert_vehicle(cell);
}

/// The endpoint end to end: the scenario's weather where the vehicle is (the running model's own
/// merge, `Engine::conditions_at`), fog then clear, the answer kept between and computed again
/// when the visibility moves; no truth in it.
#[test]
fn the_endpoint_reads_the_weather_where_the_vehicle_is() {
    use super::super::truth::{Sensing, TruthMessage};
    let world = test_world();
    add_puma(&world);
    let settings = Settings { seed: Some(1), misclassification_rate: Some(0.0), eval_truth: false, drop_s: 180.0, model: Some(SensingModel::Physical), sim_token: None };
    let tracks = Tracks::new(catalog(), settings).with_clear_day(PLATFORMS).expect("clear day");
    let mut sensing = Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Default::default() };
    sensing.weather = Some(murmur_weather::Config {
        archive: None,
        archive_sha256: None,
        timeline: [(0.0, 50.0), (100.0, 50.0), (101.0, 20_000.0)]
            .into_iter()
            .map(|(at_s, v)| murmur_weather::Frame { at_s, values: murmur_weather::Values { visibility_m: Some(v), ..Default::default() } })
            .collect(),
    });
    let msg = |s: f64| TruthMessage {
        t: T + s,
        s,
        seq: s as u64,
        scenario: "test".into(),
        seed: 1,
        sensing: sensing.clone(),
        contacts: Vec::new(),
        acts: Vec::new(),
        aim_at: Vec::new(),
        weapons: Vec::new(),
        impacts: Vec::new(), mines_live: Vec::new(), look: Vec::new(),
    };
    let now = crate::clock::wall();
    assert_eq!(tracks.reach_json(&world, &super::super::ReachOf::All, now)["available"], false, "before the first sensing step");
    tracks.on_truth_with(msg(10.0), &[], now).expect("truth");
    let v = tracks.reach_json(&world, &super::super::ReachOf::uid(1), now);
    assert_eq!(v["model"], "physical");
    let puma = &v["vehicles"][0];
    assert_eq!(puma["callsign"], "PUMA 1");
    assert_eq!(puma["conditions"]["visibility_m"], 50.0);
    assert_eq!(puma["conditions"]["synthetic"], true);
    assert_eq!(puma["conditions"]["light"], "day");
    let eo = &puma["sensors"][0];
    assert_eq!(eo["type"], "eo_ir");
    assert_eq!(eo["primary_domain"], "surface");
    let surface = eo["domains"].as_array().expect("present").iter().find(|d| d["domain"] == "surface").expect("present");
    assert!(surface["ratio"].as_f64().expect("present") < 0.2, "{surface}");
    assert_eq!(surface["limited_by"], "fog");
    assert_eq!(surface["clear_day_m"], 14_470.0);
    // bd murmur-jvdc: only the vehicles asked for, by uid or within a box
    let (lat, lon) = (BASE.0, BASE.1);
    let near = super::super::ReachOf::Within([lon - 0.01, lat - 0.01, lon + 0.01, lat + 0.01]);
    let far = super::super::ReachOf::Within([lon + 1.0, lat + 1.0, lon + 1.1, lat + 1.1]);
    assert_eq!(tracks.reach_json(&world, &near, now)["vehicles"].as_array().map(Vec::len), Some(1));
    assert_eq!(tracks.reach_json(&world, &far, now)["vehicles"].as_array().map(Vec::len), Some(0));
    assert_eq!(tracks.reach_json(&world, &super::super::ReachOf::uid(2), now)["vehicles"].as_array().map(Vec::len), Some(0));
    // kept while nothing moved
    let again = tracks.reach_json(&world, &super::super::ReachOf::uid(1), now + 1.0);
    assert_eq!(again["vehicles"][0]["age_s"], 1.0);
    assert_eq!(again["vehicles"][0]["sensors"], puma["sensors"]);
    // the fog clears: computed again at once
    tracks.on_truth_with(msg(120.0), &[], now + 2.0).expect("truth");
    let clear = tracks.reach_json(&world, &super::super::ReachOf::All, now + 2.0);
    let c = &clear["vehicles"][0];
    assert_eq!(c["age_s"], 0.0);
    assert_eq!(c["conditions"]["visibility_m"], 20_000.0);
    let s = c["sensors"][0]["domains"].as_array().expect("present").iter().find(|d| d["domain"] == "surface").expect("present");
    assert!(s["ratio"].as_f64().expect("present") > 0.8, "{s}");
    // nothing of the truth or the weather's schedule in it
    let text = clear.to_string();
    assert!(!text.contains("timeline") && !text.contains("at_s") && !text.contains("contacts"), "{text}");
}

/// What the endpoint costs at 100 vehicles (every catalog platform with a reach in turn, in fog,
/// rough sea and twilight, so every counterfactual is computed): a call that computes every
/// vehicle's answer, and one that reads them back. Run once in release for the number
/// docs/WEATHER.md quotes:
/// `cargo test --release -p murmur-bridge --bin murmur-bridge reach_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn reach_cost_at_100_vehicles() {
    use super::super::truth::{Sensing, TruthMessage};
    let world = test_world();
    let table: toml::Table = PLATFORMS.parse().expect("toml");
    let mut platforms: Vec<String> = clear_day().keys().cloned().collect();
    platforms.sort();
    for i in 0..100u64 {
        let id = &platforms[i as usize % platforms.len()];
        let p = table.get(id).and_then(|p| p.as_table()).expect("platform");
        let num = |k: &str| p.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
        let alt = num("alt_m").or_else(|| num("typical_alt_m")).unwrap_or(0.0);
        let domain = p.get("domain").and_then(|v| v.as_str()).unwrap_or("surface");
        add_vehicle(&world, i + 1, id, domain, alt, (BASE.0 + 0.001 * i as f64, BASE.1));
    }
    let settings = Settings { seed: Some(1), misclassification_rate: Some(0.0), eval_truth: false, drop_s: 180.0, model: Some(SensingModel::Physical), sim_token: None };
    let tracks = Tracks::new(catalog(), settings).with_clear_day(PLATFORMS).expect("clear day");
    let cond = Conditions { visibility_m: 300.0, wind_mps: 12.0, sea_state: 5.0, sun_elevation_deg: Some(-3.0), ..Conditions::default() };
    let sensing = Sensing { conditions: Some(cond), ..Default::default() };
    let msg = TruthMessage { t: T, s: 1.0, seq: 1, scenario: "test".into(), seed: 1, sensing, contacts: Vec::new(), acts: Vec::new(), aim_at: Vec::new(), weapons: Vec::new(), impacts: Vec::new(), mines_live: Vec::new(), look: Vec::new() };
    let now = crate::clock::wall();
    tracks.on_truth_with(msg, &[], now).expect("truth");
    let t0 = std::time::Instant::now();
    let cold = tracks.reach_json(&world, &super::super::ReachOf::All, now);
    let cold_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = std::time::Instant::now();
    let warm = tracks.reach_json(&world, &super::super::ReachOf::All, now + 1.0);
    let warm_ms = t1.elapsed().as_secs_f64() * 1e3;
    let n = cold["vehicles"].as_array().map_or(0, Vec::len);
    assert_eq!(n, 100, "{n} vehicles answered");
    let sensors: usize = cold["vehicles"].as_array().expect("present").iter().map(|v| v["sensors"].as_array().map_or(0, Vec::len)).sum();
    eprintln!("reach at {n} vehicles ({sensors} sensors): {cold_ms:.1} ms computing every answer, {warm_ms:.2} ms from the cache ({} bytes)", warm.to_string().len());
}

/// Rules 32 (bd murmur-ahss): the decision fact `reach_now` is the display's computation for the
/// same inputs, rounded so that vehicles alike share it: each detection reach the display's along
/// the ground, an EO/IR's identification reach by domain and by size class as slant range (never
/// nearer than the ground's), and on the reference day at the operating height the catalog's
/// detection reach; in fog it shrinks, and a camera without a thermal channel reaches nothing at
/// night where one with it does.
#[test]
fn the_decision_fact_is_the_displays_computation() {
    // the inputs, rounded
    let c = Conditions { visibility_m: 16_345.0, wind_mps: 5.3, ..reference_conditions() };
    let i = reach::Inputs::of("puma", 147.2, false, &c, 12.26);
    let rc = i.conditions();
    assert_eq!((i.alt_m(), rc.visibility_m, rc.wind_mps, rc.sun_elevation_deg), (150.0, 16_000.0, 5.5, Some(12.5)));
    assert_eq!(reach::Inputs::of("puma", 7_640.0, false, &c, 45.0).alt_m(), 7_600.0);
    assert_eq!(reach::Inputs::of("ugv_armed", 29.4, false, &c, 45.0).alt_m(), 29.0);
    assert_eq!(reach::Inputs::of("puma", 150.4, false, &c, 12.3), reach::Inputs::of("puma", 149.8, false, &c, 12.4), "one computation");
    let cat = catalog();
    let clear = clear_day();
    let mut checked = 0;
    for (platform, by_kind) in &clear {
        let (o, own) = at_operating(platform, &cat);
        let inputs = reach::Inputs::of(platform, o.alt_msl, o.submerged, &reference_conditions(), 45.0);
        let vr = reach::vehicle_reach(&inputs, &o.sensors, o.mast_m, &own, Some(by_kind), T);
        assert_eq!(vr.fact.sensors.len(), vr.sensors.len(), "{platform}");
        for (f, s) in vr.fact.sensors.iter().zip(&vr.sensors) {
            assert_eq!(f.kind, s.kind);
            for d in &s.domains {
                assert_eq!(f.detect_m.get(d.domain), Some(d.detect_m.round()), "{platform} {} {:?}", s.kind, d.domain);
                if inputs.alt_m() == o.alt_msl
                    && let Some(want) = d.clear_day_m
                {
                    // within the catalog's 10 m (at hundreds of kilometres the place's own
                    // geometry moves it a few hundredths of a percent)
                    let near = 10.0f64.max(1e-4 * want);
                    assert!((d.detect_m - want).abs() <= near, "{platform} {} {:?}: {} m, the catalog {want} m", s.kind, d.domain, d.detect_m);
                }
                if s.kind == "eo_ir" {
                    let slant = f.identify_m.as_ref().and_then(|r| r.get(d.domain)).expect("an identification reach");
                    // the model's own slant range (its geometry measures the line a tenth of a
                    // percent shorter than the offset it was laid out by)
                    assert!(slant >= 0.998 * d.identify_m - 1.0, "{platform} {:?}: slant {slant} m, ground {} m", d.domain, d.identify_m);
                    checked += 1;
                }
            }
            assert_eq!(f.identify_m.is_some(), s.kind == "eo_ir", "{platform} {}", s.kind);
            if s.kind == "eo_ir" {
                let by = f.identify_m.as_ref().and_then(|r| r.size_class.as_ref()).expect("reaches by class");
                for k in &s.classes {
                    assert_eq!(by.get(k.class), Some(k.identify_slant_m.round()), "{platform} {:?}", k.class);
                }
            }
        }
    }
    assert!(checked > 30, "{checked} camera domains");
    // fog shrinks a camera; at night a visible channel alone reaches nothing, a thermal one does
    let fact = |platform: &str, alt: f64, cond: Conditions, sun: f64| {
        let (o, own) = at_operating(platform, &cat);
        let inputs = reach::Inputs::of(platform, alt, false, &cond, sun);
        reach::vehicle_reach(&inputs, &o.sensors, o.mast_m, &own, clear.get(platform), T).fact
    };
    let eo = |f: &murmur_decide::ReachNow| f.sensors.iter().find(|s| s.kind == "eo_ir").cloned().expect("a camera");
    let foggy = eo(&fact("puma", 150.0, Conditions { visibility_m: 50.0, ..reference_conditions() }, 30.0));
    assert!(foggy.detect_m.surface.expect("surface") < 0.05 * 14_470.0, "{:?}", foggy.detect_m);
    let magura = eo(&fact("magura", 0.0, reference_conditions(), -20.0));
    assert!(!magura.thermal);
    assert_eq!(magura.detect_m.surface, Some(0.0));
    assert_eq!(magura.identify_m.as_ref().and_then(|r| r.size_class.as_ref()).and_then(|c| c.small_surface), Some(0.0));
    let puma = eo(&fact("puma", 150.0, reference_conditions(), -20.0));
    assert!(puma.thermal);
    assert!(puma.identify_m.as_ref().and_then(|r| r.size_class.as_ref()).and_then(|c| c.small_surface).expect("present") > 1_000.0);
}

/// The figures the engage scenarios that run on rules 32 give their vehicles' reach now (146 to
/// 148's `reach_now`, and 144 and 145's), as the bridge computes them for the same inputs:
/// `cargo test --release -p murmur-bridge --bin murmur-bridge scenario_reach_figures -- --ignored --nocapture`.
#[test]
#[ignore]
fn scenario_reach_figures() {
    let cat = catalog();
    let clear = clear_day();
    for (platform, alt, visibility_m, sun) in [
        ("puma", 150.0, 50.0, 30.0),
        ("puma", 150.0, 1_200.0, 30.0),
        ("puma", 150.0, 20_000.0, -20.0),
        ("puma", 150.0, 20_000.0, 45.0),
        ("magura", 0.0, 20_000.0, -20.0),
        ("ugv_armed", 20.0, 50.0, 30.0),
        ("x10", 100.0, 20_000.0, 45.0),
        ("mq9b", 7_600.0, 20_000.0, 45.0),
    ] {
        let (o, own) = at_operating(platform, &cat);
        let cond = Conditions { visibility_m, ..reference_conditions() };
        let inputs = reach::Inputs::of(platform, alt, false, &cond, sun);
        let vr = reach::vehicle_reach(&inputs, &o.sensors, o.mast_m, &own, clear.get(platform), T);
        eprintln!("{platform} {alt} m, {visibility_m} m visibility, the sun {sun} degrees: {}", serde_json::to_string(&vr.fact).expect("json"));
    }
}
