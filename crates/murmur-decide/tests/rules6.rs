//! Decision rules 6, the engagement part: the approval's fallback preloaded as the
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

/// A track seen at `t` by its sources (AIS by default), its altitude (when one is set)
/// measured then too.
fn track(t: Time, at: LonLat, vel: Option<[f64; 2]>, aff: Affiliation, domain: Domain) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, t),
        alt_m: None,
        alt_t: Some(t),
        velocity_mps: vel,
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(vel.map_or(1.0, |[e, n]| (0.3 * e.hypot(n)).max(1.0))),
        confidence: 0.9,
        affiliation: Fact::new(aff, t),
        affiliation_confidence: 0.8,
        identity: Some("ferry".into()),
        identity_confidence: 0.9,
        sources: vec!["ais".into()],
        source_t: [("ais".to_string(), t)].into_iter().collect(),
        domain,
        last_seen_t: t,
        declared_by: (aff == Affiliation::Hostile).then(|| "op-1".to_string()),
    }
}

/// The track's sources, each last hit at `t`.
fn seen_by(tr: &mut TrackFacts, sources: &[&str], t: Time) {
    tr.sources = sources.iter().map(|s| (*s).to_string()).collect();
    tr.source_t = sources.iter().map(|s| ((*s).to_string(), t)).collect();
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
        vehicle: None,
    }
}

/// The bridge's preload as it reports it: its write-ahead (the platform's own values, logged
/// before anything is written), then the set and its read-back.
fn set(e: &mut Engine, id: IntentId, before: &[(&str, f64)], readback: &[(&str, f64)]) -> Result<Option<FallbackOrder>, EngineError> {
    let mut intent = report("blade-1", PreloadPhase::Intent, before, &[]);
    intent.vehicle = Some(VehicleIdentity {
        platform: Some("switchblade".into()),
        callsign: Some("BLADE 1".into()),
        sysid: Some(1),
        home: Some(BLADE_AT),
    });
    e.record_preload(id, intent)?;
    e.record_preload(id, report("blade-1", PreloadPhase::Set, before, readback))
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
    // a set without its write-ahead, or with other values than it logged, is refused; then
    // the platform confirms it: logged, nothing stopped
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Set, OWN, SET)), "a set without its write-ahead");
    assert!(e.record_preload(id, report("blade-1", PreloadPhase::Intent, OWN, &[])).unwrap().is_none());
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Intent, OWN, &[])), "a second write-ahead");
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Set, SET, SET)), "other values than its write-ahead's");
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
    let f = set(&mut e, id, OWN, &long).unwrap().unwrap();
    assert_eq!(f.fallback, Fallback::Return);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted);
    let why = &i.engagement().unwrap().stop.as_ref().unwrap().reason;
    assert!(why.contains("lost-link action could not be preloaded: FS_LONG_TIMEOUT reads 120"), "{why}");
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
}

#[test]
fn a_preload_changed_before_the_release_stops_the_engagement() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (id, _) = dispatched(&mut e, &clock);
    let refused = |e: &mut Engine, r: PreloadReport| matches!(e.record_preload(id, r), Err(EngineError::NotItsOrder { .. }));
    assert!(refused(&mut e, report("blade-1", PreloadPhase::Verify, &[], SET)), "a re-read before any set");
    assert!(set(&mut e, id, OWN, SET).unwrap().is_none());
    clock.advance_ms(5_000);
    assert!(e.record_preload(id, report("blade-1", PreloadPhase::Verify, &[], SET)).unwrap().is_none());
    let eng = e.intent(id).unwrap().engagement().unwrap().clone();
    assert_eq!(engage::preload_confirmed_at(&eng), Some(clock.now()));
    // another station put its own values back: the next re-read stops it with its fallback
    clock.advance_ms(5_000);
    let f = e.record_preload(id, report("blade-1", PreloadPhase::Verify, &[], OWN)).unwrap().unwrap();
    assert_eq!(f.fallback, Fallback::Return);
    let i = e.intent(id).unwrap();
    assert_eq!(i.state(), State::Aborted);
    let why = &i.engagement().unwrap().stop.as_ref().unwrap().reason;
    assert!(why.contains("lost-link action changed: FS_LONG_TIMEOUT reads 120"), "{why}");
    assert_eq!(engage::preload_confirmed_at(i.engagement().unwrap()), None);
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // its own values are still owed (they were never confirmed back): replay says so
    assert_eq!(r.unrestored.len(), 1, "{:?}", r.unrestored);
    assert!(r.unrestored[0].starts_with("blade-1: its own lost-link values"), "{:?}", r.unrestored);
}

#[test]
fn values_owed_back_are_reported_carried_and_never_taken_off_a_preloaded_platform() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (id, _) = dispatched(&mut e, &clock);
    set(&mut e, id, OWN, SET).unwrap();
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.abort_engagement(id, op.authority(), "stop").unwrap();
    // its restore never confirmed: reported, not a mismatch
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok() && r.unrestored.len() == 1, "{:?} {:?}", r.mismatches, r.unrestored);
    // the bridge puts them back on its own (a refused engagement restore, a carried one)
    let own: BTreeMap<String, f64> = OWN.iter().map(|(n, v)| ((*n).to_string(), *v)).collect();
    let attempt = |readback: &BTreeMap<String, f64>, error: Option<&str>| PlatformRestoreReport {
        uid: Uid::new("blade-1"),
        values: own.clone(),
        carried: false,
        from: None,
        readback: readback.clone(),
        error: error.map(str::to_string),
        vehicle: None,
    };
    assert!(!e.record_platform_restore(attempt(&BTreeMap::new(), Some("no answer"))).unwrap());
    assert!(e.record_platform_restore(attempt(&own, None)).unwrap());
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    // a log that carries them over from an earlier run owes them from its start; a preload
    // that then reads the preloaded values as the platform's own is caught
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let carried = PlatformRestoreReport {
        uid: Uid::new("blade-1"),
        values: own.clone(),
        carried: true,
        from: Some("20261004T000000Z-1.jsonl I-0001".into()),
        readback: BTreeMap::new(),
        error: None,
        vehicle: None,
    };
    assert!(!e.record_platform_restore(carried).unwrap());
    let (id, _) = dispatched(&mut e, &clock);
    set(&mut e, id, SET, SET).unwrap();
    let w = whats(&log.text());
    assert!(w.iter().any(|m| m.contains("while earlier ones were owed back")), "{w:?}");
}

#[test]
fn a_crash_between_the_write_and_its_report_still_leaves_the_values_owed() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (id, _) = dispatched(&mut e, &clock);
    // the write-ahead is logged, then the bridge stops before the set's report
    let mut intent = report("blade-1", PreloadPhase::Intent, OWN, &[]);
    let identity = VehicleIdentity {
        platform: Some("switchblade".into()),
        callsign: Some("BLADE 1".into()),
        sysid: Some(1),
        home: Some(BLADE_AT),
    };
    intent.vehicle = Some(identity.clone());
    e.record_preload(id, intent).unwrap();
    let text = log.text();
    let owed = replay::owed_restores(&text);
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0].values["FS_LONG_TIMEOUT"], 120.0);
    assert_eq!(owed[0].by, [id]);
    assert_eq!(owed[0].vehicle.as_ref(), Some(&identity), "the vehicle they were preloaded on");
    let r = replay::replay_str(&text).unwrap();
    assert!(r.ok() && r.unrestored.len() == 1, "{:?} {:?}", r.mismatches, r.unrestored);
    // a log that cuts the write-ahead leaves a set without one: caught
    let lines: Vec<String> = log.lines();
    let mut forged: Vec<String> = lines[..lines.len() - 1].to_vec();
    forged.push(lines.last().unwrap().replace("\"phase\":\"intent\"", "\"phase\":\"set\""));
    let w = whats(&rechain(&forged));
    assert!(w.iter().any(|m| m.contains("not its write-ahead's (or that has none)")), "{w:?}");
}

#[test]
fn a_carried_restore_goes_only_to_the_vehicle_it_was_preloaded_on() {
    let was = VehicleIdentity {
        platform: Some("switchblade".into()),
        callsign: Some("BLADE 1".into()),
        sysid: Some(1),
        home: Some(BLADE_AT),
    };
    assert!(was.matches(&was).is_ok());
    let other = |f: &dyn Fn(&mut VehicleIdentity)| {
        let mut v = was.clone();
        f(&mut v);
        was.matches(&v)
    };
    assert!(other(&|v| v.platform = Some("magura".into())).unwrap_err().contains("platform"));
    assert!(other(&|v| v.callsign = Some("BLADE 2".into())).unwrap_err().contains("callsign"));
    assert!(other(&|v| v.sysid = Some(7)).unwrap_err().contains("system id"));
    assert!(other(&|v| v.home = None).unwrap_err().contains("not known yet"));
    let far = LonLat::new(BLADE_AT.lon + 2_000.0 / 87_943.0, BLADE_AT.lat);
    assert!(other(&|v| v.home = Some(far)).unwrap_err().contains(" m from the one preloaded"));
    let near = LonLat::new(BLADE_AT.lon + 100.0 / 87_943.0, BLADE_AT.lat);
    assert!(other(&|v| v.home = Some(near)).is_ok(), "within 500 m");
}

#[test]
fn no_restore_while_another_engagement_holds_the_platform() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (a, _) = dispatched(&mut e, &clock);
    set(&mut e, a, OWN, SET).unwrap();
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.abort_engagement(a, op.authority(), "stop").unwrap();
    // a second engagement is dispatched on the same platform before A's restore ran
    clock.advance_ms(1000);
    let (b, _) = dispatched(&mut e, &clock);
    let refused = |e: &mut Engine, id: IntentId, r: PreloadReport| matches!(e.record_preload(id, r), Err(EngineError::NotItsOrder { .. }));
    assert!(refused(&mut e, a, report("blade-1", PreloadPhase::Restore, &[], OWN)), "A's restore would undo B's preload");
    let own: BTreeMap<String, f64> = OWN.iter().map(|(n, v)| ((*n).to_string(), *v)).collect();
    let attempt = PlatformRestoreReport {
        uid: Uid::new("blade-1"),
        values: own.clone(),
        carried: false,
        from: None,
        readback: own.clone(),
        error: None,
        vehicle: None,
    };
    assert!(e.record_platform_restore(attempt.clone()).is_err(), "nor the platform's own restore");
    let n = log.lines().len();
    // B takes A's values over and ends; then they go back
    set(&mut e, b, OWN, SET).unwrap();
    e.abort_engagement(b, op.authority(), "stop").unwrap();
    assert!(e.record_preload(b, report("blade-1", PreloadPhase::Restore, &[], OWN)).unwrap().is_none());
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    // a log with A's restore logged while B held the platform: caught
    let mut lines: Vec<String> = log.lines();
    let restore = lines.last().unwrap().replace(&format!("\"intent\":{}", b.0), &format!("\"intent\":{}", a.0));
    lines.insert(n, restore);
    let w = whats(&rechain(&lines));
    assert!(w.iter().any(|m| m.contains("while another engagement holds the platform")), "{w:?}");
}

// ---- murmur-aca: carried values against what the log they name still owed ------------------

/// What the bridge logs at start from the log its `.last-run-<port>` pointer names (`file`,
/// `text`; `carry_from` in murmur-bridge): every platform whose own values that log still
/// owed, carried with the engagements that preloaded them and the vehicle they were on.
fn carry_from(e: &mut Engine, file: &str, text: &str) {
    for o in replay::owed_restores(text) {
        let by: Vec<String> = o.by.iter().map(ToString::to_string).collect();
        let from = if by.is_empty() {
            format!("{file} (carried over into it)")
        } else {
            format!("{file} (preloaded by {})", by.join(", "))
        };
        let carried = PlatformRestoreReport {
            uid: o.uid,
            values: o.values,
            carried: true,
            from: Some(from),
            readback: BTreeMap::new(),
            error: None,
            vehicle: o.vehicle,
        };
        e.record_platform_restore(carried).unwrap();
    }
}

/// A run that preloaded blade-1, stopped the engagement and ended before the platform's own
/// values went back: it still owes them.
fn owing_run() -> String {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let (id, _) = dispatched(&mut e, &clock);
    set(&mut e, id, OWN, SET).unwrap();
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.abort_engagement(id, op.authority(), "stop").unwrap();
    log.text()
}

/// The next run on the port: it carries them over from `file` (`text`) and puts them back.
fn carrying_run(file: &str, text: &str) -> String {
    let clock = ManualClock::new(T0.plus_ms(600_000));
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    carry_from(&mut e, file, text);
    clock.advance_ms(2_000);
    let own: BTreeMap<String, f64> = OWN.iter().map(|(n, v)| ((*n).to_string(), *v)).collect();
    let back = PlatformRestoreReport {
        uid: Uid::new("blade-1"),
        values: own.clone(),
        carried: false,
        from: None,
        readback: own,
        error: None,
        vehicle: None,
    };
    assert!(e.record_platform_restore(back).unwrap());
    log.text()
}

/// Replay `text` with `named` given as the log its carried values name, or trusting them.
fn carried_with(text: &str, named: Option<(&str, &str)>, trusted: bool) -> replay::ReplayReport {
    let opts = replay::ReplayOptions {
        restores_from: named,
        trust_resumed: trusted,
        ..Default::default()
    };
    replay::replay_str_with(text, &opts).unwrap()
}

/// Replay `text` with the log its carried values name (`a.jsonl`) given.
fn against(text: &str, a: &str) -> replay::ReplayReport {
    carried_with(text, Some(("a.jsonl", a)), false)
}

fn whats_of(r: replay::ReplayReport) -> Vec<String> {
    r.mismatches.into_iter().map(|m| m.what).collect()
}

#[test]
fn carried_values_are_checked_against_what_the_log_they_name_still_owed() {
    let a = owing_run();
    let b = carrying_run("a.jsonl", &a);
    let owed = replay::owed_restores(&a);
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert!(b.contains(&format!("\"from\":\"a.jsonl (preloaded by {})\"", owed[0].by[0])), "{b}");
    let a_head = replay::replay_str(&a).unwrap().head_sha256;
    let checked = |checked: bool| {
        vec![replay::CarriedRestores {
            log: "a.jsonl".into(),
            restores: 1,
            checked,
            head: if checked { a_head.clone() } else { None },
        }]
    };
    // the genuine log: checked against a.jsonl (its head reported, to compare with its
    // anchors), nothing wrong
    let r = against(&b, &a);
    assert!(r.ok() && r.unrestored.is_empty(), "{:?} {:?}", r.mismatches, r.unrestored);
    assert_eq!(r.carried_restores, checked(true));
    // without a.jsonl, or with another log than the one named: not verified, a mismatch
    // (fail closed, as for carried intents); trusted, reported unchecked
    for named in [None, Some(("other.jsonl", a.as_str()))] {
        let r = carried_with(&b, named, false);
        assert_eq!(r.carried_restores, checked(false));
        let w = whats_of(r);
        assert_eq!(
            w,
            ["lost-link values carried over from a.jsonl not verified: replay with that log (murmur-replay --prev), or trust it (--trust-resumed)"],
            "{named:?}"
        );
        let r = carried_with(&b, named, true);
        assert!(r.ok(), "{:?}", r.mismatches);
        assert_eq!(r.carried_restores, checked(false));
    }
    // forged and re-chained, each consistent within the log (it replays trusting its
    // carry-overs) and caught against a.jsonl
    let caught = |text: &str, what: &[&str]| {
        let alone = carried_with(text, None, true);
        assert!(alone.ok(), "consistent on its own: {:?}", alone.mismatches);
        let w = whats_of(against(text, &a));
        for m in what {
            assert!(w.iter().any(|x| x.contains(m)), "{m}: {w:?}");
        }
    };
    let restores = |edit: &dyn Fn(&mut EventBody)| {
        forge(&b, |ev| {
            for e in ev.iter_mut().filter(|e| matches!(e.body, EventBody::PlatformRestore { .. })) {
                edit(&mut e.body);
            }
        })
    };
    // the preloaded timer carried (and "put back") as the platform's own
    let text = restores(&|body| {
        if let EventBody::PlatformRestore { values, readback, .. } = body {
            values.insert("FS_LONG_TIMEOUT".into(), 5.0);
            if !readback.is_empty() {
                readback.insert("FS_LONG_TIMEOUT".into(), 5.0);
            }
        }
    });
    caught(&text, &["carried values differ from what the log it names still owed"]);
    // the values made to go back to another vehicle than the one they were preloaded on
    let text = restores(&|body| {
        if let EventBody::PlatformRestore { carried: true, vehicle: Some(v), .. } = body {
            v.callsign = Some("BLADE 7".into());
            v.home = None;
        }
    });
    caught(&text, &["carried restore names another vehicle than the one the log it names preloaded"]);
    // another platform's: one it did not owe, and the one it did not carried
    let text = restores(&|body| {
        if let EventBody::PlatformRestore { uid, .. } = body {
            *uid = Uid::new("sky-1");
        }
    });
    caught(&text, &["carried restore of values the log it names did not owe", "still owed a platform's values this run did not carry over"]);
    // credited to another engagement
    let text = restores(&|body| {
        if let EventBody::PlatformRestore { from: Some(f), .. } = body {
            *f = "a.jsonl (preloaded by i9)".into();
        }
    });
    caught(&text, &["carried restore names other engagements"]);
    // carried twice
    let text = forge(&b, |ev| {
        let i = ev.iter().position(|e| matches!(e.body, EventBody::PlatformRestore { carried: true, .. })).unwrap();
        let twice = ev[i].clone();
        ev.insert(i + 1, twice);
    });
    caught(&text, &["carried over twice from the log it names"]);
    // and from two logs (one start carries from one): caught however it is replayed
    let text = forge(&b, |ev| {
        let i = ev.iter().position(|e| matches!(e.body, EventBody::PlatformRestore { carried: true, .. })).unwrap();
        let mut other = ev[i].clone();
        if let EventBody::PlatformRestore { from, .. } = &mut other.body {
            *from = Some("z.jsonl (carried over into it)".into());
        }
        ev.insert(i + 1, other);
    });
    let w = whats_of(carried_with(&text, None, true));
    assert!(w.iter().any(|m| m.contains("carried restores name more than one earlier log")), "{w:?}");
}

#[test]
fn the_log_carried_values_name_must_replay_itself() {
    // review of 3ed31ba, finding 1: the values were compared with whatever text was given
    // under that name; a forged "a.jsonl" of one unchained line owing the forged values passed
    let a = owing_run();
    let b = carrying_run("a.jsonl", &a);
    let forged_b = forge(&b, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::PlatformRestore { values, readback, carried, from, .. } = &mut e.body {
                values.insert("FS_LONG_TIMEOUT".into(), 600.0);
                if !readback.is_empty() {
                    readback.insert("FS_LONG_TIMEOUT".into(), 600.0);
                }
                if *carried {
                    *from = Some("a.jsonl (carried over into it)".into());
                }
            }
        }
    });
    let line = forged_b.lines().find(|l| l.contains("\"carried\":true")).unwrap();
    let fake_a = format!("{line}\n");
    assert_eq!(replay::owed_restores(&fake_a).len(), 1, "it owes the forged values");
    let r = against(&forged_b, &fake_a);
    let w: Vec<String> = r.mismatches.iter().map(|m| m.what.clone()).collect();
    assert_eq!(w, ["the log the carried values name does not replay"], "{:?}", r.mismatches);
    assert!(r.mismatches[0].logged.contains("run.started"), "{:?}", r.mismatches);
    // nor one that can't be read at all
    let w = whats_of(against(&b, "not a log\n"));
    assert!(w.iter().any(|m| m == "the log the carried values name can't be read"), "{w:?}");
    // the genuine one replays: its head is reported
    let r = against(&b, &a);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.carried_restores[0].head.is_some());
}

#[test]
fn a_carried_restore_names_its_log_by_a_bare_file_name() {
    // review of 3ed31ba, finding 4: a file name may hold " ("; the bridge's parenthesis is
    // the last one
    for (from, file) in [
        ("a.jsonl (preloaded by i1, i4)", "a.jsonl"),
        ("a.jsonl (carried over into it)", "a.jsonl"),
        ("run (1).jsonl (preloaded by i3)", "run (1).jsonl"),
        ("run (1).jsonl (carried over into it)", "run (1).jsonl"),
        ("run (1).jsonl", "run (1).jsonl"),
    ] {
        assert_eq!(replay::carried_restore_log(Some(from)), file, "{from}");
    }
    for (name, bare) in [("a.jsonl", true), ("run (1).jsonl", true), ("../x/a.jsonl", false), ("/tmp/a.jsonl", false), ("..", false), ("", false), ("x\\a.jsonl", false)] {
        assert_eq!(replay::is_log_file_name(name), bare, "{name}");
    }
    let a = owing_run();
    let b = carrying_run("run (1).jsonl", &a);
    let r = carried_with(&b, Some(("run (1).jsonl", &a)), false);
    assert!(r.ok(), "{:?}", r.mismatches);
    assert!(r.carried_restores[0].checked, "{:?}", r.carried_restores);
    // a carried restore that names its log by a path, or names none: a mismatch, trusted or not
    let b = carrying_run("a.jsonl", &a);
    for from in [Some("../other/a.jsonl (preloaded by i1)"), None] {
        let text = forge(&b, |ev| {
            for e in ev.iter_mut() {
                if let EventBody::PlatformRestore { carried: true, from: f, .. } = &mut e.body {
                    *f = from.map(str::to_string);
                }
            }
        });
        for trusted in [false, true] {
            let w = whats_of(carried_with(&text, Some(("a.jsonl", &a)), trusted));
            assert!(
                w.iter().any(|m| m.starts_with("carried restore names no earlier log by its file name")),
                "{from:?} {trusted}: {w:?}"
            );
        }
    }
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
        .map(|(i, l)| if i == 0 { l.replacen(&format!("\"rules_version\":{},", murmur_decide::RULES_VERSION), "\"rules_version\":5,", 1) } else { l.clone() })
        .collect();
    let r = replay::replay_str_with(
        &rechain(&claimed),
        &replay::ReplayOptions { rules: replay::RulesAccepted::Exactly(5), ..Default::default() },
    )
    .unwrap();
    let w: Vec<&str> = r.mismatches.iter().map(|m| m.what.as_str()).collect();
    for marker in ["lost-link preload report", "an engagement order's lost-link preload", "a fallback_preload check"] {
        assert!(w.iter().any(|m| m.contains(marker) && m.contains("only builds of rules 6")), "{marker}: {w:?}");
    }
}

// ---- der.3 (a): the keep-out's prediction model -----------------------------------------------

#[test]
fn a_fast_track_on_a_steady_course_is_as_predictable_as_a_slow_one() {
    let ec6 = rules(6).engage_rules();
    // the issue's case: a 20 m/s ferry 300 s out. Rules 4: 2-sigma of 15 + max(1, 0.3 x 20)
    // x 300 = 3,630 m. Rules 6: tracker 15 m, velocity 1 m/s x 300 s, manoeuvre 0.2 m/s^2:
    // sqrt(15^2 + 300^2 + 0.04 x 300^3 / 3) = 671 m, 2-sigma 1,342 m
    let ferry = track(T0, TARGET_AT, Some([-20.0, 0.0]), Affiliation::Neutral, Domain::Surface);
    let old = 2.0 * engage::track_sigma_at(&ferry, T0, T0.plus_ms(300_000));
    let new = 2.0 * engage::track_prediction_sigma(&ferry, 300.0, &ec6);
    assert!((old - 3630.0).abs() < 1.0, "{old}");
    assert!((new - 1342.0).abs() < 2.0, "{new}");
    // and it no longer depends on the speed: a 6 m/s tour boat gets the same
    let boat = track(T0, TARGET_AT, Some([-6.0, 0.0]), Affiliation::Neutral, Domain::Surface);
    assert_eq!(engage::track_prediction_sigma(&boat, 300.0, &ec6), engage::track_prediction_sigma(&ferry, 300.0, &ec6));
    // a short prediction stays near the tracker's own uncertainty
    assert!(engage::track_prediction_sigma(&ferry, 10.0, &ec6) < 20.0);
    // an aircraft manoeuvres harder (2 m/s^2): at 60 s, 540 m 1-sigma
    let plane = track(T0, TARGET_AT, Some([55.0, 0.0]), Affiliation::Neutral, Domain::Air);
    assert!((engage::track_prediction_sigma(&plane, 60.0, &ec6) - 540.0).abs() < 2.0);
}

/// `keep_out_tracks` around the target for an effect `span_s` from now, under `version`.
fn tracks_check(w: &WorldSnapshot, span_s: i64, version: u32) -> CheckResult {
    let r = rules(version);
    let ec = r.engage_rules();
    let t = w.t.plus_ms(span_s * 1000);
    let alt = engage::effect_alt(w.tracks.get("T-0001"), w.t, t, 75.0, &ec);
    engage::keep_out_tracks(TARGET_AT, t, 275.0, "T-0001", w, &ec, alt)
}

#[test]
fn a_distant_fast_ferry_no_longer_blocks_but_a_near_one_does() {
    // a 20 m/s ferry whose course passes 2 km from the effect point at the effect time, 300 s
    // out (it is 6 km east of there now)
    let north_2km = LonLat::new(TARGET_AT.lon + 6_000.0 / 87_943.0, TARGET_AT.lat + 2_000.0 / 111_320.0);
    let mut w = world(T0);
    w.tracks.insert("T-0002".into(), track(T0, north_2km, Some([-20.0, 0.0]), Affiliation::Neutral, Domain::Surface));
    assert_eq!(tracks_check(&w, 300, 5).outcome, Outcome::Fail, "rules 5: 3.6 km of 2-sigma");
    let c = tracks_check(&w, 300, 6);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    // the same ferry passing 1 km away still blocks
    let north_1km = LonLat::new(north_2km.lon, TARGET_AT.lat + 1_000.0 / 111_320.0);
    w.tracks.insert("T-0002".into(), track(T0, north_1km, Some([-20.0, 0.0]), Affiliation::Neutral, Domain::Surface));
    let c = tracks_check(&w, 300, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("velocity 300 m, manoeuvre 600 m"), "{}", c.detail);
}

#[test]
fn a_track_without_a_velocity_estimate_blocks_by_its_reach() {
    // fail closed: rules 5 held it where it was (1 m/s of growth); rules 6 lets it be anywhere
    // it reaches at its domain's top speed (25 m/s on the surface)
    let mut w = world(T0);
    let at = LonLat::new(TARGET_AT.lon, TARGET_AT.lat + 2_000.0 / 111_320.0);
    w.tracks.insert("T-0002".into(), track(T0, at, None, Affiliation::Unknown, Domain::Surface));
    assert_eq!(tracks_check(&w, 100, 5).outcome, Outcome::Pass);
    let c = tracks_check(&w, 100, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("no velocity estimate"), "{}", c.detail);
}

// ---- der.3 (c): altitude ---------------------------------------------------------------------

fn friendly_check(w: &WorldSnapshot, span_s: i64, version: u32) -> CheckResult {
    let r = rules(version);
    let t = w.t.plus_ms(span_s * 1000);
    let alt = engage::effect_alt(w.tracks.get("T-0001"), w.t, t, 75.0, &r.engage_rules());
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
    // a MALE aircraft at 7,000 m over a surface effect, 120 s out: clear in 3D (diving 25
    // degrees at its 105 m/s top speed, 44 m/s, it can't get below 7,000 - 2 x 10 - 44 x 120
    // = 1,655 m), blocking in 2D
    let w = above(Some(0.0), "mq9b", Some(7_000.0));
    let c = friendly_check(&w, 120, 6);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    assert!(c.detail.contains("clear by altitude: over-1 at 7000 m"), "{}", c.detail);
    assert_eq!(friendly_check(&w, 120, 5).outcome, Outcome::Fail, "rules 5: 2D");
    // a quadcopter at 120 m still blocks: its vertical reach covers the keep-out above the effect
    let c = friendly_check(&above(Some(0.0), "x10", Some(120.0)), 120, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("within the vertical keep-out too"), "{}", c.detail);
    // the faster of climb and dive counts, either way: a CCA-class aircraft (260 m/s top,
    // 110 m/s diving) at 6,000 m could be down at the effect in 120 s; so could the MQ-9B
    // from 3,000 m (its catalog climb, 10 m/s, would have kept it 1,800 m up)
    assert_eq!(friendly_check(&above(Some(0.0), "cca", Some(6_000.0)), 120, 6).outcome, Outcome::Fail);
    let c = friendly_check(&above(Some(0.0), "mq9b", Some(3_000.0)), 120, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert_eq!(friendly_check(&above(Some(0.0), "mq9b", Some(6_000.0)), 120, 6).outcome, Outcome::Pass);
    let mq9b = fresh(vehicle("mq9b", TARGET_AT), T0);
    let rate = engage::friendly_vertical_mps(&mq9b, &rules(6).engage_rules());
    assert!((rate - 105.0 * 25f64.to_radians().sin()).abs() < 1e-9, "{rate}");
    // unknown altitude, ours or the target's: 2D, no ceiling
    let c = friendly_check(&above(Some(0.0), "mq9b", None), 120, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("its altitude is unknown"), "{}", c.detail);
    assert_eq!(friendly_check(&above(None, "mq9b", Some(7_000.0)), 120, 6).outcome, Outcome::Fail);
    // a stale altitude is not known either
    let mut w = above(Some(0.0), "mq9b", Some(7_000.0));
    w.vehicles.get_mut(&Uid::new("over-1")).unwrap().alt_msl_m = Some(Fact::new(7_000.0, T0.plus_ms(-60_000)));
    assert_eq!(friendly_check(&w, 120, 6).outcome, Outcome::Fail);
    // a protected aircraft track at 3,000 m over the effect, its altitude from radar: clear
    // in 3D; at 450 m: not
    let mut w = world(T0);
    w.tracks.get_mut("T-0001").unwrap().alt_m = Some(0.0);
    let mut plane = track(T0, TARGET_AT, Some([0.0, 0.0]), Affiliation::Neutral, Domain::Air);
    seen_by(&mut plane, &["3:radar", "1:eo_ir"], T0);
    plane.alt_m = Some(3_000.0);
    w.tracks.insert("T-0002".into(), plane.clone());
    let c = tracks_check(&w, 120, 6);
    assert_eq!(c.outcome, Outcome::Pass, "{}", c.detail);
    plane.alt_m = Some(450.0);
    w.tracks.insert("T-0002".into(), plane.clone());
    assert_eq!(tracks_check(&w, 120, 6).outcome, Outcome::Fail);
    plane.alt_m = None;
    w.tracks.insert("T-0002".into(), plane.clone());
    assert_eq!(tracks_check(&w, 120, 6).outcome, Outcome::Fail, "its altitude unknown: 2D");
    // seen by passive sensors only (a bearing, no range), its altitude does not count: 2D
    plane.alt_m = Some(3_000.0);
    seen_by(&mut plane, &["1:eo_ir", "2:esm"], T0);
    w.tracks.insert("T-0002".into(), plane);
    let c = tracks_check(&w, 120, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("altitude is unknown"), "{}", c.detail);
}

#[test]
fn a_stale_radar_altitude_does_not_count_while_eo_ir_keeps_the_position_fresh() {
    // the second review's probe: a neutral aircraft track 100 m from the point, its last
    // radar altitude 1,500 m (300 s ago; truly at 100 m now), EO/IR keeping its position
    // fresh; the effect 60 s out
    let mut w = world(T0);
    w.tracks.get_mut("T-0001").unwrap().alt_m = Some(0.0);
    let near = LonLat::new(TARGET_AT.lon + 100.0 / 87_943.0, TARGET_AT.lat);
    let mut plane = track(T0, near, Some([0.0, 0.0]), Affiliation::Neutral, Domain::Air);
    plane.alt_m = Some(1_500.0);
    seen_by(&mut plane, &["3:radar", "1:eo_ir"], T0);
    w.tracks.insert("T-0002".into(), plane.clone());
    let c = tracks_check(&w, 60, 6);
    assert_eq!(c.outcome, Outcome::Pass, "measured now, it is clear by altitude: {}", c.detail);
    assert!(c.detail.contains("clear by altitude: T-0002 at 1500 m"), "{}", c.detail);
    let ec = rules(6).engage_rules();
    let old = T0.plus_ms(-300_000);
    // the altitude measured 300 s ago: 2D
    plane.alt_t = Some(old);
    plane.source_t.insert("3:radar".into(), old);
    w.tracks.insert("T-0002".into(), plane.clone());
    assert_eq!(engage::track_altitude(&plane, T0, &ec), None);
    let c = tracks_check(&w, 60, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("altitude is unknown"), "{}", c.detail);
    // a recent altitude without a recent ranging hit behind it does not count either
    plane.alt_t = Some(T0);
    w.tracks.insert("T-0002".into(), plane.clone());
    assert_eq!(engage::track_altitude(&plane, T0, &ec), None);
    // within the bound (10 s), it counts, its margin growing from the altitude's time: 8 s
    // old here, so 2 x 30 m + 10 m/s x 68 s = 740 m of altitude uncertainty
    let t8 = T0.plus_ms(-8_000);
    plane.alt_t = Some(t8);
    plane.source_t.insert("3:radar".into(), t8);
    w.tracks.insert("T-0002".into(), plane.clone());
    assert_eq!(engage::track_altitude(&plane, T0, &ec), Some((1_500.0, t8)));
    let c = tracks_check(&w, 60, 6);
    assert!(c.detail.contains("with 740 m of altitude uncertainty"), "{}", c.detail);
    // and the logged facts replay: the bound is the policy's (schema 4 key)
    assert_eq!(ec.track_alt_max_age(), 10.0);
}

#[test]
fn only_a_ranged_altitude_counts_and_a_ground_target_stays_2d() {
    let ec = rules(6).engage_rules();
    let mut t = track(T0, TARGET_AT, Some([0.0, 0.0]), Affiliation::Hostile, Domain::Air);
    t.alt_m = Some(800.0);
    let alt = |t: &TrackFacts| engage::track_altitude(t, T0, &ec).map(|(a, _)| a);
    for (sources, want) in [
        (&["3:radar"][..], Some(800.0)),
        (&["1:eo_ir"][..], None),
        (&["2:esm", "1:eo_ir"][..], None),
        (&["ais"][..], None),
    ] {
        seen_by(&mut t, sources, T0);
        assert_eq!(alt(&t), want, "air {sources:?}");
    }
    t.domain = Domain::Surface;
    for (sources, want) in [(&["ais"][..], Some(800.0)), (&["3:radar"][..], Some(800.0)), (&["1:eo_ir"][..], None)] {
        seen_by(&mut t, sources, T0);
        assert_eq!(alt(&t), want, "surface {sources:?}");
    }
    // sonar's fix is bearing-heavy: no depth counts
    t.domain = Domain::Subsurface;
    seen_by(&mut t, &["4:sonar"], T0);
    assert_eq!(alt(&t), None);
    // an altitude without its time (a log of older facts) does not count
    t.domain = Domain::Air;
    seen_by(&mut t, &["3:radar"], T0);
    t.alt_t = None;
    assert_eq!(alt(&t), None);
    // a ground track's 0 m is not its altitude above sea level (the terrain under it is not
    // modelled): no 3D keep-out, whatever saw it. The reviewer's probe: a ground target
    // logged at 0 m with one of ours 400 m MSL right above it (150 m over the real terrain)
    t.domain = Domain::Ground;
    t.alt_t = Some(T0);
    t.alt_m = Some(0.0);
    assert_eq!(alt(&t), None);
    let mut w = world(T0);
    w.tracks.insert("T-0001".into(), t);
    let mut v = fresh(vehicle("x10", TARGET_AT), T0);
    v.alt_msl_m = Some(Fact::new(400.0, T0));
    w.vehicles.insert(Uid::new("drone-1"), v);
    let c = friendly_check(&w, 0, 6);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(engage::effect_alt(w.tracks.get("T-0001"), T0, T0, 75.0, &ec).is_none());
}

// ---- der.3 (b): lost friendlies ----------------------------------------------------------------

#[test]
fn only_an_operator_accepts_a_lost_friendly_and_only_the_loss_it_names() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    let a1 = op1.engagement_authority().unwrap();
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
    assert!(matches!(e.submit_accepting(a1, nomination(), &wrong, &w, &terrain()), Err(EngineError::NotAccepted { .. })));
    let fine = [(Uid::new("blade-1"), Some(clock.now()))];
    assert!(matches!(e.submit_accepting(a1, nomination(), &fine, &w, &terrain()), Err(EngineError::NotAccepted { .. })));
    // and only on an engagement
    let hold = Proposal::new(Task::Hold {}, vec![Uid::new("blade-1")]);
    let right = [(Uid::new("drone-1"), Some(T0))];
    assert!(matches!(e.submit_accepting(a1, hold, &right, &w, &terrain()), Err(EngineError::NotAccepted { .. })));
    assert_eq!(log.lines().len(), n, "nothing logged");
    // the loss as it is: accepted, logged before the checks with the vehicle's facts, and the
    // keep-out leaves it out
    let i = e.submit_accepting(a1, nomination(), &right, &w, &terrain()).unwrap();
    assert_eq!(i.state(), State::PendingApproval, "{:?}", i.history());
    let c = i.checks().iter().find(|c| c.rule == RuleId::KeepOutFriendly).unwrap();
    assert!(c.detail.contains("position accepted as unknown: drone-1"), "{}", c.detail);
    let id = i.id();
    // the vehicle reports again: it counts like any other (here, clear 2 km away)
    w.vehicles.get_mut(&Uid::new("drone-1")).unwrap().position = Some(Fact::new(lost_at, clock.now()));
    let r = rules(6);
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
    let err = e.accept_unknown_friendly(id, a1, Uid::new("drone-1"), Some(T0), &w2).unwrap_err();
    assert!(matches!(err, EngineError::NotAccepted { .. }), "{err}");
    let rep = replay::replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
    // the acceptance as logged: replay checks the vehicle really was lost since then
    let text = log.text();
    let line = text.lines().find(|l| l.contains("\"type\":\"engage.accepted\"")).unwrap();
    assert!(line.contains("\"world_t\":") && line.contains("\"vehicle\":{"), "{line}");
}

/// The lost-drone world of the acceptance tests at `t`: our quadcopter silent since T0, 2 km
/// east of the target.
fn lost_drone_world(t: Time) -> WorldSnapshot {
    let mut w = world(t);
    let lost_at = LonLat::new(TARGET_AT.lon + 2_000.0 / 87_943.0, TARGET_AT.lat);
    let mut drone = fresh(vehicle("x10", lost_at), T0);
    drone.autopilot = Some(Fact::new(AutopilotKind::Copter, T0));
    w.vehicles.insert(Uid::new("drone-1"), drone);
    w
}

fn engage_nomination() -> Proposal {
    Proposal::new(
        Task::Engage {
            track: "T-0001".into(),
            effect_radius_m: 75.0,
            window: None,
            fallback: Fallback::Return,
        },
        vec![Uid::new("blade-1")],
    )
}

#[test]
fn every_signer_sees_the_acceptances_and_a_token_from_before_one_is_refused() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op1 = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let op2 = e.open_operator_session_with(OperatorId::new("op-2"), Grants { engage: true }).unwrap();
    let op3 = e.open_operator_session_with(OperatorId::new("op-3"), Grants { engage: true }).unwrap();
    let (a2, a3) = (op2.engagement_authority().unwrap(), op3.engagement_authority().unwrap());
    // nominated while the drone still reported (clear 2 km away)
    let id = e.submit(op1.authority(), engage_nomination(), &lost_drone_world(T0), &terrain()).unwrap().id();
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    // a minute later it is lost; op-2 looks first
    clock.advance_ms(60_000);
    let w = lost_drone_world(clock.now());
    let old = e.issue_picture(id, a2, &w).unwrap();
    assert!(old.unknown_accepted.is_empty());
    // op-3 accepts it (with engagement authority: the only way in)
    e.accept_unknown_friendly(id, a3, Uid::new("drone-1"), Some(T0), &w).unwrap();
    // op-2 signs on the token taken before: refused, look again
    let err = e.approve_engagement(id, a2, None, old.id, &w, &terrain()).unwrap_err();
    assert!(err.to_string().contains("changed since you looked: look again"), "{err}");
    // the new token shows the acceptance; signed on it
    let token = e.issue_picture(id, a2, &w).unwrap();
    assert_eq!(token.unknown_accepted.keys().map(Uid::as_str).collect::<Vec<_>>(), ["drone-1"]);
    e.approve_engagement(id, a2, None, token.id, &w, &terrain()).unwrap();
    clock.advance_ms(1000);
    let w = lost_drone_world(clock.now());
    let token = e.issue_picture(id, a3, &w).unwrap();
    assert_eq!(token.unknown_accepted.len(), 1);
    e.approve_engagement(id, a3, None, token.id, &w, &terrain()).unwrap();
    assert_eq!(e.intent(id).unwrap().state(), State::Approved);
    // any operator withdraws it: the held picture counts the drone again and voids
    e.revoke_unknown_friendly(id, op1.authority(), Uid::new("drone-1")).unwrap();
    assert!(e.intent(id).unwrap().engagement().unwrap().accepted.is_empty());
    assert!(e.revoke_unknown_friendly(id, op1.authority(), Uid::new("drone-1")).is_err(), "not in force");
    clock.advance_ms(1000);
    let out = e.step_engagements(&lost_drone_world(clock.now()), &terrain());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let i = e.intent(id).unwrap();
    assert!(matches!(i.state(), State::Voided | State::Aborted), "{:?}", i.state());
    let rep = replay::replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
    // a forged token without the acceptance, or an acceptance under a session without
    // engagement authority, is caught by replay
    let lines: Vec<String> = log.lines();
    let forged: Vec<String> = lines
        .iter()
        .map(|l| {
            if l.contains("\"type\":\"engage.accepted\"") {
                l.replacen(&format!("\"session\":{}", op3.authority().session().0), &format!("\"session\":{}", op1.authority().session().0), 1)
                    .replacen("\"by\":\"op-3\"", "\"by\":\"op-1\"", 1)
            } else {
                l.clone()
            }
        })
        .collect();
    let w = whats(&rechain(&forged));
    assert!(w.iter().any(|m| m.contains("without engagement authority")), "{w:?}");
}

#[test]
fn replay_checks_the_vehicle_was_lost_since_the_acceptance_says() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(policy(), Some(areas()), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let op1 = e.open_operator_session_with(OperatorId::new("op-1"), Grants { engage: true }).unwrap();
    clock.advance_ms(60_000);
    let w = lost_drone_world(clock.now());
    e.submit_accepting(op1.engagement_authority().unwrap(), engage_nomination(), &[(Uid::new("drone-1"), Some(T0))], &w, &terrain())
        .unwrap();
    assert!(replay::replay_str(&log.text()).unwrap().ok());
    // the vehicle's logged report moved to just now: it was not lost
    let now = clock.now().0;
    let forged: Vec<String> = log
        .lines()
        .iter()
        .map(|l| {
            if l.contains("\"type\":\"engage.accepted\"") {
                l.replace(&format!("\"t\":{}", T0.0), &format!("\"t\":{now}"))
            } else {
                l.clone()
            }
        })
        .collect();
    let w = whats(&rechain(&forged));
    assert!(w.iter().any(|m| m.contains("not lost since that report")), "{w:?}");
}

#[test]
fn policy_keys_of_rules_6_are_bounded_and_of_schema_4() {
    let mut e = rules(6).engage_rules();
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
    // the shipped file sets them, each a schema 4 key
    let shipped = rules(6).engage_rules();
    let keys = shipped.schema_keys();
    for k in ["track_velocity_sigma_mps", "track_accel_mps2", "vertical_rate_mps", "keep_out_vertical_m", "track_alt_max_age_s"] {
        assert!(keys.contains(&(4, k)), "{k}: {keys:?}");
    }
    assert_eq!(shipped.track_accel(Domain::Surface), 0.2);
    assert_eq!(shipped.keep_out_vertical(), 200.0);
}
