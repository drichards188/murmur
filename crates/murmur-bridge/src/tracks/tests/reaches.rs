//! The decision engine's sensor reaches under the physical model (bd murmur-1u4; docs/SENSING.md,
//! "The decision engine's reaches"). For each catalog sensor of a type that confirms a contact's
//! presence or absence (`murmur_decide::engage::sees`: radar, EO/IR, sonar) and each domain it
//! confirms it in, how far out the physical model's prediction for that sensor, pointed at the
//! domain's reference contact in the reference conditions, detects it at least half the time
//! all the way: the catalog's `physical_reach_m`, which the engine reads in a run on the physical
//! model (`murmur_decide::Catalog::reaching`) where it reads `range_m` on the legacy one.
//! Derived, never tuned (decision W3), and never edited by hand:
//!
//! ```text
//! cargo test -p murmur-bridge write_physical_reaches -- --ignored --nocapture
//! ```
//!
//! rewrites the `physical_reach_m` line after each platform's `sensors` in sim/platforms.toml
//! (and prints the table SENSING.md quotes), and `physical_reaches_are_the_generators` holds the
//! committed catalog to this generator: a change to the physical model or to a sensor's figures
//! that moves a reach fails it until the line above is run again and the catalog committed.
//!
//! Rules 31 (bd murmur-8vj8): each EO/IR's line also carries its reach by size class
//! (`size_class`), the same way against each class's reference contact ([`CLASS_KINDS`], its
//! smallest member), which an identify order's plan reads; and
//! `the_default_standoff_identifies_every_member_of_the_class` holds that plan's default standoff
//! from it inside the camera's identification reach of every kind the class holds.

use std::collections::BTreeMap;
use std::path::Path;

use murmur_decide::Domain as DDomain;
use murmur_decide::SizeClass;

use super::super::physical::predict::{self, Ask};
use super::super::physical::radar;
use super::super::sensing::{Observer, SensorCatalog, SensorSpec, SensorType};
use super::super::truth::{ContactTruth, Domain, TruthAffiliation};
use super::{BASE, at, catalog};

const PLATFORMS: &str = include_str!("../../../../../sim/platforms.toml");

/// The catalog key the reaches are written under.
const KEY: &str = "physical_reach_m";

// The reference contact (its kind and each domain's height) and the reference conditions are the
// reach display's (`tracks::reach`, bd murmur-4tq), so the two answer for the same contact.
use super::super::reach::{DOMAINS, REFERENCE_KIND, domain_name as name, reference_conditions as conditions};

/// Rules 31 (bd murmur-8vj8): each size class a detection names (`signature::size_class`) that a
/// camera sees (`murmur_decide::engage::sees`: none under water), the reference contact an EO/IR's
/// reach by class is derived for, and its height: the class's smallest member among the kinds
/// the physical model knows (`signature::kind_signature`, by critical dimension), so that a reach
/// by class is a camera's for the hardest of the class to make out, at the height `DOMAINS`
/// gives the domain. The members (`signature::size_class_of`): small drone and unknown UAS (0.35
/// m); light aircraft (4 m) and helicopter (5 m); small fast boat (3.5 m) and sailboat (5 m);
/// patrol boat (8 m), fishing vessel (11 m), ferry (27 m), cargo ship and tanker (84 m); person
/// (0.75 m); vehicle (2.3 m) and truck (3 m). The headless planner's honest ceiling reads a size
/// class as the same kinds (eval/headless/v3/planner.py `SIZE_KIND`) but for `large_surface`, a
/// cargo ship there. `the_default_standoff_identifies_every_member_of_the_class` holds the
/// identify order's default standoff from each inside the camera's identification reach of every
/// member.
const CLASS_KINDS: [(SizeClass, Domain, &str); 6] = [
    (SizeClass::SmallAir, Domain::Air, "small_drone"),
    (SizeClass::LargeAir, Domain::Air, "light_aircraft"),
    (SizeClass::SmallSurface, Domain::Surface, "small_fast_boat"),
    (SizeClass::LargeSurface, Domain::Surface, "patrol_boat"),
    (SizeClass::SmallGround, Domain::Ground, "person"),
    (SizeClass::LargeGround, Domain::Ground, "vehicle"),
];

/// The reference height of a contact in `domain`, m (`DOMAINS`).
fn reference_height_m(domain: Domain) -> f64 {
    DOMAINS.iter().find(|(d, _, _)| *d == domain).map_or(0.0, |(_, _, h)| *h)
}

/// The scenario clock the predictions are asked at (the sun is the conditions' own).
const T: f64 = 1_781_467_800.0;

/// A platform's operating height and domain from the catalog: a stand-in's `alt_m`, else the
/// SITL platform's `typical_alt_m` (as the catalog table's).
fn operating(table: &toml::Table, platform: &str) -> (f64, String) {
    let p = table.get(platform).and_then(|p| p.as_table()).expect(platform);
    let num = |k: &str| p.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
    let alt = num("alt_m").or_else(|| num("typical_alt_m")).unwrap_or(0.0);
    (alt, p.get("domain").and_then(|v| v.as_str()).unwrap_or("surface").to_string())
}

/// One sensor of a platform at its operating height and a contact of `kind` in `domain` at
/// `height_m`, moved east of it along the ground (as `predict::ranges` moves a contact along its
/// line), in the reference conditions.
struct Line<'a> {
    observer: Observer,
    spec: &'a SensorSpec,
    kind: &'a str,
    domain: Domain,
    height_m: f64,
}

impl<'a> Line<'a> {
    fn new(cat: &SensorCatalog, table: &toml::Table, platform: &str, spec: &'a SensorSpec, kind: &'a str, domain: Domain, height_m: f64) -> Line<'a> {
        let ps = cat.get(platform).expect(platform);
        let (alt, own) = operating(table, platform);
        let observer = Observer {
            uid: 1,
            lat: BASE.0,
            lon: BASE.1,
            alt_msl: alt,
            mast_m: ps.mast_m,
            submerged: own == "subsurface" && alt < -0.5,
            sensors: vec![spec.clone()],
            ..Observer::default()
        };
        Line { observer, spec, kind, domain, height_m }
    }

    /// The contact `d` metres east along the ground.
    fn target(&self, d: f64) -> ContactTruth {
        let (lat, lon) = at(BASE.0, BASE.1, d, 0.0);
        ContactTruth {
            id: "reference".into(),
            name: "reference".into(),
            affiliation: TruthAffiliation::Unknown,
            domain: self.domain,
            kind: self.kind.into(),
            lat,
            lon,
            alt_m: self.height_m,
            speed_mps: 0.0,
            course_deg: 0.0,
            emits: false,
            signature: 1.0,
            ais: None,
        }
    }

    fn ask<'b>(&'b self, target: &'b ContactTruth) -> Ask<'b> {
        Ask {
            spec: self.spec,
            observer: &self.observer,
            target,
            over: None,
            conditions: conditions(),
            t: T,
            terrain: None,
            bathymetry: None,
            ais_load_per_min: 0.0,
        }
    }

    /// A pointed sensor's chance to detect the contact `d` metres away.
    fn p_detect(&self, d: f64) -> f64 {
        let t = self.target(d);
        predict::predict(&self.ask(&t)).p_detect
    }

    /// The reach, m: how far out the contact is detected at least half the time all the way (0:
    /// not even at the start). Where the detection probability only falls with distance this is
    /// the model's D range (`predict::ranges`, which bisects for one crossing); where it does not,
    /// the disc covered throughout, never a crossing beyond a gap (an airborne radar's sea
    /// clutter cuts a contact holding still on the sea out within kilometres of its nadir, so it
    /// covers no disc there). The disc starts at 1 m, a radar's at its minimum range
    /// ([`radar::MIN_RANGE_M`], the IMO's 40 m from the antenna): nearer, the contact would be
    /// alongside, inside the hull that carries the radar, where the sea's specular glare under
    /// the antenna (murmur-qlt) hides it.
    fn reach_m(&self) -> f64 {
        inner_edge(self.start_m(), |d| self.p_detect(d))
    }

    /// Where the reach's disc starts, m (ground).
    fn start_m(&self) -> f64 {
        if self.spec.kind == SensorType::Radar { radar::MIN_RANGE_M } else { 1.0 }
    }

    /// Context for the table: the nearest and the farthest the contact is detected at least half
    /// the time, m (a 1 % grid from the start out to 1,000 km; none: nowhere).
    fn seen_between(&self) -> Option<(f64, f64)> {
        let mut d = self.start_m();
        let mut seen: Option<(f64, f64)> = None;
        while d <= 1.0e6 {
            if self.p_detect(d) >= 0.5 {
                seen = Some(seen.map_or((d, d), |(a, _)| (a, d)));
            }
            d *= 1.01;
        }
        seen
    }

    /// The model's D range (`predict::ranges`'s bisection for one crossing, from where the
    /// reach's disc starts), for the table.
    fn model_d_m(&self) -> f64 {
        let (mut lo, mut hi) = (self.start_m(), 1.0e6f64);
        if self.p_detect(lo) < 0.5 {
            return 0.0;
        }
        for _ in 0..80 {
            let mid = (lo * hi).sqrt();
            if self.p_detect(mid) >= 0.5 { lo = mid } else { hi = mid }
        }
        lo
    }
}

/// How far from `start` m `p` stays at or above one half, m (0: not at the start; at most
/// 1,000 km, as `eo::range_at`): its first fall below it on a 1 % grid, refined by bisection.
fn inner_edge(start: f64, p: impl Fn(f64) -> f64) -> f64 {
    if p(start) < 0.5 {
        return 0.0;
    }
    let mut lo = start;
    while lo < 1.0e6 {
        let hi = (lo * 1.01).min(1.0e6);
        if p(hi) < 0.5 {
            let (mut a, mut b) = (lo, hi);
            for _ in 0..50 {
                let m = 0.5 * (a + b);
                if p(m) >= 0.5 {
                    a = m;
                } else {
                    b = m;
                }
            }
            return a;
        }
        lo = hi;
    }
    1.0e6
}

/// One sensor's reaches: its type, its reach per domain and, an EO/IR's (rules 31), per size
/// class, m rounded to 10 (a domain or class it does not reach, under 5 m, left out).
#[derive(Debug)]
struct SensorReaches {
    kind: String,
    domains: Vec<(DDomain, i64)>,
    classes: Option<Vec<(SizeClass, i64)>>,
}

/// A platform's reaches, in its catalog's sensor order: each sensor type the engine assesses
/// with.
type Reaches = Vec<SensorReaches>;

/// A reach rounded to 10 m.
fn tens(m: f64) -> i64 {
    ((m / 10.0).round() as i64) * 10
}

/// Every platform's reaches (none for a platform with no sensor the engine assesses with).
fn derive(cat: &SensorCatalog, table: &toml::Table) -> BTreeMap<String, Reaches> {
    let mut out = BTreeMap::new();
    for id in table.keys() {
        let Some(ps) = cat.get(id) else { continue };
        let mut reaches: Reaches = Vec::new();
        for s in &ps.sensors {
            let kind = s.kind.as_str();
            let domains: Vec<_> = DOMAINS.iter().filter(|(_, d, _)| murmur_decide::engage::sees(kind, *d)).collect();
            if domains.is_empty() {
                continue;
            }
            assert!(!reaches.iter().any(|r| r.kind == kind), "[{id}] carries two {kind} sensors: the reaches are by sensor type");
            let by_domain = domains
                .iter()
                .map(|(td, dd, h)| (*dd, tens(Line::new(cat, table, id, s, REFERENCE_KIND, *td, *h).reach_m())))
                .filter(|(_, m)| *m > 0)
                .collect();
            // rules 31: a camera's reach by size class, against each class's smallest member
            let classes = (s.kind == SensorType::EoIr).then(|| {
                CLASS_KINDS
                    .iter()
                    .filter(|(c, _, _)| murmur_decide::engage::sees(kind, c.domain()))
                    .map(|(c, td, k)| (*c, tens(Line::new(cat, table, id, s, k, *td, reference_height_m(*td)).reach_m())))
                    .filter(|(_, m)| *m > 0)
                    .collect()
            });
            reaches.push(SensorReaches { kind: kind.to_string(), domains: by_domain, classes });
        }
        if !reaches.is_empty() {
            out.insert(id.clone(), reaches);
        }
    }
    out
}

/// The catalog line for a platform's reaches.
fn line(reaches: &Reaches) -> String {
    let sensors: Vec<String> = reaches
        .iter()
        .map(|r| {
            let mut ds: Vec<String> = r.domains.iter().map(|(d, m)| format!("{} = {m}", name(*d))).collect();
            if let Some(cs) = &r.classes {
                let cs: Vec<String> = cs.iter().map(|(c, m)| format!("{} = {m}", c.as_str())).collect();
                ds.push(if cs.is_empty() { "size_class = {}".to_string() } else { format!("size_class = {{ {} }}", cs.join(", ")) });
            }
            if ds.is_empty() { format!("{} = {{}}", r.kind) } else { format!("{} = {{ {} }}", r.kind, ds.join(", ")) }
        })
        .collect();
    format!("{KEY} = {{ {} }}", sensors.join(", "))
}

/// Whether a catalog line sets `key` (a key at the start of the line, before its `=`).
fn sets(l: &str, key: &str) -> bool {
    l.trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
}

/// `text` (sim/platforms.toml) with each platform's `physical_reach_m` line taken out and the
/// one `lines` gives it put right after the end of its `sensors` value.
fn rewrite(text: &str, lines: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut section: Option<&str> = None;
    // the open brackets of a `sensors` value being read (none: not in one)
    let mut depth: Option<i64> = None;
    for l in text.lines() {
        if sets(l, KEY) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
        let t = l.trim();
        if let Some(id) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')).filter(|id| !id.starts_with('[')) {
            section = Some(id);
        }
        if depth.is_none() && sets(l, "sensors") {
            depth = Some(0);
        }
        if let Some(n) = depth.as_mut() {
            // the value, not a trailing comment (no catalog string holds a bracket or a #)
            let v = l.split('#').next().unwrap_or("");
            *n += v.matches('[').count() as i64 - v.matches(']').count() as i64;
            if *n <= 0 {
                depth = None;
                if let Some(line) = section.and_then(|s| lines.get(s)) {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// The catalog as this generator writes it, after checking that only `physical_reach_m` lines
/// changed and that every platform with reaches got its line.
fn generated(text: &str) -> String {
    let table: toml::Table = text.parse().expect("platforms.toml");
    let cat = SensorCatalog::from_toml(text).expect("catalog");
    let derived = derive(&cat, &table);
    let lines: BTreeMap<String, String> = derived.iter().map(|(id, r)| (id.clone(), line(r))).collect();
    let out = rewrite(text, &lines);
    let strip = |t: &str| -> toml::Table {
        let mut t: toml::Table = t.parse().expect("toml");
        for (_, p) in t.iter_mut() {
            if let Some(p) = p.as_table_mut() {
                p.remove(KEY);
            }
        }
        t
    };
    assert_eq!(strip(&out), strip(text), "the generator changed more than the {KEY} lines");
    let written: toml::Table = out.parse().expect("the generated catalog");
    for id in derived.keys() {
        assert!(written.get(id).and_then(|p| p.get(KEY)).is_some(), "[{id}]: no {KEY} written (no sensors line found?)");
    }
    out
}

/// The committed catalog is the generator's: every `physical_reach_m` value and line, as
/// `write_physical_reaches` writes them, and the decision layer reads them back as written.
#[test]
fn physical_reaches_are_the_generators() {
    let want = generated(PLATFORMS);
    if want != PLATFORMS {
        let diff: Vec<String> = want
            .lines()
            .filter(|l| sets(l, KEY) && !PLATFORMS.lines().any(|c| c == *l))
            .map(str::to_string)
            .collect();
        panic!(
            "sim/platforms.toml's {KEY} is not the physical model's (run `cargo test -p murmur-bridge write_physical_reaches -- --ignored` and commit it); the generator writes:\n{}",
            diff.join("\n")
        );
    }
    // the decision layer reads each sensor's reaches as written
    let decide = murmur_decide::Catalog::from_toml(PLATFORMS).expect("the decision catalog").reaching(murmur_decide::Reaches::Physical);
    let table: toml::Table = PLATFORMS.parse().expect("platforms.toml");
    let derived = derive(&catalog(), &table);
    for (id, p) in &decide.platforms {
        for s in &p.sensors {
            let want = derived.get(id).and_then(|r| r.iter().find(|x| x.kind == s.kind));
            match (want, &s.reach_m) {
                (None, None) => {}
                (Some(w), Some(r)) => {
                    for (_, d, _) in DOMAINS {
                        let m = w.domains.iter().find(|(x, _)| *x == d).map(|(_, m)| *m as f64);
                        assert_eq!(r.get(d), m, "[{id}] {}: {} reach", s.kind, name(d));
                    }
                    // rules 31: and a camera's by size class
                    assert_eq!(r.size_class.is_some(), w.classes.is_some(), "[{id}] {}: reaches by size class", s.kind);
                    for c in SizeClass::ALL {
                        let m = w.classes.iter().flatten().find(|(x, _)| *x == c).map(|(_, m)| *m as f64);
                        assert_eq!(s.reach_for_class(c).flatten(), m, "[{id}] {}: {} reach", s.kind, c.as_str());
                    }
                }
                other => panic!("[{id}] {}: {other:?}", s.kind),
            }
        }
    }
    // and the legacy facts carry none
    let legacy = murmur_decide::Catalog::from_toml(PLATFORMS).expect("the decision catalog");
    assert!(legacy.platforms.values().all(|p| p.sensors.iter().all(|s| s.reach_m.is_none())));
}

/// The table SENSING.md quotes: each reach beside `range_m`, the model's D range where it
/// differs (a curve that does not only fall: `*`), and for context the reach for a small fast
/// boat (surface) and a small drone (air, 80 m up) with the same sensor.
fn table(text: &str) {
    let table: toml::Table = text.parse().expect("platforms.toml");
    let cat = SensorCatalog::from_toml(text).expect("catalog");
    let derived = derive(&cat, &table);
    let km = |m: f64| (m / 10.0).round() / 100.0;
    eprintln!("platform:sensor          range_m km    air km    ground km   surface km  subsurface km   (fast boat, small drone km)");
    for (id, reaches) in &derived {
        let ps = cat.get(id).expect(id);
        for SensorReaches { kind, domains: ds, .. } in reaches {
            let s = ps.sensors.iter().find(|s| s.kind.as_str() == kind).expect("sensor");
            let col = |d: DDomain| -> String {
                if !murmur_decide::engage::sees(kind, d) {
                    return "-".into();
                }
                let (td, _, h) = DOMAINS.iter().find(|(_, x, _)| *x == d).expect("domain");
                let r = ds.iter().find(|(x, _)| *x == d).map_or(0.0, |(_, m)| *m as f64);
                let line = Line::new(&cat, &table, id, s, REFERENCE_KIND, *td, *h);
                let model = line.model_d_m();
                match line.seen_between() {
                    // seen only beyond a gap (an airborne radar against a contact still on the sea)
                    Some((a, b)) if a > line.start_m() * 1.02 => format!("{:.2}[{:.1}-{:.1}]", km(r), km(a), km(b)),
                    _ if (model - r).abs() > 10.0 => format!("{:.2}*{:.2}", km(r), km(model)),
                    _ => format!("{:.2}", km(r)),
                }
            };
            let boat = murmur_decide::engage::sees(kind, DDomain::Surface).then(|| km(Line::new(&cat, &table, id, s, "small_fast_boat", Domain::Surface, 0.0).reach_m()));
            let drone = murmur_decide::engage::sees(kind, DDomain::Air).then(|| km(Line::new(&cat, &table, id, s, "small_drone", Domain::Air, 80.0).reach_m()));
            eprintln!(
                "{:<26} {:9.2} {:>9} {:>12} {:>12} {:>14}   ({}, {})",
                format!("{id}:{kind}"),
                km(s.range_m),
                col(DDomain::Air),
                col(DDomain::Ground),
                col(DDomain::Surface),
                col(DDomain::Subsurface),
                boat.map_or("-".into(), |v| format!("{v:.2}")),
                drone.map_or("-".into(), |v| format!("{v:.2}")),
            );
        }
    }
}

/// Rewrites sim/platforms.toml's `physical_reach_m` lines from the physical model, and prints
/// the table (`--nocapture`).
#[test]
#[ignore]
fn write_physical_reaches() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sim/platforms.toml");
    let text = std::fs::read_to_string(&path).expect("sim/platforms.toml");
    let out = generated(&text);
    if out != text {
        std::fs::write(&path, &out).expect("write sim/platforms.toml");
        eprintln!("wrote {}", path.display());
    } else {
        eprintln!("{} already holds the physical model's reaches", path.display());
    }
    table(&out);
}

/// The table alone (`--nocapture`), from the catalog as committed.
#[test]
#[ignore]
fn physical_reach_table() {
    table(PLATFORMS);
}

/// The reach is the first fall below one half: the model's crossing where the curve only falls,
/// the near edge of a gap where it does not.
#[test]
fn the_reach_stops_at_the_first_gap() {
    let falling = |d: f64| if d < 2_500.0 { 0.9 } else { 0.1 };
    assert!((inner_edge(1.0, falling) - 2_500.0).abs() < 1.0);
    let gap = |d: f64| if !(800.0..1_200.0).contains(&d) && d < 5_000.0 { 0.9 } else { 0.1 };
    assert!((inner_edge(1.0, gap) - 800.0).abs() < 1.0);
    assert_eq!(inner_edge(1.0, |_| 0.4), 0.0);
    assert_eq!(inner_edge(1.0, |_| 0.6), 1.0e6);
    // from a radar's minimum range: what is under the antenna does not count
    let glare = |d: f64| if (10.0..3_000.0).contains(&d) { 0.9 } else { 0.1 };
    assert_eq!(inner_edge(1.0, glare), 0.0);
    assert!((inner_edge(40.0, glare) - 3_000.0).abs() < 1.0);
}

/// The rewrite puts a platform's line after its sensors however they are laid out, replaces an
/// old one wherever it was, and leaves a platform without one alone.
#[test]
fn the_rewrite_places_the_line_after_the_sensors() {
    let text = "[a]\nsensors = [\n  { type = \"radar\", range_m = 1 },  # [1]\n]\nmast_m = 3\n\n[b]\nphysical_reach_m = { old = {} }\nsensors = [{ type = \"eo_ir\", range_m = 2, channels = [{ band = \"day\", wide_deg = 1, px = 2 }] }]\nx = 1\n\n[c]\nsensors = []\n";
    let lines: BTreeMap<String, String> = [("a", "physical_reach_m = { radar = {} }"), ("b", "physical_reach_m = { eo_ir = { air = 10 } }")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let want = "[a]\nsensors = [\n  { type = \"radar\", range_m = 1 },  # [1]\n]\nphysical_reach_m = { radar = {} }\nmast_m = 3\n\n[b]\nsensors = [{ type = \"eo_ir\", range_m = 2, channels = [{ band = \"day\", wide_deg = 1, px = 2 }] }]\nphysical_reach_m = { eo_ir = { air = 10 } }\nx = 1\n\n[c]\nsensors = []\n";
    assert_eq!(rewrite(text, &lines), want);
    assert_eq!(rewrite(want, &lines), want, "a second run changes nothing");
}

/// The kinds the physical model knows by name (`signature::kind_signature`'s table).
const MODEL_KINDS: [&str; 18] = [
    "cargo_ship",
    "tanker",
    "ferry",
    "patrol_boat",
    "fishing_vessel",
    "sailboat",
    "small_fast_boat",
    "light_aircraft",
    "helicopter",
    "unknown_uas",
    "small_drone",
    "submarine",
    "periscope",
    "uuv",
    "diver",
    "vehicle",
    "truck",
    "person",
];

/// The families its recognitions name (`sensing::family`).
const FAMILIES: [&str; 7] = ["large_vessel", "small_craft", "aircraft", "small_uas", "ground_vehicle", "person", "submerged"];

/// Rules 31: the decision layer reads a label's size class (`murmur_decide::SizeClass::of_label`,
/// which an identify order's plan reads the track's shown identity by) as the physical model
/// makes it (`signature::size_class_of`), for every kind it knows, every kind a contact of the
/// contact catalog is seen as, every family and every class; and each class's reference contact
/// for a camera's reach by class is of the class and its smallest member.
#[test]
fn the_decision_layers_size_classes_are_the_models() {
    use super::super::physical::signature::{kind_signature, size_class_of};
    let contacts: toml::Table = include_str!("../../../../../sim/contacts.toml").parse().expect("contacts.toml");
    let seen: Vec<String> = contacts
        .iter()
        .filter_map(|(k, v)| v.as_table().map(|t| t.get("looks_like").and_then(|l| l.as_str()).unwrap_or(k).to_string()))
        .collect();
    assert!(seen.len() > 20, "{} contact kinds", seen.len());
    let classes = SizeClass::ALL.map(SizeClass::as_str);
    for label in MODEL_KINDS
        .into_iter()
        .chain(FAMILIES)
        .chain(classes)
        .chain(seen.iter().map(String::as_str))
        .chain(["unknown", "vessel"])
    {
        let decide = SizeClass::of_label(label).map_or("unknown", SizeClass::as_str);
        assert_eq!(decide, size_class_of(label), "{label}");
    }
    for (class, domain, kind) in CLASS_KINDS {
        assert_eq!(size_class_of(kind), class.as_str(), "{kind}");
        assert_eq!(domain.decide(), class.domain(), "{kind}");
        let dc = |k: &str| kind_signature(k, domain).dc_m;
        for member in MODEL_KINDS.iter().filter(|m| size_class_of(m) == class.as_str()) {
            assert!(dc(kind) <= dc(member), "{}: {kind} ({} m) is not its smallest member, {member} ({} m)", class.as_str(), dc(kind), dc(member));
        }
    }
    // every class a camera sees has its reference, and only those
    for class in SizeClass::ALL {
        let has = CLASS_KINDS.iter().any(|(c, _, _)| *c == class);
        assert_eq!(has, murmur_decide::engage::sees("eo_ir", class.domain()), "{}", class.as_str());
    }
}

/// Rules 31: an identify order's default standoff from each camera's reach by size class (the
/// plan's: an eighth of the reach, `IDENTIFY_PX_SHARE`, then `STANDOFF_SHARE` of that as slant
/// range, the vehicle at its catalog operating height over the class's reference height) is
/// inside the camera's identification reach of every kind the class holds, its identification
/// chance there at least one half on the reference day; where the vehicle's height over the
/// contact is beyond that eighth, the plan refuses it from there and there is nothing to hold.
#[test]
fn the_default_standoff_identifies_every_member_of_the_class() {
    use super::super::physical::signature::size_class_of;
    use murmur_decide::identify::{IDENTIFY_PX_SHARE, STANDOFF_SHARE};
    let table: toml::Table = PLATFORMS.parse().expect("platforms.toml");
    let cat = catalog();
    let derived = derive(&cat, &table);
    let (mut checked, mut refused) = (0, 0);
    for (id, reaches) in &derived {
        let Some(classes) = reaches.iter().find(|r| r.kind == "eo_ir").and_then(|r| r.classes.as_ref()) else { continue };
        let s = cat.get(id).expect(id).sensors.iter().find(|s| s.kind == SensorType::EoIr).expect("eo_ir");
        let (alt, _) = operating(&table, id);
        for (class, m) in classes {
            let (_, domain, _) = CLASS_KINDS.iter().find(|(c, _, _)| c == class).expect("a reference");
            let h = reference_height_m(*domain);
            let reach = *m as f64 * IDENTIFY_PX_SHARE;
            let height = (alt - h).abs();
            if height >= reach {
                refused += 1;
                continue;
            }
            let slant = (reach * STANDOFF_SHARE).max(height);
            let ground = (slant * slant - height * height).max(0.0).sqrt().max(1.0);
            for kind in MODEL_KINDS.iter().filter(|k| size_class_of(k) == class.as_str()) {
                let line = Line::new(&cat, &table, id, s, kind, *domain, h);
                let t = line.target(ground);
                let p = predict::predict(&line.ask(&t)).p_identify;
                assert!(
                    p >= 0.5,
                    "[{id}] a {kind} ({}) at the default standoff, {ground:.0} m out and {height:.0} m away in height: identified at {p:.2}",
                    class.as_str()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 100 && refused > 5, "{checked} checked, {refused} refused from the operating height");
}
