//! Decision rules 34, stage 4 (docs/SWARMS.md section 4.3): a group in force re-forms itself.
//! Beside the scenarios (166 to 169): each re-form tamper is caught for its own reason; a re-form
//! is never due on fewer than the material drops, before they settle, or within a minute of the
//! last; and under rules 33 the group step does nothing.

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
