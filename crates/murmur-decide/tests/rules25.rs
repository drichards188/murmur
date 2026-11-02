//! Decision rules 25 (bd murmur-lbw8.2, the owner's direction of 2026-11-02, DECISIONS.md
//! "Warheads and damage"): the damage model lives on the truth side, and the engine reads only
//! what our own vehicles report of themselves. A platform whose vehicle reports its weapons out of
//! action can't use its loadout, and one that is its own weapon can't strike once its propulsion
//! is lost (the `weapon` check); the check shows the weapon's catalog figures against the track's
//! estimated kind. Scenarios 121 (a firepower kill before the first signature, then a nomination
//! refused; tamper `drop_damage`) and 122 (a Switchblade that lost its propulsion).
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::engage::weapon_for;
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

/// A boat holding west of the bridge, declared hostile, estimated a small fast boat.
fn boat() -> TrackFacts {
    let at = LonLat::new(-122.4950, 37.8120);
    TrackFacts {
        position: Fact::new(at, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(Affiliation::Hostile, T0),
        affiliation_confidence: 0.8,
        identity: Some("small_fast_boat".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into()],
        source_t: [("3:radar".to_string(), T0)].into_iter().collect(),
        domain: Domain::Surface,
        last_seen_t: T0,
        declared_by: Some("op-1".into()),
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: None,
        first_seen_t: None,
        velocity_sigma_mps: None,
    }
}

const DISARMED: &str = "121-weapons-out-of-action.toml";
const IMMOBILE: &str = "122-loitering-munition-propulsion-lost.toml";

/// The scenarios end as designed and replay, and 121's hidden damage report is caught. Under
/// rules 24 the report is not read, and no rules 24 log carries it (the engine drops it).
#[test]
fn a_damaged_platform_is_refused_and_older_rules_never_log_the_report() {
    for name in [DISARMED, IMMOBILE] {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
    }
    let r = run(DISARMED, &RunOptions::default());
    assert_eq!(r.tamper_detected, Some(true), "{:?}", r.tamper_mismatches);
    assert!(r.log.contains("\"damage\":{\"value\":{\"firepower\":true}"));
    let v24 = RunOptions { rules_version: Some(LOADOUT_SINCE), ..RunOptions::default() };
    let old = run(DISARMED, &v24);
    assert!(old.replay_ok, "{:?}", old.replay_mismatches);
    assert!(!old.failed_rules.iter().any(|r| r == "weapon"), "{:?}", old.failed_rules);
    assert!(!old.log.contains("\"damage\":"), "a rules 24 log carries no damage report");
    assert!(!old.log.contains("as estimated: a hit"), "nor the weapon's figures");
}

/// The weapon check: the figures against the estimated kind (an alias's under its own name, a
/// kind it lists none for, an unclassified track); a firepower kill refuses every weapon, named
/// or picked; a mobility kill refuses only a platform that is its own weapon.
#[test]
fn the_weapon_check_shows_the_figures_and_reads_the_damage_report() {
    let ec = EngageRules::default();
    let mut w = WorldSnapshot::new(T0);
    w.vehicles.insert(uid("reaper-1"), vehicle("mq9b_armed", LonLat::new(-122.525, 37.80)));
    let mut ferry = boat();
    ferry.identity = Some("ferry".into());
    let c = weapon_for(Some(&uid("reaper-1")), None, Some(&ferry), &w, &ec).unwrap().unwrap();
    assert!(c.detail.contains("against a ferry (the cargo_ship figures), as estimated: a hit 0.95 in good conditions, and given a hit propulsion lost 0.45"), "{}", c.detail);
    let mut sub = boat();
    sub.identity = Some("diver".into());
    let c = weapon_for(Some(&uid("reaper-1")), None, Some(&sub), &w, &ec).unwrap().unwrap();
    assert!(c.detail.contains("against a diver (the person figures)"), "{}", c.detail);
    let mut drone = boat();
    drone.identity = Some("small_drone".into());
    let c = weapon_for(Some(&uid("reaper-1")), None, Some(&drone), &w, &ec).unwrap().unwrap();
    assert!(c.detail.contains("no hit or damage figures against a small_drone"), "{}", c.detail);
    let mut unknown = boat();
    unknown.identity = None;
    let c = weapon_for(Some(&uid("reaper-1")), None, Some(&unknown), &w, &ec).unwrap().unwrap();
    assert!(c.detail.contains("the target's kind is not estimated"), "{}", c.detail);
    // a firepower kill, reported 30 s ago: nothing can be used, named or picked
    let report = |mobility: bool, firepower: bool| Some(Fact::new(VehicleDamage { mobility, firepower }, T0.plus_ms(-30_000)));
    w.vehicles.get_mut(&uid("reaper-1")).unwrap().damage = report(false, true);
    let e = weapon_for(Some(&uid("reaper-1")), None, Some(&boat()), &w, &ec).unwrap().unwrap_err();
    assert_eq!(e, "the vehicle reported its weapons out of action (reported 30 s ago): no weapon of mq9b_armed's loadout can be used");
    let e = weapon_for(Some(&uid("reaper-1")), Some("GBU-12 Paveway II"), Some(&boat()), &w, &ec).unwrap().unwrap_err();
    assert!(e.ends_with(": the nomination names GBU-12 Paveway II"), "{e}");
    // a mobility kill: an aircraft whose missile flies from it may still fire; a one-way platform
    // can't reach its target
    w.vehicles.get_mut(&uid("reaper-1")).unwrap().damage = report(true, false);
    assert!(weapon_for(Some(&uid("reaper-1")), None, Some(&boat()), &w, &ec).unwrap().is_ok());
    w.vehicles.insert(uid("blade-1"), vehicle("switchblade", CRISSY));
    w.vehicles.get_mut(&uid("blade-1")).unwrap().damage = report(true, false);
    let e = weapon_for(Some(&uid("blade-1")), None, Some(&boat()), &w, &ec).unwrap().unwrap_err();
    assert!(e.contains("Switchblade 600 is the platform itself, whose vehicle reported its propulsion lost"), "{e}");
    // rules 24: none of it
    let ec24 = EngageRules { rules_version: LOADOUT_SINCE, ..EngageRules::default() };
    let c = weapon_for(Some(&uid("blade-1")), None, Some(&boat()), &w, &ec24).unwrap().unwrap();
    assert!(!c.detail.contains("as estimated"), "{}", c.detail);
}

/// Every weapon a nomination may use lists its figures, each kind one the truth side knows (a
/// contact kind or one an alias maps to), and a table that can't stand is refused.
#[test]
fn every_usable_weapon_lists_its_figures() {
    let kinds = [
        "small_fast_boat", "fishing_vessel", "sailboat", "cargo_ship", "vehicle", "truck", "small_drone", "light_aircraft", "uuv", "person",
    ];
    let c = catalog();
    let mut n = 0;
    for (id, p) in &c.platforms {
        for w in p.loadout.iter().filter(|w| w.kind.usable()) {
            assert!(!w.effects.is_empty(), "{id}: {} lists no figures", w.name);
            for k in w.effects.keys() {
                assert!(kinds.contains(&k.as_str()), "{id}: {}: {k}", w.name);
            }
            n += 1;
        }
    }
    assert_eq!(n, 8, "the Hellfire, the GBU-12, the AMRAAM, the Javelin, the three one-way platforms and (rules 26) the M2");
    let base = "[x]\ndomain = \"air\"\nsim = \"standin\"\narmed = true\n";
    let w = |effects: &str| {
        format!("{base}loadout = [{{ name = \"m\", kind = \"missile\", count = 2, min_range_m = 0, max_range_m = 1000, speed_mps = 100, profile = \"direct\", guidance = \"laser\", warhead = {{ class = \"heat\", mass_kg = 9 }}, targets = [\"surface\"], effects = {effects} }}]\n")
    };
    assert!(Catalog::from_toml(&w("{ sailboat = { p_hit = 0.8, p_kill = 0.6, p_mobility = 0.4 } }")).is_ok());
    let e = Catalog::from_toml(&w("{ sailboat = { p_hit = 0.8, p_kill = 0.7, p_mobility = 0.4 } }")).unwrap_err().to_string();
    assert!(e.contains("shares of a hit, summing to 1 at most"), "{e}");
    let e = Catalog::from_toml(&w("{ sailboat = { p_hit = 1.2 } }")).unwrap_err().to_string();
    assert!(e.contains("between 0 and 1"), "{e}");
}
