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

/// Rules 34, stage 6: an operator's dry run of a group order decides it as its submission would
/// (laid out and checked member by member), shows its layout, spec and each member's task, and
/// logs nothing; an agent's preview builds the same group; under rules 33 a dry run of the same
/// order is one intent's, with no group view, and a transit is refused.
#[test]
fn an_operators_dry_run_of_a_group_order_shows_its_layout() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(rules34(Policy::load(&sim_dir().join("policy.toml")).unwrap()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let w = four_quads();
    let t = terrain();
    let uids = ["sky-1", "sky-2", "sky-3", "sky-4"];
    let before = log.text().len();
    let p = e.dry_run_for_operator(&OperatorId::new("op-1"), orbit(RING, 150.0, &uids), &w, &t).unwrap();
    assert_eq!(log.text().len(), before, "a dry run logs nothing");
    assert!(p.passed, "{:?}", p.checks);
    let g = p.group.as_ref().expect("a group view");
    assert_eq!(g.layout.shares.len(), 4);
    assert_eq!(g.spec.min_members, Some(4));
    assert!(g.tasks.values().all(|t| matches!(t, Task::Orbit { .. })));
    assert!(p.checks.iter().any(|c| c.rule == RuleId::Strength), "the group's own checks");
    // a transit to stations: each member's own station
    let route = vec![LonLat::new(-122.4600, 37.8100), LonLat::new(-122.4700, 37.8100)];
    let mut tr = Proposal::new(Task::Transit { route }, uids.iter().map(|u| uid(u)).collect());
    tr.group = Some(GroupSpec::default());
    let p = e.dry_run_for_operator(&OperatorId::new("op-1"), tr.clone(), &w, &t).unwrap();
    let g = p.group.as_ref().expect("a group view");
    assert!(g.tasks.values().all(|t| matches!(t, Task::Transit { route } if route.len() == 1)), "{:?}", g.tasks);
    assert_eq!(g.layout.spread, Some(group::Spread::Stations));
    // an agent's preview builds the same group (MCP's stage), and logs only that it previewed
    let before = log.text().lines().count();
    let a = e.agent_port(AgentId::new("agent-planner")).preview(orbit(RING, 150.0, &uids), &w, &t).unwrap();
    let ga = a.group.as_ref().expect("the agent's group view");
    assert_eq!(ga.layout.shares.len(), 4);
    assert_eq!(ga.spec.min_members, Some(4));
    assert!(a.checks.iter().any(|c| c.rule == RuleId::Strength), "the group's own checks");
    assert_eq!(log.text().lines().count(), before + 1, "agent.action intent.preview");
    // an agent's preview of a transit to stations too
    let a = e.agent_port(AgentId::new("agent-planner")).preview(tr.clone(), &w, &t).unwrap();
    assert_eq!(a.group.as_ref().and_then(|g| g.layout.spread), Some(group::Spread::Stations));
    // rules 33: one intent's dry run, and no transit
    let mut old = Engine::new(Policy::load(&sim_dir().join("policy.toml")).unwrap(), roster(), Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let p = old.dry_run_for_operator(&OperatorId::new("op-1"), orbit(RING, 150.0, &uids), &w, &t).unwrap();
    assert!(p.group.is_none() && !p.checks.iter().any(|c| c.rule == RuleId::Strength));
    tr.group = None;
    assert!(old.dry_run_for_operator(&OperatorId::new("op-1"), tr.clone(), &w, &t).is_err());
    let s = old.open_operator_session(OperatorId::new("op-1")).unwrap();
    let refused = old.submit(s.authority(), tr, &w, &t).expect_err("no transit under rules 33");
    assert!(refused.to_string().contains("take no transit (rules 34)"), "{refused}");
}

/// Rules 34 (policy schema 9, staged): a transit is routed as a patrol is. Under the situations'
/// watch policy an agent's transit waits for the operator in its agent tier, which names patrol;
/// under the standing policy an agent's routine transit goes by the default tier, as a patrol does.
#[test]
fn a_transit_is_routed_as_a_patrol() {
    let clock = ManualClock::new(T0);
    let w = four_quads();
    let t = terrain();
    let route = vec![LonLat::new(-122.4600, 37.8100), LonLat::new(-122.4700, 37.8100)];
    let one = || Proposal::new(Task::Transit { route: route.clone() }, vec![uid("sky-1")]);
    let mut e = Engine::new(watch_policy(), roster(), Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let id = e.agent_port(AgentId::new("agent-planner")).propose(one(), rationale("hold a station on the strait"), &w, &t).unwrap().id();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    assert_eq!(i.policy().map(|p| p.rule.as_str()), Some("agent-tasking"));
    let mut e = Engine::new(rules34(Policy::load(&sim_dir().join("policy.toml")).unwrap()), roster(), Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let id = e.agent_port(AgentId::new("agent-planner")).propose(one(), rationale("hold a station on the strait"), &w, &t).unwrap().id();
    assert_eq!(e.intent(id).unwrap().policy().map(|p| p.rule.as_str()), Some("default"), "{:?}", e.intent(id).unwrap().policy());
}
