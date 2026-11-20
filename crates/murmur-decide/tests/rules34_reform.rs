//! Decision rules 34, stage 4 (docs/SWARMS.md section 4.3): a group in force re-forms itself.
//! Beside the scenarios (166 to 169): each re-form tamper is caught for its own reason; a re-form
//! is never due on fewer than the material drops, before they settle, or within a minute of the
//! last; a member order whose delivery found its vehicle not running it goes back to the queue
//! under its group's approval, never to a person (bd murmur-4jlq); and under rules 33 the group
//! step does nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use murmur_decide::group::{self, Cause, Due, Force, GroupSpec};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scenario(name: &str) -> scenario::ScenarioResult {
    let path = root().join("sim/scenarios/engage").join(name);
    scenario::run_file(&path, &root(), &scenario::RunOptions::default()).unwrap()
}

/// Each re-form tamper is caught, and first for its own reason: a re-form whose facts show no
/// drop (`forge_reform`, 166) by the drops replay recomputes from them; a re-formed share outside
/// its band (`reform_outside_envelope`, 169) by the layout replay lays out again.
#[test]
fn each_reform_tamper_is_caught_for_its_own_reason() {
    for (file, tamper, why) in [
        ("166-group-reforms-after-three-drop.toml", "forge_reform", "a group step's drops are not what its facts show"),
        (
            "169-group-member-back-rejoins-at-the-next-reform.toml",
            "reform_outside_envelope",
            "a re-form's layout differs from the one its facts give",
        ),
    ] {
        let r = scenario(file);
        assert!(r.ok, "{file}: {:?}", r.failures);
        assert_eq!(r.tamper.as_deref(), Some(tamper));
        assert_eq!(r.tamper_detected, Some(true), "{file}");
        let first = r.tamper_mismatches.first().cloned().unwrap_or_default();
        assert!(first.contains(why), "{file}: the first mismatch is {first:?}, not {why:?}");
    }
}

fn force_of(n: usize) -> (Force, GroupSpec) {
    let gs = GroupSpec::default().normalized(TaskKind::Orbit, n, group::MIN_SHARE);
    let mut f = Force::default();
    for i in 0..n {
        f.members.insert(uid(&format!("sky-{i}")), IntentId(i as u64 + 2));
    }
    (f, gs)
}

fn dropped(uids: &[&str], at: Time) -> Cause {
    Cause {
        dropped: uids
            .iter()
            .map(|u| {
                (
                    uid(u),
                    group::Drop {
                        why: "silent: no position for 30 s".into(),
                        at,
                    },
                )
            })
            .collect(),
        back: BTreeMap::new(),
    }
}

/// When a re-form is due (section 4.3's terms): material (at least two, or 5 % of the
/// selection), settled (no drop for 15 s), at most once a minute, and not on the cause last
/// refused; below the minimum, once settled, suspended instead.
#[test]
fn a_reform_is_due_only_on_material_settled_drops_once_a_minute() {
    let (mut f, gs) = force_of(100);
    let t = T0.plus_ms(600_000);
    let settled = t.plus_ms(-group::SETTLE_S * 1000);
    // four of a hundred: not material (five are)
    let four = dropped(&["sky-1", "sky-2", "sky-3", "sky-4"], settled);
    assert_eq!(group::due(&f, &gs, &four, 96, 0, 100, t), Due::Nothing);
    let five = dropped(&["sky-1", "sky-2", "sky-3", "sky-4", "sky-5"], settled);
    assert_eq!(group::due(&f, &gs, &five, 95, 0, 100, t), Due::Reform);
    // not settled: the last drop 14 s ago
    let fresh = dropped(&["sky-1", "sky-2", "sky-3", "sky-4", "sky-5"], t.plus_ms(-14_000));
    assert_eq!(group::due(&f, &gs, &fresh, 95, 0, 100, t), Due::Nothing);
    // within a minute of the last attempt
    f.last_attempt = Some(t.plus_ms(-59_000));
    assert_eq!(group::due(&f, &gs, &five, 95, 0, 100, t), Due::Nothing);
    f.last_attempt = Some(t.plus_ms(-60_000));
    assert_eq!(group::due(&f, &gs, &five, 95, 0, 100, t), Due::Reform);
    // the cause last refused is not tried again; a new drop is
    f.refused = Some(five.uids());
    assert_eq!(group::due(&f, &gs, &five, 95, 0, 100, t), Due::Nothing);
    let six = dropped(&["sky-1", "sky-2", "sky-3", "sky-4", "sky-5", "sky-6"], settled);
    assert_eq!(group::due(&f, &gs, &six, 94, 0, 100, t), Due::Reform);
    // twenty-one of a hundred, the minimum eighty: suspended, not re-formed; a member a newer
    // order took lowers the minimum by one
    let many: Vec<String> = (0..21).map(|i| format!("sky-{i}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let below = dropped(&many, settled);
    assert_eq!(group::due(&f, &gs, &below, 79, 0, 100, t), Due::Suspend { strength: 79, min: 80 });
    assert_eq!(group::due(&f, &gs, &below, 79, 1, 100, t), Due::Reform);
    // suspended: nothing more, ever
    f.suspended = true;
    assert_eq!(group::due(&f, &gs, &six, 94, 0, 100, t), Due::Nothing);
    // a hold or an rtl never re-forms
    let (f, _) = force_of(10);
    let hold = GroupSpec::default().normalized(TaskKind::Hold, 10, group::MIN_SHARE);
    assert_eq!(group::due(&f, &hold, &dropped(&["sky-1", "sky-2"], settled), 8, 0, 10, t), Due::Nothing);
}

/// The re-form's own layout keeps a pinned member's slot even on a gap's edge, and closes the gap
/// from its other side (a window anchored on the pinned member): the stage 2 window counted it
/// among those it moves and gave up on every gap once one bordered a pinned member.
#[test]
fn a_gap_beside_a_pinned_member_is_closed_from_its_other_side() {
    let r = scenario("166-group-reforms-after-three-drop.toml");
    assert!(r.ok, "{:?}", r.failures);
    assert!(r.groups.iter().any(|g| g.ends_with(r#"re-formed 1: moved ["sky-18", "sky-2", "sky-8"]"#)), "{:?}", r.groups);
}

/// bd murmur-4jlq: a member order whose start went out before its link was lost goes back to the
/// queue; its next delivery only confirms it there; when the vehicle does not run it, the member
/// order goes back to the queue again under its group's approval (never to a person on its own),
/// its next delivery a whole one, re-checked first. Replay holds the engine to it.
#[test]
fn a_member_found_not_running_is_requeued_under_its_groups_approval() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut p = policy();
    p.rules.rules_version = GROUP_ORDER_SINCE;
    let mut e = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let world_at = |t: Time| {
        let mut w = WorldSnapshot::new(t);
        w.areas = Some(areas());
        for i in 1..=3 {
            let mut v = vehicle("x10", LonLat::new(-122.4460 + 0.0004 * f64::from(i), 37.8068));
            if let Some(f) = v.nav_ready.as_mut() {
                f.t = t;
            }
            if let Some(f) = v.autopilot.as_mut() {
                f.t = t;
            }
            if let Some(f) = v.position.as_mut() {
                f.t = t;
            }
            if let Some(f) = v.battery_pct.as_mut() {
                f.t = t;
            }
            v.report_interval_ms = Some(1_000);
            w.vehicles.insert(uid(&format!("sky-{i}")), v);
        }
        w
    };
    let w = world_at(clock.now());
    let t = terrain();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let gid = e
        .submit(s.authority(), orbit(LonLat::new(-122.4700, 37.8150), 150.0, &["sky-1", "sky-2", "sky-3"]), &w, &t)
        .unwrap()
        .id();
    let routed = e.dispatch_group(gid, &w).unwrap();
    let mid = routed[0].0;
    let sky = e.intent(mid).unwrap().spec().uids[0].clone();
    e.record_op_state(mid, &sky, "sent", "uploading").unwrap();
    e.record_op_state_ahead(mid, &sky, "starting mission: mission mode commanded").unwrap();
    e.record_op_state(mid, &sky, "unconfirmed", "link lost waiting for the MAV_CMD_DO_SET_MODE ACK, after its start went out")
        .unwrap();
    e.requeue(mid).unwrap();
    assert_eq!(e.intent(mid).unwrap().state(), State::Queued);
    clock.advance_ms(20_000);
    let w = world_at(clock.now());
    assert!(matches!(e.dequeue(mid, &w, &t).unwrap(), Dequeued::Dispatched(_)));
    assert!(e.intent(mid).unwrap().confirming().contains(&sky), "only confirmed");
    e.record_op_state(mid, &sky, "unconfirmed", "it may have started before its link was lost, and does not run it now (mode RTL, mission still held): not started again; a human decides")
        .unwrap();
    e.requeue(mid).unwrap();
    let m = e.intent(mid).unwrap();
    assert_eq!(m.state(), State::Queued, "back to the queue, not to a person");
    assert!(m.maybe_started().is_empty(), "it does not run there: its next delivery is a whole one");
    assert!(e.inbox().is_empty(), "no member order in the inbox");
    let why = m.history().last().map(|h| h.reason.clone()).unwrap_or_default();
    assert!(why.contains("under its group's approval") && why.contains("mode RTL"), "{why}");
    clock.advance_ms(5_000);
    let w = world_at(clock.now());
    assert!(matches!(e.dequeue(mid, &w, &t).unwrap(), Dequeued::Dispatched(_)));
    assert!(e.intent(mid).unwrap().confirming().is_empty(), "a whole delivery");
    let r = replay::replay_str_with(&log.text(), &replay::ReplayOptions { rules: replay::RulesAccepted::Exactly(GROUP_ORDER_SINCE), ..Default::default() }).unwrap();
    assert!(r.mismatches.is_empty(), "{:#?}", r.mismatches);
    // replay holds it to that: the second requeue sent to a person instead
    let text = forge(&log.text(), |ev| {
        let mut n = 0;
        for e in ev.iter_mut() {
            if let audit::EventBody::IntentRequeued { inbox, .. } = &mut e.body {
                n += 1;
                if n == 2 {
                    *inbox = true;
                }
            }
        }
    });
    let r = replay::replay_str_with(&text, &replay::ReplayOptions { rules: replay::RulesAccepted::Exactly(GROUP_ORDER_SINCE), ..Default::default() }).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("requeue deadline differs")), "{:#?}", r.mismatches);
}

/// Under rules 33 the group step does nothing (there are no group orders to step).
#[test]
fn under_older_rules_the_group_step_does_nothing() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    assert!(!e.policy().rules.group_orders());
    let before = log.text().len();
    let step = e.step_groups(&world(), &terrain()).unwrap();
    assert_eq!(step, engine::GroupStep::default());
    assert_eq!(log.text().len(), before, "nothing logged");
}
