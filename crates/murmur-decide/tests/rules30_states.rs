//! Rules 30 ([`NODE_LOSS_SINCE`], docs/NODE-LOSS.md sections 2 and 4, bd murmur-zb1d.3): a vehicle
//! of ours that stops reporting is presumed lost on logged evidence, lost only when a person with
//! engagement authority confirms it on the silence they saw, and a report from it undoes the loss
//! before any decision reads that report. The scenarios 139 to 142 (`sim/scenarios/engage/`) play
//! the chain end to end; these tests take the engine's API one rule at a time.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::{T0, uid};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// An engine deciding under rules 30 (the rules being built), with the areas loaded.
fn engine30(clock: &ManualClock) -> (Engine, MemorySink) {
    let mut p = common::policy();
    p.rules.rules_version = NODE_LOSS_SINCE;
    let log = MemorySink::new();
    let e = Engine::with_areas(p, Some(common::areas()), common::roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    (e, log)
}

/// The fixture world at `t`, with every vehicle reporting then but `silent`, whose facts stay at
/// `T0`.
fn world_at(t: Time, silent: &[&str]) -> WorldSnapshot {
    let mut w = common::world();
    w.t = t;
    for (u, v) in w.vehicles.iter_mut() {
        if silent.contains(&u.as_str()) {
            continue;
        }
        if let Some(f) = v.autopilot.as_mut() {
            f.t = t;
        }
        if let Some(f) = v.position.as_mut() {
            f.t = t;
        }
        if let Some(f) = v.nav_ready.as_mut() {
            f.t = t;
        }
        if let Some(f) = v.battery_pct.as_mut() {
            f.t = t;
        }
    }
    w
}

fn replayed(text: &str) -> replay::ReplayReport {
    replay::replay_str(text).unwrap()
}

fn whats(r: &replay::ReplayReport) -> Vec<String> {
    r.mismatches.iter().map(|m| m.what.clone()).collect()
}

/// Only an operator with engagement authority confirms a loss, on exactly the silence the review
/// showed, with a note when nothing presumes it lost (decisions D1, D2); the confirmation and what
/// it read replay.
#[test]
fn only_engagement_authority_confirms_on_the_silence_it_saw() {
    let clock = ManualClock::new(T0.plus_ms(60_000));
    let (mut e, log) = engine30(&clock);
    let plain = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    assert!(plain.engagement_authority().is_none(), "a session without the grant carries no engagement authority");
    let s = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let by = s.engagement_authority().unwrap();
    let w = world_at(T0.plus_ms(60_000), &["boat-1"]);
    let refuse = |r: Result<LossOutcome, EngineError>| match r {
        Err(EngineError::NodeLoss(why)) => why,
        other => panic!("{other:?}"),
    };
    // another report than the one it went silent after
    let why = refuse(e.confirm_lost(by, uid("boat-1"), Some(T0.plus_ms(1_000)), "sunk", &w, None));
    assert!(why.contains("accept what the check shows"), "{why}");
    // nothing presumes it lost and no note says why
    let why = refuse(e.confirm_lost(by, uid("boat-1"), Some(T0), "  ", &w, None));
    assert!(why.contains("nothing presumes it lost"), "{why}");
    // a vehicle that is not silent
    let why = refuse(e.confirm_lost(by, uid("sky-1"), Some(T0.plus_ms(60_000)), "gone", &w, None));
    assert!(why.contains("fresh"), "{why}");
    let before = log.lines().len();
    let out = e.confirm_lost(by, uid("boat-1"), Some(T0), "its crew saw it go down", &w, None).unwrap();
    assert_eq!(out, LossOutcome::default(), "nothing waited for it, nothing was owed");
    assert_eq!(log.lines().len(), before + 1, "node.lost alone");
    assert!(log.lines().last().unwrap().contains("\"type\":\"node.lost\""));
    assert!(e.node_lost(&uid("boat-1")).is_some());
    let why = refuse(e.confirm_lost(by, uid("boat-1"), Some(T0), "again", &w, None));
    assert!(why.contains("already"), "{why}");
    let r = replayed(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.nodes_lost.len(), 1, "{:?}", r.nodes_lost);
    assert!(r.nodes_lost[0].contains("confirmed lost by op-2") && r.nodes_lost[0].contains("the operator's own"), "{:?}", r.nodes_lost);

    // rules 29 has no loss of a node: refused, and nothing in the ledger fills a decision
    let clock = ManualClock::new(T0.plus_ms(60_000));
    let mut p29 = common::policy();
    p29.rules.rules_version = HANDOVER_SINCE;
    let mut e29 = Engine::with_areas(p29, Some(common::areas()), common::roster(), Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let s = e29.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let why = refuse(e29.confirm_lost(s.engagement_authority().unwrap(), uid("boat-1"), Some(T0), "sunk", &w, None));
    assert!(why.contains("rules 30"), "{why}");
    assert_eq!(e29.step_nodes(&w).unwrap(), NodeStep::default());
}

/// A report from a vehicle confirmed lost revokes the loss before any decision reads that report,
/// even one the bridge's tick never stepped (`node.loss_revoked` right before the decision that
/// read it), and the order is judged on it as on any vehicle. Cut the revocation, and replay finds
/// the decision that read the report with the loss still in force.
#[test]
fn a_report_revokes_the_loss_before_any_decision_reads_it() {
    let clock = ManualClock::new(T0.plus_ms(60_000));
    let (mut e, log) = engine30(&clock);
    let s = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let w = world_at(T0.plus_ms(60_000), &["boat-1"]);
    e.confirm_lost(s.engagement_authority().unwrap(), uid("boat-1"), Some(T0), "its crew saw it go down", &w, None)
        .unwrap();
    // lost: a hold for it is refused (no one can command a lost vehicle)
    let held = e.submit(s.authority(), common::hold(&["boat-1"]), &w, &common::terrain()).unwrap().id();
    let held = e.intent(held).unwrap();
    assert_eq!(held.state(), State::Rejected);
    assert!(held.checks().iter().any(|c| c.rule == RuleId::Readiness && c.detail.contains("confirmed lost")), "{:?}", held.checks());
    // it reports again a minute later: the next decision reads it, and the loss is revoked first
    clock.set(T0.plus_ms(120_000));
    let heard = world_at(T0.plus_ms(120_000), &[]);
    let id = e.submit(s.authority(), common::hold(&["boat-1"]), &heard, &common::terrain()).unwrap().id();
    assert_eq!(e.intent(id).unwrap().state(), State::AutoApproved, "{:?}", e.intent(id).unwrap().checks());
    assert!(e.node_lost(&uid("boat-1")).is_none());
    let lines = log.lines();
    let revoked = lines.iter().position(|l| l.contains("\"type\":\"node.loss_revoked\"")).unwrap();
    let decided = lines.iter().rposition(|l| l.contains("\"type\":\"check.result\"")).unwrap();
    assert!(revoked < decided, "the revocation is logged before the decision that reads the report");
    assert!(lines[revoked].contains("\"kind\":\"heard\""), "{}", lines[revoked]);
    let r = replayed(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.losses_revoked.len(), 1, "{:?}", r.losses_revoked);
    // the revocation cut: the decision read a report from a vehicle still lost on the log
    let forged = common::forge(&log.text(), |ev| ev.retain(|x| !matches!(x.body, EventBody::NodeLossRevoked { .. })));
    let w = whats(&replayed(&forged));
    assert!(w.iter().any(|m| m.starts_with("a decision read a report from a vehicle confirmed lost")), "{w:?}");
}

/// A presumption is a flag that decides nothing: a silent vehicle with a struck report within the
/// window of its last report is presumed lost (its report cited by seq), one whose strike is far
/// from its last report is not, silence alone presumes nothing, and a report clears it.
#[test]
fn a_presumption_rests_on_logged_evidence_and_decides_nothing() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine30(&clock);
    // boat-1 struck right as it went silent (its last report at T0); remus-1 struck 40 s into its
    // silence, beyond the window; buoy-1 only silent
    e.record_vehicle_event(&uid("boat-1"), nodes::STRUCK_EVENT, "struck (W-1)").unwrap();
    clock.set(T0.plus_ms(40_000));
    e.record_vehicle_event(&uid("remus-1"), nodes::STRUCK_EVENT, "struck (W-2)").unwrap();
    clock.set(T0.plus_ms(45_000));
    let w = world_at(T0.plus_ms(45_000), &["boat-1", "remus-1", "buoy-1"]);
    let step = e.step_nodes(&w).unwrap();
    assert_eq!(step.presumed, vec![uid("boat-1")], "{step:?}");
    let p = e.nodes_presumed().get(&uid("boat-1")).unwrap();
    assert_eq!(p.evidence.len(), 1);
    assert_eq!((p.evidence[0].kind, p.evidence[0].seq), (EvidenceKind::Struck, Some(2)));
    // stepped again: nothing new
    assert_eq!(e.step_nodes(&w).unwrap(), NodeStep::default());
    // a presumption does not leave it out of anything: a hold for it is still judged as for any
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let id = e.submit(s.authority(), common::hold(&["boat-1"]), &w, &common::terrain()).unwrap().id();
    assert!(!e.intent(id).unwrap().checks().iter().any(|c| c.detail.contains("confirmed lost")));
    // heard again: cleared
    clock.set(T0.plus_ms(50_000));
    let heard = world_at(T0.plus_ms(50_000), &["remus-1", "buoy-1"]);
    let step = e.step_nodes(&heard).unwrap();
    assert_eq!(step.cleared, vec![uid("boat-1")], "{step:?}");
    let r = replayed(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.nodes_presumed.len(), 2, "{:?}", r.nodes_presumed);
    // a presumption forged without its evidence is caught
    let forged = common::forge(&log.text(), |ev| {
        for x in ev.iter_mut() {
            if let EventBody::NodePresumed { evidence, .. } = &mut x.body {
                evidence.clear();
            }
        }
    });
    let w = whats(&replayed(&forged));
    assert!(w.iter().any(|m| m.starts_with("a presumption's evidence differs")), "{w:?}");
}

/// An order dispatched on two vehicles goes on while one of them is lost (its op ends `lost`) and
/// fails once neither is left on it; replay checks each consequence follows the loss at once.
#[test]
fn a_dispatched_order_ends_when_no_vehicle_is_left_on_it() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine30(&clock);
    let s = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let w = common::world();
    let id = e
        .submit(s.authority(), common::orbit(common::BAY, 300.0, &["sky-1", "blade-1"]), &w, &common::terrain())
        .unwrap()
        .id();
    if e.intent(id).unwrap().state() == State::PendingApproval {
        let o = e.open_operator_session(OperatorId::new("op-1")).unwrap();
        e.approve(id, o.authority(), None, &w, &common::terrain()).unwrap();
    }
    e.dispatch(id, &w).unwrap();
    assert_eq!(e.intent(id).unwrap().state(), State::Dispatched);
    clock.set(T0.plus_ms(60_000));
    let silent = world_at(T0.plus_ms(60_000), &["sky-1", "blade-1"]);
    let by = s.engagement_authority().unwrap();
    let out = e.confirm_lost(by, uid("sky-1"), Some(T0), "seen to crash", &silent, None).unwrap();
    assert_eq!((out.ended.clone(), out.finished.clone()), (vec![id], vec![]), "{out:?}");
    assert_eq!(e.intent(id).unwrap().state(), State::Dispatched, "blade-1 is still on it");
    let out = e.confirm_lost(by, uid("blade-1"), Some(T0), "seen to crash", &silent, None).unwrap();
    assert_eq!((out.ended, out.finished), (vec![id], vec![id]));
    assert_eq!(e.intent(id).unwrap().state(), State::Failed);
    let r = replayed(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
    // the op's end cut: the loss's consequences do not follow it
    let forged = common::forge(&log.text(), |ev| {
        let at = ev.iter().position(|x| matches!(&x.body, EventBody::OpState { state, .. } if state == nodes::LOST_OP)).unwrap();
        ev.remove(at);
    });
    let w = whats(&replayed(&forged));
    assert!(w.iter().any(|m| m.starts_with("a confirmed loss's consequences do not follow it")), "{w:?}");
}

/// The lost-link values owed back to a vehicle close with its loss (`restore.closed` lost, never
/// retried for ever) and are owed back again once the loss is revoked, as after an un-spend.
#[test]
fn owed_values_close_with_the_loss_and_come_back_with_its_revocation() {
    let clock = ManualClock::new(T0.plus_ms(60_000));
    let mut p = common::policy();
    p.rules.rules_version = NODE_LOSS_SINCE;
    let log = MemorySink::new();
    let values: BTreeMap<String, f64> = [("FS_ACTION".to_string(), 1.0), ("FS_GCS_TIMEOUT".to_string(), 30.0)].into();
    let carried = replay::CarriedFrom {
        log: "earlier.jsonl".into(),
        owed: vec![replay::OwedRestore {
            uid: uid("boat-1"),
            values: values.clone(),
            by: Vec::new(),
            vehicle: None,
        }],
        ..Default::default()
    };
    let mut e = Engine::start(p, Some(common::areas()), common::roster(), vec![carried], Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    assert!(e.restore_owed_to(&uid("boat-1")));
    let s = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let w = world_at(T0.plus_ms(60_000), &["boat-1"]);
    let out = e.confirm_lost(s.engagement_authority().unwrap(), uid("boat-1"), Some(T0), "sunk", &w, None).unwrap();
    assert_eq!(out.closed, Some(values));
    assert!(!e.restore_owed_to(&uid("boat-1")));
    // any operator may revoke it, saying why
    let o = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    assert!(matches!(e.revoke_lost(o.authority(), &uid("boat-1"), " "), Err(EngineError::NodeLoss(_))));
    e.revoke_lost(o.authority(), &uid("boat-1"), "it was a radio fault: the boat is afloat").unwrap();
    assert!(e.restore_owed_to(&uid("boat-1")), "owed back again");
    let opts = replay::ReplayOptions {
        trust_resumed: true,
        ..replay::ReplayOptions::default()
    };
    let r = replay::replay_str_with(&log.text(), &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.restores_closed.iter().any(|c| c.contains("confirmed lost by op-2")), "{:?}", r.restores_closed);
    assert!(r.losses_revoked.iter().any(|c| c.contains("loss revoked by op-1")), "{:?}", r.losses_revoked);
    assert!(r.unrestored.iter().any(|u| u.starts_with("boat-1")), "{:?}", r.unrestored);
}

/// In a credentials run a confirmation re-enters the credential (it removes a friendly-fire
/// protection), as an un-spend does; replay checks it quotes the step-up.
#[test]
fn a_confirmation_in_a_credentials_run_steps_up() {
    let clock = ManualClock::new(T0.plus_ms(60_000));
    let mut p = common::policy();
    p.rules.rules_version = NODE_LOSS_SINCE;
    let log = MemorySink::new();
    let roster = Roster::new(["op-1", "op-2"].map(OperatorId::new).to_vec(), vec![OperatorId::new("op-2")])
        .unwrap()
        .with_auth(AuthMode::Credentials);
    let mut e = Engine::with_areas(p, Some(common::areas()), roster, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let s = e
        .open_authenticated_session(OperatorId::new("op-2"), Grants { engage: true }, AuthMethod::Password)
        .unwrap();
    let w = world_at(T0.plus_ms(60_000), &["boat-1"]);
    let by = s.engagement_authority().unwrap();
    let r = e.confirm_lost(by, uid("boat-1"), Some(T0), "sunk", &w, None);
    assert!(matches!(r, Err(EngineError::NotAuthenticated { .. })), "{r:?}");
    let step = e.step_up(&s).unwrap();
    e.confirm_lost(s.engagement_authority().unwrap(), uid("boat-1"), Some(T0), "sunk", &w, Some(step)).unwrap();
    let r = replayed(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
    // the step-up cut from the confirmation
    let forged = common::forge(&log.text(), |ev| {
        for x in ev.iter_mut() {
            if let EventBody::NodeLost { step_up, .. } = &mut x.body {
                *step_up = None;
            }
        }
    });
    assert!(!replayed(&forged).ok());
}

/// A signer shown the picture before a loss was revoked (or confirmed) can't sign it: the token
/// carries our vehicles confirmed lost, and a set that changed since is refused (`look again`).
#[test]
fn a_signature_on_a_picture_shown_before_a_revocation_is_refused() {
    let text = r#"
title = "token"
invariant = "signed_picture"
attack = "sign a picture shown before a loss was revoked"
expect = "dispatched"
expect_refusals = ["our vehicles confirmed lost changed since you looked"]
expect_revoked = ["vehicle hunter-1: loss revoked, heard again"]
min_rules = 30
end_s = 12

[[operator]]
id = "op-1"
[[operator]]
id = "op-2"
engage = true
[[operator]]
id = "op-3"
engage = true
[[operator]]
id = "op-4"
engage = true

[[vehicle]]
uid = "blade-1"
platform = "switchblade"
at = [-122.4700, 37.8160]

[[vehicle]]
uid = "hunter-1"
platform = "seahunter"
at = [-122.4848, 37.8120]
lost_for_s = 60

[[track]]
id = "T-0001"
at = [-122.4950, 37.8120]
affiliation = "suspect"
identity = "small_fast_boat"
identity_confidence = 0.9
sources = ["3:radar", "1:eo_ir"]

[[step]]
at_s = 0
action = "declare"
track = "T-0001"
affiliation = "hostile"
by = "op-1"

[[step]]
at_s = 1
action = "confirm_lost"
by = "op-2"
uid = "hunter-1"
note = "its crew's last call"

[[step]]
at_s = 2
action = "nominate"
by = "op-1"
platform = "blade-1"
track = "T-0001"
radius_m = 75
fallback = "return"
window_s = 600

[[step]]
at_s = 4
action = "approve"
by = "op-2"

[[step]]
at_s = 5
action = "look"
by = "op-3"

[[step]]
at_s = 6
action = "vehicle"
uid = "hunter-1"
silent = false

[[step]]
at_s = 8
action = "approve"
by = "op-3"

[[step]]
at_s = 9
action = "approve"
by = "op-3"
"#;
    let sc: scenario::Scenario = toml::from_str(text).unwrap();
    let r = scenario::run(&sc, "token", &root(), &scenario::RunOptions::default()).unwrap();
    assert!(r.ok && r.replay_ok, "{:?} {:?}", r.failures, r.replay_mismatches);
}

/// The minefield check leaves one of ours confirmed lost out, its orders with it (a field next to
/// a wreck is not refused for it); silent and not confirmed lost, it still counts.
#[test]
fn the_minefield_check_leaves_a_lost_vehicle_out() {
    let text = |confirm: bool, expect: &str| {
        format!(
            r#"
title = "lay"
invariant = "minefield"
attack = "lay a field next to one of ours"
expect = "{expect}"
min_rules = 30
end_s = 3

[[operator]]
id = "op-1"
[[operator]]
id = "op-2"
engage = true

[[vehicle]]
uid = "mole-1"
platform = "ugv_mines"
at = [-122.4600, 37.7950]
home = [-122.4570, 37.7950]

[[vehicle]]
uid = "mule-1"
platform = "ugv_logistics"
at = [-122.4590, 37.7951]   # on the row's line, silent a minute
lost_for_s = 60

{confirm}

[[step]]
at_s = 1
action = "lay"
by = "op-1"
platform = "mole-1"
points = [[-122.4595, 37.7950], [-122.4585, 37.7950]]
count = 10
live_h = 0.05
weapon = "TM-62M mine"
"#,
            confirm = if confirm {
                "[[step]]\nat_s = 0\naction = \"confirm_lost\"\nby = \"op-2\"\nuid = \"mule-1\"\nnote = \"burnt out on the road\"\n"
            } else {
                ""
            }
        )
    };
    let run = |t: String| {
        let sc: scenario::Scenario = toml::from_str(&t).unwrap();
        scenario::run(&sc, "lay", &root(), &scenario::RunOptions::default()).unwrap()
    };
    let lost = run(text(true, "pending_approval"));
    assert!(lost.ok && lost.replay_ok, "{:?} {:?}", lost.failures, lost.replay_mismatches);
    assert!(lost.log.contains("left out, confirmed lost: mule-1"), "the check's detail names it");
    let silent = run(text(false, "rejected"));
    assert!(silent.ok && silent.replay_ok, "{:?} {:?}", silent.failures, silent.replay_mismatches);
    assert!(silent.failed_rules.iter().any(|r| r == "minefield"), "{:?}", silent.failed_rules);
}

/// Spent and lost stay separate (decision D6): a platform spent by its own effect (here carried
/// over spent from an earlier run) that goes silent is presumed lost with that as its evidence,
/// and stays spent; nothing confirms it lost but a person.
#[test]
fn a_spent_platform_that_goes_silent_is_presumed_lost_not_lost() {
    let clock = ManualClock::new(T0.plus_ms(60_000));
    let mut p = common::policy();
    p.rules.rules_version = NODE_LOSS_SINCE;
    let log = MemorySink::new();
    let carried = replay::CarriedFrom {
        log: "earlier.jsonl".into(),
        spent: vec![replay::SpentPlatform {
            uid: uid("blade-1"),
            by: IntentId(3),
            from: None,
            values: BTreeMap::new(),
            vehicle: None,
            spent_t: Some(T0),
            ended: true,
        }],
        ..Default::default()
    };
    let mut e = Engine::start(p, Some(common::areas()), common::roster(), vec![carried], Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let w = world_at(T0.plus_ms(60_000), &["blade-1"]);
    let step = e.step_nodes(&w).unwrap();
    assert_eq!(step.presumed, vec![uid("blade-1")], "{step:?}");
    let ev = &e.nodes_presumed()[&uid("blade-1")].evidence;
    assert_eq!((ev.len(), ev[0].kind, ev[0].seq), (1, EvidenceKind::Spent, Some(1)), "{ev:?}");
    assert!(e.node_lost(&uid("blade-1")).is_none());
    assert_eq!(e.spent_platforms().len(), 1);
    let opts = replay::ReplayOptions {
        trust_resumed: true,
        ..replay::ReplayOptions::default()
    };
    let r = replay::replay_str_with(&log.text(), &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// A log claiming rules 29 that carries rules 30's events is caught by its keys (scenario 141's:
/// a loss confirmed, then revoked).
#[test]
fn rules_30_events_in_a_log_claiming_rules_29_are_caught() {
    let mut sc = scenario::load(&root().join("sim/scenarios/engage/141-lost-vehicle-reports-again.toml")).unwrap();
    sc.tamper = None;
    let r = scenario::run(&sc, "141", &root(), &scenario::RunOptions::default()).unwrap();
    assert!(r.ok && r.replay_ok, "{:?} {:?}", r.failures, r.replay_mismatches);
    let forged = common::forge(&r.log, |ev| {
        if let EventBody::RunStarted { rules_version, policy, .. } = &mut ev[0].body {
            *rules_version = Some(HANDOVER_SINCE);
            policy.rules.rules_version = HANDOVER_SINCE;
        }
    });
    let opts = replay::ReplayOptions {
        rules: replay::RulesAccepted::Exactly(HANDOVER_SINCE),
        ..replay::ReplayOptions::default()
    };
    let w = whats(&replay::replay_str_with(&forged, &opts).unwrap());
    for want in ["one of ours confirmed lost", "a loss revoked", "our vehicles confirmed lost (a decision's fact)"] {
        assert!(w.iter().any(|m| m.starts_with(want)), "{want}: {w:?}");
    }
}
