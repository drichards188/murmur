//! Decision rules 16 (`LOST_CONTACT_SINCE`, murmur-533): a contact the tracker drops is not
//! forgotten. The engine logs each drop (`track.dropped`) and holds the contact as lost; every
//! engagement's keep-out counts it anywhere it could have reached since its last fix; an
//! operator with engagement authority may accept it for one engagement (`engage.accepted`
//! naming the contact); a new track with the same AIS identity, unambiguously, retires it
//! (`track.reacquired`); replay rebuilds all of it from the log, and flags a track that left
//! the picture without a drop. The scenarios (86 to 91) play the same end to end.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::engage::Fallback;
use murmur_decide::replay::{RulesAccepted, replay_str, replay_str_with};
use murmur_decide::world::NavSource;
use murmur_decide::*;

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const BLADE_AT: LonLat = LonLat::new(-122.4700, 37.8160);

fn toward(p: LonLat, bearing_deg: f64, m: f64) -> LonLat {
    let b = bearing_deg.to_radians();
    engage::offset(p, m * b.sin(), m * b.cos())
}

fn contact(t: Time, at: LonLat, aff: Affiliation) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, t),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(aff, t),
        affiliation_confidence: 0.8,
        identity: Some("ferry".into()),
        identity_confidence: 0.9,
        sources: vec!["3:radar".into()],
        source_t: [("3:radar".to_string(), t)].into_iter().collect(),
        domain: Domain::Surface,
        last_seen_t: t,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: Some([0.0, 0.0]),
        first_seen_t: Some(Time(t.0 - 600_000)),
        velocity_sigma_mps: None,
    }
}

fn ais(mmsi: &str) -> Option<AisIdentity> {
    Some(AisIdentity {
        mmsi: mmsi.into(),
        name: format!("MV {mmsi}"),
        ship_type: "passenger".into(),
    })
}

/// The target declared by op-1, the switchblade fresh at `t`, and `others`.
fn world(t: Time, others: &[(&str, TrackFacts)]) -> WorldSnapshot {
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
    let mut target = contact(t, TARGET_AT, Affiliation::Hostile);
    target.identity = Some("small_fast_boat".into());
    target.declared_by = Some("op-1".into());
    w.tracks.insert("T-0001".into(), target);
    for (id, f) in others {
        w.tracks.insert((*id).to_string(), f.clone());
    }
    w
}

/// Re-number and re-hash a log's lines as a forger would (every other byte kept).
fn rechain(text: &str) -> String {
    let mut prev = murmur_decide::hash::GENESIS.to_string();
    let mut out = String::new();
    for l in text.lines() {
        let mut ev: audit::Event = serde_json::from_str(l).unwrap();
        ev.prev = prev.clone();
        let line = serde_json::to_string(&ev).unwrap();
        prev = murmur_decide::hash::sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    out
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

fn engine_on(clock: &ManualClock, version: Option<u32>) -> (Engine, MemorySink) {
    let log = MemorySink::new();
    let mut p = policy();
    if let Some(v) = version {
        p.rules.rules_version = v;
    }
    let mut e = Engine::with_areas(p, Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    declare_shown(&mut e, &world(clock.now(), &[]));
    (e, log)
}

fn keep_out_tracks(i: &Intent) -> CheckResult {
    i.checks().iter().find(|c| c.rule == RuleId::KeepOutTracks).unwrap().clone()
}

/// A ferry 400 m north of the target, seen last at `seen`, dropped at `drop` with the facts the
/// tracker had then.
fn drop_ferry(e: &mut Engine, clock: &ManualClock, seen: Time, drop: Time, mmsi: Option<&str>) -> TrackFacts {
    let mut f = contact(seen, toward(TARGET_AT, 0.0, 400.0), Affiliation::Neutral);
    f.pos_sigma_m = 190.0;
    clock.set(drop);
    assert_eq!(e.record_track_drops(vec![("T-0002".into(), f.clone(), mmsi.and_then(ais))]).unwrap(), 1);
    f
}

#[test]
fn a_dropped_contact_blocks_until_accepted_and_replay_rebuilds_it() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let op3 = e.open_operator_session_with(OperatorId::new("op-3"), Grants { engage: true }).unwrap();
    let f = drop_ferry(&mut e, &clock, T0, T0.plus_ms(180_000), None);
    assert_eq!(e.lost_contacts().len(), 1);
    let now = T0.plus_ms(185_000);
    clock.set(now);
    let w = world(now, &[]);
    // without an acceptance: rejected, the detail saying since when and what would clear it
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Rejected);
    let k = keep_out_tracks(i);
    assert!(k.detail.starts_with("lost contact T-0002 (neutral, ferry)"), "{}", k.detail);
    assert!(k.detail.contains("out of view since t=1781000000000 (185 s)") && k.detail.contains("may accept it"), "{}", k.detail);
    // an acceptance naming another fix is refused, nothing logged
    let auth = op1.engagement_authority().unwrap();
    let wrong = e.submit_accepting_lost(auth, None, nomination(), &[], &[("T-0002".into(), now)], &w, &terrain());
    assert!(matches!(wrong, Err(EngineError::NotAcceptedContact { .. })), "{wrong:?}");
    // accepting that very loss: the keep-out leaves it out and says so
    let id = e
        .submit_accepting_lost(auth, None, nomination(), &[], &[("T-0002".into(), f.position.t)], &w, &terrain())
        .unwrap()
        .id();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(keep_out_tracks(i).detail.contains("lost contacts accepted for it: T-0002"), "{}", keep_out_tracks(i).detail);
    // every signer's picture shows it
    let tok = e.issue_picture(id, op2.engagement_authority().unwrap(), &w).unwrap();
    assert_eq!(tok.lost_accepted.keys().collect::<Vec<_>>(), ["T-0002"]);
    let tok = tok.id;
    e.approve_engagement(id, op2.engagement_authority().unwrap(), None, tok, &w, &terrain()).unwrap();
    let tok = e.issue_picture(id, op3.engagement_authority().unwrap(), &w).unwrap().id;
    assert_eq!(e.approve_engagement(id, op3.engagement_authority().unwrap(), None, tok, &w, &terrain()).unwrap().state(), State::Approved);
    let text = log.text();
    assert_eq!(text.matches("\"type\":\"track.dropped\"").count(), 1);
    assert!(text.contains("\"contact\":\"T-0002\""));
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    assert_eq!(r.lost_contacts, 1);
}

#[test]
fn before_rules_16_a_drop_is_not_logged_and_the_contact_is_gone() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, Some(AIRCRAFT_REACH_SINCE));
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    let f = contact(T0, toward(TARGET_AT, 0.0, 400.0), Affiliation::Neutral);
    clock.set(T0.plus_ms(180_000));
    assert_eq!(e.record_track_drops(vec![("T-0002".into(), f, None)]).unwrap(), 0);
    let now = T0.plus_ms(185_000);
    clock.set(now);
    let id = e.submit(op1.authority(), nomination(), &world(now, &[]), &terrain()).unwrap().id();
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    let text = log.text();
    assert!(!text.contains("track.dropped") && !text.contains("lost_contacts"));
    let r = replay_str_with(&text, &replay::ReplayOptions { rules: RulesAccepted::Exactly(AIRCRAFT_REACH_SINCE), ..Default::default() }).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// A live track carrying a lost contact's MMSI is a claim, raised once for review (never a
/// retirement); an operator confirms it only while nothing contradicts it.
#[test]
fn a_claim_on_a_lost_contact_is_confirmed_by_an_operator_only_when_nothing_contradicts_it() {
    let seen = T0;
    let back = T0.plus_ms(240_000);
    let near = toward(TARGET_AT, 0.0, 300.0);
    // every other track carries this MMSI
    let try_with = |mmsi: Option<&str>, others: Vec<(&str, TrackFacts)>| {
        let clock = ManualClock::new(T0);
        let (mut e, log) = engine_on(&clock, None);
        let op = e.open_operator_session(OperatorId::new("op-2")).unwrap();
        drop_ferry(&mut e, &clock, seen, T0.plus_ms(180_000), mmsi);
        clock.set(back);
        let mut w = world(back, &others.iter().map(|(i, f)| (*i, f.clone())).collect::<Vec<_>>());
        for (i, _) in &others {
            w.track_ais.insert((*i).to_string(), ais("366999501").unwrap());
        }
        let step = e.step_lost(&w).unwrap();
        assert!(step.reinstated.is_empty());
        // raised once
        assert!(e.step_lost(&w).unwrap().claimed.is_empty());
        // nothing retired by the claim itself
        assert_eq!(e.lost_contacts().len(), 1);
        let confirmed = e.confirm_lost_claim(op.authority(), "T-0002", "T-0009", &w);
        let r = replay_str(&log.text()).unwrap();
        assert!(r.ok(), "{:?}", r.mismatches);
        (step.claimed, confirmed.is_ok(), e.lost_contacts().len(), log.text())
    };
    let again = |at: LonLat| {
        let mut f = contact(back, at, Affiliation::Neutral);
        f.first_seen_t = Some(back);
        f.mean_velocity_mps = None;
        f
    };
    // the one track carrying its MMSI, undeclared, consistent, first seen since, where it could be
    let (claims, ok, left, text) = try_with(Some("366999501"), vec![("T-0009", again(near))]);
    assert_eq!(claims.len(), 1);
    assert!(claims[0].confirmable(), "{:?}", claims[0].blockers);
    assert!(claims[0].evidence.contains("name MV 366999501 (the same)"), "{}", claims[0].evidence);
    assert!(claims[0].evidence.contains("the kinds agree"), "{}", claims[0].evidence);
    assert!(ok && left == 0);
    assert_eq!(text.matches("\"type\":\"lost.claimed\"").count(), 1);
    assert!(text.contains("\"type\":\"track.reacquired\",\"track\":\"T-0002\",\"by_track\":\"T-0009\",\"by\":\"op-2\""));
    // a contact without AIS is never claimed
    let (claims, ok, left, _) = try_with(None, vec![("T-0009", again(near))]);
    assert!(claims.is_empty() && !ok && left == 1);
    // two tracks carrying it: ambiguous, both claims shown, neither confirmed
    let (claims, ok, left, _) = try_with(
        Some("366999501"),
        vec![("T-0009", again(near)), ("T-0010", again(toward(TARGET_AT, 90.0, 200.0)))],
    );
    assert_eq!(claims.len(), 2);
    assert!(claims.iter().all(|c| c.blockers.iter().any(|b| b.contains("ambiguous"))));
    assert!(!ok && left == 1);
    // what our sensors see does not fit its AIS (the tracker's estimate is suspect)
    let mut odd = again(near);
    odd.affiliation = Fact::new(Affiliation::Suspect, back);
    let (claims, ok, left, _) = try_with(Some("366999501"), vec![("T-0009", odd)]);
    assert!(claims[0].blockers.iter().any(|b| b.contains("suspect")) && !ok && left == 1);
    // far beyond where it could have got to
    let (claims, ok, left, _) = try_with(Some("366999501"), vec![("T-0009", again(toward(TARGET_AT, 0.0, 30_000.0)))]);
    assert!(claims[0].blockers.iter().any(|b| b.contains("beyond")) && !ok && left == 1);
    // a track first seen before its last fix is not a new sighting of it
    let mut old = again(near);
    old.first_seen_t = Some(Time(seen.0 - 1_000));
    let (claims, ok, left, _) = try_with(Some("366999501"), vec![("T-0009", old)]);
    assert!(claims[0].blockers.iter().any(|b| b.contains("first seen before")) && !ok && left == 1);
}

/// A confirmation lasts while its claimant stands: once it turns suspect, is declared or is
/// dropped, every decision counts the lost contact again on its own picture (before the loop
/// has logged anything), and the loop logs it reinstated for good. Replay checks both.
#[test]
fn a_confirmation_is_void_once_its_claimant_turns_suspect_is_declared_or_is_dropped() {
    let back = T0.plus_ms(240_000);
    let near = toward(TARGET_AT, 0.0, 1_900.0);
    for how in ["suspect", "declared", "dropped"] {
        let clock = ManualClock::new(T0);
        let (mut e, log) = engine_on(&clock, None);
        let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
        drop_ferry(&mut e, &clock, T0, T0.plus_ms(180_000), Some("366999501"));
        clock.set(back);
        let mut claimant = contact(back, near, Affiliation::Neutral);
        claimant.first_seen_t = Some(T0.plus_ms(100_000));
        let mut w = world(back, &[("T-0009", claimant.clone())]);
        w.track_ais.insert("T-0009".into(), ais("366999501").unwrap());
        e.step_lost(&w).unwrap();
        e.confirm_lost_claim(op1.authority(), "T-0002", "T-0009", &w).unwrap();
        assert!(e.lost_contacts().is_empty());
        // the claimant then turns: the next decision reads the lost contact on its own picture
        let now = back.plus_ms(5_000);
        clock.set(now);
        let mut w2 = world(now, &[("T-0009", claimant.clone())]);
        w2.track_ais.insert("T-0009".into(), ais("366999501").unwrap());
        match how {
            "suspect" => w2.tracks.get_mut("T-0009").unwrap().affiliation = Fact::new(Affiliation::Suspect, now),
            "declared" => {
                e.declare_track(op1.authority(), None, "T-0009", Affiliation::Hostile, None, None, &w2).unwrap();
            }
            _ => {
                let f = w2.tracks.remove("T-0009").unwrap();
                let a = w2.track_ais.remove("T-0009");
                e.record_track_drops(vec![("T-0009".into(), f, a)]).unwrap();
            }
        }
        let id = e.submit(op1.authority(), nomination(), &w2, &terrain()).unwrap().id();
        let i = e.intent(id).unwrap();
        assert_eq!(i.state(), State::Rejected, "{how}");
        assert!(keep_out_tracks(i).detail.contains("T-0002"), "{how}: {}", keep_out_tracks(i).detail);
        // the loop logs it reinstated (a drop at once)
        let step = e.step_lost(&w2).unwrap();
        let text = log.text();
        assert_eq!(text.matches("\"type\":\"lost.reinstated\"").count(), 1, "{how}");
        assert_eq!(step.reinstated.len(), usize::from(how != "dropped"), "{how}");
        assert_eq!(e.lost_contacts().len(), if how == "dropped" { 2 } else { 1 }, "{how}");
        // for good: the claimant standing again does not retire it
        e.step_lost(&w).unwrap();
        assert!(e.lost_contacts().contains_key("T-0002"));
        let r = replay_str(&text).unwrap();
        assert!(r.ok(), "{how}: {:?}", r.mismatches);
        assert_eq!((r.lost_claims, r.lost_confirmed, r.lost_reinstated), (1, 1, 1), "{how}");
    }
}

/// The review's replay gap: a confirmation's picture is a picture like a decision's. A
/// claimant seen only there (no decision read it) that then leaves the picture without a drop
/// is caught, so a confirmation is judged on a picture replay holds to the rest of the log.
#[test]
fn a_track_seen_only_in_a_confirmations_picture_cannot_vanish_without_a_drop() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    drop_ferry(&mut e, &clock, T0, T0.plus_ms(180_000), Some("366999501"));
    let back = T0.plus_ms(240_000);
    clock.set(back);
    let mut claimant = contact(back, toward(TARGET_AT, 0.0, 1_900.0), Affiliation::Neutral);
    claimant.first_seen_t = Some(T0.plus_ms(100_000));
    let mut w = world(back, &[("T-0009", claimant)]);
    w.track_ais.insert("T-0009".into(), ais("366999501").unwrap());
    e.confirm_lost_claim(op1.authority(), "T-0002", "T-0009", &w).unwrap();
    // the claimant gone from the next decision's picture, no track.dropped
    clock.set(back.plus_ms(1_000));
    e.submit(op1.authority(), nomination(), &world(back.plus_ms(1_000), &[]), &terrain()).unwrap();
    let r = replay_str(&log.text()).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("left the picture without a drop")), "{:?}", r.mismatches);
}

/// A track id is dropped once: the engine refuses a second drop of one it holds.
#[test]
fn a_track_is_dropped_once() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    let f = drop_ferry(&mut e, &clock, T0, T0.plus_ms(180_000), None);
    let mut moved = f.clone();
    moved.position = Fact::new(toward(TARGET_AT, 0.0, 6_000.0), T0.plus_ms(60_000));
    assert!(e.record_track_drops(vec![("T-0002".into(), moved, None)]).is_err());
    assert_eq!(log.text().matches("\"type\":\"track.dropped\"").count(), 1);
    assert_eq!(e.lost_contacts()["T-0002"].facts.position, f.position);
}

#[test]
fn a_track_that_leaves_the_picture_without_a_drop_is_flagged() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let ferry = contact(T0, toward(TARGET_AT, 0.0, 3_000.0), Affiliation::Neutral);
    e.submit(op1.authority(), nomination(), &world(T0, &[("T-0002", ferry)]), &terrain()).unwrap();
    clock.set(T0.plus_ms(5_000));
    // the ferry gone from the picture, no track.dropped
    e.submit(op1.authority(), nomination(), &world(T0.plus_ms(5_000), &[]), &terrain()).unwrap();
    let r = replay_str(&log.text()).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("left the picture without a drop")), "{:?}", r.mismatches);
}

#[test]
fn a_rules_16_log_claiming_rules_14_is_caught_by_its_markers() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    drop_ferry(&mut e, &clock, T0, T0.plus_ms(180_000), Some("366999501"));
    let text = log.text().replacen(
        &format!("\"rules_version\":{LOST_CONTACT_SINCE},"),
        &format!("\"rules_version\":{AIRCRAFT_REACH_SINCE},"),
        1,
    );
    let out = rechain(&text);
    let r = replay_str_with(&out, &replay::ReplayOptions { rules: RulesAccepted::Exactly(AIRCRAFT_REACH_SINCE), ..Default::default() }).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("a track the tracker dropped")), "{:?}", r.mismatches);
}

#[test]
fn only_engagement_authority_accepts_a_lost_contact_and_any_operator_revokes_it() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    let plain = e.open_operator_session(OperatorId::new("op-9")).unwrap();
    // the ferry 1.5 km off, out of view: the nomination waits while its region is small
    let mut f = contact(T0, toward(TARGET_AT, 0.0, 1_500.0), Affiliation::Neutral);
    f.pos_sigma_m = 190.0;
    clock.set(T0.plus_ms(2_000));
    let id = e.submit(op1.authority(), nomination(), &world(T0.plus_ms(2_000), &[("T-0002", f.clone())]), &terrain()).unwrap().id();
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    clock.set(T0.plus_ms(180_000));
    e.record_track_drops(vec![("T-0002".into(), f.clone(), None)]).unwrap();
    let now = T0.plus_ms(185_000);
    clock.set(now);
    let w = world(now, &[]);
    // a plain operator has no EngagementAuthority to give: the API cannot take one from them
    assert!(plain.engagement_authority().is_none());
    // op-1 accepts it while it waits; then revokes it (any operator may)
    e.accept_lost_contact(id, op1.engagement_authority().unwrap(), None, "T-0002", f.position.t, &w).unwrap();
    assert_eq!(e.intent(id).unwrap().engagement().unwrap().accepted_lost.len(), 1);
    e.revoke_lost_contact(id, plain.authority(), "T-0002").unwrap();
    assert!(e.intent(id).unwrap().engagement().unwrap().accepted_lost.is_empty());
    let again = e.revoke_lost_contact(id, plain.authority(), "T-0002");
    assert!(matches!(again, Err(EngineError::NotAcceptedContact { .. })));
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    let text = log.text();
    assert!(text.contains("\"type\":\"engage.acceptance_revoked\",\"intent\":1,\"contact\":\"T-0002\""), "{text}");
    // the tracker's facts only: an identification in a drop is the ledger's to fill, and
    // replay refuses one in the log
    let forged = text.lines().map(|l| {
        if l.contains("\"type\":\"track.dropped\"") {
            let mut ev: audit::Event = serde_json::from_str(l).unwrap();
            if let audit::EventBody::TrackDropped { facts, .. } = &mut ev.body {
                facts.declared_by = Some("op-1".into());
            }
            serde_json::to_string(&ev).unwrap()
        } else {
            l.to_string()
        }
    }).collect::<Vec<_>>().join("\n");
    let r = replay_str(&rechain(&forged)).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("carry an identification")), "{:?}", r.mismatches);
}

/// Rules 16, out of view: a lost contact counts anywhere it could have got to at its domain's top
/// speed since its last fix, not only along its course (murmur-533's measurement: a prediction,
/// not a bound) nor at its own speed bound (the review: a contact that speeds up after its last
/// fix is outside that too). A ferry 600 m north-west of the target, last seen steaming away at
/// 5 m/s, dropped 180 s later: by the effect, about 280 s after its last fix, its course puts it
/// 1.4 km further on, clear of the keep-out, and its own bound (7 m/s) reaches the target too,
/// but the check names 25 m/s, the surface top speed. A track in view with the same facts keeps
/// its course regions alone (the tracker would see it turn). Scenario 91 plays it.
#[test]
fn a_lost_contact_counts_anywhere_at_its_domains_top_speed() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine_on(&clock, None);
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    let mut f = contact(T0, toward(TARGET_AT, 315.0, 600.0), Affiliation::Neutral);
    let away = [-5.0 * std::f64::consts::FRAC_1_SQRT_2, 5.0 * std::f64::consts::FRAC_1_SQRT_2];
    f.velocity_mps = Some(away);
    f.mean_velocity_mps = Some(away);
    clock.set(T0.plus_ms(181_000));
    assert_eq!(e.record_track_drops(vec![("T-0002".into(), f.clone(), None)]).unwrap(), 1);
    let now = T0.plus_ms(185_000);
    clock.set(now);
    let w = world(now, &[]);
    let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Rejected);
    let k = keep_out_tracks(i);
    assert!(k.detail.starts_with("lost contact T-0002 (neutral, ferry)"), "{}", k.detail);
    assert!(k.detail.contains("anywhere it reaches at 25.0 m/s"), "{}", k.detail);
    // the course regions alone clear it; the out-of-view region does not
    let pv = engage::preview(i.spec(), &w, &policy().rules).unwrap();
    let ec = policy().rules.engage_rules();
    let course = engage::track_regions(&f, pv.effect_t, &ec)
        .iter()
        .map(|r| murmur_decide::geo::haversine_m(r.center, pv.effect_point) - r.radius_m)
        .fold(f64::INFINITY, f64::min);
    assert!(course > pv.keep_out_m + 200.0, "course regions: edge {course:.0} m");
    let lost = engage::lost_track_standing(&f, pv.effect_point, pv.effect_t, pv.keep_out_m, now, &ec, None, false).unwrap();
    assert!(lost.blocks(pv.keep_out_m));
    assert_eq!(lost.regions[lost.nearest].kind, engage::ReachKind::OutOfView);
    assert_eq!(engage::out_of_view_speed(&f, &ec), (25.0, 0.0));
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// Rules 16, out of view before the drop (the review: the same ferry unseen 170 s let a
/// nomination through that unseen 182 s it blocked): a live track with no detection for
/// [`engage::OUT_OF_VIEW_S`] counts the same disc from its last fix; one seen within that is
/// judged by its course. Under rules 14 the stale track counted by its course alone.
#[test]
fn a_track_out_of_view_counts_at_top_speed_before_it_is_dropped() {
    let away = [-5.0 * std::f64::consts::FRAC_1_SQRT_2, 5.0 * std::f64::consts::FRAC_1_SQRT_2];
    let ferry = |seen: Time| {
        let mut f = contact(seen, toward(TARGET_AT, 315.0, 600.0), Affiliation::Neutral);
        f.velocity_mps = Some(away);
        f.mean_velocity_mps = Some(away);
        f
    };
    let now = T0.plus_ms(170_000);
    for (version, unseen_ms, expect) in [
        (None, 170_000, State::Rejected),
        (None, 9_000, State::PendingApproval),
        (Some(AIRCRAFT_REACH_SINCE), 170_000, State::PendingApproval),
    ] {
        let clock = ManualClock::new(now);
        let (mut e, log) = engine_on(&clock, version);
        let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
        let seen = Time(now.0 - unseen_ms);
        let w = world(now, &[("T-0002", ferry(seen))]);
        let id = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap().id();
        let i = e.intent(id).unwrap();
        assert_eq!(i.state(), expect, "rules {version:?}, unseen {unseen_ms} ms: {}", keep_out_tracks(i).detail);
        if expect == State::Rejected {
            let d = keep_out_tracks(i).detail;
            assert!(d.starts_with("track T-0002 (neutral, ferry)") && d.contains("out of view"), "{d}");
            assert!(d.contains("no detection for 170 s"), "{d}");
        }
        let opts = replay::ReplayOptions {
            rules: version.map_or(RulesAccepted::default(), RulesAccepted::Exactly),
            ..Default::default()
        };
        let r = replay_str_with(&log.text(), &opts).unwrap();
        assert!(r.ok(), "{:?}", r.mismatches);
    }
}

/// The out-of-view region stands aside where a region at the last fix already reaches as far
/// (no velocity: the top speed; young: the top speed or its own bound), and runs at the contact's
/// own speed when the tracker saw it go faster than the top speed.
#[test]
fn the_out_of_view_region_is_not_doubled() {
    let ec = policy().rules.engage_rules();
    let mut f = contact(T0, TARGET_AT, Affiliation::Neutral);
    let at = T0.plus_ms(300_000);
    let kinds = |f: &TrackFacts| engage::lost_reach(f, at, &ec).iter().map(|r| r.kind).collect::<Vec<_>>();
    assert!(kinds(&f).contains(&engage::ReachKind::OutOfView));
    f.manoeuvre = Some(Manoeuvre { t: T0, change_mps: 4.0, speed_mps: 31.0 });
    assert_eq!(engage::out_of_view_speed(&f, &ec).0, 31.0, "the fastest it went, above the top speed");
    f.manoeuvre = None;
    f.mean_velocity_mps = None; // young
    assert!(!kinds(&f).contains(&engage::ReachKind::OutOfView), "{:?}", kinds(&f));
    f.velocity_mps = None;
    assert_eq!(kinds(&f), [engage::ReachKind::NoVelocity]);
}
