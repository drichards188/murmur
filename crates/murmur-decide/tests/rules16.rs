//! Decision rules 16 (`LOST_CONTACT_SINCE`, murmur-533): a contact the tracker drops is not
//! forgotten. The engine logs each drop (`track.dropped`) and holds the contact as lost; every
//! engagement's keep-out counts it anywhere it could have reached since its last fix; an
//! operator with engagement authority may accept it for one engagement (`engage.accepted`
//! naming the contact); a new track with the same AIS identity, unambiguously, retires it
//! (`track.reacquired`); replay rebuilds all of it from the log, and flags a track that left
//! the picture without a drop. The scenarios (84 to 89) play the same end to end.
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

#[test]
fn a_contact_seen_again_is_retired_only_on_an_unambiguous_ais_match() {
    let seen = T0;
    let back = T0.plus_ms(240_000);
    let near = toward(TARGET_AT, 0.0, 300.0);
    // each other track with the MMSI it carries (all of them here)
    let try_with = |mmsi: Option<&str>, others: Vec<(&str, TrackFacts)>| {
        let clock = ManualClock::new(T0);
        let (mut e, log) = engine_on(&clock, None);
        drop_ferry(&mut e, &clock, seen, T0.plus_ms(180_000), mmsi);
        clock.set(back);
        let mut w = world(back, &others.iter().map(|(i, f)| (*i, f.clone())).collect::<Vec<_>>());
        for (i, _) in &others {
            w.track_ais.insert((*i).to_string(), ais("366999501").unwrap());
        }
        let out = e.step_lost(&w).unwrap();
        let r = replay_str(&log.text()).unwrap();
        assert!(r.ok(), "{:?}", r.mismatches);
        (out, e.lost_contacts().len())
    };
    let again = |mmsi: &str, at: LonLat| {
        assert_eq!(mmsi, "366999501", "try_with gives every other track this MMSI");
        let mut f = contact(back, at, Affiliation::Neutral);
        f.first_seen_t = Some(back);
        f.mean_velocity_mps = None;
        f
    };
    // the one track carrying its MMSI, undeclared, consistent, first seen since, where it could be
    let (out, left) = try_with(Some("366999501"), vec![("T-0009", again("366999501", near))]);
    assert_eq!((out, left), (vec![("T-0002".to_string(), "T-0009".to_string())], 0));
    // a contact without AIS is never matched
    let (out, left) = try_with(None, vec![("T-0009", again("366999501", near))]);
    assert!(out.is_empty() && left == 1);
    // two tracks carrying it: ambiguous
    let (out, left) = try_with(
        Some("366999501"),
        vec![("T-0009", again("366999501", near)), ("T-0010", again("366999501", toward(TARGET_AT, 90.0, 200.0)))],
    );
    assert!(out.is_empty() && left == 1);
    // what our sensors see does not fit its AIS (the tracker's estimate is suspect)
    let mut odd = again("366999501", near);
    odd.affiliation = Fact::new(Affiliation::Suspect, back);
    let (out, left) = try_with(Some("366999501"), vec![("T-0009", odd)]);
    assert!(out.is_empty() && left == 1);
    // far beyond where it could have got to
    let (out, left) = try_with(Some("366999501"), vec![("T-0009", again("366999501", toward(TARGET_AT, 0.0, 30_000.0)))]);
    assert!(out.is_empty() && left == 1);
    // a track first seen before its last fix is not a new sighting of it
    let mut old = again("366999501", near);
    old.first_seen_t = Some(Time(seen.0 - 1_000));
    let (out, left) = try_with(Some("366999501"), vec![("T-0009", old)]);
    assert!(out.is_empty() && left == 1);
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

/// murmur-533's measurement found the regions along a lost contact's course alone a prediction,
/// not a bound: a contact last seen steady that turns after its last fix can be inside a keep-out
/// they clear. A ferry 600 m north-west of the target, last seen steaming away at 5 m/s, dropped
/// 180 s later: by the effect, about 280 s after its last fix, its course puts it 1.4 km further
/// on (clear of the keep-out), but it could have turned back and be at the target. It also counts
/// anywhere it reaches at its own plausible speed (its speed and 2-sigma of its 1 m/s velocity
/// error, 7 m/s), so the nomination is rejected on that region. A live track with the same facts
/// keeps the course regions alone (the tracker would see it turn). Scenario 89 plays it.
#[test]
fn a_lost_contact_counts_anywhere_at_its_own_plausible_speed() {
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
    assert!(k.detail.contains("its own plausible speed, 7.0 m/s"), "{}", k.detail);
    // the course regions alone clear it; the plausible-speed region does not
    let pv = engage::preview(i.spec(), &w, &policy().rules).unwrap();
    let ec = policy().rules.engage_rules();
    let course = engage::track_standing(&f, pv.effect_point, pv.effect_t, pv.keep_out_m, now, &ec, None, false).unwrap();
    assert!(course.edge > pv.keep_out_m + 200.0, "course regions: edge {:.0} m", course.edge);
    let lost = engage::lost_track_standing(&f, pv.effect_point, pv.effect_t, pv.keep_out_m, now, &ec, None, false).unwrap();
    assert!(lost.blocks(pv.keep_out_m));
    assert_eq!(lost.regions[lost.nearest].kind, engage::ReachKind::PlausibleSpeed);
    // a live track keeps its course regions alone
    let kinds: Vec<_> = engage::track_reach(&f, pv.effect_t, &ec).iter().map(|r| r.kind).collect();
    assert!(!kinds.contains(&engage::ReachKind::PlausibleSpeed), "{kinds:?}");
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// The plausible-speed region stands aside where a region at the last fix already reaches at
/// least as far: a lost contact the tracker saw manoeuvre faster than its speed bound, or one
/// without a velocity estimate (its domain's top speed).
#[test]
fn the_plausible_speed_region_is_not_doubled() {
    let ec = policy().rules.engage_rules();
    let mut f = contact(T0, TARGET_AT, Affiliation::Neutral);
    let at = T0.plus_ms(300_000);
    let kinds = |f: &TrackFacts| engage::lost_reach(f, at, &ec).iter().map(|r| r.kind).collect::<Vec<_>>();
    assert!(kinds(&f).contains(&engage::ReachKind::PlausibleSpeed));
    f.manoeuvre = Some(Manoeuvre { t: T0, change_mps: 4.0, speed_mps: 9.0 });
    assert!(!kinds(&f).contains(&engage::ReachKind::PlausibleSpeed), "{:?}", kinds(&f));
    f.manoeuvre = None;
    f.velocity_mps = None;
    assert_eq!(kinds(&f), [engage::ReachKind::NoVelocity]);
}
