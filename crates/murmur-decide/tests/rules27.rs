//! Decision rules 27 (bd murmur-lbw8.4, the owner's direction of 2026-11-02, DECISIONS.md "Mines:
//! laid on two signatures, recorded, live for a set period"): a lay order (`Task::Lay`) is signed
//! as an engagement is, by two operators with engagement authority, never its proposer and never
//! an agent; its `minefield` check judges what the field could catch beside a declared hostile;
//! each mine laid is logged as the record, spends a round and is carried over a restart; a field
//! whose period is over is logged inert; and while it is live it is a no-go area for our own
//! vehicles it could catch (routed round, refused across, an rtl warned). Scenarios 128 (laid on
//! two signatures, recorded, inert at its period's end; tamper `move_mine`), 129 (refused for a
//! neutral car on its line) and 130 (our transit routed round our field, an rtl across it warned).
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::replay::{CarriedFrom, ReplayOptions, RulesAccepted, log_head, owed_debts, replay_str, replay_str_with};
use murmur_decide::scenario::{self, RunOptions, ScenarioResult, Tampered};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(name: &str, opts: &RunOptions) -> ScenarioResult {
    scenario::run_file(&root().join("sim/scenarios/engage").join(name), &root(), opts).unwrap()
}

const LAID: &str = "128-minefield-laid-on-two-signatures.toml";
const REFUSED: &str = "129-minefield-refused-for-traffic-on-its-line.toml";
const KEEP_OUT: &str = "130-our-field-is-a-keep-out-for-us.toml";
const MINE: &str = "TM-62M mine";

/// Every `mine.laid` of a log: its time, number, place, weapon (rounds left), arming and inert times.
fn mines(log: &str) -> Vec<(Time, u32, LonLat, WeaponRelease, Time, Time)> {
    parse_log(log)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::MineLaid { n, at, weapon, arms_t, inert_t, .. } => Some((e.t, n, at, weapon, arms_t, inert_t)),
            _ => None,
        })
        .collect()
}

/// The scenarios end as designed and replay, and their tampers are caught.
#[test]
fn the_minefield_scenarios_end_as_designed_replay_and_catch_their_tamper() {
    for name in [LAID, REFUSED, KEEP_OUT] {
        let r = run(name, &RunOptions::default());
        assert!(r.ok, "{name}: {:?}", r.failures);
        assert!(r.replay_ok, "{name}: {:?}", r.replay_mismatches);
        if r.tamper.is_some() {
            assert_eq!(r.tamper_detected, Some(true), "{name}: {:?}", r.tamper_not_applicable);
        }
    }
    let r = run(LAID, &RunOptions::default());
    assert!(
        r.tamper_mismatches.iter().any(|m| m.contains("a mine laid elsewhere than its place on the order's line")),
        "{:?}",
        r.tamper_mismatches
    );
}

/// A lay order is signed as an engagement is: the proposer and an agent can't sign it, the first
/// signer's leaves it waiting, the second approves it; the mines then go down in order along the
/// line as the layer passes, each logged with its place, its round and its live period, and the
/// field is logged inert once that period is over.
#[test]
fn a_minefield_is_laid_on_two_signatures_and_recorded() {
    let r = run(LAID, &RunOptions::default());
    // op-1, who ordered it, has no engagement authority: refused (with it, the two-person rule would)
    assert!(r.refusals.iter().any(|x| x.contains("only an operator with engagement authority may approve it")), "{:?}", r.refusals);
    assert!(r.refusals.iter().any(|x| x.contains("no approve method on AgentPort")), "{:?}", r.refusals);
    let events = parse_log(&r.log).unwrap();
    let concurred: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ApprovalConcurred { by, n: 1, of: 2, .. } => Some(by.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(concurred, vec!["op-2"]);
    assert!(events.iter().any(|e| matches!(&e.body, EventBody::ApprovalGranted { by, .. } if by.as_str() == "op-3")));
    let m = mines(&r.log);
    assert_eq!(m.len(), 10);
    let line = [LonLat::new(-122.4595, 37.7950), LonLat::new(-122.4585, 37.7950)];
    let planned = murmur_decide::mines::mine_points(&line, 10);
    for (i, (t, n, at, w, arms, inert)) in m.iter().enumerate() {
        assert_eq!((*n, *at), (i as u32 + 1, planned[i]));
        assert_eq!((w.name.as_str(), w.kind, w.left), (MINE, WeaponKind::Mine, Some(19 - i as u32)));
        assert_eq!((arms.ms_since(*t), inert.ms_since(*t)), (60_000, 180_000));
    }
    // inert once the last mine's period is over, and never before
    let inert = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::MinefieldInert { intent, mines, inert_t, .. } => Some((e.t, *intent, *mines, *inert_t)),
            _ => None,
        })
        .unwrap();
    assert_eq!((inert.1, inert.2, inert.3), (IntentId(1), 10, m[9].5));
    assert!(inert.0 >= m[9].5 && inert.0.ms_since(m[9].5) <= 1000, "{inert:?}");
    assert_eq!(r.inert.len(), 1, "{:?}", r.inert);
    // the log's debts: 10 rounds of the TM-62M spent on mole-1, and no live field left to carry
    let d = owed_debts(&r.log);
    assert_eq!(d.rounds.len(), 1);
    assert_eq!((d.rounds[0].uid.as_str(), d.rounds[0].weapon.as_str(), d.rounds[0].used), ("mole-1", MINE, 10));
    assert!(d.fields.is_empty(), "{:?}", d.fields);
    // a log that dispatched it on one signature is caught
    let Tampered::Forged(f) = scenario::tamper(&r.log, "hide_lay_signature").unwrap() else {
        panic!("nothing to forge")
    };
    assert!(!replay_str(&f).unwrap().ok());
    // and one signed by an operator without engagement authority
    let forged = forge(&r.log, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::ApprovalGranted { by, session, .. } = &mut e.body {
                *by = OperatorId::new("op-1");
                *session = SessionId(1);
            }
        }
    });
    let rr = replay_str(&forged).unwrap();
    assert!(rr.mismatches.iter().any(|m| m.what.contains("without engagement authority") || m.what.contains("not the approver's")), "{:?}", rr.mismatches);
}

/// A neutral car whose course crosses the row within the check's horizon refuses the order, saying
/// which track, how near, how soon and by how much it is short.
#[test]
fn a_lay_is_refused_for_a_neutral_track_crossing_its_line() {
    let r = run(REFUSED, &RunOptions::default());
    assert_eq!(r.final_state.as_deref(), Some("rejected"));
    assert!(r.failed_rules.iter().any(|x| x == "minefield"), "{:?}", r.failed_rules);
    let detail = parse_log(&r.log)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.body {
            EventBody::CheckResult { results, .. } => results.into_iter().find(|c| c.rule == RuleId::Minefield),
            _ => None,
        })
        .unwrap()
        .detail;
    assert!(detail.contains("T-0001 (vehicle, neutral) on its course now passes"), "{detail}");
    assert!(detail.contains("m short"), "{detail}");
    assert!(mines(&r.log).is_empty());
}

/// Our live field is a no-go area for our vehicles of the domain it catches: an rtl across it goes
/// with a warning, a transit across it is routed round it.
#[test]
fn our_field_routes_our_transit_round_it_and_warns_an_rtl_across_it() {
    let r = run(KEEP_OUT, &RunOptions::default());
    let events = parse_log(&r.log).unwrap();
    let mut warned = false;
    let mut routed = false;
    for e in &events {
        let EventBody::CheckResult { intent, results, inputs, .. } = &e.body else { continue };
        if *intent == IntentId(2) {
            let g = results.iter().find(|c| c.rule == RuleId::Geofence).unwrap();
            assert_eq!(g.outcome, Outcome::Pass);
            warned = g.warning.as_deref().is_some_and(|w| w.starts_with("rtl path crosses minefield MF-1"));
            assert_eq!(inputs.minefields.len(), 1);
        }
        if *intent == IntentId(3) {
            routed = inputs.route_plans.get(&uid("ferret-1")).into_iter().flatten().any(|p| p.met == "no-go minefield MF-1" && p.no_route.is_none());
        }
    }
    assert!(warned && routed, "warned {warned}, routed {routed}");
}

/// A live field is carried over a restart with the rounds it spent: the next run reads it in its
/// decisions (a transit routed round it), and replay with the earlier log beside it agrees.
#[test]
fn a_live_field_is_carried_over_a_restart() {
    let a = run(KEEP_OUT, &RunOptions::default()).log;
    let (head, seq) = log_head(&a).unwrap();
    let d = owed_debts(&a);
    assert_eq!(d.fields.len(), 1);
    let carry = CarriedFrom {
        log: "a.jsonl".into(),
        head: Some(head),
        last_seq: Some(seq),
        owed: d.owed,
        pending: d.pending,
        spent: d.spent,
        rounds: d.rounds,
        fields: d.fields,
        ..Default::default()
    };
    let t = T0.plus_ms(200_000);
    let clock = ManualClock::new(t);
    let log = MemorySink::new();
    let mut e = Engine::start(policy(), None, roster(), vec![carry], Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let fields = e.minefields();
    assert_eq!(fields.len(), 1);
    assert_eq!((fields[0].from.as_deref(), fields[0].mines.len()), (Some("a.jsonl"), 10));
    assert_eq!(e.platform_rounds(&uid("mole-1"), None).spent(MINE), 10);
    // a recon UGV south of the row sent to orbit north of it: routed round the carried field
    let mut w = WorldSnapshot::new(t);
    w.areas = Some(areas());
    let mut v = vehicle("ugv_recon", LonLat::new(-122.4590, 37.7935));
    v.home = Some(Fact::new(LonLat::new(-122.4590, 37.7990), t));
    for f in [&mut v.position, &mut v.home] {
        if let Some(f) = f.as_mut() {
            f.t = t;
        }
    }
    v.autopilot = v.autopilot.map(|mut f| {
        f.t = t;
        f
    });
    v.nav_ready = Some(Fact::new(true, t));
    v.battery_pct = Some(Fact::new(95.0, t));
    w.vehicles.insert(uid("ferret-1"), v);
    let id = e
        .agent_port(AgentId::new("agent-planner"))
        .propose(orbit(LonLat::new(-122.4590, 37.7980), 40.0, &["ferret-1"]), rationale("look north of the row"), &w, &terrain())
        .unwrap()
        .id();
    let checks = e.intent(id).unwrap().checks().to_vec();
    let g = checks.iter().find(|c| c.rule == RuleId::Geofence).unwrap();
    assert_eq!(g.outcome, Outcome::Pass, "{g:?}");
    assert!(g.detail.contains("routed around no-go minefield MF-1 of a.jsonl"), "{}", g.detail);
    let logs = [("a.jsonl", a.as_str())];
    let r = replay_str_with(&log.text(), &ReplayOptions { carried_logs: &logs, ..ReplayOptions::default() }).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// Rules 26 logs replay unchanged: the fixtures of rules 26 under their own rules; a lay order run
/// under rules 26 is refused (no build of theirs takes one) and its log carries none of rules 27's
/// markers; and a rules 27 log relabelled rules 26 is caught.
#[test]
fn rules_26_logs_replay_unchanged() {
    for name in ["v26-engagement-scenario-01.jsonl", "v26-gun-bursts-until-the-target-ceases-scenario-123.jsonl"] {
        let text = std::fs::read_to_string(root().join("crates/murmur-decide/tests/data/audit").join(name)).unwrap();
        let r = replay_str_with(&text, &ReplayOptions { rules: RulesAccepted::Exactly(GUN_SINCE), ..ReplayOptions::default() }).unwrap();
        assert!(r.ok(), "{name}: {:?}", r.mismatches);
    }
    let v26 = RunOptions { rules_version: Some(GUN_SINCE), ..RunOptions::default() };
    let then = run(LAID, &v26);
    assert!(then.replay_ok, "{:?}", then.replay_mismatches);
    assert!(then.refusals.iter().any(|x| x.contains("these decision rules lay no mines")), "{:?}", then.refusals);
    for marker in ["mine.laid", "minefield.inert", "\"kind\":\"lay\"", "\"minefields\":", "\"rule\":\"minefield\""] {
        assert!(!then.log.contains(marker), "rules 26 logs {marker}");
    }
    let now = run(LAID, &RunOptions::default());
    let relabelled = forge(&now.log, |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(GUN_SINCE);
        }
    });
    let rr = replay_str_with(&relabelled, &ReplayOptions { rules: RulesAccepted::Exactly(GUN_SINCE), ..ReplayOptions::default() }).unwrap();
    assert!(!rr.ok());
    assert!(replay_str(&now.log).unwrap().ok());
}
