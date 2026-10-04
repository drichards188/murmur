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
    assert!(files.len() >= 24, "{} scenarios", files.len());
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

/// What a console shows a signer: the engine's preview of intent `id` on `w`.
fn seen(e: &Engine, id: IntentId, w: &WorldSnapshot) -> SeenPicture {
    let spec = e.intent(id).unwrap().spec().clone();
    let p = engage::preview(&spec, &w.subset_for(&spec), &e.policy().rules).unwrap();
    SeenPicture {
        t: w.t,
        effect_point: p.effect_point,
        effect_t: Some(p.effect_t),
    }
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
    // and the proposer, with engagement authority, still can't approve it
    let id = i.id();
    let w = engage_world(T0);
    let err = e
        .approve_engagement(
            id,
            op.engagement_authority().unwrap(),
            None,
            seen(&e, id, &w),
            &w,
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
    let w = engage_world(clock.now());
    e.approve_engagement(
        id,
        op2.engagement_authority().unwrap(),
        None,
        seen(&e, id, &w),
        &w,
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
            seen(&e, id, &w),
            &w,
            &terrain(),
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::AlreadySigned { .. }), "{err}");
    e.approve_engagement(
        id,
        op3.engagement_authority().unwrap(),
        None,
        seen(&e, id, &w),
        &w,
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
fn every_signature_is_on_the_same_picture() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, op3, id) = two_signers(&mut e, &w);
    e.approve_engagement(id, op2.engagement_authority().unwrap(), None, seen(&e, id, &w), &w, &terrain())
        .unwrap();
    // the track jumps 300 m between the signatures (beyond the 75 m tolerance), still a
    // valid nomination where it is now
    clock.advance_ms(3000);
    let mut moved = engage_world(clock.now());
    moved.tracks.insert(
        "T-0001".into(),
        target(clock.now(), LonLat::new(-122.4985, 37.8130), [0.0, 0.0], true),
    );
    e.approve_engagement(
        id,
        op3.engagement_authority().unwrap(),
        None,
        seen(&e, id, &moved),
        &moved,
        &terrain(),
    )
    .unwrap();
    // op-2's signature was dropped; op-3's is the first on the new picture
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    assert_eq!(i.concurrences().len(), 1);
    assert_eq!(i.concurrences()[0].by, OperatorId::new("op-3"));
    let reset = i.engagement().unwrap().reset.clone().unwrap();
    assert_eq!(reset.dropped, vec![OperatorId::new("op-2")]);
    assert!(reset.reason.contains("predicts"), "{}", reset.reason);
    assert!(has_event(&log, "approval.reset"));
    // op-2 signs again, on the picture as it is now
    clock.advance_ms(1000);
    let w = {
        let mut w = engage_world(clock.now());
        w.tracks.insert(
            "T-0001".into(),
            target(clock.now(), LonLat::new(-122.4985, 37.8130), [0.0, 0.0], true),
        );
        w
    };
    e.approve_engagement(id, op2.engagement_authority().unwrap(), None, seen(&e, id, &w), &w, &terrain())
        .unwrap();
    assert_eq!(e.intent(id).unwrap().state(), State::Approved);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_waiting_signature_is_dropped_when_the_picture_moves() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, _, id) = two_signers(&mut e, &w);
    e.approve_engagement(id, op2.engagement_authority().unwrap(), None, seen(&e, id, &w), &w, &terrain())
        .unwrap();
    // held while it holds
    clock.advance_ms(1000);
    let out = e.step_engagements(&engage_world(clock.now()));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(e.intent(id).unwrap().concurrences().len(), 1);
    // reclassified while waiting for the second signature
    clock.advance_ms(1000);
    let mut w = engage_world(clock.now());
    w.tracks.get_mut("T-0001").unwrap().identity = Some("fishing_boat".into());
    let out = e.step_engagements(&w);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.concurrences().is_empty());
    assert!(i.engagement().unwrap().reset.as_ref().unwrap().reason.contains("reclassified"));
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_signer_must_have_seen_the_current_preview() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine_at(&clock, &log);
    let w = engage_world(T0);
    let (op2, _, id) = two_signers(&mut e, &w);
    clock.advance_ms(30_000);
    let now = engage_world(clock.now());
    let auth = op2.engagement_authority().unwrap();
    // a preview 30 s old
    let mut old = seen(&e, id, &now);
    old.t = T0;
    let err = e.approve_engagement(id, auth, None, old, &now, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::StalePicture { .. }), "{err}");
    // an effect point 400 m from the engine's
    let mut off = seen(&e, id, &now);
    off.effect_point = engage::offset(off.effect_point, 0.0, 400.0);
    let err = e.approve_engagement(id, auth, None, off, &now, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::StalePicture { .. }), "{err}");
    // a preview dated in the future
    let mut ahead = seen(&e, id, &now);
    ahead.t = clock.now().plus_ms(60_000);
    let err = e.approve_engagement(id, auth, None, ahead, &now, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::StalePicture { .. }), "{err}");
    assert!(e.intent(id).unwrap().concurrences().is_empty());
    // the one on screen now is accepted
    e.approve_engagement(id, auth, None, seen(&e, id, &now), &now, &terrain())
        .unwrap();
    assert_eq!(e.intent(id).unwrap().concurrences().len(), 1);
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
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    // the agent's preview says so too
    let pv = e
        .agent_port(AgentId::new("agent-planner"))
        .preview(hold(), &w, &terrain())
        .unwrap();
    assert_eq!(pv.would_reach, State::PendingApproval);
    // an operator's own hold still runs, and supersedes the engagement (no fallback)
    let op = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    let h = e.submit(op.authority(), hold(), &w, &terrain()).unwrap();
    assert!(matches!(h.state(), State::AutoApproved | State::Approved), "{:?}", h.history());
    let hid = h.id();
    e.dispatch(hid).unwrap();
    e.supersede_engagement(id, hid).unwrap();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Voided);
    let stop = i.engagement().unwrap().stop.clone().unwrap();
    assert_eq!(stop.fallback, None);
    assert!(matches!(stop.cause, StopCause::Superseded { by, .. } if by == hid));
    assert!(!stop.reason.contains("fallback return"), "{}", stop.reason);
    let line = log
        .text()
        .lines()
        .find(|l| l.contains("\"type\":\"engage.stopped\""))
        .unwrap()
        .to_string();
    assert!(line.contains("\"fallback\":null"), "{line}");
    assert!(line.contains("\"superseded\""), "{line}");
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
    for op in [&op2, &op3] {
        e.approve_engagement(id, op.engagement_authority().unwrap(), None, seen(&e, id, &w), &w, &terrain())
            .unwrap();
    }
    e.dispatch_order(id).unwrap();
    for _ in 0..3 {
        clock.advance_ms(1000);
        let out = e.step_engagements(&engage_world(clock.now()));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
    }
    assert_eq!(e.intent(id).unwrap().state(), State::Dispatched);
    // the loop stalls for 12 s
    clock.advance_ms(12_000);
    let out = e.step_engagements(&engage_world(clock.now()));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted, "{:?}", i.history());
    assert!(i.history().last().unwrap().reason.contains("picture_gap"), "{:?}", i.history());
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
    let mut w = engage_world(T0);
    let edge = engage::offset(LonLat::new(-122.481, 37.8150), -40.0, 0.0);
    w.tracks.insert("T-0001".into(), target(T0, edge, [0.0, 0.0], true));
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
    for op in [&op2, &op3] {
        e.approve_engagement(
            id,
            op.engagement_authority().unwrap(),
            None,
            seen(&e, id, &snap_world),
            &snap_world,
            &terrain(),
        )
        .unwrap();
    }
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
        ("seen_max_age_s", 0.0),
        ("deviation_fraction", f64::NAN),
    ] {
        let mut r = engage::EngageRules::default();
        match field {
            "keep_out_m" => r.keep_out_m = v,
            "min_identity_confidence" => r.min_identity_confidence = v,
            "max_track_age_s" => r.max_track_age_s = v,
            "max_picture_gap_s" => r.max_picture_gap_s = v,
            "seen_max_age_s" => r.seen_max_age_s = v,
            _ => r.deviation_fraction = v,
        }
        let err = r.validate().unwrap_err();
        assert!(err.contains(field), "{err}");
    }
    assert!(engage::EngageRules::default().validate().is_ok());
}

