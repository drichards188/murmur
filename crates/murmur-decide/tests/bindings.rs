//! Replay binds what was executed to what was approved (sixth review). The reviewer forged
//! logs whose hash chain verifies and whose every decision reproduces, but whose orders were
//! not the ones approved: N2, an engagement's effect released by another vehicle; N3, an
//! engagement dispatched to another vehicle; v1, an approved armed orbit dispatched to
//! another vehicle; v2, that orbit's centre moved about 2 km. Each is a regression here, on
//! committed logs (rules 1 to 3: the bindings are audit checks, not decision rules, and hold
//! for every version), beside a test for each binding: an `intent.dispatched` is exactly the
//! approved intent's order; an effect comes from the dispatched platform, at the frozen
//! plan's point and radius, inside its time tolerance; op states refer to a dispatched
//! intent and its own vehicles; a superseding order and a reported fallback are what the
//! stop says.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{ReplayOptions, ReplayReport, RulesAccepted, replay_str_with};
use murmur_decide::scenario::{self, RunOptions};
use murmur_decide::*;

/// Rules 3: a scenario's engagement, released by blade-1 at its plan's point.
const SCENARIO_V3: &str = "v3-engagement-scenario-01.jsonl";
/// Rules 1: a live run before Phase E; intent 2 is an armed orbit on vehicle 6, sent to a
/// human by the armed-platform tier, approved by op-2 and dispatched.
const LIVE_V1: &str = "v1-pre-engagement-live-c7.jsonl";
/// Rules 2: a live engagement aborted when its picture changed, its fallback (return)
/// commanded and reported.
const LIVE_V2: &str = "v2-engagement-live.jsonl";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/audit")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Replay accepting exactly the log's own rules version.
fn replay_as(text: &str, version: u32) -> ReplayReport {
    replay_str_with(
        text,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(version),
            ..ReplayOptions::default()
        },
    )
    .unwrap()
}

fn whats(r: &ReplayReport) -> Vec<&str> {
    r.mismatches.iter().map(|m| m.what.as_str()).collect()
}

/// The honest log replays; the forged one (chain recomputed) does not, and says `want`.
fn caught(honest: &str, forged: &str, version: u32, want: &str) -> ReplayReport {
    let r = replay_as(honest, version);
    assert!(r.ok(), "the honest log: {:#?}", r.mismatches);
    assert_ne!(honest, forged, "nothing was forged");
    let r = replay_as(forged, version);
    assert!(!r.ok(), "the forged log passed replay");
    assert!(
        r.mismatches.iter().all(|m| !m.what.contains("hash chain")),
        "the forger recomputed the chain: {:#?}",
        r.mismatches
    );
    assert!(whats(&r).contains(&want), "{want:?} not in {:#?}", r.mismatches);
    r
}

fn edit_each(events: &mut [Event], mut f: impl FnMut(&mut EventBody)) {
    for e in events {
        f(&mut e.body);
    }
}

#[test]
fn the_forger_reproduces_an_untouched_log_byte_for_byte() {
    for name in [SCENARIO_V3, LIVE_V1, LIVE_V2] {
        let text = fixture(name);
        assert_eq!(forge(&text, |_| {}), text, "{name}");
    }
}

// ---- the reviewer's forged logs ---------------------------------------------------------------

#[test]
fn n2_an_effect_released_by_another_vehicle_fails_replay() {
    let honest = fixture(SCENARIO_V3);
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::EngageEffect { uid, .. } = b {
                *uid = Uid::new("blade-9");
            }
        })
    });
    let r = caught(
        &honest,
        &forged,
        3,
        "effect released by another vehicle than the dispatched engagement's platform",
    );
    assert_eq!(r.mismatches.len(), 1, "{:#?}", r.mismatches);
    assert_eq!((r.mismatches[0].logged.as_str(), r.mismatches[0].recomputed.as_str()), ("blade-9", "\"blade-1\""));
}

#[test]
fn n3_an_engagement_dispatched_to_another_vehicle_fails_replay() {
    // the dispatch's vehicles and its engagement order's platform both say blade-9, at the
    // signed point
    let honest = fixture(SCENARIO_V3);
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentDispatched {
                uids,
                engage: Some(o),
                ..
            } = b
            {
                *uids = vec![Uid::new("blade-9")];
                o.uid = Uid::new("blade-9");
            }
        })
    });
    let r = caught(
        &honest,
        &forged,
        3,
        "dispatched to other vehicles than the approved intent's",
    );
    assert!(
        whats(&r).contains(&"engagement order sends another platform than the approved nomination's"),
        "{:#?}",
        r.mismatches
    );
}

/// The live log's approved armed orbit (intent 2, on vehicle 6).
fn armed_orbit(text: &str) -> IntentId {
    let events = parse_log(text).unwrap();
    let approved = |id: IntentId| {
        events.iter().any(|e| {
            matches!(&e.body, EventBody::ApprovalGranted { intent, .. } if *intent == id)
        }) && events.iter().any(|e| {
            matches!(&e.body, EventBody::IntentDispatched { intent, .. } if *intent == id)
        })
    };
    let id = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::PolicyDecision {
                intent, decision, ..
            } if decision.rule == "armed-platform" && approved(*intent) => Some(*intent),
            _ => None,
        })
        .unwrap();
    assert_eq!(id, IntentId(2));
    id
}

#[test]
fn v1_an_approved_armed_orbit_dispatched_to_another_vehicle_fails_replay() {
    let honest = fixture(LIVE_V1);
    let id = armed_orbit(&honest);
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentDispatched { intent, uids, .. } = b
                && *intent == id
            {
                // vehicle 9 is in the fleet (intent 4 names it)
                *uids = vec![Uid::new("9")];
            }
        })
    });
    let r = caught(
        &honest,
        &forged,
        1,
        "dispatched to other vehicles than the approved intent's",
    );
    assert_eq!(whats(&r).len(), 1, "{:#?}", r.mismatches);
}

#[test]
fn v2_an_approved_armed_orbit_dispatched_with_its_centre_moved_fails_replay() {
    let honest = fixture(LIVE_V1);
    let id = armed_orbit(&honest);
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentDispatched {
                intent,
                task: Task::Orbit { center, .. },
                ..
            } = b
                && *intent == id
            {
                // about 2 km north
                *center = LonLat::new(center.lon, center.lat + 0.018);
            }
        })
    });
    let r = caught(
        &honest,
        &forged,
        1,
        "dispatched task differs from the approved intent's",
    );
    assert_eq!(whats(&r).len(), 1, "{:#?}", r.mismatches);
}

/// An agent's orbit on the armed switchblade, approved by op-1 in the inbox, dispatched,
/// reported and done, then another vehicle's event (the current rules).
fn approved_armed_orbit() -> (String, IntentId) {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    let (w, tr) = (world(), terrain());
    let id = e
        .agent_port(AgentId::new("planner"))
        .propose(
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            rationale("cover the LZ"),
            &w,
            &tr,
        )
        .unwrap()
        .id();
    assert_eq!(e.intent(id).unwrap().policy().unwrap().rule, "armed-platform");
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    clock.advance_ms(1_000);
    e.approve(id, op.authority(), None, &w, &tr).unwrap();
    e.dispatch(id).unwrap();
    e.record_op_state(id, &uid("blade-1"), "active", "on station")
        .unwrap();
    e.finish(id, TaskOutcome::Done).unwrap();
    e.record_vehicle_event(&uid("sky-1"), "mode", "RTL").unwrap();
    (log.text(), id)
}

#[test]
fn the_same_forgeries_fail_under_the_current_rules() {
    let (honest, _) = approved_armed_orbit();
    let to_sky = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentDispatched { uids, .. } = b {
                *uids = vec![uid("sky-1")];
            }
        })
    });
    caught(
        &honest,
        &to_sky,
        RULES_VERSION,
        "dispatched to other vehicles than the approved intent's",
    );
    let moved = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentDispatched {
                task: Task::Orbit { center, radius_m },
                ..
            } = b
            {
                *center = LonLat::new(center.lon, center.lat + 0.018);
                *radius_m = 400.0;
            }
        })
    });
    caught(
        &honest,
        &moved,
        RULES_VERSION,
        "dispatched task differs from the approved intent's",
    );
}

// ---- intent.dispatched ------------------------------------------------------------------------

#[test]
fn a_dispatch_needs_its_order_and_an_order_its_dispatch() {
    let (honest, id) = approved_armed_orbit();
    let v = RULES_VERSION;
    // the order dropped: the dispatch transition is unbacked
    let forged = forge(&honest, |ev| {
        ev.retain(|e| !matches!(e.body, EventBody::IntentDispatched { .. }))
    });
    caught(&honest, &forged, v, "dispatched without its intent.dispatched order");
    // an order for an intent that does not exist (and none for the one dispatched)
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentDispatched { intent, .. } = b {
                *intent = IntentId(99);
            }
        })
    });
    let r = caught(&honest, &forged, v, "dispatch of an unknown intent");
    assert!(whats(&r).contains(&"dispatched without its intent.dispatched order"));
    // the order released, then the intent voided instead of dispatched
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentState {
                intent,
                transition,
                to,
                ..
            } = b
                && *intent == id
                && *transition == Transition::Dispatch
            {
                *transition = Transition::Void;
                *to = State::Voided;
            }
        })
    });
    caught(
        &honest,
        &forged,
        v,
        "intent.dispatched followed by another transition than its dispatch",
    );
    // the order released and the intent never dispatched (its later events gone)
    let forged = forge(&honest, |ev| {
        let at = ev
            .iter()
            .position(|e| matches!(e.body, EventBody::IntentDispatched { .. }))
            .unwrap();
        let mut n = 0;
        ev.retain(|e| {
            n += 1;
            n <= at + 1 || !matches!(&e.body, EventBody::IntentState { intent, .. } | EventBody::OpState { intent, .. } if *intent == id)
        });
    });
    caught(
        &honest,
        &forged,
        v,
        "intent.dispatched never followed by its dispatch transition",
    );
}

#[test]
fn an_engagement_order_is_the_frozen_plan_on_the_nominated_platform() {
    let honest = fixture(SCENARIO_V3);
    type Edit = fn(&mut Option<EngageOrder>);
    let cases: [(Edit, &str); 5] = [
        (
            |o| o.as_mut().unwrap().track = "T-0002".into(),
            "engagement order names another intent or track than the approved one",
        ),
        (
            |o| o.as_mut().unwrap().intent = IntentId(7),
            "engagement order names another intent or track than the approved one",
        ),
        (
            |o| {
                let o = o.as_mut().unwrap();
                o.window_until = o.window_until.plus_ms(60_000);
            },
            "engagement order's window differs from the approved plan's",
        ),
        (
            |o| o.as_mut().unwrap().fallback = Fallback::Abort,
            "engagement dispatched to something other than its approval's snapshot",
        ),
        (|o| *o = None, "engagement dispatched without its order"),
    ];
    for (edit, want) in cases {
        let forged = forge(&honest, |ev| {
            edit_each(ev, |b| {
                if let EventBody::IntentDispatched { engage, .. } = b {
                    edit(engage);
                }
            })
        });
        caught(&honest, &forged, 3, want);
    }
}

// ---- engage.effect ----------------------------------------------------------------------------

#[test]
fn an_effect_logged_before_the_plans_time_fails_replay() {
    // the releasing picture and the effect moved back to 105.000 s: the plan's time is
    // 105.113 s, and a holding target is released from it, never before
    let honest = fixture(SCENARIO_V3);
    let forged = forge(&honest, |ev| {
        let eff = ev
            .iter()
            .position(|e| matches!(e.body, EventBody::EngageEffect { .. }))
            .unwrap();
        let pic = (0..eff)
            .rev()
            .find(|&i| matches!(ev[i].body, EventBody::EngagePicture { .. }))
            .unwrap();
        let EventBody::EngageEffect { planned_t, .. } = ev[eff].body else {
            unreachable!()
        };
        assert_eq!(planned_t, Time(1_781_000_105_113));
        for i in [pic, eff] {
            ev[i].t = Time(1_781_000_105_000);
        }
    });
    caught(
        &honest,
        &forged,
        3,
        "effect released outside the plan's time tolerance",
    );
}

#[test]
fn the_release_window_is_the_plans_time_tolerance() {
    let events = parse_log(&fixture(SCENARIO_V3)).unwrap();
    let mut preview = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::ApprovalGranted {
                snapshot: Some(s), ..
            } => Some(s.preview.clone()),
            _ => None,
        })
        .unwrap();
    let ec = policy().rules.engage_rules();
    // a holding target: from the plan's time, until late_s after it
    let (from, until) = engage::release_window(&preview, &ec);
    assert_eq!(from, preview.effect_t);
    assert_eq!(until.ms_since(preview.effect_t), (ec.late_s * 1000.0) as i64);
    // a moving one: from plan_slack_s before it
    preview.track_speed_mps = 8.0;
    let (from, _) = engage::release_window(&preview, &ec);
    let slack = engage::plan_slack_s(&preview, &ec);
    assert!(slack > 0.0);
    assert_eq!(preview.effect_t.ms_since(from), (slack * 1000.0).round() as i64);
    // never before the window opens, never after it closes
    preview.window.from = Some(preview.effect_t);
    preview.window.until = preview.effect_t.plus_ms(5_000);
    assert_eq!(
        engage::release_window(&preview, &ec),
        (preview.effect_t, preview.effect_t.plus_ms(5_000))
    );
}

#[test]
fn an_engagement_ends_by_its_assessment_or_an_abort_not_as_a_failed_task() {
    let honest = fixture(SCENARIO_V3);
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::IntentState { transition, to, .. } = b
                && *transition == Transition::Complete
            {
                *transition = Transition::Fail;
                *to = State::Failed;
            }
        })
    });
    caught(
        &honest,
        &forged,
        3,
        "engagement failed like a task (it ends by its assessment or an abort)",
    );
}

// ---- op.state ---------------------------------------------------------------------------------

#[test]
fn op_states_refer_to_a_dispatched_intent_and_its_own_vehicles() {
    // the rules 1 live log: intent 1, an orbit on vehicle 5, reports sent, active, done;
    // intent 3, a patrol on 11, was rejected
    let honest = fixture(LIVE_V1);
    let first_report = |ev: &[Event]| {
        ev.iter()
            .position(|e| matches!(&e.body, EventBody::OpState { intent, .. } if *intent == IntentId(1)))
            .unwrap()
    };
    let forged = forge(&honest, |ev| {
        let i = first_report(ev);
        if let EventBody::OpState { uid, .. } = &mut ev[i].body {
            *uid = Uid::new("9");
        }
    });
    caught(
        &honest,
        &forged,
        1,
        "op state for a vehicle or resource the intent was not dispatched to",
    );
    // intent 1's last report ("done") credited to intent 3, rejected by then
    let forged = forge(&honest, |ev| {
        let i = ev
            .iter()
            .rposition(|e| matches!(&e.body, EventBody::OpState { intent, .. } if *intent == IntentId(1)))
            .unwrap();
        if let EventBody::OpState { intent, uid, .. } = &mut ev[i].body {
            *intent = IntentId(3);
            *uid = Uid::new("11");
        }
    });
    caught(
        &honest,
        &forged,
        1,
        "op state for an intent that was never dispatched",
    );
    let forged = forge(&honest, |ev| {
        let i = first_report(ev);
        if let EventBody::OpState { intent, .. } = &mut ev[i].body {
            *intent = IntentId(42);
        }
    });
    caught(&honest, &forged, 1, "op state for an unknown intent");
    // a report after the task ended: its outcome is final
    let forged = forge(&honest, |ev| {
        let done = ev
            .iter()
            .position(|e| {
                matches!(&e.body, EventBody::IntentState { intent, transition: Transition::Complete, .. } if *intent == IntentId(1))
            })
            .unwrap();
        let mut late = ev[first_report(ev)].clone();
        late.t = ev[done].t;
        ev.insert(done + 1, late);
    });
    caught(&honest, &forged, 1, "op state for a task that already ended");
}

#[test]
fn a_reported_fallback_is_the_one_the_stop_released() {
    // the rules 2 live log: the stop released `return`, reported as "return commanded (rtl)"
    let honest = fixture(LIVE_V2);
    assert!(honest.contains("\"state\":\"fallback\",\"detail\":\"return commanded (rtl): "));
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::OpState { state, detail, .. } = b
                && state == "fallback"
            {
                *detail = detail.replacen("return commanded (rtl)", "abort commanded (hold)", 1);
            }
        })
    });
    caught(
        &honest,
        &forged,
        2,
        "fallback reported is not the one its engagement's stop released",
    );
    // a stop that released none
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::EngageStopped { fallback, .. } = b {
                *fallback = None;
            }
        })
    });
    caught(
        &honest,
        &forged,
        2,
        "fallback reported for an engagement no stop released a fallback for",
    );
    // the intercept reported on another vehicle
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::OpState { state, uid, .. } = b
                && state.starts_with("intercept")
            {
                *uid = Uid::new("1");
            }
        })
    });
    caught(
        &honest,
        &forged,
        2,
        "op state for a vehicle or resource the intent was not dispatched to",
    );
}

#[test]
fn the_engine_refuses_op_states_replay_would_reject() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let (w, tr) = (world(), terrain());
    let id = e
        .submit(op.authority(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &tr)
        .unwrap()
        .id();
    assert_eq!(e.intent(id).unwrap().state(), State::AutoApproved);
    let refused = |r: Result<(), EngineError>| matches!(r, Err(EngineError::NotItsOrder { .. }));
    let before = log.lines().len();
    assert!(refused(e.record_op_state(id, &uid("sky-1"), "sent", "")));
    e.dispatch(id).unwrap();
    let after_dispatch = log.lines().len();
    assert!(refused(e.record_op_state(id, &uid("hawk-1"), "sent", "")));
    assert_eq!(log.lines().len(), after_dispatch, "nothing logged");
    e.record_op_state(id, &uid("sky-1"), "done", "on station")
        .unwrap();
    e.finish(id, TaskOutcome::Done).unwrap();
    assert!(refused(e.record_op_state(id, &uid("sky-1"), "active", "")));
    assert_eq!(after_dispatch - before, 2, "dispatched and its transition only");
    let r = replay::replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
}

// ---- engage.stopped ---------------------------------------------------------------------------

#[test]
fn a_superseding_order_is_dispatched_on_the_platform_from_its_own_source() {
    // scenario 29: op-1 approves the agent's hold on blade-1 (acknowledging the engagement),
    // its dispatch supersedes the engagement
    let f = root().join("sim/scenarios/engage/29-approve-order-on-engaging-platform.toml");
    let honest = scenario::run_file(&f, &root(), &RunOptions::default())
        .unwrap()
        .log;
    assert!(honest.contains("\"kind\":\"superseded\""));
    let v = RULES_VERSION;
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::EngageStopped {
                cause: StopCause::Superseded { source, .. },
                ..
            } = b
            {
                *source = Source::Human {
                    id: OperatorId::new("op-1"),
                };
            }
        })
    });
    caught(
        &honest,
        &forged,
        v,
        "superseded with another source than its superseding intent's",
    );
    let forged = forge(&honest, |ev| {
        edit_each(ev, |b| {
            if let EventBody::EngageStopped {
                intent,
                cause: StopCause::Superseded { by, .. },
                ..
            } = b
            {
                *by = *intent;
            }
        })
    });
    caught(&honest, &forged, v, "an engagement can't supersede itself");
    // the stop (and its void) moved before the superseding order's dispatch
    let forged = forge(&honest, |ev| {
        let stop = ev
            .iter()
            .position(|e| matches!(e.body, EventBody::EngageStopped { .. }))
            .unwrap();
        let order = ev
            .iter()
            .position(|e| matches!(e.body, EventBody::IntentDispatched { .. }))
            .unwrap();
        let moved: Vec<Event> = ev.drain(stop..stop + 2).collect();
        for (k, e) in moved.into_iter().enumerate() {
            ev.insert(order + k, e);
        }
    });
    caught(
        &honest,
        &forged,
        v,
        "superseded by an order that was not dispatched",
    );
}
