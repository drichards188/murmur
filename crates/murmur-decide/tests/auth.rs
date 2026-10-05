//! Rules 8 (murmur-der.1): every human act is bound to an authenticated person. The run says
//! how its operators prove who they are (`credentials` or `dev`); a credentials run opens
//! sessions only on a checked login, records every session's end, and takes an engagement
//! signature (or any approval its policy marks consequential) only with a step-up, the
//! signer's credential re-entered at that moment; replay checks all of it, and tells a dev
//! run apart so evaluation can exclude it from any claim about people.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::policy::TierRule;
use murmur_decide::replay::{self, ReplayOptions, RulesAccepted};
use murmur_decide::world::NavSource;
use murmur_decide::*;

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const BLADE_AT: LonLat = LonLat::new(-122.4700, 37.8160);

/// A switchblade west of the bridge and a declared-hostile boat holding in the zone (as in
/// `tests/engage.rs`), every fact fresh at `t`.
fn engage_world(t: Time) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(t);
    w.areas = Some(areas());
    let mut blade = vehicle("switchblade", BLADE_AT);
    blade.position = Some(Fact::new(BLADE_AT, t));
    blade.autopilot = Some(Fact::new(AutopilotKind::Plane, t));
    blade.nav_ready = Some(Fact::new(true, t));
    blade.nav_source = Some(Fact::new(NavSource::Gnss, t));
    blade.gps_fix = Some(Fact::new(3, t));
    blade.gps_glitch = Some(Fact::new(false, t));
    blade.battery_pct = Some(Fact::new(95.0, t));
    blade.velocity_mps = Some(Fact::new([0.0, 0.0], t));
    w.vehicles.insert(uid("blade-1"), blade);
    w.tracks.insert(
        "T-0001".into(),
        TrackFacts {
            position: Fact::new(TARGET_AT, t),
            alt_m: None,
            alt_t: None,
            velocity_mps: Some([0.0, 0.0]),
            pos_sigma_m: 15.0,
            sigma_growth_mps: Some(1.0),
            confidence: 0.9,
            affiliation: Fact::new(Affiliation::Hostile, t),
            affiliation_confidence: 0.8,
            identity: Some("small_fast_boat".into()),
            identity_confidence: 0.9,
            sources: vec!["3:radar".into()],
            source_t: [("3:radar".to_string(), t)].into_iter().collect(),
            domain: Domain::Surface,
            last_seen_t: t,
            declared_by: Some("op-1".into()),
            declaration: None,
            review_flags: Vec::new(),
            manoeuvre: None,
            mean_velocity_mps: None,
        },
    );
    w
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

/// op-1 (no engagement authority), op-2 and op-3 (engagement authority).
fn three() -> Roster {
    Roster::new(
        ["op-1", "op-2", "op-3"].map(OperatorId::new).to_vec(),
        ["op-2", "op-3"].map(OperatorId::new).to_vec(),
    )
    .unwrap()
}

fn engine(policy: Policy, roster: Roster, clock: &ManualClock, log: &MemorySink) -> Engine {
    Engine::with_areas(policy, Some(areas()), roster, Box::new(clock.clone()), Box::new(log.clone())).unwrap()
}

fn login(e: &mut Engine, op: &str, engage: bool) -> OperatorSession {
    e.open_authenticated_session(OperatorId::new(op), Grants { engage }, AuthMethod::Password)
        .unwrap()
}

fn whats(text: &str) -> Vec<String> {
    replay::replay_str(text).unwrap().mismatches.into_iter().map(|m| m.what).collect()
}

/// A credentials run: op-1 nominates, op-2 and op-3 each step up and sign. Returns the log
/// and what was refused along the way.
fn signed_engagement() -> (String, Vec<String>) {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(policy(), three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e, "op-1", false);
    let op2 = login(&mut e, "op-2", true);
    let op3 = login(&mut e, "op-3", true);
    let w = engage_world(T0);
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    assert!(e.intent(id).unwrap().policy().unwrap().step_up, "an engagement always steps up");
    let mut refused = Vec::new();
    clock.advance_ms(2000);
    let w = engage_world(clock.now());
    let a2 = op2.engagement_authority().unwrap();
    // no step-up: refused, and the refusal is logged
    let t = e.issue_picture(id, a2, &w).unwrap().id;
    let err = e.approve_engagement(id, a2, None, t, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::StepUpRequired { .. }), "{err}");
    refused.push(err.to_string());
    // with one: signature 1 of 2
    let s = e.step_up(&op2).unwrap();
    let state = e.approve_engagement_stepped(id, a2, s, None, t, &w, &terrain()).unwrap().state();
    assert_eq!(state, State::PendingApproval);
    // the same person can't supply the second signature, however often they step up
    let t2 = e.issue_picture(id, a2, &w);
    assert!(matches!(t2, Err(EngineError::AlreadySigned { .. })), "{t2:?}");
    let a3 = op3.engagement_authority().unwrap();
    let t3 = e.issue_picture(id, a3, &w).unwrap().id;
    let s = e.step_up(&op2).unwrap();
    let err = e.approve_engagement_stepped(id, a2, s, None, t3, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::AlreadySigned { .. }), "{err}");
    refused.push(err.to_string());
    // nor sign with another's step-up
    let s = e.step_up(&op2).unwrap();
    let err = e.approve_engagement_stepped(id, a3, s, None, t3, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    refused.push(err.to_string());
    // nor with a stale one
    let s = e.step_up(&op3).unwrap();
    clock.advance_ms(STEP_UP_MAX_AGE_MS + 1);
    let w = engage_world(clock.now());
    let t3 = e.issue_picture(id, a3, &w).unwrap().id;
    let err = e.approve_engagement_stepped(id, a3, s, None, t3, &w, &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    refused.push(err.to_string());
    // a fresh one: approved, the second person's credential on it
    let s = e.step_up(&op3).unwrap();
    let i = e.approve_engagement_stepped(id, a3, s, None, t3, &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::Approved);
    assert!(i.approval().unwrap().step_up);
    assert!(i.concurrences().iter().all(|a| a.step_up));
    e.close_operator_session(op1, "logout").unwrap();
    e.close_operator_session(op2, "expired (idle)").unwrap();
    e.close_operator_session(op3, "credential removed").unwrap();
    (log.text(), refused)
}

#[test]
fn a_dev_run_says_so_and_is_no_evidence_about_people() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    // a roster without a mode: the engine logs dev
    let mut e = engine(policy(), three(), &clock, &log);
    assert_eq!(e.auth_mode(), Some(AuthMode::Dev));
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    // nothing here checks a credential: no password session, no step-up
    assert!(matches!(
        e.open_authenticated_session(OperatorId::new("op-2"), Grants::default(), AuthMethod::Password),
        Err(EngineError::NotAuthenticated { .. })
    ));
    assert!(matches!(e.step_up(&op1), Err(EngineError::NotAuthenticated { .. })));
    e.submit(op1.authority(), hold(&["sky-1"]), &world(), &terrain()).unwrap();
    let text = log.text();
    let first = text.lines().next().unwrap();
    assert!(first.contains("\"roster\":{\"operators\":[\"op-1\",\"op-2\",\"op-3\"],\"engage\":[\"op-2\",\"op-3\"],\"auth\":\"dev\"}"), "{first}");
    assert!(text.contains("\"action\":\"session.opened\",\"detail\":\"\",\"auth\":\"asserted\""), "{text}");
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.auth, Some(AuthMode::Dev));
    assert!(!r.authenticated(), "a dev run is never evidence about people");
    // and in a dev run an engagement signs without a step-up (it has nothing to re-enter)
    let mut e = engine(policy(), three(), &clock, &MemorySink::new());
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let w = engage_world(clock.now());
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    let a2 = op2.engagement_authority().unwrap();
    let t = e.issue_picture(id, a2, &w).unwrap().id;
    assert!(e.approve_engagement(id, a2, None, t, &w, &terrain()).is_ok());
}

#[test]
fn a_credentials_run_opens_sessions_only_on_a_checked_login() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(policy(), three().with_auth(AuthMode::Credentials), &clock, &log);
    assert_eq!(e.auth_mode(), Some(AuthMode::Credentials));
    // the old way in (an asserted name) is closed, and so is claiming one in a password run
    let err = e.open_operator_session(OperatorId::new("op-1")).unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    let err = e
        .open_authenticated_session(OperatorId::new("op-1"), Grants::default(), AuthMethod::Asserted)
        .unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    // the roster still binds: no session for someone not on it, no authority not granted
    assert!(matches!(
        e.open_authenticated_session(OperatorId::new("mallory"), Grants::default(), AuthMethod::Password),
        Err(EngineError::NotOnRoster { .. })
    ));
    assert!(matches!(
        e.open_authenticated_session(OperatorId::new("op-1"), Grants { engage: true }, AuthMethod::Password),
        Err(EngineError::NotOnRoster { .. })
    ));
    let op1 = login(&mut e, "op-1", false);
    // two logins, two sessions
    let again = login(&mut e, "op-1", false);
    assert_ne!(op1.authority().session(), again.authority().session());
    let text = log.text();
    assert_eq!(text.lines().count(), 3, "refusals log nothing: {text}");
    assert!(text.lines().next().unwrap().contains("\"auth\":\"credentials\""));
    assert!(text.contains("\"auth\":\"password\""));
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok() && r.authenticated(), "{:?}", r.mismatches);
}

#[test]
fn every_engagement_signature_steps_up_and_two_people_sign() {
    let (text, refused) = signed_engagement();
    assert_eq!(refused.len(), 4, "{refused:?}");
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.authenticated());
    assert_eq!((r.stepped_signatures, r.sessions_closed), (2, 3));
    assert_eq!(r.step_ups, 5, "every step-up is logged, used or not");
    // the refusals are in the log, under the refused signer's session
    let events = parse_log(&text).unwrap();
    let refusals: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::OperatorAction { action, detail, .. } if action == "approval.refused" => Some(detail.as_str()),
            _ => None,
        })
        .collect();
    assert!(refusals.iter().any(|d| d.contains("step-up required")), "{refusals:?}");
    assert!(refusals.iter().any(|d| d.contains("already signed")), "{refusals:?}");
    assert!(refusals.iter().any(|d| d.contains("not this signer's")), "{refusals:?}");
    assert!(refusals.iter().any(|d| d.contains("ms old")), "{refusals:?}");
    // each signature names its step-up, a distinct event under its own session
    let quoted: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::ApprovalConcurred { step_up, .. } | EventBody::ApprovalGranted { step_up, .. } => {
                step_up.map(|s| s.seq)
            }
            _ => None,
        })
        .collect();
    assert_eq!(quoted.len(), 2);
    assert_ne!(quoted[0], quoted[1]);
}

#[test]
fn replay_catches_a_signature_without_its_step_up_or_with_anothers() {
    let (text, _) = signed_engagement();
    // the final signature's step-up stripped: a consequential signature without one
    let forged = forge(&text, |events| {
        for e in events.iter_mut() {
            if let EventBody::ApprovalGranted { step_up, .. } = &mut e.body {
                *step_up = None;
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("consequential signature without a step-up")), "{w:?}");
    // the final signature quoting the first signer's step-up (one credential, two signatures)
    let forged = forge(&text, |events| {
        let first = events.iter().find_map(|e| match &e.body {
            EventBody::ApprovalConcurred { step_up, .. } => *step_up,
            _ => None,
        });
        for e in events.iter_mut() {
            if let EventBody::ApprovalGranted { step_up, .. } = &mut e.body {
                *step_up = first;
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("not the signer's own, fresh and unused")), "{w:?}");
}

#[test]
fn replay_catches_a_relabelled_run_or_session() {
    let (text, _) = signed_engagement();
    // the run relabelled dev, its step-ups kept: a step-up outside a credentials run (and the
    // sessions were opened by password, which a dev run doesn't do)
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { roster: Some(r), .. } = &mut events[0].body {
            r.auth = Some(AuthMode::Dev);
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("a step-up outside a credentials run")), "{w:?}");
    assert!(w.iter().any(|m| m.contains("session opened by another method")), "{w:?}");
    // a session relabelled as an asserted name in a credentials run
    let forged = forge(&text, |events| {
        for e in events.iter_mut() {
            if let EventBody::OperatorAction { action, auth, .. } = &mut e.body
                && action == "session.opened"
            {
                *auth = Some(AuthMethod::Asserted);
                break;
            }
        }
    });
    assert!(whats(&forged).iter().any(|m| m.contains("session opened by another method")));
    // the mode dropped: a rules 8 log always says how its operators were authenticated
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { roster: Some(r), .. } = &mut events[0].body {
            r.auth = None;
        }
    });
    assert!(whats(&forged).iter().any(|m| m.contains("says how its operators were authenticated")));
}

#[test]
fn nothing_acts_under_an_ended_session() {
    let (text, _) = signed_engagement();
    // op-3's session closed (a logout, an expiry) before its signature
    let forged = forge(&text, |events| {
        let (at, op, session) = events
            .iter()
            .enumerate()
            .find_map(|(i, e)| match &e.body {
                EventBody::ApprovalGranted { by, session, .. } => Some((i, by.clone(), *session)),
                _ => None,
            })
            .unwrap();
        let t = events[at].t;
        events.insert(
            at - 1,
            audit::Event {
                seq: 0,
                t,
                prev: String::new(),
                body: EventBody::OperatorAction {
                    operator: op,
                    session,
                    action: "session.closed".into(),
                    detail: "logout".into(),
                    grants: None,
                    auth: None,
                    step_up: None,
                    passkey: None,
                },
            },
        );
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("an act under a session that had ended")), "{w:?}");
    // a session can't be closed in someone else's name either
    let forged = forge(&text, |events| {
        for e in events.iter_mut().rev() {
            if let EventBody::OperatorAction { action, operator, .. } = &mut e.body
                && action == "session.closed"
            {
                *operator = OperatorId::new("op-1");
                break;
            }
        }
    });
    assert!(whats(&forged).iter().any(|m| m.contains("not the operator's")));
}

#[test]
fn a_closed_session_drops_its_picture_tokens() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(policy(), three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e, "op-1", false);
    let op2 = login(&mut e, "op-2", true);
    let w = engage_world(T0);
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    let t = e.issue_picture(id, op2.engagement_authority().unwrap(), &w).unwrap().id;
    assert!(e.picture_token(t).is_some());
    e.close_operator_session(op2, "logout").unwrap();
    assert!(e.picture_token(t).is_none(), "the token died with its session");
    // a new login of the same person is a new session, with no token
    let op2 = login(&mut e, "op-2", true);
    let s = e.step_up(&op2).unwrap();
    let err = e
        .approve_engagement_stepped(id, op2.engagement_authority().unwrap(), s, None, t, &w, &terrain())
        .unwrap_err();
    assert!(matches!(err, EngineError::StalePicture { .. }), "{err}");
    assert!(replay::replay_str(&log.text()).unwrap().ok());
}

/// The shipped policy with the armed-platform tier marked consequential.
fn armed_step_up(rules_version: u32) -> Policy {
    let mut p = policy();
    for t in &mut p.tiers {
        if t.name == "armed-platform" {
            t.step_up = Some(true);
        }
    }
    p.rules.rules_version = rules_version;
    p
}

#[test]
fn a_policy_tier_can_mark_an_approval_consequential() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(armed_step_up(RULES_VERSION), three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e, "op-1", false);
    // the operator's own armed orbit is not approved by sending it: it waits
    let i = e.submit(op1.authority(), orbit(CRISSY, 300.0, &["blade-1"]), &world(), &terrain()).unwrap();
    let id = i.id();
    let d = i.policy().unwrap();
    assert!(d.step_up && !d.submitter_authorizes, "{d:?}");
    assert_eq!(i.state(), State::PendingApproval);
    // approving it needs the credential again
    let err = e.approve(id, op1.authority(), None, &world(), &terrain()).unwrap_err();
    assert!(matches!(err, EngineError::StepUpRequired { .. }), "{err}");
    let s = e.step_up(&op1).unwrap();
    let i = e.approve_stepped(id, op1.authority(), s, None, false, &world(), &terrain()).unwrap();
    assert_eq!(i.state(), State::Approved);
    assert!(i.approval().unwrap().step_up && i.approval().unwrap().self_approved);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.stepped_signatures, 1);
    // under rules 6 the key means nothing: approved at submission, as before
    let mut p = armed_step_up(6);
    p.tiers.iter_mut().for_each(|t| t.step_up = None);
    let mut e = engine(p, three(), &clock, &MemorySink::new());
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let i = e.submit(op1.authority(), orbit(CRISSY, 300.0, &["blade-1"]), &world(), &terrain()).unwrap();
    assert!(!i.policy().unwrap().step_up);
    assert_eq!(i.state(), State::Approved);
}

#[test]
fn step_up_is_a_human_tiers_key_of_policy_schema_5() {
    let mut p = policy();
    p.tiers.insert(
        0,
        TierRule {
            name: "odd".into(),
            when: Default::default(),
            tier: Tier::Auto,
            reason: "an auto tier asking for a step-up".into(),
            submitter_authorizes: None,
            approvers: None,
            proposer_may_approve: None,
            step_up: Some(true),
        },
    );
    assert!(p.validate().is_err());
    // a log of the policy with the key, claiming schema 4
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(armed_step_up(RULES_VERSION), three(), &clock, &log);
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.submit(op1.authority(), hold(&["sky-1"]), &world(), &terrain()).unwrap();
    let text = log.text();
    assert!(replay::replay_str(&text).unwrap().ok());
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { policy_schema, .. } = &mut events[0].body {
            *policy_schema = Some(STEP_UP_SCHEMA_SINCE - 1);
        }
    });
    assert!(
        whats(&forged).iter().any(|m| m.contains("a key of a later policy schema")),
        "{:?}",
        whats(&forged)
    );
}

#[test]
fn a_rules_8_log_claiming_older_rules_carries_its_markers() {
    let (text, _) = signed_engagement();
    let older = OPERATOR_AUTH_SINCE - 1;
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { rules_version, .. } = &mut events[0].body {
            *rules_version = Some(older);
        }
    });
    let r = replay::replay_str_with(
        &forged,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(older),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let w: Vec<&str> = r.mismatches.iter().map(|m| m.what.as_str()).collect();
    for marker in ["how an operator was authenticated", "a step-up", "a session's end"] {
        assert!(
            w.iter().any(|m| m.contains(marker) && m.contains(&format!("only builds of rules {OPERATOR_AUTH_SINCE}"))),
            "{marker}: {w:?}"
        );
    }
}

#[test]
fn closing_a_session_logs_nothing_before_rules_8() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut p = policy();
    p.rules.rules_version = OPERATOR_AUTH_SINCE - 1;
    let mut e = engine(p, three(), &clock, &log);
    assert_eq!(e.auth_mode(), None);
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.close_operator_session(op1, "logout").unwrap();
    let text = log.text();
    assert!(!text.contains("session.closed") && !text.contains("\"auth\""), "{text}");
    let r = replay::replay_str_with(
        &text,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(OPERATOR_AUTH_SINCE - 1),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.auth, None);
    assert!(!r.authenticated());
}

/// Review fix 7: a human's intent is made in a live session of theirs: one forged after its
/// proposer logged out (a copy of their nomination under a new id, at the end of the log) is
/// caught.
#[test]
fn a_human_intent_after_its_proposers_logout_is_caught() {
    let (text, _) = signed_engagement();
    let forged = forge(&text, |events| {
        let mut copy = events
            .iter()
            .find(|e| matches!(&e.body, EventBody::IntentCreated { intent, .. } if !intent.source.is_agent()))
            .cloned()
            .unwrap();
        if let EventBody::IntentCreated { intent, .. } = &mut copy.body {
            intent.id = IntentId(99);
        }
        copy.t = events.last().unwrap().t;
        events.push(copy);
    });
    let w = whats(&forged);
    assert!(
        w.iter().any(|m| m.contains("session that had ended") || m.contains("a live session of theirs")),
        "{w:?}"
    );
}

/// Review fix 2: a harness-held credential (`auth: scripted`) opens an authenticated session
/// in a credentials run, but its operator is no person: replay names the people (password)
/// apart from the scripted operators, and a scripted step-up can't pass as a person's.
#[test]
fn scripted_operators_are_authenticated_but_never_people() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(policy(), three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e, "op-1", false);
    let op2 = e
        .open_authenticated_session(OperatorId::new("op-2"), Grants { engage: true }, AuthMethod::Scripted)
        .unwrap();
    assert_eq!(op2.method(), Some(AuthMethod::Scripted));
    let op3 = login(&mut e, "op-3", true);
    let w = engage_world(T0);
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    for op in [&op2, &op3] {
        let a = op.engagement_authority().unwrap();
        let t = e.issue_picture(id, a, &w).unwrap().id;
        let s = e.step_up(op).unwrap();
        e.approve_engagement_stepped(id, a, s, None, t, &w, &terrain()).unwrap();
    }
    assert_eq!(e.intent(id).unwrap().state(), State::Approved);
    let text = log.text();
    assert_eq!(text.matches("\"auth\":\"scripted\"").count(), 2, "the session and its step-up: {text}");
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok() && r.authenticated(), "{:?}", r.mismatches);
    let ids = |v: &[&str]| v.iter().map(|o| OperatorId::new(*o)).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(r.people, ids(&["op-1", "op-3"]));
    assert_eq!(r.scripted, ids(&["op-2"]));
    // a scripted step-up relabelled a person's: caught
    let forged = forge(&text, |events| {
        for ev in events.iter_mut() {
            if let EventBody::OperatorAction { operator, action, auth, .. } = &mut ev.body
                && operator.as_str() == "op-2"
                && action == "step_up"
            {
                *auth = Some(AuthMethod::Password);
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("not by the credential its session was opened with")), "{w:?}");
    // a dev run opens no scripted session: nothing there is checked
    let mut d = engine(policy(), three(), &clock, &MemorySink::new());
    assert!(matches!(
        d.open_authenticated_session(OperatorId::new("op-1"), Grants::default(), AuthMethod::Scripted),
        Err(EngineError::NotAuthenticated { .. })
    ));
}

/// Review fix 1: a refused login is in the audit log (`auth.refused`: the id, why, the lock),
/// for an operator the run lists and only in a credentials run; never a secret. Replay counts
/// them and catches one where it can't be.
#[test]
fn refused_logins_are_logged_for_listed_operators() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(policy(), three().with_auth(AuthMode::Credentials), &clock, &log);
    e.record_login_refused(&OperatorId::new("op-3"), "wrong password", None).unwrap();
    clock.advance_ms(500);
    e.record_login_refused(&OperatorId::new("op-3"), "wrong password", Some(30)).unwrap();
    let n = log.lines().len();
    assert!(e.record_login_refused(&OperatorId::new("mallory"), "wrong password", None).is_err());
    assert_eq!(log.lines().len(), n, "a made-up id is not logged");
    let text = log.text();
    assert!(text.contains("\"type\":\"auth.refused\",\"operator\":\"op-3\",\"reason\":\"wrong password\",\"locked_s\":30"), "{text}");
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.logins_refused, 2);
    // a dev run has no logins to refuse
    let mut d = engine(policy(), three(), &clock, &MemorySink::new());
    assert!(d.record_login_refused(&OperatorId::new("op-3"), "wrong password", None).is_err());
    // replay: one for an id the run does not list, and one in a run relabelled dev
    let forged = forge(&text, |events| {
        if let EventBody::AuthRefused { operator, .. } = &mut events[1].body {
            *operator = OperatorId::new("mallory");
        }
    });
    assert!(whats(&forged).iter().any(|m| m.contains("an operator the run does not list")), "{:?}", whats(&forged));
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { roster: Some(r), .. } = &mut events[0].body {
            r.auth = Some(AuthMode::Dev);
        }
    });
    assert!(whats(&forged).iter().any(|m| m.contains("a refused login outside a credentials run")), "{:?}", whats(&forged));
}

/// The policy with `[approval] step_up_acts` set.
fn stepping(acts: &[StepUpAct]) -> Policy {
    let mut p = policy();
    p.approval.step_up_acts = Some(acts.to_vec());
    p
}

/// User decision (step-up scope): declaring a track hostile steps up when the policy lists
/// `declare_hostile` (off by default, as built); replay requires it and checks it.
#[test]
fn the_policy_can_make_a_hostile_declaration_step_up() {
    let clock = ManualClock::new(T0);
    // rules 9: a declaration is the identification ledger's; the step-up rides on it
    let mut w = engage_world(T0);
    let mut other = w.tracks["T-0001"].clone();
    other.declared_by = None;
    other.position.value = LonLat::new(-122.5100, 37.8200);
    w.tracks.insert("T-0002".into(), other);
    // off by default: a declaration is logged as it always was
    let log = MemorySink::new();
    let mut e = engine(policy(), three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e, "op-1", false);
    e.declare_track(op1.authority(), None, "T-0001", Affiliation::Hostile, None, None, &w).unwrap();
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok() && r.stepped_acts == 0, "{:?}", r.mismatches);
    // under rules 9 the rules 8 path is closed: a declaration that a decision would not read
    let err = e.record_declaration(&op1, None, "T-0002", "hostile", None).unwrap_err();
    assert!(matches!(err, EngineError::Identification(_)), "{err}");
    let err = e.record_operator_action(op1.authority(), "track.declare", "track T-0002 declared hostile").unwrap_err();
    assert!(matches!(err, EngineError::Identification(_)), "{err}");
    // listed: hostile needs the credential re-entered; another affiliation does not
    let log = MemorySink::new();
    let mut e = engine(stepping(&[StepUpAct::DeclareHostile]), three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e, "op-1", false);
    let err = e
        .declare_track(op1.authority(), None, "T-0001", Affiliation::Hostile, None, None, &w)
        .unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    e.declare_track(op1.authority(), None, "T-0002", Affiliation::Neutral, None, None, &w).unwrap();
    let s = e.step_up(&op1).unwrap();
    e.declare_track(op1.authority(), Some(s), "T-0001", Affiliation::Hostile, Some("fired on the cutter".into()), None, &w)
        .unwrap();
    let text = log.text();
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.stepped_acts, 1);
    // the step-up dropped from the declaration: caught
    let forged = forge(&text, |events| {
        for ev in events.iter_mut() {
            if let EventBody::IdentDeclared { declaration } = &mut ev.body
                && let Declarer::Operator { step_up, .. } = &mut declaration.by
            {
                *step_up = None;
            }
        }
    });
    assert!(whats(&forged).iter().any(|m| m.contains("a hostile declaration without a step-up")), "{:?}", whats(&forged));
    // rules 8 (the version that added the list): `track.declare` steps up by either path in
    let log = MemorySink::new();
    let mut p8 = stepping(&[StepUpAct::DeclareHostile]);
    p8.rules.rules_version = OPERATOR_AUTH_SINCE;
    let mut e8 = engine(p8, three().with_auth(AuthMode::Credentials), &clock, &log);
    let op1 = login(&mut e8, "op-1", false);
    let err = e8.record_declaration(&op1, None, "T-0001", "hostile", None).unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    let err = e8.record_operator_action(op1.authority(), "track.declare", "track T-0001 declared hostile").unwrap_err();
    assert!(matches!(err, EngineError::NotAuthenticated { .. }), "{err}");
    e8.record_declaration(&op1, None, "T-0002", "neutral", None).unwrap();
    let s = e8.step_up(&op1).unwrap();
    e8.record_declaration(&op1, Some(s), "T-0001", "hostile", Some("fired on the cutter")).unwrap();
    let r8 = replay::replay_str_with(
        &log.text(),
        &ReplayOptions {
            rules: RulesAccepted::Exactly(OPERATOR_AUTH_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r8.ok() && r8.stepped_acts == 1, "{:?}", r8.mismatches);
    // the key is policy schema 5
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { policy_schema, .. } = &mut events[0].body {
            *policy_schema = Some(STEP_UP_SCHEMA_SINCE - 1);
        }
    });
    let r = replay::replay_str(&forged).unwrap();
    assert!(r.mismatches.iter().any(|m| m.logged.contains("approval.step_up_acts")), "{:?}", r.mismatches);
    // a dev run has no credential to re-enter: the list asks nothing there
    let mut d = engine(stepping(&[StepUpAct::DeclareHostile]), three(), &clock, &MemorySink::new());
    let op1 = d.open_operator_session(OperatorId::new("op-1")).unwrap();
    d.declare_track(op1.authority(), None, "T-0001", Affiliation::Hostile, None, None, &w).unwrap();
    // the session's own events are the engine's to log
    assert!(d.record_operator_action(op1.authority(), "session.closed", "forged").is_err());
}
