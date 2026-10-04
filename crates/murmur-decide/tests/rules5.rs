//! Decision rules 5, the engagement part: the approval's fallback preloaded as the
//! platform's own lost-link action (and nothing released without it confirmed), and the
//! keep-outs' prediction model, altitude and accepted lost friendlies. Every change fails
//! closed on missing data; older rules decide as they did.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use murmur_decide::engage::{self, LostLinkPreload, PreloadPhase};
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::world::NavSource;
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const BLADE_AT: LonLat = LonLat::new(-122.4700, 37.8160);

fn track(t: Time, at: LonLat, vel: Option<[f64; 2]>, aff: Affiliation, domain: Domain) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, t),
        alt_m: None,
        velocity_mps: vel,
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(vel.map_or(1.0, |[e, n]| (0.3 * e.hypot(n)).max(1.0))),
        confidence: 0.9,
        affiliation: Fact::new(aff, t),
        affiliation_confidence: 0.8,
        identity: Some("ferry".into()),
        identity_confidence: 0.9,
        sources: vec!["ais".into()],
        domain,
        last_seen_t: t,
        declared_by: (aff == Affiliation::Hostile).then(|| "op-1".to_string()),
    }
}

fn fresh(mut v: VehicleFacts, t: Time) -> VehicleFacts {
    let at = v.position.as_ref().unwrap().value;
    v.position = Some(Fact::new(at, t));
    v.nav_ready = Some(Fact::new(true, t));
    v.nav_source = Some(Fact::new(NavSource::Gnss, t));
    v.gps_fix = Some(Fact::new(3, t));
    v.gps_glitch = Some(Fact::new(false, t));
    v.battery_pct = Some(Fact::new(95.0, t));
    v.velocity_mps = Some(Fact::new([0.0, 0.0], t));
    v.report_interval_ms = Some(1000);
    v
}

/// The engagement world: our switchblade, the declared target holding west of the bridge.
fn world(t: Time) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(t);
    w.areas = Some(areas());
    let mut blade = fresh(vehicle("switchblade", BLADE_AT), t);
    blade.autopilot = Some(Fact::new(AutopilotKind::Plane, t));
    w.vehicles.insert(Uid::new("blade-1"), blade);
    let mut target = track(t, TARGET_AT, Some([0.0, 0.0]), Affiliation::Hostile, Domain::Surface);
    target.identity = Some("small_fast_boat".into());
    w.tracks.insert("T-0001".into(), target);
    w
}

fn rules(version: u32) -> RulesConfig {
    let mut r = policy().rules;
    r.rules_version = version;
    r
}

// ---- der.2: the fallback as the platform's own lost-link action ------------------------------

#[test]
fn each_firmware_carries_the_fallbacks_it_has_a_lost_link_action_for() {
    let p = |f, ap| engage::preload_for(f, ap, 5_000);
    let names = |l: &LostLinkPreload| l.set.iter().map(|s| (s.name.clone(), s.value)).collect::<Vec<_>>();
    // ArduPlane: return only (FS_LONG_ACTN 1, RTL); nothing holds where it is on a lost link
    let plane = p(Fallback::Return, Some(AutopilotKind::Plane)).unwrap();
    assert_eq!(names(&plane), [("FS_GCS_ENABL".into(), 1.0), ("FS_LONG_ACTN".into(), 1.0)]);
    assert_eq!((plane.timer.as_str(), plane.timer_max_s), ("FS_LONG_TIMEOUT", 5.0));
    for f in [Fallback::Abort, Fallback::Loiter] {
        for ap in [AutopilotKind::Plane, AutopilotKind::Vtol] {
            let e = p(f, Some(ap)).unwrap_err();
            assert!(e.contains("only return can"), "{e}");
        }
    }
    // ArduCopter: RTL for return, Brake (else Land) for abort and loiter; FS_OPTIONS bit 1 (continue
    // the mission in AUTO) cleared
    let c = p(Fallback::Return, Some(AutopilotKind::Copter)).unwrap();
    assert_eq!(names(&c), [("FS_GCS_ENABLE".into(), 1.0)]);
    for f in [Fallback::Abort, Fallback::Loiter] {
        let c = p(f, Some(AutopilotKind::Copter)).unwrap();
        assert_eq!(names(&c), [("FS_GCS_ENABLE".into(), 7.0)]);
        assert_eq!(c.clear_bits[0].name, "FS_OPTIONS");
        assert_eq!(c.clear_bits[0].clear, 2);
        assert_eq!(c.timer, "FS_GCS_TIMEOUT");
    }
    // ArduRover (boats, UGVs): FS_ACTION 1 RTL, 2 Hold, 6 Loiter or Hold; FS_GCS_ENABLE 1, not 2
    for ap in [AutopilotKind::Boat, AutopilotKind::Ugv] {
        for (f, v) in [(Fallback::Return, 1.0), (Fallback::Abort, 2.0), (Fallback::Loiter, 6.0)] {
            let r = p(f, Some(ap)).unwrap();
            assert_eq!(names(&r), [("FS_GCS_ENABLE".into(), 1.0), ("FS_ACTION".into(), v)]);
        }
    }
    // ArduSub: depth hold for abort and loiter, no return
    assert_eq!(names(&p(Fallback::Abort, Some(AutopilotKind::Sub)).unwrap()), [("FS_GCS_ENABLE".into(), 3.0)]);
    assert!(p(Fallback::Return, Some(AutopilotKind::Sub)).is_err());
    // none at all: Blimp (Land only), a tracker, a drifter, an unknown autopilot
    for ap in [Some(AutopilotKind::Blimp), Some(AutopilotKind::Tracker), Some(AutopilotKind::Drifter), None] {
        assert!(p(Fallback::Return, ap).is_err(), "{ap:?}");
    }
    // the timer: the approval's link-gap limit in whole seconds, at least 2 s
    let t = |ms| engage::preload_for(Fallback::Return, Some(AutopilotKind::Boat), ms).unwrap().timer_max_s;
    assert_eq!((t(5_000), t(5_400), t(60_000), t(1_000)), (5.0, 6.0, 60.0, 2.0));
}

#[test]
fn a_preload_holds_only_when_its_read_back_shows_it_in_force() {
    let c = engage::preload_for(Fallback::Abort, Some(AutopilotKind::Copter), 5_000).unwrap();
    let rb = |pairs: &[(&str, f64)]| pairs.iter().map(|(n, v)| ((*n).to_string(), *v)).collect::<BTreeMap<_, _>>();
    assert!(c.holds(&rb(&[("FS_GCS_ENABLE", 7.0), ("FS_OPTIONS", 16.0), ("FS_GCS_TIMEOUT", 5.0)])).is_ok());
    // a shorter timer is in force too; a firmware without FS_OPTIONS has nothing to override it
    assert!(c.holds(&rb(&[("FS_GCS_ENABLE", 7.0), ("FS_GCS_TIMEOUT", 3.0)])).is_ok());
    for (bad, why) in [
        (rb(&[("FS_GCS_ENABLE", 1.0), ("FS_OPTIONS", 16.0), ("FS_GCS_TIMEOUT", 5.0)]), "reads 1, not 7"),
        (rb(&[("FS_GCS_ENABLE", 7.0), ("FS_OPTIONS", 18.0), ("FS_GCS_TIMEOUT", 5.0)]), "not clear"),
        (rb(&[("FS_GCS_ENABLE", 7.0), ("FS_OPTIONS", 16.0), ("FS_GCS_TIMEOUT", 30.0)]), "not within"),
        (rb(&[("FS_GCS_ENABLE", 7.0), ("FS_OPTIONS", 16.0)]), "FS_GCS_TIMEOUT was not read back"),
        (rb(&[]), "was not read back"),
    ] {
        let e = c.holds(&bad).unwrap_err();
        assert!(e.contains(why), "{e} (want {why})");
    }
    // a restore puts back exactly what the preload read first
    let before = rb(&[("FS_GCS_ENABLE", 1.0), ("FS_OPTIONS", 16.0), ("FS_GCS_TIMEOUT", 30.0)]);
    assert!(engage::restored(&before, &before).is_ok());
    assert!(engage::restored(&before, &rb(&[("FS_GCS_ENABLE", 7.0), ("FS_OPTIONS", 16.0), ("FS_GCS_TIMEOUT", 30.0)])).is_err());
    assert!(engage::restored(&BTreeMap::new(), &before).is_err(), "nothing was read: nothing to put back");
}

/// Nominate (op-1), sign (op-2, op-3), dispatch: the engagement order and its id.
fn dispatched(e: &mut Engine, clock: &ManualClock) -> (IntentId, EngageOrder) {
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let op3 = e.open_operator_session_with(OperatorId::new("op-3"), Grants { engage: true }).unwrap();
    let nomination = Proposal::new(
        Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Return,
        },
        vec![Uid::new("blade-1")],
    );
    let id = e.submit(op1.authority(), nomination, &world(clock.now()), &terrain()).unwrap().id();
    for op in [&op2, &op3] {
        clock.advance_ms(1000);
        let w = world(clock.now());
        let auth = op.engagement_authority().unwrap();
        let token = e.issue_picture(id, auth, &w).unwrap().id;
        e.approve_engagement(id, auth, None, token, &w, &terrain()).unwrap();
    }
    let Order::Engage(o) = e.dispatch_order(id).unwrap() else { panic!("not an engagement order") };
    (id, o)
}

fn report(uid: &str, phase: PreloadPhase, before: &[(&str, f64)], readback: &[(&str, f64)]) -> PreloadReport {
    let m = |p: &[(&str, f64)]| p.iter().map(|(n, v)| ((*n).to_string(), *v)).collect();
    PreloadReport {
        uid: Uid::new(uid),
        phase,
        before: m(before),
        readback: m(readback),
        error: None,
    }
}

const OWN: &[(&str, f64)] = &[("FS_GCS_ENABL", 1.0), ("FS_LONG_ACTN", 1.0), ("FS_LONG_TIMEOUT", 120.0)];
const SET: &[(&str, f64)] = &[("FS_GCS_ENABL", 1.0), ("FS_LONG_ACTN", 1.0), ("FS_LONG_TIMEOUT", 5.0)];

#[test]
fn the_order_carries_the_preload_and_the_engine_binds_its_reports() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (id, order) = dispatched(&mut e, &clock);
    let p = order.preload.clone().unwrap();
    assert_eq!((p.firmware.as_str(), p.action.as_str(), p.fallback), ("ArduPlane", "RTL", Fallback::Return));
    assert_eq!(p.timer_max_s, 5.0, "the approval's 5 s link-gap limit");
    // what replay would reject, refused with nothing logged
    let n = log.lines().len();
    let refused = |e: &mut Engine, r: PreloadReport| matches!(e.record_preload(id, r), Err(EngineError::NotItsOrder { .. }));
    assert!(refused(&mut e, report("other-1", PreloadPhase::Set, OWN, SET)), "another vehicle");
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Restore, &[], OWN)), "a restore before any set");
    assert_eq!(log.lines().len(), n);
    // the platform confirms it: logged, nothing stopped
    assert!(e.record_preload(id, report("blade-1", PreloadPhase::Set, OWN, SET)).unwrap().is_none());
    assert!(e.intent(id).unwrap().engagement().unwrap().preloaded.as_ref().unwrap().confirmed);
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Set, OWN, SET)), "a second set");
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Restore, &[], OWN)), "a restore while under way");
    // once it ended, its own values go back
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.abort_engagement(id, op.authority(), "stop").unwrap();
    assert!(e.record_preload(id, report("blade-1", PreloadPhase::Restore, &[], SET)).unwrap().is_none());
    assert!(!e.intent(id).unwrap().engagement().unwrap().restored.as_ref().unwrap().confirmed, "not its own values");
    assert!(e.record_preload(id, report("blade-1", PreloadPhase::Restore, &[], OWN)).unwrap().is_none());
    assert!(e.intent(id).unwrap().engagement().unwrap().restored.as_ref().unwrap().confirmed);
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Restore, &[], OWN)), "after it was put back");
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_preload_that_does_not_confirm_stops_the_engagement_with_its_fallback() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (id, _) = dispatched(&mut e, &clock);
    // the timer read back longer than the approval allows
    let long = [("FS_GCS_ENABL", 1.0), ("FS_LONG_ACTN", 1.0), ("FS_LONG_TIMEOUT", 120.0)];
    let f = e.record_preload(id, report("blade-1", PreloadPhase::Set, OWN, &long)).unwrap().unwrap();
    assert_eq!(f.fallback, Fallback::Return);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted);
    let why = &i.engagement().unwrap().stop.as_ref().unwrap().reason;
    assert!(why.contains("lost-link action could not be preloaded: FS_LONG_TIMEOUT reads 120"), "{why}");
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

/// A released engagement's log (scenario 01), its lines.
fn released_log() -> Vec<String> {
    let f = root().join("sim/scenarios/engage/01-nominal.toml");
    let r = scenario::run_file(&f, &root(), &RunOptions::default()).unwrap();
    assert!(r.ok, "{:?}", r.failures);
    r.log.lines().map(str::to_string).collect()
}

fn rechain(lines: &[String]) -> String {
    let mut prev = "0".repeat(64);
    let mut out = String::new();
    for (n, l) in lines.iter().enumerate() {
        let mut e: audit::Event = serde_json::from_str(l).unwrap();
        e.seq = n as u64 + 1;
        e.prev = prev.clone();
        let line = serde_json::to_string(&e).unwrap();
        prev = murmur_decide::hash::sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn whats(text: &str) -> Vec<String> {
    replay::replay_str(text).unwrap().mismatches.into_iter().map(|m| m.what).collect()
}

#[test]
fn replay_binds_the_order_to_the_approved_fallbacks_preload() {
    let lines = released_log();
    // the dispatched order says the platform keeps continuing its mission on a lost link
    let forged: Vec<String> = lines
        .iter()
        .map(|l| {
            if l.contains("\"type\":\"intent.dispatched\"") {
                l.replace("{\"name\":\"FS_LONG_ACTN\",\"value\":1.0}", "{\"name\":\"FS_LONG_ACTN\",\"value\":0.0}")
            } else if l.contains("\"type\":\"engage.preload\"") {
                l.replace("\"FS_LONG_ACTN\":1.0", "\"FS_LONG_ACTN\":0.0")
            } else {
                l.clone()
            }
        })
        .collect();
    assert_ne!(forged, lines);
    let w = whats(&rechain(&forged));
    assert!(w.iter().any(|m| m.contains("lost-link preload is not the approved fallback")), "{w:?}");
    // and a preload report on a log of older rules is a downgrade marker
    let claimed: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| if i == 0 { l.replacen("\"rules_version\":5,", "\"rules_version\":4,", 1) } else { l.clone() })
        .collect();
    let r = replay::replay_str_with(
        &rechain(&claimed),
        &replay::ReplayOptions { rules: replay::RulesAccepted::Exactly(4), ..Default::default() },
    )
    .unwrap();
    let w: Vec<&str> = r.mismatches.iter().map(|m| m.what.as_str()).collect();
    for marker in ["lost-link preload report", "an engagement order's lost-link preload", "a fallback_preload check"] {
        assert!(w.iter().any(|m| m.contains(marker) && m.contains("only builds of rules 5")), "{marker}: {w:?}");
    }
}

// ---- der.3 (a): the keep-out's prediction model -----------------------------------------------

#[test]
fn a_fast_track_on_a_steady_course_is_as_predictable_as_a_slow_one() {
    let ec5 = rules(5).engage_rules();
    // the issue's case: a 20 m/s ferry 300 s out. Rules 4: 2-sigma of 15 + max(1, 0.3 x 20)
    // x 300 = 3,630 m. Rules 5: tracker 15 m, velocity 1 m/s x 300 s, manoeuvre 0.2 m/s^2:
    // sqrt(15^2 + 300^2 + 0.04 x 300^3 / 3) = 671 m, 2-sigma 1,342 m
    let ferry = track(T0, TARGET_AT, Some([-20.0, 0.0]), Affiliation::Neutral, Domain::Surface);
    let old = 2.0 * engage::track_sigma_at(&ferry, T0, T0.plus_ms(300_000));
    let new = 2.0 * engage::track_prediction_sigma(&ferry, 300.0, &ec5);
    assert!((old - 3630.0).abs() < 1.0, "{old}");
    assert!((new - 1342.0).abs() < 2.0, "{new}");
    // and it no longer depends on the speed: a 6 m/s tour boat gets the same
    let boat = track(T0, TARGET_AT, Some([-6.0, 0.0]), Affiliation::Neutral, Domain::Surface);
    assert_eq!(engage::track_prediction_sigma(&boat, 300.0, &ec5), engage::track_prediction_sigma(&ferry, 300.0, &ec5));
    // a short prediction stays near the tracker's own uncertainty
    assert!(engage::track_prediction_sigma(&ferry, 10.0, &ec5) < 20.0);
    // an aircraft manoeuvres harder (2 m/s^2): at 60 s, 540 m 1-sigma
    let plane = track(T0, TARGET_AT, Some([55.0, 0.0]), Affiliation::Neutral, Domain::Air);
    assert!((engage::track_prediction_sigma(&plane, 60.0, &ec5) - 540.0).abs() < 2.0);
}

/// `keep_out_tracks` around the target for an effect `span_s` from now, under `version`.
fn tracks_check(w: &WorldSnapshot, span_s: i64, version: u32) -> CheckResult {
    let r = rules(version);
    let ec = r.engage_rules();
    let t = w.t.plus_ms(span_s * 1000);
    let alt = engage::effect_alt(w.tracks.get("T-0001"), t, 75.0, &ec);
    engage::keep_out_tracks(TARGET_AT, t, 275.0, "T-0001", w, &ec, alt)
}

#[test]
fn a_distant_fast_ferry_no_longer_blocks_but_a_near_one_does() {
    // a 20 m/s ferry whose course passes 2 km from the effect point at the effect time, 300 s
    // out (it is 6 km east of there now)
    let north_2km = LonLat::new(TARGET_AT.lon + 6_000.0 / 87_943.0, TARGET_AT.lat + 2_000.0 / 111_320.0);
    let mut w = world(T0);
    w.tracks.insert("T-0002".into(), track(T0, north_2km, Some([-20.0, 0.0]), Affiliation::Neutral, Domain::Surface));
    assert_eq!(tracks_check(&w, 300, 4).outcome, Outcome::Fail, "rules 4: 3.6 km of 2-sigma");
    let c = tracks_check(&w, 300, 5);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    // the same ferry passing 1 km away still blocks
    let north_1km = LonLat::new(north_2km.lon, TARGET_AT.lat + 1_000.0 / 111_320.0);
    w.tracks.insert("T-0002".into(), track(T0, north_1km, Some([-20.0, 0.0]), Affiliation::Neutral, Domain::Surface));
    let c = tracks_check(&w, 300, 5);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("velocity 300 m, manoeuvre 600 m"), "{}", c.detail);
}

#[test]
fn a_track_without_a_velocity_estimate_blocks_by_its_reach() {
    // fail closed: rules 4 held it where it was (1 m/s of growth); rules 5 lets it be anywhere
    // it reaches at its domain's top speed (25 m/s on the surface)
    let mut w = world(T0);
    let at = LonLat::new(TARGET_AT.lon, TARGET_AT.lat + 2_000.0 / 111_320.0);
    w.tracks.insert("T-0002".into(), track(T0, at, None, Affiliation::Unknown, Domain::Surface));
    assert_eq!(tracks_check(&w, 100, 4).outcome, Outcome::Pass);
    let c = tracks_check(&w, 100, 5);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("no velocity estimate"), "{}", c.detail);
}

// ---- der.3 (c): altitude ---------------------------------------------------------------------

fn friendly_check(w: &WorldSnapshot, span_s: i64, version: u32) -> CheckResult {
    let r = rules(version);
    let t = w.t.plus_ms(span_s * 1000);
    let alt = engage::effect_alt(w.tracks.get("T-0001"), t, 75.0, &r.engage_rules());
    engage::keep_out_friendly(TARGET_AT, t, 275.0, Some(&Uid::new("blade-1")), w, &r, alt)
}

/// The world with the target on the surface (altitude 0 m known or not) and one of ours, a
/// MALE aircraft (or the `platform` given) hovering over the effect point at `alt` (None:
/// not reported).
fn above(target_alt: Option<f64>, platform: &str, alt: Option<f64>) -> WorldSnapshot {
    let mut w = world(T0);
    w.tracks.get_mut("T-0001").unwrap().alt_m = target_alt;
    let mut v = fresh(vehicle(platform, TARGET_AT), T0);
    v.alt_msl_m = alt.map(|a| Fact::new(a, T0));
    w.vehicles.insert(Uid::new("over-1"), v);
    w
}

#[test]
fn keep_outs_are_3d_only_where_both_altitudes_are_known() {
    // a MALE aircraft at 7,000 m over a surface effect, 120 s out: clear in 3D (it can't get
    // below 7,000 - 2 x 10 - 10 m/s x 120 s = 5,780 m), blocking in 2D
    let w = above(Some(0.0), "mq9b", Some(7_000.0));
    let c = friendly_check(&w, 120, 5);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    assert!(c.detail.contains("clear by altitude: over-1 at 7000 m"), "{}", c.detail);
    assert_eq!(friendly_check(&w, 120, 4).outcome, Outcome::Fail, "rules 4: 2D");
    // a quadcopter at 120 m still blocks: its vertical reach covers the keep-out above the effect
    let c = friendly_check(&above(Some(0.0), "x10", Some(120.0)), 120, 5);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("within the vertical keep-out too"), "{}", c.detail);
    // the catalog's climb rate counts when it is faster: a CCA-class aircraft (40 m/s) at
    // 3,000 m could be down at the effect in 120 s
    assert_eq!(friendly_check(&above(Some(0.0), "cca", Some(3_000.0)), 120, 5).outcome, Outcome::Fail);
    assert_eq!(friendly_check(&above(Some(0.0), "mq9b", Some(3_000.0)), 120, 5).outcome, Outcome::Pass);
    // unknown altitude, ours or the target's: 2D, no ceiling
    let c = friendly_check(&above(Some(0.0), "mq9b", None), 120, 5);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("its altitude is unknown"), "{}", c.detail);
    assert_eq!(friendly_check(&above(None, "mq9b", Some(7_000.0)), 120, 5).outcome, Outcome::Fail);
    // a stale altitude is not known either
    let mut w = above(Some(0.0), "mq9b", Some(7_000.0));
    w.vehicles.get_mut(&Uid::new("over-1")).unwrap().alt_msl_m = Some(Fact::new(7_000.0, T0.plus_ms(-60_000)));
    assert_eq!(friendly_check(&w, 120, 5).outcome, Outcome::Fail);
    // a protected aircraft track at 3,000 m over the effect: clear in 3D; at 450 m: not
    let mut w = world(T0);
    w.tracks.get_mut("T-0001").unwrap().alt_m = Some(0.0);
    let mut plane = track(T0, TARGET_AT, Some([0.0, 0.0]), Affiliation::Neutral, Domain::Air);
    plane.alt_m = Some(3_000.0);
    w.tracks.insert("T-0002".into(), plane.clone());
    let c = tracks_check(&w, 120, 5);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    plane.alt_m = Some(450.0);
    w.tracks.insert("T-0002".into(), plane.clone());
    assert_eq!(tracks_check(&w, 120, 5).outcome, Outcome::Fail);
    plane.alt_m = None;
    w.tracks.insert("T-0002".into(), plane);
    assert_eq!(tracks_check(&w, 120, 5).outcome, Outcome::Fail, "its altitude unknown: 2D");
}

// ---- der.3 (b): lost friendlies ----------------------------------------------------------------

#[test]
fn only_an_operator_accepts_a_lost_friendly_and_only_the_loss_it_names() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    // our quadcopter went silent 2 km east of the target a minute ago
    clock.advance_ms(60_000);
    let mut w = world(clock.now());
    let lost_at = LonLat::new(TARGET_AT.lon + 2_000.0 / 87_943.0, TARGET_AT.lat);
    let mut drone = fresh(vehicle("x10", lost_at), T0);
    drone.autopilot = Some(Fact::new(AutopilotKind::Copter, T0));
    w.vehicles.insert(Uid::new("drone-1"), drone);
    let nomination = || {
        Proposal::new(
            Task::Engage {
                track: "T-0001".into(),
                effect_radius_m: 75.0,
                window: None,
                fallback: Fallback::Return,
            },
            vec![Uid::new("blade-1")],
        )
    };
    // without an acceptance: rejected, and the detail says since when
    let i = e.submit(op1.authority(), nomination(), &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::Rejected);
    let c = i.checks().iter().find(|c| c.rule == RuleId::KeepOutFriendly).unwrap();
    assert!(c.detail.contains(&format!("since t={}", T0.0)) && c.detail.contains("an operator may accept"), "{}", c.detail);
    // an acceptance must name the loss as it is: not another report, not a vehicle that is fine
    let n = log.lines().len();
    let wrong = [(Uid::new("drone-1"), Some(T0.plus_ms(1000)))];
    assert!(matches!(e.submit_accepting(op1.authority(), nomination(), &wrong, &w, &terrain()), Err(EngineError::NotAccepted { .. })));
    let fine = [(Uid::new("blade-1"), Some(clock.now()))];
    assert!(matches!(e.submit_accepting(op1.authority(), nomination(), &fine, &w, &terrain()), Err(EngineError::NotAccepted { .. })));
    // and only on an engagement
    let hold = Proposal::new(Task::Hold {}, vec![Uid::new("blade-1")]);
    let right = [(Uid::new("drone-1"), Some(T0))];
    assert!(matches!(e.submit_accepting(op1.authority(), hold, &right, &w, &terrain()), Err(EngineError::NotAccepted { .. })));
    assert_eq!(log.lines().len(), n, "nothing logged");
    // the loss as it is: accepted, logged before the checks, and the keep-out leaves it out
    let i = e.submit_accepting(op1.authority(), nomination(), &right, &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    let c = i.checks().iter().find(|c| c.rule == RuleId::KeepOutFriendly).unwrap();
    assert!(c.detail.contains("position accepted as unknown: drone-1"), "{}", c.detail);
    let id = i.id();
    // the vehicle reports again: it counts like any other (here, clear 2 km away)
    w.vehicles.get_mut(&Uid::new("drone-1")).unwrap().position = Some(Fact::new(lost_at, clock.now()));
    let r = rules(5);
    let c = engage::keep_out_friendly(TARGET_AT, clock.now(), 275.0, Some(&Uid::new("blade-1")), &{
        let mut x = w.clone();
        x.unknown_accepted = e.intent(id).unwrap().engagement().unwrap().accepted.clone();
        x
    }, &r, None);
    assert!(!c.detail.contains("accepted"), "{}", c.detail);
    // after a signature an acceptance would change what was signed: refused
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let mut w2 = world(clock.now());
    w2.vehicles.insert(Uid::new("drone-1"), {
        let mut d = fresh(vehicle("x10", lost_at), T0);
        d.autopilot = Some(Fact::new(AutopilotKind::Copter, T0));
        d
    });
    let auth = op2.engagement_authority().unwrap();
    let token = e.issue_picture(id, auth, &w2).unwrap().id;
    e.approve_engagement(id, auth, None, token, &w2, &terrain()).unwrap();
    let err = e.accept_unknown_friendly(id, op1.authority(), Uid::new("drone-1"), Some(T0), &w2).unwrap_err();
    assert!(matches!(err, EngineError::NotAccepted { .. }), "{err}");
    let rep = replay::replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

#[test]
fn policy_keys_of_rules_5_are_bounded_and_of_schema_3() {
    let mut e = rules(5).engage_rules();
    assert!(e.validate().is_ok());
    for (k, v) in [("track_velocity_sigma_mps", 0.0), ("vertical_rate_mps", 0.0), ("keep_out_vertical_m", 10.0)] {
        let mut x = e.clone();
        match k {
            "track_velocity_sigma_mps" => x.track_velocity_sigma_mps = Some(v),
            "vertical_rate_mps" => x.vertical_rate_mps = Some(v),
            _ => x.keep_out_vertical_m = Some(v),
        }
        assert!(x.validate().unwrap_err().contains(k), "{k}");
    }
    e.track_accel_mps2 = Some([(Domain::Air, 0.0)].into_iter().collect());
    assert!(e.validate().unwrap_err().contains("track_accel_mps2"));
    // the shipped file sets them, each a schema 3 key
    let shipped = rules(5).engage_rules();
    let keys = shipped.schema_keys();
    for k in ["track_velocity_sigma_mps", "track_accel_mps2", "vertical_rate_mps", "keep_out_vertical_m"] {
        assert!(keys.contains(&(3, k)), "{k}: {keys:?}");
    }
    assert_eq!(shipped.track_accel(Domain::Surface), 0.2);
    assert_eq!(shipped.keep_out_vertical(), 200.0);
}
