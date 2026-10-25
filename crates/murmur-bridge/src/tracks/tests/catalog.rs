//! The platform catalog under the physical model (WORLD-V3-PLAN section 3, bd murmur-1z6.4):
//! every sensor carries its own figures (none runs on placeholders), every field it gives is one
//! the schema reads for its type, the research's corrections hold, and (ignored) the table of each
//! sensor's `range_m` against the physical model's typical range (docs/SENSING.md quotes it:
//! `cargo test -p murmur-bridge catalog_table -- --ignored --nocapture`).

use super::super::physical::conditions::Conditions;
use super::super::physical::predict::{self, Ask};
use super::super::physical::signature::SignatureOverride;
use super::super::physical::spec::{Resolved, SonarMode, SonarSensor, placeholders, resolve};
use super::super::sensing::{Observer, SensorSpec, SensorType};
use super::super::truth::{Ais, ContactTruth, Domain};
use super::{BASE, at, catalog, contact};

const PLATFORMS: &str = include_str!("../../../../../sim/platforms.toml");

/// Every catalog sensor runs on its own figures under the physical model: none on the
/// placeholders derived from `range_m` (the bridge's start-up list of them is empty).
#[test]
fn no_catalog_sensor_runs_on_placeholders() {
    let cat = catalog();
    let ph = placeholders(&cat);
    assert!(ph.is_empty(), "catalog sensors on placeholders under the physical model: {ph:?}");
    for (id, p) in &cat.platforms {
        for s in &p.sensors {
            for airborne in [false, true] {
                assert!(!resolve(s, airborne).placeholder(), "{id}:{}", s.kind.as_str());
            }
        }
    }
}

/// The physical fields are flattened into a sensor entry and serde would pass over a misspelt
/// one: every key a catalog sensor gives is one the schema reads, for that sensor's type, and a
/// sonar's reference ranges name kinds the model knows.
#[test]
fn every_catalog_sensor_field_is_one_the_schema_reads() {
    let table: toml::Table = PLATFORMS.parse().expect("platforms.toml");
    let common = ["type", "range_m", "guess"];
    let by_type = |t: &str| -> &'static [&'static str] {
        match t {
            "eo_ir" => &["slew_dps", "channels"],
            "radar" => &["height", "r50_m", "rcs_ref_m2", "scan_s", "beam_deg", "cell_m", "band_ghz"],
            "sonar" => &["mode", "freq_khz", "ranges", "max_m", "scan_s", "passive"],
            "esm" => &["s_min_dbm", "bearing_deg"],
            _ => &[],
        }
    };
    let kinds = [
        "cargo_ship", "tanker", "ferry", "patrol_boat", "fishing_vessel", "sailboat", "small_fast_boat", "submarine", "periscope", "uuv", "diver",
    ];
    for (id, p) in &table {
        let Some(sensors) = p.get("sensors").and_then(|s| s.as_array()) else { continue };
        for s in sensors {
            let s = s.as_table().expect("a sensor is a table");
            let t = s.get("type").and_then(|v| v.as_str()).expect("a sensor's type");
            for k in s.keys() {
                assert!(common.contains(&k.as_str()) || by_type(t).contains(&k.as_str()), "[{id}] {t}: {k} is not a field the schema reads for it");
            }
            if let Some(r) = s.get("ranges").and_then(|v| v.as_table()) {
                for k in r.keys() {
                    assert!(kinds.contains(&k.as_str()), "[{id}] sonar ranges.{k}: not a kind the sonar model knows");
                }
            }
            // an active sonar's passive mode: its own band, ranges and reach
            if let Some(p) = s.get("passive").and_then(|v| v.as_table()) {
                for k in p.keys() {
                    assert!(["freq_khz", "ranges", "max_m"].contains(&k.as_str()), "[{id}] sonar passive.{k}: not a field the schema reads");
                }
                for k in p.get("ranges").and_then(|v| v.as_table()).map(|r| r.keys().collect::<Vec<_>>()).unwrap_or_default() {
                    assert!(kinds.contains(&k.as_str()), "[{id}] sonar passive.ranges.{k}: not a kind the sonar model knows");
                }
            }
        }
    }
}

fn kinds_of(platform: &str) -> Vec<SensorType> {
    catalog().get(platform).expect(platform).sensors.iter().map(|s| s.kind).collect()
}

fn sonar_of(platform: &str) -> SonarSensor {
    let s = catalog().get(platform).expect(platform).sensors.iter().find(|s| s.kind == SensorType::Sonar).cloned().expect("a sonar");
    match resolve(&s, false) {
        Resolved::Sonar(sn) => sn,
        other => panic!("{other:?}"),
    }
}

fn reference(sn: &SonarSensor, kind: &str) -> f64 {
    sn.ranges.iter().find(|(k, _)| k == kind).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {kind} reference"))
}

/// The research's corrections to the catalog (docs/research/sensors.md, "Conflicts with
/// sim/platforms.toml"): the Corsair passive EO/IR and ESM with no radar [42], the Magura EO
/// with no radar [41], the Saildrone Explorer cameras with no radar [47]; REMUS's side-scan 150
/// to 300 m a side [50], the BlueROV's Ping360 50 m [49], the buoy and the Orca passive with a
/// range per target class, a quiet submarine far shorter than a ship.
#[test]
fn the_research_corrections_hold() {
    let corsair = kinds_of("corsair");
    assert!(!corsair.contains(&SensorType::Radar) && corsair.contains(&SensorType::EoIr) && corsair.contains(&SensorType::Esm), "{corsair:?}");
    let magura = kinds_of("magura");
    assert!(!magura.contains(&SensorType::Radar) && magura.contains(&SensorType::EoIr), "{magura:?}");
    let sail = kinds_of("saildrone");
    assert!(!sail.contains(&SensorType::Radar) && sail.contains(&SensorType::EoIr), "{sail:?}");
    let remus = sonar_of("remus");
    assert!(remus.mode == SonarMode::Active && (150.0..=300.0).contains(&remus.max_m), "{remus:?}");
    let rov = sonar_of("bluerov");
    assert!(rov.max_m <= 50.0, "{rov:?}");
    for p in ["buoy", "orca"] {
        let sn = sonar_of(p);
        assert!(sn.mode == SonarMode::Passive && sn.ranges.len() >= 2, "{p}: {sn:?}");
        assert!(reference(&sn, "submarine") * 5.0 < reference(&sn, "cargo_ship"), "{p}: {sn:?}");
    }
}

/// A platform's operating height and domain from the catalog: a stand-in's `alt_m`, else the
/// SITL platform's `typical_alt_m`.
fn operating(platform: &str) -> (f64, String) {
    let table: toml::Table = PLATFORMS.parse().expect("platforms.toml");
    let p = table.get(platform).and_then(|p| p.as_table()).expect(platform);
    let num = |k: &str| p.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
    let alt = num("alt_m").or_else(|| num("typical_alt_m")).unwrap_or(0.0);
    (alt, p.get("domain").and_then(|v| v.as_str()).unwrap_or("surface").to_string())
}

/// The legacy model's signature of a kind (`murmur-contacts` `KINDS`): its 50 % range for the
/// kind is `range_m` times it.
fn legacy_signature(kind: &str) -> Option<f64> {
    Some(match kind {
        "cargo_ship" | "tanker" => 1.6,
        "ferry" => 1.2,
        "patrol_boat" => 0.9,
        "fishing_vessel" => 0.7,
        "sailboat" => 0.5,
        "small_fast_boat" => 0.35,
        "submarine" => 0.3,
        "uuv" => 0.15,
        _ => return None,
    })
}

/// The 50 % ranges of one sensor of `platform` against `target` (placed 1 km east; moved along
/// the line), in a clear day with the sun 45 degrees up, sea state 2 and the sonar's reference
/// environment, no terrain: detect, recognise, identify, m.
fn ranges_of(platform: &str, spec: &SensorSpec, target: &ContactTruth, over: Option<&SignatureOverride>) -> [f64; 3] {
    let cat = catalog();
    let ps = cat.get(platform).expect(platform);
    let (alt, domain) = operating(platform);
    let o = Observer {
        uid: 1,
        lat: BASE.0,
        lon: BASE.1,
        alt_msl: alt,
        mast_m: ps.mast_m,
        submerged: domain == "subsurface" && alt < -0.5,
        sensors: vec![spec.clone()],
        ..Observer::default()
    };
    let c = Conditions { sun_elevation_deg: Some(45.0), ..Conditions::default() };
    predict::ranges(&Ask {
        spec,
        observer: &o,
        target,
        over,
        conditions: c,
        t: 1_781_467_800.0,
        terrain: None,
        bathymetry: None,
        ais_load_per_min: 0.0,
    })
}

fn target(kind: &str, domain: Domain) -> ContactTruth {
    let (lat, lon) = at(BASE.0, BASE.1, 1000.0, 0.0);
    let mut c = contact(kind, kind, domain, lat, lon);
    if domain == Domain::Subsurface {
        c.alt_m = -30.0;
    }
    c
}

/// The high cameras' D/R/I ranges through the atmosphere (murmur-7vp, the slant path; docs/SENSING.md
/// quotes it: `cargo test -p murmur-bridge slant_camera_table -- --ignored --nocapture`): the
/// RQ-4's, the MQ-9B's and the CCA's EO/IR at their catalog altitudes against a vehicle (NATO
/// 2.3 m, 2 K) and a small fast boat, by day (the sun 45 degrees up) at 20 km visibility, as the
/// ground distance and the slant range at which each falls to one half; beside them the slant
/// ranges the geometry and blur alone allow (each channel's pixels on target through the ACQUIRE
/// curve, no atmosphere).
#[test]
#[ignore]
fn slant_camera_table() {
    use super::super::physical::eo;
    let cat = catalog();
    let km = |m: f64| (m / 10.0).round() / 100.0;
    for id in ["rq4", "mq9b", "cca"] {
        let (alt, _) = operating(id);
        let spec = cat.get(id).expect(id).sensors.iter().find(|s| s.kind == SensorType::EoIr).cloned().expect("a camera");
        let Resolved::EoIr(cam) = resolve(&spec, true) else { panic!("a camera") };
        for (kind, domain) in [("vehicle", Domain::Ground), ("small_fast_boat", Domain::Surface)] {
            let t = target(kind, domain);
            let r = ranges_of(id, &spec, &t, None);
            let sig = super::super::physical::signature::kind_signature(kind, domain);
            let slant = |d: f64| if d > 0.0 { d.hypot(alt - sig.height_m) } else { 0.0 };
            let geometry: Vec<f64> = (0..3)
                .map(|level| {
                    eo::range_at(0.5, |s| {
                        cam.channels
                            .iter()
                            .map(|ch| {
                                // as the prediction: detection and recognition in either field,
                                // identification in the narrow one
                                let p = |narrow: bool| eo::acquire(sig.dc_m / (s * ch.ifov(narrow)), eo::P50_PX[level]);
                                if level == 2 { p(ch.zooms()) } else { p(false).max(p(ch.zooms())) }
                            })
                            .fold(0.0, f64::max)
                    })
                })
                .collect();
            eprintln!(
                "{id:<5} {alt:6.0} m  {kind:<16} ground D/R/I {:6.2} {:6.2} {:6.2} km | slant {:6.2} {:6.2} {:6.2} km | geometry and blur alone {:6.2} {:6.2} {:6.2} km",
                km(r[0]),
                km(r[1]),
                km(r[2]),
                km(slant(r[0])),
                km(slant(r[1])),
                km(slant(r[2])),
                km(geometry[0]),
                km(geometry[1]),
                km(geometry[2])
            );
        }
    }
}

/// Each catalog sensor's `range_m` (what the decision engine and the legacy model read) against
/// the physical model's typical range: for EO/IR the detection range of the placeholders'
/// nominal target (5 m, 3 m high, 2 K: a surface contact of no known kind) with its
/// identification range beside it; for radar a 100 m^2 target 3 m high (the placeholders' own
/// reference); for ESM a patrol boat's navigation radar; for AIS a ferry's class A transponder;
/// for sonar each reference kind (and a cargo ship, a fast boat, a submarine and a UUV) against
/// the legacy model's `range_m` times the kind's signature. Horizon included, no terrain. A
/// ratio beyond 2 is flagged.
#[test]
#[ignore]
fn catalog_table() {
    let cat = catalog();
    let mut ids: Vec<&String> = cat.platforms.keys().collect();
    ids.sort();
    let flag = |a: f64, b: f64| if a > 2.0 * b || b > 2.0 * a { "  > 2x" } else { "" };
    let km = |m: f64| (m / 10.0).round() / 100.0;
    eprintln!("platform:sensor              range_m km  physical km   ratio");
    for id in ids {
        for s in &cat.get(id).expect("platform").sensors {
            let name = format!("{id}:{}", s.kind.as_str());
            match s.kind {
                SensorType::EoIr => {
                    let nominal = target("nominal", Domain::Surface);
                    let r = ranges_of(id, s, &nominal, None);
                    eprintln!(
                        "{name:<28} {:9.2}  D {:7.2}  {:6.2}{}   (R {:.2}, I {:.2})",
                        km(s.range_m),
                        km(r[0]),
                        r[0] / s.range_m,
                        flag(r[0], s.range_m),
                        km(r[1]),
                        km(r[2])
                    );
                }
                SensorType::Radar => {
                    let over = SignatureOverride { rcs_m2: Some(100.0), ..SignatureOverride::default() };
                    let r = ranges_of(id, s, &target("nominal", Domain::Surface), Some(&over));
                    eprintln!("{name:<28} {:9.2}  D {:7.2}  {:6.2}{}   (100 m^2, 3 m high)", km(s.range_m), km(r[0]), r[0] / s.range_m, flag(r[0], s.range_m));
                }
                SensorType::Esm => {
                    let mut t = target("patrol_boat", Domain::Surface);
                    t.emits = true;
                    let r = ranges_of(id, s, &t, None);
                    eprintln!("{name:<28} {:9.2}  D {:7.2}  {:6.2}{}   (a patrol boat's navigation radar)", km(s.range_m), km(r[0]), r[0] / s.range_m, flag(r[0], s.range_m));
                }
                SensorType::Ais => {
                    let mut t = target("ferry", Domain::Surface);
                    t.ais = Some(Ais { mmsi: "366000001".into(), name: "T".into(), ship_type: "passenger".into(), spoofed: false });
                    let r = ranges_of(id, s, &t, None);
                    eprintln!("{name:<28} {:9.2}  D {:7.2}  {:6.2}{}   (a ferry's class A)", km(s.range_m), km(r[0]), r[0] / s.range_m, flag(r[0], s.range_m));
                }
                SensorType::Sonar => {
                    let Resolved::Sonar(sn) = resolve(s, false) else { panic!("a sonar") };
                    let mut kinds: Vec<String> = ["cargo_ship", "small_fast_boat", "submarine", "uuv"].iter().map(|k| (*k).to_string()).collect();
                    for (k, _) in &sn.ranges {
                        if !kinds.contains(k) {
                            kinds.push(k.clone());
                        }
                    }
                    for k in kinds {
                        let domain = if matches!(k.as_str(), "submarine" | "uuv" | "diver") { Domain::Subsurface } else { Domain::Surface };
                        let r = ranges_of(id, s, &target(&k, domain), None);
                        let mine = if sn.ranges.iter().any(|(rk, _)| *rk == k) { "reference" } else { "" };
                        match legacy_signature(&k) {
                            Some(sig) => {
                                let legacy = s.range_m * sig;
                                eprintln!("{name:<28} {:9.2}  D {:7.2}  {:6.2}{}   ({k} {mine})", km(legacy), km(r[0]), r[0] / legacy, flag(r[0], legacy));
                            }
                            None => eprintln!("{name:<28} {:>9}  D {:7.2}            ({k} {mine}; not a legacy kind)", "-", km(r[0])),
                        }
                    }
                }
            }
        }
    }
}
