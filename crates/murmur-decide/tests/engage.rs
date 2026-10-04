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
    assert!(matches!(e.dispatch(id), Err(EngineError::EngageIntent(_))));
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
    e.dispatch(aid).unwrap();
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
    // with the engagement gone, the agent's next hold runs on its own again
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
    let mut e = Engine::with_areas(p, Some(areas()), Box::new(clock.clone()), Box::new(log.clone()))
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
    // rules 3: anywhere on its disc, 600 m out less its uncertainty (2 x 1 m/s x 30 s)
    let c = friendly(&w, effect_t, 3);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    assert!(
        c.detail.contains("blade-2 at 539 m") || c.detail.contains("blade-2 at 540 m"),
        "{}",
        c.detail
    );
    // a circle that reaches into the keep-out still blocks it (the vehicle on its edge)
    let v = w.vehicles.get_mut(&uid("blade-2")).unwrap();
    v.position = Some(Fact::new(engage::offset(p, 0.0, 600.0), T0));
    v.circling = Some(circle(engage::offset(p, 0.0, 400.0), 200.0, T0));
    let c = friendly(&w, effect_t, 3);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("circling within 200 m"), "{}", c.detail);
    // and a stale circle is not trusted: back to its heading
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
fn near_the_effect_a_circling_friendly_is_read_from_its_position_too() {
    // on its 200 m circle 600 m north of the effect point, 40 m outside the radius (within
    // the slack), heading at the point at 20 m/s
    let p = TARGET_AT;
    let w = circling_friend(
        engage::offset(p, 0.0, 360.0),
        [0.0, -20.0],
        Some(circle(engage::offset(p, 0.0, 600.0), 200.0, T0)),
    );
    // 30 s out the disc alone counts: 400 m less 60 m of uncertainty
    let c = friendly(&w, T0.plus_ms(30_000), 3);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    // 5 s out (a release, or a check just before one) its position counts too: 260 m along
    // its velocity, less 10 m, inside the 275 m keep-out
    let c = friendly(&w, T0.plus_ms(5_000), 3);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("predicted along its velocity"), "{}", c.detail);
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
