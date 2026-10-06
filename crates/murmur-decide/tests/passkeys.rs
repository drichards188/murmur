//! Rules 10 (murmur-437): passkeys. A credentials run logs its relying party and its
//! operators' passkeys as public keys; a login or a step-up may be a passkey's, a WebAuthn
//! assertion with user verification whose challenge is derived from the run, the session and
//! the act (an engagement signature: the intent and the picture token it quotes); the engine
//! verifies it against the logged key before logging it, and the act quoting it must be the act
//! it was made for. Replay verifies every logged assertion again, with its own code, and
//! catches a factor relabelled, a credential swapped, an assertion used twice or moved to
//! another act, and a study whose participant stepped up by password. The authenticator here is
//! the crate's software one (`passkey::soft`).
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::audit::{EventBody, parse_log};
use murmur_decide::passkey::soft::SoftAuthenticator;
use murmur_decide::passkey::{self, PasskeyUse, StepUpFor};
use murmur_decide::policy::StepUpFactor;
use murmur_decide::replay::{self, ReplayReport};
use murmur_decide::world::NavSource;
use murmur_decide::*;

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const BLADE_AT: LonLat = LonLat::new(-122.4700, 37.8160);
const RP: &str = "localhost";
const ORIGIN: &str = "http://localhost:5173";

/// A switchblade west of the bridge and a declared-hostile boat holding in the zone, every
/// fact fresh at `t` (as `tests/auth.rs`).
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

fn key(op: &str, a: &SoftAuthenticator) -> PasskeyKey {
    PasskeyKey {
        operator: OperatorId::new(op),
        id: a.credential_id(),
        public_key: a.public_key(),
        counter: 0,
    }
}

/// op-1 (no engagement authority), op-2 and op-3 (engagement authority), a credentials run
/// whose relying party is `localhost` and whose operators' passkeys are `keys`.
fn roster(keys: Vec<PasskeyKey>) -> Roster {
    Roster::new(
        ["op-1", "op-2", "op-3"].map(OperatorId::new).to_vec(),
        ["op-2", "op-3"].map(OperatorId::new).to_vec(),
    )
    .unwrap()
    .with_auth(AuthMode::Credentials)
    .with_passkeys(PasskeyRoster {
        rp_id: RP.into(),
        origins: vec![ORIGIN.into()],
        keys,
    })
}

fn passkey_policy() -> Policy {
    let mut p = policy();
    p.approval.step_up_factor = Some(StepUpFactor::Passkey);
    p
}

fn engine(policy: Policy, roster: Roster, clock: &ManualClock, log: &MemorySink) -> Result<Engine, EngineError> {
    Engine::with_areas(policy, Some(areas()), roster, Box::new(clock.clone()), Box::new(log.clone()))
}

fn login(e: &mut Engine, op: &str, engage: bool) -> OperatorSession {
    e.open_authenticated_session(OperatorId::new(op), Grants { engage }, AuthMethod::Password)
        .unwrap()
}

/// What the browser and the bridge do for a passkey use: the engine issues the challenge for
/// `purpose`, the authenticator signs it.
fn assertion(e: &mut Engine, a: &mut SoftAuthenticator, op: &str, s: Option<&OperatorSession>, purpose: StepUpFor, nonce: &str) -> PasskeyUse {
    let issued = e.passkey_challenge(&OperatorId::new(op), s, purpose, nonce.into()).unwrap();
    let assertion = a.assert(RP, ORIGIN, &issued.challenge);
    PasskeyUse {
        credential: a.credential_id(),
        counter: a.counter,
        purpose: issued.purpose,
        nonce: issued.nonce,
        issued_t: issued.issued_t,
        assertion,
    }
}

fn report(text: &str) -> ReplayReport {
    replay::replay_str(text).unwrap()
}

fn whats(text: &str) -> Vec<String> {
    report(text).mismatches.into_iter().map(|m| m.what).collect()
}

/// A credentials run whose policy asks for passkeys: op-1 nominates, op-2 and op-3 each sign
/// with their passkey. Returns the log and what was refused on the way.
fn signed_with_passkeys() -> (String, Vec<String>) {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let mut k3 = SoftAuthenticator::new("op-3 phone");
    let mut e = engine(passkey_policy(), roster(vec![key("op-2", &k2), key("op-3", &k3)]), &clock, &log).unwrap();
    let op1 = login(&mut e, "op-1", false);
    let op2 = login(&mut e, "op-2", true);
    let op3 = login(&mut e, "op-3", true);
    let w = engage_world(T0);
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    let mut refused = Vec::new();
    clock.advance_ms(2000);
    let w = engage_world(clock.now());
    let a2 = op2.engagement_authority().unwrap();
    let t = e.issue_picture(id, a2, &w).unwrap().id;
    // the password is not enough where the policy asks for a passkey
    let s = e.step_up(&op2).unwrap();
    let err = e.approve_engagement_stepped(id, a2, s, None, t, &w, &terrain()).unwrap_err();
    assert!(err.to_string().contains("asks for your passkey"), "{err}");
    refused.push(err.to_string());
    // a passkey assertion made for another intent does not sign this one
    let other = StepUpFor::Sign { intent: IntentId(id.0 + 7), token: Some(t), picture_sha256: None };
    let u = assertion(&mut e, &mut k2, "op-2", Some(&op2), other, "n-other");
    let s = e.step_up_passkey(&op2, u).unwrap();
    let err = e.approve_engagement_stepped(id, a2, s, None, t, &w, &terrain()).unwrap_err();
    assert!(err.to_string().contains("was made for"), "{err}");
    refused.push(err.to_string());
    // one made for this signature: signature 1 of 2
    let purpose = e.sign_purpose(id, Some(t)).unwrap();
    let u = assertion(&mut e, &mut k2, "op-2", Some(&op2), purpose, "n-op2");
    let s = e.step_up_passkey(&op2, u).unwrap();
    assert_eq!(s.factor(), AuthMethod::Passkey);
    let state = e.approve_engagement_stepped(id, a2, s, None, t, &w, &terrain()).unwrap().state();
    assert_eq!(state, State::PendingApproval);
    // op-3 with op-2's authenticator: another operator's passkey
    let a3 = op3.engagement_authority().unwrap();
    let t3 = e.issue_picture(id, a3, &w).unwrap().id;
    let purpose = e.sign_purpose(id, Some(t3)).unwrap();
    let u = assertion(&mut e, &mut k2, "op-3", Some(&op3), purpose.clone(), "n-swap");
    let err = e.step_up_passkey(&op3, u).unwrap_err();
    assert!(err.to_string().contains("another operator's passkey"), "{err}");
    refused.push(err.to_string());
    // op-3's own: approved
    let u = assertion(&mut e, &mut k3, "op-3", Some(&op3), purpose, "n-op3");
    let s = e.step_up_passkey(&op3, u).unwrap();
    let i = e.approve_engagement_stepped(id, a3, s, None, t3, &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::Approved);
    e.close_operator_session(op1, "logout").unwrap();
    e.close_operator_session(op2, "logout").unwrap();
    e.close_operator_session(op3, "logout").unwrap();
    (log.text(), refused)
}

#[test]
fn two_people_sign_an_engagement_with_their_passkeys() {
    let (text, refused) = signed_with_passkeys();
    assert_eq!(refused.len(), 3, "{refused:?}");
    let r = report(&text);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.authenticated());
    assert_eq!(r.rules_version, PASSKEY_SINCE);
    assert_eq!(r.stepped_signatures, 2);
    assert_eq!((r.passkey_step_ups, r.password_step_ups), (3, 1), "every step-up is logged, used or not");
    // the run names its relying party and keys; each use its assertion, never a secret
    let first = text.lines().next().unwrap();
    assert!(first.contains("\"passkeys\":{\"rp_id\":\"localhost\",\"origins\":[\"http://localhost:5173\"]"), "{first}");
    assert!(first.contains("\"step_up_factor\":\"passkey\""), "{first}");
    assert!(text.contains("\"auth\":\"passkey\",\"passkey\":{\"credential\":"), "{text}");
    assert!(text.contains("\"for\":{\"act\":\"sign\""), "{text}");
}

#[test]
fn a_counter_that_does_not_count_up_is_refused_as_a_clone() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let mut e = engine(policy(), roster(vec![key("op-2", &k2)]), &clock, &log).unwrap();
    let op2 = login(&mut e, "op-2", true);
    let decl = StepUpFor::DeclareHostile { track: "T-0001".into() };
    let u = assertion(&mut e, &mut k2, "op-2", Some(&op2), decl.clone(), "n1");
    assert_eq!(u.counter, 1);
    e.step_up_passkey(&op2, u).unwrap();
    // a copy of the authenticator, behind the original: its count repeats
    let mut clone = SoftAuthenticator::new("op-2 laptop");
    let u = assertion(&mut e, &mut clone, "op-2", Some(&op2), decl.clone(), "n2");
    assert_eq!(u.counter, 1);
    let err = e.step_up_passkey(&op2, u).unwrap_err();
    assert!(err.to_string().contains("cloned"), "{err}");
    // an authenticator that never counts (a synced passkey) reports 0 every time: accepted
    let mut synced = SoftAuthenticator::new("op-3 synced");
    synced.counts = false;
    let clock = ManualClock::new(T0);
    let mut e = engine(policy(), roster(vec![key("op-3", &synced)]), &clock, &MemorySink::new()).unwrap();
    let op3 = login(&mut e, "op-3", true);
    for n in ["a", "b"] {
        let u = assertion(&mut e, &mut synced, "op-3", Some(&op3), decl.clone(), n);
        assert_eq!(u.counter, 0);
        e.step_up_passkey(&op3, u).unwrap();
    }
}

#[test]
fn an_assertion_is_answered_once_fresh_and_with_user_verification() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let mut e = engine(policy(), roster(vec![key("op-2", &k2)]), &clock, &log).unwrap();
    let op2 = login(&mut e, "op-2", true);
    let decl = StepUpFor::DeclareHostile { track: "T-0001".into() };
    let u = assertion(&mut e, &mut k2, "op-2", Some(&op2), decl.clone(), "once");
    e.step_up_passkey(&op2, u.clone()).unwrap();
    let err = e.step_up_passkey(&op2, u).unwrap_err();
    assert!(err.to_string().contains("answered already"), "{err}");
    // a challenge older than its lifetime
    let u = assertion(&mut e, &mut k2, "op-2", Some(&op2), decl.clone(), "late");
    clock.advance_ms(passkey::CHALLENGE_TTL_MS + 1);
    let err = e.step_up_passkey(&op2, u).unwrap_err();
    assert!(err.to_string().contains("ms old"), "{err}");
    // an authenticator that does not verify the user
    k2.uv = false;
    let u = assertion(&mut e, &mut k2, "op-2", Some(&op2), decl.clone(), "no-uv");
    let err = e.step_up_passkey(&op2, u).unwrap_err();
    assert!(err.to_string().contains("not verified"), "{err}");
    k2.uv = true;
    // another session's challenge
    let other = login(&mut e, "op-2", true);
    let u = assertion(&mut e, &mut k2, "op-2", Some(&other), decl, "other-session");
    let err = e.step_up_passkey(&op2, u).unwrap_err();
    assert!(err.to_string().contains("another challenge"), "{err}");
    let r = report(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_passkey_login_opens_a_session_and_the_policy_says_how_people_sign_in() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let mut e = engine(policy(), roster(vec![key("op-2", &k2)]), &clock, &log).unwrap();
    let u = assertion(&mut e, &mut k2, "op-2", None, StepUpFor::Login, "login");
    let s = e.open_passkey_session(OperatorId::new("op-2"), Grants { engage: true }, u).unwrap();
    assert_eq!(s.method(), Some(AuthMethod::Passkey));
    // a step-up's assertion is not a login, nor a login's a step-up
    let u = assertion(&mut e, &mut k2, "op-2", Some(&s), StepUpFor::DeclareHostile { track: "T-0001".into() }, "x");
    assert!(e.open_passkey_session(OperatorId::new("op-2"), Grants::default(), u).is_err());
    let r = report(&log.text());
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.passkey_logins, 1);
    assert!(r.people.contains(&OperatorId::new("op-2")), "a passkey login is a person's");
    // login_factor = "passkey": a password login is refused
    let mut p = policy();
    p.approval.login_factor = Some(LoginFactor::Passkey);
    let mut e = engine(p, roster(vec![key("op-2", &k2)]), &clock, &MemorySink::new()).unwrap();
    let err = e
        .open_authenticated_session(OperatorId::new("op-2"), Grants::default(), AuthMethod::Password)
        .unwrap_err();
    assert!(err.to_string().contains("login_factor"), "{err}");
    // and "password": a passkey login is
    let mut p = policy();
    p.approval.login_factor = Some(LoginFactor::Password);
    let mut e = engine(p, roster(vec![key("op-2", &k2)]), &clock, &MemorySink::new()).unwrap();
    let u = assertion(&mut e, &mut k2, "op-2", None, StepUpFor::Login, "pw-only");
    assert!(e.open_passkey_session(OperatorId::new("op-2"), Grants::default(), u).is_err());
}

#[test]
fn a_passkey_registered_mid_run_is_logged_and_usable_and_one_removed_is_not() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = engine(policy(), roster(Vec::new()), &clock, &log).unwrap();
    let op2 = login(&mut e, "op-2", true);
    let mut k = SoftAuthenticator::new("op-2 new key");
    // not without the password re-entered
    let s = e.step_up(&op2).unwrap();
    clock.advance_ms(STEP_UP_MAX_AGE_MS + 1);
    assert!(e.record_passkey_registered(key("op-2", &k), Some((&op2, s))).is_err());
    let s = e.step_up(&op2).unwrap();
    e.record_passkey_registered(key("op-2", &k), Some((&op2, s))).unwrap();
    // twice is refused
    let s = e.step_up(&op2).unwrap();
    assert!(e.record_passkey_registered(key("op-2", &k), Some((&op2, s))).is_err());
    let decl = StepUpFor::DeclareHostile { track: "T-0001".into() };
    let u = assertion(&mut e, &mut k, "op-2", Some(&op2), decl.clone(), "after-registration");
    e.step_up_passkey(&op2, u).unwrap();
    // an invitation (no session) for op-3
    let k3 = SoftAuthenticator::new("op-3 invited");
    e.record_passkey_registered(key("op-3", &k3), None).unwrap();
    // removed: no longer usable
    e.record_passkey_removed(&OperatorId::new("op-2"), &k.credential_id(), "murmur-auth passkey-remove").unwrap();
    let u = assertion(&mut e, &mut k, "op-2", Some(&op2), decl, "after-removal");
    let err = e.step_up_passkey(&op2, u).unwrap_err();
    assert!(err.to_string().contains("removed"), "{err}");
    let text = log.text();
    let r = report(&text);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!((r.passkeys_registered, r.passkeys_removed, r.passkey_step_ups), (2, 1, 1));
    // forged: the registration's step-up dropped
    let forged = forge(&text, |events| {
        for ev in events.iter_mut() {
            if let EventBody::PasskeyRegistered { step_up, via, .. } = &mut ev.body
                && via == "session"
            {
                *step_up = None;
            }
        }
    });
    assert!(whats(&forged).iter().any(|w| w.contains("passkey registration without a step-up")), "{:?}", whats(&forged));
}

#[test]
fn a_study_requires_its_participants_passkey_unless_overridden_and_logged() {
    let clock = ManualClock::new(T0);
    let k2 = SoftAuthenticator::new("op-2 laptop");
    let study = |over: Option<&str>| Study {
        participant: OperatorId::new("op-2"),
        password_step_up: over.map(str::to_string),
    };
    // a participant without a passkey: the run does not start
    let err = engine(passkey_policy(), roster(Vec::new()).with_study(study(None)), &clock, &MemorySink::new()).unwrap_err();
    assert!(err.to_string().contains("has no passkey"), "{err}");
    // a policy that would take a password: the run does not start either
    let err = engine(policy(), roster(vec![key("op-2", &k2)]).with_study(study(None)), &clock, &MemorySink::new()).unwrap_err();
    assert!(err.to_string().contains("requires passkey step-ups"), "{err}");
    // a dev run can't be a study
    let dev = Roster::new(vec![OperatorId::new("op-2")], vec![]).unwrap().with_auth(AuthMode::Dev).with_study(study(None));
    assert!(engine(passkey_policy(), dev, &clock, &MemorySink::new()).is_err());
    // the explicit override: password step-ups allowed, logged, and never evidence
    let log = MemorySink::new();
    let mut e = engine(policy(), roster(Vec::new()).with_study(study(Some("pilot: no authenticator yet"))), &clock, &log).unwrap();
    let op1 = login(&mut e, "op-1", false);
    let op2 = login(&mut e, "op-2", true);
    let op3 = login(&mut e, "op-3", true);
    let w = engage_world(T0);
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    for s in [&op2, &op3] {
        let a = s.engagement_authority().unwrap();
        let t = e.issue_picture(id, a, &w).unwrap().id;
        let step = e.step_up(s).unwrap();
        e.approve_engagement_stepped(id, a, step, None, t, &w, &terrain()).unwrap();
    }
    let text = log.text();
    assert!(text.lines().next().unwrap().contains("\"study\":{\"participant\":\"op-2\",\"password_step_up\":\"pilot: no authenticator yet\"}"));
    let r = report(&text);
    assert!(r.ok(), "{:?}", r.mismatches);
    let st = r.study.clone().unwrap();
    assert_eq!((st.passkey_acts, st.password_acts), (0, 1));
    assert!(r.authenticated() && !r.study_evidence(), "an overridden study is never evidence");
}

#[test]
fn a_study_with_passkeys_is_evidence_and_its_forgeries_are_caught() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut k2 = SoftAuthenticator::new("op-2 laptop");
    let mut k3 = SoftAuthenticator::new("op-3 phone");
    let study = Study { participant: OperatorId::new("op-2"), password_step_up: None };
    let mut e = engine(passkey_policy(), roster(vec![key("op-2", &k2), key("op-3", &k3)]).with_study(study), &clock, &log).unwrap();
    let op1 = login(&mut e, "op-1", false);
    let op2 = login(&mut e, "op-2", true);
    let op3 = login(&mut e, "op-3", true);
    let w = engage_world(T0);
    declare_shown(&mut e, &w);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    for (s, k, op, n) in [(&op2, &mut k2, "op-2", "a"), (&op3, &mut k3, "op-3", "b")] {
        let a = s.engagement_authority().unwrap();
        let t = e.issue_picture(id, a, &w).unwrap().id;
        let purpose = e.sign_purpose(id, Some(t)).unwrap();
        let u = assertion(&mut e, k, op, Some(s), purpose, n);
        let step = e.step_up_passkey(s, u).unwrap();
        e.approve_engagement_stepped(id, a, step, None, t, &w, &terrain()).unwrap();
    }
    let text = log.text();
    let r = report(&text);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.study.as_ref().map(|s| (s.passkey_acts, s.password_acts)), Some((1, 0)));
    assert!(r.study_evidence());

    // the factor relabelled: the participant's step-up claimed a password's (its assertion
    // dropped), where the study's policy asks for a passkey
    let forged = forge(&text, |events| {
        for ev in events.iter_mut() {
            if let EventBody::OperatorAction { operator, action, auth, passkey, .. } = &mut ev.body
                && action == "step_up"
                && operator.as_str() == "op-2"
            {
                *auth = Some(AuthMethod::Password);
                *passkey = None;
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("by password where the policy requires a passkey")), "{w:?}");
    let fr = report(&forged);
    assert!(!fr.study_evidence());

    // a password step-up relabelled as a passkey's: it carries no assertion
    let (pw_text, _) = signed_with_passkeys();
    let forged = forge(&pw_text, |events| {
        for ev in events.iter_mut() {
            if let EventBody::OperatorAction { action, auth, passkey, .. } = &mut ev.body
                && action == "step_up"
                && *auth == Some(AuthMethod::Password)
            {
                *auth = Some(AuthMethod::Passkey);
                assert!(passkey.is_none());
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("without its assertion")), "{w:?}");

    // the credential swapped: op-3's assertion claimed for op-2's credential
    let forged = forge(&text, |events| {
        let op2_cred = k2.credential_id();
        for ev in events.iter_mut() {
            if let EventBody::OperatorAction { operator, action, passkey: Some(u), .. } = &mut ev.body
                && action == "step_up"
                && operator.as_str() == "op-3"
            {
                u.credential = op2_cred.clone();
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("passkey step-up:")), "{w:?}");

    // the assertion reused: op-2's step-up logged again (another seq) and quoted by op-3's
    // signature in place of op-3's own
    let forged = forge(&text, |events| {
        let op2_use = events.iter().find_map(|ev| match &ev.body {
            EventBody::OperatorAction { operator, action, passkey: Some(u), .. } if action == "step_up" && operator.as_str() == "op-2" => {
                Some(u.clone())
            }
            _ => None,
        });
        for ev in events.iter_mut() {
            if let EventBody::OperatorAction { operator, action, passkey: Some(u), .. } = &mut ev.body
                && action == "step_up"
                && operator.as_str() == "op-3"
            {
                *u = op2_use.clone().unwrap();
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("answered twice")), "{w:?}");

    // an assertion moved to another act: the step-up's purpose rewritten to another token
    let forged = forge(&text, |events| {
        for ev in events.iter_mut() {
            if let EventBody::OperatorAction { action, passkey: Some(u), .. } = &mut ev.body
                && action == "step_up"
                && let StepUpFor::Sign { token: Some(t), .. } = &mut u.purpose
            {
                *t += 100;
            }
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("another challenge than this act's")), "{w:?}");

    // the study dropped from the roster: the run's first line changed, so every assertion's
    // challenge (bound to it) no longer verifies; only the anchors would show the rewrite
    // otherwise
    let forged = forge(&text, |events| {
        if let EventBody::RunStarted { roster: Some(ro), .. } = &mut events[0].body {
            ro.study = None;
        }
    });
    let w = whats(&forged);
    assert!(w.iter().any(|m| m.contains("another challenge than this act's")), "{w:?}");
}

#[test]
fn passkeys_belong_to_rules_10_credentials_runs() {
    let clock = ManualClock::new(T0);
    let k2 = SoftAuthenticator::new("op-2 laptop");
    // a dev run takes no passkey
    let dev = Roster::new(vec![OperatorId::new("op-2")], vec![])
        .unwrap()
        .with_auth(AuthMode::Dev)
        .with_passkeys(PasskeyRoster { rp_id: RP.into(), origins: vec![ORIGIN.into()], keys: vec![key("op-2", &k2)] });
    assert!(engine(policy(), dev, &clock, &MemorySink::new()).is_err());
    // an IP address is no relying party
    let mut r = roster(vec![]);
    r.passkeys.as_mut().unwrap().rp_id = "127.0.0.1".into();
    let err = engine(policy(), r, &clock, &MemorySink::new()).unwrap_err();
    assert!(err.to_string().contains("IP address"), "{err}");
    // a rules 9 engine logs no passkeys (and the scenario markers keep them out of rules 9
    // claims)
    let mut p = policy();
    p.rules.rules_version = RULE_ID_SINCE;
    let log = MemorySink::new();
    let mut e = engine(p, roster(vec![key("op-2", &k2)]), &clock, &log).unwrap();
    let op2 = login(&mut e, "op-2", true);
    assert!(e.passkey_challenge(&OperatorId::new("op-2"), Some(&op2), StepUpFor::Login, "n".into()).is_err());
    assert!(!log.text().contains("\"passkeys\""));
    // a rules 10 log claiming rules 9: its passkeys are caught by the markers
    let (text, _) = signed_with_passkeys();
    let claim = text.replacen(
        &format!("\"rules_version\":{RULES_VERSION}"),
        &format!("\"rules_version\":{}", PASSKEY_SINCE - 1),
        1,
    );
    let r = replay::replay_str_with(
        &forge(&claim, |_| {}),
        &replay::ReplayOptions { rules: replay::RulesAccepted::AtLeast(1), ..replay::ReplayOptions::default() },
    )
    .unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("only builds of rules")), "{:?}", r.mismatches);
}

#[test]
fn nothing_secret_is_logged() {
    let (text, _) = signed_with_passkeys();
    let events = parse_log(&text).unwrap();
    // public keys and assertions only: no private scalar can be in a log the engine wrote,
    // since it never had one; the soft authenticator's private key derives from its seed,
    // which is not in the log either
    assert!(!text.contains("op-2 laptop"));
    assert!(events.iter().any(|e| matches!(&e.body, EventBody::OperatorAction { passkey: Some(_), .. })));
}
