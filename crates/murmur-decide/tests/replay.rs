//! Replay: a generated log reproduces every decision; tampering is detected.
#![allow(clippy::unwrap_used)]

mod common;

use std::process::Command;

use common::*;
use murmur_decide::replay::{replay, replay_str};
use murmur_decide::*;
use serde_json::Value;

/// A run that exercises every decision path. Returns the log text.
fn scenario() -> String {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let a = op.authority();
    let w = world();
    let tr = terrain();

    // auto, dispatched, done
    let i1 = e
        .submit(a, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w, &tr)
        .unwrap()
        .id();
    e.dispatch(i1).unwrap();
    e.record_op_state(i1, &uid("sky-1"), "active", "on station")
        .unwrap();
    e.finish(i1, TaskOutcome::Done).unwrap();

    // rejected by rules (with a suggested fix), and a domain failure
    e.submit(a, orbit(NEAR_CRISSY, 250.0, &["hawk-1"]), &w, &tr)
        .unwrap();
    e.submit(a, orbit(NEAR_CRISSY, 100.0, &["boat-1"]), &w, &tr)
        .unwrap();

    // boat on water with terrain sampled, and with no oracle (not evaluated)
    e.submit(a, orbit(ON_WATER, 200.0, &["boat-1"]), &w, &tr)
        .unwrap();
    e.submit(a, orbit(ON_WATER, 200.0, &["boat-1"]), &w, &NoTerrain)
        .unwrap();

    // agent nominates on an armed platform, human approves after a while
    clock.advance_ms(2_000);
    let i2 = e
        .agent_port(AgentId::new("planner"))
        .propose(
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            rationale("cover the LZ"),
            &w,
            &tr,
        )
        .unwrap()
        .id();
    clock.advance_ms(30_000);
    let mut w_later = w.clone();
    w_later.t = T0.plus_ms(32_000);
    for v in w_later.vehicles.values_mut() {
        v.nav_ready.as_mut().unwrap().t = w_later.t;
        v.battery_pct.as_mut().unwrap().t = w_later.t;
        v.position.as_mut().unwrap().t = w_later.t;
    }
    e.approve(i2, a, Some("picture holds".into()), &w_later, &tr)
        .unwrap();
    e.dispatch(i2).unwrap();
    e.finish(
        i2,
        TaskOutcome::Failed {
            detail: "mission rejected".into(),
        },
    )
    .unwrap();

    // human rejects; leaves approved area; expires; voided at approval; withdrawn
    let i3 = e
        .submit(a, orbit(NEAR_CRISSY, 150.0, &["blade-1"]), &w_later, &tr)
        .unwrap()
        .id();
    e.reject(i3, a, Some("not now".into())).unwrap();
    let i4 = e
        .submit(
            a,
            orbit(OUTSIDE_APPROVED, 5000.0, &["hawk-1"]),
            &w_later,
            &tr,
        )
        .unwrap()
        .id();
    let i5 = e
        .submit(a, orbit(NEAR_CRISSY, 150.0, &["blade-1"]), &w_later, &tr)
        .unwrap()
        .id();
    let mut stale = w_later.clone();
    stale.t = stale.t.plus_ms(60_000);
    clock.advance_ms(60_000);
    assert!(e.approve(i5, a, None, &stale, &tr).is_err());
    let i6 = e
        .agent_port(AgentId::new("planner"))
        .propose(hold(&["blade-1"]), rationale("pause"), &w_later, &tr)
        .unwrap()
        .id();
    assert_eq!(e.intent(i6).unwrap().state(), State::AutoApproved);
    let i7 = e
        .agent_port(AgentId::new("planner"))
        .propose(
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            rationale("again"),
            &w_later,
            &tr,
        )
        .unwrap()
        .id();
    e.agent_port(AgentId::new("planner"))
        .withdraw(i7, "superseded")
        .unwrap();
    clock.advance_ms(200_000);
    assert_eq!(e.expire_due().unwrap(), vec![i4]);
    e.record_vehicle_event(&uid("sky-1"), "mode", "RTL")
        .unwrap();
    log.text()
}

fn lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn unlines(v: &[Value]) -> String {
    v.iter()
        .map(|l| serde_json::to_string(l).unwrap() + "\n")
        .collect()
}

#[test]
fn replay_reproduces_every_decision() {
    let text = scenario();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(r.intents, 11);
    assert!(r.checks_recomputed >= 13, "{r:?}");
    assert!(r.policy_recomputed >= 9, "{r:?}");
    assert!(r.transitions_verified >= 30, "{r:?}");
    for kind in [
        "run.started",
        "intent.created",
        "check.result",
        "policy.decision",
        "intent.state",
        "approval.requested",
        "approval.granted",
        "approval.rejected",
        "approval.expired",
        "intent.dispatched",
        "op.state",
        "vehicle.event",
        "operator.action",
        "agent.action",
    ] {
        assert!(
            text.contains(&format!("\"type\":\"{kind}\"")),
            "no {kind} event"
        );
    }
    assert!(text.contains("\"outcome\":\"not_evaluated\""));
}

#[test]
fn runs_are_byte_identical() {
    assert_eq!(scenario(), scenario());
}

#[test]
fn log_round_trips_exactly() {
    let text = scenario();
    let events = parse_log(&text).unwrap();
    let again: String = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    assert_eq!(again, text);
    assert!(replay(&events).ok());
}

fn find(v: &[Value], ty: &str, pred: impl Fn(&Value) -> bool) -> usize {
    v.iter().position(|e| e["type"] == ty && pred(e)).unwrap()
}

#[test]
fn tampered_input_is_detected() {
    let mut v = lines(&scenario());
    // Lower sky-1's battery in the first check's inputs: reserves would now fail.
    let i = find(&v, "check.result", |_| true);
    v[i]["inputs"]["vehicles"]["sky-1"]["battery_pct"]["value"] = Value::from(25.0);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(!r.ok());
    assert_eq!(r.mismatches[0].what, "check results differ");
    assert_eq!(r.mismatches[0].seq, v[i]["seq"].as_u64().unwrap());
}

#[test]
fn tampered_policy_input_is_detected() {
    let mut v = lines(&scenario());
    // Claim the switchblade was unarmed when policy sent the agent's intent to a human.
    let i = find(&v, "policy.decision", |e| {
        e["decision"]["rule"] == "armed-platform"
    });
    v[i]["inputs"]["vehicles"]["blade-1"]["platform"]["armed"] = Value::from(false);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "policy decision differs"),
        "{r:#?}"
    );
}

#[test]
fn tampered_decision_is_detected() {
    let mut v = lines(&scenario());
    let i = find(&v, "policy.decision", |e| e["decision"]["tier"] == "human");
    v[i]["decision"]["tier"] = Value::from("auto");
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "policy decision differs")
    );
}

#[test]
fn approval_without_a_human_is_detected() {
    let mut v = lines(&scenario());
    // Drop the human's approval.granted and renumber, as a forger would.
    let i = find(&v, "approval.granted", |_| true);
    v.remove(i);
    for (n, e) in v.iter_mut().enumerate() {
        e["seq"] = Value::from(n as u64 + 1);
    }
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "approved without a human approval and a passing re-check"),
        "{r:#?}"
    );
}

#[test]
fn late_approval_and_gaps_are_detected() {
    let mut v = lines(&scenario());
    let i = find(&v, "approval.granted", |_| true);
    let intent = v[i]["intent"].clone();
    let req = find(&v, "approval.requested", |e| e["intent"] == intent);
    v[req]["expires_t"] = v[i]["t"].clone();
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "approval granted at or after its deadline"),
        "{r:#?}"
    );

    let mut v = lines(&scenario());
    v.remove(5);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what == "seq not contiguous"));
}

#[test]
fn replay_binary() {
    let dir = std::env::temp_dir().join(format!("murmur-decide-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.jsonl");
    // Write through the real append-only file sink.
    {
        let mut sink = JsonlFile::open(&good).unwrap();
        for l in scenario().lines() {
            sink.append(l).unwrap();
        }
    }
    let bin = env!("CARGO_BIN_EXE_murmur-replay");
    let out = Command::new(bin).arg(&good).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("OK: every decision reproduced exactly"));

    let mut v = lines(&std::fs::read_to_string(&good).unwrap());
    let i = find(&v, "check.result", |_| true);
    v[i]["inputs"]["vehicles"]["sky-1"]["nav_ready"]["value"] = Value::from(false);
    let bad = dir.join("bad.jsonl");
    std::fs::write(&bad, unlines(&v)).unwrap();
    let out = Command::new(bin).arg(&bad).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("MISMATCH"));

    std::fs::write(&bad, "not json\n").unwrap();
    assert_eq!(
        Command::new(bin).arg(&bad).output().unwrap().status.code(),
        Some(2)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
