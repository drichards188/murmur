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

/// World v3: the Golden Gate's land/water grid and current field as the water reads them. In
/// the strait at 10:00Z the ebb runs west at a knot or more (tidewater answered -1.79 east,
/// 0.46 north at the bridge); Crissy Field is land; nothing outside the field; a current step
/// is the current while it lasts.
#[test]
fn the_world_water_reads_the_baked_grids() {
    use murmur_headless::sim::spec::WaterSpec;
    use murmur_headless::sim::water::{CurrentField, Water};
    let t = 1_781_431_200.0; // 2026-06-14T10:00:00Z
    let f = CurrentField::load(&repo().join("sim/headless/terrain/golden-gate.currents.gz")).expect("field");
    let (e, n) = f.at(37.8199, -122.4783, t).expect("the bridge");
    assert!(e < -1.0 && e > -2.6 && n.abs() < 1.0, "{e} {n}");
    // between two hourly steps the current is between them
    let (a, b) = (f.at(37.8199, -122.4783, t).expect("a").0, f.at(37.8199, -122.4783, t + 3600.0).expect("b").0);
    let mid = f.at(37.8199, -122.4783, t + 1800.0).expect("mid").0;
    assert!((mid - (a + b) / 2.0).abs() < 1e-9, "{a} {mid} {b}");
    assert_eq!(f.at(37.0, -122.48, t), None, "outside the field");
    let surface = Arc::new(Grid::load(&repo().join("sim/headless/terrain/golden-gate.grid.gz")).expect("grid"));
    let spec = WaterSpec {
        currents: Some(repo().join("sim/headless/terrain/golden-gate.currents.gz")),
        land_water: Some(repo().join("sim/headless/terrain/golden-gate.water.grid.gz")),
        wind_en: Some([1.0, -2.0]),
    };
    let w = Water::load(&spec, Some(surface)).expect("water");
    let sea = w.sample(37.8199, -122.4783, t);
    let depth = sea.water_depth_m.expect("depth");
    assert!(depth > 50.0, "the channel under the bridge: {depth}");
    assert!((sea.water_level_m.expect("level") - sea.elevation_m.expect("bed") - depth).abs() < 1e-9);
    assert!(sea.navigable(5.0) && sea.current_east.is_some_and(|c| c < -1.0));
    assert_eq!((sea.wind_east, sea.wind_north), (Some(1.0), Some(-2.0)));
    let land = w.sample(37.8060, -122.4480, t);
    assert!(!land.navigable(0.1) && land.surface_m().is_some_and(|s| s > 1.0), "{land:?}");
    assert_eq!((land.current_east, land.current_north), (Some(0.0), Some(0.0)));
    assert_eq!(w.sample(37.0, -122.48, t).elevation_m, None, "out of the area");
    // a current step in place of the field, while it lasts
    w.set_uniform(1.5, 0.5, t + 10.0, Some(60.0));
    assert_eq!(w.current(37.8199, -122.4783, t + 20.0), Some((1.5, 0.5)));
    assert_eq!(w.current(37.8199, -122.4783, t + 75.0), f.at(37.8199, -122.4783, t + 75.0));
    assert_eq!(w.current(37.0, -122.48, t + 20.0), Some((1.5, 0.5)), "uniform everywhere");
    assert_eq!(Water::still().current(37.8199, -122.4783, t), None, "still water: none, as before world v3");
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

/// The truth side's and the decision core's tables of the contact kinds each weapon's figures
/// stand for are one table (bd murmur-lbw8.2): the figures the weapon check shows are the ones a
/// strike draws on.
#[test]
fn the_kind_alias_tables_agree() {
    assert_eq!(murmur_contacts::damage::KIND_ALIASES, murmur_decide::catalog::KIND_ALIASES);
}

/// The contact catalog's loadouts are the decision core's weapon facts (bd murmur-y85j.1): each
/// entry, less the truth side's own keys (`reload_s`, `sense`), is read and validated by the
/// decision core's own catalog parser, and reads back field for field as the truth side reads it.
#[test]
fn the_contact_catalog_loadouts_are_weapon_facts() {
    let text = std::fs::read_to_string(repo().join("sim/contacts.toml")).expect("sim/contacts.toml");
    let ours = murmur_contacts::catalog::Catalog::from_toml(&text).expect("the truth side reads it");
    let mut raw: toml::Table = toml::from_str(&text).expect("toml");
    for (_, kind) in raw.iter_mut() {
        let kind = kind.as_table_mut().expect("a kind is a table");
        // what the platform catalog needs of a platform, and none of the node's own sensors
        // (a reach each, not our catalog's sensor list)
        kind.insert("sim".into(), "contact".into());
        kind.remove("sensors");
        for w in kind.get_mut("loadout").and_then(|l| l.as_array_mut()).into_iter().flatten() {
            let w = w.as_table_mut().expect("a weapon is a table");
            w.remove("reload_s");
            w.remove("sense");
        }
    }
    let facts = murmur_decide::Catalog::from_toml(&toml::to_string(&raw).expect("toml")).expect("the decision core reads every loadout");
    let mut weapons = 0;
    for (id, k) in &ours.kinds {
        let theirs = &facts.get(id).expect(id).loadout;
        assert_eq!(theirs.len(), k.loadout.len(), "{id}");
        for (w, f) in k.loadout.iter().zip(theirs) {
            let mut w = serde_json::to_value(w).expect("json");
            let w = w.as_object_mut().expect("object");
            w.remove("reload_s");
            w.remove("sense");
            assert_eq!(serde_json::Value::Object(w.clone()), serde_json::to_value(f).expect("json"), "{id}");
            weapons += 1;
        }
    }
    assert!(weapons >= 10, "the catalog's weapons: {weapons}");
}

/// What the bridge hands the truth side with a strike reads there as it was meant (bd
/// murmur-lbw8.2): the catalog's weapon facts as a weapon with its figures, and the engine's
/// munition flight as a launch and an impact.
#[test]
fn the_catalog_weapon_and_flight_read_on_the_truth_side() {
    let catalog = murmur_decide::Catalog::load(&repo().join("sim/platforms.toml")).expect("catalog");
    let hellfire = catalog.get("mq9b_armed").expect("mq9b_armed").loadout[0].clone();
    let w: murmur_contacts::damage::WeaponDamage = serde_json::from_value(serde_json::to_value(&hellfire).expect("json")).expect("reads");
    assert!(w.modelled() && w.kind == "missile" && w.guidance == "semi_active_laser");
    assert_eq!(w.warhead.as_ref().map(|h| h.mass_kg), Some(9.0));
    let (k, f) = w.figures_for("ferry").expect("a ferry reads the cargo ship's figures");
    assert_eq!((k, f.p_hit), ("cargo_ship", Some(0.95)));
    let flight = murmur_decide::engage::MunitionFlight {
        name: hellfire.name.clone(),
        speed_mps: 425.0,
        launch_t: murmur_decide::Time(0),
        launch_point: murmur_decide::LonLat::new(-122.5116, 37.8075),
        launch_alt_m: 7600.0,
        impact_t: murmur_decide::Time(18_400),
        impact_point: murmur_decide::LonLat::new(-122.495, 37.812),
        impact_alt_m: 0.0,
        tof_s: 18.4,
    };
    let f: murmur_contacts::effect::Flight = serde_json::from_value(serde_json::to_value(&flight).expect("json")).expect("reads");
    // 1.5 km over the ground from 7,600 m up: well inside half the 8 km range, no range factor
    let out = f.ground_m().expect("over the ground");
    assert!((1400.0..1600.0).contains(&out), "{out}");
    let target = murmur_contacts::damage::Target { kind: "small_fast_boat", speed_mps: 0.0, armed: true, surface: true };
    let day = murmur_contacts::damage::Conditions { visibility_m: 20_000.0, sea_state: 2.0, sun_elevation_deg: 40.0 };
    assert_eq!(murmur_contacts::damage::hit_chance(&w, &target, &day, Some(out)).3, 0.75);
}

/// An armed boat holding mid-strait under an order to attack our vehicle 1 from second 2 with
/// its missiles, from where it is.
const ATTACK: &str = r#"title = "headless step"
seed = 17
[[contact]]
id = "raider"
affiliation = "hostile"
domain = "surface"
kind = "armed_fast_boat"
motion = "station"
at = [-122.4950, 37.8120]
[[order]]
at_s = 2
node = "raider"
do = "attack"
target = { vehicle = 1 }
weapon = "ATGM-class missile"
stay = true
"#;

/// The bridge's answer to a truth message, as `sim_contacts` gives it: `ours` only when the
/// message asks (a node's look), our vehicle 1 still on the water 1.5 km east of the boat.
fn bridge_answer(msg: &serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    let mut a = json!({"ok": true, "contacts": 1, "detections": 0, "tracks": 0, "effects": []});
    if msg.get("look").and_then(|l| l.as_array()).is_some_and(|l| !l.is_empty()) {
        a["ours"] = json!([{"uid": 1, "lat": 37.8120, "lon": -122.47797, "alt_m": 0.0, "ve": 0.0, "vn": 0.0,
                            "platform": "seahunter", "domain": "surface"}]);
    }
    a
}

/// Live and headless give the same truth (docs/AUTONOMOUS-NODES.md stage 5, bd murmur-y85j.5):
/// the simulator's contacts loop, on the paused clock and posting through a router, sends exactly
/// the messages the shared step (`murmur_contacts::step`) gives when driven second by second as
/// the contacts process's `main.rs` drives it, with the same answers from the bridge. The boat
/// takes its order, looks, perceives our vehicle 1 in the answer, fires its catalog missile at
/// its reload, and each strike is drawn by the damage model, the same both ways.
#[test]
fn the_simulators_contacts_loop_is_the_contacts_process_step() {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use murmur_contacts::effect::{self, DamageCtx};
    use murmur_contacts::motion::Mover;
    use murmur_contacts::scenario::Scenario;
    use murmur_contacts::step::{self, Step};
    use murmur_headless::sim::contacts::{self as hc, Contacts, Ours};
    use murmur_headless::sim::fleet::Fleet;
    use murmur_headless::sim::water::Water;
    use serde_json::Value;

    const SECONDS: u32 = 40;
    const SEED: u64 = 5;
    let t0 = 1_781_463_600.0; // 2026-06-14 19:00Z, local noon at the Gate
    let path = std::env::temp_dir().join(format!("murmur-headless-step-{}.toml", std::process::id()));
    std::fs::write(&path, ATTACK).expect("the scenario");

    // the contacts process's loop as main.rs runs it: the movers step (no current), then the
    // shared step; the bridge's answer read back
    let scenario = Scenario::load(&path).expect("loads");
    let mut movers: Vec<Mover> =
        scenario.contacts().expect("contacts").into_iter().enumerate().map(|(i, c)| Mover::new(c, SEED, i as u64)).collect();
    let (mut nodes, _) = step::nodes(&scenario, &movers, &repo(), true);
    let mut live = Step::new(&scenario, SEED, t0);
    let ctx = DamageCtx { seed: SEED, conditions: scenario.conditions, hit_override: None };
    let mut land = |e: &effect::EffectNotice, movers: &mut [Mover], s: f64| effect::apply_with(e, movers, t0 + s, Some(&ctx));
    let mut want: Vec<Value> = Vec::new();
    for k in 0..=SECONDS {
        let (s, dt) = (f64::from(k), if k == 0 { 0.0 } else { 1.0 });
        for m in &mut movers {
            m.step(s, dt, None);
        }
        let msg = serde_json::to_value(live.tick(&scenario, s, dt, &mut movers, &mut nodes, &ctx, &mut land).msg).expect("json");
        live.answer(&bridge_answer(&msg), s, &mut movers, &mut land, &mut |_| {});
        want.push(msg);
    }

    // the simulator's loop on the paused clock, the bridge a router answering the same way
    let posted: Arc<Mutex<Vec<Value>>> = Arc::default();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().expect("runtime");
    rt.block_on(async {
        let p = posted.clone();
        let api = axum::Router::new().route(
            "/api/sim/contacts",
            axum::routing::post(move |body: String| {
                let p = p.clone();
                async move {
                    let msg: Value = serde_json::from_str(&body).expect("a truth message");
                    let a = bridge_answer(&msg);
                    p.lock().expect("posted").push(msg);
                    axum::Json(a)
                }
            }),
        );
        let c = Arc::new(Contacts::load(&path, Some(SEED), t0, &repo(), None).expect("the simulator loads it"));
        let fleet = Arc::new(Fleet::new(Vec::new(), SEED, &BTreeMap::new(), None).expect("fleet"));
        let ours = Ours { fleet, by_uid: BTreeMap::new(), seed: SEED };
        let start = tokio::time::Instant::now();
        let water = (Arc::new(Water::still()), start, t0);
        let task = tokio::spawn(hc::run(c, api, None, start, start, None, water, ours));
        tokio::time::sleep(Duration::from_secs_f64(f64::from(SECONDS) + 0.5)).await;
        task.abort();
    });
    let _ = std::fs::remove_file(&path);
    let got = posted.lock().expect("posted").clone();
    assert_eq!(got.len(), want.len());
    for (k, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "second {k}");
    }
    // what both ways did: the order taken, a look answered, the catalog's missile at its 30 s
    // reload, each strike on our vehicle 1 drawn by the damage model
    let all = |key: &str| want.iter().flat_map(|m| m[key].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>();
    assert!(all("node_events").iter().any(|e| e["what"] == "order" && e["node"] == "raider"));
    assert!(all("look").iter().any(|l| l["node"] == "raider"));
    let mut launched: Vec<(String, f64)> = all("weapons")
        .iter()
        .map(|w| (w["name"].as_str().unwrap_or_default().to_string(), w["launch_t"].as_f64().unwrap_or_default()))
        .collect();
    launched.dedup();
    assert_eq!(launched.len(), 2, "{launched:?}");
    assert!(launched.iter().all(|(n, _)| n == "ATGM-class missile"));
    assert!((launched[1].1 - launched[0].1 - 30.0).abs() < 1e-6, "{launched:?}");
    let impacts = all("impacts");
    assert_eq!(impacts.len(), 2);
    assert!(impacts.iter().all(|i| i["against_vehicle"] == 1 && i["damage"]["figures"] == "fishing_vessel"));
}
