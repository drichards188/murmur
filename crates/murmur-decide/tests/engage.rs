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
use murmur_decide::world::NavSource;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn every_adversarial_scenario_ends_as_designed_and_replays() {
    let dir = root().join("sim/scenarios/engage");
    let files = scenario::files(&dir).unwrap();
    assert!(files.len() >= 36, "{} scenarios", files.len());
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
        "signed_picture",
        "platform_nav",
        "platform_free",
        "engagement_in_play",
        "picture_gap",
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
        velocity_noise_mps: 0.5,
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
    blade.nav_source = Some(Fact::new(NavSource::Gnss, t));
    blade.gps_fix = Some(Fact::new(3, t));
    blade.gps_glitch = Some(Fact::new(false, t));
    blade.battery_pct = Some(Fact::new(95.0, t));
    blade.velocity_mps = Some(Fact::new([0.0, 0.0], t));
    w.vehicles.insert(uid("blade-1"), blade);
    w.tracks.insert("T-0001".into(), target(t, TARGET_AT, [0.0, 0.0], true));
    w
}

/// `engage_world` with the target at `at`.
fn world_with_target(t: Time, at: LonLat, vel: [f64; 2]) -> WorldSnapshot {
    let mut w = engage_world(t);
    w.tracks.insert("T-0001".into(), target(t, at, vel, true));
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

/// The console's look-then-sign: ask for the picture, sign it at once.
fn sign(e: &mut Engine, id: IntentId, op: &OperatorSession, w: &WorldSnapshot) -> Result<State, EngineError> {
    let auth = op.engagement_authority().unwrap();
    let token = e.issue_picture(id, auth, w)?.id;
    e.approve_engagement(id, auth, None, token, w, &terrain())
        .map(|i| i.state())
}

fn engine_at(clock: &ManualClock, log: &MemorySink) -> Engine {
    Engine::with_areas(
        policy(),
        Some(areas()),
        roster(),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap()
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
    // files that would let an engagement run on its own, on its submitter's say-so, or on
    // one signature
    for (tier, approvers) in [(Tier::Auto, None), (Tier::Human, Some(1)), (Tier::Human, None)] {
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
                tier,
                reason: "a careless file".into(),
                submitter_authorizes: Some(true),
                approvers,
                proposer_may_approve: Some(true),
            },
        );
        floor_holds(p);
    }
}

fn floor_holds(p: Policy) {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op = e
        .open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true })
        .unwrap();
    let i = e
        .submit(op.authority(), nomination(), &engage_world(T0), &terrain())
        .unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    let d = i.policy().unwrap();
    assert_eq!(d.tier, Tier::Human);
    assert!(!d.submitter_authorizes);
    assert!(d.proposer_excluded);
    assert_eq!(d.approvers, 2, "the two-person floor");
    // and the proposer, with engagement authority, can neither be shown the picture to sign
    // nor sign it
    let id = i.id();
    let w = engage_world(T0);
    let auth = op.engagement_authority().unwrap();
    let err = e.issue_picture(id, auth, &w).unwrap_err();
    assert!(matches!(err, EngineError::ProposerCannotApprove { .. }), "{err}");
    let err = e.approve_engagement(id, auth, None, 1, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::ProposerCannotApprove { .. }), "{err}");
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    assert!(replay::replay_str(&log.text()).unwrap().ok());
}

#[test]
fn authority_and_lifecycle_are_enforced_by_the_engine() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
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
    assert!(matches!(e.dispatch(id, &engage_world(T0)), Err(EngineError::EngageIntent(_))));
    assert!(e.finish(id, TaskOutcome::Done).is_err());
    assert!(e.abort_engagement(id, op1.authority(), "x").is_err());
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    // two distinct approvers with authority, on one plan
    clock.advance_ms(2000);
    let w = engage_world(clock.now());
    assert_eq!(sign(&mut e, id, &op2, &w).unwrap(), State::PendingApproval);
    assert_eq!(e.intent(id).unwrap().concurrences().len(), 1);
    let plan = e.intent(id).unwrap().engagement().unwrap().plan.clone().unwrap();
    assert_eq!(plan.by, OperatorId::new("op-2"));
    let err = sign(&mut e, id, &op2, &w).unwrap_err();
    assert!(matches!(err, EngineError::AlreadySigned { .. }), "{err}");
    clock.advance_ms(3000);
    let w = engage_world(clock.now());
    // what op-3 is shown is the frozen plan, not a fresh preview
    let token = e.issue_picture(id, op3.engagement_authority().unwrap(), &w).unwrap();
    assert!(token.frozen);
    assert_eq!(token.plan, plan.preview);
    e.approve_engagement(id, op3.engagement_authority().unwrap(), None, token.id, &w, &terrain())
        .unwrap();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Approved);
    let snap = i.engagement().unwrap().snapshot.clone().unwrap();
    assert_eq!(snap.track_id, "T-0001");
    assert_eq!(snap.track.declared_by.as_deref(), Some("op-1"));
    // the approval holds the first signer's plan, and the platform is sent there
    assert_eq!(snap.preview, plan.preview);
    let order = match e.dispatch_order(id).unwrap() {
        Order::Engage(o) => o,
        other => panic!("{other:?}"),
    };
    assert_eq!(order.effect_point, plan.preview.effect_point);
    assert_eq!(order.effect_t, plan.preview.effect_t);
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
    let mut e = engine_at(&clock, &log);
    // crossing south to north-west at 6 m/s
    let w = world_with_target(T0, LonLat::new(-122.5000, 37.8080), [-2.0, 5.5]);
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
    let mut e = engine_at(&clock, &log);
    assert!(Rationale::new("  ").is_err());
    // the agent's preview never says it would be approved
    let pv = e
        .agent_port(AgentId::new("agent-planner"))
        .preview(nomination(), &engage_world(T0), &terrain())
        .unwrap();
    assert_eq!(pv.would_reach, State::PendingApproval);
    assert!(pv.engage.is_some());
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
    // one engagement per platform: a second nomination on blade-1 is rejected
    let pv = e
        .agent_port(AgentId::new("agent-planner"))
        .preview(nomination(), &engage_world(T0), &terrain())
        .unwrap();
    assert_eq!(pv.would_reach, State::Rejected);
    assert!(pv.checks.iter().any(|c| c.rule.as_str() == "platform_free" && c.outcome == Outcome::Fail));
    assert!(replay::replay_str(&log.text()).unwrap().ok());
}

/// Two operators with engagement authority and a pending nomination by a third.
fn two_signers(e: &mut Engine, w: &WorldSnapshot) -> (OperatorSession, OperatorSession, IntentId) {
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let op2 = e
        .open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true })
        .unwrap();
    let op3 = e
        .open_operator_session_with(OperatorId::new("op-3"), Grants { engage: true })
        .unwrap();
    let id = e.submit(op1.authority(), nomination(), w, &terrain()).unwrap().id();
    (op2, op3, id)
}

fn has_event(log: &MemorySink, kind: &str) -> bool {
    log.text().lines().any(|l| l.contains(&format!("\"type\":\"{kind}\"")))
}

#[test]
fn every_signature_is_on_the_frozen_plan() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    let first = e.intent(id).unwrap().engagement().unwrap().plan.clone().unwrap();
    // the track jumps 300 m between the signatures (beyond the 75 m tolerance), still a
    // valid nomination where it is now
    clock.advance_ms(3000);
    let moved = world_with_target(clock.now(), LonLat::new(-122.4985, 37.8130), [0.0, 0.0]);
    // op-3 is shown the frozen plan and signs it: it no longer holds, every signature is
    // dropped with it and this one refused
    let err = sign(&mut e, id, &op3, &moved).unwrap_err();
    assert!(matches!(err, EngineError::PlanChanged { .. }), "{err}");
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    assert!(i.concurrences().is_empty());
    assert!(i.engagement().unwrap().plan.is_none());
    let reset = i.engagement().unwrap().reset.clone().unwrap();
    assert_eq!(reset.dropped, vec![OperatorId::new("op-2")]);
    assert!(reset.reason.contains("plan"), "{}", reset.reason);
    assert!(has_event(&log, "approval.reset"));
    // a new plan: op-3 freezes it, op-2 signs it again
    clock.advance_ms(1000);
    let w = world_with_target(clock.now(), LonLat::new(-122.4985, 37.8130), [0.0, 0.0]);
    sign(&mut e, id, &op3, &w).unwrap();
    let second = e.intent(id).unwrap().engagement().unwrap().plan.clone().unwrap();
    assert_ne!(second.preview.effect_point, first.preview.effect_point);
    assert_eq!(sign(&mut e, id, &op2, &w).unwrap(), State::Approved);
    let snap = e.intent(id).unwrap().engagement().unwrap().snapshot.clone().unwrap();
    assert_eq!(snap.preview, second.preview);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_waiting_signature_is_dropped_when_the_plan_stops_holding() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, _, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    // held while it holds
    clock.advance_ms(1000);
    let out = e.step_engagements(&engage_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(e.intent(id).unwrap().concurrences().len(), 1);
    // reclassified while waiting for the second signature
    clock.advance_ms(1000);
    let mut w = engage_world(clock.now());
    w.tracks.get_mut("T-0001").unwrap().identity = Some("fishing_boat".into());
    let out = e.step_engagements(&w, &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.concurrences().is_empty());
    assert!(i.engagement().unwrap().plan.is_none());
    assert!(i.engagement().unwrap().reset.as_ref().unwrap().reason.contains("changed since the plan was frozen"));
    // and signatures that take too long drop it too
    let w = engage_world(clock.now());
    sign(&mut e, id, &op2, &w).unwrap();
    clock.advance_ms(130_000);
    let out = e.step_engagements(&engage_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert!(i.concurrences().is_empty());
    let why = &i.engagement().unwrap().reset.as_ref().unwrap().reason;
    assert!(why.contains("signing_time"), "{why}");
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_signature_quotes_the_picture_its_signer_was_shown() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    let a2 = op2.engagement_authority().unwrap();
    let a3 = op3.engagement_authority().unwrap();
    // a token nobody issued
    let err = e.approve_engagement(id, a2, None, 999, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::StalePicture { .. }), "{err}");
    // another signer's token
    let t3 = e.issue_picture(id, a3, &w).unwrap();
    let err = e.approve_engagement(id, a2, None, t3.id, &w, &terrain()).unwrap_err();
    assert!(err.to_string().contains("issued to op-3"), "{err}");
    // an expired one
    let t2 = e.issue_picture(id, a2, &w).unwrap();
    clock.advance_ms(20_000);
    let now = engage_world(clock.now());
    let err = e.approve_engagement(id, a2, None, t2.id, &now, &terrain()).unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
    // one shown before the target was reclassified
    let t2 = e.issue_picture(id, a2, &now).unwrap();
    let mut reclassified = now.clone();
    reclassified.tracks.get_mut("T-0001").unwrap().identity = Some("ferry".into());
    let err = e
        .approve_engagement(id, a2, None, t2.id, &reclassified, &terrain())
        .unwrap_err();
    assert!(err.to_string().contains("changed since you looked"), "{err}");
    assert!(e.intent(id).unwrap().concurrences().is_empty());
    // one shown before another signature froze the plan
    let t2 = e.issue_picture(id, a2, &now).unwrap();
    clock.advance_ms(1000);
    let later = engage_world(clock.now());
    sign(&mut e, id, &op3, &later).unwrap();
    let err = e.approve_engagement(id, a2, None, t2.id, &later, &terrain()).unwrap_err();
    assert!(err.to_string().contains("frozen by op-3"), "{err}");
    // the one shown now is accepted, once
    let t2 = e.issue_picture(id, a2, &later).unwrap();
    assert!(t2.frozen);
    e.approve_engagement(id, a2, None, t2.id, &later, &terrain()).unwrap();
    assert_eq!(e.intent(id).unwrap().state(), State::Approved);
    assert!(has_event(&log, "picture.issued"));
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn an_agent_order_on_an_engaging_platform_goes_to_a_human() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (_, _, id) = two_signers(&mut e, &w);
    // an agent's hold on blade-1 would run on its own anywhere else; here it waits
    let hold = || Proposal::new(Task::Hold {}, vec![uid("blade-1")]);
    let mut port = e.agent_port(AgentId::new("agent-planner"));
    let a = port
        .propose(hold(), Rationale::new("hold it there").unwrap(), &w, &terrain())
        .unwrap();
    assert_eq!(a.state(), State::PendingApproval, "{:?}", a.history());
    assert_eq!(a.policy().unwrap().rule, "(engagement in play)");
    let aid = a.id();
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    // the agent's preview says so too
    let pv = e
        .agent_port(AgentId::new("agent-planner"))
        .preview(hold(), &w, &terrain())
        .unwrap();
    assert_eq!(pv.would_reach, State::PendingApproval);
    // approving the agent's order would stop the engagement: only acknowledged
    let op = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    let err = e.approve(aid, op.authority(), None, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::SupersedesEngagement { .. }), "{err}");
    assert_eq!(e.intent(aid).unwrap().state(), State::PendingApproval);
    e.approve_acknowledging(aid, op.authority(), None, &w, &terrain()).unwrap();
    assert_eq!(e.intent(aid).unwrap().state(), State::Approved);
    e.dispatch(aid, &w).unwrap();
    e.supersede_engagement(id, aid).unwrap();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Voided);
    let stop = i.engagement().unwrap().stop.clone().unwrap();
    assert_eq!(stop.fallback, None);
    assert!(matches!(stop.cause, StopCause::Superseded { by, .. } if by == aid));
    let line = log
        .text()
        .lines()
        .find(|l| l.contains("\"type\":\"engage.stopped\""))
        .unwrap()
        .to_string();
    assert!(line.contains("\"fallback\":null"), "{line}");
    assert!(line.contains("\"superseded\""), "{line}");
    let granted = log
        .text()
        .lines()
        .find(|l| l.contains("\"type\":\"approval.granted\""))
        .unwrap()
        .to_string();
    assert!(granted.contains("\"supersedes\":[1]"), "{granted}");
    // with the engagement gone (and the operator's hold under way: rules 5 protects a human's
    // order no vehicle started yet), the agent's next hold runs on its own again
    e.record_op_state(aid, &uid("blade-1"), "done", "holding").unwrap();
    let mut port = e.agent_port(AgentId::new("agent-planner"));
    let b = port
        .propose(hold(), Rationale::new("hold it there").unwrap(), &w, &terrain())
        .unwrap();
    assert_eq!(b.state(), State::AutoApproved, "{:?}", b.history());
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_gap_in_holding_the_picture_aborts() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    for _ in 0..3 {
        clock.advance_ms(1000);
        let out = e.step_engagements(&engage_world(clock.now()), &terrain());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
    }
    assert_eq!(e.intent(id).unwrap().state(), State::Dispatched);
    // the loop stalls for 12 s
    clock.advance_ms(12_000);
    let out = e.step_engagements(&engage_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted, "{:?}", i.history());
    assert!(i.history().last().unwrap().reason.contains("picture_gap"), "{:?}", i.history());
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn losing_gnss_after_approval_aborts() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    clock.advance_ms(1000);
    let mut w = engage_world(clock.now());
    w.vehicles.get_mut(&uid("blade-1")).unwrap().nav_source =
        Some(Fact::new(NavSource::DeadReckoning, clock.now()));
    let out = e.step_engagements(&w, &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted, "{:?}", i.history());
    assert!(i.history().last().unwrap().reason.contains("platform_nav"));
    assert!(matches!(out.events.as_slice(), [EngagementEvent::Stopped(f)] if f.fallback == Fallback::Return));
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn checks_cover_the_whole_effect_circle_and_protect_undeclared_tracks() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let rule = |i: &Intent, r: &str| {
        i.checks()
            .iter()
            .find(|c| c.rule.as_str() == r)
            .map(|c| c.outcome)
    };
    // the effect point 40 m inside the zone's east edge (-122.481), the circle 75 m:
    // part of it outside the zone
    let edge = engage::offset(LonLat::new(-122.481, 37.8150), -40.0, 0.0);
    let w = world_with_target(T0, edge, [0.0, 0.0]);
    let i = e.submit(op.authority(), nomination(), &w, &terrain()).unwrap();
    assert_eq!(rule(i, "engagement_zone"), Some(Outcome::Fail), "{:?}", i.checks());
    // another hostile track 100 m from the effect point, never declared by an operator:
    // protected
    let mut w = engage_world(T0);
    let mut other = target(T0, engage::offset(TARGET_AT, 100.0, 0.0), [0.0, 0.0], false);
    other.affiliation = Fact::new(Affiliation::Hostile, T0);
    w.tracks.insert("T-0002".into(), other.clone());
    let i = e.submit(op.authority(), nomination(), &w, &terrain()).unwrap();
    assert_eq!(rule(i, "keep_out_tracks"), Some(Outcome::Fail), "{:?}", i.checks());
    // declared by an operator: exempt (a design question, DECISIONS.md)
    other.declared_by = Some("op-1".into());
    w.tracks.insert("T-0002".into(), other);
    let i = e.submit(op.authority(), nomination(), &w, &terrain()).unwrap();
    assert_eq!(rule(i, "keep_out_tracks"), Some(Outcome::Pass), "{:?}", i.checks());
}

#[test]
fn an_unclassified_target_classified_after_approval_voids() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut p = policy();
    // a file that would let an unclassified track be engaged: the reclassification check
    // must still see "unclassified then a kind" as a change
    p.rules.engage.as_mut().unwrap().min_identity_confidence = 0.3;
    let mut e = Engine::with_areas(p, Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone()))
        .unwrap();
    let snap_world = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &snap_world);
    sign(&mut e, id, &op2, &snap_world).unwrap();
    sign(&mut e, id, &op3, &snap_world).unwrap();
    let snap = e.intent(id).unwrap().engagement().unwrap().snapshot.clone().unwrap();
    let mut then = snap.track.clone();
    then.identity = None;
    let mut now = snap.track.clone();
    now.identity = Some("small_fast_boat".into());
    let mut s2 = snap.clone();
    s2.track = then;
    let mut w = engage_world(T0.plus_ms(1000));
    w.tracks.insert("T-0001".into(), now);
    let (results, ok, _) = engage::picture(
        e.intent(id).unwrap().spec(),
        &s2,
        &w.subset_for(e.intent(id).unwrap().spec()),
        &e.policy().rules,
        false,
        T0,
    );
    assert!(!ok, "{results:?}");
    assert!(
        results.iter().any(|c| c.rule.as_str() == "target_identity" && c.outcome == Outcome::Fail),
        "{results:?}"
    );
}

#[test]
fn engage_thresholds_are_bounded() {
    for (field, v) in [
        ("keep_out_m", 0.0),
        ("min_identity_confidence", 0.0),
        ("max_track_age_s", 1e9),
        ("max_picture_gap_s", 600.0),
        ("token_ttl_s", 0.0),
        ("max_signing_s", 1e6),
        ("deviation_fraction", f64::NAN),
    ] {
        let mut r = engage::EngageRules::default();
        match field {
            "keep_out_m" => r.keep_out_m = v,
            "min_identity_confidence" => r.min_identity_confidence = v,
            "max_track_age_s" => r.max_track_age_s = v,
            "max_picture_gap_s" => r.max_picture_gap_s = v,
            "token_ttl_s" => r.token_ttl_s = Some(v),
            "max_signing_s" => r.max_signing_s = v,
            _ => r.deviation_fraction = v,
        }
        let err = r.validate().unwrap_err();
        assert!(err.contains(field), "{err}");
    }
    assert!(engage::EngageRules::default().validate().is_ok());
    // and the freshness the keep-outs trust
    let text = std::fs::read_to_string(sim_dir().join("policy.toml")).unwrap();
    for (from, to) in [
        ("max_fact_age_s = 10 ", "max_fact_age_s = 100000 "),
        ("fact_age_intervals = 3 ", "fact_age_intervals = 500 "),
    ] {
        assert!(text.contains(from), "{from}");
        let err = Policy::from_toml(&text.replace(from, to)).unwrap_err();
        assert!(err.to_string().contains(from.split(' ').next().unwrap()), "{err}");
    }
}

/// `terrain()`, but every effect point is answered from a sample 30 m east of it.
struct NearSamples;

impl TerrainOracle for NearSamples {
    fn sample(&self, t: Time, points: &[LonLat]) -> Result<Vec<Surface>, String> {
        terrain().sample(t, points)
    }

    fn sample_near(&self, t: Time, p: LonLat, _within_m: f64) -> Result<NearSample, String> {
        let at = engage::offset(p, 30.0, 0.0);
        Ok(NearSample {
            surface: terrain().sample(t, &[at])?.remove(0),
            at,
            distance_m: geo::haversine_m(p, at),
        })
    }
}

#[test]
fn a_near_terrain_sample_is_logged_and_bounded() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    // an armed USV: the effect point must be on water
    let mut w = engage_world(T0);
    let mut boat = vehicle("magura", LonLat::new(-122.4800, 37.8250));
    boat.nav_source = Some(Fact::new(NavSource::Gnss, T0));
    w.vehicles.insert(uid("boat-1"), boat);
    let mut p = nomination();
    p.uids = vec![uid("boat-1")];
    let i = e.submit(op.authority(), p, &w, &NearSamples).unwrap();
    let reach = i.checks().iter().find(|c| c.rule.as_str() == "reach").unwrap();
    assert_eq!(reach.outcome, Outcome::Pass, "{reach:?}");
    assert!(reach.detail.contains("on water"), "{}", reach.detail);
    let text = log.text();
    assert!(text.contains("\"from\":{"), "the sample's own place is logged");
    assert!(replay::replay_str(&text).unwrap().ok());
    // a log that says the sample was 80 m away (beyond 50 m), or not where it says
    for (from, to) in [("\"distance_m\":30.0", "\"distance_m\":80.0"), ("\"distance_m\":30.0", "\"distance_m\":10.0")] {
        assert!(text.contains(from), "{from}");
        let r = replay::replay_str(&text.replace(from, to)).unwrap();
        assert!(
            r.mismatches.iter().any(|m| m.what.contains("terrain answer from a sample")),
            "{:?}",
            r.mismatches
        );
    }
}

#[test]
fn a_signature_logs_the_instant_its_token_was_checked_at() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, _, id) = two_signers(&mut e, &w);
    clock.advance_ms(2000);
    let w = engage_world(clock.now());
    sign(&mut e, id, &op2, &w).unwrap();
    let text = log.text();
    let line = text
        .lines()
        .find(|l| l.contains("\"type\":\"approval.concurred\""))
        .unwrap()
        .to_string();
    let at = (T0.0 + 2000).to_string();
    assert!(line.contains(&format!("\"checked_t\":{at}")), "{line}");
    assert!(replay::replay_str(&text).unwrap().ok());
    // a log that moves the instant out of the signature's re-check (to make an expired
    // token look fresh, say) is caught
    for forged in [(T0.0 - 60_000).to_string(), (T0.0 + 60_000).to_string()] {
        let r = replay::replay_str(&text.replace(&format!("\"checked_t\":{at}"), &format!("\"checked_t\":{forged}")))
            .unwrap();
        assert!(
            r.mismatches.iter().any(|m| m.what.contains("checked at an instant outside")),
            "{:?}",
            r.mismatches
        );
    }
}

#[test]
fn one_engagement_per_platform() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    // a second nomination on blade-1, for another declared hostile: its dispatch would stop
    // the first, so it is rejected
    let mut w = engage_world(T0);
    w.tracks.insert("T-0002".into(), target(T0, LonLat::new(-122.5100, 37.8150), [0.0, 0.0], true));
    let op = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    let mut p = nomination();
    if let Task::Engage { track, .. } = &mut p.task {
        *track = "T-0002".into();
    }
    let i = e.submit(op.authority(), p, &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::Rejected, "{:?}", i.history());
    let free = i.checks().iter().find(|c| c.rule.as_str() == "platform_free").unwrap();
    assert_eq!(free.outcome, Outcome::Fail, "{free:?}");
    assert!(free.detail.contains(&id.to_string()), "{}", free.detail);
    assert!(replay::replay_str(&log.text()).unwrap().ok());
}

#[test]
fn a_stale_first_picture_is_refused_not_fatal() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    // the target crosses at 8 m/s
    let w = world_with_target(T0, LonLat::new(-122.5050, 37.8150), [-8.0, 0.0]);
    let (op2, _, id) = two_signers(&mut e, &w);
    let auth = op2.engagement_authority().unwrap();
    let token = e.issue_picture(id, auth, &w).unwrap();
    // it turns around before op-2 signs: the plan shown no longer holds, the nomination does
    clock.advance_ms(5000);
    let turned = world_with_target(clock.now(), LonLat::new(-122.5055, 37.8150), [8.0, 0.0]);
    let err = e
        .approve_engagement(id, auth, None, token.id, &turned, &terrain())
        .unwrap_err();
    assert!(err.to_string().contains("look again"), "{err}");
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    // a fresh look signs
    assert_eq!(sign(&mut e, id, &op2, &turned).unwrap(), State::PendingApproval);
    assert_eq!(e.intent(id).unwrap().concurrences().len(), 1);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_wobbling_velocity_estimate_keeps_the_plan_a_real_turn_drops_it() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, _, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    // the holding target's velocity estimate wobbles by 1.2 m/s: the plan holds
    for (k, v) in [[1.2, 0.0], [-0.8, 0.9], [0.4, -1.1]].into_iter().enumerate() {
        clock.advance_ms(1000);
        let mut w = engage_world(clock.now());
        w.tracks.get_mut("T-0001").unwrap().velocity_mps = Some(v);
        let out = e.step_engagements(&w, &terrain());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(e.intent(id).unwrap().concurrences().len(), 1, "wobble {k}");
    }
    // it gets under way at 4 m/s: dropped at once
    clock.advance_ms(1000);
    let w = world_with_target(clock.now(), TARGET_AT, [-4.0, 0.0]);
    e.step_engagements(&w, &terrain());
    let i = e.intent(id).unwrap();
    assert!(i.concurrences().is_empty());
    assert!(i.engagement().unwrap().reset.as_ref().unwrap().reason.contains("plan"));
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_gps_glitch_after_approval_aborts() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    clock.advance_ms(1000);
    // the filter still says GNSS, but flags the GPS as glitching
    let mut w = engage_world(clock.now());
    w.vehicles.get_mut(&uid("blade-1")).unwrap().gps_glitch = Some(Fact::new(true, clock.now()));
    e.step_engagements(&w, &terrain());
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted, "{:?}", i.history());
    assert!(i.history().last().unwrap().reason.contains("glitching"));
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

fn circle(center: LonLat, radius_m: f64, t: Time) -> Fact<Circle> {
    Fact::new(
        Circle {
            center,
            radius_m,
            source: Some(CircleSource::Loiter),
            center_t: Some(t),
        },
        t,
    )
}

/// One of our fixed wings at `at`, heading `vel`, with a reported circle.
fn circling_friend(at: LonLat, vel: [f64; 2], c: Option<Fact<Circle>>) -> WorldSnapshot {
    let mut w = engage_world(T0);
    let mut f = vehicle("switchblade", at);
    f.velocity_mps = Some(Fact::new(vel, T0));
    f.circling = c;
    w.vehicles.insert(uid("blade-2"), f);
    w
}

fn friendly(w: &WorldSnapshot, effect_t: Time, version: u32) -> CheckResult {
    let mut cfg = policy().rules;
    cfg.rules_version = version;
    engage::keep_out_friendly(TARGET_AT, effect_t, 275.0, Some(&uid("blade-1")), w, &cfg)
}

#[test]
fn a_circling_friendly_is_predicted_on_its_circle_not_along_its_heading() {
    // one of our fixed wings, 600 m north of the effect point on the south edge of a 200 m
    // circle, heading straight at it: 30 s ahead its heading puts it on the point
    let p = TARGET_AT;
    let center = engage::offset(p, 0.0, 800.0);
    let mut w = circling_friend(
        engage::offset(p, 0.0, 600.0),
        [0.0, -20.0],
        Some(circle(center, 200.0, T0)),
    );
    let effect_t = T0.plus_ms(30_000);
    // rules 2 carry it along its heading, circle or not
    let c = friendly(&w, effect_t, 2);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("predicted along its velocity"), "{}", c.detail);
    // rules 3: its disc (800 m out, less 200 m, the 50 m slack and 60 m of uncertainty: 490 m)
    // or 5 s along its heading (500 m less 60 m: 440 m), the nearer
    let c = friendly(&w, effect_t, 3);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    assert!(
        ["439", "440", "441"]
            .iter()
            .any(|d| c.detail.contains(&format!("blade-2 at {d} m"))),
        "{}",
        c.detail
    );
    // a circle that reaches into the keep-out still blocks it (the vehicle on its edge)
    let v = w.vehicles.get_mut(&uid("blade-2")).unwrap();
    v.circling = Some(circle(engage::offset(p, 0.0, 400.0), 200.0, T0));
    let c = friendly(&w, effect_t, 3);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("circling within 200 m"), "{}", c.detail);
    // and a circle whose center is old is not trusted: back to its heading
    let v = w.vehicles.get_mut(&uid("blade-2")).unwrap();
    v.circling = Some(circle(center, 200.0, T0.plus_ms(-60_000)));
    let c = friendly(&w, effect_t, 3);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("predicted along its velocity"), "{}", c.detail);
}

#[test]
fn a_circle_that_does_not_hold_the_friendly_is_not_trusted() {
    // the fifth review's probe: our vehicle 50 m from the effect point, a fresh circle fact
    // 1500 m north. The circle can't hold a vehicle that is not on it: its position counts
    let p = TARGET_AT;
    let w = circling_friend(
        engage::offset(p, 0.0, 50.0),
        [0.0, 0.0],
        Some(circle(engage::offset(p, 0.0, 1500.0), 210.0, T0)),
    );
    for effect_t in [T0, T0.plus_ms(60_000)] {
        let c = friendly(&w, effect_t, 3);
        assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
        assert!(c.detail.contains("does not hold its position"), "{}", c.detail);
    }
    // a vehicle just outside its circle, within the slack, is on it
    let w = circling_friend(
        engage::offset(p, 0.0, 1500.0 - 210.0 - 40.0),
        [0.0, 0.0],
        Some(circle(engage::offset(p, 0.0, 1500.0), 210.0, T0)),
    );
    let c = friendly(&w, T0.plus_ms(60_000), 3);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
}

#[test]
fn the_disc_is_never_farther_than_the_reported_position() {
    // the targeted check's probe: at the release, our fixed wing reported 243 m from the
    // effect point (inside the 275 m keep-out), on the slack band of its circle (210 m
    // around a center 498 m north), heading in at 28 m/s, its position some seconds old.
    // Whatever the span, it is no farther than where it was reported, less nothing
    let p = TARGET_AT;
    let center = engage::offset(p, 0.0, 498.0);
    let at = engage::offset(center, 0.0, -(210.0 + 45.0));
    for age_s in [0, 4, 5, 6, 7, 10] {
        let pos_t = T0.plus_ms(-age_s * 1000);
        let mut w = circling_friend(at, [0.0, -28.0], Some(circle(center, 210.0, pos_t)));
        let v = w.vehicles.get_mut(&uid("blade-2")).unwrap();
        v.position = Some(Fact::new(at, pos_t));
        v.velocity_mps = Some(Fact::new([0.0, -28.0], pos_t));
        let c = friendly(&w, T0, 3);
        assert_eq!(c.outcome, Outcome::Fail, "{age_s} s: {}", c.detail);
    }
}

#[test]
fn a_circling_friendly_counts_from_its_position_too_without_a_cliff() {
    // on its 200 m circle 600 m north of the effect point, 40 m outside the radius (within
    // the slack)
    let p = TARGET_AT;
    let at = engage::offset(p, 0.0, 360.0);
    let c600 = engage::offset(p, 0.0, 600.0);
    // flying off its circle toward the point at 20 m/s: 5 s of its heading counts at every
    // span, 30 s out (260 m less 60 m) as 5 s out (260 m less 10 m)
    let w = circling_friend(at, [0.0, -20.0], Some(circle(c600, 200.0, T0)));
    for span_ms in [30_000, 5_000] {
        let c = friendly(&w, T0.plus_ms(span_ms), 3);
        assert_eq!(c.outcome, Outcome::Fail, "{span_ms}: {}", c.detail);
        assert!(c.detail.contains("for 5 s, then on its circle"), "{}", c.detail);
    }
    // flying along its circle (east): its disc counts, 30 s out (350 m less 60 m) as 5 s out
    let w = circling_friend(at, [20.0, 0.0], Some(circle(c600, 200.0, T0)));
    for span_ms in [30_000, 6_000, 5_000, 4_000] {
        let c = friendly(&w, T0.plus_ms(span_ms), 3);
        assert_eq!(c.outcome, Outcome::Pass, "{span_ms}: {}", c.detail);
    }
}

#[test]
fn a_circle_on_a_reduced_link_holds_its_friendly() {
    // a reduced link: positions every 2 s, the navigation target every 5 s or so. The
    // center 8 s old and the position 1 s old are each fresh on their own, so the circle
    // holds; a center 13 s old does not
    let p = TARGET_AT;
    let center = engage::offset(p, 0.0, 800.0);
    let at = engage::offset(p, 0.0, 600.0);
    let pos_t = T0.plus_ms(-1_000);
    let effect_t = T0.plus_ms(30_000);
    let make = |center_age_ms: i64| {
        let mut w = circling_friend(at, [0.0, -20.0], None);
        let v = w.vehicles.get_mut(&uid("blade-2")).unwrap();
        v.report_interval_ms = Some(2_000);
        v.position = Some(Fact::new(at, pos_t));
        v.velocity_mps = Some(Fact::new([0.0, -20.0], pos_t));
        let ct = T0.plus_ms(-center_age_ms);
        let mut c = circle(center, 200.0, ct);
        c.t = if ct < pos_t { ct } else { pos_t };
        v.circling = Some(c);
        w
    };
    let c = friendly(&make(8_000), effect_t, 3);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    let c = friendly(&make(13_000), effect_t, 3);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("does not hold its position"), "{}", c.detail);
}

#[test]
fn a_fix_from_high_latency2_counts_and_says_so() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    clock.advance_ms(1000);
    // the platform reports only HIGH_LATENCY2 now: no fix type, no GPS failure flag
    let mut w = engage_world(clock.now());
    w.vehicles.get_mut(&uid("blade-1")).unwrap().gps_fix_from = Some(GpsFixFrom::HighLatency2);
    e.step_engagements(&w, &terrain());
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Dispatched, "{:?}", i.history());
    assert!(
        log.text().contains("3D fix: a HIGH_LATENCY2 report without its GPS failure flag"),
        "the picture says where the fix came from"
    );
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

// ---- what was executed is what was approved (sixth review) -----------------------------------

fn not_its_order<T: std::fmt::Debug>(r: Result<T, EngineError>) -> bool {
    matches!(r, Err(EngineError::NotItsOrder { .. }))
}

#[test]
fn a_stop_for_a_failsafe_quotes_its_own_platforms_event_and_the_fallback_it_released() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    clock.advance_ms(1000);
    let out = e.step_engagements(&engage_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    // the intercept is reported on its own platform only, and no fallback before a stop
    e.record_op_state(id, &uid("blade-1"), "intercept sent", "uploading")
        .unwrap();
    assert!(not_its_order(e.record_op_state(id, &uid("sky-1"), "intercept sent", "")));
    assert!(not_its_order(e.record_op_state(
        id,
        &uid("blade-1"),
        "fallback",
        "return commanded (rtl): early"
    )));
    // the bridge logs the platform's failsafe and stops the engagement quoting it
    let text = "[severity 4] GCS Failsafe On: switched to RTL";
    e.record_vehicle_event(&uid("blade-1"), "failsafe", text)
        .unwrap();
    let f = e
        .stop_engagement_system(id, &format!("the platform's failsafe took over ({text})"))
        .unwrap()
        .unwrap();
    assert_eq!((f.uid.as_str(), f.fallback), ("blade-1", Fallback::Return));
    // and reports the fallback it commanded: the stop's, not another
    let wrong = format!("abort commanded (hold): {}", f.reason);
    assert!(not_its_order(e.record_op_state(id, &f.uid, "fallback", &wrong)));
    let right = format!("{} commanded (rtl): {}", f.fallback.as_str(), f.reason);
    e.record_op_state(id, &f.uid, "fallback", &right).unwrap();
    e.record_op_state(id, &f.uid, "fallback acked", "mode RTL")
        .unwrap();
    let honest = log.text();
    let r = replay::replay_str(&honest).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // the failsafe pinned on another vehicle, its chain recomputed
    let forged = forge(&honest, |ev| {
        for e in ev.iter_mut() {
            if let audit::EventBody::VehicleEvent { uid, .. } = &mut e.body {
                *uid = Uid::new("sky-1");
            }
        }
    });
    let r = replay::replay_str(&forged).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "stop quotes a vehicle event its platform did not report"),
        "{:?}",
        r.mismatches
    );
}

/// Two engagements on blade-1, as in a live run (`phase-e-rules2c`): A dispatched, its
/// intercept reported, aborted by an operator with its fallback commanded; later B
/// nominated, signed, dispatched, its intercept reported, then stopped when holding its
/// picture lapsed (a 12 s stall), its fallback commanded. Returns the log and (A, B).
fn two_engagements_on_one_platform() -> (String, IntentId, IntentId) {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, a) = two_signers(&mut e, &w);
    sign(&mut e, a, &op2, &w).unwrap();
    sign(&mut e, a, &op3, &w).unwrap();
    e.dispatch_order(a).unwrap();
    e.record_op_state(a, &uid("blade-1"), "intercept sent", "uploading 2 mission items")
        .unwrap();
    let f = e
        .abort_engagement(a, op2.authority(), "not now")
        .unwrap()
        .unwrap();
    let commanded = format!("{} commanded ({}): {}", f.fallback.as_str(), f.fallback.task(), f.reason);
    e.record_op_state(a, &f.uid, "fallback", &commanded).unwrap();
    e.record_op_state(a, &f.uid, "intercept superseded", "replaced by rtl #2")
        .unwrap();
    // B, a minute later
    clock.advance_ms(60_000);
    let w = engage_world(clock.now());
    let op9 = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    let b = e.submit(op9.authority(), nomination(), &w, &terrain()).unwrap().id();
    sign(&mut e, b, &op2, &w).unwrap();
    sign(&mut e, b, &op3, &w).unwrap();
    e.dispatch_order(b).unwrap();
    // A's fallback is replaced by B's intercept
    e.record_op_state(a, &uid("blade-1"), "fallback superseded", "replaced by orbit #3")
        .unwrap();
    clock.advance_ms(1000);
    e.record_op_state(b, &uid("blade-1"), "intercept active", "2935 m from center")
        .unwrap();
    let out = e.step_engagements(&engage_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    clock.advance_ms(12_000);
    let out = e.step_engagements(&engage_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let Some(EngagementEvent::Stopped(f)) = out.events.into_iter().next() else {
        panic!("B was not stopped: {:?}", e.intent(b).unwrap().history());
    };
    let commanded = format!("{} commanded ({}): {}", f.fallback.as_str(), f.fallback.task(), f.reason);
    e.record_op_state(b, &f.uid, "fallback", &commanded).unwrap();
    e.record_op_state(b, &f.uid, "intercept superseded", "replaced by rtl #4")
        .unwrap();
    assert_eq!(e.intent(a).unwrap().state(), State::Aborted);
    assert_eq!(e.intent(b).unwrap().state(), State::Aborted);
    (log.text(), a, b)
}

fn mismatches(text: &str) -> Vec<String> {
    replay::replay_str(text)
        .unwrap()
        .mismatches
        .into_iter()
        .map(|m| m.what)
        .collect()
}

#[test]
fn a_stop_is_not_relabelled_as_superseded_by_an_old_engagement() {
    // eG: B's picture stop relabelled "superseded by A" (aborted long before), no fallback,
    // B's fallback reports gone
    let (honest, a, b) = two_engagements_on_one_platform();
    assert!(mismatches(&honest).is_empty(), "{:?}", mismatches(&honest));
    let forged = forge(&honest, |ev| {
        ev.retain(|e| {
            !matches!(&e.body, audit::EventBody::OpState { intent, state, .. }
                if *intent == b && (state == "fallback" || state == "intercept superseded"))
        });
        for e in ev.iter_mut() {
            if let audit::EventBody::EngageStopped {
                intent,
                cause,
                reason,
                fallback,
            } = &mut e.body
                && *intent == b
            {
                *cause = StopCause::Superseded {
                    by: a,
                    source: Source::Human {
                        id: OperatorId::new("op-1"),
                    },
                };
                *reason = format!("superseded by {a} from human op-1: that order stands; no fallback commanded");
                *fallback = None;
            }
        }
    });
    let w = mismatches(&forged);
    assert!(w.iter().any(|m| m == "superseded by an order no longer under way"), "{w:?}");
    assert!(
        w.iter().any(|m| m == "a failed picture's stop reported with another cause than the picture"),
        "{w:?}"
    );
}

#[test]
fn a_picture_stop_is_not_relabelled_as_a_system_stop() {
    // eF3: B's picture stop relabelled as a system stop quoting an intercept failure
    let (honest, _, b) = two_engagements_on_one_platform();
    let forged = forge(&honest, |ev| {
        for e in ev.iter_mut() {
            if let audit::EventBody::EngageStopped {
                intent,
                cause,
                reason,
                ..
            } = &mut e.body
                && *intent == b
            {
                *cause = StopCause::System;
                *reason = "system: the platform's intercept failed (lost)".into();
            }
        }
    });
    let w = mismatches(&forged);
    assert!(
        w.iter().any(|m| m == "a failed picture's stop reported with another cause than the picture"),
        "{w:?}"
    );
    assert!(
        w.iter().any(|m| m == "stop quotes an intercept report the engagement did not make"),
        "{w:?}"
    );
}

#[test]
fn an_intercept_report_is_not_moved_to_an_earlier_engagement() {
    // eH: B's "intercept active" credited to A, aborted before B took the platform
    let (honest, a, b) = two_engagements_on_one_platform();
    let forged = forge(&honest, |ev| {
        for e in ev.iter_mut() {
            if let audit::EventBody::OpState { intent, state, .. } = &mut e.body
                && *intent == b
                && state == "intercept active"
            {
                *intent = a;
            }
        }
    });
    let w = mismatches(&forged);
    assert!(
        w.iter().any(|m| m == "op state for an order a newer dispatch replaced on that vehicle"),
        "{w:?}"
    );
}

#[test]
fn the_engine_refuses_reports_and_supersedes_a_newer_order_rules_out() {
    // A aborted, then B dispatched to the same platform: A reports only how it ended
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, first) = two_signers(&mut e, &w);
    sign(&mut e, first, &op2, &w).unwrap();
    sign(&mut e, first, &op3, &w).unwrap();
    e.dispatch_order(first).unwrap();
    e.abort_engagement(first, op2.authority(), "not now").unwrap();
    let op9 = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    let second = e.submit(op9.authority(), nomination(), &w, &terrain()).unwrap().id();
    sign(&mut e, second, &op2, &w).unwrap();
    sign(&mut e, second, &op3, &w).unwrap();
    e.dispatch_order(second).unwrap();
    let blade = uid("blade-1");
    assert!(not_its_order(e.record_op_state(first, &blade, "intercept active", "")));
    assert!(not_its_order(e.record_op_state(second, &blade, "intercept superseded", "")));
    e.record_op_state(first, &blade, "intercept superseded", "replaced by orbit #2")
        .unwrap();
    // an operator's hold dispatched to blade-1 supersedes B; A, long aborted, can't
    let hold = e
        .submit(op9.authority(), Proposal::new(Task::Hold {}, vec![blade.clone()]), &w, &terrain())
        .unwrap()
        .id();
    e.dispatch(hold, &w).unwrap();
    assert!(not_its_order(e.supersede_engagement(second, first)));
    e.supersede_engagement(second, hold).unwrap();
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_stop_for_a_failed_intercept_quotes_its_report() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    sign(&mut e, id, &op2, &w).unwrap();
    sign(&mut e, id, &op3, &w).unwrap();
    e.dispatch_order(id).unwrap();
    // the bridge logs the intercept's failure and stops the engagement quoting it
    e.record_op_state(id, &uid("blade-1"), "intercept failed", "mission upload timed out")
        .unwrap();
    e.stop_engagement_system(id, "the platform's intercept failed: mission upload timed out")
        .unwrap();
    let honest = log.text();
    assert!(mismatches(&honest).is_empty(), "{:?}", mismatches(&honest));
    let forged = forge(&honest, |ev| {
        for e in ev.iter_mut() {
            if let audit::EventBody::EngageStopped { reason, .. } = &mut e.body {
                *reason = "system: the platform's intercept failed: link lost".into();
            }
        }
    });
    assert!(
        mismatches(&forged).iter().any(|m| m == "stop quotes an intercept report the engagement did not make"),
        "{:?}",
        mismatches(&forged)
    );
}

#[test]
fn only_a_dispatched_order_on_the_platform_supersedes_an_engagement() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let mut w = engage_world(T0);
    w.vehicles.insert(uid("sky-1"), vehicle("x10", CRISSY));
    let (_, _, id) = two_signers(&mut e, &w);
    let op = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    let hold = |u: &str| Proposal::new(Task::Hold {}, vec![uid(u)]);
    // an operator's own holds run at once: one on another vehicle, one on the platform
    let elsewhere = e.submit(op.authority(), hold("sky-1"), &w, &terrain()).unwrap().id();
    let here = e.submit(op.authority(), hold("blade-1"), &w, &terrain()).unwrap().id();
    assert_eq!(e.intent(here).unwrap().state(), State::AutoApproved);
    e.dispatch(elsewhere, &w).unwrap();
    let before = log.lines().len();
    // not itself, not an order that does not take the platform, not one not yet dispatched
    assert!(not_its_order(e.supersede_engagement(id, id)));
    assert!(not_its_order(e.supersede_engagement(id, elsewhere)));
    assert!(not_its_order(e.supersede_engagement(id, here)));
    assert_eq!(log.lines().len(), before, "nothing logged");
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    e.dispatch(here, &w).unwrap();
    e.supersede_engagement(id, here).unwrap();
    assert_eq!(e.intent(id).unwrap().state(), State::Voided);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}
