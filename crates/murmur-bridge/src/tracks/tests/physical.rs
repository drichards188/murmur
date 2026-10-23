//! The physical sensing model against the published figures and its own properties
//! (WORLD-V3-PLAN 2.8 items 1 and 2): the EO/IR anchors (FLIR M364C and EDGE G270 detection
//! ranges within the research's 40 %, the MX-20 and Mantis i45 D/R/I geometry of the catalog's
//! channels), the IMO anchors on the catalog's navigation radar; nothing seen beyond the
//! horizon, behind terrain or outside a camera's field; identification falling with range; no
//! day camera at night; sea state 5 losing a small boat and keeping a ferry. The radar, AIS
//! and sonar anchors are the modules' own tests (`physical::{radar,rf,sonar}`).

use super::super::model::Track;
use super::super::physical::conditions::Conditions;
use super::super::physical::eo::{self, P50_PX, range_at};
use super::super::physical::geometry::{Grid, Header, K_RADIO, visible_height_m};
use super::super::physical::predict::{self, Ask};
use super::super::physical::spec::{Band, EoChannel, Resolved, resolve};
use super::super::physical::{Engine, Step, radar};
use super::super::sensing::{Detection, Level, Look, Observer, SearchArea, SensorSpec, SensorType};
use super::super::truth::{ContactTruth, Domain, Sensing};
use super::{BASE, at, catalog, contact};

const NM: f64 = 1852.0;

fn day() -> Sensing {
    Sensing { conditions: Some(Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() }), ..Sensing::default() }
}

fn with(c: Conditions) -> Sensing {
    Sensing { conditions: Some(c), ..Sensing::default() }
}

fn sensors_of(platform: &str) -> Vec<SensorSpec> {
    catalog().get(platform).expect(platform).sensors.clone()
}

fn eo_channels(platform: &str) -> Vec<EoChannel> {
    let s = sensors_of(platform).into_iter().find(|s| s.kind == SensorType::EoIr).expect("an EO/IR sensor");
    match resolve(&s, true) {
        Resolved::EoIr(e) => {
            assert!(!e.placeholder, "{platform}'s camera has its own figures");
            e.channels
        }
        other => panic!("{other:?}"),
    }
}

/// The geometric D/R/I ranges of a channel's narrow field against a critical dimension, km:
/// where the pixels on target meet the 50 % thresholds (the research's platform table).
fn geometric_km(ch: &EoChannel, dc: f64) -> [f64; 3] {
    P50_PX.map(|p50| dc / (p50 * ch.ifov(true)) / 1000.0)
}

fn close(got: f64, want: f64, tol: f64) -> bool {
    (got / want - 1.0).abs() <= tol
}

/// The 50 % range of one channel (no zoom) against a target in clear daylight, km.
fn r50_km(ch: &EoChannel, dc: f64, dt: f64, level: usize) -> f64 {
    let c = Conditions::default();
    range_at(0.5, |r| eo::channel_p(ch, false, r, [0.0; 2], dc, dt, &c, 45.0)[level]) / 1000.0
}

/// FLIR M364C LR (640 px, 18 degrees; [13]): a person detected at 0.97 km and the NATO 2.3 m
/// target at 2.34 km; EDGE G270 (uncooled, 7.3 degrees; [14]): a vehicle detected at 5.5 km and
/// recognised at 1.8. Within 40 %, the research's own spread (WORLD-V3-PLAN 2.8).
#[test]
fn the_thermal_camera_anchors_reproduce_within_40_percent() {
    let lwir = |fov: f64| EoChannel { band: Band::Lwir, wide_rad: fov.to_radians(), narrow_rad: fov.to_radians(), px: 640.0, blur_rad: 0.0, panoramic: false };
    let m364 = lwir(18.0);
    for (what, dc, dt, level, km) in [("M364C person D", 0.75, 5.0, 0, 0.97), ("M364C NATO target D", 2.3, 2.0, 0, 2.34)] {
        let got = r50_km(&m364, dc, dt, level);
        assert!(close(got, km, 0.4), "{what}: {got:.2} km against {km}");
    }
    let g270 = lwir(7.3);
    for (what, level, km) in [("G270 vehicle D", 0, 5.5), ("G270 vehicle R", 1, 1.8)] {
        let got = r50_km(&g270, 2.3, 2.0, level);
        assert!(close(got, km, 0.4), "{what}: {got:.2} km against {km}");
    }
}

/// The catalog's MX-20 (MQ-9B) and Mantis i45 (Puma) give the research's D/R/I geometry
/// ([3], [37], [38]): MX-20 MWIR vehicle 77/19/9.6 km, person 25/6.3/3.1; Mantis LWIR at 5.3
/// degrees vehicle 10.6/2.6/1.3, person 3.5/0.9/0.4; Mantis EO vehicle identification about
/// 7.7 km.
#[test]
fn the_mx20_and_mantis_i45_geometry_reproduces() {
    let mq9 = eo_channels("mq9b");
    let mwir = mq9.iter().find(|c| c.band == Band::Mwir).expect("MWIR");
    for (dc, want) in [(2.3, [77.0, 19.0, 9.6]), (0.75, [25.0, 6.3, 3.1])] {
        let got = geometric_km(mwir, dc);
        for k in 0..3 {
            assert!(close(got[k], want[k], 0.05), "MX-20 MWIR {dc} m level {k}: {:.2} km against {}", got[k], want[k]);
        }
    }
    let puma = eo_channels("puma");
    let lw = puma.iter().find(|c| c.band == Band::Lwir).expect("LWIR");
    for (dc, want) in [(2.3, [10.6, 2.6, 1.3]), (0.75, [3.5, 0.9, 0.4])] {
        let got = geometric_km(lw, dc);
        for k in 0..3 {
            assert!(close(got[k], want[k], 0.1), "Mantis LWIR {dc} m level {k}: {:.2} km against {}", got[k], want[k]);
        }
    }
    let eo = puma.iter().find(|c| c.band == Band::Day).expect("EO");
    let i = geometric_km(eo, 2.3)[2];
    assert!(close(i, 7.7, 0.05), "Mantis EO vehicle I {i:.2} km against 7.7");
    // the atmosphere caps the MWIR's long geometric ranges (research: about 15-25 km)
    let capped = r50_km(&EoChannel { narrow_rad: mwir.narrow_rad, ..mwir.clone() }, 2.3, 2.0, 0);
    let narrow = EoChannel { wide_rad: mwir.narrow_rad, ..mwir.clone() };
    let d = range_at(0.5, |r| eo::channel_p(&narrow, false, r, [0.0; 2], 2.3, 2.0, &Conditions::default(), 45.0)[0]) / 1000.0;
    assert!(d < 25.0 && capped < 25.0, "{d:.1} {capped:.1}");
}

/// The catalog's navigation radar (Sea Hunter's) reproduces IMO MSC.192(79) Table 2 from a
/// 15 m antenna within 15 % (the radar module's test checks the same anchors on a radar it
/// builds; this one checks the catalog says that radar).
#[test]
fn the_catalogs_navigation_radar_reproduces_the_imo_anchors() {
    let s = sensors_of("seahunter").into_iter().find(|s| s.kind == SensorType::Radar).expect("radar");
    let Resolved::Radar(r) = resolve(&s, false) else { panic!("a radar") };
    assert!(!r.placeholder && r.scan_s <= 5.0);
    for (rcs, h, nm) in [(10_000.0, 10.0, 11.0), (1_000.0, 5.0, 8.0), (7.5, 4.0, 5.0), (5.0, 3.5, 4.6), (2.5, 2.0, 3.4)] {
        let got = range_at(0.8, |g| radar::pd(&r, rcs, g, g, 15.0, visible_height_m(h, h, g, 15.0, K_RADIO), None)) / NM;
        assert!(close(got, nm, 0.15), "{rcs} m^2, {h} m: {got:.2} NM against {nm}");
    }
}

/// Every anchor, computed against published (`cargo test -p murmur-bridge anchor_table --
/// --ignored --nocapture`; docs/SENSING.md quotes it).
#[test]
#[ignore]
fn anchor_table() {
    let lwir = |fov: f64| EoChannel { band: Band::Lwir, wide_rad: fov.to_radians(), narrow_rad: fov.to_radians(), px: 640.0, blur_rad: 0.0, panoramic: false };
    eprintln!("EO/IR detection ranges, km (clear day; computed / published)");
    for (what, ch, dc, dt, level, km) in [
        ("M364C person D", lwir(18.0), 0.75, 5.0, 0, 0.97),
        ("M364C NATO 2.3 m D", lwir(18.0), 2.3, 2.0, 0, 2.34),
        ("G270 vehicle D", lwir(7.3), 2.3, 2.0, 0, 5.5),
        ("G270 vehicle R", lwir(7.3), 2.3, 2.0, 1, 1.8),
    ] {
        eprintln!("  {what:<22} {:6.2} / {km:5.2}", r50_km(&ch, dc, dt, level));
    }
    let mq9 = eo_channels("mq9b");
    let mwir = mq9.iter().find(|c| c.band == Band::Mwir).expect("MWIR");
    let puma = eo_channels("puma");
    let lw = puma.iter().find(|c| c.band == Band::Lwir).expect("LWIR");
    let day = puma.iter().find(|c| c.band == Band::Day).expect("EO");
    eprintln!("D/R/I geometry, km (computed / research table)");
    for (what, ch, dc, want) in [
        ("MX-20 MWIR vehicle", mwir, 2.3, [77.0, 19.0, 9.6]),
        ("MX-20 MWIR person", mwir, 0.75, [25.0, 6.3, 3.1]),
        ("Mantis LWIR vehicle", lw, 2.3, [10.6, 2.6, 1.3]),
        ("Mantis LWIR person", lw, 0.75, [3.5, 0.9, 0.4]),
        ("Mantis EO vehicle", day, 2.3, [f64::NAN, f64::NAN, 7.7]),
    ] {
        let g = geometric_km(ch, dc);
        eprintln!("  {what:<22} {:5.1}/{:5.1}/{:5.1}  {:5.1}/{:5.1}/{:5.1}", g[0], g[1], g[2], want[0], want[1], want[2]);
    }
    let s = sensors_of("seahunter").into_iter().find(|s| s.kind == SensorType::Radar).expect("radar");
    let Resolved::Radar(r) = resolve(&s, false) else { panic!("a radar") };
    eprintln!("IMO Table 2, Pd 0.8 from 15 m, NM (computed / IMO)");
    for (what, rcs, h, nm) in [("ship > 5,000 GT", 10_000.0, 10.0, 11.0), ("ship > 500 GT", 1_000.0, 5.0, 8.0), ("small vessel, reflector", 7.5, 4.0, 5.0), ("navigation buoy", 5.0, 3.5, 4.6), ("10 m vessel", 2.5, 2.0, 3.4)] {
        let got = range_at(0.8, |g| radar::pd(&r, rcs, g, g, 15.0, visible_height_m(h, h, g, 15.0, K_RADIO), None)) / NM;
        eprintln!("  {what:<24} {got:5.2} / {nm:4.1}");
    }
}

/// W2: the catalog refuses a radar that revisits slower than every 5 s.
#[test]
fn a_radar_revisiting_slower_than_5_s_is_refused() {
    let bad = "[x]\ndomain = \"surface\"\nsensors = [{ type = \"radar\", range_m = 1000, scan_s = 6 }]\n";
    let e = super::super::sensing::SensorCatalog::from_toml(bad).expect_err("refused");
    assert!(e.contains("W2"), "{e}");
    let ok = "[x]\ndomain = \"surface\"\nsensors = [{ type = \"radar\", range_m = 1000, scan_s = 5 }]\n";
    assert!(super::super::sensing::SensorCatalog::from_toml(ok).is_ok());
}

/// Every platform's every sensor resolves to a working physical sensor (none on placeholders:
/// `catalog::no_catalog_sensor_runs_on_placeholders`).
#[test]
fn every_catalog_sensor_resolves() {
    let cat = catalog();
    for (id, p) in &cat.platforms {
        for s in &p.sensors {
            match resolve(s, false) {
                Resolved::EoIr(e) => assert!(!e.channels.is_empty() && e.channels.iter().all(|c| c.ifov(true) > 0.0), "{id}"),
                Resolved::Radar(r) => assert!(r.r50_m > 0.0 && r.scan_s <= 5.0, "{id}"),
                Resolved::Sonar(sn) => assert!(!sn.ranges.is_empty(), "{id}"),
                Resolved::Esm(e) => assert!(e.s_min_dbm < 0.0, "{id}"),
                Resolved::Ais => {}
            }
        }
    }
}

fn usv(uid: u64, lat: f64, lon: f64, sensors: Vec<SensorSpec>) -> Observer {
    Observer { uid, lat, lon, alt_msl: 0.0, mast_m: 10.0, sensors, ..Observer::default() }
}

/// `n` ticks of the engine from second `from`; every detection.
fn run(e: &mut Engine, sensing: &Sensing, obs: &[Observer], contacts: &[ContactTruth], from: i64, n: i64) -> Vec<Detection> {
    let mut out = Vec::new();
    for s in from..from + n {
        let st = Step { seed: 7, tick: s, s: s as f64, t: 1_781_467_800.0 + s as f64, misclassification_rate: 0.0, sensing };
        out.extend(e.step(&st, obs, contacts));
    }
    out
}

fn radar_only() -> Vec<SensorSpec> {
    sensors_of("seahunter").into_iter().filter(|s| s.kind == SensorType::Radar).collect()
}

/// Nothing beyond the horizon: a 2 m boat is under a 10 m mast's radio horizon past 18.8 km
/// (4.12 (sqrt 10 + sqrt 2)); a 25 m cargo ship at the same 20 km still shows.
#[test]
fn nothing_is_seen_beyond_its_horizon() {
    let o = usv(1, BASE.0, BASE.1, radar_only());
    let sensing = day();
    let far = |kind: &str, north: f64| {
        let (lat, lon) = at(BASE.0, BASE.1, 0.0, north);
        contact(kind, kind, Domain::Surface, lat, lon)
    };
    let mut e = Engine::new(None);
    assert!(run(&mut e, &sensing, std::slice::from_ref(&o), &[far("small_fast_boat", 20_000.0)], 0, 120).is_empty());
    let mut e = Engine::new(None);
    assert!(!run(&mut e, &sensing, std::slice::from_ref(&o), &[far("small_fast_boat", 4_000.0)], 0, 60).is_empty());
    let mut e = Engine::new(None);
    assert!(!run(&mut e, &sensing, std::slice::from_ref(&o), &[far("cargo_ship", 20_000.0)], 0, 60).is_empty());
}

/// Nothing behind terrain: a 100 m ridge between a USV and a ferry 6 km off hides it from radar
/// and camera; without it, both see it.
#[test]
fn nothing_is_seen_behind_terrain() {
    let h = Header { lat0: BASE.0 - 0.02, lon0: BASE.1 - 0.02, dlat: 0.0002, dlon: 0.0002, nlat: 600, nlon: 200, scale: 0.1, nodata: -32768 };
    let flat = vec![0.0; 600 * 200];
    let mut ridge = flat.clone();
    for i in 200..210 {
        for j in 0..200 {
            ridge[i * 200 + j] = 100.0;
        }
    }
    let mut sensors = radar_only();
    sensors.extend(sensors_of("magura").into_iter().filter(|s| s.kind == SensorType::EoIr));
    let o = usv(1, BASE.0, BASE.1, sensors);
    let (lat, lon) = at(BASE.0, BASE.1, 0.0, 6_000.0);
    let ferry = contact("ferry", "ferry", Domain::Surface, lat, lon);
    let sensors_seeing = |grid: &[f64]| {
        let mut e = Engine::new(None);
        e.set_terrain(Grid::from_heights(h.clone(), grid));
        let dets = run(&mut e, &day(), std::slice::from_ref(&o), std::slice::from_ref(&ferry), 0, 200);
        dets.iter().map(|d| d.sensor).collect::<std::collections::BTreeSet<_>>()
    };
    assert!(sensors_seeing(&ridge).is_empty());
    let open = sensors_seeing(&flat);
    assert!(open.contains(&SensorType::Radar) && open.contains(&SensorType::EoIr), "{open:?}");
}

/// A camera sees nothing outside its field: a fixed camera looking north (a vehicle heading
/// north with a camera that cannot slew) never sees a boat to its south, and does see one to
/// its north.
#[test]
fn a_camera_sees_nothing_outside_its_field() {
    let fixed: SensorSpec = toml::from_str::<toml::Table>(
        "s = { type = \"eo_ir\", range_m = 3000, slew_dps = 0, channels = [{ band = \"day\", wide_deg = 60, px = 1920 }] }",
    )
    .expect("toml")["s"]
        .clone()
        .try_into()
        .expect("spec");
    let o = Observer { heading_deg: Some(0.0), ..usv(1, BASE.0, BASE.1, vec![fixed]) };
    let boat = |north: f64| {
        let (lat, lon) = at(BASE.0, BASE.1, 0.0, north);
        contact("b", "small_fast_boat", Domain::Surface, lat, lon)
    };
    let mut e = Engine::new(None);
    assert!(run(&mut e, &day(), std::slice::from_ref(&o), &[boat(-800.0)], 0, 60).is_empty());
    let mut e = Engine::new(None);
    assert!(!run(&mut e, &day(), std::slice::from_ref(&o), &[boat(800.0)], 0, 60).is_empty());
}

fn puma_at(lat: f64, lon: f64) -> Observer {
    Observer { uid: 5, lat, lon, alt_msl: 120.0, mast_m: 0.0, sensors: sensors_of("puma"), ..Observer::default() }
}

fn ask<'a>(spec: &'a SensorSpec, o: &'a Observer, c: &'a ContactTruth, cond: Conditions) -> Ask<'a> {
    Ask { spec, observer: o, target: c, over: None, conditions: cond, t: 1_781_467_800.0, terrain: None, bathymetry: None, ais_load_per_min: 0.0 }
}

/// Identification falls with range: the Puma's camera pointed at a small fast boat, by day,
/// identifies it less and less likely the further it is (and recognition outlasts it).
#[test]
fn identification_falls_with_range() {
    let o = puma_at(BASE.0, BASE.1);
    let spec = &o.sensors[0];
    let cond = Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() };
    let mut last = 1.0;
    for d in [500.0, 2_000.0, 4_000.0, 8_000.0, 12_000.0, 20_000.0] {
        let (lat, lon) = at(BASE.0, BASE.1, d, 0.0);
        let c = contact("b", "small_fast_boat", Domain::Surface, lat, lon);
        let p = predict::predict(&ask(spec, &o, &c, cond));
        assert!(p.p_identify <= last && p.p_identify <= p.p_recognise + 1e-9, "{d}: {p:?}");
        last = p.p_identify;
    }
    assert!(last < 0.1, "{last}");
    let (lat, lon) = at(BASE.0, BASE.1, 1_000.0, 0.0);
    let c = contact("b", "small_fast_boat", Domain::Surface, lat, lon);
    let r = predict::ranges(&ask(spec, &o, &c, cond));
    assert!(r[0] > r[1] && r[1] > r[2] && r[2] > 1_000.0, "{r:?}");
}

/// Night removes the day camera's detections; a thermal camera still sees.
#[test]
fn night_removes_day_camera_detections() {
    let o = puma_at(BASE.0, BASE.1);
    let spec = &o.sensors[0];
    let (lat, lon) = at(BASE.0, BASE.1, 1_500.0, 0.0);
    let c = contact("b", "small_fast_boat", Domain::Surface, lat, lon);
    let day_only: SensorSpec = {
        let mut s = spec.clone();
        s.physical.channels.retain(|ch| ch.band == super::super::physical::spec::Band::Day);
        s
    };
    let night = Conditions { sun_elevation_deg: Some(-30.0), ..Conditions::default() };
    let noon = Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() };
    assert_eq!(predict::predict(&ask(&day_only, &o, &c, night)).p_detect, 0.0);
    assert!(predict::predict(&ask(&day_only, &o, &c, noon)).p_detect > 0.9);
    assert!(predict::predict(&ask(spec, &o, &c, night)).p_detect > 0.5);
    // and the running model: a day-only camera over a boat at night detects nothing
    let night_only = Observer { sensors: vec![day_only.clone()], ..o.clone() };
    let mut e = Engine::new(None);
    assert!(run(&mut e, &with(night), std::slice::from_ref(&night_only), std::slice::from_ref(&c), 0, 120).is_empty());
    let mut e = Engine::new(None);
    assert!(!run(&mut e, &with(noon), std::slice::from_ref(&night_only), std::slice::from_ref(&c), 0, 120).is_empty());
}

/// Sea state 5 loses a small boat to a navigation radar at 5 km and keeps a ferry.
#[test]
fn sea_state_5_loses_a_small_boat_to_a_navigation_radar_and_keeps_a_ferry() {
    let o = usv(1, BASE.0, BASE.1, radar_only());
    let (lat, lon) = at(BASE.0, BASE.1, 0.0, 5_000.0);
    let boat = contact("boat", "small_fast_boat", Domain::Surface, lat, lon);
    let ferry = contact("ferry", "ferry", Domain::Surface, lat + 0.0005, lon);
    let seen = |ss: f64, c: &ContactTruth| {
        let mut e = Engine::new(None);
        let sensing = with(Conditions { sea_state: ss, ..Conditions::default() });
        run(&mut e, &sensing, std::slice::from_ref(&o), std::slice::from_ref(c), 0, 100).len()
    };
    assert!(seen(3.0, &boat) > 10, "{}", seen(3.0, &boat));
    assert_eq!(seen(5.0, &boat), 0);
    assert!(seen(5.0, &ferry) > 30, "{}", seen(5.0, &ferry));
}

/// The payload manager (decision W1): a Puma orbiting a point, a small fast boat in its area
/// that a USV's radar holds; its camera searches, is cued by the radar's track, locks on in the
/// narrow field and identifies it, all without an order naming it.
#[test]
fn the_payload_manager_cues_to_a_radar_track_and_identifies_it() {
    let center = at(BASE.0, BASE.1, 0.0, 2_000.0);
    let puma = Observer {
        heading_deg: Some(90.0),
        area: Some(SearchArea::Orbit { center, radius_m: 1_500.0 }),
        ..puma_at(BASE.0, BASE.1)
    };
    let ship = usv(1, BASE.0 - 0.01, BASE.1, radar_only());
    let (lat, lon) = at(center.0, center.1, 700.0, 300.0);
    let boat = contact("boat", "small_fast_boat", Domain::Surface, lat, lon);
    let mut e = Engine::new(None);
    let dets = run(&mut e, &day(), &[ship, puma], std::slice::from_ref(&boat), 0, 120);
    let id = dets.iter().find(|d| d.uid == 5 && d.look.is_some_and(|l| l.level == Level::Identify)).expect("identified");
    assert_eq!(id.label.as_deref(), Some("small_fast_boat"));
    assert!(dets.iter().any(|d| d.uid == 1 && d.sensor == SensorType::Radar));
}

/// A placeholder sensor of `kind` reaching `range_m` (no catalog figures: B1 changes those).
fn placeholder(kind: &str, range_m: f64) -> SensorSpec {
    toml::from_str::<toml::Table>(&format!("s = {{ type = \"{kind}\", range_m = {range_m} }}")).expect("toml")["s"].clone().try_into().expect("spec")
}

/// `n` ticks of the engine with the contact moved each second by `motion(s)`: every detection.
fn run_moving(e: &mut Engine, obs: &[Observer], n: i64, motion: impl Fn(i64) -> ContactTruth) -> Vec<Detection> {
    let sensing = day();
    let mut out = Vec::new();
    for s in 0..n {
        let st = Step { seed: 7, tick: s, s: s as f64, t: 1_781_467_800.0 + s as f64, misclassification_rate: 0.0, sensing: &sensing };
        out.extend(e.step(&st, obs, &[motion(s)]));
    }
    out
}

/// A 2D radar's plot of an aircraft carries no elevation (murmur-7vp): a USV's camera cued by
/// its own navigation radar to a light aircraft 1,500 m up and 5 km out, 17 degrees above the
/// sea where the plot is (the wide field reaches 6 degrees either side of its middle), searches
/// in elevation, acquires it and, its image tracker following it across the sky at 50 m/s,
/// identifies it.
#[test]
fn a_camera_cued_by_a_2d_radar_finds_an_aircraft_in_elevation_and_identifies_it() {
    let o = usv(1, BASE.0, BASE.1, vec![placeholder("radar", 40_000.0), placeholder("eo_ir", 15_000.0)]);
    let plane = |s: i64| {
        let (lat, lon) = at(BASE.0, BASE.1, -3_000.0 + 50.0 * s as f64, 5_000.0);
        let mut c = contact("plane", "light_aircraft", Domain::Air, lat, lon);
        (c.alt_m, c.speed_mps, c.course_deg) = (1_500.0, 50.0, 90.0);
        c
    };
    let dets = run_moving(&mut Engine::new(None), std::slice::from_ref(&o), 120, plane);
    let radar = dets.iter().find(|d| d.sensor == SensorType::Radar).expect("the radar holds it");
    assert!(radar.slant_from.is_some() && radar.alt_m.is_none(), "{radar:?}");
    let eo: Vec<&Detection> = dets.iter().filter(|d| d.sensor == SensorType::EoIr).collect();
    assert!(eo.iter().any(|d| d.look.is_some_and(|l| l.level == Level::Identify) && d.label.as_deref() == Some("light_aircraft")), "{eo:?}");
    // never an altitude from the camera: the tracker's facts stay 2D
    assert!(eo.iter().all(|d| d.alt_m.is_none()));
}

/// The image tracker keeps a locked contact (murmur-7vp): a camera 120 m up locks on a small
/// boat crossing its view at 12 m/s, 1.5 km off, and identifies it; the gimbal follows its line
/// of sight in closed loop rather than slewing and settling again each second.
#[test]
fn a_locked_camera_follows_a_moving_boat_and_identifies_it() {
    let o = Observer { uid: 5, lat: BASE.0, lon: BASE.1, alt_msl: 120.0, mast_m: 0.0, sensors: vec![placeholder("eo_ir", 3_000.0)], ..Observer::default() };
    let boat = |s: i64| {
        let (lat, lon) = at(BASE.0, BASE.1, -600.0 + 12.0 * s as f64, 1_500.0);
        let mut c = contact("boat", "small_fast_boat", Domain::Surface, lat, lon);
        (c.speed_mps, c.course_deg) = (12.0, 90.0);
        c
    };
    let dets = run_moving(&mut Engine::new(None), std::slice::from_ref(&o), 120, boat);
    assert!(dets.iter().any(|d| d.look.is_some_and(|l| l.level == Level::Identify)), "{dets:?}");
}

/// Deterministic: the same seed and inputs give the same detections.
#[test]
fn the_physical_model_is_deterministic() {
    let center = at(BASE.0, BASE.1, 0.0, 2_000.0);
    let puma = Observer { heading_deg: Some(90.0), area: Some(SearchArea::Orbit { center, radius_m: 1_500.0 }), ..puma_at(BASE.0, BASE.1) };
    let obs = [usv(1, BASE.0 - 0.01, BASE.1, sensors_of("seahunter")), puma];
    let (lat, lon) = at(center.0, center.1, 700.0, 300.0);
    let cs = [contact("boat", "small_fast_boat", Domain::Surface, lat, lon), contact("ferry", "ferry", Domain::Surface, lat + 0.01, lon)];
    let a = run(&mut Engine::new(None), &day(), &obs, &cs, 0, 90);
    let b = run(&mut Engine::new(None), &day(), &obs, &cs, 0, 90);
    assert!(!a.is_empty());
    assert_eq!(a, b);
}

fn look(uid: u64, sensor: SensorType, level: Level, label: &str, confidence: f64) -> Detection {
    Detection {
        contact_id: "c".into(),
        uid,
        sensor,
        lat: BASE.0,
        lon: BASE.1,
        alt_m: None,
        sigma_m: 30.0,
        range_m: 1_000.0,
        velocity_mps: None,
        label: Some(label.into()),
        ais: None,
        domain: Domain::Surface,
        measured_t: Some(1_000.0),
        slant_from: None,
        look: Some(Look { level, confidence }),
        scan_s: None,
    }
}

/// Identification evidence per observer (WORLD-V3-PLAN 2.7): a wrong recognition is superseded
/// by the same observer's closer look (murmur-jln), two observers' identifications combine as
/// independent looks, and a disagreeing identification is a conflict.
#[test]
fn looks_are_kept_per_observer_and_a_closer_look_corrects_a_wrong_call() {
    let mut t = Track::new("T-0001".into(), 1, &look(1, SensorType::EoIr, Level::Recognise, "large_vessel", 0.8), 1_000.0);
    let id = t.identity();
    assert_eq!((id.kind.as_deref(), id.conflict.as_ref()), (Some("large_vessel"), None));
    t.update(&look(1, SensorType::EoIr, Level::Identify, "small_fast_boat", 0.6), 1_001.0);
    let id = t.identity();
    assert_eq!(id.kind.as_deref(), Some("small_fast_boat"));
    assert!((id.confidence - 0.6).abs() < 1e-9 && id.conflict.is_none(), "{id:?}");
    t.update(&look(2, SensorType::EoIr, Level::Identify, "small_fast_boat", 0.6), 1_002.0);
    assert!((t.identity().confidence - 0.84).abs() < 1e-9);
    t.update(&look(3, SensorType::EoIr, Level::Identify, "fishing_vessel", 0.5), 1_003.0);
    let id = t.identity();
    assert_eq!(id.kind.as_deref(), Some("small_fast_boat"));
    assert!(id.conflict.as_deref().is_some_and(|c| c.contains("fishing_vessel")), "{id:?}");
    // a radar's size class alone names the track at the detection level, with its confidence
    let r = Track::new("T-0002".into(), 2, &look(1, SensorType::Radar, Level::Detect, "small_surface", 0.3), 1_000.0);
    assert_eq!(r.identity().kind.as_deref(), Some("small_surface"));
}
