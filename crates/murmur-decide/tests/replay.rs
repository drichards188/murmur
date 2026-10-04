//! Replay: a generated log reproduces every decision; tampering is detected, by the
//! decisions themselves, by the area-set hash, and by the hash chain.
#![allow(clippy::unwrap_used)]

mod common;

use std::process::Command;

use common::*;
use murmur_decide::hash::{GENESIS, sha256_hex};
use murmur_decide::replay::{ReplayReport, replay, replay_str};
use murmur_decide::*;
use serde_json::Value;

/// A run that exercises every decision path, on [`waiting_policy`] (human tiers make even
/// the submitter's own intent wait, so the inbox paths all run). Returns the log text.
fn scenario() -> String {
    let clock = ManualClock::new(T0);
    let (mut e, log) = waiting_engine(&clock);
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
    // what the operator's console showed while deciding (Phase G, `operator.view`)
    let shown = ViewShown {
        checks: vec!["capability".into(), "readiness".into(), "geofence".into()],
        warnings: vec!["armed platform: vehicle blade-1".into()],
        rationale: true,
        tracks: vec![],
    };
    e.record_operator_view(
        a,
        i2,
        OperatorView {
            event: "open".into(),
            action: None,
            shown: shown.clone(),
            open_ms: None,
            client_t: Some(1_700_000_000_000),
        },
    )
    .unwrap();
    e.record_operator_view(
        a,
        i2,
        OperatorView {
            event: "decide".into(),
            action: Some("approve".into()),
            shown,
            open_ms: Some(28_000),
            client_t: None,
        },
    )
    .unwrap();
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

    // a human approves their own proposal (allowed, flagged)
    let own = e
        .submit(a, orbit(NEAR_CRISSY, 150.0, &["blade-1"]), &w_later, &tr)
        .unwrap()
        .id();
    e.approve(own, a, None, &w_later, &tr).unwrap();

    // endurance unknown: waits for a human, who approves it
    let mule = e
        .submit(a, orbit(NEAR_CRISSY, 100.0, &["mule-1"]), &w_later, &tr)
        .unwrap()
        .id();
    assert_eq!(e.intent(mule).unwrap().state(), State::PendingApproval);
    e.approve(mule, a, None, &w_later, &tr).unwrap();

    // rtl whose way home crosses a no-go area: rejected by the geofence (return leg)
    let mut crossing = w_later.clone();
    let v = crossing.vehicles.get_mut(&uid("sky-1")).unwrap();
    v.position = Some(Fact::new(LonLat::new(-122.405, 37.8267), w_later.t));
    v.home = Some(Fact::new(LonLat::new(-122.435, 37.8267), w_later.t));
    let r = e.submit(a, rtl(&["sky-1"]), &crossing, &tr).unwrap();
    assert_eq!(r.state(), State::Rejected);

    // the area set changes mid-run: logged once more, as areas.loaded
    let mut moved = w_later.clone();
    moved.areas.as_mut().unwrap().loaded_t = w_later.t;
    e.submit(a, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &moved, &tr)
        .unwrap();
    e.submit(a, orbit(NEAR_CRISSY, 160.0, &["sky-1"]), &moved, &tr)
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

/// The line the engine would write for this event (field order, not Value's sorted keys).
fn canon(v: &Value) -> String {
    let e: Event = serde_json::from_value(v.clone()).unwrap();
    serde_json::to_string(&e).unwrap()
}

fn unlines(v: &[Value]) -> String {
    v.iter().map(|l| canon(l) + "\n").collect()
}

fn renumber(v: &mut [Value]) {
    for (n, e) in v.iter_mut().enumerate() {
        e["seq"] = Value::from(n as u64 + 1);
    }
}

/// Recompute every `prev`, as a forger who can write the file would.
fn rechain(v: &mut [Value]) {
    let mut prev = GENESIS.to_string();
    for e in v.iter_mut() {
        e["prev"] = Value::from(prev);
        prev = sha256_hex(canon(e).as_bytes());
    }
}

fn whats(r: &ReplayReport) -> Vec<&str> {
    r.mismatches.iter().map(|m| m.what.as_str()).collect()
}

const CHAIN: &str = "hash chain broken: prev is not the hash of the previous event";

/// The chain, and nothing else, objects; its first objection is at `seq`. (Renumbering
/// rewrites every later event, so every later link breaks too.)
fn only_chain_from(r: &ReplayReport, seq: u64) {
    assert!(!r.mismatches.is_empty(), "undetected");
    assert!(r.mismatches.iter().all(|m| m.what == CHAIN), "{r:#?}");
    assert_eq!(r.mismatches[0].seq, seq);
}

#[test]
fn replay_reproduces_every_decision() {
    let text = scenario();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(r.intents, 16);
    assert!(r.checks_recomputed >= 19, "{r:?}");
    assert!(r.policy_recomputed >= 13, "{r:?}");
    assert!(r.transitions_verified >= 43, "{r:?}");
    assert_eq!((r.approvals, r.self_approvals), (3, 2));
    assert_eq!(r.area_sets, 3);
    assert_eq!(
        r.head_sha256.as_deref(),
        Some(sha256_hex(text.lines().last().unwrap().as_bytes()).as_str())
    );
    for kind in [
        "run.started",
        "areas.loaded",
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
        "operator.view",
        "agent.action",
    ] {
        assert!(
            text.contains(&format!("\"type\":\"{kind}\"")),
            "no {kind} event"
        );
    }
    assert!(text.contains("\"outcome\":\"not_evaluated\""));
    assert!(text.contains("\"outcome\":\"unverified\""));
    assert!(text.contains("return leg (position to home) crosses alcatraz"));
}

#[test]
fn runs_are_byte_identical() {
    assert_eq!(scenario(), scenario());
}

/// An `operator.view` event is additive: replay makes no decision from it, verifies the
/// chain through it, and the engine refuses one for an unknown intent or a malformed view.
#[test]
fn operator_views_are_logged_and_ignored_by_replay() {
    let clock = ManualClock::new(T0);
    let (mut e, log) = waiting_engine(&clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let a = op.authority();
    let w = world();
    let tr = terrain();
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
    let view = |event: &str, action: Option<&str>| OperatorView {
        event: event.into(),
        action: action.map(str::to_string),
        shown: ViewShown::default(),
        open_ms: None,
        client_t: None,
    };
    assert!(matches!(
        e.record_operator_view(a, IntentId(99), view("open", None)),
        Err(EngineError::UnknownIntent(_))
    ));
    assert!(matches!(
        e.record_operator_view(a, id, view("peek", None)),
        Err(EngineError::Proposal(_))
    ));
    assert!(matches!(
        e.record_operator_view(a, id, view("open", Some("approve"))),
        Err(EngineError::Proposal(_))
    ));
    assert!(matches!(
        e.record_operator_view(a, id, view("decide", None)),
        Err(EngineError::Proposal(_))
    ));
    let mut big = view("open", None);
    big.shown.tracks = vec!["T-0001".into(); OperatorView::MAX_ITEMS + 1];
    assert!(matches!(
        e.record_operator_view(a, id, big),
        Err(EngineError::Proposal(_))
    ));
    let mut long = view("open", None);
    long.shown.warnings = vec!["x".repeat(OperatorView::MAX_TEXT + 1)];
    assert!(matches!(
        e.record_operator_view(a, id, long),
        Err(EngineError::Proposal(_))
    ));
    e.record_operator_view(a, id, view("open", None)).unwrap();
    e.record_operator_view(a, id, view("decide", Some("reject")))
        .unwrap();
    e.reject(id, a, Some("neutral track".into())).unwrap();
    // after the decision: no late open or decide, a close is still taken
    assert!(matches!(
        e.record_operator_view(a, id, view("decide", Some("approve"))),
        Err(EngineError::ViewNotPending { .. })
    ));
    assert!(matches!(
        e.record_operator_view(a, id, view("open", None)),
        Err(EngineError::ViewNotPending { .. })
    ));
    e.record_operator_view(a, id, view("close", None)).unwrap();
    let text = log.text();
    assert_eq!(text.matches("\"type\":\"operator.view\"").count(), 3);
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(e.intent(id).unwrap().state(), State::Rejected);
    // editing a view event breaks the chain like any other event
    let tampered = text.replacen("\"event\":\"open\"", "\"event\":\"close\"", 1);
    assert!(!replay_str(&tampered).unwrap().ok());
    // a view moved to another operator's session, the chain recomputed: replay objects
    let mut v: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let other = v
        .iter()
        .find(|e| e["type"] == "operator.action" && e["action"] == "session.opened")
        .map(|e| e["session"].as_u64().unwrap() + 100)
        .unwrap();
    for e in v.iter_mut().filter(|e| e["type"] == "operator.view") {
        e["session"] = Value::from(other);
    }
    rechain(&mut v);
    let forged: String = v.iter().map(|e| canon(e) + "\n").collect();
    let r = replay_str(&forged).unwrap();
    assert!(
        whats(&r).contains(&"view under a session that is not the operator's"),
        "{:#?}",
        r.mismatches
    );
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
    assert_eq!(&whats(&r)[..2], &["check results differ", CHAIN]);
    assert_eq!(r.mismatches[0].seq, v[i]["seq"].as_u64().unwrap());
    assert_eq!(r.mismatches[1].seq, v[i]["seq"].as_u64().unwrap() + 1);
    // A forger who recomputes the chain is still caught by the decision itself.
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert_eq!(whats(&r)[0], "check results differ");
    assert!(!whats(&r).contains(&CHAIN));
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
    // Drop the human's approval.granted, renumber and recompute the chain, as a forger
    // would: the approval is still unbacked.
    let i = find(&v, "approval.granted", |_| true);
    v.remove(i);
    renumber(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert_eq!(r.mismatches[0].what, CHAIN);
    assert_eq!(r.mismatches[0].seq, i as u64 + 1);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(!whats(&r).contains(&CHAIN));
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

// ---- hash chain -------------------------------------------------------------------------

#[test]
fn chain_detects_edit_delete_and_reorder_of_any_event() {
    let original = lines(&scenario());
    // an op state's detail carries no decision: only the chain can notice an edit to it
    // (its intent and vehicle are bound to the dispatch: tests/bindings.rs)
    let i = find(&original, "op.state", |_| true);

    let mut v = original.clone();
    v[i]["detail"] = Value::from("never got there");
    let r = replay_str(&unlines(&v)).unwrap();
    assert_eq!(whats(&r), vec![CHAIN]);
    assert_eq!(r.mismatches[0].seq, v[i + 1]["seq"].as_u64().unwrap());

    let mut v = original.clone();
    v.remove(i);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(whats(&r).contains(&CHAIN));
    assert!(whats(&r).contains(&"seq not contiguous"));
    // Renumbered to hide the gap: the chain still breaks where the event was.
    renumber(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    only_chain_from(&r, i as u64 + 1);

    let mut v = original.clone();
    v.swap(i, i - 1);
    renumber(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(whats(&r).contains(&CHAIN), "{r:#?}");

    // An edit the parser would drop (an extra field) is still an edit.
    let text = scenario();
    let line = text.lines().nth(i).unwrap();
    let edited = text.replacen(line, &line.replacen("{", r#"{"x":1,"#, 1), 1);
    let r = replay_str(&edited).unwrap();
    assert!(
        whats(&r).contains(&"line is not its event's canonical serialization"),
        "{r:#?}"
    );
}

#[test]
fn chain_detects_a_whole_intent_deleted_and_renumbered() {
    let text = scenario();
    let original = lines(&text);
    let head = replay_str(&text).unwrap().head_sha256;
    // Delete every event of one intent (the boat orbit with no terrain oracle) and
    // renumber seq: every remaining decision is still consistent.
    let created = original
        .iter()
        .filter(|e| e["type"] == "intent.created")
        .nth(4)
        .unwrap();
    let id = created["intent"]["id"].clone();
    let mut v: Vec<Value> = original
        .iter()
        .filter(|e| e["intent"] != id && e["intent"]["id"] != id)
        .cloned()
        .collect();
    assert!(original.len() - v.len() >= 4);
    let first_gone = original
        .iter()
        .position(|e| e["intent"]["id"] == id)
        .unwrap();
    renumber(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    // Only the chain sees it, starting where the intent's first event was.
    only_chain_from(&r, first_gone as u64 + 1);

    // The limit, stated as a test: someone who can rewrite the file can recompute the
    // whole chain, and replay passes. Only the head hash differs, which is why it has to
    // be anchored outside the writer's reach (tests/anchors.rs: run-time anchors).
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(r.ok(), "{r:#?}");
    assert_ne!(r.head_sha256, head);
    // Same for a truncated tail: a valid, shorter chain with a different head.
    let short: String = text.lines().take(10).map(|l| format!("{l}\n")).collect();
    let r = replay_str(&short).unwrap();
    assert!(r.ok());
    assert_ne!(r.head_sha256, head);
}

#[test]
fn first_event_must_start_the_chain() {
    let mut v = lines(&scenario());
    v.remove(0);
    renumber(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    let chain = r.mismatches.iter().find(|m| m.what == CHAIN).expect("chain broken");
    assert_eq!(chain.recomputed, GENESIS);
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "the log does not start with run.started"),
        "{:?}",
        r.mismatches
    );
}

// ---- area set, logged once ----------------------------------------------------------------

#[test]
fn area_set_is_logged_once_and_referenced_by_hash() {
    let text = scenario();
    let v = lines(&text);
    let loaded: Vec<&Value> = v.iter().filter(|e| e["type"] == "areas.loaded").collect();
    // Engine::new had no area set: logged at first use, then once each time it changed
    // (to the moved set, and back), never per decision.
    assert_eq!(loaded.len(), 3);
    let hashes: Vec<&str> = loaded
        .iter()
        .map(|e| e["sha256"].as_str().unwrap())
        .collect();
    assert_ne!(hashes[0], hashes[1]);
    assert_eq!(hashes[0], hashes[2]);
    for e in v
        .iter()
        .filter(|e| e["type"] == "check.result" || e["type"] == "policy.decision")
    {
        assert!(e["inputs"].get("areas").is_none(), "{e}");
        let h = e["inputs"]["areas_sha256"].as_str().unwrap();
        assert!(hashes.contains(&h));
    }
    // The hash is of the canonical serialization of the set.
    let set: AreaSet = serde_json::from_value(loaded[0]["areas"].clone()).unwrap();
    assert_eq!(set.content_hash().unwrap(), hashes[0]);
    assert_eq!(
        hashes[0],
        sha256_hex(serde_json::to_string(&set).unwrap().as_bytes())
    );
    // Decision events no longer carry ~700 bytes of polygons each.
    let check = text
        .lines()
        .find(|l| l.contains(r#""type":"check.result""#))
        .unwrap();
    assert!(!check.contains("alcatraz"), "{check}");
}

#[test]
fn area_set_given_at_start_is_in_run_started() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::with_areas(
        policy(),
        Some(areas()),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.submit(
        op.authority(),
        orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
        &world(),
        &terrain(),
    )
    .unwrap();
    let v = lines(&log.text());
    assert_eq!(v[0]["type"], "run.started");
    assert_eq!(
        v[0]["areas"]["sha256"],
        Value::from(areas().content_hash().unwrap())
    );
    assert!(v.iter().all(|e| e["type"] != "areas.loaded"));
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(r.area_sets, 1);
    assert_eq!(r.head_sha256.as_deref(), Some(e.head_hash()));
}

#[test]
fn area_hash_mismatch_is_detected() {
    // Shrink Alcatraz in the logged set but keep its hash, and recompute the chain.
    let mut v = lines(&scenario());
    let i = find(&v, "areas.loaded", |_| true);
    let areas = v[i]["areas"]["areas"].as_array_mut().unwrap();
    let alcatraz = areas.iter_mut().find(|a| a["id"] == "alcatraz").unwrap();
    alcatraz["polygons"][0]["exterior"][0][0] = Value::from(-122.42);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    let w = whats(&r);
    assert_eq!(w[0], "area set hash does not match its content");
    assert!(w.contains(&"decision references an area set whose hash did not verify"));

    // A decision pointing at a set that isn't the one in force.
    let mut v = lines(&scenario());
    let i = find(&v, "check.result", |_| true);
    v[i]["inputs"]["areas_sha256"] = Value::from(GENESIS);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(
        whats(&r).contains(&"decision references an area set that is not the one in force"),
        "{r:#?}"
    );
}

// ---- self-approval --------------------------------------------------------------------------

#[test]
fn self_approval_flag_is_verified() {
    let mut v = lines(&scenario());
    let i = find(&v, "approval.granted", |e| e["self_approved"] == true);
    v[i]["self_approved"] = Value::from(false);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert_eq!(whats(&r), vec!["self_approved flag differs"]);
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("OK: every decision reproduced exactly"));
    assert!(
        stdout.contains("3 approval(s), 2 self-approved"),
        "{stdout}"
    );
    assert!(stdout.contains("head "), "{stdout}");

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

// ---- approval at submission -------------------------------------------------------------------

/// On the sim policy as shipped: op-1's own armed and outside-area orbits are approved at
/// submission; the agent's armed orbit waits and op-1 approves it in the inbox.
fn scenario_at_submission() -> String {
    let clock = ManualClock::new(T0);
    let (mut e, log) = engine(&clock);
    let op = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let a = op.authority();
    let (w, tr) = (world(), terrain());
    let armed = e
        .submit(a, orbit(NEAR_CRISSY, 150.0, &["blade-1"]), &w, &tr)
        .unwrap()
        .id();
    e.dispatch(armed).unwrap();
    let outside = e
        .submit(a, orbit(OUTSIDE_APPROVED, 5000.0, &["hawk-1"]), &w, &tr)
        .unwrap()
        .id();
    assert_eq!(e.intent(outside).unwrap().state(), State::Approved);
    let proposed = e
        .agent_port(AgentId::new("planner"))
        .propose(
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            rationale("cover the LZ"),
            &w,
            &tr,
        )
        .unwrap()
        .id();
    assert_eq!(e.intent(proposed).unwrap().state(), State::PendingApproval);
    clock.advance_ms(1_000);
    e.approve(proposed, a, None, &w, &tr).unwrap();
    e.void(outside, "superseded").unwrap();
    log.text()
}

const UNBACKED_AT_SUBMISSION: &str = "approved at submission without passing checks, a human tier that allows it, and the submitter's approval.at_submission";

fn armed_tier(v: &[Value]) -> usize {
    v[0]["policy"]["tier"]
        .as_array()
        .unwrap()
        .iter()
        .position(|t| t["name"] == "armed-platform")
        .unwrap()
}

#[test]
fn at_submission_approvals_replay() {
    let text = scenario_at_submission();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(
        (r.approvals, r.self_approvals, r.approvals_at_submission),
        (1, 0, 2)
    );
    let v = lines(&text);
    let ev = &v[find(&v, "approval.at_submission", |_| true)];
    assert_eq!(ev["by"], "op-1");
    assert_eq!(ev["self_approved"], true);
    assert_eq!(ev["rule"], "armed-platform");
    let dec = &v[find(&v, "policy.decision", |_| true)];
    assert_eq!(dec["decision"]["submitter_authorizes"], true);
    // The policy as logged says so explicitly.
    assert_eq!(v[0]["policy"]["approval"]["submitter_authorizes"], true);
    assert_eq!(
        v[0]["policy"]["tier"][armed_tier(&v)]["submitter_authorizes"],
        true
    );
}

#[test]
fn at_submission_approval_by_someone_else_is_detected() {
    let mut v = lines(&scenario_at_submission());
    let i = find(&v, "approval.at_submission", |_| true);
    v[i]["by"] = Value::from("op-2");
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    let w = whats(&r);
    assert!(
        w.contains(&"approval at submission by someone other than the submitter"),
        "{r:#?}"
    );
    assert!(w.contains(&UNBACKED_AT_SUBMISSION));
}

#[test]
fn an_agent_intent_forged_as_approved_at_submission_is_detected() {
    // Rewrite the agent's proposal as if it had been approved at submission: its
    // approval.requested becomes an approval.at_submission, its require_approval an
    // authorize_at_submission, and the later inbox approval is dropped. Chain recomputed.
    let mut v = lines(&scenario_at_submission());
    let req = find(&v, "approval.requested", |_| true);
    let id = v[req]["intent"].clone();
    v[req] = serde_json::json!({
        "seq": 0, "t": v[req]["t"], "prev": "", "type": "approval.at_submission",
        "intent": id, "by": "op-1", "session": 1, "self_approved": true,
        "rule": "armed-platform",
    });
    let st = find(&v, "intent.state", |e| {
        e["intent"] == id && e["transition"] == "require_approval"
    });
    v[st]["transition"] = Value::from("authorize_at_submission");
    v[st]["to"] = Value::from("approved");
    let granted = find(&v, "approval.granted", |e| e["intent"] == id);
    v.remove(granted);
    let approve = find(&v, "intent.state", |e| {
        e["intent"] == id && e["transition"] == "approve"
    });
    v.remove(approve);
    renumber(&mut v);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    let w = whats(&r);
    assert!(
        w.contains(&"agent-sourced intent approved at submission"),
        "{r:#?}"
    );
    assert!(w.contains(&"approval at submission in a tier that doesn't allow it"));
    assert!(w.contains(&UNBACKED_AT_SUBMISSION));
}

#[test]
fn at_submission_without_its_event_or_its_tier_is_detected() {
    // Drop the approval.at_submission: the transition is unbacked.
    let mut v = lines(&scenario_at_submission());
    let i = find(&v, "approval.at_submission", |_| true);
    v.remove(i);
    renumber(&mut v);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    assert!(whats(&r).contains(&UNBACKED_AT_SUBMISSION), "{r:#?}");
    // Claim the logged policy made the armed tier two-person: the decision no longer
    // reproduces, and the at-submission approval is not allowed by it.
    let mut v = lines(&scenario_at_submission());
    let t = armed_tier(&v);
    v[0]["policy"]["tier"][t]["submitter_authorizes"] = Value::from(false);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    let w = whats(&r);
    assert!(w.contains(&"policy decision differs"), "{r:#?}");
    assert!(w.contains(&"approval at submission in a tier that doesn't allow it"));
}

#[test]
fn a_submitter_sent_to_the_inbox_in_a_self_authorizing_tier_is_detected() {
    // The waiting scenario's decisions say the submitter does not authorize. Make the
    // logged policy say it does: those intents should have been approved at submission,
    // not sent to the inbox.
    let mut v = lines(&scenario());
    v[0]["policy"]["approval"]["submitter_authorizes"] = Value::from(true);
    rechain(&mut v);
    let r = replay_str(&unlines(&v)).unwrap();
    let w = whats(&r);
    assert!(w.contains(&"policy decision differs"), "{r:#?}");
    assert!(w.contains(&"sent for approval, but the submitter authorizes this tier"));
}

#[test]
fn logs_from_before_submitter_authorizes_still_replay() {
    // An old log: no submitter_authorizes anywhere (the policy and decisions as they were
    // written then). Unset reads as false, which is how that run behaved.
    let mut v = lines(&scenario());
    v[0]["policy"]["approval"]
        .as_object_mut()
        .unwrap()
        .remove("submitter_authorizes");
    for t in v[0]["policy"]["tier"].as_array_mut().unwrap() {
        t.as_object_mut().unwrap().remove("submitter_authorizes");
    }
    rechain(&mut v);
    let text = unlines(&v);
    assert!(!text.contains("submitter_authorizes"));
    assert!(!text.contains("at_submission"));
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!((r.approvals, r.approvals_at_submission), (3, 0));
}
