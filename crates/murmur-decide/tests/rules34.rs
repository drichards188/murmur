//! Decision rules 34, stage 3 (docs/SWARMS.md section 13): group orders in the engine. The claims
//! beside the scenarios (158 to 165): one approval covers its members and nothing more, so an
//! agent's group order is one card in the inbox and none of its member orders ever is; under
//! older rules an order to many vehicles is one intent as it was, and a group spec is refused; a
//! log of rules 34 cannot pass for one of older rules; and a group's end counts its members as
//! section 2.2 says.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use murmur_decide::group::{self, GroupSpec};
use murmur_decide::*;

fn rules34(mut p: Policy) -> Policy {
    p.rules.rules_version = GROUP_ORDER_SINCE;
    p
}

/// The watch policy (an agent's orbit waits for the person on watch), under rules 34.
fn watch_policy() -> Policy {
    let (p, _) = Policy::load_layers(&sim_dir().join("situations/policies/watch.toml")).unwrap();
    rules34(p)
}

fn four_quads() -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    for i in 1..=4 {
        w.vehicles.insert(uid(&format!("sky-{i}")), vehicle("x10", CRISSY));
    }
    w
}

const RING: LonLat = LonLat::new(-122.4700, 37.8150);

#[test]
fn an_agents_group_order_is_one_card_and_its_members_never_reach_the_inbox() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(watch_policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let w = four_quads();
    let t = terrain();
    let uids = ["sky-1", "sky-2", "sky-3", "sky-4"];
    let id = e
        .agent_port(AgentId::new("agent-planner"))
        .propose(orbit(RING, 150.0, &uids), rationale("a picket on the strait"), &w, &t)
        .unwrap()
        .id();
    let g = e.intent(id).unwrap();
    assert_eq!(g.state(), State::PendingApproval);
    assert_eq!(g.spec().group().and_then(|g| g.min_members), Some(4), "four of four: 0.8 of 4 rounds up to 4");
    assert_eq!(e.inbox().len(), 1, "one card for the group");
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.approve(id, s.authority(), None, &w, &t).unwrap();
    let routed = e.dispatch_group(id, &w).unwrap();
    assert_eq!(routed.len(), 4);
    assert!(routed.iter().all(|(_, r)| matches!(r, Routed::Dispatched(_))), "{routed:?}");
    assert_eq!(e.intent(id).unwrap().state(), State::Dispatched);
    assert!(e.inbox().is_empty(), "no member order in the inbox");
    // each member order: one vehicle, its group the parent, its share the layout's, approved under
    // the group (never pending)
    for (mid, _) in &routed {
        let m = e.intent(*mid).unwrap();
        assert_eq!(m.spec().parent(), Some(id));
        assert_eq!(m.spec().uids.len(), 1);
        assert!(m.spec().share().and_then(|s| s.phase_deg).is_some());
        assert!(m.history().iter().all(|h| h.state != State::PendingApproval));
    }
    let r = replay::replay_str_with(&log.text(), &replay::ReplayOptions { rules: replay::RulesAccepted::Exactly(GROUP_ORDER_SINCE), ..Default::default() }).unwrap();
    assert!(r.mismatches.is_empty(), "{:#?}", r.mismatches);
    // the group is a dispatch path of its own: the ordinary ones refuse it
    assert!(e.dispatch_or_queue(id, &w).is_err());
}

#[test]
fn under_older_rules_an_order_to_many_is_one_intent_and_a_group_spec_is_refused() {
    let clock = ManualClock::new(T0);
    let (mut e, _) = engine(&clock);
    assert_eq!(e.policy().rules.rules_version, RULES_VERSION);
    let w = four_quads();
    let t = terrain();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let i = e.submit(s.authority(), orbit(RING, 150.0, &["sky-1", "sky-2"]), &w, &t).unwrap();
    assert!(i.spec().group().is_none(), "rules 33: one intent, all or nothing, as before");
    let mut p = orbit(RING, 150.0, &["sky-3", "sky-4"]);
    p.group = Some(GroupSpec::default());
    let err = e.submit(s.authority(), p, &w, &t).unwrap_err().to_string();
    assert!(err.contains("take no group order (rules 34)"), "{err}");
}

#[test]
fn a_log_of_rules_34_cannot_pass_for_one_of_older_rules() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(rules34(policy()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let w = four_quads();
    let t = terrain();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let id = e.submit(s.authority(), orbit(RING, 150.0, &["sky-1", "sky-2", "sky-3"]), &w, &t).unwrap().id();
    e.dispatch_group(id, &w).unwrap();
    let text = forge(&log.text(), |ev| claim_older_rules(&mut ev[0].body, RULES_VERSION));
    let r = replay::replay_str_with(&text, &replay::ReplayOptions { rules: replay::RulesAccepted::AtLeast(1), ..Default::default() }).unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.what.contains("only builds of rules 34 or later write it")),
        "{:#?}",
        r.mismatches
    );
}

#[test]
fn a_groups_end_counts_its_members() {
    use State::*;
    // four reached their shares, one failed: at the minimum of four, done
    let (ok, why) = group::outcome(4, &[(Done, false), (Done, false), (Done, false), (Done, false), (Failed, false)]);
    assert!(ok, "{why}");
    assert_eq!(why, "done: 4 reached their shares, 1 failed; the minimum is 4");
    // three reached them, one never sent, one taken by a newer order: the minimum drops to three
    let (ok, why) = group::outcome(4, &[(Done, false), (Done, false), (Done, false), (Expired, false), (Failed, true)]);
    assert!(ok, "{why}");
    assert_eq!(why, "done: 3 reached their shares, 1 never sent (its wait ran out), 1 taken by a newer order; the minimum is 3 (4 less 1 taken)");
    // two of five: failed
    let (ok, why) = group::outcome(4, &[(Done, false), (Done, false), (Voided, false), (Failed, false), (Failed, false)]);
    assert!(!ok);
    assert!(why.starts_with("failed: 2 reached their shares, 2 failed, 1 voided"), "{why}");
}
