//! Phase E: the engagement decision chain. Every adversarial scenario in
//! `sim/scenarios/engage/` ends as designed and replays; the policy floor holds whatever
//! the file says; the two-person and authority rules are enforced by the engine; the
//! engagement lifecycle can't be shortcut through the generic calls; the preview's
//! intercept is what it says.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn every_adversarial_scenario_ends_as_designed_and_replays() {
    let dir = root().join("sim/scenarios/engage");
    let files = scenario::files(&dir).unwrap();
    assert!(files.len() >= 15, "{} scenarios", files.len());
    let mut invariants = Vec::new();
    for f in &files {
        let r = scenario::run_file(f, &root(), &RunOptions::default()).unwrap();
        assert!(r.ok, "{}: {:?}", r.file, r.failures);
        assert!(r.replay_ok, "{}: {:?}", r.file, r.replay_mismatches);
        if r.tamper.is_some() {
            assert_eq!(r.tamper_detected, Some(true), "{}", r.file);
        }
        invariants.push(r.invariant.clone());
    }
    // PLAN.md exit: every invariant has a scenario that tries to break it
    for want in [
        "agent_cannot_authorize",
        "two_person",
        "keep_out_tracks",
        "keep_out_friendly",
        "target_identity",
        "target_declared",
        "track_deviation",
        "platform_link",
        "target_recency",
        "engagement_zone",
        "no_strike",
        "platform_armed",
        "reach",
        "abort",
        "assessment",
        "audit",
    ] {
        assert!(invariants.iter().any(|i| i == want), "no scenario attacks {want}");
    }
}

#[test]
fn scenarios_are_deterministic_and_seedable() {
    let f = root().join("sim/scenarios/engage/01-nominal.toml");
    let a = scenario::run_file(&f, &root(), &RunOptions::default()).unwrap();
    let b = scenario::run_file(&f, &root(), &RunOptions::default()).unwrap();
    assert_eq!(a.log, b.log);
    let noisy = RunOptions {
        seed: Some(7),
        track_noise_m: 3.0,
    };
    let c = scenario::run_file(&f, &root(), &noisy).unwrap();
    let d = scenario::run_file(&f, &root(), &noisy).unwrap();
    assert_eq!(c.log, d.log);
    assert_ne!(a.log, c.log);
    assert!(c.replay_ok, "{:?}", c.replay_mismatches);
}

#[test]
fn replay_counts_the_chain() {
    let f = root().join("sim/scenarios/engage/01-nominal.toml");
    let r = scenario::run_file(&f, &root(), &RunOptions::default()).unwrap();
    let rep = replay::replay_str(&r.log).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(rep.engagements, 1);
    assert_eq!(rep.concurrences, 1);
    assert_eq!(rep.approvals, 1);
    assert_eq!(rep.self_approvals, 0);
    assert_eq!(rep.effects, 1);
    assert_eq!(rep.assessments, 1);
    assert_eq!(rep.aborts, 0);
    assert!(rep.pictures_recomputed > 50);
    assert_eq!(rep.picture_voids, 0);
}

// ---- a small world for the engine-level tests ------------------------------------------------

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const BLADE_AT: LonLat = LonLat::new(-122.4700, 37.8160);

fn engage_world(t: Time) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(t);
    w.areas = Some(areas());
    let mut blade = vehicle("switchblade", BLADE_AT);
    // every fact fresh at `t`
    blade.position = Some(Fact::new(BLADE_AT, t));
    blade.autopilot = Some(Fact::new(AutopilotKind::Plane, t));
    blade.nav_ready = Some(Fact::new(true, t));
    blade.battery_pct = Some(Fact::new(95.0, t));
    blade.velocity_mps = Some(Fact::new([0.0, 0.0], t));
    w.vehicles.insert(uid("blade-1"), blade);
    w.tracks.insert("T-0001".into(), target(t, TARGET_AT, [0.0, 0.0], true));
    w
}

fn target(t: Time, at: LonLat, vel: [f64; 2], declared: bool) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, t),
        alt_m: None,
        velocity_mps: Some(vel),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(
            if declared {
                Affiliation::Hostile
            } else {
                Affiliation::Suspect
            },
            t,
        ),
        affiliation_confidence: 0.8,
        identity: Some("small_fast_boat".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into()],
        domain: Domain::Surface,
        last_seen_t: t,
        declared_by: declared.then(|| "op-1".to_string()),
    }
}

fn nomination() -> Proposal {
    Proposal::new(
        Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Return,
        },
        vec![uid("blade-1")],
    )
}

#[test]
fn the_policy_floor_holds_whatever_the_file_says() {
    // a file that would let an engagement run on its own, or on its submitter's say-so
    let mut p = policy();
    p.tiers.retain(|t| t.name != "engagement");
    p.tiers.insert(
        0,
        policy::TierRule {
            name: "careless".into(),
            when: policy::When {
                kind: Some(vec![TaskKind::Engage]),
                ..Default::default()
            },
            tier: Tier::Auto,
            reason: "a careless file".into(),
            submitter_authorizes: Some(true),
            approvers: None,
            proposer_may_approve: Some(true),
        },
    );
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(p, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op = e
        .open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true })
        .unwrap();
    let i = e
        .submit(op.authority(), nomination(), &engage_world(T0), &terrain())
        .unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    let d = i.policy().unwrap();
    assert_eq!(d.tier, Tier::Human);
    assert_eq!(d.rule, "(engage)");
    assert!(!d.submitter_authorizes);
    assert!(d.proposer_excluded);
    assert!(d.approvers >= 1);
    // and the proposer, with engagement authority, still can't approve it
    let id = i.id();
    let err = e
        .approve_engagement(
            id,
            op.engagement_authority().unwrap(),
            None,
            &engage_world(T0),
            &terrain(),
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::ProposerCannotApprove { .. }), "{err}");
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    assert!(replay::replay_str(&log.text()).unwrap().ok());
}

#[test]
fn authority_and_lifecycle_are_enforced_by_the_engine() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(
        policy(),
        Some(areas()),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let op2 = e
        .open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true })
        .unwrap();
    let op3 = e
        .open_operator_session_with(OperatorId::new("op-3"), Grants { engage: true })
        .unwrap();
    assert!(op1.engagement_authority().is_none());
    let id = e
        .submit(op1.authority(), nomination(), &engage_world(T0), &terrain())
        .unwrap()
        .id();
    // the window was written into the spec
    assert!(matches!(
        e.intent(id).unwrap().spec().task,
        Task::Engage { window: Some(_), .. }
    ));
    // a plain operator authority can't approve an engagement, even with authority elsewhere
    let err = e
        .approve(id, op2.authority(), None, &engage_world(T0), &terrain())
        .unwrap_err();
    assert!(matches!(err, EngineError::EngagementNeedsAuthority(_)), "{err}");
    // not dispatchable, not finishable, not abortable while pending
    assert!(matches!(e.dispatch(id), Err(EngineError::EngageIntent(_))));
    assert!(e.finish(id, TaskOutcome::Done).is_err());
    assert!(e.abort_engagement(id, op1.authority(), "x").is_err());
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    // two distinct approvers with authority
    clock.advance_ms(2000);
    e.approve_engagement(
        id,
        op2.engagement_authority().unwrap(),
        None,
        &engage_world(clock.now()),
        &terrain(),
    )
    .unwrap();
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    assert_eq!(e.intent(id).unwrap().concurrences().len(), 1);
    let err = e
        .approve_engagement(
            id,
            op2.engagement_authority().unwrap(),
            None,
            &engage_world(clock.now()),
            &terrain(),
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::AlreadySigned { .. }), "{err}");
    e.approve_engagement(
        id,
        op3.engagement_authority().unwrap(),
        None,
        &engage_world(clock.now()),
        &terrain(),
    )
    .unwrap();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Approved);
    let snap = i.engagement().unwrap().snapshot.clone().unwrap();
    assert_eq!(snap.track_id, "T-0001");
    assert_eq!(snap.track.declared_by.as_deref(), Some("op-1"));
    let order = match e.dispatch_order(id).unwrap() {
        Order::Engage(o) => o,
        other => panic!("{other:?}"),
    };
    assert_eq!(order.effect_point, snap.preview.effect_point);
    // any operator may stop it; the fallback comes back to be commanded
    let fb = e
        .abort_engagement(id, op1.authority(), "changed my mind")
        .unwrap()
        .unwrap();
    assert_eq!(fb.fallback, Fallback::Return);
    assert_eq!(e.intent(id).unwrap().state(), State::Aborted);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.aborts, 1);
}

#[test]
fn the_preview_intercepts_a_moving_track() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(
        policy(),
        Some(areas()),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    let mut w = engage_world(T0);
    // crossing south to north-west at 6 m/s
    w.tracks.insert(
        "T-0001".into(),
        target(T0, LonLat::new(-122.5000, 37.8080), [-2.0, 5.5], true),
    );
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let i = e.submit(op.authority(), nomination(), &w, &terrain()).unwrap();
    let p = i.engagement().unwrap().preview.clone().unwrap();
    let travel_s = p.effect_t.ms_since(p.start_t) as f64 / 1000.0;
    let reach = p.cruise_mps * travel_s;
    assert!(
        (p.distance_m - reach).abs() < 0.02 * reach,
        "{} m vs {} m",
        p.distance_m,
        reach
    );
    // the effect point is where the track will be at the effect time
    let track = &w.tracks["T-0001"];
    let there = engage::track_at(track, p.effect_t);
    assert!(geo::haversine_m(there, p.effect_point) < 0.5);
    // the ellipse is longer along the course than across
    assert!(p.ellipse.semi_major_m > p.ellipse.semi_minor_m);
}

#[test]
fn an_agent_nomination_needs_a_rationale_and_waits_for_humans() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(
        policy(),
        Some(areas()),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    assert!(Rationale::new("  ").is_err());
    let mut port = e.agent_port(AgentId::new("agent-planner"));
    let i = port
        .propose(
            nomination(),
            Rationale::new("declared hostile, holding in the zone").unwrap(),
            &engage_world(T0),
            &terrain(),
        )
        .unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.policy().unwrap().proposer_excluded);
    // the agent's preview never says it would be approved
    let pv = e
        .agent_port(AgentId::new("agent-planner"))
        .preview(nomination(), &engage_world(T0), &terrain())
        .unwrap();
    assert_eq!(pv.would_reach, State::PendingApproval);
    assert!(pv.engage.is_some());
    assert!(replay::replay_str(&log.text()).unwrap().ok());
}
