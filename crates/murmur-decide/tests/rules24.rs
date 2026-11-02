//! Decision rules 24 (bd murmur-lbw8.1, the owner's direction of 2026-11-02, DECISIONS.md "Real
//! weapon loadouts on armed platforms"): an armed platform's catalog loadout. An engagement names
//! the weapon it uses, or the engine picks the first of the loadout usable against the target with
//! a round left and says why (the `weapon` check, the preview's `weapon`, frozen with the plan); a
//! missile, bomb or torpedo is released within its minimum and maximum slant range at its own
//! speed (rules 20's flight); each release of a counted weapon spends a round, logged with the
//! `engage.effect` and as a decision fact (`rounds`), recomputed by replay, and carried over a
//! restart onto the same vehicle. Scenarios 119 (two bombs named and spent, a third refused, the
//! Hellfire picked) and 120 (inside the Hellfire's minimum range: never released); 106 and 107
//! (rules 20's) now run on the MQ-9B's loadout.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::engage::{self, weapon_for};
use murmur_decide::geo::haversine_m;
use murmur_decide::replay::{CarriedFrom, ReplayOptions, RulesAccepted, log_head, owed_debts, replay_str, replay_str_with};
use murmur_decide::scenario::{self, RunOptions, ScenarioResult};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

const SPENT: &str = "119-rounds-spent-then-another-weapon.toml";
const MIN_RANGE: &str = "120-inside-the-weapons-minimum-range.toml";
const RELEASED_EARLY: &str = "106-standoff-missile-released-early.toml";
const OUT_OF_RANGE: &str = "107-standoff-missile-out-of-range.toml";

fn v23() -> RunOptions {
    RunOptions {
        rules_version: Some(IDENTIFY_OWN_LOOK_SINCE),
        ..RunOptions::default()
    }
}

/// Every `engage.effect` of a log: its time, munition and weapon.
fn effects(log: &str) -> Vec<(Time, Option<engage::MunitionFlight>, Option<WeaponRelease>)> {
    parse_log(log)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::EngageEffect { munition, weapon, .. } => Some((e.t, munition, weapon)),
            _ => None,
        })
        .collect()
}

/// The scenarios end as designed and replay, and 119's forged rounds are caught.
#[test]
fn the_loadout_scenarios_end_as_designed_replay_and_catch_their_tamper() {
    for name in [SPENT, MIN_RANGE, RELEASED_EARLY, OUT_OF_RANGE] {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        if r.tamper.is_some() {
            assert_eq!(r.tamper_detected, Some(true), "{name}: {:?}", r.tamper_not_applicable);
        }
    }
    let r = run(SPENT, &RunOptions::default());
    assert!(
        r.tamper_mismatches.iter().any(|m| m.contains("rounds left")),
        "{:?}",
        r.tamper_mismatches
    );
}

/// A weapon named is the one released: the GBU-12 flies at its own speed from within its own
/// range, each release spends a round (2, then 1 of 2 left, then none), and the decision facts
/// carry the rounds the ledger counts.
#[test]
fn a_named_weapon_is_released_and_each_release_spends_a_round() {
    let r = run(SPENT, &RunOptions::default());
    let fx = effects(&r.log);
    assert_eq!(fx.len(), 2, "{fx:?}");
    for (n, (t, m, w)) in fx.iter().enumerate() {
        let w = w.as_ref().unwrap();
        assert_eq!((w.name.as_str(), w.kind, w.left), ("GBU-12 Paveway II", WeaponKind::Bomb, Some(1 - n as u32)));
        let m = m.as_ref().unwrap();
        assert_eq!((m.name.as_str(), m.speed_mps, m.launch_t), ("GBU-12 Paveway II", 200.0, *t));
        let slant = haversine_m(m.launch_point, m.impact_point).hypot(m.launch_alt_m - m.impact_alt_m);
        assert!((1000.0..=14_800.0).contains(&slant), "released from {slant:.0} m");
        assert!((m.tof_s - slant / 200.0).abs() < 0.001, "{} s for {slant:.0} m", m.tof_s);
    }
    // the second nomination's decision read one round spent; the third's, both
    let rounds: Vec<u32> = parse_log(&r.log)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::CheckResult { inputs, .. } => inputs.rounds.get(&Uid::new("reaper-1")).map(|p| p.spent("GBU-12 Paveway II")),
            _ => None,
        })
        .collect();
    assert!(rounds.contains(&1) && rounds.contains(&2), "{rounds:?}");
    // what the log still had spent at its end: two bombs, on reaper-1
    let d = owed_debts(&r.log);
    assert_eq!(d.rounds.len(), 1, "{:?}", d.rounds);
    assert_eq!((d.rounds[0].uid.as_str(), d.rounds[0].weapon.as_str(), d.rounds[0].used), ("reaper-1", "GBU-12 Paveway II", 2));
}

/// None named: the engine picks the first of the loadout usable against the target with a round
/// left (106: the Hellfire, 3 of 4 left after it), and the preview frozen with the plan names it.
#[test]
fn an_unnamed_weapon_is_picked_and_frozen_with_the_plan() {
    let r = run(RELEASED_EARLY, &RunOptions::default());
    let fx = effects(&r.log);
    let (_, m, w) = &fx[0];
    let w = w.as_ref().unwrap();
    assert_eq!((w.name.as_str(), w.left), ("AGM-114R Hellfire II", Some(3)));
    assert_eq!(m.as_ref().unwrap().name, "AGM-114R Hellfire II");
    let events = parse_log(&r.log).unwrap();
    let picked = events.iter().any(|e| match &e.body {
        EventBody::CheckResult { results, .. } => results
            .iter()
            .any(|c| c.rule == RuleId::Weapon && c.outcome == Outcome::Pass && c.detail.starts_with("picked (none named): AGM-114R Hellfire II")),
        _ => false,
    });
    assert!(picked, "the nomination's weapon check says the engine picked it");
    let plan = events.iter().find_map(|e| match &e.body {
        EventBody::ApprovalGranted { snapshot: Some(s), .. } => Some(s.preview.clone()),
        _ => None,
    });
    assert_eq!(plan.unwrap().weapon.as_deref(), Some("AGM-114R Hellfire II"));
    // 107 (9,000 m up): the Hellfire's maximum range, as rules 20's munition's
    let high = run(OUT_OF_RANGE, &RunOptions::default());
    assert_eq!(high.final_state.as_deref(), Some("aborted"));
    assert!(effects(&high.log).is_empty());
}

/// Inside the weapon's minimum range nothing is released, and the arrival check says the range
/// from its minimum; rules 23 knew no minimum and released its munition over the point.
#[test]
fn inside_the_minimum_range_nothing_is_released() {
    let r = run(MIN_RANGE, &RunOptions::default());
    assert_eq!(r.final_state.as_deref(), Some("aborted"), "{:?}", r.final_reason);
    assert!(effects(&r.log).is_empty());
    let said = parse_log(&r.log).unwrap().into_iter().any(|e| match e.body {
        EventBody::EngagePicture { results, .. } => results.iter().any(|c| {
            c.rule == RuleId::Arrival
                && c.outcome == Outcome::Fail
                && c.detail.contains("the AGM-114R Hellfire II reaches from 500 to 8000 m")
        }),
        _ => false,
    });
    assert!(said, "the arrival check names the minimum range");
    let then = run(MIN_RANGE, &v23());
    assert!(then.replay_ok, "{:?}", then.replay_mismatches);
    let fx = effects(&then.log);
    assert_eq!(fx.len(), 1, "rules 23 releases it");
    assert_eq!(fx[0].1.as_ref().unwrap().name, "Hellfire-class missile");
    assert!(fx[0].2.is_none());
}

/// A boat holding off Lands End, declared hostile, for a dry run of a nomination.
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

fn reaper_world(who: Option<VehicleIdentity>) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let mut v = vehicle("mq9b_armed", LonLat::new(-122.5250, 37.8000));
    v.alt_msl_m = Some(Fact::new(7600.0, T0));
    w.vehicles.insert(uid("reaper-1"), v);
    w.tracks.insert("T-0001".into(), boat());
    if let Some(i) = who {
        w.identities.insert(uid("reaper-1"), i);
    }
    w
}

fn nomination(weapon: Option<&str>) -> IntentSpec {
    IntentSpec {
        id: IntentId(1),
        created_t: T0,
        source: human(),
        task: Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Return,
            weapon: weapon.map(str::to_string),
        },
        uids: vec![uid("reaper-1")],
        constraints: Default::default(),
        rationale: None,
    }
}

fn policy_on(version: u32) -> Policy {
    let mut p = policy();
    p.rules.rules_version = version;
    p
}

/// The weapon check a nomination would get on `e` (its dry run: the engine's facts filled).
fn weapon_check(e: &Engine, spec: &IntentSpec, w: &WorldSnapshot) -> (Option<PlatformRounds>, Option<CheckResult>) {
    let inputs = e.dry_run_inputs(spec, w, &terrain());
    let c = engage::evaluate(spec, &inputs, &e.policy().rules).into_iter().find(|c| c.rule == RuleId::Weapon);
    (inputs.rounds.get(&uid("reaper-1")).cloned(), c)
}

/// The rounds a log released stay spent in the next run (`run.started`'s `carried_from`,
/// `rounds`), on the same vehicle only; replay checks them against that log; a rules 23 start
/// carries none.
#[test]
fn rounds_are_carried_over_a_restart_onto_the_same_vehicle() {
    let a = run(SPENT, &RunOptions::default()).log;
    let (head, seq) = log_head(&a).unwrap();
    let d = owed_debts(&a);
    let carry = CarriedFrom {
        log: "a.jsonl".into(),
        head: Some(head),
        last_seq: Some(seq),
        owed: d.owed,
        pending: d.pending,
        spent: d.spent,
        rounds: d.rounds.clone(),
        ..Default::default()
    };
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::start(policy(), None, roster(), vec![carry.clone()], Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    // the rounds as the console reads them, and as the nomination's dry run does
    let p = e.platform_rounds(&uid("reaper-1"), None);
    assert_eq!(p.spent("GBU-12 Paveway II"), 2);
    let w = reaper_world(None);
    let (rounds, c) = weapon_check(&e, &nomination(Some("GBU-12 Paveway II")), &w);
    assert_eq!(rounds.unwrap().carried[0].from, "a.jsonl");
    let c = c.unwrap();
    assert_eq!(c.outcome, Outcome::Fail, "{c:?}");
    assert!(c.detail.contains("GBU-12 Paveway II has no round left (2 of 2 released)"), "{}", c.detail);
    let (_, c) = weapon_check(&e, &nomination(None), &w);
    assert!(c.unwrap().detail.starts_with("picked (none named): AGM-114R Hellfire II"));
    // a nomination naming it, logged: refused by the weapon check
    declare_shown(&mut e, &w);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let mut prop = Proposal::new(nomination(Some("GBU-12 Paveway II")).task, vec![uid("reaper-1")]);
    prop.rationale = None;
    let i = e.submit(op.authority(), prop, &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::Rejected, "{:?}", i.history());
    let failed: Vec<&CheckResult> = i.checks().iter().filter(|c| c.outcome != Outcome::Pass).collect();
    assert!(
        failed.iter().any(|c| c.rule == RuleId::Weapon && c.detail.contains("no round left")),
        "{failed:?}"
    );
    let b = log.text();
    assert!(b.contains("\"rounds\":["), "run.started carries the rounds");
    // replay with the earlier log beside it: what it carried is what that log had released
    let logs = [("a.jsonl", a.as_str())];
    let opts = ReplayOptions {
        carried_logs: &logs,
        ..ReplayOptions::default()
    };
    let r = replay_str_with(&b, &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // without it, not verified; with the rounds forged (one bomb), caught
    assert!(!replay_str(&b).unwrap().ok());
    let forged = forge(&b, |ev| {
        if let EventBody::RunStarted { carried_from, .. } = &mut ev[0].body {
            carried_from[0].rounds[0].used = 1;
        }
    });
    let r = replay_str_with(&forged, &opts).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("rounds carried as released differ")), "{:?}", r.mismatches);
    // on the vehicle they were released from only: another under the uid does not count them
    let mut known = carry.clone();
    let reaper = VehicleIdentity {
        platform: Some("mq9b_armed".into()),
        callsign: Some("REAPER 1".into()),
        sysid: None,
        home: None,
    };
    known.rounds[0].vehicle = Some(reaper.clone());
    let e2 = Engine::start(policy(), None, roster(), vec![known], Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let (_, c) = weapon_check(&e2, &nomination(Some("GBU-12 Paveway II")), &reaper_world(Some(reaper.clone())));
    assert_eq!(c.unwrap().outcome, Outcome::Fail);
    let mut other = reaper;
    other.callsign = Some("REAPER 2".into());
    let (rounds, c) = weapon_check(&e2, &nomination(Some("GBU-12 Paveway II")), &reaper_world(Some(other)));
    assert!(rounds.unwrap().carried[0].other_vehicle.is_some());
    let c = c.unwrap();
    assert_eq!(c.outcome, Outcome::Pass, "{c:?}");
    assert!(c.detail.contains("2 of 2 rounds left"), "{}", c.detail);
    // rules 23 had no loadout: its start carries no rounds, and no weapon is checked
    let log23 = MemorySink::new();
    let e23 = Engine::start(policy_on(IDENTIFY_OWN_LOOK_SINCE), None, roster(), vec![carry], Box::new(clock.clone()), Box::new(log23.clone())).unwrap();
    assert!(!log23.text().contains("\"rounds\":"));
    assert_eq!(weapon_check(&e23, &nomination(None), &reaper_world(None)), (None, None));
}

/// A rules 23 log replays unchanged: every fixture of rules 20 to 23 under its own rules, and a
/// scenario run under rules 23 today logs no weapon, no rounds and no weapon check.
#[test]
fn rules_23_logs_replay_unchanged() {
    for (name, version) in [
        ("v20-engagement-scenario-01.jsonl", MUNITION_FLIGHT_SINCE),
        ("v20-standoff-missile-released-early-scenario-106.jsonl", MUNITION_FLIGHT_SINCE),
        ("v23-engagement-scenario-01.jsonl", IDENTIFY_OWN_LOOK_SINCE),
        ("v23-own-look-not-ended-by-another-camera-scenario-118.jsonl", IDENTIFY_OWN_LOOK_SINCE),
    ] {
        let text = std::fs::read_to_string(root().join("crates/murmur-decide/tests/data/audit").join(name)).unwrap();
        let r = replay_str_with(
            &text,
            &ReplayOptions {
                rules: RulesAccepted::Exactly(version),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
    }
    for name in [RELEASED_EARLY, SPENT] {
        let r = run(name, &v23());
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        for marker in ["\"weapon\":\"", "\"weapon\":{", "\"rounds\":", "\"rule\":\"weapon\""] {
            assert!(!r.log.contains(marker), "{name} under rules 23 logs {marker}");
        }
    }
    // and a rules 23 log that claims a weapon is caught (a marker of rules 24)
    let r = run(RELEASED_EARLY, &RunOptions::default());
    let relabelled = forge(&r.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(IDENTIFY_OWN_LOOK_SINCE);
        }
    });
    let rr = replay_str_with(
        &relabelled,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(IDENTIFY_OWN_LOOK_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(!rr.ok());
}

/// The catalog: every platform armed in real life carries its loadout (guns and mines listed,
/// inert); the CCA's missile is for air targets only; a loadout that can't stand is refused.
#[test]
fn the_catalog_lists_the_loadouts_and_the_cca_fights_only_in_the_air() {
    let c = catalog();
    let with: Vec<&str> = c.platforms.iter().filter(|(_, p)| !p.loadout.is_empty()).map(|(id, _)| id.as_str()).collect();
    assert_eq!(with, vec!["cca", "fpv", "magura", "mq9b_armed", "orca", "switchblade", "ugv_armed", "ugv_mines"]);
    // armed and usable now, or listed and inert (not armed until stage 4)
    for (id, p) in &c.platforms {
        if p.loadout.iter().any(|w| w.kind.usable()) {
            assert!(p.armed, "{id}: a usable weapon on an unarmed platform");
        }
    }
    assert!(!c.get("orca").unwrap().armed && !c.get("ugv_mines").unwrap().armed);
    // the frozen worlds' platforms keep no loadout
    for id in ["mq9b", "mq9b_radar", "seahunter", "seahunter_radar", "puma", "magura_pre_loadout"] {
        assert!(c.get(id).unwrap().loadout.is_empty(), "{id}");
    }
    // the CCA's AMRAAM against a boat: refused, saying why; against an aircraft: used
    let ec = EngageRules::default();
    let mut w = WorldSnapshot::new(T0);
    w.vehicles.insert(uid("fury-1"), vehicle("cca", LonLat::new(-122.6, 37.8)));
    let e = weapon_for(Some(&uid("fury-1")), None, Some(&boat()), &w, &ec).unwrap().unwrap_err();
    assert!(e.contains("AIM-120C AMRAAM is for air targets, not surface"), "{e}");
    let mut plane = boat();
    plane.domain = Domain::Air;
    let ok = weapon_for(Some(&uid("fury-1")), None, Some(&plane), &w, &ec).unwrap().unwrap();
    assert_eq!(ok.weapon.name, "AIM-120C AMRAAM");
    // the armed UGV: its gun inert, its Javelin used
    w.vehicles.insert(uid("bull-1"), vehicle("ugv_armed", CRISSY));
    let mut truck = boat();
    truck.domain = Domain::Ground;
    let j = weapon_for(Some(&uid("bull-1")), None, Some(&truck), &w, &ec).unwrap().unwrap();
    assert_eq!(j.weapon.name, "FGM-148 Javelin");
    assert!(j.detail.contains("passed over: M2 .50 HMG is a gun"), "{}", j.detail);
    let g = weapon_for(Some(&uid("bull-1")), Some("M2 .50 HMG"), Some(&truck), &w, &ec).unwrap().unwrap_err();
    assert!(g.contains("murmur-lbw8.3"), "{g}");
    // a platform without a loadout and none named: no weapon check (rules 23's abstract effect)
    w.vehicles.insert(uid("viper-1"), vehicle("magura_pre_loadout", BAY));
    assert!(weapon_for(Some(&uid("viper-1")), None, Some(&boat()), &w, &ec).is_none());
    assert!(weapon_for(Some(&uid("viper-1")), Some("x"), Some(&boat()), &w, &ec).unwrap().is_err());
    // bad entries
    let base = "[x]\ndomain = \"air\"\nsim = \"standin\"\narmed = true\n";
    let ok_w = "{ name = \"m\", kind = \"missile\", count = 2, min_range_m = 0, max_range_m = 1000, speed_mps = 100, profile = \"direct\", guidance = \"laser\", warhead = { class = \"heat\", mass_kg = 9 }, targets = [\"surface\"] }";
    assert!(Catalog::from_toml(&format!("{base}loadout = [{ok_w}]\n")).is_ok());
    for (bad, why) in [
        (format!("loadout = [{ok_w}, {ok_w}]"), "named twice"),
        (ok_w.replace("count = 2", "count = 0").to_string(), "count is at least 1"),
        (ok_w.replace("min_range_m = 0", "min_range_m = 2000"), "min_range_m < max_range_m"),
        (ok_w.replace("kind = \"missile\"", "kind = \"self\""), "a self strike"),
        (ok_w.replace("targets = [\"surface\"]", "targets = []"), "at least one domain"),
        (ok_w.replace("kind = \"missile\"", "kind = \"laser\""), "unknown variant"),
    ] {
        let line = if bad.starts_with("loadout") { bad.clone() } else { format!("loadout = [{bad}]") };
        let e = Catalog::from_toml(&format!("{base}{line}\n")).unwrap_err().to_string();
        assert!(e.contains(why), "{line}: {e}");
    }
}
